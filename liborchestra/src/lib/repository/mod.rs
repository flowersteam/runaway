//! liborchestra/repository/mod.rs
//! 
//! This module contains a `Campaign` structure representing a campaign repository, and providing the
//! main methods to act on it. An asynchronous interface `CampaignResourceHandle` allows to act on
//! the repository using futures. It communicates with a `CampaignResource` that manages the operations
//! executions on the actual `Campaign`. If you have difficulties with the asynchronous design, check
//! the primitives module.


//------------------------------------------------------------------------------------------ IMPORTS


use super::{CMPCONF_RPATH, DATA_RPATH, EXCCONF_RPATH, EXCS_RPATH, XPRP_RPATH};
use crate::misc;
use crate::commons::Dropper;
use chrono::prelude::*;
use git2;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use std::{error, fmt, fs, io, path, str, thread};
use std::sync::Arc;
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future::Future;
use futures::prelude::*;
use futures::task::LocalSpawnExt;
use futures::lock::Mutex;
use url::Url;
use uuid;
use uuid::Uuid;
use walkdir::WalkDir;


//------------------------------------------------------------------------------------------- MODULE


pub mod synchro;


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    NotARepo,
    AlreadyRepo,
    InvalidRepo,
    InvalidExpeCommit,
    NoOutputAvailable,
    Channel(String),
    FetchExperiment(String),
    CreateExecution(String),
    UpdateExecution(String),
    FinishExecution(String),
    FetchExecutions(String),
    DeleteExecution(String),
    CacheError(String),
    OperationFetch(String),
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
            Error::Channel(s) => write!(f, "Communication channel error: \n{}", s),
            Error::FetchExperiment(s) => write!(f, "Failed to fetch experiment: \n{}", s),
            Error::CreateExecution(s) => write!(f, "Failed to create execution: \n{}", s),
            Error::UpdateExecution(s) => write!(f, "Failed to update execution: \n{}", s),
            Error::FinishExecution(s) => write!(f, "Failed to finish execution: \n{}", s),
            Error::FetchExecutions(s) => write!(f, "Failed to fetch executions: \n{}", s),
            Error::DeleteExecution(s) => write!(f, "Failed to delete execution: \n{}", s),
            Error::CacheError(s) => write!(f, "Error occurred with executions chache: \n{}", s),
            Error::OperationFetch(s) => write!(f, "Error occurred when fetching the operation: \n{}", s),
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

impl From<Error> for crate::commons::Error {
    fn from(other: Error) -> crate::commons::Error {
        return crate::commons::Error::Operation(format!("{}", other));
    }
}


//----------------------------------------------------------------------------------- CONFIGURATIONS


/// Represents the configuration of a campaign repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct CampaignConf {
    path: Option<path::PathBuf>,
    synchro: synchro::Synchronizer,
    version: String,
}

impl CampaignConf {
    /// Reads campaign from file.
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

    /// Gets a list of executions from the available files (Slow).
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
    Running,
    Failed,
    Completed,
}

impl<'a> From<&'a str> for ExecutionState {
    fn from(s: &str) -> ExecutionState {
        match s {
            "initialized" => ExecutionState::Initialized,
            "running" => ExecutionState::Running,
            "failed" => ExecutionState::Failed,
            "completed" => ExecutionState::Completed,
            e => panic!("Unknown state {}", e),
        }
    }
}

impl fmt::Display for ExecutionState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecutionState::Initialized => write!(f, "Initialized"),
            ExecutionState::Running => write!(f, "Running"),
            ExecutionState::Failed => write!(f, "Interrupted"),
            ExecutionState::Completed => write!(f, "Completed"),
        }
    }
}

/// Newtype representing a commit string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExperimentCommit(pub String);
impl fmt::Display for ExperimentCommit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype representing parameters string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionParameters(pub String);
impl fmt::Display for ExecutionParameters {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype representing tag string.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct ExecutionTag(pub String);
impl fmt::Display for ExecutionTag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype representing an execution identifier.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone, Hash)]
pub struct ExecutionId(pub Uuid);
impl fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Represents the configuration and results of an execution of the experiment.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ExecutionConf {
    pub identifier: ExecutionId,
    pub path: Option<path::PathBuf>,
    pub commit: ExperimentCommit,
    pub parameters: ExecutionParameters,
    pub state: ExecutionState,
    pub experiment_elements: Vec<path::PathBuf>,
    pub executor: Option<String>,
    pub execution_beginning_date: Option<DateTime<Utc>>,
    pub execution_ending_date: Option<DateTime<Utc>>,
    pub execution_stdout: Option<String>,
    pub execution_stderr: Option<String>,
    pub execution_message: Option<String>,
    pub execution_exit_code: Option<i32>,
    pub execution_features: Option<Vec<f64>>,
    pub generator: String,
    pub generation_date: DateTime<Utc>,
    pub tags: Vec<ExecutionTag>,
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

    /// Reads execution from file
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
        write!(f, "{}", self.identifier)
    }
}

/// Represents the update of an execution.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ExecutionUpdate {
    state: Option<ExecutionState>,
    executor: Option<String>,
    execution_stdout: Option<String>,
    execution_stderr: Option<String>,
    execution_exit_code: Option<i32>,
    execution_features: Option<Vec<f64>>,
    execution_message: Option<String>,
    execution_beginning_date: Option<DateTime<Utc>>,
    execution_ending_date: Option<DateTime<Utc>>,
}

impl ExecutionUpdate {
    /// Consumes the update and an execution to generate a new execution.
    fn apply(self, conf: ExecutionConf) -> ExecutionConf {
        let mut conf = conf;
        if let Some(s) = self.state {
            conf.state = s
        }
        if let Some(s) = self.executor {
            conf.executor = Some(s)
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
        if let Some(s) = self.execution_features {
            conf.execution_features = Some(s)
        }
        if let Some(s) = self.execution_message {
            conf.execution_message = Some(s)
        }
        if let Some(s) = self.execution_beginning_date {
            conf.execution_beginning_date = Some(s)
        }
        if let Some(s) = self.execution_ending_date {
            conf.execution_ending_date = Some(s)
        }
        return conf;
    }
}

/// A structure to build Execution updates
pub struct ExecutionUpdateBuilder(ExecutionUpdate);

impl ExecutionUpdateBuilder {
    /// Creates a new update.
    pub fn new() -> ExecutionUpdateBuilder {
        ExecutionUpdateBuilder(ExecutionUpdate {
            state: None,
            executor: None,
            execution_stdout: None,
            execution_stderr: None,
            execution_message: None,
            execution_exit_code: None,
            execution_features: None,
            execution_beginning_date: None,
            execution_ending_date: None,
        })
    }

    /// Sets the state
    pub fn state(mut self, e: ExecutionState) -> Self {
        self.0.state = Some(e);
        return self;
    }
    /// Sets the executor
    pub fn executor(mut self, e: String) -> Self {
        self.0.executor = Some(e);
        return self;
    }

    /// Sets the execution stdout
    pub fn stdout(mut self, d: String) -> Self {
        self.0.execution_stdout = Some(d);
        return self;
    }

    /// Sets the execution stderr
    pub fn stderr(mut self, d: String) -> Self {
        self.0.execution_stderr = Some(d);
        return self;
    }

    /// Sets the execution message
    pub fn message(mut self, m: String) -> Self {
        self.0.execution_message = Some(m);
        return self;
    }

    /// Sets the execution exit code
    pub fn exit_code(mut self, m: i32) -> Self {
        self.0.execution_exit_code = Some(m);
        return self;
    }

    /// Sets the execution features
    pub fn features(mut self, m: Vec<f64>) -> Self {
        self.0.execution_features = Some(m);
        return self;
    }

    /// Sets the beginning time
    pub fn beginning_date(mut self, m: DateTime<Utc>) -> Self {
        self.0.execution_beginning_date = Some(m);
        return self;
    }

    /// Sets the end time
    pub fn ending_date(mut self, m: DateTime<Utc>) -> Self {
        self.0.execution_ending_date = Some(m);
        return self;
    }

    /// Returns the execution update
    pub fn build(self) -> ExecutionUpdate {
        return self.0;
    }
}


//----------------------------------------------------------------------------------------- CAMPAIGN

/// The inner synchronous resource. Contains the different basic methods to manipulate a repository.
pub struct Campaign {
    pub conf: CampaignConf,
    cache: HashMap<ExecutionId, ExecutionConf>,
    synchro: Box<dyn synchro::SyncRepository>,
}

impl Campaign {
    
    /// Opens a Campaign from a local path.
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

    /// Opens a new repository at the local path, using the experiment repository url.
    pub fn new(local_path: &path::PathBuf, experiment_url: Url) -> Result<Campaign, Error> {
        debug!(
            "Campaign: Initializing campaign on experiment {} in {}",
            experiment_url,
            local_path.to_str().unwrap()
        );
        fs::create_dir_all(local_path)?;
        git2::Repository::clone(experiment_url.as_str(), local_path.join(XPRP_RPATH))?;
        let campaign = CampaignConf {
            path: Some(local_path.to_owned()),
            synchro: synchro::Synchronizer::Null(synchro::NullSynchronizer {}),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        fs::create_dir(campaign.get_executions_path())?;
        campaign.to_file(&local_path.join(CMPCONF_RPATH)).unwrap();
        return Campaign::from(campaign);
    }

    /// Fetches the last experiment from its remote repository.
    async fn fetch_experiment(cmp: Arc<Mutex<Campaign>>) -> Result<CampaignConf, Error> {
        debug!("Campaign: Fetch experiment");
        let experiment_repo = git2::Repository::open({cmp.lock().await.conf.get_experiment_path()}).unwrap();
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
        let (analysis, _) = experiment_repo
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
            {
                let cmp = cmp.lock().await;
                cmp.synchro.fetch_experiment_hook(&cmp.conf)?;
                return Ok(cmp.conf.clone());
            };
        } else {
            return Err(Error::NoFFPossible);
        }
    }

    /// Creates a new execution.
    async fn create_execution(
        cmp: Arc<Mutex<Campaign>>,
        commit: ExperimentCommit,
        param: ExecutionParameters,
        tags: Vec<ExecutionTag>,
    ) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Creating Execution");
        let mut exc_conf = ExecutionConf {
            commit,
            execution_message: None,
            parameters: param,
            state: ExecutionState::Initialized,
            path: None,
            experiment_elements: vec![],
            executor: None,
            execution_beginning_date: None,
            execution_ending_date: None,
            execution_stdout: None,
            execution_stderr: None,
            execution_exit_code: None,
            execution_features: None,
            generator: misc::get_hostname().unwrap(),
            generation_date: Utc::now(),
            identifier: ExecutionId(uuid::Uuid::new_v4()),
            tags: tags.clone(),
        };
        exc_conf.path = Some(
            {
                cmp.lock()
                    .await
                    .conf
                    .get_path()
                    .join(EXCS_RPATH)
                    .join(format!("{}", exc_conf.identifier))
            }
        );
        fs::create_dir(&exc_conf.get_path())
            .map_err(|_| Error::CreateExecution("Failed to create directory".to_owned()))?;
        let url = format!(
            "file://{}",
            {cmp.lock().await.conf.get_experiment_path().to_str().unwrap()}
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
        let path = exc_conf.get_path();
        WalkDir::new(&path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.metadata().unwrap().is_file())
            .map(|e| e.path().strip_prefix(&path).unwrap().to_path_buf())
            .map(|e| exc_conf.experiment_elements.push(e))
            .for_each(|_| {});
        fs::create_dir(exc_conf.get_path().join(DATA_RPATH))
            .map_err(|_| Error::CreateExecution("Failed to create data folder".to_owned()))?;
        exc_conf
            .to_file(&exc_conf.get_path().join(EXCCONF_RPATH))
            .map_err(|_| Error::CreateExecution("Failed to write config file.".to_owned()))?;
        {cmp.lock().await.synchro.create_execution_hook(&exc_conf)?};
        {
            cmp.lock()
                .await
                .cache
                .insert(exc_conf.identifier.clone(), exc_conf.clone())
        };
        return Ok(exc_conf);
    }

    /// Updates an execution.
    async fn update_execution(
        cmp: Arc<Mutex<Campaign>>,
        id: ExecutionId,
        upd: ExecutionUpdate,
    ) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Updating execution {}", id.0);
        let conf_path = 
        {
            cmp.lock()
                .await
                .conf
                .get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id))
                .join(EXCCONF_RPATH)
        };
        let exc_conf = 
        {
            cmp.lock()
                .await
                .cache
                .remove(&id)
                .ok_or(Error::UpdateExecution(format!(
                    "Tried to remove execution {} from cache but \
                    it was not there.",id.0)))?
        };
        let exc_conf = upd.apply(exc_conf);
        exc_conf.to_file(&conf_path)?;
        {cmp.lock().await.synchro.update_execution_hook(&exc_conf)?};
        {cmp.lock().await.cache.insert(exc_conf.identifier.clone(), exc_conf.clone())};
        Ok(exc_conf)
    }

    /// Finishes an execution.
    async fn finish_execution(cmp: Arc<Mutex<Campaign>>, id: ExecutionId) -> Result<ExecutionConf, Error> {
        debug!("Campaign: Finishing Execution {}", id.0);
        let conf_path = 
        {
            cmp.lock()
                .await
                .conf
                .get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id))
                .join(EXCCONF_RPATH)
        };
        let mut exc_conf = 
        {
            cmp.lock()
                .await
                .cache
                .remove(&id)
                .ok_or(Error::UpdateExecution(format!(
                    "Tried to remove execution {} from cache but \
                    it was not there.",id.0)))?
        };
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
        exc_conf.state = ExecutionState::Completed;
        exc_conf.to_file(&conf_path)?;
        {cmp.lock().await.synchro.finish_execution_hook(&exc_conf)?};
        {cmp.lock().await.cache.insert(exc_conf.identifier.clone(), exc_conf.clone())};
        Ok(exc_conf)
    }

    /// Deletes an execution.
    async fn delete_execution(cmp: Arc<Mutex<Campaign>>, id: ExecutionId) -> Result<(), Error> {
        debug!("Campaign: Delete Execution {}", id.0);
        let conf_path = 
        {
            cmp.lock()
                .await
                .conf
                .get_path()
                .join(EXCS_RPATH)
                .join(format!("{}", id))
        };
        let exc_conf = 
        { 
            cmp.lock()
                .await
                .cache
                .remove(&id)
                .ok_or(Error::UpdateExecution(format!("Tried to remove execution {} from cache but \
                    it was not there.", id.0)))?
        };
        fs::remove_dir_all(conf_path)
            .map_err(|_| Error::DeleteExecution("Failed to remove execution files".to_owned()))?;
        {cmp.lock().await.synchro.delete_execution_hook(&exc_conf)?};
        Ok(())
    }

    /// Fetches possibly distant executions. Fetches executions or not depending on the synchronizer.
    async fn fetch_executions(cmp: Arc<Mutex<Campaign>>) -> Result<Vec<ExecutionConf>, Error> {
        debug!("Campaign: Fetching Executions");
        let exc_path = {cmp.lock().await.conf.get_executions_path()};
        let before: HashSet<path::PathBuf> = fs::read_dir(exc_path.clone())
            .unwrap()
            .map(|p| p.unwrap().path())
            .filter(|p| p.join(EXCCONF_RPATH).exists())
            .collect();
        {
            let cmp = cmp.lock().await;
            cmp.synchro.fetch_executions_hook(&cmp.conf)?;
        }
        let after: HashSet<path::PathBuf> = fs::read_dir(exc_path.clone())
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
                .map(|_| ExecutionConf::from_file(&exc_path))
                .collect::<Result<Vec<ExecutionConf>, Error>>()
        }
    }

    /// Returns a list of the executions.
    async fn get_executions(cmp: Arc<Mutex<Campaign>>) -> Result<Vec<ExecutionConf>, Error> {
        debug!("Campaign: Getting executions");
        let cmp = cmp.lock().await;
        Ok(cmp.cache.iter().map(|(_, conf)| conf.clone()).collect())
    }
}


//------------------------------------------------------------------------------------------- HANDLE


#[derive(Debug)]
enum OperationInput{
    FetchExperiment,
    CreateExecution(ExperimentCommit, ExecutionParameters, Vec<ExecutionTag>),
    UpdateExecution(ExecutionId, ExecutionUpdate),
    FinishExecution(ExecutionId),
    DeleteExecution(ExecutionId),
    FetchExecutions,
    GetExecutions,
}

#[derive(Debug)]
enum OperationOutput{
    FetchExperiment(Result<CampaignConf, Error>),
    CreateExecution(Result<ExecutionConf, Error>),
    UpdateExecution(Result<ExecutionConf, Error>),
    FinishExecution(Result<ExecutionConf, Error>),
    DeleteExecution(Result<(), Error>),
    FetchExecutions(Result<Vec<ExecutionConf>, Error>),
    GetExecutions(Result<Vec<ExecutionConf>, Error>),
}

/// Asynchronous handle to the campaign resource. Allows to perform operations on the campaign, in
/// an asynchronous fashion.
#[derive(Clone)]
pub struct CampaignHandle {
    _sender: mpsc::UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    _dropper: Dropper,
}

impl CampaignHandle {
    /// This function spawns the thread that will handle all the repository operations using the
    /// CampaignResource, and returns a handle to it.
    pub fn spawn(camp_conf: CampaignConf) -> Result<CampaignHandle, Error> {
        debug!("CampaignHandle: Start campaign thread");
        let campaign = Campaign::from(camp_conf)?;
        let (sender, receiver) = mpsc::unbounded();
        let handle = thread::Builder::new().name(format!("orch-campaign"))
        .spawn(move || {
            trace!("Campaign Thread: Creating resource in thread");
            let res = Arc::new(Mutex::new(campaign));
            trace!("Campaign Thread: Starting resource loop");
            let mut pool = executor::LocalPool::new();
            let mut spawner = pool.spawner();
            let handling_stream = receiver.for_each(
                move |(sender, operation): (oneshot::Sender<OperationOutput>, OperationInput)| {
                    trace!("Campaign Thread: received operation {:?}", operation);
                    match operation {
                        OperationInput::FetchExperiment => {
                            spawner.spawn_local(
                                Campaign::fetch_experiment(res.clone())
                                    .map(|a| {
                                        sender.send(OperationOutput::FetchExperiment(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::CreateExecution(commit, params, tags) =>{
                            spawner.spawn_local(
                                Campaign::create_execution(res.clone(), commit, params, tags)
                                    .map(|a|{
                                        sender.send(OperationOutput::CreateExecution(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::UpdateExecution(id, upd) =>{
                            spawner.spawn_local(
                                Campaign::update_execution(res.clone(), id, upd)
                                    .map(|a|{
                                        sender.send(OperationOutput::UpdateExecution(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::FinishExecution(id) =>{
                            spawner.spawn_local(
                                Campaign::finish_execution(res.clone(), id)
                                    .map(|a|{
                                        sender.send(OperationOutput::FinishExecution(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::DeleteExecution(id) =>{
                            spawner.spawn_local(
                                Campaign::delete_execution(res.clone(), id)
                                    .map(|a|{
                                        sender.send(OperationOutput::DeleteExecution(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::FetchExecutions =>{
                            spawner.spawn_local(
                                Campaign::fetch_executions(res.clone())
                                    .map(|a|{
                                        sender.send(OperationOutput::FetchExecutions(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::GetExecutions =>{
                            spawner.spawn_local(
                                Campaign::get_executions(res.clone())
                                    .map(|a|{
                                        sender.send(OperationOutput::GetExecutions(a))
                                            .map_err(|e| error!("Campaign Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                    }.map_err(|e| error!("Campaign Thread: Failed to spawn the operation: \n{:?}", e))
                    .unwrap();
                    future::ready(())
                }
            );
            let mut spawner = pool.spawner();
            spawner.spawn_local(handling_stream)
                .map_err(|_| error!("Campaign Thread: Failed to spawn handling stream"))
                .unwrap();
            trace!("Campaign Thread: Starting local executor.");
            pool.run();
            trace!("Campaign Thread: All futures executed. Leaving...");
        }).expect("Failed to spawn campaign thread.");
        let drop_sender = sender.clone();
        Ok(CampaignHandle {
            _sender: sender,
            _dropper: Dropper::from_closure(
                Box::new(move ||{
                    drop_sender.close_channel();
                    handle.join();
                }), 
                format!("CampaignHandle")),
        })
    }

    /// Async method, returning a future that ultimately resolves in a campaign, after having
    /// fetched the origin changes on the experiment repository.
    pub fn async_fetch_experiment(&self) -> impl Future<Output=Result<CampaignConf,Error>> {
        debug!("CampaignHandle: Building async_fetch_experiment future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_fetch_experiment_future: Sending input");
            chan.send((sender, OperationInput::FetchExperiment))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_fetch_experiement_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::FetchExperiment(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected FetchExperiment, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration,
    /// after it has been created.
    pub fn async_create_execution(
        &self,
        commit: &ExperimentCommit,
        parameters: &ExecutionParameters,
        tags: Vec<&ExecutionTag>,
    ) -> impl Future<Output=Result<ExecutionConf, Error>> {
        debug!("CampaignHandle: Building async_create_execution future");
        let mut chan = self._sender.clone();
        let commit = commit.to_owned();
        let parameters = parameters.to_owned();
        let tags = tags.into_iter().map(|a| a.to_owned()).collect::<Vec<ExecutionTag>>();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_create_execution_future: Sending input");
            chan.send((sender, OperationInput::CreateExecution(commit, parameters, tags)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_create_execution_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::CreateExecution(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected CreateExecution, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves in the new configuration after it
    /// was updated.
    pub fn async_update_execution(
        &self,
        id: &ExecutionId,
        upd: &ExecutionUpdate,
    ) -> impl Future<Output=Result<ExecutionConf, Error>>{
        debug!("CampaignHandle: Building async_update_execution future");
        let mut chan = self._sender.clone();
        let id = id.to_owned();
        let upd = upd.to_owned();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_update_executio_future: Sending input");
            chan.send((sender, OperationInput::UpdateExecution(id, upd)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_update_execution_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::UpdateExecution(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected UpdateExecution, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn async_finish_execution(&self, id: &ExecutionId) -> impl Future<Output=Result<ExecutionConf, Error>>{
        debug!("CampaignHandle: Building async_finish_execution future");
        let mut chan = self._sender.clone();
        let id = id.to_owned();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_finish_execution_future: Sending input");
            chan.send((sender, OperationInput::FinishExecution(id)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_finish_execution_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::FinishExecution(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected FinishExecution, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves in an empty type after it was
    /// finished.
    pub fn async_delete_execution(&self, id: &ExecutionId) -> impl Future<Output=Result<(), Error>> {
        debug!("CampaignHandle: Building async_delete_execution future");
        let mut chan = self._sender.clone();
        let id = id.to_owned();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_delete_execution_future: Sending input");
            chan.send((sender, OperationInput::DeleteExecution(id)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_delete_execution_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::DeleteExecution(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected DeleteExecution, found {:?}", e)))
            }
        }
    }    
    
    /// Async method, returning a future that ultimately resolves in an execution configuration
    /// after it was finished.
    pub fn async_fetch_executions(&self) -> impl Future<Output=Result<Vec<ExecutionConf>, Error>> {
        debug!("CampaignHandle: Building async_fetch_executions future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_fetch_executions_future: Sending input");
            chan.send((sender, OperationInput::FetchExecutions))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_fetch_executions_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::FetchExecutions(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected FetchExecutions, found {:?}", e)))
            }
        }
    }    
 
    /// Async method, returning a future that ultimately resolves in a vector of execution conf.
    pub fn async_get_executions(&self) -> impl Future<Output=Result<Vec<ExecutionConf>, Error>> {
        debug!("CampaignHandle: Building async_get_executions future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("CampaignHandle::async_get_executions_future: Sending input");
            chan.send((sender, OperationInput::GetExecutions))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("CampaignHandle::async_get_executions_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::GetExecutions(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Exepected GetExecutions, found {:?}", e)))
            }
        }
    }
}


//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod tests {

    use super::*;
    use std::io::prelude::*;
    use std::time::Duration;
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
        std::thread::sleep(Duration::new(1, 000));
        if let Ok(Some(_)) = server.try_wait() {
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
        Campaign::new(
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
        let repo = CampaignHandle::spawn(repo.conf).unwrap();
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
        thread::sleep(Duration::new(1, 000));
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
        let repo = CampaignHandle::spawn(repo.conf).unwrap();

        use futures::task::SpawnExt;
        let mut executor = futures::executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        let commit = get_expe_repo_head();

        for _ in 1..200 {
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

        thread::sleep(Duration::new(1, 000));
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
        let repo = CampaignHandle::spawn(repo.conf).unwrap();
        let commit = get_expe_repo_head();
        let exc = block_on(repo.async_create_execution(
            &ExperimentCommit(commit.clone()),
            &ExecutionParameters("".to_owned()),
            vec![],
        ))
        .unwrap();
        println!("Execution: {:?}", exc);
        let upd = ExecutionUpdate {
            state: Some(ExecutionState::Initialized),
            executor: Some("apere-pc".to_owned()),
            execution_stdout: Some("Some stdout messages".to_owned()),
            execution_stderr: Some("Some stderr messages".to_owned()),
            execution_exit_code: Some(0),
            execution_beginning_date: Some(Utc::now()),
            execution_ending_date: Some(Utc::now()),
            execution_features: Some(vec![1.5, 1.5]),
            execution_message: Some("Some orchestra messages".to_owned()),
        };

        let exc_u = block_on(repo.async_update_execution(&exc.identifier, &upd)).unwrap();
        println!("Execution updated: {:?}", exc);
        assert_eq!(exc.identifier, exc_u.identifier);

        thread::sleep(Duration::new(1, 000));
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
        let repo = CampaignHandle::spawn(repo.conf).unwrap();
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

        thread::sleep(Duration::new(1, 000));
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
        let repo = CampaignHandle::spawn(repo.conf).unwrap();
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

        thread::sleep(Duration::new(1, 000));
        let executions = block_on(repo.async_get_executions()).unwrap();
        assert!(!executions.contains(&exc));

        clean_expe_repo();
        clean_cmp_repo();
    }

}
