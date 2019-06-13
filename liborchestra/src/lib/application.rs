// liborchestra/application.rs
// Author: Alexandre Péré

/// This module contains a shared asynchronous resource, meant to be used to spawn jobs and handle 
/// post-processing of jobs. It holds an inner threadpool that is used to drive the jobs to 
/// completion.

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use crate::hosts;
use crate::repository;
use crate::repository::{ExecutionState, ExecutionUpdateBuilder};
//use crate::schedulers;
use crate::{
    FEATURES_RPATH, FETCH_ARCH_RPATH, FETCH_IGNORE_RPATH, SEND_ARCH_RPATH, SEND_IGNORE_RPATH,
};
use crate::primitives::Dropper;
use chrono::Utc;
use fasthash;
use fasthash::StreamHasher;
use ignore;
use std::error;
use std::fmt;
use std::fs;
use std::collections::HashMap;
use std::hash::Hasher;
use std::io::prelude::*;
use std::path;
use std::process::Output;
use std::os::unix::process::ExitStatusExt; 
use tar;
use tar::Archive;
use uuid::Uuid;
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future::Future;
use futures::prelude::*;
use futures::task::LocalSpawnExt;
use futures::task::SpawnExt;
use std::sync::{Arc, Mutex};

////////////////////////////////////////////////////////////////////////////////////////////// ERROR
#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    PerformExecFailed(String),
    PostProcessExecFailed(String),
    ExecutionFailed(String),
    UnknownHost(String),
    OperationFetch(String),
    Channel(String),
    AcquireLock(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Error::*;
        match self {
            PerformExecFailed(ref s) => {
                write!(f, "Execution failed: \n{}", s)
            },
            PostProcessExecFailed(ref s) => {
                write!(f, "Post processing of execution failed: \n{}", s)
            }
            ExecutionFailed(ref s) =>{
                write!(f, "Remote command failed: \n{}", s)
            }
            UnknownHost(ref s) => { write!(f, "Unknown host: \n{}", s) }
            OperationFetch(ref s) => { write!(f, "Failed to fetch operation: \n{}", s) }
            Channel(ref s)  => {write!(f, "Failed to send operation to channel: \n{}", s)}
            AcquireLock(ref s) => {write!(f, "Failed to acquire lock: \n{}", s)}
        }
    }
}

////////////////////////////////////////////////////////////////////////////////////////////// TRAIT

/// A trait allowing to turn a structure into a result.
trait AsResult {
    /// Consumes the object to make a result out of it.
    fn result(self) -> Result<String, Error>;
}

/// Implementation for `Ouput`. The exit code is used as a marker for failure. 
impl AsResult for Output {
    fn result(self) -> Result<String, Error> {
        if self.status.success() {
            Ok(format!(
                "Successful execution\nstdout: {}\nstderr: {}",
                String::from_utf8(self.stdout).unwrap(),
                String::from_utf8(self.stderr).unwrap()
            ))
        } else {
            Err(Error::ExecutionFailed(format!(
                "Failed execution({})\nstdout: {}\nstderr: {}",
                self.status.code().unwrap_or(911),
                String::from_utf8(self.stdout).unwrap(),
                String::from_utf8(self.stderr).unwrap()
            )))
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////// APPLICATION

/// The inner application resource. It holds a list of the known hosts, and a handle to the 
/// repository. Also, it contains an threadpool executor on which the jobs will be spawned. It is 
/// important to understand that, though the application will be accessed by an asynchronous handle,
/// the methods will not await for the whole execution of the job, but only for its submission. For 
/// this reason, as soon as the job was submitted, there will be no way to keep rtack of it. In 
/// practice, a handle to the application will be given to the job, which will allow the executor to 
/// call for a post-processing on the execution. This allows to sort of gain back the control on the
/// execution once it was executed.
struct Application {
    hosts: HashMap<String, hosts::HostHandle>,
    repo: repository::CampaignHandle,
    reschedule: bool,
    jobs_executor: executor::ThreadPool,
}

impl Application {
    /// Asynchronous function that submits an execution to the inner executor. 
    async fn submit_exec(app: Arc<Mutex<Application>>,
                         commit: repository::ExperimentCommit,
                         parameters: repository::ExecutionParameters,
                         tags: Vec<repository::ExecutionTag>,
                         host: String,
                         application_handle: ApplicationHandle) 
                         -> Result<(), Error> {
        debug!("Application: Submitting execution to host {} at commit {} with parameters {}", 
            host, commit.0, parameters.0);
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        let host = app.hosts
            .get(&host)
            .ok_or(Error::UnknownHost(format!("Unknown host {}", host)))?;
        let fut = perform_new_job(host.to_owned(),
                                  app.repo.clone(),
                                  application_handle,
                                  commit,
                                  parameters,
                                  tags);
        trace!("Application: Spawning job in executor");
        app.jobs_executor
            .spawn(fut)
            .map_err(|e| Error::PerformExecFailed(format!("Failed to spawn future: {:?}", e)))?;
        Ok(())        
    }

    /// Asynchronous function that submits post-processing of an execution. 
    async fn post_process_exec(app: Arc<Mutex<Application>>, 
                               exec: repository::ExecutionConf, 
                               app_handle: ApplicationHandle) -> Result<(), Error>{
                                      use repository::ExecutionState::*;
        debug!("Application: Post processing execution {:?}", exec.identifier);
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        match (exec.state, app.reschedule){
            (Completed, _) => {
                trace!("Application: Execution found completed. Ending...");
                Ok(())},
            (_, true) => {
                info!("Application: Execution found in uncompleted state. Rescheduling...");
                let host = app.hosts
                    .get(exec.executor.as_ref().ok_or(Error::UnknownHost(format!("No host provided")))?)
                    .ok_or(Error::UnknownHost(format!("Unknown host {:?}", exec.executor)))?;  
                let fut = reschedule_job(host.to_owned(),
                                         app.repo.clone(),
                                         app_handle,
                                         exec.identifier,
                                         exec.commit,
                                         exec.parameters,
                                         exec.tags);
                app.jobs_executor
                    .spawn(fut)
                    .map_err(|e| Error::PerformExecFailed(format!("Failed to spawn future: {:?}", e)))?;
                Ok(())                
            }
            (_, false) =>{Ok(())}
        }
    }

    /// Asynchronous function that registers a host.
    async fn register_host(app: Arc<Mutex<Application>>, 
                           host: hosts::HostHandle) 
                           -> Result<(), Error> {
        debug!("Application: Registering host {:?}", host);
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        app.hosts.insert(host.get_name(), host);
        Ok(())
    }

    /// Asynchronous function that unregisters a host.
    async fn unregister_host(app: Arc<Mutex<Application>>, host: String) -> Result<(), Error>{
        debug!("Application: Unregistering host {}", host);
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        app.hosts.remove(&host);
        Ok(())
    }

    /// Asynchronous function that activates rescheduling.
    async fn activate_rescheduling(app: Arc<Mutex<Application>>) -> Result<(), Error>{
        debug!("Application: Activating rescheduling");
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        app.reschedule = true;
        Ok(())
    }

    /// Asynchronous function that deactivates rescheduling.
    async fn deactivate_rescheduling(app: Arc<Mutex<Application>>) -> Result<(), Error>{
        debug!("Application: Deactivate rescheduling");
        let mut app = app.lock()
            .map_err(|e| Error::AcquireLock(e.to_string()))?;
        app.reschedule = false;
        Ok(())
    }
}

/// The operation inputs, sent by the outer futures and processed by inner thread.
#[derive(Debug)]
enum OperationInput {
    SubmitExec(repository::ExperimentCommit, 
               repository::ExecutionParameters, 
               Vec<repository::ExecutionTag>, 
               String, 
               ApplicationHandle),
    PostProcessExec(repository::ExecutionConf,
                    ApplicationHandle),
    RegisterHost(hosts::HostHandle),
    UnregisterHost(String),
    ActivateRescheduling,
    DeactivateRescheduling,
}

/// The operation outputs, sent by the inner futures and processed by the outer thread.
#[derive(Debug)]
enum OperationOutput {
    SubmitExec(Result<(), Error>),
    PostProcessExec(Result<(), Error>),
    RegisterHost(Result<(), Error>),
    UnregisterHost(Result<(), Error>),
    ActivateRescheduling(Result<(), Error>),
    DeactivateRescheduling(Result<(), Error>),
}

/// A handle to an inner application. Offer a future interface to the inner application.
#[derive(Clone)]
pub struct ApplicationHandle {
    _sender: mpsc::UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    _dropper: Option<Dropper<()>>,
    // We use an option that will set to None when post-processing, to avoid having strong refs 
    // which would bug the dropper. It is definately hacky and should be changed. (TODO)
}

impl fmt::Debug for ApplicationHandle{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        write!(f, "ApplicationHandle")
    }
}

impl ApplicationHandle {
    
    /// Spawns the application and returns a handle to it. 
    pub fn spawn(campaign_handle: repository::CampaignHandle) -> Result<ApplicationHandle, Error> {
        debug!("ApplicationHandle: Start application thread.");
        let (sender, receiver) = mpsc::unbounded();
        let handle = std::thread::Builder::new().name("application".to_owned()).spawn(move || {
            trace!("Application Thread: Creating resource in thread");
            let application = Arc::new(Mutex::new(Application {
                repo: campaign_handle,
                hosts: HashMap::new(),
                reschedule: false,
                jobs_executor: executor::ThreadPoolBuilder::new().name_prefix("application-pool")
                    .create().unwrap(),
            }));
            trace!("Application Thread: Starting handling stream");
            let mut pool = executor::LocalPool::new();
            let mut spawner = pool.spawner();
            let handling_stream = receiver.for_each(
                move |(sender, operation): (oneshot::Sender<OperationOutput>, OperationInput)| {
                    trace!("Application Thread: received operation {:?}", operation);
                    match operation {
                        OperationInput::SubmitExec(commit, repo, tags, host, app) => {
                            spawner.spawn_local(
                                Application::submit_exec(application.clone(), commit, repo, tags, host, app)
                                    .map(|a| {
                                        sender.send(OperationOutput::SubmitExec(a))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )
                        }
                        OperationInput::PostProcessExec(exec, app) =>{
                            spawner.spawn_local(
                                Application::post_process_exec(application.clone(), exec, app)
                                    .map(|a|{
                                        sender.send(OperationOutput::PostProcessExec(a))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )
                        }
                        OperationInput::RegisterHost(host) =>{
                            spawner.spawn_local(
                                Application::register_host(application.clone(), host)
                                    .map(|r|{
                                        sender.send(OperationOutput::RegisterHost(r))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )                        
                        }
                        OperationInput::UnregisterHost(host) =>{
                            spawner.spawn_local(
                                Application::unregister_host(application.clone(), host)
                                    .map(|r|{
                                        sender.send(OperationOutput::UnregisterHost(r))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )                        
                        }
                        OperationInput::ActivateRescheduling =>{
                            spawner.spawn_local(
                                Application::activate_rescheduling(application.clone())
                                    .map(|r|{
                                        sender.send(OperationOutput::ActivateRescheduling(r))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )                        
                        }
                        OperationInput::DeactivateRescheduling =>{
                            spawner.spawn_local(
                                Application::deactivate_rescheduling(application.clone())
                                    .map(|r|{
                                        sender.send(OperationOutput::DeactivateRescheduling(r))
                                            .map_err(|e| error!("Application Thread: Failed to \\
                                            send an operation output: \n{:?}", e));
                                    })
                            )                        
                        }
                        _ => unimplemented!()
                    }.map_err(|e| error!("Application Thread: Failed to spawn the operation: \n{:?}", e));
                    future::ready(())
                }
            );
            let mut spawner = pool.spawner();
            spawner.spawn_local(handling_stream)
                .map_err(|_| error!("Application Thread: Failed to spawn handling stream"));
            trace!("Application Thread: Starting local executor.");
            pool.run();
            trace!("Application Thread: All futures executed. Leaving...");
        }).expect("Failed to spawn application thread.");
        Ok(ApplicationHandle {
            _sender: sender,
            _dropper: Some(Dropper::from_handle(handle)),
        })
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// execution was submitted.
    pub fn async_submit_exec(&self,
                             commit: repository::ExperimentCommit,
                             parameters: repository::ExecutionParameters,
                             tags: Vec<repository::ExecutionTag>,
                             host: String) 
                             -> impl Future<Output=Result<(), Error>> {
        debug!("ApplicationHandle: Building async_submit_exec future to host {} at commit {} with parameters {}", 
            host, commit.0, parameters.0);
        let mut chan = self._sender.clone();
        let mut app = (*self).clone();
        app._dropper = None;
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle::async_submit_exec_future: Sending submit exec input");
            chan.send((sender, OperationInput::SubmitExec(commit, parameters, tags, host, app)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle::async_submit_exec_future: Awaiting submit exec output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::SubmitExec(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected SubmitExec, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// post processing of an execution was submitted.
    pub fn async_post_process_exec(&self, 
                                   exec: repository::ExecutionConf)
                                   -> impl Future<Output=Result<(), Error>>{
        debug!("ApplicationHandle: Post-Processing execution {}", exec);
        let mut chan = self._sender.clone();
        let mut app = (*self).clone();
        app._dropper = None;
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle: Sending post-process input");
            chan.send((sender, OperationInput::PostProcessExec(exec, app)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle: Awaiting post-process output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::PostProcessExec(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected PostProcessExec, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// host was registered.
    pub fn async_register_host(&self, 
                               host: hosts::HostHandle)
                               -> impl Future<Output=Result<(), Error>>{
        debug!("ApplicationHandle: Registering host {}", host);
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle: Sending register host input");
            chan.send((sender, OperationInput::RegisterHost(host)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle: Awaiting register host output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::RegisterHost(r)) => r,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected RegisterHost, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// host was unregistered.
    pub fn async_unregister_host(&self, 
                                 host: String)
                                 -> impl Future<Output=Result<(), Error>>{
        debug!("ApplicationHandle: Unregistering host {}", host);
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle: Sending unregister host input");
            chan.send((sender, OperationInput::UnregisterHost(host)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle: Awaiting unregister host output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::UnregisterHost(r)) => r,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected UnregisterHost, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// rescheduling was activated.
    pub fn async_activate_rescheduling(&self) -> impl Future<Output=Result<(), Error>>{
        debug!("ApplicationHandle: Activating rescheduling");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle: Sending activate rescheduling input");
            chan.send((sender, OperationInput::ActivateRescheduling))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle: Awaiting activate rescheduling output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::ActivateRescheduling(r)) => r,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected ActivateRescheduling, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// rescheduling was deactivated.
    pub fn async_deactivate_rescheduling(&self) -> impl Future<Output=Result<(), Error>>{
        debug!("ApplicationHandle: Deactivating rescheduling");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("ApplicationHandle: Sending deactivate rescheduling input");
            chan.send((sender, OperationInput::DeactivateRescheduling))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("ApplicationHandle: Awaiting deactivate rescheduling output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::DeactivateRescheduling(r)) => r,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected DeactivateRescheduling, found {:?}", e)))
            }
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////// JOB

/// This asynchronous function contains the whole logic of a job execution. An instantiation of this 
/// async function is refered to as a job.
async fn perform_new_job(host: hosts::HostHandle,
                         repo: repository::CampaignHandle,
                         app: ApplicationHandle,
                         commit: repository::ExperimentCommit,
                         parameters: repository::ExecutionParameters,
                         tags: Vec<repository::ExecutionTag>){
        debug!("Job: Performing a new job!");
        trace!("Job: Creating execution on {} at commit {} with parameters {}",
            host,
            commit.0,
            parameters.0
        );
        let tags_ref = tags.iter().collect::<Vec<&repository::ExecutionTag>>();
        let mut execution = match repo.async_create_execution(&commit, &parameters, tags_ref).await {
            Ok(e) => e,
            Err(e) => {
                error!("Job: Failed to create execution: \n{}", e);
                return;
            }
        };

        let output: Result<(), Error> = try{
            trace!("Job: Writing executor name");
            execution = repo.async_update_execution(
                &execution.identifier,
                &ExecutionUpdateBuilder::new()
                        .executor(host.get_name())
                        .build())
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to update exec: {}", e)))?;

            trace!("Job: Packing folder");
            let hash = pack_folder(execution.get_path())?;
    
            trace!("Job: Acquiring node");
            let node = host.async_acquire()
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to acquire node: {}", e)))?;
    
            trace!("Job: Sending data");
            let remote_dir = host.get_host_directory().join(hash.to_string());
            let remote_send = remote_dir.join(SEND_ARCH_RPATH);
            let remote_defl = remote_dir.join(execution.identifier.0.to_string());
            let remote_ignore = remote_defl.join(FETCH_IGNORE_RPATH);
            let remote_fetch = remote_defl.join(FETCH_ARCH_RPATH);
            let already_there = node.async_exec(&format!("ls {}", remote_dir.to_str().unwrap()))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to check for data: {}", e)))?;
            if !already_there.status.success() {
                node.async_exec(&format!("mkdir {}", remote_dir.to_str().unwrap()))
                    .await
                    .map_err(|e| Error::PerformExecFailed(format!("Failed to make remote dir: {}", e)))
                    .and_then(|e| e.result())?;
                node.async_scp_send(&execution.get_path().join(SEND_ARCH_RPATH), &remote_send)
                    .await
                    .map_err(|e| Error::PerformExecFailed(format!("Failed to send data: {}", e)))?;
            }
            node.async_exec(&format!("mkdir {}", remote_defl.to_str().unwrap()))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to make remote dir: {}", e)))
                .and_then(|e| e.result())?;
            node.async_exec(&format!("tar -xf {} -C {}",
                                     remote_send.to_str().unwrap(),
                                     remote_defl.to_str().unwrap()))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to deflate data: {}", e)))
                .and_then(|e| e.result())?;
    
            trace!("Job: Starting execution");
            execution = repo.async_update_execution(
                &execution.identifier,
                &ExecutionUpdateBuilder::new()
                        .beginning_date(Utc::now())
                        .state(ExecutionState::Running)
                        .build())
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to update exec: {}", e)))?;
            let output = node.async_exec(&format!("cd {} && ./run {}",
                                                  remote_defl.to_str().unwrap(),
                                                  parameters.0))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to execute: {}", e)))?;
            execution = repo.async_update_execution(
                &execution.identifier,
                &ExecutionUpdateBuilder::new()
                        .ending_date(Utc::now())
                        .stdout(String::from_utf8(output.stdout).unwrap())
                        .stderr(String::from_utf8(output.stderr).unwrap())
                        .exit_code(output.status.code().unwrap_or(output.status.signal().unwrap_or(911)))
                        .build())
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to update exec: {}", e)))?;
    
            trace!("Job: Fetching data");
            node.async_exec(&format!("cd {} && (tar -cf {} -X {} * || tar -cf {} *)",
                                     remote_defl.to_str().unwrap(),
                                     remote_fetch.to_str().unwrap(),
                                     remote_ignore.to_str().unwrap(),
                                     remote_fetch.to_str().unwrap()))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to pack remote files: {}", e)))
                .and_then(|e| e.result())?;
            node.async_scp_fetch(&remote_fetch, &execution.get_path().join(FETCH_ARCH_RPATH))
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to fetch data: {}", e)))?;
            unpack_arch(execution.get_path().join(FETCH_ARCH_RPATH))?;
    
            trace!("Job: Trying to get the features");
            if execution.get_path().join(FEATURES_RPATH).exists() {
                trace!("Job: Features found");
                let mut file = fs::File::open(execution.get_path().join(FEATURES_RPATH)).unwrap();
                let mut contents = String::new();
                file.read_to_string(&mut contents)
                    .map_err(|e| Error::PerformExecFailed(format!("Failed to read features: {}", e)))?;
                let parsed = contents
                    .trim_start_matches('\n')
                    .trim_end_matches('\n')
                    .lines()
                    .map(|e| e
                        .parse::<f64>()
                        .map_err(|e| Error::ExecutionFailed(e.to_string())))
                    .collect::<Result<Vec<f64>, Error>>()?;
                execution = repo.async_update_execution(
                    &execution.identifier,
                    &ExecutionUpdateBuilder::new().features(parsed).build())
                    .await
                    .map_err(|e| Error::PerformExecFailed(format!("Failed to update exec: {}", e)))?;
            } else {
                trace!("Job: Features not found...")
            }
    
            trace!("Job: Finishing execution");
            execution = repo.async_finish_execution(&execution.identifier)
                .await
                .map_err(|e| Error::PerformExecFailed(format!("Failed to finish exec: {}", e)))?;
        };

        trace!("Job: Updating status depending on job completion");
        execution = match output {
            Ok(()) => {
                info!("Job: Execution completed succesfully. Leaving now...");
                execution
            },
            Err(e) => {
                warn!("Job: Execution failed. Updating status accordingly.");
                let update = repository::ExecutionUpdateBuilder::new()
                    .state(repository::ExecutionState::Failed)
                    .message(format!("Failed to execute the job: {}", e))
                    .build();
                repo.async_update_execution(&execution.identifier, &update)
                    .await
                    .map_err(|e| error!("Job: Failed to update execution on job failure: {}", e))
                    .unwrap_or(execution)
            }
        };

        trace!("Job: Calling application post-processing");
        app.async_post_process_exec(execution)
            .await
            .map_err(|e| error!("Job: Failed to post-process execution: {}", e));        
}

/// This asynchronous function contains the logic of code rescheduling.
async fn reschedule_job(host: hosts::HostHandle,
                        repo: repository::CampaignHandle,
                        app: ApplicationHandle,
                        remove_id: repository::ExecutionId,
                        commit: repository::ExperimentCommit,
                        parameters: repository::ExecutionParameters,
                        tags: Vec<repository::ExecutionTag>
                        ){
    debug!("Rescheduling: Rescheduling job {}", remove_id);
    trace!("Rescheduling: Deleting execution");
    if let Err(e) = repo.async_delete_execution(&remove_id).await {
        error!("Rescheduling: Failed to delete execution: \n{}", e);
        return;
    }
    
    trace!("Rescheduling: Starting new job");
    perform_new_job(host, repo, app, commit, parameters, tags).await;
}


/// This function allows to pack an experiment folder into a tar archive, and returns its hash.
fn pack_folder(folder_path: path::PathBuf) -> Result<u64, Error> {
    // We create the file iterator
    let mut walker = ignore::WalkBuilder::new(&folder_path);
    walker
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .follow_links(false)
        .hidden(false);
    if let Some(e) = walker.add_ignore(folder_path.join(SEND_IGNORE_RPATH)) {
        warn!("Pack folder: Failed to add ignore file: {}", e);
    }
    let walk = walker.build();
    // We append file
    let archive_file = fs::File::create(folder_path.join(SEND_ARCH_RPATH)).unwrap();
    let mut archive = tar::Builder::new(archive_file);
    walk.filter(|e| e.is_ok())
        .map(|e| e.unwrap().into_path())
        .filter(|p| p.is_file())
        .filter(|p| p.file_name().unwrap() != SEND_ARCH_RPATH)
        .inspect(|p| trace!("Pack Folder: found file {:?}", p))
        .map(|p| {
            archive
                .append_file(
                    &p.strip_prefix(&folder_path).unwrap(),
                    &mut fs::File::open(&p).unwrap(),
                )
                .map_err(|e| Error::ExecutionFailed(format!("Failed to add file to the archive: {}", e)))
        })
        .collect::<Result<Vec<()>, Error>>()?;
    archive
        .finish()
        .map_err(|e| Error::ExecutionFailed(format!("Failed to finish the archive: {}", e)))?;
    // We compute the hash of the archive
    let mut file = fs::File::open(folder_path.join(SEND_ARCH_RPATH)).unwrap();
    let mut hasher = fasthash::xx::Hasher64::default();
    hasher.write_stream(&mut file);
    Ok(hasher.finish())
}

/// This function allows to unpack a tar archive into an experiment folder.
fn unpack_arch(arch_path: path::PathBuf) -> Result<(), Error> {
    let mut archive = Archive::new(fs::File::open(&arch_path).unwrap());
    archive.unpack(arch_path.parent().unwrap()).unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::prelude::*;
    use std::process;
    use std::time::Duration;
    use url::Url;
    use shells::wrap_sh;
    use futures::executor::block_on;

    fn init_logger() {
        std::env::set_var("RUST_LOG", "liborchestra::application=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    fn setup_folder(){
        fs::create_dir_all("/tmp/pack_folder/folder").unwrap();
        fs::create_dir_all("/tmp/pack_folder/folder/subfolder").unwrap();
        dbg!(wrap_sh!("dd if=/dev/urandom of=/tmp/pack_folder/1 bs=35M count=1")).unwrap();
        dbg!(wrap_sh!("dd if=/dev/urandom of=/tmp/pack_folder/1.txt bs=35M count=1")).unwrap();
        dbg!(wrap_sh!("dd if=/dev/urandom of=/tmp/pack_folder/folder/1 bs=35M count=1")).unwrap();
        dbg!(wrap_sh!("dd if=/dev/urandom of=/tmp/pack_folder/folder/subfolder/1 bs=35M count=1")).unwrap();
        dbg!(wrap_sh!("dd if=/dev/urandom of=/tmp/pack_folder/folder/subfolder/1.txt bs=35M count=1")).unwrap();
        let mut file = fs::File::create("/tmp/pack_folder/.sendignore").unwrap();
        file.write_all(b"*.txt\n**/folder/subfolder/*\n!**/folder/subfolder/1.txt")
            .unwrap();
        std::thread::sleep(Duration::new(1, 000));
    }

    fn setup_expe_repo() {
        fs::create_dir_all("/tmp/expe_repo").unwrap();
        let mut file = fs::File::create("/tmp/expe_repo/run").unwrap();
        file.write_all(
b"#!/usr/bin/env python
import os
import time
time.sleep(2)
print(\"Bonjour guy\")
open(\".features\", \"a\").close()
os.utime(\".features\", None)").unwrap();
        dbg!(wrap_sh!("chmod +x /tmp/expe_repo/run")).unwrap();
        dbg!(wrap_sh!("cd /tmp/expe_repo && git init")).unwrap();
        dbg!(wrap_sh!("cd /tmp/expe_repo && git add run")).unwrap();
        dbg!(wrap_sh!("cd /tmp/expe_repo && git commit -m First")).unwrap();
        let mut server = process::Command::new("git")
            .args(&["daemon", "--reuseaddr", "--base-path=/tmp", "--export-all"])
            .spawn()
            .expect("Failed to start git server");
        std::thread::sleep_ms(1000);
        server.try_wait().unwrap();
    }

    fn setup_ssh_server() -> std::process::Child{
        dbg!(wrap_sh!("echo | ssh-keygen -t rsa -f /tmp/key"));
        process::Command::new("/usr/bin/sshd")
            .args(&["-h", "/tmp/key", "-D", "-p", "55555"])
            .spawn()
            .unwrap()
    }

    fn get_expe_repo_head() -> String {
        return format!(
            "{}",
            git2::Repository::open("/tmp/expe_repo")
                .unwrap()
                .head()
                .unwrap()
                .target()
                .unwrap()
        );
    }

    fn clean_expe_repo() {
        dbg!(wrap_sh!("killall git-daemon"));
        fs::remove_dir_all("/tmp/expe_repo");
    }

    fn clean_cmp_repo() {
        fs::remove_dir_all("/tmp/cmp_repo");
    }

    #[test]
    fn test_perform_new_execution() {

        init_logger();
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        let mut sshd = setup_ssh_server();

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = crate::repository::Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
            .unwrap();
        let repo = crate::repository::CampaignHandle::spawn(repo.conf).unwrap();

        let conf = crate::hosts::HostConf {
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:55555".to_owned(),
            start_alloc: "".to_owned(),
            get_alloc_nodes: "echo localhost".to_owned(),
            cancel_alloc: "".to_owned(),
            alloc_duration: 5,
            executions_per_nodes: 2,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/home/apere/Executions"),
        };
        let host = crate::hosts::HostHandle::spawn_resource(conf).unwrap();
        let commit = get_expe_repo_head();

        let app = ApplicationHandle::spawn(repo.clone()).unwrap();

        block_on(app.async_register_host(host.clone())).unwrap();

        block_on(app.async_activate_rescheduling()).unwrap();

        block_on(app.async_submit_exec(
            crate::repository::ExperimentCommit(commit.clone()),
            crate::repository::ExecutionParameters("".to_owned()), 
            Vec::new(), 
            "localhost".to_owned()))
            .unwrap();
        drop(app); 

        std::thread::sleep_ms(3000);
        let excs = dbg!(block_on(repo.async_get_executions()).unwrap());
        assert_eq!(excs.len(), 1);
        assert_eq!(excs.get(0).unwrap().state, repository::ExecutionState::Completed);

        clean_expe_repo();
        clean_cmp_repo();
        sshd.kill();
        sshd.wait();

    }

    #[test]
    fn test_wait_connection() {

        init_logger();
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = crate::repository::Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
            .unwrap();
        let repo = crate::repository::CampaignHandle::spawn(repo.conf).unwrap();

        let conf = crate::hosts::HostConf {
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:55555".to_owned(),
            start_alloc: "".to_owned(),
            get_alloc_nodes: "echo localhost".to_owned(),
            cancel_alloc: "".to_owned(),
            alloc_duration: 10,
            executions_per_nodes: 2,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/home/apere/Executions"),
        };
        let host = crate::hosts::HostHandle::spawn_resource(conf).unwrap();
        let commit = get_expe_repo_head();

        let app = ApplicationHandle::spawn(repo.clone()).unwrap();

        block_on(app.async_register_host(host.clone())).unwrap();

        block_on(app.async_submit_exec(
            crate::repository::ExperimentCommit(commit.clone()),
            crate::repository::ExecutionParameters("".to_owned()), 
            Vec::new(), 
            "localhost".to_owned()))
            .unwrap();

        std::thread::sleep_ms(1000);
        let excs = dbg!(block_on(repo.async_get_executions()).unwrap());
        assert_eq!(excs.len(), 1);
        let ex1 = excs.get(0).unwrap();
        assert!(ex1.state != repository::ExecutionState::Completed);

        std::thread::sleep_ms(10000);
        let mut sshd = setup_ssh_server();
        std::thread::sleep_ms(10000);

        let excs_new = dbg!(block_on(repo.async_get_executions()).unwrap());
        assert_eq!(excs_new.len(), 1);
        let ex2 = excs_new.get(0).unwrap();
        assert!(ex2.state == repository::ExecutionState::Completed);
        assert_eq!(ex1.identifier, ex2.identifier );

        drop(app); 

        sshd.kill();
        sshd.wait();
        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_reschedule_execution() {
        use std::os::unix::fs::PermissionsExt;

        init_logger();
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        let mut sshd = setup_ssh_server();


        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = crate::repository::Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
            .unwrap();
        let repo = crate::repository::CampaignHandle::spawn(repo.conf).unwrap();

        let conf = crate::hosts::HostConf {
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:55555".to_owned(),
            start_alloc: "".to_owned(),
            get_alloc_nodes: "echo localhost".to_owned(),
            cancel_alloc: "".to_owned(),
            alloc_duration: 10,
            executions_per_nodes: 2,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/home/apere/Executions"),
        };
        let host = crate::hosts::HostHandle::spawn_resource(conf).unwrap();
        let commit = get_expe_repo_head();

        let app = ApplicationHandle::spawn(repo.clone()).unwrap();

        block_on(app.async_register_host(host.clone())).unwrap();

        block_on(app.async_activate_rescheduling()).unwrap();

        block_on(app.async_submit_exec(
            crate::repository::ExperimentCommit(commit.clone()),
            crate::repository::ExecutionParameters("".to_owned()), 
            Vec::new(), 
            "localhost".to_owned()))
            .unwrap();

        std::thread::sleep_ms(1);
        let excs = dbg!(block_on(repo.async_get_executions()).unwrap());
        assert_eq!(excs.len(), 1);
        let ex1 = excs.get(0).unwrap();
        let mut perm = std::fs::metadata(ex1.get_path()).unwrap().permissions();
        dbg!(wrap_sh!("rm -rf /home/apere/Executions"));
        std::thread::sleep_ms(10000);
        dbg!(wrap_sh!("mkdir /home/apere/Executions"));
        std::thread::sleep_ms(10000);

        let excs_new = dbg!(block_on(repo.async_get_executions()).unwrap());
        assert_eq!(excs_new.len(), 1);
        let ex2 = excs_new.get(0).unwrap();
        assert!(ex2.state == repository::ExecutionState::Completed);
        assert_ne!(ex1.identifier, ex2.identifier );

        drop(app); 

        sshd.kill();
        sshd.wait();
        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_pack_folder() {
        setup_folder();
        pack_folder(path::PathBuf::from("/tmp/pack_folder")).unwrap();
        fs::remove_dir_all("/tmp/pack_folder").unwrap();
    }
}
