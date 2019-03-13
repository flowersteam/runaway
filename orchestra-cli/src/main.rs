// orchestra-cli/main.rs
// Author: Alexandre Péré
///
/// Higher level command line tool, allowing to open, create, clone a campaign, and generate and
/// run executions.

// CRATES
#[macro_use] extern crate serde_derive;
#[macro_use] extern crate serde_json;
extern crate pretty_logger;
#[macro_use] extern crate log;
extern crate actix;
extern crate actix_web;
extern crate handlebars;
extern crate liborchestra;
extern crate regex;
extern crate futures;
extern crate clap;

// IMPORTS
use std::{fs, env, path};
use std::sync::{Arc, Mutex};
use liborchestra::{misc, repository, tasks};

// MODULES
mod web;

// CONSTANTS
const NAME: &str = "orchestra-cli";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
const DESC: &str = "Automate experimental campaigns";

/// Used to validate user inputs as being uints
fn _validate_uint(v: String) -> Result<(), String> {
    let parsed = v.parse::<usize>();
    match parsed {
        Err(_) => Err(String::from("The input value is not a valid integer")),
        Ok(_) => Ok(()),
    }
}

// MAIN
fn main() {
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
        .subcommand(clap::SubCommand::with_name("gui")
            .about("Start web interface")
            .arg(clap::Arg::with_name("PATH")
                .help("The path to a campaign if you want to skip start")
                .index(1)
                .default_value("")
                .required(true))
            .arg(clap::Arg::with_name("port")
                .help("The port to bind the server to")
                .short("p")
                .long("port")
                .default_value("8088")
                .validator(_validate_uint))
            .arg(clap::Arg::with_name("workers")
                .help("The number of workers to use to execute jobs")
                .short("w")
                .long("workers")
                .default_value("10")
                .validator(_validate_uint)))
        .subcommand(clap::SubCommand::with_name("init")
            .about("Initializes campaign repository")
            .arg(clap::Arg::with_name("EXPE-URL")
                .help("Url of the experiment repository")
                .index(1)
                .required(true))
            .arg(clap::Arg::with_name("CAMPAIGN-URL")
                .help("Url of the campaign. To be created by orchestra-cli.")
                .index(2)
                .required(true))
            .arg(clap::Arg::with_name("PATH")
                .help("Path to the local campaign repository")
                .index(3)
                .default_value(".")))
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
        .subcommand(clap::SubCommand::with_name("sync")
            .about("Synchronize changes between remote and local. Pulls experiment repo as well."))
        .subcommand(clap::SubCommand::with_name("jobs")
            .about("Manages jobs")
            .setting(clap::AppSettings::SubcommandRequired)
            .subcommand(clap::SubCommand::with_name("run")
                .about("Generate and run a batch of jobs")
                .arg(clap::Arg::with_name("PROFILE")
                    .help("The execution profile to use for execution")
                    .required(true))
                .arg(clap::Arg::with_name("COMMIT")
                    .help("The experiment commit to use for execution")
                    .default_value(""))
                .arg(clap::Arg::with_name("PARAMETERS")
                    .help("The parameters generator to use to generate jobs. Separate parameters variations with ; and parameters with ¤. Example '--param¤1;2¤--flag;'.")
                    .default_value(""))
                .arg(clap::Arg::with_name("repeat")
                    .short("r")
                    .long("repeat")
                    .default_value("1")
                    .validator(_validate_uint)
                    .help("Number of repetitions for each configuration"))
                .arg(clap::Arg::with_name("workers")
                    .short("w")
                    .long("workers")
                    .default_value("10")
                    .validator(_validate_uint)
                    .help("Number of workers to use to perform the tasks")))
            .subcommand(clap::SubCommand::with_name("reschedule")
                .about("Reschedule jobs")
                .arg(clap::Arg::with_name("IDENTIFIERS")
                    .help("Identifiers of the executions to reschedule")
                    .required(true)
                    .multiple(true))
                .arg(clap::Arg::with_name("workers")
                    .short("w")
                    .long("workers")
                    .default_value("10")
                    .validator(_validate_uint)
                    .help("Number of workers to use to perform the tasks")))
            .subcommand(clap::SubCommand::with_name("delete")
                .about("Deletes jobs")
                .arg(clap::Arg::with_name("IDENTIFIERS")
                    .help("Identifiers of the executions to delete")
                    .multiple(true)
                    .required(true))))
        .get_matches();

    // We initialize the logger.
    if matches.is_present("verbose") {
        pretty_logger::init(pretty_logger::Destination::Stdout,
                            log::LogLevelFilter::Info,
                            pretty_logger::Theme::default()).unwrap();
    }

    // We check for git/lfs versions
    match misc::check_git_lfs_versions() {
        Err(err) => {
            eprintln!("orchestra-cli: failed to find Git or Lfs. {}.", err);
            std::process::exit(1);
        }
        Ok(gl) => {
            eprintln!("orchestra-cli: using git {} and lfs {}", gl.0, gl.1);
        }
    };

    // Orchestra-Gui
    if let Some(matches) = matches.subcommand_matches("gui") {
        let port: u32 = matches.value_of("port").unwrap().parse().unwrap();
        let workers: u32 = matches.value_of("workers").unwrap().parse().unwrap();
        let path = matches.value_of("PATH").unwrap();
        eprintln!("orchestra-cli: starting gui ...");
        let campaign = match path.as_ref() {
            "" => {
                eprintln!("orchestra-cli: launching start application");
                web::start::launch_start(port)
            }
            _ => {
                let path = fs::canonicalize(env::current_dir().unwrap().join(path)).unwrap();
                match repository::Campaign::from_path(&path) {
                    Err(err) => {
                        eprintln!("orchestra-cli: an error occured while opening the expegit configuration: {}", err);
                        std::process::exit(2);
                    }
                    Ok(cmp) => {
                        eprintln!("orchestra-cli: campaign {} successfully initialized", cmp);
                        cmp
                    }
                }
            }
        };
        web::app::launch_orchestra(campaign, port, workers);
    }

    // Orchestra-Clone
    if let Some(matches) = matches.subcommand_matches("clone") {
        eprintln!("orchestra-cli: cloning repository from {}", matches.value_of("URL").unwrap());
        match repository::Campaign::from_url(matches.value_of("URL").unwrap(), &path::PathBuf::from(matches.value_of("PATH").unwrap())) {
            Err(err) => {
                eprintln!("orchestra-cli: an error occurred during campaign cloning: {}", err);
                std::process::exit(3);
            }
            Ok(cmp) => {
                eprintln!("orchestra-cli: campaign {} successfully cloned", cmp);
            }
        }
    }

    // Orchestra-Sync
    if matches.subcommand_matches("sync").is_some() {
        eprintln!("orchestra-cli: syncing local with remote ...");
        let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")) {
            Err(_) => {
                eprintln!("orchestra-cli: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                std::process::exit(8);
            }
            Ok(cmp_path) => cmp_path,
        };
        let campaign = match repository::Campaign::from_path(&cmp_path) {
            Err(err) => {
                eprintln!("orchestra-cli: an error occured while opening the expegit configuration: {}", err);
                std::process::exit(9);
            }
            Ok(cmp) => cmp,
        };
        eprintln!("orchestra-cli: pulling remote changes...");
        match campaign.pull() {
            Err(err) => {
                eprintln!("orchestra-cli: an error occurred while pulling changes from remote: {}", err);
                std::process::exit(10);
            }
            Ok(_) => {
                eprintln!("orchestra-cli: campaign successfully pulled");
            }
        }
        eprintln!("orchestra-cli: pushing local changes...");
        match campaign.push() {
            Err(err) => {
                eprintln!("orchestra-cli: an error occurred while pushing changes to remote: {}", err);
                std::process::exit(11);
            }
            Ok(_) => {
                eprintln!("orchestra-cli: campaign successfully pushed");
            }
        }
        eprintln!("orchestra-cli: fetching experiment...");
        match campaign.fetch_experiment() {
            Err(err) => {
                eprintln!("orchestra-cli: an error occurred while fetchinig experiment changes: {}", err);
                std::process::exit(12);
            }
            Ok(_) => {
                eprintln!("orchestra-cli: experiment successfully fetched");
            }
        }
    }

    // Orchestra-jobs-generate
    if let Some(matches) = matches.subcommand_matches("jobs") {
        if let Some(matches) = matches.subcommand_matches("run") {
            eprintln!("orchestra-cli: generating jobs");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")) {
                Err(_) => {
                    eprintln!("orchestra-cli: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(13);
                }
                Ok(cmp_path) => cmp_path,
            };
            let profile = String::from(matches.value_of("PROFILE").unwrap());
            let commit = match matches.value_of("COMMIT").unwrap() {
                "" => {
                    eprintln!("orchestra-cli: no commit hash encountered. Continuing with HEAD ...");
                    None
                }
                s => {
                    eprintln!("orchestra-cli: commit {} encountered", s);
                    Some(s.to_owned())
                }
            };
            let repeat = matches.value_of("repeat")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let parameters = match matches.value_of("PARAMETERS").unwrap() {
                "" => {
                    eprintln!("orchestra-cli: parameters encountered ...");
                    (0..repeat).map(|_| String::new()).collect()
                }
                p => {
                    eprintln!("orchestra-cli: parameters {} encountered", p);
                    misc::parse_parameters(p, repeat)
                }
            };
            let workers = matches.value_of("workers")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("orchestra-cli: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(14);
                }
                Ok(cmp) => Arc::new(Mutex::new(cmp)),
            };
            let mut taskqueue = tasks::TaskQueue::new(workers as u32);
            parameters
                .into_iter()
                .map(|s| {
                    tasks::Task::new(
                        campaign.clone(),
                        s,
                        commit.clone(),
                        profile.clone(),
                    )
                }).for_each(|t| taskqueue.push(t));
            taskqueue.wait();
        }
    }

    // Orchestra-jobs-reschedule
    if let Some(matches) = matches.subcommand_matches("jobs") {
        if let Some(matches) = matches.subcommand_matches("reschedule") {
            eprintln!("orchestra-cli: rescheduling executions");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")) {
                Err(_) => {
                    eprintln!("ochestra: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(14);
                }
                Ok(cmp_path) => cmp_path,
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("orchestra-cli: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(15);
                }
                Ok(cmp) => Arc::new(Mutex::new(cmp)),
            };
            let excs_path = campaign.lock().unwrap().get_executions_path();
            let mut executions: Vec<repository::Execution> = matches.values_of("IDENTIFIERS")
                .unwrap()
                .map(|s| {
                    match repository::Execution::from_path(&excs_path.join(s)) {
                        Err(_) => {
                            eprintln!("orchestra-cli: failed to open {} execution.", s);
                            std::process::exit(16);
                        }
                        Ok(e) => e,
                    }
                })
                .collect();
            let mut profiles: Vec<String> = executions.iter()
                .map(|e| e.get_executor().as_ref().unwrap().clone())
                .collect();
            executions.iter_mut()
                .for_each(|e| {
                    eprintln!("orchestra-cli: resetting execution {}", e);
                    campaign.lock().unwrap().reset_execution(e).unwrap()
                });
            let workers = matches.value_of("workers")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let mut taskqueue = tasks::TaskQueue::new(workers as u32);
            for _ in 0..executions.len() {
                let execution = executions.pop().unwrap();
                let profile = profiles.pop().unwrap();
                let t = tasks::Task::from_execution(
                    campaign.clone(),
                    execution,
                   profile,
                );
                taskqueue.push(t);
            }
            taskqueue.wait();
        }
    }

    // Orchestra-jobs-delete
    if let Some(matches) = matches.subcommand_matches("jobs") {
        if let Some(matches) = matches.subcommand_matches("delete") {
            eprintln!("orchestra-cli: deleting executions");
            let cmp_path = match misc::search_expegit_root(&path::PathBuf::from(".")) {
                Err(_) => {
                    eprintln!("ochestra: unable to find an expegit repository in parent folders. Are you in an expegit repository?");
                    std::process::exit(17);
                }
                Ok(cmp_path) => cmp_path,
            };
            let campaign = match repository::Campaign::from_path(&cmp_path) {
                Err(err) => {
                    eprintln!("orchestra-cli: an error occured while opening the expegit configuration: {}", err);
                    std::process::exit(18);
                }
                Ok(cmp) => Arc::new(Mutex::new(cmp)),
            };
            let excs_path = campaign.lock().unwrap().get_executions_path();
            let mut executions: Vec<repository::Execution> = matches.values_of("IDENTIFIERS")
                .unwrap()
                .map(|s| {
                    match repository::Execution::from_path(&excs_path.join(s)) {
                        Err(_) => {
                            eprintln!("orchestra-cli: failed to open {} execution.", s);
                            std::process::exit(19);
                        }
                        Ok(e) => e,
                    }
                })
                .collect();
            executions.into_iter()
                .for_each(|e| {
                    eprintln!("orchestra-cli: deleting execution {}", e);
                    campaign.lock().unwrap().remove_execution(e).unwrap()
                });
        }
    }
    std::process::exit(0);
}