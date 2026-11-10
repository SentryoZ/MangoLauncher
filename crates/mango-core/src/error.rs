use std::path::PathBuf;

/// Errors produced by the launcher core.
#[derive(Debug, thiserror::Error)]
pub enum MangoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("could not determine a data directory for this platform")]
    NoDataDir,

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("file is missing or invalid: {0}")]
    InvalidFile(PathBuf),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MangoError>;
