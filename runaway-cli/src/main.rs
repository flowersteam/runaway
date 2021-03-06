//! runaway-cli/main.rs
//! Author: Alexandre Péré
//! 
//! Runaway command line tool. Allows to execute scripts and batches of scripts on remote hosts. 
//! Every subcommand of the application is implemented in a separate function of the `subcommands` 
//! module.


//------------------------------------------------------------------------------------------ IMPORTS

extern crate openssl_probe;

#[macro_use]
extern crate lazy_static;

use clap;
use exit::Exit;
use tracing::{self, error};


//------------------------------------------------------------------------------------------ MODULES


mod subcommands;
mod misc;
mod exit;
mod logger;


//---------------------------------------------------------------------------------------- CONSTANTS


const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
const DESC: &str = "Execute code on remote hosts.";


//--------------------------------------------------------------------------------------------- MAIN

// The application entrypoint.
fn main(){

    // We get openssl certificates
    openssl_probe::init_ssl_cert_env_vars();

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
                .short("V")
                .long("verbose")
                .help("Print more messages"))
            .arg(clap::Arg::with_name("debug-ssh")
                .long("debug-ssh")
                .help("Print ssh messages"))
            .arg(clap::Arg::with_name("trace")
                .takes_value(true)
                .long("trace")
                .help("Print trace message of the mentionned module"))
            .arg(clap::Arg::with_name("silent")
                .short("S")
                .long("silent")
                .help("Print no messages from runaway"))
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
            .arg(clap::Arg::with_name("remote-folder")
                .short("r")
                .long("remote-folder")
                .default_value("$RUNAWAY_PATH/$RUNAWAY_UUID")
                .help("Folder to deflate data in, on the remote."))
            .arg(clap::Arg::with_name("output-folder")
                .short("o")
                .long("output-folder")
                .default_value(".")
                .help("Folder to deflate data in, on local."))
            .arg(clap::Arg::with_name("no-ecode")
                .long("no-ecode")
                .help("Do not copy the remote exit code to the local command. Returns 0 whatever the script exit code."))
            .arg(clap::Arg::with_name("no-env-read")
                .long("no-env-read")
                .help("Do not read the local environment variables to apply it to the remote context."))
            .arg(clap::Arg::with_name("on-local")
                .short("L")
                .long("on-local")
                .help("This allows to reduce the transfers when the profile executes locally."))
            .arg(clap::Arg::with_name("ARGUMENTS")
                .help("Script arguments")
                .multiple(true)
                .allow_hyphen_values(true)
                .last(true)))
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
                .short("V")
                .long("verbose")
                .help("Print more messages"))
            .arg(clap::Arg::with_name("debug-ssh")
                .long("debug-ssh")
                .help("Print ssh messages"))
            .arg(clap::Arg::with_name("trace")
                .takes_value(true)
                .long("trace")
                .help("Print trace message of the mentionned module"))
            .arg(clap::Arg::with_name("silent")
                .short("S")
                .long("silent")
                .help("Print no messages from runaway"))
            .arg(clap::Arg::with_name("repeats")
                .short("x")
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
            .arg(clap::Arg::with_name("arguments-file")
                .short("A")
                .long("arguments-file")
                .takes_value(true)
                .help("A file specifying a list of newline-separated arguments. Template strings can also be used."))
            .arg(clap::Arg::with_name("outputs-file")
                .short("O")
                .long("outputs-file")
                .takes_value(true)
                .help("A file specifying a list of newline-separated output directories."))
            .arg(clap::Arg::with_name("remotes-file")
                .short("R")
                .long("remotes-file")
                .takes_value(true)
                .help("Folder to deflate data in, on local."))
            .arg(clap::Arg::with_name("output-folders")
                .short("o")
                .long("output-folders")
                .takes_value(true)
                .default_value("./batch/$RUNAWAY_UUID")
                .help("The output folders to put the results in. Template strings can be used."))
            .arg(clap::Arg::with_name("remote-folders")
                .short("r")
                .long("remote-folders")
                .default_value("$RUNAWAY_PATH/$RUNAWAY_UUID")
                .help("The folders to put the code in, on the remote. Template strings can be used."))
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
            .arg(clap::Arg::with_name("no-env-read")
                .long("no-env-read")
                .help("Do not read the local environment variables to apply it to the remote context."))
            .arg(clap::Arg::with_name("on-local")
                .short("L")
                .long("on-local")
                .help("This allows to reduce the transfers when the profile executes locally. \
                       Remote folders are overriden to follow the output folders."))
            .arg(clap::Arg::with_name("post-command")
                .short("p")
                .long("post-command")
                .default_value("cd $RUNAWAY_OUTPUT_FOLDER && \
                                echo \"$RUNAWAY_ECODE\" > ecode && \
                                echo \"$RUNAWAY_STDOUT\" > stdout && \
                                echo \"$RUNAWAY_STDERR\" > stderr ")
                .help("Bash command executed after the data were fetched to the local end. \
                       Runaway environment variables from the execution can be used. In particular \
                       we set $RUNAWAY_OUTPUT_FOLDER, $RUNAWAY_ECODE, $RUNAWAY_STDOUT and \
                       $RUNAWAY_STDERR."))
           .arg(clap::Arg::with_name("post-script")
                .short("P")
                .long("post-script")
                .takes_value(true)
                .help("Bash script to execute instead of post-proc-command, after the data were \
                       fetched to the local end."))
           .arg(clap::Arg::with_name("ARGUMENTS")
                .help("Script argument string. Template strings can be used.")
                .multiple(true)
                .allow_hyphen_values(true)
                .last(true)))
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
                .help("Search command to use to schedule experiment parameters.")
                .required(true))
            .arg(clap::Arg::with_name("verbose")
                .short("V")
                .long("verbose")
                .help("Print more messages"))
            .arg(clap::Arg::with_name("debug-ssh")
                .long("debug-ssh")
                .help("Print ssh messages"))
            .arg(clap::Arg::with_name("trace")
                .takes_value(true)
                .long("trace")
                .help("Print trace message of the mentionned module"))
            .arg(clap::Arg::with_name("silent")
                .short("S")
                .long("silent")
                .help("Print no messages from runaway"))
            .arg(clap::Arg::with_name("leave")
                .long("leave")
                .takes_value(true)
                .possible_value("nothing")
                .possible_value("code")
                .possible_value("everything")
                .default_value("nothing")
                .help("What to leave on the remote host after execution"))
            .arg(clap::Arg::with_name("output-folders")
                .short("o")
                .long("output-folders")
                .takes_value(true)
                .default_value("./batch/$RUNAWAY_UUID")
                .help("The output folders to put the results in. Template strings can be used."))
            .arg(clap::Arg::with_name("remote-folders")
                .short("r")
                .long("remote-folders")
                .default_value("$RUNAWAY_PATH/$RUNAWAY_UUID")
                .help("The folders to put the code in, on the remote. Template strings can be used."))
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
            .arg(clap::Arg::with_name("no-env-read")
                .long("no-env-read")
                .help("Do not read the local environment variables to apply it to the remote context."))
            .arg(clap::Arg::with_name("on-local")
                .short("L")
                .long("on-local")
                .help("This allows to reduce the transfers when the profile executes locally. \
                       Remote folders are overriden to follow the output folders."))
            .arg(clap::Arg::with_name("post-command")
                .short("p")
                .long("post-command")
                .default_value("cd $RUNAWAY_OUTPUT_FOLDER && \
                                echo \"$RUNAWAY_ECODE\" > ecode && \
                                echo \"$RUNAWAY_STDOUT\" > stdout && \
                                echo \"$RUNAWAY_STDERR\" > stderr && \
                                echo \"$RUNAWAY_FEATURES\" > features ")
                .help("Bash command executed after the data were fetched to the local end. \
                       Runaway environment variables from the execution can be used. In particular \
                       we set $RUNAWAY_OUTPUT_FOLDER, $RUNAWAY_ECODE, $RUNAWAY_STDOUT, \
                       $RUNAWAY_STDERR and $RUNAWAY_FEATURES."))
           .arg(clap::Arg::with_name("post-script")
                .short("P")
                .long("post-script")
                .takes_value(true)
                .help("Bash script to execute instead of post-proc-command, after the data were \
                       fetched to the local end."))
            .arg(clap::Arg::with_name("stop-on-fail")
                .long("stop-on-fail")
                .help(""))
        );

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

    // We dispatch to subcommands and exit;
    let output;
    if let Some(matches) = matches.subcommand_matches("exec"){
        output = subcommands::exec(matches.clone());
    } else if let Some(matches) = matches.subcommand_matches("batch"){
        output = subcommands::batch(matches.clone());
    } else if let Some(_) = matches.subcommand_matches("install-completion"){
        output = subcommands::install_completion(application);
    } else if let Some(matches) = matches.subcommand_matches("sched"){
        output = subcommands::sched(matches.clone());
    } else {
        output = Ok(Exit::AllGood);
    }

    // Depending on the output, we return a different message
    let exit = match output{
        Ok(Exit::AllGood) => Exit::AllGood.into(),
        Ok(Exit::ScriptFailedWithCode(e)) => {
            error!("Script execution failed with exit code {}", e);
            e
        }
        Ok(Exit::ScriptFailedWithoutCode) => {
            error!("Script execution failed without returning exit code");
            Exit::ScriptFailedWithoutCode.into()
        }
        Ok(Exit::SomeExecutionFailed(nb)) => {
            error!("{} executions failed.", nb);
            Exit::SomeExecutionFailed(nb).into()
        }
        Ok(_) => unreachable!(),
        Err(e) => {
            error!("Runaway experienced an error: {}", e);
            e.into()
        }
    };
    std::process::exit(exit);
}