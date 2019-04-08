// liborchestra/mod.rs
// Author: Alexandre Péré
#![feature(await_macro, async_await, futures_api, result_map_or_else, associated_type_defaults)]
///
/// Liborchestra gives tools to manipulate expegit repositories, run scripts on user-defined hosts,
/// and orchestrate the executions of batches of experiments under variations of parameters.
///
/// Concepts around which the code is written:
/// + Experiment: refers to the code of the experiment. By extension, refers to the separate
/// repository in which the experiment code is managed.
/// + Execution: refers to the result of running the experiment once, for a given experiment
/// repository commit, parameter and machine. By extension, refers to the encompassing structure.
/// + Campaign: refers to an ensemble of executions. By extension, refers to the separate repository
/// in which the executions are stored.
/// + Running: refers to the act of running an experiment so as to obtain an execution.

// CRATES
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;
extern crate chrono;
extern crate regex;
extern crate yaml_rust;
extern crate pretty_logger;
extern crate uuid;
extern crate serde;
extern crate serde_yaml;
extern crate crypto;
extern crate rpassword;
extern crate ssh2;
extern crate dirs;



// IMPORTS
use std::{io, process, fmt};

// MODULES
/// The library is divided into various modules:
/// + `repository`: Structures to handle expegit repositories
/// + `run`: General structure to run executions on remote hosts.
/// + `async`: General worker structure allowing asynchronous execution of jobs from a queue.
/// + `tasks`: Specific taskqueue and task structure for remote run of expegit executions.
/// + `git`: Utilities to perform classic operations on git repositories
/// + `misc`: Few miscellaneous functions publicly available
/// + `utilities`: Few miscellaneous meant for private use of the different modules
//mod utilities;
pub mod ssh;
//pub mod git;
//pub mod tasks;
//pub mod run;
pub mod repository;
pub mod misc;
pub mod error;


// STATICS
// The path to the script to launch in the experiment repo
pub static SCRIPT_RPATH: &str = "run";
// globs pattern for files to ignore in send
pub static SEND_IGNORE_RPATH: &str = ".sendignore";
// globs pattern for files to ignore in fetch
pub static FETCH_IGNORE_RPATH: &str = ".fetchignore";
// folder containing execution profiles in $HOME
pub static PROFILES_FOLDER_RPATH: &str = ".runaway";
// file containing known hosts keys
pub static KNOWN_HOSTS_RPATH: &str = ".runaway/known_hosts";
// file name of tar archive to send
pub static SEND_ARCH_RPATH: &str = ".send.tar";
// file name of tar to fetch
pub static FETCH_ARCH_RPATH: &str = ".fetch.tar";
// folder containing the experiment repository as a submodule
pub static XPRP_RPATH: &str = "xprp";
// folder containing the experiment executions
pub static EXCS_RPATH: &str = "excs";
// file containing the execution parameters.
pub static EXCCONF_RPATH: &str = ".excconf";
// folder containing the output data in an execution folder
pub static DATA_RPATH: &str = "data";
// file containing expegit configuration
pub static CMPCONF_RPATH: &str = ".cmpconf";             


use error::Error;
