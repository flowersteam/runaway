//! runaway-cli/subcommands/mod.rs
//! Author: Alexandre Péré
//! 
//! This module contains subcommand functions. Those functions are called based on the dispatch of 
//! the application main. They each implement the logic of one of the different subcommands.


//------------------------------------------------------------------------------------------ MODULES


mod exec;
pub use exec::exec;

mod batch;
pub use batch::batch;

mod sched;
pub use sched::sched;

mod complete;
pub use complete::install_completion;