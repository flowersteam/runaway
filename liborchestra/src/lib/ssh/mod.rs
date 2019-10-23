//! liborchestra/mod.rs
//!
//! This module contains a structure that wraps the ssh2 session object to provide ways to handle
//! ssh configurations, authentications, and proxy-commands. In particular, since libssh2
//! does not provide ways to handle proxy-commands, we propose our own structure based on a threaded 
//! copy loop that opens a proxy-command as a subprocess and copy the output on a random tcp socket.
 

//------------------------------------------------------------------------------------------ IMPORTS


use ssh2::{
    Session,
    KnownHostFileKind,
    CheckResult,
    MethodType,
    KnownHostKeyFormat};
use std::{
    net::{TcpStream, SocketAddr, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    io::{prelude::*, BufReader},
    process::{Stdio, Command, Output, ExitStatus},
    os::unix::process::ExitStatusExt,
    error,
    fmt,
    path::PathBuf,
    collections::HashSet,
    fs::{File, OpenOptions},
};
use dirs;
use crate::KNOWN_HOSTS_RPATH;
use futures::future::Future;
use crate::derive_from_error;
use crate::commons::Dropper;
use std::intrinsics::transmute;
use futures::lock::Mutex;
use futures::executor;
use futures::future;
use futures::StreamExt;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use futures::SinkExt;
use futures::channel::{mpsc, oneshot};
use crate::commons::{
    Cwd, 
    EnvironmentStore, 
    EnvironmentKey, 
    EnvironmentValue, 
    RawCommand, 
    TerminalContext
};
use crate::*;


//------------------------------------------------------------------------------------------  MODULE


pub mod config;


//---------------------------------------------------------------------------------------- PTY AGENT


/// The `pty` method proposed by the `Remote` object, needs a remote side agent to relay stdout, 
/// stderr, exit codes and more, using only a single pty stream. As of today this agent consists 
/// of a piece of bash agent implemented through a few functions.
/// 
/// In practice, the pty should format messages according to the following protocol: 
///     + `RUNAWAY_STDOUT: {some stdout}\n` to transmit an stdout line
///     + `RUNAWAY_STDERR: {some stderr}\n` to transmit an stderr line
///     + `RUNAWAY_ECODE: {some exit code}\n` to transmit an error code
///     + `RUNAWAY_CWD: {a directory path}\n` to transmit a current working directory path
///     + `RUNAWAY_ENV: {environment variables}={environment value}\n` to transmit an environment
///        variable
///     + `RUNAWAY_EOF:\n`to express the end of the stream of messages.


/// Bash pty agent, injected in the pty before executing the commands.
static BASH_PTY_AGENT: &str = "
rw_init() {
    # We remove the prompt banner
    export PS1=
    # We disable stdin echo
    stty -echo
    # We generate two fifo that will be used to format stdout messages and stderr messages
    export stdout_fifo=/tmp/$(uuidgen)
    export stderr_fifo=/tmp/$(uuidgen)
    mkfifo $stdout_fifo
    mkfifo $stderr_fifo
}


# Function used to execute a command. 
rw_run() {

    # Since stdout and stderr are handled in separate threads, we have to ensure that stdout and 
    # stderr were completely read before moving to the next command. We do so by using file locks, 
    # which are unix locks using file descriptors.

    # We create the files
    way_in_lock=/tmp/$(uuidgen)
    way_out_lock=/tmp/$(uuidgen)

    # We create the file descriptors
    exec 201>$way_in_lock
    exec 202>$way_out_lock

    # We lock the way_in_lock in exclusive mode. This will prevent the stdout and stderr handlers to 
    # break before the command completed.
    flock -x 201

    # We start a subcommand that handles the stdout messages on the stdout_fifo.
    echo Starting stdout handler
    # First we lock the way_out_lock in shared mode to prevent the command from returning before all 
    # messages were handled.
    (
    flock -s 202;
    while true; do
        # We format the line
        if read line ; then
            echo RUNAWAY_STDOUT: $line;
        # We try to acquire a shared lock on the way_in_lock. This can only happen when the exclusive
        # lock hold by the command will be released, after the command was executed.
        elif flock -ns 201; then
            # We release our lock on the way_out_lock.
            flock -u 202;
            break;
        fi;
    done<$stdout_fifo;
    )&
    stdout_pid=$!

    # We start the same subcommand for the stderr.
    echo Starting stderr handler
    (
    flock -s 202;
    while true; do
        if read line ; then
            echo RUNAWAY_STDERR: $line;
        elif flock -ns 201; then
            flock -u 202;
            break;
        fi;
    done<$stderr_fifo;
    )&
    stderr_pid=$!

    # Now we are ready to evaluate the command. The stdout and stderr are forwarded to the right 
    # fifos for further handling under the adequqte subprocesses.

    { eval $1 ; } 1>$stdout_fifo 2>$stderr_fifo
    # We retrieve the exit code
    RUNAWAY_ECODE=$?

    # We release the exclusive lock on the way_in_lock. This has the effect to break the loops in 
    # the stdout and stderr handlers.
    flock -u 201

    # We wait to acquire an exclusive lock on the way_out_lock. This can only occur after the two 
    # handlers broke and released their shared lock.
    flock -x 202

    # We remove the locks 
    rm $way_in_lock
    rm $way_out_lock

    # We echo the exit code.
    echo RUNAWAY_ECODE: $RUNAWAY_ECODE

}

# Function to teardown the agent.
rw_close(){

    # We output the current working directory
    echo RUNAWAY_CWD: $(pwd)

    # We echo the environment variables
    env | sed 's/^/RUNAWAY_ENV: /g'

    # We cleanup
    rm $stdout_fifo
    rm $stderr_fifo

    # We leave
    echo RUNAWAY_EOF:

}
";


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    // Leaf Errors
    WouldBlock,
    ConnectionFailed(String),
    ProxyCommandStartup(String),
    ExecutionFailed(String),
    ScpSendFailed(String),
    ScpFetchFailed(String),
    PollExecutionFailed(String),
    FuturePoll(String),
    SpawnThread(String),
    Channel(String),
    OperationFetch(String),
    ChannelNotAvailable,
    // Chaining Errors
    Config(config::Error),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::WouldBlock =>
                write!(f, "Blocking operation."),
            Error::ConnectionFailed(ref s) =>
                write!(f, "An error occurred while trying to connect to connect to a remote \
                host: \n{}", s),
            Error::ProxyCommandStartup(ref s) =>
                write!(f, "An error occurred when starting a proxycommand: \n{}", s),
            Error::ExecutionFailed(s) =>
                write!(f, "An error occurred while running a command on a remote host: \n{}", s),
            Error::ScpSendFailed(s) =>
                write!(f, "An error occurred while sending a file to a remote host: \n{}", s),
            Error::ScpFetchFailed(s) =>
                write!(f, "An error occurred while fetching a file from a remote host: \n{}", s),
            Error::PollExecutionFailed(s) =>
                write!(f, "An error occurred while polling a command: \n{}", s),
            Error::FuturePoll(s) =>
                write!(f, "An error occurred while polling a future: \n{}", s),
            Error::SpawnThread(s) =>
                write!(f, "An error occured while spawning the thread: \n{}", s),
            Error::ChannelNotAvailable =>
                write!(f, "Channel is not yet available"),
            Error::Channel(e) =>
                write!(f, "A channel related error occured: {}", e),
            Error::OperationFetch(e) =>
                write!(f, "Failed to fetch the operation.: {}", e),
            Error::Config(e) =>
                write!(f, "An ssh config-related error occurred: {}", e),
        }
    }
}

derive_from_error!(Error, config::Error, Config);

impl From<Error> for crate::commons::Error{
    fn from(other: Error) -> crate::commons::Error{
        crate::commons::Error::Operation(format!("{}", other))
    }
}


//------------------------------------------------------------------------------------ PROXY-COMMAND


// This type represents the handle to the proxycommand forwarding process. 
type ProxyCommandHandle = Option<thread::JoinHandle<(thread::JoinHandle<()>,
                                                     thread::JoinHandle<()>,
                                                     thread::JoinHandle<()>)>>;

/// This structure starts a proxy command which is forwarded on a tcp socket. This allows
/// a libssh2 session to get connected through a proxy command. On the first connection to the
/// socket, the proxycommand will be started in a subprocess, and the reads from the socket will be
/// copied to the process stdin, and the stdout from the process will be written to the socket. The 
/// forthcoming connections are rejected. The messages get forwarded as long as the forwarder stays 
/// in scope. The forwarding is explicitly stopped when the forwarder is dropped. 
pub struct ProxyCommandForwarder {
    keep_alive: Arc<AtomicBool>,
    handle: ProxyCommandHandle ,
    repr: String,
}

impl ProxyCommandForwarder {
    /// Creates a new `ProxyCommandForwarder` from a command. An available port is automatically
    /// given by the OS, and is returned along with the forwarder. 
    pub fn from_command(command: &str) -> Result<(ProxyCommandForwarder, SocketAddr), Error> {
        debug!("ProxyCommandForwarder: Starting proxy command: {}", command);
        trace!("ProxyCommandForwarder: Starting tcp listener");
        let stream = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(s) => s,
            Err(e) => return Err(Error::ProxyCommandStartup(
                format!("Failed to find a port to bind to: {}", e))
            )
        };
        let command_string = command.to_owned();
        let address = stream.local_addr().unwrap();
        let keep_forwarding = Arc::new(AtomicBool::new(true));
        let kf = keep_forwarding.clone();
        let mut cmd = command.split_whitespace().collect::<Vec<_>>();
        let args = cmd.split_off(1);
        let cmd = cmd.pop().unwrap();
        trace!("ProxyCommandForwarder: Spawning proxy command");
        let mut command = Command::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        trace!("ProxyCommandForwarder: Spawning proxy command forwarding thread");
        let handle = std::thread::spawn(move || {
            let (socket, _) = stream.accept().unwrap();
            let mut socket1 = socket.try_clone().unwrap();
            let mut socket2 = socket.try_clone().unwrap();
            let kf1 = kf.clone();
            let kf2 = kf.clone();
            let kf3 = kf.clone();
            let mut command_stdin = command.stdin.take().unwrap();
            let mut command_stdout = command.stdout.take().unwrap();
            let h1 = std::thread::spawn(move || {
                while kf1.load(Ordering::Relaxed) {
                    if std::io::copy(&mut command_stdout, &mut socket1).is_err(){
                        break
                    } else{
                        socket1.flush().unwrap()
                    }
                }
                trace!("ProxyCommandForwarder: Exiting command -> socket thread");
            });
            let h2 = std::thread::spawn(move || {
                while kf2.load(Ordering::Relaxed) {
                    if std::io::copy(&mut socket2, &mut command_stdin).is_err(){
                        break
                    } else {
                        command_stdin.flush().unwrap();
                    }
                }
                trace!("ProxyCommandForwarder: Exiting socket -> command thread");
            });
            let h3 = std::thread::spawn(move || {
                while kf3.load(Ordering::Relaxed) {
                    if let Ok(Some(_)) = command.try_wait() {
                        trace!("ProxyCommandForwarder: Proxy Command has stopped. Exiting");
                        kf3.store(false, Ordering::Relaxed);
                    }
                }
                trace!("ProxyCommandForwarder: Exiting watch thread");
            });
            (h1, h2, h3)
        });
        trace!("ProxyCommandForwarder: Returning proxy command");
        Ok((ProxyCommandForwarder {
            keep_alive: keep_forwarding,
            handle: Some(handle),
            repr: command_string
        },
                   address))
    }
}

impl fmt::Debug for ProxyCommandForwarder{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ProxyCommandForwarder{}", self.repr)
    }
}

// Care must be taken to join the threads at drop, to appropriately close the connection and the
// process.
impl Drop for ProxyCommandForwarder { 
    fn drop(&mut self) {
        self.keep_alive.store(false, Ordering::Relaxed);
        let handle = self.handle.take().unwrap();
        let (h1, h2, h3) = handle.join().unwrap();
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
    }
}


//-------------------------------------------------------------------------------- REMOTE CONNECTION


// A synchronous wrapper over a non-blocking Remote.
struct Remote{
    session: Option<&'static Session>,
    _stream: TcpStream,
    proxycommand: Option<ProxyCommandForwarder>,
}

impl Drop for Remote{
    fn drop(&mut self){
        // Unsafe trick to avoid memory leaks. Since we know that all our references to the
        // session were dropped we transmute the session to a Box and drop it.
        unsafe{
            let sess: Box<Session> = transmute(self.session.take().unwrap() as *const Session);
            // We disconnect the session
            match sess.disconnect(None, "Over", None){
                Ok(_) => info!("Remote: Successfully disconnected."),
                Err(e) => error!("Remote: Failed to disconnect from remote: {}", e)
            }
            drop(sess);
        }
    }
}

impl Remote {
    /// Helper to avoid boilerplate.
    fn session(&self) -> &'static Session{
        self.session.as_ref().unwrap()
    }

    /// Build a Remote from a profile.
    fn from_profile(profile: config::SshProfile) -> Result<Remote, Error>{
        debug!("Remote: Creating remote connection from profile: {:?}", profile);
        match profile{
            config::SshProfile{
                hostname:Some(host),
                user: Some(user),
                proxycommand: Some(cmd),
                ..} => {
                Remote::from_proxy_command(&cmd, &host, &user)
            }
            config::SshProfile{
                hostname: Some(host),
                user:Some(user),
                proxycommand: None,
                port: Some(port),
                ..
            } => {
                let address = format!("{}:{}", host, port);
                Remote::from_addr(address, &host, &user)
            }
            _ => Err(Error::ConnectionFailed(format!("The encountered profile is invalid: \
                 \n{:?}\nIt should either provide a proxycommand or a port", profile)))
        }
    }

    /// Build, authenticate and starts an ssh session from an adress.
    fn from_addr(addr: impl ToSocketAddrs+fmt::Display,
                     host: &str,
                     user: &str) -> Result<Remote, Error> {
        debug!("Remote: Creating remote connection from address");
        trace!("Remote: Connection tcp stream");
        let stream = TcpStream::connect(&addr)
            .map_err(|_| Error::ConnectionFailed(format!("Failed to connect to address {}", addr)))?;
        let session = Remote::start_session(&stream, host, user)?;
        let session: &'static Session = Box::leak(Box::new(session));
        Ok(Remote {
            session: Some(session),
            _stream: stream,
            proxycommand: None,
        })
    }

    /// Build, authenticate and starts an ssh session from a proxycommand.
    fn from_proxy_command(command: &str, host: &str, user: &str) -> Result<Remote, Error> {
        debug!("Remote: Creating remote connection from proxycommand {}", command);
        let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
        let mut remote = Remote::from_addr(addr, host, user)?;
        remote.proxycommand.replace(proxy_command);
        Ok(remote)
    }

    /// Starts the ssh session.
    fn start_session(stream: &TcpStream, host: &str, user: &str) -> Result<Session, Error> {
        debug!("Remote: Opening remote connection to host {} as user {}", host, user);
        let mut session = new_session(stream)?;
        authenticate_host(host, &mut session)?;
        authenticate_local(user, &mut session) ?;
        session.set_blocking(false);
        Ok(session)
    }


    /// Asynchronous function used to execute a command on the remote.
    async fn exec(remote: Arc<Mutex<Remote>>, command: RawCommand<String>) -> Result<Output, Error>{
        debug!("Remote: Executing command `{}`", command.0);
        let RawCommand(cmd) = command; 
        let mut channel = acquire_exec_channel(&remote).await?;
        setup_exec(cmd, &mut channel).await?;
        let mut output = read_exec_out_err(&mut channel).await?;
        output.status = close_exec(&mut channel).await?;
        Ok(output)
    }

    /// Asynchronous function used to execute an interactive command on the remote.
    async fn pty(remote: Arc<Mutex<Remote>>,
                 context: TerminalContext<PathBuf>, 
                 commands: Vec<RawCommand<String>>,
                 stdout_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
                 stderr_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>) 
                 -> Result<(TerminalContext<PathBuf>, Vec<Output>), Error> {
        debug!("Remote: Executing pty commands");
        
        if commands.is_empty(){
            return Err(Error::ExecutionFailed("No command was provided.".to_string()))
        }
        let TerminalContext{cwd: Cwd(cwd), envs } = context;
        let mut channel = acquire_pty_channel(&remote).await
            .map_err(|e| Error::ExecutionFailed(format!("Failed to start pty channel: {}", e)))?;
        setup_pty(&mut channel, &cwd, &envs).await?;
        let (context, outputs) = perform_pty(&mut channel, commands, stdout_cb, stderr_cb).await?;
        close_pty(&mut channel).await?;

        Ok((context, outputs))
    }

    

    /// Asynchronous function used to send a file to a remote.
    async fn scp_send(remote: Arc<Mutex<Remote>>, local_path: PathBuf, remote_path: PathBuf) -> Result<(), Error>{
        debug!("Remote: Starting transfert of local {} to remote {}", 
            local_path.to_str().unwrap(), 
            remote_path.to_str().unwrap());

        let (mut local_file, bytes, mut channel) = setup_scp_send(&remote, &local_path, &remote_path).await?;
        perform_scp_send(&mut channel, &mut local_file, bytes).await?;
        close_scp_channel(channel).await
            .map_err(|e| Error::ScpSendFailed(format!("Failed to close channel: {}", e)))?;
        Ok(())
    }

    /// Asynchronous method used to fetch a file from remote.
    async fn scp_fetch(remote: Arc<Mutex<Remote>>, remote_path: PathBuf, local_path: PathBuf) -> Result<(), Error>{
        debug!("Remote: Starting transfert of remote {} to local {}", 
            remote_path.to_str().unwrap(), 
            local_path.to_str().unwrap());

        std::fs::remove_file(&local_path).unwrap_or(());
        let (file, mut channel, bytes) = setup_scp_fetch(&remote, &remote_path, &local_path).await?;
        process_scp_fetch(&mut channel, file, bytes).await?;
        close_scp_channel(channel).await
            .map_err(|e| Error::ScpFetchFailed(format!("Failed to close channel: {}", e)))?;
        Ok(())
    }
}

/// The operation inputs, sent by the outer futures and processed by inner thread.
enum OperationInput {
    Exec(RawCommand<String>),
    Pty(TerminalContext<PathBuf>, Vec<RawCommand<String>>, Box<dyn Fn(Vec<u8>)+Send+'static>, Box<dyn Fn(Vec<u8>)+Send+'static>),
    ScpSend(PathBuf, PathBuf),
    ScpFetch(PathBuf, PathBuf)
}

impl std::fmt::Debug for OperationInput{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self{
            OperationInput::Exec(c) => write!(f, "Exec({:?})", c),
            OperationInput::Pty(t, c, out, err) => write!(f, "Pty({:?}, {}, {})", c, stringify!(out), stringify!(err)),
            OperationInput::ScpSend(a, b) => write!(f, "ScpSend({:?}, {:?})", a, b),
            OperationInput::ScpFetch(a, b) => write!(f, "ScpFetch({:?}, {:?})", a, b),
        }
    }
}

/// The operation outputs, sent by the inner futures and processed by the outer thread.
#[derive(Debug)]
enum OperationOutput {
    Exec(Result<Output, Error>),
    Pty(Result<(TerminalContext<PathBuf>, Vec<Output>), Error>),
    ScpSend(Result<(), Error>),
    ScpFetch(Result<(), Error>)
}


/// A handle to an inner application. Offer a future interface to the inner application.
#[derive(Clone)]
pub struct RemoteHandle {
    sender: mpsc::UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    profile: config::SshProfile,
    dropper: Dropper,
}

impl fmt::Debug for RemoteHandle{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        write!(f, "RemoteHandle<{}>", self.profile.hostname
            .as_ref()
            .unwrap_or(self.profile.proxycommand.as_ref().unwrap_or(&"Unknown".to_string())))
    }
}

impl PartialEq for RemoteHandle {
    fn eq(&self, other: &Self) -> bool {
        self.profile == other.profile
    }
}

impl RemoteHandle {
    /// Spawns the application and returns a handle to it. 
    pub fn spawn(profile: config::SshProfile) -> Result<RemoteHandle, Error> {
        debug!("RemoteHandle: Start remote thread.");
        let (sender, receiver) = mpsc::unbounded();
        let (start_tx, start_rx) = crossbeam::channel::unbounded();
        let repr = match &profile.proxycommand{
            Some(p) => p.to_string(),
            None => format!("{}:{}", profile.hostname.as_ref().unwrap(), profile.port.as_ref().unwrap())
        };
        let moving_profile = profile.clone();
        let handle = std::thread::Builder::new().name("remote".to_owned()).spawn(move || {
            trace!("Remote Thread: Creating resource in thread");
            let remote = match Remote::from_profile(moving_profile){
                Ok(r) =>{
                    start_tx.send(Ok(())).unwrap();
                    r
                }
                Err(e) =>{
                    start_tx.send(Err(e)).unwrap();
                    return ();
                }
            };
            let remote = Arc::new(Mutex::new(remote));
            trace!("Remote Thread: Starting handling stream");
            let mut pool = executor::LocalPool::new();
            let mut spawner = pool.spawner();
            let handling_stream = receiver.for_each(
                move |(sender, operation): (oneshot::Sender<OperationOutput>, OperationInput)| {
                    trace!("Remote Thread: received operation {:?}", operation);
                    match operation {
                        OperationInput::Exec(command) => {
                            spawner.spawn_local(
                                Remote::exec(remote.clone(), command)
                                    .map(|a| {
                                        sender.send(OperationOutput::Exec(a))
                                            .map_err(|e| error!("Remote Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        },
                        OperationInput::Pty(context, commands, stdout_cb, stderr_cb) => {
                            spawner.spawn_local(
                                Remote::pty(remote.clone(), context, commands, stdout_cb, stderr_cb)
                                    .map(|a| {
                                        sender.send(OperationOutput::Pty(a))
                                            .map_err(|e| error!("Remote Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        },                        
                        OperationInput::ScpSend(local_path, remote_path) => {
                            spawner.spawn_local(
                                Remote::scp_send(remote.clone(), local_path, remote_path)
                                    .map(|a| {
                                        sender.send(OperationOutput::ScpSend(a))
                                            .map_err(|e| error!("Remote Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::ScpFetch(remote_path, local_path) => {
                            spawner.spawn_local(
                                Remote::scp_fetch(remote.clone(), remote_path, local_path)
                                    .map(|a| {
                                        sender.send(OperationOutput::ScpFetch(a))
                                            .map_err(|e| error!("Remote Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                    }
                    .map_err(|e| error!("Remote Thread: Failed to spawn the operation: \n{:?}", e))
                    .unwrap();
                    future::ready(())
                }
            );
            let mut spawner = pool.spawner();
            spawner.spawn_local(handling_stream)
                .map_err(|_| error!("Remote Thread: Failed to spawn handling stream"))
                .unwrap();
            trace!("Remote Thread: Starting local executor.");
            pool.run();
            trace!("Remote Thread: All futures executed. Leaving...");
        }).expect("Failed to spawn application thread.");
        start_rx.recv().unwrap()?;
        let drop_sender = sender.clone();
        Ok(RemoteHandle {
            sender,
            profile,
            dropper: Dropper::from_closure(
                Box::new(move ||{
                    drop_sender.close_channel();
                    handle.join().unwrap();
                }), 
                "RemoteHandle".to_string()),
        })
    }

    /// A function that returns a future that resolves in a result over an output, after the command
    /// was executed.
    pub fn async_exec(&self, command:RawCommand<String>) -> impl Future<Output=Result<Output, Error>> {
        debug!("RemoteHandle: Building async_exec future to command: {:?}", command);
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("RemoteHandle::async_exec_future: Sending exec input");
            chan.send((sender, OperationInput::Exec(command)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("RemoteHandle::async_exec_future: Awaiting exec output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Exec(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected Exec, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an output, after the command
    /// was executed in interactive mode. Callbacks can be provided to be called on stdout and 
    /// stderr messages.  
    pub fn async_pty(&self, 
        context: TerminalContext<PathBuf>,
        commands: Vec<RawCommand<String>>, 
        stdout_cb: Option<Box<dyn Fn(Vec<u8>)+Send+'static>>,
        stderr_cb: Option<Box<dyn Fn(Vec<u8>)+Send+'static>>) 
        -> impl Future<Output=Result<(TerminalContext<PathBuf>, Vec<Output>), Error>> {
        debug!("RemoteHandle: Building async_pty future to commands: {:?}", commands);
        let mut chan = self.sender.clone();
        let mut stdout_cb = stdout_cb;
        if stdout_cb.is_none(){
            stdout_cb = Some(Box::new(|_|{}));
        }
        let mut stderr_cb = stderr_cb;
        if stderr_cb.is_none(){
            stderr_cb = Some(Box::new(|_|{}));
        }
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("RemoteHandle::async_pty_future: Sending pty input");
            chan.send((sender, OperationInput::Pty(context, commands, stdout_cb.unwrap(), stderr_cb.unwrap())))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("RemoteHandle::async_pty_future: Awaiting pty output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Pty(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected Pty, found {:?}", e)))
            }
        }
    }

    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// file was sent.
    pub fn async_scp_send(&self, local_path: PathBuf, remote_path: PathBuf) -> impl Future<Output=Result<(), Error>> {
        debug!("RemoteHandle: Building async_scp_send future from local {} to remote {}", 
            local_path.to_str().unwrap(),
            remote_path.to_str().unwrap());
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("RemoteHandle::async_scp_send_future: Sending scp send input");
            chan.send((sender, OperationInput::ScpSend(local_path, remote_path)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("RemoteHandle::async_scp_send_future: Awaiting scp send output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::ScpSend(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected ScpSend, found {:?}", e)))
            }
        }
    }
    
    /// A function that returns a future that resolves in a result over an empty type, after the 
    /// file was fetch
    pub fn async_scp_fetch(&self, remote_path: PathBuf, local_path: PathBuf) -> impl Future<Output=Result<(), Error>> {
        debug!("RemoteHandle: Building async_scp_fetch future from remote {} to local {}", 
            remote_path.to_str().unwrap(),
            local_path.to_str().unwrap());
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("RemoteHandle::async_scp_fetch_future: Sending scp fetch input");
            chan.send((sender, OperationInput::ScpFetch(remote_path, local_path)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("RemoteHandle::async_scp_fetch_future: Awaiting scp fetch output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::ScpFetch(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected ScpFetch, found {:?}", e)))
            }
        }
    }
}


//--------------------------------------------------------------------------------------- PROCEDURES


// Generates a new session following our preferences
fn new_session(stream: &TcpStream) -> Result<Session, Error>{
    trace!("Remote: Creates a new session");

    let mut session = Session::new().unwrap();
    session.method_pref(MethodType::HostKey, "ssh-rsa")
        .map_err(|_| Error::ConnectionFailed("Failed to set preferences".to_string()))?;
    session.handshake(stream)
        .map_err(|e| Error::ConnectionFailed(format!("Failed to perform handshake: \n{}", e)))?;
    Ok(session)
}


// Checks the host identity against our known hosts.
fn authenticate_host(host: &str, session: &mut Session) -> Result<(),Error>{
    trace!("Remote: Checking host key");

    // We set the session known hosts to the database file
    let mut known_hosts = session.known_hosts().unwrap();
    let mut known_hosts_path = dirs::home_dir()
        .ok_or(Error::ConnectionFailed("Failed to find the local home directory".to_string()))?;
    known_hosts_path.push(KNOWN_HOSTS_RPATH);
    if !known_hosts_path.exists(){
        File::create(known_hosts_path.clone())
            .map_err(|e| Error::ConnectionFailed(format!("Failed to create knownhost file: {}", e)))?;
    }
    known_hosts.read_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
        .map_err(|e| Error::ConnectionFailed(format!("Failed to reach knownhost file:\n{}", e)))?;

    // We retrieve the host key 
    let (key, _) = session.host_key()
        .ok_or(Error::ConnectionFailed("Host did not provide a key.".to_string()))?;
    
    // We check host key against known identities
    match known_hosts.check(host, key) {
        // Host recognized
        CheckResult::Match => {
            Ok(())
        }
        // Host is different
        CheckResult::Mismatch => {
            trace!("Remote: Host key mismatch....");
            Err(Error::ConnectionFailed("The key presented by the host mismatches the one we know. ".to_string()))
        }
        // Host not in database
        CheckResult::NotFound => {
            trace!("Remote: Host not known. Writing the key in the registry.");
            known_hosts.add(host, key, "", KnownHostKeyFormat::SshRsa)
                .map_err(|_| Error::ConnectionFailed("Failed to add the host key in the registry".to_string()))?;
            known_hosts.write_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
                .map_err(|_| Error::ConnectionFailed("Failed to write the knownhost file".into()))?;
            Ok(())
        }
        // Unknown error
        CheckResult::Failure => {
            trace!("Remote: Failed to check the host");
            Err(Error::ConnectionFailed("Failed to check the host.".to_string()))
        }
    } 
}


// Authenticates ourselves on the remote end, using an ssh agent.
fn authenticate_local(user: &str, session: &mut Session) -> Result<(), Error>{
    trace!("Remote: Authenticating ourselves");
    
    // We retrieve the agent. 
    let mut agent = session.agent().unwrap();
    agent.connect()
        .map_err(|e| Error::ConnectionFailed(format!("Ssh Agent was unavailable: \n{}", e)))?;
    agent.list_identities()
        .map_err(|e| Error::ConnectionFailed(format!("Couldn't list identities: \n{}", e)))?;
    
    // We go through the stored identities to try to authenticate.
    let ids = agent.identities()
        .map(|id| id.unwrap().comment().to_owned())
        .collect::<HashSet<_>>();
    let failed_ids = agent.identities()
        .take_while(|id|  agent.userauth(user, id.as_ref().unwrap()).is_err() )
        .map(|id| { id.unwrap().comment().to_owned() })
        .collect::<HashSet<_>>();
    if ids == failed_ids {
        trace!("RemoteResource: No identities registered in the agent allowed to authenticate.");
        Err(Error::ConnectionFailed("No identities registered in the ssh agent allowed to connect.".into()))
    } else {
        Ok(())
    }
    
}


// Acquires an exec channel on the remote
async fn acquire_exec_channel(remote: &Arc<Mutex<Remote>>) -> Result<ssh2::Channel<'_>, Error>{
    trace!("Remote: Acquiring exec channel.");

    // We query a channel session. Error -21 corresponds to missing available channels. It 
    // must be retried until an other execution comes to an end, and makes a channel available. 
    let ret = await_wouldblock_ssh!(await_retry_ssh!(
        {
            remote.lock()
                .await
                .session()
                .channel_session()
        },
        -21
        )
    );
    match ret {
        Ok(c) => Ok(c),
        Err(e) => Err(Error::ExecutionFailed(format!("Failed to open channel: {}", e)))
    }
}


// Performs exec command.
async fn setup_exec(cmd: String, channel: &mut ssh2::Channel<'_>) -> Result<(), Error>{
    trace!("Remote: Perform exec command `{}`", cmd);

    // We execute the command in the cwd.
    await_wouldblock_ssh!(channel.exec(&format!("{}\n", cmd)))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to exec command {}: {}", cmd, e)))?;

    // We close
    await_wouldblock_ssh!(channel.send_eof())
        .map_err(|e| Error::ExecutionFailed(format!("Failed to send EOF: {}", e)))?;
    Ok(())
}


// Reads the output of an exec command, and returns the output.
async fn read_exec_out_err(channel: &mut ssh2::Channel<'_>) -> Result<Output, Error>{
    trace!("Remote: Reading exec output");

    // We generate a new output
    let mut output = Output {
        status: ExitStatusExt::from_raw(911),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };

    // We perform a buffered read
    let mut buf = [0 as u8; 8*1024];
    loop {
        // We check for eof before-hand
        let eof = channel.eof();
        {   // extra scope allows to drop borrowed stream
            let mut stream = channel.stream(0); // stream 0 is stdout
            match await_wouldblock_io!(stream.read(&mut buf)){
                Err(e) => return Err(Error::ExecutionFailed(format!("Failed to read stdout: {:?}", e))),
                Ok(b) => output.stdout.write_all(&buf[..b]).unwrap()
            }
        }
        {  
            let mut stream = channel.stream(1); // stream 1 is stderr
            match await_wouldblock_io!(stream.read(&mut buf)){
                Err(e) => return Err(Error::ExecutionFailed(format!("Failed to read stderr: {:?}", e))),
                Ok(b) => {
                    output.stderr.write_all(&buf[..b]).unwrap();
                }
            }
        }
        if eof{  // enf of field reached, everything was read.
            break
        } else { // if not, we wait for a while
            async_sleep!(std::time::Duration::from_millis(1));
        }
    }
    Ok(output)
}


// Closes the exec channel retrieving tyhe exit status 
async fn close_exec(channel: &mut ssh2::Channel<'_>) -> Result<ExitStatus, Error>{
    trace!("Remote: Closing exec channel");

    // We close the channel and retrieve the execution code
    let ecode: Result<i32, ssh2::Error> = try {
        await_wouldblock_ssh!(channel.wait_eof())?;
        await_wouldblock_ssh!(channel.close())?;
        await_wouldblock_ssh!(channel.wait_close())?;
        await_wouldblock_ssh!(channel.exit_status())?
    };
    let ecode = ecode.map_err(|e| Error::ExecutionFailed(format!("Failed to close channel: {}", e)))?;
    Ok(ExitStatusExt::from_raw(ecode))
}


// Starts a pty channel
async fn acquire_pty_channel<'a>(remote: &Arc<Mutex<Remote>>) -> Result<ssh2::Channel<'_>, ssh2::Error>{
    trace!("Remote: Acquiring pty channel ");

    let mut channel = await_wouldblock_ssh!(await_retry_ssh!({remote.lock().await.session().channel_session()},-21))?;
    await_wouldblock_ssh!(await_retry_n_ssh!(channel.request_pty("xterm", None, Some((0,0,0,0))), 10, -14))?;
    await_wouldblock_ssh!(channel.shell())?;
    Ok(channel)
}


// Setups the pty 
async fn setup_pty(channel: &mut ssh2::Channel<'_>, cwd: &PathBuf, envs: &EnvironmentStore) -> Result<(), Error>{
    trace!("Remote: Setting up a pty channel");

    // We make sure we run on bash
    await_wouldblock_io!(channel.write_all("export HISTFILE=/dev/null\nbash\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to start bash: {}", e)))?;

    // We inject the linux pty agent on the remote end.
    await_wouldblock_io!(channel.write_all(BASH_PTY_AGENT.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to inject agent: {}", e)))?;
    await_wouldblock_io!(channel.write_all("rw_init\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to initialize agent: {}", e)))?;

    // We setup the context
    let context = envs.iter()
        .fold(format!("cd {}\n", cwd.to_str().unwrap()), |acc, (EnvironmentKey(n), EnvironmentValue(v))|{
            format!("{}export {}=\"{}\"\n", acc, n, v)
        });
    await_wouldblock_io!(channel.write_all(context.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to set context up: {}", e)))?;
    
    Ok(())
}


// Performs a set of pty commands
async fn perform_pty(channel: &mut ssh2::Channel<'_>, 
                     cmds: Vec<RawCommand<String>>, 
                     stdout_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
                     stderr_cb: Box<dyn Fn(Vec<u8>) + Send + 'static> ) 
                     -> Result<(TerminalContext<PathBuf>, Vec<Output>), Error>{
    trace!("Remote: Performs pty commands");

    // We prepare necessary variables
    let mut outputs = vec!();
    let mut cmds = cmds.into_iter()
            .map(|RawCommand(c)| c)
            .collect::<Vec<_>>();
    let mut stream = BufReader::new(channel);
    let mut buffer = String::new();
    let mut out_ctx = TerminalContext{
        cwd: Cwd(PathBuf::from("/")),
        envs: EnvironmentStore::new()
    };

    // We execute commands
    'commands: loop{ // We need named loops here
        // We write next command
        let cmd = cmds.remove(0);
        await_wouldblock_io!(stream.get_mut().write_all(format!("rw_run \"{}\"\n", cmd).as_bytes()))
            .map_err(|e| Error::ExecutionFailed(format!("Failed to exec command '{}': {}", cmd, e)))?;
        let output = Output {
            status: ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        outputs.push(output);

        // We read the output
        'messages: loop{
            buffer.clear();
            await_wouldblock_io!(stream.read_line(&mut buffer))
                .map_err(|e| Error::ExecutionFailed(format!("Failed to read outputs: {}", e)))?;
            buffer = buffer.replace("\r\n", "\n");
            // We receive an exit code
            if buffer.starts_with("RUNAWAY_ECODE: "){
                let ecode = buffer.replace("RUNAWAY_ECODE: ", "")
                    .replace("\n", "")
                    .parse::<i32>()
                    .unwrap();
                let mut output = outputs.last_mut().unwrap();
                output.status = ExitStatusExt::from_raw(ecode);
                // If non zero, we stop the execution now. 
                if ecode != 0{
                    cmds.clear();
                }
                // We write a new command if any
                if cmds.is_empty(){
                    await_wouldblock_io!(stream.get_mut().write_all("rw_close \n".as_bytes()))
                        .map_err(|e| Error::ExecutionFailed(format!("Failed to finish exec : {}", e)))?;
                    await_wouldblock_io!(stream.get_mut().flush()).unwrap();
                } else {
                    break 'messages
                }
            // We receive stdout message
            } else if buffer.starts_with("RUNAWAY_STDOUT: "){
                // We write the stdout message in the output command.
                let out = buffer.replace("RUNAWAY_STDOUT: ", "");
                outputs.last_mut().unwrap().stdout.write_all(out.as_bytes()).unwrap();
                stdout_cb(out.as_bytes().to_vec());
            // We receive an stderr message
            } else if buffer.starts_with("RUNAWAY_STDERR: "){
                // We write the stderr message in the output command.
                let err = buffer.replace("RUNAWAY_STDERR: ", "");
                outputs.last_mut().unwrap().stderr.write_all(err.as_bytes()).unwrap();
                stderr_cb(err.as_bytes().to_vec());
            // We receive a CWD message.
            } else if buffer.starts_with("RUNAWAY_CWD:"){
                out_ctx.cwd = Cwd(PathBuf::from(buffer.replace("RUNAWAY_CWD: ", "")));
            // We receive a env message.
            } else if buffer.starts_with("RUNAWAY_ENV:"){
                let env = buffer.replace("RUNAWAY_ENV: ", "")
                    .replace("\n", "")
                    .split('=')
                    .map(str::to_owned)
                    .collect::<Vec<String>>();
                out_ctx.envs.insert(EnvironmentKey(env.get(0).unwrap().to_owned()),
                                EnvironmentValue(env.get(1).unwrap_or(&format!("")).to_owned()));
            // We receive an EOF message.
            } else if buffer.starts_with("RUNAWAY_EOF:"){
                break 'commands
            }

        }
    }

    // We clear env of non-runaway environment variables.
    out_ctx.envs.retain(|EnvironmentKey(k), _| k.starts_with("RUNAWAY") );

    // We return    
    Ok((out_ctx, outputs))

}


// Closes a pty channel.
async fn close_pty(channel: &mut ssh2::Channel<'_>) -> Result<(), Error>{
    trace!("Remote: Closing pty channel");

    // We make sure to leave bash and the landing shell
    await_wouldblock_io!(channel.write_all("history -c \nexit\n history -c \nexit\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to start bash: {}", e)))?;

    // We close the channel
    trace!("Remote: Waiting to stop.");
    let out: Result<(), ssh2::Error> = try {
        await_wouldblock_ssh!(channel.wait_eof())?;
        await_wouldblock_ssh!(channel.close())?;
        await_wouldblock_ssh!(channel.wait_close())?;
    };
    if let Err(e) = out {
        return Err(Error::ExecutionFailed(format!("Failed to close channel: {}", e)))
    }
    Ok(())
}


// Sets scp send up
async fn setup_scp_send<'a>(remote: &'a Arc<Mutex<Remote>>, 
                        local_path: &PathBuf,
                        remote_path: &PathBuf) 
                        -> Result<(BufReader<File>, i64, ssh2::Channel<'a>), Error>{
    trace!("Remote: Setting up scp send");

    // Open local file and compute statistics
    let local_file = match File::open(local_path) {
        Ok(f) => BufReader::new(f),
        Err(e) => return Err(Error::ScpSendFailed(format!("Failed to open local file: {}", e))),
    };
    let bytes = local_file.get_ref().metadata().unwrap().len();
    
    // Open channel
    let ret = await_wouldblock_ssh!(
        {
            remote.lock()
                .await
                .session()
                .scp_send(&remote_path,0o755,bytes,None)
        }
    );
    let channel = match ret{
        Ok(chan) => chan,
        Err(ref e) if e.code() == -21 => {
            error!("Remote: Failed to obtain channel");
            return Err(Error::ChannelNotAvailable)
        },
        Err(e) => return Err(Error::ScpSendFailed(format!("Failed to open scp send channel: {}", e))),
    };
    Ok((local_file, bytes as i64, channel))
}


// Performs the scp send
async fn perform_scp_send(channel: &mut ssh2::Channel<'_>, 
                          local_file: &mut BufReader<File>, 
                          bytes: i64) -> Result<(), Error>{
    trace!("Remote: Performs scp send copy");
    let mut stream = channel.stream(0);
    let mut remaining_bytes = bytes as i64;
    loop{
        match local_file.fill_buf(){
            Ok(buf) => {
                let l = std::cmp::min(1_024_000, buf.len());
                let ret = await_wouldblock_io!(
                    stream.write(&buf[..l]).and_then(|r| stream.flush().and(Ok(r)))
                );
                match ret{
                    Ok(0) => {
                        if remaining_bytes == 0 {
                            break
                        }
                    }
                    Ok(b) => {
                        remaining_bytes -= b as i64;
                        local_file.consume(b);
                    }
                    Err(e) => return Err(Error::ScpSendFailed(format!("Failed to send data: {:?}", e))),
               }
            },
            Err(e) => return Err(Error::ScpSendFailed(format!("Failed to fill buffer: {}", e)))
        }
    }
    if remaining_bytes != 0{
        return Err(Error::ScpSendFailed(format!("Some bytes were not sent: {}", remaining_bytes)))
    }   
    Ok(())
}


// Closes scp channel
async fn close_scp_channel(channel: ssh2::Channel<'_>) -> Result<(), ssh2::Error>{
    trace!("Remote: Closing scp channel");

    let mut channel = channel;
    await_wouldblock_ssh!(channel.send_eof())?;
    await_wouldblock_ssh!(channel.wait_eof())?;
    await_wouldblock_ssh!(channel.close())?;
    await_wouldblock_ssh!(channel.wait_close())?;
    Ok(())
}


// Sets up scp fetch
async fn setup_scp_fetch<'a>(remote: &'a Arc<Mutex<Remote>>, 
                             remote_path: &PathBuf, 
                             local_path: &PathBuf ) 
                             -> Result<(File, ssh2::Channel<'a>, i64), Error>{
    trace!("Remote: Setting up scp fetch");

    let local_file = match OpenOptions::new().write(true).create_new(true).open(local_path) {
        Ok(f) => f,
        Err(e) => return Err(Error::ScpFetchFailed(format!("Failed to open local file: {}", e)))
    };
    let ret = await_wouldblock_ssh!(
        {
            remote.lock()
                .await
                .session()
                .scp_recv(&remote_path)
        }
    );
    let (channel, stats) = match ret {
        Ok(c) => c,
        Err(ref e) if e.code() == -21 => {
            error!("Remote: Failed to open channel...");
            return Err(Error::ChannelNotAvailable)
        }
        Err(e) => return Err(Error::ScpFetchFailed(format!("Failed to open scp recv channel: {}", e))),
    };
    Ok((local_file, channel, stats.size() as i64))
}


// Processes scp fetch
async fn process_scp_fetch(channel: &mut ssh2::Channel<'_>, 
                           local_file: File, 
                           remaining_bytes: i64) 
                           -> Result<(), Error>{
    trace!("Remote: Processing scp fetch");

    let mut remaining_bytes = remaining_bytes;
    let mut local_file = local_file;
    let mut stream = channel.stream(0);
    trace!("Remote: Starting scp send copy");
    let mut buf = [0; 8192];
    loop{
        match await_wouldblock_io!(stream.read(&mut buf)){
            Ok(0) => {
                if remaining_bytes == 0{
                    break
                }
            }
            Ok(r) => {
                match local_file.write(&buf[..r]){
                    Ok(w) => {
                        remaining_bytes -= w as i64;
                        local_file.flush().unwrap();
                    }
                    Err(e) => {
                        local_file.flush().unwrap();
                        return Err(Error::ScpFetchFailed(format!("Failed to fetch data: {}", e)))
                    }
                }
            }
            Err(e) => {
                return Err(Error::ScpFetchFailed(format!("Failed to read stream: {}", e)))
            }
        }
    }
    Ok(())
}


//--------------------------------------------------------------------------------------------- TEST


#[cfg(test)]
mod test {

    use super::*;
    use crate::ssh::config::SshProfile;
    use crate::misc;

    fn init_logger() {
        std::env::set_var("RUST_LOG", "liborchestra=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_proxy_command_forwarder() {
        let (proxy_command, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut buf = [0 as u8; 5];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(buf, "kikou".as_bytes());
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn test_async_exec() {
        use futures::executor::block_on;
        init_logger();
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            let command = RawCommand("echo kikou && sleep 1 && { echo hello! 1>&2 }".into());
            let output = remote.async_exec(command).await.unwrap();
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "hello!\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }


    #[test]
    fn test_async_pty_order() {
        use futures::executor::block_on;
        init_logger();
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("echo 1".into()), 
                          RawCommand("echo 2".into()),
                          RawCommand("echo 3".into()),
                          RawCommand("echo 4".into()),
                          RawCommand("echo 5".into()),
                          RawCommand("echo 6".into()),
                          RawCommand("echo 7".into()),
                          RawCommand("echo 8".into()),
                          RawCommand("echo 9".into()),
                          RawCommand("echo 10".into()),
                          RawCommand("echo 11".into()),
                          RawCommand("echo 12 && echo 13".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n");
            assert_eq!(output.status.code().unwrap(), 0);
       }
        block_on(test());
    }

    #[test]
    fn test_async_pty_cds() {
        use futures::executor::block_on;
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("cd /tmp".into()), 
                          RawCommand("pwd".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "/tmp\n");
            assert_eq!(output.status.code().unwrap(), 0);
       }
        block_on(test());
    }

    #[test]
    fn test_async_pty_envs() {
        use futures::executor::block_on;
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("a=KIKOU".into()), 
                          RawCommand("echo $a".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "KIKOU\n");
            assert_eq!(output.status.code().unwrap(), 0);
       }
        block_on(test());
    }

    #[test]
    fn test_async_pty_stderr() {
        use futures::executor::block_on;
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("echo kikou_stdout".into()), 
                          RawCommand("echo kikou_stderr 1>&2".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou_stdout\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "kikou_stderr\n");
            assert_eq!(output.status.code().unwrap(), 0);
       }
        block_on(test());
    }

    use crate::commons::OutputBuf;

    #[test]
    fn test_async_pty_program_stdout() {
        use futures::executor::block_on;
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: Some(22),
                proxycommand: None//Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("export PYTHONUNBUFFERED=x".into()),
                                RawCommand("echo Python: $(which python)".into()),
                                RawCommand("cd /home/apere/Downloads/test_runaway".into()), 
                                RawCommand("./run.py 10".into()),
                                RawCommand("echo Its over".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let outputs: Vec<OutputBuf> = outputs.into_iter().map(Into::into).collect();
            dbg!(outputs);
       }
        block_on(test());
    }

    #[test]
    fn test_async_pty_context() {
        use futures::executor::block_on;
        init_logger();
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("localhost".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let mut context = TerminalContext::default();
            context.cwd = Cwd("/tmp".into());
            context.envs.insert(EnvironmentKey("RW_TEST".into()),
                                             EnvironmentValue("VAL1".into()));
            let commands = vec![RawCommand("pwd".into()), 
                                RawCommand("echo $RW_TEST".into()),
                                RawCommand("export RW_TEST=VAL2".into())];
            let (context, outputs) = remote.async_pty(context, commands, None, None).await.unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "/tmp\nVAL1\n");
            assert_eq!(context.envs.get(&EnvironmentKey("RW_TEST".into())).unwrap(), &EnvironmentValue("VAL2".into()));
            assert_eq!(output.status.code().unwrap(), 0);
       }
        block_on(test());
    }
 

    #[test]
    fn test_async_scp_send() {
        use futures::executor::block_on;
        init_logger();
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/local.txt", "bs=35M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            remote.async_scp_send(local_f.clone(), remote_f.clone()).await.unwrap();
            let dur = std::time::Instant::now().duration_since(bef);
            println!("Duration: {:?}", dur);
            println!("Check if files are the same");
            let local_file = File::open(local_f).unwrap();
            let remote_file = File::open(remote_f).unwrap();
            local_file.bytes()
                .zip(remote_file.bytes())
                .for_each(|(a, b)| {
                    assert_eq!(a.unwrap(), b.unwrap())
                });
            std::fs::remove_file("/tmp/local.txt").unwrap();
            std::fs::remove_file("/tmp/remote.txt").unwrap();
        }
        block_on(test());
    }

    #[test]
    fn test_async_scp_fetch(){
        use futures::executor::block_on;
        init_logger();
        async fn test() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: Some(22),
                proxycommand: None, //Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/remote.txt", "bs=33M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            remote.async_scp_fetch(remote_f.clone(), local_f.clone()).await.unwrap();
            let dur = std::time::Instant::now().duration_since(bef);
            println!("Duration = {:?}", dur);
            println!("checking files");
            let local_file = File::open(local_f).unwrap();
            let remote_file = File::open(remote_f).unwrap();
            local_file.bytes()
                .zip(remote_file.bytes())
                .for_each(|(a, b)| {
                    assert_eq!(a.unwrap(), b.unwrap())
                });
            std::fs::remove_file("/tmp/local.txt").unwrap();
            std::fs::remove_file("/tmp/remote.txt").unwrap();
        }
        block_on(test());        
    }

    #[test]
    fn test_async_concurrent_exec(){

        //init_logger();

        let profile = SshProfile{
            name: "test".to_owned(),
            hostname: Some("localhost".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None // Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
        };
        let remote = RemoteHandle::spawn(profile).unwrap();
        async fn test(remote: RemoteHandle) -> Output{
            let command  = RawCommand("echo 1 && sleep 1 && echo 2".into());
            return remote.async_exec(command).await.unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..50{
            handles.push(executor.spawn_with_handle(test(remote.clone())).unwrap())
        }
        let bef = std::time::Instant::now();
        for handle in handles{
            let output = executor.run(handle);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "1\n2\n".to_owned());
        }
        let dur = std::time::Instant::now().duration_since(bef);
        println!("Duration: {:?}", dur);

    }

    #[test]
    fn test_async_concurrent_pty(){

        //init_logger();

        let profile = SshProfile{
            name: "test".to_owned(),
            hostname: Some("localhost".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None // Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
        };
        let remote = RemoteHandle::spawn(profile).unwrap();
        async fn test(remote: RemoteHandle) -> (TerminalContext<PathBuf>, Vec<Output>){
            let commands = vec![RawCommand("echo 1".into()), 
                          RawCommand("sleep 1".into()),
                          RawCommand("echo 2".into())];
            let context = TerminalContext::default();
            return remote.async_pty(context, commands, None, None)
                .await.unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..10{
            handles.push(executor.spawn_with_handle(test(remote.clone())).unwrap())
        }
        let bef = std::time::Instant::now();
        for handle in handles{
            let (_, outputs) = executor.run(handle);
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "1\n2\n".to_owned());
        }
        let dur = std::time::Instant::now().duration_since(bef);
        println!("Duration: {:?}", dur);

    }
}
