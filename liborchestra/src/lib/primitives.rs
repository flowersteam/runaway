// liborchestra/primitives.rs
// Author: Alexandre Péré
/// This module contains primitives used to implement futures in the whole library. Indeed, in
/// Orchestra, there exist resources that must be concurrently accessed, but does not rely on io
/// bounds, as is the case with most Tokio applications. For this reason, we have to implement the
/// concurrent handling of several operations on a shared resource by ourselves. Our design is the
/// following:
/// + Futures themselves are only responsible for sending _Operations_ to an asynchronous _Handler_.
/// + A Handler holds a resource and is the executor of the associated operations. It lives in a
///   separate thread and receives operations via a channel. It loops through the operations and
///   advance each of those, in turns.
/// + Operations are Stateful objects that represent the progress of a resource operation toward
///   completion. They implement the HandledBy<Resource> trait, which describes how the operation
///   will use the resource to progress. For this reason, operations are sent to the handler as mere
///   Trait objects.
///
/// To see how everything fits, check the test.
//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use crate::stateful::{Stateful, State, TransitTo};
use crossbeam::channel::{unbounded, Sender, Receiver, TryRecvError};
use std::marker::PhantomData;
use std::rc::Rc;
use std::cell::RefCell;
use std::fmt::{Debug};
use std::fmt;
use std::future::Future;
use std::task::Poll;
use std::pin::Pin;
use std::task::Waker;
use std::error;
use core::borrow::BorrowMut;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
pub enum Error {
    FuturePoll(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::FuturePoll(s) =>
                write!(f, "An error occurred while polling a future: \n{}", s),
        }
    }
}

///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS
// Operations are stateful trait objects that follows the lifecycle:
// Starting(A) -> Progressing(B) --> Finished(C)
//                       ^-------'
// It is best to have the C type being a result.

/// An operation is a Trait object (via its state), which is marked by an operation type M. This
/// marker type allows to differentiate between the different operations, when creating aliases.
pub struct Operation<M>{
    state: Stateful,
    sender: Sender<Operation<M>>,
    waker: Option<Waker>,
    op_marker_phantom: PhantomData<M>
}
impl<T> Operation<T>{
    pub fn from(state: Stateful) -> (Receiver<Operation<T>>, Operation<T>){
        let (sender, receiver) = unbounded();
        return (receiver,
                Operation {
                    state,
                    sender,
                    waker: None,
                    op_marker_phantom:PhantomData
                })
    }
}

/// A trait alias representing the bounds that must be filled by operations. Mainly to avoid
/// boilerplate.
pub trait OperationsBound = Debug + Send + 'static;

/// Type representing an operation in a Starting state.
#[derive(Clone, Debug, State)]
pub struct StartingOperation<A>(A) where A: OperationsBound;
impl<A> StartingOperation<A> where A: OperationsBound {
    // Starts a new operation from a given input.
    fn from_input(input: A) -> StartingOperation<A>{
        return StartingOperation(input);
    }
}
/// Type representing an operation in a Progressing state.
#[derive(Clone, Debug, State)]
pub struct ProgressingOperation<B>(B) where B: OperationsBound;
/// Type representing an operation in a Finished state.
#[derive(Clone, Debug, State)]
pub struct FinishedOperation<A>(A) where A: OperationsBound;

// Allowed transition between operation states
impl<A, B> TransitTo<ProgressingOperation<B>> for StartingOperation<A>
    where A: OperationsBound, B: OperationsBound {}
impl<B> TransitTo<ProgressingOperation<B>> for ProgressingOperation<B>
    where B: OperationsBound {}
impl<B,C> TransitTo<FinishedOperation<C>> for ProgressingOperation<B>
    where B: OperationsBound, C: OperationsBound {}

///////////////////////////////////////////////////////////////////////////////////////// HANDLED BY
/// The HandledBy trait must be implemented by operations to specify how they will use the resource
/// to progress.
pub trait HandledBy<R> where Self: Send{
    fn get_handled(mut self: Box<Self>, resource: &mut R);
}

//////////////////////////////////////////////////////////////////////////////////////////// FUTURES

// Represents the future state.
enum OperationFutureState<M, R> where Operation<M>: HandledBy<R>, M: 'static{
    Starting((Operation<M>, Sender<Box<dyn HandledBy<R>>>, Receiver<Operation<M>>)),
    Waiting(Receiver<Operation<M>>),
    Finished,
    Hazardous
}

/// A generic future that allows to drive an operation to completion on a resource.
pub struct OperationFuture<M,R,O> where Operation<M>: HandledBy<R>, M: 'static{
    state: Rc<RefCell<OperationFutureState<M,R>>>,
    output_phantom: PhantomData<O>
}
impl<M, R, O> OperationFuture<M, R, O> where Operation<M>: HandledBy<R>, M: 'static{
    /// Creates a new future.
    pub fn new(ope: Operation<M>,
           sender: Sender<Box<dyn HandledBy<R>>>,
           receiver: Receiver<Operation<M>>) -> OperationFuture<M, R, O>{
        return OperationFuture {
            state: Rc::new(RefCell::new(OperationFutureState::Starting((ope, sender, receiver)))),
            output_phantom: PhantomData,
        }
    }
}

// Generic implementation of the Operation future.
impl<M, R, O> Future for OperationFuture<M, R, O>
    where
        Operation<M>: HandledBy<R>,
        O:OperationsBound + Clone,
        M: 'static
{
    type Output = Result<O, crate::Error>;

    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Self::Output> {
        loop {
            match self.state.replace(OperationFutureState::Hazardous) {
                OperationFutureState::Starting((ope, sender, receiver)) => {
                    let mut ope = ope;
                    ope.waker = Some(wake.to_owned());
                    let (mut state, ret) = sender.send(Box::new(ope))
                        .map_or_else(
                            |_| {
                                (OperationFutureState::Finished,
                                 Poll::Ready(Err(crate::Error::from(
                                     Error::FuturePoll(format!("Failed to send operation."))))
                                 )
                                )
                            },
                            |_| {
                                (OperationFutureState::Waiting(receiver.to_owned()), Poll::Pending)});
                    self.state.replace(state);
                    return ret;
                }
                OperationFutureState::Waiting(receiver) => {
                    self.state.replace(OperationFutureState::Finished);
                    let ope = match receiver.recv() {
                        Ok(ope) => ope,
                        Err(e) => {
                            return Poll::Ready(Err(crate::Error::from(
                                Error::FuturePoll(format!("Failed to receive operation: {}", e))))
                            )
                        }
                    };
                    if let Some(FinishedOperation(o)) = ope.state.to_state::<FinishedOperation<O>>(){
                        return Poll::Ready(Ok(o))
                    } else {
                        panic!("Operation retrieved in a the wrong state.");
                    }
                }
                OperationFutureState::Finished | OperationFutureState::Hazardous => {
                    unreachable!();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::ToOwned;

    #[test]
    fn test() {
        // How pieces fits in the whole thing.

        // First we implement a resource as an asynchronous handling object.
        struct MyResource {
            queue: Vec<Box<dyn HandledBy<MyResource>>>
        }
        impl MyResource{
            fn spawn() -> MyResourceHandle{
                type OpTraitObj = Box<dyn HandledBy<MyResource>>;
                let (sender, receiver): (Sender<OpTraitObj>, Receiver<OpTraitObj>) = unbounded();
                std::thread::spawn(move ||{
                    let mut res = MyResource{queue: Vec::new()};
                    loop{
                        while let Some(s) = res.queue.pop(){
                            s.get_handled(&mut res);
                        }
                        match receiver.try_recv(){
                            Ok(o) => res.queue.push(o),
                            Err(TryRecvError::Empty) => continue,
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                });
                return MyResourceHandle{sender}
            }
        }

        // We implement a (probably Clone/Send/Sync) handle to the resource, which allows to create
        // futures.
        struct MyResourceHandle{
            sender: Sender<Box<dyn HandledBy<MyResource>>>
        }
        impl MyResourceHandle{
            fn async_op_1(&self) -> MyOp1Fut{
                let (recv, op) = MyOp1::from(Stateful::from(StartingOperation("Starting".to_owned())));
                return MyOp1Fut::new(op, self.sender.clone(), recv);
            }
            fn async_op_2(&self) -> MyOp2Fut{
                let (recv, op) = MyOp2::from(Stateful::from(StartingOperation(0 as u32)));
                return MyOp2Fut::new(op, self.sender.clone(), recv);
            }
        }

        // We declare the two types of operations we will perform.
        struct MyOp1Marker {};
        type MyOp1 = Operation<MyOp1Marker>;
        impl HandledBy<MyResource> for MyOp1 {
            fn get_handled(mut self: Box<Self>, resource: &mut MyResource) {
                if let Some(s) = self.state.to_state::<StartingOperation<String>>() {
                    println!("Op1 received in state Starting: {:?}", s);
                    self.state = Stateful::from(s.transit_to(ProgressingOperation("Progressing".to_owned())));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<ProgressingOperation<String>>() {
                    println!("Op1 received in state Progressing: {:?}", s);
                    self.state = Stateful::from(s.transit_to(FinishedOperation("Succeeded".to_owned())));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<FinishedOperation<String>>() {
                    println!("Op1 received in state Finished: {:?}", s);
                    println!("Sending back Op1");
                    let waker = self.waker.as_ref().expect("No waker given with the Op1.").to_owned();
                    let sendv = self.sender.clone();
                    sendv.send(*self);
                    println!("Waking Op1");
                    waker.wake();
                } else {
                    panic!("MyOp1 {:?} received in a wrong state");
                }
            }
        }
        type MyOp1Fut = OperationFuture<MyOp1Marker, MyResource, String>;

        struct MyOp2Marker {};
        type MyOp2 = Operation<MyOp2Marker>;
        impl HandledBy<MyResource> for MyOp2 {
            fn get_handled(mut self: Box<Self>, resource: &mut MyResource) {
                if let Some(s) = self.state.to_state::<StartingOperation<u32>>() {
                    println!("Op2 received in state Starting: {:?}", s);
                    self.state = Stateful::from(s.transit_to(ProgressingOperation(1 as u32)));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<ProgressingOperation<u32>>() {
                    println!("Op2 received in state Progressing: {:?}", s);
                    self.state = Stateful::from(s.transit_to(FinishedOperation(2 as u32)));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<FinishedOperation<u32>>() {
                    println!("Op2 received in state Finished: {:?}", s);
                    println!("Sending back Op2");
                    let waker = self.waker.as_ref().expect("No waker given with Op2.").to_owned();
                    let sendv = self.sender.clone();
                    sendv.send(*self);
                    println!("Waking Op2");
                    waker.wake();
                } else {
                    panic!("Not intended");
                }
            }
        }
        type MyOp2Fut = OperationFuture<MyOp2Marker, MyResource, u32>;

        // Now we try to use those
        let handle = MyResource::spawn();
        use futures::executor::block_on;
        let op1 = handle.async_op_1();
        let op2 = handle.async_op_2();
        let res1 = block_on(op1);
        eprintln!("res1 = {:#?}", res1);
        assert_eq!(res1.unwrap(), "Succeeded".to_string());
        let res2 = block_on(op2);
        eprintln!("res2 = {:#?}", res2);
        assert_eq!(res2.unwrap(), 2 as u32)

    }
}



