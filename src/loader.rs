use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::model::{Issue, Sprint};
use crate::{BvrError, Result};

pub const BEADS_DIR_ENV: &str = "BEADS_DIR";

const PREFERRED_JSONL_NAMES: &[&str] = &["beads.jsonl", "issues.jsonl", "beads.base.jsonl"];
const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;
pub const SPRINTS_FILE_NAME: &str = "sprints.jsonl";

#[must_use]
pub fn is_robot_mode() -> bool {
    std::env::var("BV_ROBOT").is_ok_and(|value| value == "1")
}

fn find_beads_dir_from(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".beads");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    None
}

pub fn get_beads_dir(repo_path: Option<&Path>) -> Result<PathBuf> {
    if let Ok(dir) = std::env::var(BEADS_DIR_ENV)
        && !dir.trim().is_empty()
    {
        let candidate = PathBuf::from(dir);
        if candidate.is_dir() {
            return Ok(candidate);
        }

        return Err(BvrError::MissingBeadsDir(candidate));
    }

    let root = if let Some(path) = repo_path {
        path.to_path_buf()
    } else {
        std::env::current_dir()?
    };

    find_beads_dir_from(&root)
        .map_or_else(|| Err(BvrError::MissingBeadsDir(root.join(".beads"))), Ok)
}

pub fn find_jsonl_path(beads_dir: &Path) -> Result<PathBuf> {
    for preferred in PREFERRED_JSONL_NAMES {
        let path = beads_dir.join(preferred);
        if path.is_file() && std::fs::metadata(&path)?.len() > 0 {
            return Ok(path);
        }
    }

    let mut fallback_candidates = Vec::<PathBuf>::new();
    for entry in std::fs::read_dir(beads_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension() != Some(OsStr::new("jsonl")) {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase();

        let skip = file_name.contains(".backup")
            || file_name.contains(".orig")
            || file_name.contains(".merge")
            || file_name == "deletions.jsonl"
            || file_name.starts_with("beads.left")
            || file_name.starts_with("beads.right");

        if skip {
            continue;
        }

        fallback_candidates.push(path);
    }

    fallback_candidates.sort();
    fallback_candidates
        .into_iter()
        .next()
        .ok_or_else(|| BvrError::MissingBeadsFile(beads_dir.to_path_buf()))
}

pub fn load_issues(repo_path: Option<&Path>) -> Result<Vec<Issue>> {
    let beads_dir = get_beads_dir(repo_path)?;
    let path = find_jsonl_path(&beads_dir)?;
    load_issues_from_file(&path)
}

pub fn load_issues_from_file(path: &Path) -> Result<Vec<Issue>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut issues = Vec::new();

    let mut line_no = 0usize;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        line_no += 1;

        if bytes > MAX_LINE_BYTES {
            warn(format!(
                "skipping line {line_no} in {}: line exceeds {MAX_LINE_BYTES} bytes",
                path.display()
            ));
            continue;
        }

        let trimmed = if line_no == 1 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };

        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Issue>(trimmed) {
            Ok(mut issue) => {
                issue.status = issue.normalized_status();
                if let Err(error) = issue.validate() {
                    warn(format!(
                        "skipping invalid issue on line {line_no} in {}: {error}",
                        path.display()
                    ));
                    continue;
                }
                issues.push(issue);
            }
            Err(error) => {
                warn(format!(
                    "skipping malformed JSON on line {line_no} in {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Ok(issues)
}

pub fn load_sprints(repo_path: Option<&Path>) -> Result<Vec<Sprint>> {
    let beads_dir = get_beads_dir(repo_path)?;
    let path = beads_dir.join(SPRINTS_FILE_NAME);
    load_sprints_from_file(&path)
}

pub fn load_sprints_from_file(path: &Path) -> Result<Vec<Sprint>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut sprints = Vec::new();

    let mut line_no = 0usize;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        line_no += 1;

        if bytes > MAX_LINE_BYTES {
            warn(format!(
                "skipping line {line_no} in {}: line exceeds {MAX_LINE_BYTES} bytes",
                path.display()
            ));
            continue;
        }

        let trimmed = if line_no == 1 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Sprint>(trimmed) {
            Ok(sprint) => {
                if sprint.id.trim().is_empty() || sprint.name.trim().is_empty() {
                    warn(format!(
                        "skipping invalid sprint on line {line_no} in {}: missing id or name",
                        path.display()
                    ));
                    continue;
                }
                if sprint
                    .start_date
                    .zip(sprint.end_date)
                    .is_some_and(|(start, end)| end < start)
                {
                    warn(format!(
                        "skipping invalid sprint on line {line_no} in {}: end_date before start_date",
                        path.display()
                    ));
                    continue;
                }
                sprints.push(sprint);
            }
            Err(error) => {
                warn(format!(
                    "skipping malformed sprint JSON on line {line_no} in {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Ok(sprints)
}

fn warn(message: String) {
    if !is_robot_mode() {
        eprintln!("Warning: {message}");
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn parses_minimal_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("issues.jsonl");
        let mut file = File::create(&path).expect("create file");

        writeln!(
            file,
            "{{\"id\":\"A\",\"title\":\"Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}}"
        )
        .expect("write line A");
        writeln!(
            file,
            "{{\"id\":\"B\",\"title\":\"Child\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{{\"depends_on_id\":\"A\",\"type\":\"blocks\"}}]}}"
        )
        .expect("write line B");

        let issues = load_issues_from_file(&path).expect("load issues");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "A");
        assert_eq!(issues[1].dependencies.len(), 1);
    }

    #[test]
    fn finds_preferred_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads_dir = dir.path();
        std::fs::write(beads_dir.join("issues.jsonl"), "{}\n").expect("write issues");
        std::fs::write(beads_dir.join("beads.jsonl"), "{}\n").expect("write beads");

        let path = find_jsonl_path(beads_dir).expect("find path");
        assert!(path.ends_with("beads.jsonl"));
    }

    #[test]
    fn get_beads_dir_finds_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".beads")).expect("create .beads");
        let nested = root.join("nested/work");
        std::fs::create_dir_all(&nested).expect("create nested");

        let beads_dir = get_beads_dir(Some(&nested)).expect("find parent .beads");
        assert_eq!(beads_dir, root.join(".beads"));
    }

    #[test]
    fn find_jsonl_fallback_is_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads_dir = dir.path();
        std::fs::write(beads_dir.join("zeta.jsonl"), "{}\n").expect("write zeta");
        std::fs::write(beads_dir.join("alpha.jsonl"), "{}\n").expect("write alpha");

        let path = find_jsonl_path(beads_dir).expect("find fallback path");
        assert!(path.ends_with("alpha.jsonl"));
    }

    #[test]
    fn load_sprints_uses_nested_repo_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let beads_dir = root.join(".beads");
        let nested = root.join("nested/work");
        std::fs::create_dir_all(&beads_dir).expect("create .beads");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(
            beads_dir.join("sprints.jsonl"),
            "{\"id\":\"s1\",\"name\":\"Sprint 1\",\"bead_ids\":[\"A\"]}\n",
        )
        .expect("write sprints");

        let sprints = load_sprints(Some(&nested)).expect("load sprints");
        assert_eq!(sprints.len(), 1);
        assert_eq!(sprints[0].id, "s1");
    }
}
