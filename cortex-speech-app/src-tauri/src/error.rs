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
