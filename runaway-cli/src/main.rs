//! runaway-cli/main.rs
//! Author: Alexandre Péré
//! 
//! Runaway command line tool. Allows to execute scripts and batches of scripts on remote hosts. 


//------------------------------------------------------------------------------------------ IMPORTS


#![feature(async_await, futures_api)]
use std::path;
use liborchestra::{hosts, SEND_ARCH_RPATH, FETCH_IGNORE_RPATH, FETCH_ARCH_RPATH,
                   PROFILES_FOLDER_RPATH};
use liborchestra::hosts::{HostConf, HostHandle, LeaveConfig};
use liborchestra::ssh::RemoteHandle;
use liborchestra::primitives::AsResult;
use liborchestra::primitives::{DropBack, Expire};
use clap;
use chrono::prelude::*;
use std::io::prelude::*;
use uuid;
use dirs;
use env_logger;
use log::*;
use futures::executor::block_on;
use futures::task::SpawnExt;
use futures::future::{FutureExt, Future};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use std::process::Output;
use std::io;
use ctrlc;
use std::sync::atomic::{AtomicUsize, Ordering};


//---------------------------------------------------------------------------------------- CONSTANTS


const NAME: &'static str = env!("CARGO_PKG_NAME");
const VERSION: &'static str = env!("CARGO_PKG_VERSION");
const AUTHOR: &'static str = env!("CARGO_PKG_AUTHORS");
const DESC: &'static str = "Execute code on remote hosts.";


//-------------------------------------------------------------------------------------------- MACRO


#[macro_export]
macro_rules! try_return_code {
    ($result:expr, $text:expr, $ecode:expr) => {
        match $result{
            Ok(h) => h,
            Err(e) => {
                eprintln!("runaway: {}: {}", $text, e);
                return $ecode;
            }
        };
    }
}

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


//-------------------------------------------------------------------------------------------- ASYNC
// 
// The different async blocks used to assemble a future that will be run to perform the actual task.
//

/// Packs a folder into an archive and returns its hash.
async fn pack_folder(folder: path::PathBuf) -> Result<u64, String>{
    info!("Job: Packing input data");
    liborchestra::application::pack_folder(folder)
        .map_err(|e| format!("Failed to pack input data: {}", e))
}

/// Acquire a node from a host.
async fn acquire_node(host: HostHandle) -> Result<DropBack<Expire<RemoteHandle>>, String>{
    info!("Job: Acquiring node");
    host.async_acquire()
        .await
        .map_err(|e| format!("Failed to acquire node: {}", e))
}

/// Sends the archive to the remote host given a handle.  
async fn send_data(node: DropBack<Expire<RemoteHandle>>, archive: path::PathBuf, remote_dir: path::PathBuf) 
    -> Result<(), String>{
    info!("Job: Sending input data");
    let already_there = node.async_exec(format!("cd {}", remote_dir.to_str().unwrap()))
        .await
        .map_err(|e| format!("Failed to check for data: {}", e))?;
    info!("Job: Already there: {:?}", already_there);
    if !already_there.status.success() {
        info!("Job: Data not found on remote. Sending...");
        node.async_exec(format!("mkdir {}", remote_dir.to_str().unwrap()))
            .await
            .map_err(|e| format!("Failed to make remote dir: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to make remote dir: {}", e)))?;
        node.async_scp_send(archive, remote_dir.join(SEND_ARCH_RPATH))
            .await
            .map_err(|e| format!("Failed to send input data: {}", e))?;
    } else {
        info!("Job: Data found on remote. Continuing.");
    }
    Ok(())
}

/// Sends the archive to the remote host given a handle.  
async fn send_data_to_front(node: RemoteHandle, archive: path::PathBuf, remote_dir: path::PathBuf) 
    -> Result<(), String>{
    info!("Job: Sending input data");
    let already_there = node.async_exec(format!("cd {}", remote_dir.to_str().unwrap()))
        .await
        .map_err(|e| format!("Failed to check for data: {}", e))?;
    info!("Job: Already there: {:?}", already_there);
    if !already_there.status.success() {
        info!("Job: Data not found on remote. Sending...");
        node.async_exec(format!("mkdir {}", remote_dir.to_str().unwrap()))
            .await
            .map_err(|e| format!("Failed to make remote dir: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to make remote dir: {}", e)))?;
        node.async_scp_send(archive, remote_dir.join(SEND_ARCH_RPATH))
            .await
            .map_err(|e| format!("Failed to send input data: {}", e))?;
    } else {
        info!("Job: Data found on remote. Continuing.");
    }
    Ok(())
}

/// Perform the job on a node. E.g. deflate the data and run the script. 
async fn perform_job(node: DropBack<Expire<RemoteHandle>>,
                     before_exec: String,
                     after_exec: String,
                     script_name: String,
                     parameters: String,
                     remote_defl: String,
                     remote_send: String,
                     stdout_cb: Box<dyn Fn(Vec<u8>)+Send+'static>,
                     stderr_cb: Box<dyn Fn(Vec<u8>)+Send+'static>,
                     )
                     -> Result<Output, String>{
        info!("Job: Deflating input data");
        node.async_exec(format!("mkdir {}", remote_defl))
            .await
            .map_err(|e| format!("Failed to make remote dir: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to make remote dir: {}", e)))?;
        node.async_exec(format!("tar -xf {} -C {}", remote_send, remote_defl))
            .await
            .map_err(|e| format!("Failed to deflate input data: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to deflate input data: {}", e)))?;
        info!("Job: Starting execution");
        node.async_pty(vec!(before_exec,
                            format!("cd {} && ./{} {}", remote_defl, script_name, parameters),
                            after_exec),
                        Some(stdout_cb),
                        Some(stderr_cb))
            .await
            .map_err(|e| format!("Failed to execute: {}", e))
}

/// Packs the data back into an archive, fetches it and unpacks it. 
async fn fetch_data(node: DropBack<Expire<RemoteHandle>>,
                    remote_defl: String,
                    remote_fetch: String,
                    remote_ignore: String,
                    output_path: path::PathBuf) -> Result<(), String>{
        info!("Job: Packing output data");
        node.async_exec(format!("cd {} && (tar -cf {} -X {} * || tar -cf {} *)",
                                 remote_defl,
                                 remote_fetch,
                                 remote_ignore,
                                 remote_fetch))
            .await
            .map_err(|e| format!("Failed to pack output data: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to pack output data: {}", e)))?;
        info!("Job: Fetching output data");
        if !output_path.exists(){
            std::fs::create_dir_all(&output_path);
        }
        node.async_scp_fetch(remote_fetch.into(), output_path.join(FETCH_ARCH_RPATH))
            .await
            .map_err(|e| format!("Failed to fetch output data: {}", e))?;
        info!("Job: Deflating output data");
        liborchestra::application::unpack_arch(output_path.join(FETCH_ARCH_RPATH))
            .map_err(|e| format!("Failed to unpack output data: {}", e))?;
        Ok(())
}

/// Cleans data on both local and remote hand. 
async fn cleaning_data(node: DropBack<Expire<RemoteHandle>>,
                       remote_dir: String,
                       remote_defl: String,
                       input_path: path::PathBuf,
                       output_path: path::PathBuf,
                       leave_config: LeaveConfig,
                       keep_send: bool) -> Result<(), String>{
        info!("Job: Cleaning...");
        match leave_config{
            LeaveConfig::Nothing => {
                node.async_exec(format!("rm -rf {}", remote_dir))
                    .await
                    .map_err(|e| format!("Failed to remove everything: {}", e))?;
            }
            LeaveConfig::Code =>{
                node.async_exec(format!("rm -rf {}", remote_defl))
                    .await
                    .map_err(|e| format!("Failed to remove data: {}", e))?;

            }
            LeaveConfig::Everything => {}
        }
        if !keep_send{
            std::fs::remove_file(&input_path.join(SEND_ARCH_RPATH))
                .map_err(|e| format!("Failed to remove send archive"))?;
        }
        std::fs::remove_file(&output_path.join(FETCH_ARCH_RPATH))
            .map_err(|e| format!("Failed to remove the fetch archive"))?;
        Ok(())
}


//-------------------------------------------------------------------------------------- SUBCOMMANDS
//
// Functions representing every subcommands of the program. 
//

/// Executes a single execution of the script with the command arguments and returns exit code.
fn exec(matches: &clap::ArgMatches) -> i32 {

    if matches.is_present("vvverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=TRACE,liborchestra=TRACE,liborchestra::ssh=DEBUG");
    } else if matches.is_present("vverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=DEBUG,liborchestra=DEBUG,liborchestra::ssh=INFO");
    } else if matches.is_present("verbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=INFO,liborchestra=INFO,liborchestra::ssh=INFO");
    }

    env_logger::init();

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
    install_ctrlc_handler(host.clone());

    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    let script_path = matches.value_of("SCRIPT").unwrap();

    let parameters = matches.value_of("parameters").unwrap_or("");

    let script_abs_path = try_return_code!(std::fs::canonicalize(script_path),
                                            "failed to get absolute script path",
                                            3);
    let script_folder = try_return_code!(script_abs_path.parent().ok_or("None"),
                                          "failed to get script folder",
                                          4);
    let script_name = try_return_code!(script_abs_path.file_name().ok_or("Non"),
                                        "failed to get script name",
                                        5);
    let future = async move{
        let hash = try_return_err!(pack_folder(script_folder.into()).await);
        let remote_dir = host.get_host_directory().join(format!("{}", hash));
        let remote_send = remote_dir.join(SEND_ARCH_RPATH);
        let id = uuid::Uuid::new_v4();
        let remote_defl = remote_dir.join(format!("{}", id));
        let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
        let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);

        let node = try_return_err!(acquire_node(host.clone()).await);
        try_return_err!(send_data(node.clone(),
                                  script_folder.join(SEND_ARCH_RPATH),
                                  remote_dir.clone()).await);
        let stdout_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            println!("{}", string);
        });
        let stderr_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            eprintln!("{}", string);
        });
        let out = try_return_err!(perform_job(node.clone(),
                                              host.get_before_execution_command(),
                                              host.get_after_execution_command(),
                                              script_name.to_str().unwrap().to_owned(),
                                              parameters.to_owned(),
                                              remote_defl.to_str().unwrap().to_owned(),
                                              remote_send.to_str().unwrap().to_owned(),
                                              stdout_callback,
                                              stderr_callback).await);
        try_return_err!(fetch_data(node.clone(),
                                   remote_defl.to_str().unwrap().to_owned(),
                                   remote_fetch.to_str().unwrap().to_owned(),
                                   remote_ignore.to_str().unwrap().to_owned(),
                                   script_folder.into()).await);
        try_return_err!(cleaning_data(node.clone(),
                                      remote_dir.to_str().unwrap().to_owned(),
                                      remote_defl.to_str().unwrap().to_owned(),
                                      script_folder.into(),
                                      script_folder.into(),
                                      leave,
                                      false).await);
        Ok(out.status.code().unwrap_or(911))
    };
    let out = try_return_code!(block_on(future),
                               "failed to perform job",
                               6);
    return out;
}

// Executes a batch of executions on a remote host.
fn batch(matches: &clap::ArgMatches) -> i32 {

    if matches.is_present("vvverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=TRACE,liborchestra=TRACE,liborchestra::ssh=DEBUG");
    } else if matches.is_present("vverbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=DEBUG,liborchestra=DEBUG,liborchestra::ssh=INFO");
    } else if matches.is_present("verbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=INFO,liborchestra=INFO,liborchestra::ssh=INFO");
    } else if matches.is_present("benchmark"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=INFO,liborchestra::hosts=TRACE/Host: Allocation succeeded|Allocation cancelling succeeded|Job: Starting execution");
    }

    env_logger::init();

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
    install_ctrlc_handler(host.clone());

    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());

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
            let trimmed = params.trim_left_matches('\n').trim_right_matches('\n');
            trimmed.lines()
                .map(|e| e.to_owned())
                .collect::<Vec<String>>()
        }
        None => {
            let repeats = try_return_code!(matches.value_of("repeats").unwrap().parse::<u32>(),
                                            "failed to read the number of repeats",
                                            8);
            let params = matches.value_of("parameters_string").unwrap_or("");
            let params = parse_parameters(&params, repeats as usize);
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
            let trimmed = folders.trim_left_matches('\n').trim_right_matches('\n');
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

    let mut executor = try_return_code!(futures::executor::ThreadPoolBuilder::new()
        //.pool_size(1)
        .name_prefix("runaway-worker")
        .create(),
        "failed to spawn worker",
        12);

    let script_abs_path = try_return_code!(std::fs::canonicalize(script_path),
                                            "failed to get absolute script path",
                                            3);
    let script_folder = try_return_code!(script_abs_path.parent().ok_or("None"),
                                          "failed to get script folder",
                                          4).to_owned();
    let script_name = try_return_code!(script_abs_path.file_name().ok_or("Non"),
                                        "failed to get script name",
                                        5).to_owned();
    let hash = try_return_code!(block_on(pack_folder(script_folder.clone())),
                                "failed to pack folder",
                                11);
    let remote_dir = host.get_host_directory().join(format!("{}", hash));
    let remote_send = remote_dir.join(SEND_ARCH_RPATH);
    try_return_code!(block_on(send_data_to_front(host.get_frontend(), script_folder.join(SEND_ARCH_RPATH), remote_dir.clone())),
                     "failed to send data",
                     13);

    let l = match leave{
            LeaveConfig::Everything => LeaveConfig::Everything,
            LeaveConfig::Nothing => LeaveConfig::Code,
            LeaveConfig::Code => LeaveConfig::Code,
    };

    let len = outputs.len();

    let mut handles = Vec::new();
    for (i, (p, b)) in params.into_iter().zip(outputs.into_iter()).enumerate(){
        let l = l.clone();
        let script_name = script_name.clone();
        let script_folder = script_folder.clone();
        let host = host.clone();
        let remote_dir = remote_dir.clone();
        let remote_send = remote_send.clone();
        let id = uuid::Uuid::new_v4();
        let remote_defl = remote_dir.join(format!("{}", id));
        let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
        let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
        let benchmark = matches.is_present("benchmark");

        let future = async move{
            let p = p.clone();
            let node = try_return_err!(acquire_node(host.clone()).await);
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
            let out = try_return_err!(perform_job(node.clone(),
                                                  host.get_before_execution_command(),
                                                  host.get_after_execution_command(),
                                                  script_name.to_str().unwrap().to_owned(),
                                                  p,
                                                  remote_defl.to_str().unwrap().to_owned(),
                                                  remote_send.to_str().unwrap().to_owned(),
                                                  stdout_callback,
                                                  stderr_callback).await);
            try_return_err!(fetch_data(node.clone(),
                                       remote_defl.to_str().unwrap().to_owned(),
                                       remote_fetch.to_str().unwrap().to_owned(),
                                       remote_ignore.to_str().unwrap().to_owned(),
                                       b.clone()).await);
            try_return_err!(cleaning_data(node.clone(),
                                          remote_dir.to_str().unwrap().to_owned(),
                                          remote_defl.to_str().unwrap().to_owned(),
                                          script_folder.into(),
                                          b.clone(),
                                          l,
                                          true).await);
            let mut stdout_file = std::fs::File::create(b.join("stdout")).unwrap();
            stdout_file.write_all(&out.stdout).unwrap();
            let mut stderr_file = std::fs::File::create(b.join("stderr")).unwrap();
            stderr_file.write_all(&out.stderr).unwrap();
            let mut ecode_file = std::fs::File::create(b.join("ecode")).unwrap();
            ecode_file.write_all(format!("{}", out.status.code().unwrap_or(911)).as_bytes()).unwrap();

            Ok(out.status.code().unwrap_or(911))
        };
        let handle = try_return_code!(executor.spawn_with_handle(future).map_err(|e| format!("{:?}", e)),
                                      "failed to spawn future",
                                      14);
        info!("Job: Spawned job");
        handles.push(handle);
    };
    let future = futures::future::join_all(handles);
    let output = executor.run(future);
    let completed = output.into_iter()
        .inspect(|o| if o.is_err(){error!("Failed to execute a job: {:?}",o)})
        .filter_map(|c| c.ok())
        .collect::<Vec<_>>()
        .len();
    if let LeaveConfig::Nothing = leave{
        executor.run(host.get_frontend()
            .async_exec(format!("rm -rf {}", remote_dir.to_str().unwrap())))
            .map_err(|e| error!("Failed to remove code."));
    }
    if completed == len{
        eprintln!("runaway: brought all jobs to completion.");
        return 0;
    } else{
        eprintln!("runaway: brought {} jobs to completion over {}.", 
            completed,
            len);
        return 911;
    }
    0
}

// Executes the test and returns the exit code.
fn test(matches: &clap::ArgMatches) -> i32 {

    if matches.is_present("verbose"){
        std::env::set_var("RUST_LOG", "WARNING,runaway_cli=TRACE,liborchestra=TRACE,liborchestra::ssh=TRACE");
    }

    env_logger::init();
    
    eprintln!("runaway: opening configuration");
    let config = try_return_code!(HostConf::from_file(&matches.value_of("FILE").unwrap().into()),
                                   "can not load the host configuration",
                                   90);

    eprintln!("runaway: opening ssh config file");
    let profile = try_return_code!(liborchestra::ssh::config::get_profile(
                    &dirs::home_dir().unwrap().join(liborchestra::SSH_CONFIG_RPATH),
                    &config.ssh_config), 
            "failed to load ssh config", 
            91);
    eprintln!("runaway: spawning frontend");
    let frontend = try_return_code!(liborchestra::ssh::RemoteHandle::spawn(profile.clone()),
                                    "failed to connect to frontend",
                                    92);
    eprintln!("runaway: connection successful");

    let command = config.start_alloc();
    eprintln!("runaway: starting allocation with command: ```{}```", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
                               "failed to start allocation",
                               93);
    eprintln!("runaway: returned: {:#?}", out);

    let alloc_ret = String::from_utf8(out.stdout).unwrap();
    let command = config.get_alloc_nodes().replace("$ALLOCRET", alloc_ret.trim_right_matches('\n'));
    eprintln!("runaway: getting nodes with command: ```{}```", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
        "failed to get nodes",
        94);
    eprintln!("runaway: returned: {:#?}", out);

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

    let command = config.before_execution();
    eprintln!("runaway: executing before_exec command {}", command);
    let out = try_return_code!(block_on(node.async_exec(command)),
        "failed to execute before_exec command",
        96);
    eprintln!("runaway: command returned: {:#?}", out);
    
    let command = config.after_execution();
    eprintln!("runaway: executing after_exec command {}", command);
    let out = try_return_code!(block_on(node.async_exec(command)),
        "failed to execute after_exec command",
        97);
    eprintln!("runaway: command returned: {:#?}", out);

    let command = config.cancel_alloc().replace("$ALLOCRET", &alloc_ret);
    eprintln!("runaway: cancelling allocation with command {}", command);
    let out = try_return_code!(block_on(frontend.async_exec(command)),
        "failed to cancel allocation",
        98);
    eprintln!("runaway: returned: {:#?}", out);

    let command = config.before_execution();
    eprintln!("runaway: trying a dummy command on node (should fail) {}", command);
    let out = block_on(node.async_exec(command));
    eprintln!("runaway: command returned: {:#?}", out);

    0
}

// Allows to parse cartesian product strings to generate a set of parameters. 
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

// Installs a ctrlc handler that takes care about cancelling the allocation on the host before 
// leaving. 
pub fn install_ctrlc_handler(signal_host: HostHandle){
    // We have to downgrade the handle to the host, because the ctrl-c handler is not dropped during
    // program execution. For this reason we have to downgrade the dropper. 
    let mut signal_host = signal_host;
    signal_host.downgrade();
    let signal_counter = AtomicUsize::new(0);
    ctrlc::set_handler(move || {
        eprintln!("runaway: received ctrl-c.");
        let mut host = signal_host.clone();
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


//--------------------------------------------------------------------------------------------- MAIN


fn main(){
    
    // We parse the arguments
    let matches = clap::App::new(NAME)
        .version(VERSION)
        .about(DESC)
        .author(AUTHOR)
        .setting(clap::AppSettings::ArgRequiredElseHelp) 
        .about("Execute code on remote hosts")
        .subcommand(clap::SubCommand::with_name("exec")
            .about("Runs a single execution on a remote host")
            .arg(clap::Arg::with_name("SCRIPT")
                .help("File name of the script to be executed")
                .index(2)
                .required(true))
            .arg(clap::Arg::with_name("REMOTE")
                .help("Name of remote profile to execute script with")
                .index(1)
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
                        .help("Script parameters. In normal mode, it should be written as they would \
                               be for the program to execute. In batch mode, you can use a product \
                               parameters string.")
                        .multiple(true)
                        .allow_hyphen_values(true)
                        .last(true))
        )
        .subcommand(clap::SubCommand::with_name("batch")
            .about("Runs a batch of executions on a remote host")
            .arg(clap::Arg::with_name("SCRIPT")
                .help("File name of the script to be executed")
                .index(2)
                .required(true))
            .arg(clap::Arg::with_name("REMOTE")
                .help("Name of remote profile to execute script with")
                .index(1)
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
            .arg(clap::Arg::with_name("repeats")
                .short("R")
                .long("repeats")
                .takes_value(true)
                .default_value("1")
                .help("The number of time every parameter must be repeated. Used with product string."))
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
            .arg(clap::Arg::with_name("parameters_string")
                        .help("Script parameters product string.")
                        .multiple(true)
                        .allow_hyphen_values(true)
                        .last(true))
        )
        .subcommand(clap::SubCommand::with_name("test")
             .about("Tests a remote profile")
            .arg(clap::Arg::with_name("verbose")
                .long("verbose")
                .help("Print light logs"))
             .arg(clap::Arg::with_name("FILE")
                 .help("The yaml profile to test.")
                 .index(1)
                 .required(true)))

        .get_matches();

    // We execute and exit;
    if let Some(matches) = matches.subcommand_matches("test"){
        std::process::exit(test(matches));
    } else if let Some(matches) = matches.subcommand_matches("exec"){
        std::process::exit(exec(matches));
    } else if let Some(matches) = matches.subcommand_matches("batch"){
        std::process::exit(batch(matches));
    }
}