// liborchestra/mod.rs
// Author: Alexandre Péré
/// This module contains a `Repository` structure representing a campaign repository. It is built as
/// a `Future` __resource__, i.e. a structure that provides futures, and takes care about waking the
/// tasks when the computation can carry on.
//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use super::{CMPCONF_RPATH, DATA_RPATH, EXCCONF_RPATH, EXCS_RPATH, XPRP_RPATH};
use crate::misc;
use crate::primitives::Error as PrimErr;
use crate::primitives::{
    Dropper, FinishedOperation, Operation, OperationFuture, ProgressingOperation,
    StartingOperation, UseResource,
};
use crate::stateful::Stateful;
use chashmap::CHashMap;
use chrono;
use crossbeam::channel::{bounded, unbounded, Receiver, Sender, TryRecvError};
use crypto::ed25519::exchange;
use futures::future::Future;
use git2;
use regex;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::task::{Poll, Waker};
use std::{error, fmt, fs, io, path, str, thread};
use url::Url;
use uuid;
use uuid::Uuid;

///////////////////////////////////////////////////////////////////////////////////////////// MODULE
pub mod synchro;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
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
    CacheError(String),
    ReadExecution,
    WriteExecution,
    ReadCampaign,
    WriteCampaign,
    WrongPath,
    NoFFPossible,
    Unknown,
    // Branch Errors
    Io(String),
    Git(String),
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
            Error::ChannelBroken(s) => write!(f, "Communication channel broken: \n{}", s),
            Error::FetchExperiment(s) => write!(f, "Failed to fetch experiment: \n{}", s),
            Error::CreateExecution(s) => write!(f, "Failed to create execution: \n{}", s),
            Error::UpdateExecution(s) => write!(f, "Failed to update execution: \n{}", s),
            Error::FinishExecution(s) => write!(f, "Failed to finish execution: \n{}", s),
            Error::FetchExecutions(s) => write!(f, "Failed to fetch executions: \n{}", s),
            Error::DeleteExecution(s) => write!(f, "Failed to delete execution: \n{}", s),
            Error::CacheError(s) => write!(f, "Error occurred with executions chache: \n{}", s),
            Error::ReadExecution => write!(f, "Failed to read execution from file"),
            Error::WriteExecution => write!(f, "Failed to write execution from file"),
            Error::ReadCampaign => write!(f, "Failed to read campaign from file"),
            Error::WriteCampaign => write!(f, "Failed to write campaign from file"),
            Error::WrongPath => write!(f, "Something went wrong with a path"),
            Error::NoFFPossible => write!(f, "Couldn't perform fast-forward pull"),
            Error::Unknown => write!(f, "Unknown error occurred"),
            Error::Io(s) => write!(f, "Io related error occurred: \n{}", s),
            Error::Git(s) => write!(f, "Git related error occurred: \n {}", s),
        }
    }
}

impl From<io::Error> for Error {
    fn from(other: io::Error) -> Error {
        return Error::Io(format!("{}", other));
    }
}

impl From<git2::Error> for Error {
    fn from(other: git2::Error) -> Error {
        return Error::Git(format!("{}", other));
    }
}

impl From<Error> for crate::primitives::Error {
    fn from(other: Error) -> crate::primitives::Error {
        return crate::primitives::Error::Operation(format!("{}", other));
    }
}

///////////////////////////////////////////////////////////////////////////////////// CONFIGURATIONS
/// Represents the configuration of a campaign repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct CampaignConf {
    path: Option<path::PathBuf>,
    synchro: synchro::Synchronizer,
    version: String,
}

impl CampaignConf {
    /// Read campaign from file.
    pub fn from_file(conf_path: &path::PathBuf) -> Result<CampaignConf, crate::Error> {
        debug!(
            "Reading campaign configuration from file {}",
            conf_path.to_str().unwrap()
        );
        let file = fs::File::open(conf_path).map_err(|e| crate::Error::Io(e))?;
        let mut config: CampaignConf = serde_yaml::from_reader(file)?;
        config.path = Some(conf_path.parent().unwrap().to_path_buf().clone());
        return Ok(config);
    }

    /// Writes campaign to file.
    pub fn to_file(&self, path: &path::PathBuf) -> Result<(), crate::Error> {
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
    fn get_executions_from_files(&self) -> Vec<ExecutionConf> {
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
        return write!(f, "Campaign<[{}]>", self.get_path().to_str().unwrap());
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

// Newtype representing a commit string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExperimentCommit(String);
impl fmt::Display for ExperimentCommit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Newtype representing parameters string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionParameters(String);

// Newtype representing tag string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionTag(String);

// Newtype representing an execution identifier.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone, Hash)]
pub struct ExecutionId(Uuid);
impl fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
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
    pub fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error> {
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
        let mut config: ExecutionConf =
            serde_yaml::from_reader(file).map_err(|_| Error::ReadExecution)?;
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
pub struct ExecutionUpdate {
    executor: Option<String>,
    execution_date: Option<String>,
    execution_duration: Option<u32>,
    execution_stdout: Option<String>,
    execution_stderr: Option<String>,
    execution_exit_code: Option<u32>,
    execution_fitness: Option<f64>,
}

impl ExecutionUpdate {
    // Consumes the update and an execution to generate a new execution.
    fn apply(self, conf: ExecutionConf) -> ExecutionConf {
        let mut conf = conf;
        if let Some(s) = self.executor {
            conf.executor = Some(s)
        }
        if let Some(s) = self.execution_date {
            conf.execution_date = Some(s)
        }
        if let Some(s) = self.execution_duration {
            conf.execution_duration = Some(s)
        }
        if let Some(s) = self.execution_stdout {
            conf.execution_stdout = Some(s)
        }
        if let Some(s) = self.execution_stderr {
            conf.execution_stderr = Some(s)
        }
        if let Some(s) = self.execution_exit_code {
            conf.execution_exit_code = Some(s)
        }
        if let Some(s) = self.execution_fitness {
            conf.execution_fitness = Some(s)
        }
        return conf;
    }
}

struct Campaign {
    conf: CampaignConf,
    cache: HashMap<ExecutionId, ExecutionConf>,
    synchro: Box<dyn synchro::SyncRepository>,
}

impl Campaign {
    // Opens a Campaign from a local path.
    fn from(conf: CampaignConf) -> Result<Campaign, Error> {
        debug!("Campaign: Open campaign from conf {}", conf);
        let cache: HashMap<ExecutionId, ExecutionConf> = conf
            .get_executions_from_files()
            .iter()
            .map(|e| (e.identifier.clone(), e.to_owned()))
            .collect();
        let synchro = match &conf.synchro {
            synchro::Synchronizer::Null(null) => Box::new(null.clone()),
        };
        Ok(Campaign {
            conf,
            cache,
            synchro,
        })
    }

    // Opens a new repository at the local path, using the experiment repository url.
    fn new(local_path: &path::PathBuf, experiment_url: Url) -> Result<Campaign, Error> {
        debug!(
            "Campaign: Initializing campaign on experiment {} in {}",
            experiment_url,
            local_path.to_str().unwrap()
        );
        fs::create_dir_all(local_path)?;
        let expe_repository =
            git2::Repository::clone(experiment_url.as_str(), local_path.join(XPRP_RPATH))?;
        let campaign = CampaignConf {
            path: Some(local_path.to_owned()),
            synchro: synchro::Synchronizer::Null(synchro::NullSynchronizer {}),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        fs::create_dir(campaign.get_executions_path())?;
        campaign.to_file(&local_path.join(CMPCONF_RPATH));
        return Campaign::from(campaign);
    }

    fn fetch_experiment(&mut self) -> Result<CampaignConf, Error> {
        debug!("Campaign: Fetch experiment");
        let experiment_repo = git2::Repository::open(self.conf.get_experiment_path()).unwrap();
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
                rh.target().ok_or(Error::FetchExperiment(
                    "Couldn't find origin head target".to_owned(),
                ))
            })
            .and_then(|oid| {
                experiment_repo.find_annotated_commit(oid).map_err(|_| {
                    Error::FetchExperiment("Couldn't find annotated commit".to_owned())
                })
            })?;
        let (analysis, preferences) = experiment_repo
            .merge_analysis(&[&ann_remote_head])
            .map_err(|_| Error::Unknown)?;
        if analysis.is_fast_forward() {
            let tree = experiment_repo
                .find_commit(ann_remote_head.id())
                .map_err(|_| Error::FetchExperiment("Couldn't find commit".to_owned()))?
                .tree()
                .map_err(|_| Error::FetchExperiment("Couldn't find commit tree".to_owned()))?;
            experiment_repo
                .checkout_tree(tree.as_object(), None)
                .map_err(|_| Error::FetchExperiment("Couldn't checkout tree".to_owned()))?;
            experiment_repo
                .find_reference("refs/heads/master")
                .map_err(|_| {
                    Error::FetchExperiment("Couldn't find local master head ref".to_owned())
                })?
                .set_target(ann_remote_head.id(), "fast forward")
                .map_err(|_| Error::FetchExperiment("Couldn't set HEAD to target".to_owned()))?;
            experiment_repo
                .head()
                .map_err(|_| Error::FetchExperiment("Coudln't find repository HEAD".to_owned()))?
                .set_target(ann_remote_head.id(), "fast forward")
                .map_err(|_| {
                    Error::FetchExperiment("Couldn't set reposity HEAD to target".to_owned())
                })?;
            self.synchro.fetch_experiment_hook(&self.conf)?;
            return Ok(self.conf.clone());
        } else {
            return Err(Error::NoFFPossible);
        }
    }

    fn create_execution(
        &mut self,
        commit: ExperimentCommit,
        param: ExecutionParameters,
        tags: Vec<ExecutionTag>,
    ) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Creating Execution");
        let mut exc_conf = ExecutionConf {
            commit,
            parameters: param,
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
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            identifier: ExecutionId(uuid::Uuid::new_v4()),
            tags: tags.clone(),
        };
        exc_conf.path = Some(
            self.conf
                .get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", exc_conf.identifier)),
        );
        fs::create_dir(&exc_conf.get_path())
            .map_err(|_| Error::CreateExecution("Failed to create directory".to_owned()))?;
        let url = format!(
            "file://{}",
            self.conf.get_experiment_path().to_str().unwrap()
        );
        let repo = git2::Repository::clone(&url, exc_conf.get_path())
            .map_err(|_| Error::CreateExecution("Failed to local clone".to_owned()))?;
        let commit_id = git2::Oid::from_str(&format!("{}", exc_conf.commit))
            .map_err(|_| Error::CreateExecution("Ill formed commit".to_owned()))?;
        let commit = repo
            .find_commit(commit_id)
            .map_err(|_| Error::CreateExecution("Commit is not known".to_owned()))?;
        let tree = commit
            .tree()
            .map_err(|_| Error::CreateExecution("No tree attached to commit".to_owned()))?;
        repo.checkout_tree(tree.as_object(), None)
            .map_err(|_| Error::CreateExecution("Couldn't checkout tree".to_owned()))?;
        fs::remove_dir_all(exc_conf.get_path().join(".git"))
            .map_err(|_| Error::CreateExecution("Couldn't remove git folder".to_owned()))?;
        fs::read_dir(exc_conf.get_path())
            .unwrap()
            .map(|r| match r {
                Err(_) => Err(Error::CreateExecution(
                    "Failed in elements gathering".to_owned(),
                )),
                Ok(r) => Ok(exc_conf.experiment_elements.push(r.path())),
            })
            .collect::<Result<Vec<()>, Error>>()?;
        fs::create_dir(exc_conf.get_path().join(DATA_RPATH))
            .map_err(|_| Error::CreateExecution("Failed to create data folder".to_owned()))?;
        exc_conf
            .to_file(&exc_conf.get_path().join(EXCCONF_RPATH))
            .map_err(|_| Error::CreateExecution("Failed to write config file.".to_owned()))?;
        self.synchro.create_execution_hook(&exc_conf)?;
        self.cache
            .insert(exc_conf.identifier.clone(), exc_conf.clone());
        return Ok(exc_conf);
    }

    // Function called by the thread to handle the update execution operation.
    fn update_execution(
        &mut self,
        id: ExecutionId,
        upd: ExecutionUpdate,
    ) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Updating execution {}", id.0);
        let conf_path = self
            .conf
            .get_path()
            .join(EXCS_RPATH)
            .join(format!("{}", id))
            .join(EXCCONF_RPATH);
        let exc_conf = self
            .cache
            .remove(&id)
            .ok_or(Error::UpdateExecution(format!(
                "Tried to remove execution {} from cache but \
                 it was not there.",
                id.0
            )))?;
        let exc_conf = upd.apply(exc_conf);
        exc_conf.to_file(&conf_path)?;
        self.synchro.update_execution_hook(&exc_conf)?;
        self.cache
            .insert(exc_conf.identifier.clone(), exc_conf.clone());
        Ok(exc_conf)
    }

    // Function called by the thread to handle the finishing of an execution.
    fn finish_execution(&mut self, id: ExecutionId) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Finishing Execution {}", id.0);
        let conf_path = self
            .conf
            .get_path()
            .join(EXCS_RPATH)
            .join(format!("{}", id))
            .join(EXCCONF_RPATH);
        let mut exc_conf = self
            .cache
            .remove(&id)
            .ok_or(Error::UpdateExecution(format!(
                "Tried to remove execution {} from cache but \
                 it was not there.",
                id.0
            )))?;
        exc_conf
            .experiment_elements
            .iter()
            .map(|p| exc_conf.get_path().join(p))
            .map(|p| {
                if p.is_file() {
                    fs::remove_file(p)
                } else {
                    fs::remove_dir_all(p)
                }
            })
            .map(|p| {
                p.map_err(|_| {
                    Error::FinishExecution("Failed to remote one experiment element".to_owned())
                })
            })
            .collect::<Result<Vec<()>, Error>>()?;
        exc_conf.state = ExecutionState::Finished;
        exc_conf.to_file(&conf_path)?;
        self.synchro.update_execution_hook(&exc_conf)?;
        self.cache
            .insert(exc_conf.identifier.clone(), exc_conf.clone());
        Ok(exc_conf)
    }

    // Function called by the thread to handle the removal of an execution.
    fn delete_execution(&mut self, id: ExecutionId) -> Result<(), Error> {
        debug!("Campaign: Delete Execution {}", id.0);
        let conf_path = self
            .conf
            .get_path()
            .join(EXCS_RPATH)
            .join(format!("{}", id));
        let exc_conf = self
            .cache
            .remove(&id)
            .ok_or(Error::UpdateExecution(format!(
                "Tried to remove execution {} from cache but \
                 it was not there.",
                id.0
            )))?;
        fs::remove_dir_all(conf_path)
            .map_err(|_| Error::DeleteExecution("Failed to remove execution files".to_owned()))?;
        self.synchro.delete_execution_hook(&exc_conf)?;
        Ok(())
    }

    fn fetch_executions(&mut self) -> Result<Vec<ExecutionConf>, Error> {
        debug!("Campaign: Fetching Executions");
        let before: HashSet<path::PathBuf> = fs::read_dir(self.conf.get_executions_path())
            .unwrap()
            .map(|p| p.unwrap().path())
            .filter(|p| p.join(EXCCONF_RPATH).exists())
            .collect();
        self.synchro.fetch_executions_hook(&self.conf)?;
        let after: HashSet<path::PathBuf> = fs::read_dir(self.conf.get_executions_path())
            .unwrap()
            .map(|p| p.unwrap().path())
            .filter(|p| p.join(EXCCONF_RPATH).exists())
            .collect();
        if before.difference(&after).next().is_some() {
            Err(Error::FetchExecutions(
                "Some executions existed before fetch but not after".to_owned(),
            ))
        } else {
            after
                .difference(&before)
                .map(|p| ExecutionConf::from_file(&self.conf.get_executions_path().join(p)))
                .collect::<Result<Vec<ExecutionConf>, Error>>()
        }
    }

    fn get_executions(&self) -> Result<Vec<ExecutionConf>, Error> {
        debug!("Campaign: Getting executions");
        Ok(self.cache.iter().map(|(id, conf)| conf.clone()).collect())
    }
}

/////////////////////////////////////////////////////////////////////////////////////////// RESOURCE

// The type alias for the Repository resource operations.
type CampaignOp = Box<dyn UseResource<CampaignResource>>;

struct CampaignResource {
    campaign: Campaign,
    queue: Vec<CampaignOp>,
}

#[derive(Clone)]
pub struct CampaignResourceHandle {
    sender: Sender<CampaignOp>,
    dropper: Dropper<()>,
}

impl CampaignResourceHandle {
    // This function spawns the thread that will handle all the repository operations.
    pub fn spawn_resource(camp_conf: CampaignConf) -> Result<CampaignResourceHandle, Error> {
        debug!("RepositoryResourceHandle: Start Repository Thread");
        let campaign = Campaign::from(camp_conf)?;
        let (sender, receiver): (Sender<CampaignOp>, Receiver<CampaignOp>) = unbounded();
        let handle = thread::spawn(move || {
            trace!("RepositoryResource: Creating resource in thread");
            let mut res = CampaignResource {
                campaign,
                queue: Vec::new(),
            };
            trace!("RepositoryResource: Starting resource loop");
            loop {
                // Handle a message
                match receiver.try_recv() {
                    Ok(o) => {
                        trace!("RepositoryResource: Received operation");
                        res.queue.push(o)
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        trace!("RepositoryResource: Channel disconnected. Leaving...");
                        break;
                    }
                }
                // Handle an operation
                if let Some(s) = res.queue.pop() {
                    s.progress(&mut res);
                }
            }
            trace!("RepositoryResource: Operations channel disconnected. Leaving thread.");
            return ();
        });
        return Ok(CampaignResourceHandle {
            sender,
            dropper: Dropper::from_handle(handle),
        });
    }

    /// Async method, returning a future that ultimately resolves in a campaign, after having
    /// fetched the origin changes on the experiment repository.
    pub fn async_fetch_experiment(&self) -> FetchExperimentFuture {
        let (recv, op) = FetchExperimentOp::from(Stateful::from(StartingOperation(())));
        return FetchExperimentFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration,
    /// after it has been created.
    pub fn async_create_execution(
        &self,
        commit: &ExperimentCommit,
        parameters: &ExecutionParameters,
        tags: Vec<&ExecutionTag>,
    ) -> CreateExecutionFuture {
        let (recv, op) = CreateExecutionOp::from(Stateful::from(StartingOperation((
            commit.to_owned(),
            parameters.to_owned(),
            tags.into_iter().map(|a| a.to_owned()).collect::<Vec<_>>(),
        ))));
        return CreateExecutionFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in the new configuration after it
    /// was updated.
    pub fn async_update_execution(
        &self,
        id: &ExecutionId,
        upd: &ExecutionUpdate,
    ) -> UpdateExecutionFuture {
        let (recv, op) = UpdateExecutionOp::from(Stateful::from(StartingOperation((
            id.to_owned(),
            upd.to_owned(),
        ))));
        return UpdateExecutionFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn async_finish_execution(&self, id: &ExecutionId) -> FinishExecutionFuture {
        let (recv, op) = FinishExecutionOp::from(Stateful::from(StartingOperation(id.to_owned())));
        return FinishExecutionFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in an empty type after it was
    /// finished.
    pub fn async_delete_execution(&self, id: &ExecutionId) -> DeleteExecutionFuture {
        let (recv, op) = DeleteExecutionOp::from(Stateful::from(StartingOperation(id.to_owned())));
        return DeleteExecutionFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn async_fetch_executions(&self) -> FetchExecutionsFuture {
        let (recv, op) = FetchExecutionsOp::from(Stateful::from(StartingOperation(())));
        return FetchExecutionsFuture::new(op, self.sender.clone(), recv);
    }

    /// Async method, returning a future that ultimately resolves in a vector of execution conf.
    pub fn async_get_executions(&self) -> GetExecutionsFuture {
        let (recv, op) = GetExecutionsOp::from(Stateful::from(StartingOperation(())));
        return GetExecutionsFuture::new(op, self.sender.clone(), recv);
    }
}

///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS
struct FetchExperimentMarker {}
type FetchExperimentOp = Operation<FetchExperimentMarker>;
impl UseResource<CampaignResource> for FetchExperimentOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self.state.to_state::<StartingOperation<()>>() {
            trace!("FetchExperimentOp: Found Starting");
            let result = resource.campaign.fetch_experiment();
            self.state
                .transition::<StartingOperation<()>, FinishedOperation<_>>(FinishedOperation(
                    result.map_err(PrimErr::from),
                ));
            resource.queue.push(self);
        } else if let Some(s) = self.state.to_state::<FinishedOperation<CampaignConf>>() {
            trace!("FetchExperimentOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with FetchExperimentOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("FetchExperimentOp: Sending...");
            sender.send(*self);
            trace!("FetchExperimentOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("FetchExperimentOp: Progress over.");
    }
}
pub type FetchExperimentFuture =
    OperationFuture<FetchExperimentMarker, CampaignResource, CampaignConf>;

struct CreateExecutionMarker {}
type CreateExecutionOp = Operation<CreateExecutionMarker>;
type CreateExecutionInput = (ExperimentCommit, ExecutionParameters, Vec<ExecutionTag>);
impl UseResource<CampaignResource> for CreateExecutionOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self
            .state
            .to_state::<StartingOperation<CreateExecutionInput>>()
        {
            trace!("CreateExecutionOp: Found Starting");
            let result = resource
                .campaign
                .create_execution((s.0).0, (s.0).1, (s.0).2);
            self.state
                .transition::<StartingOperation<CreateExecutionInput>, FinishedOperation<_>>(
                    FinishedOperation(result.map_err(PrimErr::from)),
                );
            resource.queue.push(self);
        } else if let Some(s) = self.state.to_state::<FinishedOperation<ExecutionConf>>() {
            trace!("CreateExecutionOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with CreateExecutionOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("CreateExecutionOp: Sending...");
            sender.send(*self);
            trace!("CreateExecutionOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("CreateExecutionOp: Progress over.");
    }
}
pub type CreateExecutionFuture =
    OperationFuture<CreateExecutionMarker, CampaignResource, ExecutionConf>;

struct UpdateExecutionMarker {}
type UpdateExecutionOp = Operation<UpdateExecutionMarker>;
type UpdateExecutionInput = (ExecutionId, ExecutionUpdate);
impl UseResource<CampaignResource> for UpdateExecutionOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self
            .state
            .to_state::<StartingOperation<UpdateExecutionInput>>()
        {
            trace!("UpdateExecutionOp: Found Starting");
            let result = resource.campaign.update_execution((s.0).0, (s.0).1);
            self.state
                .transition::<StartingOperation<UpdateExecutionInput>, FinishedOperation<_>>(
                    FinishedOperation(result.map_err(PrimErr::from)),
                );
            resource.queue.push(self);
        } else if let Some(s) = self.state.to_state::<FinishedOperation<ExecutionConf>>() {
            trace!("UpdateExecutionOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with UpdateExecutionOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("UpdateExecutionOp: Sending...");
            sender.send(*self);
            trace!("UpdateExecutionOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("UpdateExecutionOp: Progress over.");
    }
}
pub type UpdateExecutionFuture =
    OperationFuture<UpdateExecutionMarker, CampaignResource, ExecutionConf>;

struct FinishExecutionMarker {}
type FinishExecutionOp = Operation<FinishExecutionMarker>;
impl UseResource<CampaignResource> for FinishExecutionOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self.state.to_state::<StartingOperation<ExecutionId>>() {
            trace!("FinishExecutionOp: Found Starting");
            let result = resource.campaign.finish_execution(s.0);
            self.state
                .transition::<StartingOperation<ExecutionId>, FinishedOperation<_>>(
                    FinishedOperation(result.map_err(PrimErr::from)),
                );
            resource.queue.push(self);
        } else if let Some(s) = self.state.to_state::<FinishedOperation<ExecutionConf>>() {
            trace!("FinishExecutionOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with FinishExecutionOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("FinishExecutionOp: Sending...");
            sender.send(*self);
            trace!("FinishExecutionOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("FinishExecutionOp: Progress over.");
    }
}
pub type FinishExecutionFuture =
    OperationFuture<FinishExecutionMarker, CampaignResource, ExecutionConf>;

struct DeleteExecutionMarker {}
type DeleteExecutionOp = Operation<DeleteExecutionMarker>;
impl UseResource<CampaignResource> for DeleteExecutionOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self.state.to_state::<StartingOperation<ExecutionId>>() {
            trace!("DeleteExecutionOp: Found Starting");
            let result = resource.campaign.delete_execution(s.0);
            self.state
                .transition::<StartingOperation<ExecutionId>, FinishedOperation<_>>(
                    FinishedOperation(result.map_err(PrimErr::from)),
                );
            resource.queue.push(self);
        } else if let Some(s) = self.state.to_state::<FinishedOperation<()>>() {
            trace!("DeleteExecutionOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with DeleteExecutionOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("DeleteExecutionOp: Sending...");
            sender.send(*self);
            trace!("DeleteExecutionOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("DeleteExecutionOp: Progress over.");
    }
}
pub type DeleteExecutionFuture = OperationFuture<DeleteExecutionMarker, CampaignResource, ()>;

struct FetchExecutionsMarker {}
type FetchExecutionsOp = Operation<FetchExecutionsMarker>;
impl UseResource<CampaignResource> for FetchExecutionsOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self.state.to_state::<StartingOperation<()>>() {
            trace!("FetchExecutionsOp: Found Starting");
            let result = resource.campaign.fetch_executions();
            self.state
                .transition::<StartingOperation<()>, FinishedOperation<_>>(FinishedOperation(
                    result.map_err(PrimErr::from),
                ));
            resource.queue.push(self);
        } else if let Some(s) = self
            .state
            .to_state::<FinishedOperation<Vec<ExecutionConf>>>()
        {
            trace!("FetchExecutionsOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with FetchExecutionsOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("FetchExecutionsOp: Sending...");
            sender.send(*self);
            trace!("FetchExecutionsOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("FetchExecutionsOp: Progress over.");
    }
}
pub type FetchExecutionsFuture =
    OperationFuture<FetchExecutionsMarker, CampaignResource, Vec<ExecutionConf>>;

struct GetExecutionsMarker {}
type GetExecutionsOp = Operation<GetExecutionsMarker>;
impl UseResource<CampaignResource> for GetExecutionsOp {
    fn progress(mut self: Box<Self>, resource: &mut CampaignResource) {
        if let Some(s) = self.state.to_state::<StartingOperation<()>>() {
            trace!("GetExecutionsOp: Found Starting");
            let result = resource.campaign.get_executions();
            self.state
                .transition::<StartingOperation<()>, FinishedOperation<_>>(FinishedOperation(
                    result.map_err(PrimErr::from),
                ));
            resource.queue.push(self);
        } else if let Some(s) = self
            .state
            .to_state::<FinishedOperation<Vec<ExecutionConf>>>()
        {
            trace!("GetExecutionsOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with GetExecutionsOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("GetExecutionsOp: Sending...");
            sender.send(*self);
            trace!("GetExecutionsOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("GetExecutionsOp: Progress over.");
    }
}
pub type GetExecutionsFuture =
    OperationFuture<GetExecutionsMarker, CampaignResource, Vec<ExecutionConf>>;

////////////////////////////////////////////////////////////////////////////////////////////// TESTS
#[cfg(test)]
mod tests {

    use super::*;
    use std::io::prelude::*;
    use std::{fs, process};

    fn init_logger() {
        std::env::set_var("RUST_LOG", "liborchestra::repository=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    fn setup_expe_repo() {
        fs::create_dir_all("/tmp/expe_repo").unwrap();
        let mut file = fs::File::create("/tmp/expe_repo/run.py").unwrap();
        file.write_all(b"#!/usr/bin/env python\nimport time\ntime.sleep(2)")
            .unwrap();
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
            .args(&["daemon", "--reuseaddr", "--base-path=/tmp", "--export-all"])
            .spawn()
            .expect("Failed to start git server");
        std::thread::sleep_ms(1000);
        if let Ok(Some(o)) = server.try_wait() {
            panic!("Server did not start");
        }
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
        process::Command::new("killall")
            .args(&["git-daemon"])
            .output()
            .unwrap();
        fs::remove_dir_all("/tmp/expe_repo");
    }

    fn clean_cmp_repo() {
        fs::remove_dir_all("/tmp/cmp_repo");
    }

    #[test]
    fn test_new_repository() {
        init_logger();
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        );
        assert!(path::PathBuf::from("/tmp/cmp_repo/.cmpconf").is_file());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp").is_dir());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp/.git").is_dir());
        assert!(path::PathBuf::from("/tmp/cmp_repo/xprp/run.py").is_file());
        assert!(path::PathBuf::from("/tmp/cmp_repo/excs").is_dir());

        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_create_execution() {
        init_logger();
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
        .unwrap();
        let repo = CampaignResourceHandle::spawn_resource(repo.conf).unwrap();
        let commit = get_expe_repo_head();
        let exc = block_on(repo.async_create_execution(
            &ExperimentCommit(commit.clone()),
            &ExecutionParameters("".to_owned()),
            vec![],
        ))
        .unwrap();
        println!("Result: {:?}", exc);
        assert_eq!(exc.commit, ExperimentCommit(commit.clone()));
        assert_eq!(exc.parameters, ExecutionParameters("".to_owned()));
        assert!(exc.tags.is_empty());
        thread::sleep_ms(1000);
        let executions = block_on(repo.async_get_executions());
        assert!(executions.unwrap().contains(&exc));
        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH)).unwrap();
        assert_eq!(exc, exc_file);
        clean_expe_repo();
        clean_cmp_repo();
    }

     #[test]
     fn test_stress_create_execution() {
         init_logger();
         clean_expe_repo();
         clean_cmp_repo();
         setup_expe_repo();

         use futures::executor::block_on;

         let repo_path = path::PathBuf::from("/tmp/cmp_repo");
         let repo = Campaign::new(
             &repo_path,
             Url::parse("git://localhost:9418/expe_repo").unwrap(),
         )
         .unwrap();
         let repo = CampaignResourceHandle::spawn_resource(repo.conf).unwrap();

         use futures::task::SpawnExt;
         let mut executor = futures::executor::ThreadPool::new().unwrap();
         let mut handles = Vec::new();
         let commit = get_expe_repo_head();

         for i in (1..200) {
             handles.push(
                 executor
                     .spawn_with_handle(repo.async_create_execution(
                         &ExperimentCommit(commit.clone()),
                         &ExecutionParameters("".to_owned()),
                         vec![],
                     ))
                     .unwrap(),
             )
         }
         let excs = handles
             .into_iter()
             .map(|h| executor.run(h).unwrap())
             .collect::<Vec<_>>();

         thread::sleep_ms(1000);
         let executions = block_on(repo.async_get_executions()).unwrap();
         for exc in excs {
             assert!(executions.contains(&exc));
         }

         clean_expe_repo();
         clean_cmp_repo();
     }


    #[test]
    fn test_update_execution() {
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
        .unwrap();
        let repo = CampaignResourceHandle::spawn_resource(repo.conf).unwrap();
        let commit = get_expe_repo_head();
        let exc = block_on(repo.async_create_execution(
            &ExperimentCommit(commit.clone()),
            &ExecutionParameters("".to_owned()),
            vec![],
        ))
        .unwrap();
        println!("Execution: {:?}", exc);
        let upd = ExecutionUpdate {
            executor: Some("apere-pc".to_owned()),
            execution_date: Some("now".to_owned()),
            execution_duration: Some(50),
            execution_stdout: Some("Some stdout messages".to_owned()),
            execution_stderr: Some("Some stderr messages".to_owned()),
            execution_exit_code: Some(0),
            execution_fitness: Some(50.),
        };

        let exc_u = block_on(repo.async_update_execution(&exc.identifier, &upd)).unwrap();
        println!("Execution updated: {:?}", exc);
        assert_eq!(exc.identifier, exc_u.identifier);

        thread::sleep_ms(1000);
        let executions = block_on(repo.async_get_executions()).unwrap();
        assert!(!executions.contains(&exc));
        assert!(executions.contains(&exc_u));
        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH)).unwrap();
        assert_eq!(exc_u, exc_file);

        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_finish_execution() {
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
        .unwrap();
        let repo = CampaignResourceHandle::spawn_resource(repo.conf).unwrap();
        let commit = get_expe_repo_head();
        let exc = block_on(repo.async_create_execution(
            &ExperimentCommit(commit.clone()),
            &ExecutionParameters("".to_owned()),
            vec![],
        ))
        .unwrap();
        println!("Execution: {:?}", exc);

        let exc_f = block_on(repo.async_finish_execution(&exc.identifier)).unwrap();

        assert_eq!(exc.identifier, exc_f.identifier);
        assert_ne!(exc, exc_f);

        assert!(exc.get_path().exists());
        assert!(!exc.get_path().join("run.py").exists());

        thread::sleep_ms(1000);
        let executions = block_on(repo.async_get_executions()).unwrap();
        assert!(!executions.contains(&exc));
        assert!(executions.contains(&exc_f));

        let exc_file = ExecutionConf::from_file(&exc.get_path().join(EXCCONF_RPATH)).unwrap();
        assert_eq!(exc_f, exc_file);

        clean_expe_repo();
        clean_cmp_repo();
    }

    #[test]
    fn test_delete_execution() {
        clean_expe_repo();
        clean_cmp_repo();
        setup_expe_repo();

        use futures::executor::block_on;

        let repo_path = path::PathBuf::from("/tmp/cmp_repo");
        let repo = Campaign::new(
            &repo_path,
            Url::parse("git://localhost:9418/expe_repo").unwrap(),
        )
        .unwrap();
        let repo = CampaignResourceHandle::spawn_resource(repo.conf).unwrap();
        let commit = get_expe_repo_head();
        let exc = block_on(repo.async_create_execution(
            &ExperimentCommit(commit.clone()),
            &ExecutionParameters("".to_owned()),
            vec![],
        ))
        .unwrap();
        println!("Execution: {:?}", exc);

        block_on(repo.async_delete_execution(&exc.identifier)).unwrap();

        assert!(!exc.get_path().exists());

        thread::sleep_ms(1000);
        let executions = block_on(repo.async_get_executions()).unwrap();
        assert!(!executions.contains(&exc));

        clean_expe_repo();
        clean_cmp_repo();
    }

}
