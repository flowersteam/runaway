// liborchestra/async.rs
// Author: Alexandre Péré
///
/// This module provides a general worker structure, that allows to asynchronously perform tasks
/// on a shared queue of data.

// IMPORTS
use std::thread;
use std::time;
use std::fmt;
use std::sync::{Arc, Mutex};

// WORKER
/// Represents a unit of execution for asynchronous tasks. It will iteratively pick pieces of
/// data in a shared queue, and consume them with a given function.
///
/// Basic example
/// ```rust,notest
/// use std::thread;
/// use std::sync::{Arc, Mutex};
///
/// fn function(i: usize){
///     println!("Handling number {}", i);
///     thread::sleep_ms(1000);
/// }
///
/// let queue = Arc::new(Mutex::new(vec![1,2,3,4]));
/// let function_ptr: fn(usize) = function;
/// let workers: Vec<_> = (0..5).map(|x| Worker::new(queue.clone(), function_ptr)).collect();
/// ```
///
/// The worker will keep picking data in the queue and applying the function, until
/// it is specifically told to stop. This can be done by calling the `stop()` function. This
/// function will set an exit flag for the thread, which will exit its loop after finishing its
/// current operation.
#[derive(Debug)]
pub struct Worker {
    // Allows to retrieve the thread
    thread_handle: thread::JoinHandle<()>,
    // Allows to identify the worker
    thread_id: Arc<Mutex<thread::ThreadId>>,
    // Allows to describe the task currently executed
    task_description: Arc<Mutex<String>>,
    // Allows to stop the worker
    exit_flag: Arc<Mutex<bool>>,
    // Allows to check if worker is idle
    idle_flag: Arc<Mutex<bool>>,               }

impl Worker {
    /// Creates a new worker, given a queue on a given type and a function pointer.
    pub fn new<T>(queue: Arc<Mutex<Vec<T>>>, function: fn(T)) -> Worker
        where
            T: Send + 'static + fmt::Display,
    {
        let exit_flag = Arc::new(Mutex::new(false));
        let exit_flag_t = Arc::clone(&exit_flag);
        let idle_flag = Arc::new(Mutex::new(true));
        let idle_flag_t = Arc::clone(&idle_flag);
        let task_description = Arc::new(Mutex::new(String::new()));
        let task_description_t = Arc::clone(&task_description);
        let thread_id = Arc::new(Mutex::new(thread::current().id()));
        let thread_id_t = Arc::clone(&thread_id);
        let thread_handle = thread::spawn(move || {
            // In this closure, take care f the extra scopes, they allow to release the locks
            {
                *thread_id_t.lock().expect("Failed to lock thread_id") = thread::current().id();
            }
            loop {
                let do_exit = {
                    *exit_flag_t.lock().expect("Failed to lock exit_flag")
                };
                if do_exit {
                    break;
                } else {
                    let queue_element = {
                        let mut queue = queue.lock().expect("Failed to lock queue");
                        queue.pop()
                    };
                    if let Some(element) = queue_element {
                        {
                            *idle_flag_t.lock().expect("Failed to lock idle_flag") = false;
                        }
                        {
                            *task_description_t.lock().expect("Failed to lock task_description") = format!("{}", element);
                        }
                        function(element);
                    } else {
                        *idle_flag_t.lock().expect("Failed to lock idle_flag") = true;
                    }
                }
                thread::sleep(time::Duration::from_millis(5));
            }
        });
        Worker {
            thread_handle,
            thread_id,
            task_description,
            exit_flag,
            idle_flag,
        }
    }

    /// Stops the worker after its current operation. Note that this function blocks the execution until
    /// the worker is stopped.
    pub fn stop(self) {
        *self.exit_flag.lock().unwrap() = true;
        self.thread_handle.join().unwrap();
    }

    /// Checks if the worker is idle, or performing a task.
    pub fn is_idle(&self) -> bool {
        *self.idle_flag.lock().unwrap()
    }
}

impl fmt::Display for Worker {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let thread_id = { *self.thread_id.lock().unwrap() };
        let mut thread_string = format!("{:?}", thread_id);
        thread_string.retain(|c| c.is_numeric());
        if self.is_idle() {
            write!(f, "Worker⟦{}⟧ Idle", thread_string)
        } else {
            let description = { self.task_description.lock().unwrap() };
            write!(f, "Worker⟦{}⟧ Running on {}", thread_string, description)
        }
    }
}

// TESTS
#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_worker() {
        let queue = Arc::new(Mutex::new(vec![1]));
        let queue_w = Arc::clone(&queue);
        fn function(x: usize) {
            println!("Hello {}", x);
            thread::sleep_ms(2000);
        }
        let function_p: fn(usize) = function;
        let mut worker = Worker::new(queue_w, function_p);
        thread::sleep_ms(2);
        assert!(!worker.is_idle());
        thread::sleep_ms(3000);
        assert!(worker.is_idle());
        worker.stop();
        assert_eq!(*queue.lock().unwrap(), vec![] as Vec<usize>);
        let workers: Vec<_> = (1..6)
            .map(|x| Worker::new(queue.clone(), function_p))
            .collect();
        thread::sleep_ms(2000);
        assert_eq!(queue.lock().unwrap().len(), 0);
        println!("{:?}", queue);
    }
}
