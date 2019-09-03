//! liborchestra/scheduler.rs
//! Author: Alexandre Péré
//!
//! This module contains a structure that allows to use an external command as a scheduler. A 
//! scheduler is a program that will provide experiments parameters on request, based on the results
//! of previous parameters execution. Schedulers are meant to implement automatic experiment 
//! selection such as bayesian optimization, exploration, and so on.
//! 
//! The communication between the scheduler and the library will use stdin and stdout. 
//! Communications will be initiated by the library, which will send a request over stdin. The 
//! command will treat this request and answer with a response on stdout. Request should be treated 
//! synchronously by the command, and no particular order of requests should be assumed (any 
//! necessary book-keeping must be done on the command side).


//------------------------------------------------------------------------------------------ IMPORTS


use crate::primitives::Dropper;
use futures::Future;
use std::{error, fmt, str};
use std::io::{Read, Write};
use futures::channel::{mpsc, oneshot};
use futures::executor;
use futures::future;
use futures::task::LocalSpawnExt;
use futures::FutureExt;
use std::thread;
use futures::channel::mpsc::{UnboundedSender};
use std::fmt::{Display, Debug};
use std::process::Command;
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json;


//----------------------------------------------------------------------------------------- MESSAGES


#[derive(Serialize, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
/// This enumeration represents the different request messages that can be sent to the command. Those 
/// requests will be serialized to the following jsons when sent to the command stdin:
pub enum RequestMessages{
    /// Example of json transcript: `{"GET_PARAMETERS_REQUEST": {}}`
    GetParametersRequest{},
    /// Example of json transcript: `{"RECORD_OUTPUT_REQUEST": {"parametes": "some params", 
    /// "features": [0.5, 0.5] }}`
    RecordOutputRequest{ parameters: String, features: Vec<f64>},
    // Example of json transcript: `{"SHUTDOWN_REQUEST": {}}`
    ShutdownRequest{},
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
/// This enumeration represents the different request messages that can be sent by the command. Those 
/// requests are expected to be serialized to json using the following templates:
pub enum ResponseMessages{
    /// Example of json transcript: `{"GET_PARAMETERS_RESPONSE": {"parameters": "some params"}}`
    GetParametersResponse{ parameters: String },
    /// Example of json transcript: `{"RECORD_OUTPUT_RESPONSE": {}}`
    RecordOutputResponse{},
    /// Example of json transcript: `{"SHUTDOWN_RESPONSE": {}}`
    ShutdownResponse{},
    /// Example of json transcript: `{"ERROR_RESPONSE": {"message": "some error message" }}`
    ErrorResponse{message: String}
}


//------------------------------------------------------------------------------------------- MACROS


/// This macro allows to send a particular request to the scheduler, and retrieve the output
#[macro_export]
macro_rules! query_command {
    ($sched: expr, $req: expr ) => {
            { 
                // We send the message
                let mut message = format!("{}\n", serde_json::to_string($req).unwrap())
                    .as_bytes()
                    .to_owned();
                loop {
                    match await_wouldblock_io!({$sched.stdin.write(&message)}){
                        Ok(0) => break,
                        Ok(b) => {message = message[b..].to_owned()},
                        Err(e) => return Err(Error::Query(format!("{}", e)))
                    }            
                }

                // We retrieve the response from the command
                let mut output = vec!();
                let mut buf = [0 as u8; 1024];
                loop {
                    match await_wouldblock_io!({$sched.stdout.read(&mut buf)}){
                        Ok(0) => break,
                        Ok(b) => {
                            output.append(&mut buf[..b].to_vec());
                            if output.contains(&b'\n'){break} 
                        },
                        Err(e) => return Err(Error::Query(format!("{}", e)))
                    }
                }
            
                // We parse the answer
                let output = String::from_utf8(output).unwrap();
                serde_json::from_str(output.as_str())
                    .map_err(|e| Error::Message(format!("Failed to parse message: \n{}", e)))
            }
    };
}


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    Query(String),
    Message(String),
    Command(String),
    Channel(String),
    OperationFetch(String),
    Spawn(String),
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Query(ref s) => write!(f, "Failed to query the scheduler: \n{}", s),
            Error::Message(ref s) => write!(f, "Unexpected message: \n{}", s),
            Error::Command(ref s) => write!(f, "Command returned an error: \n{}", s),
            Error::Channel(ref s) => write!(f, "Channel error: \n{}", s),
            Error::OperationFetch(ref s) => write!(f, "When fetching operation result: \n{}", s),
            Error::Spawn(ref s) => write!(f, "Error occurred when spawning the command: \n {}", s)
        }
    }
}


//--------------------------------------------------------------------------------------------- HOST


use std::sync::Arc;
use futures::lock::Mutex;
use futures::SinkExt;
use futures::StreamExt;
use std::process::{Child, ChildStdin, ChildStdout};

/// A `Scheduler` represents an instance of a running process which implement an automatic scheduling 
/// logic, and which can be communicated with through the json messaging exposed earlier via stdin
/// and stdout. The child process handles to stdin and stdout are turned into non-blocking ones, 
/// to allow the tasks to yield whenever needed.
struct Scheduler {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Scheduler {

    /// Generates a scheduler from a child process spawned elswhere. Basically, this makes the stdin 
    /// and stdout file descriptors non blocking.
    fn from_child(child: Child) -> Result<Scheduler, Error> {
        debug!("Scheduler: Creating Sheduler from child: {:?}", child);
        let mut child = child;
        let stdin = unblock(child.stdin.take().unwrap())
            .map_err(|e| Error::Spawn(format!("Failed to unblock stdin: \n{}", e)))?;
        let stdout = unblock(child.stdout.take().unwrap())
            .map_err(|e| Error::Spawn(format!("Failed to unblock stdout: \n{}", e)))?;
        return Ok(Scheduler {
            child,
            stdin,
            stdout,
        });
    }

    /// Inner future containing the logic to request parameters. 
    async fn request_parameters(sched: Arc<Mutex<Scheduler>>) -> Result<String, Error>{
        debug!("Scheduler: Requesting parameters");
        {   
            // We bind the command to this scope. Such that if one of the io blocks, we are sure that 
            // an other task doesn't get woken up before this one, and reads/write the end of the
            // messages meant for this task. Note, that this is different from the ssh connection 
            // case where io ocurs on the same connection, but separate channels.
            let mut sched = sched.lock().await;
 
            // We query the command
            let request = RequestMessages::GetParametersRequest{};
            match query_command!(sched, &request)?{
                ResponseMessages::GetParametersResponse{parameters: p} => return Ok(p),
                m => return Err(Error::Message(format!("Unexpected message received {:?}", m)))
            }
        }
    }

    /// Inner future containing the logic to record an output. 
    async fn record_output(sched: Arc<Mutex<Scheduler>>, parameters: String, features: Vec<f64>) -> Result<(), Error>{
        debug!("Scheduler: Recording output");
        {   
            // We bind the command to this scope. Such that if one of the io blocks, we are sure that 
            // an other task doesn't get woken up before this one, and reads/write the end of the
            // messages meant for this task. Note, that this is different from the ssh connection 
            // case where io ocurs on the same connection, but separate channels.
            let mut sched = sched.lock().await;
 
            // We query the command
            let request = RequestMessages::RecordOutputRequest{parameters, features};
            match query_command!(sched, &request)?{
                ResponseMessages::RecordOutputResponse{} => return Ok(()),
                m => return Err(Error::Message(format!("Unexpected message received {:?}", m)))
            }
        }
    }

    /// Inner future containing the logic to shutdown the command.
    async fn shutdown(sched: Arc<Mutex<Scheduler>>) -> Result<(), Error>{
        debug!("Scheduler: Shutting scheduler down");
        {   
            // We bind the command to this scope. Such that if one of the io blocks, we are sure that 
            // an other task doesn't get woken up before this one, and reads/write the end of the
            // messages meant for this task. Note, that this is different from the ssh connection 
            // case where io ocurs on the same connection, but separate channels.
            let mut sched = sched.lock().await;
 
            // We query the command 
            let request = RequestMessages::ShutdownRequest{};
            match query_command!(sched, &request)?{
                ResponseMessages::ShutdownResponse{} => {},
                m => return Err(Error::Message(format!("Unexpected message received {:?}", m)))
            }

            // We wait for the child to close. No need to yield the thread, since there should be no
            // other tasks left.
            sched.child.wait().unwrap();

            Ok(())
        }
    }
}

//------------------------------------------------------------------------------------------- HANDLE

#[derive(Debug)]
/// Messages sent by the outer future to the resource inner thread, so as to start an operation. 
/// This contains the input of the operation if any.
enum OperationInput{
    RequestParameters,
    RecordOutput(String, Vec<f64>),
}

#[derive(Debug)]
/// Messages sent by the inner future to the outer future, so as to return the result of an 
/// operation.
enum OperationOutput{
    RequestParameters(Result<String, Error>),
    RecordOutput(Result<(), Error>),
}

#[derive(Clone)]
/// An asynchronous handle to the scheduler resource.
pub struct SchedulerHandle {
    _sender: UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    _name: String,
    _dropper: Dropper,
}

impl SchedulerHandle {

    /// Spawns a `Scheduler` from a command. This function contains most of the logic concerning the 
    /// dispatch of the operations to the inner futures. 
    pub fn spawn(command: Command, name: String) -> Result<SchedulerHandle, Error> {

        debug!("SchedulerHandle: Start scheduler thread");

        // We create the scheduler resource. This one will be transferred into a separate thread.
        let mut command = command;
        let sched = Scheduler::from_child(
            command.spawn()
                .map_err(|e| Error::Spawn(format!("{}", e)))?
            )?;
        // We create the channel that will be used to transmit operations from the outer logic (when 
        // the user call one of the async api methods) to the inner handling thread.
        let (sender, receiver) = mpsc::unbounded();

        // We spawn the thread that dispatches the operations sent by the different handles to inner 
        // futures.
        let handle = thread::Builder::new().name(format!("orch-sched"))
        .spawn(move || {
            trace!("Scheduler Thread: Creating resource in thread");
            let res = Arc::new(Mutex::new(sched));
            let reres = res.clone();

            // We spawn the local executor, in charge of executing the inner tasks
            trace!("Scheduler Thread: Starting resource loop");
            let mut pool = executor::LocalPool::new();
            let mut spawner = pool.spawner();

            // We describe the message dispatching task
            let handling_stream = receiver.for_each(
                move |(sender, operation): (oneshot::Sender<OperationOutput>, OperationInput)| {
                    trace!("Scheduler Thread: received operation {:?}", operation);
                    match operation {
                        OperationInput::RequestParameters => {
                            spawner.spawn_local(
                                Scheduler::request_parameters(res.clone())
                                    .map(|a| {
                                        sender.send(OperationOutput::RequestParameters(a))
                                            .map_err(|e| error!("Scheduler Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                        OperationInput::RecordOutput(parameters, features) => {
                            spawner.spawn_local(
                                Scheduler::record_output(res.clone(), parameters, features)
                                    .map(|a| {
                                        sender.send(OperationOutput::RecordOutput(a))
                                            .map_err(|e| error!("Scheduler Thread: Failed to \\
                                            send an operation output: \n{:?}", e))
                                            .unwrap();
                                    })
                            )
                        }
                    }.map_err(|e| error!("Scheduler Thread: Failed to spawn the operation: \n{:?}", e))
                    .unwrap();
                    future::ready(())
                }
            );

            // We spawn the message dispatching task
            let mut spawner = pool.spawner();
            spawner.spawn_local(handling_stream)
                .map_err(|_| error!("Scheduler Thread: Failed to spawn handling stream"))
                .unwrap();

            // We wait for every tasks to complete (the last will be the message dispatching task
            // that will return when the channel closes)
            trace!("Scheduler Thread: Starting local executor.");
            pool.run();

            // All the tasks are done, we shutdown the resource.
            trace!("Scheduler Thread: All futures processed. Shutting command down.");
            executor::block_on(Scheduler::shutdown(reres.clone()))
                .unwrap_or_else(|e| error!("Scheduler: Failed to shutdown scheduler: \n {}", e));
            trace!("Scheduler Thread: All good. Leaving...");
        }).expect("Failed to spawn scheduler thread.");

        // We return the handle
        let drop_sender = sender.clone();
        Ok(SchedulerHandle {
            _sender: sender,
            _name: name,
            _dropper: Dropper::from_closure(
                Box::new(move || {
                    drop_sender.close_channel();
                    handle.join().unwrap();
                }), 
                format!("SchedulerHandle")
            ),
        })
    }

    /// Async method, returning a future that ultimately resolves in a parameter string coming from 
    /// the scheduler.
    pub fn async_request_parameters(&self) -> impl Future<Output=Result<String,Error>> {
        debug!("SchedulerHandle: Building async_request_parameters future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("SchedulerHandle::async_request_parameters_future: Sending input");
            chan.send((sender, OperationInput::RequestParameters))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("SchedulerHandle::async_request_parameters_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::RequestParameters(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected RequestParameters, found {:?}", e)))
            }
        }
    }

    /// Async method, returning a future that ultimately resolves after the output was recorded.
    pub fn async_record_output(&self, parameters: String, output: Vec<f64>) -> impl Future<Output=Result<(),Error>> {
        debug!("SchedulerHandle: Building async_record_output future");
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("SchedulerHandle::async_record_output_future: Sending input");
            chan.send((sender, OperationInput::RecordOutput(parameters, output)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("SchedulerHandle::async_record_output_future: Awaiting output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::RecordOutput(res)) => res,
                Ok(e) => Err(Error::OperationFetch(format!("Expected RecordOutput, found {:?}", e)))
            }
        }
    }
}

impl Debug for SchedulerHandle{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "SchedulerHandle<{:?}>", self._name)
    }
}

impl Display for SchedulerHandle{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "{}", self._name)
    }
}


//------------------------------------------------------------------------------------------ UNBLOCK


use libc;
use std::io;
use std::os::unix::io::AsRawFd;

// From tokio-process
fn unblock<T>(io: T) -> Result<T, io::Error>
where
    T: AsRawFd,
{
    // Set the fd to nonblocking before we pass it to the event loop
    unsafe {
        let fd = io.as_raw_fd();
        let r = libc::fcntl(fd, libc::F_GETFL);
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        let r = libc::fcntl(fd, libc::F_SETFL, r | libc::O_NONBLOCK);
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(io)
}



//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod test {

    use super::*;
    use env_logger;

    fn init() {
        
        std::env::set_var("RUST_LOG", "liborchestra::scheduler=trace,liborchestra::primitives=trace");
        let _ = env_logger::builder().is_test(true).try_init();
    }

    fn write_python_scheduler() {
        let program = "#!/usr/bin/env python
import json 
import sys

if __name__ == \"__main__\":
    while True:
        inpt = json.loads(input())
        if \"GET_PARAMETERS_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python received GET_PARAMETERS_REQUEST\\n\")
            print(json.dumps({\"GET_PARAMETERS_RESPONSE\": {\"parameters\": \"params_from_python\"}}))
            sys.stderr.write(\"Python sent GET_PARAMETERS_RESPONSE\")
        elif \"RECORD_OUTPUT_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python received RECORD_OUTPUT_REQUEST\\n\")
            if inpt[\"RECORD_OUTPUT_REQUEST\"][\"parameters\"] != \"params_from_rust\": raise Exception()
            if inpt[\"RECORD_OUTPUT_REQUEST\"][\"features\"] != [1.5]: raise Exception()
            sys.stdout.write(json.dumps({\"RECORD_OUTPUT_RESPONSE\": {}}))
            sys.stdout.write(\"\\n\")
        elif \"SHUTDOWN_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python rceived SHUTDOWN_REQUEST\\n\")
            sys.stdout.write(json.dumps({\"SHUTDOWN_RESPONSE\": {}}))
            sys.stdout.write(\"\\n\")
            break
        else:
            raise Exception(\"Unknown Message\")
";
        let mut file = std::fs::File::create("/tmp/scheduler.py").unwrap();
        file.write_all(program.as_bytes()).unwrap();
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
        file.flush().unwrap();
    }

    fn write_failing_python_scheduler() {
        let program = "#!/usr/bin/env python
import json 
import sys

if __name__ == \"__main__\":
    while True:
        inpt = json.loads(input())
        if \"GET_PARAMETERS_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python received GET_PARAMETERS_REQUEST\\n\")
            print(json.dumps({\"ERROR_RESPONSE\": {\"message\": \"error_from_python\"}}))
            sys.stderr.write(\"Python sent GET_PARAMETERS_RESPONSE\")
        elif \"RECORD_OUTPUT_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python received RECORD_OUTPUT_REQUEST\\n\")
            if inpt[\"RECORD_OUTPUT_REQUEST\"][\"parameters\"] != \"params_from_rust\": raise Exception()
            if inpt[\"RECORD_OUTPUT_REQUEST\"][\"features\"] != [1.5]: raise Exception()
            print(json.dumps({\"ERROR_RESPONSE\": {\"message\": \"error_from_python\"}}))
        elif \"SHUTDOWN_REQUEST\" in inpt.keys():
            sys.stderr.write(\"Python rceived SHUTDOWN_REQUEST\\n\")
            sys.stdout.write(json.dumps({\"SHUTDOWN_RESPONSE\": {}}))
            break
        else:
            raise Exception(\"Unknown Message\")
";
        let mut file = std::fs::File::create("/tmp/scheduler.py").unwrap();
        file.write_all(program.as_bytes()).unwrap();
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
        file.flush().unwrap();
    }

    
    #[test]
    fn test_scheduler_resource() {
        use futures::executor::block_on;

        init();
        
        write_python_scheduler();

        let mut command = std::process::Command::new("/tmp/scheduler.py");
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::inherit());
        let scheduler = SchedulerHandle::spawn(command, "scheduler.py".into()).unwrap();

        let parameters = block_on(scheduler.async_request_parameters()).unwrap();
        assert_eq!(parameters, format!("params_from_python"));

        block_on(scheduler.async_record_output("params_from_rust".into(), vec![1.5])).unwrap();

        drop(scheduler);

        write_failing_python_scheduler();

        let mut command = std::process::Command::new("/tmp/scheduler.py");
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::inherit());
        let scheduler = SchedulerHandle::spawn(command, "scheduler.py".into()).unwrap();

        block_on(scheduler.async_request_parameters()).unwrap_err();

        block_on(scheduler.async_record_output("params_from_rust".into(), vec![1.5])).unwrap_err();


    }

}
