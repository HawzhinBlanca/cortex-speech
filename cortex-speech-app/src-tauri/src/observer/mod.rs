use serde::{Deserialize, Serialize};
use std::sync::mpsc;

/// Event emitted during pipeline processing.
/// Used for real-time progress reporting via Tauri events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineProgress {
    Started { total: usize },
    Progress { current: usize, total: usize, file: String, status: String },
    FileCompleted { file: String, result: String },
    FileFailed { file: String, error: String },
    Completed { total: usize, succeeded: usize, failed: usize },
}

/// Observer for pipeline progress events.
/// Can be used with an MPSC channel for sync code
/// or with Tauri events for async frontend communication.
pub trait ProgressObserver: Send + 'static {
    fn on_event(&self, event: &PipelineProgress);
}

/// Channel-based observer that forwards events to a Tauri app handle.
pub struct TauriEventObserver {
    app_handle: tauri::AppHandle,
}

impl TauriEventObserver {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }
}

impl ProgressObserver for TauriEventObserver {
    fn on_event(&self, event: &PipelineProgress) {
        use tauri::Emitter;
        let event_name = match event {
            PipelineProgress::Started { .. } => "pipeline-started",
            PipelineProgress::Progress { .. } => "pipeline-progress",
            PipelineProgress::FileCompleted { .. } => "pipeline-file-completed",
            PipelineProgress::FileFailed { .. } => "pipeline-file-failed",
            PipelineProgress::Completed { .. } => "pipeline-completed",
        };
        if let Err(error) = self.app_handle.emit(event_name, event) {
            tracing::warn!("Failed to emit observer event {event_name}: {error}");
        }
    }
}

/// No-op observer for testing.
pub struct NullObserver;

impl ProgressObserver for NullObserver {
    fn on_event(&self, _event: &PipelineProgress) {
        // no-op
    }
}

/// Create an MPSC channel pair for progress events.
pub fn progress_channel() -> (mpsc::Sender<PipelineProgress>, mpsc::Receiver<PipelineProgress>) {
    mpsc::channel()
}

/// Observer wrapping an MPSC sender.
pub struct ChannelObserver {
    sender: mpsc::Sender<PipelineProgress>,
}

impl ChannelObserver {
    pub fn new(sender: mpsc::Sender<PipelineProgress>) -> Self {
        Self { sender }
    }
}

impl ProgressObserver for ChannelObserver {
    fn on_event(&self, event: &PipelineProgress) {
        if let Err(error) = self.sender.send(event.clone()) {
            tracing::warn!("Failed to send progress observer event: {error}");
        }
    }
}

/// Multi-observer that fans out events to multiple observers.
pub struct MultiObserver {
    observers: Vec<Box<dyn ProgressObserver>>,
}

impl Default for MultiObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiObserver {
    pub fn new() -> Self {
        Self { observers: Vec::new() }
    }

    pub fn add<T: ProgressObserver>(&mut self, observer: T) {
        self.observers.push(Box::new(observer));
    }
}

impl ProgressObserver for MultiObserver {
    fn on_event(&self, event: &PipelineProgress) {
        for observer in &self.observers {
            observer.on_event(event);
        }
    }
}

/// Observable trait for structs that emit progress events.
pub trait Observable {
    fn set_observer(&mut self, observer: Box<dyn ProgressObserver>);
    fn notify(&self, event: &PipelineProgress);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingObserver {
        count: Arc<AtomicUsize>,
    }

    impl ProgressObserver for CountingObserver {
        fn on_event(&self, _event: &PipelineProgress) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_channel_observer() {
        let (tx, rx) = progress_channel();
        let observer = ChannelObserver::new(tx);

        observer.on_event(&PipelineProgress::Started { total: 10 });
        observer.on_event(&PipelineProgress::Progress {
            current: 5,
            total: 10,
            file: "test.wav".to_string(),
            status: "processing".to_string(),
        });

        let received: Vec<_> = rx.try_iter().collect();
        assert_eq!(received.len(), 2);
    }

    #[test]
    fn test_multi_observer() {
        let count1 = Arc::new(AtomicUsize::new(0));
        let count2 = Arc::new(AtomicUsize::new(0));

        let mut multi = MultiObserver::new();
        multi.add(CountingObserver { count: count1.clone() });
        multi.add(CountingObserver { count: count2.clone() });

        multi.on_event(&PipelineProgress::Started { total: 5 });
        multi.on_event(&PipelineProgress::Completed { total: 5, succeeded: 3, failed: 2 });

        assert_eq!(count1.load(Ordering::SeqCst), 2);
        assert_eq!(count2.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_null_observer_does_not_panic() {
        let observer = NullObserver;
        observer.on_event(&PipelineProgress::Started { total: 0 });
        // Should not panic
    }
}
