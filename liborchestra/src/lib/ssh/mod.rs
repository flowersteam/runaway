// liborchestra/mod.rs
// Author: Alexandre Péré
/// This module contains structures that wraps the ssh2 session object to provide ways to handle 
/// ssh configurations, authentications, and proxy-commands. In particular, since libssh2
/// does not provide ways to handle proxy-commands, we use a threaded copy loop that open a
/// proxy-command as a subprocess and copy the output on a random tcp socket.
///
/// After instantiation, the session object is used through new style futures. For this reason,
/// after authentication the session will be placed in a thread that will take care about handling
/// the operations that may be required by the user. Most importantly, particular care is taken to
/// wake the futures when the operations are over. This means that the session object is a pure
/// resource, and does not need any other care. The operations are made in a totally asynchronous
/// fashion: if a command blocks, it will
///
///
/// Note: Though ssh operations are handled asynchronously, the connection and the handshake are 
/// made in a synchronous and blocking manner.

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
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
    cell::RefCell,
    thread,
    rc::Rc,
    io,
    fmt::{Debug, Formatter},
    pin::Pin,
    task::{Poll, Waker},
    io::{prelude::*, BufReader, BufWriter,copy, ErrorKind},
    process::{Stdio, Command, Output},
    os::unix::process::ExitStatusExt,
    time::{Instant, Duration},
    error,
    fmt,
    path::PathBuf,
    collections::{HashSet, HashMap},
    fs::{File, OpenOptions},
};
use dirs;
use crate::KNOWN_HOSTS_RPATH;
use uuid::Uuid;
use futures::future::Future;
use chashmap::CHashMap;

///////////////////////////////////////////////////////////////////////////////////////////// MODULE
pub mod config;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
#[derive(Debug, Clone)]
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
    ExecutionFailed(String),
    PollExecutionFailed(String),
    ScpSendFailed(String),
    ScpFetchFailed(String),
    Future(String),
    WouldBlock,
    Unknown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::ProxySocketNotFound => write!(f, "Failed to find a socket to bind the proxy command on."),
            Error::ProxyCommandReturned => write!(f, "The proxy command returned. Check command and connection"),
            Error::NoSuchFile => write!(f, "The given file could not be found"),
            Error::ConnectionFailed => write!(f, "Failed to connect to the provided host."),
            Error::PreferenceSettingFailed => write!(f, "Failed to set ssh preferences for handshake"),
            Error::HandshakeFailed => write!(f, "Failed to handshake with remote host."),
            Error::HomeNotFound => write!(f, "Home folder could not be found through the $HOME variable"),
            Error::KnownHostsUnreachable => write!(f, "Failed to read runaway known_hosts file."),
            Error::CouldntRegisterHost => write!(f, "Failed to register host"),
            Error::CouldntSaveKnownHosts => write!(f, "Failed to save the runaway known_hosts file"),
            Error::Unknown => write!(f, "Unknown error occurred"),
            Error::HostKeyMismatch => write!(f, "Host was found but the key does not match!"),
            Error::HostNotKnown => write!(f, "Host was not found in the known hosts."),
            Error::HostCheckFailed => write!(f, "Host key check failed somehow."),
            Error::SshAgentUnavailable => write!(f, "Ssh Agent could not be found. Is there an ssh agent on $SSH_AUTH_SOCK ?"),
            Error::ClientAuthFailed => write!(f, "Client authentication failed. Is the necessary identity available in the ssh agent ?"),
            Error::ReceiverCalledOnEmptyChannel => write!(f, "A receiver was called on an empty channel"),
            Error::ChannelClosed => write!(f, "A channel was used but was closed"),
            Error::ExecutionFailed(s) => write!(f, "The execution of a command failed: {}", s),
            Error::PollExecutionFailed(s) => write!(f, "The polling of a command failed: {}", s),
            Error::ScpSendFailed(s) => write!(f, "The sending of the file failed: {}", s),
            Error::ScpFetchFailed(s) => write!(f, "The fetching of the file failed: {}", s),
            Error::Future(s) => write!(f, "An error occured when polling the future: {}", s),
            Error::WouldBlock => write!(f, "Blocking operation"),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////////// PROXY-COMMAND
/// This structure starts a proxy command which is forwarded on a tcp socket. This allows
/// a libssh2 session to get connected through a proxy command. On the first connection to the
/// socket, the proxycommand will be started in a subprocess, and the reads from the socket will be
/// copied to the process stdin, and the stdout from the process will be written to the socket. The 
/// forthcoming connections are rejected. The messages get forwarded as long as the forwarder stays 
/// in scope. The forwarding is explicitly stopped when the forwarder is dropped. 
pub struct ProxyCommandForwarder {
    keep_alive: Arc<AtomicBool>,
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
                let mut buf = [0; 2048];
                while kf1.load(Ordering::Relaxed) {
                    if std::io::copy(&mut command_stdout, &mut socket1).is_err(){
                        break
                    } else{
                        socket1.flush().unwrap()
                    }
                }
                socket1.shutdown(Shutdown::Write);
                trace!("Exiting command -> socket thread");
            });
            let h2 = std::thread::spawn(move || {
                let mut buf = [0; 2048];
                while kf2.load(Ordering::Relaxed) {
                    if std::io::copy(&mut socket2, &mut command_stdin).is_err(){
                        break
                    } else {
                        command_stdin.flush().unwrap();
                    }
                }
                socket2.shutdown(Shutdown::Read);
                trace!("Exiting socket -> command thread");
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
            keep_alive: keep_forwarding,
            handle: Some(handle),
        },
                   address));
    }
}

// Care must be taken to join the threads at drop, to appropriately close the connection and the
// process.
impl Drop for ProxyCommandForwarder { 
    fn drop(&mut self) {
        debug!("Dropping proxy command");
        trace!("Stop forwarding");
        self.keep_alive.store(false, Ordering::Relaxed);
        trace!("Waiting for thread handles");
        let handle = self.handle.take().unwrap();
        let (h1, h2, h3) = handle.join().unwrap();
        h1.join();
        h2.join();
        h3.join();
        trace!("Proxy command fully dropped");
    }
}


////////////////////////////////////////////////////////////////////////////////// REMOTE OPERATIONS
// Remote operations are messages sent and retrieve from the future to the connection thread. The
// remote connection will iterate over the `Progressing(progress_data)` by adding the data gathered
// from the operation in `progress_data`.
// See the different `handle_(some operation name)_operation)` functions of `RemoteConnection` for
// more information.

// This enumeration represents the different operations that can be done by the RemoteConnection.
#[doc(hidden)]
#[derive(Debug)]
enum RemoteOperation{
    Exec(ExecOp),
    ScpSend(ScpSendOp),
    ScpFetch(ScpFetchOp),
}

// This enumeration represents the different states an operation can lie in.
#[derive(Debug, Clone)]
enum OperationState<I, P, O, E>{
    Starting(I),
    Progressing(P),
    Succeeded(O),
    Failed(E),
    Transitioning,
}

impl<I, P, O, E> OperationState<I, P, O, E>{
    // Starts a new operation from a given input.
    fn from_input(input: I) -> OperationState<I, P, O, E>{
        return OperationState::Starting(input);
    }
    // Transitions from `Starting` to `Transitioning` state by taking the input values.
    fn begin(&mut self) -> I{
        match std::mem::replace(self, OperationState::Transitioning){
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

// This macro allows to create an operation as a structure wrapping an inner OperationState.
#[macro_export]
macro_rules! make_operation {
    ( $name:ident, $I:ty, $P:ty, $O:ty, $E:ty ) => {
        #[derive(Debug, Clone)]
        struct $name{
            state: OperationState<$I, $P, $O, $E >
        }
        impl $name {
            fn from_input(input: $I) -> $name { return $name{state:OperationState::from_input(input)}; }
            fn begin(&mut self) -> $I { return self.state.begin() }
            fn save(mut self, progress: $P) -> $name { self.state.save(progress); return self }
            fn resume(&mut self) -> $P { return self.state.resume() }
            fn succeed(mut self, output: $O) -> $name { self.state.succeed(output); return self }
            fn fail(mut self, error: $E) -> $name { self.state.fail(error); return self}
            fn into_result(mut self) -> Result<$O, $E> { return self.state.into_result() }
            fn is_over(&self) -> bool { return self.state.is_over() }
            fn is_progressing(&self) -> bool { return self.state.is_progressing() }
            fn is_new(&self) -> bool { return self.state.is_new() }
        }
    }
}

// A newtype representing remaining bytes to be sent or retrieved.
#[derive(Debug, Clone)]
struct RemainingBytes(u64);

// As channel are not `Send`, we can't requeue them for further advance in the operation channel.
// For this reason we store them in a thread-local hash-map under a given uuid. This newtype
// represents this uuid.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
struct ChannelId(Uuid);

make_operation!(ExecOp, String, (ChannelId, Output), Output, Error);
make_operation!(ScpSendOp, (PathBuf, PathBuf), (ChannelId, RemainingBytes, Arc<BufReader<File>>), (), Error);
make_operation!(ScpFetchOp, (PathBuf, PathBuf), (ChannelId, RemainingBytes, Arc<BufWriter<File>>), (), Error);

////////////////////////////////////////////////////////////////////////////////// REMOTE CONNECTION
/// This structure allows to perform operations on a remote host. Operations are executed in a
/// separated thread, which allows to avoid moving and synchronizing the ssh2 session between
/// different tasks. As such, operations are represented by futures, which are driven to completion
/// by the thread.
// Under the hood, the future sends an operation to the connection thread which takes care about
// running the operation to completion, and wakes the future when the result is available.
#[derive(Clone)]
pub struct RemoteConnection{
    keep_alive: Arc<AtomicBool>,
    proxy_command: Arc<Mutex<Option<ProxyCommandForwarder>>>,
    thread_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    operations_results: Arc<CHashMap<Uuid, RemoteOperation>>,
    operations_sender: mpsc::Sender<(RemoteOperation, Waker, Uuid)>,
}

impl RemoteConnection{
    /// Build, authenticate and starts an ssh session from an adress.
    pub fn from_addr(addr: impl ToSocketAddrs,
                     host: &str,
                     user: &str) -> Result<RemoteConnection, Error> {
        debug!("Creating remote connection");
        trace!("Connection tcp stream");
        let stream = TcpStream::connect(addr).map_err(|_| Error::ConnectionFailed)?;
        let session = RemoteConnection::start_session(&stream, host, user)?;
        let (op_tx, op_rx) = mpsc::channel::<(RemoteOperation, Waker, Uuid)>();
        let operations_results = Arc::new(CHashMap::new());
        let keep_alive = Arc::new(AtomicBool::new(true));
        let thread_handle = RemoteConnection::spawn_thread(session,
                                                           stream,
                                                           op_rx,
                                                           op_tx.clone(),
                                                           operations_results.clone(),
                                                            keep_alive.clone());
        return Ok(RemoteConnection{
            keep_alive,
            proxy_command: Arc::new(Mutex::new(None)),
            thread_handle: Arc::new(Mutex::new(Some(thread_handle))),
            operations_sender: op_tx,
            operations_results});
    }
    /// Build, authenticate and starts an ssh session from a proxycommand.
    pub fn from_proxy_command(command: &str, host: &str, user: &str)
                              -> Result<RemoteConnection, Error> {
        let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
        let mut remote = RemoteConnection::from_addr(addr, host, user)?;
        remote.proxy_command.lock().unwrap().replace(proxy_command);
        return Ok(remote);
    }
    /// Creates a RemoteExecFuture that will resolve to a Result giving the session back as well as
    /// the output of the command.
    pub fn async_exec(&self, command: &str) -> ExecFuture {
        let ope = ExecOp::from_input(command.to_owned());
        return ExecFuture::new(ope, self.clone());
    }

    /// Creates a scpsendfuture that will resolve to a result giving the session back.
    pub fn async_scp_send(&self, local_path: &PathBuf, remote_path: &PathBuf) -> ScpSendFuture {
        let ope = ScpSendOp::from_input((local_path.to_owned(), remote_path.to_owned()));
        return ScpSendFuture::new(ope, self.clone());
    }


    /// Creates a scpsendfuture that will resolve to a result giving the session back.
    pub fn async_scp_fetch(&self, remote_path: &PathBuf, local_path: &PathBuf) -> ScpFetchFuture {
        let ope = ScpFetchOp::from_input((remote_path.to_owned(), local_path.to_owned()));
        return ScpFetchFuture::new(ope, self.clone());
    }

    /// Returns the number of handles over connection
    pub fn strong_count(&self) -> usize{
        return Arc::strong_count(&self.thread_handle);
    }

    // Authenticate the ssh session. 
    fn start_session(stream: &TcpStream, host: &str, user: &str) -> Result<Session, Error> {
        debug!("Opening remote connection to host {} as user {}", host, user); let mut session = Session::new().unwrap();
        session.method_pref(MethodType::HostKey, "ssh-rsa")
            .map_err(|_| Error::PreferenceSettingFailed)?;
        trace!("Performing handshake");
        session.handshake(stream).map_err(|_| Error::HandshakeFailed)?;
        {
            trace!("Checking host key");
            let mut known_hosts = session.known_hosts().unwrap();
            let mut known_hosts_path = dirs::home_dir().ok_or(Error::HomeNotFound)?;
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
            trace!("Authenticating ourselves");
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

    // Spawn the operation handling thread. This thread dispatches operations to handle_* functions.
    fn spawn_thread(session: Session,
                    stream: TcpStream,
                    operations_receiver: mpsc::Receiver<(RemoteOperation, Waker, Uuid)>,
                    operations_sender: mpsc::Sender<(RemoteOperation, Waker, Uuid)>,
                    operations_results: Arc<CHashMap<Uuid, RemoteOperation>>,
                    keep_alive: Arc<AtomicBool>
                    ) -> thread::JoinHandle<()> {
        debug!("Start connection thread");
        let thread_handle = thread::spawn(move || {
            trace!("In thread; starting operation loop");
            let stream = stream; // move the stream
            let mut session = session;
            let mut channels_map = Rc::new(RefCell::new(HashMap::new()));
            while keep_alive.load(Ordering::Relaxed){
               match operations_receiver.try_recv() {
                   Ok((operation, waker, uuid)) => {
                       trace!("Received operation to perform: {:?}", operation);
                       match operation {
                           RemoteOperation::Exec(ope) => {
                               match ope.state {
                                   OperationState::Starting(_) => {
                                       let ope = RemoteConnection::handle_starting_exec_operation(&session,
                                                             ope,
                                                             channels_map.clone());
                                       operations_sender.send((RemoteOperation::Exec(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Progressing(_) => {
                                       let ope = RemoteConnection::handle_progressing_exec_operation(&session,
                                                             ope,
                                                             channels_map.clone());
                                       operations_sender.send((RemoteOperation::Exec(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Succeeded(_) | OperationState::Failed(_) =>  {
                                       operations_results.insert_new(uuid,
                                                                     RemoteOperation::Exec(ope));
                                       waker.wake();
                                   }
                                   OperationState::Transitioning => {
                                       panic!("Operation found in transitioning state.")
                                   }
                               }
                           }
                           RemoteOperation::ScpSend(ope) => {
                               match ope.state {
                                   OperationState::Starting(_) => {
                                       let ope = RemoteConnection::
                                       handle_starting_scp_send_operation(&session,
                                                                          ope,
                                                                          channels_map.clone());
                                       operations_sender.send((RemoteOperation::ScpSend(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Progressing(_) => {
                                       let ope = RemoteConnection::
                                       handle_progressing_scp_send_operation(&session,
                                                                          ope,
                                                                          channels_map.clone());
                                       operations_sender.send((RemoteOperation::ScpSend(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Succeeded(_) | OperationState::Failed(_) => {
                                       operations_results.insert_new(uuid,
                                                                     RemoteOperation::ScpSend(ope));
                                       waker.wake();
                                   }
                                   OperationState::Transitioning => {
                                       panic!("Operation found in transitioning state.")
                                   }
                               }
                           }
                           RemoteOperation::ScpFetch(ope) => {
                               match ope.state {
                                   OperationState::Starting(_) => {
                                       let ope = RemoteConnection::
                                       handle_starting_scp_fetch_operation(&session,
                                                                          ope,
                                                                          channels_map.clone());
                                       operations_sender.send((RemoteOperation::ScpFetch(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Progressing(_) => {
                                       let ope = RemoteConnection::
                                       handle_progressing_scp_fetch_operation(&session,
                                                                          ope,
                                                                          channels_map.clone());
                                       operations_sender.send((RemoteOperation::ScpFetch(ope), waker, uuid)).unwrap();
                                   }
                                   OperationState::Succeeded(_) | OperationState::Failed(_) => {
                                       operations_results.insert_new(uuid,
                                                                     RemoteOperation::ScpFetch(ope));
                                       waker.wake();
                                   }
                                   OperationState::Transitioning => {
                                       panic!("Operation found in transitioning state.")
                                   }
                               }
                           }
                       }
                   }
                   _ => continue
               }
            }
        });
        trace!("Returning thread handle");
        return thread_handle;
    }

}

impl<'a> RemoteConnection{

    fn handle_starting_exec_operation(session: &'a Session,
                             ope: ExecOp,
                             chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ExecOp {
        let mut ope = ope;
        let cmd = ope.begin();
        let mut channel = match session.channel_session() {
            Ok(c) => c,
            Err(e) => return ope.fail(Error::ExecutionFailed(format!("Failed to open channel")))
        };
        if channel.exec(&cmd).is_err() {
            return return ope.fail(Error::ExecutionFailed(format!("Failed to execute command")))
        };
        let output = Output {
            status: ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let chan_id = ChannelId(Uuid::new_v4());
        chan_map.borrow_mut().insert(chan_id.clone(), channel);
        return ope.save((chan_id, output));
    }

    fn handle_progressing_exec_operation(session: &'a Session,
                             ope: ExecOp,
                             chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ExecOp{
        let mut ope = ope;
        let (chan_id, mut output) = ope.resume();
        let mut chan = chan_map.borrow_mut().remove(&chan_id).unwrap();
        session.set_blocking(false);
        let eof = chan.wait_eof(); // We read eof beforehand, to avoid leaving bytes.
        let copied = copy(&mut chan.stream(0), &mut output.stdout)
            .and(copy(&mut chan.stream(1), &mut output.stderr));
        match copied {
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                session.set_blocking(true);
                chan_map.borrow_mut().insert(chan_id.clone(), chan);
                return ope.save((chan_id, output));
            }
            Err(e) => {
                session.set_blocking(true);
                return ope.fail(Error::ExecutionFailed(format!("Failed to read stdout: {}", e)))
            }
            Ok(c) => {}
        }
        if let Err(e) = eof {
            if e.code() as i64 == -37 {
                session.set_blocking(true);
                chan_map.borrow_mut().insert(chan_id.clone(), chan);
                return ope.save((chan_id, output));
            } else {
                session.set_blocking(true);
                return ope.fail(Error::ExecutionFailed(format!("Failed to acquire eof")));
            }
        }
        let eof = eof.unwrap();
        session.set_blocking(true);
        let ecode = match chan.exit_status() {
            Ok(c) => c,
            Err(e) => return ope.fail(Error::ExecutionFailed(format!("Failed to get exit code")))
        };
        output.status = ExitStatusExt::from_raw(ecode);
        if let Err(e) = chan.close() {
            return ope.fail(Error::ExecutionFailed(format!("Failed to close channel")))
        }
        if let Err(_) = chan.wait_close() {
            return ope.fail(Error::ExecutionFailed(format!("Failed to wait to close channel")));
        }
        return ope.succeed(output);
    }

    fn handle_starting_scp_send_operation(session: &'a Session,
                                          ope: ScpSendOp,
                                          chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ScpSendOp {
        let mut ope = ope;
        let (local_path, remote_path) = ope.begin();
        let mut local_file = match File::open(local_path) {
            Ok(f) => f,
            Err(_) => return ope.fail(Error::ScpSendFailed(format!("Failed to open local file"))),
        };
        let bytes = local_file.metadata().unwrap().len();
        let mut chan = match session.scp_send(&remote_path,0o755,bytes,None) {
            Ok(chan) => chan,
            Err(_) => return ope.fail(Error::ScpSendFailed(format!("Failed to open scp send channel"))),
        };
        let chan_id = ChannelId(Uuid::new_v4());
        chan_map.borrow_mut().insert(chan_id.clone(), chan);
        return ope.save((chan_id,RemainingBytes(bytes),Arc::new(BufReader::new(local_file))));
    }

    fn handle_progressing_scp_send_operation(session: &'a Session,
                                             ope: ScpSendOp,
                                             chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ScpSendOp {
        let mut ope = ope;
        let (chan_id, r_bytes, mut local_file) = ope.resume();
        let mut chan =chan_map.borrow_mut().remove(&chan_id).unwrap();
        let mut file = Arc::get_mut(&mut local_file).unwrap();
        session.set_blocking(false);
        match bufread_bufcopy(&mut file, &mut chan.stream(0)){
            (b, Err(ref e)) if e.kind() == ErrorKind::WouldBlock => {
                session.set_blocking(true);
                chan_map.borrow_mut().insert(chan_id.clone(), chan);
                return ope.save((chan_id,RemainingBytes(r_bytes.0 - b as u64),local_file));
            }
            (b, Ok(())) =>{
                if r_bytes.0 != 0 {
                    session.set_blocking(true);
                    chan_map.borrow_mut().insert(chan_id.clone(), chan);
                    return ope.save((chan_id,RemainingBytes(r_bytes.0 - b as u64),local_file));
                } else {
                    session.set_blocking(true);
                    if chan.send_eof().is_err(){ return ope.fail(Error::ScpSendFailed(format!("Failed to send eof")))};
                    if chan.wait_eof().is_err(){ return ope.fail(Error::ScpSendFailed(format!("Failed to wait eof")))};
                    if chan.close().is_err() { return ope.fail(Error::ScpSendFailed(format!("Failed to send channel close")))};
                    if chan.wait_close().is_err(){return ope.fail(Error::ScpSendFailed(format!("Failed to wait channel close")))};
                    return ope.succeed(())
                }
            }
            (b, Err(e)) => {
                session.set_blocking(true);
                return ope.fail(Error::ScpSendFailed(format!("Failed to copy")));
            }
        }
    }

    fn handle_starting_scp_fetch_operation(session: &'a Session,
                                  ope: ScpFetchOp,
                                  chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ScpFetchOp {
        let mut ope = ope;
        let (remote_path, local_path) = ope.begin();
        std::fs::remove_file(&local_path);
        let mut local_file = match OpenOptions::new().write(true).create_new(true).open(local_path) {
            Ok(f) => f,
            Err(_) => return ope.fail(Error::ScpFetchFailed(format!("Failed to open local file")))
        };
        let (mut chan, stats) = match session.scp_recv(&remote_path) {
            Ok(c) => c,
            Err(_) => return ope.fail(Error::ScpFetchFailed(format!("Failed to start scp recv")))
        };
        let chan_id = ChannelId(Uuid::new_v4());
        chan_map.borrow_mut().insert(chan_id.clone(), chan);
        return ope.save((chan_id, RemainingBytes(stats.size()), Arc::new(BufWriter::new(local_file))))
    }

    fn handle_progressing_scp_fetch_operation(session: &'a Session,
                                              ope: ScpFetchOp,
                                              chan_map: Rc<RefCell<HashMap<ChannelId, ssh2::Channel<'a>>>>)
        -> ScpFetchOp{
        let mut ope = ope;
        let (chan_id, r_bytes, mut local_file) = ope.resume();
        let mut chan = chan_map.borrow_mut().remove(&chan_id).unwrap();
        let mut file = Arc::get_mut(&mut local_file).unwrap();
        session.set_blocking(false);
        trace!("Copying data from stream to file");
        match read_bufcopy(&mut chan.stream(0), &mut file) {
            (b, Err(ref e)) if e.kind() == ErrorKind::WouldBlock => {
                session.set_blocking(true);
                chan_map.borrow_mut().insert(chan_id.clone(), chan);
                return ope.save((chan_id, RemainingBytes(r_bytes.0 - b as u64), local_file));
            }
            (b, Ok(())) =>{
                if r_bytes.0 != 0{
                   session.set_blocking(true);
                    chan_map.borrow_mut().insert(chan_id.clone(), chan);
                    return ope.save((chan_id, RemainingBytes(r_bytes.0 - b as u64), local_file))
                } else {
                    session.set_blocking(true);
                    if chan.send_eof().is_err() { return ope.fail(Error::ScpFetchFailed(format!("Failed to send eof"))) };
                    if chan.wait_eof().is_err() { return ope.fail(Error::ScpFetchFailed(format!("Failed to wait eof"))) };
                    if chan.close().is_err() { return ope.fail(Error::ScpFetchFailed(format!("Failed to send channel close"))) };
                    if chan.wait_close().is_err() { return ope.fail(Error::ScpFetchFailed(format!("Failed to wait channel close"))) };
                    return ope.succeed(())
                }
            }
            (b, Err(e)) => {
                session.set_blocking(true);
                return ope.fail(Error::ScpFetchFailed(format!("Failed to copy data")));
            }
        }
    }
}

// We take care of closing the inner thread before dropping the connection.
impl Drop for RemoteConnection{
    fn drop(&mut self) {
        if Arc::strong_count(&self.thread_handle) == 1 {
            debug!("Dropping remote connection");
            self.keep_alive.store(false, Ordering::Relaxed);
            self.thread_handle.lock().unwrap().take().unwrap().join().unwrap();
            let pxc = self.proxy_command
                .lock()
                .unwrap()
                .take();
            drop(pxc);
        }
    }
}

// This function copies a bufread reader into a writer by taking care about always returning the
// number of bytes read even in case of an error (for WouldBlock error)
fn bufread_bufcopy<R: BufRead, W:Write>(reader: &mut R, writer: &mut W) -> (u64, Result<(), io::Error>){
    let mut bytes = 0 as u64;
    loop{
        match reader.fill_buf(){
            Ok(buf) => {
                let l = std::cmp::min(1024000, buf.len());
                match writer.write(&buf[..l]){
                    Ok(0) => {
                        writer.flush();
                        return (bytes, Ok(()))
                    }
                    Ok(b) => {
                        bytes += b as u64;
                        reader.consume(b);
                    }
                    Err(e) => {
                        writer.flush();
                        return (bytes, Err(e));
                    }
                }
            },
            Err(e) => {
                writer.flush();
                return (bytes, Err(e))
            }
        }
    }
    return (bytes, Ok(()))
}

//This function does the same for an unbuffered read.
fn read_bufcopy<R:Read, W:Write>(reader: &mut R, writer: &mut W) -> (u64, Result<(), io::Error>) {
    let mut bytes = 0 as u64;
    let mut buf = [0; 8192];
    loop{
        match reader.read(&mut buf){
            Ok(0) => {
                return (bytes, Ok(()))
            }
            Ok(r) => {
                match writer.write(&buf[..r]){
                    Ok(w) if w == r => {
                        bytes += w as u64;
                    }
                    Ok(w) => {
                        bytes += w as u64;
                        writer.flush();
                        return (bytes, Err(io::Error::new(ErrorKind::Other, Error::Unknown)))
                    }
                    Err(e) => {
                        writer.flush();
                        return (bytes, Err(e))
                    }
                }
            }
            Err(e) => {
                writer.flush();
                return (bytes, Err(e))
            }
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////// FUTURES
// Futures returned by the different methods proposed by the RemoteConnection object.

enum FutureState<T>{
    Starting(T),
    Waiting,
    Finished,
}

#[macro_export]
macro_rules! make_operation_future {
    ( $name:ident, $operation:ty, $variant:ident, $output:ty) => {
        pub struct $name {
            state: FutureState<$operation>,
            connection: RemoteConnection,
            uuid: Uuid,
        }
        impl $name{
            fn new(ope:$operation, connection:RemoteConnection) -> $name{
                return $name{
                    state: FutureState::Starting(ope),
                    connection: connection,
                    uuid: Uuid::new_v4(),
                }
            }
        }
        impl Future for $name {
            type Output = $output;
            fn poll(mut self: Pin<&mut Self>, wake: &Waker) -> Poll<$output> {
                loop {
                    match &self.state {
                        FutureState::Starting(ope) => {
                            let (state, ret) = self.connection
                                .operations_sender
                                .send((RemoteOperation::$variant(ope.clone()), wake.clone(), self.uuid.clone()))
                                .map_or_else(
                                    |_| {(FutureState::Finished,
                                          Poll::Ready(Err(Error::ChannelClosed)))},
                                    |_| {(FutureState::Waiting, Poll::Pending)});
                            self.state = state;
                            return ret;
                        }
                        FutureState::Waiting => {
                            self.state = FutureState::Finished;
                            let ope = match self.connection.operations_results.remove(&self.uuid) {
                                Some(ope) => ope,
                                None => {
                                    return Poll::Ready(Err(Error::Future("Future polled, but \
                                    result is not there.".to_owned())))
                                }
                            };
                            if let RemoteOperation::$variant(ope) = ope{
                                return Poll::Ready(ope.into_result())
                            } else {
                                panic!("Wrong operation retrieved.")
                            }

                        }
                        FutureState::Finished => {
                            panic!("Future was polled in a finished state");
                        }
                    }
                }
            }
        }
    }
}

/// A future returned by the method `async_exec`. Resolves in a `Result<Output,Error>`.
make_operation_future!(ExecFuture, ExecOp, Exec, Result<Output, Error>);
/// A future returned by the method `async_scp_send`. Resolves in a `Result<(),Error>`.
make_operation_future!(ScpSendFuture, ScpSendOp, ScpSend, Result<(), Error>);
/// A future returned by the method `async_scp_fetch`. Resolves in a `Result<(),Error>`.
make_operation_future!(ScpFetchFuture, ScpFetchOp, ScpFetch, Result<(), Error>);

#[cfg(test)]
mod test {
    use super::*;

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
        let remote = RemoteConnection::from_proxy_command( "ssh -A -l apere localhost -W localhost:22", "localhost", "apere" ).unwrap();
    }

    #[test]
    fn test_async_exec() {
        use futures::executor::block_on;
        async fn connect_and_ls() {
            //let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let remote = RemoteConnection::from_proxy_command( "ssh -A -l apere localhost -W localhost:22", "localhost", "apere" ).unwrap();
            let output = await!(remote.async_exec("echo kikou && sleep 1 && echo hello!")).unwrap();
            println!("Executed and resulted in {:?}", String::from_utf8(output.stdout).unwrap());
        }
        block_on(connect_and_ls());
    }

    #[test]
    fn test_async_scp_send() {
        use futures::executor::block_on;
        async fn connect_and_scp_send() {
            //let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let remote = RemoteConnection::from_proxy_command( "ssh -A -l apere localhost -W localhost:22", "localhost", "apere" ).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/local.txt", "bs=35M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            let connection = await!(remote.async_scp_send( &local_f, &remote_f)).unwrap();
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
        block_on(connect_and_scp_send());
    }

    #[test]
    fn test_async_scp_fetch(){
        use futures::executor::block_on;
        async fn connect_and_scp_fetch() {
            //let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
            let remote = RemoteConnection::from_proxy_command( "ssh -A -l apere localhost -W localhost:22", "localhost", "apere" ).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/remote.txt", "bs=33M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            let connection = await!(remote.async_scp_fetch( &remote_f, &local_f)).unwrap();
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
        block_on(connect_and_scp_fetch());        
    }

    #[test]
    fn test_async_concurrent_exec(){
        let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
        async fn connect_and_ls(remote: RemoteConnection) -> std::process::Output{
            return await!(remote.async_exec("echo 1 && sleep 1 && echo 2")).unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for i in (1..10){
            handles.push(executor.spawn_with_handle(connect_and_ls(remote.clone())).unwrap())
        }
        let bef = std::time::Instant::now();
        for handle in handles{
            assert_eq!(String::from_utf8(executor.run(handle).stdout).unwrap(), "1\n2\n".to_owned());
        }
        let dur = std::time::Instant::now().duration_since(bef);
        println!("Duration: {:?}", dur);
    }
}

