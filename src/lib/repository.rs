// liborchestra/repository.rs
// Author: Alexandre Péré
///
/// This module contains the structures to manipulate the main concepts of expegit:
/// + The campaign repository, manipulated by `Campaign`
/// + The executions, manipulated by `Execution`s
///
/// In particular, all operations related to the filesystem are available through `Campaign`. In an
/// asynchronous system, this allows to share a single instance of `Campaign` protected with a mutex,
/// and as such, avoid races condition on the file system.

// IMPORTS
use std::{fmt, fs, io, path, str};
use serde_yaml;
use regex;
use chrono;
use uuid;
use super::{git, utilities, Error};
use super::{EXPEGIT_RPATH, XPRP_RPATH, EXCS_RPATH, DATA_RPATH, LFS_RPATH, EXCCONF_RPATH};

// STRUCTURES
/// Represents a campaign repository.
/// There are three ways to construct a campaign object:
/// + `::init(local_path, xprp_repo_url)` , this one will initialize a new expegit repository
/// at the `local_path` location and will generate the repo for experimental code found at
/// `xprp_repo_url`.
/// + `::from_url(cmp_url, cmp_parent_path)` , which will clone an existing campaign repo from
/// `cmp_url` address, at `cmp_parent_path` location.
/// + `::from_path(cmp_path)` , which imports the configuration from an existing local campaign
/// repository at location `cmp_path`.
///
/// ```rust,no_run
/// use std::path;
/// use libexpegit::Campaign;
///
/// let repo_path = path::PathBuf::from("/home/user/campaignrepo");
/// let experiment_repo_url = "ssh://git@github.com:user/myexpe.git";
/// let campaign = Campaign::init(&repo_path, experiment_repo_url).unwrap();
///
/// println!("My campaign {} was initialized", campaign);
/// ```
///
/// Also, `::push()` and `::pull()` methods are provided, to synchronize the local repository
/// with distant branches.
///
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct Campaign {
    // The url of the origin remote. Used to push and pull.
    _cmp_origin_url: String,
    // The url of the experiment origin remote.
    _xpr_origin_url: String,
    // The absolute path to the campaign
    _cmp_path: path::PathBuf,
    // The path to the experiment repo relative to the campaign folder.
    _xprp_rpath: path::PathBuf,
    // The path to the executions folder relative to the campaign folder.
    _excs_rpath: path::PathBuf,
    // The path to the .expegit file relative to the campaign folder.
    _expegit_rpath: path::PathBuf,
}

impl Campaign {
    /// Allows to initialize a new expegit repository. The directory pointed to by `local_path`
    /// must be an empty git repository, and the `xprp_repo_url` must be an url pointing to
    /// the experiment repository remote. This address will be used to fetch the experiment
    /// repository. In most cases, you would prefer it to be an `ssh://` type adress, which
    /// will avoid typing your username and password every time.
    pub fn init(local_path: &path::PathBuf, xprp_repo_url: &str) -> Result<Campaign, Error> {
        info!(
            "Initializing campaign on experiment {} in {}",
            xprp_repo_url,
            local_path.to_str().unwrap()
        );
        // We check that local_path is valid as a repository
        if !local_path.is_dir() || !local_path.join(".git").is_dir() {
            return Err(Error::Git(String::from("Not a valid repository")));
        }
        // We check that we are not already in an expegit repository
        if local_path.join(EXPEGIT_RPATH).exists() {
            return Err(Error::AlreadyRepository);
        }
        // We get origin url of repo
        let origin_url = match git::get_origin_url(&local_path) {
            Err(err) => {
                return Err(err);
            }
            Ok(url) => url,
        };
        // We generate the campaign config
        let cmp = Campaign {
            _cmp_origin_url: origin_url,
            _xpr_origin_url: String::from(xprp_repo_url),
            _cmp_path: local_path.to_owned(),
            _xprp_rpath: path::PathBuf::from(XPRP_RPATH),
            _excs_rpath: path::PathBuf::from(EXCS_RPATH),
            _expegit_rpath: path::PathBuf::from(EXPEGIT_RPATH),
        };
        // We pull xprp repo as a submodule
        git::add_submodule(cmp.get_experiment_origin_url(), XPRP_RPATH, &cmp.get_path())?;
        // We create excs directory
        fs::create_dir(cmp.get_executions_path())?;
        // We generate .gitignore to not push something else than data
        utilities::write_gitkeep(&cmp.get_executions_path())?;
        // We export the config
        let file = fs::File::create(cmp.get_expegit_path()).map_err(Error::Io)?;
        let mut cmps = cmp.clone();
        cmps._cmp_path = path::PathBuf::from("");
        serde_yaml::to_writer(file, &cmps).map_err(Error::Json)?;
        // We track-commit the whole repo
        git::stage(&path::PathBuf::from("."), &cmp.get_path())?;
        git::commit("Initializes Campaign Repository", &cmp.get_path())?;
        // We return the configuration.
        return Ok(cmp);
    }

    /// Allows to clone an existing expegit repository from a remote repository. The `cmp_url` must be
    /// a valid expegit repository. Again, `ssh://` adresses are prefered.
    pub fn from_url(cmp_url: &str, cmp_parent_path: &path::PathBuf) -> Result<Campaign, Error> {
        info!(
            "Cloning experiment {} in {}",
            cmp_url,
            cmp_parent_path.to_str().unwrap()
        );
        // We check that cmp_parent_path exists
        if !cmp_parent_path.exists() {
            warn!("Cloning path does not exists");
            return Err(Error::Io(io::Error::new(io::ErrorKind::NotFound, "")));
        }
        // We clone the repo
        git::clone_remote_repo(cmp_url, cmp_parent_path)?;
        // We import the campaign config
        let repo_name = regex::Regex::new(r"([^/]*)\.git")?
            .captures(cmp_url)
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
        // We initialize lfs
        git::init_lfs(&cmp_parent_path.join(repo_name))?;
        // We retrieve campaign
        let cmp_config = Campaign::from_path(&cmp_parent_path.join(repo_name))?;
        // We return the campaign configuration
        return Ok(cmp_config);
    }

    /// Allows to open an existing expegit repository from its local path.
    pub fn from_path(cmp_path: &path::PathBuf) -> Result<Campaign, Error> {
        info!("Loading Campaign from {}", cmp_path.to_str().unwrap());
        let file = fs::File::open(cmp_path.join(EXPEGIT_RPATH))?;
        let mut config: Campaign = serde_yaml::from_reader(file)?;
        config._cmp_path = fs::canonicalize(cmp_path)?;
        let config = config;
        return Ok(config);
    }

    /// Pulls the changes from the origin repository. Note that the last changes in the experiment repository
    /// are not affected by this method.
    pub fn pull(&self) -> Result<(), Error> {
        info!("Pulling changes from remote");
        // We pull the repo
        git::pull(&self.get_path())?;
        // We return
        return Ok(());
    }

    /// Pushes the changes to the origin branch.
    pub fn push(&self) -> Result<(), Error> {
        info!("Pushing changes to remote");
        // We try to push
        git::push(None, &self.get_path())?;
        // We return
        return Ok(());
    }

    /// Fetches the last changes on the experiment repository.
    pub fn fetch_experiment(&self) -> Result<(), Error> {
        info!("Fetching experiment from remote");
        // We fetch changes
        git::fetch("origin", &self.get_experiment_path()).expect("Failed to fetch 'origin/master'");
        // We retrieve experiment origin/master and master tips
        let origin_tip = git::get_tip("origin/master", &self.get_experiment_path())
            .expect("Failed to get experiment 'origin/master' tip");
        let local_tip = git::get_tip("HEAD", &self.get_experiment_path())
            .expect("Failed to get experiment 'master' tip");
        // If they differ, we update submodule
        if origin_tip != local_tip {
            debug!("Remote tip different with local HEAD. Fetching.");
            // We get remote changes on submodules and merge
            git::update_submodule(&self.get_path())?;
            // We commit changes
            git::stage(&path::PathBuf::from(XPRP_RPATH), &self.get_path())?;
            git::commit("Updates Experiment Repository", &self.get_path())?;
        }
        // We return
        return Ok(());
    }

    /// Returns the campaign repository origin url as inputed at repository creation.
    pub fn get_campaign_origin_url(&self) -> &str {
        info!("Getting campaign origin url");
        &self._cmp_origin_url
    }

    /// Returns the campaign remote address for web-browsing
    pub fn get_campaign_url(&self) -> String {
        info!("Getting campaign url");
        if self.get_campaign_origin_url().contains("ssh://") {
            let capture = regex::Regex::new(r"^ssh://git@([^/^:]*):?[0-9]*(.*).git")
                .unwrap()
                .captures(self.get_campaign_origin_url())
                .unwrap();
            return format!("http://{}{}", capture.get(1).unwrap().as_str(), capture.get(2).unwrap().as_str());
        }
        else{
            return self.get_campaign_origin_url().replace(".git", "");
        }
    }

    /// Returns the name of the campaign repository
    pub fn get_campaign_name(&self) -> &str {
        info!("Getting campaign name");
        // We retrieve name by regex
        return regex::Regex::new(r"([^/]*)\.git")
            .unwrap()
            .captures(self.get_campaign_origin_url())
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
    }

    // Returns the name of the experiment repository
    pub fn get_experiment_name(&self) -> &str {
        info!("Getting experiment name");
        // We compute the regex
        return regex::Regex::new(r"([^/]*)\.git")
            .unwrap()
            .captures(self.get_experiment_origin_url())
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
    }

    /// Returns the experiment repository origin url as inputed at repository creation.
    pub fn get_experiment_origin_url(&self) -> &str {
        info!("Getting experiment origin url");
        &self._xpr_origin_url
    }

    /// Returns the experiment remote address for web-browsing
    pub fn get_experiment_url(&self) -> String {
        info!("Getting campaign url");
        if self.get_experiment_origin_url().contains("ssh://") {
            let capture = regex::Regex::new(r"^ssh://git@([^/^:]*):?[0-9]*(.*).git")
                .unwrap()
                .captures(self.get_experiment_origin_url())
                .unwrap();
            return format!("http://{}{}", capture.get(1).unwrap().as_str(), capture.get(2).unwrap().as_str());
        }
        else{
            return self.get_experiment_origin_url().replace(".git", "");
        }
    }

    /// Returns the path to the local repository root.
    pub fn get_path(&self) -> path::PathBuf {
        info!("Getting campaign path");
        self._cmp_path.clone()
    }

    /// Returns the path to the experiment submodule.
    pub fn get_experiment_path(&self) -> path::PathBuf {
        info!("Getting experiment path");
        self._cmp_path.join(&self._xprp_rpath)
    }

    /// Returns the path to the executions directory.
    pub fn get_executions_path(&self) -> path::PathBuf {
        info!("Getting executions path");
        self._cmp_path.join(&self._excs_rpath)
    }

    /// Returns a list of executions
    pub fn get_executions(&self) -> Vec<Execution>{
        info!("Getting executions ids");
        return self.get_executions_path()
            .read_dir()
            .unwrap()
            .map(|r| r.unwrap())
            .filter(|r| r.path().is_dir())
            .map(|r| Execution::from_path(&r.path()).unwrap())
            .collect()
    }

    /// Returns the path to the expegit file.
    pub fn get_expegit_path(&self) -> path::PathBuf {
        info!("Getting .expegit path");
        self._cmp_path.join(&self._expegit_rpath)
    }

    /// Create a new execution.
    pub fn new_execution(
        &self,
        commit: Option<String>,
        parameters: String,
    ) -> Result<Execution, Error> {
        info!(
            "Adding execution on commit {:?} with parameters {}",
            commit, parameters
        );
        // If the commit is None, we give it the last possible value.
        let commit = commit.unwrap_or(git::get_last_commit("master", &self.get_experiment_path())?);
        // We check that commit number is valid
        let commits = git::get_all_commits(&self.get_experiment_path())?;
        if !commits.contains(&commit.to_string()) {
            warn!("Commit could not be found");
            return Err(Error::InvalidCommit(commit.to_string()));
        }
        let exc_config = Execution {
            _commit: commit.to_owned(),
            _parameters: String::from(parameters),
            _state: ExecutionState::Initialized,
            _reset_count: 0,
            _excs_path: self.get_executions_path(),
            _data_rpath: path::PathBuf::from(DATA_RPATH),
            _lfs_rpath: path::PathBuf::from(LFS_RPATH),
            _executor: None,
            _execution_date: None,
            _execution_duration: None,
            _execution_stdout: None,
            _execution_stderr: None,
            _execution_exit_code: None,
            _generator: utilities::get_hostname().unwrap(),
            _generation_date: chrono::prelude::Utc::now()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            _identifier: format!("{}", uuid::Uuid::new_v4()),
        };
        // We build the directory
        self.build_execution_directory(&exc_config)?;
        // We commit the folder
        git::stage(&path::PathBuf::from("."), &exc_config.get_path())?;
        git::commit(
            format!("Initialize execution {}", exc_config.get_identifier()).as_ref(),
            &self.get_path(),
        )?;
        // We return the execution config
        return Ok(exc_config);
    }

    /// Allows to build an execution directory
    fn build_execution_directory(&self, exc_config: &Execution) -> Result<(), Error>{
        info!("Building execution {} directory", exc_config.get_identifier());
        // We make the directory
        fs::create_dir(&exc_config.get_path())?;
        // We clone the xprp local repo with depth 1 and make a hard reset to commit
        git::clone_local_repo(&self.get_experiment_path(), &exc_config.get_path())?;
        git::checkout(exc_config.get_commit(), &exc_config.get_path())?;
        // We create data directories
        fs::create_dir(&exc_config.get_data_path())?;
        fs::create_dir(&exc_config.get_lfs_path())?;
        // We replace the .gitignore
        utilities::write_exc_gitignore(&exc_config.get_path())?;
        // We append the .gitattributes
        utilities::write_lfs_gitattributes(&exc_config.get_lfs_path())?;
        // We save the execution config
        self.sync_execution(&exc_config)?;
        // We remove the .git folder
        fs::remove_dir_all(exc_config.get_path().join(".git"))?;
        Ok(())
    }

    /// Remove an existing execution from the repository.
    pub fn remove_execution(&self, execution:Execution) -> Result<(), Error> {
        info!("Removing execution {}", execution.get_identifier());
        // We clean up the folder
        git::clean(&execution.get_path())?;
        fs::remove_dir_all(&execution.get_path()).unwrap();
        // We commit changes
        git::stage(&execution.get_path(), &self.get_path())?;
        git::commit(
            format!("Remove execution {}", &execution.get_identifier()).as_ref(),
            &self.get_path(),
        )?;
        // We return
        return Ok(());
    }

    /// Reset an existing execution
    pub fn reset_execution(&self, execution: &mut Execution) -> Result<(), Error> {
        info!("Resetting execution {}", execution.get_identifier());
        // We reset the execution config
        execution.reset_executor();
        execution.reset_execution_date();
        execution.reset_execution_duration();
        execution.reset_execution_stdout();
        execution.reset_execution_stderr();
        execution.reset_execution_exit_code();
        execution.increment_reset_count();
        // We remove the directory
        fs::remove_dir_all(&execution.get_path())?;
        // We rebuild the execution directory
        self.build_execution_directory(&execution)?;
        // We commit the changes
        git::stage(&path::PathBuf::from("."), &execution.get_path())?;
        git::commit(
            format!("Reset execution {}", execution.get_identifier()).as_ref(),
            &self.get_path(),
        )?;
        // We return
        return Ok(());
    }

    /// Finishes an execution
    pub fn finish_execution(&self, execution: &mut Execution) -> Result<(), Error> {
        info!("Finishing execution {}", execution.get_identifier());
        // We modify the execution config
        execution.set_state(ExecutionState::Finished);
        self.sync_execution(&execution)?;
        // We commit the changes
        git::stage(&path::PathBuf::from("."), &execution.get_path())?;
        git::commit(
            format!("Finishes execution {}", execution.get_identifier()).as_ref(),
            &execution.get_path(),
        )?;
        // We clean up the folder
        git::clean(&execution.get_path())?;
        // We return the Execution config
        return Ok(());
    }

    /// Writes an execution to the .excconf file.
    pub fn sync_execution(&self, execution: &Execution) -> Result<(), Error> {
        info!("Writing execution {}", execution.get_identifier());
        // We write execution
        let file_path = &execution.get_path().join(EXCCONF_RPATH);
        let file = fs::File::create(file_path).map_err(Error::Io)?;
        let mut config = execution.clone();
        config._excs_path = path::PathBuf::from("");
        serde_yaml::to_writer(file, &config).map_err(Error::Json)?;
        // We return
        return Ok(());
    }
}

impl fmt::Display for Campaign {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        return write!(
            f,
            "Campaign<{}[{}]>",
            self.get_campaign_name(),
            self.get_experiment_name()
        );
    }
}

/// Represents the state of an execution.
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Clone)]
pub enum ExecutionState {
    Initialized,
    Finished,
}

impl<'a> From<&'a str> for ExecutionState {
    fn from(s: &str) -> ExecutionState {
        match s {
            "initialized" => ExecutionState::Initialized,
            "finished" => ExecutionState::Finished,
            _ => panic!("Unknown state string"),
        }
    }
}

impl fmt::Display for ExecutionState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecutionState::Initialized => write!(f, "Initialized"),
            ExecutionState::Finished => write!(f, "Finished"),
        }
    }
}

/// Represent an an execution of the experimental code, for a given parameterization and a given
/// commit number in the experiment repository.
#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone)]
pub struct Execution {
    // The experiment commit hash checked out before execution
    _commit: String,
    // The parameters string fed to the script
    _parameters: String,
    // The state of the execution
    _state: ExecutionState,
    // The nimber of times it was reset
    _reset_count: u32,
    // The absolute path to the execution
    _excs_path: path::PathBuf,
    // The path to the data relative to the execution
    _data_rpath: path::PathBuf,
    // The path to the lfs folder relative to the data
    _lfs_rpath: path::PathBuf,
    // The name of the machine which executed the code
    _executor: Option<String>,
    // The date of the execution
    _execution_date: Option<String>,
    // The duration of the execution
    _execution_duration: Option<u32>,
    // The standard output of the execution
    _execution_stdout: Option<String>,
    // The standard error of the execution
    _execution_stderr: Option<String>,
    // The exit code of the execution
    _execution_exit_code: Option<u32>,
    // The name of the generator (of the job)
    _generator: String,
    // The date of the generation
    _generation_date: String,
    // The uuid of the identifier
    _identifier: String,
}

impl Execution {

    /// Returns the commit hash
    pub fn get_commit(&self) -> &str {
        info!("Getting Execution commit");
        return &self._commit;
    }

    /// Returns the parameters string
    pub fn get_parameters(&self) -> &str {
        info!("Getting Execution parameters");
        return &self._parameters;
    }

    /// Returns the state
    pub fn get_state(&self) -> ExecutionState {
        info!("Getting Execution state");
        return self._state.clone();
    }

    /// Sets the state
    pub fn set_state(&mut self, state: ExecutionState) {
        info!("Setting Execution state to {}", state);
        self._state = state;
    }

    /// Returns the reset count
    pub fn get_reset_count(&self) -> u32 {
        info!("Getting Execution reset count");
        return self._reset_count;
    }

    /// Increments the reset count
    pub fn increment_reset_count(&mut self) {
        info!("Incrementing Execution reset count");
        self._reset_count += 1;
    }

    /// Get the absolute execution path
    pub fn get_path(&self) -> path::PathBuf {
        info!("Getting Execution path");
        return self._excs_path.join(&self._identifier);
    }

    /// Get the absolute data path
    pub fn get_data_path(&self) -> path::PathBuf {
        info!("Getting Execution data path");
        return self.get_path().join(&self._data_rpath);
    }

    /// Get the absolute lfs path
    pub fn get_lfs_path(&self) -> path::PathBuf {
        info!("Getting Execution lfs path");
        return self.get_data_path().join(&self._lfs_rpath);
    }

    /// Get the executor
    pub fn get_executor(&self) -> &Option<String> {
        info!("Getting Execution executor");
        return &self._executor;
    }

    /// Set the executor
    pub fn set_executor(&mut self, executor: &str) {
        info!("Setting Execution executor to {}", executor);
        self._executor = Some(String::from(executor));
    }

    /// Reset the executor
    pub fn reset_executor(&mut self) {
        info!("Resetting Execution executor");
        self._executor = None;
    }

    /// Get the execution date
    pub fn get_execution_date(&self) -> &Option<String> {
        info!("Getting Execution execution date");
        return &self._execution_date;
    }

    /// Set the execution Date
    pub fn set_execution_date(&mut self, date: &str) {
        info!("Setting Execution execution date to {}", date);
        self._execution_date = Some(String::from(date));
    }

    /// Reset the execution date
    pub fn reset_execution_date(&mut self) {
        info!("Resetting Execution execution date");
        self._execution_date = None;
    }

    /// Get the execution duration
    pub fn get_execution_duration(&self) -> &Option<u32> {
        info!("Getting Execution execution duration");
        return &self._execution_duration;
    }

    /// Set the execution duration
    pub fn set_execution_duration(&mut self, duration: u32) {
        info!("Setting Execution execution duration to {}", duration);
        self._execution_duration = Some(duration);
    }

    /// Reset the execution duration
    pub fn reset_execution_duration(&mut self) {
        info!("Resetting Execution execution duration");
        self._execution_duration = None;
    }

    /// Get the execution exit code
    pub fn get_execution_exit_code(&self) -> &Option<u32> {
        info!("Getting Execution exit code");
        return &self._execution_exit_code;
    }

    /// Set the execution exit code
    pub fn set_execution_exit_code(&mut self, duration: u32) {
        info!("Setting Execution exit code to {}", duration);
        self._execution_exit_code = Some(duration);
    }

    /// Reset the execution exit code
    pub fn reset_execution_exit_code(&mut self) {
        info!("Resetting Execution exit code");
        self._execution_exit_code = None;
    }

    /// Get the execution stdout
    pub fn get_execution_stdout(&self) -> &Option<String> {
        info!("Getting Execution stdout");
        return &self._execution_stdout;
    }

    /// Set the execution stdout
    pub fn set_execution_stdout(&mut self, stdout: &str) {
        info!("Setting Execution stdout to {}", stdout);
        self._execution_stdout = Some(String::from(stdout));
    }

    /// Reset the execution stdout
    pub fn reset_execution_stdout(&mut self) {
        info!("Resetting Execution stdout");
        self._execution_stdout = None;
    }

    /// Get the execution stderr
    pub fn get_execution_stderr(&self) -> &Option<String> {
        info!("Getting execution stderr");
        return &self._execution_stderr;
    }

    /// Set the execution stderr
    pub fn set_execution_stderr(&mut self, stderr: &str) {
        info!("Setting Execution stderr to {}", stderr);
        self._execution_stderr = Some(String::from(stderr));
    }

    /// Reset the execution stderr
    pub fn reset_execution_stderr(&mut self) {
        info!("Resetting Execution stderr");
        self._execution_stderr = None;
    }

    /// Get the generator
    pub fn get_generator(&self) -> &str {
        info!("Getting Execution generator");
        return &self._generator;
    }

    /// Get the generation date
    pub fn get_generation_date(&self) -> &str {
        info!("Getting Execution generation date");
        return &self._generation_date;
    }

    /// Get the uuid of the execution
    pub fn get_identifier(&self) -> &str {
        info!("Getting Execution identifier");
        return &self._identifier;
    }

    /// Load an execution from its path
    pub fn from_path(exc_path: &path::PathBuf) -> Result<Execution, Error> {
        // We log
        info!("Loading Execution from {}", exc_path.to_str().unwrap());
        // We load file
        let file = fs::File::open(exc_path.join(EXCCONF_RPATH))?;
        let mut config: Execution = serde_yaml::from_reader(file)?;
        config._excs_path = exc_path.parent().unwrap().to_path_buf().clone();
        return Ok(config);
    }

}

impl fmt::Display for Execution {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Execution<{}>", self.get_identifier())
    }
}

// TESTS
#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashSet;
    use std::env;
    use std::fs;
    use std::iter::FromIterator;
    use std::thread;
    use std::time;

    // Modify the files with the variables that suits your setup to run the test.
    static TEST_PATH: &str = include_str!("../../test/constants/test_path");
    static EXPERIMENT_REPOSITORY_URL: &str = include_str!("../../test/constants/experiment_repository_url");
    static EXPERIMENT_REPOSITORY_HEAD: &str = include_str!("../../test/constants/experiment_repository_head");
    static INITIAL_CAMPAIGN_REPOSITORY_NAME: &str = include_str!("../../test/constants/initial_campaign_repository_name");
    static INITIAL_CAMPAIGN_REPOSITORY_URL: &str = include_str!("../../test/constants/initial_campaign_repository_url");
    static CAMPAIGN_REPOSITORY_NAME: &str = include_str!("../../test/constants/campaign_repository_name");
    static CAMPAIGN_REPOSITORY_URL: &str = include_str!("../../test/constants/campaign_repository_url");
    static CHANGING_EXPERIMENT_REPOSITORY_URL: &str = include_str!("../../test/constants/changing_experiment_repository_url");
    static CHANGING_EXPERIMENT_REPOSITORY_NAME: &str = include_str!("../../test/constants/changing_experiment_repository_name");
    static EMPTY_REPOSITORY_URL: &str = include_str!("../../test/constants/empty_repository_url");
    static EMPTY_REPOSITORY_NAME: &str = include_str!("../../test/constants/empty_repository_name");

    // UNIT TESTS
    #[test]
    fn test_campaign_init() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_init");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME).exists() {
            for thing in test_path
                .join(INITIAL_CAMPAIGN_REPOSITORY_NAME)
                .read_dir()
                .unwrap()
            {
                let entry = thing.unwrap();
                if entry.path().is_file() {
                    fs::remove_file(entry.path());
                } else if entry.path().file_name().unwrap() != ".git" {
                    fs::remove_dir_all(entry.path());
                }
            }
            git::stage(
                &path::PathBuf::from("."),
                &test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME),
            ).expect("Failed to stage changes");
            git::commit(
                "Reset Repository",
                &test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME),
            ).expect("Failed to commit changes");
            git::push(None, &test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME))
                .expect("Failed to push");
            git::pull(&test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME)).expect("Failed to pull");
            assert_eq!(
                test_path
                    .join(INITIAL_CAMPAIGN_REPOSITORY_NAME)
                    .read_dir()
                    .unwrap()
                    .collect::<Vec<_>>()
                    .len(),
                1
            );
            fs::remove_dir_all(test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME));
        }
        git::clone_remote_repo(INITIAL_CAMPAIGN_REPOSITORY_URL, &test_path).unwrap();
        let cmp_cfg = Campaign::init(
            &test_path.join(INITIAL_CAMPAIGN_REPOSITORY_NAME),
            EXPERIMENT_REPOSITORY_URL,
        ).expect("Failed to initialize repository.");
        cmp_cfg.push().expect("Failed to push");
        assert!(
            test_path
                .join(INITIAL_CAMPAIGN_REPOSITORY_NAME)
                .join(EXPEGIT_RPATH)
                .exists()
        );
        assert!(
            test_path
                .join(INITIAL_CAMPAIGN_REPOSITORY_NAME)
                .join(XPRP_RPATH)
                .exists()
        );
        assert!(
            test_path
                .join(INITIAL_CAMPAIGN_REPOSITORY_NAME)
                .join(EXCS_RPATH)
                .exists()
        );
    }

    #[test]
    fn test_campaign_from_url() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_from_url");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(CAMPAIGN_REPOSITORY_NAME));
        }
        let cmp = Campaign::from_url(CAMPAIGN_REPOSITORY_URL, &test_path)
            .expect("Failed to initialize repository.");
        assert!(
            test_path
                .join(CAMPAIGN_REPOSITORY_NAME)
                .join(EXPEGIT_RPATH)
                .exists()
        );
        assert!(
            test_path
                .join(CAMPAIGN_REPOSITORY_NAME)
                .join(XPRP_RPATH)
                .exists()
        );
        assert!(
            test_path
                .join(CAMPAIGN_REPOSITORY_NAME)
                .join(EXCS_RPATH)
                .exists()
        );
        assert!(
            test_path
                .join(CAMPAIGN_REPOSITORY_NAME)
                .join(EXCS_RPATH)
                .exists()
        );
    }

    #[test]
    fn test_campaign_from_path() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_from_path");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if !test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path);
        }
        let cmp = Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME))
            .expect("Failed to open repository.");
        assert_eq!(cmp.get_campaign_origin_url(), CAMPAIGN_REPOSITORY_URL);
    }

    #[test]
    fn test_campaign_fetch_experiment() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_fetch_experiment");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if !test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME).exists() {
            git::clone_remote_repo(CHANGING_EXPERIMENT_REPOSITORY_URL, &test_path).unwrap();
        }
        if test_path.join(EMPTY_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(&test_path.join(EMPTY_REPOSITORY_NAME)).unwrap();
        }
        fs::create_dir(&test_path.join(EMPTY_REPOSITORY_NAME)).unwrap();
        git::clone_remote_repo(EMPTY_REPOSITORY_URL, &test_path);
        let cmp = Campaign::init(
            &test_path.join(EMPTY_REPOSITORY_NAME),
            CHANGING_EXPERIMENT_REPOSITORY_URL,
        ).expect("Failed to initialize campaign");
        assert_eq!(
            git::get_tip(
                "master",
                &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
            ).unwrap(),
            git::get_tip("master", &cmp.get_experiment_path()).unwrap()
        );
        let content = fs::read(
            &test_path
                .join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
                .join("1"),
        ).expect("Failed to read 1 file");
        let mut content = String::from_utf8(content).unwrap();
        content.push('a');
        fs::write(
            &test_path
                .join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
                .join("1"),
            content,
        ).expect("Unable to write to file");
        git::stage(
            &test_path
                .join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
                .join("1"),
            &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME),
        ).expect("Failed to stage");
        git::commit(
            "append",
            &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME),
        ).expect("Failed to commit");
        git::push(None, &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME))
            .expect("Failed to push changes");
        assert_ne!(
            git::get_tip(
                "master",
                &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
            ).unwrap(),
            git::get_tip("master", &cmp.get_experiment_path()).unwrap()
        );
        thread::sleep_ms(1000);
        cmp.fetch_experiment().expect("Failed to update submodule");
        assert_eq!(
            git::get_tip(
                "master",
                &test_path.join(CHANGING_EXPERIMENT_REPOSITORY_NAME)
            ).unwrap(),
            git::get_tip("master", &cmp.get_experiment_path()).unwrap()
        );
    }

    #[test]
    fn test_execution_new() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_new_execution");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if !test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path);
        }
        let campaign = Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME))
            .expect("Failed to open campaign");
        let execution = campaign
            .new_execution(None, String::from("N-p --all 111"))
            .expect("Failed to create execution");
        assert!(execution.get_path().exists());
        assert!(execution.get_data_path().exists());
        assert!(execution.get_lfs_path().exists());
    }

    #[test]
    fn test_execution_remove() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_remove_execution");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if !test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path);
        }
        let campaign = Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME))
            .expect("Failed to open campaign");
        let execution = campaign
            .new_execution(None, String::from("-m -p --all 111"))
            .expect("Failed to create execution");
        let execution_path = execution.get_path();
        campaign.remove_execution(execution).unwrap();
        assert!(!execution_path.exists());
    }

    #[test]
    fn test_execution_finish() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/repository/campaign_finish_execution");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if !test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path);
        }
        let campaign = Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME))
            .expect("Failed to open campaign");
        let mut execution = campaign
            .new_execution(None, String::from("-m -p --all 111"))
            .expect("Failed to create execution");
        let execution_path = execution.get_path();
        campaign
            .finish_execution(&mut execution)
            .expect("Failed to finish execution");
        assert_eq!(execution.get_state(), ExecutionState::Finished);
    }
}
