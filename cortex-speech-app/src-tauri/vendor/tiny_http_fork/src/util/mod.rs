mod custom_stream;
mod deadline_reader;
mod equal_reader;
mod fused_reader;

/// A legacy messages queue.
#[cfg(feature = "message_queue_legacy")]
mod messages_queue_legacy;

/// A message queue based on channel.
#[cfg(feature = "message_queue_channel")]
mod messages_queue_channel;

pub(crate) mod refined_tcp_stream;
mod sequential;

#[cfg(feature = "task_pool_legacy")]
mod task_pool_legacy;

#[cfg(feature = "task_pool_channel")]
mod task_pool_channel;

pub use self::custom_stream::CustomStream;
pub(crate) use self::deadline_reader::{DeadlineReader, REQUEST_READ_DEADLINE};
pub use self::equal_reader::EqualReader;
pub use self::fused_reader::FusedReader;

#[cfg(feature = "message_queue_channel")]
pub use self::messages_queue_channel::{MessageQueueSender, MessagesQueue};

#[cfg(feature = "message_queue_legacy")]
pub use self::messages_queue_legacy::{MessageQueueSender, MessagesQueue};

#[cfg(all(not(feature = "message_queue_channel"), not(feature = "message_queue_legacy")))]
compile_error!("select either message_queue_channel or message_queue_legacy");

#[cfg(all(feature = "message_queue_channel", feature = "message_queue_legacy"))]
compile_error!("select either message_queue_channel or message_queue_legacy");

pub use self::refined_tcp_stream::RefinedTcpStream;
pub use self::sequential::{SequentialReader, SequentialReaderBuilder};
pub use self::sequential::{SequentialWriter, SequentialWriterBuilder};

/// A legacy `taskpool` implementation.
#[cfg(feature = "task_pool_legacy")]
pub use self::task_pool_legacy::TaskPool;

/// A new `taskpool` implementation.
#[cfg(feature = "task_pool_channel")]
pub use self::task_pool_channel::TaskPool;

#[cfg(all(not(feature = "task_pool_legacy"), not(feature = "task_pool_channel")))]
compile_error!("select either task_pool_legacy or task_pool_channel");

#[cfg(all(feature = "task_pool_legacy", feature = "task_pool_channel"))]
compile_error!("select either task_pool_legacy or task_pool_channel");

use std::str::FromStr;

/// Parses a the value of a header.
/// Suitable for `Accept-*`, `TE`, etc.
///
/// For example with `text/plain, image/png; q=1.5` this function would
/// return `[ ("text/plain", 1.0), ("image/png", 1.5) ]`
pub fn parse_header_value(input: &str) -> Vec<(&str, f32)> {
    input
        .split(',')
        .filter_map(|elem| {
            let mut params = elem.split(';');

            let t = params.next()?;

            let mut value = 1.0_f32;

            for p in params {
                if p.trim_start().starts_with("q=") {
                    if let Ok(val) = f32::from_str(p.trim_start()[2..].trim()) {
                        value = val;
                        break;
                    }
                }
            }

            Some((t.trim(), value))
        })
        .collect()
}

#[cfg(test)]
mod test {
    #[test]
    #[allow(clippy::float_cmp)]
    fn test_parse_header() {
        let result = super::parse_header_value("text/html, text/plain; q=1.5 , image/png ; q=2.0");

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "text/html");
        assert_eq!(result[0].1, 1.0);
        assert_eq!(result[1].0, "text/plain");
        assert_eq!(result[1].1, 1.5);
        assert_eq!(result[2].0, "image/png");
        assert_eq!(result[2].1, 2.0);
    }
}
