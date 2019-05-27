// liborchestra/ssh/config.rs
// Author: Alexandre Péré
/// This module contains a parser for openssh config files. It could be done with Regex but it was
/// so much fun to learn about lexer/parsers :).

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use std::str::CharIndices;
use std::iter::Peekable;
use std::fmt;
use std::error;
use std::io::prelude::*;
use std::path::PathBuf;
use std::fs::File;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    Lexer(IndexedString, String),
    Parser(IndexedString, String),
    Reader(IndexedString, String),
    GettingProfile(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Lexer(ref is, ref r) =>
                write!(f, "The lexer encountered an unexpected character:\n```\n{}\n```\nHint: {}", is, r),
            Error::Parser(ref is, ref r) =>
                write!(f, "The parser encountered an unexpected token:\n```\n{}\n```\nHint: {}", is, r),
            Error::Reader(ref is, ref r) =>
                write!(f, "The configuration reader encountered an unexpected node:\n```\n{}\n```\nHint; {}", is, r),
            Error::GettingProfile(ref s) =>
                write!(f, "An error occurred while getting a profile: {}", s),
        }
    }
}

/////////////////////////////////////////////////////////////////////////////////////// SSH-PROFILES
#[derive(Debug, Clone, Hash)]
/// Represents a reduced ssh host configuration.
pub struct SshProfile{
    pub name: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<usize>,
    pub proxycommand: Option<String>,
}

impl SshProfile{

    fn from(name: String) -> SshProfile{
        return SshProfile{
            name,
            hostname: None,
            user: None,
            port: None,
            proxycommand: None,
        }
    }

    fn set_hostname(mut self, hostname: String) -> SshProfile{
        debug!("Setting profile hostname");
        self.hostname.replace(hostname);
        self
    }

   fn set_user(mut self, user: String) -> SshProfile{
        debug!("Setting profile user");
        self.user.replace(user);
        self
    }

    fn set_port(mut self, port: usize) -> SshProfile{
        debug!("Setting profile port");
        self.port.replace(port);
        self
    }

    fn set_proxycommand(mut self, proxycommand: String) -> SshProfile{
        debug!("Setting profile proxycommand");
        self.proxycommand.replace(proxycommand);
        self
    }

    fn complete(mut self) -> SshProfile{
        debug!("Completing profile");
        if self.hostname.is_none(){
            trace!("Setting hostname to name {}", self.name);
            self.hostname = Some(self.name.clone());
        }
        if self.port.is_none(){
            trace!("Setting port to 22");
            self.port = Some(22);
        }
        if self.user.is_none(){
            let user = std::env::var_os("USER")
                .map_or("user".to_owned(), |s| s.into_string().unwrap());
            trace!("Setting user to current username {:?}", user);
            self.user = Some(user);
        }
        return self;
    }
}

//////////////////////////////////////////////////////////////////////////////////// INDEXED STRINGS
// The two following structures allow to work on located string slices. This helps to implement
// parsing in a non-owning way.

// This structure represents a(n explicitly indexed) slice of a string. Could either represents a
// string span (if the first and second index are different), or a cursor (if the first and second
// index are the same).
#[derive(Clone, Copy)]
struct IndexedSlice<'s>(&'s str, usize, usize);

impl<'s> IndexedSlice<'s>{
    
    // Construct an indexed slice from a cursor position
    fn from_cursor(slice: &'s str, pos: usize) -> IndexedSlice<'s>{
        return IndexedSlice(slice, pos, pos);
    }

    // Construct an indexed slice which points to the beginning of the string.
    fn beginning(slice: &'s str) -> IndexedSlice<'s>{
        return IndexedSlice(slice, 0, 0);
    }

    // Construct an indexed slice which points to the end of the string.
    fn end(slice: &'s str) -> IndexedSlice<'s>{
        return IndexedSlice(slice, slice.len(), slice.len());
    }

    // Constructs an indexed slice from a span
    fn from_span(slice: &'s str, begin: usize, end: usize) -> IndexedSlice<'s>{
        return IndexedSlice(slice, begin, end);
    }

    // Moves begining to a new index
    fn move_begining(&mut self, idx: usize){
        self.1 = idx;
    }

    // Moves end to a new index
    fn move_end(&mut self, idx: usize) {
        self.2 = idx;
    }

    // Moves begining to a new index
    fn move_begining_by(&mut self, idx: isize){
        match idx{
            i if i<0 => {
                self.1 = self._before(self.1, -i as usize)
            }
            i if i>0 =>{
                self.1 = self._after(self.1, i as usize)
            }
            _ =>{}
        }
    }

    // Moves end to a new index
    fn move_end_by(&mut self, idx: isize) {
        match idx{
            i if i<0 => {
                self.2 = self._before(self.2, -i as usize)
            }
            i if i>0 =>{
                println!("{}", i as usize);
                self.2 = self._after(self.2, i as usize +1)
            }
            _ =>{}
        }
    }

    // Moves both ends to a new index
    fn move_both(&mut self, idx: usize){
        self.1 = idx;
        self.2 = idx;
    }

    // Returns the indexed slice as an &str.
    fn as_str(&self) -> &str{
        return &self.0[self.1..self.2]
    }

    // Return an owned IndexedString out of the indexed slice.
    fn to_owned(&self) -> IndexedString{
        return IndexedString(self.0.to_owned(), self.1, self.2);
    }

    // Allows to retrieve the index of the character e indexes before the i-th character.
    fn _before(&self, i: usize, e:usize) -> usize{
        match self.0[..i]
            .char_indices()
            .rev()
            .take(e)
            .last() {
                Some(c) => c.0,
                None => i
            }
    }

    // Allows to retrieve the index of the character e indexes after the i-th character.
    fn _after(&self, i: usize, e: usize) -> usize{
        match self.0[i..]
            .char_indices()
            .take(e)
            .last(){
                Some(c) => c.0+i,
                None => i
            }
    }

}

impl<'s> fmt::Debug for IndexedSlice<'s> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let before = match self._before(self.1, 5) {
            0 => format!("{}", &self.0[..self.1]),
            b => format!("...{}", &self.0[b..self.1])
        };
        let after = match self._after(self.2, 5){
            i if i == self.0.len() => format!("{}", &self.0[self.2..]),
            e => format!("{}...", &self.0[self.2..e])
        };
        let middle = match self.1 == self.2 {
            true => format!("↓"),
            false => format!("↳{}↲", &self.0[self.1..self.2]),
        };
        let output = format!("{}{}{}", before, middle, after);
        write!(f, "IndexedSlice({:?})", output)
    }
}

// A public and owned counterpart to IndexedSlice. Used to print locations of unexpected characters
// in errors.
#[derive(Clone)]
pub struct IndexedString(String, usize, usize);

impl IndexedString{
    fn _before(&self, i: usize, e:usize) -> usize{
        match self.0[..i]
            .char_indices()
            .rev()
            .take(e)
            .last() {
                Some(c) => c.0,
                None => i
            }
    }
    fn _after(&self, i: usize, e: usize) -> usize{
        match self.0[i..]
            .char_indices()
            .take(e)
            .last(){
                Some(c) => c.0+i,
                None => i
            }
    }
}

impl fmt::Debug for IndexedString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let before = match self._before(self.1, 5) {
            0 => format!("{}", &self.0[..self.1]),
            b => format!("...{}", &self.0[b..self.1])
        };
        let after = match self._after(self.2, 5){
            i if i==self.0.len() => format!("{}", &self.0[self.2..]),
            e => format!("{}...", &self.0[self.2..e])
        };
        let middle = match self.1 == self.2 {
            true => format!("↓"),
            false => format!("↳{}↲", &self.0[self.1..self.2]),
        };
        let output = format!("{}{}{}", before, middle, after);
        write!(f, "IndexedSlice({:?})", output)
    }
}

impl fmt::Display for IndexedString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let before = match self._before(self.1, 20) {
            0 => format!("{}", &self.0[..self.1]),
            b => format!("...{}", &self.0[b..self.1])
        };
        let after = match self._after(self.2, 20){
            i if i==self.0.len() => format!("{}", &self.0[self.2..]),
            e => format!("{}...", &self.0[self.2..e])
        };
        let middle = match self.1 == self.2 {
            true => format!("↓"),
            false => format!("↳{}↲", &self.0[self.1..self.2]),
        };
        write!(f, "{}{}{}", before, middle, after)
    }
}

////////////////////////////////////////////////////////////////////////////////////////////// LEXER

// Represents the different tokens types expected in the config file.
#[derive(Debug, Clone, PartialEq)]
enum TokenType{
    Word,
    Comment,
    NewLine,
    Tab,
}

// A token is tagged IndexedSlice.
#[derive(Debug)]
struct Token<'s>(TokenType, IndexedSlice<'s>);

// An iterator that streams tokens out of a string. The `consume_*` methods returns Option<Token>.
// If the output is None, than no token should be emitted. If the output is Some, then a token
// should be emitted.
struct Lexer<'s>{
    string: &'s str,
    iter: Peekable<CharIndices<'s>>,
    exhausted: bool
}

impl<'s> Iterator for Lexer<'s>{
    type Item = Result<Token<'s>, Error>;

    fn next(&mut self) -> Option<Result<Token<'s>, Error>>{
        // We loop to keep consuming when no token was emitted.
        loop{
            trace!("Lexer: Consuming new character: {:?}", self.iter.peek());
            // We perform a 1-lookahead to decide which consumer to use.
            let ret = match self.iter.peek() {
                Some((_, '\n')) => self.consume_newline(),
                Some((_, '\t')) => self.consume_tab(),
                Some((_, '#')) => self.consume_comment(),
                Some((_, ' ')) => self.consume_whitespace(),
                Some((_, _)) => self.consume_word(),
                None => return None // If a None is seen, then the lexer iterator is consumed.
            };
            // If a token was emitted by the consumer then we return it
            if let Some(t) = ret{
                trace!("Lexer: Token emitted: {:?}", t);
                return Some(t)
            }
        }
    }
}

impl<'s> Lexer<'s> {
    // Creates a lexer from a string.
    fn from(string: &str) -> Lexer {
        let mut iter = string.char_indices().peekable();
        Lexer {
            string,
            iter,
            exhausted: false,
        }
    }

    // Consumes a newline token
    fn consume_newline(&mut self) -> Option<Result<Token<'s>, Error>>{
        debug!("Lexer: Consuming newline");
        let mut ret = Token(TokenType::NewLine,IndexedSlice::beginning(self.string));
        // We consume the first character which should match our expectations if the dispatch is
        // working
        if let Some((b, '\n'))=self.iter.next(){
            (ret.1).move_begining(b);
            (ret.1).move_end(b);
        } else {panic!("Consume newline called on wrong character")}
        // We look ahead to retrieve the next character index
        match self.iter.peek(){
            Some((e, _)) => {
                (ret.1).move_end(*e)
            }
            None => {
                (ret.1).move_end(self.string.len())
            }
        }
        // While next lookahead is a newline, we consume, since multiple newlines have no particular
        // meaning
        while let Some((_, '\n')) = self.iter.peek(){
            self.iter.next();
        }
        return Some(Ok(ret));
    }

    // Consumes a tab token
    fn consume_tab(&mut self) -> Option<Result<Token<'s>, Error>>{
        debug!("Lexer: Consuming tab");
        let mut ret = Token(TokenType::Tab,IndexedSlice::beginning(self.string));
        // We consume the first character which should match our expectations if the dispatch is
        // working
        if let Some((b, '\t'))=self.iter.next(){
            (ret.1).move_begining(b);
            (ret.1).move_end(b);
        } else {panic!("Consume tab called on wrong character.") }
        // We look ahead to retrieve the next character index.
        match self.iter.peek(){
            Some((e, _))  => {
                (ret.1).move_end(*e)
            }
            None => {
                (ret.1).move_end(self.string.len())
            }
        }
        // There is no more to do, as tabs shall not be coerced into one.
        return Some(Ok(ret));
    }

    // Consumes a comment token
    fn consume_comment(&mut self) -> Option<Result<Token<'s>, Error>>{
        debug!("Lexer: Consuming Comment");
        let mut ret = Token(TokenType::Comment, IndexedSlice::beginning(self.string));
        // We consume the first character which should match our expectations if the dispatch is
        // working
        if let Some((b, '#')) = self.iter.next() {
            (ret.1).move_begining(b);
            (ret.1).move_end(b);
        } else {panic!("Consume Comment called on wrong character.")}
        // While no newline or none (eof) is encountered, we keep consuming characters.
        loop{
            match self.iter.peek(){
                Some((e, '\n')) => {
                    (ret.1).move_end(*e);
                    break
                }
                None =>{
                    (ret.1).move_end(self.string.len());
                    break
                }
                _ => {
                    self.iter.next();
                }
            }
        }
        return Some(Ok(ret));
    }

    // Consumes a word token
    fn consume_word(&mut self) -> Option<Result<Token<'s>, Error>>{
        debug!("Lexer: Consuming word");
        let mut ret = Token(TokenType::Word, IndexedSlice::beginning(self.string));
        // We consume the first character which should match our expectations if the dispatch is
        // working
        match self.iter.next() {
            Some((_, '\n')) | Some((_, '\t')) | Some((_, '#')) | None => {
                panic!("Consume word called on wrong character");
            }
            Some((_, ' ')) => {
                panic!("Consume word called on wrong character");
            }
            Some((b, chr)) => {
                (ret.1).move_begining(b);
                (ret.1).move_end(b);
            }
        }
        // While the characters are good, we consume them.
        loop {
            match self.iter.peek() {
                Some((e, '\n')) | Some((e, '\t')) | Some((e, '#')) => {
                    (ret.1).move_end(*e);
                    return Some(Ok(ret))
                }
                None => {
                    (ret.1).move_end(self.string.len());
                    return Some(Ok(ret))
                }
                Some((e, chr)) if !chr.is_ascii() => {
                    (ret.1).move_both(*e);
                    (ret.1).move_end_by(1);
                    println!("{:?}", ret.1);
                    return Some(Err(Error::Lexer(ret.1.to_owned(),
                                                 "Character is not ascii.".to_owned())))
                }
                Some((e, ' ')) => {
                    (ret.1).move_end(*e);
                    break
                }
                _ => {
                    self.iter.next();
                }
            }
        }
        // While characters are whitespaces, we consume.
        loop{
            match self.iter.peek(){
                Some((_, ' ')) => {
                    self.iter.next();
                }
                _ => {
                    return Some(Ok(ret))
                }
            }
        }
    }

    // Consume whitespaces
    fn consume_whitespace(&mut self) -> Option<Result<Token<'s>, Error>>{
        debug!("Lexer: Consuming whitespace.");
        // We consume the first whitespace
        match self.iter.next(){
            Some((_, ' ')) => { },
            _ => panic!("Consume whitespace called on wrong character")
        }
        // And all following whitespaces
        loop{
            match self.iter.peek(){
                Some((_, ' ')) => {self.iter.next();}
                _ => return None
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////////////////// PARSER

// Represents the different types a parsed node can be.
#[derive(Debug, PartialEq)]
enum NodeType{
    Host,
    HostNameClause,
    UserClause,
    PortClause,
    ProxyCommandClause,
}

// A node is a tagged indexed slice.
#[derive(Debug)]
struct Node<'s>(NodeType, IndexedSlice<'s>);

// An iterator that yields a stream of nodes out of a lexer iterator.
struct Parser<'s>{
    string: &'s str,
    iter: Peekable<Lexer<'s>>,
    exhausted: bool,
}

impl<'s> Iterator for Parser<'s>{
    type Item = Result<Node<'s>, Error>;
    fn next(&mut self) -> Option<Result<Node<'s>, Error>>{
        if !self.exhausted{
            loop{
                trace!("Parser: Consuming a new token: {:?}", self.iter.peek());
                let ret = match self.iter.peek() {
                    Some(Ok(Token(TokenType::Word, _))) => self.consume_host(),
                    Some(Ok(Token(TokenType::Tab, _))) => self.consume_clause(),
                    Some(Ok(Token(TokenType::NewLine, _))) => self.consume_newline(),
                    Some(Ok(Token(TokenType::Comment, _))) => self.consume_comment(),
                    Some(Err(e)) => return Some(Err(self.iter.next().unwrap().unwrap_err())),
                    None => return None
                };
                if let Some(n) = ret{
                    trace!("Parser: Node emitted: {:?}", n);
                    return Some(n)
                }
            }
        } else{
            return None
        }

    }
}

impl<'s> Parser<'s> {

    // Creates a parser out of a lexer.
    fn from_lexer(lexer: Lexer) -> Parser{
        let string = lexer.string.clone();
        let mut iter = lexer.peekable();
        Parser {
            string,
            iter,
            exhausted: false,
        }
    }

    // Consumes a host line, e.g. "Host myhost\n"
    fn consume_host(&mut self) -> Option<Result<Node<'s>, Error>> {
        debug!("Parser: Consuming host");
        let mut ret = Node(NodeType::Host, IndexedSlice::beginning(self.string));
        // We consume the first keyword word token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "Host" => {}
            _ => { panic!("Consume host called on wrong token") }
        }
        // We consume the value token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) => {
                ret.1 = ib;
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a nickname here".to_owned())))
            }
            Some(Err(e)) => {
                return Some(Err(e))
            },
            None =>{
                self.exhausted = true;
                return Some(Err(Error::Parser(
                    IndexedSlice::end(self.string).to_owned(),
                    "Expected a nickname here".to_owned())));
            }
        }
        // We consume extra tokens
        loop{
            match self.iter.peek(){
                Some(Ok(Token(TokenType::Word, ib))) => {
                    self.exhausted = true;
                    return Some(Err(Error::Parser(ib.to_owned(),
                                                  "The nickname must be a single word".to_owned())));
                }
                Some(Ok(Token(TokenType::NewLine, _)))=> {
                    self.iter.next();
                    return Some(Ok(ret));
                }
                Some(Ok(Token(TokenType::Tab, _))) => {
                    self.iter.next();
                }
                Some(Ok(Token(TokenType::Comment, ib))) => {
                    self.iter.next();
                    return Some(Ok(ret));
                }
                Some(Err(e)) => {return None},
                None => {
                    self.iter.next();
                    return Some(Ok(ret));
                }
            }
        }
    }

    // Consume a clause, e.g. "\tClauseKeyword ClauseValue\n"
    fn consume_clause(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming clause");
        // We consume tab token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Tab, _))) => {}
            _ => panic!("Consume host called on wrong token")
        }
        // We dispatch with next keyword
        match self.iter.peek(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "HostName" => {
                return self.consume_hostname_clause();
            }
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "User" => {
                return self.consume_user_clause();
            }
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "Port" => {
                return self.consume_port_clause();
            }
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "ProxyCommand" => {
                return self.consume_proxycommand_clause();
            }
            Some(Ok(Token(TokenType::Word, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Only HostName, User, Port and ProxyCommand \
                                              are supported.".to_owned())));
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a clause name here.".to_owned())));
            }
            None => {
                self.exhausted = true;
                return Some(Err(Error::Parser(
                    IndexedSlice::end(self.string).to_owned(),
                    "Expected a clause name here.".to_owned())));
            }
            Some(Err(e)) => return None
        }
    }

    // Consumes a hostname clause, e.g. "\tHostName locahost"
    fn consume_hostname_clause(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming hostname clause");
        let mut ret = Node(NodeType::HostNameClause, IndexedSlice::beginning(self.string));
        // We consume the keyword token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "HostName" => {}
            _ => panic!("Consume hostname clause called on wrong token")
        }
        // We consume value token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib)))=> {
                ret.1 = ib;
            }
            Some(Ok(Token(_, ib))) =>{
                ret.1 = ib;
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a word here.".to_owned())))
            }
            None => {
                self.exhausted = true;
                return Some(Err(Error::Parser(
                    IndexedSlice::end(self.string).to_owned(),
                    "Expected a word here.".to_owned())))
            }
            Some(Err(e)) =>{return Some(Err(e))},
        }
        // We consume until newline
        match self.iter.peek(){
            Some(Ok(Token(TokenType::NewLine, _))) | None => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(TokenType::Comment, _))) => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a newline here".to_owned())))
            }
            Some(Err(e)) => return None
        }
    }

    // Consumes a user clause, e.g. "\tUser me\n"
    fn consume_user_clause(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming user clause");
        let mut ret = Node(NodeType::UserClause, IndexedSlice::beginning(self.string));
        // We consume the keyword token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "User" => {}
            _ => panic!("Consume user clause called on wrong token")
        }
        // We consume value token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) => {
                ret.1 = ib;
            }
            Some(Ok(Token(_, ib))) =>{
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a word here.".to_owned())))
            }
            None => {
                self.exhausted = true;
                return Some(Err(Error::Parser(
                    IndexedSlice::end(self.string).to_owned(),
                    "Expected a word here.".to_owned())))
            }
            Some(Err(e)) => return Some(Err(e)),
        }
        // We consume until newline
        match self.iter.peek(){
            Some(Ok(Token(TokenType::NewLine, _))) | None => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(TokenType::Comment, _))) => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a newline here.".to_owned())))
            }
            Some(Err(e)) => return None
        }
    }

    // Consumes a port clause, e.g. "\tPort 22\n"
    fn consume_port_clause(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming port clause");
        let mut ret = Node(NodeType::PortClause, IndexedSlice::beginning(self.string));
        // We consume the keyword token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "Port" => {}
            _ => panic!("Consume port clause called on wrong token")
        }
        // We consume the value token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str().parse::<u32>().is_ok() => {
                ret.1 = ib;
            }
            Some(Ok(Token(_, ib))) =>{
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a port number here".to_owned())))
            }
            Some(Err(e)) => return Some(Err(e)),
            None => {
                self.exhausted = true;
                return Some(Err(Error::Parser(IndexedSlice::end(self.string).to_owned(),
                                              "Expected a port number here".to_owned())))
            }
        }
        // We consume until newline
        match self.iter.peek(){
            Some(Ok(Token(TokenType::NewLine, _))) | None => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(TokenType::Comment, _))) => {
                self.iter.next();
                return Some(Ok(ret));
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a newline here".to_owned())))
            }
            Some(Err(e)) => {return None}
        }
    }

    // Cosumes a proxycommand clause e.g. "\t\ProxyCommand ssh...\n"
    fn consume_proxycommand_clause(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming proxycommand clause");
        let mut ret = Node(NodeType::ProxyCommandClause, IndexedSlice(self.string, 0, 0));
        // We consume the keyword token
        match self.iter.next(){
            Some(Ok(Token(TokenType::Word, ib))) if ib.as_str() == "ProxyCommand" => {
                ret.1 = ib;
            }
            _ => panic!("Consume proxycommand clause called on wrong token")
        }
        // We consume the first proxycommand word
        match self.iter.peek() {
            Some(Ok(Token(TokenType::Word, ib))) => {
                (ret.1).1 = ib.1;
                self.iter.next();
            }
            Some(Ok(Token(_, ib))) => {
                self.exhausted = true;
                return Some(Err(Error::Parser(ib.to_owned(),
                                              "Expected a command here".to_owned())))
            }
            None =>{
                self.exhausted = true;
                return Some(Err(Error::Parser(IndexedSlice::end(self.string).to_owned(),
                                              "Expected a command here".to_owned())));
            }
            Some(Err(e)) => return None
        }
        // We consume the upcoming proxycommand words
        loop {
            match self.iter.peek() {
                Some(Ok(Token(TokenType::Word, ib))) => {
                    (ret.1).2 = ib.2;
                    self.iter.next();
                }
                Some(Ok(Token(TokenType::NewLine, _))) | None => {
                    self.iter.next();
                    return Some(Ok(ret));
                }
                Some(Ok(Token(TokenType::Comment, _))) => {
                    self.iter.next();
                    return Some(Ok(ret));
                }
                Some(Ok(Token(_, ib))) => {
                    self.exhausted = true;
                    return Some(Err(Error::Parser(ib.to_owned(),
                                                  "Expected word, comment or newline here".to_owned())))
                }
                Some(Err(e)) => return None
            }
        }
    }

    // Consumes unnecessary newlines
    fn consume_newline(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming newline");
        match self.iter.next() {
            Some(Ok(Token(TokenType::NewLine, _))) => {return None}
            _ => panic!("Consume newline called on wrong character")
        }
    }

    // Consumes unnecessary comments
    fn consume_comment(&mut self) -> Option<Result<Node<'s>, Error>>{
        debug!("Parser: Consuming comment");
        match self.iter.next(){
            Some(Ok(Token(TokenType::Comment, _))) =>{return None},
            _ => panic!("Consume comment called on wrong token")
        }
    }

}

////////////////////////////////////////////////////////////////////////////////////// CONFIG READER
/// This structure is an iterator over SshProfiles defined in a string following the openssh format.
/// The following clauses are supported:
/// + HostName
/// + User
/// + Port
/// + ProxyCommand
///
/// The string may contain comments.
pub struct ConfigReader<'s>{
    iter: Peekable<Parser<'s>>,
}

impl<'s> Iterator for ConfigReader<'s>{
    type Item = Result<SshProfile, Error>;

    fn next(&mut self) -> Option<Result<SshProfile, Error>>{
       loop{
           trace!("Reader: Consuming a new node: {:?}", self.iter.peek());
           let ret = match self.iter.peek() {
               Some(Ok(Node(NodeType::Host, _))) => self._consume_host(),
               Some(Ok(Node(_, ib))) => {
                   return Some(Err(Error::Reader(ib.to_owned(),
                                                 "Expected Host".to_owned())));
               }
               Some(Err(e)) => return Some(Err(self.iter.next().unwrap().unwrap_err())),
               None => return None
           };
           if let Some(n) = ret{
               trace!("Reader: Profile emitted: {:?}", n);
               return Some(n)
           }
       }
    }
}

impl<'s> ConfigReader<'s>{

    /// Instantiates a new configuration reader out of a string.
    pub fn from_str(string: &'s str) -> ConfigReader<'s>{
        let mut lexer = Lexer::from(string);
        let mut parser = Parser::from_lexer(lexer).peekable();
        return ConfigReader{iter: parser};
    }

    // Consumes a host, e.g. a complete host declaration with starting line and clauses.
    fn _consume_host(&mut self) -> Option<Result<SshProfile, Error>>{
        debug!("Reader: Consuming host");
        // We consume the name
        let mut profile = match self.iter.next(){
            Some(Ok(Node(NodeType::Host, ib))) => {SshProfile::from(ib.as_str().to_owned())}
            _ => panic!("Consume host called on wrong node")
        };
        // We consume clause until new host
        loop {
            match self.iter.peek() {
                Some(Ok(Node(NodeType::HostNameClause, ib))) => {
                    profile = profile.set_hostname(ib.as_str().to_owned());
                    self.iter.next();
                }
                Some(Ok(Node(NodeType::UserClause, ib)))  => {
                    profile = profile.set_user(ib.as_str().to_owned());
                    self.iter.next();
                }
                Some(Ok(Node(NodeType::PortClause, ib))) => {
                    profile = profile.set_port(ib.as_str().parse::<usize>().unwrap());
                    self.iter.next();
                }
                Some(Ok(Node(NodeType::ProxyCommandClause, ib))) => {
                    profile = profile.set_proxycommand(ib.as_str().to_owned());
                    self.iter.next();
                }
                Some(Ok(Node(NodeType::Host, _))) | None=>{
                    return Some(Ok(profile));
                }
                Some(Err(_)) => return None,
            }
        }
    }
}

/// This convenient function allows to parse a config file and retrieve a profile if it exists.
pub fn get_profile(config_path: &PathBuf, name: &str) -> Result<SshProfile, Error>{
    let mut profiles = File::open(config_path)
        .map_err(|_| Error::GettingProfile(format!("Failed to open the file {}",
                                                   config_path.to_str().unwrap())))?;
    let mut profiles_string = String::new();
    profiles.read_to_string(&mut profiles_string)
        .map_err(|_| Error::GettingProfile(format!("Failed to read file {}",
                                                   config_path.to_str().unwrap())))?;
    let initial_err = Err(Error::GettingProfile(
        format!("There is no {} profile in {}",
                name,
                config_path.to_str().unwrap())));
    let mut profile = ConfigReader::from_str(&profiles_string)
        .fold( initial_err,|res, r| {
                  if r.is_err(){
                      r
                  } else if r.as_ref().unwrap().name == name{
                      Ok(r.unwrap())
                  } else {
                      res
                  }
              }
        )?;
    return Ok(profile.complete());
}



#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_lexer() {
        //setup();
        let a = "\tHost \t plafrim   #kalbfezjk \t jjja -p  \n\n\n   ".to_owned();
        let mut lexer = Lexer::from(&a);
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::Tab);
        assert_eq!(n.1.as_str(), "\t");
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::Word);
        assert_eq!(n.1.as_str(), "Host");
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::Tab);
        assert_eq!(n.1.as_str(), "\t");
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::Word);
        assert_eq!(n.1.as_str(), "plafrim");
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::Comment);
        assert_eq!(n.1.as_str(), "#kalbfezjk \t jjja -p  ");
        let n = lexer.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, TokenType::NewLine);
        assert_eq!(n.1.as_str(), "\n");
        assert!(lexer.next().is_none());
    }

    #[test]
    fn test_lexer_error(){
        let a = "Höst pla\n\tHostName plafrim".to_owned();
        let mut lexer = Lexer::from(&a);
        let n = lexer.next().unwrap().unwrap_err();
        println!("n: {}", n);
    }


    #[test]
    fn test_parser(){
        let a = "# my configurations\n\
        Host localhost #Kikou \n\
        \tHostName localhost # comments \t \n\
        # Some comments\n\n\
        \tUser apere #some comments \n\
        \tPort 222 #not classic\n\
        \tProxyCommand ssh -A -l apere localhost -W localhost:22 # some comments".to_owned();
        let mut lexer = Lexer::from(&a);
        let mut parser = Parser::from_lexer(lexer);
        let n = parser.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, NodeType::Host);
        assert_eq!(n.1.as_str(), "localhost");
        let n = parser.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, NodeType::HostNameClause);
        assert_eq!(n.1.as_str(), "localhost");
        let n = parser.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, NodeType::UserClause);
        assert_eq!(n.1.as_str(), "apere");
        let n = parser.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, NodeType::PortClause);
        assert_eq!(n.1.as_str(), "222");
        let n = parser.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.0, NodeType::ProxyCommandClause);
        assert_eq!(n.1.as_str(), "ssh -A -l apere localhost -W localhost:22");
        assert!(parser.next().is_none());
    }

    #[test]
    fn test_parser_error(){
        let a = "\tPort 222a".to_owned();
        let mut lexer = Lexer::from(&a);
        let mut parser = Parser::from_lexer(lexer);
        let n = parser.next().unwrap().unwrap_err();
        println!("n: {}", n);
    }

    #[test]
    fn test_config_reader(){
        let a =
            "# My configurations\n\
            \n
            Host test # first profile for testing purpose\n\
            \tHostName localhost\n\
            \tPort 222 # not the usual port \n\
            \n
            \tUser apere\n\
            \tProxyCommand ssh -a -l blahblah bhal \n\
            \n\
            # The second profile\n\
            Host test2\n\
            \tUser apere\n\
            \n\
            \n\
            #The last one which is wrong\n\
            Host blahblah\n
            \tPort 222a\n".to_owned();
        let mut reader = ConfigReader::from_str(&a);
        let n = reader.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.name, "test");
        assert_eq!(n.hostname.as_ref().unwrap(), "localhost");
        assert_eq!(n.user.as_ref().unwrap(), "apere");
        assert_eq!(*n.port.as_ref().unwrap(), 222 as usize);
        assert_eq!(n.proxycommand.as_ref().unwrap(), "ssh -a -l blahblah bhal");
        let n = reader.next().unwrap().unwrap();
        println!("n: {:?}", n);
        assert_eq!(n.name, "test2");
        assert!(n.hostname.is_none());
        assert_eq!(n.user.as_ref().unwrap(), "apere");
        assert!(n.port.is_none());
        assert!(n.proxycommand.is_none());
        let n = reader.next().unwrap().unwrap_err();
        println!("error: {}", n);
        assert!(reader.next().is_none());
    }


}