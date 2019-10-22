
#![feature(trace_macros, async_await, result_map_or_else, trait_alias, try_blocks)]
//! liborchestra/mod.rs
//!
//! Liborchestra:
//! =============
//!  
//! Welcome to the developer documentation of liborchestra. Liborchestra gives tools to manipulate 
//! expegit repositories, run scripts on user-defined hosts, and orchestrate the executions of 
//! batches of experiments under variations of parameters.
//! 
//! Concepts around which the code is written:
//! + Experiment: refers to the code of the experiment. By extension, refers to the separate
//! repository in which the experiment code is managed.
//! + Execution: refers to the result of running the experiment once, for a given experiment
//! repository commit, parameter and machine. By extension, refers to the encompassing structure.
//! + Campaign: refers to an ensemble of executions. By extension, refers to the separate repository
//! in which the executions are stored.
//! + Running: refers to the act of running an experiment so as to obtain an execution.
//! 
//! On the use of asynchronous code:
//! --------------------------------
//! 
//! The program is mainly implemented in an asynchronous fashion. This means that most of the logic 
//! is implemented using [futures](https://rust-lang-nursery.github.io/futures-api-docs/0.3.0-alpha.18/futures/).
//! You can think of futures as small pieces of code logic, that could yield a thread to another task
//! when it would block on some evaluation. For example, if we want to execute a command on a remote 
//! host but don't want to block the thread for the time being, we can use the futures/async/await 
//! way.
//! 
//! As we write this line, the story of futures/async/await is still in genesis. Some of the features
//! making it possible are still only supported in nightly. You may find some informations about it 
//! in [the async-rust book](https://rust-lang.github.io/async-book/). If you never read or wrote 
//! asynchronous code before, you may well try to get intuition about it with some experiments and 
//! tutorials. If you already know about asynchronous code, and want to speed up, here are the 
//! important bits to sort the code out:
//!     + `Future` is a trait whose implementation contains a piece of logic of your code. This 
//!       piece of asynchronous execution is coded in the `poll` function of the trait. Put simply, 
//!       this function returns `Pending`, if the execution can't be concluded yet, and `Ready(...)` 
//!       if it is done.
//!     + A `Future` in itself, is pretty useless, it has to be spawned (as a _task_) on an 
//!       `Executor`, which will take care about driving the task to completion, by repetedly 
//!       calling `.poll()` on the different tasks it's responsible for (in fact not really, see the 
//!       last comment).
//!     + Since writing all your code in via `Future` trait implementation is a mess, the language 
//!       team implemented the `async` block feature that makes everything simpler.
//!     + Tagging a block or a function with an `async` keyword, will turn this block into a future. 
//!       Those blocks does not give you the ability to create your own future logic (for example 
//!       implementing an asynchronous io operation), but it does allow you to write a larger pieces 
//!       of code that uses multiple futures, with a classic coding style. When a future must be 
//!       polled until a result is given in an `async` block, it has to be made explicit to the 
//!       compiler by using the `.await` suffix (for example `my_file.read(some_buffer).await;`). 
//!     + An async block being itself a future, it can be `await`ed for in another future (async 
//!       blocks are composable).
//!     + Important: Executor won't actually call `.poll()` repetedly on every tasks. If a task 
//!       returns `Pending`, then the executor will _park_ the task until further notice. Every task 
//!       has a related `context` that can be used to ask the executor to _unpark_ the task and try 
//!       to poll it again. In general this context will be moved to a separate thread, that will 
//!       take care about checking whether or not a task must be awoken. The structure responsible 
//!       for waking up the executor at the right time is often called a reactor.
//! 
//! Futures-based concurrency for synchronous resources access:
//! -----------------------------------------------------------
//! 
//! The Futures-based asynchronous paradigm used in rust allows to run several concurrent tasks on a 
//! handful of threads. The most common use of this concurrency, is asynchronous i/o in io-bound 
//! applications. For this reason, most current tools are developed around this need, i.e. the 
//! `tokio` framework which provides asynchronous `TcpListeners`. 
//! 
//! This being said, concurrency can be applied to several other situations, for example the 
//! parallel execution of long-running processes on remote servers. Indeed, let's assume that one is 
//! interested in running 1000 scripts on a remote machine with an unlimited resources (all scripts 
//! can be run in parallel), but we only have access to say 8 local threads to launch the scripts 
//! and gather the outputs. Then two main logics can be used:
//!     + Every threads take a task and drive it to completion (starts the script, wait for it to 
//!       finish and gather data) beforemoving to a new task. In our case, we have at most 8 tasks 
//!       running at the same time.
//!     + Every threads perform as much work as possible on a task before switching to another task 
//!       that, can be advanced, before eventually coming back to the first to advance it further. 
//!       In this case, the concurrent number of tasks is not limited by the number of threads. 
//! The second approach is a perfect fit for Futures-based concurrency as existing in rust.
//! 
//! Still a conceptual barier avoids direct use of concurrency with every such problems. Indeed, we 
//! will probably want our task to use a singleton resource concurrently during their execution. For 
//! example, communicating with a remote server will involve a single ssh connection, which will be 
//! used by every tasks. In this case the futures can be used to synchronize the access of the 
//! different task to a single resource, pretty much like an mutex which would yield a thread if the
//! resource is already locked.  
//! 
//! This kind of use case is pervasive in the problems addressed by this library. For this reason, 
//! we had to wrap several synchronous resources into an asynchronous handle. This is the case of 
//! following structures:
//!     + `ssh::RemoteHandle` is a handle to a synchronous ssh remote connection.
//!     + `repository::CampaignHandle` is a handle to a synchronous campaign repository.
//!     + `application::ApplicationHandle` is a handle to a synchronous application managing several 
//!       remote executions.
//!     + `hosts::HostHandle` is a handle to a synchronous cluster connection.
//!     + `scheduler::SchedulerHandle` is a handle to a synchronous scheduler command running in 
//!       background.
//! 
//! Sync-To-Async design pattern:
//! -----------------------------
//! 
//! To implement those different resources, we used the same pattern consistently throughout the 
//! library code. Several patterns were tried before, but this one seems to be the best in terms of 
//! readability, simplicity and control. Though it could be made more generic through a set of neat 
//! traits, we have preferred, for now, to just implement it in the different places of the code. 
//! 
//! In what follows, we will distinguish two fundemental concepts. The __resource__, represents the 
//! synchronous structure we want to make availble to tasks. This one is probably not Clone, not 
//! Send and not Sync. On the other side, we aim at providing a __handle__ to this resource, that 
//! will be Clone+Send+Sync, and will basically propose the same api as the resource, but in an 
//! asynchronous manner. 
//! 
//! To do that, in a dedicated thread, we will instantiate the resource, along with a 
//! `LocalExecutor` which we will call the _inner_ executor. This executor only uses the current 
//! thread to execute the futures spawned onto it. In particular it will be used to poll a future 
//! (_Msg. Dispatch_ on the figure) which will handle all the messages that  come from the receiving 
//! end of the _operation submission channel_. The sending end of this channel will be used by the 
//! handle to submit __operations__ to be executed by the resource (inner api calls). For example, 
//! assuming the async function `func1(a: usize)` of the handle was spawned on the outer executor 
//! with the argument 1, then it will only send a message `InMsg::Func1(1)` to the _Msg. Dispatch_ 
//! task that runs in the inner executor. 
//! 
//! When the _Msg. Dispatch_ task receives this message, it will spawn the inner future 
//! `_func1(a: usize)`  with the argument provided in the message, along with the resource (denoted 
//! _res_ on the figure). This task will be spawned not on the outer executor, but on the inner one. 
//! Since the inner future are spawned on a single-threaded executor, this allows us to synchronize
//! the different operations over the resource. The following figure summarizes this design.
//! 
//!      
//!       +------------------------+                      +-----------------------------------+
//!       |     Outer Executor     |                      |            Inner Executor         |
//!       |  (nb. of threads > 1)  |                      |        (nb. of threads = 1)       |
//!       +------------------------+                      +-----------------------------------+
//!       |                        |                      |                                   |
//!       |   +----------------+   | InMsg::Func1(1)      |    +---------------+              |
//!       |   | async func1(1) |---+-----------------+----+--->| Msg. Dispatch |              |
//!       |   +----------------+   |                 |    |    +-+-------------+              |
//!       |                        |                 |    |      |                            |
//!       |   +---------------+    | InMsg::Func2    |    |      | +----------------------+   |
//!       |   | async func2() |----+-----------------+    |      * | async _func1(res, 1) |   |
//!       |   +---------------+    |                 |    |      | +----------------------+   |
//!       |                        |                 |    |      |                            |
//!       |   +----------------+   | InMsg::Func1(2) |    |      | +-------------------+      |
//!       |   | async func1(2) |---+-----------------+    |      * | async _func2(res) |      |
//!       |   +----------------+   |                      |      | +-------------------+      |
//!       |                        |                      |      |                            |
//!       |                        |                      |      | +----------------------+   |
//!       |                        |                      |      * | async _func1(res, 2) |   |
//!       |                        |                      |        +----------------------+   |
//!       |                        |                      |                                   |
//!       |                        |                      |                                   |
//!       +------------------------+                      +-----------------------------------+
//!       
//!
//! Now what about the results of those api calls ? Indeed, we would prefer our outer api to be able
//! to return values from the inner api. Actually, when the outer api sends a message to the inner 
//! api, not only the `InMsg` instance is sent, but the sending end of a channel is also packed in 
//! the message. The real message for the `func1(1)` call is `(channel::Sender<OutMsg>, InMsg)`. 
//! The receiving end of the channel will stay in the `func1(1)` scope, and will eventually be 
//! awaited for by the outer executor. This channel only serves the purpose of returning the result 
//! (_r_ in the figure) of the inner api call, but in the same time allows to yield the thread for 
//! the inner call duration thanks to the future-awareness of channels (indeed, every ends of 
//! `futures::channels` are awaitable). The following figure describes this patern.
//! 
//!     +------------------------+                         +-------------------------------+
//!     |     Outer Executor     |                         |         Inner Executor        |
//!     |  (nb. of threads > 1)  |                         |      (nb. of threads = 1)     |
//!     +------------------------+    (Sender<OutMsg>,     +-------------------------------+
//!     |                        |     InMsg::Func1(1))    |                               |
//!     |  +------------------+  |                         |  +---------------+            |
//!     |  |  async func1(1)  |--+-------------------------+->| Msg. Dispatch |            |
//!     |  +------------------+  |                         |  +-+-------------+            |
//!     |  | Receiver<OutMsg> |  |                         |    |                          |
//!     |  |                  |  |                         |    | +----------------------+ |
//!     |  +------------------+  |                         |    * | async _func1(res, 1) | |
//!     |           ^            |                         |      +---------------+------+ |
//!     |           |            |                         |                      |        |
//!     |           |            | OutMsg::Func1(Ok("ok")) |    +---------------+ |        |
//!     |           +------------+-------------------------+---.| async send(r) | *        |
//!     |                        |                         |    +---------------+          |
//!     |                        |                         |                               |
//!     |                        |                         |                               |
//!     |                        |                         |                               |
//!     |                        |                         |                               |
//!     |                        |                         |                               |
//!     |                        |                         |                               |
//!     +------------------------+                         +-------------------------------+
//!
//!  
//! Managing inner concurrency with awaitable locks:
//! ------------------------------------------------
//! 
//! Regarding the inner api, functions signatures are often of the type `Fn(Arc<Mutex<Res>>, ...) 
//! -> ...`, e.g. the resource is protected by a mutex. This mutex is future aware (comes from 
//! `futures::lock`) whicih means that the `lock()` function returns an awaitable future. Still, one
//! must take care about the following point. If one future yields while holding a lock, it will 
//! keep ownership of the lock. Consequently, other futures awaiting the lock, will not be awoken by
//! the executor. This may seem to be a burden at first, but in practice, it allows to control 
//! precisely the level of concurrency that we want to introduce between the different inner 
//! futures. 
//! 
//! For example, for ssh remote connections, different commands are running on separate channels of 
//! the connection (channels are multiplexed over the connection). Data may be available on one 
//! channel, and not the other one, so switching between tasks when one blocks (introducing 
//! concurrency) is the expected behavior. This can be achieved with locks, by taking care of always 
//! scoping every lock calls, to ensure that it will be released when the future blocks on an 
//! operation not involving the lock. Here is an hypothetical example where we copy the output of a 
//! channel on a second channel:
//! ```rust
//! let mut buf = [0 as u8; 1024];
//! loop{
//!     // Reading from channel 1 
//!     let out = {
//!         session.lock().await.get_channel(1).read(&mut buf)    // Lock acquired here
//!     };    // Lock released here
//!     match out {
//!         Err(e) if e.kind() == WouldBlock => { // logic to yield the thread };
//!         _ => {} 
//!     }
//! 
//!     // Writing to channel 2
//!     let out = {
//!         session.lock().await.get_channel(2).write(&mut buf) // Lock acquired here
//!     }; // Lock released here
//!     match out {
//!         Err(e) if e.kind() == WouldBlock => { // logic to yield the thread };
//!         _ => {} 
//!     }
//!     // Code goes on ...
//! } 
//! ```
//! As you can see, we take care about scoping every calls to the `lock()` method, to release lock 
//! before yielding the thread.. 
//! 
//! On the other side, some tasks may require to hold the lock for the whole time (blocking the 
//! other tasks). For example, the `scheduler::Scheduler` communicates with an external process 
//! via messaging through stdin and stdout. There is no multiplexing between those messages, and 
//! every communication has to happen without concurrency. Still, one read of the stdin may block, 
//! and we may want to yield the thread (for example to handle messages in the _Msg. Dispatch_ task)
//! . Thanks to awaitable locks, we can hold the lock inside of the task, as examplified here:
//! ```rust
//! let scheduler = scheduler.lock().await; // Lock is acquired here
//! let mut buf = [0 as u8; 1024] ;
//! loop {
//!     // Reading from stdout
//!     match scheduler.stdout.read(&mut buf) {
//!         Err(e) if e.kind() == WouldBlock => { 
//!             // logic to yield the thread. Lock is still acquired ! 
//!         };
//!         // rest of logic
//!     }
//! }
//! ``` 
//! Here, we can see that the lock stays acquired even if the read call yields the thread. This 
//! means that we ensure that the read will keep going until the message was completely received.
//! 
//! What is interesting with this pattern, is that we can mix both types of locking strategies. Some 
//! tasks may occur concurrently, but one may hold the lock for its complete lifespan.
//! 


//------------------------------------------------------------------------------------------ IMPORTS


extern crate regex;
extern crate yaml_rust;
extern crate env_logger;
extern crate uuid;
extern crate serde;
extern crate serde_yaml;
extern crate serde_json;
extern crate crypto;
extern crate rpassword;
extern crate ssh2;
extern crate dirs;
extern crate libc;
extern crate chrono;

#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;


//------------------------------------------------------------------------------------------ MODULES


pub mod ssh;
pub mod hosts;
pub mod repository;
pub mod primitives;
//pub mod application;
pub mod scheduler;
pub mod commons;

#[macro_use]
pub mod error;
#[macro_use]
pub mod misc;


//------------------------------------------------------------------------------------------ STATICS


/// The path to the script to launch in the experiment repo
pub static SCRIPT_RPATH: &str = "run";
/// globs pattern for files to ignore in send
pub static SEND_IGNORE_RPATH: &str = ".sendignore";
/// globs pattern for files to ignore in fetch
pub static FETCH_IGNORE_RPATH: &str = ".fetchignore";
/// folder containing execution profiles in $HOME
pub static PROFILES_FOLDER_RPATH: &str = ".orchestra";
/// file containing known hosts keys
pub static KNOWN_HOSTS_RPATH: &str = ".orchestra/known_hosts";
/// file containing ssh configs
pub static SSH_CONFIG_RPATH: &str = ".orchestra/config";
/// file name of tar archive to send
pub static SEND_ARCH_RPATH: &str = ".send.tar";
/// file name of tar to fetch
pub static FETCH_ARCH_RPATH: &str = ".fetch.tar";
/// folder containing the experiment repository as a submodule
pub static XPRP_RPATH: &str = "xprp";
/// folder containing the experiment executions
pub static EXCS_RPATH: &str = "excs";
/// file containing the execution parameters.
pub static EXCCONF_RPATH: &str = ".excconf";
/// folder containing the output data in an execution folder
pub static DATA_RPATH: &str = "data";
/// file containing expegit configuration
pub static CMPCONF_RPATH: &str = ".cmpconf";
/// file containing the execution features
pub static FEATURES_RPATH: &str = ".features";


//-------------------------------------------------------------------------------------------- ERROR


use error::Error;
