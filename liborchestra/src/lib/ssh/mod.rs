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
/// fashion: If a command blocks, it will be parked, until further data is available.
///
/// Note: Though ssh operations are handled asynchronously, the connection and the handshake are 
/// made in a synchronous and blocking manner.

//////////////////////////////////////////////////////////////////////////////////////////// IMPORTS
use ssh2::{
    Session,
    Channel,
    KnownHostFileKind,
    CheckResult,
    MethodType,
    KnownHostKeyFormat};
use std::{
    net::{TcpStream, SocketAddr, ToSocketAddrs, Shutdown},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    io,
    pin::Pin,
    task::{Poll, Waker, Context},
    io::{prelude::*, BufReader, copy, ErrorKind},
    process::{Stdio, Command, Output},
    os::unix::process::ExitStatusExt,
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
use crate::derive_from_error;
use crate::primitives::{
    Dropper, Finished, Operation, OperationFuture, Progressing,
    Starting, UseResource,
};
use crossbeam::channel::{unbounded, Receiver, Sender, TryRecvError};
use crate::stateful::Stateful;
use std::intrinsics::transmute;

///////////////////////////////////////////////////////////////////////////////////////////// MODULE
pub mod config;

///////////////////////////////////////////////////////////////////////////////////////////// ERRORS
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
            Error::Config(e) =>
                write!(f, "An ssh config-related error occurred: {}", e),
        }
    }
}

derive_from_error!(Error, config::Error, Config);

impl From<Error> for crate::primitives::Error{
    fn from(other: Error) -> crate::primitives::Error{
        return crate::primitives::Error::Operation(format!("{}", other));
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
    repr: String,
}

impl ProxyCommandForwarder {
    /// Creates a new `ProxyCommandForwarder` from a command. An available port is automatically
    /// given by the OS, and is returned along with the forwarder. 
    pub fn from_command(command: &str) -> Result<(ProxyCommandForwarder, SocketAddr), Error> {
        debug!("Starting proxy command: {}", command);
        trace!("Starting proxy command tcp listener");
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
        trace!("Spawning proxy command");
        let mut command = Command::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        trace!("Spawning proxy command forwarding thread");
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
                socket1.shutdown(Shutdown::Write);
                trace!("Exiting command -> socket thread");
            });
            let h2 = std::thread::spawn(move || {
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
                    if let Ok(Some(_)) = command.try_wait() {
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
            repr: command_string
        },
                   address));
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


////////////////////////////////////////////////////////////////////////////////// REMOTE CONNECTION

// A newtype representing remaining bytes to be sent or retrieved.
#[derive(Debug, Clone)]
struct RemainingBytes(u64);

// A synchronous wrapper over a non-blocking Remote.
struct Remote{
    session: Option<&'static Session>,
    stream: TcpStream,
    proxycommand: Option<ProxyCommandForwarder>,
    channels: HashMap<Uuid, Channel<'static>>,
    progressing_outputs: HashMap<Uuid, Output>,
    progressing_reading_files: HashMap<Uuid, (usize, BufReader<File>)>,
    progressing_writing_files: HashMap<Uuid, (usize, File)>,
}

impl Drop for Remote{
    fn drop(&mut self){
        if !self.channels.is_empty(){
            panic!("Remote dropped, while channels were still open.");
        } else{
            // Unsafe trick to avoid memory leaks. Since we know that all our references to the
            // session were dropped (those are only channels that stays in the hashmap which is
            // now empty), we transmute the session to a Box and drop it.
            unsafe{
                let sess: Box<Session> = transmute(self.session.take().unwrap() as *const Session);
                drop(sess);
            }
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
                return Remote::from_proxy_command(&cmd, &host, &user);
            }
            config::SshProfile{
                hostname: Some(host),
                user:Some(user),
                proxycommand: None,
                port: Some(port),
                ..
            } => {
                let address = format!("{}:{}", host, port);
                return Remote::from_addr(address, &host, &user);
            }
            _ => return Err(Error::ConnectionFailed(format!("The encountered profile is invalid: \
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
        return Ok(Remote {
            session: Some(session),
            stream: stream,
            proxycommand: None,
            channels: HashMap::new(),
            progressing_outputs: HashMap::new(),
            progressing_reading_files: HashMap::new(),
            progressing_writing_files: HashMap::new(),
        })
    }

    /// Build, authenticate and starts an ssh session from a proxycommand.
    fn from_proxy_command(command: &str, host: &str, user: &str) -> Result<Remote, Error> {
        debug!("Remote: Creating remote connection from proxycommand {}", command);
        let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
        let mut remote = Remote::from_addr(addr, host, user)?;
        remote.proxycommand.replace(proxy_command);
        return Ok(remote);
    }

    /// Authenticate the ssh session.
    fn start_session(stream: &TcpStream, host: &str, user: &str) -> Result<Session, Error> {
        debug!("Remote: Opening remote connection to host {} as user {}", host, user);
        let mut session = Session::new().unwrap();
        session.method_pref(MethodType::HostKey, "ssh-rsa")
            .map_err(|_| Error::ConnectionFailed(format!("Failed to preferences")))?;
        trace!("Remote: Performing handshake");
        session.handshake(stream)
            .map_err(|e| Error::ConnectionFailed(format!("Failed to perform handshake: \n{}", e)))?;
        {
            trace!("Remote: Checking host key");
            let mut known_hosts = session.known_hosts().unwrap();
            let mut known_hosts_path = dirs::home_dir()
                .ok_or(Error::ConnectionFailed(format!("Failed to find the local home directory")))?;
            known_hosts_path.push(KNOWN_HOSTS_RPATH);
            known_hosts.read_file(known_hosts_path.as_path(),
                                  KnownHostFileKind::OpenSSH)
                .map_err(|e| Error::ConnectionFailed(format!("Failed to reach knownhost file:\n{}", e)))?;
            let (key, _) = session.host_key()
                .ok_or(Error::ConnectionFailed(format!("Host did not provide a key.")))?;
            match known_hosts.check(host, key) {
                CheckResult::Match => {}
                CheckResult::Mismatch => {
                    trace!("Remote: Host key mismatch....");
                    return Err(Error::ConnectionFailed(format!("The key presented by the host \
                    mismatches the one we know. ")));
                }
                CheckResult::NotFound => {
                    trace!("Remote: Host not known. Writing the key in the registry.");
                    known_hosts.add(host,
                                    key,
                                    "",
                                    KnownHostKeyFormat::SshRsa)
                        .map_err(|_| Error::ConnectionFailed(format!("Failed to add the host \
                        key in the registry")))?;
                    known_hosts.write_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
                        .map_err(|_| Error::ConnectionFailed(format!("Failed to write the \
                        knownhost file")))?;
                }
                CheckResult::Failure => {
                    trace!("Remote: Failed to check the host");
                    return Err(Error::ConnectionFailed(format!("Failed to check the host.")));
                }
            }
            trace!("Remote: Authenticating ourselves");
            let mut agent = session.agent().unwrap();
            agent.connect()
                .map_err(|e| Error::ConnectionFailed(format!("Ssh Agent was unavailable: \n{}", e)))?;
            agent.list_identities()
                .map_err(|e| Error::ConnectionFailed(format!("Couldn't list identities: \n{}", e)))?;
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
                trace!("RemoterResource: No identities registered in the agent allowed to authenticate.");
                return Err(Error::ConnectionFailed(format!("No identities registered in the ssh \
                agent allowed to connect.")));
            }
        }
        trace!("RemoteResource: Ssh session ready.");
        return Ok(session);
    }

    fn start_exec(&mut self, command: &str) -> Result<Uuid, Error>{
        debug!("Remote: Starting execution: {}", command);
        let mut channel = match self.session().channel_session() {
            Ok(c) => c,
            Err(_) => return Err(Error::ExecutionFailed(format!("Failed to open channel")))
        };
        if channel.exec(&command).is_err() {
            return Err(Error::ExecutionFailed(format!("Failed to exec command {}", command)))
        }
        let output = Output {
            status: ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let id = Uuid::new_v4();
        self.channels.insert(id.clone(), channel);
        self.progressing_outputs.insert(id.clone(), output);
        return Ok(id);
    }

    fn continue_exec(&mut self, id: &Uuid) -> Result<Output, Error>{
        debug!("Remote: Continuing execution {}", id);
        self.session().set_blocking(false);
        let mut chan = self.channels.remove(&id).unwrap();
        let mut output = self.progressing_outputs.remove(&id).unwrap();
        let eof = chan.wait_eof(); // We read eof beforehand, to avoid leaving bytes.
        let copied = copy(&mut chan.stream(0), &mut output.stdout)
            .and(copy(&mut chan.stream(1), &mut output.stderr));
        match copied {
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                self.session().set_blocking(true);
                self.channels.insert(id.clone(), chan);
                self.progressing_outputs.insert(id.clone(), output);
                return Err(Error::WouldBlock)
            }
            Err(e) => {
                self.session().set_blocking(true);
                return Err(Error::ExecutionFailed(format!("Failed to read stdout: {}", e)))
            }
            Ok(_) => {}
        }
        if let Err(e) = eof {
            if e.code() as i64 == -37 {
                self.session().set_blocking(true);
                self.channels.insert(id.clone(), chan);
                self.progressing_outputs.insert(id.clone(), output);
                return Err(Error::WouldBlock)
            } else {
                self.session().set_blocking(true);
                return Err(Error::ExecutionFailed(format!("Failed to acquire eof")));
            }
        }
        self.session().set_blocking(true);
        let ecode = match chan.exit_status() {
            Ok(c) => {
                trace!("Remote: Found exit code {}", c); c
            }
            Err(_) => return Err(Error::ExecutionFailed(format!("Failed to get exit code")))
        };
        output.status = ExitStatusExt::from_raw(ecode);
        trace!("Remote: Output is now {:?}", output);
        if let Err(_) = chan.close() {
            return Err(Error::ExecutionFailed(format!("Failed to close channel")))
        }
        if let Err(_) = chan.wait_close() {
            return Err(Error::ExecutionFailed(format!("Failed to wait to close channel")));
        }
        return Ok(output);
    }

    fn start_scp_send(&mut self, local_path: &PathBuf, remote_path: &PathBuf) -> Result<Uuid, Error>{
        debug!("Remote: Starting send of {} to {}",
               local_path.to_str().unwrap(),
               remote_path.to_str().unwrap());
        let local_file = match File::open(local_path) {
            Ok(f) => f,
            Err(_) => return Err(Error::ScpSendFailed(format!("Failed to open local file"))),
        };
        let bytes = local_file.metadata().unwrap().len();
        let chan = match self.session().scp_send(&remote_path,0o755,bytes,None) {
            Ok(chan) => chan,
            Err(_) => return Err(Error::ScpSendFailed(format!("Failed to open scp send channel"))),
        };
        let id = Uuid::new_v4();
        self.channels.insert(id.clone(), chan);
        self.progressing_reading_files.insert(id.clone(), (bytes as usize, BufReader::new(local_file)));
        return Ok(id);
    }

    fn continue_scp_send(&mut self, id: &Uuid) -> Result<(), Error>{
        debug!("Remote: Continuing send {}", id);
        let mut chan = self.channels.remove(&id).unwrap();
        let (bytes, mut file) = self.progressing_reading_files.remove(&id).unwrap();
        self.session().set_blocking(false);
        match bufread_bufcopy(&mut file, &mut chan.stream(0)){
            (b, Err(ref e)) if e.kind() == ErrorKind::WouldBlock => {
                self.session().set_blocking(true);
                self.channels.insert(id.clone(), chan);
                self.progressing_reading_files.insert(id.clone(), (bytes - b as usize, file));
                return Err(Error::WouldBlock);
            }
            (b, Ok(())) =>{
                if bytes != 0 {
                    self.session().set_blocking(true);
                    self.channels.insert(id.clone(), chan);
                    self.progressing_reading_files.insert(id.clone(), (bytes - b as usize, file));
                    return Err(Error::WouldBlock);
                } else {
                    self.session().set_blocking(true);
                    if chan.send_eof().is_err(){ return Err(Error::ScpSendFailed(format!("Failed to send eof")))};
                    if chan.wait_eof().is_err(){ return Err(Error::ScpSendFailed(format!("Failed to wait eof")))};
                    if chan.close().is_err() { return Err(Error::ScpSendFailed(format!("Failed to send channel close")))};
                    if chan.wait_close().is_err(){return Err(Error::ScpSendFailed(format!("Failed to wait channel close")))};
                    return Ok(())
                }
            }
            (_, Err(_)) => {
                self.session().set_blocking(true);
                return Err(Error::ScpSendFailed(format!("Failed to copy")));
            }
        }
    }

    fn start_scp_fetch(&mut self, remote_path: &PathBuf, local_path: &PathBuf) -> Result<Uuid, Error>{
        debug!("Remote: Starting fetch of {} to {}",
               remote_path.to_str().unwrap(),
               local_path.to_str().unwrap());
        std::fs::remove_file(&local_path);
        let local_file = match OpenOptions::new().write(true).create_new(true).open(local_path) {
            Ok(f) => f,
            Err(_) => return Err(Error::ScpFetchFailed(format!("Failed to open local file")))
        };
        let (chan, stats) = match self.session().scp_recv(&remote_path) {
            Ok(c) => c,
            Err(_) => return Err(Error::ScpFetchFailed(format!("Failed to start scp recv")))
        };
        let id = Uuid::new_v4();
        self.channels.insert(id.clone(), chan);
        self.progressing_writing_files.insert(id.clone(), (stats.size() as usize, local_file));
        return Ok(id);
    }

    fn continue_scp_fetch(&mut self, id: &Uuid) -> Result<(), Error>{
        debug!("Remote: Continuing fetch {}", id);
        let mut chan = self.channels.remove(&id).unwrap();
        let (bytes, mut file) = self.progressing_writing_files.remove(&id).unwrap();
        self.session().set_blocking(false);
        match read_bufcopy(&mut chan.stream(0), &mut file) {
            (b, Err(ref e)) if e.kind() == ErrorKind::WouldBlock => {
                self.session().set_blocking(true);
                self.channels.insert(id.clone(), chan);
                self.progressing_writing_files.insert(id.clone(), (bytes - b as usize, file));
                return Err(Error::WouldBlock);
            }
            (b, Ok(())) =>{
                if bytes != 0{
                    self.session().set_blocking(true);
                    self.channels.insert(id.clone(), chan);
                    self.progressing_writing_files.insert(id.clone(), (bytes - b as usize, file));
                    return Err(Error::WouldBlock);
                } else {
                    self.session().set_blocking(true);
                    if chan.send_eof().is_err() { return Err(Error::ScpFetchFailed(format!("Failed to send eof"))) };
                    if chan.wait_eof().is_err() { return Err(Error::ScpFetchFailed(format!("Failed to wait eof"))) };
                    if chan.close().is_err() { return Err(Error::ScpFetchFailed(format!("Failed to send channel close"))) };
                    if chan.wait_close().is_err() { return Err(Error::ScpFetchFailed(format!("Failed to wait channel close"))) };
                    return Ok(())
                }
            }
            (_, Err(_)) => {
                self.session().set_blocking(true);
                return Err(Error::ScpFetchFailed(format!("Failed to copy data")));
            }
        }
    }
}

/// Type alias for the Repository resource operations.
type RemoteOp = Box<dyn UseResource<RemoteResource>>;

/// The remote resource.
struct RemoteResource{
    remote: Remote,
    queue: Vec<RemoteOp>
}

/// This structure allows to perform operations on a remote host. Operations are executed in a
/// separate thread, which allows to avoid moving and synchronizing the ssh2 session between
/// different tasks. As such, operations are represented by futures, which are driven to completion
/// by the thread.
// Under the hood, the future sends an operation to the connection thread which takes care about
// running the operation to completion, and wakes the future when the result is available.
#[derive(Clone)]
pub struct RemoteHandle {
    sender: Sender<RemoteOp>,
    repr: String,
    dropper: Dropper<()>,
}

impl RemoteHandle {

    /// Spawn a remote connection from a profile. A handle is returned to create futures.
    pub fn spawn_resource(profile: config::SshProfile) -> Result<RemoteHandle, Error>{
        debug!("RemoteHandle: Start connection thread");
        let (sender, receiver): (Sender<RemoteOp>, Receiver<RemoteOp>) = unbounded();
        let (rs, rr): (Sender<Result<String, Error>>, Receiver<Result<String, Error>>) = unbounded();
        let handle = thread::spawn(move || {
            trace!("RemoteResource: Instantiating the resource");
            let remote = match Remote::from_profile(profile){
                Ok(r) => r,
                Err(e) => {
                    rs.send(Err(e));
                    return ();
                }
            };
            let mut res = RemoteResource{remote, queue:Vec::new()};
            let repr = match res.remote.proxycommand{
                Some(ref s) => format!("ProxyCommand \'{}\'", s.repr),
                None => format!("Address"),
            };
            rs.send(Ok(repr));
            trace!("RemoteResource: Starting resource loop");
            loop{
                // Handle a message
                match receiver.try_recv() {
                    Ok(o) => {
                        trace!("RemoteResource: Received operation");
                        res.queue.push(o)
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        trace!("RemoteResource: Channel disconnected. Leaving...");
                        break;
                    }
                }
                // Handle an operation
                if let Some(s) = res.queue.pop() {
                    s.progress(&mut res);
                }
            }
            trace!("RemoteResource: Leaving thread.");
            return ();
        });
        let repr = rr.recv().unwrap()?;
        return Ok(RemoteHandle{
            sender,
            repr,
            dropper: Dropper::from_handle(handle),
        });
    }

    /// Creates a RemoteExecFuture that resolves into a Result over an Output after the command was
    /// executed on the remote host.
    pub fn async_exec(&self, command: &str) -> ExecFuture {
        let (recv, op) = ExecOp::from(Stateful::from(Starting(command.to_owned())));
        return ExecFuture(OperationFuture::new(op, self.sender.clone(), recv));
    }

    /// Creates a ScpSendFuture that resolves into a Result over () after the file was sent to the
    /// remote host.
    pub fn async_scp_send(&self, local_path: &PathBuf, remote_path: &PathBuf) -> ScpSendFuture {
        let (recv, op) = ScpSendOp::from(Stateful::from(Starting((local_path.to_owned(),
                                                                  remote_path.to_owned()))));
        return ScpSendFuture(OperationFuture::new(op, self.sender.clone(), recv));
    }


    /// Creates a ScpFetchFuture that resolves into a Result over () after the file was fetched from
    /// remote host.
    pub fn async_scp_fetch(&self, remote_path: &PathBuf, local_path: &PathBuf) -> ScpFetchFuture {
        let (recv, op) = ScpFetchOp::from(Stateful::from(Starting((remote_path.to_owned(),
                                                                  local_path.to_owned()))));
        return ScpFetchFuture(OperationFuture::new(op, self.sender.clone(), recv));
    }

    /// Returns the number of handles over connection
    pub fn handles_count(&self) -> usize{
        return self.dropper.strong_count();
    }

}

impl fmt::Debug for RemoteHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "RemoteHandle<{}>", self.repr)
    }
}


///////////////////////////////////////////////////////////////////////////////////////// OPERATIONS

/// A Future that resolves into a Result over an Output, after the command was executed on the remote
/// host.
pub struct ExecFuture(OperationFuture<ExecMarker, RemoteResource, Output>);
impl Future for ExecFuture {
    type Output = Result<Output, crate::primitives::Error>;
    fn poll(mut self: Pin<&mut Self>, context: &mut Context) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}
struct ExecMarker;
type ExecOp = Operation<ExecMarker>;
impl UseResource<RemoteResource> for ExecOp {
    fn progress(mut self: Box<Self>, resource: &mut RemoteResource) {
        debug!("ExecOp: Progressing...");
        if let Some(s) = self.state.to_state::<Starting<String>>() {
            trace!("ExecOp: Found Starting");
            match resource.remote.start_exec(&s.0){
                Ok(id) => {
                    self.state.transition::<Starting<String>, Progressing<_>>(Progressing(id));
                    resource.queue.push(self);
                }
                Err(e) => {
                    self.state.transition::<Starting<String>, Finished<Output>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        }else if let Some(s) = self.state.to_state::<Progressing<Uuid>>(){
            trace!("ExecOp: Found Progressing");
            match resource.remote.continue_exec(&s.0){
                Ok(o) => {
                    self.state.transition::<Progressing<Uuid>, Finished<_>>(Finished(Ok(o)));
                    resource.queue.push(self);
                }
                Err(Error::WouldBlock) => resource.queue.push(self),
                Err(e) => {
                    self.state.transition::<Progressing<Uuid>, Finished<Output>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        } else if let Some(_) = self.state.to_state::<Finished<Output>>() {
            trace!("ExecOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with ExecOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("ExecOp: Sending...");
            sender.send(*self);
            trace!("ExecOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("ExecOp: Progress over.");
    }
}

/// A Future that resolves into a Result over () after the file was sent to the remote host.
pub struct ScpSendFuture(OperationFuture<ScpSendMarker, RemoteResource, ()>);
impl Future for ScpSendFuture {
    type Output = Result<(), crate::primitives::Error>;
    fn poll(mut self: Pin<&mut Self>, context: &mut Context) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}
struct ScpSendMarker {}
type ScpSendOp = Operation<ScpSendMarker>;
impl UseResource<RemoteResource> for ScpSendOp {
    fn progress(mut self: Box<Self>, resource: &mut RemoteResource) {
        debug!("ScpSendOp: Progressing...");
        if let Some(s) = self.state.to_state::<Starting<(PathBuf, PathBuf)>>() {
            trace!("ScpSendOp: Found Starting");
            match resource.remote.start_scp_send(&(s.0).0, &(s.0).1){
                Ok(id) => {
                    self.state.transition::<Starting<(PathBuf, PathBuf)>, Progressing<_>>(
                        Progressing(id)
                    );
                    resource.queue.push(self);
                }
                Err(e) => {
                    self.state.transition::<Starting<(PathBuf, PathBuf)>, Finished<()>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        }else if let Some(s) = self.state.to_state::<Progressing<Uuid>>(){
            trace!("ScpSendOp: Found Progressing");
            match resource.remote.continue_scp_send(&s.0){
                Ok(o) => {
                    self.state.transition::<Progressing<Uuid>, Finished<()>>(Finished(Ok(o)));
                    resource.queue.push(self);
                }
                Err(Error::WouldBlock) => resource.queue.push(self),
                Err(e) => {
                    self.state.transition::<Progressing<Uuid>, Finished<()>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        } else if let Some(_) = self.state.to_state::<Finished<()>>() {
            trace!("ScpSendOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with ExecOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("ScpSendOp: Sending...");
            sender.send(*self);
            trace!("ScpSendOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("ScpSendOp: Progress over.");
    }
}

/// A Future that resolves into a Result over a () after the file was fetched from the remote host
pub struct ScpFetchFuture(OperationFuture<ScpFetchMarker, RemoteResource, ()>);
impl Future for ScpFetchFuture {
    type Output = Result<(), crate::primitives::Error>;
    fn poll(mut self: Pin<&mut Self>, context: &mut Context) -> Poll<Self::Output> { 
        Pin::new(&mut self.0).poll(context)
    }
}
struct ScpFetchMarker {}
type ScpFetchOp = Operation<ScpFetchMarker>;
impl UseResource<RemoteResource> for ScpFetchOp {
    fn progress(mut self: Box<Self>, resource: &mut RemoteResource) {
        debug!("ScpFetchOp: Progressing...");
        if let Some(s) = self.state.to_state::<Starting<(PathBuf, PathBuf)>>() {
            trace!("ScpFetchOp: Found Starting");
            match resource.remote.start_scp_fetch(&(s.0).0, &(s.0).1){
                Ok(id) => {
                    self.state.transition::<Starting<(PathBuf, PathBuf)>, Progressing<_>>(
                        Progressing(id)
                    );
                    resource.queue.push(self);
                }
                Err(e) => {
                    self.state.transition::<Starting<(PathBuf, PathBuf)>, Finished<()>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        }else if let Some(s) = self.state.to_state::<Progressing<Uuid>>(){
            trace!("ScpFetchOp: Found Progressing");
            match resource.remote.continue_scp_fetch(&s.0){
                Ok(o) => {
                    self.state.transition::<Progressing<Uuid>, Finished<()>>(Finished(Ok(o)));
                    resource.queue.push(self);
                }
                Err(Error::WouldBlock) => resource.queue.push(self),
                Err(e) => {
                    self.state.transition::<Progressing<Uuid>, Finished<()>>(Finished(Err(
                        crate::primitives::Error::from(e))));
                    resource.queue.push(self);
                }
            }
        } else if let Some(_) = self.state.to_state::<Finished<()>>() {
            trace!("ScpFetchOp: Found Finished");
            let waker = self
                .waker
                .as_ref()
                .expect("No waker given with ExecOp")
                .to_owned();
            let sender = self.sender.clone();
            trace!("ScpFetchOp: Sending...");
            sender.send(*self);
            trace!("ScpFetchOp: Waking ...");
            waker.wake();
        } else {
            unreachable!()
        }
        trace!("ScpFetchOp: Progress over.");
    }
}

//////////////////////////////////////////////////////////////////////////////////// COPY PRIMITIVES

// This function copies a bufread reader into a writer by taking care about always returning the
// number of bytes read even in case of an error (for WouldBlock error)
fn bufread_bufcopy<R: BufRead, W:Write>(reader: &mut R, writer: &mut W) -> (u64, Result<(), io::Error>){
    let mut bytes = 0 as u64;
    loop{
        match reader.fill_buf(){
            Ok(buf) => {
                let l = std::cmp::min(1_024_000, buf.len());
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
    //return (bytes, Ok(()))
}

// This function does the same for an unbuffered read.
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
                        return (bytes, Err(io::Error::new(ErrorKind::Other, "Don't know")))
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

/////////////////////////////////////////////////////////////////////////////////////////////// TEST
#[cfg(test)]
mod test {
    use super::*;
    use crate::ssh::config::SshProfile;

    #[test]
    fn test_proxy_command_forwarder() {
        let (_, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut buf = [0 as u8; 5];
        stream.read(&mut buf).unwrap();
        assert_eq!(buf, "kikou".as_bytes());
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn test_async_exec() {
        use futures::executor::block_on;
        async fn connect_and_ls() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn_resource(profile).unwrap();
            let output = await!(remote.async_exec("echo kikou && sleep 1 && echo hello!")).unwrap();
            println!("Executed and resulted in {:?}", String::from_utf8(output.stdout).unwrap());
        }
        block_on(connect_and_ls());
    }

    #[test]
    fn test_async_scp_send() {
        use futures::executor::block_on;
        async fn connect_and_scp_send() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn_resource(profile).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/local.txt", "bs=35M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            await!(remote.async_scp_send( &local_f, &remote_f)).unwrap();
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
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: Some(22),
                proxycommand: None, //Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn_resource(profile).unwrap();
            let output = std::process::Command::new("dd")
                .args(&["if=/dev/urandom", "of=/tmp/remote.txt", "bs=33M", "count=1"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let local_f = PathBuf::from("/tmp/local.txt");
            let remote_f = PathBuf::from("/tmp/remote.txt");
            let bef = std::time::Instant::now();
            await!(remote.async_scp_fetch( &remote_f, &local_f)).unwrap();
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
        let profile = SshProfile{
            name: "localhost".to_owned(),
            hostname: Some("127.0.0.1".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None,
        };
        let remote = RemoteHandle::spawn_resource(profile).unwrap();
        async fn connect_and_ls(remote: RemoteHandle) -> std::process::Output{
            return await!(remote.async_exec("echo 1 && sleep 1 && echo 2")).unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..10{
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

