//! liborchestra/hosts.rs
//! Author: Alexandre Péré
//!
//! This module contains structure that manages host allocations. The resulting tool is the
//! HostResource, which given an host configuration provide asynchronous nodes allocation. Put
//! differently, it allows to await a node to be available for computation, given the restrictions
//! of the configuration. The allocation are automatically started and revoked.

//------------------------------------------------------------------------------------------ IMPORTS


use crate::derive_from_error;
use crate::primitives::{Dropper, DropBack, Expire};
use crate::ssh;
use crate::ssh::RemoteHandle;
use dirs;
use futures::Future;
use std::{error, fmt, fs, path, str};
use std::collections::HashMap;
use chrono::prelude::*;
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use futures::Stream;
use std::thread;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use std::fmt::{Display, Debug};
use std::pin::Pin;


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    SshProfile(String),
    ReadingHost(String),
    WritingHost(String),
    AllocationFailed(String),
    HostResourceCrashed(String),
    AcquireNodeFailed(String),
    ConnectingNodes(String),
    SpawningThread(String),
    Channel(String),
    OperationFetch(String),
    Aborted,
    Shutdown,
    // Chaining Errors
    Ssh(ssh::Error),
    SshConfigParse(ssh::config::Error),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::SshProfile(ref s) => {
                write!(f, "An error occurred while reading the ssh config: \n{}", s)
            }
            Error::ReadingHost(ref s) => {
                write!(f, "An error occurred while reading the host: \n{}", s)
            }
            Error::WritingHost(ref s) => {
                write!(f, "An error occurred while writing the host: \n{}", s)
            }
            Error::AllocationFailed(ref s) => write!(f, "Failed to allocate: \n{}", s),
            Error::HostResourceCrashed(ref s) => {
                write!(f, "Host resource crash caused from error: \n{}", s)
            }
            Error::AcquireNodeFailed(ref s) => write!(f, "Node acquisition failed: \n{}", s),
            Error::ConnectingNodes(ref s) => write!(f, "Failed to connect to nodes: \n{}", s),
            Error::Ssh(ref e) => write!(f, "An ssh-related error occurred: \n{}", e),
            Error::SshConfigParse(ref e) => {
                write!(f, "A ssh config parsing-related error occurred: \n{}", e)
            }
            Error::SpawningThread(ref s) => write!(f, "Failed to spawn host: \n{}", s),
            Error::Channel(ref s) => write!(f, "A channel related error occurred: \n {}", s),
            Error::OperationFetch(ref s) => write!(f, "Failed to fetch an operation: \n{}", s),
            Error::Aborted => write!(f, "Execution Aborted."),
            Error::Shutdown => write!(f, "Host Shutdown."),
        }
    }
}

impl From<Error> for crate::primitives::Error {
    fn from(other: Error) -> crate::primitives::Error {
        crate::primitives::Error::Operation(format!("{}", other))
    }
}

derive_from_error!(Error, ssh::Error, Ssh);
derive_from_error!(Error, ssh::config::Error, SshConfigParse);

//---------------------------------------------------------------------------- ENVIRONMENT VARIABLES


// A structure that holds environment variables, and substitute them in strings.
struct EnvironmentVariables(HashMap<String, String>);

impl EnvironmentVariables {
    // Creates a new one
    fn new() -> EnvironmentVariables {
        return EnvironmentVariables(HashMap::new());
    }

    // Insert a variable
    fn insert(&mut self, k: String, v: String) -> Option<String> {
        self.0.insert(k, v)
    }

    // Clear all variables
    fn clear(&mut self) {
        self.0.clear()
    }

    // Substitute variables in a string
    fn substitute(&self, string: &str) -> String {
        self.0
            .iter()
            .fold(string.to_owned(), |string, (k, v)| string.replace(k, v))
    }
}


//--------------------------------------------------------------------------------------- STRUCTURES


/// A host configuration represents the implementation of the (imaginary) orchestra host interface
/// for a given host. It can write to/read from yaml files. The fields have the following
/// meaning:
/// + name: The name of the configuration
/// + ssh_config: The name of the ssh config used (found in SSH_CONFIG_RPATH)
/// + node_proxy_command: The proxycommand used to access the nodes.
/// + start_alloc: Command to start allocation. Should return an identifier kept in $ALLOCRET
/// + get_alloc_nodes: Command to get nodes of _this_ allocation. Use $ALLOCRET.
/// + cancel_alloc: Command to cancel _this_ allocation. Use $ALLOCRET.
/// + alloc_durations: Number of minutes after which nodes will no longer be issued.
/// + executions_per_nodes: Number of parallel executions on a node.
/// + directory: The directory where to put the executions
/// + before_execution: Commands to execute before the execution
/// + after_execution: Commands to execute after the execution
#[derive(Serialize, Deserialize, Debug, Hash, Clone)]
pub struct HostConf {
    pub name: String,
    pub ssh_config: String,
    pub node_proxy_command: String,
    pub start_alloc: Vec<String>,
    pub get_alloc_nodes: Vec<String>,
    pub cancel_alloc: Vec<String>, 
    pub alloc_duration: usize, 
    pub executions_per_nodes: Vec<String>,
    pub directory: path::PathBuf, 
    pub before_execution: Vec<String>, 
    pub execution: String,
    pub after_execution: Vec<String>, 
}

impl HostConf {
    /// Load an host configuration from a file.
    pub fn from_file(host_path: &path::PathBuf) -> Result<HostConf, Error> {
        debug!("Loading host from {}", host_path.to_str().unwrap());
        let file = fs::File::open(host_path).map_err(|_| {
            Error::ReadingHost(format!(
                "Failed to open host configuration file {}",
                host_path.to_str().unwrap()
            ))
        })?;
        let config: HostConf = serde_yaml::from_reader(file).map_err(|e| {
            Error::ReadingHost(format!(
                "Failed to parse host configuration file {}: \n{}",
                host_path.to_str().unwrap(), e
            ))
        })?;
        return Ok(config);
    }

    /// Writes host configuration to a file.
    pub fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error> {
        debug!("Writing host configuration {:?} to file", self);
        let file = fs::File::create(conf_path).map_err(|_| {
            Error::WritingHost(format!(
                "Failed to open host configuration file {}",
                conf_path.to_str().unwrap()
            ))
        })?;;
        serde_yaml::to_writer(file, &self).map_err(|_| {
            Error::WritingHost(format!(
                "Failed to write host configuration file {}",
                conf_path.to_str().unwrap()
            ))
        })?;;
        Ok(())
    }

    /// Returns the start_alloc string
    pub fn start_alloc(&self) -> String{
        return self.start_alloc.join(" && ");
    }

    /// Returns the get_alloc_nodes string
    pub fn get_alloc_nodes(&self) -> String{
        return self.get_alloc_nodes.join(" && ");
    }

    /// Returns the cancel_alloc string
    pub fn cancel_alloc(&self) -> String{
        return self.cancel_alloc.join(" && ");
    }

    /// Returns the executions_per_nodes string
    pub fn executions_per_nodes(&self) -> String{
        return self.executions_per_nodes.join(" && ");
    }

    /// Returns the before_execution string
    pub fn before_execution(&self) -> String{
        return self.before_execution.join(" && ");
    }

    /// Returns the execution string
    pub fn execution(&self) -> String{
        return self.execution.clone();
    }

    /// Returns the after_execution string
    pub fn after_execution(&self) -> String{
        return self.after_execution.join(" && ");
    }
}

// Represents what should be kept on the host once the execution is done.
#[derive(Debug, Clone)]
pub enum LeaveConfig {
    Nothing,
    Code,
    Everything,
}

impl<'a> From<&'a str> for LeaveConfig {
    fn from(conf: &str) -> LeaveConfig {
        match conf {
            "nothing" => LeaveConfig::Nothing,
            "code" => LeaveConfig::Code,
            "everything" => LeaveConfig::Everything,
            _ => panic!("Unknown LeaveConfig input encountered {}", conf),
        }
    }
}

//-------------------------------------------------------------------------------------------- NODES

#[derive(Clone, Debug)]
pub struct NodeHandle{
    remote_handle: RemoteHandle,
    pub before_execution: String,
    pub execution: String,
    pub after_execution:String,

}

use std::ops::Deref;

impl Deref for NodeHandle{

    type Target = RemoteHandle;

    fn deref(&self) -> &RemoteHandle {
        &self.remote_handle
    }
}

//--------------------------------------------------------------------------------------------- HOST


use futures::stream;
use crate::SSH_CONFIG_RPATH;
use std::sync::Arc;
use futures::lock::Mutex;
use futures::SinkExt;
use futures::StreamExt;

// Enumeration for the messages in the channel.
#[derive(Clone)] 
enum ChannelMessages{
    NoAllocationsMade,
    Node(DropBack<Expire<NodeHandle>>),
    NoNodesLeft,
    WaitForNodes,
    Abort,
    Shutdown
}

// This structure is the executor of an host configuration. It communicates with the host frontend 
// and allows to perform the necessary operations to provide execution slots, i.e. connections to a 
// node. In particular, the Host structure is responsible for a few key scheduling aspects:
//     + Expiration of resource allocation. The Host periodically revokes and starts new allocations 
//       on the frontend (provided that executions needs slots). This allows to ensure that no 
//       executions will be cut while running. 
//     + Management of the limited nodes slots. Only a few executions are allowed to run on the 
//       same node at once. The Host structure takes care about that.
//     + Management of abort. If the user wants to cancel the remaining executions, then we
//       shouldn't deliver any more nodes slots, and return a specific error. The running executions
//       are still able to run to completion.
//     + Management of shutdown. If the user wants to shut the program down right away, we should 
//       take care about cancelling the allocation on the platform. 
// 
// Implementing those functionalities while retaining the asynchronous aspect of the code is not 
// straightforward. Some details about the implementations:
//     + First, the asynchronous logic is handled in the same way every resources are in the library
//       as documented in the module level documentation.
//     + All the scheduling logic is implemented around the `chan` field, which is a stream of 
//       `ChannelMessages`. It is basically the receiver end of an async channel followed by a 
//       message asking for reallocation. The inner channel carries nodes slots when they are made 
//       available, assuming that the allocation didn't expire. 
//     + Nodes slots are sent as `RemoteHandles` wrapped in the `DropBack` and `Expire` 
//       smart-pointers. The `Expire` smart-pointer is just a wrapper that attaches an expiration 
//       date to a value. This allows to represent the fact that a node slot can be used until a 
//       given time only (when a new allocation must be done). The `DropBack` smart-pointer allows 
//       to send back a value through a channel when it is dropped rather than actually dropping it.
//       This allows to send back non-expired node slots, back into the `chan` channel for further 
//       use by other executions. 
//     + When all nodes slots are expired, the channel is dropped, which triggers the send of the 
//       reallocation message. The execution that encounters this message cancels the allocation 
//       (since the channel was dropped, every executions that used the allocations are done), and 
//       starts a new one. 
//     + After reallocation, the `chan` is replaced by a new channel whose nodes slots dropbacks 
//       points to.

struct Host {
    conf: HostConf,
    conn: ssh::RemoteHandle,
    chan: Pin<Box<dyn stream::Stream<Item=ChannelMessages> + Send>>,
    vars: EnvironmentVariables,
}

impl Host {
    // Builds a host from a configuration.
    fn from_conf(conf: HostConf) -> Result<Host, Error> {
        debug!("Host: Creating Host from HostConf: {:?}", conf);
        let profile = ssh::config::get_profile(
            &dirs::home_dir().unwrap().join(SSH_CONFIG_RPATH),
            &conf.ssh_config,
        )?;
        trace!("Host: Profile retrieved: {:?}", profile);
        let conn = ssh::RemoteHandle::spawn(profile)?;
        trace!("Host: Connection acquired: {:?}", conn);
        let vars = EnvironmentVariables::new();
        let chan = Box::pin(stream::once(future::ready(ChannelMessages::NoAllocationsMade))
                .chain(stream::repeat(ChannelMessages::WaitForNodes)));
        return Ok(Host {
            conf,
            conn,
            chan,
            vars,
        });
    }

    // Starts an allocation.
    async fn start_alloc(host: Arc<Mutex<Host>>) -> Result<(), Error> {
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Starting allocation on {} with command: {}", conf.name, conf.start_alloc());
        let alloc_output = {host.lock().await.conn.async_exec(conf.start_alloc())}
            .await
            .map_err(|e| Error::AllocationFailed(format!("Failed to allocate: {}", e)))?;
        let alloc_id = match alloc_output.status.code() {
            Some(0) => {
                trace!("Host: Allocation succeeded: {:?}", alloc_output);
                String::from_utf8(alloc_output.stdout)
                    .unwrap()
                    .trim_end_matches('\n')
                    .to_owned()
            }
            _ => {
                trace!("Host: Allocation Failed: {:?}", alloc_output);
                return Err(Error::AllocationFailed(format!(
                    "Failed to obtain allocation on profile {}: {:?}",
                    conf.name, alloc_output
                )));
            }
        };
        {host.lock().await.vars.insert("$ALLOCRET".to_owned(), alloc_id)};
        let get_nodes_command = {host.lock().await.vars.substitute(&conf.get_alloc_nodes())};
        trace!("Host: Trying to get nodes with command: {}", get_nodes_command);
        let nodes_output = {host.lock().await.conn.async_exec(get_nodes_command)}
                .await
                .map_err(|e| Error::AllocationFailed(format!("Failed to get nodes: {}", e)))?;
        let nodes = match nodes_output.status.code() {
            Some(0) => {
                trace!("Host: Nodes string obtained: {:?}", nodes_output);
                String::from_utf8(nodes_output.stdout)
                    .unwrap()
                    .trim_end_matches('\n')
                    .to_owned()
            }
            _ => {
                trace!("Host: Failed to acquire nodes: {:?}", nodes_output);
                return Err(Error::AllocationFailed(format!(
                    "Failed to get node on profile {}: {:?}",
                    conf.name, nodes_output
                )));
            }
        };
        let profile = ssh::config::get_profile(
            &dirs::home_dir().unwrap().join(SSH_CONFIG_RPATH),
            &conf.ssh_config,
        )?;
        trace!("Host: Connecting to nodes...");
        let expiration = Utc::now() + chrono::Duration::minutes(conf.alloc_duration as i64);
        let (mut tx, rx) = unbounded();
        let mut nodes_list = Vec::new();

        for node_name in nodes.split("\n"){
            let mut config = profile.clone();
            let cmd = conf.node_proxy_command.replace("$NODENAME", node_name);
            config.proxycommand.replace(cmd);
            config.hostname.replace(format!("{}::{}", config.name, node_name));
            trace!("Host: Spawning node configuration: {:?}", config);
            let mut node = Err(Error::ConnectingNodes("Failed to connect nodes".into()));
            for _ in 1..10{
                match ssh::RemoteHandle::spawn(config.clone()){
                    Err(e) =>{
                        error!("Host: Failed to connect to node {}: {}\nWaiting and trying again.", node_name, e);
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    Ok(o) =>{
                        info!("Host: Successfully connect node {}", node_name);
                        node = Ok(o);
                        break
                    }
                };
            }
            let node = node?;
            let output = node.async_exec(conf.executions_per_nodes()).await.unwrap();
            let n_handles = String::from_utf8(output.stdout).unwrap().parse::<u64>().unwrap();
            info!("Host: Number of nodes: {}", n_handles);
            for i in 1..n_handles{
                let handle = NodeHandle{
                    remote_handle: node.clone(),
                    before_execution: conf.before_execution().replace("$RUNAWAY_EID", &format!("{}", i)),
                    execution: conf.execution().replace("$RUNAWAY_EID", &format!("{}", i)),
                    after_execution: conf.after_execution().replace("$RUNAWAY_EID", &format!("{}", i)),
                };
                let out = DropBack::new(Expire::new(handle,expiration), tx.clone());
                nodes_list.push(out);
            }
        }

        info!("Host: Sending nodes!");
        let mut nodes_stream = stream::iter(nodes_list);
        tx.send_all(&mut nodes_stream).await
            .map_err(|e| Error::AllocationFailed(format!("Failed to send nodes: {}", e)))?;
        {
            let mut host = host.lock().await;
            host.chan = Box::pin(rx.map(|n| ChannelMessages::Node(n))
                .chain(stream::once(future::ready(ChannelMessages::NoNodesLeft)))
                .chain(stream::repeat(ChannelMessages::WaitForNodes)));
        }
        Ok(())
    }

    // Cancel the current allocation
    async fn cancel_alloc(host: Arc<Mutex<Host>>) -> Result<(), Error> {
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Cancelling allocation on {} ...", conf.name);
        let cancel_alloc = {host.lock().await.vars.substitute(&conf.cancel_alloc())};
        let cancel_output = {host.lock().await.conn.async_exec(cancel_alloc)}
            .await
            .map_err(|e| {
                Error::AllocationFailed(format!("Failed to cancel allocation: {}", e))
            })?;
        match cancel_output.status.code() {
            Some(0) => {
                trace!("Host: Allocation cancelling succeeded");
                {host.lock().await.vars.clear()};
                return Ok(());
            }
            _ => {
                trace!("Host: Allocation cancelling failed: {:?}", cancel_output);
                return Err(Error::AllocationFailed(format!(
                    "Failed to cancel allocation on profile {}: {:?}",
                    conf.name, cancel_output
                )));
            }
        };
    }

    // Acquire a node 
    async fn acquire_node(host: Arc<Mutex<Host>>) -> Result<DropBack<Expire<NodeHandle>>, Error>{
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Acquiring node on {} ...", conf.name);
        loop{
            let val = {
                trace!("Host: Getting channel");
                let chan = &mut host.lock().await.chan; 
                trace!("Host: Getting Next");
                chan.next().await
            };
            match val{
                Some(ChannelMessages::NoAllocationsMade) =>{
                    trace!("Host: No allocations made yet! Starting allocation...");
                    Host::start_alloc(host.clone()).await?;
                }
                Some(ChannelMessages::Node(r)) => {
                    trace!("Host: retrieved node {:?}", *r);
                    if r.is_expired(){
                        trace!("Host: node {:?} is expired. Dropping...", r);
                        r.consume();
                    } else {
                        trace!("Host: node {:?} still good. Returning it...", r);
                        return Ok(r);
                    }
                }
                Some(ChannelMessages::NoNodesLeft) => {
                    trace!("Host: No nodes left! Starting allocation...");
                    Host::cancel_alloc(host.clone()).await?;
                    Host::start_alloc(host.clone()).await?;
                }
                Some(ChannelMessages::WaitForNodes) => {
                    trace!("Host: Allocation ongoing. Waiting...");
                    let(tx, rx) = oneshot::channel();
                        thread::spawn(move || {
                            thread::sleep(std::time::Duration::from_millis(500));
                            tx.send(()).unwrap();
                        });
                    rx.await.unwrap();
                }
                Some(ChannelMessages::Abort) => {
                    trace!("Host: Aborting...");
                    Host::abort(host.clone()).await?;
                    return Err(Error::Aborted);
                }
                Some(ChannelMessages::Shutdown) => {
                    trace!("Host: Shutdown...");
                    return Err(Error::Shutdown);
                }
                None => {
                    panic!("Unexpected");
                }
            }
        }
    }

    // Allows to trigger abort. Every node acquisition will return an error after that.
    async fn abort(host: Arc<Mutex<Host>>) -> Result<(), Error>{
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Aborting on {} ...", conf.name);
        let mut host = host.lock().await;
        let mut rem: Pin<Box<(dyn Stream<Item = ChannelMessages> + Send + 'static)>> 
        = Box::pin(stream::once(future::ready(ChannelMessages::Abort)));
        std::mem::swap(&mut host.chan, &mut rem);
        host.chan = Box::pin(stream::once(future::ready(ChannelMessages::Abort))
            .chain(rem));
        return Ok(())
    }

    // Allows to trigger shutdown. Every node acquisition will return an error after that, and 
    // allocation is cancelled right away. 
    async fn shutdown(host: Arc<Mutex<Host>>) -> Result<(), Error>{
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Shutting down on {} ...", conf.name);
        Host::cancel_alloc(host.clone()).await?;
        let mut host = host.lock().await;
        host.chan = Box::pin(stream::repeat(ChannelMessages::Shutdown));
        return Ok(())
    }
}

//------------------------------------------------------------------------------------------- HANDLE

#[derive(Debug)]
enum OperationInput{
    AcquireNode,
    Abort,
    Shutdown
}

#[derive(Debug)]
enum OperationOutput{
    AcquireNode(Result<DropBack<Expire<NodeHandle>>, Error>),
    Abort(Result<(), Error>),
    Shutdown(Result<(), Error>)
}

#[derive(Clone)]
pub struct HostHandle {
    _sender: UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    _conf: HostConf, 
    _conn: RemoteHandle,
    _dropper: Dropper,
}

impl HostHandle {
    /// This function spawns the thread that will handle all the repository operations using the
    /// CampaignResource, and returns a handle to it.
    pub fn spawn(host_conf: HostConf) -> Result<HostHandle, Error> {
        debug!("HostHandle: Start host thread");
        let host = Host::from_conf(host_conf.clone())?;
        let conn = host.conn.clone();
        let (sender, receiver) = mpsc::unbounded();
        let handle = thread::Builder::new().name(format!("orch-host-{}", host_conf.name))
        .spawn(move || {
            trace!("Host Thread: Creating resource in thread");
            let res = Arc::new(Mutex::new(host));
            let reres = res.clone();
            trace!("Host Thread: Starting resource loop");
            let mut pool = executor::LocalPool::new();
            let mut spawner = pool.spawner();
            let handling_stream = receiver.for_each(
                move |(sender, operation): (oneshot::Sender<OperationOutput>, OperationInput)| {
                    trace!("Host Thread: received operation {:?}", operation);
                    match operation {
                        OperationInput::AcquireNode => {
                            spawner.spawn_local(
                                Host::acquire_node(res.clone())
                                    .map(|a| {
                                        sender.send(OperationOutput::AcquireNode(a))
                                            .map_err(|e| error!("Host Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::Abort => {
                            spawner.spawn_local(
                                Host::abort(res.clone())
                                    .map(|a| {
                                        sender.send(OperationOutput::Abort(a))
                                            .map_err(|e| error!("Host Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::Shutdown => {
                            spawner.spawn_local(
                                Host::shutdown(res.clone())
                                    .map(|a| {
                                        sender.send(OperationOutput::Shutdown(a))
                                            .map_err(|e| error!("Host Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                    }.map_err(|e| error!("Host Thread: Failed to spawn the operation: \n{:?}", e))
                    .unwrap();
                    future::ready(())
                }
            );
            let mut spawner = pool.spawner();
            spawner.spawn_local(handling_stream)
                .map_err(|_| error!("Host Thread: Failed to spawn handling stream"))
                .unwrap();
            trace!("Host Thread: Starting local executor.");
            pool.run();
            trace!("Host Thread: All futures processed. Getting nodes back.");
            executor::block_on(async {
                loop {
                    let mess = {
                        let mut res = reres.lock().await;
                        res.chan.next().await
                    };
                    match mess{
                        Some(ChannelMessages::Abort) => {
                            // Skip abort messages that may remain.
                        },
                        Some(ChannelMessages::Node(e)) => {
                            debug!("Host Thread: Still getting nodes back: {:?}", e);
                            // Forcing expiration. Otherwise we will wait for it to expire...
                            e.consume();
                        },
                        Some(ChannelMessages::Shutdown) => {
                            debug!("Host Thread: Encountered shutdown. Leaving.");
                            // If we receive shutdown messages, the allocation was already cancelled
                            break
                        },
                        _ => {
                            // If anything else, we have to cancel the allocation and leave.
                            debug!("Host Thread: Cancelling allocation ...");
                            if let Err(e) = Host::cancel_alloc(reres.clone()).await{
                                error!("Host Thread: Failed to cancel allocation on drop.");
                            };
                            break
                        }
                    }
                }
            });
            trace!("Host Thread: All good. Leaving...");
        }).expect("Failed to spawn host thread.");
        let drop_sender = sender.clone();
        Ok(HostHandle {
            _sender: sender,
            _conf: host_conf,
            _conn: conn,
            _dropper: Dropper::from_closure(
                Box::new(move || {
                    drop_sender.close_channel();
                    handle.join();
                }), 
                format!("HostHandle")
            ),
        })
    }

    /// Async method, returning a future that ultimately resolves in a campaign, after having
    /// fetched the origin changes on the experiment repository.
    pub fn async_acquire(&self) -> impl Future<Output=Result<DropBack<Expire<NodeHandle>>,Error>> {
        debug!("HostHandle: Building async_acquire_node future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("HostHandle::async_acquire_future: Sending input");
            chan.send((sender, OperationInput::AcquireNode))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("HostHandle::async_acquire_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::AcquireNode(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected AcquireNode, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves after the abortion was started.
    pub fn async_abort(&self) -> impl Future<Output=Result<(),Error>> {
        debug!("HostHandle: Building async_abort future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("HostHandle::async_abort_future: Sending input");
            chan.send((sender, OperationInput::Abort))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("HostHandle::async_abort_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Abort(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected Abort, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves after the shutdown was started.
    pub fn async_shutdown(&self) -> impl Future<Output=Result<(),Error>> {
        debug!("HostHandle: Building async_shutdown future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("HostHandle::async_shutdown_future: Sending input");
            chan.send((sender, OperationInput::Shutdown))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("HostHandle::async_shutdown_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Shutdown(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected Shutdown, found {:?}", e)))
            }
        }
    }

    /// Returns the directory that contains the executions.
    pub fn get_host_directory(&self) -> path::PathBuf {
        return self._conf.directory.clone();
    }

    /// Returns the name of the host.
    pub fn get_name(&self) -> String {
        return self._conf.name.clone();
    }

    /// Returns the command to execute before the executions.
    pub fn get_before_execution_command(&self) -> String{
        return self._conf.before_execution().clone();
    }

    /// Returns the command to execute after the executions.
    pub fn get_after_execution_command(&self) -> String{
        return self._conf.after_execution().clone();
    }

    /// Returns a handle to the frontend connection. 
    pub fn get_frontend(&self) -> RemoteHandle{
        return self._conn.clone();
    }

    /// Downgrades the handle, meaning that the resource could be dropped before this guy.
    pub fn downgrade(&mut self) {
        self._dropper.downgrade();
    }

}

impl Debug for HostHandle{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "HostHandle<{:?}>", self._conf)
    }
}

impl Display for HostHandle{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "{}", self._conf.name)
    }
}


//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod test {

    use super::*;
    use env_logger;

    fn init() {
        
        std::env::set_var("RUST_LOG", "liborchestra::host=trace,liborchestra::primitives=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_host_conf() {
        let conf = HostConf{
            name: "plafrim-court".to_owned(),
            ssh_config: "plafrim-ext".to_owned(),
            node_proxy_command: "ssh -A -l apere plafrim-ext -W $NODENAME:22".to_owned(),
            start_alloc: vec!["module load slurm".to_owned(), "(salloc -N10 -p court --no-shell 2>&1 | sed -e 's/salloc: Granted job allocation //\')".to_owned()],
            get_alloc_nodes: vec!["while read LINE; do scontrol show hostnames \"$LINE\"; done <<< $(squeue -j $ALLOCRET -o \"%N\" | sed '1d' -E)".to_owned()],
            cancel_alloc: vec!["scancel -j $ALLOCRET".to_owned()],
            alloc_duration: 10000,
            executions_per_nodes: vec!["echo 16".to_owned()],
            before_execution: vec!["".to_owned()],
            execution: "$RUNAWAY_COMMAND".to_owned(),
            after_execution: vec!["".to_owned()],
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
    fn test_host_resource() {
        use futures::executor::block_on;
        use std::thread;
        use std::time::Duration;

        init();

        let conf = HostConf {
            name: "localhost".to_owned(),
            ssh_config: "localhost".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:22".to_owned(),
            start_alloc: vec!["".to_owned()],
            get_alloc_nodes: vec!["echo localhost".to_owned()],
            cancel_alloc: vec!["".to_owned()],
            alloc_duration: 1,
            executions_per_nodes: vec!["echo 16".to_owned()],
            before_execution: vec!["".to_owned()],
            execution: "$RUNAWAY_COMMAND".to_owned(),
            after_execution: vec!["".to_owned()],
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        let res_handle = HostHandle::spawn(conf).unwrap();
        let op1 = res_handle.async_acquire();
        let conn1 = block_on(op1);
        println!("conn1: {:?}", conn1);
        assert!(conn1.is_ok());
        let fut = conn1.as_ref().unwrap().async_exec("echo 'from conn1'".to_owned());
        dbg!(block_on(fut)).unwrap();
        thread::sleep(Duration::new(1, 0));
        drop(conn1);
        let op2 = res_handle.async_acquire();
        let conn2 = block_on(op2);
        let op3 = res_handle.async_acquire();
        let conn3 = block_on(op3);
        println!("conn2: {:?}", conn2);
        assert!(conn2.is_ok());
        let fut = conn2.as_ref().unwrap().async_exec("echo 'from conn2'".to_owned());
        dbg!(block_on(fut)).unwrap();
        println!("conn3: {:?}", conn3);
        assert!(conn3.is_ok());
        let fut = conn3.as_ref().unwrap().async_exec("echo 'from conn3'".to_owned());
        dbg!(block_on(fut)).unwrap();
        std::thread::spawn(|| {
            thread::sleep(Duration::new(2, 0));
            drop(conn2);
            drop(conn3);
            println!("conn2-3 dropped");
            thread::sleep(Duration::new(2, 0));
        });
        let op4 = res_handle.async_acquire();
        let conn4 = block_on(op4);
        println!("conn4: {:?}", conn4);
        assert!(conn4.is_ok());
        let fut = conn4.as_ref().unwrap().async_exec("echo 'from conn4'".to_owned());
        dbg!(block_on(fut)).unwrap();

        thread::sleep(Duration::new(60, 000));
        drop(conn4);
        let op = res_handle.async_acquire();
        let conn = block_on(op);
        println!("conn: {:?}", conn);
        assert!(conn.is_ok());
        let fut = conn.as_ref().unwrap().async_exec("echo 'from conn'".to_owned());
        dbg!(block_on(fut)).unwrap();

    }

    #[test]
    fn test_stress_host_resource() {

        use futures::executor;
        use futures::task::SpawnExt;
        use std::thread;
        use std::time::Duration;

        init();

        let conf = HostConf {
            name: "localhost".to_owned(),
            ssh_config: "localhost_proxy".to_owned(),
            node_proxy_command: "ssh -A -l apere localhost -W $NODENAME:22".to_owned(),
            start_alloc: vec!["".to_owned()],
            get_alloc_nodes: vec!["echo localhost".to_owned()],
            cancel_alloc: vec!["".to_owned()],
            alloc_duration: 1,
            executions_per_nodes: vec!["echo 16".to_owned()],
            before_execution: vec!["".to_owned()],
            execution: "$RUNAWAY_COMMAND".to_owned(),
            after_execution: vec!["".to_owned()],
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        let res_handle = HostHandle::spawn(conf).unwrap();

        async fn test(res: HostHandle) {
            let node = res.async_acquire().await.unwrap();
            println!("Node caught: {:?}", node);
            std::thread::sleep_ms(1000);
            let out = node.async_exec("echo 'test'".to_owned()).await.unwrap();
            println!("Output obtained: {:?}", out);
        }

        let mut pool = executor::ThreadPoolBuilder::new()
            .create()
            .unwrap();

        let handles = (1..100).into_iter()
            .map(|_| pool.spawn_with_handle(test(res_handle.clone())).unwrap())
            .collect::<Vec<_>>();

        let fut = futures::future::join_all(handles);
        
        let out = pool.run(fut);
        
    }

}
