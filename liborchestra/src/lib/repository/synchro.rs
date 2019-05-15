// liborchestra/repository/synchro.rs
// Author: Alexandre Péré
/// This module contains synchronization mechanisms for the campaign repositories. Those can be used
/// to save the results of experiments, or collaborate on a campaign.

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use super::{CampaignConf, Error, ExecutionConf};

///////////////////////////////////////////////////////////////////////// REPOSITORY SYNCHRONIZATION

/// A Trait implements the methods used to synchronize the repository.
pub trait SyncRepository
where
    Self: Send,
{
    /// Hook called after the repository was initialized
    fn init_repository_hook(&self, cmp: &CampaignConf) -> Result<(), Error>;

    /// Hook called after the experiment was fetched
    fn fetch_experiment_hook(&self, cmp: &CampaignConf) -> Result<(), Error>;

    /// Hook called after an execution was created
    fn create_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error>;

    /// Hook called after an execution was updated
    fn update_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error>;

    /// Hook called after the execution was deleted
    fn delete_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error>;

    /// Hook called after the execution was finished
    fn finish_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error>;

    /// Hook called after the executions were fetched
    fn fetch_executions_hook(&self, cmp: &CampaignConf) -> Result<(), Error>;
}

/// This enumeration represents the different mechanisms that can be used to synchronize a
/// repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub enum Synchronizer {
    Null(NullSynchronizer),
}

/// Synchronizes nothing.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct NullSynchronizer {}


impl SyncRepository for NullSynchronizer {
    fn init_repository_hook(&self, cmp: &CampaignConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Repository initialization hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn fetch_experiment_hook(&self, cmp: &CampaignConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Fetch experiment hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn create_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Execution creation hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn update_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Execution update hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn delete_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Execution deletion hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn finish_execution_hook(&self, exc: &ExecutionConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Execution finished hook called.");
        match self {
            Null => Ok(()),
        }
    }

    fn fetch_executions_hook(&self, cmp: &CampaignConf) -> Result<(), Error> {
        debug!("NullSynchronizer: Execution fetched hook called.");
        match self {
            Null => Ok(()),
        }
    }
}
