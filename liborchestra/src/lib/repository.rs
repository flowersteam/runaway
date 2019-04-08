// liborchestra/repository.rs
// Author: Alexandre Péré
/// This module contains a `Repository` structure representing a campaign repository. It is built as
/// a `Future` __resource__, i.e. a structure that provides futures, and takes care about waking the
/// tasks when the computation can carry on.

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use std::{fmt, fs, io, path, str, error, thread};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Poll, Waker};
use std::collections::{HashMap, HashSet};
use serde_yaml;
use regex;
use chrono;
use uuid;
use super::{CMPCONF_RPATH, XPRP_RPATH, EXCS_RPATH, DATA_RPATH, EXCCONF_RPATH};
use crate::misc;
use chashmap::CHashMap;
use uuid::Uuid;
use futures::future::Future;
use std::pin::Pin;
use url::Url;
use git2;
use std::sync::atomic::{AtomicBool, Ordering};

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
pub enum Error {
    NotARepo,
    AlreadyRepo,
    InvalidRepo,
    InvalidExpeCommit,
    NoOutputAvailable,
    ChannelBroken(String),
    FetchExperiment(String),
    CreateExecution(String),
    UpdateExecution(String),
    FinishExecution(String),
    FetchExecutions(String),
    DeleteExecution(String),
    ReadExecution,
    WriteExecution,
    ReadCampaign,
    WriteCampaign,
    WrongPath,
    NoFFPossible,
    Unknown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NotARepo => write!(f, "Not a expegit repository"),
            Error::AlreadyRepo => write!(f, "Already an expegit repository"),
            Error::InvalidRepo => write!(f, "Invalid expegit repository"),
            Error::InvalidExpeCommit => write!(f, "Invalid experiment commit"),
            Error::NoOutputAvailable => write!(f, "No output was available"),
            Error::ChannelBroken(s) => write!(f, "Communication channel broken: {}", s),
            Error::FetchExperiment(s) => write!(f, "Failed to fetch experiment: {}", s),
            Error::CreateExecution(s) => write!(f, "Failed to create execution: {}", s),
            Error::UpdateExecution(s) => write!(f, "Failed to update execution: {}", s),
            Error::FinishExecution(s) => write!(f, "Failed to finish execution: {}", s),
            Error::FetchExecutions(s) => write!(f, "Failed to fetch executions: {}", s),
            Error::DeleteExecution(s) => write!(f, "Failed to delete execution: {}", s),
            Error::ReadExecution => write!(f, "Failed to read execution from file"),
            Error::WriteExecution => write!(f, "Failed to write execution from file"),
            Error::ReadCampaign => write!(f, "Failed to read campaign from file"),
            Error::WriteCampaign => write!(f, "Failed to write campaign from file"),
            Error::WrongPath => write!(f, "Something went wrong with a path"),
            Error::NoFFPossible => write!(f, "Couldn't perform fast-forward pull"),
            Error::Unknown => write!(f, "Unknown error occurred"),
        }
    }
}

///////////////////////////////////////////////////////////////////////// REPOSITORY SYNCHRONIZATION

/// This enumeration represents the different mechanisms that can be used to synchronize a
/// repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub enum Synchronizer{
    Null,
}

impl Synchronizer{

    /// Hook called after the repository was initialized
    fn init_repository_hook(&self, cmp: &CampaignConf) -> Result<(),Error>{
        debug!("Repository initialization hook called.");
        match self {
            Null => Ok(()),
        }
    }

    /// Hook called after the experiment was fetched
    fn fetch_experiment_hook(&self, cmp: &CampaignConf) -> Result<(),Error>{
        debug!("Fetch experiment hook called.");
        match self {
            Null => Ok(()),
        }
    }

    /// Hook called after an execution was created
    fn create_execution_hook(&self, exc: &ExecutionConf) -> Result<(),Error>{
        debug!("Execution creation hook called.");
        match self {
            Null => Ok(()),

        }
    }

    /// Hook called after an execution was updated
    fn update_execution_hook(&self, exc: &ExecutionConf) -> Result<(),Error>{
        debug!("Execution update hook called.");
        match self{
            Null => Ok(()),

        }
    }

    /// Hook called after the execution was reset
    fn reset_execution_hook(&self, exc: &ExecutionConf) -> Result<(),Error>{
        debug!("Execution reset hook called.");
        match self{
            Null => Ok(()),

        }
    }

    /// Hook called after the execution was finished
    fn finish_execution_hook(&self, exc: &ExecutionConf) -> Result<(),Error>{
        debug!("Execution finished hook called.");
        match self {
            Null => Ok(()),

        }
    }

    /// Hook called after the executions were fetched
    fn fetch_executions_hook(&self, cmp: &CampaignConf) -> Result<(),Error>{
        debug!("Execution fetched hook called.");
        match self {
            Null => Ok(()),

        }
    }
}

///////////////////////////////////////////////////////////////////////////////////// CONFIGURATIONS

/// Represents the configuration of a campaign repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct CampaignConf {
    path: Option<path::PathBuf>,
    synchronizer: Synchronizer,
    version: String,
}

impl CampaignConf {
    /// Read campaign from file.
    pub fn from_file(conf_path: &path::PathBuf) -> Result<CampaignConf, crate::Error>{
        debug!("Reading campaign configuration from file {}", conf_path.to_str().unwrap());
        let file = fs::File::open(conf_path).map_err(|e| crate::Error::Io(e))?;
        let mut config: CampaignConf = serde_yaml::from_reader(file)?;
        config.path = Some(conf_path.parent().unwrap().to_path_buf().clone());
        return Ok(config);
    }

    /// Writes campaign to file.
    pub fn to_file(&self, path: &path::PathBuf) -> Result<(), crate::Error>{
        debug!("Writing campaign configuration {} to file", self);
        let file = fs::File::create(path).map_err(|e| crate::Error::Io(e))?;
        let mut cmp = self.clone();
        cmp.path = None;
        serde_yaml::to_writer(file, &cmp).map_err(|e| crate::Error::Yaml(e))?;
        Ok(())
    }

    /// Returns the root path of the campaign
    pub fn get_path(&self) -> path::PathBuf {
        self.path.as_ref().unwrap().to_owned()
    }

    /// Returns the path to the experiment repository
    pub fn get_experiment_path(&self) -> path::PathBuf {
        self.get_path().join(XPRP_RPATH)
    }

    /// Returns the path to the executions
    pub fn get_executions_path(&self) -> path::PathBuf {
        self.get_path().join(EXCS_RPATH)
    }

    // Get a list of executions from the available files (Slow).
    fn get_executions_from_files(&self) -> Vec<ExecutionConf>{
        fs::read_dir(self.get_executions_path())
            .unwrap()
            .map(|p| p.unwrap().path().join(EXCCONF_RPATH))
            .filter(|p| p.exists())
            .map(|p| ExecutionConf::from_file(&p).unwrap())
            .collect::<Vec<ExecutionConf>>()
    }
}

impl fmt::Display for CampaignConf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        return write!(
            f,
            "Campaign<[{}]>",
            self.get_path().to_str().unwrap()
        );
    }
}

/// Represents the possible states of an execution
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Clone)]
pub enum ExecutionState {
    Initialized,
    Interrupted,
    Finished,
    Hazard,
}

impl<'a> From<&'a str> for ExecutionState {
    fn from(s: &str) -> ExecutionState {
        match s {
            "initialized" => ExecutionState::Initialized,
            "interrupted" => ExecutionState::Interrupted,
            "finished" => ExecutionState::Finished,
            _ => ExecutionState::Hazard,
        }
    }
}

impl fmt::Display for ExecutionState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecutionState::Initialized => write!(f, "Initialized"),
            ExecutionState::Interrupted => write!(f, "Interrupted"),
            ExecutionState::Finished => write!(f, "Finished"),
            ExecutionState::Hazard => write!(f, "Hazard"),
        }
    }
}

/// Newtype representing a commit string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExperimentCommit(String);
impl fmt::Display for ExperimentCommit{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        write!(f, "{}", self.0)
    }
}

/// Newtype representing parameters string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionParameters(String);

/// Newtype representing tag string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionTag(String);

/// Newtype representing an execution identifier.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone, Hash)]
pub struct ExecutionId(Uuid);
impl fmt::Display for ExecutionId{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        write!(f, "{}", self.0)
    }
}

/// Represents the configuration and results of an execution of the experiment
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ExecutionConf {
    identifier: ExecutionId,
    path: Option<path::PathBuf>,
    commit: ExperimentCommit,
    parameters: ExecutionParameters,
    state: ExecutionState,
    experiment_elements: Vec<path::PathBuf>,
    executor: Option<String>,
    execution_date: Option<String>,
    execution_duration: Option<u32>,
    execution_stdout: Option<String>,
    execution_stderr: Option<String>,
    execution_exit_code: Option<u32>,
    execution_fitness: Option<f64>,
    generator: String,
    generation_date: String,
    tags: Vec<ExecutionTag>,
}

impl ExecutionConf {
    /// Writes execution to file.
    pub fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error>{
        debug!("Writing execution configuration {} to file", self);
        let file = fs::File::create(conf_path).map_err(|_| Error::WriteExecution)?;
        let mut conf = self.clone();
        conf.path = None;
        serde_yaml::to_writer(file, &conf).map_err(|_| Error::WriteExecution)?;
        Ok(())
    }
    /// Load an execution from its path
    pub fn from_file(exc_path: &path::PathBuf) -> Result<ExecutionConf, Error> {
        trace!("Loading Execution from {}", exc_path.to_str().unwrap());
        let file = fs::File::open(exc_path).map_err(|_| Error::ReadExecution)?;
        let mut config: ExecutionConf = serde_yaml::from_reader(file).map_err(|_| Error::ReadExecution)?;
        config.path = Some(exc_path.parent().unwrap().to_path_buf().clone());
        return Ok(config);
    }

    /// Returns the root path of the execution
    pub fn get_path(&self) -> path::PathBuf {
        self.path.as_ref().unwrap().to_owned()
    }

    /// Returns the path to the data folder
    pub fn get_data_path(&self) -> path::PathBuf {
        self.get_path().join(DATA_RPATH)
    }
}

impl fmt::Display for ExecutionConf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Execution<{:?}>", self.identifier)
    }
}

// Represents the update of an execution.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ExecutionUpdate{
    executor: Option<String>,
    execution_date: Option<String>,
    execution_duration: Option<u32>,
    execution_stdout: Option<String>,
    execution_stderr: Option<String>,
    execution_exit_code: Option<u32>,
    execution_fitness: Option<f64>,
}

impl ExecutionUpdate{

    // Consumes the update and an execution to generate a new execution.
    fn apply(self, conf: ExecutionConf) -> ExecutionConf {
        let mut conf = conf;
        if let Some(s) = self.executor{
            conf.executor = Some(s)
        }
        if let Some(s) = self.execution_date{
            conf.execution_date = Some(s)
        }
        if let Some(s) = self.execution_duration{
            conf.execution_duration = Some(s)
        }
        if let Some(s) = self.execution_stdout{
            conf.execution_stdout = Some(s)
        }
        if let Some(s) = self.execution_stderr{
            conf.execution_stderr = Some(s)
        }
        if let Some(s) = self.execution_exit_code{
            conf.execution_exit_code = Some(s)
        }
        if let Some(s) = self.execution_fitness{
            conf.execution_fitness = Some(s)
        }
        return conf;
    }
}

////////////////////////////////////////////////////////////////////////////// REPOSITORY OPERATIONS

/// Enumerates the different operations that can be performed on a repository.
#[derive(Clone,Debug)]
pub enum RepositoryOperation{
    FetchExperiment(FetchExperimentOp),
    CreateExecution(CreateExecutionOp),
    UpdateExecution(UpdateExecutionOp),
    FinishExecution(FinishExecutionOp),
    DeleteExecution(DeleteExecutionOp),
    FetchExecutions(FetchExecutionsOp),
}

/// A structure representing an operation of fetch of the experiment.
#[derive(Clone, Debug)]
pub struct FetchExperimentOp{
    input: (),
    output: Option<Result<(),Error>>
}

/// A structure representing an operation of creation of an operation.
#[derive(Clone, Debug)]
pub struct CreateExecutionOp{
    input: (ExperimentCommit, ExecutionParameters, Vec<ExecutionTag>),
    output: Option<Result<ExecutionConf ,Error>>
}

/// A structure representing an operation of update of an execution.
#[derive(Clone, Debug)]
pub struct UpdateExecutionOp{
    input: (ExecutionId, ExecutionUpdate),
    output: Option<Result<ExecutionConf, Error>>
}

/// A structure representing an operation of finish of an execution.
#[derive(Clone, Debug)]
pub struct FinishExecutionOp{
    input: ExecutionId,
    output: Option<Result<ExecutionConf, Error>>
}

/// A structure representing an operation of removal of an execution.
#[derive(Clone, Debug)]
pub struct DeleteExecutionOp{
    input: ExecutionId,
    output: Option<Result<(), Error>>
}

/// A structure representing an operation of a fetch of executions.
#[derive(Clone, Debug)]
pub struct FetchExecutionsOp{
    input:(),
    output: Option<Result<Vec<ExecutionConf>, Error>>
}

///////////////////////////////////////////////////////////////////////////////////////// REPOSITORY

/// Messages handled by the execution cache.
#[derive(Debug)]
enum ExecutionsCacheMessage{
    New((ExecutionId, ExecutionConf)),
    Update((ExecutionId, ExecutionConf)),
    Delete(ExecutionId),
}

/// This structure allows to cache executions. A thread takes care about handling messages
/// specifying changes on the executions set, and makes sure that a channel always has the last
/// version available.
#[derive(Clone)]
struct ExecutionsCache {
    receiver: Option<crossbeam::channel::Receiver<Vec<ExecutionConf>>>,
    thread_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>
}

impl ExecutionsCache{
    /// Creates a new execution cacher.
    pub fn new(existing_executions: Vec<ExecutionConf>)
        -> (ExecutionsCache, crossbeam::channel::Sender<ExecutionsCacheMessage>){
        debug!("Creating new execution cache");
        let (messages_sender, messages_receiver) = crossbeam::channel::unbounded();
        let (executions_sender, executions_receiver) = crossbeam::channel::bounded(1);
        let executions_receiver_1 = executions_receiver.clone();
        trace!("Starting execution cache thread");
        let thread_handle = thread::spawn(move ||{
            let mut executions_map: HashMap<ExecutionId, ExecutionConf> = existing_executions.iter()
                .map(|e| (e.identifier.clone(), e.to_owned()))
                .collect();
            trace!("Execution Cache Thread: Starting event loop");
            loop {
                match messages_receiver.try_recv(){
                    Ok(ExecutionsCacheMessage::New((id, conf))) => {
                        trace!("Execution Cache Thread: Received create message for id {}",id);
                        executions_map.insert(id, conf);
                    },
                    Ok(ExecutionsCacheMessage::Update((id, conf))) => {
                        trace!("Execution Cache Thread: Received update message for id {}",id);
                        executions_map.remove(&id).unwrap();
                        executions_map.insert(id, conf);
                    },
                    Ok(ExecutionsCacheMessage::Delete(id)) => {
                        trace!("Execution Cache Thread: Received delete message for id {}",id);
                        let conf = executions_map.remove(&id).unwrap();
                    },
                    Err(crossbeam::channel::TryRecvError::Empty) => {
                        //trace!("Execution Cache Thread: No messages. Updating executions.");
                        let executions: Vec<ExecutionConf> = executions_map.values()
                            .map(|e| e.clone())
                            .collect();
                        match executions_sender.try_send(executions){
                            Err(crossbeam::channel::TrySendError::Disconnected(_)) => {
                                panic!("Execution Cache Thread: Executions sending channel found \
                                disconnected...")
                            },
                            Err(crossbeam::channel::TrySendError::Full(_)) => {
                                executions_receiver_1.try_recv();
                            },
                            Ok(()) => {}
                        }
                    },
                    Err(crossbeam::channel::TryRecvError::Disconnected) =>{
                        trace!("Execution Cacher Thread: No receiver left. Breaking loop");
                        break
                    }
                }
            }
            trace!("Execution Cacher Thread: Shutting thread.")
        });
        let cache = ExecutionsCache{
            receiver: Some(executions_receiver),
            thread_handle: Arc::new(Mutex::new(Some(thread_handle))),
        };
        return (cache, messages_sender);
    }

    /// Retrieves executions from the cache
    pub fn get_executions(&self) -> Result<Vec<ExecutionConf>, Error>{
        self.receiver
            .as_ref()
            .unwrap()
            .recv()
            .map_err(|_| Error::ChannelBroken("Executions channel".to_owned()))
    }
}

impl Drop for ExecutionsCache{
    fn drop(&mut self) {
        if Arc::strong_count(&self.thread_handle) == 1 {
            debug!("Dropping executions cache");
            drop(self.receiver.take().unwrap());
            self.thread_handle
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .join()
                .unwrap();
        }
    }
}

/// This structure represents a repository. It offers synchronized operations over a repository,
/// through the use of futures. Every operations are represented by asynchronous futures, which
/// will communicate with an inner event processing loop, that will treat operations in a
/// synchronized manner. The repository is `Clone` so that you can copy it around, while being
/// guaranteed to access the same resource. Put differently, it is a clonable handle
/// over a synchronized repository.
// This repository works by spawning a thread (see spawn_thread), that will handle repository
// operations (RepositoryOperations) sent to it via a channel. The future takes care about sending
// the right operation (along with the wake handle) to the thread, before Pending the task. When
// the thread comes to the operation (identified with a uuid), the thread performs it and stores the
// results in a shared hashmap. The threads wakes the task and keep processing the other operations.
// When the task is woken, it will retrieve the result of the operation from the shared
// hashmap, and resolve to the result (see any of the futures).
#[derive(Clone)]
pub struct Repository{
    operations_results: Arc<CHashMap<Uuid, RepositoryOperation>>,
    operations_sender: Option<mpsc::Sender<(RepositoryOperation, Waker, Uuid)>>,
    thread_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    executions: ExecutionsCache,
    campaign: CampaignConf,
}

impl Repository {

    /// Opens a new repository at the local path, using the experiment repository url.
    pub fn new(local_path: &path::PathBuf, experiment_url: Url)-> Result<Repository, crate::Error> {
        debug!("Initializing campaign on experiment {} in {}",
               experiment_url,
               local_path.to_str().unwrap());
        fs::create_dir_all(local_path)?;
        let expe_repository = git2::Repository::clone(experiment_url.as_str(),
                                                      local_path.join(XPRP_RPATH))?;
        let campaign = CampaignConf{
            path: Some(local_path.to_owned()),
            synchronizer: Synchronizer::Null,
            version:  env!("CARGO_PKG_VERSION").to_owned()};
        fs::create_dir(campaign.get_executions_path())?;
        let operations_results = Arc::new(CHashMap::new());
        let (operations_sender, operations_receiver) = mpsc::channel();
        let (executions, executions_sender) = ExecutionsCache::new(
            campaign.get_executions_from_files());
        let thread_handle = Repository::spawn_thread(campaign.clone(),
                                 expe_repository,
                                 operations_receiver,
                                 operations_results.clone(),
                                 executions_sender);
        campaign.to_file(&local_path.join(CMPCONF_RPATH));
        let repo = Repository{
            operations_results,
            operations_sender: Some(operations_sender),
            thread_handle: Arc::new(Mutex::new(Some(thread_handle))),
            executions,
            campaign,
        };
        return Ok(repo);
    }

    // This function spawns the thread that will handle all the repository operations.
    fn spawn_thread(campaign: CampaignConf,
                    experiment_repo: git2::Repository,
                    operation_receiver: mpsc::Receiver<(RepositoryOperation, Waker, Uuid)>,
                    operation_results: Arc<CHashMap<Uuid, RepositoryOperation>>,
                    executions_sender: crossbeam::channel::Sender<ExecutionsCacheMessage>
                    ) -> thread::JoinHandle<()>{
        debug!("Start Repository Thread");
        let thread_handle = thread::spawn(move || {
            trace!("Repository Thread; starting operation loop");
            let mut experiment_repo = experiment_repo;
            while let Ok((operation, waker, uuid)) = operation_receiver.recv(){
                trace!("Repository Thread: Received operation {:?} to perform", operation);
                match operation {
                    RepositoryOperation::FetchExperiment(ope) => {
                        let performed_ope = Repository::handle_fetch_experiment_operation(
                            &campaign, ope);
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::FetchExperiment(performed_ope));
                    },
                    RepositoryOperation::CreateExecution(ope) => {
                        let performed_ope = Repository::handle_create_execution_operation(
                            &campaign, ope);
                        if let Some(Ok(ref exc)) = performed_ope.output {
                            let mess = ExecutionsCacheMessage::New((exc.identifier.clone(),
                                                                     exc.clone()));
                            executions_sender.send(mess).unwrap();
                        }
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::CreateExecution(performed_ope));
                    },
                    RepositoryOperation::UpdateExecution(ope) => {
                        let performed_ope = Repository::handle_update_execution_operation(
                            &campaign, ope);
                        if let Some(Ok(ref exc)) = performed_ope.output {
                            let mess = ExecutionsCacheMessage::Update((exc.identifier.clone(),
                                                                        exc.clone()));
                            executions_sender.send(mess).unwrap();
                        };
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::UpdateExecution(performed_ope));
                    },
                    RepositoryOperation::FinishExecution(ope) => {
                        let performed_ope = Repository::handle_finish_execution_operation(
                            &campaign, ope);
                        if let Some(Ok(ref exc)) = performed_ope.output{
                            let mess = ExecutionsCacheMessage::Update((exc.identifier.clone(),
                                                                        exc.clone()));
                            executions_sender.send(mess).unwrap();
                        }
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::FinishExecution(performed_ope));
                    },
                    RepositoryOperation::DeleteExecution(ope) => {
                        let performed_ope = Repository::handle_delete_execution_operation(
                            &campaign, ope);
                        if let Some(Ok(ref exc)) = performed_ope.output{
                            let mess = ExecutionsCacheMessage::Delete(performed_ope.input.clone());
                            executions_sender.send(mess).unwrap();
                        }
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::DeleteExecution(performed_ope));
                    },
                    RepositoryOperation::FetchExecutions(ope) => {
                        let performed_ope = Repository::handle_fetch_executions_operation(
                            &campaign, ope);
                        if let Some(Ok(ref exc)) = performed_ope.output{
                            exc.iter()
                                .map(|e| ExecutionsCacheMessage::New((e.identifier.clone(), e.clone())))
                                .for_each(|m| executions_sender.send(m).unwrap())

                        }
                        operation_results.insert_new(
                            uuid,
                            RepositoryOperation::FetchExecutions(performed_ope));
                    },
                }
                trace!("Repository Thread: Operation performed. Calling waker.");
                waker.wake();
            }
            trace!("Repository Thread: Operations channel disconnected. Stopping thread.")
        });
        return thread_handle;
    }

    /// Async method, returning a future that ultimately resolves in a campaign, after having
    /// fetched the origin changes on the experiment repository.
    pub fn fetch_experiment(&self) -> FetchExperimentFuture{
        let ope = FetchExperimentOp{
            input: (),
            output: None
        };
        let state = FutureState::Starting(ope);
        return FetchExperimentFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the fetch experiment operation.
    fn handle_fetch_experiment_operation(cmp: &CampaignConf, ope: FetchExperimentOp)
        -> FetchExperimentOp{

        let fetch_exp = || -> Result<(), Error>{
            let experiment_repo = git2::Repository::open(cmp.get_experiment_path()).unwrap();
            let mut remote = experiment_repo
                .find_remote("origin")
                .map_err(|_| Error::FetchExperiment("No origin remote found".to_owned()))?;
            remote
                .fetch(&[], None, None)
                .map_err(|_| Error::FetchExperiment("Couldn't fetch origin".to_owned()))?;
            let ann_remote_head = experiment_repo
                .find_reference("refs/remotes/origin/HEAD")
                .map_err(|_| Error::FetchExperiment("Couldn't find origin head ref".to_owned()))
                .and_then(|rh| {
                    rh.target()
                        .ok_or(Error::FetchExperiment("Couldn't find origin head target".to_owned()))
                })
                .and_then(|oid|{
                    experiment_repo.find_annotated_commit(oid)
                        .map_err(|_|
                            Error::FetchExperiment("Couldn't find annotated commit".to_owned()))
                })?;
            let (analysis, preferences) = experiment_repo.merge_analysis(&[&ann_remote_head])
                .map_err(|_| Error::Unknown)?;
            if analysis.is_fast_forward(){
                let tree = experiment_repo.find_commit(ann_remote_head.id())
                    .map_err(|_| Error::FetchExperiment("Couldn't find commit".to_owned()))?
                    .tree()
                    .map_err(|_| Error::FetchExperiment("Couldn't find commit tree".to_owned()))?;
                experiment_repo.checkout_tree(tree.as_object(), None)
                    .map_err(|_| Error::FetchExperiment("Couldn't checkout tree".to_owned()))?;
                experiment_repo.find_reference("refs/heads/master")
                    .map_err(|_|
                        Error::FetchExperiment("Couldn't find local master head ref".to_owned()))?
                    .set_target(ann_remote_head.id(), "fast forward")
                    .map_err(|_| Error::FetchExperiment("Couldn't set HEAD to target".to_owned()))?;
                experiment_repo.head()
                    .map_err(|_| Error::FetchExperiment("Coudln't find repository HEAD".to_owned()))?
                    .set_target(ann_remote_head.id(), "fast forward")
                    .map_err(|_|
                        Error::FetchExperiment("Couldn't set reposity HEAD to target".to_owned()))?;
                cmp.synchronizer.fetch_experiment_hook(&cmp)?;
                return Ok(());
            } else {
                return Err(Error::NoFFPossible);
            } 
        };

        let mut ope = ope;
        ope.output = Some(fetch_exp());
        return ope;
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration,
    /// after it has been created.
    pub fn create_execution(&self, commit: &str, parameters: &str, tags: Vec<&str>)
        -> CreateExecutionFuture{
        let ope = CreateExecutionOp{
            input: (ExperimentCommit(commit.to_owned()),
                    ExecutionParameters(parameters.to_owned()),
                    tags.into_iter().map(|a| ExecutionTag(a.to_owned())).collect()),
            output: None
        };
        let state = FutureState::Starting(ope);
        return CreateExecutionFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the create execution operation.
    fn handle_create_execution_operation(cmp: &CampaignConf, ope: CreateExecutionOp)
        -> CreateExecutionOp{

        let new_execution =  |ope: &CreateExecutionOp| -> Result<ExecutionConf, Error> {
            let (commit, params, tags) = ope.clone().input;
            let mut exc_config = ExecutionConf {
                commit: commit,
                parameters: params,
                state: ExecutionState::Initialized,
                path: None,
                experiment_elements: vec![],
                executor: None,
                execution_date: None,
                execution_duration: None,
                execution_stdout: None,
                execution_stderr: None,
                execution_exit_code: None,
                execution_fitness: None,
                generator: misc::get_hostname().unwrap(),
                generation_date: chrono::prelude::Utc::now()
                    .format("%Y-%m-%d %H:%M:%S").to_string(),
                identifier: ExecutionId(uuid::Uuid::new_v4()),
                tags: tags.clone(),
            };
            exc_config.path = Some(cmp.get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", exc_config.identifier)));
            fs::create_dir(&exc_config.get_path())
                .map_err(|_|
                    Error::CreateExecution("Failed to create directory".to_owned()))?;
            let url = format!("file://{}", cmp.get_experiment_path().to_str().unwrap());
            let repo = git2::Repository::clone(&url, exc_config.get_path())
                .map_err(|_| Error::CreateExecution("Failed to local clone".to_owned()))?;
            let commit_id = git2::Oid::from_str(&format!("{}", exc_config.commit))
                .map_err(|_| Error::CreateExecution("Ill formed commit".to_owned()))?;
            let commit = repo.find_commit(commit_id)
                .map_err(|_| Error::CreateExecution("Commit is not known".to_owned()))?;
            let tree = commit.tree()
                .map_err(|_| Error::CreateExecution("No tree attached to commit".to_owned()))?;
            repo.checkout_tree(tree.as_object(), None)
                .map_err(|_| Error::CreateExecution("Couldn't checkout tree".to_owned()))?;
            fs::remove_dir_all(exc_config.get_path().join(".git"))
                .map_err(|_|
                    Error::CreateExecution("Couldn't remove git folder".to_owned()))?;
            fs::read_dir(exc_config.get_path())
                .unwrap()
                .map(|r| {
                    match r {
                        Err(_) => Err(Error::CreateExecution(
                            "Failed in elements gathering".to_owned())),
                        Ok(r) => Ok(exc_config.experiment_elements.push(r.path()))
                    }
                })
                .collect::<Result<Vec<()>, Error>>()?;
            fs::create_dir(exc_config.get_path().join(DATA_RPATH))
                .map_err(|_|
                    Error::CreateExecution("Failed to create data folder".to_owned()))?;
            exc_config.to_file(
                &exc_config.get_path().join(EXCCONF_RPATH))
                .map_err(|_|
                    Error::CreateExecution("Failed to write config file.".to_owned()))?;
            cmp.synchronizer.create_execution_hook(&exc_config)?;
            Ok(exc_config)
        };

        let mut ope = ope;
        ope.output = Some(new_execution(&ope));
        return ope;
    }

    /// Async method, returning a future that ultimately resolves in the new configuration after it
    /// was updated.
    pub fn update_execution(&self, id: &ExecutionId, upd: &ExecutionUpdate)-> UpdateExecutionFuture{
        let ope = UpdateExecutionOp{
            input: (id.to_owned(), upd.to_owned()),
            output: None
        };
        let state = FutureState::Starting(ope);
        return UpdateExecutionFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the update execution operation.
    fn handle_update_execution_operation(cmp:&CampaignConf, ope: UpdateExecutionOp)
        -> UpdateExecutionOp{
        let update_execution =  |ope: &UpdateExecutionOp| -> Result<ExecutionConf, Error> {
            let (id, upd) = &ope.input;
            let conf_path = cmp.get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id))
                .join(EXCCONF_RPATH);
            let exc_conf = ExecutionConf::from_file(&conf_path)?;
            let exc_conf = upd.clone().apply(exc_conf);
            exc_conf.to_file(&conf_path)?;
            cmp.synchronizer.update_execution_hook(&exc_conf)?;
            Ok(exc_conf)
        };
        let mut ope = ope;
        ope.output = Some(update_execution(&ope));
        return ope;
    }


    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn finish_execution(&self, id: &ExecutionId) -> FinishExecutionFuture{
        let ope = FinishExecutionOp{
            input: id.to_owned(),
            output: None
        };
        let state = FutureState::Starting(ope);
        return FinishExecutionFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the finishing of an execution.
    fn handle_finish_execution_operation(cmp:&CampaignConf, ope: FinishExecutionOp)-> FinishExecutionOp{
        let finish_execution =  |ope: &FinishExecutionOp| -> Result<ExecutionConf, Error> {
            let id = &ope.input;
            let conf_path = cmp.get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id))
                .join(EXCCONF_RPATH);
            let mut exc_conf = ExecutionConf::from_file(&conf_path)?;
            exc_conf.experiment_elements
                .iter()
                .map(|p| exc_conf.get_path().join(p))
                .map(|p| if p.is_file() {fs::remove_file(p)} else {fs::remove_dir_all(p)})
                .map(|p| p.map_err(|_|
                    Error::FinishExecution("Failed to remote one experiment element".to_owned())))
                .collect::<Result<Vec<()>, Error>>()?;
            exc_conf.state = ExecutionState::Finished;
            exc_conf.to_file(&conf_path)?;
            cmp.synchronizer.update_execution_hook(&exc_conf)?;
            Ok(exc_conf)
        };
        let mut ope = ope;
        ope.output = Some(finish_execution(&ope));
        return ope;
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn delete_execution(&self, id: &ExecutionId) -> DeleteExecutionFuture{
        let ope = DeleteExecutionOp{
            input: id.to_owned(),
            output: None
        };
        let state = FutureState::Starting(ope);
        return DeleteExecutionFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the removal of an execution.
    fn handle_delete_execution_operation(cmp:&CampaignConf, ope: DeleteExecutionOp)-> DeleteExecutionOp{
        let delete_execution =  |ope: &DeleteExecutionOp| -> Result<(), Error> {
            let id = &ope.input;
            let conf_path = cmp.get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id));
            fs::remove_dir_all(conf_path)
                .map_err(|_| Error::DeleteExecution("Failed to remove execution files".to_owned()))?;
            Ok(())
        };
        let mut ope = ope;
        ope.output = Some(delete_execution(&ope));
        return ope;
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn fetch_executions(&self) -> FetchExecutionsFuture{
        let ope = FetchExecutionsOp{
            input: (),
            output: None
        };
        let state = FutureState::Starting(ope);
        return FetchExecutionsFuture{
            state,
            repository: self.clone(),
            uuid: Uuid::new_v4(),
        };
    }

    // Function called by the thread to handle the fetch executions operation.
    fn handle_fetch_executions_operation(cmp:&CampaignConf, ope: FetchExecutionsOp) -> FetchExecutionsOp{
        let fetch_executions =  || -> Result<Vec<ExecutionConf>, Error> {
            let before: HashSet<path::PathBuf> = fs::read_dir(cmp.get_executions_path())
                .unwrap()
                .map(|p| p.unwrap().path())
                .filter(|p| p.join(EXCCONF_RPATH).exists())
                .collect();
            cmp.synchronizer.fetch_executions_hook(cmp)?;
            let after: HashSet<path::PathBuf> = fs::read_dir(cmp.get_executions_path())
                .unwrap()
                .map(|p| p.unwrap().path())
                .filter(|p| p.join(EXCCONF_RPATH).exists())
                .collect();
            if before.difference(&after).next().is_some(){
                Err(Error::FetchExecutions(
                    "Some executions existed before fetch but not after".to_owned()))
            } else {
                after.difference(&before)
                    .map(|p| ExecutionConf::from_file(&cmp.get_executions_path().join(p)))
                    .collect::<Result<Vec<ExecutionConf>, Error>>()
            }
        };
        let mut ope = ope;
        ope.output = Some(fetch_executions());
        return ope;
    }

    /// Synchronous function that allows to retrieve the executions that curently exists in the
    /// repository.
    pub fn get_executions(&self) -> Result<Vec<ExecutionConf>, Error> {
        return self.executions.get_executions();
    }
}

impl Drop for Repository{
    fn drop(&mut self) {
        if Arc::strong_count(&self.thread_handle) == 1 {
            debug!("Dropping repository");
            drop(self.operations_sender.take().unwrap());
            self.thread_handle
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .join()
                .unwrap();
        }
    }
}

///////////////////////////////////////////////////////////////////////////////// REPOSITORY FUTURES

enum FutureState<T> {
    Starting(T),
    Waiting,
    Finished,
}


/// This future is issued by the `Repository::fetch_experiment` method. It will ultimately resolve
/// into a result over a null type.
pub struct FetchExperimentFuture {
    state: FutureState<FetchExperimentOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for FetchExperimentFuture {
    type Output = Result<(), Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<(), Error>> {
        debug!("Fetch Experiment Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending fetch experiment operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::FetchExperiment(ope.clone()),
                               wake.clone(),
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving create execution results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::FetchExperiment(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::CreateExecution(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::FetchExperiment(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::FetchExperiment(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("FetchExperimentFuture was polled in a finished state");
                }
            }
        }
    }
}


/// This future is issued by the `Repository::create_execution` method. It will ultimately resolve
/// into a result over an execution configuration.
pub struct CreateExecutionFuture {
    state: FutureState<CreateExecutionOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for CreateExecutionFuture {
    type Output = Result<ExecutionConf, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<ExecutionConf, Error>> {
        debug!("Create Execution Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending create execution operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::CreateExecution(ope.clone()),
                               wake.clone(), 
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving create execution results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::CreateExecution(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::CreateExecution(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::CreateExecution(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::CreateExecution(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("CreateExecutionFuture was polled in a finished state");
                }
            }
        }
    }
}

/// This future is issued by the `Repository::update_execution` method. It will ultimately resolve
/// into a result over an execution configuration.
pub struct UpdateExecutionFuture {
    state: FutureState<UpdateExecutionOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for UpdateExecutionFuture {
    type Output = Result<ExecutionConf, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<ExecutionConf, Error>> {
        debug!("Update Execution Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending update execution operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::UpdateExecution(ope.clone()),
                               wake.clone(),
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving update execution results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::UpdateExecution(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::UpdateExecution(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::UpdateExecution(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::UpdateExecution(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("UpdateExecutionFuture was polled in a finished state");
                }
            }
        }
    }
}

/// This future is issued by the `Repository::finish_execution` method. It will ultimately resolve
/// into a result over an execution configuration.
pub struct FinishExecutionFuture {
    state: FutureState<FinishExecutionOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for FinishExecutionFuture {
    type Output = Result<ExecutionConf, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<ExecutionConf, Error>> {
        debug!("Update Execution Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending update execution operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::FinishExecution(ope.clone()),
                               wake.clone(),
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving update execution results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::FinishExecution(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::FinishExecution(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::FinishExecution(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::FinishExecution(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("FinishExecutionFuture was polled in a finished state");
                }
            }
        }
    }
}

/// This future is issued by the `Repository::delete_execution` method. It will ultimately resolve
/// into a result over a null type after having deleted the execution.
pub struct DeleteExecutionFuture {
    state: FutureState<DeleteExecutionOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for DeleteExecutionFuture {
    type Output = Result<(), Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<(), Error>> {
        debug!("Delete Execution Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending delete execution operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::DeleteExecution(ope.clone()),
                               wake.clone(),
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving delete execution results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::DeleteExecution(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::DeleteExecution(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::DeleteExecution(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::DeleteExecution(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("DeleteExecutionFuture was polled in a finished state");
                }
            }
        }
    }
}


/// This future is issued by the `Repository::fetch_executions` method. It will ultimately resolve
/// into a result over a null type.
pub struct FetchExecutionsFuture {
    state: FutureState<FetchExecutionsOp>,
    repository: Repository,
    uuid: Uuid,
}

impl Future for FetchExecutionsFuture {
    type Output = Result<Vec<ExecutionConf>, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<Vec<ExecutionConf>, Error>> {
        debug!("Fetch Executions Future is being polled ...");
        loop {
            match &self.state {
                FutureState::Starting(ope) => {
                    trace!("Sending fetch executions operation");
                    let (state, ret) = self.repository
                        .operations_sender
                        .as_ref()
                        .unwrap()
                        .send((RepositoryOperation::FetchExecutions(ope.clone()),
                               wake.clone(),
                               self.uuid.clone()))
                        .map_or_else(
                            |_| {
                                (FutureState::Finished,
                                 Poll::Ready(Err(Error::ChannelBroken(
                                     "repository operations sender".to_owned())))
                                )},
                            |_| {(FutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                FutureState::Waiting => {
                    trace!("Retrieving fetch executions results");
                    self.state = FutureState::Finished;
                    return match self.repository.operations_results.remove(&self.uuid){
                        Some(RepositoryOperation::FetchExecutions(ret)) => {
                            ret.output.map_or_else(
                                || { Poll::Ready(
                                    Err(Error::FetchExecutions(
                                        "Operation gathered, output is None".to_owned())))
                                },
                                |res| { Poll::Ready(res)})
                        },
                        None => {
                            Poll::Ready(Err(Error::FetchExecutions(
                                "Future woken but result not available".to_owned())))
                        }
                        _ => {
                            Poll::Ready(Err(Error::FetchExecutions(
                                "Unexpected result type".to_owned())))
                        }
                    };
                }
                FutureState::Finished => {
                    panic!("FetchExecutionFuture was polled in a finished state");
                }
            }
        }
    }
}


////////////////////////////////////////////////////////////////////////////////////////////// TESTS
#[cfg(test)]
mod tests {

    use std::{fs, process};
    use std::io::prelude::*;
    use super::*;
    use pretty_logger;
    use log;

    fn setup_logger() {
        pretty_logger::init(pretty_logger::Destination::Stdout,
                            log::LogLevelFilter::Trace,
                            pretty_logger::Theme::default());
    }

    fn setup_expe_repo(){
        fs::create_dir_all("/tmp/expe_repo").unwrap();
        let mut file = fs::File::create("/tmp/expe_repo/run.py").unwrap();
        file.write_all(b"#!/usr/bin/env python\nimport time\ntime.sleep(2)").unwrap();
        process::Command::new("git")
            .args(&["init"])
            .current_dir("/tmp/expe_repo")
            .output()
            .unwrap();
        process::Command::new("git")
            .args(&["add", "run.py"])
            .current_dir("/tmp/expe_repo")
            .output()
            .unwrap();
        process::Command::new("git")
            .args(&["commit", "-m", "First"])
            .current_dir("/tmp/expe_repo")
            .output()
            .unwrap();
        let mut server = process::Command::new("git")
            .args(&["daemon","--reuseaddr", "--base-path=/tmp", "--export-all"])
            .spawn()
            .expect("Failed to start git server");
        std::thread::sleep_ms(1000);
        if let Ok(Some(o)) = server.try_wait(){
            panic!("Server did not start");
        }
    }

    fn get_expe_repo_head() -> String{
        return format!("{}", git2::Repository::open("/tmp/expe_repo")
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap());
    }

    fn clean_expe_repo(){
        process::Command::new("killall")
            .args(&["git-daemon"])
            .output()
            .unwrap();
        fs::remove_dir_all("/tmp/expe_repo");
    }

    fn clean_cmp_repo(){
        fs::remove_dir_all("/tmp/cmp_repo");
    }

    #[test]
    fn test_execution_cacher(){
        setup_logger();
        let (cache, mut sender) = ExecutionsCache::new(vec![]);
        let exc1 = ExecutionConf {
                commit: ExperimentCommit("1".to_owned()),
                parameters: ExecutionParameters("--param".to_owned()),
                state: ExecutionState::Initialized,
                path: None,
                experiment_elements: vec![],
                executor: None,
                execution_date: None,
                execution_duration: None,
                execution_stdout: None,
                execution_stderr: None,
                execution_exit_code: None,
                execution_fitness: None,
                generator: "local".to_owned(),
                generation_date: chrono::prelude::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                identifier: ExecutionId(uuid::Uuid::new_v4()),
                tags: vec![],
            };
        sender.send(ExecutionsCacheMessage::New((exc1.identifier.clone(), exc1.clone()))).unwrap();
        std::thread::sleep_ms(1);
        let excs = cache.get_executions().unwrap();
        println!("Executions: {:#?}", excs);
        assert_eq!(*excs.get(0).unwrap(), exc1);
        let mut exc2 = exc1.clone();
        exc2.executor = Some("me".to_owned());
        sender.send(ExecutionsCacheMessage::New((exc2.identifier.clone(), exc2.clone()))).unwrap();
        std::thread::sleep_ms(1);
        let excs = cache.get_executions().unwrap();
        println!("Executions: {:#?}", excs);
        assert_ne!(*excs.get(0).unwrap(), exc1);
        assert_eq!(*excs.get(0).unwrap(), exc2);
        sender.send(ExecutionsCacheMessage::Delete(exc2.identifier.clone())).unwrap();
        std::thread::sleep_ms(1);
        let excs = cache.get_executions().unwrap();
        println!("Executions: {:#?}", excs);
        assert_eq!(excs.len(), 0);

    }

    #[test]
    fn test_new_repository() {
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path, Url::parse("git://localhost:9418/expe_repo").unwrap());
        assert!(path::PathBuf::from("/tmp/cmp_repo/.cmpconf").is_file());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp").is_dir());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp/.git").is_dir());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp/run.py").is_file());
        assert!(path::PathBuf::from("/tmp/cmp_repo/excs").is_dir());

        clean_expe_repo();
        clean_cmp_repo();

    }

    #[test]
    fn test_create_execution(){
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path,
                                         Url::parse("git://localhost:9418/expe_repo").unwrap())
            .unwrap();
        let commit = get_expe_repo_head();
        let create_fut = repository.create_execution(&commit,"", vec![]);
        let exc = block_on(create_fut).unwrap();
        println!("Result: {:?}", exc);

        thread::sleep_ms(1000);
        assert!(repository.get_executions().unwrap().contains(&exc));
        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH))
            .unwrap();
        assert_eq!(exc, exc_file);
        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_stress_create_execution(){
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        use futures::executor;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path,
                                         Url::parse("git://localhost:9418/expe_repo").unwrap())
            .unwrap();

        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        let commit = get_expe_repo_head();

        for i in (1..200){
            handles.push(executor.spawn_with_handle(repository.create_execution(&commit, "", vec![])).unwrap())
        }
        let excs = handles.into_iter()
            .map(|h| executor.run(h).unwrap())
            .collect::<Vec<_>>();

        thread::sleep_ms(1000);
        for exc in excs{
            assert!(repository.get_executions().unwrap().contains(&exc));
        }

        clean_expe_repo();
        clean_cmp_repo();
    }


    #[test]
    fn test_update_execution(){
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path,
                                         Url::parse("git://localhost:9418/expe_repo").unwrap())
            .unwrap();
        let commit = get_expe_repo_head();
        let create_fut = repository.create_execution(&commit,"", vec![]);
        let exc= block_on(create_fut).unwrap();
        println!("Execution: {:?}", exc);

        let upd = ExecutionUpdate{
            executor: Some("apere-pc".to_owned()),
            execution_date: Some("now".to_owned()),
            execution_duration: Some(50),
            execution_stdout: Some("Some stdout messages".to_owned()),
            execution_stderr: Some("Some stderr messages".to_owned()),
            execution_exit_code: Some(0),
            execution_fitness: Some(50.),
        };

        let update_fut = repository.update_execution(&exc.identifier,&upd);

        let exc_u = block_on(update_fut).unwrap();
        println!("Execution updated: {:?}", exc);
        assert_eq!(exc.identifier, exc_u.identifier);

        thread::sleep_ms(1000);
        assert!(!repository.get_executions().unwrap().contains(&exc));
        assert!(repository.get_executions().unwrap().contains(&exc_u));
        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH))
            .unwrap();
        assert_eq!(exc_u, exc_file);

        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_finish_execution(){
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path,
                                         Url::parse("git://localhost:9418/expe_repo").unwrap())
            .unwrap();
        let commit = get_expe_repo_head();
        let create_fut = repository.create_execution(&commit,"", vec![]);
        let exc= block_on(create_fut).unwrap();
        println!("Execution: {:?}", exc);

        let finish_fut = repository.finish_execution(&exc.identifier);

        let exc_f = block_on(finish_fut).unwrap();

        assert_eq!(exc.identifier, exc_f.identifier);
        assert_ne!(exc, exc_f);

        assert!(exc.get_path().exists());
        assert!(!exc.get_path().join("run.py").exists());

        thread::sleep_ms(1000);
        assert!(repository.get_executions().unwrap().contains(&exc_f));
        assert!(!repository.get_executions().unwrap().contains(&exc));

        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH))
            .unwrap();
        assert_eq!(exc_f, exc_file);

        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_delete_execution(){
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();
        setup_logger();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repository = Repository::new(&repo_path,
                                         Url::parse("git://localhost:9418/expe_repo").unwrap())
            .unwrap();
        let commit = get_expe_repo_head();
        let create_fut = repository.create_execution(&commit,"", vec![]);
        let exc= block_on(create_fut).unwrap();
        println!("Execution: {:?}", exc);

        let delete_fut = repository.delete_execution(&exc.identifier);

        block_on(delete_fut).unwrap();

        assert!(!exc.get_path().exists());

        thread::sleep_ms(1000);
        assert!(!repository.get_executions().unwrap().contains(&exc));

        clean_expe_repo();
        clean_cmp_repo();
    }

}

