//! liborchestra/mod.rs
//! Author: Alexandre Péré
#![feature(trace_macros, async_await, result_map_or_else, trait_alias, try_blocks)]
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


//-------------------------------------------------------------------------------------------- TYPES

// Using types to represent a part of your program logic allows it to be checked by the compiler 
// before being executed. This is pretty interesting in terms of safety and is one of the 'good 
// points' of strongly typed functional languages, plus it makes the api more readable, as part of 
// logic is made explicit in the function signature.

use std::path::{Path, PathBuf};

/// A trait alias for AsRef<Path>
pub trait AsPa = AsRef<Path>;


// For instance, for paths, we prefer to express the logic of a path being absolute or relative in 
// the type system, to avoid this kind of errors at runtime.


/// Represents a path expressed in a relative fashion. Note that you can use the 'R' generic to 
/// enforce a set of paths to be expressed at the same root.
/// ```
/// pub struct HomePath(PathBuf);
/// impl Asref<Path> for HomePath{ // implement traits } 
/// let home = HomePath(PathBuf::from(r"/home/user"));
/// 
/// let bashrc = RelativePath{root: home, path: PathBuf::from(r".bashrc")};
/// let zshhrc = RelativePath{root: home, path: PathBuf::from(r".zshrc")};
/// let fishrc = RelativePath{root: PathBuf::from(r"/home/user"), path: PathBuf::from(r".fishrc")};
/// 
/// let mut rc_list = Vec::new();
/// rc_list.push(bashrc);
/// rc_list.push(zshrc); // this works, as bashrc and zshrc have compatible signatures.
/// rc_list.push(fishrc); // this is caught by the compiler, as signatures are incompatible.
/// ``` 
pub struct RelativePath<R: AsPa, P: AsPa> {root: R, path: P}
impl<R: AsPa, P: AsPa> RelativePath<R, P>{
    pub fn to_absolute(&self) -> AbsolutePath<PathBuf>{
        AbsolutePath{
            path: self.root.as_ref().join(self.path.as_ref()),
        }
    }
}

/// Represents a path expressed in a absolute fashion.
#[derive(Debug)]
pub struct AbsolutePath<P: AsPa> {path: P}

/// Represents a file.
pub struct File<P>{path: P}

/// Represents a folder.
#[derive(Debug)]
pub struct Folder<P>{path: P}

/// Represents an archive, which unites a file and a hash value.
pub struct Archive<P>{file: File<P>, hash: u64}

/// Represents an ignore file.
pub struct Ignore<P>{file: File<P>}

/// Represents a located resource. This allows to attach the location of any resource with it.
pub struct Located<L, C> {
    location: L, 
    content: C
}

/// Represents the local host
pub struct LocalLocation;

/// Represents any C located on the local host.
pub type Local<C> = Located<LocalLocation, C>;

/// Represents a remote host marked with the marker M.
pub struct RemoteLocation<M>{marker: M, node: ssh::RemoteHandle}

/// Represents any C located on a remote M.
pub type Remote<M, C> = Located<RemoteLocation<M>, C>;

/// Represents a command
#[derive(Debug)]
pub struct RawCommand<S: AsRef<str>>(S);
impl<S: AsRef<str>> From<S> for RawCommand<S>{
    fn from(s: S) -> Self{
        RawCommand(s)
    }
}

/// Represents any O that exist within a context C.
#[derive(Debug, Clone)]
pub struct InContext<C, I>{
    context: C,
    inner: I,
}
impl<C, I> InContext<C, I>{
    pub fn map<O, F: FnOnce(I) -> O>(self, f: F) -> InContext<C,O>{
        let InContext{context, inner} = self;
        InContext{
            context,
            inner: f(inner),
        }
    }

    pub fn map_context<K, F: FnOnce(C) -> K>(self, f: F) -> InContext<K,O>{
        let InContext{context, inner} = self;
        InContext{
            context: f(context),
            inner,
        }
    }
}

/// Represents an environment variable
#[derive(Debug, PartialEq, Clone)]
pub struct EnvironmentVariable<N: AsRef<str>, V: AsRef<str>>{
    name: N,
    value:V, 
}

/// Represents a Current Working Directory
#[derive(Debug)]
pub struct Cwd<P>(Folder<P>);

/// Represents a classic terminal context made out of a cwd and some  enrironment variables
#[derive(Debug)]
pub struct TerminalContext<P:AsPa, S:AsRef<str>, T:AsRef<str>>{
    cwd: Cwd<AbsolutePath<P>>,
    envs: Vec<EnvironmentVariable<S,T>>
}

/// Represents the output of a runnable
#[derive(Debug)]
pub struct Runned<C,R>{
    context:C,
    output:R,
}

pub struct LocalCodeRoot<P:AsRef<Path>>(P);
impl<P:AsRef<Path>> AsRef<Path> for LocalCodeRoot<P>{ 
    fn as_ref(&self) -> &Path{
        return self.0.as_ref()
    }
}

pub struct Glob<S: AsRef<str>>(S);

