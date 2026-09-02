use crate::error::AppResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)) }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn check(&self) -> AppResult<()> {
        if self.is_cancelled() {
            Err(crate::error::AppError::Other("Operation cancelled".into()))
        } else {
            Ok(())
        }
    }

    pub fn child_token(&self) -> Self {
        Self { cancelled: self.cancelled.clone() }
    }

    /// True only for handles backed by the exact same cancellation flag. This lets an operation
    /// clear its own slot without erasing a newer operation's token after an async boundary.
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }

    /// The raw cancellation flag, for lower-level APIs (e.g. the WSL subprocess poller) that take an
    /// `&AtomicBool` directly rather than a `CancellationToken` — so an in-flight external call can be
    /// killed the instant Cancel is pressed, not only between calls.
    pub fn as_atomic(&self) -> &AtomicBool {
        &self.cancelled
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}
