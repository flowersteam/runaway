// liborchestra/mod.rs
// Author: Alexandre Péré
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
extern crate clap;
extern crate serde;
extern crate serde_yaml;
extern crate crypto;

// IMPORTS
use std::{io, process, fmt, error};

// MODULES
/// The library is divided into various modules:
/// + `repository`: Structures to handle expegit repositories
/// + `run`: General structure to run executions on remote hosts.
/// + `async`: General worker structure allowing asynchronous execution of jobs from a queue.
/// + `tasks`: Specific taskqueue and task structure for remote run of expegit executions.
/// + `git`: Utilities to perform classic operations on git repositories
/// + `misc`: Few miscellaneous functions publicly available
/// + `utilities`: Few miscellaneous meant for private use of the different modules
mod async;
mod utilities;
pub mod git;
pub mod tasks;
pub mod run;
pub mod repository;
pub mod misc;


// STATICS
// The path to the script to launch in the experiment repo
pub static SCRIPT_RPATH: &str = "run";
// globs pattern for files to ignore in send
pub static SEND_IGNORE_RPATH: &str = ".sendignore";
// globs pattern for files to ignore in fetch
pub static FETCH_IGNORE_RPATH: &str = ".fetchignore";
// folder containing execution profiles in $HOME
pub static PROFILES_FOLDER_RPATH: &str = ".runaway";
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
// folder containing lfs managed data in a data folder
pub static LFS_RPATH: &str = "lfs";
// file containing expegit configuration
pub static EXPEGIT_RPATH: &str = ".expegit";             


// ERROR
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Git(String),
    Json(serde_yaml::Error),
    Regex(regex::Error),
    YamlScanError(yaml_rust::ScanError),
    ExecutionFailed(process::Output),
    InvalidCommit(String),
    InvalidIdentifier(String),
    ProfileError,
    Packing,
    Unpacking,
    ScpSend,
    ScpFetch,
    InvalidRepository,
    AlreadyRepository,
    NotImplemented,
    Unknown,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => write!(f, "Io Error: {}", err),
            Error::Git(ref err) => write!(f, "Git Error: {}", err),
            Error::Json(ref err) => write!(f, "Json Error: {}", err),
            Error::Regex(ref err) => write!(f, "Regex Error: {}", err),
            Error::NotImplemented => write!(f, "Not Implemented"),
            Error::InvalidCommit(ref com) => write!(f, "Commit {} not found", com),
            Error::InvalidIdentifier(ref id) => write!(f, "Execution {} not found", id),
            Error::YamlScanError(ref err) => write!(f, "Yaml Error: {}", err),
            Error::ExecutionFailed(ref o) => write!(f, "Remote execution failed. Process output: {:?}", o),
            Error::InvalidRepository => write!(f, "Not an expegit repository"),
            Error::AlreadyRepository => write!(f, "Already an expegit repository"),
            Error::ProfileError => write!(f, "Malformed Profile"),
            Error::Packing => write!(f, "Packing of directory before send failed."),
            Error::Unpacking => write!(f, "Unpacking of archive after fetch failed."),
            Error::ScpSend => write!(f, "Scp send command failed."),
            Error::ScpFetch => write!(f, "Scp fetch command failed."),
            Error::Unknown => write!(f, "Unknown error occured"),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Io(ref err) => err.description(),
            Error::Git(_) => "Error occurred when performing git command",
            Error::Json(ref err) => err.description(),
            Error::Regex(ref err) => err.description(),
            Error::YamlScanError(ref err) => err.description(),
            Error::ExecutionFailed(_) => "Remote execution failed",
            Error::NotImplemented => "Feature not yet implemented",
            Error::InvalidCommit(_) => "Commit not found in experiment repo commits",
            Error::InvalidIdentifier(_) => "Execution not found",
            Error::InvalidRepository => "Not an expegit repository",
            Error::AlreadyRepository => "Already a repository",
            Error::ProfileError => "Malformed Profile",
            Error::Packing => "Packing failed",
            Error::Unpacking => "Unpacking Failed",
            Error::ScpSend => "Scp failed",
            Error::ScpFetch => "Scp failed",
            Error::Unknown => "Unknown error occured",
        }
    }
    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Io(ref err) => Some(err),
            Error::Git(_) => None,
            Error::Json(ref err) => Some(err),
            Error::Regex(ref err) => Some(err),
            Error::YamlScanError(ref err) => Some(err),
            Error::ExecutionFailed(_) => None,
            Error::NotImplemented => None,
            Error::InvalidCommit(_) => None,
            Error::InvalidRepository => None,
            Error::InvalidIdentifier(_) => None,
            Error::AlreadyRepository => None,
            Error::ProfileError => None,
            Error::Packing => None,
            Error::Unpacking => None,
            Error::ScpSend => None,
            Error::ScpFetch => None,
            Error::Unknown => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::Io(err)
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(err: serde_yaml::Error) -> Error {
        Error::Json(err)
    }
}

impl From<regex::Error> for Error {
    fn from(err: regex::Error) -> Error {
        Error::Regex(err)
    }
}

impl From<yaml_rust::ScanError> for Error {
    fn from(err: yaml_rust::ScanError) -> Error {
        Error::YamlScanError(err)
    }
}
