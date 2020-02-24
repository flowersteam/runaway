//! lib/commons.rs
//!
//! This module contains some type definitions common to the whole project.


//------------------------------------------------------------------------------------------ IMPORTS


use std::error;
use std::fmt;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use futures::channel::mpsc::UnboundedSender;
use chrono::prelude::*;
use std::ops::{Deref, DerefMut};
use std::process::{Output};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::hash::Hash;
use std::os::unix::process::ExitStatusExt;
use std::ffi::OsStr;
use tracing::{self, trace, error, instrument};


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    FuturePoll(String),
    Operation(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::FuturePoll(s) => write!(f, "An error occurred while polling a future:\n{}", s),
            Error::Operation(s) => write!(
                f,
                "An error occurred while performing the operation:\n{}",
                s
            ),
        }
    }
}


//------------------------------------------------------------------------------------------ DROPPER


/// Most of this library code relies on a structure that handles messages from the receiving end of
/// a channel in a separate thread. Handles to the transmitting end of the channel are freely cloned
/// in the rest of the program with the insurance of the operations being synchronized by the
/// channel. When the last handle is dropped, it is important to close the channel and wait for the
/// thread to cleanup and join.
///
/// This structure allows to do just that. It holds a reference to a closure, and at drop time,
/// checks whether it is the last handle or not. If it is, it waits for the handle to join before
/// dropping. This dropper should be included as the __last__ field of the handle structure, to
/// ensure that it is dropped last (see https://github.com/rust-lang/rfcs/blob/246ff86b320a72f98ed2df92805e8e3d48b402d6/text/1857-stabilize-drop-order.md)
///
/// A `Dropper` could be either `Strong` or `Weak`. If Strong, the dropper is accounted for in the
/// dropping. In practice, a handle with a `Strong` dropper is guaranteed to access the resource (if
/// not crashed). On the other side, if `Weak`, the dropper is not accounted for in the dropping.
/// This means that a handle with a weak dropper is not guaranteed to access the resource.
///
/// In practice, `Weak` droppers are used for references that do not need to be waited for, and
/// could ruin the dropping strategy. Two cases can arise:
///     + A self referential handle as seen in the `application` module
///     + A handle that is never dropped during program execution, as seen in ctrl-c handling.
#[derive(Clone)]
pub enum Dropper{
    Strong(Arc<Mutex<Option<Box<dyn FnOnce()+Send+'static>>>>, String),
    Weak,
}
impl Dropper {
    /// Creates a `Dropper` instance from the closure to execute at drop-time.
    pub fn from_closure(other: Box<dyn FnOnce()+Send+'static>, name: String) -> Dropper {
        Dropper::Strong(Arc::new(Mutex::new(Some(other))), name)
    }

    /// Downgrade a `Strong` dropper to a `Weak` dropper.
    pub fn downgrade(&mut self){
        if let Dropper::Strong(_, _) = self{
            *self = Dropper::Weak;
        }
    }
}

impl Drop for Dropper{
    #[instrument(name="Dropper::drop", skip(self))]
    fn drop(&mut self) {
        match &self{
            Dropper::Weak => {},
            Dropper::Strong(r, _) if Arc::strong_count(&r) == 1 => {
                trace!("Counter at 1. Joining...");
                (r.lock().unwrap().take().unwrap())();
            }
            Dropper::Strong(r, _) =>{
                trace!("Counter at {}. Dropping...", Arc::strong_count(&r))
            }
        }
    }
}


//---------------------------------------------------------------------------------------- EXPIRABLE


/// A smart pointer containing an expirable value, a value that should not be used after a given
/// time. Used to implement the handle acquisition in `hosts`.
#[derive(Clone)]
pub struct Expire<T:Clone+Send+Sync>{
    inner: T,
    expiration: DateTime<Utc>,
}

impl<T> Expire<T> where T:Clone+Send+Sync{
    /// Creates a new Expire pointer.
    pub fn new(inner: T, expiration: DateTime<Utc>) -> Expire<T>{
        Expire{inner, expiration}
    }

    /// Checks if the value has expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expiration
    }
}

impl<T> Deref for Expire<T> where T:Clone+Send+Sync{
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for Expire<T> where T:Clone+Send+Sync{
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> Debug for Expire<T> where T:Clone+Send+Sync+Debug{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "Expire[@{}]<{:?}>", self.expiration.to_rfc3339(), self.inner)
    }
}


//--------------------------------------------------------------------------------- DROPBACK POINTER


/// A smart pointer representing a value that should be sent back through a channel at drop-time.
/// Take good care about consuming every unnecessary DropBack. Indeed, if the value is not consumed,
/// then, a self-referential sending loop can occur, that would prevent the whole channel to be
/// closed.
#[derive(Clone)]
pub struct DropBack<T:Clone+Send+Sync>{
    inner: Option<T>,
    channel: UnboundedSender<DropBack<T>>,
}

impl<T> DropBack<T> where T:Clone+Send+Sync{
    /// Creates a new DropBack pointer.
    pub fn new(inner: T, channel: UnboundedSender<DropBack<T>>) -> DropBack<T>{
        DropBack{
            inner: Some(inner),
            channel
        }
    }

    /// Consumes the DropBack pointer, by taking the value inside. Nothing will be sent back after
    /// that.
    pub fn consume(mut self) -> T{
        self.inner.take().unwrap()
    }
}

impl<T> Drop for DropBack<T> where T:Clone+Send+Sync{
    #[instrument(name="DropBack::drop", skip(self))]
    fn drop(&mut self) {
        trace!("Dropping.");
        if self.inner.is_some(){
            trace!("Still some data. Sending back.");
            if self.channel.unbounded_send(self.clone()).is_err(){
                error!("Failed to send the value back.")
            };
        } else {
            trace!("No more data. Disconnecting");
        }
    }
}

impl<T> Deref for DropBack<T> where T:Clone+Send+Sync{
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.as_ref().unwrap()
    }
}

impl<T> DerefMut for DropBack<T> where T:Clone+Send+Sync{
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap()
    }
}

impl<T> Debug for DropBack<T> where T:Clone+Send+Sync+Debug{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "DropBack<{:?}>", self.inner)
    }
}


//-------------------------------------------------------------------------------------------- TRAIT


/// A trait allowing to turn a structure into a resultdepending on the exit code.
pub trait AsResult {
    /// Consumes the object to make a result out of it.
    fn result(self) -> Result<String, String>;
}

/// Implementation for `Ouput`. The exit code is used as a marker for failure.
impl AsResult for Output {
    fn result(self) -> Result<String, String> {
        if self.status.success() {
            Ok(format!(
                "Successful execution\nstdout: {}\nstderr: {}",
                String::from_utf8(self.stdout).unwrap(),
                String::from_utf8(self.stderr).unwrap()
            ))
        } else {
            Err(format!(
                "Failed execution({})\nstdout: {}\nstderr: {}",
                self.status.code().unwrap_or(911),
                String::from_utf8(self.stdout).unwrap(),
                String::from_utf8(self.stderr).unwrap()
            ))
        }
    }
}


//-------------------------------------------------------------------------------------------- TYPES

/// Represents a command
#[derive(Clone, Derivative)]
#[derivative(Debug="transparent")]
pub struct RawCommand<S: AsRef<str>>(pub S);
impl<S: AsRef<str>> From<S> for RawCommand<S>{
    fn from(s: S) -> Self{
        RawCommand(s)
    }
}

/// Represents an environment variable key
#[derive(PartialEq, Eq, Hash, Clone, Derivative)]
#[derivative(Debug="transparent")]
pub struct EnvironmentKey<S: AsRef<str>>(pub S);
impl<S: AsRef<str>> AsRef<OsStr> for EnvironmentKey<S>{
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref().as_ref()
    }
}

/// Represents an environment variable value
#[derive(Clone, Derivative, PartialEq, Eq)]
#[derivative(Debug="transparent")]
pub struct EnvironmentValue<S: AsRef<str>>(pub S);
impl<S: AsRef<str>> AsRef<OsStr> for EnvironmentValue<S>{
    fn as_ref(&self) -> &OsStr {
        self.0.as_ref().as_ref()
    }
}

/// Represents a set of environment variables
pub type EnvironmentStore = HashMap<EnvironmentKey<String>, EnvironmentValue<String>>;
pub fn substitute_environment(store: &EnvironmentStore, string: &str) -> String{
    store.iter()
        .fold(string.to_owned(), |acc, (EnvironmentKey(key), EnvironmentValue(val))| {
            acc.replace(&format!("${}", key), val)
        })
}
pub fn push_env<K: AsRef<str>, V: AsRef<str>>(store: &mut EnvironmentStore, key: K, value: V){
    store.insert(EnvironmentKey(key.as_ref().to_owned()), EnvironmentValue(value.as_ref().to_owned()));
}
pub fn format_env(store: &EnvironmentStore) -> String {
    store.iter()
        .fold(String::new(), |mut acc, (EnvironmentKey(k), EnvironmentValue(v))| {
            acc.push_str(&format!("{} = {}\n", k, v));
            acc
        })
}

/// Represents a Current Working Directory
#[derive(Clone, Derivative)]
#[derivative(Debug="transparent")]
pub struct Cwd<P: AsRef<Path>>(pub P);

/// Represents a classic terminal context made out of a cwd and some enrironment variables
#[derive(Debug, Clone)]
pub struct TerminalContext<P:AsRef<Path>>{
    pub cwd: Cwd<P>,
    pub envs: EnvironmentStore
}
impl Default for TerminalContext<PathBuf>{
     fn default() -> Self{
        TerminalContext{
            cwd: Cwd(PathBuf::from("/")),
            envs: EnvironmentStore::new(),
        }
     }
}

/// Representing an output in a simpler form
#[derive(Debug)]
pub struct OutputBuf{
    pub stdout: String,
    pub stderr: String,
    pub ecode: i32,
}
impl OutputBuf{
    pub fn success(&self) -> bool{
        self.ecode == 0
    }
}
impl From<Output> for OutputBuf{
    fn from(other: Output) -> OutputBuf{
        let stdout = String::from_utf8(other.stdout.clone()).unwrap();
        let stderr = String::from_utf8(other.stderr.clone()).unwrap();
        let ecode = other.status.code().unwrap_or_else(|| other.status.signal().unwrap() );
        OutputBuf{stdout, stderr, ecode}
    }
}
impl AsResult for OutputBuf {
    fn result(self) -> Result<String, String> {
        if self.ecode ==0 {
            Ok(format!(
                "Successful execution\nstdout: {}\nstderr: {}", self.stdout, self.stderr
            ))
        } else {
            Err(format!(
                "Failed execution({})\nstdout: {}\nstderr: {}", self.ecode, self.stdout, self.stderr
            ))
        }
    }
}
