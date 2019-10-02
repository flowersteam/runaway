//! runaway-cli/main.rs
//! Author: Alexandre Péré
//! 
//! Runaway command line tool. Allows to execute scripts and batches of scripts on remote hosts. 
//! Every subcommand of the application is implemented in a separate function of the `subcommands` 
//! module.


//------------------------------------------------------------------------------------------ IMPORTS


#![feature(async_await, futures_api)]
use clap;


//------------------------------------------------------------------------------------------ MODULES

mod subcommands;
mod misc;
mod exit;

//---------------------------------------------------------------------------------------- CONSTANTS

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
const DESC: &str = "Execute code on remote hosts.";


//--------------------------------------------------------------------------------------------- MAIN

// The application entrypoint.
fn main(){

    // We get available profiles
    let profiles = misc::get_available_profiles();

    // We define the arguments parser
    let application = clap::App::new(NAME)
        .version(VERSION)
        .about(DESC)
        .author(AUTHOR)
        .setting(clap::AppSettings::ArgRequiredElseHelp) 
        .about("Execute code on remote hosts")
        .subcommand(clap::SubCommand::with_name("install-completion")
            .about("Install bash completion script"))
        .subcommand(clap::SubCommand::with_name("exec")
            .about("Runs a single execution on a remote host")
            .arg(clap::Arg::with_name("REMOTE")
                .help("Name of remote profile to execute script with")
                .possible_values(&profiles.iter().map(|a| a.as_str()).collect::<Vec<_>>()[..])
                .required(true))
            .arg(clap::Arg::with_name("SCRIPT")
                .help("File name of the script to be executed")
                .required(true))
            .arg(clap::Arg::with_name("verbose")
                .long("verbose")
                .help("Print light logs"))
            .arg(clap::Arg::with_name("vverbose")
                .long("vverbose")
                .help("Print logs"))
            .arg(clap::Arg::with_name("vvverbose")
                .long("vvverbose")
                .help("Print all logs"))
            .arg(clap::Arg::with_name("leave-tars")
                .long("leave-tars")
                .help("Leave transfered tar files to debug .*ignore files."))
            .arg(clap::Arg::with_name("send-ignore")
                .short("s")
                .long("send-ignore")
                .default_value(".sendignore")
                .help("File containing glob patterns used to ignore files when sending data."))
            .arg(clap::Arg::with_name("fetch-ignore")
                .short("f")
                .long("fetch-ignore")
                .default_value(".fetchignore")
                .help("File containing glob patterns used to ignore files when fetching data."))
            .arg(clap::Arg::with_name("leave")
                .short("l")
                .long("leave")
                .takes_value(true)
                .possible_value("nothing")
                .possible_value("code")
                .possible_value("everything")
                .default_value("nothing")
                .help("What to leave on the remote host after execution"))
           .arg(clap::Arg::with_name("parameters")
                .help("Script parameters. In normal mode, it should be written as they would \
                       be for the program to execute. In batch mode, you can use a product \
                       parameters string.")
                .multiple(true)
                .allow_hyphen_values(true)
                .last(true))
        )
        .subcommand(clap::SubCommand::with_name("batch")
            .about("Runs a batch of executions on a remote host")
            .arg(clap::Arg::with_name("REMOTE")
                .help("Name of remote profile to execute script with")
                .possible_values(&profiles.iter().map(|a| a.as_str()).collect::<Vec<_>>()[..])
                .required(true))
            .arg(clap::Arg::with_name("SCRIPT")
                .help("File name of the script to be executed")
                .required(true)) 
            .arg(clap::Arg::with_name("verbose")
                .long("verbose")
                .help("Print light logs"))
            .arg(clap::Arg::with_name("vverbose")
                .long("vverbose")
                .help("Print logs"))
            .arg(clap::Arg::with_name("vvverbose")
                .long("vvverbose")
                .help("Print all logs"))
            .arg(clap::Arg::with_name("benchmark")
                .long("benchmark")
                .help("Print only allocations and executions messages for statistics purposes."))
            .arg(clap::Arg::with_name("leave-tars")
                .long("leave-tars")
                .help("Leave transfered tar files to debug .*ignore files."))
            .arg(clap::Arg::with_name("repeats")
                .short("R")
                .long("repeats")
                .takes_value(true)
                .default_value("1")
                .help("The number of time every parameter must be repeated. Used with product string."))
            .arg(clap::Arg::with_name("leave")
                .short("l")
                .long("leave")
                .takes_value(true)
                .possible_value("nothing")
                .possible_value("code")
                .possible_value("everything")
                .default_value("nothing")
                .help("What to leave on the remote host after execution"))
            .arg(clap::Arg::with_name("parameters_file")
                .short("f")
                .long("parameters_file")
                .takes_value(true)
                .help("A file specifying a list of newline-separated arguments."))
            .arg(clap::Arg::with_name("outputs_file")
                .short("O")
                .long("outputs_file")
                .takes_value(true)
                .help("A file specifying a list of newline-separated output directories."))
            .arg(clap::Arg::with_name("output_folder")
                .short("o")
                .long("output_folder")
                .takes_value(true)
                .default_value("batch")
                .help("The output folder to put the executions result in."))
            .arg(clap::Arg::with_name("send-ignore")
                .short("s")
                .long("send-ignore")
                .default_value(".sendignore")
                .help("File containing glob patterns used to ignore files when sending data."))
            .arg(clap::Arg::with_name("fetch-ignore")
                .short("f")
                .long("fetch-ignore")
                .default_value(".fetchignore")
                .help("File containing glob patterns used to ignore files when fetching data."))
           .arg(clap::Arg::with_name("parameters_string")
                .help("Script parameters product string.")
                .multiple(true)
                .allow_hyphen_values(true)
                .last(true))
        )
        .subcommand(clap::SubCommand::with_name("sched")
            .about("Use an online scheduler to optimize / explore experiment results.")
            .arg(clap::Arg::with_name("REMOTE")
                .help("Name of remote profile to execute script with")
                .possible_values(&profiles.iter().map(|a| a.as_str()).collect::<Vec<_>>()[..])
                .required(true))
            .arg(clap::Arg::with_name("SCRIPT")
                .help("File name of the script to be executed")
                .required(true)) 
            .arg(clap::Arg::with_name("SCHEDULER")
                .help("Search command to use to schedule experiment parameters."))
            .arg(clap::Arg::with_name("verbose")
                .long("verbose")
                .help("Print light logs"))
            .arg(clap::Arg::with_name("vverbose")
                .long("vverbose")
                .help("Print logs"))
            .arg(clap::Arg::with_name("vvverbose")
                .long("vvverbose")
                .help("Print all logs"))
            .arg(clap::Arg::with_name("leave")
                .long("leave")
                .takes_value(true)
                .possible_value("nothing")
                .possible_value("code")
                .possible_value("everything")
                .default_value("nothing")
                .help("What to leave on the remote host after execution"))
            .arg(clap::Arg::with_name("send-ignore")
                .short("s")
                .long("send-ignore")
                .default_value(".sendignore")
                .help("File containing glob patterns used to ignore files when sending data."))
            .arg(clap::Arg::with_name("fetch-ignore")
                .short("f")
                .long("fetch-ignore")
                .default_value(".fetchignore")
                .help("File containing glob patterns used to ignore files when fetching data."))
            .arg(clap::Arg::with_name("output_folder")
                .short("o")
                .long("output_folder")
                .takes_value(true)
                .default_value("batch")
                .help("The output folder to put the executions result in."))
        )
        .subcommand(clap::SubCommand::with_name("test")
             .about("Tests a remote profile")
            .arg(clap::Arg::with_name("verbose")
                .long("verbose")
                .help("Print light logs"))
             .arg(clap::Arg::with_name("FILE")
                 .help("The yaml profile to test.")
                 .index(1)
                 .required(true)));

    // If the completion_file already exists, we update it to account for the new available profiles
    match misc::which_shell(){
        Ok(clap::Shell::Zsh) => {
            misc::generate_zsh_completion(application.clone());
        }
        Ok(clap::Shell::Bash) => {
            misc::generate_bash_completion(application.clone());
        }
        Err(_) => {},
        _ => unreachable!()
    }

    // We parse the arguments, keeping the application untouched if we want to generate the 
    // completion files. 
    let matches = application.clone().get_matches();

    // We rispatch to subcommands and exit;
    if let Some(matches) = matches.subcommand_matches("test"){
        //std::process::exit(subcommands::test(matches));
        std::process::exit(exit::Exit::AllGood.into());
    } else if let Some(matches) = matches.subcommand_matches("exec"){
        std::process::exit(subcommands::exec(matches).into());
    } else if let Some(matches) = matches.subcommand_matches("batch"){
        //std::process::exit(subcommands::batch(matches));
        std::process::exit(exit::Exit::AllGood.into());
    } else if let Some(_) = matches.subcommand_matches("install-completion"){
        //std::process::exit(subcommands::install_completion(application));
        std::process::exit(exit::Exit::AllGood.into());
    } else if let Some(matches) = matches.subcommand_matches("sched"){
        //std::process::exit(subcommands::sched(matches));
        std::process::exit(exit::Exit::AllGood.into());
    }
}