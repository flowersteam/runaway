//! liborchestra/misc.rs
//! Author: Alexandre Péré
//!
//! A few miscellaneous available library wide.


//------------------------------------------------------------------------------------------ IMPORTS


use std::{process, path, fs, error, fmt};
use regex;
use super::CMPCONF_RPATH;
use tracing::{self, warn, debug};
use super::commons::OutputBuf;
use super::timer::TimerHandle;


//------------------------------------------------------------------------------------------- STATIC

lazy_static! {
    pub static ref TIMER: TimerHandle = TimerHandle::spawn().expect("Failed to spawn timer.");
}


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug)]
pub enum Error {
    InvalidRepository,
    Unknown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::InvalidRepository => write!(f, "Invalid expegit repository"),
            Error::Unknown => write!(f, "Unknown error occured"),
        }
    }
}


//------------------------------------------------------------------------------------------- MACROS


/// This macro allows to asynchronously wait for (at least) a given time. This means that the thread
/// is yielded when it is done. For now, it creates a separate thread each time a sleep is needed, 
/// which is far from ideal.
#[macro_export] 
macro_rules! async_sleep {
    ($dur: expr) => {
        {
            use crate::misc::TIMER;
            (*TIMER).async_sleep($dur).await.unwrap();
        }
    };
}

/// This macro allows to intercept a wouldblock error returned by the expression evaluation, and 
/// awaits for 1 ns (at least) before retrying. 
#[macro_export]
macro_rules! await_wouldblock_io {
    ($expr:expr) => {
        {
            use tracing::trace_span;
            let span = trace_span!("await_wouldblock_io!");
            let _guard = span.enter();
            loop{
                match $expr {
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        async_sleep!(std::time::Duration::from_millis(2))
                    }
                    res => break res,
                }
            }
        }
    }
}

/// This macro allows to intercept a wouldblock error returned by the expression evaluation, and 
/// awaits for 1 ns (at least) before retrying. 
#[macro_export]
macro_rules! await_wouldblock_ssh {
    ($expr:expr) => {
        {
            use tracing::trace_span;
            let span = trace_span!("await_wouldblock_ssh!");
            let _guard = span.enter();
            loop{
                match $expr {
                    Err(ref e) if e.code() == -37 => {
                        async_sleep!(std::time::Duration::from_millis(2))
                    }
                    Err(ref e) if e.code() == -21 => {
                        async_sleep!(std::time::Duration::from_millis(2))
                    }
                    res => break res,
                }
            }
        }
    }
}

/// This macro allows to retry an ssh expression if the error code received was $code. It allows to 
/// retry commands that fails every now and then for a limited amount of time.
#[macro_export]
macro_rules! await_retry_n_ssh {
    ($expr:expr, $nb:expr, $($code:expr),*) => {
       {    
            use tracing::trace_span;
            let span = trace_span!("await_retry_n_ssh!");
            let _guard = span.enter();
            let nb = $nb as usize;
            let mut i = 1 as usize;
            loop{
                match $expr {
                    Err(e)  => {
                        if i == nb {
                            break Err(e)
                        }
                        $(
                            else if e.code() == $code as i32 {
                                async_sleep!(std::time::Duration::from_millis(2));
                                i += 1;
                            }
                        )*
                    }
                    res => {
                        break res
                    }
                }
            }
        }
    }
}

/// This macro allows to retry an ssh expression if the error code received was $code. It allows to 
/// retry commands that fail but must be retried until it's ok. For example 
#[macro_export]
macro_rules! await_retry_ssh {
    ($expr:expr, $($code:expr),*) => {
       {    
            use tracing::trace_span;
            let span = trace_span!("await_retry_n_ssh!");
            let _guard = span.enter();
            loop{
                match $expr {
                    Err(e)  => {
                        $(  if e.code() == $code as i32 {
                                async_sleep!(std::time::Duration::from_millis(2));
                            } else  )*
                        {
                            break Err(e)
                        }
                    }
                    res => {
                        break res
                    }
                }
            }
        }
    }
}

/// This macro allows to retry an expression it returns an error. It allows to 
/// retry commands that fails every now and then for a limited amount of time.
#[macro_export]
macro_rules! await_retry_n {
    ($expr:expr, $nb:expr) => {
       {    
            use tracing::trace_span;
            let span = trace_span!("await_retry_n!");
            let _guard = span.enter();
            let nb = $nb as usize;
            let mut i = 1 as usize;
            loop{
                match $expr {
                    Err(e)  => {
                        if i == nb {
                            break Err(e)
                        }
                        else{
                            async_sleep!(std::time::Duration::from_millis(2));
                            i += 1;
                        }
                    }
                    res => {
                        break res
                    }
                }
            }
        }
    }
}



//---------------------------------------------------------------------------------------- FUNCTIONS

/// Returns a tuple containing the git and git-lfs versions.
pub fn check_git_lfs_versions() -> Result<(String, String), crate::Error> {
    debug!("Checking git and lfs versions");
    let git_version = String::from_utf8(process::Command::new("git")
        .args(&["--version"])
        .output()?.stdout)
        .expect("Failed to parse utf8 string");
    let lfs_version = String::from_utf8(process::Command::new("git")
        .args(&["lfs", "--version"])
        .output()?.stdout)
        .expect("Failed to parse utf8 string");
    let git_regex = regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+")?;
    let git_version = String::from(git_regex.find(&git_version)
        .unwrap()
        .as_str());
    let lfs_regex = regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+")?;
    let lfs_version = String::from(lfs_regex.find(&lfs_version)
        .unwrap()
        .as_str());
    Ok((git_version, lfs_version))
}

/// Returns the absolute path to the higher expegit folder starting from `start_path`.
pub fn search_expegit_root(start_path: &path::PathBuf) -> Result<path::PathBuf, crate::Error> {
    debug!("Searching expegit repository root from {}", 
        fs::canonicalize(start_path).unwrap().to_str().unwrap());
    let start_path = fs::canonicalize(start_path)?;
    if start_path.is_file() { panic!("Should provide a folder path.") };
    // We add a dummy folder that will be popped directly to check for .
    let mut start_path = start_path.join("dummy_folder");
    while start_path.pop() {
        if start_path.join(CMPCONF_RPATH).exists() {
            debug!("Expegit root found at {}", start_path.to_str().unwrap());
            return Ok(start_path);
        }
    }
    warn!("Unable to find .expegit file in parents folders");
    Err(crate::Error::Misc(Error::InvalidRepository))
}

/// Parses a parameters string with a number of repetitions and generate a vector of parameters
/// combinations.
pub fn parse_parameters(param_string: &str, repeats: usize) -> Vec<String> {
    // We compute the products of entered parameters recursively
    fn parameters_generator(p: Vec<&str>, repeat: usize) -> Vec<String> {
        if p.len() == 1 {
            p.first()
                .unwrap()
                .split(';')
                .map::<Vec<String>, _>(|s| {
                    (0..repeat).map(|_| String::from(s.trim())).collect()
                }).flatten()
                .collect()
        } else {
            p.first()
                .unwrap()
                .split(';')
                .map::<Vec<String>, _>(|s| {
                    let mut params =
                        parameters_generator(p.split_first().unwrap().1.to_vec(), repeat);
                    params.iter_mut().for_each(|b| {
                        b.insert(0, ' ');
                        b.insert_str(0, s)
                    });
                    params
                }).flatten()
                .collect()
        }
    }
    parameters_generator(param_string.split("¤").collect(), repeats)
}

/// Returns the current hostname
pub fn get_hostname() -> Result<String, crate::Error> {
    debug!("Retrieving hostname");
    // We retrieve hostname
    let user = str::replace(&String::from_utf8(process::Command::new("id")
        .arg("-u")
        .arg("-n")
        .output()?
        .stdout)
        .unwrap(), "\n", "");
    let host = str::replace(&String::from_utf8(process::Command::new("hostname")
        .output()?
        .stdout)
        .unwrap(), "\n", "");
    Ok(format!("{}@{}", user, host))
}

use std::process::{Output};
use std::os::unix::process::ExitStatusExt;

/// Compacts a list of outputs in a single output: 
/// + The stdouts are concatenated
/// + The stderrs are concatenated
/// + The last error code is kept
pub fn compact_outputs(outputs: Vec<Output>) -> Output{
    let mut outputs = outputs;
    outputs.iter_mut()
        .fold(Output{status: ExitStatusExt::from_raw(0), stdout: Vec::new(), stderr: Vec::new()},
              |mut acc, o| {
                  acc.stdout.append(&mut o.stdout);
                  acc.stderr.append(&mut o.stderr);
                  acc.status = o.status;
                  acc
              })
} 

/// Formats commands and output properly
pub fn format_commands_outputs(commands: &Vec<String>, outputs: &Vec<Output>) -> String{
    commands.iter().zip(outputs.iter().map(|o| OutputBuf::from(o.to_owned())))
        .fold(String::new(), |mut acc, (c, o)|{
            acc.push_str(&format!("{:?} :\n    stdout: {:?}\n    stderr: {:?}\n    ecode: {}\n",
                c, o.stdout, o.stderr, o.ecode));
            acc
        })
}

//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod test {
    use std::fs;
    use std::path;
    use super::*;

    // Modify the files with the variables that suits your setup to run the test.
    static TEST_PATH: &str = "/tmp";
    static TEST_HOSTNAME: &str = "";

    #[test]
    fn test_check_git_lfs_versions() {
        check_git_lfs_versions().unwrap();
    }

    #[test]
    fn test_search_expegit_root() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/misc/search_expegit_root");
        println!("{:?}", test_path);
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
            fs::create_dir_all(&test_path.join("no/1/2")).unwrap();
            fs::create_dir_all(&test_path.join("yes/1/2")).unwrap();
            fs::File::create(&test_path.join("yes/.expegit")).unwrap();
        }
        let no_res = search_expegit_root(&test_path.join("no/1/2"));
        assert!(no_res.is_err());
        let yes_res = search_expegit_root(&test_path.join("yes/1/2"));
        assert!(yes_res.is_ok());
        assert_eq!(yes_res.unwrap(), test_path.join("yes"));
        let yes_res = search_expegit_root(&test_path.join("yes"));
        assert!(yes_res.is_ok());
        assert_eq!(yes_res.unwrap(), test_path.join("yes"));
    }
}
