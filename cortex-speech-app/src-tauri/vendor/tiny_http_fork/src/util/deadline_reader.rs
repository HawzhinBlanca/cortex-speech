use std::io::{Error, ErrorKind, Read, Result};
use std::time::{Duration, Instant};

/// Maximum time a peer may spend producing one request head or body.
///
/// The socket also carries this as an idle timeout, while [`DeadlineReader`]
/// makes the bound absolute: trickling one byte before every socket timeout
/// must not hold a server worker forever.
pub(crate) const REQUEST_READ_DEADLINE: Duration = Duration::from_secs(10);

/// Maximum time one response write may make NO progress before the connection is dropped.
///
/// The read side was bounded first; the write side was not, and it is the half a phone can stall
/// most easily: request a 500 KB clip, then stop reading. The socket buffer fills, `write` blocks,
/// and — with one server thread per reviewer — a handful of stalled phones takes the whole review
/// server down with nothing in the UI to say why.
///
/// This is `SO_SNDTIMEO`, so it fires only when the peer has acknowledged NOTHING for the whole
/// window; a genuinely slow cellular download keeps making progress and never trips it. Longer than
/// the read deadline because bytes-in-flight legitimately take longer than parsing a request head.
pub(crate) const RESPONSE_WRITE_DEADLINE: Duration = Duration::from_secs(30);

/// A reader with an absolute wall-clock deadline.
pub(crate) struct DeadlineReader<R> {
    inner: R,
    timeout: Duration,
    deadline: Option<Instant>,
}

impl<R> DeadlineReader<R> {
    pub(crate) fn new(inner: R, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            // Start when the application first reads (or when Drop first drains), not while
            // the Request is waiting in the server queue.
            deadline: None,
        }
    }

    #[cfg(test)]
    fn with_deadline(inner: R, deadline: Instant) -> Self {
        Self { inner, timeout: Duration::ZERO, deadline: Some(deadline) }
    }
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let deadline = *self.deadline.get_or_insert_with(|| Instant::now() + self.timeout);
        if Instant::now() >= deadline {
            return Err(Error::new(ErrorKind::TimedOut, "request read deadline exceeded"));
        }

        let read = self.inner.read(buf)?;
        if Instant::now() >= deadline {
            return Err(Error::new(ErrorKind::TimedOut, "request read deadline exceeded"));
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::DeadlineReader;
    use std::io::{Cursor, ErrorKind, Read};
    use std::time::{Duration, Instant};

    #[test]
    fn expired_reader_fails_without_touching_the_source() {
        let source = Cursor::new(b"body".to_vec());
        let mut reader = DeadlineReader::with_deadline(source, Instant::now() - Duration::from_millis(1));
        let mut output = Vec::new();
        let error = reader.read_to_end(&mut output).expect_err("expired request body must time out");
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(output.is_empty());
    }
}
