//! runaway-cli/subcommands/sched.rs
//! Author: Alexandre Péré
//! 
//! This module contains the sched subcommand. 


//-------------------------------------------------------------------------------------------IMPORTS


use liborchestra::{
    SEND_ARCH_RPATH, 
    FETCH_ARCH_RPATH};
use liborchestra::hosts::{HostHandle, LeaveConfig};
use clap;
use uuid;
use futures::executor::{block_on};
use futures::task::SpawnExt;
use crate::{to_exit};
use liborchestra::commons::{EnvironmentStore,substitute_environment, push_env, OutputBuf, AsResult,
                            EnvironmentKey, EnvironmentValue, DropBack, Expire, format_env};
use liborchestra::hosts::NodeHandle;
use liborchestra::primitives::{self, Glob, Sha1Hash};
use liborchestra::ssh::RemoteHandle;
use liborchestra::scheduler::SchedulerHandle;
use crate::color;
use crate::misc;
use crate::exit::Exit;
use std::path::{PathBuf, Path};
use std::process::{Command, Stdio};
use std::mem;
use std::convert::TryInto;
use rand::{self, Rng};
use std::io::Write;
use tracing::{self, info, error, debug};
use path_abs::PathAbs;


//--------------------------------------------------------------------------------------- SUBCOMMAND


/// Schedules executions auto;atically
pub fn sched(matches: clap::ArgMatches<'static>) -> Result<Exit, Exit>{

    // We initialize the logger
    misc::init_logger(&matches);

    // We create the store that will keep env vars.
    let mut store = EnvironmentStore::new();

    // We read the local envs to the store
    if !matches.is_present("no-env-read"){
        let envs = misc::read_local_runaway_envs();
        debug!("Local environment variables captured: {}", envs.iter()
            .fold(String::new(), |mut acc, (k, v)| {acc.push_str(&format!("\n{:?}={:?}", k, v)); acc}));
        envs.into_iter()
            .for_each(|(k, v)| {push_env(&mut store, k.0, v.0);});
    }

    // We load the host
    info!("Loading host");
    let host = misc::get_host(matches.value_of("REMOTE").unwrap(), store.clone())?;
    push_env(&mut store, "RUNAWAY_REMOTE", host.get_name());
    debug!("Host {} loaded", host);

    // We setup a few variables that will be used afterward.
    info!("Reading arguments");
    let leave;
    if matches.is_present("on-local"){
        leave = LeaveConfig::Everything;
    } else{
        leave = LeaveConfig::from(matches.value_of("leave").unwrap());
    }
    push_env(&mut store, "RUNAWAY_LEAVE", format!("{}", leave));
    debug!("Leave option set to {}", leave);
    let script = PathBuf::from(matches.value_of("SCRIPT").unwrap());
    push_env(&mut store, "RUNAWAY_SCRIPT_PATH", script.to_str().unwrap());
    debug!("Script path set to {}", script.to_str().unwrap());

    // We generate the iterator over things that will vary from executions to executions
    let remotes_template = matches.value_of("remote-folders").unwrap().to_owned();
    let outputs_template = matches.value_of("output-folders").unwrap().to_owned();

    // We compute some paths
    let local_folder = to_exit!(std::env::current_dir(), Exit::ScriptFolder)?;
    push_env(&mut store, "RUNAWAY_LOCAL_FOLDER", local_folder.to_str().unwrap());
    debug!("Local folder is {}", local_folder.to_str().unwrap());
    let local_send_archive = local_folder.join(SEND_ARCH_RPATH);
    if !script.exists() {
        return Err(Exit::ScriptPath)
    }

    // We generate globs for file sending and fetching
    info!("Reading ignore files");
    let (mut send_ignore_globs, mut fetch_ignore_globs) = misc::get_send_fetch_ignores_globs(
        &local_folder,
        matches.value_of("send-ignore").unwrap(),
        matches.value_of("fetch-ignore").unwrap()
    )?;
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

    // We list the local files to be send
    let files_to_send = to_exit!(primitives::list_local_folder(
            &local_folder, 
            &send_ignore_globs, 
            &send_include_globs),
        Exit::ReadLocalFolder)?;
    debug!("Files to be send to remote: {}", files_to_send.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


    // We create the archive
    info!("Compress files");
    let local_send_hash = to_exit!(primitives::tar_local_files(&local_folder, 
                                                &files_to_send,
                                                &local_send_archive),
                    Exit::PackLocalArchive)?;
    push_env(&mut store, "RUNAWAY_SEND_HASH", format!("{}", local_send_hash));
    debug!("Archive hash is {}", local_send_hash);

    // We set the archive name
    let remote_send_archive = host.get_host_directory().join(format!("{}.tar",local_send_hash));

    // We send the archive to the host using the frontend    
    block_on(send_data_on_front(&host, &remote_send_archive, &local_send_archive, &local_send_hash))?;

    // We start the executor
    let mut executor = to_exit!(futures::executor::ThreadPoolBuilder::new()
                .name_prefix("runaway-worker")
                .create(),
        Exit::SpawnThreadPool)?;

    // We spawn the scheduler command.
    let sched_string = matches.value_of("SCHEDULER").unwrap().to_owned();
    let mut sched_args: Vec<String> = sched_string
        .split(' ')
        .map(ToOwned::to_owned)
        .collect();
    let mut sched_cmd = Command::new(sched_args.remove(0));
    sched_cmd.args(sched_args)
        .envs(&store)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let sched = to_exit!(SchedulerHandle::spawn(sched_cmd, sched_string), Exit::SpawnScheduler)?;

    // We install ctrl-c handler
    misc::install_ctrlc_handler(Some(host.clone()), Some(sched.clone()));

    // We perform the executions
    let mut execs_handle = Vec::new();

    let stopping_exit = loop{

        // We make local copies of variables
        let host = host.clone();
        let remote_send_archive = remote_send_archive.clone();
        let remotes_template = remotes_template.clone();
        let outputs_template = outputs_template.clone();
        let leave = leave.clone();
        let fetch_ignore_globs = fetch_ignore_globs.clone();
        let fetch_include_globs = fetch_include_globs.clone();
        let sched = sched.clone();
        let store = store.clone();
        let matches = matches.clone();

        // This first future captures the arguments and the nodes.
        let arg_and_node_and_store_fut = async {

            // Again, we make local copies.
            let sched = sched.clone();
            let host = host.clone();
            let mut store = store;

            // We generate an uuid
            let id = uuid::Uuid::new_v4().hyphenated().to_string();
            push_env(&mut store, "RUNAWAY_UUID", id.clone());
            debug!("Execution id set to {}", format!("{}", id));

            // We get the arguments
            info!("Querying the scheduler");
            let arguments: String = match sched.async_request_parameters(id.clone()).await{
                Ok(arg) => Ok(arg),
                Err(liborchestra::scheduler::Error::Crashed) => Err(Exit::SchedulerCrashed),
                Err(liborchestra::scheduler::Error::Shutdown) => Err(Exit::SchedulerShutdown),
                Err(e) => to_exit!(Err(e), Exit::RequestParameters)
            }?;
            debug!("Scheduler returned argument {:?}", arguments);

            // We acquire the node
            let node = to_exit!(host.async_acquire().await, Exit::NodeAcquisition)?;

            store.extend(node.context.envs.clone().into_iter());
            debug!("Acquired node with context: \nCwd: {}\nEnvs:\n    {}", 
                node.context.cwd.0.to_str().unwrap(), 
                format_env(&node.context.envs).replace("\n", "\n    ")
            );

            Ok((arguments, node, store, id))
        };

        // We execute this future and breaks if an error was encountered
        let (arguments, node, store, id) = match executor.run(arg_and_node_and_store_fut){
            Ok(a) => a,
            Err(e) => break e
        };

        // We spawn the execution future
        let perform_fut = async move {
            info!("Starting execution with arguments\"{}\"", arguments);
            // We perform the exec
            let (local_fetch_archive, store, remote_fetch_hash, output) = perform_on_node(
                store,
                node,
                &host,
                arguments.as_str(),
                &remote_send_archive,
                &remotes_template,
                &outputs_template,
                &leave,
                &fetch_ignore_globs,
                &fetch_include_globs,
                matches.is_present("on-local"),
            ).await?;
            let ret = unpacks_fetch_post_proc(&matches, local_fetch_archive.clone(), store.clone(), remote_fetch_hash, output.ecode);
            let features = match store.get(&EnvironmentKey("RUNAWAY_FEATURES".into())){
                Some(EnvironmentValue(features)) => features.to_string(),
                None => "".to_owned()
            };
            let path = local_fetch_archive
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            to_exit!(sched.async_record_output(id, arguments, output.stdout, output.stderr, output.ecode, features, path).await,
                Exit::RecordFeatures)?;
            ret
        };

        // We spawn and add the handle
        let handle = to_exit!(executor.spawn_with_handle(perform_fut), Exit::ExecutionSpawnFailed)?;
        execs_handle.push(handle);

    };

    // We wait for the futures
    let exits: Vec<Exit> = executor.run(futures::future::join_all(execs_handle))
        .into_iter()
        .map(|r| match r {
            Ok(e) => e,
            Err(e) => e,
        })
        .collect();

    // Depending on the leave options, we remove the send archive on the remote
    info!("Cleaning data on remote");
    if let LeaveConfig::Nothing = leave{
        let res = executor.run(primitives::remove_remote_files(
            vec!(remote_send_archive), 
            &host.get_frontend())
        );
        to_exit!(res, Exit::Cleanup)?;
    }

    // If exit was triggered by user
    if mem::discriminant(&stopping_exit) == mem::discriminant(&Exit::SchedulerShutdown){
        if exits.iter().all(|e| mem::discriminant(e) == mem::discriminant(&Exit::AllGood)){
            Ok(Exit::AllGood)
        } else {
            error!("Some executions failed.");
            let nb = exits.iter()
                .filter(|e| mem::discriminant(*e) != mem::discriminant(&Exit::AllGood))
                .count();
            Ok(Exit::SomeExecutionFailed(nb.try_into().unwrap()))
        }
    } else {
        Err(stopping_exit)
    }
}


//------------------------------------------------------------------------------------------ HELPERS


// Returns the absolute path if it is not absolute
fn absolutize(p: PathBuf) -> PathBuf{
    if p.is_absolute(){
        p
    } else{
        std::env::current_dir().unwrap().join(p)
    }
}

// This type allows to return an iterator that owns a piece of data. I don't know how to write the 
// next function without that, as the boxed iterator would reference to content read from the file 
// that is owned by the function.
struct OwnedVecIter<S>(Vec<S>);
impl<S> Iterator for OwnedVecIter<S>{
    type Item = S;
    fn next(&mut self) -> Option<Self::Item>{
       self.0.pop() 
    }
}

// Sends data to the remote using the frontend.
async fn send_data_on_front(host: &HostHandle, 
                            remote_send_archive: &PathBuf,
                            local_send_archive: &PathBuf,
                            local_send_hash: &primitives::Sha1Hash) -> Result<(), Exit>{
        info!("Transferring data");
        let node = host.get_frontend();
        let remote_send_exists = to_exit!(primitives::remote_file_exists(&remote_send_archive, &node).await,
                                          Exit::CheckPresence)?;
        if !remote_send_exists{
            debug!("Archive does not exist on host. Sending data...");
            to_exit!(primitives::send_local_file(&local_send_archive, &remote_send_archive, &node).await,
                     Exit::SendArchive)?;
            let remote_send_hash = to_exit!(primitives::compute_remote_sha1(&remote_send_archive, &node).await,
                                            Exit::ComputeRemoteHash)?;
            if &remote_send_hash != local_send_hash{
                error!("Differing local and remote hashs for send archive: local is {} and \\
                           remote is {}", local_send_hash, remote_send_hash); 
                return Err(Exit::Send)
            }
        }
        to_exit!(std::fs::remove_file(local_send_archive),
                 Exit::RemoveArchive)?;
        Ok(())
}

// Unpacks archive on node
async fn unpacks_send_on_node(remote_folder: &PathBuf, 
                              remote_send_archive: &PathBuf,
                              node: &RemoteHandle) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Exit>{
    if remote_send_archive.is_relative(){return Err(Exit::WrongRemoteFolderString)}
    let remote_folder_exists = to_exit!(primitives::remote_folder_exists(&remote_folder, &node).await,
                                        Exit::CheckRemotePresence)?;
    let remote_files_before;
    if remote_folder_exists{
        debug!("Remote folder already exist. Listing files.");
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
    debug!("Extracting data in remote folder");
    let remote_files = to_exit!(primitives::untar_remote_archive(&remote_send_archive,
                                                                 &remote_folder,
                                                                 &node).await,
                                Exit::UnpackRemoteArchive)?;
    debug!("Files extracted from remote: {}", remote_files.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));

    Ok((remote_files_before, remote_files))
}


// Performs all actions that need access to the node: Deflate, run and send back.
async fn perform_on_node(store: EnvironmentStore,
                         node: DropBack<Expire<NodeHandle>>,
                         host: &HostHandle,
                         arguments: &str,
                         remote_send_archive: &PathBuf,
                         remote_folder_pattern: &str,
                         output_folder_pattern: &str,
                         leave: &LeaveConfig,
                         fetch_ignore_globs: &Vec<Glob<String>>,
                         fetch_include_globs: &Vec<Glob<String>>,
                         on_local: bool
                         ) -> Result<(PathBuf, EnvironmentStore, Sha1Hash, OutputBuf), Exit>{


    let mut store = store;
    push_env(&mut store, "RUNAWAY_ARGUMENTS", arguments);


    // We generate the remote folder and unpack data into it
    let remote_folder;
    if on_local{
        let remote_path = PathBuf::from(substitute_environment(&store, output_folder_pattern));
        remote_folder = absolutize(remote_path);
    } else {
        remote_folder = PathBuf::from(substitute_environment(&store, remote_folder_pattern));
    }
    push_env(&mut store, "RUNAWAY_PWD", remote_folder.to_str().unwrap());
    let (remote_files_before, _) = unpacks_send_on_node(
        &remote_folder, 
        &remote_send_archive, 
        &node
    ).await?;

    // We retrieve the id 
    let EnvironmentValue(id) = store.get(&EnvironmentKey("RUNAWAY_UUID".into())).unwrap().to_owned();

    // We perform the job
    debug!("Executing script");
    let color: u8 = rand::thread_rng().gen();
    let stdout_id = id.clone();
    let stdout_callback = Box::new(move |a|{
        let string = String::from_utf8(a).unwrap().replace("\r\n", "");
        print!("{}", color!(color, "{}: {}", stdout_id, string));
        std::io::stdout().flush().unwrap();
    });
    let stderr_id = id.clone();
    let stderr_callback = Box::new(move |a|{
        let string = String::from_utf8(a).unwrap().replace("\r\n", "");
        eprint!("{}", color!(color, "{}: {}", stderr_id, string));
        std::io::stderr().flush().unwrap();
    });
    let mut context = node.context.clone();
    context.envs.extend(store.into_iter());
    let (mut execution_context, outs) = to_exit!(node.async_pty(
            context,
            host.get_execution_procedure(),
            Some(stdout_callback), 
            Some(stderr_callback)).await,
        Exit::Execute)?;
    let out: OutputBuf = liborchestra::misc::compact_outputs(outs).into();
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
    debug!("Compressing data to be fetched");
    let remote_fetch_archive = remote_folder.join(".to_fetch.tar");
    let remote_fetch_hash = to_exit!(primitives::tar_remote_files(&remote_folder,
                                                                 &files_to_fetch,
                                                                 &remote_fetch_archive,
                                                                 &node).await,
                                     Exit::PackRemoteArchive)?;
    debug!("Archive hash is {}", remote_fetch_hash);


    // We generate output folder
    let local_output_folder;
    if on_local{
        local_output_folder = remote_folder.clone();
    } else {
        let local_output_string = substitute_environment(&execution_context.envs, output_folder_pattern);
        let abs_local_output_folder = to_exit!(PathAbs::new(local_output_string), Exit::OutputFolder)?;
        let abs_local_output_folder: &PathBuf = abs_local_output_folder.as_ref();
        local_output_folder = abs_local_output_folder.to_owned();
    }     

    debug!("Local output folder set to: {}", local_output_folder.to_str().unwrap());
    if !local_output_folder.exists(){
        debug!("Creating output folder");
        to_exit!(std::fs::create_dir_all(&local_output_folder), Exit::OutputFolder)?;
    }
    push_env(&mut execution_context.envs, "RUNAWAY_OUTPUT_FOLDER", local_output_folder.to_str().unwrap());
    let local_fetch_archive = local_output_folder.join(FETCH_ARCH_RPATH);

    
    // We fetch data back in  
    debug!("Transferring data");
    to_exit!(primitives::fetch_remote_file(&remote_fetch_archive,
                                           &local_fetch_archive,
                                           &node).await,
             Exit::Fetch)?;
    to_exit!(primitives::remove_remote_files(vec!(remote_fetch_archive), &node).await,
             Exit::RemoveArchive)?;


    // Depending on the leave config, we clean the remote execution folder
    debug!("Cleaning data on remote");
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


    // We return needed informations
    Ok((local_fetch_archive, execution_context.envs, remote_fetch_hash, out))

}


// Finalize execution on local 
fn unpacks_fetch_post_proc(matches: &clap::ArgMatches<'_>,
                           local_fetch_archive: PathBuf, 
                           store: EnvironmentStore, 
                           remote_fetch_hash: Sha1Hash,
                           execution_ecode: i32) -> Result<Exit, Exit> {


    // We compute the local hash
    let local_fetch_hash = to_exit!(primitives::compute_local_sha1(&local_fetch_archive),
                                    Exit::ComputeLocalHash)?;
    if remote_fetch_hash != local_fetch_hash{
        error!("Differing local and remote hashs for fetch archive: local is {} and \\
                   remote is {}", local_fetch_hash, remote_fetch_hash);
        return Err(Exit::Fetch)
    }
    

    // We unpack the data
    debug!("Extracting archive");
    let local_files = to_exit!(primitives::untar_local_archive(
            &local_fetch_archive, 
            &local_fetch_archive.parent().unwrap().to_path_buf()),
        Exit::UnpackRemoteArchive)?;
    to_exit!(std::fs::remove_file(local_fetch_archive), Exit::RemoveArchive)?;
    debug!("Files fetched from remote: {}", local_files.iter()
            .fold(String::new(), |mut acc, s| {acc.push_str(&format!("\n{}", s.to_str().unwrap())); acc}));


    // We execute the post processing
    debug!("Executing post script");
    let command_string = if matches.is_present("post-script"){
        let path_str = PathBuf::from(matches.value_of("post-script").unwrap())
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        format!("bash {}", path_str)
    } else {
        matches.value_of("post-command").unwrap().to_owned()
    };
    let post_proc_out = Command::new("bash")
        .arg("-c")
        .arg(command_string)
        .envs(store)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    to_exit!(post_proc_out.result(), Exit::PostProcFailed)?;

    if execution_ecode == 0{
        Ok(Exit::AllGood)
    } else {
        Ok(Exit::ScriptFailedWithCode(execution_ecode))
    }

}