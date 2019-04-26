// liborchestra/hosts.rs
// Author: Alexandre Péré
/// Th

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use std::{path, process, fs, io, str, io::prelude::*, env, error, fmt};
use super::{SEND_ARCH_RPATH,
            PROFILES_FOLDER_RPATH,
            SEND_IGNORE_RPATH,
            FETCH_IGNORE_RPATH,
            FETCH_ARCH_RPATH,
            SSH_CONFIG_RPATH};
use std::sync::{Arc, mpsc, Mutex};
use crossbeam::channel::{TryRecvError, Sender, Receiver, unbounded};
use std::thread;
use uuid::Uuid;
use chrono::prelude::*;
use dirs;
use std::collections::HashMap;
use crate::ssh;
use crate::misc;
use crate::ssh::RemoteConnection;
use crate::derive_from_error;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug)]
pub enum Error {
    // Leaf Errors
    SshProfile(String),
    ReadingHost(String),
    WritingHost(String),
    AllocationFailed(String),
    // Chaining Errors
    Io(io::Error),
    Ssh(ssh::Error),
    SshConfigParse(ssh::config::Error),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::SshProfile(ref s) => write!(f, "An error occurred while reading the ssh config: \n{}", s),
            Error::ReadingHost(ref s) => write!(f, "An error occurred while reading the host: \n{}", s),
            Error::WritingHost(ref s) => write!(f, "An error occurred while writing the host: \n{}", s),
            Error::AllocationFailed(ref s) => write!(f, "Failed to allocate: \n{}", s),
            Error::Ssh(ref e) => write!(f, "An ssh-related error occurred: \n{}", e),
            Error::Io(ref e) => write!(f, "An io-related error occurred: \n{}", e),
            Error::SshConfigParse(ref e) => write!(f, "A ssh config parsing-related error occurred: \n{}", e)
        }
    }
}

derive_from_error!(Error, ssh::Error, Ssh);
derive_from_error!(Error, io::Error, Io);
derive_from_error!(Error, ssh::config::Error, SshConfigParse);

////////////////////////////////////////////////////////////////////////////// ENVIRONMENT VARIABLES
pub struct EnvironmentVariables(HashMap<String, String>);

impl EnvironmentVariables{
    pub fn new() -> EnvironmentVariables{
        return EnvironmentVariables(HashMap::new());
    }

    pub fn insert(&mut self, k: String, v: String) -> Option<String>{
        self.0.insert(k, v)
    }

    pub fn clear(&mut self) {
        self.0.clear()
    }

    pub fn substitute(&self, string: &str) -> String{
        self.0.iter()
            .fold(string.to_owned(), |string, (k, v)|{string.replace(k, v)})
    }
}


///////////////////////////////////////////////////////////////////////////////////////// STRUCTURES
#[derive(Serialize, Deserialize, Debug, Hash)]
struct HostConf {
    name: String,
    ssh_config: String,
    node_proxy_command: String,
    start_alloc: String,
    get_alloc_nodes: String,
    cancel_alloc: String,
    alloc_duration: usize,
    executions_per_nodes: usize,
    directory: path::PathBuf,
    before_execution: String,
    after_execution: String,
}

impl HostConf {
    /// Load an host configuration from its path
    pub fn from_file(host_path: &path::PathBuf) -> Result<HostConf, Error> {
        debug!("Loading host from {}", host_path.to_str().unwrap());
        let file = fs::File::open(host_path)
            .map_err(|_| Error::ReadingHost(format!("Failed to open host configuration file {}",
                                                      host_path.to_str().unwrap())))?;
        let mut config: HostConf = serde_yaml::from_reader(file)
            .map_err(|_| Error::ReadingHost(format!("Failed to parse host configuration file {}",
                                                      host_path.to_str().unwrap())))?;
        return Ok(config);
    }

    /// Writes execution to file.
    pub fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error>{
        debug!("Writing host configuration {:?} to file", self);
        let file = fs::File::create(conf_path)
            .map_err(|_| Error::WritingHost(format!("Failed to open host configuration file {}",
                                                      conf_path.to_str().unwrap())))?;;
        serde_yaml::to_writer(file, &self)
            .map_err(|_| Error::WritingHost(format!("Failed to write host configuration file {}",
                                                      conf_path.to_str().unwrap())))?;;
        Ok(())
    }

}

#[derive(Debug)]
pub enum LeaveConfig{
    Nothing,
    Code,
    Everything,
}

impl<'a> From<&'a str> for LeaveConfig{
    fn from(conf: &str) -> LeaveConfig{
        match conf{
            "nothing" => LeaveConfig::Nothing,
            "code" => LeaveConfig::Code,
            "everything" => LeaveConfig::Everything,
            _ => panic!("Unknown LeaveConfig input encountered {}", conf),
        }
    }
}

///////////////////////////////////////////////////////////////////////////////////////// PRIMITIVES

pub trait UseResource<R>{
    fn make_progress(&mut self, resource: &mut R) -> Result<(), Error>;
}

//make macro to derive operations newtypes

/////////////////////////////////////////////////////////////////////////////////////////////// HOST

/// This structure allows to perform the basic needed operations on the host.
pub struct Host {
    conf: HostConf,
    conn: ssh::RemoteConnection,
    pool: Vec<ssh::RemoteConnection>,
    vars: EnvironmentVariables,
}

impl Host{
    fn from(conf: HostConf) -> Result<Host, Error>{
        debug!("Host: Creating Host from HostConf: {:?}", conf);
        let profile = ssh::config::get_profile(&dirs::home_dir()
            .unwrap()
            .join(SSH_CONFIG_RPATH),
                                               &conf.ssh_config)?;
        trace!("Host: Profile retrieved: {:?}", profile);
        let conn = ssh::RemoteConnection::from_profile(profile)?;
        trace!("Host: Connection acquired: {:?}", conn);
        let pool = Vec::new();
        let vars = EnvironmentVariables(HashMap::new());
        return Ok(Host{conf, conn, pool, vars});
    }

    fn start_alloc(&mut self) -> Result<(), Error>{
        debug!("Host: Starting allocation on {:?}", self.conn);
        trace!("Host: Trying to acquire...");
        let alloc_output: std::process::Output =
            futures::executor::block_on(self.conn.async_exec(&self.conf.start_alloc))?;
        let alloc_id = match alloc_output.status.code(){
            Some(0) => {
                trace!("Host: Allocation succeeded: {:?}", alloc_output);
                String::from_utf8(alloc_output.stdout).unwrap().trim_end_matches('\n').to_owned()
            }
            _ => {
                trace!("Host: Allocation Failed: {:?}", alloc_output);
                return Err(Error::AllocationFailed(format!("Failed to obtain allocation on profile {}: {:?}",
                                                           self.conf.name,
                                                           alloc_output)))
            }
        };
        trace!("Host: Trying to get nodes...");
        self.vars.insert("$ALLOCRET".to_owned(), alloc_id);
        let get_nodes_command = self.vars.substitute(&self.conf.get_alloc_nodes);
        let nodes_output: std::process::Output =
            futures::executor::block_on(self.conn.async_exec(&get_nodes_command))
                .map_err(|e| Error::AllocationFailed(format!("Failed to get nodes: {}", e)))?;
        let nodes = match nodes_output.status.code(){
            Some(0) => {
                trace!("Host: Nodes string obtained: {:?}", nodes_output);
                String::from_utf8(nodes_output.stdout).unwrap().trim_end_matches('\n').to_owned()
            }
            _ => {
                trace!("Host: Failed to acquire nodes: {:?}", nodes_output);
                return Err(Error::AllocationFailed(format!("Failed to get node on profile {}: {:?}",
                                                           self.conf.name,
                                                           nodes_output)))
            }
        };
        let profile = ssh::config::get_profile(&dirs::home_dir().unwrap().join(SSH_CONFIG_RPATH),
                                               &self.conf.ssh_config)?;
        trace!("Host: Connecting to nodes...");
        self.pool = nodes.split("\n")
            .inspect(|n| trace!("Host: Connecting to {}", n))
            .map(|n|{
                let mut config = profile.clone();
                let cmd = self.conf.node_proxy_command
                    .replace("$NODENAME", n);
                config.proxycommand.replace(cmd);
                config.hostname.replace(format!("{}::{}", config.name, n));
                trace!("Host: Node configuration: {:?}", config);
                ssh::RemoteConnection::from_profile(config)
                    .map_err(|e| Error::from(e))
            })
            .collect::<Result<Vec<ssh::RemoteConnection>, Error>>()?;
        Ok(())
    }

    fn cancel_alloc(&mut self) -> Result<(), Error>{
        let cancel_alloc = self.vars.substitute(&self.conf.cancel_alloc);
        let cancel_output: std::process::Output =
            futures::executor::block_on(self.conn.async_exec(&cancel_alloc))?;
        match cancel_output.status.code(){
            Some(0) => {
                self.pool.clear();
                self.vars.clear();
                return Ok(());
            }
            _ => {
                return Err(Error::AllocationFailed(format!("Failed to cancel allocation on profile {}: {:?}",
                                                           self.conf.name,
                                                           cancel_output)))
            }
        };
    }

    fn try_acquire(&mut self) -> Option<RemoteConnection>{
        self.pool.iter()
            .filter(|conn| conn.strong_count() < self.conf.executions_per_nodes as usize +1)
            .nth(0)
            .map(|c| c.clone())
    }

    fn nodes_available(&self) -> bool{
        self.pool.iter()
            .any(|conn| conn.strong_count() < self.conf.executions_per_nodes +1)
    }

    fn is_allocated(&self) -> bool {
        !self.pool.is_empty()
    }

    fn is_full(&self) -> bool{
        self.is_allocated() && !self.nodes_available()
    }

    fn is_free(&self) -> bool{
        self.is_allocated() && self.pool.iter().all(|conn| conn.strong_count() == 1)
    }
}

/*

///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS
// Remote operations are messages sent and retrieve from the future to the connection thread. The
// remote connection will iterate over the `Progressing(progress_data)` by adding the data gathered
// from the operation in `progress_data`.
// See the different `handle_(some operation name)_operation)` functions of `RemoteConnection` for
// more information.

#[doc(hidden)]
#[derive(Debug)]
// The different operations that can be performed
enum HostOperation{
    Acquire(AcquireOp),
}

// The different states an operation may lie in.
enum OperationState<I, P, O, E>{
    Starting(I),
    Progressing(P),
    Succeeded(O),
    Failed(E),
}

impl<I, P, O, E> OperationState<I, P, O, E>{
    // Starts a new operation from a given input.
    fn from_input(input: I) -> OperationState<I, P, O, E>{
        return OperationState::Starting(input);
    }
    // Transitions from `Starting` to `Transitioning` state by taking the input values.
    fn begin(&mut self) -> I{
        match std::mem::replace(self.state, OperationState::Transitioning){
            OperationState::Starting(input) => input,
            _ => panic!("Operation began in a wrong state.")
        }
    }
    // Transitions from `Transitioning` to `Progressing` state by saving the progress value.
    fn save(&mut self, progress: P) {
        match std::mem::replace(self, OperationState::Progressing(progress)){
            OperationState::Transitioning => {},
            _ => panic!("Operation succeeded in a wrong state.")
        }
    }
    // Transitions from `Progressing` to `Transitioning` state by taking the progress value.
    fn resume(&mut self) -> P {
        match std::mem::replace(self, OperationState::Transitioning) {
            OperationState::Progressing(progress) => progress,
            _ =>panic!("Operation resumed in a wrong state.")
        }
    }
    // Transitions from `Transitioning` to `Succeeded` state by saving the output value.
    fn succeed(&mut self, output: O){
        match std::mem::replace(self, OperationState::Succeeded(output)){
            OperationState::Transitioning => {},
            _ => panic!("Operation succeeded in a wrong state.")
        }
    }
    // Transition from `Transitioning` to `Failed` state by saving the error value.
    fn fail(&mut self, error: E){
        match std::mem::replace(self, OperationState::Failed(error)){
            OperationState::Transitioning => {},
            _ => panic!("Operation succeeded in a wrong state.")
        }
    }
    // Consumes a finished operation (either in `Succeeded` or `Failed` state) to produce a result
    // out of the operation.
    fn into_result(mut self) -> Result<O, E> {
        match std::mem::replace(&mut self, OperationState::Transitioning){
            OperationState::Succeeded(output) => Ok(output),
            OperationState::Failed(error) => Err(error),
            _ => panic!("Operation turned to result in a wrong state.")
        }
    }
    // Returns whether the operation is in a final stage.
    fn is_over(&self) -> bool{
        match self{
            OperationState::Succeeded(_) | OperationState::Failed(_) => true,
            _ => false
        }
    }
    // Returns whether the operation is progressing.
    fn is_progressing(&self) -> bool{
        match self{
            OperationState::Progressing(_) => true,
            _ => false
        }
    }
    // Returns whether the operation is starting.
    fn is_new(&self) -> bool{
        match self{
            OperationState::Starting(_) => true,
            _ => false
        }
    }
}

// The base of an operation carried by the host.
struct Operation<I, P, O, E>{
    state: OperationState<I, P, O, E>,
    id: Uuid,
    waker: Waker,
    sender: Sender<Operation<I, P, O, E>>,
    callback: Fn(OperationState<I, P, O, E>) -> ()
}


impl Operation<I,P,O,E> {
    fn from(input: I, waker: Waker, callback: (Fn(OperationState<I, P, O, E>) -> ()))
        -> (Receiver<Operation<I, P, O, E>>, Operation<I,P, O, E>){
        let state = OperationState::from(input);
        let (receiver, sender): (Receiver<Operation<I, P, O, E>>, Sender<Operation<I, P, O, E>>) =
            unbounded();
        return (receiver,
                Operation {
                    state,
                    id: Uuid::new_v4(),
                    waker,
                    sender,
                    callback
                })
    }
}


///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS
pub enum HostOps {
    Acquire(Operation<(), (), String, String>),
}

//////////////////////////////////////////////////////////////////////////////////////////// HANDLER

// The different states the handler may lie in.
pub enum HostHandlerState {
    Idle,
    Available(Time, AllocationTable),
    Pending(Time, AllocationTable),
    Locked(AllocationTable),
}

// A handler that may not perform handle the operations in the same way depending on its state.
pub struct HostHandler {
    state: HandlerState,
    host: Host,
    receiver: mpsc::Receiver<Operations>,
    queue: Vec<HostOps>,
    disconnected: bool,
}

impl HostHandler{
    fn spawn() -> mpsc::Sender<Operations>{

    }

    fn step(mut self) -> HostHandler{
        use HostHandlerState::*;
        self.state = match self{
            HostHandler{state: Idle, receiver, ..} if receiver.is_empty() => {
                Idle
            },
            HostHandler{state: Idle, host, ..} => {
                let hosts = host.allocate();
                let time = now();
                Allocated(time, hosts)
            },
            HostHandler{state: Available(time, hosts), ..} if time.has_expired() =>{
                Locked(hosts)
            },
            HostHandler{state: Available(time, hosts), ..} if !hosts.available() =>{
                Pending(time, hosts)
            }
            HostHandler {state: Available(time, hosts), ..} => {
                Available(time, hosts)
            },
            HostHandler {state: Locked(hosts), ..} if hosts.all_over() =>{
                Idle
            },
            HostHandler {state: Exiting, .. } => {
                Exiting
            }
        };
        return self
    }

    fn drain_channel(&mut self){
        while let Some(ope) = self.receiver.recv(){
            self.vec.insert(0, ope)
        }
    }

    fn handle_loop(&mut self) {
        while !self.disconnected{
            self.step();
            self.drain_channel();
            match self.recv(){
                Some(ope) => self.dispatch(ope),
                Err(TryRecvError::Disconnected) => {self.disconnected = true},
                Err(TryRecvError::Empty) => continue
            }
        }
    }

    fn dispatch(&mut self, ope: HostOps){
        use HostOps::*;
        use HostHandlerState::*;
        match (self.state, ope) {
            (Idle, _) | (Pending(..), _) | (Locked(..), _) => self.handle_reschedule(ope),
            (Available(..), Acquire(OperationState::Starting(_))) => self.handle_starting_acquire(ope),
            (Available(..), Acquire(OperationState::Progressing(_))) => self.handle_progressing_acquire(ope),
            (Available(..), Acquire(OperationState::Succeeded(_))) => self.handle_succeeded_acquire(ope),
            (Available(..), Acquire(OperationState::Failed(_))) => self.handle_failed_acquire(ope),
        }
    }

    fn handle_reschedule(&mut self, ope: HostOps){
        self.queue.push(ope);
    }

    fn handle_starting_acquire(&mut self, ope: HostOps){

    }
}
*/

// TESTS
#[cfg(test)]
mod test {

    use super::*;
    use env_logger;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_host_conf() {
        let conf = HostConf{
            name: "plafrim-court".to_owned(),
            ssh_config: "plafrim-ext".to_owned(),
            node_proxy_command: "ssh -A -l apere plafrim-ext -W $NODENAME:22".to_owned(),
            start_alloc: "module load slurm && (salloc -N10 -p court --no-shell 2>&1 | sed -e \"s/salloc: Granted job allocation //\")".to_owned(),
            get_alloc_nodes: "while read LINE; do scontrol show hostnames \"$LINE\"; done <<< $(squeue -j $ALLOCRET -o \"%N\" | sed '1d' -E)".to_owned(),
            cancel_alloc: "scancel -j $ALLOCRET".to_owned(),
            alloc_duration: 10000,
            executions_per_nodes: 10,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        conf.to_file(&path::PathBuf::from("/tmp/test_host.yml"));
    }

    #[test]
    fn test_host_conf_from_file() {
        init();
        let config = HostConf::from_file(&path::PathBuf::from("/tmp/test_host.yml"));
        eprintln!("config = {:#?}", config);
    }

    #[test]
    fn test_host(){

        init();

        let conf = HostConf{
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:22".to_owned(),
            start_alloc: "".to_owned(),
            get_alloc_nodes: "echo localhost".to_owned(),
            cancel_alloc: "".to_owned(),
            alloc_duration: 10000,
            executions_per_nodes: 2,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        let mut host = Host::from(conf).unwrap();
        assert!(!host.is_allocated());
        assert!(!host.is_free());
        assert!(!host.is_full());
        assert!(!host.nodes_available());
        host.start_alloc().unwrap();
        assert!(host.is_allocated());
        assert!(host.nodes_available());
        assert!(host.is_free());
        assert!(!host.is_full());
        let a = host.try_acquire().unwrap();
        assert!(host.nodes_available());
        assert!(!host.is_full());
        assert!(!host.is_free());
        let b = host.try_acquire().unwrap();
        assert!(!host.is_free());
        assert!(!host.nodes_available());
        assert!(host.is_full());
        assert!(host.try_acquire().is_none());
        host.cancel_alloc().unwrap();
        assert!(!host.is_allocated());
        assert!(!host.is_free());
        assert!(!host.is_full());
        assert!(!host.nodes_available());


    }



}

