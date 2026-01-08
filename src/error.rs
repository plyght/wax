use thiserror::Error;

#[derive(Error, Debug)]
pub enum WaxError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON parsing failed: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Formula not found: {0}")]
    FormulaNotFound(String),

    #[error("Cask not found: {0}")]
    CaskNotFound(String),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Homebrew directory not found")]
    HomebrewNotFound,
}

pub type Result<T> = std::result::Result<T, WaxError>;
