//! liborchestra/primitives.rs
//! Author: Alexandre Péré
//! 
//! This module contains some primitives structures that are used througout the project. 


//------------------------------------------------------------------------------------------ IMPORTS


use std::error;
use std::fmt;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, Weak};
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


/// Most of this library code relies on a structure that handles messages from the receiving end of 
/// a channel in a separate thread. Handles to the transmitting end of the channel are freely cloned
/// in the rest of the program with the insurance of the operations being synchronized by the 
/// channel. When the last handle is dropped, it is important to clsoe the channel and wait for the 
/// thread to cleanup and join. 
/// 
/// This structure allows to do just that. It holds a reference to a closure, and at drop time, 
/// checks whether it is the last handle or not. If it is, it waits for the handle to join before 
/// dropping. This dropper should be included as the __last__ field of the handles structure, to 
/// ensure that it is dropped last (see https://github.com/rust-lang/rfcs/blob/246ff86b320a72f98ed2df92805e8e3d48b402d6/text/1857-stabilize-drop-order.md)
/// 
/// A Dropper could be either `Strong` or `Weak`. If Strong, the dropper is accounted for in the 
/// dropping. In practice, a handle with a Strong dropper is guaranteed to access the resource (if 
/// not crashed). On the other side, if Weak, the dropper is not accounted for in the dropping. This 
/// means that a handle with a weak dropper is not guaranteed to access the resource. 
/// 
/// In practice, Weak droppers are used for reference that do not need to be waited for, and could 
/// ruin the dropping strategy. Two cases can arise:
///     + A self referential handle as seen in the application module
///     + A handle that is never dropped during program execution, as seen in ctrl-c handling. 
#[derive(Clone)]
pub enum Dropper{
    Strong(Arc<Mutex<Option<Box<dyn FnOnce()+Send+'static>>>>, String),
    Weak,
}
impl Dropper {
    pub fn from_closure(other: Box<dyn FnOnce()+Send+'static>, name: String) -> Dropper {
        return Dropper::Strong(Arc::new(Mutex::new(Some(other))), name);
    }

    pub fn downgrade(&mut self){
        if let Dropper::Strong(r, s) = self{
            *self = Dropper::Weak;
        }
    }
}

impl Drop for Dropper{
    fn drop(&mut self) {
        match &self{
            Dropper::Weak => {},
            Dropper::Strong(r, s) if Arc::strong_count(&r) == 1 => {
                trace!("Dropper<{}>: Counter at 1. Joining...", s);
                (r.lock().unwrap().take().unwrap())();
            }
            Dropper::Strong(r, s) =>{
                trace!("Dropper<{}>: Counter at {}. Dropping...", s, Arc::strong_count(&r))
            }
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
            if self.channel.unbounded_send(self.clone()).is_err(){
                error!("DropBack: Failed to send the value back.")
            };
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