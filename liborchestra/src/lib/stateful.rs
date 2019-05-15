// liborchestra/stateful.rs
// Author: Alexandre Péré
/// This module contains tools to create Stateful objects whose transitions are checked to be
/// correct by the compiler.
//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use std::any::Any;
use std::fmt::Debug;

/////////////////////////////////////////////////////////////////////////////////////////// STATEFUL

/// A type is a State if it is Debug and implement the `as_any` method. This last point allows
/// Stateful to cast the trait object into concrete type. There is a custom derive proc macro in
/// liborchestra_derive. Note that State implementors must also be Clone to be casted into by stateful.
pub trait State where Self: Debug + Send{
    fn as_any(&self) -> &dyn Any;
}

/// A trait that allows to transition from one state (self:Self) to another (init:A). It is a marker
/// trait that unlock the transition_to method, which itself allows to transition to another state.
/// It replaces the From<T> trait which is usually used for state machines and allows to transition to
/// value based states.
pub trait TransitionsTo<A> where Self: Sized{
    fn transition_to(self, init:A) -> A{
       init
    }
}

/// Stateful is a trait object that wraps a state, and allows to cast to concrete type.
#[derive(Debug)]
pub struct Stateful{
    state: Box<dyn State>,
}

impl Stateful{
    /// Returns whether the state is A
    pub fn is_state<A: 'static>(&self) -> bool{
        return self.state.as_any().is::<A>()
    }
    /// Creates a clone of the state if it is A
    pub fn to_state<A: 'static + Clone>(&self) -> Option<A>{
        self.state.as_any().downcast_ref::<A>().map(|a| a.to_owned())
    }
    /// Transitions from one state to another.
    pub fn transition<F:State+TransitionsTo<T>+Clone+'static, T:State+'static>(&mut self, other:T){
        if let Some(f) = self.to_state::<F>() {
            self.state = Box::new(other);
        } else {
            panic!("Wrong transition occurred!")
        }
    }
}

impl<A> From<A> for Stateful where A: State + 'static{
    fn from(other: A) -> Stateful{
        return Stateful{state: Box::new(other)}
    }
}


/////////////////////////////////////////////////////////////////////////////////////////////// TEST
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stateful() {

        #[derive(Clone, Debug, State)]
        struct StateA{
            integer: i32
        }

        #[derive(Clone, Debug, State)]
        struct StateB{
            string: String
        }

        // Transitions
        impl TransitionsTo<StateB> for StateA{}
        impl TransitionsTo<StateA> for StateB{}

        // Using the state machine
        let mut machine: Stateful = Stateful::from(StateA{integer:0});

        for a in 1..10{
            println!("Machine state is: {:?}", machine);
            // We transition depending on the state. This is not far from a match statement.
            if let Some(state_a) = machine.to_state::<StateA>(){
                machine.transition::<StateA, StateB>(StateB{string:format!("{}", state_a.integer+1)});
            } else if let Some(state_b) = machine.to_state::<StateB>(){
                machine.transition::<StateB, StateA>(StateA{integer:state_b.string.parse::<i32>().unwrap()});
            } else {
                panic!("No transition occurred");
            }
        }
    }

    #[test]
    fn test_derive(){

        #[derive(Clone, Debug, State)]
        struct St<A>(A) where A: Clone + Debug + Send + 'static;
        let a = St(44);
        a.as_any();


    }
}

