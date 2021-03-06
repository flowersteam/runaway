//! This module contains code that manages host allocations. The resulting tool is the
//! HostResource, which given an host configuration provide asynchronous nodes allocation. Put
//! differently, it allows to await a node to be available for computation, given the restrictions
//! of the configuration. The allocation are automatically started and revoked.

//------------------------------------------------------------------------------------------ IMPORTS

use super::NodeHandle;
use crate::commons::{DropBack, Expire};
use chrono::{DateTime, Utc};
use futures::channel::mpsc::unbounded;
use futures::future;
use futures::lock::Mutex;
use futures::sink::SinkExt;
use futures::stream::{self, StreamExt};
use futures::Future;
use futures::Stream;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::{error, fmt};
use tracing::{self, error, instrument, trace, trace_span};
use tracing_futures::Instrument;

//------------------------------------------------------------------------------------------- ERRORS

#[derive(Debug, Clone)]
pub enum Error {
    New,
    Empty,
    Closed,
    UnexpectedMessage(String),
    Unhandled(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::New => write!(f, "Provider is new !"),
            Error::Unhandled(ref s) => write!(f, "Unhandled error occurred: \n{}", s),
            Error::Empty => write!(f, "Provider is empty"),
            Error::Closed => write!(f, "Provider was closed"),
            Error::UnexpectedMessage(ref s) => {
                write!(f, "Provider encountered an unexpected message.\n{}", s)
            }
        }
    }
}

//-------------------------------------------------------------------------------------------- QUEUE

// Enumeration for the messages in the channel.
#[derive(Clone, Debug)]
enum ProviderMsg {
    /// Signals that the Queue is new, and should be initialized.
    New,
    /// Signals that the Queue is empty, and no new handle should be expected
    Empty,
    /// A handle to a node
    Handle(DropBack<Expire<NodeHandle>>),
    /// Handles that should be consumed without inquiry
    ToConsume(DropBack<Expire<NodeHandle>>),
    /// Signals that the queue has been shut
    Closed,
}

/// Central to the host structure, this structure allows to manage the lifecycle of node handles.
/// In essence, it is simply an awaitable handles provider: You can feed handles in via `push`, and
/// pull handles from with `pull`, but there is a special twist to that. The handles given by the
/// `pull` method will return the node wrapped in a `DropBack<Expire<_>>` smart pointer.
/// The `DropBack<_>` part will allow the node handle to be sent back to the provider on drop. The
/// `Expire` part allows to add an expiration date to a node handle.
///
/// Then it goes like this: the provider will be given some handles via the `push` function, along
/// with an expiration date. While they do not reach their expiration date, those handles will be
/// given to the user in a awaitable fashion. Every handles will be given to the first users, and then
/// the next one will have to wait for one of those users to drop the handle. When an handle is dropped,
/// it is checked by the provider for expiration. If the handle has expired, it is dropped for good,
/// and if not, it is given to the next user.
///
/// In practice, the implementation revolves around an inner `Stream` implementor which will be mutated
/// along the way. to provide the necessary messages.
#[derive(Clone)]
pub struct Provider(Arc<Mutex<Pin<Box<dyn stream::Stream<Item = ProviderMsg> + Send>>>>);

impl Provider {
    /// Creates a new provider
    #[instrument(name = "Provider::new")]
    pub fn new() -> Provider {
        trace!("Creating new provider");
        // At first, we want the queue to output Error::Empty to every pull, to signal to the user
        // that some handles should be pushed in. So we set the inner stream to repeat this message.
        Provider(Arc::new(Mutex::new(Box::pin(stream::repeat(
            ProviderMsg::New,
        )))))
    }

    /// Gives a node if any is available.
    pub fn pull(
        &mut self,
    ) -> impl Future<Output = Result<DropBack<Expire<NodeHandle>>, Error>> + '_ {
        let inner = self.0.clone();
        async move {
            loop {
                // We retrieve the next message on the inner stream
                let next = {
                    let mut chan = inner.lock().await;
                    chan.next().await
                };
                match next {
                    // The provider is new. We forward that to the user to push its first nodes
                    Some(ProviderMsg::New) => {
                        trace!("Encountered a new message. Provider must be filled.");
                        return Err(Error::New);
                    }
                    // There is no messages left, and we forward that to the user as an error.
                    Some(ProviderMsg::Empty) => {
                        trace!("Encountered an empty message. Provider must be filled");
                        return Err(Error::Empty);
                    }
                    // A handle was received on the stream. We check if it has not expired yet.
                    Some(ProviderMsg::Handle(n)) => {
                        trace!("Encountered a handle message");
                        if n.is_expired() {
                            trace!("Handle expired. Consuming...");
                            n.consume();
                        } else {
                            trace!("Returning handle");
                            return Ok(n);
                        }
                    }
                    // This means that the provider is getting closed. We consume the node and
                    // forward that to the user.
                    Some(ProviderMsg::ToConsume(n)) => {
                        trace!("Encountered a to-consume message. Consuming");
                        n.consume();
                        return Err(Error::Closed);
                    }
                    // The provider is closed.
                    Some(ProviderMsg::Closed) => {
                        trace!("Encountered a closed message.");
                        return Err(Error::Closed);
                    }
                    // Something went very wrong
                    None => {
                        error!("None encountered in provider");
                        return Err(Error::Unhandled("Channel closed ...".to_string()));
                    }
                }
            }
        }
        .instrument(trace_span!("Provider::pull"))
    }

    /// Pushes a set of new handles into the queue
    pub fn push(
        &mut self,
        handles: Vec<NodeHandle>,
        expiration: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), Error>> + '_ {
        let inner = self.0.clone();
        async move {
            // We check that the queue is in the expected state, i.e. it should output an empty message.
            let next = {
                let mut chan = inner.lock().await;
                chan.next().await
            };
            match next {
                Some(ProviderMsg::Empty) | Some(ProviderMsg::New) => {}
                a => {
                    error!("Encountered in wrong state while pushing");
                    return Err(Error::UnexpectedMessage(format!("{:?}", a)));
                }
            }
            // To allow the nodes to be sent back to the provider at drop time, we use a channel as
            // queue for the inner stream.
            let (mut tx, rx) = unbounded();
            // We map the node handles to the expected type
            let handles = handles
                .into_iter()
                .map(|h| DropBack::new(Expire::new(h, expiration), tx.clone()))
                .collect::<Vec<_>>();
            let mut handles_stream = stream::iter(handles);
            // and we send all the handles in the channel. We put them in the queue if you see the
            // channel as an awaitable fifo queue.
            trace!("Pushing handles to channel");
            tx.send_all(&mut handles_stream)
                .await
                .map_err(|e| Error::Unhandled(format!("Failed to send nodes: {}", e)))?;
            // We replace the inner stream by this channel, followed by a stream repeating that the
            // channel is empty. If handles are still undropped somewhere, there are still some
            // channel receiving end preventing the receiver to be closed.
            // When all handles will be dropped, the receiver will be closed, and the stream will
            // start to repeat empty messages, signalling to the user that new nodes must be pushed
            // in.
            let new_stream = Box::pin(
                rx.map(ProviderMsg::Handle)
                    .chain(stream::repeat(ProviderMsg::Empty)),
            );
            {
                trace!("Changing stream");
                *self.0.lock().await = new_stream;
            }
            Ok(())
        }
        .instrument(trace_span!("Provider::push"))
    }

    /// Closes the provider, preventing it from issuing any more handles.
    pub fn shutdown(&mut self) -> impl Future<Output = ()> + '_ {
        let inner = self.0.clone();
        async move {
            // We capture the inner stream which may yield some more handles.
            trace!("Getting current stream");
            let mut remaining: Pin<Box<(dyn Stream<Item = ProviderMsg> + Send + 'static)>> =
                Box::pin(stream::once(future::ready(ProviderMsg::Closed)));
            std::mem::swap(&mut *inner.lock().await, &mut remaining);
            // We replace the inner stream by a new one which transforms Handle messages to ToConsume
            // messages (allowing handles to be consumed), and Empty messages to Closing messages.
            // This way, every new issued message will signal to users that the provider is closed.
            trace!("Mutating stream");
            let remaining = Box::pin(remaining.map(|m| match m {
                ProviderMsg::Handle(h) => ProviderMsg::ToConsume(h),
                ProviderMsg::Empty | ProviderMsg::New => ProviderMsg::Closed,
                m => m,
            }));
            {
                *self.0.lock().await = remaining;
            }
        }
        .instrument(trace_span!("Provider::shutdown"))
    }

    /// Collects handles to be consumed and return afterward. Should be called after shutdown was
    /// called. Would panic otherwise.
    pub fn collect(&mut self) -> impl Future<Output = ()> + '_ {
        let inner = self.0.clone();
        async move {
            let span = trace_span!("Provider::collect");
            let _guard = span.enter();
            let mut chan = inner.lock().await;
            trace!("Collecting remaining handles");
            loop {
                // We consume the ToConsume messages and return when Closed is encountered.
                match chan.next().await {
                    Some(ProviderMsg::ToConsume(h)) => {
                        trace!("Encountered a to-consume message. Consuming");
                        h.consume();
                    }
                    Some(ProviderMsg::Closed) => {
                        trace!("All handles consumed");
                        break;
                    }
                    m => panic!("Wrong message encountered when collecting handles: {:?}", m),
                }
            }
        }
        .instrument(trace_span!("Provider::collect"))
    }
}
