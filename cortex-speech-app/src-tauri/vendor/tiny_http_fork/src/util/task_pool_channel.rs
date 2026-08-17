use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fmt, io};

use crossbeam::channel::{Receiver, Sender, unbounded};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::alarm;

use crate::log;

static COMPLETED_TASKS: AtomicUsize = AtomicUsize::new(0);
static TASKS_PER_SECOND: AtomicUsize = AtomicUsize::new(0);
static SPAWNED_THREADS: AtomicUsize = AtomicUsize::new(0);

static MIN_THREADS_VAL: OnceLock<usize> = OnceLock::new();
static MAX_THREADS_VAL: OnceLock<usize> = OnceLock::new();

/// A struct for the task thread.
#[derive(Debug)]
struct TaskThread {
    /// Name of the thread.
    thr_id: usize,

    /// A shared access to the thread pool for managment.
    inner: Arc<Mutex<TaskPoolInner>>,

    /// A sender endpoint of channel.
    task_send_ch: Sender<TaskTask>,

    /// A receiver which is polled.
    recv_ch: Receiver<TaskTask>,

    /// A exit reason trap.
    tec: TaskExitCatcher,
}

impl TaskThread {
    fn thread(mut self) {
        loop {
            let task = if self.thr_id >= *MIN_THREADS_VAL.get().unwrap() {
                let Ok(task) = self.recv_ch.recv_timeout(Duration::from_secs(5)) else {
                    break;
                };

                task
            } else {
                let Ok(task) = self.recv_ch.recv() else {
                    break;
                };

                task
            };

            match task {
                TaskTask::Exec(mut fn_mut) => {
                    (fn_mut)();

                    COMPLETED_TASKS.fetch_add(1, Ordering::SeqCst);
                }
                TaskTask::ExitReason(TaskExitReason::Ok(thr_no)) => {
                    let cur_thr = thread::current();

                    log::info!("[{}] thread #{} exited", cur_thr.name().unwrap_or("UNKNOWN"), thr_no);
                    let mut task_pool_lock = self.inner.lock().unwrap();

                    let Some(task_handle) = task_pool_lock.thread_pool.get_mut(thr_no).unwrap().take() else {
                        continue;
                    };

                    drop(task_pool_lock);

                    task_handle.take_result();
                }
                TaskTask::ExitReason(TaskExitReason::Panic(thr_no)) => {
                    let cur_thr = thread::current();

                    // restart the thread
                    let mut task_pool_lock = self.inner.lock().unwrap();

                    let task_handle =
                        match task_pool_lock.add_thread(self.inner.clone(), self.tec.block_panic.clone(), thr_no) {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!(
                                    "[{}] thread no: {} creation error: {}. Stalled thread.",
                                    cur_thr.name().unwrap_or("UNKNOWN"),
                                    thr_no,
                                    e
                                );
                                continue;
                            }
                        };

                    let Some(prev_task_hndl) = task_pool_lock.thread_pool.get_mut(thr_no).unwrap().replace(task_handle)
                    else {
                        continue;
                    };

                    drop(task_pool_lock);

                    prev_task_hndl.take_result();
                }
                TaskTask::ExitAll => {
                    let _ = self.task_send_ch.send(TaskTask::ExitAll);

                    break;
                }
            }
        }

        self.tec.exit_ok();
    }
}

/// An inner struct which is shared across threads.
#[derive(Debug)]
struct TaskPoolInner {
    /// A pool of the threads.
    thread_pool: Vec<Option<TaskHandle>>,

    /// A sender endpoint.
    task_send_ch: Sender<TaskTask>,

    /// A receiver endpoint of the above.
    task_recv_ch: Receiver<TaskTask>,
}

impl TaskPoolInner {
    fn add_thread(
        &self,
        task_pool_inner: Arc<Mutex<TaskPoolInner>>,
        block_panic: Arc<AtomicBool>,
        thr_no: usize,
    ) -> io::Result<TaskHandle> {
        let recv_ch = self.task_recv_ch.clone();
        let tec = TaskExitCatcher::new(self.task_send_ch.clone(), thr_no, block_panic);

        let thread_ctx = TaskThread {
            thr_id: thr_no,
            inner: task_pool_inner,
            task_send_ch: self.task_send_ch.clone(),
            recv_ch,
            tec,
        };

        let thb = thread::Builder::new().name(format!("task-{}s", thr_no));

        let handler = thb.spawn(move || TaskThread::thread(thread_ctx))?;

        SPAWNED_THREADS.fetch_add(1, Ordering::SeqCst);

        return Ok(TaskHandle { handler });
    }

    /// Counts the threads which [TaskHandle] is `some` and thread is `not finished`.
    fn count_live_threads(&self) -> usize {
        self.thread_pool
            .iter()
            .filter(|v| v.is_some() == true && v.as_ref().unwrap().handler.is_finished() == false)
            .count()
    }

    /// Counts the threads which [TaskHandle] is `none` or thread is `finished`.
    fn count_completed_threads(&self) -> usize {
        self.thread_pool
            .iter()
            .filter(|v| v.is_none() == true || v.as_ref().unwrap().handler.is_finished() == true)
            .count()
    }

    fn find_completed_thread_or_unalloc(&self) -> Option<usize> {
        self.thread_pool.iter().enumerate().find(|(_pos, c)| c.is_none()).map(|(pos, _)| pos).map_or_else(
            || {
                let pos = self.thread_pool.len();
                if pos == self.thread_pool.capacity() {
                    return None;
                }

                return Some(pos);
            },
            |f| Some(f),
        )
    }
}

/// A protocol which is used for thread control.
enum TaskTask {
    /// Exec task.
    Exec(Box<dyn FnMut() + Send + 'static>),

    /// Thread exited.
    ExitReason(TaskExitReason),

    /// All threads should end.
    ExitAll,
}

impl fmt::Debug for TaskTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exec(_arg0) => f.debug_tuple("Exec").finish(),
            Self::ExitReason(arg0) => f.debug_tuple("ExitReason").field(arg0).finish(),
            Self::ExitAll => f.debug_tuple("ExitAll").finish(),
        }
    }
}

/// The reason task has been completed.
#[derive(Debug)]
enum TaskExitReason {
    /// Task ended normally.
    ///
    /// * `0` - task number (in [TaskPoolInner])
    Ok(usize),

    /// Exit reason is panic.
    ///
    /// * `0` - task number (in [TaskPoolInner])
    Panic(usize),
}

/// Catches the reason the thread exited.
#[derive(Debug)]
struct TaskExitCatcher {
    /// Is set to true when task pool is destructed.
    block_panic: Arc<AtomicBool>,

    /// A sender endpoint for a reason transmission.
    send: Sender<TaskTask>,

    /// A thread numer which is assigend in [TaskPoolInner]
    thread_no: usize,

    /// A flag which indicates that thread exited normally.
    exit_ok: bool,
}

impl Drop for TaskExitCatcher {
    fn drop(&mut self) {
        SPAWNED_THREADS.fetch_sub(1, Ordering::SeqCst);

        if self.exit_ok == true || self.block_panic.load(Ordering::SeqCst) == true {
            let _ = self.send.send(TaskTask::ExitReason(TaskExitReason::Ok(self.thread_no)));
        } else {
            let _ = self.send.send(TaskTask::ExitReason(TaskExitReason::Panic(self.thread_no)));
        }
    }
}

impl TaskExitCatcher {
    fn new(send: Sender<TaskTask>, thread_no: usize, block_panic: Arc<AtomicBool>) -> Self {
        Self { send, thread_no, exit_ok: false, block_panic }
    }

    fn exit_ok(&mut self) {
        self.exit_ok = true;
    }
}

#[derive(Debug)]
struct TaskHandle {
    handler: JoinHandle<()>,
}

impl TaskHandle {
    fn take_result(self) {
        let thread_name = self.handler.thread().name().map_or("unknown".to_string(), |n| n.to_string());

        match self.handler.join() {
            Ok(_) => {
                log::debug!("thread '{}' exited", thread_name);
            }
            Err(e) => {
                log::error!("thread '{}' exit with error: {:?}", thread_name, e);
            }
        }
    }
}

/// SIGNAL_ALARM handler. Rings every second.
extern "C" fn signal_alarm(_: nix::libc::c_int) {
    let tasks_cnt = COMPLETED_TASKS.swap(0, Ordering::SeqCst);

    TASKS_PER_SECOND.store(tasks_cnt, Ordering::Release);

    // restart alarm
    alarm::set(1);
}

/// Manages a collection of threads.
///
/// A new thread is created every time all the existing threads are full.
/// Any idle thread will automatically die after a few seconds.
#[derive(Debug)]
pub struct TaskPool {
    /// The shared inner which threads' handles and channel endpoints.
    inner: Arc<Mutex<TaskPoolInner>>,

    /// A task sender.
    task_submit: Sender<TaskTask>,

    /// When `taskpool` is destruc
    block_panic: Arc<AtomicBool>,
}

impl TaskPool {
    /// Minimum number of active threads.
    pub const MIN_THREADS: usize = 4;

    /// Maximum number of active threads.
    pub const MAX_THREADS: usize = 128;

    pub fn new(min_threads_opt: Option<usize>, max_threads_opt: Option<usize>) -> io::Result<TaskPool> {
        let sa = SigAction::new(SigHandler::Handler(signal_alarm), SaFlags::SA_RESTART, SigSet::empty());

        unsafe {
            sigaction(Signal::SIGALRM, &sa).unwrap();
        }

        let min_threads = *MIN_THREADS_VAL.get_or_init(|| min_threads_opt.unwrap_or(Self::MIN_THREADS));
        let max_threads = *MAX_THREADS_VAL.get_or_init(|| max_threads_opt.unwrap_or(Self::MAX_THREADS));

        let (send, recv) = unbounded::<TaskTask>();
        let task_submit = send.clone();

        let block_panic: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        let pool_inner = Arc::new(Mutex::new(TaskPoolInner {
            thread_pool: Vec::with_capacity(max_threads),
            task_send_ch: send,
            task_recv_ch: recv,
        }));

        let pool = Self { inner: pool_inner, task_submit: task_submit, block_panic: block_panic.clone() };

        let mut pool_lock = pool.inner.lock().unwrap();

        for i in 0..min_threads {
            let handle = pool_lock.add_thread(pool.inner.clone(), block_panic.clone(), i)?;

            pool_lock.thread_pool.push(Some(handle));
        }

        drop(pool_lock);

        // start alarm
        alarm::set(1);

        return Ok(pool);
    }

    /// Executes a function in a thread.
    /// If no thread is available, spawns a new one.
    pub fn spawn(&self, code: Box<dyn FnMut() + Send>) -> io::Result<()> {
        if let Err(_) = self.task_submit.send(TaskTask::Exec(code)) {
            return Err(io::Error::new(ErrorKind::BrokenPipe, format!("submit_task disconnected")));
        }

        // check pressure (todo)
        // i.e check how many messages are in the queue.
        // and what is the thoughput i.e how many tasks were completed

        let thr_cnt = SPAWNED_THREADS.load(Ordering::SeqCst);

        //log::debug!("debug: spawned threads: {}, tps: {}", thr_cnt, TASKS_PER_SECOND.load(Ordering::Acquire));

        let max_threads = *MAX_THREADS_VAL.get().unwrap();

        if thr_cnt < max_threads {
            let tsk_cnt = self.task_submit.len();
            let tsk_ps = TASKS_PER_SECOND.load(Ordering::Acquire);

            if tsk_ps < tsk_cnt {
                let spawn_more = (tsk_cnt - tsk_ps) / thr_cnt / (max_threads - thr_cnt);

                if spawn_more == 0 {
                    return Ok(());
                }

                log::info!(
                    "more {} threads are required to clear the queue len: {}, tasks_per_sec: {}",
                    spawn_more,
                    tsk_cnt,
                    tsk_ps
                );

                let thr_max =
                    if thr_cnt + spawn_more > max_threads { thr_cnt + spawn_more - max_threads } else { spawn_more };

                log::info!("spawning {} threads count: {}, max threads {}", thr_max, thr_cnt, max_threads);

                let mut pool_lock = self.inner.lock().unwrap();

                for _ in 0..thr_max {
                    let Some(thr_idx) = pool_lock.find_completed_thread_or_unalloc() else {
                        log::error!(
                            "cannot spawn thread, cannot find unallocated or finished, spawned_thrs: {}, list len: {}",
                            thr_cnt,
                            pool_lock.thread_pool.len()
                        );

                        return Ok(());
                    };

                    let handle = pool_lock.add_thread(self.inner.clone(), self.block_panic.clone(), thr_idx)?;

                    pool_lock.thread_pool.push(Some(handle));
                }
            }
        }

        return Ok(());
    }

    fn stop_all_internal(&mut self) -> io::Result<()> {
        self.block_panic.store(true, Ordering::SeqCst);

        return self.task_submit.send(TaskTask::ExitAll).map_err(|_e| {
            io::Error::new(ErrorKind::BrokenPipe, "TaskPool::drop() task_bumit send error, channel closed")
        });
    }

    pub fn stop_wait_all(mut self) -> io::Result<()> {
        self.stop_wait_all_timeout_internal(60)
    }

    pub fn stop_wait_all_timeout(mut self, timeout_sec: u32) -> io::Result<()> {
        self.stop_wait_all_timeout_internal(timeout_sec)
    }

    pub fn stop_wait_all_timeout_internal(&mut self, timeout_sec: u32) -> io::Result<()> {
        self.stop_all_internal()?;

        // wait for tasks
        for i in 0..timeout_sec {
            std::thread::sleep(Duration::from_millis(999));

            let task_pool_lock = self.inner.lock().unwrap();

            log::debug!(
                "drop() itration: {}, count: {}, len: {}",
                i,
                task_pool_lock.count_completed_threads(),
                task_pool_lock.thread_pool.len()
            );
            if task_pool_lock.count_completed_threads() == task_pool_lock.thread_pool.len() {
                break;
            }

            drop(task_pool_lock);
        }

        let mut task_pool_lock = self.inner.lock().unwrap();
        for task in task_pool_lock.thread_pool.iter_mut().filter(|p| p.is_some() == true).map(|p| p.take().unwrap()) {
            if task.handler.is_finished() == true {
                task.take_result();
            } else {
                log::error!("stalled thread: {}", task.handler.thread().name().unwrap_or("UNKNOWN"));
            }
        }

        return Ok(());
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        if let Err(e) = self.stop_wait_all_timeout_internal(10) {
            log::error!("TaskPool::drop() error: {}", e);

            return;
        }
    }
}

#[cfg(test)]
mod test_task_pool {
    use std::time::{Duration, Instant};

    use super::*;

    fn test_function() {
        println!("[{}] function started", std::thread::current().name().unwrap_or("UNKNOWN"));

        let timeout = rand::random_range(1..2000);
        std::thread::sleep(Duration::from_millis(timeout));

        println!("[{}] function completed", std::thread::current().name().unwrap_or("UNKNOWN"));
    }

    fn test_function_panic() {
        println!("[{}] function started", std::thread::current().name().unwrap_or("UNKNOWN"));

        panic!("paniking");
    }

    fn test_function_long() {
        println!("[{}] function started", std::thread::current().name().unwrap_or("UNKNOWN"));

        let timeout = rand::random_range(5..1000);
        std::thread::sleep(Duration::from_millis(timeout));

        println!("[{}] function completed", std::thread::current().name().unwrap_or("UNKNOWN"));
    }

    #[test]
    fn test_simple() {
        let taskpool = TaskPool::new(Some(5), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        taskpool.spawn(Box::new(test_function)).unwrap();
        taskpool.spawn(Box::new(test_function)).unwrap();

        drop(taskpool);
    }

    #[test]
    fn test_simple_thread_destruct() {
        let taskpool = TaskPool::new(Some(1), Some(2)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..50 {
            let timeout = rand::random_range(1..200);
            std::thread::sleep(Duration::from_millis(timeout));

            taskpool.spawn(Box::new(test_function_long)).unwrap();
        }

        std::thread::sleep(Duration::from_secs(15));

        taskpool.stop_wait_all().unwrap();
    }

    #[test]
    fn test_simple_thread_panic() {
        let taskpool = TaskPool::new(Some(5), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        taskpool.spawn(Box::new(test_function)).unwrap();
        taskpool.spawn(Box::new(test_function_panic)).unwrap();

        taskpool.stop_wait_all().unwrap();
    }

    #[test]
    fn test_simple_thread_panic2() {
        let taskpool = TaskPool::new(Some(2), Some(2)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        taskpool.spawn(Box::new(test_function)).unwrap();
        taskpool.spawn(Box::new(test_function_panic)).unwrap();

        std::thread::sleep(Duration::from_secs(1));
        taskpool.spawn(Box::new(test_function)).unwrap();

        taskpool.stop_wait_all().unwrap();
    }

    #[test]
    fn test_low_load() {
        let taskpool = TaskPool::new(Some(5), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..10 {
            taskpool.spawn(Box::new(test_function)).unwrap();
        }

        drop(taskpool);
    }

    #[test]
    fn test_low_load_single_thread() {
        let taskpool = TaskPool::new(Some(1), Some(1)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..10 {
            taskpool.spawn(Box::new(test_function)).unwrap();
        }

        drop(taskpool);
    }

    #[test]
    fn test_low_load_wait_all() {
        let taskpool = TaskPool::new(Some(5), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..10 {
            taskpool.spawn(Box::new(test_function)).unwrap();
        }

        taskpool.stop_wait_all().unwrap();
    }

    #[test]
    fn test_low_load_low_threads() {
        let taskpool = TaskPool::new(Some(1), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..50 {
            let timeout = rand::random_range(1..200);
            std::thread::sleep(Duration::from_millis(timeout));

            taskpool.spawn(Box::new(test_function_long)).unwrap();
        }

        taskpool.stop_wait_all().unwrap();
    }

    #[test]
    fn test_med_load() {
        let taskpool = TaskPool::new(Some(5), Some(8)).unwrap();

        assert_eq!(taskpool.block_panic.load(Ordering::Relaxed), false);

        for _ in 0..40 {
            let s = Instant::now();
            taskpool.spawn(Box::new(test_function)).unwrap();
            let e = s.elapsed();
            println!("elaspsed: {:?}", e);

            let timeout = rand::random_range(500..1100);
            std::thread::sleep(Duration::from_micros(timeout));
        }

        drop(taskpool);
    }
}
