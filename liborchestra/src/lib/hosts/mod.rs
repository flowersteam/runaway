//! liborchestra/hosts/mod.rs
//! Author: Alexandre Péré
//!
//! This module contains structure that manages host allocations. The resulting tool is the
//! HostResource, which given an host configuration provide asynchronous nodes allocation. Put
//! differently, it allows to await a node to be available for computation, given the restrictions
//! of the configuration. The allocation are automatically started and revoked.


//------------------------------------------------------------------------------------------ IMPORTS


use crate::derive_from_error;
use crate::commons::{Dropper, DropBack, Expire, AsResult};
use crate::ssh;
use crate::ssh::RemoteHandle;
use dirs;
use futures::Future;
use std::{error, fs, path, str};
use chrono::prelude::*;
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use futures::stream::{self, StreamExt};
use std::thread;
use futures::channel::mpsc::UnboundedSender;
use std::fmt::{self, Display, Debug};
use std::path::{PathBuf};
use crate::misc;
use crate::*;
use std::ops::Deref;
use crate::commons::{EnvironmentKey, EnvironmentValue, RawCommand, TerminalContext};
use crate::SSH_CONFIG_RPATH;
use std::sync::Arc;
use futures::lock::Mutex;
use futures::SinkExt;



//------------------------------------------------------------------------------------------ MODULES


mod provider;


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

impl From<Error> for crate::commons::Error {
    fn from(other: Error) -> crate::commons::Error {
        crate::commons::Error::Operation(format!("{}", other))
    }
}

derive_from_error!(Error, ssh::Error, Ssh);
derive_from_error!(Error, ssh::config::Error, SshConfigParse);


//-------------------------------------------------------------------------------------------- TYPES



/// Represents a frontend
#[derive(Clone)]
struct Frontend(ssh::RemoteHandle);

/// Represents a node id.
#[derive(Clone, Debug)]
struct NodeId(String);

/// Represents a node.
struct Node(ssh::RemoteHandle);

/// Represents a handle id
struct HandleId(String);

/// Represents a handle
struct Handle(ssh::RemoteHandle);

/// Represents a start_alloc procedure
struct StartAllocationProcedure(Vec<RawCommand<String>>);

/// Represents a cancel_alloc procedure
struct CancelAllocationProcedure(Vec<RawCommand<String>>);

/// Represents a get_handles procedure
#[derive(Clone)]
struct GetHandlesProcedure(Vec<RawCommand<String>>);

/// Represent a node proxycommand
struct NodeProxycommand(String);

/// Represents a context on the frontend node
#[derive(Clone)]
struct FrontendContext(TerminalContext<PathBuf>);

/// Represents a context on the allocated node
struct NodeContext(TerminalContext<PathBuf>);

/// Represents a context on the allocated handle to a node
struct HandleContext(TerminalContext<PathBuf>);

/// Represents a the handles as produced by the async_aquire function
#[derive(Clone, Debug)]
pub struct NodeHandle{remote: ssh::RemoteHandle, pub context: TerminalContext<PathBuf>}
impl Deref for NodeHandle {
    type Target = ssh::RemoteHandle;
    fn deref(&self) -> &Self::Target {
        &self.remote
    }
}


//--------------------------------------------------------------------------------------- STRUCTURES


/// A host configuration represents the implementation of the (imaginary) orchestra host interface
/// for a given host. It can write to/read from yaml files. The fields have the following
/// meaning:
/// + name: The name of the configuration
/// + ssh_configuration: The name of the ssh config used (found in SSH_CONFIG_RPATH)
/// + node_proxycommand: The proxycommand used to access the nodes.
/// + start_allocation: Command to start allocation. Should return an identifier kept in $ALLOCRET
/// + cancel_allocation: Command to cancel _this_ allocation. Use $ALLOCRET.
/// + allocation_duration: Number of minutes after which nodes will no longer be issued.
/// + directory: The directory where to put the executions
/// + before_execution: Commands to execute before the execution
/// + execution: Co,,ands to execute the script
/// + after_execution: Commands to execute after the execution
#[derive(Serialize, Deserialize, Debug, Hash, Clone)]
pub struct HostConf {
    pub name: String,
    pub ssh_configuration: String,
    pub node_proxycommand: String,
    pub start_allocation: Vec<String>,
    pub cancel_allocation: Vec<String>, 
    pub allocation_duration: usize, 
    pub get_node_handles: Vec<String>,
    pub directory: path::PathBuf, 
    pub execution: Vec<String>,
}

impl HostConf {
    /// Load an host configuration from a file.
    pub fn from_file(host_path: &path::PathBuf) -> Result<HostConf, Error> {
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
        Ok(config)
    }

    /// Writes host configuration to a file.
    pub fn to_file(&self, conf_path: &path::PathBuf) -> Result<(), Error> {
        let file = fs::File::create(conf_path).map_err(|_| {
            Error::WritingHost(format!(
                "Failed to open host configuration file {}",
                conf_path.to_str().unwrap()
            ))
        })?;
        serde_yaml::to_writer(file, &self).map_err(|_| {
            Error::WritingHost(format!(
                "Failed to write host configuration file {}",
                conf_path.to_str().unwrap()
            ))
        })?;
        Ok(())
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

impl Display for LeaveConfig{
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
       match self{
           LeaveConfig::Nothing => write!(f, "nothing"),
           LeaveConfig::Code => write!(f, "code"),
           LeaveConfig::Everything => write!(f, "everything")
       }
   }
}


//--------------------------------------------------------------------------------------------- HOST


// Enumeration for the messages in the channel.
#[derive(Clone)] 
enum ChannelMessages{
    NoAllocationsMade,
    Node(DropBack<Expire<RemoteHandle>>),
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
    profile: ssh::config::SshProfile,
    provider:  provider::Provider,
    conn: Frontend,
    context: FrontendContext,
}

impl Host {
    // Builds a host from a configuration.
    fn from_conf(conf: HostConf) -> Result<Host, Error> {
        // We retrieve the ssh profile from the configuration
        let profile = ssh::config::get_profile(
            &dirs::home_dir().unwrap().join(SSH_CONFIG_RPATH),
            &conf.ssh_configuration,
        )?;

        // We spawn the frontend remote
        let conn = ssh::RemoteHandle::spawn(profile.clone())?;
        debug!("Host: Connection to frontend acquired: {:?}", conn);

        // We generate the host
        let mut context = FrontendContext(TerminalContext::default());
        context.0.envs.insert(EnvironmentKey("RUNAWAY_PATH".into()), 
                              EnvironmentValue(conf.directory.to_str().unwrap().into()));
        Ok(Host {
            conf,
            conn: Frontend(conn),
            profile,
            provider: provider::Provider::new(),
            context
        })
    }

    // Starts an allocation
    async fn start_alloc(host: Arc<Mutex<Host>>) -> Result<(), Error> {
        // We lock the host. This prevent other futures to start an allocation in the same time.
        let mut host = host.lock().await;

        // We start an allocation on the frontend
        let start_alloc_proc = StartAllocationProcedure(
            host.conf.clone().start_allocation.clone().into_iter().map(Into::into).collect());
        let frontend_context = allocate_nodes(&host.conn, &host.context, &start_alloc_proc).await?;

        // We update the host frontend context (needed to cancel allocation)
        host.context = frontend_context.clone();

        // We retrieve node ids from the terminal context
        let node_ids = extract_nodes(&frontend_context.0)?;
        debug!("Host: Retrieved node ids: {:?}", node_ids);

        // We spawn the nodes
        let nodes = stream::iter(node_ids.clone())
            .then(|nid| spawn_node(nid, host.profile.clone(), NodeProxycommand(host.conf.node_proxycommand.clone())))
            // We can't directly collect as wanted hence the following.
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>,Error>>()?;
        
        // We generate node_contexts
        let node_contexts = node_ids
            .iter()
            .zip(std::iter::repeat_with(||frontend_context.clone()))
            .map(|(id, context)| front_to_node_context(id, context))
            .collect::<Vec<_>>();


        // We generate handles
        let get_handles_proc = GetHandlesProcedure(
            host.conf.get_node_handles.clone().into_iter().map(Into::into).collect());
        let handles = stream::iter(nodes)
            .zip(stream::iter(node_contexts))
            // We query handles to set the right environment variable in the context
            .then(|(node, context)| spawn_handles(node, get_handles_proc.clone(), context))
            // Again, we can't collect easily here
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>,Error>>()?
            .into_iter()
            .flatten()
            .map(|(Handle(remote), HandleContext(context))| NodeHandle{remote, context})
            .collect();

        // We push nodes to the provider
        let expiration = Utc::now() + chrono::Duration::minutes(host.conf.allocation_duration as i64);
        host.provider.push(handles, expiration).await
            .map_err(|e| Error::AllocationFailed(format!("Failed to push nodes: {}", e)))?;

        Ok(())
    }

    // Cancel the current allocation
    async fn cancel_alloc(host: Arc<Mutex<Host>>) -> Result<(), Error> {

        // We lock the host. This prevent another future to cancel the allocation in the same time. 
        let mut host = host.lock().await;

        // We cancel allocation
        let cancel_alloc_procedure = CancelAllocationProcedure(
            host.conf.cancel_allocation.clone().into_iter().map(Into::into).collect());
        let frontend_context = cancel_allocation(&host.conn, 
                                                 &host.context, 
                                                 &cancel_alloc_procedure).await?;

        // We update the host
        host.context = frontend_context;

        Ok(())
    }

    // Acquire a node 
    async fn acquire_node(host: Arc<Mutex<Host>>) -> Result<DropBack<Expire<NodeHandle>>, Error>{
        loop{
            let maybe_node = {
                let provider = &mut host.lock().await.provider;
                provider.pull().await
            };
            match maybe_node{
                Ok(node) => return Ok(node),
                Err(provider::Error::New) => {
                    Host::start_alloc(host.clone()).await?;
                }
                Err(provider::Error::Empty) => {
                    Host::cancel_alloc(host.clone()).await?;
                    Host::start_alloc(host.clone()).await?;
                }
                Err(e) => return Err(Error::AllocationFailed(format!("Failed to acquire node: {}", e)))
            }
        }
    }

    // Allows to trigger abort. Every node acquisition will return an error after that.
    async fn abort(host: Arc<Mutex<Host>>) -> Result<(), Error>{
        let conf = {host.lock().await.conf.clone()};
        debug!("Host: Aborting on {} ...", conf.name);
        let mut host = host.lock().await;
        host.provider.shutdown().await;
        Ok(())
    }

    // Allows to trigger shutdown. Every node acquisition will return an error after that, and 
    // allocation is cancelled right away. 
    async fn shutdown(host: Arc<Mutex<Host>>) -> Result<(), Error>{
        {
            let mut host = host.lock().await;
            host.provider.shutdown().await;
        }
        Host::cancel_alloc(host.clone()).await?;
        Ok(())
    }

    /// Allows to drop the remote correctly 
    async fn drop(host: Arc<Mutex<Host>>){
        {
            let mut host = host.lock().await;
            host.provider.shutdown().await;
            host.provider.collect().await;
        }
        if let Err(e) = Host::cancel_alloc(host.clone()).await{
                error!("Host: Failed to cancel allocation on drop: {}", e);
        }
    }
}

//------------------------------------------------------------------------------------------- HANDLE

#[derive(Debug)]
enum OperationInput{
    AcquireNode,
    Abort,
    Shutdown,
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
        let host = Host::from_conf(host_conf.clone())?;
        let conn = host.conn.clone();
        let (sender, receiver) = mpsc::unbounded();
        let handle = thread::Builder::new().name(format!("orch-host-{}", host_conf.name))
        .spawn(move || {
            let res = Arc::new(Mutex::new(host));
            let reres = res.clone();
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
            spawner.spawn_local(Host::drop(reres))
                .map_err(|_| error!("Host Thread: Failed to spawn cleaning future"))
                .unwrap();
            pool.run();
            trace!("Host Thread: All futures processed. Leaving...");
        }).expect("Failed to spawn host thread.");
        let drop_sender = sender.clone();
        Ok(HostHandle {
            _sender: sender,
            _conf: host_conf,
            _conn: conn.0,
            _dropper: Dropper::from_closure(
                Box::new(move || {
                    drop_sender.close_channel();
                    handle.join().unwrap();
                }), 
                "HostHandle".to_string()
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
        self._conf.directory.clone()
    }

    /// Returns the name of the host.
    pub fn get_name(&self) -> String {
        self._conf.name.clone()
    }

    /// Returns the execution strings
    pub fn get_execution_procedure(&self) -> Vec<RawCommand<String>>{
        self._conf.execution.iter().map(Into::into).map(ToOwned::to_owned).map(RawCommand).collect()
    }

    /// Returns a handle to the frontend connection. 
    pub fn get_frontend(&self) -> RemoteHandle{
        self._conn.clone()
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


//--------------------------------------------------------------------------------------- PROCEDURES


/// Allows to allocate nodes on the host. This 
async fn allocate_nodes(frontend: &Frontend,
                        context: &FrontendContext, 
                        start_alloc: &StartAllocationProcedure) 
                        -> Result<FrontendContext, Error>{
    // We retrieve the commands
    let StartAllocationProcedure(cmds) = start_alloc;
    let FrontendContext(context) = context;
    // We start the allocation by executing the start alloc command
    let (context, outputs) = frontend.0.async_pty(context.to_owned(), cmds.to_owned(), None, None)
        .await
        .map_err(|e| Error::AllocationFailed(format!("Failed to allocate: {}", e)))?;
    let output_len = outputs.len();
    // We eventually print the 
    debug!("Host: Allocation procedure returned:"); 
    outputs.iter()
        .zip(cmds)
        .for_each(|(o, c)| debug!("   {} => {:?}", c.0, o));
    // If the allocation failed we return an error
    misc::compact_outputs(outputs)
        .result()
        .map_err(|e| Error::AllocationFailed(format!("Failed to allocate on command {:?}: {}", cmds.get(output_len-1).unwrap().0, e)))?;
    // We return the Allocation context
    Ok(FrontendContext(context))
}

// Extracts node ids from terminal context
fn extract_nodes(context: &TerminalContext<PathBuf>) -> Result<Vec<NodeId>, Error>{
    context
        // We search the nodes string in environment variables
        .envs
        .get(&EnvironmentKey("RUNAWAY_NODES".into()))
        .map(|EnvironmentValue(s)| s)
        .ok_or(Error::AllocationFailed("RUNAWAY_NODES was not set.".to_string()))?
        // We split and map to node ids
        .split(' ')
        .map(|s| Ok(NodeId(s.to_owned())))
        .collect()
}

/// Turns a frontend context to a node context
fn front_to_node_context(node: &NodeId, context: FrontendContext) -> NodeContext {
    let FrontendContext(mut context) = context;
    let NodeId(node) = node;
    context.envs.insert(EnvironmentKey("RUNAWAY_NODE_ID".to_owned()), EnvironmentValue(node.to_owned()));
    NodeContext(context)
}

/// Allows to spawn the nodes.
async fn spawn_node(node: NodeId,
                    frontend_profile: ssh::config::SshProfile, 
                    proxycommand: NodeProxycommand) 
                     -> Result<Node, Error>{
        // We retrieve the important bits
        let NodeProxycommand(pcmd) = proxycommand;
        let NodeId(node) = node;
        let mut profile = frontend_profile.clone();
        // We change the proxycommand
        profile.proxycommand.replace(pcmd.replace("$RUNAWAY_NODE_ID", &node));
        // We change the hostname
        profile.hostname.replace(format!("{}::{}", profile.name, &node));
        // We cancel port to avoid issues
        profile.port = None;
        // We spawn the profile
        await_retry_n!({
            ssh::RemoteHandle::spawn(profile.clone())
                .map(|n| Node(n))
                .map_err(|e| Error::AllocationFailed(format!("Failed to spawn node: {}", e)))
        }, 10)
}

// This function allows to query the handles
async fn spawn_handles(node: Node, 
                       get_handles_proc: GetHandlesProcedure,
                       context: NodeContext) 
                     -> Result<Vec<(Handle, HandleContext)>, Error>{
    // We retrieve the important bits
    let Node(node) = node;
    let GetHandlesProcedure(cmds) = get_handles_proc;
    let NodeContext(context) = context;
    // We perform the commands
    let (output_context, outputs) = node.async_pty(context.to_owned(), cmds.to_owned(), None, None)
            .await
            .map_err(|e| Error::AllocationFailed(format!("Failed to get handles: {}", e)))?;
    // If the commands failed we return an error
    misc::compact_outputs(outputs)
        .result()
        .map_err(|e| Error::AllocationFailed(format!("Handles query command failed: {}", e)))?;
    // We  extract the handles ids
    let handle_ids = extract_handles(&output_context)?;
    // We generate handles
    Ok(handle_ids.into_iter()
        .zip(std::iter::repeat_with(|| NodeContext(output_context.clone())))
        .map(|(id, ctx)| node_to_handle_context(&id, ctx))
        .zip(std::iter::repeat_with(|| Handle(node.clone())))
        .map(|(a, b)| (b, a))
        .collect())
}

// Extracts handle ids from terminal context
fn extract_handles(context: &TerminalContext<PathBuf>) -> Result<Vec<HandleId>, Error>{
    context
        // We search the nodes string in environment variables
        .envs
        .get(&EnvironmentKey("RUNAWAY_HANDLES".into()))
        .map(|EnvironmentValue(s)| s)
        .ok_or(Error::AllocationFailed(format!("RUNAWAY_HANDLES was not set.")))?
        // We split and map to node ids
        .split(' ')
        .map(|s| Ok(HandleId(s.to_owned())))
        .collect()
}

/// Turns a frontend context to a node context
fn node_to_handle_context(handle: &HandleId, context: NodeContext) -> HandleContext {
    let NodeContext(mut context) = context;
    let HandleId(handle) = handle;
    context.envs.insert(EnvironmentKey("RUNAWAY_HANDLE_ID".to_owned()), EnvironmentValue(handle.to_owned()));
    HandleContext(context)
}


/// Allows to cancel allocation on the host.
async fn cancel_allocation(frontend: &Frontend, 
                           context: &FrontendContext,
                           cancel_alloc: &CancelAllocationProcedure) 
                        -> Result<FrontendContext, Error>{
    // We retrieve the commands
    let CancelAllocationProcedure(cmds) = cancel_alloc;
    let FrontendContext(context) = context;
    // We cancel the allocation by executing the cancel alloc command
    let (context, outputs) = frontend.0.async_pty(context.to_owned(), cmds.to_owned(), None, None)
        .await
        .map_err(|e| Error::AllocationFailed(format!("Failed to cancel allocation: {}", e)))?;
    // If the command failed we return an error
    misc::compact_outputs(outputs)
        .result()
        .map_err(|e| Error::AllocationFailed(format!("Cancel allocation command failed: {}", e)))?;
    // We return the frontend context
    Ok(FrontendContext(context))
}


//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod test {

    use super::*;
    use env_logger;

    fn init() {
        
        std::env::set_var("RUST_LOG", "liborchestra::host=trace,liborchestra::ssh=debug");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_host_conf() {
        let conf = HostConf {
            name: "localhost".to_owned(),
            ssh_configuration: "localhost_proxy".to_owned(),
            node_proxycommand: "ssh -A -l apere localhost -W $NODENAME:22".to_owned(),
            start_allocation: vec!["".to_owned()],
            cancel_allocation: vec!["".to_owned()],
            allocation_duration: 1,
            get_node_handles: vec!["echo 16".to_owned()],
            execution: vec!["$RUNAWAY_COMMAND".to_owned()],
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

    use futures::future::BoxFuture;
    use futures::task::{Poll, noop_waker, Context};

    fn poll_fut<T>(future: BoxFuture<T>, retries: u64) -> Poll<T>{
        let mut future = future;
        let waker = noop_waker(); 
        let mut context = Context::from_waker(&waker);
        for _ in 0..retries{
                        match future.as_mut().poll(&mut context){
                Poll::Pending => {}
                Poll::Ready(c) => return Poll::Ready(c)
            }
        } 
        Poll::Pending
    }

    #[test]
    // To test this, please add localhost2 in your /etc/hosts
    fn test_host_handles_envs() {
        use futures::executor::block_on;
        use std::thread;
        use std::time::Duration;

        //init();

        let conf = HostConf {
            name: "localhost".to_owned(),
            ssh_configuration: "localhost".to_owned(),
            node_proxycommand: "ssh -A -l apere localhost -W $RUNAWAY_NODE_ID:22".to_owned(),
            start_allocation: vec!["export RUNAWAY_NODES='localhost localhost2'".to_owned()],
            cancel_allocation: vec!["echo $RUNAWAY_JOB_ID > /tmp/jobid".to_owned()],
            allocation_duration: 1,
            get_node_handles: vec!["export RUNAWAY_HANDLES='first second'".to_owned()],
            execution: vec!["$RUNAWAY_COMMAND".to_owned()],
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        let res_handle = HostHandle::spawn(conf).unwrap();

        // We test environment of first connection
        let conn1 = {
            let conn = block_on(res_handle.async_acquire()).unwrap();
            let commands = vec![RawCommand("echo $RUNAWAY_NODE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "localhost\n".to_string());
            let commands = vec![RawCommand("echo $RUNAWAY_HANDLE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "first\n".to_string());
            conn
        };

        // We test environment of second connection
        let conn2 = {
            let conn = block_on(res_handle.async_acquire()).unwrap();
            let commands = vec![RawCommand("echo $RUNAWAY_NODE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "localhost\n".to_string());
            let commands = vec![RawCommand("echo $RUNAWAY_HANDLE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "second\n".to_string());
            conn
        };


        // We test environment of third connection
        let conn3 = {
            let conn = block_on(res_handle.async_acquire()).unwrap();
            let commands = vec![RawCommand("echo $RUNAWAY_NODE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(),"localhost2\n".to_string());
            let commands = vec![RawCommand("echo $RUNAWAY_HANDLE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(),"first\n".to_string());
            conn
        };

        // We test environment of fourth connection
        let conn4 = {
            let conn = block_on(res_handle.async_acquire()).unwrap();
            let commands = vec![RawCommand("echo $RUNAWAY_NODE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(),"localhost2\n".to_string());
            let commands = vec![RawCommand("echo $RUNAWAY_HANDLE_ID".to_owned())];
            let (_, outputs) = block_on(conn.async_pty(conn.context.clone(), commands, None, None)).unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!( String::from_utf8(output.stdout).unwrap(), "second\n".to_string());
            conn
        };

    }

    use std::io::Read;

    #[test]
    fn test_stress_host_resource() {

        use futures::executor;
        use futures::task::SpawnExt;
        use std::thread;
        use std::time::Duration;
        use std::ops::Deref;

        init();

        std::fs::remove_file("/tmp/alloc_test");
        std::fs::remove_file("/tmp/cancel_test");

        let conf = HostConf {
            name: "localhost".to_owned(),
            ssh_configuration: "localhost".to_owned(),
            node_proxycommand: "ssh -A -l apere localhost -W $RUNAWAY_NODE_ID:22".to_owned(),
            start_allocation: vec!["sleep 5".to_owned(),
                                   "echo 1 >> /tmp/alloc_test".to_owned(),
                                   "export RUNAWAY_NODES='localhost'".to_owned()],
            cancel_allocation: vec!["sleep 5".to_owned(),
                                    "echo 1 >> /tmp/cancel_test".to_owned()],
            allocation_duration: 1,
            get_node_handles: vec!["export RUNAWAY_HANDLES='first second'".to_owned()],
            execution: vec!["$RUNAWAY_COMMAND".to_owned()],
            directory: path::PathBuf::from("/projets/flowers/alex/executions"),
        };

        let res_handle = HostHandle::spawn(conf).unwrap();

        async fn test(res: HostHandle) {
            let conn = res.async_acquire().await.unwrap();
            std::thread::sleep_ms(1000);
            let command = RawCommand("echo 'test'".into());
            let out = conn.async_exec(command).await.unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), "test\n".to_string());
        }

        let mut pool = executor::ThreadPoolBuilder::new()
            .create()
            .unwrap();

        let handles = (1..200).into_iter()
            .map(|_| pool.spawn_with_handle(test(res_handle.clone())).unwrap())
            .collect::<Vec<_>>();

        let fut = futures::future::join_all(handles);
        pool.run(fut);
        drop(res_handle);

        let mut alloc_file = std::fs::File::open("/tmp/alloc_test").unwrap();
        let mut alloc_string = String::new();
        alloc_file.read_to_string(&mut alloc_string).unwrap();
        let n_alloc = alloc_string.lines().count();
        dbg!(&n_alloc);
        let n_alloc = alloc_string.lines().count();
        assert!(n_alloc > 1);
        let mut cancel_file = std::fs::File::open("/tmp/cancel_test").unwrap();
        let mut cancel_string = String::new();
        cancel_file.read_to_string(&mut cancel_string).unwrap();
        let n_cancel = alloc_string.lines().count();
        dbg!(&n_cancel);
        assert_eq!(alloc_string, cancel_string);
        
    }

}
