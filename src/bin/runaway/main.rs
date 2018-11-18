// runaway/main.rs
// Author: Alexandre Péré
///
/// Runaway command line tool. Allows to execute a script on a remote host.

// CRATES
extern crate liborchestra;
extern crate clap;
extern crate pretty_logger;
extern crate log;
extern crate yaml_rust;
extern crate uuid;

// IMPORTS
use std::path;

// CONSTANTS
const NAME: &'static str = "runaway";
const VERSION: &'static str = env!("CARGO_PKG_VERSION");
const AUTHOR: &'static str = env!("CARGO_PKG_AUTHORS");
const DESC: &'static str = "Execute code on remote hosts.";

fn main(){
    // We parse the arguments
    let matches = clap::App::new(NAME)
        .version(VERSION)
        .about(DESC)
        .author(AUTHOR)
        .about("Execute code on remote hosts")
        .arg(clap::Arg::with_name("SCRIPT")
            .help("File name of the script to be executed")
            .index(2)
            .required(true))
        .arg(clap::Arg::with_name("REMOTE")
            .help("Name of remote profile to execute script with")
            .index(1)
            .required(true))
        .arg(clap::Arg::with_name("verbose")
            .short("v")
            .long("verbose")
            .help("Print logs"))
        .arg(clap::Arg::with_name("leave")
            .short("l")
            .long("leave")
            .takes_value(true)
            .possible_value("nothing")
            .possible_value("code")
            .possible_value("everything")
            .required(true)
            .default_value("everything")
            .help("What to leave on the remote host after execution"))
        .arg(clap::Arg::with_name("parameters")
                    .help("Script parameters written as they would for the program to execute")
                    .multiple(true)
                    .allow_hyphen_values(true)
                    .last(true))
        .get_matches();

    // We initialize the logger.
    if matches.is_present("verbose"){
        pretty_logger::init(pretty_logger::Destination::Stdout,
                        log::LogLevelFilter::Debug,
                        pretty_logger::Theme::default()).unwrap();
    }

    // We construct the config
    let config = liborchestra::run::RunConfig{
        script_path: path::PathBuf::from(matches.value_of("SCRIPT").unwrap()),
        profile: matches.value_of("REMOTE").unwrap().to_owned(),
        parameters: match matches.value_of("parameters"){
            Some(s) => String::from(s),
            None => String::new(),
        }
    };

    // We execute the configuration
    match config.execute(liborchestra::run::LeaveConfig::from(matches.value_of("leave").unwrap()), false) {
        Err(liborchestra::Error::ProfileError) => {
            eprintln!("runaway: malformed profile encountered");
            std::process::exit(2);
        },
        Err(liborchestra::Error::Packing) => {
            eprintln!("runaway: failed to pack data");
            std::process::exit(4);
        },
        Err(liborchestra::Error::Unpacking) => {
            eprintln!("runaway: failed to unpack data");
            std::process::exit(5);
        },
        Err(liborchestra::Error::ScpSend) => {
            eprintln!("runaway: failed to send data");
            std::process::exit(6);
        },
        Err(liborchestra::Error::ScpFetch) => {
            eprintln!("runaway: failed to fetch data");
            std::process::exit(7);
        },
        Err(liborchestra::Error::ExecutionFailed(output)) => {
            eprintln!("runaway: error occured: {}", output);
            std::process::exit(output.status.code().unwrap_or(8));
        }
        Ok(output) => std::process::exit(output.status.code().unwrap_or(0)),
    }
}

