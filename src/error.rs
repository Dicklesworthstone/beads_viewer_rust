use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BvrError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("beads directory not found: {0}")]
    MissingBeadsDir(PathBuf),

    #[error("no beads JSONL file found in {0}")]
    MissingBeadsFile(PathBuf),

    #[error("invalid issue data: {0}")]
    InvalidIssue(String),

    #[error(
        "bd export failed for {beads_dir}: {message}; use BV_DATA_SOURCE=jsonl for an existing issues.jsonl snapshot"
    )]
    DoltExport {
        beads_dir: PathBuf,
        message: String,
    },

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
    fn sqlite_error_converts_from_rusqlite() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        let sqlite_err = conn
            .execute("SELECT definitely_not_valid_sql", [])
            .expect_err("invalid SQL should fail");
        let err: BvrError = sqlite_err.into();
        assert!(err.to_string().contains("sqlite error"));
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
    fn dolt_error_identifies_workspace_and_explicit_snapshot_option() {
        let err = BvrError::DoltExport {
            beads_dir: PathBuf::from("/tmp/live/.beads"),
            message: "timed out after 5 seconds".to_string(),
        };
        let message = err.to_string();
        assert!(message.contains("bd export"));
        assert!(message.contains("/tmp/live/.beads"));
        assert!(message.contains("timed out"));
        assert!(message.contains("BV_DATA_SOURCE=jsonl"));
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
