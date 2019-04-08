// liborchestra/error.rs
// Author: Alexandre Péré

/// This module contains module-level error type to interface with the error types implemented at
/// the sub-module level. 

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use crate::{ssh, repository, misc};
use std::{io, error, fmt};
use regex;
use git2;
use yaml_rust;

////////////////////////////////////////////////////////////////////////////////////////////// ERROR
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Yaml(serde_yaml::Error),
    Regex(regex::Error),
    YamlScanError(yaml_rust::ScanError),
    Ssh(ssh::Error),
    Git(git2::Error),
    Repository(repository::Error),
    Misc(misc::Error),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => write!(f, "Io Error: {}", err),
            Error::Yaml(ref err) => write!(f, "Yaml Error: {}", err),
            Error::Regex(ref err) => write!(f, "Regex Error: {}", err),
            Error::YamlScanError(ref err) => write!(f, "Yaml Error: {}", err),
            Error::Ssh(ref s) => write!(f, "Ssh Error: {}", s),
            Error::Git(ref s) => write!(f, "Git Error: {}", s),
            Error::Repository(ref s) => write!(f, "Repository Error: {}", s),
            Error::Misc(ref s) => write!(f, "Misc Error: {}", s),
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
        Error::Yaml(err)
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

impl From<git2::Error> for Error {
    fn from(err: git2::Error) -> Error {
        Error::Git(err)
    }
}

impl From<repository::Error> for Error{
    fn from(err: repository::Error) -> Error{
        Error::Repository(err)
    }
}

impl From<misc::Error> for Error{
    fn from(err: misc::Error) -> Error{
        Error::Misc(err)
    }
}
