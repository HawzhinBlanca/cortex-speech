use std::{
    io::{self, ErrorKind},
    time::{Duration, Instant},
};

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender};

#[derive(Debug)]
enum Control<T> {
    Elem(T),
    Unblock,
}

#[derive(Debug)]
pub struct MessageQueueSender<T>
where
    T: Send,
{
    queue: Sender<Control<T>>,
}

impl<T: Send> Clone for MessageQueueSender<T> {
    fn clone(&self) -> Self {
        Self { queue: self.queue.clone() }
    }
}

impl<T> MessageQueueSender<T>
where
    T: Send,
{
    /// Pushes an element to the queue.
    pub fn push(&self, value: T) -> io::Result<()> {
        self.queue
            .send(Control::Elem(value))
            .map_err(|_e| io::Error::new(ErrorKind::BrokenPipe, "channel (receiver) disconnected"))
    }
}

#[derive(Debug)]
pub struct MessagesQueue<T>
where
    T: Send,
{
    queue: Receiver<Control<T>>,
    s: Sender<Control<T>>,
}

impl<T> MessagesQueue<T>
where
    T: Send,
{
    pub fn with_capacity(_capacity: usize) -> MessagesQueue<T> {
        let (sender, recv) = crossbeam::channel::unbounded::<Control<T>>();

        MessagesQueue { queue: recv, s: sender }
    }

    /// Creates a message injector.
    pub fn create_sender(&self) -> MessageQueueSender<T> {
        MessageQueueSender { queue: self.s.clone() }
    }

    /// Unblock one thread stuck in pop loop.
    pub fn unblock(&self) -> io::Result<()> {
        self.s.send(Control::Unblock).map_err(|_e| io::Error::new(ErrorKind::BrokenPipe, "channel disconnected"))
    }

    /// Pops an element. Blocks until one is available.
    /// Returns None in case unblock() was issued.
    pub fn pop(&self) -> io::Result<Option<T>> {
        let msg = self.queue.recv().map_err(|_e| io::Error::new(ErrorKind::BrokenPipe, "channel disconnected"))?;

        match msg {
            Control::Elem(value) => return Ok(Some(value)),
            Control::Unblock => return Ok(None),
        }
    }

    /// Tries to pop an element without blocking.
    pub fn try_pop(&self) -> io::Result<Option<T>> {
        self.pop_timeout(Duration::from_nanos(1))
    }

    /// Tries to pop an element without blocking
    /// more than the specified timeout duration
    /// or unblock() was issued
    pub fn pop_timeout(&self, timeout: Duration) -> io::Result<Option<T>> {
        match self.queue.recv_timeout(timeout) {
            Ok(Control::Elem(value)) => return Ok(Some(value)),
            Ok(Control::Unblock) => return Ok(None),
            Err(RecvTimeoutError::Timeout) => return Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(ErrorKind::BrokenPipe, "channel disconnected"));
            }
        }
    }
}
