//! runaway-cli/subcommands/exec.rs
//! Author: Alexandre Péré
//! 
//! This module contains the exec subcommand. 
//! 
//! Todos: 
//!     + Makes it possible to use patterns for output folder.


//-------------------------------------------------------------------------------------------IMPORTS


use liborchestra::{
    SEND_ARCH_RPATH, 
    FETCH_ARCH_RPATH};
use liborchestra::hosts::LeaveConfig;
use clap;
use uuid;
use futures::executor::block_on;
use crate::{to_exit};
use liborchestra::commons::{EnvironmentValue, EnvironmentStore, 
    substitute_environment, OutputBuf, push_env};
use crate::misc;
use crate::exit::Exit;
use liborchestra::primitives;
use std::path::{PathBuf, Path};
use tracing::{self, info, error, debug};


//-------------------------------------------------------------------------------------- SUBCOMMANDS


/// Executes a single execution of the script with the command arguments and returns exit code.
pub fn exec(matches: clap::ArgMatches) -> Result<Exit, Exit>{


    // We initialize the logger
    misc::init_logger(&matches);

    // We create the store that will keep env vars
    let mut store = EnvironmentStore::new();

    // We load the host
    info!("Loading host");
    let host = misc::get_host(matches.value_of("REMOTE").unwrap())?;
    push_env(&mut store, "RUNAWAY_REMOTE", host.get_name());
    debug!("Host {:?} loaded", host);


    // We install ctrl-c handler
    misc::install_ctrlc_handler(Some(host.clone()), None);


    // We setup some parameters
    info!("Reading arguments");
    let leave;
    if matches.is_present("on-local"){
        leave = LeaveConfig::Everything;
    } else{
        leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    }
    push_env(&mut store, "RUNAWAY_LEAVE", format!("{}", leave));
    debug!("Leave option set to {}", leave);
    let parameters = matches.value_of("ARGUMENTS").unwrap_or("").to_owned();
    push_env(&mut store, "RUNAWAY_ARGUMENTS", parameters.clone());
    debug!("Arguments set to {}", parameters);
    let script = PathBuf::from(matches.value_of("SCRIPT").unwrap());
    push_env(&mut store, "RUNAWAY_SCRIPT_PATH", script.to_str().unwrap());
    debug!("Script set to {}", script.to_str().unwrap());
    let local_folder = to_exit!(std::env::current_dir(), Exit::ScriptFolder)?;
    push_env(&mut store, "RUNAWAY_LOCAL_FOLDER", host.get_name());
    debug!("Local folder is {}", local_folder.to_str().unwrap());
    if !script.exists() {
        return Err(Exit::ScriptPath)
    }


    // We list files to send
    info!("Reading ignore files");
    let (mut send_ignore_globs, mut fetch_ignore_globs) = misc::get_send_fetch_ignores_globs(&local_folder, 
        matches.value_of("send-ignore").unwrap(),
        matches.value_of("fetch-ignore").unwrap())?;
    send_ignore_globs.push(primitives::Glob(format!("**/{}", SEND_ARCH_RPATH)));
    send_ignore_globs.push(primitives::Glob(matches.value_of("send-ignore").unwrap().into()));
    send_ignore_globs.push(primitives::Glob(matches.value_of("fetch-ignore").unwrap().into()));
    if matches.is_present("on-local"){
        fetch_ignore_globs = vec!(primitives::Glob("*".into()));
    }
    debug!("Sendignore globs set to {}", send_ignore_globs.iter()
        .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.0)); acc}));
    debug!("Fetchignore globs set to {}", fetch_ignore_globs.iter()
        .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.0)); acc}));
    let send_include_globs = vec!();
    let fetch_include_globs = vec!();



    // We declare the future 
    let future = async move{
        
        // We copy the matches
        let matches = matches.clone();
        
        // We generate an uuid
        let id = uuid::Uuid::new_v4().hyphenated().to_string();
        push_env(&mut store, "RUNAWAY_UUID", id.clone());
        debug!("Execution id set to {}", id);

        
        // We list the files
        let files_to_send = to_exit!(primitives::list_local_folder(&local_folder, 
                                                                   &send_ignore_globs, 
                                                                   &send_include_globs),
                                     Exit::ReadLocalFolder)?;
        debug!("Files to be send to remote: {}", files_to_send.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


        // We pack the folder
        info!("Compress files");
        let local_send_archive = local_folder.join(SEND_ARCH_RPATH);
        let local_send_hash = to_exit!(primitives::tar_local_files(&local_folder, 
                                                        &files_to_send,
                                                        &local_send_archive),
                            Exit::PackLocalArchive)?;
        push_env(&mut store, "RUNAWAY_SEND_HASH", format!("{}", local_send_hash));
        debug!("Archive hash is {}", local_send_hash);


        // We acquire the node
        info!("Acquiring node on the host");
        let node = to_exit!(host.clone().async_acquire().await,
                                Exit::NodeAcquisition)?;
        store.extend(node.context.envs.clone().into_iter());
        debug!("Node {:#?} acquired", node);

        // We update the context to append some values of interest
        if !matches.is_present("no-env-read"){
            let envs = misc::read_local_runaway_envs();
            debug!("Local environment variables captured: {}", envs.iter()
                .fold(String::new(), |mut acc, (k, v)| {acc.push_str(&format!("\n{:?}={:?}", k, v)); acc}));
            envs.into_iter()
                .for_each(|(k, v)| {push_env(&mut store, k.0, v.0);});
        }

        // We send the data, if needed.
        info!("Transferring data");
        let remote_send_archive = host.get_host_directory().join(format!("{}.tar",local_send_hash));
        push_env(&mut store, "RUNAWAY_SEND_ARCH_PATH", remote_send_archive.to_str().unwrap());
        let remote_send_exists = to_exit!(primitives::remote_file_exists(&remote_send_archive, &node).await,
                                          Exit::CheckPresence)?;
        if !remote_send_exists{
            debug!("Archive does not exist on host. Sending data...");
            to_exit!(primitives::send_local_file(&local_send_archive, &remote_send_archive, &node).await,
                     Exit::SendArchive)?;
            let remote_send_hash = to_exit!(primitives::compute_remote_sha1(&remote_send_archive, &node).await,
                                            Exit::ComputeRemoteHash)?;
            if remote_send_hash != local_send_hash{
                error!("Differing local and remote hashs for send archive: local is {} and \\
                           remote is {}", local_send_hash, remote_send_hash); 
                return Err(Exit::Send)
            }
        }
        to_exit!(std::fs::remove_file(local_send_archive),
                 Exit::RemoveArchive)?;


        // We substitute the remote folder and create it if needed
        let remote_folder;
        if matches.is_present("on-local"){
            let remote_path = PathBuf::from(
                substitute_environment(&store, matches.value_of("output-folder").unwrap()));
            remote_folder =  to_exit!(remote_path.canonicalize(), Exit::OutputFolder)?;
        } else {
            remote_folder = PathBuf::from(
                substitute_environment(&store, matches.value_of("remote-folder").unwrap()));
        }
        debug!("Remote folder set to {}", remote_folder.to_str().unwrap());
        if remote_send_archive.is_relative(){return Err(Exit::WrongRemoteFolderString)}
        let remote_folder_exists = to_exit!(primitives::remote_folder_exists(&remote_folder, &node).await,
                                            Exit::CheckRemotePresence)?;
        let remote_files_before;
        if remote_folder_exists{
            debug!("Remote folder already exists. Listing existing files.");
            let globs = vec!();
            remote_files_before = to_exit!(primitives::list_remote_folder(&remote_folder,
                                                                          &globs,
                                                                          &globs,
                                                                          &node).await,
                                           Exit::ReadRemoteFolder)?;
            debug!("Files encountered: {}", remote_files_before.iter()
                .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));
        } else {
            debug!("Creating remote folder.");
            to_exit!(primitives::create_remote_folder(&remote_folder, &node).await,
                     Exit::CreateRemoteFolder)?;
            remote_files_before = vec!();
        }
        push_env(&mut store, "RUNAWAY_PWD", remote_folder.to_str().unwrap());


        // We unpack the data in the remote folder
        info!("Extracting data in remote folder");
        let remote_files = to_exit!(primitives::untar_remote_archive(&remote_send_archive,
                                                                     &remote_folder,
                                                                     &node).await,
                                    Exit::UnpackRemoteArchive)?;
        debug!("Files extracted in remote folder: {}", remote_files.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


        // Depending on the leave options, we remove the send archive
        if let LeaveConfig::Nothing = leave{
            info!("Removing archive");
            to_exit!(primitives::remove_remote_files(vec!(remote_send_archive), &node).await,
                     Exit::Cleanup)?;
        }


        // We perform the job
        info!("Executing script");
        let stdout_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            print!("{}", string);
        });
        let stderr_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            eprint!("{}", string);
        });
        let mut context = node.context.clone();
        context.envs.extend(store.into_iter());
        let (mut execution_context, outs) = to_exit!(node.async_pty(
                context,
                host.get_execution_procedure(),
                Some(stdout_callback), 
                Some(stderr_callback)).await,
            Exit::Execute)?;
        let out: OutputBuf = liborchestra::misc::compact_outputs(outs.clone()).into();
        push_env(&mut execution_context.envs, "RUNAWAY_ECODE", format!("{}", out.ecode));
        push_env(&mut execution_context.envs, "RUNAWAY_STDOUT", &out.stdout);
        push_env(&mut execution_context.envs, "RUNAWAY_STDERR", &out.stderr);

        
        // We list the files to fetch
        let files_to_fetch = to_exit!(primitives::list_remote_folder(&remote_folder,
                                                                     &fetch_ignore_globs,
                                                                     &fetch_include_globs,
                                                                     &node).await,
                                    Exit::ReadRemoteFolder)?;
        debug!("Files to be fetched from remote: {}", files_to_fetch.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


        // We pack data to fetch
        info!("Compressing data to be fetched");
        let remote_fetch_archive = remote_folder.join(".to_fetch.tar");
        let remote_fetch_hash = to_exit!(primitives::tar_remote_files(&remote_folder,
                                                                     &files_to_fetch,
                                                                     &remote_fetch_archive,
                                                                     &node).await,
                                         Exit::PackRemoteArchive)?;
        debug!("Archive hash is {}", remote_fetch_hash);


        // We fetch data back
        let local_output_folder;
        if matches.is_present("on-local"){
            local_output_folder = remote_folder.clone();
        } else{
            let local_output_string = substitute_environment(&execution_context.envs, matches.value_of("output-folder").unwrap());
            local_output_folder = to_exit!(PathBuf::from(local_output_string).canonicalize(), Exit::OutputFolder)?;
        }
        debug!("Local output folder set to: {}", local_output_folder.to_str().unwrap());
        if !local_output_folder.exists(){
            debug!("Creating output folder");
            to_exit!(std::fs::create_dir_all(&local_output_folder), Exit::OutputFolder)?;
        }
        push_env(&mut execution_context.envs, "RUNAWAY_OUTPUT_FOLDER", local_output_folder.to_str().unwrap());
        let local_fetch_archive = local_output_folder.join(FETCH_ARCH_RPATH);
        info!("Transferring data");
        to_exit!(primitives::fetch_remote_file(&remote_fetch_archive,
                                               &local_fetch_archive,
                                               &node).await,
                 Exit::Fetch)?;
        let local_fetch_hash = to_exit!(primitives::compute_local_sha1(&local_fetch_archive),
                                        Exit::ComputeLocalHash)?;
        if remote_fetch_hash != local_fetch_hash{
            error!("Differing local and remote hashs for fetch archive: local is {} and \\
                       remote is {}", local_fetch_hash, remote_fetch_hash);
            return Err(Exit::Fetch)
        }
        to_exit!(primitives::remove_remote_files(vec!(remote_fetch_archive), &node).await,
                 Exit::RemoveArchive)?;


        // We unpack the data
        info!("Extracting archive");
        let local_files = to_exit!(primitives::untar_local_archive(&local_fetch_archive, &local_output_folder),
                 Exit::UnpackRemoteArchive)?;
        to_exit!(std::fs::remove_file(local_fetch_archive), Exit::RemoveArchive)?;
        debug!("Files fetched from remote: {}", local_files.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


        // Depending on the leave config, we clean the remote execution folder
        info!("Cleaning data on remote");
        match leave {
            LeaveConfig::Code | LeaveConfig::Nothing => {
                if remote_files_before.is_empty(){
                    debug!("No files were there, removing everything");
                    to_exit!(primitives::remove_remote_folder(remote_folder, &node).await,
                             Exit::Cleanup)?;
                } else {
                    debug!("Files existed before, keeping those.");
                    let ignore = remote_files_before.iter()
                        .map(AsRef::as_ref)
                        .map(Path::to_str)
                        .map(Option::unwrap)
                        .map(ToOwned::to_owned)
                        .map(primitives::Glob)
                        .collect();
                    let globs = vec!();
                    let remote_files_to_remove = to_exit!(primitives::list_remote_folder(&remote_folder, 
                                                                               &ignore,
                                                                               &globs,
                                                                               &node).await,
                                                          Exit::Cleanup)?;
                    debug!("Files to be removed: {}", remote_files_to_remove.iter()
                        .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));
                    to_exit!(primitives::remove_remote_files(remote_files_to_remove, &node).await,
                             Exit::Cleanup)?;
                }
            }
            _ => {}
        } 

        // We copy the ecode eventually.
        if matches.is_present("no-ecode"){
            Ok(Exit::AllGood)
        } else {
            if out.ecode!=0{
                let outs:Vec<OutputBuf> = outs.into_iter()
                    .map(Into::into)
                    .collect();
                error!("Failed to execute command: {}",
                    host.get_execution_procedure().get(outs.len() - 1).unwrap().0);
                error!("     stdout: {}", outs.last().unwrap().stdout);
                error!("     stderr: {}", outs.last().unwrap().stderr);
                error!("     ecode: {}",  outs.last().unwrap().ecode);
                Ok(Exit::ScriptFailedWithCode(outs.last().unwrap().ecode))
            } else {
                Ok(Exit::AllGood)
            }
        }
    };

    // We execute the future
    block_on(future)
}