use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BvrError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("beads directory not found: {0}")]
    MissingBeadsDir(PathBuf),

    #[error("no beads JSONL file found in {0}")]
    MissingBeadsFile(PathBuf),

    #[error("invalid issue data: {0}")]
    InvalidIssue(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("tui runtime error: {0}")]
    Tui(String),
}

pub type Result<T> = std::result::Result<T, BvrError>;
