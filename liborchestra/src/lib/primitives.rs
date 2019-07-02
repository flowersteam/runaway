//! liborchestra/primitives.rs
//! Author: Alexandre Péré
//! 
//! This module contains some primitives structures that are used througout the project. 


//------------------------------------------------------------------------------------------ IMPORTS


use std::error;
use std::fmt;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use futures::channel::mpsc::UnboundedSender;
use chrono::prelude::*;
use std::ops::Deref;
use std::process::{Output};


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


/// A helper structure that allows to wait for threads to exit when dropping. To use it, add it as 
/// the last field of the structure so that the dropping works. 
/// See https://github.com/rust-lang/rfcs/blob/246ff86b320a72f98ed2df92805e8e3d48b402d6/text/1857-stabilize-drop-order.md .
#[derive(Clone)]
pub struct Dropper<M>(Arc<Mutex<Option<JoinHandle<M>>>>, String);
impl<M> Dropper<M> {
    pub fn from_handle(other: JoinHandle<M>, name: String) -> Dropper<M> {
        return Dropper(Arc::new(Mutex::new(Some(other))), name);
    }

    pub fn strong_count(&self) -> usize{
        return Arc::strong_count(&self.0);
    }
}
impl<M> Drop for Dropper<M> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            trace!("Dropper<{}>: Counter at 1. Joining...", self.1);
            self.0.lock().unwrap().take().unwrap().join().unwrap();
        } else{
            trace!("Dropper<{}>: Counter at {}. Dropping...",self.1, Arc::strong_count(&self.0))
        }
    }
}


//---------------------------------------------------------------------------------------- EXPIRABLE


/// A smart pointer representing an expirable value, a value that should not be used after a given 
/// time.
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

impl<T> Debug for Expire<T> where T:Clone+Send+Sync+Debug{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "Expire[@{}]<{:?}>", self.expiration.to_rfc3339(), self.inner)
    }
}


//--------------------------------------------------------------------------------- DROPBACK POINTER


/// A smart pointer representing a value that should be sent back at drop-time, through a channel. 
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
    fn drop(&mut self) {
        trace!("DropBack: Dropping.");
        if self.inner.is_some(){
            trace!("DropBack: Still some data. Sending back.");
            self.channel.unbounded_send(self.clone()).unwrap();
        } else {
            trace!("DropBack: No more data. Disconnecting");
        }
    }
}

impl<T> Deref for DropBack<T> where T:Clone+Send+Sync{
    type Target = T;

    fn deref(&self) -> &T {
        self.inner.as_ref().unwrap()
    }
}

impl<T> Debug for DropBack<T> where T:Clone+Send+Sync+Debug{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "DropBack<{:?}>", self.inner)
    }
}


//-------------------------------------------------------------------------------------------- TRAIT


/// A trait allowing to turn a structure into a result.
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