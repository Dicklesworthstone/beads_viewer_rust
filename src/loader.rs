use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::model::{Issue, Sprint};
use crate::{BvrError, Result};
use serde::Deserialize;

pub const BEADS_DIR_ENV: &str = "BEADS_DIR";

const PREFERRED_JSONL_NAMES: &[&str] = &["beads.jsonl", "issues.jsonl", "beads.base.jsonl"];
const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;
pub const SPRINTS_FILE_NAME: &str = "sprints.jsonl";
pub const WORKSPACE_CONFIG_PATH: &str = ".bv/workspace.yaml";

#[must_use]
pub fn is_robot_mode() -> bool {
    std::env::var("BV_ROBOT").is_ok_and(|value| value == "1")
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub repos: Vec<WorkspaceRepoConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceRepoConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub beads_path: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceLoadSummary {
    pub total_repos: usize,
    pub successful_repos: usize,
    pub failed_repos: usize,
    pub total_issues: usize,
    pub failed_repo_names: Vec<String>,
    pub repo_prefixes: Vec<String>,
}

#[derive(Debug, Clone)]
struct WorkspaceRepoLoadResult {
    repo_name: String,
    prefix: String,
    issues: Vec<Issue>,
    error: Option<String>,
}

impl WorkspaceRepoConfig {
    fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    fn effective_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }

        Path::new(self.path.trim())
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| self.path.trim())
            .to_string()
    }

    fn effective_prefix(&self) -> String {
        if !self.prefix.trim().is_empty() {
            return self.prefix.trim().to_string();
        }

        let fallback = self.effective_name();
        format!("{}-", fallback.to_ascii_lowercase())
    }

    fn effective_beads_path(&self) -> String {
        if self.beads_path.trim().is_empty() {
            ".beads".to_string()
        } else {
            self.beads_path.trim().to_string()
        }
    }
}

impl WorkspaceConfig {
    fn validate(&self) -> Result<()> {
        if self.repos.is_empty() {
            return Err(BvrError::InvalidArgument(
                "workspace must define at least one repository".to_string(),
            ));
        }

        let mut seen_prefixes = HashSet::<String>::new();
        let mut enabled_count = 0usize;

        for (index, repo) in self.repos.iter().enumerate() {
            if !repo.is_enabled() {
                continue;
            }

            enabled_count = enabled_count.saturating_add(1);

            if repo.path.trim().is_empty() {
                return Err(BvrError::InvalidArgument(format!(
                    "workspace repo[{index}] has an empty path"
                )));
            }

            let prefix = repo.effective_prefix().to_ascii_lowercase();
            if !seen_prefixes.insert(prefix.clone()) {
                return Err(BvrError::InvalidArgument(format!(
                    "workspace repo[{index}] has duplicate prefix '{prefix}'"
                )));
            }
        }

        if enabled_count == 0 {
            return Err(BvrError::InvalidArgument(
                "workspace has no enabled repositories".to_string(),
            ));
        }

        Ok(())
    }
}

fn resolve_workspace_root(config_path: &Path) -> PathBuf {
    config_path.parent().and_then(Path::parent).map_or_else(
        || {
            config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        },
        PathBuf::from,
    )
}

fn qualify_id(local_id: &str, prefix: &str) -> String {
    if local_id.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        local_id.to_string()
    } else {
        format!("{prefix}{local_id}")
    }
}

fn has_known_prefix(id: &str, prefixes: &[String]) -> bool {
    let id_lower = id.to_ascii_lowercase();
    prefixes
        .iter()
        .any(|prefix| id_lower.starts_with(&prefix.to_ascii_lowercase()))
}

fn namespace_workspace_issues(
    issues: &mut [Issue],
    prefix: &str,
    repo_name: &str,
    known_prefixes: &[String],
) {
    let local_ids = issues
        .iter()
        .map(|issue| issue.id.trim().to_string())
        .collect::<HashSet<_>>();

    for issue in issues.iter_mut() {
        let local_issue_id = issue.id.trim().to_string();
        issue.id = qualify_id(&local_issue_id, prefix);
        issue.source_repo = repo_name.to_string();

        for dependency in &mut issue.dependencies {
            let dep_issue_id = dependency.issue_id.trim();
            dependency.issue_id = if dep_issue_id.is_empty() {
                issue.id.clone()
            } else {
                qualify_id(dep_issue_id, prefix)
            };

            let depends_on = dependency.depends_on_id.trim();
            dependency.depends_on_id = if depends_on.is_empty() {
                depends_on.to_string()
            } else if local_ids.contains(depends_on) {
                qualify_id(depends_on, prefix)
            } else if has_known_prefix(depends_on, known_prefixes) {
                depends_on.to_string()
            } else {
                qualify_id(depends_on, prefix)
            };
        }

        for comment in &mut issue.comments {
            let comment_issue_id = comment.issue_id.trim();
            comment.issue_id = if comment_issue_id.is_empty() {
                issue.id.clone()
            } else {
                qualify_id(comment_issue_id, prefix)
            };
        }
    }
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

pub fn find_workspace_config_from(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(WORKSPACE_CONFIG_PATH);
        if candidate.is_file() {
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

pub fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig> {
    let config_text = std::fs::read_to_string(path)?;
    let config = serde_yaml::from_str::<WorkspaceConfig>(&config_text).map_err(|error| {
        BvrError::InvalidArgument(format!(
            "invalid workspace config {}: {error}",
            path.display()
        ))
    })?;

    config.validate()?;
    Ok(config)
}

pub fn load_workspace_issues(path: &Path) -> Result<Vec<Issue>> {
    let (issues, _) = load_workspace_issues_with_summary(path)?;
    Ok(issues)
}

pub fn load_workspace_issues_with_summary(
    path: &Path,
) -> Result<(Vec<Issue>, WorkspaceLoadSummary)> {
    let config = load_workspace_config(path)?;
    let workspace_root = resolve_workspace_root(path);

    let enabled_repos = config
        .repos
        .iter()
        .filter(|repo| repo.is_enabled())
        .cloned()
        .collect::<Vec<_>>();

    let known_prefixes = enabled_repos
        .iter()
        .map(WorkspaceRepoConfig::effective_prefix)
        .collect::<Vec<_>>();

    let mut per_repo_results = Vec::<WorkspaceRepoLoadResult>::new();

    for repo in &enabled_repos {
        let repo_name = repo.effective_name();
        let prefix = repo.effective_prefix();

        let repo_path = if Path::new(repo.path.trim()).is_absolute() {
            PathBuf::from(repo.path.trim())
        } else {
            workspace_root.join(repo.path.trim())
        };
        let beads_dir = repo_path.join(repo.effective_beads_path());

        let repo_result = (|| -> Result<Vec<Issue>> {
            let jsonl_path = find_jsonl_path(&beads_dir)?;
            let mut issues = load_issues_from_file(&jsonl_path)?;
            namespace_workspace_issues(&mut issues, &prefix, &repo_name, &known_prefixes);
            Ok(issues)
        })();

        match repo_result {
            Ok(issues) => {
                per_repo_results.push(WorkspaceRepoLoadResult {
                    repo_name,
                    prefix,
                    issues,
                    error: None,
                });
            }
            Err(error) => {
                warn(format!(
                    "workspace repo '{repo_name}' failed to load: {error}"
                ));
                per_repo_results.push(WorkspaceRepoLoadResult {
                    repo_name,
                    prefix,
                    issues: Vec::new(),
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let mut issues = Vec::<Issue>::new();
    let mut summary = WorkspaceLoadSummary {
        total_repos: per_repo_results.len(),
        ..WorkspaceLoadSummary::default()
    };

    for result in per_repo_results {
        if result.error.is_some() {
            summary.failed_repos = summary.failed_repos.saturating_add(1);
            summary.failed_repo_names.push(result.repo_name);
            continue;
        }

        summary.successful_repos = summary.successful_repos.saturating_add(1);
        if !result.prefix.trim().is_empty() {
            summary.repo_prefixes.push(result.prefix);
        }
        summary.total_issues = summary.total_issues.saturating_add(result.issues.len());
        issues.extend(result.issues);
    }

    Ok((issues, summary))
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

    #[test]
    fn find_workspace_config_walks_up_directory_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        let config_path = workspace_dir.join("workspace.yaml");
        std::fs::write(&config_path, "repos:\n  - path: api\n").expect("write workspace config");

        let nested = root.join("services/api/src");
        std::fs::create_dir_all(&nested).expect("create nested path");

        let found = find_workspace_config_from(&nested).expect("find workspace config");
        assert_eq!(found, config_path);
    }

    #[test]
    fn load_workspace_issues_namespaces_ids_and_dependencies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let workspace_dir = root.join(".bv");
        let api_beads = root.join("services/api/.beads");
        let web_beads = root.join("apps/web/.beads");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        std::fs::create_dir_all(&api_beads).expect("create api .beads");
        std::fs::create_dir_all(&web_beads).expect("create web .beads");

        std::fs::write(
            workspace_dir.join("workspace.yaml"),
            "name: demo\nrepos:\n  - name: api\n    path: services/api\n    prefix: api-\n  - name: web\n    path: apps/web\n    prefix: web-\n",
        )
        .expect("write workspace config");

        std::fs::write(
            api_beads.join("issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"dependencies\":[{\"issue_id\":\"AUTH-1\",\"depends_on_id\":\"AUTH-2\",\"type\":\"blocks\"}]}\n{\"id\":\"AUTH-2\",\"title\":\"Auth Prereq\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        std::fs::write(
            web_beads.join("issues.jsonl"),
            "{\"id\":\"UI-1\",\"title\":\"UI\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"dependencies\":[{\"issue_id\":\"UI-1\",\"depends_on_id\":\"api-AUTH-1\",\"type\":\"blocks\"}]}\n",
        )
        .expect("write web issues");

        let (issues, summary) =
            load_workspace_issues_with_summary(&workspace_dir.join("workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 2);
        assert_eq!(summary.successful_repos, 2);
        assert_eq!(summary.failed_repos, 0);
        assert_eq!(summary.total_issues, 3);

        let auth_issue = issues
            .iter()
            .find(|issue| issue.id == "api-AUTH-1")
            .expect("api-AUTH-1 issue");
        assert_eq!(auth_issue.source_repo, "api");
        assert_eq!(auth_issue.dependencies.len(), 1);
        assert_eq!(auth_issue.dependencies[0].depends_on_id, "api-AUTH-2");

        let web_issue = issues
            .iter()
            .find(|issue| issue.id == "web-UI-1")
            .expect("web-UI-1 issue");
        assert_eq!(web_issue.source_repo, "web");
        assert_eq!(web_issue.dependencies[0].depends_on_id, "api-AUTH-1");
    }

    #[test]
    fn load_workspace_issues_continues_when_some_repos_fail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let workspace_dir = root.join(".bv");
        let api_beads = root.join("services/api/.beads");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        std::fs::create_dir_all(&api_beads).expect("create api .beads");

        std::fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - name: api\n    path: services/api\n    prefix: api-\n  - name: missing\n    path: services/missing\n    prefix: missing-\n",
        )
        .expect("write workspace config");
        std::fs::write(
            api_beads.join("issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        let (issues, summary) =
            load_workspace_issues_with_summary(&workspace_dir.join("workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 2);
        assert_eq!(summary.successful_repos, 1);
        assert_eq!(summary.failed_repos, 1);
        assert_eq!(summary.failed_repo_names, vec!["missing".to_string()]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "api-AUTH-1");
    }

    #[test]
    fn load_workspace_config_rejects_duplicate_prefixes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        let config_path = workspace_dir.join("workspace.yaml");
        std::fs::write(
            &config_path,
            "repos:\n  - path: services/api\n    prefix: app-\n  - path: services/web\n    prefix: app-\n",
        )
        .expect("write config");

        let error = load_workspace_config(&config_path).expect_err("duplicate prefixes rejected");
        assert!(error.to_string().contains("duplicate prefix"));
    }
}
