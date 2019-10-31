//! runaway-cli/subcommands/misc.rs
//! 
//! This module contains miscellaneous functions used in various subcommands. 


//-------------------------------------------------------------------------------------------IMPORTS


use dirs;
use ctrlc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use liborchestra::{
    PROFILES_FOLDER_RPATH};
use liborchestra::hosts::{HostConf, HostHandle};
use clap;
use crate::exit::Exit;
use crate::logger::EchoSubscriber;
use liborchestra::primitives::{read_globs_from_file, list_local_folder, Glob};
use liborchestra::commons::{EnvironmentStore, EnvironmentKey, EnvironmentValue};
use liborchestra::scheduler::SchedulerHandle;
use itertools::Itertools;
use tracing::{self, info, error};
use std::env;


//------------------------------------------------------------------------------------------ STATICS


lazy_static!{
    pub static ref NO_COLORS: bool = env::var("NO_COLOR").is_ok();
}


//-------------------------------------------------------------------------------------------- MACRO


/// This macro allows to format a text with ansi color sequence added to color the text.
#[macro_export]
macro_rules! color{
    ($color:tt, $($arg:tt)*) => {
        if *$crate::misc::NO_COLORS{
            format!($($arg)*)
        } else{
            format!("\x1B[38;5;{}m{}\x1B[0m", $color, format_args!($($arg)*))
        }
    }
}

/// This macro allows to execute a `Result` expression. On error, it prints an error message to the 
/// user, and returns an error code. On ok, it unwraps the value.
#[macro_export]
macro_rules! try_return_code {
    ($result:expr, $text:expr, $ecode:expr) => {
        match $result{
            Ok(h) => h,
            Err(e) => {
                error!("{}: {}", $text, e);
                return $ecode;
            }
        };
    }
}

/// This macro allows to execute a `Result` expression. On error, the error is printed, and mapped 
/// to the provided exit.
#[macro_export]
macro_rules! to_exit {
    ($result:expr, $exit:expr) => {
        match $result{
            Ok(h) => Ok(h),
            Err(e) => {
                error!("{}", e);
                Err($exit)
            }
        };
    }
}

/// This macro allows to execute a `Result` expression. On error, the containing function returns 
/// the error. On ok, it unwraps the value.
#[macro_export]
macro_rules! try_return_err {
    ($result:expr) => {
        match $result{
            Ok(h) => h,
            Err(e) => {
                return Err(e);
            }
        };
    }
}


//---------------------------------------------------------------------------------------- FUNCTIONS


/// Allows to load host from a host path configuration
pub fn get_host(host_name: &str) -> Result<HostHandle, Exit>{
    let host_path = get_host_path(host_name);
    let config = to_exit!(HostConf::from_file(&host_path), Exit::LoadHostConfiguration)?;
    to_exit!(HostHandle::spawn(config), Exit::SpawnHost)
}

/// Allows to generate globs from send and fetch ignore file.
pub fn get_send_fetch_ignores_globs(root: &PathBuf, send_path: &str, fetch_path: &str) 
        -> Result<(Vec<Glob<String>>, Vec<Glob<String>>), Exit>{
    // We get the paths
    let send_ignore_path = PathBuf::from(send_path);
    let fetch_ignore_path = PathBuf::from(fetch_path);
    // Depending on the case, we return a different set of globs.
    if send_ignore_path.exists() && fetch_ignore_path.exists(){
        let send_globs = to_exit!(read_globs_from_file(&send_ignore_path.canonicalize().unwrap()), 
            Exit::SendIgnoreRead)?;
        let fetch_globs = to_exit!(read_globs_from_file(&fetch_ignore_path.canonicalize().unwrap()), 
            Exit::FetchIgnoreRead)?;
        Ok((send_globs, fetch_globs))
    } else if send_ignore_path.exists() {
        let send_globs = to_exit!(read_globs_from_file(&send_ignore_path.canonicalize().unwrap()), 
            Exit::SendIgnoreRead)?;
        let include_globs = vec!();
        let fetch_globs = to_exit!(list_local_folder(root, &send_globs, &include_globs), 
            Exit::ReadLocalFolder)?;
        let fetch_globs = fetch_globs.iter()
            .map(AsRef::as_ref)
            .map(Path::to_str)
            .map(Option::unwrap)
            .map(ToOwned::to_owned)
            .map(Glob)
            .collect();
        Ok((send_globs, fetch_globs))
    } else if fetch_ignore_path.exists() {
        let send_globs = vec!();
        let fetch_globs = to_exit!(read_globs_from_file(&fetch_ignore_path.canonicalize().unwrap()), 
            Exit::FetchIgnoreRead)?;
        Ok((send_globs, fetch_globs))
    } else {
        Ok((vec!(), vec!()))
    }
}

/// Allows to expand a template string into a set of strings
pub fn expand_template_string(param_string: &str) -> Vec<String> {

    fn cut(p: &str, cut: &str, ig_left: char, ig_right: char) -> Vec<String> {
        p.split(cut)
            .map(|s| s.trim()
                .trim_start_matches(ig_left)
                .trim_end_matches(ig_right)
                .into())
            .collect()
    }

    cut(param_string, "+", '{', '}').iter()
        .map(|s| cut(s, ";", '\'', '\''))
        .multi_cartesian_product()
        .map(|v| v.join(""))
        .collect()
}


// When the user wants to leave runaway, it will likely hit Ctrl-C. In this case, we may need to clean
// a few things before leaving. For instance, the allocation should always be revoked before leaving
// to avoid spending credit for nothing. Also, when using a scheduler, we would need to shut it down 
// before leaving. All that is handled by the following Ctrl-C handler.
pub fn install_ctrlc_handler(signal_host: Option<HostHandle>, signal_scheduler: Option<SchedulerHandle>){

    // We have to downgrade the handle to the host, because the ctrl-c handler is not dropped during
    // program execution. This would prevent the host from being dropped by the dropper. For this 
    // reason we have to downgrade the dropper. 
    let mut signal_host = signal_host;
    if let Some(host) = signal_host.as_mut() { host.downgrade() }
    let mut signal_scheduler = signal_scheduler;
    if let Some(sched) = signal_scheduler.as_mut() { sched.downgrade() }
   
    // We set the ctrl c handler
    let signal_counter = AtomicUsize::new(0);

    ctrlc::set_handler(move || {
        info!("Received ctrl-c.");
        let mut host = signal_host.clone();
        let mut sched = signal_scheduler.clone();
        let n_ctrlc = signal_counter.fetch_add(1, Ordering::SeqCst);
        match n_ctrlc{
            0 => {
                if let Some(sched) = sched.as_mut() {
                    futures::executor::block_on(sched.async_shutdown())
                        .unwrap_or_else(|e| error!("Failed to shutdown scheduler: {}", e));
                    info!("Scheduler shutdown.") 
                }
                if let Some(host) = host.as_mut(){ 
                    futures::executor::block_on(host.async_abort())
                        .unwrap_or_else(|e| error!("Failed to abort host: {}", e));
                    info!("Host aborted. Waiting for running execution ... ");
                }
            }
            1 => {
                if let Some(host) = host.as_mut(){ 
                    futures::executor::block_on(host.async_shutdown())
                        .unwrap_or_else(|e| error!("Failed to shutdown host: {}", e));
                    info!("Shutting host down. Execution were not awaited. Saving execution data.");
                }
            }
            2 => {
                info!("Data were not saved. Leaving.");
                std::process::exit(900);
            }
            _ => {
                std::process::exit(901);
            }
        }
    }).unwrap();
}


/// Returns current shell.
pub fn which_shell() -> Result<clap::Shell, String>{
    let shell = std::env::var("SHELL")
        .unwrap()
        .split("/")
        .map(|a| a.to_owned())
        .last()
        .unwrap();
    match shell.as_ref(){
        "zsh" => Ok(clap::Shell::Zsh),
        "bash" => Ok(clap::Shell::Bash),
        shell => Err(shell.into()),
    }
}


/// Returns the name of the binary.
pub fn get_bin_name() -> String{
    std::env::args()
        .next()
        .unwrap()
        .split("/")
        .map(|a| a.to_owned())
        .collect::<Vec<String>>()
        .last()
        .unwrap()
        .to_owned()
}

/// Generates zsh completion
pub fn generate_zsh_completion(application: clap::App) {
    let bin_name = get_bin_name();
    let file_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("_{}", &bin_name));
    if !file_path.exists(){ return }
    let mut application = application;
    std::fs::remove_file(&file_path).unwrap();
    application.gen_completions(bin_name, clap::Shell::Zsh, file_path.parent().unwrap());
    std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Generates bash completion
pub fn generate_bash_completion(application: clap::App) {
    let bin_name = get_bin_name();
    let file_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("{}.bash", &bin_name));
    if !file_path.exists(){ return }
    let mut application = application;
    std::fs::remove_file(&file_path).unwrap();
    application.gen_completions(bin_name, clap::Shell::Bash, file_path.parent().unwrap());
    std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Retrieves available profiles.
pub fn get_available_profiles() -> Vec<String>{
    std::fs::read_dir(dirs::home_dir().unwrap().join(PROFILES_FOLDER_RPATH))
        .unwrap()
        .map(|a| a.unwrap().file_name().into_string().unwrap())
        .filter_map(|a| if a.contains(".yml"){Some(a.replace(".yml", ""))} else { None })
        .collect::<Vec<_>>()
}

/// Initializes the logger based on the matches 
pub fn init_logger(matches: &clap::ArgMatches) {

    if matches.is_present("silent"){
    } else if matches.is_present("verbose"){
        EchoSubscriber::setup_verbose();
    } else {
        EchoSubscriber::setup_normal();
    }
}

/// Returns the path to the host config file.
pub fn get_host_path(name: &str) -> PathBuf{
    dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("{}.yml", name))
}


// Read environment variables starting with "RUNAWAY" and puts them in an environment store
pub fn read_local_runaway_envs() -> EnvironmentStore{
    let store = EnvironmentStore::new();
    std::env::vars()
        .filter(|(k, _)| k.starts_with("RUNAWAY"))
        .fold(store, |mut store, (k,v)| {
            store.insert(EnvironmentKey(k), EnvironmentValue(v));
            store
        })
}