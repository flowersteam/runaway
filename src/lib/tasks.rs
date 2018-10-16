// liborchestra/tasks.rs
// Author: Alexandre Péré
///
/// This module contains the tools to asynchronously run whole lifecycles of executions. The `Task`
/// structure contains the elements needed to create and run an execution. It also provides a
/// `.execute()` method which consumes the structure while performing the lifecycle. A `Taskqueue`
/// allows to run multiple such tasks in parallel, thanks to a pool of workers which consume
/// tasks from a queue.

// IMPORTS
use super::async;
use std::sync::{Arc, Mutex};
use std::fmt;
use chrono::prelude::*;
use super::Error;
use super::repository::{Campaign, Execution};
use super::run::{RunConfig, LeaveConfig};

// STRUCTURES
/// Contains the elements needed for a worker to perform a complete execution lifecycle thanks to
/// the `.execute()` method. This lifecycle will consist in:
/// + Creating the execution folder if it is not given
/// + Run the code on a remote host
/// + Retrieve the results
/// + Finish the execution
/// A task can both be created from an execution using `.from_execution()` or from scratch with
/// `.new()` in which case, the execution creation will be handled.
#[derive(Debug)]
pub struct Task {
    // The campaign, protected by a mutex, since all tasks will access it. This allows to avoid
    // data race conditions on the file system
    campaign_repo: Arc<Mutex<Campaign>>,
    // The parameters string
    parameters: String,
    // The commit hash
    commit: Option<String>,
    // The runaway profile name
    profile: String,
    // The execution if it is given
    execution: Option<Execution>,
}

impl Task {
    /// Creates a task from scratch
    pub fn new(campaign_repo: Arc<Mutex<Campaign>>, parameters: String, commit: Option<String>, profile: String) -> Task {
        Task {
            campaign_repo,
            parameters,
            commit,
            profile,
            execution: None,
        }
    }

    /// Creates a task from an existing execution. This is used in the case of an execution rerun,
    /// when the execution was reset.
    pub fn from_execution(campaign_repo: Arc<Mutex<Campaign>>, execution: Execution, profile: String) -> Task {
        Task {
            campaign_repo,
            parameters: execution.get_parameters().to_owned(),
            commit: Some(execution.get_commit().to_owned()),
            profile,
            execution: Some(execution),
        }
    }

    /// Performs an execution lifecycle. Called by a worker.
    pub fn execute(self) {
        // We create the execution. Note that aquire the campaign repo in a closed scope,
        // so as to be sure to release the mutex right after the scope (to not block it
        // for other threads). This is safe since Execution doesn't keep a reference to it.
        let mut execution = match self.execution {
            None => {
                let cmp_repo = self.campaign_repo.lock().unwrap();
                cmp_repo.new_execution(self.commit, self.parameters).expect("Failed to generate execution")
            }
            Some(execution) => execution
        };
        // We start timer
        let execution_start = Local::now();
        // We retrieve the runaway configuration
        let runconf = RunConfig {
            script_path: execution.get_path().join(super::SCRIPT_RPATH),
            profile: self.profile,
            parameters: execution.get_parameters().to_owned(),
        };
        // We run the config
        let (stdout, stderr, ecode): (String, String, u32) = match runconf.execute(LeaveConfig::Code, true) {
            Err(Error::ExecutionFailed(ref output)) => {
                let stdout = String::from_utf8(output.stdout.clone()).unwrap();
                let stderr = String::from_utf8(output.stderr.clone()).unwrap();
                let ecode = output.status.code().unwrap() as u32;
                (stdout, stderr, ecode)
            }
            Err(e) => {
                let stdout = String::new();
                let stderr = format!("Runaway execution failed:\n{}", e);
                let ecode = 911;
                (stdout, stderr, ecode)
            }
            Ok(output) => {
                let stdout = String::from_utf8(output.stdout).unwrap();
                let stderr = String::from_utf8(output.stderr).unwrap();
                let ecode: u32 = output.status.code().unwrap_or(0) as u32;
                (stdout, stderr, ecode)
            }
        };
        // We set the information
        execution.set_executor(&runconf.profile);
        execution.set_execution_date(Local::now().to_rfc3339().as_str());
        execution.set_execution_duration({
            let duration = Local::now() - execution_start;
            duration.num_seconds() as u32
        });
        execution.set_execution_stdout(stdout.as_str());
        execution.set_execution_stderr(stderr.as_str());
        execution.set_execution_exit_code(ecode);
        // We finish the execution
        {
            let cmp_repo = self.campaign_repo.lock().unwrap();
            if cmp_repo.finish_execution(&mut execution).is_err(){
                warn!("Failed to finish execution {}", execution);
                return;
            }
        }
        // We pull push the execution
        {
            let cmp_repo = self.campaign_repo.lock().unwrap();
            for _ in 0..5 {
                if cmp_repo.pull().is_ok() && cmp_repo.push().is_ok(){
                    return ;
                }
                else{
                    warn!("Failed to push the execution {}. Retrying ...", execution);
                }
            }
            warn!("Failed to push the execution {}. Giving up ... ", execution);
        }
    }
}

impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Task <{} {}>@{}#{}", super::SCRIPT_RPATH, self.parameters, self.commit.as_ref().unwrap_or(&String::from("None")), self.profile)
    }
}

/// Allows to queue different tasks which are asynchronously consumed by a pool of workers.
#[derive(Debug)]
pub struct TaskQueue {
    queue: Arc<Mutex<Vec<Task>>>,
    workers: Vec<async::Worker>,
}

impl TaskQueue {

    /// Create a new task queue with a given number of workers.
    pub fn new(n_workers: u32) -> TaskQueue {
        // We initialize the queue
        let queue: Arc<Mutex<Vec<Task>>> = Arc::new(Mutex::new(Vec::new()));
        // The worker need a ` fn(Task)` to perform on the task. We define one
        fn execute_task(task: Task) {
            task.execute();
        }
        let function_ptr: fn(Task) = execute_task;
        // We generate the workers pool
        let workers: Vec<_> = (0..n_workers).map(|_| {
            async::Worker::new(Arc::clone(&queue), function_ptr)
        }).collect();
        return TaskQueue {
            queue,
            workers,
        };
    }

    /// Adds a task to the queue
    pub fn push(&mut self, task: Task) {
        self.queue.lock().unwrap().push(task);
    }

    /// Wait for the tasks to all have been consumed by workers.
    pub fn wait(&self) {
        loop {
            let do_break = {
                let is_queue_empty = self.queue.lock().unwrap().len() == 0;
                let are_threads_idle = self.workers.iter().all(|w| w.is_idle());
                is_queue_empty && are_threads_idle
            };
            if do_break {
                break;
            }
        };
    }

    /// Returns the number of elements in the queue
    pub fn len(&self) -> usize {
        return self.queue.lock().unwrap().len();
    }

    /// Returns a bool stating if the queue is empty
    pub fn is_empty(&self) -> bool {
        return self.len() == 0;
    }

    /// Returns a string representing the workers pool.
    pub fn get_workers_string(&self) -> String {
        return self.workers.iter().map(|w| format!("{}", w)).fold(String::new(), |acc, r| acc + " ¤ " + &r);
    }

    /// Returns a string representing the queue
    pub fn get_tasks_string(&self) -> String {
        return self.queue.lock().unwrap().iter().map(|t| format!("{}", t)).fold(String::new(), |acc, r| acc + " ¤ " + &r);
    }
}

impl fmt::Display for TaskQueue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TaskQueue [{}] ({})", self.get_workers_string(), self.get_tasks_string())
    }
}

impl Drop for TaskQueue {
    // This allows to clean the task queue properly when it goes out of scope.
    fn drop(&mut self) {
        loop {
            match self.workers.pop() {
                Some(w) => w.stop(),
                None => break,
            }
        }
    }
}

// TESTS
#[cfg(test)]
mod tests {

    use super::*;
    use std::fs;
    use std::path;
    use super::super::git;
    use super::super::repository;

    static TEST_PATH: &str = include_str!("../../test/constants/test_path");
    static CAMPAIGN_REPOSITORY_NAME: &str = include_str!("../../test/constants/campaign_repository_name");
    static CAMPAIGN_REPOSITORY_URL: &str = include_str!("../../test/constants/campaign_repository_url");


    #[test]
    fn test_task() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/tasks/task");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(CAMPAIGN_REPOSITORY_NAME)).unwrap();
        }
        git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path).unwrap();
        let cmp = Arc::new(Mutex::new(Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME)).expect("Failed to open repository.")));
        {
            let campaign = cmp.lock().unwrap();
            let mut removes: Vec<_> = campaign.get_executions_path()
                .read_dir()
                .unwrap()
                .map(|r| r.unwrap().path())
                .filter(|r| r.is_dir())
                .map(|p| Execution::from_path(&p).unwrap())
                .collect();
            if removes.len() != 0 {
                while let Some(exec) = removes.pop() {
                    campaign.remove_execution(exec).unwrap();
                }
                campaign.push().unwrap();
            }
        }
        let cmp_c = Arc::clone(&cmp);
        let task = Task::new(cmp_c, String::from(""), None, String::from("localhost"));
        task.execute();
        let executions_after: Vec<_> = cmp.lock().unwrap().get_executions_path()
            .read_dir()
            .unwrap()
            .map(|r| r.unwrap().path())
            .filter(|r| r.is_dir())
            .map(|p| Execution::from_path(&p).unwrap())
            .collect();
        assert_eq!(executions_after.len(), 1);
        assert!(executions_after.iter().all(|e| e.get_state() == repository::ExecutionState::Finished));
    }

    #[test]
    fn test_taskqueue() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/tasks/taskqueue");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if test_path.join(CAMPAIGN_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(CAMPAIGN_REPOSITORY_NAME)).unwrap();
        }
        git::clone_remote_repo(CAMPAIGN_REPOSITORY_URL, &test_path).unwrap();
        let cmp = Arc::new(Mutex::new(Campaign::from_path(&test_path.join(CAMPAIGN_REPOSITORY_NAME)).expect("Failed to open repository.")));
        {
            let campaign = cmp.lock().unwrap();
            let mut removes: Vec<_> = campaign.get_executions_path()
                .read_dir()
                .unwrap()
                .map(|r| r.unwrap().path())
                .filter(|r| r.is_dir())
                .map(|p| Execution::from_path(&p).unwrap())
                .collect();
            if removes.len() != 0 {
                while let Some(exec) = removes.pop() {
                    campaign.remove_execution(exec).unwrap();
                }
                campaign.push().unwrap();
            }
        }
        let mut taskqueue = TaskQueue::new(10);
        for _ in 0..10 {
            let cmp_c = Arc::clone(&cmp);
            let task = Task::new(cmp_c, String::from(""), None, String::from("localhost"));
            taskqueue.push(task);
        }
        taskqueue.wait();
        let executions_after: Vec<_> = cmp.lock().unwrap().get_executions_path()
            .read_dir()
            .unwrap()
            .map(|r| r.unwrap().path())
            .filter(|r| r.is_dir())
            .map(|p| Execution::from_path(&p).unwrap())
            .collect();
        assert_eq!(executions_after.len(), 10);
        assert!(executions_after.iter().all(|e| e.get_state() == repository::ExecutionState::Finished));
        drop(taskqueue);
    }
}