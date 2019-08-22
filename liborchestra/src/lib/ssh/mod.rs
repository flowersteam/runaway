//! liborchestra/mod.rs
//! Author: Alexandre Péré
//!
//! This module contains structures that wraps the ssh2 session object to provide ways to handle 
//! ssh configurations, authentications, and proxy-commands. In particular, since libssh2
//! does not provide ways to handle proxy-commands, we use a threaded copy loop that open a
//! proxy-command as a subprocess and copy the output on a random tcp socket.
//!
//! After instantiation, the session object is used through new style futures. For this reason,
//! after authentication the session will be placed in a thread that will take care about handling
//! the operations that may be required by the user. The operations are made in a totally 
//! asynchronous fashion: If a command blocks, it will be parked, until further data is available.
//!
//! Note: Though ssh operations are handled asynchronously, the connection and the handshake are 
//! made in a synchronous and blocking manner.


//------------------------------------------------------------------------------------------ IMPORTS


use ssh2::{
    Session,
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
    io::{prelude::*, BufReader},
    process::{Stdio, Command, Output},
    os::unix::process::ExitStatusExt,
    error,
    fmt,
    path::PathBuf,
    collections::HashSet,
    fs::{File, OpenOptions},
    time::Duration,
};
use dirs;
use crate::KNOWN_HOSTS_RPATH;
use futures::future::Future;
use crate::derive_from_error;
use crate::primitives::Dropper;
use std::intrinsics::transmute;
use futures::lock::Mutex;
use futures::executor;
use futures::future;
use futures::StreamExt;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use futures::SinkExt;
use futures::channel::{mpsc, oneshot};
use std::time;


//------------------------------------------------------------------------------------------  MODULE


pub mod config;


//------------------------------------------------------------------------------------------- MACROS

/// This macro allows to asynchronously wait for (at least) a given time. This means that the thread
/// is yielded when it is done. For now, it creates a separate thread each time a sleep is needed, 
/// which is far from ideal.
#[macro_export] 
macro_rules! async_sleep {
    ($dur: expr) => {
        {
            let (tx, rx) = oneshot::channel();
            thread::spawn(move || {
                thread::sleep($dur);
                tx.send(()).unwrap();
            });
            rx.await.unwrap();

        }
    };
}

/// This macro allows to intercept a wouldblock error returned by the expression evaluation, and 
/// awaits for 1 ns (at least) before retrying. 
#[macro_export]
macro_rules! await_wouldblock_io {
    ($expr:expr) => {
        {
            loop{
                match $expr {
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        async_sleep!(Duration::from_nanos(1))
                    }
                    res => break res,
                }
            }
        }
    }
}

/// This macro allows to intercept a wouldblock error returned by the expression evaluation, and 
/// awaits for 1 ns (at least) before retrying. 
#[macro_export]
macro_rules! await_wouldblock_ssh {
    ($expr:expr) => {
        {
            loop{
                match $expr {
                    Err(ref e) if e.code() == -37 => {
                        async_sleep!(Duration::from_nanos(1))
                    }
                    Err(ref e) if e.code() == -21 => {
                        async_sleep!(Duration::from_nanos(1))
                    }
                    res => break res,
                }
            }
        }
    }
}

/// This macro allows to retry an ssh expression if the error code received was $code. It allows to 
/// retry commands that fails every now and then for a limited amount of time.
#[macro_export]
macro_rules! await_retry_n_ssh {
    ($expr:expr, $nb:expr, $($code:expr),*) => {
       {    
            let nb = $nb as usize;
            let mut i = 1 as usize;
            loop{
                match $expr {
                    Err(e)  => {
                        if i == nb {
                            break Err(e)
                        }
                        $(
                            else if e.code() == $code as i32 {
                                async_sleep!(Duration::from_nanos(1));
                                i += 1;
                            }
                        )*
                    }
                    res => {
                        break res
                    }
                }
            }
        }
    }
}

/// This macro allows to retry an ssh expression if the error code received was $code. It allows to 
/// retry commands that fail but must be retried until it's ok. For example 
#[macro_export]
macro_rules! await_retry_ssh {
    ($expr:expr, $($code:expr),*) => {
       {    
            loop{
                match $expr {
                    Err(e)  => {
                        $(  if e.code() == $code as i32 {
                                async_sleep!(Duration::from_nanos(1));
                            } else  )*
                        {
                            break Err(e)
                        }
                    }
                    res => {
                        break res
                    }
                }
            }
        }
    }
}

/// This macro generates the pty exec string out of an actual string. Just intended to remove 
/// boilerplate.
#[macro_export]
macro_rules! ptyxec {
    ($expr: expr) =>{
        format!("\\
            {{ (({}); echo RUNAWAY_ECODE: $?) | sed 's/^/RUNAWAY_STDOUT: /'; }}  \\
            2>&1 1>&3 | sed 's/^/RUNAWAY_STDERR: /'\n\\
            ", $expr)
            .as_bytes()
    }
}


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

impl From<Error> for crate::primitives::Error{
    fn from(other: Error) -> crate::primitives::Error{
        return crate::primitives::Error::Operation(format!("{}", other));
    }
}


//------------------------------------------------------------------------------------ PROXY-COMMAND


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
                socket1.shutdown(Shutdown::Write);
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
                socket2.shutdown(Shutdown::Read);
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
            return (h1, h2, h3);
        });
        trace!("ProxyCommandForwarder: Returning proxy command");
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
            if !known_hosts_path.exists(){
                File::create(known_hosts_path.clone())
                    .map_err(|e| Error::ConnectionFailed(format!("Failed to create knownhost file")));
            }
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
                    let result = agent.userauth(user, id.as_ref().unwrap());
                    if result.is_err(){
                        debug!("Remote: Failed to authenticate to {} with {}: {:?}", 
                                host,
                                id.as_ref().unwrap().comment(), 
                                result);
                    } else {
                        info!("Remote: Authentication to {} succeeded with {}", host, 
                        id.as_ref().unwrap().comment());
                    }
                    result.is_err()
                })
                .map(|id| { id.unwrap().comment().to_owned() })
                .collect::<HashSet<_>>();
            if ids == failed_ids {
                trace!("RemoteResource: No identities registered in the agent allowed to authenticate.");
                return Err(Error::ConnectionFailed(format!("No identities registered in the ssh \
                agent allowed to connect.")));
            }
        }
        session.set_blocking(false);
        trace!("RemoteResource: Ssh session ready.");
        return Ok(session);
    }

    /// Asynchronous function used to execute a command on the remote. 
    async fn exec(remote: Arc<Mutex<Remote>>, command: String) -> Result<Output, Error>{
        debug!("Remote: Starting execution: {}", command);
        // Error -21 corresponds to not enough channels available. It must be retried until an other 
        // execution came to an end, and made a channel available. 
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
        let mut channel  = match ret {
            Ok(c) => c,
            Err(e) => return Err(Error::ExecutionFailed(format!("Failed to open channel: {}", e)))
        };
        if let Err(e) = await_wouldblock_ssh!(channel.exec(&format!("{}\n", command))) {
            return Err(Error::ExecutionFailed(format!("Failed to exec command {}: {}", command, e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.send_eof()){
            return Err(Error::ExecutionFailed(format!("Failed to send EOF: {}", e)))
        }
        let mut output = Output {
            status: ExitStatusExt::from_raw(911),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let mut buf = [0 as u8; 8*1024];
        loop {
            let eof = channel.eof();
            {   
                let mut stream = channel.stream(0);
                match await_wouldblock_io!(stream.read(&mut buf)){
                    Err(e) => return Err(Error::ExecutionFailed(format!("Failed to read stdout: {:?}", e))),
                    Ok(b) => {
                        output.stdout.write(&mut buf[..b]).unwrap();
                    }
                }
            }
            {
                let mut stream = channel.stream(1);
                match await_wouldblock_io!(stream.read(&mut buf)){
                    Err(e) => return Err(Error::ExecutionFailed(format!("Failed to read stderr: {:?}", e))),
                    Ok(b) => {
                        output.stderr.write(&mut buf[..b]).unwrap();
                    }
                }
            }
            if eof{
                break
            } else {
                let(tx, rx) = oneshot::channel();
                     thread::spawn(move || {
                         thread::sleep(Duration::from_nanos(1));
                         tx.send(()).unwrap();
                     });
                 rx.await.unwrap();
            }
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_eof()){
            return Err(Error::ExecutionFailed(format!("Failed to wait eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.close()) {
            return Err(Error::ExecutionFailed(format!("Failed to close channel: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_close()) {
            return Err(Error::ExecutionFailed(format!("Failed to wait to close channel: {}", e)));
        }
        let ecode = match await_wouldblock_ssh!(channel.exit_status()) {
            Ok(c) => {
                trace!("Remote: Found exit code {}", c); c
            }
            Err(e) => return Err(Error::ExecutionFailed(format!("Failed to get exit code: {}", e)))
        };
        output.status = ExitStatusExt::from_raw(ecode);
        trace!("Remote: Returning output {:?}", output);

        return Ok(output);
    }

    /// Asynchronous function used to execute an interactive command on the remote. 
    async fn pty (remote: Arc<Mutex<Remote>>, 
        commands: Vec<String>, 
        stdout_cb: Box<dyn Fn(Vec<u8>)+Send+'static>,
        stderr_cb: Box<dyn Fn(Vec<u8>)+Send+'static>) -> Result<Output, Error>{
        debug!("Remote: Starting pty: {:?}", commands);
        let ret = await_wouldblock_ssh!(await_retry_ssh!(
            {
                remote.lock()
                    .await
                    .session()
                    .channel_session()
            },
            -21)
        );
        let mut channel  = match ret {
            Ok(c) => c,
            Err(e) => return Err(Error::ExecutionFailed(format!("Failed to open channel: {}", e)))
        }; 
        if let Err(e) = await_wouldblock_ssh!(
            await_retry_n_ssh!(channel.request_pty("xterm", None, Some((0,0,0,0))), 10, -14)){
            return Err(Error::ExecutionFailed(format!("Failed to request pty: {}", e)));
        }
        if let Err(e) = await_wouldblock_ssh!(channel.shell()){
            return Err(Error::ExecutionFailed(format!("Failed to request shell: {}", e)));
        }
        if let Err(e) = await_wouldblock_io!(channel.write_all("exec 3>&2;\nsh\n".as_bytes())){
            return Err(Error::ExecutionFailed(format!("Failed to request shell: {}", e)));
        }
        let mut commands = commands;
        let mut output = Output {
            status: ExitStatusExt::from_raw(911),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let mut stream = BufReader::new(channel);
        let mut buffer = String::new(); 
        let v = commands.remove(0);
        if let Err(e) = await_wouldblock_io!(stream.get_mut().write_all(ptyxec!(v))){
                return Err(Error::ExecutionFailed(format!("Failed to exec command '{}': {}", v, e)))
        }
        loop{
            buffer.clear();
            match await_wouldblock_io!(stream.read_line(&mut buffer)){
                Ok(0) => break,
                Ok(_) => {
                    trace!("Buffer: {}", buffer);
                    if buffer.contains("RUNAWAY_STDOUT: RUNAWAY_ECODE: "){
                        let ecode = buffer.replace("RUNAWAY_STDOUT: RUNAWAY_ECODE: ", "")
                            .replace("\r\n", "")
                            .parse::<i32>()
                            .unwrap(); 
                        output.status = ExitStatusExt::from_raw(ecode);
                        if ecode != 0{
                            break
                        }
                        if commands.len() == 0{
                            break
                        } else {
                            let v = commands.remove(0);
                            if let Err(e) = await_wouldblock_io!(stream.get_mut().write_all(ptyxec!(v))){
                                    return Err(Error::ExecutionFailed(format!("Failed to exec command '{}': {}", v, e)))
                            }
                        }
                        await_wouldblock_io!(stream.get_mut().flush()).unwrap();
                    } else if buffer.contains("RUNAWAY_STDOUT: ") && !buffer.contains("sed"){
                        let out = buffer.replace("RUNAWAY_STDOUT: ", "");
                        output.stdout.write(out.as_bytes()).unwrap();
                        stdout_cb(out.as_bytes().to_vec());
                    } else if buffer.contains("RUNAWAY_STDERR: ") && !buffer.contains("sed"){
                        let err = buffer.replace("RUNAWAY_STDERR: ", "");
                        output.stderr.write(err.as_bytes()).unwrap();
                        stderr_cb(err.as_bytes().to_vec());
                    } else if buffer.contains("sh: ") { // Intercept shell error 
                        output.stderr.write_all(format!("sh failed to execute your command: '{}'",
                                                buffer)
                            .as_bytes());
                        break
                    }
                },
                Err(e) => {
                    return Err(Error::ExecutionFailed(format!("Failed to read outputs: {}", e)));
                }
            }
        }
        if let Err(e) = await_wouldblock_io!(stream.get_mut().write_all(format!("exit\nexit\n")
                                        .as_bytes())){
            return Err(Error::ExecutionFailed(format!("Failed to exit shell.")))
                                    }
        if let Err(e) = await_wouldblock_ssh!(stream.get_mut().wait_eof()){
            return Err(Error::ExecutionFailed(format!("Failed to wait eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(stream.get_mut().close()) {
            return Err(Error::ExecutionFailed(format!("Failed to close channel: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(stream.get_mut().wait_close()) {
            return Err(Error::ExecutionFailed(format!("Failed to wait to close channel: {}", e)));
        }
        output.stdout = String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n")
            .as_bytes()
            .to_vec();
        output.stderr = String::from_utf8(output.stderr)
            .unwrap()
            .replace("\r\n", "\n")
            .as_bytes()
            .to_vec();
        return Ok(output);
    }

    /// Asynchronous function used to send a file to a remote.
    async fn scp_send(remote: Arc<Mutex<Remote>>, local_path: PathBuf, remote_path: PathBuf) -> Result<(), Error>{
        debug!("Remote: Starting transfert of local {} to remote {}", 
            local_path.to_str().unwrap(), 
            remote_path.to_str().unwrap());
        let mut local_file = match File::open(local_path) {
            Ok(f) => BufReader::new(f),
            Err(e) => return Err(Error::ScpSendFailed(format!("Failed to open local file: {}", e))),
        };
        let bytes = local_file.get_ref().metadata().unwrap().len();
        let ret = await_wouldblock_ssh!(
            {
                remote.lock()
                    .await
                    .session()
                    .scp_send(&remote_path,0o755,bytes,None)
            }
        );
        let mut channel = match ret{
            Ok(chan) => chan,
            Err(ref e) if e.code() == -21 => {
                error!("Remote: Failed to obtain channel");
                return Err(Error::ChannelNotAvailable)
            },
            Err(e) => return Err(Error::ScpSendFailed(format!("Failed to open scp send channel: {}", e))),
        };
        let mut stream = channel.stream(0);
        let mut remaining_bytes = bytes as i64;
        trace!("Remote: Starting scp send copy");
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
        if let Err(e) = await_wouldblock_ssh!(channel.send_eof()){
            return Err(Error::ScpSendFailed(format!("Failed to send eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_eof()){
            return Err(Error::ScpSendFailed(format!("Failed to wait eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.close()) {
            return Err(Error::ScpSendFailed(format!("Failed to close channel: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_close()) {
            return Err(Error::ScpSendFailed(format!("Failed to wait to close channel: {}", e)));
        }
        return Ok(());
    }

    /// Asynchronous method used to fetch a file from remote.
    async fn scp_fetch(remote: Arc<Mutex<Remote>>, remote_path: PathBuf, local_path: PathBuf) -> Result<(), Error>{
        debug!("Remote: Starting transfert of remote {} to local {}", 
            remote_path.to_str().unwrap(), 
            local_path.to_str().unwrap());
        std::fs::remove_file(&local_path);
        let mut local_file = match OpenOptions::new().write(true).create_new(true).open(local_path) {
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
        let (mut channel, stats) = match ret {
            Ok(c) => c,
            Err(ref e) if e.code() == -21 => {
                error!("Remote: Failed to open channel...");
                return Err(Error::ChannelNotAvailable)
            }
            Err(e) => return Err(Error::ScpFetchFailed(format!("Failed to open scp recv channel: {}", e))),
        };
        let mut stream = channel.stream(0);
        let mut remaining_bytes = stats.size() as i64;
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
        if let Err(e) = await_wouldblock_ssh!(channel.send_eof()){
            return Err(Error::ScpSendFailed(format!("Failed to send eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_eof()){
            return Err(Error::ScpSendFailed(format!("Failed to wait eof: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.close()) {
            return Err(Error::ScpSendFailed(format!("Failed to close channel: {}", e)))
        }
        if let Err(e) = await_wouldblock_ssh!(channel.wait_close()) {
            return Err(Error::ScpSendFailed(format!("Failed to wait to close channel: {}", e)));
        }

        return Ok(());
    }

}

/// The operation inputs, sent by the outer futures and processed by inner thread.
enum OperationInput {
    Exec(String),
    Pty(Vec<String>, Box<dyn Fn(Vec<u8>)+Send+'static>, Box<dyn Fn(Vec<u8>)+Send+'static>),
    ScpSend(PathBuf, PathBuf),
    ScpFetch(PathBuf, PathBuf)
}

impl std::fmt::Debug for OperationInput{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self{
            OperationInput::Exec(c) => write!(f, "Exec({:?})", c),
            OperationInput::Pty(c, out, err) => write!(f, "Pty({:?}, {}, {})", c, stringify!(out), stringify!(err)),
            OperationInput::ScpSend(a, b) => write!(f, "ScpSend({:?}, {:?})", a, b),
            OperationInput::ScpFetch(a, b) => write!(f, "ScpFetch({:?}, {:?})", a, b),
        }
    }
}

/// The operation outputs, sent by the inner futures and processed by the outer thread.
#[derive(Debug)]
enum OperationOutput {
    Exec(Result<Output, Error>),
    Pty(Result<Output, Error>),
    ScpSend(Result<(), Error>),
    ScpFetch(Result<(), Error>)
}


/// A handle to an inner application. Offer a future interface to the inner application.
#[derive(Clone)]
pub struct RemoteHandle {
    sender: mpsc::UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    repr: String,
    dropper: Dropper,
}

impl fmt::Debug for RemoteHandle{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result{
        write!(f, "RemoteHandle<{}>", self.repr)
    }
}

impl RemoteHandle {
    /// Spawns the application and returns a handle to it. 
    pub fn spawn(profile: config::SshProfile) -> Result<RemoteHandle, Error> {
        debug!("RemoteHandle: Start remote thread.");
        let (sender, receiver) = mpsc::unbounded();
        let (start_tx, start_rx) = crossbeam::channel::unbounded();
        let repr = match &profile.proxycommand{
            Some(p) => format!("{}", p),
            None => format!("{}:{}", profile.hostname.as_ref().unwrap(), profile.port.as_ref().unwrap())
        };
        let handle = std::thread::Builder::new().name("remote".to_owned()).spawn(move || {
            trace!("Remote Thread: Creating resource in thread");
            let remote = match Remote::from_profile(profile){
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
                        OperationInput::Pty(commands, stdout_cb, stderr_cb) => {
                            spawner.spawn_local(
                                Remote::pty(remote.clone(), commands, stdout_cb, stderr_cb)
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
            sender: sender,
            repr: repr,
            dropper: Dropper::from_closure(
                Box::new(move ||{
                    drop_sender.close_channel();
                    handle.join();
                }), 
                format!("RemoteHandle")),
        })
    }

    /// A function that returns a future that resolves in a result over an output, after the command
    /// was executed.
    pub fn async_exec(&self, command: String) -> impl Future<Output=Result<Output, Error>> {
        debug!("RemoteHandle: Building async_exec future to command: {}", command);
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
        commands: Vec<String>, 
        stdout_cb: Option<Box<dyn Fn(Vec<u8>)+Send+'static>>,
        stderr_cb: Option<Box<dyn Fn(Vec<u8>)+Send+'static>>) 
        -> impl Future<Output=Result<Output, Error>> {
        debug!("RemoteHandle: Building async_pty future to commands: {:?}", commands);
        let mut chan = self.sender.clone();
        let mut stdout_cb = stdout_cb;
        if let None = stdout_cb{
            stdout_cb = Some(Box::new(|_|{}));
        }
        let mut stderr_cb = stderr_cb;
        if let None = stderr_cb{
            stderr_cb = Some(Box::new(|_|{}));
        }
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("RemoteHandle::async_pty_future: Sending pty input");
            chan.send((sender, OperationInput::Pty(commands, stdout_cb.unwrap(), stderr_cb.unwrap())))
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


//--------------------------------------------------------------------------------------------- TEST


#[cfg(test)]
mod test {

    use super::*;
    use crate::ssh::config::SshProfile;

    fn init_logger() {
        std::env::set_var("RUST_LOG", "liborchestra::ssh=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_proxy_command_forwarder() {
        let (proxy_command, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut buf = [0 as u8; 5];
        stream.read(&mut buf).unwrap();
        assert_eq!(buf, "kikou".as_bytes());
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn test_async_exec() {
        use futures::executor::block_on;
        //init_logger();
        async fn connect_and_ls() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            let output = remote.async_exec("echo kikou && sleep 1 && { echo hello! 1>&2 }".into()).await.unwrap();
            println!("executed and resulted in {:?}", output);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "hello!\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(connect_and_ls());
    }

    #[test]
    fn test_async_pty() {
        use futures::executor::block_on;
        //init_logger();
        async fn connect_and_ls() {
            let profile = config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: Some(22),
                proxycommand: None,
            };
            let remote = RemoteHandle::spawn(profile).unwrap();
            let output = remote.async_pty(vec!["echo kikou".into(), 
                                               "sleep 1".into(), 
                                               "echo 'hello!' 1>&2".into()], 
                None, None).await.unwrap();
            println!("executed and resulted in {:?}", output);
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "kikou\n");
            assert_eq!(String::from_utf8(output.stderr).unwrap(), "hello!\n");
            assert_eq!(output.status.code().unwrap(), 0);
        }
        block_on(connect_and_ls());
    }

    #[test]
    fn test_async_scp_send() {
        use futures::executor::block_on;
        init_logger();
        async fn connect_and_scp_send() {
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
        block_on(connect_and_scp_send());
    }

    #[test]
    fn test_async_scp_fetch(){
        use futures::executor::block_on;
        init_logger();
        async fn connect_and_scp_fetch() {
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
        block_on(connect_and_scp_fetch());        
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
        async fn connect_and_ls(remote: RemoteHandle) -> std::process::Output{
            return remote.async_exec("echo 1 && sleep 1 && echo 2".into()).await.unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..50{
            handles.push(executor.spawn_with_handle(connect_and_ls(remote.clone())).unwrap())
        }
        let bef = std::time::Instant::now();
        for handle in handles{
            assert_eq!(String::from_utf8(executor.run(handle).stdout).unwrap(), "1\n2\n".to_owned());
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
        async fn connect_and_ls(remote: RemoteHandle) -> std::process::Output{
            return remote.async_pty(vec!["echo 1 && sleep 1 && echo 2".into()], None, None)
                .await.unwrap()
        }
        use futures::executor;
        use futures::task::SpawnExt;
        let mut executor = executor::ThreadPool::new().unwrap();
        let mut handles = Vec::new();
        for _ in 1..100{
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

