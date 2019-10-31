//! liborchestra/repository/synchro.rs
//! 
//! This module contains synchronization mechanisms for the campaign repositories. Those can be used
//! to save the results of experiments, or collaborate on a campaign.


//------------------------------------------------------------------------------------------ IMPORTS


use super::{CampaignConf, Error, ExecutionConf};

//----------------------------------------------------------------------- REPOSITORY SYNCHRONIZATION


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
    fn init_repository_hook(&self, _cmp: &CampaignConf) -> Result<(), Error> {
        Ok(())
    }

    fn fetch_experiment_hook(&self, _cmp: &CampaignConf) -> Result<(), Error> {
        Ok(())
    }

    fn create_execution_hook(&self, _exc: &ExecutionConf) -> Result<(), Error> {
        Ok(())
    }

    fn update_execution_hook(&self, _exc: &ExecutionConf) -> Result<(), Error> {
        Ok(())
    }

    fn delete_execution_hook(&self, _exc: &ExecutionConf) -> Result<(), Error> {
        Ok(())
    }

    fn finish_execution_hook(&self, _exc: &ExecutionConf) -> Result<(), Error> {
        Ok(())
    }

    fn fetch_executions_hook(&self, _cmp: &CampaignConf) -> Result<(), Error> {
        Ok(())
    }
}
