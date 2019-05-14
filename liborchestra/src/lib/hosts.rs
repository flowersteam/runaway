// liborchestra/hosts.rs
// Author: Alexandre Péré
/// This module contains structure that manages host allocations. The resulting tool is the
/// HostResource, which given an host configuration provide asynchronous nodes allocation. Put
/// differently, it allows to await a node to be available for computation, given the restrictions
/// of the configuration. The allocation are automatically started and revoked.
// If you have troubles understanding how pieces fit, check the primitives module test.
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
use std::time::{Instant, Duration};
use uuid::Uuid;
use chrono::prelude::*;
use dirs;
use std::collections::HashMap;
use crate::ssh;
use crate::misc;
use crate::ssh::RemoteConnection;
use crate::derive_from_error;
use crate::primitives::{
    UseResource,
    Operation,
    StartingOperation,
    ProgressingOperation,
    FinishedOperation,
    OperationFuture,
    Dropper
};
use crate::stateful::{
    TransitionsTo,
    Stateful,
};
use std::hint::unreachable_unchecked;
use std::thread::JoinHandle;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    SshProfile(String),
    ReadingHost(String),
    WritingHost(String),
    AllocationFailed(String),
    HostResourceCrashed(String),
    AcquireNodeFailed(String),
    // Chaining Errors
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
            Error::HostResourceCrashed(ref s) => write!(f, "Host resource crash caused from error: \n{}", s),
            Error::AcquireNodeFailed(ref s) => write!(f, "Node acquisition failed: \n{}", s),
            Error::Ssh(ref e) => write!(f, "An ssh-related error occurred: \n{}", e),
            Error::SshConfigParse(ref e) => write!(f, "A ssh config parsing-related error occurred: \n{}", e)
        }
    }
}

impl From<Error> for crate::primitives::Error{
    fn from(other: Error) -> crate::primitives::Error{
        crate::primitives::Error::Operation(format!("{}", other))
    }
}

derive_from_error!(Error, ssh::Error, Ssh);
derive_from_error!(Error, ssh::config::Error, SshConfigParse);

////////////////////////////////////////////////////////////////////////////// ENVIRONMENT VARIABLES

// A structure that holds environment variables, and substitute them in strings.
struct EnvironmentVariables(HashMap<String, String>);

impl EnvironmentVariables{
    // Creates a new one
    fn new() -> EnvironmentVariables{
        return EnvironmentVariables(HashMap::new());
    }

    // Insert a variable
    fn insert(&mut self, k: String, v: String) -> Option<String>{
        self.0.insert(k, v)
    }

    // Clear all variables
    fn clear(&mut self) {
        self.0.clear()
    }

    // Substitute variables in a string
    fn substitute(&self, string: &str) -> String{
        self.0.iter()
            .fold(string.to_owned(), |string, (k, v)|{string.replace(k, v)})
    }
}


///////////////////////////////////////////////////////////////////////////////////////// STRUCTURES

/// A host configuration represents the implementation of the (imaginary) orchestra host interface
/// for a given host. It can write to/read from yaml files.
// Todo: Implement the hash so that it can be insensitive to the name.
// Todo: Document the fact that -F configfile must be added to the nodes proxycommand
#[derive(Serialize, Deserialize, Debug, Hash)]
pub struct HostConf {
    pub name: String, // The name of the configuration
    pub ssh_config: String, // The name of the ssh config used (found in SSH_CONFIG_RPATH)
    pub node_proxy_command: String, // The proxycommand used to access the nodes.
    pub start_alloc: String, // Command to start allocation. Should return an identifier kept in $ALLOCRET
    pub get_alloc_nodes: String, // Command to get nodes of _this_ allocation. Use $ALLOCRET.
    pub cancel_alloc: String, // Command to cancel _this_ allocation. Use $ALLOCRET.
    pub alloc_duration: usize, // Number of seconds after which nodes will no longer be issued.
    pub executions_per_nodes: usize, // Number of parallel executions on a node.
    pub directory: path::PathBuf, // The directory where to put the executions
    pub before_execution: String, // Command to execute before the execution
    pub after_execution: String, // Command to execute after the execution
}

impl HostConf {
    /// Load an host configuration from a file.
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

    /// Writes host configuration to a file.
    fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error>{
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

// Represents what should be kept on the host once the execution is done.
#[derive(Debug)]
enum LeaveConfig{
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

/////////////////////////////////////////////////////////////////////////////////////////////// HOST

// This structure is the executor of an host configuration. It connects to the host and allows to
// perform the needed operations to provide execution nodes, under the forms of RemoteConnection
// objects. It holds a main host connection, which is used to send the various command needed for
// allocation management. When it is allocated, the pool field is filled with RemoteConnection that
// each represent a specific node. The try acquire gives a node clone under the restrictions
// (executions per nodes) of the configuration. It is synchronous over the host (main) connection.
struct Host {
    conf: HostConf,
    conn: ssh::RemoteConnection,
    pool: Vec<ssh::RemoteConnection>,
    vars: EnvironmentVariables,
}

impl Host{
    // Builds a host from a configuration.
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

    // Starts an allocation.
    fn start_alloc(&mut self) -> Result<(), Error>{
        debug!("Host: Starting allocation on {:?}", self.conn);
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

    // Cancel the current allocation
    fn cancel_alloc(&mut self) -> Result<(), Error>{
        debug!("Host: Cancelling allocation...");
        let cancel_alloc = self.vars.substitute(&self.conf.cancel_alloc);
        let cancel_output: std::process::Output =
            futures::executor::block_on(self.conn.async_exec(&cancel_alloc))?;
        match cancel_output.status.code(){
            Some(0) => {
                trace!("Host: Allocation cancelling succeeded");
                self.pool.clear();
                self.vars.clear();
                return Ok(());
            }
            _ => {
                trace!("Host: Allocation cancelling failed: {:?}", cancel_output);
                return Err(Error::AllocationFailed(format!("Failed to cancel allocation on profile {}: {:?}",
                                                           self.conf.name,
                                                           cancel_output)))
            }
        };
    }

    // Tries to acquire a node connection
    fn try_acquire(&mut self) -> Option<RemoteConnection>{
        debug!("Host: Trying to acquire a connection.");
        self.pool.iter()
            .filter(|conn| conn.strong_count() < self.conf.executions_per_nodes as usize +1)
            .nth(0)
            .map(|c| c.clone())
    }

    // Check whether nodes are available
    fn nodes_available(&self) -> bool{
        self.pool.iter()
            .any(|conn| conn.strong_count() < self.conf.executions_per_nodes +1)
    }

    // Checks whether host is allocated
    fn is_allocated(&self) -> bool {
        !self.pool.is_empty()
    }

    // Checks whether all nodes are used
    fn is_full(&self) -> bool{
        self.is_allocated() && !self.nodes_available()
    }

    // Checks whether all nodes are free
    fn is_free(&self) -> bool{
        self.is_allocated() && self.pool.iter().all(|conn| conn.strong_count() == 1)
    }
}

/////////////////////////////////////////////////////////////////////////////////////////// RESOURCE
// The Host Resource is special in that it is stateful. It loops through the states:
// Idling -> Allocated -> Locked -> Idling -> Allocated ....
// At any time, it can transition to the absorbing Crashed state, in which case every operation will
// return the same error contained in the crash.

// The allowed states
#[derive(Clone, Debug, State)]
struct HostIdling;
#[derive(Clone, Debug, State)]
struct HostAllocated(Instant);
#[derive(Clone, Debug, State)]
struct HostLocked;
#[derive(Clone, Debug, State)]
struct HostCrashed(Error);

// The allowed transitions
impl TransitionsTo<HostAllocated> for HostIdling {}
impl TransitionsTo<HostLocked> for HostAllocated {}
impl TransitionsTo<HostIdling> for HostLocked {}
impl TransitionsTo<HostCrashed> for HostIdling {}
impl TransitionsTo<HostCrashed> for HostAllocated {}
impl TransitionsTo<HostCrashed> for HostLocked {}

// The type alias for the Host resource operations (a trait object type)
type HostOp = Box<dyn UseResource<HostResource>>;

// The host resource holds the operation queue and the host, and is used to perform the actual
// acquire operation. It is not meant to be created directly, but rather spawned in a separate
// using the spawn method, which contains the loop to handle messages, update state and progress
// operations.
struct HostResource {
    state: Stateful,
    host: Host,
    queue: Vec<HostOp>,
}
impl HostResource{
    fn step(&mut self){
        debug!("HostResource: Stepping {:?}", self.host.conf.name);
        if let Some(HostIdling) = self.state.to_state::<HostIdling>(){
            trace!("HostResource: Found Idling");
            if !self.queue.is_empty(){
                trace!("HostResource: Found operations in queue. Allocating");
                match self.host.start_alloc(){
                    Ok(_) => {
                        trace!("HostResource: Allocation granted :)");
                        self.state.transition::<HostIdling, HostAllocated>(HostAllocated(Instant::now()))
                    },
                    Err(e) => {
                        trace!("HostResource: Allocation failed :(");
                        self.state.transition::<HostIdling, HostCrashed>(HostCrashed(Error::HostResourceCrashed(format!("{}", e))))
                    },
                }
            }
        } else if let Some(HostAllocated(beginning)) = self.state.to_state::<HostAllocated>(){
            trace!("HostResource: Found Allocated");
            if Instant::now().duration_since(beginning) > Duration::from_secs(self.host.conf.alloc_duration as u64){
                trace!("HostResource: Allocation too old. Locking...");
                self.state.transition::<HostAllocated, HostLocked>(HostLocked);
            }
        } else if let Some(HostLocked) = self.state.to_state::<HostLocked>(){
            trace!("HostResource: Found Locked");
            if self.host.is_free(){
                trace!("HostResource: No running jobs encountered. Cancelling allocation...");
                match self.host.cancel_alloc() {
                    Ok(_) => self.state.transition::<HostLocked, HostIdling>(HostIdling),
                    Err(e) => self.state.transition::<HostLocked, HostCrashed>(HostCrashed(Error::HostResourceCrashed(format!("{}", e)))),
                }
            } else {
                trace!("HostResource: Jobs still running. Postponing canceling.")
            }
        } else if let Some(HostCrashed(e)) = self.state.to_state::<HostCrashed>(){
            trace!("HostResource: Found Crashed: {}", e);
        } else {
            unreachable!()
        }
    }
}



/// A handle that allows to create futures to perform operations on the host. Mainly, acquiring
/// nodes. The handle is Clone and can be sent around.
#[derive(Clone)]
pub struct HostResourceHandle{
    sender: Sender<HostOp>,
    dropper: Dropper<()>,
}

impl HostResourceHandle{
    /// Spawns the resource in a separate thread and returns a handle to it.
    pub fn spawn_resource(host_conf: HostConf) -> Result<HostResourceHandle, Error>{
        debug!("HostResourceHandle: Spawning host resource {:?}", host_conf.name);
        let host = Host::from(host_conf)?;
        let (sender, receiver): (Sender<HostOp>, Receiver<HostOp>) = unbounded();
        let handle = std::thread::spawn(move ||{
            trace!("HostResourceThread: Creating resource in thread");
            let mut res = HostResource{
                state: Stateful::from(HostIdling),
                host,
                queue: Vec::new()
            };
            trace!("HostResource: Starting resource loop");
            loop{
                // Handle a message
                match receiver.try_recv(){
                    Ok(o) => {
                        trace!("HostResource: Received operation");
                        res.queue.push(o)
                    },
                    Err(TryRecvError::Empty) => {},
                    Err(TryRecvError::Disconnected) => {
                        trace!("HostResource: Channel disconnected. Leaving...");
                        break
                    },
                }
                // Updates state
                res.step();
                // Handle an operation
                if let Some(s) = res.queue.pop(){
                    s.progress(&mut res);
                }
            }
            trace!("HostResource: Resource loop left.");
            while !res.state.is_state::<HostIdling>(){
                res.step();
            }
            trace!("HostResource: Idling Reached. Leaving thread.");
            return ();
        });
        return Ok(HostResourceHandle{sender, dropper: Dropper::from_handle(handle)});
    }

    /// Returns a future that ultimately resolves to a Result over a RemoteConnection to a node.
    pub fn async_acquire(&self) -> HostAcquireFuture{
        let (recv, op) = HostAcquireOp::from(Stateful::from(StartingOperation(())));
        return HostAcquireFuture::new(op, self.sender.clone(), recv)
    }
}

////////////////////////////////////////////////////////////////////////////////////////// OPERATION
// The only operation is the Acquire operation which allows to get a remote connection to a node.
struct HostAcquireOpM {}
type HostAcquireOp = Operation<HostAcquireOpM>;
impl UseResource<HostResource> for HostAcquireOp{
    fn progress(mut self: Box<Self>, resource: &mut HostResource){
        if let Some(s) = self.state.to_state::<StartingOperation<()>>() {
            trace!("HostAcquire: Found Starting");
            if let Some(HostAllocated(_)) = resource.state.to_state::<HostAllocated>() {
                if resource.host.is_full() {
                    trace!("HostAcquire: All nodes taken. Rescheduling...");
                    resource.queue.push(self);
                } else {
                    trace!("HostAcquire: Node available. Acquiring...");
                    self.state
                        .transition::<StartingOperation<()>, FinishedOperation<_>>
                        (FinishedOperation(resource.host
                            .try_acquire()
                            .ok_or(
                                crate
                                ::primitives
                                ::Error
                                ::from(Error::AcquireNodeFailed("Failed to acquire.".to_owned())))
                        )
                        );
                    resource.queue.push(self);
                }
            } else if let Some(HostCrashed(e)) = resource.state.to_state::<HostCrashed>() {
                trace!("HostAcquire: HostResource found crashed. Finishing...");
                self.state.transition::<StartingOperation<()>, FinishedOperation<RemoteConnection>>
                (FinishedOperation(Err(crate::primitives::Error::from(e))))

            } else {
                trace!("HostAcquire: HostResource Locked or Idle. Rescheduling...");
                resource.queue.push(self);
            }
        } else if let Some(s) = self.state.to_state::<FinishedOperation<RemoteConnection>>(){
            trace!("HostAcquire: Found Finished");
            let waker = self.waker
                .as_ref()
                .expect("No waker given with Host Acquire Operation")
                .to_owned();
            let sender = self.sender
                .clone();
            trace!("HostAcquire: Sending...");
            sender.send(*self);
            trace!("HostAcquire: Waking...");
            waker.wake();
        } else{
            unreachable!()
        }
        trace!("HostAcquire: Progress over.");
    }
}
pub type HostAcquireFuture = OperationFuture<HostAcquireOpM, HostResource, RemoteConnection>;

////////////////////////////////////////////////////////////////////////////////////////////// TESTS
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

    #[test]
    fn test_host_resource(){

        use futures::executor::block_on;

        init();

        let conf = HostConf{
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:22".to_owned(),
            start_alloc: "".to_owned(),
            get_alloc_nodes: "echo localhost".to_owned(),
            cancel_alloc: "".to_owned(),
            alloc_duration: 10,
            executions_per_nodes: 2,
            before_execution: "".to_owned(),
            after_execution: "".to_owned(),
            directory: path::PathBuf::from("/home/apere/Executions"),
        };

        let res_handle = HostResourceHandle::spawn_resource(conf).unwrap();
        let op1 = res_handle.async_acquire();
        let conn1 = block_on(op1);
        println!("conn1: {:?}", conn1);
        assert!(conn1.is_ok());
        thread::sleep_ms(11000);
        drop(conn1);
        let op2 = res_handle.async_acquire();
        let conn2 = block_on(op2);
        let op3 = res_handle.async_acquire();
        let conn3 = block_on(op3);
        println!("conn2: {:?}", conn2);
        assert!(conn2.is_ok());
        println!("conn3: {:?}", conn3);
        assert!(conn3.is_ok());
        thread::sleep_ms(2000);
        std::thread::spawn(||{
            thread::sleep_ms(11000);
            drop(conn2);
            drop(conn3);
            println!("conn2-3 dropped");
            thread::sleep_ms(2000);
        });
        let op4 = res_handle.async_acquire();
        let conn4 = block_on(op4);
        println!("conn4: {:?}", conn4);
        assert!(conn4.is_ok());
        thread::sleep_ms(5000);
    }



}

