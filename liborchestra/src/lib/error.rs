// liborchestra/error.rs
// Author: Alexandre Péré

/// This module contains module-level error type to interface wit the error types implemented at
/// the sub-module level. 


// IMPORTS
use crate::ssh;
use std::{io, error, fmt};
use regex;

// ERROR
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_yaml::Error),
    Regex(regex::Error),
    YamlScanError(yaml_rust::ScanError),
    Ssh(ssh::Error)
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => write!(f, "Io Error: {}", err),
            Error::Json(ref err) => write!(f, "Json Error: {}", err),
            Error::Regex(ref err) => write!(f, "Regex Error: {}", err),
            Error::YamlScanError(ref err) => write!(f, "Yaml Error: {}", err),
            Error::Ssh(ref s) => write!(f, "Ssh Error: {}", s),
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

impl From<ssh::Error> for Error {
    fn from(err: ssh::Error) -> Error {
        Error::Ssh(err)
    }
}
