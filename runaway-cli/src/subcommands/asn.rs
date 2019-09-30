//! runaway-cli/subcommands/asn.rs
//! Author: Alexandre Péré
//! 
//! This module contains all the bits of asynchronous code used by the subcommands to perform their
//! tasks.


//-------------------------------------------------------------------------------------------IMPORTS


use std::path;
use liborchestra::{
    SEND_ARCH_RPATH, 
    FETCH_ARCH_RPATH};
use liborchestra::hosts::{HostHandle, LeaveConfig};
use liborchestra::ssh::RemoteHandle;
use liborchestra::primitives::AsResult;
use liborchestra::primitives::{DropBack, Expire};
use log::*;
use std::process::Output;


//-------------------------------------------------------------------------------------------- ASYNC

/// Acquire a node from a host.
pub async fn acquire_node(host: HostHandle) -> Result<DropBack<Expire<RemoteHandle>>, String>{
    info!("Job: Acquiring node");

    // We acquire a node on the host.
    host.async_acquire()
        .await
        .map_err(|e| format!("Failed to acquire node: {}", e))
}

/// Sends the archive to the remote host given a handle.  
pub async fn send_data(node: DropBack<Expire<RemoteHandle>>, 
                   archive: path::PathBuf, 
                   remote_dir: path::PathBuf) 
    -> Result<(), String>
{
    info!("Job: Sending input data");

    // We check if data are already present on the remote end
    let already_there = node.async_exec(format!("cd {}", remote_dir.to_str().unwrap()))
        .await
        .map_err(|e| format!("Failed to check for data: {}", e))?;
    info!("Job: Already there: {:?}", already_there);

    // Depending on the result, we send the archive.
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
pub async fn send_data_to_front(node: RemoteHandle, 
                            archive: path::PathBuf, 
                            remote_dir: path::PathBuf) -> Result<(), String>
{
    info!("Job: Sending input data");

    // We check if data are already present on the remote end
    let already_there = node.async_exec(format!("cd {}", remote_dir.to_str().unwrap()))
        .await
        .map_err(|e| format!("Failed to check for data: {}", e))?;
    info!("Job: Already there: {:?}", already_there);

    // Depending on the result, we send the archive.
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

/// Performs the job on a node: deflates the data and run the script. 
pub async fn perform_job(node: DropBack<Expire<RemoteHandle>>,
                     before_exec: String,
                     after_exec: String,
                     script_name: String,
                     parameters: String,
                     remote_defl: String,
                     remote_send: String,
                     stdout_cb: Box<dyn Fn(Vec<u8>)+Send+'static>,
                     stderr_cb: Box<dyn Fn(Vec<u8>)+Send+'static>)  -> Result<Output, String>
{
        // We deflate data 
        info!("Job: Deflating input data");
        node.async_exec(format!("mkdir {}", remote_defl))
            .await
            .map_err(|e| format!("Failed to make remote dir: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to make remote dir: {}", e)))?;
        node.async_exec(format!("tar -xf {} -C {}", remote_send, remote_defl))
            .await
            .map_err(|e| format!("Failed to deflate input data: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to deflate input data: {}", e)))?;

        // We start the interactive execution
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
pub async fn fetch_data(node: DropBack<Expire<RemoteHandle>>,
                    remote_defl: String,
                    remote_fetch: String,
                    remote_ignore: String,
                    output_path: path::PathBuf) -> Result<(), String>
{
        // We pack the remote data
        info!("Job: Packing output data");
        node.async_exec(format!("cd {} && (tar -cf {} -X {} * || tar -cf {} *)",
                                 remote_defl,
                                 remote_fetch,
                                 remote_ignore,
                                 remote_fetch))
            .await
            .map_err(|e| format!("Failed to pack output data: {}", e))
            .and_then(|e| e.result().map_err(|e| format!("Failed to pack output data: {}", e)))?;
        
        // We fetch the archive
        info!("Job: Fetching output data");
        if !output_path.exists(){
            std::fs::create_dir_all(&output_path).unwrap();
        }
        node.async_scp_fetch(remote_fetch.into(), output_path.join(FETCH_ARCH_RPATH))
            .await
            .map_err(|e| format!("Failed to fetch output data: {}", e))?;
        
        // We deflate the archive locally
        info!("Job: Deflating output data");
        liborchestra::application::unpack_arch(output_path.join(FETCH_ARCH_RPATH))
            .map_err(|e| format!("Failed to unpack output data: {}", e))?;
        Ok(())
}

/// Cleans data on remote hand. 
pub async fn clean_data(node: DropBack<Expire<RemoteHandle>>,
                    remote_dir: String,
                    remote_defl: String,
                    leave_config: LeaveConfig) -> Result<(), String>
{
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
        Ok(())
}