//! runaway-cli/subcommands/complete.rs
//! Author: Alexandre Péré
//! 
//! This module contains the batch subcommand. 


//-------------------------------------------------------------------------------------------IMPORTS


use liborchestra::PROFILES_FOLDER_RPATH;
use clap;
use crate::exit::Exit;
use crate::misc;
use std::io::Write;
use tracing::{self, error, info};
use std::os::unix::fs::PermissionsExt;


//--------------------------------------------------------------------------------------- SUBCOMMAND


// Output completion for profiles.
pub fn install_completion(application: clap::App) -> Result<Exit, Exit>{
    // We initialize the logger
    let matches = application.clone().get_matches();
    misc::init_logger(&matches);
    match which_shell(){
        Ok(clap::Shell::Zsh) => {
            info!("Zsh recognized. Proceeding.");
            let res = std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(dirs::home_dir().unwrap().join(".zshrc"));
            let mut file = match res{
                Ok(f) => f,
                Err(e) =>{
                    error!("Impossible to open .zshrc file: {}", e);
                    return Err(Exit::InstallCompletion);
                } 
            };
            file.write_all("\n# Added by Runaway for zsh completion\n".as_bytes()).unwrap();
            file.write_all(format!("fpath=(~/{} $fpath)\n", PROFILES_FOLDER_RPATH).as_bytes()).unwrap();
            file.write_all("autoload -U compinit\ncompinit".as_bytes()).unwrap();
            generate_zsh_completion(application);
            info!("Zsh completion installed. Open a new terminal to use it.");
        }
        Ok(clap::Shell::Bash) => {
            info!("Bash recognized. Proceeding.");
            let res = std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(dirs::home_dir().unwrap().join(".bashrc"));
            let mut file = match res{
                Ok(f) => f,
                Err(e) =>{
                    error!("Impossible to open .bashrc file: {}", e);
                    return Err(Exit::InstallCompletion);
                } 
            };
            file.write_all("\n# Added by Runaway for bash completion\n".as_bytes()).unwrap();
            file.write_all(format!("source ~/{}/{}.bash\n", 
                                    PROFILES_FOLDER_RPATH, 
                                    get_bin_name()).as_bytes()).unwrap();
            generate_bash_completion(application);
            info!("Bash completion installed. Open a new terminal to use it.");
        }
        Err(e) => {
            error!("Shell {} is not available for completion. Use bash or zsh.", e);
            return Err(Exit::InstallCompletion);
        }
        _ => unreachable!()
    }
    return Ok(Exit::AllGood)
}


// Returns current shell.
fn which_shell() -> Result<clap::Shell, String>{
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

// Returns the name of the binary.
fn get_bin_name() -> String{
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

// Generates zsh completion
fn generate_zsh_completion(application: clap::App) {
    let bin_name = get_bin_name();
    let file_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("_{}", &bin_name));
    let mut application = application;
    match std::fs::remove_file(&file_path){
        Ok(_) => info!("Replacing completion file"),
        Err(_) => info!("No completion file existed"),
    };
    application.gen_completions(bin_name, clap::Shell::Zsh, file_path.parent().unwrap());
    std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// Generates bash completion
fn generate_bash_completion(application: clap::App) {
    let bin_name = get_bin_name();
    let file_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("{}.bash", &bin_name));
    let mut application = application;
    match std::fs::remove_file(&file_path){
        Ok(_) => info!("Replacing completion file"),
        Err(_) => info!("No completion file existed"),
    }
    application.gen_completions(bin_name, clap::Shell::Bash, file_path.parent().unwrap());
    std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

