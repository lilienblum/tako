use std::path::PathBuf;
use thiserror::Error;

/// Configuration errors
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read file {0}: {1}")]
    FileRead(PathBuf, std::io::Error),

    #[error("Failed to write file {0}: {1}")]
    FileWrite(PathBuf, std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("Failed to serialize TOML: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("Failed to parse JSON: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Server '{0}' not found in global config")]
    ServerNotFound(String),

    #[error("Duplicate server name: {0}")]
    DuplicateServerName(String),

    #[error("Duplicate server host: {0}")]
    DuplicateServerHost(String),

    #[error("Environment '{0}' not found")]
    EnvironmentNotFound(String),

    #[error("Invalid route pattern: {0}")]
    InvalidRoutePattern(String),

    #[error("Secret '{0}' not found")]
    SecretNotFound(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),
}

pub type Result<T> = std::result::Result<T, ConfigError>;
