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
//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use crate::stateful::{Stateful, State, TransitTo};
use crossbeam::channel::{unbounded, Sender, Receiver};
use std::marker::PhantomData;
use std::fmt::Debug;

///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS
/// Operations are stateful trait objects that follows the lifecycle:
///                         ,-> Failed
/// Starting -> Progressing --> Succeeded
///                 ^-------'

/// An operation is a Trait object (via its state), which is marked by an operation type T. This
/// marker type allows to differentiate between the different operations.
struct Operation<T>{
    state: Stateful,
    sender: Sender<Operation<T>>,
    op_type: PhantomData<T>
}
impl<T> Operation<T>{
    fn from(state: Stateful) -> (Receiver<Operation<T>>, Operation<T>){
        let (sender, receiver) = unbounded();
        return (receiver,
                Operation {
                    state,
                    sender,
                    op_type:PhantomData
                })
    }
}

/// A trait alias representing the bounds that must be filled by operations. Just to avoid boilerplate
pub trait OperationsBound = Clone + Debug + 'static;

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
/// Type representing an operation in a Succeeded state.
#[derive(Clone, Debug, State)]
pub struct SucceededOperation<A>(A) where A: OperationsBound;
/// Type representing an operation in a Failed state.
#[derive(Clone, Debug, State)]
pub struct FailedOperation<A>(A) where A: OperationsBound;

// Allowed transition between operation states
impl<A, B> TransitTo<ProgressingOperation<B>> for StartingOperation<A>
    where A: OperationsBound, B: OperationsBound {}
impl<B> TransitTo<ProgressingOperation<B>> for ProgressingOperation<B>
    where B: OperationsBound {}
impl<B,C> TransitTo<SucceededOperation<C>> for ProgressingOperation<B>
    where B: OperationsBound, C: OperationsBound {}
impl<B,D> TransitTo<FailedOperation<D>> for ProgressingOperation<B>
    where B: OperationsBound, D: OperationsBound {}

///////////////////////////////////////////////////////////////////////////////////////// HANDLED BY
/// The HandledBy trait must be implemented by operations to specify how they will use the resource
/// to progress.
pub trait HandledBy<R>{
    fn get_handled(mut self: Box<Self>, resource: &mut R);
    fn wake(mut self: Box<Self>);
}

//////////////////////////////////////////////////////////////////////////////////////////// FUTURES


#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::ToOwned;

    #[test]
    fn test_handling() {
        struct MyHandler {
            queue: Vec<Box<dyn HandledBy<MyHandler>>>
        }
        struct MyOpType {};
        type MyOp = Operation<MyOpType>;
        impl HandledBy<MyHandler> for MyOp {
            fn get_handled(mut self: Box<Self>, resource: &mut MyHandler) {
                if let Some(s) = self.state.to_state::<StartingOperation<String>>() {
                    println!("Started received");
                    self.state = Stateful::from(s.transit_to(ProgressingOperation("Progressing".to_owned())));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<ProgressingOperation<String>>() {
                    println!("Progressing received");
                    self.state = Stateful::from(s.transit_to(SucceededOperation("Succeeded".to_owned())));
                    resource.queue.push(self);
                } else if let Some(s) = self.state.to_state::<SucceededOperation<String>>() {
                    println!("Succeeded received");
                    println!("Waking");
                    self.wake();
                } else if let Some(s) = self.state.to_state::<FailedOperation<String>>() {
                    println!("Failed received");
                    println!("Waking");
                    self.wake();
                } else {
                    panic!("Not supposed")
                }
            }
            fn wake(mut self: Box<Self>) {
                let sendv = self.sender.clone();
                sendv.send(*self);
            }
        }

        let (recv, op) = MyOp::from(Stateful::from(StartingOperation("Starting".to_owned())));
        let mut handler = MyHandler {
            queue: Vec::new()
        };
        handler.queue.push(Box::new(op));
        while let Some(op) = handler.queue.pop(){
            op.get_handled(&mut handler);
        }
        let op = recv.recv().unwrap();
        if let Some(SucceededOperation(s)) = op.state.to_state::<SucceededOperation<String>>(){
            println!("Operation succeeded with output: {:?}", s)
        } else if let Some(FailedOperation(s)) = op.state.to_state::<FailedOperation<String>>(){
            println!("Operation fauled with output: {:?}", s)
        }
    }
}



