//! This module contains a structure that wraps the ssh2 session object to provide ways to handle
//! ssh configurations, authentications, and proxy-commands. In particular, since libssh2
//! does not provide ways to handle proxy-commands, we propose our own structure based on a threaded
//! copy loop that opens a proxy-command as a subprocess and copy the output on a random tcp socket.

//------------------------------------------------------------------------------------------ IMPORTS

use crate::commons::Dropper;
use crate::commons::{
    Cwd, EnvironmentKey, EnvironmentStore, EnvironmentValue, RawCommand, TerminalContext,
};
use crate::derive_from_error;
use crate::KNOWN_HOSTS_RPATH;
use crate::*;
use dirs;
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future;
use futures::future::Future;
use futures::lock::Mutex;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use futures::SinkExt;
use futures::StreamExt;
use libc::{signal, SIGINT, SIG_IGN};
use ssh2::{CheckResult, KnownHostFileKind, KnownHostKeyFormat, Session};
use std::intrinsics::transmute;
use std::os::unix::process::CommandExt;
use std::{
    collections::HashSet,
    error, fmt,
    fs::{File, OpenOptions},
    io::{prelude::*, BufReader},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    os::unix::process::ExitStatusExt,
    path::PathBuf,
    process::{Command, ExitStatus, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};
use tracing::{self, debug, error, instrument, trace, trace_span, warn};
use tracing_futures::Instrument;

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
static BASH_PTY_AGENT_INIT: &str = include_str!("agent_init.sh");
static BASH_PTY_AGENT_RUN: &str = include_str!("agent_run.sh");
static BASH_PTY_AGENT_CLOSE: &str = include_str!("agent_close.sh");

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
            Error::WouldBlock => write!(f, "Blocking operation."),
            Error::ConnectionFailed(ref s) => write!(
                f,
                "An error occurred while trying to connect to a remote \
                host: \n{}",
                s
            ),
            Error::ProxyCommandStartup(ref s) => {
                write!(f, "An error occurred when starting a proxycommand: \n{}", s)
            }
            Error::ExecutionFailed(s) => write!(
                f,
                "An error occurred while running a command on a remote host: \n{}",
                s
            ),
            Error::ScpSendFailed(s) => write!(
                f,
                "An error occurred while sending a file to a remote host: \n{}",
                s
            ),
            Error::ScpFetchFailed(s) => write!(
                f,
                "An error occurred while fetching a file from a remote host: \n{}",
                s
            ),
            Error::PollExecutionFailed(s) => {
                write!(f, "An error occurred while polling a command: \n{}", s)
            }
            Error::FuturePoll(s) => write!(f, "An error occurred while polling a future: \n{}", s),
            Error::SpawnThread(s) => {
                write!(f, "An error occured while spawning the thread: \n{}", s)
            }
            Error::ChannelNotAvailable => write!(f, "Channel is not yet available"),
            Error::Channel(e) => write!(f, "A channel related error occured: {}", e),
            Error::OperationFetch(e) => write!(f, "Failed to fetch the operation.: {}", e),
            Error::Config(e) => write!(f, "An ssh config-related error occurred: {}", e),
        }
    }
}

derive_from_error!(Error, config::Error, Config);

impl From<Error> for crate::commons::Error {
    fn from(other: Error) -> crate::commons::Error {
        crate::commons::Error::Operation(format!("{}", other))
    }
}

//------------------------------------------------------------------------------------ PROXY-COMMAND

/// This structure starts a proxy command which is forwarded on a tcp socket. This allows
/// a libssh2 session to get connected through a proxy command. On the first connection to the
/// socket, the proxycommand will be started in a subprocess, and the reads from the socket will be
/// copied to the process stdin, and the stdout from the process will be written to the socket. The
/// forthcoming connections are rejected. The messages get forwarded as long as the forwarder stays
/// in scope. The forwarding is explicitly stopped when the forwarder is dropped.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ProxyCommandForwarder {
    #[derivative(Debug = "ignore")]
    keep_alive: Arc<AtomicBool>,
    #[derivative(Debug = "ignore")]
    handle: Option<thread::JoinHandle<(thread::JoinHandle<()>, thread::JoinHandle<()>)>>,
    command: String,
}

impl ProxyCommandForwarder {
    /// Creates a new `ProxyCommandForwarder` from a command. An available port is automatically
    /// given by the OS, and is returned along with the forwarder.
    #[instrument(name = "ProxyCommandForwarder::from_command")]
    pub fn from_command(command: &str) -> Result<(ProxyCommandForwarder, SocketAddr), Error> {
        trace!("Starting tcp listener");
        let stream = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(s) => s,
            Err(e) => {
                return Err(Error::ProxyCommandStartup(format!(
                    "Failed to find a port to bind to: {}",
                    e
                )))
            }
        };
        let command_string = command.to_owned();
        let c1 = command.to_owned();
        let c2 = command.to_owned();
        let c3 = command.to_owned();
        let address = stream.local_addr().unwrap();
        let keep_forwarding = Arc::new(AtomicBool::new(true));
        let kf = keep_forwarding.clone();
        let mut cmd = command.split_whitespace().collect::<Vec<_>>();
        let args = cmd.split_off(1);
        let cmd = cmd.pop().unwrap();

        trace!("Spawning proxy command");
        let mut command = unsafe {
            Command::new(cmd)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                // This allows to make sure the proxycommand ignores Ctrl-C. The opposite would
                // prevent the program to cleanup properly.
                .pre_exec(|| {
                    signal(SIGINT, SIG_IGN);
                    Ok(())
                })
                .spawn()
                .map_err(|e| Error::ProxyCommandStartup(format!("failed to start command: {}", e)))
        }?;
        debug!(
            "ProxyCommand {} started with pid {}",
            command_string,
            command.id()
        );

        trace!("Spawning proxy command forwarding thread");
        let handle = std::thread::Builder::new()
            .name("proxycommand".into())
            .spawn(move || {
                let span = trace_span!("ProxyCommandForwarder::InitThread", command = c1.as_str());
                let _guard = span.enter();
                let (socket, _) = stream.accept().unwrap();
                let mut socket1 = socket.try_clone().unwrap();
                let mut socket2 = socket.try_clone().unwrap();
                let kf1 = kf.clone();
                let kf2 = kf.clone();
                let mut command_stdin = command.stdin.take().unwrap();
                let mut command_stdout = command.stdout.take().unwrap();

                trace!("Creating stdout forwarding thread");
                let h1 = std::thread::spawn(move || {
                    let span =
                        trace_span!("ProxyCommandForwarder::StdoutThread", command = c2.as_str());
                    let _guard = span.enter();
                    while kf1.load(Ordering::Relaxed) {
                        match std::io::copy(&mut command_stdout, &mut socket1) {
                            Err(e) => {
                                error!("stdout forwarding failed: {}", e);
                                break;
                            }
                            Ok(0) => {
                                error!("stdout forwarding returned prematurely");
                                break;
                            }
                            Ok(_) => socket1.flush().unwrap(),
                        }
                    }
                    match socket1.shutdown(Shutdown::Write) {
                        Ok(_) => trace!("Forwarding shutdown"),
                        Err(e) => warn!("Failed to shutdown forwarding: {}", e),
                    };
                });

                trace!("Creating stderr forwarding thread");
                let h2 = std::thread::spawn(move || {
                    let span =
                        trace_span!("ProxyCommandForwarder::StderrThread", command = c3.as_str());
                    let _guard = span.enter();
                    while kf2.load(Ordering::Relaxed) {
                        match std::io::copy(&mut socket2, &mut command_stdin) {
                            Err(e) => {
                                error!("stdin forwarding failed: {}", e);
                                break;
                            }
                            Ok(0) => break,
                            Ok(_) => command_stdin.flush().unwrap(),
                        }
                    }
                    match socket2.shutdown(Shutdown::Read) {
                        Ok(_) => trace!("Forwarding shutdown"),
                        Err(e) => warn!("Failed to shutdown forwarding: {}", e),
                    };
                });
                (h1, h2)
            })
            .unwrap();

        trace!("Returning proxy command");
        Ok((
            ProxyCommandForwarder {
                keep_alive: keep_forwarding,
                handle: Some(handle),
                command: command_string,
            },
            address,
        ))
    }
}

// Care must be taken to join the threads at drop, to appropriately close the connection and the
// process.
impl Drop for ProxyCommandForwarder {
    #[instrument(name = "ProxyCommandForwarder::drop")]
    fn drop(&mut self) {
        trace!("Stop keeping alive");
        self.keep_alive.store(false, Ordering::Relaxed);
        trace!("Joining init thread handle.");
        let handle = self.handle.take().unwrap();
        trace!("Joining each thread handle.");
        let (h1, h2) = handle.join().unwrap();
        h1.join().unwrap();
        h2.join().unwrap();
    }
}

//-------------------------------------------------------------------------------- REMOTE CONNECTION

// A synchronous wrapper over a non-blocking Remote.
#[derive(Derivative)]
#[derivative(Debug)]
struct Remote {
    #[derivative(Debug = "ignore")]
    session: Option<&'static Session>,
    #[derivative(Debug = "ignore")]
    proxycommand: Option<ProxyCommandForwarder>,
    repr: String,
}

impl Drop for Remote {
    #[instrument(name = "Remote::drop")]
    fn drop(&mut self) {
        // Unsafe trick to avoid memory leaks. Since we know that all our references to the
        // session were dropped we transmute the session to a Box and drop it.
        unsafe {
            let sess: Box<Session> = transmute(self.session.take().unwrap() as *const Session);
            // We disconnect the session
            match sess.disconnect(None, "Over", None) {
                Ok(_) => trace!("Successfully disconnected."),
                Err(e) => error!("Failed to disconnect from remote: {}", e),
            }
            drop(sess);
        }
    }
}

impl Remote {
    /// Helper to avoid boilerplate.
    #[inline]
    fn session(&self) -> &'static Session {
        self.session.as_ref().unwrap()
    }

    /// Build a Remote from a profile.
    #[instrument(name = "Remote::from_profile")]
    fn from_profile(profile: config::SshProfile) -> Result<Remote, Error> {
        trace!("Creating remote connection.");
        match profile {
            config::SshProfile {
                hostname: Some(host),
                user: Some(user),
                proxycommand: Some(cmd),
                ..
            } => Remote::from_proxy_command(&cmd, &host, &user),
            config::SshProfile {
                hostname: Some(host),
                user: Some(user),
                proxycommand: None,
                port: Some(port),
                ..
            } => {
                let address = format!("{}:{}", host, port);
                Remote::from_addr(address, &host, &user)
            }
            _ => Err(Error::ConnectionFailed(format!(
                "The encountered profile is invalid: \
                 \n{:?}\nIt should either provide a proxycommand or a port",
                profile
            ))),
        }
    }

    /// Build, authenticate and starts an ssh session from an adress.
    #[instrument(name = "Remote::from_addr")]
    fn from_addr(
        addr: impl ToSocketAddrs + fmt::Display + fmt::Debug,
        host: &str,
        user: &str,
    ) -> Result<Remote, Error> {
        trace!("Creating connection from address");
        let stream = TcpStream::connect(&addr).map_err(|_| {
            Error::ConnectionFailed(format!("Failed to connect to address {}", addr))
        })?;
        let session = Remote::start_session(stream, host, user)?;
        let session: &'static Session = Box::leak(Box::new(session));
        Ok(Remote {
            session: Some(session),
            proxycommand: None,
            repr: format!("{}", addr),
        })
    }

    /// Build, authenticate and starts an ssh session from a proxycommand.
    #[instrument(name = "Remote::from_proxy_command")]
    fn from_proxy_command(command: &str, host: &str, user: &str) -> Result<Remote, Error> {
        trace!("Creating remote connection from proxycommand");
        let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
        let mut remote = Remote::from_addr(addr, host, user)?;
        remote.proxycommand.replace(proxy_command);
        remote.repr = format!("{}", command);
        Ok(remote)
    }

    /// Starts the ssh session.
    #[instrument(name = "Remote::start_session")]
    fn start_session(stream: TcpStream, host: &str, user: &str) -> Result<Session, Error> {
        trace!("Opening remote connection to host");
        let mut session = new_session(stream)?;
        authenticate_host(host, &mut session)?;
        authenticate_local(user, &mut session)?;
        session.set_blocking(false);
        Ok(session)
    }

    /// Asynchronous function used to execute a command on the remote.
    #[instrument(name = "Remote::exec")]
    async fn exec(
        remote: Arc<Mutex<Remote>>,
        command: RawCommand<String>,
    ) -> Result<Output, Error> {
        trace!("Executing command");
        let RawCommand(cmd) = command;
        let mut channel = acquire_exec_channel(&remote).await?;
        setup_exec(cmd, &mut channel).await?;
        let mut output = read_exec_out_err(&mut channel).await?;
        output.status = close_exec(&mut channel).await?;
        Ok(output)
    }

    /// Asynchronous function used to execute an interactive command on the remote.
    #[instrument(
        name = "Remote::pty",
        skip(remote, context, commands, stdout_cb, stderr_cb)
    )]
    async fn pty(
        remote: Arc<Mutex<Remote>>,
        context: TerminalContext<PathBuf>,
        commands: Vec<RawCommand<String>>,
        stdout_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
        stderr_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
    ) -> Result<(TerminalContext<PathBuf>, Vec<Output>), Error> {
        trace!("Executing pty commands");
        if commands.is_empty() {
            return Err(Error::ExecutionFailed(
                "No command was provided.".to_string(),
            ));
        }
        let TerminalContext {
            cwd: Cwd(cwd),
            envs,
        } = context;
        let mut channel = acquire_pty_channel(&remote)
            .await
            .map_err(|e| Error::ExecutionFailed(format!("Failed to start pty channel: {}", e)))?;
        setup_pty(&mut channel, &cwd, &envs).await?;
        let (context, outputs) = perform_pty(&mut channel, commands, stdout_cb, stderr_cb).await?;
        close_pty(&mut channel).await?;

        Ok((context, outputs))
    }

    /// Asynchronous function used to send a file to a remote.
    #[instrument(name = "Remote::scp_send")]
    async fn scp_send(
        remote: Arc<Mutex<Remote>>,
        local_path: PathBuf,
        remote_path: PathBuf,
    ) -> Result<(), Error> {
        trace!("Starting transfer");
        let (mut local_file, bytes, mut channel) =
            setup_scp_send(&remote, &local_path, &remote_path).await?;
        perform_scp_send(&mut channel, &mut local_file, bytes).await?;
        close_scp_channel(channel)
            .await
            .map_err(|e| Error::ScpSendFailed(format!("Failed to close channel: {}", e)))?;
        Ok(())
    }

    /// Asynchronous method used to fetch a file from remote.
    #[instrument(name = "Remote::scp_fetch")]
    async fn scp_fetch(
        remote: Arc<Mutex<Remote>>,
        remote_path: PathBuf,
        local_path: PathBuf,
    ) -> Result<(), Error> {
        trace!("Starting transfer");
        std::fs::remove_file(&local_path).unwrap_or(());
        let (file, mut channel, bytes) =
            setup_scp_fetch(&remote, &remote_path, &local_path).await?;
        process_scp_fetch(&mut channel, file, bytes).await?;
        close_scp_channel(channel)
            .await
            .map_err(|e| Error::ScpFetchFailed(format!("Failed to close channel: {}", e)))?;
        Ok(())
    }
}

/// The operation inputs, sent by the outer futures and processed by inner thread.
enum OperationInput {
    Exec(RawCommand<String>),
    Pty(
        TerminalContext<PathBuf>,
        Vec<RawCommand<String>>,
        Box<dyn Fn(Vec<u8>) + Send + 'static>,
        Box<dyn Fn(Vec<u8>) + Send + 'static>,
    ),
    ScpSend(PathBuf, PathBuf),
    ScpFetch(PathBuf, PathBuf),
}

impl std::fmt::Debug for OperationInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationInput::Exec(c) => write!(f, "Exec({:?})", c),
            OperationInput::Pty(_, c, _out, _err) => write!(
                f,
                "Pty({:?}, {}, {})",
                c,
                stringify!(_out),
                stringify!(_err)
            ),
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
    ScpFetch(Result<(), Error>),
}

/// A handle to an inner application. Offer a future interface to the inner application.
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct RemoteHandle {
    repr: String,
    #[derivative(Debug = "ignore")]
    sender: mpsc::UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    #[derivative(Debug = "ignore")]
    profile: config::SshProfile,
    #[derivative(Debug = "ignore")]
    dropper: Dropper,
}

impl PartialEq for RemoteHandle {
    fn eq(&self, other: &Self) -> bool {
        self.profile == other.profile
    }
}

impl RemoteHandle {
    /// Spawns the application and returns a handle to it.
    #[instrument(name = "RemoteHandle::spawn")]
    pub fn spawn(profile: config::SshProfile) -> Result<RemoteHandle, Error> {
        trace!("Start remote thread.");
        let (sender, receiver) = mpsc::unbounded();
        let (start_tx, start_rx) = crossbeam::channel::unbounded();
        let repr = match &profile.proxycommand {
            Some(p) => p.to_string(),
            None => format!(
                "{}:{}",
                profile.hostname.as_ref().unwrap(),
                profile.port.as_ref().unwrap()
            ),
        };
        let moving_profile = profile.clone();
        let handle = std::thread::Builder::new()
            .name("remote".to_owned())
            .spawn(move || {
                let span = trace_span!("Remote::Thread");
                let _guard = span.enter();
                trace!("Creating resource in thread");
                let remote = match Remote::from_profile(moving_profile) {
                    Ok(r) => {
                        start_tx.send(Ok(())).unwrap();
                        r
                    }
                    Err(e) => {
                        start_tx.send(Err(e)).unwrap();
                        return;
                    }
                };
                let stream_span = trace_span!("Handling_Stream", ?remote);
                let remote = Arc::new(Mutex::new(remote));
                trace!("Starting handling stream");
                let mut pool = executor::LocalPool::new();
                let mut spawner = pool.spawner();
                let handling_stream = receiver.for_each(
                    move |(sender, operation): (
                        oneshot::Sender<OperationOutput>,
                        OperationInput,
                    )| {
                        let span = stream_span.clone();
                        let _guard = span.enter();
                        trace!(?operation, "Received operation");
                        match operation {
                            OperationInput::Exec(command) => spawner.spawn_local(
                                Remote::exec(remote.clone(), command)
                                    .map(|a| {
                                        sender
                                            .send(OperationOutput::Exec(a))
                                            .map_err(|e| {
                                                error!(
                                                    "Failed to \\
                                            send an operation output: \n{:?}",
                                                    e
                                                )
                                            })
                                            .unwrap();
                                    })
                                    .instrument(span.clone()),
                            ),
                            OperationInput::Pty(context, commands, stdout_cb, stderr_cb) => spawner
                                .spawn_local(
                                    Remote::pty(
                                        remote.clone(),
                                        context,
                                        commands,
                                        stdout_cb,
                                        stderr_cb,
                                    )
                                    .map(|a| {
                                        sender
                                            .send(OperationOutput::Pty(a))
                                            .map_err(|e| {
                                                error!(
                                                    "Failed to \\
                                            send an operation output: \n{:?}",
                                                    e
                                                )
                                            })
                                            .unwrap();
                                    })
                                    .instrument(span.clone()),
                                ),
                            OperationInput::ScpSend(local_path, remote_path) => spawner
                                .spawn_local(
                                    Remote::scp_send(remote.clone(), local_path, remote_path)
                                        .map(|a| {
                                            sender
                                                .send(OperationOutput::ScpSend(a))
                                                .map_err(|e| {
                                                    error!(
                                                        "Failed to \\
                                            send an operation output: \n{:?}",
                                                        e
                                                    )
                                                })
                                                .unwrap();
                                        })
                                        .instrument(span.clone()),
                                ),
                            OperationInput::ScpFetch(remote_path, local_path) => spawner
                                .spawn_local(
                                    Remote::scp_fetch(remote.clone(), remote_path, local_path)
                                        .map(|a| {
                                            sender
                                                .send(OperationOutput::ScpFetch(a))
                                                .map_err(|e| {
                                                    error!(
                                                        "Failed to \\
                                            send an operation output: \n{:?}",
                                                        e
                                                    )
                                                })
                                                .unwrap();
                                        })
                                        .instrument(span.clone()),
                                ),
                        }
                        .map_err(|e| error!(error=?e, "Failed to spawn operation"))
                        .unwrap();
                        future::ready(())
                    },
                );
                let mut spawner = pool.spawner();
                spawner
                    .spawn_local(handling_stream)
                    .map_err(|e| error!(error=?e, "Failed to spawn handling stream"))
                    .unwrap();
                trace!("Starting local executor.");
                pool.run();
                trace!("All futures executed. Leaving...");
            })
            .expect("Failed to spawn application thread.");
        start_rx.recv().unwrap()?;
        let drop_sender = sender.clone();
        Ok(RemoteHandle {
            repr,
            sender,
            profile,
            dropper: Dropper::from_closure(
                Box::new(move || {
                    drop_sender.close_channel();
                    handle.join().unwrap();
                }),
                "RemoteHandle".to_string(),
            ),
        })
    }

    /// A function that returns a future that resolves in a result over an output, after the command
    /// was executed.
    pub fn async_exec(
        &self,
        command: RawCommand<String>,
    ) -> impl Future<Output = Result<Output, Error>> {
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("Sending exec input");
            chan.send((sender, OperationInput::Exec(command)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("Awaiting exec output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Exec(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!(
                    "Expected Exec, found {:?}",
                    e
                ))),
            }
        }
        .instrument(trace_span!("RemoteHandle::async_exec"))
    }

    /// A function that returns a future that resolves in a result over an output, after the command
    /// was executed in interactive mode. Callbacks can be provided to be called on stdout and
    /// stderr messages.
    pub fn async_pty(
        &self,
        context: TerminalContext<PathBuf>,
        commands: Vec<RawCommand<String>>,
        stdout_cb: Option<Box<dyn Fn(Vec<u8>) + Send + 'static>>,
        stderr_cb: Option<Box<dyn Fn(Vec<u8>) + Send + 'static>>,
    ) -> impl Future<Output = Result<(TerminalContext<PathBuf>, Vec<Output>), Error>> {
        let mut chan = self.sender.clone();
        let mut stdout_cb = stdout_cb;
        if stdout_cb.is_none() {
            stdout_cb = Some(Box::new(|_| {}));
        }
        let mut stderr_cb = stderr_cb;
        if stderr_cb.is_none() {
            stderr_cb = Some(Box::new(|_| {}));
        }
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("Sending pty input");
            chan.send((
                sender,
                OperationInput::Pty(context, commands, stdout_cb.unwrap(), stderr_cb.unwrap()),
            ))
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("Awaiting pty output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Pty(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!(
                    "Expected Pty, found {:?}",
                    e
                ))),
            }
        }
        .instrument(trace_span!("RemoteHandle::async_pty"))
    }

    /// A function that returns a future that resolves in a result over an empty type, after the
    /// file was sent.
    pub fn async_scp_send(
        &self,
        local_path: PathBuf,
        remote_path: PathBuf,
    ) -> impl Future<Output = Result<(), Error>> {
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("Sending scp send input");
            chan.send((sender, OperationInput::ScpSend(local_path, remote_path)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("Awaiting scp send output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::ScpSend(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!(
                    "Expected ScpSend, found {:?}",
                    e
                ))),
            }
        }
        .instrument(trace_span!("RemoteHandle::async_scp_send"))
    }

    /// A function that returns a future that resolves in a result over an empty type, after the
    /// file was fetch
    pub fn async_scp_fetch(
        &self,
        remote_path: PathBuf,
        local_path: PathBuf,
    ) -> impl Future<Output = Result<(), Error>> {
        let mut chan = self.sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("Sending scp fetch input");
            chan.send((sender, OperationInput::ScpFetch(remote_path, local_path)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("Awaiting scp fetch output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::ScpFetch(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!(
                    "Expected ScpFetch, found {:?}",
                    e
                ))),
            }
        }
        .instrument(trace_span!("RemoteHandle::async_scp_fetch"))
    }
}

//--------------------------------------------------------------------------------------- PROCEDURES

// Generates a new session following our preferences
#[instrument]
fn new_session(stream: TcpStream) -> Result<Session, Error> {
    trace!("Creates a new session");
    let mut session = Session::new().unwrap();
    session.set_tcp_stream(stream);
    session
        .handshake()
        .map_err(|e| Error::ConnectionFailed(format!("Failed to perform handshake: \n{}", e)))?;
    Ok(session)
}

// Checks the host identity against our known hosts.
#[instrument(name = "authenticate_host", skip(host, session))]
fn authenticate_host(host: &str, session: &mut Session) -> Result<(), Error> {
    trace!("Checking host key");
    // We set the session known hosts to the database file
    let mut known_hosts = session.known_hosts().unwrap();
    let mut known_hosts_path = dirs::home_dir().ok_or(Error::ConnectionFailed(
        "Failed to find the local home directory".to_string(),
    ))?;
    known_hosts_path.push(KNOWN_HOSTS_RPATH);
    if !known_hosts_path.exists() {
        File::create(known_hosts_path.clone()).map_err(|e| {
            Error::ConnectionFailed(format!("Failed to create knownhost file: {}", e))
        })?;
    }
    known_hosts
        .read_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
        .map_err(|e| Error::ConnectionFailed(format!("Failed to reach knownhost file:\n{}", e)))?;

    // We retrieve the host key
    let (key, _) = session.host_key().ok_or(Error::ConnectionFailed(
        "Host did not provide a key.".to_string(),
    ))?;

    // We check host key against known identities
    match known_hosts.check(host, key) {
        // Host recognized
        CheckResult::Match => Ok(()),
        // Host is different
        CheckResult::Mismatch => {
            trace!("Host key mismatch....");
            Err(Error::ConnectionFailed(
                "The key presented by the host mismatches the one we know. ".to_string(),
            ))
        }
        // Host not in database
        CheckResult::NotFound => {
            trace!("Host not known. Writing the key in the registry.");
            known_hosts
                .add(host, key, "", KnownHostKeyFormat::SshRsa)
                .map_err(|_| {
                    Error::ConnectionFailed(
                        "Failed to add the host key in the registry".to_string(),
                    )
                })?;
            known_hosts
                .write_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
                .map_err(|_| {
                    Error::ConnectionFailed("Failed to write the knownhost file".into())
                })?;
            Ok(())
        }
        // Unknown error
        CheckResult::Failure => {
            trace!("Failed to check the host");
            Err(Error::ConnectionFailed(
                "Failed to check the host.".to_string(),
            ))
        }
    }
}

// Authenticates ourselves on the remote end, using an ssh agent.
#[instrument(name = "authenticate_local", skip(session))]
fn authenticate_local(user: &str, session: &mut Session) -> Result<(), Error> {
    trace!("Authenticating ourselves");
    // We retrieve the agent.
    let mut agent = session.agent().unwrap();
    agent
        .connect()
        .map_err(|e| Error::ConnectionFailed(format!("Ssh Agent was unavailable: \n{}", e)))?;
    agent
        .list_identities()
        .map_err(|e| Error::ConnectionFailed(format!("Couldn't list identities: \n{}", e)))?;

    // We go through the stored identities to try to authenticate.
    let ids = agent
        .identities()
        .map(|id| id.unwrap().comment().to_owned())
        .collect::<HashSet<_>>();
    let failed_ids = agent
        .identities()
        .take_while(|id| agent.userauth(user, id.as_ref().unwrap()).is_err())
        .map(|id| id.unwrap().comment().to_owned())
        .collect::<HashSet<_>>();
    if ids == failed_ids {
        warn!("No identities registered in the agent allowed to authenticate.");
        Err(Error::ConnectionFailed(
            "No identities registered in the ssh agent allowed to connect.".into(),
        ))
    } else {
        trace!("Authenticated.");
        Ok(())
    }
}

// Acquires an exec channel on the remote
#[instrument(skip(remote))]
async fn acquire_exec_channel(remote: &Arc<Mutex<Remote>>) -> Result<ssh2::Channel, Error> {
    trace!("Acquiring exec channel.");
    // We query a channel session. Error -21 corresponds to missing available channels. It
    // must be retried until an other execution comes to an end, and makes a channel available.
    let ret = await_wouldblock_ssh!(await_retry_ssh!(
        { remote.lock().await.session().channel_session() },
        -21
    ));
    match ret {
        Ok(c) => Ok(c),
        Err(e) => Err(Error::ExecutionFailed(format!(
            "Failed to open channel: {}",
            e
        ))),
    }
}

// Performs exec command.
#[instrument(skip(channel))]
async fn setup_exec(cmd: String, channel: &mut ssh2::Channel) -> Result<(), Error> {
    trace!("Perform exec command `{}`", cmd);
    // We execute the command in the cwd.
    await_wouldblock_ssh!(channel.exec(&format!("{}\n", cmd)))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to exec command {}: {}", cmd, e)))?;

    // We close
    await_wouldblock_ssh!(channel.send_eof())
        .map_err(|e| Error::ExecutionFailed(format!("Failed to send EOF: {}", e)))?;
    Ok(())
}

// Reads the output of an exec command, and returns the output.
#[instrument(skip(channel))]
async fn read_exec_out_err(channel: &mut ssh2::Channel) -> Result<Output, Error> {
    trace!("Reading exec output");
    // We generate a new output
    let mut output = Output {
        status: ExitStatusExt::from_raw(911),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };

    // We perform a buffered read
    let mut buf = [0 as u8; 8 * 1024];
    loop {
        // We check for eof before-hand
        let eof = channel.eof();
        {
            // extra scope allows to drop borrowed stream
            trace!("Reading stdout");
            let mut stream = channel.stream(0); // stream 0 is stdout
            match await_wouldblock_io!(stream.read(&mut buf)) {
                Err(e) => {
                    return Err(Error::ExecutionFailed(format!(
                        "Failed to read stdout: {:?}",
                        e
                    )))
                }
                Ok(b) => output.stdout.write_all(&buf[..b]).unwrap(),
            }
        }
        {
            trace!("Reading stderr");
            let mut stream = channel.stream(1); // stream 1 is stderr
            match await_wouldblock_io!(stream.read(&mut buf)) {
                Err(e) => {
                    return Err(Error::ExecutionFailed(format!(
                        "Failed to read stderr: {:?}",
                        e
                    )))
                }
                Ok(b) => {
                    output.stderr.write_all(&buf[..b]).unwrap();
                }
            }
        }
        if eof {
            // enf of field reached, everything was read.
            trace!("Eof reached.");
            break;
        } else {
            // if not, we wait for a while
            trace!("Eof not yet reached");
            async_sleep!(std::time::Duration::from_millis(1));
        }
    }
    Ok(output)
}

// Closes the exec channel retrieving tyhe exit status
#[instrument(skip(channel))]
async fn close_exec(channel: &mut ssh2::Channel) -> Result<ExitStatus, Error> {
    trace!("Closing exec channel");
    // We close the channel and retrieve the execution code
    let ecode: Result<i32, ssh2::Error> = try {
        await_wouldblock_ssh!(channel.wait_eof())?;
        await_wouldblock_ssh!(channel.close())?;
        await_wouldblock_ssh!(channel.wait_close())?;
        await_wouldblock_ssh!(channel.exit_status())?
    };
    let ecode =
        ecode.map_err(|e| Error::ExecutionFailed(format!("Failed to close channel: {}", e)))?;
    Ok(ExitStatusExt::from_raw(ecode))
}

// Starts a pty channel
#[instrument(skip(remote))]
async fn acquire_pty_channel<'a>(
    remote: &Arc<Mutex<Remote>>,
) -> Result<ssh2::Channel, ssh2::Error> {
    trace!("Acquiring pty channel");
    let mut channel = await_wouldblock_ssh!(await_retry_ssh!(
        { remote.lock().await.session().channel_session() },
        -21
    ))?;
    await_wouldblock_ssh!(await_retry_n_ssh!(
        channel.request_pty("ansi", None, Some((0, 0, 0, 0))),
        10,
        -14
    ))?;
    await_wouldblock_ssh!(channel.shell())?;
    Ok(channel)
}

// Setups the pty
#[instrument(skip(channel))]
async fn setup_pty(
    channel: &mut ssh2::Channel,
    cwd: &PathBuf,
    envs: &EnvironmentStore,
) -> Result<(), Error> {
    trace!("Setting up a pty channel");
    // We make sure we run on bash
    await_wouldblock_io!(channel.write_all("export HISTFILE=/dev/null\nexec bash\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to start bash: {}", e)))?;

    // We inject the linux pty agent on the remote end, pieces by pieces to avoid overflowing the pty.
    await_wouldblock_io!(channel.write_all(BASH_PTY_AGENT_INIT.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to inject agent: {}", e)))?;
    std::thread::sleep(std::time::Duration::from_millis(10));
    await_wouldblock_io!(channel.write_all(BASH_PTY_AGENT_RUN.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to inject agent: {}", e)))?;
    std::thread::sleep(std::time::Duration::from_millis(10));
    await_wouldblock_io!(channel.write_all(BASH_PTY_AGENT_CLOSE.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to inject agent: {}", e)))?;
    std::thread::sleep(std::time::Duration::from_millis(10));
    await_wouldblock_io!(channel.write_all("rw_init\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to initialize agent: {}", e)))?;

    // We setup the context
    let context = envs.iter().fold(
        format!("cd {}\n", cwd.to_str().unwrap()),
        |acc, (EnvironmentKey(n), EnvironmentValue(v))| format!("{}export {}='{}'\n", acc, n, v),
    );
    await_wouldblock_io!(channel.write_all(context.as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to set context up: {}", e)))?;

    Ok(())
}

// Performs a set of pty commands
#[instrument(skip(channel, stdout_cb, stderr_cb))]
async fn perform_pty(
    channel: &mut ssh2::Channel,
    cmds: Vec<RawCommand<String>>,
    stdout_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
    stderr_cb: Box<dyn Fn(Vec<u8>) + Send + 'static>,
) -> Result<(TerminalContext<PathBuf>, Vec<Output>), Error> {
    trace!("Performs pty commands");
    // We prepare necessary variables
    let mut outputs = vec![];
    let mut cmds = cmds.into_iter().map(|RawCommand(c)| c).collect::<Vec<_>>();
    let mut stream = BufReader::new(channel);
    let mut buffer = String::new();
    let mut out_ctx = TerminalContext {
        cwd: Cwd(PathBuf::from("/")),
        envs: EnvironmentStore::new(),
    };

    // We execute commands
    'commands: loop {
        // We need named loops here
        // We write next command
        let cmd = cmds.remove(0);
        trace!(?cmd, "Writing next command");
        await_wouldblock_io!(stream
            .get_mut()
            .write_all(format!("rw_run '{}'\n", cmd).as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to exec command '{}': {}", cmd, e)))?;
        let output = Output {
            status: ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        outputs.push(output);

        // We read the output
        'messages: loop {
            buffer.clear();
            await_wouldblock_io!(stream.read_line(&mut buffer))
                .map_err(|e| Error::ExecutionFailed(format!("Failed to read outputs: {}", e)))?;
            buffer = buffer.replace("\r\n", "\n");
            debug!("Reading command output: {:?}", buffer);
            // We receive an exit code
            if buffer.starts_with("RUNAWAY_ECODE: ") {
                trace!("Ecode message detected");
                let ecode = buffer
                    .replace("RUNAWAY_ECODE: ", "")
                    .replace("\n", "")
                    .parse::<i32>()
                    .unwrap();
                let mut output = outputs.last_mut().unwrap();
                output.status = ExitStatusExt::from_raw(ecode);
                // If non zero, we stop the execution now.
                if ecode != 0 {
                    trace!("Non zero ecode.");
                    cmds.clear();
                }
                // We write a new command if any
                if cmds.is_empty() {
                    trace!("No more commands");
                    await_wouldblock_io!(stream.get_mut().write_all("rw_close \n".as_bytes()))
                        .map_err(|e| {
                            Error::ExecutionFailed(format!("Failed to finish exec : {}", e))
                        })?;
                    await_wouldblock_io!(stream.get_mut().flush()).unwrap();
                } else {
                    trace!("To the next command");
                    break 'messages;
                }
            // We receive stdout message
            } else if buffer.starts_with("RUNAWAY_STDOUT: ") {
                trace!("Stdout message detected");
                // We write the stdout message in the output command.
                let out = buffer.replace("RUNAWAY_STDOUT: ", "");
                outputs
                    .last_mut()
                    .unwrap()
                    .stdout
                    .write_all(out.as_bytes())
                    .unwrap();
                stdout_cb(out.as_bytes().to_vec());
            // We receive an stderr message
            } else if buffer.starts_with("RUNAWAY_STDERR: ") {
                trace!("Stderr message detected");
                // We write the stderr message in the output command.
                let err = buffer.replace("RUNAWAY_STDERR: ", "");
                outputs
                    .last_mut()
                    .unwrap()
                    .stderr
                    .write_all(err.as_bytes())
                    .unwrap();
                stderr_cb(err.as_bytes().to_vec());
            // We receive a CWD message.
            } else if buffer.starts_with("RUNAWAY_CWD:") {
                trace!("Cwd message detected");
                out_ctx.cwd = Cwd(PathBuf::from(buffer.replace("RUNAWAY_CWD: ", "")));
            // We receive a env message.
            } else if buffer.starts_with("RUNAWAY_ENV:") {
                trace!("Env message detected");
                let env = buffer
                    .replace("RUNAWAY_ENV: ", "")
                    .replace("\n", "")
                    .split('=')
                    .map(str::to_owned)
                    .collect::<Vec<String>>();
                out_ctx.envs.insert(
                    EnvironmentKey(env.get(0).unwrap().to_owned()),
                    EnvironmentValue(env.get(1).unwrap_or(&format!("")).to_owned()),
                );
            // We receive an EOF message.
            } else if buffer.starts_with("RUNAWAY_EOF:") {
                trace!("Eof message detected. Commands are over.");
                break 'commands;
            }
        }
    }

    // We clear env of non-runaway environment variables.
    out_ctx
        .envs
        .retain(|EnvironmentKey(k), _| k.starts_with("RUNAWAY"));

    // We return
    Ok((out_ctx, outputs))
}

// Closes a pty channel.
#[instrument(skip(channel))]
async fn close_pty(channel: &mut ssh2::Channel) -> Result<(), Error> {
    trace!("Closing pty channel");
    // We make sure to leave bash and the landing shell
    await_wouldblock_io!(channel.write_all("history -c\nexit\n".as_bytes()))
        .map_err(|e| Error::ExecutionFailed(format!("Failed to start bash: {}", e)))?;

    // We close the channel
    trace!("Waiting to stop.");
    let out: Result<(), ssh2::Error> = try {
        await_wouldblock_ssh!(channel.wait_eof())?;
        await_wouldblock_ssh!(channel.close())?;
        await_wouldblock_ssh!(channel.wait_close())?;
    };
    if let Err(e) = out {
        return Err(Error::ExecutionFailed(format!(
            "Failed to close channel: {}",
            e
        )));
    }
    Ok(())
}

// Sets scp send up
#[instrument(skip(remote))]
async fn setup_scp_send<'a>(
    remote: &'a Arc<Mutex<Remote>>,
    local_path: &PathBuf,
    remote_path: &PathBuf,
) -> Result<(BufReader<File>, i64, ssh2::Channel), Error> {
    trace!("Setting up scp send");
    // Open local file and compute statistics
    let local_file = match File::open(local_path) {
        Ok(f) => BufReader::new(f),
        Err(e) => {
            return Err(Error::ScpSendFailed(format!(
                "Failed to open local file: {}",
                e
            )))
        }
    };
    let bytes = local_file.get_ref().metadata().unwrap().len();

    // Open channel
    let ret = await_wouldblock_ssh!({
        remote
            .lock()
            .await
            .session()
            .scp_send(&remote_path, 0o755, bytes, None)
    });
    let channel = match ret {
        Ok(chan) => chan,
        Err(ref e) if e.code() == -21 => {
            error!("Failed to obtain channel");
            return Err(Error::ChannelNotAvailable);
        }
        Err(e) => {
            return Err(Error::ScpSendFailed(format!(
                "Failed to open scp send channel. Does the path exist ? : {}",
                e
            )))
        }
    };
    Ok((local_file, bytes as i64, channel))
}

// Performs the scp send
#[instrument(skip(channel, local_file, bytes))]
async fn perform_scp_send(
    channel: &mut ssh2::Channel,
    local_file: &mut BufReader<File>,
    bytes: i64,
) -> Result<(), Error> {
    trace!("Performing scp send copy");
    let mut stream = channel.stream(0);
    let mut remaining_bytes = bytes as i64;
    loop {
        match local_file.fill_buf() {
            Ok(buf) => {
                let l = std::cmp::min(1_024_000, buf.len());
                let ret = await_wouldblock_io!(stream
                    .write(&buf[..l])
                    .and_then(|r| stream.flush().and(Ok(r))));
                match ret {
                    Ok(0) => {
                        if remaining_bytes == 0 {
                            break;
                        }
                    }
                    Ok(b) => {
                        remaining_bytes -= b as i64;
                        local_file.consume(b);
                    }
                    Err(e) => {
                        return Err(Error::ScpSendFailed(format!(
                            "Failed to send data: {:?}",
                            e
                        )))
                    }
                }
            }
            Err(e) => {
                return Err(Error::ScpSendFailed(format!(
                    "Failed to fill buffer: {}",
                    e
                )))
            }
        }
    }
    if remaining_bytes != 0 {
        return Err(Error::ScpSendFailed(format!(
            "Some bytes were not sent: {}",
            remaining_bytes
        )));
    }
    Ok(())
}

// Closes scp channel
#[instrument(skip(channel))]
async fn close_scp_channel(channel: ssh2::Channel) -> Result<(), ssh2::Error> {
    trace!("Closing scp channel");
    let mut channel = channel;
    await_wouldblock_ssh!(channel.send_eof())?;
    await_wouldblock_ssh!(channel.wait_eof())?;
    await_wouldblock_ssh!(channel.close())?;
    await_wouldblock_ssh!(channel.wait_close())?;
    Ok(())
}

// Sets up scp fetch
#[instrument(skip(remote))]
async fn setup_scp_fetch<'a>(
    remote: &'a Arc<Mutex<Remote>>,
    remote_path: &PathBuf,
    local_path: &PathBuf,
) -> Result<(File, ssh2::Channel, i64), Error> {
    trace!("Setting up scp fetch");
    let local_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(local_path)
    {
        Ok(f) => f,
        Err(e) => {
            return Err(Error::ScpFetchFailed(format!(
                "Failed to open local file: {}",
                e
            )))
        }
    };
    let ret = await_wouldblock_ssh!({ remote.lock().await.session().scp_recv(&remote_path) });
    let (channel, stats) = match ret {
        Ok(c) => c,
        Err(ref e) if e.code() == -21 => {
            error!("Failed to open channel...");
            return Err(Error::ChannelNotAvailable);
        }
        Err(e) => {
            return Err(Error::ScpFetchFailed(format!(
                "Failed to open scp recv channel: {}",
                e
            )))
        }
    };
    Ok((local_file, channel, stats.size() as i64))
}

// Processes scp fetch
#[instrument(skip(channel))]
async fn process_scp_fetch(
    channel: &mut ssh2::Channel,
    local_file: File,
    remaining_bytes: i64,
) -> Result<(), Error> {
    trace!("Processing scp fetch");
    let mut remaining_bytes = remaining_bytes;
    let mut local_file = local_file;
    let mut stream = channel.stream(0);
    trace!("Starting scp send copy");
    let mut buf = [0; 8192];
    loop {
        match await_wouldblock_io!(stream.read(&mut buf)) {
            Ok(0) => {
                if remaining_bytes == 0 {
                    break;
                }
            }
            Ok(r) => match local_file.write(&buf[..r]) {
                Ok(w) => {
                    remaining_bytes -= w as i64;
                    local_file.flush().unwrap();
                }
                Err(e) => {
                    local_file.flush().unwrap();
                    return Err(Error::ScpFetchFailed(format!(
                        "Failed to fetch data: {}",
                        e
                    )));
                }
            },
            Err(e) => {
                return Err(Error::ScpFetchFailed(format!(
                    "Failed to read stream: {}",
                    e
                )))
            }
        }
    }
    Ok(())
}

//--------------------------------------------------------------------------------------------- TEST

#[cfg(test)]
mod test {

    use super::*;
    use crate::commons::OutputBuf;
    use crate::misc;
    use crate::ssh::config::SshProfile;
    use futures::executor;
    use futures::executor::block_on;
    use futures::task::SpawnExt;
    use futures::Future;
    use shells::wrap_sh;
    use std::pin::Pin;

    static TEST_FOLDER: &str = "/tmp/runaway_test";

    fn random_test_path() -> String {
        format!("{}/{}", TEST_FOLDER, misc::get_uuid())
    }

    fn get_profile() -> SshProfile {
        let user = misc::get_user();
        config::SshProfile {
            name: "test".to_owned(),
            hostname: Some("127.0.0.1".to_owned()),
            user: Some(misc::get_user()),
            port: None,
            proxycommand: Some(format!("ssh -A -l {} localhost -W localhost:22", user)),
        }
    }

    #[test]
    fn test_proxy_command_forwarder() {
        let (_proxy_command, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(TcpStream::connect(address).is_err());
        let mut buf = [0 as u8; 6];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(buf, "kikou\n".as_bytes());
    }

    #[test]
    fn test_async_exec() { 
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            let command = RawCommand("echo kikou && sleep 1 && echo hello! 1>& 2".into());
            let output = remote.async_exec(command).await.unwrap();
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "hello!\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_order() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![
                RawCommand("echo 1".into()),
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
                RawCommand("echo 12 && echo 13".into()),
            ];
            let context = TerminalContext::default();
            let (_, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n"
            );
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_cds() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![RawCommand("cd /tmp".into()), RawCommand("pwd".into())];
            let context = TerminalContext::default();
            let (_, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "/tmp\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_envs() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![
                RawCommand("a=\"KIKOU KIKOU\"".into()),
                RawCommand("echo $a".into()),
            ];
            let context = TerminalContext::default();
            let (_, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "KIKOU KIKOU\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_stderr() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![
                RawCommand("echo kikou_stdout".into()),
                RawCommand("echo kikou_stderr 1>&2".into()),
            ];
            let context = TerminalContext::default();
            let (_, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou_stdout\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "kikou_stderr\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_program_stdout() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let commands = vec![
                RawCommand("export PYTHONUNBUFFERED=x".into()),
                RawCommand("echo Python: $(which python)".into()),
                RawCommand("cd /home/apere/Downloads/test_runaway".into()),
                RawCommand("./run.py 10".into()),
                RawCommand("echo Its over".into()),
            ];
            let context = TerminalContext::default();
            let (_, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let outputs: Vec<OutputBuf> = outputs.into_iter().map(Into::into).collect();
            dbg!(outputs);
        }
        block_on(test());
    }

    #[test]
    fn test_async_pty_context() {
        async fn test() {
            let profile = get_profile();
            let remote = RemoteHandle::spawn(profile).unwrap();
            // Check order of outputs
            let mut context = TerminalContext::default();
            context.cwd = Cwd("/tmp".into());
            context.envs.insert(
                EnvironmentKey("RUNAWAY_TEST".into()),
                EnvironmentValue("VAL1".into()),
            );
            let commands = vec![
                RawCommand("pwd".into()),
                RawCommand("echo $RUNAWAY_TEST".into()),
                RawCommand("export RUNAWAY_TEST=\"VAL2 VAL3\"".into()),
            ];
            let (context, outputs) = remote
                .async_pty(context, commands, None, None)
                .await
                .unwrap();
            let output = misc::compact_outputs(outputs);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "/tmp\nVAL1\n");
            assert_eq!(
                context
                    .envs
                    .get(&EnvironmentKey("RUNAWAY_TEST".into()))
                    .unwrap(),
                &EnvironmentValue("VAL2 VAL3".into())
            );
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(test());
    }

    fn test_transfer<F: FnOnce(String, String) -> Pin<Box<dyn Future<Output = ()>>>>(
        transfer_func: F,
    ) {
        let first_file = random_test_path();
        let second_file = random_test_path();
        wrap_sh!("dd if=/dev/urandom of={} bs=4096 count=1", first_file).unwrap();
        let before = std::time::Instant::now();
        block_on(transfer_func(first_file.clone(), second_file.clone()));
        let dur = std::time::Instant::now().duration_since(before);
        let first_file = File::open(first_file).unwrap();
        let second_file = File::open(second_file).unwrap();
        first_file
            .bytes()
            .zip(second_file.bytes())
            .for_each(|(a, b)| assert_eq!(a.unwrap(), b.unwrap()));
    }

    #[test]
    fn test_async_scp_send() {
        let profile = get_profile();
        let remote = RemoteHandle::spawn(profile).unwrap();
        let transfer_func = move |first_file: String, second_file: String| {
            async move {
                remote
                    .async_scp_send(first_file.clone().into(), second_file.clone().into())
                    .await
                    .unwrap();
            }
            .boxed_local()
        };
        test_transfer(transfer_func);
    }

    #[test]
    fn test_async_scp_fetch() {
        let profile = get_profile();
        let remote = RemoteHandle::spawn(profile).unwrap();
        let transfer_func = move |first_file: String, second_file: String| {
            async move {
                remote
                    .async_scp_fetch(first_file.clone().into(), second_file.clone().into())
                    .await
                    .unwrap();
            }
            .boxed_local()
        };
        test_transfer(transfer_func);
    }

    fn test_async_concurrent<T: Fn(RemoteHandle) -> Pin<Box<dyn Future<Output = ()> + Send>>>(
        test: T,
    ) {
        let profile = get_profile();
        let remote = RemoteHandle::spawn(profile).unwrap();

        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..50 {
            handles.push(executor.spawn_with_handle(test(remote.clone())).unwrap())
        }
        for handle in handles {
            executor.run(handle);
        }
    }

    #[test]
    fn test_async_concurrent_exec() {
        fn test(remote: RemoteHandle) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            async move {
                let command = RawCommand("echo 1 && sleep 1 && echo 2".into());
                let output = remote.async_exec(command).await.unwrap();
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    "1\n2\n".to_owned()
                );
            }
            .boxed()
        }
        test_async_concurrent(test);
    }

    #[test]
    fn test_async_concurrent_pty() {
        fn test(remote: RemoteHandle) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            async move {
                let commands = vec![
                    RawCommand("echo 1".into()),
                    RawCommand("sleep 1".into()),
                    RawCommand("echo 2".into()),
                ];
                let context = TerminalContext::default();
                let (_, outputs) = remote
                    .async_pty(context, commands, None, None)
                    .await
                    .unwrap();
                let output = misc::compact_outputs(outputs);
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    "1\n2\n".to_owned()
                );
            }
            .boxed()
        }
        test_async_concurrent(test);
    }
}
