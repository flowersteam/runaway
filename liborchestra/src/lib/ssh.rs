// liborchestra/ssh.rs
// Author: Alexandre Péré

/// This module contains structures that wraps the ssh2 session object to provide ways to handle 
/// ssh configurations, authentications, and proxy-commands. In particular, since libssh2
/// does not provide ways to handle proxy-commands, we use a threaded copy loop that open a
/// proxycommand as a subprocess and copy the output on a random tcp socket. 
///
/// After instantiation, the session object is used through new style futures. For this reason,
/// after authentication a thread is spawned that will take care about handling the operations that
/// may be required by the user. Most importantly, particular care is taken to wake the futures
/// when the operations are over. This means that the session object is a pure ressource, and does
/// not need any other care. 
///
/// Note: Though ssh operations are handled asynchronously, the connection and the handshake are 
/// made in a synchronous and blocking manner. 

// IMPORTS
use ssh2::{
    Session,
    KnownHosts,
    KnownHostFileKind,
    CheckResult,
    MethodType,
    KnownHostKeyFormat};
use std::{
    net::{TcpListener, TcpStream, SocketAddr, ToSocketAddrs, Shutdown},
    sync::{
        Arc,
        Mutex,
        mpsc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    pin::Pin,
    task::{Poll, Waker},
    future::Future,
    io::{prelude::*, BufReader, copy},
    process::{Stdio, Command, Output},
    os::unix::process::ExitStatusExt,
    time::{Instant, Duration},
    error,
    fmt,
    path::PathBuf,
    collections::HashSet,
    fs::{File, OpenOptions},
};
use dirs;
use crate::{PROFILES_FOLDER_RPATH, KNOWN_HOSTS_RPATH};

// ERRORS
#[derive(Debug)]
pub enum Error {
    ProxySocketNotFound,
    ProxyCommandReturned,
    NoSuchFile,
    ConnectionFailed,
    PreferenceSettingFailed,
    HandshakeFailed,
    HomeNotFound,
    KnownHostsUnreachable,
    CouldntRegisterHost,
    CouldntSaveKnownHosts,
    HostKeyMismatch,
    HostNotKnown,
    HostCheckFailed,
    SshAgentUnavailable,
    ClientAuthFailed,
    ReceiverCalledOnEmptyChannel,
    ChannelClosed,
    ExecutionFailed,
    ScpSendFailed,
    ScpFetchFailed,
    Unknown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ProxySocketNotFound => write!(f, "Failed to find a socket to bind the proxy command on."),
            ProxyCommandReturned => write!(f, "The proxy command returned. Check command and connection"),
            NoSuchFile => write!(f, "The given file could not be found"),
            ConnectionFailed => write!(f, "Failed to connect to the provided host."),
            PreferenceSettingFailed => write!(f, "Failed to set ssh preferences for handshake"),
            HandshakeFailed => write!(f, "Failed to handshake with remote host."),
            HomeNotFound => write!(f, "Home folder could not be found through the $HOME variable"),
            KnownHostsUnreachable => write!(f, "Failed to read runaway known_hosts file."),
            CouldntRegisterHost => write!(f, "Failed to register host"),
            CouldntSaveKnownHosts => write!(f, "Failed to save the runaway known_hosts file"),
            Unknown => write!(f, "Unknown error occured"),
            HostKeyMismatch => write!(f, "Host was found but the key does not match!"),
            HostNotKnown => write!(f, "Host was not found in the known hosts."),
            HostCheckFailed => write!(f, "Host key check failed somehow."),
            SshAgentUnavailable => write!(f, "Ssh Agent could not be found. Is there an ssh agent on $SSH_AUTH_SOCK ?"),
            ClientAuthFailed => write!(f, "Client authentication failed. Is the necessary identity available in the ssh agent ?"),
            ReceiverCalledOnEmptyChannel => write!(f, "A receiver was called on an empty channel"),
            ChannelClosed => write!(f, "A channel was used but was closed"),
            ExecutionFailed => write!(f, "The execution of a command failed"),
            ScpSendFailed => write!(f, "The sending of the file failed."),
            ScpFetchFailed => write!(f, "The fetching of the file failed."),
        }
    }
}

// PROXYCOMMAND
/// This structure starts a proxy command which is forwarded on a tcp socket. This allows
/// a libssh2 session to get connected through a proxy command. On the first connection to the
/// socket, the proxycommand will be started in a subprocess, and the reads from the socket will be
/// copied to the process stdin, and the stdout from the process will be written to the socket. The 
/// forthcoming connections are rejected. The messages get forwarded as long as the forwarder stays 
/// in scope. The forwarding is explicitly stopped when the forwarder is dropped. 
pub struct ProxyCommandForwarder {
    keep_forwarding: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<(thread::JoinHandle<()>,
                                       thread::JoinHandle<()>,
                                       thread::JoinHandle<()>)>>,
}

impl ProxyCommandForwarder {
    /// Creates a new `ProxyCommandForwarder` from a command. An available port is automatically
    /// given by the OS, and is returned along with the forwarder. 
    pub fn from_command(command: &str) -> Result<(ProxyCommandForwarder, SocketAddr), Error> {
        debug!("Starting proxy command: {}", command);
        trace!("Starting proxy command tcp listener");
        let stream = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(s) => s,
            Err(_) => return Err(Error::ProxySocketNotFound)
        };
        let address = stream.local_addr().unwrap();
        let mut keep_forwarding = Arc::new(AtomicBool::new(true));
        let mut kf = keep_forwarding.clone();
        let mut cmd = command.split_whitespace().collect::<Vec<_>>();
        let args = cmd.split_off(1);
        let cmd = cmd.pop().unwrap();
        trace!("Spawning proxy command");
        let mut command = Command::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        trace!("Spawning proxy command forwarding thread");
        let handle = std::thread::spawn(move || {
            let (mut socket, addr) = stream.accept().unwrap();
            let mut socket1 = socket.try_clone().unwrap();
            let mut socket2 = socket.try_clone().unwrap();
            let mut kf1 = kf.clone();
            let mut kf2 = kf.clone();
            let mut kf3 = kf.clone();
            let mut command_stdin = command.stdin.take().unwrap();
            let mut command_stdout = command.stdout.take().unwrap();
            let h1 = std::thread::spawn(move || {
                while kf1.load(Ordering::Relaxed) {
                    if std::io::copy(&mut command_stdout, &mut socket1).is_err() {
                        break;
                    }
                }
                socket1.shutdown(Shutdown::Write);
                trace!("Exiting command -> socket thread");
            });
            let h2 = std::thread::spawn(move || {
                while kf2.load(Ordering::Relaxed) {
                    if std::io::copy(&mut socket2, &mut command_stdin).is_err() {
                        break;
                    }
                }
                socket2.shutdown(Shutdown::Read);
                trace!("Exiting socker -> command thread");
            });
            let h3 = std::thread::spawn(move || {
                while kf3.load(Ordering::Relaxed) {
                    if let Ok(Some(e)) = command.try_wait() {
                        trace!("Proxy Command has stopped. Exiting");
                        kf3.store(false, Ordering::Relaxed);
                    }
                }
                trace!("Exiting alive watch thread");
            });
            return (h1, h2, h3);
        });
        trace!("Returning proxy command");
        return Ok((ProxyCommandForwarder {
            keep_forwarding,
            handle: Some(handle),
        },
                   address));
    }
}

// Care must be taken to join the threads at drop, to appropriately close the connection and the
// process. This is made done here. 
impl Drop for ProxyCommandForwarder { 
    fn drop(&mut self) {
        debug!("Dropping proxy command");
        trace!("Stop forwarding");
        self.keep_forwarding.store(false, Ordering::Relaxed);
        trace!("Waiting for thread handles");
        let handle = self.handle.take().unwrap();
        let (h1, h2, h3) = handle.join().unwrap();
        h1.join();
        h2.join();
        h3.join();
        trace!("Proxy command fully dropped");
    }
}


// REMOTE OPERATIONS
#[doc(hidden)]
#[derive(Debug)]
/// This enumeration represents the different operations that can be made by the RemoteConnection.
enum RemoteOperations {
    ScpSend((PathBuf, PathBuf)),
    ScpFetch((PathBuf, PathBuf)),
    Exec(String),
}

// REMOTE EXECUTION FUTURE
/// This Future is created by the `RemoteConnection::async_exec` method, and allows to execute a
/// command on the remote host. Ultimately resolves in a result over a process output.
pub struct RemoteExecFuture {
    state: RemoteExecFutureState,
    connection: Option<RemoteConnection>,
}

enum RemoteExecFutureState {
    Starting(String),
    Waiting,
    Finished,
}

impl Future for RemoteExecFuture {
    type Output = Result<(RemoteConnection, Output), Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<(RemoteConnection, Output), Error>> {
        debug!("Remote execution Future is being polled ...");
        loop {
            match &self.state {
                RemoteExecFutureState::Starting(command) => {
                    trace!("Sending ssh execution operation");
                    let (state, ret) = self.connection
                        .as_ref()
                        .unwrap()
                        .op_tx
                        .as_ref()
                        .unwrap()
                        .send((RemoteOperations::Exec(command.to_string()), wake.clone()))
                        .map_or_else(
                            |_| {(RemoteExecFutureState::Finished, Poll::Ready(Err(Error::ChannelClosed)))},
                            |_| {(RemoteExecFutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                RemoteExecFutureState::Waiting => {
                    trace!("Retrieving ssh execution results");
                    let ret = self.connection
                        .as_ref()
                        .unwrap()
                        .exc_rx
                        .try_recv()
                        .unwrap_or_else(|e| {
                            match e {
                                mpsc::TryRecvError::Empty => Err(Error::ReceiverCalledOnEmptyChannel),
                                mpsc::TryRecvError::Disconnected => Err(Error::ChannelClosed),
                            }
                        })
                        .map_or_else(
                            |e| { Poll::Ready(Err(Error::ExecutionFailed))},
                            |output| { Poll::Ready(Ok((self.connection.take().unwrap(), output)))});
                    self.state = RemoteExecFutureState::Finished;
                    return ret;
                }
                RemoteExecFutureState::Finished => {
                    panic!("RemoteExecFuture was polled in a finished state");
                }
            }
        }
    }
}

// SCP SEND FUTURE
/// This future allows to send a file via SCP. Ultimately resolves to a result over an empty type.
pub struct ScpSendFuture {
    state: ScpSendFutureState,
    connection: Option<RemoteConnection>,
}

enum ScpSendFutureState {
    Starting((PathBuf, PathBuf)),
    Waiting,
    Finished,
}

impl Future for ScpSendFuture {
    type Output = Result<RemoteConnection, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<RemoteConnection, Error>> {
        debug!("Scp Send Future is being polled ...");
        loop {
            match &self.state {
                ScpSendFutureState::Starting((local_path, remote_path)) => {
                    trace!("Sending scp send operation");
                    let (state, ret) = self.connection
                        .as_ref()
                        .unwrap()
                        .op_tx
                        .as_ref()
                        .unwrap()
                        .send((RemoteOperations::ScpSend((local_path.to_owned(), remote_path.to_owned())),
                               wake.clone()))
                        .map_or_else(
                            |_| {(ScpSendFutureState::Finished, Poll::Ready(Err(Error::ChannelClosed)))},
                            |_| {(ScpSendFutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                ScpSendFutureState::Waiting => {
                    trace!("Retrieving scp send result");
                    let ret = self.connection
                        .as_ref()
                        .unwrap()
                        .scps_rx
                        .try_recv()
                        .unwrap_or_else(|e| {
                            match e {
                                mpsc::TryRecvError::Empty => Err(Error::ReceiverCalledOnEmptyChannel),
                                mpsc::TryRecvError::Disconnected => Err(Error::ChannelClosed),
                            }
                        })
                        .map_or_else(
                            |e|{Poll::Ready(Err(e))},
                            |_|{Poll::Ready(Ok(self.connection.take().unwrap()))}
                        );
                    self.state = ScpSendFutureState::Finished;
                    return ret;
                }
                ScpSendFutureState::Finished => {
                    panic!("ScpSendFuture was polled in a finished state");
                }
            }
        }
    }
}

// SCP FETCH FUTURE
/// This future allows to fetch a file over SCP. Ultimately resolves into a result over an empty
/// type. 
pub struct ScpFetchFuture {
    state: ScpFetchFutureState,
    connection: Option<RemoteConnection>,
}

enum ScpFetchFutureState {
    Starting((PathBuf, PathBuf)),
    Waiting,
    Finished,
}

impl Future for ScpFetchFuture {
    type Output = Result<RemoteConnection, Error>;
    fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<Result<RemoteConnection, Error>> {
        debug!("Scp Fetch Future is being polled ...");
        loop {
            match &self.state {
                ScpFetchFutureState::Starting((remote_path, local_path)) => {
                    trace!("Sending scp Fetch operation");
                    let (state, ret) = self.connection
                        .as_ref()
                        .unwrap()
                        .op_tx
                        .as_ref()
                        .unwrap()
                        .send((RemoteOperations::ScpFetch((remote_path.to_owned(), local_path.to_owned())),
                               wake.clone()))
                        .map_or_else(
                            |_| {(ScpFetchFutureState::Finished, Poll::Ready(Err(Error::ChannelClosed)))},
                            |_| {(ScpFetchFutureState::Waiting, Poll::Pending)});
                    self.state = state;
                    return ret;
                }
                ScpFetchFutureState::Waiting => {
                    trace!("Retrieving scp fetch result");
                    let ret = self.connection
                        .as_ref()
                        .unwrap()
                        .scpf_rx
                        .try_recv()
                        .unwrap_or_else(|e| {
                            match e {
                                mpsc::TryRecvError::Empty => Err(Error::ReceiverCalledOnEmptyChannel),
                                mpsc::TryRecvError::Disconnected => Err(Error::ChannelClosed),
                            }
                        })
                        .map_or_else(
                            |e|{Poll::Ready(Err(e))},
                            |_|{Poll::Ready(Ok(self.connection.take().unwrap()))}
                        );
                    self.state = ScpFetchFutureState::Finished;
                    return ret;
                }
                ScpFetchFutureState::Finished => {
                    panic!("ScpFetchFuture was polled in a finished state");
                }
            }
        }
    }
}

/// This structure allows to perform operations on a remote host. Operations are executed in a
/// separated thread, which allows to avoid moving and synchronizing the ssh2 session between
/// different tasks. As such, operations are represented by futures, which sends operations to
/// perform to the connection thread through a channel. The connection thread performs the
/// operation and notifies the waker when the operation is done.
pub struct RemoteConnection {
    proxy_command: Option<ProxyCommandForwarder>,
    thread_handle: Option<thread::JoinHandle<()>>,
    scps_rx: mpsc::Receiver<Result<(), Error>>,
    scpf_rx: mpsc::Receiver<Result<(), Error>>,
    exc_rx: mpsc::Receiver<Result<Output, Error>>,
    op_tx: Option<mpsc::Sender<(RemoteOperations, Waker)>>,
}

impl RemoteConnection {

    // Authenticate the ssh session. 
    fn start_session(stream: &TcpStream, host: &str, user: &str) -> Result<Session, Error> {
        debug!("Opening remote connection to host {} as user {}", host, user);
        let mut session = Session::new().unwrap();
        session.method_pref(MethodType::HostKey, "ssh-rsa")
            .map_err(|_| Error::PreferenceSettingFailed)?;
        trace!("Performing handshake");
        session.handshake(stream).map_err(|_| Error::HandshakeFailed)?;
        {
            trace!("Checking host key");
            let mut known_hosts = session.known_hosts().unwrap();
            let mut known_hosts_path = dirs::home_dir().ok_or(Error::HomeNotFound)?;
            known_hosts_path.push(PROFILES_FOLDER_RPATH);
            known_hosts_path.push(KNOWN_HOSTS_RPATH);
            known_hosts.read_file(known_hosts_path.as_path(),
                                  KnownHostFileKind::OpenSSH)
                .map_err(|_| Error::KnownHostsUnreachable)?;
            let (key, key_type) = session.host_key().ok_or(Error::Unknown)?;
            match known_hosts.check(host, key) {
                CheckResult::Match => {}
                CheckResult::Mismatch => {
                    trace!("Host key mismatch....");
                    return Err(Error::HostKeyMismatch);
                }
                CheckResult::NotFound => {
                    trace!("Host not known. Writing the key in the registry.");
                    known_hosts.add(host,
                                    key,
                                    "",
                                    KnownHostKeyFormat::SshRsa)
                        .map_err(|_| Error::CouldntRegisterHost)?;
                    known_hosts.write_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
                        .map_err(|_| Error::CouldntSaveKnownHosts)?;
                }
                CheckResult::Failure => {
                    trace!("Failed to check the host");
                    return Err(Error::HostCheckFailed);
                }
            }
            trace!("Authentificating ourselves");
            let mut agent = session.agent().unwrap();
            agent.connect().map_err(|_| Error::SshAgentUnavailable)?;
            agent.list_identities().map_err(|_| Error::Unknown)?;
            let ids = agent.identities()
                .into_iter()
                .map(|id| id.unwrap().comment().to_owned())
                .collect::<HashSet<_>>();
            let failed_ids = agent.identities()
                .into_iter()
                .take_while(|id| {
                    agent.userauth(user, id.as_ref().unwrap()).is_err()
                })
                .map(|id| { id.unwrap().comment().to_owned() })
                .collect::<HashSet<_>>();
            if ids == failed_ids {
                trace!("No identities registered in the agent allowed to authenticate.");
                return Err(Error::ClientAuthFailed);
            }
        }
        trace!("Ssh session ready.");
        return Ok(session);
    }
    
    // Spawn the operation handling thread. This thread dispatch operation to other functions.
    fn spawn_thread(session: Session,
                    stream: TcpStream, 
                    op_rx: mpsc::Receiver<(RemoteOperations, Waker)>,
                    exc_tx: mpsc::Sender<Result<Output, Error>>,
                    scps_tx: mpsc::Sender<Result<(), Error>>,
                    scpf_tx: mpsc::Sender<Result<(), Error>>,
                    ) -> thread::JoinHandle<()> {
        debug!("Start connection thread");
        let thread_handle = thread::spawn(move || {
            trace!("In thread; starting operation loop");
            let stream = stream;
            let mut session = session;
            while let Ok((operation, waker)) = op_rx.recv() {
                trace!("Received operation to perform: {:?}", operation);
                match operation {
                    RemoteOperations::Exec(command) => {
                        let to_send = RemoteConnection::handle_exec_operation(&session, &command);
                        exc_tx.send(to_send).unwrap();
                    }
                    RemoteOperations::ScpSend((local_path, remote_path)) => {
                        let to_send = RemoteConnection::handle_scp_send_operation(&session, &local_path, &remote_path);
                        scps_tx.send(to_send).unwrap();
                    }
                    RemoteOperations::ScpFetch((remote_path, local_path)) => { 
                        let to_send = RemoteConnection::handle_scp_fetch_operation(&session, &remote_path, &local_path);
                        scpf_tx.send(to_send).unwrap();
                    }
                }
                trace!("Operation performed ===> Calling waker");
                waker.wake();
            }
        });
        return thread_handle;
    }
    
    // Handles the command execution operation. See the spawn_thread function
    fn handle_exec_operation(session: &Session, command: &str) -> Result<Output, Error> {
        debug!("Executing command '{}'", command);
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut ecode = 0 as i32;
        session.channel_session()
            .and_then(|mut channel| {
                channel.exec(&command)?;
                channel.wait_eof()?;
                channel.read_to_string(&mut stdout).map_err(|_| ssh2::Error::unknown())?;
                channel.stderr().read_to_string(&mut stderr).map_err(|_| ssh2::Error::unknown())?;
                ecode = channel.exit_status()?;
                channel.close()?;
                channel.wait_close()?;
                Ok(())
            })
            .map_or_else(
                |e| { 
                    error!("Error occured while executing command: {}", e);
                    Err(Error::ExecutionFailed) 
                },
                |_| {
                    Ok(Output {
                        status: ExitStatusExt::from_raw(ecode),
                        stdout: stdout.as_bytes().to_vec(),
                        stderr: stderr.as_bytes().to_vec()
                    })
                }
            )
    }

    // Handles the scp send operation. See the spawn_thread function.
    fn handle_scp_send_operation(session: &Session, local_path: &PathBuf, remote_path: &PathBuf) -> Result<(), Error>{
        debug!("Sending file {:?} to remote {:?}", local_path, remote_path); 
        let mut local_file = File::open(local_path)
            .map_err(|e| {
                error!("Error opening the local file to send: {}", e);
                return Error::NoSuchFile})?;
        let file_size = local_file.metadata().unwrap().len();
        session.scp_send(&remote_path, 0o755, file_size, None)
            .and_then(|mut channel| {
                let n_bytes = copy(&mut local_file, &mut channel)
                    .map_err(|e| {
                        error!("Failed to copy bytes to channel: {}", e);
                        ssh2::Error::unknown()})?;
                trace!("Number of bytes transfered {:?} / {:?}", n_bytes, file_size);
                channel.send_eof()?;
                channel.wait_eof()?;
                channel.close()?;
                channel.wait_close()?;
                if n_bytes == file_size {
                    Ok(())
                } else {
                    error!("The file size and the number of bytes sent does not match.");
                    Err(ssh2::Error::unknown())
                }})
            .map_or_else(
                |e| {error!("Error occured while sending file: {}", e); Err(Error::ScpSendFailed)},
                |_| {Ok(())}
            )
    }
    
    // Handles the scp fetch operation. See the spawn_thread function. 
    fn handle_scp_fetch_operation(session: &Session, remote_path: &PathBuf, local_path: &PathBuf) -> Result<(), Error>{
        debug!("Fetching remote file {:?} to {:?}", remote_path, local_path); 
        std::fs::remove_file(&local_path);
        let mut local_file = OpenOptions::new().write(true).create_new(true).open(local_path)
            .map_err(|e| {
                error!("Error opening the local file to fetch: {}", e);
                return Error::NoSuchFile})?;
        session.scp_recv(&remote_path)
            .and_then(|(mut channel, stats)| {
                let n_bytes = copy(&mut channel, &mut local_file)
                    .map_err(|e| {
                        error!("Failed to copy bytes from channel to file: {}", e);
                        ssh2::Error::unknown()})?;
                trace!("Number of bytes transfered {:?} / {:?}", n_bytes, stats.size());
                channel.send_eof()?;
                channel.wait_eof()?;
                channel.close()?;
                channel.wait_close()?;
                if n_bytes == stats.size(){
                    Ok(())
                } else {
                    error!("The remote file size and the number of bytes received does not match.");
                    Err(ssh2::Error::unknown())
                }
                })
            .map_or_else(
                |e| {error!("Error occured while fetching file: {}", e); Err(Error::ScpFetchFailed)},
                |_| {Ok(())}
            )
    } 
    
    /// Build, authenticate and starts an ssh session from an adress. 
    pub fn from_addr(addr: impl ToSocketAddrs,
                     host: &str,
                     user: &str) -> Result<RemoteConnection, Error> {
        debug!("Creating remote connection");
        trace!("Connection tcp stream");
        let stream = TcpStream::connect(addr).map_err(|_| Error::ConnectionFailed)?;
        let session = RemoteConnection::start_session(&stream, host, user)?;
        let (op_tx, op_rx) = mpsc::channel::<(RemoteOperations, Waker)>();
        let (scpf_tx, scpf_rx) = mpsc::channel();
        let (scps_tx, scps_rx) = mpsc::channel();
        let (exc_tx, exc_rx) = mpsc::channel();
        let thread_handle = RemoteConnection::spawn_thread(session, stream, op_rx, exc_tx, scps_tx, scpf_tx);
        return Ok(RemoteConnection { proxy_command: None, thread_handle: Some(thread_handle), op_tx: Some(op_tx), scps_rx, scpf_rx, exc_rx });
    }
    
    /// Build, authenticate and starts an ssh session from a proxycommand. 
    pub fn from_proxy_command(command: &str, host: &str, user: &str)
                              -> Result<RemoteConnection, Error> {
        let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
        let mut remote = RemoteConnection::from_addr(addr, host, user)?;
        remote.proxy_command.replace(proxy_command);
        return Ok(remote);
    }

    /// Creates a RemoteExecFuture that will resolve to a Result giving the session back as well as
    /// the output of the command. The future takes ownership of the session to avoid sharing the 
    /// inner ssh2 session object to different threads at the same time. 
    pub fn async_exec(self, command: &str) -> RemoteExecFuture {
        return RemoteExecFuture {
            state: RemoteExecFutureState::Starting(command.to_string()),
            connection: Some(self),
        };
    }
    
    /// Creates a scpsendfuture that will resolve to a result giving the session back. the future 
    /// takes ownership of the session to avoid sharing the inner ssh2 session object to different 
    /// threads at the same time. 
    pub fn async_scp_send(self, local_path: &PathBuf, remote_path: &PathBuf) -> ScpSendFuture {
        return ScpSendFuture {
            state: ScpSendFutureState::Starting((local_path.to_owned(), remote_path.to_owned())),
            connection: Some(self),
        };
    }

    /// Creates a scpsendfuture that will resolve to a result giving the session back. the future 
    /// takes ownership of the session to avoid sharing the inner ssh2 session object to different 
    /// threads at the same time. 
    pub fn async_scp_fetch(self, remote_path: &PathBuf, local_path: &PathBuf) -> ScpFetchFuture {
        return ScpFetchFuture {
            state: ScpFetchFutureState::Starting((remote_path.to_owned(), local_path.to_owned())),
            connection: Some(self),
        };
    }
}

// We take care of closing the enclosed thread before dropping the connection. 
impl Drop for RemoteConnection {
    fn drop(&mut self) {
        let op_tx = self.op_tx.take().unwrap();
        drop(op_tx);
        let thread_handle = self.thread_handle.take().unwrap();
        thread_handle.join().unwrap();
        let pxc = self.proxy_command.take();
        drop(pxc);
    }
}


#[cfg(test)]
mod test {
    use super::*;
    use pretty_logger;
    use log;

    fn setup() {
        pretty_logger::init(pretty_logger::Destination::Stdout,
                            log::LogLevelFilter::Trace,
                            pretty_logger::Theme::default());
    }

    #[test]
    fn test_proxy_command_forwarder() {
        let (pxc, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut buf = [0 as u8; 5];
        stream.read(&mut buf);
        assert_eq!(buf, "kikou".as_bytes());
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn test_remote_from_addr() {
        let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
    }

    #[test]
    fn test_remote_from_proxy_command() {
        let remote = RemoteConnection::from_addr( "127.0.0.1:22", "localhost", "apere" ).unwrap();
    }

    #[test]
    fn test_async_exec() {
        setup();
        use futures::executor::block_on;
        async fn connect_and_ls() {
            let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let (remote, output) = await!(remote.async_exec("sleep 1 & ls")).unwrap();
            println!("Executed and resulted in {:?}", String::from_utf8(output.stdout).unwrap());
        }
        block_on(connect_and_ls());
    }

    #[test]
    fn test_async_scp_send() {
        setup();
        use futures::executor::block_on;
        async fn connect_and_scp_send() {
            let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/local.txt", "bs=1M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let connection = await!(remote.async_scp_send( &local_f, &remote_f)).unwrap();
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
        block_on(connect_and_scp_send());
    }

    #[test]
    fn test_async_scp_fetch(){
        setup();
        use futures::executor::block_on;
        async fn connect_and_scp_fetch() {
            let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/remote.txt", "bs=1M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let connection = await!(remote.async_scp_fetch( &remote_f, &local_f)).unwrap();
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
        block_on(connect_and_scp_fetch());

        
    }
}

