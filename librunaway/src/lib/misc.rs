//! A few miscellaneous functions available library wide.


//------------------------------------------------------------------------------------------ IMPORTS


use std::{process, path, fs, error, fmt};
use regex;
use super::CMPCONF_RPATH;
use tracing::{self, warn, debug};
use tracing_subscriber::fmt::Subscriber;
use tracing::Level;
use std::process::{Output};
use std::os::unix::process::ExitStatusExt;
use super::commons::OutputBuf;
use super::timer::TimerHandle;
use uuid::Uuid;


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

/// Initializes tracing
pub fn init_tracing(level: tracing::Level, env: String){
    let subscriber = Subscriber::builder()
        .compact()
        .with_max_level(level)
        .with_env_filter(env.as_str())
        .without_time()
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber for tracing.");
}

/// Retrieves user name
pub fn get_user() -> String{
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("echo $USER")
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().replace("\n", "")
}

/// Get a uuid string
pub fn get_uuid() -> String {
    let uuid = Uuid::new_v4();
    format!("{}", uuid)
}
