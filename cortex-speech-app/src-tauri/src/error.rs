use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Audio processing failed: {0}")]
    Audio(#[from] AudioError),

    #[error("ASR inference failed: {0}")]
    Asr(String),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Model not found at {path}: {reason}")]
    ModelNotFound { path: PathBuf, reason: String },

    #[error("ONNX Runtime error: {0}")]
    Onnx(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// Classify retryable SQLite writer contention without leaking database-backend types into
    /// command handlers. The text fallback preserves classification for errors that crossed an
    /// older string-only boundary before becoming an `AppError`.
    pub(crate) fn is_database_busy(&self) -> bool {
        if matches!(
            self,
            Self::Database(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        ) {
            return true;
        }

        let normalized = self.to_string().to_ascii_lowercase();
        normalized.contains("database is locked") || normalized.contains("database is busy")
    }
}

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("Unsupported codec: {0}")]
    UnsupportedCodec(String),

    #[error("Decoding failed: {0}")]
    Decode(String),

    #[error("Resampling failed: {0}")]
    Resample(String),

    #[error("VAD processing failed: {0}")]
    Vad(String),

    #[error("No audio tracks found in {0}")]
    NoTracks(PathBuf),

    #[error("Empty audio buffer")]
    EmptyBuffer,
}

impl From<Box<dyn std::error::Error>> for AppError {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        AppError::Other(e.to_string())
    }
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Other(s)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Other(s.to_string())
    }
}

impl From<AppError> for String {
    fn from(e: AppError) -> Self {
        e.to_string()
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::AppError;

    #[test]
    fn database_busy_classification_handles_structured_and_legacy_errors() {
        for result_code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let structured =
                AppError::Database(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result_code), None));
            assert!(structured.is_database_busy());
        }
        assert!(AppError::Other("database is locked by another writer".into()).is_database_busy());
        assert!(!AppError::Other("database file is corrupt".into()).is_database_busy());
    }
}
