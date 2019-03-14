// liborchestra/ssh.rs
// Author: Alexandre Péré
///
/// This module contains a structure that wraps the ssh2 session object into some facilities to
/// handle ssh configurations, authentications, and proxy-commands. In particular, since libssh2
/// does not provide any ways to handle proxy-commands, we use a threaded copy loop that open a
/// proxycommand as a subprocess and copy the output on a random tcp socket. 

// IMPORTS
use ssh2::{Session, KnownHosts, KnownHostFileKind, CheckResult, MethodType, KnownHostKeyFormat};
use std::net::{TcpListener, TcpStream, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::io::prelude::*;
use std::process::{Stdio, Command};
use std::time::{Instant, Duration};
use std::error;
use std::fmt;
use std::env;
use std::path::Path;
use std::collections::HashSet;
use std::process::Output;
use dirs;
use super::{PROFILES_FOLDER_RPATH, KNOWN_HOSTS_RPATH};

// ERRORS
#[derive(Debug)]
pub enum Error{
    ProxySocketNotFound,
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
    Unknown,
}

impl error::Error for Error{
}

impl fmt::Display for Error{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self{
            ProxySocketNotFound => write!(f, "Failed to find a socket to bind the proxy command on."),
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
        }
    } 
}

// PROXYCOMMAND
/// This structure allows to start a proxy command which is forwarded on a tcp socket. This allows
/// a libssh2 session to get connected through a proxy command. On the first connection to the
/// socket, the proxycommand will be started in a subprocess, and the reads from the socket will be
/// copied to the process stdin, and the stdout from the process will be written to the socket. The 
/// forthcoming connections are rejected. The messages get forwarded as long as the forwarder stays 
/// in scope. The forwarding is explicitly stopped when the forwarder is dropped. 
pub struct ProxyCommandForwarder{
    keep_forwarding: Arc<AtomicBool>,
    handle: Option<JoinHandle<(JoinHandle<()>, JoinHandle<()>)>>,
}

impl ProxyCommandForwarder{ 
    /// Creates a new `ProxyCommandForwarder` from a command. An available port is automatically
    /// given by the OS, and is returned along with the forwarder. 
    pub fn from_command(command: &str) -> Result<(ProxyCommandForwarder, SocketAddr), Error>{
        let stream = match std::net::TcpListener::bind("127.0.0.1:0"){
            Ok(s) => s,
            Err(_) => return Err(Error::ProxySocketNotFound)
        };
        let address = stream.local_addr().unwrap();
        let mut keep_forwarding = Arc::new(AtomicBool::new(true));
        let mut kf = keep_forwarding.clone();
        let command = command.to_owned();
        let handle = std::thread::spawn(move ||{
            let (mut socket, addr) = stream.accept().unwrap();
            let mut socket1 = socket.try_clone().unwrap();
            let mut socket2 = socket.try_clone().unwrap();
            let mut kf1 = kf.clone();
            let mut kf2 = kf.clone();
            let mut cmd = command.split_whitespace().collect::<Vec<_>>();
            let args = cmd.split_off(1);
            let cmd = cmd.pop().unwrap();
            let mut command = Command::new(cmd)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut command_stdin = command.stdin.take().unwrap();
            let mut command_stdout = command.stdout.take().unwrap();
            let h1 = std::thread::spawn(move ||{
                while kf1.load(Ordering::Relaxed){
                    std::io::copy(&mut command_stdout, &mut socket1);
                }
            });
            let h2 = std::thread::spawn(move ||{
                while kf2.load(Ordering::Relaxed){
                    std::io::copy(&mut socket2, &mut command_stdin);
                }
            });
            return (h1, h2);
        });
        return Ok((ProxyCommandForwarder{keep_forwarding, handle: Some(handle)}, address));
    }
}

impl Drop for ProxyCommandForwarder{
    fn drop(&mut self) {
        self.keep_forwarding.store(false, Ordering::Relaxed);
        let handle = self.handle.take().unwrap();
        let (h1, h2) = handle.join().unwrap();
        h1.join();
        h2.join();
    }
}

// REMOTE CONNECTION

pub struct RemoteConnection{
    stream: TcpStream,
    session: Session,
    proxy_command: Option<ProxyCommandForwarder>,
}

impl RemoteConnection {
   pub fn from_addr(addr: impl ToSocketAddrs,
                    host: &str,
                    user: &str) -> Result<RemoteConnection, Error> {
        let stream = TcpStream::connect(addr).map_err(|_| Error::ConnectionFailed)?;
        stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        stream.set_write_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut session = Session::new().unwrap();        
        session.method_pref(MethodType::HostKey, "ssh-rsa")
            .map_err(|_| Error::PreferenceSettingFailed)?;
        session.handshake(&stream).map_err(|_| Error::HandshakeFailed)?;
        {   
            let mut known_hosts = session.known_hosts().unwrap();
            let mut known_hosts_path = dirs::home_dir().ok_or(Error::HomeNotFound)?;
            known_hosts_path.push(PROFILES_FOLDER_RPATH);
            known_hosts_path.push(KNOWN_HOSTS_RPATH);
            known_hosts.read_file(known_hosts_path.as_path(), 
                                  KnownHostFileKind::OpenSSH)
                .map_err(|_|Error::KnownHostsUnreachable)?;
            let (key, key_type) = session.host_key().ok_or(Error::Unknown)?;
            match known_hosts.check(host, key){
                CheckResult::Match => {},
                CheckResult::Mismatch => return Err(Error::HostKeyMismatch),
                CheckResult::NotFound => {
                    known_hosts.add(host, 
                                    key, 
                                    "", 
                                    KnownHostKeyFormat::SshRsa)
                        .map_err(|_| Error::CouldntRegisterHost)?;
                    known_hosts.write_file(known_hosts_path.as_path(), KnownHostFileKind::OpenSSH)
                        .map_err(|_| Error::CouldntSaveKnownHosts)?;
                },
                CheckResult::Failure => return Err(Error::HostCheckFailed),
            }  
            let mut  agent = session.agent().unwrap();
            agent.connect().map_err(|_| Error::SshAgentUnavailable)?;
            agent.list_identities().map_err(|_| Error::Unknown)?;
            let ids = agent.identities()
                .into_iter()
                .map(|id| id.unwrap().comment().to_owned())
                .collect::<HashSet<_>>();
            let failed_ids = agent.identities()
                .into_iter()
                .take_while( |id| {
                    agent.userauth(user, id.as_ref().unwrap()).is_err()
                })
                .map(|id| {id.unwrap().comment().to_owned()})
                .collect::<HashSet<_>>();
            if ids == failed_ids {
                return Err(Error::ClientAuthFailed);
            }
        }
        return Ok(RemoteConnection{stream, session, proxy_command: None});   
        }

   pub fn from_proxy_command(host: &str, user: &str, command: &str) 
       -> Result<RemoteConnection, Error>{
           let (proxy_command, addr) = ProxyCommandForwarder::from_command(command)?;
           let mut remote = RemoteConnection::from_addr(addr, host, user)?;    
           remote.proxy_command.replace(proxy_command);
           return Ok(remote);
       }
}


#[cfg(test)]
mod test {
    use super::*;
    
    #[test]
    fn test_proxy_command_forwarder(){
        let (pxc, address) = ProxyCommandForwarder::from_command("echo kikou").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut buf = [0 as u8;5];
        stream.read(&mut buf);
        assert_eq!(buf, "kikou".as_bytes());
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn test_remote_from_addr(){
        let remote = RemoteConnection::from_addr("127.0.0.1:22", "localhost", "apere").unwrap();
    } 
    
    #[test]
    fn test_remote_from_proxy_command(){
        let remote = RemoteConnection::from_proxy_command("localhost", "apere", "ssh -A -l apere localhost -W localhost:22").unwrap();
    }
}

