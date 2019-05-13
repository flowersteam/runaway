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
pub trait State where Self: Debug {
    fn as_any(&self) -> &dyn Any;
}

/// A trait that allows to transit from one state (self:Self) to another (init:A). It is a marker
/// trait that unlock the transit_to method, which itself allows to transit to another state. It
/// replaces the From<T> trait which is usually used for state machines and allows to transit to
/// value based states.
pub trait TransitTo<A> where Self: Sized{
    fn transit_to(self, init:A) -> A{
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
        impl From<StateB> for StateA{
            fn from(other: StateB) -> StateA{
                return StateA{integer: other.string.parse::<i32>().unwrap()}
            }
        }
        impl From<StateA> for StateB{
            fn from(other: StateA) -> StateB{
                return StateB{string: format!("{}", other.integer +1)}
            }
        }

        // Using the state machine
        let mut machine: Stateful = Stateful::from(StateA{integer:0});

        for a in 1..10{
            println!("Machine state is: {:?}", machine);
            // We transition depending on the state. This is not far from a match statement.
            if let Some(state_a) = machine.to_state::<StateA>(){
                let state_b: StateB = From::<StateA>::from(state_a);
                machine = Stateful::from(state_b);
            } else if let Some(state_b) = machine.to_state::<StateB>(){
                let state_a: StateA = From::<StateB>::from(state_b);
                machine = Stateful::from(state_a);
            } else {
                panic!("No transition occurred");
            }
        }
    }

    #[test]
    fn test_derive(){

        #[derive(Clone, Debug, State)]
        struct St<A>(A) where A: Clone + Debug + 'static;
        let a = St(44);
        a.as_any();


    }
}

