//! runaway-cli/subcommands/misc.rs
//! 
//! This module contains miscellaneous functions used in various subcommands. 


//-------------------------------------------------------------------------------------------IMPORTS


use liborchestra::PROFILES_FOLDER_RPATH;
use liborchestra::hosts::HostHandle;
use clap;
use dirs;
use env_logger;
use ctrlc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::os::unix::fs::PermissionsExt;


//----------------------------------------------------------------------------------------- FUNCTIONS


/// Allows to parse cartesian product strings to generate a set of parameters. 
pub fn parse_parameters(param_string: &str, repeats: usize) -> Vec<String> {
    // We compute the products of entered parameters recursively
    fn parameters_generator(p: Vec<&str>, repeat: usize) -> Vec<String> {
        if p.len() == 1 {
            p.first()
                .unwrap()
                .split('|')
                .map::<Vec<String>, _>(|s| {
                    (0..repeat).map(|_| String::from(s.trim())).collect()
                }).flatten()
                .collect()
        } else {
            p.first()
                .unwrap()
                .split('|')
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
    parameters_generator(param_string.split("&").collect(), repeats)
}

/// Installs a ctrlc handler that takes care about cancelling the allocation on the host before 
/// leaving. 
pub fn install_ctrlc_handler(signal_host: HostHandle){
    // We have to downgrade the handle to the host, because the ctrl-c handler is not dropped during
    // program execution. This would prevent the host from being dropped by the dropper. For this 
    // reason we have to downgrade the dropper. 
    let mut signal_host = signal_host;
    signal_host.downgrade();

    // We set the ctrl c handler
    let signal_counter = AtomicUsize::new(0);
    ctrlc::set_handler(move || {
        eprintln!("runaway: received ctrl-c.");
        let host = signal_host.clone();
        let n_ctrlc = signal_counter.fetch_add(1, Ordering::SeqCst);
        match n_ctrlc{
            0 => {
                futures::executor::block_on(host.async_abort()).unwrap();
                eprintln!("runaway: waiting for running execution to finish");
            }
            1 => {
                futures::executor::block_on(host.async_shutdown()).unwrap();
                eprintln!("runaway: host shutdown. Saving execution data.");
            }
            2 => {
                eprintln!("runaway: you want to quit too hard. Leaving.");
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

    if matches.is_present("vvverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=TRACE,liborchestra=TRACE,liborchestra::ssh=DEBUG");
    } else if matches.is_present("vverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=DEBUG,liborchestra=DEBUG,liborchestra::ssh=INFO");
    } else if matches.is_present("verbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=INFO,liborchestra=INFO,liborchestra::ssh=INFO");
    }

    env_logger::init();
}