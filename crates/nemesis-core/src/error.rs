use std::io;
use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("run {0} was not found")]
    RunNotFound(uuid::Uuid),
    #[error("project is not initialized: {0}")]
    NotInitialized(PathBuf),
    #[error("invalid model data: {0}")]
    InvalidModel(String),
}

pub fn io_error(path: impl Into<PathBuf>, source: io::Error) -> CoreError {
    CoreError::Io {
        path: path.into(),
        source,
    }
}
