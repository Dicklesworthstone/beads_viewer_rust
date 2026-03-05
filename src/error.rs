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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_display_includes_source() {
        let err = BvrError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert!(err.to_string().contains("io error"));
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn json_error_converts_from_serde() {
        let bad_json = "{{invalid";
        let result: std::result::Result<serde_json::Value, _> = serde_json::from_str(bad_json);
        let serde_err = result.unwrap_err();
        let err: BvrError = serde_err.into();
        assert!(err.to_string().contains("json parse error"));
    }

    #[test]
    fn missing_beads_dir_shows_path() {
        let err = BvrError::MissingBeadsDir(PathBuf::from("/tmp/nope"));
        assert!(err.to_string().contains("/tmp/nope"));
    }

    #[test]
    fn missing_beads_file_shows_path() {
        let err = BvrError::MissingBeadsFile(PathBuf::from("/tmp/.beads"));
        assert!(err.to_string().contains("/tmp/.beads"));
    }

    #[test]
    fn invalid_issue_shows_detail() {
        let err = BvrError::InvalidIssue("bad id".to_string());
        assert!(err.to_string().contains("bad id"));
    }

    #[test]
    fn invalid_argument_shows_detail() {
        let err = BvrError::InvalidArgument("--unknown".to_string());
        assert!(err.to_string().contains("--unknown"));
    }

    #[test]
    fn tui_error_shows_detail() {
        let err = BvrError::Tui("render crash".to_string());
        assert!(err.to_string().contains("render crash"));
    }

    #[test]
    fn error_debug_format_works() {
        let err = BvrError::InvalidArgument("test".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("InvalidArgument"));
    }
}
