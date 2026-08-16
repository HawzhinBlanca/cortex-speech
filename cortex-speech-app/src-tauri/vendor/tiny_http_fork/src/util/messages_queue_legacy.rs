use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
enum Control<T> {
    Elem(T),
    Unblock,
}

#[derive(Debug)]
struct MessagesQueueInner<T>
where
    T: Send,
{
    queue: Mutex<VecDeque<Control<T>>>,
    condvar: Condvar,
}

#[derive(Debug)]
pub struct MessageQueueSender<T>
where
    T: Send,
{
    msg_queue: Arc<MessagesQueueInner<T>>,
}

impl<T> Clone for MessageQueueSender<T>
where
    T: Send,
{
    fn clone(&self) -> Self {
        Self { msg_queue: self.msg_queue.clone() }
    }
}

impl<T> MessageQueueSender<T>
where
    T: Send,
{
    /// Pushes an element to the queue.
    pub fn push(&self, value: T) -> io::Result<()> {
        let mut queue = self.msg_queue.queue.lock().unwrap();
        queue.push_back(Control::Elem(value));
        self.msg_queue.condvar.notify_one();

        return Ok(());
    }
}

#[derive(Debug, Clone)]
pub struct MessagesQueue<T>
where
    T: Send,
{
    msg_queue: Arc<MessagesQueueInner<T>>,
}

impl<T> MessagesQueue<T>
where
    T: Send,
{
    pub fn with_capacity(capacity: usize) -> MessagesQueue<T> {
        let msg_queue = Arc::new(MessagesQueueInner {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            condvar: Condvar::new(),
        });

        return Self { msg_queue: msg_queue };
    }

    /// Creates a message injector.
    pub fn create_sender(&self) -> MessageQueueSender<T> {
        MessageQueueSender { msg_queue: self.msg_queue.clone() }
    }

    /// Unblock one thread stuck in pop loop.
    pub fn unblock(&self) -> io::Result<()> {
        let mut queue = self.msg_queue.queue.lock().unwrap();
        queue.push_back(Control::Unblock);
        self.msg_queue.condvar.notify_one();

        return Ok(());
    }

    /// Pops an element. Blocks until one is available.
    /// Returns None in case unblock() was issued.
    pub fn pop(&self) -> io::Result<Option<T>> {
        let mut queue = self.msg_queue.queue.lock().unwrap();

        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => return Ok(Some(value)),
                Some(Control::Unblock) => return Ok(None),
                None => (),
            }

            queue = self.msg_queue.condvar.wait(queue).unwrap();
        }
    }

    /// Tries to pop an element without blocking.
    pub fn try_pop(&self) -> io::Result<Option<T>> {
        let mut queue = self.msg_queue.queue.lock().unwrap();

        match queue.pop_front() {
            Some(Control::Elem(value)) => return Ok(Some(value)),
            Some(Control::Unblock) | None => return Ok(None),
        }
    }

    /// Tries to pop an element without blocking
    /// more than the specified timeout duration
    /// or unblock() was issued
    pub fn pop_timeout(&self, timeout: Duration) -> io::Result<Option<T>> {
        let mut queue = self.msg_queue.queue.lock().unwrap();
        let mut duration = timeout;
        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => return Ok(Some(value)),
                Some(Control::Unblock) => return Ok(None),
                None => (),
            }
            let now = Instant::now();
            let (_queue, result) = self.msg_queue.condvar.wait_timeout(queue, timeout).unwrap();
            queue = _queue;
            let sleep_time = now.elapsed();
            duration = if duration > sleep_time { duration - sleep_time } else { Duration::from_millis(0) };

            if result.timed_out() || (duration.as_secs() == 0 && duration.subsec_nanos() < 1_000_000) {
                return Ok(None);
            }
        }
    }
}
