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
use liborchestra::commons::{EnvironmentKey, EnvironmentValue, substitute_environment, OutputBuf};
use crate::misc;
use crate::exit::Exit;
use liborchestra::primitives;
use std::path::{PathBuf, Path};
use tracing::{self, info, error};


//-------------------------------------------------------------------------------------- SUBCOMMANDS


/// Executes a single execution of the script with the command arguments and returns exit code.
pub fn exec(matches: clap::ArgMatches) -> Result<Exit, Exit>{


    // We initialize the logger
    misc::init_logger(&matches);


    // We load the host
    let host = misc::get_host(matches.value_of("REMOTE").unwrap())?;


    // We install ctrl-c handler
    misc::install_ctrlc_handler(Some(host.clone()), None);


    // We setup some parameters
    let leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    let parameters = matches.value_of("ARGUMENTS").unwrap_or("").to_owned();
    let script = PathBuf::from(matches.value_of("SCRIPT").unwrap());
    let local_folder = to_exit!(std::env::current_dir(), Exit::ScriptFolder)?;
    if !script.exists() {
        return Err(Exit::ScriptPath)
    }


    // We list files to send
    let (mut send_ignore_globs, fetch_ignore_globs) = misc::get_send_fetch_ignores_globs(&local_folder, 
        matches.value_of("send-ignore").unwrap(),
        matches.value_of("fetch-ignore").unwrap())?;
    send_ignore_globs.push(primitives::Glob(format!("**/{}", SEND_ARCH_RPATH)));
    let send_include_globs = vec!();
    let fetch_include_globs = vec!();


    // We declare the future 
    let future = async move{
        
        // We copy the matches
        let matches = matches.clone();
        
        // We generate an uuid
        let id = uuid::Uuid::new_v4().hyphenated().to_string();

        
        // We list the files
        let files_to_send = to_exit!(primitives::list_local_folder(&local_folder, 
                                                                   &send_ignore_globs, 
                                                                   &send_include_globs),
                                     Exit::ReadLocalFolder)?;
        if matches.is_present("print-files"){
            info!("Files to be send to remote :");
            files_to_send.iter()
                .for_each(|e| info!("     {}", e.to_str().unwrap()))
        }


        // We pack the folder
        let local_send_archive = local_folder.join(SEND_ARCH_RPATH);
        let local_send_hash = to_exit!(primitives::tar_local_files(&local_folder, 
                                                        &files_to_send,
                                                        &local_send_archive),
                            Exit::PackLocalArchive)?;


        // We acquire the node
        let mut node = to_exit!(host.clone().async_acquire().await,
                                Exit::NodeAcquisition)?;

        // We update the context to append some values of interest
        if !matches.is_present("no-env-read"){
            misc::read_local_runaway_envs().into_iter()
                .for_each(|(k, v)| {node.context.envs.insert(k, v);});
        }
        node.context.envs.insert(EnvironmentKey("RUNAWAY_UUID".into()), EnvironmentValue(id.clone()));
        node.context.envs.insert(EnvironmentKey("RUNAWAY_SEND_HASH".into()), EnvironmentValue(local_send_hash.clone().into()));
        node.context.envs.insert(EnvironmentKey("RUNAWAY_ARGUMENTS".into()), EnvironmentValue(parameters.into()));
        node.context.envs.insert(EnvironmentKey("RUNAWAY_SCRIPT_PATH".into()), EnvironmentValue(script.to_str().unwrap().into()));
        node.context.envs.insert(EnvironmentKey("RUNAWAY_LOCAL_FOLDER".into()), EnvironmentValue(local_folder.to_str().unwrap().into()));


        // We send the data, if needed.
        let remote_send_archive = host.get_host_directory().join(format!("{}.tar",local_send_hash));
        node.context.envs.insert(EnvironmentKey("RUNAWAY_SEND_ARCH_PATH".into()),  
                                 EnvironmentValue(remote_send_archive.to_str().unwrap().to_owned()));
        let remote_send_exists = to_exit!(primitives::remote_file_exists(&remote_send_archive, &node).await,
                                          Exit::CheckPresence)?;
        if !remote_send_exists{
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
        let remote_folder= PathBuf::from(
            substitute_environment(&node.context.envs, 
                                   matches.value_of("remote-folder").unwrap())
            );
        if remote_send_archive.is_relative(){return Err(Exit::WrongRemoteFolderString)}
        let remote_folder_exists = to_exit!(primitives::remote_folder_exists(&remote_folder, &node).await,
                                            Exit::CheckRemotePresence)?;
        let remote_files_before;
        if remote_folder_exists{
            let globs = vec!();
            remote_files_before = to_exit!(primitives::list_remote_folder(&remote_folder,
                                                                          &globs,
                                                                          &globs,
                                                                          &node).await,
                                           Exit::ReadRemoteFolder)?;
        } else {
            to_exit!(primitives::create_remote_folder(&remote_folder, &node).await,
                     Exit::CreateRemoteFolder)?;
            remote_files_before = vec!();
        }
        node.context.envs.insert(EnvironmentKey("RUNAWAY_PWD".into()),
                                 EnvironmentValue(remote_folder.to_str().unwrap().to_owned()));


        // We unpack the data in the remote folder
        let remote_files = to_exit!(primitives::untar_remote_archive(&remote_send_archive,
                                                                     &remote_folder,
                                                                     &node).await,
                                    Exit::UnpackRemoteArchive)?;
        if matches.is_present("print-files"){
            info!("Files received on remote :");
            remote_files.iter()
                .for_each(|e| info!("     {}", e.to_str().unwrap()))
        }


        // Depending on the leave options, we remove the send archive
        if let LeaveConfig::Nothing = leave{
            to_exit!(primitives::remove_remote_files(vec!(remote_send_archive), &node).await,
                     Exit::Cleanup)?;
        }


        // We perform the job
        let stdout_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            print!("{}", string);
        });
        let stderr_callback = Box::new(|a|{
            let string = String::from_utf8(a).unwrap().replace("\r\n", "");
            eprint!("{}", string);
        });
        let (context, outs) = to_exit!(node.async_pty(
                node.context.clone(),
                host.get_execution_procedure(),
                Some(stdout_callback), 
                Some(stderr_callback)).await,
            Exit::Execute)?;
        let outs: Vec<OutputBuf> = outs.into_iter()
            .map(Into::into)
            .collect();


        // We list the files to fetch
        let files_to_fetch = to_exit!(primitives::list_remote_folder(&remote_folder,
                                                                     &fetch_ignore_globs,
                                                                     &fetch_include_globs,
                                                                     &node).await,
                                    Exit::ReadRemoteFolder)?;
        if matches.is_present("print-files"){
            info!("Files to be fetched from remote :");
            files_to_fetch.iter()
                .for_each(|e| info!("     {}", e.to_str().unwrap()))
        }


        // We pack data to fetch
        let remote_fetch_archive = remote_folder.join(FETCH_ARCH_RPATH);
        let remote_fetch_hash = to_exit!(primitives::tar_remote_files(&remote_folder,
                                                                     &files_to_fetch,
                                                                     &remote_fetch_archive,
                                                                     &node).await,
                                         Exit::PackRemoteArchive)?;


        // We fetch data back
        let local_output_string = substitute_environment(&context.envs, matches.value_of("output-folder").unwrap());
        let local_output_folder = to_exit!(PathBuf::from(local_output_string).canonicalize(), Exit::OutputFolder)?;
        if !local_output_folder.exists(){
            to_exit!(std::fs::create_dir_all(&local_output_folder), Exit::OutputFolder)?;
        } 
        let local_fetch_archive = local_output_folder.join(FETCH_ARCH_RPATH);
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
        let local_files = to_exit!(primitives::untar_local_archive(&local_fetch_archive, &local_output_folder),
                 Exit::UnpackRemoteArchive)?;
        to_exit!(std::fs::remove_file(local_fetch_archive), Exit::RemoveArchive)?;
        if matches.is_present("print-files"){
            info!("Files fetched on local :");
            local_files.iter()
                .for_each(|e| info!("     {}", e.to_str().unwrap()))
        }


        // Depending on the leave config, we clean the remote execution folder
        match leave {
            LeaveConfig::Code | LeaveConfig::Nothing => {
                if remote_files_before.is_empty(){
                    to_exit!(primitives::remove_remote_folder(remote_folder, &node).await,
                             Exit::Cleanup)?;
                } else {
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
            if !outs.last().unwrap().success(){
                error!("Failed to execute command: {}", 
                    host.get_execution_procedure().get(outs.len() - 1).unwrap().0);
                error!("     stdout: {}", outs.last().unwrap().stdout);
                error!("     stderr: {}", outs.last().unwrap().stderr);
                error!("     ecode: {}", outs.last().unwrap().ecode);
                Ok(Exit::ScriptFailedWithCode(outs.last().unwrap().ecode))
            } else {
                Ok(Exit::AllGood)
            }
        }
    };

    // We execute the future
    block_on(future)
}