use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::{io, thread};

fn try_increment_below(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

/// Manages a collection of threads.
///
/// A new thread is created every time all the existing threads are full.
/// Any idle thread will automatically die after a few seconds.
pub struct TaskPool {
    sharing: Arc<Sharing>,

    // min number of threads
    min_threads: usize,

    // hard ceiling for live worker threads
    max_threads: usize,
}

struct Sharing {
    // list of the tasks to be done by worker threads
    todo: Mutex<VecDeque<Box<dyn FnMut() + Send>>>,

    // condvar that will be notified whenever a task is added to `todo`
    condvar: Condvar,

    // number of total worker threads running
    active_tasks: AtomicUsize,

    // number of idle worker threads
    waiting_tasks: AtomicUsize,

    // number of workers currently executing a task
    running_tasks: AtomicUsize,

    // number of allocated worker threads. Reserved before spawning so a
    // connection burst cannot race past max_threads.
    thread_count: AtomicUsize,
}

struct Registration<'a> {
    nb: &'a AtomicUsize,
}

impl<'a> Registration<'a> {
    fn new(nb: &'a AtomicUsize) -> Registration<'a> {
        nb.fetch_add(1, Ordering::Release);
        Registration { nb }
    }
}

impl<'a> Drop for Registration<'a> {
    fn drop(&mut self) {
        self.nb.fetch_sub(1, Ordering::Release);
    }
}

struct ThreadRegistration {
    sharing: Arc<Sharing>,
}

impl Drop for ThreadRegistration {
    fn drop(&mut self) {
        self.sharing.thread_count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TaskPool {
    /// Minimum number of active threads.
    const MIN_THREADS: usize = 4;
    /// Default hard ceiling, matching the channel task-pool implementation.
    const MAX_THREADS: usize = 128;

    pub fn new(min_threads_opt: Option<usize>, max_threads_opt: Option<usize>) -> io::Result<TaskPool> {
        let min_threads = min_threads_opt.unwrap_or(Self::MIN_THREADS);
        let max_threads = max_threads_opt.unwrap_or(Self::MAX_THREADS).max(min_threads).max(1);

        let pool = TaskPool {
            sharing: Arc::new(Sharing {
                todo: Mutex::new(VecDeque::new()),
                condvar: Condvar::new(),
                active_tasks: AtomicUsize::new(0),
                waiting_tasks: AtomicUsize::new(0),
                running_tasks: AtomicUsize::new(0),
                thread_count: AtomicUsize::new(0),
            }),
            min_threads,
            max_threads,
        };

        for _ in 0..min_threads {
            pool.add_thread(None)?;
        }

        return Ok(pool);
    }

    /// Executes a function in a thread.
    /// If no thread is available, spawns one up to max_threads. Once every
    /// worker is occupied, returns WouldBlock so the caller can close the new
    /// connection instead of retaining unbounded sockets, tasks, or threads.
    pub fn spawn(&self, code: Box<dyn FnMut() + Send>) -> io::Result<()> {
        let mut queue = self.sharing.todo.lock().unwrap();

        // A reserved-but-not-yet-started minimum worker is available too, so
        // derive capacity from reserved threads minus workers actually running.
        // Comparing that capacity with queue.len() keeps the queue bounded even
        // while notified workers are still waking.
        let available = self
            .sharing
            .thread_count
            .load(Ordering::Acquire)
            .saturating_sub(self.sharing.running_tasks.load(Ordering::Acquire));
        if available > queue.len() {
            queue.push_back(code);
            self.sharing.condvar.notify_one();
            return Ok(());
        }

        drop(queue);
        self.add_thread(Some(code))?;

        return Ok(());
    }

    fn add_thread(&self, initial_fn: Option<Box<dyn FnMut() + Send>>) -> io::Result<()> {
        if !try_increment_below(&self.sharing.thread_count, self.max_threads) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "task pool worker limit reached"));
        }

        let sharing = self.sharing.clone();
        let rollback = self.sharing.clone();
        let min_threads = self.min_threads;

        let spawn = thread::Builder::new().name("tiny-http-client".to_string()).spawn(move || {
            let sharing = sharing;
            let _thread_guard = ThreadRegistration { sharing: sharing.clone() };
            let _active_guard = Registration::new(&sharing.active_tasks);

            if let Some(mut f) = initial_fn {
                let _running_guard = Registration::new(&sharing.running_tasks);
                f();
            }

            loop {
                let mut task: Box<dyn FnMut() + Send> = {
                    let mut todo = sharing.todo.lock().unwrap();

                    let task;
                    loop {
                        if let Some(poped_task) = todo.pop_front() {
                            task = poped_task;
                            break;
                        }
                        let _waiting_guard = Registration::new(&sharing.waiting_tasks);

                        let received = if sharing.active_tasks.load(Ordering::Acquire) <= min_threads {
                            todo = sharing.condvar.wait(todo).unwrap();
                            true
                        } else {
                            let (new_lock, waitres) =
                                sharing.condvar.wait_timeout(todo, Duration::from_millis(5000)).unwrap();
                            todo = new_lock;
                            !waitres.timed_out()
                        };

                        if !received && todo.is_empty() {
                            return;
                        }
                    }

                    task
                };

                let _running_guard = Registration::new(&sharing.running_tasks);
                task();
            }
        });

        match spawn {
            Ok(_) => Ok(()),
            Err(error) => {
                rollback.thread_count.fetch_sub(1, Ordering::AcqRel);
                Err(error)
            }
        }
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.sharing.active_tasks.store(999_999_999, Ordering::Release);
        self.sharing.condvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::TaskPool;
    use std::io::ErrorKind;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn worker_limit_rejects_excess_tasks_instead_of_spawning_unbounded_threads() {
        let pool = TaskPool::new(Some(1), Some(1)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        pool.spawn(Box::new(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }))
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).expect("first worker starts");

        let error = pool
            .spawn(Box::new(|| panic!("a saturated pool must not execute this task")))
            .expect_err("the configured worker ceiling must be enforced");
        assert_eq!(error.kind(), ErrorKind::WouldBlock);

        release_tx.send(()).unwrap();
    }
}
