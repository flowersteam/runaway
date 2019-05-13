// liborchestra/error.rs
// Author: Alexandre Péré

/// This module contains module-level error type to interface with the error types implemented at
/// the sub-module level. 

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use crate::{ssh, repository, misc, primitives, derive_from_error};
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
    Primitives(primitives::Error),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Io(ref err) => write!(f, "Io related error happened:\n{}", err),
            Error::Yaml(ref err) => write!(f, "Yaml related error happened:\n{}", err),
            Error::Regex(ref err) => write!(f, "Regex related error happened:\n{}", err),
            Error::YamlScanError(ref err) => write!(f, "Yaml related error happened:\n{}", err),
            Error::Ssh(ref s) => write!(f, "Ssh related error happened:\n{}", s),
            Error::Git(ref s) => write!(f, "Git related error happened:\n{}", s),
            Error::Repository(ref s) => write!(f, "Repository related error happened:\n{}", s),
            Error::Misc(ref s) => write!(f, "Misc related error happened:\n{}", s),
            Error::Primitives(ref s) => write!(f, "Primitives related error occurred:\n{}", s)
        }
    }
}

derive_from_error!(Error, io::Error, Io);
derive_from_error!(Error, serde_yaml::Error, Yaml);
derive_from_error!(Error, regex::Error, Regex);
derive_from_error!(Error, yaml_rust::ScanError, YamlScanError);
derive_from_error!(Error, ssh::Error, Ssh);
derive_from_error!(Error, git2::Error, Git);
derive_from_error!(Error, repository::Error, Repository);
derive_from_error!(Error, misc::Error, Misc);
derive_from_error!(Error, primitives::Error, Primitives);