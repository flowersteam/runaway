//! liborchestra/mod.rs
//! Author: Alexandre Péré
#![feature(trace_macros, await_macro, async_await, result_map_or_else, trait_alias,
try_blocks)]
//! Liborchestra gives tools to manipulate expegit repositories, run scripts on user-defined hosts,
//! and orchestrate the executions of batches of experiments under variations of parameters.
//! 
//! Concepts around which the code is written:
//! + Experiment: refers to the code of the experiment. By extension, refers to the separate
//! repository in which the experiment code is managed.
//! + Execution: refers to the result of running the experiment once, for a given experiment
//! repository commit, parameter and machine. By extension, refers to the encompassing structure.
//! + Campaign: refers to an ensemble of executions. By extension, refers to the separate repository
//! in which the executions are stored.
//! + Running: refers to the act of running an experiment so as to obtain an execution.


//------------------------------------------------------------------------------------------ IMPORTS


#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;
#[macro_use]
extern crate liborchestra_derive;
extern crate chrono;
extern crate regex;
extern crate yaml_rust;
extern crate env_logger;
extern crate uuid;
extern crate serde;
extern crate serde_yaml;
extern crate crypto;
extern crate rpassword;
extern crate ssh2;
extern crate dirs;


//------------------------------------------------------------------------------------------ MODULES


pub mod ssh;
pub mod hosts;
pub mod repository;
pub mod misc;
pub mod primitives;
pub mod stateful;
pub mod application;
#[macro_use]
pub mod error;


//------------------------------------------------------------------------------------------- MACROS


#[macro_export]
macro_rules! derive_from_error {
    ($error:ident, $from_type:ty, $variant:ident) => {
        impl From<$from_type> for $error {
            fn from(err: $from_type) -> $error {
                    $error::$variant(err)
            }
        }
    }
}


//------------------------------------------------------------------------------------------ STATICS


/// The path to the script to launch in the experiment repo
pub static SCRIPT_RPATH: &str = "run";
/// globs pattern for files to ignore in send
pub static SEND_IGNORE_RPATH: &str = ".sendignore";
/// globs pattern for files to ignore in fetch
pub static FETCH_IGNORE_RPATH: &str = ".fetchignore";
/// folder containing execution profiles in $HOME
pub static PROFILES_FOLDER_RPATH: &str = ".orchestra";
/// file containing known hosts keys
pub static KNOWN_HOSTS_RPATH: &str = ".orchestra/known_hosts";
/// file containing ssh configs
pub static SSH_CONFIG_RPATH: &str = ".orchestra/config";
/// file name of tar archive to send
pub static SEND_ARCH_RPATH: &str = ".send.tar";
/// file name of tar to fetch
pub static FETCH_ARCH_RPATH: &str = ".fetch.tar";
/// folder containing the experiment repository as a submodule
pub static XPRP_RPATH: &str = "xprp";
/// folder containing the experiment executions
pub static EXCS_RPATH: &str = "excs";
/// file containing the execution parameters.
pub static EXCCONF_RPATH: &str = ".excconf";
/// folder containing the output data in an execution folder
pub static DATA_RPATH: &str = "data";
/// file containing expegit configuration
pub static CMPCONF_RPATH: &str = ".cmpconf";
/// file containing the execution features
pub static FEATURES_RPATH: &str = ".features";


//-------------------------------------------------------------------------------------------- ERROR


use error::Error;