//! runaway-cli/subcommands/mod.rs
//! Author: Alexandre Péré
//! 
//! This module contains subcommand functions. Those functions are called based on the dispatch of 
//! the application main. They each implement the logic of one of the different subcommands.


//-------------------------------------------------------------------------------------------IMPORTS


use std::path;
use liborchestra::{
    SEND_ARCH_RPATH, 
    FETCH_IGNORE_RPATH, 
    FETCH_ARCH_RPATH,
    PROFILES_FOLDER_RPATH};
use liborchestra::hosts::{HostConf, HostHandle, LeaveConfig};
use liborchestra::scheduler::SchedulerHandle;
use clap;
use chrono::prelude::*;
use std::io::prelude::*;
use uuid;
use dirs;
use log::*;
use futures::executor::block_on;
use futures::task::SpawnExt;
use futures::channel::mpsc::*;
use crate::{escape, to_exit};
use crate::misc::{self, SendIgnore, FetchIgnore};
use crate::exit::Exit;
use liborchestra::primitives::{local, File, AbsolutePath, Folder};
use liborchestra::asynclets;
use std::path::{PathBuf, Path};


//-------------------------------------------------------------------------------------------- TYPES


/// A newtype representing a path to the local root folder, i.e. the path at which the command was 
/// started.
pub struct LocalRootDir(PathBuf);
impl LocalRootDir{
    fn from(path: PathBuf) -> LocalRootDir{
        LocalRootDir(path)
    }
}
impl AsRef<Path> for LocalRootDir{
    fn as_ref(&self) -> &Path{
        self.0.as_ref()
    }
}



//-------------------------------------------------------------------------------------- SUBCOMMANDS


/// Executes a single execution of the script with the command arguments and returns exit code.
pub fn exec(matches: &clap::ArgMatches) -> Result<(), Exit>{

    // We initialize the logger
    misc::init_logger(&matches);

    // We load the host
    let host = misc::get_host(matches.value_of("REMOTE").unwrap())?;

    // We install ctrl-c handler
    misc::install_ctrlc_handler(host.clone());

    // We setup some parameters
    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    let leave_tar = matches.is_present("leave-tars");
    let parameters = matches.value_of("parameters").unwrap_or("");
    let (script, folder) = misc::get_script_and_folder(matches.value_of("SCRIPT").unwrap())?;

    // We setup ignore files
    let send_ignore_globs = misc::get_send_ignore_globs(matches.value_of("send-ignore").unwrap())?;

    // We list script_folder
    let files_to_send = to_exit!(asynclets::list_local_folder(local(folder.content.0), 
                                                              send_ignore_globs, 
                                                              vec!()),
                                 Exit::ReadFolder)?;
    
    let fetch_ignore_globs = misc::get_fetch_ignore_globs(matches.value_of("fetch-ignore").unwrap(), 
                                                          &files_to_send)?;

    // We declare the future 
    let future = async move{

        // We pack the folder
        let hash = try_return_err!(asn::pack_folder(script_folder.into()).await);
        
        
        // We acquire the node
        let node = try_return_err!(asn::acquire_node(host.clone()).await);

        // We send the data
        let remote_dir = host.get_host_directory().join(format!("{}", hash));
        let remote_send = remote_dir.join(SEND_ARCH_RPATH);
        let id = uuid::Uuid::new_v4();
        let remote_defl = remote_dir.join(format!("{}", id));
        let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
        let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
        try_return_err!(asn::send_data(node.clone(),
                                  script_folder.join(SEND_ARCH_RPATH),
                                  remote_dir.clone()).await);

        // We perform the job
        let stdout_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            println!("{}", string);
        });
        let stderr_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            eprintln!("{}", string);
        });
        let out = try_return_err!(asn::perform_job(node.clone(),
                                              host.get_before_execution_command(),
                                              host.get_after_execution_command(),
                                              script_name.to_str().unwrap().to_owned(),
                                              parameters.to_owned(),
                                              remote_defl.to_str().unwrap().to_owned(),
                                              remote_send.to_str().unwrap().to_owned(),
                                              stdout_callback,
                                              stderr_callback).await);

        // We fetch the results
        try_return_err!(asn::fetch_data(node.clone(),
                                   remote_defl.to_str().unwrap().to_owned(),
                                   remote_fetch.to_str().unwrap().to_owned(),
                                   remote_ignore.to_str().unwrap().to_owned(),
                                   script_folder.into()).await);

        // We clear the remote end
        try_return_err!(asn::clean_data(node.clone(),
                                   remote_dir.to_str().unwrap().to_owned(),
                                   remote_defl.to_str().unwrap().to_owned(),
                                   leave).await);
        
        // We clear the local end
        if !leave_tar{
            std::fs::remove_file(script_folder.join(SEND_ARCH_RPATH)).unwrap();
            std::fs::remove_file(script_folder.join(FETCH_ARCH_RPATH)).unwrap();
        }

        Ok(out.status.code().unwrap_or(911))
    };

    // We execute the future
    let out = try_return_code!(block_on(future),
                               "failed to perform job",
                               6);
    return out;
}

/*

// Executes a batch of executions on a remote host.
pub fn batch(matches: &clap::ArgMatches) -> i32 {

    // We initialize the logger
    misc::init_logger(&matches);

    // We load the host
    let host_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("{}.yml", matches.value_of("REMOTE").unwrap()));
    let config = try_return_code!(HostConf::from_file(&host_path),
                                   "can not load the host configuration",
                                   1);
    let host = try_return_code!(HostHandle::spawn(config), 
                                 "failed to spawn host", 
                                 2);

    // We install the ctrl c handler
    misc::install_ctrlc_handler(host.clone());

    // We setup some parameters
    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    let leave_tars = matches.is_present("leave-tars");
    let script_path = matches.value_of("SCRIPT").unwrap();
    let params = match matches.value_of("parameters_file"){
        Some(f) => {
            let mut params = String::new();
            let mut file = try_return_code!(std::fs::File::open(f),
                                         "failed to open parameters file",
                                         7);
            try_return_code!(file.read_to_string(&mut params),
                              "failed to read parameters file",
                              14);
            let trimmed = params.trim_start_matches('\n').trim_end_matches('\n');
            trimmed.lines()
                .map(|e| e.to_owned())
                .collect::<Vec<String>>()
        }
        None => {
            let repeats = try_return_code!(matches.value_of("repeats").unwrap().parse::<u32>(),
                                            "failed to read the number of repeats",
                                            8);
            let params = matches.value_of("parameters_string").unwrap_or("");
            let params = misc::parse_parameters(&params, repeats as usize);
            info!("Parameters expansion: {:#?}",params);
            params
        }
    };
    let output_folder: path::PathBuf = matches.value_of("output_folder").unwrap().into();
    let outputs = match matches.value_of("outputs_file"){
        Some(f) => {
            let mut folders = String::new();
            let mut file = try_return_code!(std::fs::File::open(f),
                                         "failed to open outputs file",
                                         9);
            try_return_code!(file.read_to_string(&mut folders),
                              "failed to read output folders file",
                              15);
            let trimmed = folders.trim_start_matches('\n').trim_end_matches('\n');
            trimmed.lines()
                .map(|e| e.into())
                .collect::<Vec<path::PathBuf>>()
        }
        None => {
            params.iter()
                .map(|_| output_folder.join(Utc::now().to_string()))
                .collect::<Vec<path::PathBuf>>()
        }
    };
    if params.len() != outputs.len(){
        eprintln!("runaway: the number of parameters and outputs is different. Leaving.");
        return 10;
    }
    let script_abs_path = try_return_code!(std::fs::canonicalize(script_path),
                                            "failed to get absolute script path",
                                            3);
    let script_folder = try_return_code!(script_abs_path.parent().ok_or("None"),
                                          "failed to get script folder",
                                          4).to_owned();
    let script_name = try_return_code!(script_abs_path.file_name().ok_or("Non"),
                                        "failed to get script name",
                                        5).to_owned();        
    
    // We pack and send the code
    let hash = try_return_code!(block_on(asn::pack_folder(script_folder.clone())),
                                "failed to pack folder",
                                11);
    let remote_dir = host.get_host_directory().join(format!("{}", hash));
    let remote_send = remote_dir.join(SEND_ARCH_RPATH);
    try_return_code!(block_on(asn::send_data_to_front(host.get_frontend(), script_folder.join(SEND_ARCH_RPATH), remote_dir.clone())),
                     "failed to send data",
                     13);

    // We adapt the leave option to keep the code between executions
    let l = match leave{ 
        LeaveConfig::Everything => LeaveConfig::Everything,
        LeaveConfig::Nothing => LeaveConfig::Code, 
        LeaveConfig::Code => LeaveConfig::Code,
    };

    // We start the executor
    let mut executor = try_return_code!(futures::executor::ThreadPoolBuilder::new()
        .name_prefix("runaway-worker")
        .create(),
        "failed to spawn worker",
        12);


    // We spawn a task for all parameters
    let len = outputs.len();
    let mut handles = Vec::new();
    for (i, (p, b)) in params.into_iter().zip(outputs.into_iter()).enumerate(){
        
        // We setup some parameters
        let l = l.clone();
        let script_name = script_name.clone();
        let host = host.clone();
        let remote_dir = remote_dir.clone();
        let remote_send = remote_send.clone();
        let id = uuid::Uuid::new_v4();
        let remote_defl = remote_dir.join(format!("{}", id));
        let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
        let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
        let benchmark = matches.is_present("benchmark");

        // We build the future
        let future = async move{
            
            let p = p.clone();
            
            // We acquire a node
            let node = try_return_err!(asn::acquire_node(host.clone()).await);

            // We setup callbacks
            let stdout_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
            let stderr_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
            if benchmark==true{
                stdout_callback = Box::new(|_|{});
                stderr_callback = Box::new(|_|{});
            } else{
                stdout_callback = Box::new(move |a|{
                    let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                    println!("#{}: {}", i, string);
                });
                stderr_callback = Box::new(move |a|{
                    let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                    eprintln!("#{}: {}", i, string);
                });
            }

            // We perform the job
            let out = try_return_err!(asn::perform_job(node.clone(),
                                                  host.get_before_execution_command(),
                                                  host.get_after_execution_command(),
                                                  script_name.to_str().unwrap().to_owned(),
                                                  p,
                                                  remote_defl.to_str().unwrap().to_owned(),
                                                  remote_send.to_str().unwrap().to_owned(),
                                                  stdout_callback,
                                                  stderr_callback).await);

            // We fetch the data
            try_return_err!(asn::fetch_data(node.clone(),
                                       remote_defl.to_str().unwrap().to_owned(),
                                       remote_fetch.to_str().unwrap().to_owned(),
                                       remote_ignore.to_str().unwrap().to_owned(),
                                       b.clone()).await);

            // We clean the remote end
            try_return_err!(asn::clean_data(node.clone(),
                                       remote_dir.to_str().unwrap().to_owned(),
                                       remote_defl.to_str().unwrap().to_owned(),
                                       l).await);
            // We write the stdout, stderr, ecode
            let mut stdout_file = std::fs::File::create(b.join("stdout")).unwrap();
            stdout_file.write_all(&out.stdout).unwrap();
            let mut stderr_file = std::fs::File::create(b.join("stderr")).unwrap();
            stderr_file.write_all(&out.stderr).unwrap();
            let mut ecode_file = std::fs::File::create(b.join("ecode")).unwrap();
            ecode_file.write_all(format!("{}", out.status.code().unwrap_or(911)).as_bytes()).unwrap();

            // We clean the local end
            if !leave_tars{
                std::fs::remove_file(b.join(FETCH_ARCH_RPATH)).unwrap();
            }

            Ok(out.status.code().unwrap_or(911))
        };

        // We spawn the task
        let handle = try_return_code!(executor.spawn_with_handle(future).map_err(|e| format!("{:?}", e)),
                                      "failed to spawn future",
                                      14);
        info!("Job: Spawned job");
        handles.push(handle);
    };
    
    // We build the future waiting for all handles to finish
    let future = futures::future::join_all(handles);

    // We execute this future 
    let output = executor.run(future);

    // We check whether all tasks were driven to completion 
    let completed = output.into_iter()
        .inspect(|o| if o.is_err(){error!("Failed to execute a job: {:?}",o)})
        .filter_map(|c| c.ok())
        .collect::<Vec<_>>()
        .len();
    
    // We clear the remote end (really)
    if let LeaveConfig::Nothing = leave{
        executor.run(host.get_frontend()
            .async_exec(format!("rm -rf {}", remote_dir.to_str().unwrap())))
            .map_err(|e| error!("Failed to remove code: {}", e))
            .unwrap();
    }
    
    // We clear the local end
    if !leave_tars{
        std::fs::remove_file(script_folder.join(SEND_ARCH_RPATH)).unwrap();
    }

    // We print some message to the user.
    if completed == len{
        eprintln!("runaway: brought all jobs to completion.");
        return 0;
    } else{
        eprintln!("runaway: brought {} jobs to completion over {}.", 
            completed,
            len);
        return 911;
    }
}

// Executes the test and returns the exit code.
pub fn test(matches: &clap::ArgMatches) -> i32 {

    // We initialize the logger
    misc::init_logger(&matches);

    // We open the configuration
    eprintln!("runaway: opening configuration");
    let config = try_return_code!(HostConf::from_file(&matches.value_of("FILE").unwrap().into()),
                                   "can not load the host configuration",
                                   90);

    // We open the ssh profile
    eprintln!("runaway: opening ssh config file");
    let profile = try_return_code!(liborchestra::ssh::config::get_profile(
                    &dirs::home_dir().unwrap().join(liborchestra::SSH_CONFIG_RPATH),
                    &config.ssh_config), 
            "failed to load ssh config", 
            91);

    // We spawn the handle to the frontend
    eprintln!("runaway: spawning frontend");
    let frontend = try_return_code!(liborchestra::ssh::RemoteHandle::spawn(profile.clone()),
                                    "failed to connect to frontend",
                                    92);
    eprintln!("runaway: connection successful");

    // We start an allocation
    let command = config.start_alloc();
    eprintln!("runaway: starting allocation with command: ```{}```", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
                               "failed to start allocation",
                               93);
    eprintln!("runaway: returned: {:#?}", out);


    // We get allocated nodes
    let alloc_ret = String::from_utf8(out.stdout).unwrap();
    let command = config.get_alloc_nodes().replace("$ALLOCRET", alloc_ret.trim_end_matches('\n'));
    eprintln!("runaway: getting nodes with command: ```{}```", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
        "failed to get nodes",
        94);
    eprintln!("runaway: returned: {:#?}", out);

    // We connect to a node
    let node = String::from_utf8(out.stdout).unwrap().lines().nth(0).unwrap().to_owned();
    eprintln!("runaway: trying to connect to node {}", node);
    let mut node_profile = profile.clone();
    node_profile.proxycommand = Some(config.node_proxy_command.replace("$NODENAME", &node));
    node_profile.port = None;
    node_profile.hostname = Some(format!("{}::{}",node_profile.hostname.take().unwrap(), node));
    eprintln!("runaway: spawning node: {:?}", node_profile);
    let node = try_return_code!(liborchestra::ssh::RemoteHandle::spawn(node_profile),
                                "failed to connect to node",
                                95);
    eprintln!("runaway: connection successful");

    // We execute the before exec commands on the node
    let command = config.before_execution();
    eprintln!("runaway: executing before_exec command {}", command);
    let out = try_return_code!(block_on(node.async_exec(command)),
        "failed to execute before_exec command",
        96);
    eprintln!("runaway: command returned: {:#?}", out);

    // We execute the after exec commands on the node
    let command = config.after_execution();
    eprintln!("runaway: executing after_exec command {}", command);
    let out = try_return_code!(block_on(node.async_exec(command)),
        "failed to execute after_exec command",
        97);
    eprintln!("runaway: command returned: {:#?}", out);

    // We cancel the allocation
    let command = config.cancel_alloc().replace("$ALLOCRET", &alloc_ret);
    eprintln!("runaway: cancelling allocation with command {}", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
        "failed to cancel allocation",
        98);
    eprintln!("runaway: returned: {:#?}", out);

    // We try a dummy commandon the node that should now fail. 
    let command = config.before_execution();
    eprintln!("runaway: trying a dummy command on node (should fail) {}", command);
    let out = block_on(node.async_exec(command));
    eprintln!("runaway: command returned: {:#?}", out);

    0
}

// Output completion for profiles.
pub fn install_completion(application: clap::App) -> i32 {

    // We check which shell and install the right completion
    match misc::which_shell(){
        Ok(clap::Shell::Zsh) => {

            // We append the .zshrc file
            eprintln!("runaway: zsh recognized. Proceeding.");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(dirs::home_dir().unwrap().join(".zshrc"))
                .unwrap_or_else(|e| {
                    eprintln!("runaway: impossible to open .zshrc file: {}", e);
                    std::process::exit(551);
                });
            file.write_all("\n# Added by Runaway for zsh completion\n".as_bytes()).unwrap();
            file.write_all(format!("fpath=(~/{} $fpath)\n", PROFILES_FOLDER_RPATH).as_bytes()).unwrap();
            file.write_all("autoload -U compinit\ncompinit".as_bytes()).unwrap();

            // We generate and install completion files
            misc::generate_zsh_completion(application);
            eprintln!("runaway: zsh completion installed. Open a new terminal to use it.");
        }
        Ok(clap::Shell::Bash) => {

            // We append the bashrc file
            eprintln!("runaway: bash recognized. Proceeding.");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(dirs::home_dir().unwrap().join(".bashrc"))
                .unwrap_or_else(|e| {
                    eprintln!("runaway: impossible to open .bashrc file: {}", e);
                    std::process::exit(551);
                });
            file.write_all("\n# Added by Runaway for bash completion\n".as_bytes()).unwrap();
            file.write_all(format!("source ~/{}/{}.bash\n", 
                                    PROFILES_FOLDER_RPATH, 
                                    misc::get_bin_name()).as_bytes()).unwrap();

            // We generate and install completion files.
            misc::generate_bash_completion(application);
            eprintln!("runaway: bash completion installed. Open a new terminal to use it.");
        }
        Err(e) => {
            eprintln!("runaway: shell {} is not available for completion. Use bash or zsh.", e);
            return 553;
        }
        _ => unreachable!()
    }

    return 0
}

// Executes executions scheduled online by an external command.
pub fn sched(matches: &clap::ArgMatches) -> i32 {

    // We initialize the logger
    misc::init_logger(&matches);

    // We load the host
    let host_path = dirs::home_dir()
        .unwrap()
        .join(PROFILES_FOLDER_RPATH)
        .join(format!("{}.yml", matches.value_of("REMOTE").unwrap()));
    let config = try_return_code!(HostConf::from_file(&host_path),
                                   "can not load the host configuration",
                                   1);
    let host = try_return_code!(HostHandle::spawn(config), 
                                 "failed to spawn host", 
                                 2);

    // We install the ctrl c handler
    misc::install_ctrlc_handler(host.clone());

    // We setup some parameters
    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    let leave_tars = matches.is_present("leave-tars");
    let script_path = matches.value_of("SCRIPT").unwrap();
    let output_folder: path::PathBuf = matches.value_of("output_folder").unwrap().into();
    let script_abs_path = try_return_code!(std::fs::canonicalize(script_path),
                                            "failed to get absolute script path",
                                            3);
    let script_folder = try_return_code!(script_abs_path.parent().ok_or("None"),
                                          "failed to get script folder",
                                          4).to_owned();
    let script_name = try_return_code!(script_abs_path.file_name().ok_or("Non"),
                                        "failed to get script name",
                                        5).to_owned();        
    
    // We pack and send the code
    let hash = try_return_code!(block_on(asn::pack_folder(script_folder.clone())),
                                "failed to pack folder",
                                11);
    let remote_dir = host.get_host_directory().join(format!("{}", hash));
    let remote_send = remote_dir.join(SEND_ARCH_RPATH);
    try_return_code!(block_on(asn::send_data_to_front(host.get_frontend(), script_folder.join(SEND_ARCH_RPATH), remote_dir.clone())),
                     "failed to send data",
                     13);

    // We adapt the leave option to keep the code between executions
    let l = match leave{ 
        LeaveConfig::Everything => LeaveConfig::Everything,
        LeaveConfig::Nothing => LeaveConfig::Code, 
        LeaveConfig::Code => LeaveConfig::Code,
    };

    // We start the executor
    let mut executor = try_return_code!(futures::executor::ThreadPoolBuilder::new()
        .name_prefix("runaway-worker")
        .create(),
        "failed to spawn worker",
        12);

    // We spawn the scheduler
    let mut sched_args = matches.value_of("SCHEDULER").unwrap()
        .trim_start_matches(" ")
        .trim_end_matches(" ")
        .split(" ")
        .collect::<Vec<_>>();
    let mut sched_command = std::process::Command::new(sched_args.remove(0));
    sched_command.args(sched_args)
        .current_dir(script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let scheduler = try_return_code!(SchedulerHandle::spawn(sched_command, 
            matches.value_of("SCHEDULER").unwrap().to_owned()),
        "failed to spawn scheduler",
        15);

    // We instantiate the channel
    let (roll_sender, roll_receiver) = unbounded();

    // We define the start future
    let init_fut = async move {

        let scheduler = scheduler.clone();
        let host = host.clone();
        let executor = executor.clone();
        let roll_sender = roll_sender.clone();

        loop{

            let p = match try_return_err!(scheduler.async_try_request_parameters()) {
                Some(p) => p,
                None => break, 
            };
            let node = match try_return_err!(host.async_try_acquire()) {
                Some(n) => n,
                None => break,
            };

            // We setup some parameters
            let l = l.clone();
            let script_name = script_name.clone();
            let host = host.clone();
            let remote_dir = remote_dir.clone();
            let remote_send = remote_send.clone();
            let id = uuid::Uuid::new_v4();
            let remote_defl = remote_dir.join(format!("{}", id));
            let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
            let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
            let benchmark = matches.is_present("benchmark");
            let b = output_folder.join(Utc::now().to_string()); 

            // We build the future
            let future = async move{


                // We setup callbacks
                let stdout_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
                let stderr_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
                if benchmark==true{
                    stdout_callback = Box::new(|_|{});
                    stderr_callback = Box::new(|_|{});
                } else{
                    stdout_callback = Box::new(move |a|{
                        let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                        println!("#{}: {}", p, string);
                    });
                    stderr_callback = Box::new(move |a|{
                        let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                        eprintln!("#{}: {}", p, string);
                    });
                }

                // We perform the job
                let out = try_return_err!(asn::perform_job(node.clone(),
                                                      host.get_before_execution_command(),
                                                      host.get_after_execution_command(),
                                                      script_name.to_str().unwrap().to_owned(),
                                                      p,
                                                      remote_defl.to_str().unwrap().to_owned(),
                                                      remote_send.to_str().unwrap().to_owned(),
                                                      stdout_callback,
                                                      stderr_callback).await);

                // We fetch the data
                try_return_err!(asn::fetch_data(node.clone(),
                                           remote_defl.to_str().unwrap().to_owned(),
                                           remote_fetch.to_str().unwrap().to_owned(),
                                           remote_ignore.to_str().unwrap().to_owned(),
                                           b.clone()).await);

                // We clean the remote end
                try_return_err!(asn::clean_data(node.clone(),
                                           remote_dir.to_str().unwrap().to_owned(),
                                           remote_defl.to_str().unwrap().to_owned(),
                                           l).await);

                // We write the stdout, stderr, ecode
                let mut stdout_file = std::fs::File::create(b.join("stdout")).unwrap();
                stdout_file.write_all(&out.stdout).unwrap();
                let mut stderr_file = std::fs::File::create(b.join("stderr")).unwrap();
                stderr_file.write_all(&out.stderr).unwrap();
                let mut ecode_file = std::fs::File::create(b.join("ecode")).unwrap();
                ecode_file.write_all(format!("{}", out.status.code().unwrap_or(911)).as_bytes()).unwrap();

                // We clean the local end
                if !leave_tars{
                    std::fs::remove_file(b.join(FETCH_ARCH_RPATH)).unwrap();
                }

                // We retrieve features
                let features_file = std::fs::File::open(b.join("features")).unwrap();
                let mut features = String::new();
                features_file.read_to_string(&mut features);
                let features = features.split("\n")
                    .map(|e| e.parse::<f64>().map_err(|e| format!("{}", e)))
                    .collect::<Result<Vec<f64>, String>>();
                let features = try_return_err!(features);

                // We record in scheduler
                try_return_err!(scheduler.async_record_output(p.to_owned(), features));

                // We return 
                Ok(())
            };

            // We spawn the future.
            executor.spawn(future);
        }
    }

    // We spawn a task for all parameters
    let len = outputs.len();
    let mut handles = Vec::new();
    for (i, (p, b)) in params.into_iter().zip(outputs.into_iter()).enumerate(){
        
        // We setup some parameters
        let l = l.clone();
        let script_name = script_name.clone();
        let host = host.clone();
        let remote_dir = remote_dir.clone();
        let remote_send = remote_send.clone();
        let id = uuid::Uuid::new_v4();
        let remote_defl = remote_dir.join(format!("{}", id));
        let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
        let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
        let benchmark = matches.is_present("benchmark");

        // We build the future
        let future = async move{
            
            let p = p.clone();
            
            // We acquire a node
            let node = try_return_err!(asn::acquire_node(host.clone()).await);

            // We setup callbacks
            let stdout_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
            let stderr_callback: Box<dyn Fn(Vec<u8>)+Send+'static>;
            if benchmark==true{
                stdout_callback = Box::new(|_|{});
                stderr_callback = Box::new(|_|{});
            } else{
                stdout_callback = Box::new(move |a|{
                    let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                    println!("#{}: {}", i, string);
                });
                stderr_callback = Box::new(move |a|{
                    let string = String::from_utf8(a).unwrap().replace("\r\n", "");
                    eprintln!("#{}: {}", i, string);
                });
            }

            // We perform the job
            let out = try_return_err!(asn::perform_job(node.clone(),
                                                  host.get_before_execution_command(),
                                                  host.get_after_execution_command(),
                                                  script_name.to_str().unwrap().to_owned(),
                                                  p,
                                                  remote_defl.to_str().unwrap().to_owned(),
                                                  remote_send.to_str().unwrap().to_owned(),
                                                  stdout_callback,
                                                  stderr_callback).await);

            // We fetch the data
            try_return_err!(asn::fetch_data(node.clone(),
                                       remote_defl.to_str().unwrap().to_owned(),
                                       remote_fetch.to_str().unwrap().to_owned(),
                                       remote_ignore.to_str().unwrap().to_owned(),
                                       b.clone()).await);

            // We clean the remote end
            try_return_err!(asn::clean_data(node.clone(),
                                       remote_dir.to_str().unwrap().to_owned(),
                                       remote_defl.to_str().unwrap().to_owned(),
                                       l).await);
            // We write the stdout, stderr, ecode
            let mut stdout_file = std::fs::File::create(b.join("stdout")).unwrap();
            stdout_file.write_all(&out.stdout).unwrap();
            let mut stderr_file = std::fs::File::create(b.join("stderr")).unwrap();
            stderr_file.write_all(&out.stderr).unwrap();
            let mut ecode_file = std::fs::File::create(b.join("ecode")).unwrap();
            ecode_file.write_all(format!("{}", out.status.code().unwrap_or(911)).as_bytes()).unwrap();

            // We clean the local end
            if !leave_tars{
                std::fs::remove_file(b.join(FETCH_ARCH_RPATH)).unwrap();
            }

            Ok(out.status.code().unwrap_or(911))
        };

        // We spawn the task
        let handle = try_return_code!(executor.spawn_with_handle(future).map_err(|e| format!("{:?}", e)),
                                      "failed to spawn future",
                                      14);
        info!("Job: Spawned job");
        handles.push(handle);
    };
    
    // We build the future waiting for all handles to finish
    let future = futures::future::join_all(handles);

    // We execute this future 
    let output = executor.run(future);

    // We check whether all tasks were driven to completion 
    let completed = output.into_iter()
        .inspect(|o| if o.is_err(){error!("Failed to execute a job: {:?}",o)})
        .filter_map(|c| c.ok())
        .collect::<Vec<_>>()
        .len();
    
    // We clear the remote end (really)
    if let LeaveConfig::Nothing = leave{
        executor.run(host.get_frontend()
            .async_exec(format!("rm -rf {}", remote_dir.to_str().unwrap())))
            .map_err(|e| error!("Failed to remove code: {}", e))
            .unwrap();
    }
    
    // We clear the local end
    if !leave_tars{
        std::fs::remove_file(script_folder.join(SEND_ARCH_RPATH)).unwrap();
    }

    // We print some message to the user.
    if completed == len{
        eprintln!("runaway: brought all jobs to completion.");
        return 0;
    } else{
        eprintln!("runaway: brought {} jobs to completion over {}.", 
            completed,
            len);
        return 911;
    }
}


*/