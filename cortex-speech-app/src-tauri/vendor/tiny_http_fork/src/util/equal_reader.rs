use std::io::Read;
use std::io::Result as IoResult;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};

/// A `Reader` that reads exactly the number of bytes from a sub-reader.
///
/// If the limit is reached, it returns EOF. If the limit is not reached
/// when the destructor is called, the remaining bytes will be read and
/// thrown away.
pub struct EqualReader<R>
where
    R: Read,
{
    reader: R,
    size: usize,
    last_read_signal: Sender<IoResult<()>>,
}

impl<R> EqualReader<R>
where
    R: Read,
{
    pub fn new(reader: R, size: usize) -> (EqualReader<R>, Receiver<IoResult<()>>) {
        let (tx, rx) = channel();

        let r = EqualReader { reader, size, last_read_signal: tx };

        (r, rx)
    }
}

impl<R> Read for EqualReader<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.size == 0 {
            return Ok(0);
        }

        let buf = if buf.len() < self.size { buf } else { &mut buf[..self.size] };

        match self.reader.read(buf) {
            Ok(len) => {
                self.size -= len;
                Ok(len)
            }
            err @ Err(_) => err,
        }
    }
}

impl<R> Drop for EqualReader<R>
where
    R: Read,
{
    fn drop(&mut self) {
        let mut remaining_to_read = self.size;
        // Never size an allocation from an unauthenticated Content-Length header. A peer can declare
        // usize::MAX and close without sending a body; allocating `remaining_to_read` here aborts the
        // host process before the application can enforce its own body limit. Drain incrementally
        // through a fixed stack buffer instead.
        let mut buf = [0_u8; 8 * 1024];

        while remaining_to_read > 0 {
            let chunk_len = remaining_to_read.min(buf.len());
            match self.reader.read(&mut buf[..chunk_len]) {
                Err(e) => {
                    self.last_read_signal.send(Err(e)).ok();
                    break;
                }
                Ok(0) => {
                    self.last_read_signal.send(Ok(())).ok();
                    break;
                }
                Ok(other) => {
                    remaining_to_read -= other;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EqualReader;
    use std::io::Read;

    #[test]
    fn test_limit() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut string = String::new();
            equal_reader.read_to_string(&mut string).unwrap();
            assert_eq!(string, "hello");
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_not_enough() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut vec = [0];
            equal_reader.read_exact(&mut vec).unwrap();
            assert_eq!(vec[0], b'h');
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn drop_drains_a_huge_declared_body_with_a_bounded_buffer() {
        struct RecordingReader {
            remaining: usize,
            largest_buffer: usize,
        }

        impl Read for RecordingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.largest_buffer = self.largest_buffer.max(buf.len());
                let read = self.remaining.min(buf.len());
                self.remaining -= read;
                Ok(read)
            }
        }

        let mut reader = RecordingReader { remaining: 100_000, largest_buffer: 0 };
        {
            let (_equal_reader, _) = EqualReader::new(&mut reader, usize::MAX);
        }
        assert_eq!(reader.remaining, 0);
        assert!(reader.largest_buffer <= 8 * 1024);
    }
}
