// expegit/main.rs
//
// Author: Alexandre Péré
/// The expegit command line tool. Allows to manipulate an expegit repository.

// CRATES
extern crate liborchestra;
extern crate log;
extern crate pretty_logger;
extern crate clap;

// IMPORTS
use std::{env, fs, path};
use liborchestra::{repository, misc, Error};

// TODO: Allows multiple executions as arguments for rm

// CONSTANTS
const NAME: &'static str = "expegit";
const VERSION: &'static str = env!("CARGO_PKG_VERSION");
const AUTHOR: &'static str = env!("CARGO_PKG_AUTHORS");
const DESC: &'static str = "Organise, archive, and collaborate on your experimental campaign with git.";

// MAIN
fn main(){
    // We parse the arguments
    let matches = clap::App::new(NAME)
        .version(VERSION)
        .about(DESC)
        .author(AUTHOR)
        .setting(clap::AppSettings::SubcommandRequired)
        .setting(clap::AppSettings::GlobalVersion)
        .arg(clap::Arg::with_name("verbose")
            .help("Prints debug messages")
            .long("verbose")
            .short("v"))
        .subcommand(clap::SubCommand::with_name("init")
            .about("Initializes campaign repository")
            .arg(clap::Arg::with_name("URL")
                .help("Url of the experiment repository")
                .index(1)
                .required(true))
            .arg(clap::Arg::with_name("PATH")
                .help("Path to the local campaign repository")
                .index(2)
                .default_value(".")
                .required(true))
            .arg(clap::Arg::with_name("push")
                .short("p")
                .long("push")
                .help("Pushes the modifications to distant branch")))
        .subcommand(clap::SubCommand::with_name("push")
            .about("Pushes executions to remote"))
        .subcommand(clap::SubCommand::with_name("pull")
            .about("Updates executions from remote"))
        .subcommand(clap::SubCommand::with_name("clone")
            .about("Clones existing campaign repository")
            .arg(clap::Arg::with_name("URL")
                .help("Url of the campaign repository")
                .index(1)
                .required(true))
            .arg(clap::Arg::with_name("PATH")
                .help("Path to clone repository in")
                .index(2)
                .default_value(".")
                .required(true)))
        .subcommand(clap::SubCommand::with_name("update-expe")
            .about("Fetch the changes on experiment repository")
            .arg(clap::Arg::with_name("push")
                .short("p")
                .long("push")
                .help("Pushes the modifications to distant branch")))
        .subcommand(clap::SubCommand::with_name("exec")
            .about("Manages executions")
            .setting(clap::AppSettings::SubcommandRequired)
            .subcommand(clap::SubCommand::with_name("new")
                .about("Creates a new execution")
                .arg(clap::Arg::with_name("push")
                    .short("p")
                    .long("push")
                    .help("Pushes the modifications to distant branch"))
                .arg(clap::Arg::with_name("COMMIT")
                    .help("Hash value of the commit")
                    .default_value("")
                    .required(true))
                .arg(clap::Arg::with_name("parameters")
                    .help("Script parameters written as they would for the program to start")
                    .multiple(true)
                    .allow_hyphen_values(true)
                    .last(true)))
            .subcommand(clap::SubCommand::with_name("reset")
                .about("Resets an execution")
                .arg(clap::Arg::with_name("IDENTIFIER")
                    .help("Identifier of the execution to reset")
                    .required(true))
                .arg(clap::Arg::with_name("push")
                    .short("p")
                    .long("push")
                    .help("Pushes the modifications to distant branch")))
            .subcommand(clap::SubCommand::with_name("remove")
                .about("Removes an execution")
                .arg(clap::Arg::with_name("IDENTIFIER")
                    .help("Identifier of the execution to remove")
                    .required(true))
                .arg(clap::Arg::with_name("push")
                    .short("p")
                    .long("push")
                    .help("Pushes the modifications to distant branch")))
            .subcommand(clap::SubCommand::with_name("finish")
                .about("Finishes an execution")
                .arg(clap::Arg::with_name("IDENTIFIER")
                    .help("Identifier of the execution to finish")
                    .required(true))
                .arg(clap::Arg::with_name("push")
                    .short("p")
                    .long("push")
                    .help("Pushes the modifications to distant branch"))))
        .get_matches();

    // We initialize the logger.
    if matches.is_present("verbose"){
        pretty_logger::init(pretty_logger::Destination::Stdout,
                            log::LogLevelFilter::Debug,
                            pretty_logger::Theme::default()).unwrap();
    }

    // We check for git/lfs versions
    match misc::check_git_lfs_versions(){
        Err(err) => {
            eprintln!("expegit: failed to find Git or Lfs. {}.", err);
            std::process::exit(1);
        },
        Ok(gl) =>{
            eprintln!("expegit: using git {} and lfs {}", gl.0, gl.1);
        },
    };

    // Expegit-Init
    if let Some(matches) = matches.subcommand_matches("init"){
        let url = matches.value_of("URL").unwrap();
        let path = matches.value_of("PATH").unwrap();
        let path = fs::canonicalize(env::current_dir().unwrap().join(path)).unwrap();
        eprintln!("expegit: initializing campaign repository in {} for experiment {}", path.to_str().unwrap(), url);
        let campaign = match repository::Campaign::init(&path, url) {
            Err(Error::Git(kind)) => {
                eprintln!("expegit: an error occured while run a git command: {}", kind);
                std::process::exit(3);
            },
            Err(Error::AlreadyRepository) => {
                eprintln!("expegit: the repository was already initialized");
                std::process::exit(4);
            }
            Err(error) => {
                eprintln!("expegit: an error occurred while initialing the repository: {}", error);
                std::process::exit(5);
            }
            Ok(cmp) => {
                eprintln!("expegit: campaign {} successfully initialized", cmp);
                cmp
            }
        };
        if matches.is_present("push"){
            eprintln!("expegit: pushing changes to remote ... ");
            match campaign.push(){
                Err(err) =>{
                    eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                    std::process::exit(6);
                }
                Ok(()) => {
                    eprintln!("expegit: campaign successfully pushed");
                }
            }
        }
    }

    // Expegit-Clone
    if let Some(matches) = matches.subcommand_matches("clone"){
        eprintln!("expegit: cloning repository from {}", matches.value_of("URL").unwrap());
        match repository::Campaign::from_url(matches.value_of("URL").unwrap(),
                                 &path::PathBuf::from(matches.value_of("PATH").unwrap())){
            Err(err)=>{
                eprintln!("expegit: an error occurred during campaign cloning: {}", err);
                std::process::exit(7);
            }
            Ok(cmp)=>{
                eprintln!("expegit: campaign {} successfully cloned", cmp);
            }
        }
    }

    // Expegit-Push
    if matches.subcommand_matches("push").is_some(){
        eprintln!("expegit: pushing changes to remote ...");
        let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
            Err(_) => {
                eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                std::process::exit(8);
            },
            Ok(cmp_path)=> cmp_path,
        };
        let campaign = match repository::Campaign::from_path(&cmp_path){
            Err(err)=>{
                eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                std::process::exit(9);
            },
            Ok(cmp) => cmp,
        };
        match campaign.push(){
            Err(err) =>{
                eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                std::process::exit(10);
            }
            Ok(_) => {
                eprintln!("expegit: campaign successfully pushed");
            }
        }
    }

    // Expegit-Pull
    if matches.subcommand_matches("pull").is_some(){
        eprintln!("expegit: pulling changes from remote ...");
        let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
            Err(_) => {
                eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                std::process::exit(11);
            },
            Ok(cmp_path)=> cmp_path,
        };
        let campaign = match repository::Campaign::from_path(&cmp_path) {
            Err(err) => {
                eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                std::process::exit(12);
            },
            Ok(cmp) => cmp,
        };
        match campaign.pull(){
            Err(err)=>{
                eprintln!("expegit: an error occurred while pulling changes from remote: {}", err);
                std::process::exit(13);
            }
            Ok(_)=>{
                eprintln!("expegit: campaign succesfully pulled");
            }
        }
    }

    //Expegit-Update-Expe
    if let Some(matches) = matches.subcommand_matches("update-expe"){
        eprintln!("expegit: updating experiment from its remote ...");
        let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
            Err(_) => {
                eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                std::process::exit(21);
            },
            Ok(cmp_path)=> cmp_path,
        };
        let campaign = match repository::Campaign::from_path(&cmp_path) {
            Err(err) => {
                eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                std::process::exit(22);
            },
            Ok(cmp) => cmp,
        };
        match campaign.fetch_experiment(){
            Err(err)=>{
                eprintln!("expegit: an error occurred while pulling changes from remote: {}", err);
                std::process::exit(23);
            }
            Ok(_)=>{
                eprintln!("expegit: experiment successfully updated");
            }
        }
        if matches.is_present("push"){
            eprintln!("expegit: pushing changes to remote ... ");
            match campaign.push(){
                Err(err) =>{
                    eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                    std::process::exit(24);
                }
                Ok(()) => {
                    eprintln!("expegit: campaign successfully pushed");
                }
            }
        }
    }

    //Expegit-Exec-New
    if let Some(matches) = matches.subcommand_matches("exec"){
        if let Some(matches) = matches.subcommand_matches("new"){
            eprintln!("expegit: creating new execution");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
                Err(_) => {
                    eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(9);
                },
                Ok(cmp_path)=> cmp_path,
            };
            let params = match matches.values_of("parameters"){
                None => {
                    eprintln!("expegit: no parameters encountered. Continuing with experiment HEAD ...");
                    String::from("")
                },
                Some(params) => {
                    eprintln!("expegit: parameters '{}' encountered", params.clone().collect::<Vec<_>>().join(" "));
                    params.collect::<Vec<_>>().join(" ")
                },
            };
            let commit = match matches.value_of("COMMIT").unwrap(){
                "" => {
                    eprintln!("expegit: no commit hash encountered. Continuing with HEAD ...");
                    None
                },
                s => {
                    eprintln!("expegit: commit {} encountered", s);
                    Some(s.to_owned())
                },
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(12);
                },
                Ok(cmp) => cmp,
            };
            match campaign.new_execution(commit,params){
                Err(Error::InvalidCommit(s))=>{
                    eprintln!("expegit: the commit hash provided does not exist : {}. Try to fetch the experiment.", s);
                    std::process::exit(10);
                }
                Err(err)=>{
                    eprintln!("expegit: an error occurred while creating the execution: {}", err);
                    std::process::exit(11);
                }
                Ok(exc)=>{
                    eprintln!("expegit: execution {} successfully created", exc);
                }
            };
            if matches.is_present("push"){
                eprintln!("expegit: pushing changes to remote ...");
                match campaign.push(){
                    Err(err) =>{
                        eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                        std::process::exit(11);
                    }
                    Ok(_) => {
                        eprintln!("expegit: campaign successfully pushed");
                    }
                }
            }
        }
    }

    // Expegit-Exec-Reset
    if let Some(matches) = matches.subcommand_matches("exec"){
        if let Some(matches) = matches.subcommand_matches("reset"){
            eprintln!("expegit: resetting execution");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
                Err(_) => {
                    eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(12);
                },
                Ok(cmp_path)=> cmp_path,
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(12);
                },
                Ok(cmp) => cmp,
            };
            let exc_path = campaign.get_executions_path().join(matches.value_of("IDENTIFIER").unwrap());
            let mut execution = match repository::Execution::from_path(&exc_path){
                Err(err)=>{
                    eprintln!("expegit: an error occurred while creating the execution: {}", err);
                    std::process::exit(11);
                }
                Ok(exc)=>{
                    eprintln!("expegit: execution {} successfully created", exc);
                    exc
                }
            };
            match campaign.reset_execution(&mut execution){
                Err(err)=>{
                    eprintln!("expegit: an error occurred during execution reset: {}", err);
                    std::process::exit(13);
                }
                Ok(_)=>{
                    eprintln!("expegit: execution successfully reset");
                }
            }
            if matches.is_present("push"){
                eprintln!("expegit: pushing changes to remote ...");
                match campaign.push(){
                    Err(err) =>{
                        eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                        std::process::exit(14);
                    }
                    Ok(_) => {
                        eprintln!("expegit: campaign successfully pushed");
                    }
                }
            }
        }
    }

    // Expegit-Exec-Remove
    if let Some(matches) = matches.subcommand_matches("exec"){
        if let Some(matches) = matches.subcommand_matches("remove"){
            eprintln!("expegit: removing execution");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
                Err(_) => {
                    eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(15);
                },
                Ok(cmp_path)=> cmp_path,
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(12);
                },
                Ok(cmp) => cmp,
            };
            let exc_path = campaign.get_executions_path().join(matches.value_of("IDENTIFIER").unwrap());
            let execution = match repository::Execution::from_path(&exc_path){
                Err(err)=>{
                    eprintln!("expegit: an error occurred while creating the execution: {}", err);
                    std::process::exit(11);
                }
                Ok(exc)=>{
                    eprintln!("expegit: execution {} successfully created", exc);
                    exc
                }
            };
            match campaign.remove_execution(execution){
                Err(err)=>{
                    eprintln!("expegit: an error occurred during execution removal: {}", err);
                    std::process::exit(16);
                }
                Ok(_)=>{
                    eprintln!("expegit: execution successfully removed");
                }
            }
            if matches.is_present("push"){
                eprintln!("expegit: pushing changes to remote ...");
                match campaign.push(){
                    Err(err) =>{
                        eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                        std::process::exit(17)
                    }
                    Ok(_) => {
                        eprintln!("expegit: campaign successfully pushed");
                    }
                }
            }
        }
    }

    // Expegit-Exec-Finish
    if let Some(matches) = matches.subcommand_matches("exec"){
        if let Some(matches) = matches.subcommand_matches("finish"){
            eprintln!("expegit: finishing execution");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")){
                Err(_) => {
                    eprintln!("expegit: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(18);
                },
                Ok(cmp_path)=> cmp_path,
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("expegit: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(12);
                },
                Ok(cmp) => cmp,
            };
            let exc_path = campaign.get_executions_path().join(matches.value_of("IDENTIFIER").unwrap());
            let mut execution = match repository::Execution::from_path(&exc_path){
                Err(err)=>{
                    eprintln!("expegit: an error occurred while creating the execution: {}", err);
                    std::process::exit(11);
                }
                Ok(exc)=>{
                    eprintln!("expegit: execution {} successfully created", exc);
                    exc
                }
            };
            match campaign.finish_execution(&mut execution){
                Err(err)=>{
                    eprintln!("expegit: an error occurred during execution finishing: {}", err);
                    std::process::exit(19);
                }
                Ok(_)=>{
                    eprintln!("expegit: execution successfully finished");
                }
            };
            if matches.is_present("push"){
                eprintln!("expegit: pushing changes to remote ...");
                match campaign.push(){
                    Err(err) =>{
                        eprintln!("expegit: an error occurred while pushing changes to remote: {}", err);
                        std::process::exit(20);
                    }
                    Ok(_) => {
                        eprintln!("expegit: campaign succesfully pushed");
                    }
                }
            }
        }
    }
    std::process::exit(0);
}
