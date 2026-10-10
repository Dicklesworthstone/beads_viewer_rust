use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::model::{Issue, Sprint};
use crate::{BvrError, Result};
use serde::Deserialize;

pub const BEADS_DIR_ENV: &str = "BEADS_DIR";
static ROBOT_WARNING_SUPPRESSION: AtomicBool = AtomicBool::new(false);

const PREFERRED_JSONL_NAMES: &[&str] = &["beads.jsonl", "issues.jsonl", "beads.base.jsonl"];
const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;
pub const SPRINTS_FILE_NAME: &str = "sprints.jsonl";
pub const WORKSPACE_CONFIG_PATH: &str = ".bv/workspace.yaml";
const DEFAULT_WORKSPACE_DISCOVERY_PATTERNS: &[&str] = &[
    "*",
    "packages/*",
    "apps/*",
    "services/*",
    "libs/*",
    "modules/*",
];
const DEFAULT_WORKSPACE_EXCLUDE_PATTERNS: &[&str] =
    &["node_modules", "vendor", ".git", "dist", "build", "target"];
const DEFAULT_WORKSPACE_DISCOVERY_MAX_DEPTH: usize = 2;

#[must_use]
pub fn is_robot_mode() -> bool {
    ROBOT_WARNING_SUPPRESSION.load(Ordering::Relaxed)
        || std::env::var("BV_ROBOT").is_ok_and(|value| value == "1")
}

pub fn set_robot_warning_suppression(enabled: bool) {
    ROBOT_WARNING_SUPPRESSION.store(enabled, Ordering::Relaxed);
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub repos: Vec<WorkspaceRepoConfig>,
    #[serde(default)]
    pub discovery: WorkspaceDiscoveryConfig,
    #[serde(default)]
    pub defaults: WorkspaceDefaultsConfig,
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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceDiscoveryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub max_depth: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceDefaultsConfig {
    #[serde(default)]
    pub beads_path: String,
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

    pub fn effective_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }

        Path::new(self.path.trim())
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| self.path.trim())
            .to_string()
    }

    pub fn effective_prefix(&self) -> String {
        if !self.prefix.trim().is_empty() {
            return self.prefix.trim().to_string();
        }

        let fallback = self.effective_name();
        format!("{}-", fallback.to_ascii_lowercase())
    }

    pub fn effective_beads_path(&self, defaults: Option<&WorkspaceDefaultsConfig>) -> String {
        if !self.beads_path.trim().is_empty() {
            self.beads_path.trim().to_string()
        } else if let Some(defaults) = defaults
            && !defaults.beads_path.trim().is_empty()
        {
            defaults.beads_path.trim().to_string()
        } else {
            ".beads".to_string()
        }
    }
}

impl WorkspaceConfig {
    fn apply_defaults(&mut self) {
        if self.discovery.enabled {
            if self.discovery.patterns.is_empty() {
                self.discovery.patterns = DEFAULT_WORKSPACE_DISCOVERY_PATTERNS
                    .iter()
                    .map(|pattern| (*pattern).to_string())
                    .collect();
            }
            if self.discovery.exclude.is_empty() {
                self.discovery.exclude = DEFAULT_WORKSPACE_EXCLUDE_PATTERNS
                    .iter()
                    .map(|pattern| (*pattern).to_string())
                    .collect();
            }
            if self.discovery.max_depth == 0 {
                self.discovery.max_depth = DEFAULT_WORKSPACE_DISCOVERY_MAX_DEPTH;
            }
        }
    }

    fn resolve_repos(
        &self,
        workspace_root: &Path,
        config_path: &Path,
    ) -> Result<Vec<WorkspaceRepoConfig>> {
        let mut repos = self.repos.clone();
        if self.discovery.enabled {
            repos.extend(discover_workspace_repos(
                workspace_root,
                &self.discovery,
                &self.defaults,
                &repos,
            )?);

            if repos.is_empty() {
                let searched_patterns = self.discovery.patterns.join(", ");
                let excludes = self.discovery.exclude.join(", ");
                return Err(BvrError::InvalidArgument(format!(
                    "workspace discovery found no repositories for {}.\n\
                     Searched root: {}\n\
                     Patterns: [{}]\n\
                     Exclude: [{}]\n\
                     Max depth: {}\n\
                     Remediation:\n\
                       1. Add explicit repos: entries to {}.\n\
                       2. Adjust discovery.patterns or defaults.beads_path to match your layout.\n\
                       3. Or rerun with --workspace <path-to-.bv/workspace.yaml> pointing at a config with explicit repos.",
                    config_path.display(),
                    workspace_root.display(),
                    searched_patterns,
                    excludes,
                    self.discovery.max_depth,
                    config_path.display(),
                )));
            }
        }

        for repo in &mut repos {
            if !repo.is_enabled() || !repo.name.trim().is_empty() {
                continue;
            }

            repo.name = inferred_repo_name(Path::new(repo.path.trim()), workspace_root);
        }

        let mut seen_repo_paths = HashSet::<String>::new();
        for (index, repo) in repos.iter().enumerate() {
            if !repo.is_enabled() {
                continue;
            }

            let identity = repo_identity_key(Path::new(repo.path.trim()), workspace_root);
            if !seen_repo_paths.insert(identity.clone()) {
                return Err(BvrError::InvalidArgument(format!(
                    "workspace repo[{index}] duplicates repository path '{identity}'"
                )));
            }
        }

        Ok(repos)
    }

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

pub fn resolve_workspace_root(config_path: &Path) -> PathBuf {
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

fn normalize_path_for_display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_path_for_identity(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
            Component::RootDir | Component::Prefix(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn relative_path_matches_pattern(relative_path: &Path, pattern: &str) -> bool {
    if relative_path.as_os_str().is_empty() {
        return pattern.trim().is_empty() || pattern == ".";
    }

    let path_segments = relative_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let pattern_segments = pattern
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if path_segments.len() != pattern_segments.len() {
        return false;
    }

    pattern_segments
        .iter()
        .zip(&path_segments)
        .all(|(pattern_segment, path_segment)| {
            *pattern_segment == "*" || *pattern_segment == path_segment
        })
}

fn is_excluded_workspace_path(relative_path: &Path, exclude_patterns: &[String]) -> bool {
    let components = relative_path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    exclude_patterns.iter().any(|pattern| {
        if pattern.contains('/') || pattern.contains('*') {
            relative_path_matches_pattern(relative_path, pattern)
        } else {
            components.iter().any(|component| component == pattern)
        }
    })
}

fn repo_identity_key(repo_path: &Path, workspace_root: &Path) -> String {
    let resolved = if repo_path.is_absolute() {
        repo_path.to_path_buf()
    } else {
        workspace_root.join(repo_path)
    };
    let normalized =
        std::fs::canonicalize(&resolved).unwrap_or_else(|_| normalize_path_for_identity(&resolved));
    normalize_path_for_display(&normalized)
}

fn workspace_root_repo_name(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("root")
        .to_string()
}

fn inferred_repo_name(repo_path: &Path, workspace_root: &Path) -> String {
    let resolved = if repo_path.is_absolute() {
        repo_path.to_path_buf()
    } else {
        workspace_root.join(repo_path)
    };
    let normalized =
        std::fs::canonicalize(&resolved).unwrap_or_else(|_| normalize_path_for_identity(&resolved));
    normalized.file_name().and_then(OsStr::to_str).map_or_else(
        || workspace_root_repo_name(workspace_root),
        ToString::to_string,
    )
}

fn discover_workspace_repos(
    workspace_root: &Path,
    discovery: &WorkspaceDiscoveryConfig,
    defaults: &WorkspaceDefaultsConfig,
    explicit_repos: &[WorkspaceRepoConfig],
) -> Result<Vec<WorkspaceRepoConfig>> {
    let mut discovered = Vec::<WorkspaceRepoConfig>::new();
    let mut seen_repo_paths = explicit_repos
        .iter()
        .filter(|repo| repo.is_enabled())
        .map(|repo| repo_identity_key(Path::new(repo.path.trim()), workspace_root))
        .collect::<HashSet<_>>();

    if discovery
        .patterns
        .iter()
        .any(|pattern| relative_path_matches_pattern(Path::new(""), pattern))
    {
        let identity = repo_identity_key(Path::new("."), workspace_root);
        let beads_dir = workspace_root
            .join(WorkspaceRepoConfig::default().effective_beads_path(Some(defaults)));
        if seen_repo_paths.insert(identity) && beads_dir.is_dir() {
            discovered.push(WorkspaceRepoConfig {
                name: workspace_root_repo_name(workspace_root),
                path: ".".to_string(),
                ..WorkspaceRepoConfig::default()
            });
        }
    }

    let mut stack = vec![(workspace_root.to_path_buf(), 0usize)];

    while let Some((current_dir, depth)) = stack.pop() {
        if depth >= discovery.max_depth {
            continue;
        }

        let mut child_dirs = Vec::<PathBuf>::new();
        for entry in std::fs::read_dir(&current_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                child_dirs.push(path);
            }
        }
        child_dirs.sort();

        for child_dir in child_dirs {
            let relative = child_dir
                .strip_prefix(workspace_root)
                .unwrap_or(child_dir.as_path());
            if is_excluded_workspace_path(relative, &discovery.exclude) {
                continue;
            }

            let next_depth = depth.saturating_add(1);
            if next_depth <= discovery.max_depth {
                stack.push((child_dir.clone(), next_depth));
            }

            if !discovery
                .patterns
                .iter()
                .any(|pattern| relative_path_matches_pattern(relative, pattern))
            {
                continue;
            }

            let identity = repo_identity_key(relative, workspace_root);
            if !seen_repo_paths.insert(identity) {
                continue;
            }

            let beads_dir =
                child_dir.join(WorkspaceRepoConfig::default().effective_beads_path(Some(defaults)));
            if !beads_dir.is_dir() {
                continue;
            }

            discovered.push(WorkspaceRepoConfig {
                path: normalize_path_for_display(relative),
                ..WorkspaceRepoConfig::default()
            });
        }
    }

    discovered.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(discovered)
}

fn qualify_id(local_id: &str, prefix: &str) -> String {
    if local_id
        .to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
    {
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

pub fn namespace_workspace_issues(
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
        issue.workspace_prefix = Some(prefix.trim().to_string());
        issue.workspace_local_id = Some(local_issue_id.clone());

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

fn resolve_gitdir_pointer(git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(git_file).ok()?;
    let raw = contents.strip_prefix("gitdir:")?.trim();
    if raw.is_empty() {
        return None;
    }

    let pointed = PathBuf::from(raw);
    if pointed.is_absolute() {
        Some(pointed)
    } else {
        git_file.parent().map(|parent| parent.join(pointed))
    }
}

fn resolve_worktree_common_dir(gitdir: &Path) -> Option<PathBuf> {
    let commondir_path = gitdir.join("commondir");
    let raw = std::fs::read_to_string(commondir_path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let common_dir = PathBuf::from(trimmed);
    if common_dir.is_absolute() {
        std::fs::canonicalize(common_dir).ok()
    } else {
        std::fs::canonicalize(gitdir.join(common_dir)).ok()
    }
}

fn find_worktree_main_repo_beads_dir_from(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let git_file = ancestor.join(".git");
        if !git_file.is_file() {
            continue;
        }

        let Some(gitdir) = resolve_gitdir_pointer(&git_file) else {
            continue;
        };
        let Some(common_dir) = resolve_worktree_common_dir(&gitdir) else {
            continue;
        };
        let Some(repo_root) = common_dir.parent() else {
            continue;
        };
        let beads_dir = repo_root.join(".beads");
        if beads_dir.is_dir() {
            return Some(beads_dir);
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

/// Resolve the `.beads` directory, in priority order:
/// 1. `BEADS_DB` (a database/JSONL file — its parent — or a `.beads` dir);
/// 2. `BEADS_DIR`;
/// 3. the nearest `.beads` at or above `repo_path` (or the cwd);
/// 4. the main repository's `.beads` when running inside a git worktree.
///
/// Every route then follows a `.beads/redirect` chain (see
/// [`resolve_beads_redirect`]) so bvr reads the store br writes.
pub fn get_beads_dir(repo_path: Option<&Path>) -> Result<PathBuf> {
    if let Ok(db) = std::env::var(BEADS_DB_ENV)
        && !db.trim().is_empty()
    {
        let candidate = PathBuf::from(db.trim());
        if candidate.is_dir() {
            return resolve_beads_redirect(&candidate);
        }
        if candidate.is_file()
            && let Some(parent) = candidate.parent()
        {
            return Ok(parent.to_path_buf());
        }
        return Err(BvrError::MissingBeadsDir(candidate));
    }

    if let Ok(dir) = std::env::var(BEADS_DIR_ENV)
        && !dir.trim().is_empty()
    {
        let candidate = PathBuf::from(dir);
        if candidate.is_dir() {
            return resolve_beads_redirect(&candidate);
        }

        return Err(BvrError::MissingBeadsDir(candidate));
    }

    let root = if let Some(path) = repo_path {
        path.to_path_buf()
    } else {
        std::env::current_dir()?
    };

    if let Some(beads_dir) = find_beads_dir_from(&root) {
        return resolve_beads_redirect(&beads_dir);
    }

    if let Some(beads_dir) = find_worktree_main_repo_beads_dir_from(&root) {
        return resolve_beads_redirect(&beads_dir);
    }

    Err(BvrError::MissingBeadsDir(root.join(".beads")))
}

pub fn find_jsonl_path(beads_dir: &Path) -> Result<PathBuf> {
    let mut found_preferred: Option<PathBuf> = None;
    let mut other_preferred = Vec::<&str>::new();

    for preferred in PREFERRED_JSONL_NAMES {
        let path = beads_dir.join(preferred);
        if path.is_file() && std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 0) {
            if found_preferred.is_none() {
                found_preferred = Some(path);
            } else {
                other_preferred.push(preferred);
            }
        }
    }

    if let Some(ref chosen) = found_preferred {
        if !other_preferred.is_empty() {
            tracing::warn!(
                "multiple issue files found in {}: using {}, ignoring {}",
                beads_dir.display(),
                chosen.file_name().unwrap_or_default().to_string_lossy(),
                other_preferred.join(", ")
            );
        }
        return Ok(chosen.clone());
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
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() == 0) {
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
    if let Some(path) = beads_db_env_file() {
        return load_issues_from_path(&path);
    }
    let beads_dir = get_beads_dir(repo_path)?;
    load_issues_from_beads_dir(&beads_dir)
}

// ---------------------------------------------------------------------------
// Source authority: which on-disk store a `.beads` directory is read from.
//
// beads_rust (`br`) keeps its live state in a SQLite database and mirrors it to
// a JSONL export; `metadata.json` names both. The export can lag the database
// (no `br sync --flush-only` yet) and the database can lag the export (a `git
// pull` before `br sync --import-only`), so the freshest declared store wins.
// Dolt-native `bd` workspaces are read from a bounded live `bd export` on
// stdout, without creating a compatibility file. An existing `issues.jsonl`
// is used only with the explicit JSONL override; other JSONL is never used.
// ---------------------------------------------------------------------------

/// Env var naming a specific database/JSONL file or a `.beads` directory.
/// Takes priority over `BEADS_DIR`.
pub const BEADS_DB_ENV: &str = "BEADS_DB";

/// Env var forcing the store read from a `.beads` directory:
/// `jsonl`, `sqlite`, or `auto` (default: live Dolt or freshest br store).
pub const DATA_SOURCE_ENV: &str = "BV_DATA_SOURCE";

/// Bounds for `.beads/redirect` resolution, matching br's routing limits so
/// bvr and br agree on the target store.
const MAX_REDIRECT_BYTES: u64 = 4096;
const MAX_REDIRECT_DEPTH: usize = 10;

/// `.beads/metadata.json` as written by br and bd.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeadsMetadata {
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub jsonl_export: String,
    #[serde(default)]
    pub backend: String,
}

#[must_use]
pub fn read_beads_metadata(beads_dir: &Path) -> Option<BeadsMetadata> {
    let text = std::fs::read_to_string(beads_dir.join("metadata.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// A concrete store issues are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueSource {
    Jsonl(PathBuf),
    Sqlite(PathBuf),
    Dolt(PathBuf),
}

impl IssueSource {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Jsonl(path) | Self::Sqlite(path) | Self::Dolt(path) => path,
        }
    }

    /// Sources to poll for changes: a JSONL file, SQLite and its WAL, or a
    /// Dolt directory whose live snapshot must be checked through the loader.
    #[must_use]
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Jsonl(path) | Self::Dolt(path) => vec![path.clone()],
            Self::Sqlite(path) => vec![path.clone(), sqlite_wal_path(path)],
        }
    }
}

fn sqlite_wal_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// True for a Dolt-native `bd` workspace: `.beads/dolt/` (server mode) or
/// `.beads/embeddeddolt/` (embedded, bd 1.1+), or metadata declaring
/// `backend: dolt`.
#[must_use]
pub fn is_bd_workspace(beads_dir: &Path) -> bool {
    if let Some(metadata) = read_beads_metadata(beads_dir) {
        let backend = metadata.backend.trim();
        if !backend.is_empty() {
            return backend.eq_ignore_ascii_case("dolt");
        }
        // A declared SQLite store takes precedence over leftover Dolt files.
        if is_sqlite_path(Path::new(metadata.database.trim())) {
            return false;
        }
    }
    ["dolt", "embeddeddolt"]
        .iter()
        .any(|dir| beads_dir.join(dir).is_dir())
}

fn is_sqlite_path(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "db" | "sqlite" | "sqlite3"
        )
    })
}

/// `BEADS_DB` when it names an existing file: that exact file is the source.
fn beads_db_env_file() -> Option<PathBuf> {
    let raw = std::env::var(BEADS_DB_ENV).ok()?;
    let path = PathBuf::from(raw.trim());
    (!raw.trim().is_empty() && path.is_file()).then_some(path)
}

/// Follow a `.beads/redirect` chain to its terminal beads directory.
///
/// This mirrors `br where`, so bvr reads the store br writes. Without a redirect file
/// the directory is returned unchanged. A malformed chain (oversized, loop,
/// missing target, or a target that is not a `.beads`/`_beads` directory) is
/// an error rather than a silent fall back to the stale local directory.
pub fn resolve_beads_redirect(beads_dir: &Path) -> Result<PathBuf> {
    let mut current = beads_dir.to_path_buf();
    let mut visited = HashSet::<PathBuf>::new();
    visited.insert(current.clone());

    for depth in 0.. {
        let redirect = current.join("redirect");
        let Ok(meta) = std::fs::metadata(&redirect) else {
            break;
        };
        if !meta.is_file() {
            return Err(BvrError::InvalidArgument(format!(
                "redirect path is not a regular file: {}",
                redirect.display()
            )));
        }
        if meta.len() > MAX_REDIRECT_BYTES {
            return Err(BvrError::InvalidArgument(format!(
                "redirect file exceeds {MAX_REDIRECT_BYTES} bytes: {}",
                redirect.display()
            )));
        }
        let text = std::fs::read_to_string(&redirect).map_err(|error| {
            BvrError::InvalidArgument(format!(
                "failed to read redirect file {}: {error}",
                redirect.display()
            ))
        })?;
        let target = text.trim();
        if target.is_empty() {
            break;
        }
        if depth >= MAX_REDIRECT_DEPTH {
            return Err(BvrError::InvalidArgument(format!(
                "redirect chain exceeds max depth ({MAX_REDIRECT_DEPTH}): {}",
                beads_dir.display()
            )));
        }
        let target = if Path::new(target).is_absolute() {
            PathBuf::from(target)
        } else {
            current.join(target)
        };
        let target = normalize_path_for_identity(&target);
        if target == current {
            break;
        }
        if !visited.insert(target.clone()) {
            return Err(BvrError::InvalidArgument(format!(
                "redirect loop detected: {} -> {}",
                current.display(),
                target.display()
            )));
        }
        current = target;
    }

    if current == beads_dir {
        return Ok(current);
    }
    if !current.is_dir() {
        return Err(BvrError::InvalidArgument(format!(
            "redirect target not found: {}",
            current.display()
        )));
    }
    let base = current
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    if base != ".beads" && base != "_beads" {
        return Err(BvrError::InvalidArgument(format!(
            "redirect target must be a .beads or _beads directory: {}",
            current.display()
        )));
    }
    Ok(current)
}

fn resolve_metadata_path(beads_dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let path = Path::new(name);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        beads_dir.join(path)
    };
    path.is_file().then_some(path)
}

fn newest_mtime(paths: &[PathBuf]) -> Option<std::time::SystemTime> {
    paths
        .iter()
        .filter_map(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        })
        .max()
}

/// Pick the store to read without launching a backend or writing any files.
pub fn select_issue_source(beads_dir: &Path) -> Result<IssueSource> {
    let mode = std::env::var(DATA_SOURCE_ENV)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(mode.as_str(), "" | "auto" | "jsonl" | "sqlite") {
        return Err(BvrError::InvalidArgument(format!(
            "{DATA_SOURCE_ENV} must be auto, jsonl, or sqlite (got {mode:?})"
        )));
    }

    if is_bd_workspace(beads_dir) {
        return match mode.as_str() {
            "jsonl" => {
                let path = beads_dir.join("issues.jsonl");
                if path.is_file() {
                    Ok(IssueSource::Jsonl(path))
                } else {
                    Err(BvrError::InvalidArgument(format!(
                        "{DATA_SOURCE_ENV}=jsonl requires an existing {}",
                        path.display()
                    )))
                }
            }
            "sqlite" => Err(BvrError::InvalidArgument(format!(
                "{DATA_SOURCE_ENV}=sqlite cannot read a Dolt workspace: {}",
                beads_dir.display()
            ))),
            _ => Ok(IssueSource::Dolt(std::fs::canonicalize(beads_dir)?)),
        };
    }

    let metadata = read_beads_metadata(beads_dir).unwrap_or_default();
    let jsonl = resolve_metadata_path(beads_dir, &metadata.jsonl_export)
        .map_or_else(|| find_jsonl_path(beads_dir), Ok);
    let database = resolve_metadata_path(beads_dir, &metadata.database);
    if mode == "jsonl" {
        return jsonl.map(IssueSource::Jsonl);
    }
    if mode == "sqlite" {
        return database.map(IssueSource::Sqlite).ok_or_else(|| {
            BvrError::InvalidArgument(format!(
                "{DATA_SOURCE_ENV}=sqlite requires a declared database in {}",
                beads_dir.display()
            ))
        });
    }
    match (database, jsonl) {
        (Some(db), Ok(jsonl)) => {
            let db_source = IssueSource::Sqlite(db);
            let db_time = newest_mtime(&db_source.watch_paths());
            let jsonl_time = newest_mtime(std::slice::from_ref(&jsonl));
            if db_time >= jsonl_time {
                Ok(db_source)
            } else {
                Ok(IssueSource::Jsonl(jsonl))
            }
        }
        (Some(db), Err(_)) => Ok(IssueSource::Sqlite(db)),
        (None, jsonl) => jsonl.map(IssueSource::Jsonl),
    }
}

const BD_EXPORT_TIMEOUT: Duration = Duration::from_secs(5);
const BD_EXPORT_SUCCESS_TTL: Duration = Duration::from_secs(2);
const BD_EXPORT_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const BD_EXPORT_MAX_STDOUT: u64 = 128 * 1024 * 1024;
const BD_EXPORT_MAX_STDERR: u64 = 1024 * 1024;
const BD_EXPORT_DIAGNOSTIC_BYTES: u64 = 4096;

type BdSnapshotResult = std::result::Result<Vec<Issue>, String>;

struct BdExportSnapshot {
    refresh_at: Instant,
    result: BdSnapshotResult,
}

type BdExportCache = HashMap<PathBuf, Arc<Mutex<Option<BdExportSnapshot>>>>;
static BD_EXPORT_CACHE: OnceLock<Mutex<BdExportCache>> = OnceLock::new();

/// Hash the already loaded snapshot for a canonical Dolt directory without
/// refreshing it or doing I/O. Watch initialization uses this even after the
/// TTL expires, so its token describes the data actually exported. Ordinary
/// polling must still use the live loader to discover subsequent changes.
#[must_use]
pub fn cached_dolt_snapshot_hash(beads_dir: &Path) -> Option<[u8; 32]> {
    let entry = BD_EXPORT_CACHE
        .get()?
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(beads_dir)
        .cloned()?;
    let snapshot = entry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let issues = snapshot.as_ref()?.result.as_ref().ok()?;
    Some(crate::robot::compute_snapshot_hash(issues))
}

fn load_issues_from_bd(beads_dir: &Path) -> Result<Vec<Issue>> {
    // Source selection canonicalizes this path, so redirects share one cache.
    // Only callers for the same workspace wait for its export; the global map
    // lock is released before running a child process.
    let entry = {
        let mut cache = BD_EXPORT_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            cache
                .entry(beads_dir.to_path_buf())
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        )
    };
    let mut snapshot = entry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = if let Some(cached) = snapshot
        .as_ref()
        .filter(|cached| Instant::now() < cached.refresh_at)
    {
        cached.result.clone()
    } else {
        let result = read_bd_export(beads_dir);
        let ttl = if result.is_ok() {
            BD_EXPORT_SUCCESS_TTL
        } else {
            BD_EXPORT_FAILURE_BACKOFF
        };
        *snapshot = Some(BdExportSnapshot {
            refresh_at: Instant::now() + ttl,
            result: result.clone(),
        });
        result
    };
    result.map_err(|message| BvrError::DoltExport {
        beads_dir: beads_dir.to_path_buf(),
        message,
    })
}

fn read_bd_export(beads_dir: &Path) -> BdSnapshotResult {
    let repo_root = beads_dir
        .parent()
        .ok_or_else(|| "the beads directory has no parent".to_string())?;
    // Anonymous private files avoid pipe deadlocks, unbounded capture buffers,
    // and waiting for descendants that inherit the exporter's output handles.
    // No compatibility snapshot is created or replaced inside the tracker.
    let mut stdout =
        tempfile::tempfile().map_err(|error| format!("could not capture stdout: {error}"))?;
    let mut stderr =
        tempfile::tempfile().map_err(|error| format!("could not capture stderr: {error}"))?;
    let mut child = Command::new("bd")
        .arg("export")
        .current_dir(repo_root)
        .env_remove(BEADS_DB_ENV)
        .env_remove("BD_DB")
        .env(BEADS_DIR_ENV, beads_dir)
        .stdin(Stdio::null())
        .stdout(
            stdout
                .try_clone()
                .map_err(|error| format!("could not capture stdout: {error}"))?,
        )
        .stderr(
            stderr
                .try_clone()
                .map_err(|error| format!("could not capture stderr: {error}"))?,
        )
        .spawn()
        .map_err(|error| format!("could not start bd: {error}"))?;
    let status = match wait_for_bd_export(&mut child, &stdout, &stderr) {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    if !status.success() {
        stderr
            .rewind()
            .map_err(|error| format!("could not read stderr: {error}"))?;
        let mut diagnostic = Vec::new();
        stderr
            .take(BD_EXPORT_DIAGNOSTIC_BYTES)
            .read_to_end(&mut diagnostic)
            .map_err(|error| format!("could not read stderr: {error}"))?;
        return Err(format!(
            "{status}: {}",
            String::from_utf8_lossy(&diagnostic).trim()
        ));
    }
    let output_len = stdout
        .metadata()
        .map_err(|error| format!("could not inspect stdout: {error}"))?
        .len();
    if output_len > BD_EXPORT_MAX_STDOUT {
        return Err(format!("stdout exceeded {BD_EXPORT_MAX_STDOUT} bytes"));
    }
    stdout
        .rewind()
        .map_err(|error| format!("could not read stdout: {error}"))?;
    // Take only the completed snapshot, even if a descendant retains a handle.
    parse_bd_export(BufReader::new(stdout.take(output_len)))
}

fn wait_for_bd_export(
    child: &mut Child,
    stdout: &File,
    stderr: &File,
) -> std::result::Result<ExitStatus, String> {
    let started = Instant::now();
    loop {
        let status = child
            .try_wait()
            .map_err(|error| format!("could not wait for bd: {error}"))?;
        for (name, file, limit) in [
            ("stdout", stdout, BD_EXPORT_MAX_STDOUT),
            ("stderr", stderr, BD_EXPORT_MAX_STDERR),
        ] {
            if file
                .metadata()
                .map_err(|error| format!("could not inspect {name}: {error}"))?
                .len()
                > limit
            {
                return Err(format!("{name} exceeded {limit} bytes"));
            }
        }
        if let Some(status) = status {
            return Ok(status);
        }
        if started.elapsed() >= BD_EXPORT_TIMEOUT {
            return Err(format!(
                "timed out after {} seconds",
                BD_EXPORT_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Parse a complete live snapshot strictly. A corrupt or partial export must
/// never become a successful partial database or silently select stale JSONL.
fn parse_bd_export(mut reader: impl BufRead) -> BdSnapshotResult {
    let mut issues = Vec::new();
    let mut ids = HashSet::new();
    let mut line = String::new();
    let mut line_no = 0usize;
    loop {
        line.clear();
        let bytes = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_line(&mut line)
            .map_err(|error| format!("could not read JSONL line {}: {error}", line_no + 1))?;
        if bytes == 0 {
            break;
        }
        line_no += 1;
        if bytes > MAX_LINE_BYTES {
            return Err(format!(
                "JSONL line {line_no} exceeded {MAX_LINE_BYTES} bytes"
            ));
        }
        let trimmed = if line_no == 1 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        if trimmed.is_empty() {
            continue;
        }
        let record: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|error| format!("invalid JSON on line {line_no}: {error}"))?;
        if let Some(kind) = record.get("_type") {
            match kind.as_str() {
                // bd 1.0 includes memories by default; newer releases omit them.
                Some("memory") => continue,
                Some("issue") => {}
                _ => return Err(format!("unknown record type on JSONL line {line_no}")),
            }
        }
        let mut issue: Issue = serde_json::from_value(record)
            .map_err(|error| format!("invalid issue on JSONL line {line_no}: {error}"))?;
        issue.status = issue.normalized_status();
        issue
            .validate()
            .map_err(|error| format!("invalid issue on JSONL line {line_no}: {error}"))?;
        if !ids.insert(issue.id.clone()) {
            return Err(format!(
                "duplicate issue ID {:?} on JSONL line {line_no}",
                issue.id
            ));
        }
        issues.push(issue);
    }
    Ok(issues)
}

/// Load issues from a `.beads` directory through its selected store. A
/// failing SQLite read falls back to the JSONL export with a warning.
pub fn load_issues_from_beads_dir(beads_dir: &Path) -> Result<Vec<Issue>> {
    match select_issue_source(beads_dir)? {
        IssueSource::Jsonl(path) => load_issues_from_file(&path),
        IssueSource::Dolt(path) => load_issues_from_bd(&path),
        IssueSource::Sqlite(db) => match load_issues_from_sqlite(&db) {
            Ok(issues) => Ok(issues),
            Err(error) => {
                if std::env::var(DATA_SOURCE_ENV)
                    .is_ok_and(|mode| mode.trim().eq_ignore_ascii_case("sqlite"))
                {
                    return Err(error);
                }
                warn(format!(
                    "could not read {} ({error}); falling back to the JSONL export",
                    db.display()
                ));
                let metadata = read_beads_metadata(beads_dir).unwrap_or_default();
                let jsonl = resolve_metadata_path(beads_dir, &metadata.jsonl_export)
                    .map_or_else(|| find_jsonl_path(beads_dir), Ok)?;
                load_issues_from_file(&jsonl)
            }
        },
    }
}

/// Load issues from an explicit path: a `.beads` directory, a SQLite
/// database (`.db`/`.sqlite`/`.sqlite3`), or a JSONL file.
pub fn load_issues_from_path(path: &Path) -> Result<Vec<Issue>> {
    if path.is_dir() {
        let beads_dir = resolve_beads_redirect(path)?;
        return load_issues_from_beads_dir(&beads_dir);
    }
    if is_sqlite_path(path) {
        return load_issues_from_sqlite(path);
    }
    load_issues_from_file(path)
}

/// Watch paths for the store a `.beads` directory currently reads from.
pub fn issue_source_watch_paths(beads_dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(select_issue_source(beads_dir)?.watch_paths())
}

/// Watch the same source as an explicit file/directory load, including redirects
/// and SQLite WAL writes.
pub fn issue_path_watch_paths(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_dir() {
        return issue_source_watch_paths(&resolve_beads_redirect(path)?);
    }
    if is_sqlite_path(path) {
        return Ok(IssueSource::Sqlite(path.to_path_buf()).watch_paths());
    }
    Ok(vec![path.to_path_buf()])
}

/// Watch the same source as `load_issues`, including a BEADS_DB file override.
pub fn issue_watch_paths(repo_path: Option<&Path>) -> Result<Vec<PathBuf>> {
    if let Some(path) = beads_db_env_file() {
        return issue_path_watch_paths(&path);
    }
    issue_source_watch_paths(&get_beads_dir(repo_path)?)
}

fn parse_sqlite_time(raw: Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = raw?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(parsed.with_timezone(&chrono::Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Some(naive.and_utc());
        }
    }
    chrono::DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z")
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

fn prepare_sqlite_read<'conn>(
    conn: &'conn rusqlite::Connection,
    query: &str,
) -> rusqlite::Result<rusqlite::Statement<'conn>> {
    // A database can forge sqlite_schema.rootpage, so checking that catalog
    // value alone must never authorize a virtual-table module to run.
    conn.prepare_with_flags(query, rusqlite::PrepFlags::SQLITE_PREPARE_NO_VTAB)
}

fn sqlite_columns(conn: &rusqlite::Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    // table is one of the four fixed source names below, never database text.
    let mut stmt = prepare_sqlite_read(conn, &format!("PRAGMA main.table_info({table})"))?;
    stmt.query_map([], |row| {
        row.get::<_, String>(1)
            .map(|name| name.to_ascii_lowercase())
    })?
    .collect()
}

/// Read issues straight from a beads SQLite database (br schema, tolerant of
/// older/variant schemas). Opened read-only; never writes. Ephemeral issues
/// are skipped exactly as br's JSONL export skips them.
pub fn load_issues_from_sqlite(path: &Path) -> Result<Vec<Issue>> {
    use rusqlite::{Connection, OpenFlags};

    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        BvrError::InvalidArgument(format!("sqlite read of {}: {error}", path.display()))
    })?;
    read_issues_from_sqlite_connection(&mut conn, path)
}

fn read_issues_from_sqlite_connection(
    conn: &mut rusqlite::Connection,
    path: &Path,
) -> Result<Vec<Issue>> {
    use rusqlite::{OptionalExtension, config::DbConfig};

    let sql_err = |error: rusqlite::Error| {
        BvrError::InvalidArgument(format!("sqlite read of {}: {error}", path.display()))
    };
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(sql_err)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(sql_err)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)
        .map_err(sql_err)?;

    // Hold the read snapshot from schema validation through all related rows.
    // In WAL mode a writer may otherwise replace a checked table with a view,
    // or commit new relationships after we have read the old issue rows.
    let conn = conn.transaction().map_err(sql_err)?;
    {
        let mut stmt = prepare_sqlite_read(
            &conn,
            "SELECT type, rootpage FROM main.sqlite_schema \
             WHERE name = ?1 COLLATE NOCASE AND type IN ('table', 'view')",
        )
        .map_err(sql_err)?;
        for table in ["issues", "labels", "dependencies", "comments"] {
            let object = stmt
                .query_row([table], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()
                .map_err(sql_err)?;
            if let Some((kind, rootpage)) = object
                && (kind != "table" || rootpage <= 0)
            {
                return Err(BvrError::InvalidArgument(format!(
                    "sqlite read of {}: {table} must be a stored table; \
                     views and virtual tables are not supported",
                    path.display()
                )));
            }
        }
    }

    // Validate every optional object before reading data. A present but invalid
    // schema is an error, rather than a silently incomplete issue snapshot.
    let issue_cols = sqlite_columns(&conn, "issues").map_err(sql_err)?;
    let label_cols = sqlite_columns(&conn, "labels").map_err(sql_err)?;
    let dep_cols = sqlite_columns(&conn, "dependencies").map_err(sql_err)?;
    let comment_cols = sqlite_columns(&conn, "comments").map_err(sql_err)?;
    if !issue_cols.contains("id") || !issue_cols.contains("title") {
        return Err(BvrError::InvalidArgument(format!(
            "{} has no beads issues table",
            path.display()
        )));
    }
    let col = |name: &str, fallback: &str| -> String {
        if issue_cols.contains(name) {
            format!("i.{name}")
        } else {
            fallback.to_string()
        }
    };
    let due_col = if issue_cols.contains("due_at") {
        "i.due_at".to_string()
    } else {
        col("due_date", "NULL")
    };
    let mut filters = Vec::new();
    if issue_cols.contains("ephemeral") {
        filters.push("COALESCE(i.ephemeral, 0) = 0");
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", filters.join(" AND "))
    };
    let query = format!(
        "SELECT i.id, i.title, {desc}, {design}, {ac}, {notes}, {status}, {priority}, \
         {itype}, {assignee}, {est}, {created}, {updated}, {due}, {defer}, {closed}, \
         {ext}, {repo}, {deleted} FROM main.issues i {where_clause} ORDER BY i.id",
        desc = col("description", "''"),
        design = col("design", "''"),
        ac = col("acceptance_criteria", "''"),
        notes = col("notes", "''"),
        status = col("status", "'open'"),
        priority = col("priority", "2"),
        itype = col("issue_type", "'task'"),
        assignee = col("assignee", "NULL"),
        est = col("estimated_minutes", "NULL"),
        created = col("created_at", "NULL"),
        updated = col("updated_at", "NULL"),
        due = due_col,
        defer = col("defer_until", "NULL"),
        closed = col("closed_at", "NULL"),
        ext = col("external_ref", "NULL"),
        repo = col("source_repo", "NULL"),
        deleted = col("deleted_at", "NULL"),
    );

    let mut issues = Vec::<Issue>::new();
    {
        let mut stmt = prepare_sqlite_read(&conn, &query).map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                let text = |idx: usize| -> rusqlite::Result<String> {
                    Ok(row.get::<_, Option<String>>(idx)?.unwrap_or_default())
                };
                let mut issue = Issue {
                    id: text(0)?,
                    title: text(1)?,
                    description: text(2)?,
                    design: text(3)?,
                    acceptance_criteria: text(4)?,
                    notes: text(5)?,
                    status: text(6)?,
                    priority: row.get::<_, Option<i64>>(7)?.unwrap_or(2) as i32,
                    issue_type: text(8)?,
                    assignee: text(9)?,
                    estimated_minutes: row.get::<_, Option<i64>>(10)?.map(|v| v as i32),
                    created_at: parse_sqlite_time(row.get(11)?),
                    updated_at: parse_sqlite_time(row.get(12)?),
                    due_date: parse_sqlite_time(row.get(13)?),
                    defer_until: parse_sqlite_time(row.get(14)?),
                    closed_at: parse_sqlite_time(row.get(15)?),
                    external_ref: row
                        .get::<_, Option<String>>(16)?
                        .filter(|value| !value.trim().is_empty()),
                    source_repo: text(17)?,
                    ..Issue::default()
                };
                if row.get::<_, Option<String>>(18)?.is_some()
                    && !issue.status.eq_ignore_ascii_case("tombstone")
                {
                    issue.status = "tombstone".to_string();
                }
                Ok(issue)
            })
            .map_err(sql_err)?;
        for row in rows {
            let mut issue = row.map_err(sql_err)?;
            issue.status = issue.normalized_status();
            if let Err(error) = issue.validate() {
                warn(format!(
                    "skipping invalid issue {} in {}: {error}",
                    issue.id,
                    path.display()
                ));
                continue;
            }
            issues.push(issue);
        }
    }

    let index: HashMap<String, usize> = issues
        .iter()
        .enumerate()
        .map(|(idx, issue)| (issue.id.clone(), idx))
        .collect();

    if label_cols.contains("label") {
        let mut stmt = prepare_sqlite_read(
            &conn,
            "SELECT issue_id, label FROM main.labels ORDER BY issue_id, label",
        )
        .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        for (issue_id, label) in rows.filter_map(std::result::Result::ok) {
            if let Some(&idx) = index.get(&issue_id) {
                issues[idx].labels.push(label);
            }
        }
    }

    if dep_cols.contains("issue_id") && dep_cols.contains("depends_on_id") {
        let type_col = if dep_cols.contains("type") {
            "type"
        } else if dep_cols.contains("dependency_type") {
            "dependency_type"
        } else {
            "''"
        };
        let created_by = if dep_cols.contains("created_by") {
            "created_by"
        } else {
            "''"
        };
        let created_at = if dep_cols.contains("created_at") {
            "created_at"
        } else {
            "NULL"
        };
        let mut stmt = prepare_sqlite_read(
            &conn,
            &format!(
                "SELECT issue_id, depends_on_id, {type_col}, {created_by}, {created_at} \
                 FROM main.dependencies ORDER BY issue_id, depends_on_id"
            ),
        )
        .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::model::Dependency {
                    issue_id: row.get(0)?,
                    depends_on_id: row.get(1)?,
                    dep_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    created_by: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    created_at: parse_sqlite_time(row.get(4)?),
                })
            })
            .map_err(sql_err)?;
        for dep in rows.filter_map(std::result::Result::ok) {
            if dep.depends_on_id.trim().is_empty() {
                continue;
            }
            if let Some(&idx) = index.get(&dep.issue_id) {
                issues[idx].dependencies.push(dep);
            }
        }
    }

    if comment_cols.contains("issue_id") {
        let text_col = if comment_cols.contains("text") {
            "text"
        } else if comment_cols.contains("body") {
            "body"
        } else {
            "''"
        };
        let id_col = if comment_cols.contains("id") {
            "id"
        } else {
            "0"
        };
        let author_col = if comment_cols.contains("author") {
            "author"
        } else {
            "''"
        };
        let created_col = if comment_cols.contains("created_at") {
            "created_at"
        } else {
            "NULL"
        };
        let mut stmt = prepare_sqlite_read(
            &conn,
            &format!(
                "SELECT {id_col} AS comment_id, issue_id, {author_col}, {text_col}, \
                 {created_col} AS comment_created_at FROM main.comments \
                 ORDER BY issue_id, comment_created_at, comment_id"
            ),
        )
        .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::model::Comment {
                    id: row.get::<_, Option<i64>>(0)?.unwrap_or_default(),
                    issue_id: row.get(1)?,
                    author: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    text: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    created_at: parse_sqlite_time(row.get(4)?),
                })
            })
            .map_err(sql_err)?;
        for comment in rows.filter_map(std::result::Result::ok) {
            if let Some(&idx) = index.get(&comment.issue_id) {
                issues[idx].comments.push(comment);
            }
        }
    }

    Ok(deduplicate_issues(issues))
}

pub fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig> {
    let config_text = std::fs::read_to_string(path)?;
    let mut config = serde_yaml::from_str::<WorkspaceConfig>(&config_text).map_err(|error| {
        BvrError::InvalidArgument(format!(
            "invalid workspace config {}: {error}",
            path.display()
        ))
    })?;

    config.apply_defaults();
    let workspace_root = resolve_workspace_root(path);
    config.repos = config.resolve_repos(&workspace_root, path)?;
    config.validate()?;
    Ok(config)
}

pub fn load_workspace_issues(path: &Path) -> Result<Vec<Issue>> {
    let (issues, _) = load_workspace_issues_with_summary(path)?;
    Ok(issues)
}

pub fn find_workspace_issue_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let config = load_workspace_config(path)?;
    let workspace_root = resolve_workspace_root(path);
    let mut paths = Vec::<PathBuf>::new();

    for repo in config.repos.iter().filter(|repo| repo.is_enabled()) {
        let repo_name = repo.effective_name();
        let repo_path = if Path::new(repo.path.trim()).is_absolute() {
            PathBuf::from(repo.path.trim())
        } else {
            let joined = workspace_root.join(repo.path.trim());
            std::fs::canonicalize(&joined).unwrap_or_else(|_| normalize_path_for_identity(&joined))
        };
        let beads_dir = repo_path.join(repo.effective_beads_path(Some(&config.defaults)));

        match resolve_beads_redirect(&beads_dir)
            .and_then(|beads_dir| issue_source_watch_paths(&beads_dir))
        {
            Ok(source_paths) => paths.extend(source_paths),
            Err(error) => warn(format!(
                "workspace repo '{repo_name}' watch source unavailable: {error}"
            )),
        }
    }

    if paths.is_empty() {
        return Err(BvrError::InvalidArgument(format!(
            "workspace has no readable issues.jsonl sources: {}",
            path.display()
        )));
    }

    Ok(paths)
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
            let joined = workspace_root.join(repo.path.trim());
            std::fs::canonicalize(&joined).unwrap_or_else(|_| normalize_path_for_identity(&joined))
        };
        let beads_dir = repo_path.join(repo.effective_beads_path(Some(&config.defaults)));

        let repo_result = (|| -> Result<Vec<Issue>> {
            let beads_dir = resolve_beads_redirect(&beads_dir)?;
            let mut issues = load_issues_from_beads_dir(&beads_dir)?;
            namespace_workspace_issues(&mut issues, &prefix, &repo_name, &known_prefixes);
            for issue in &mut issues {
                issue.workspace_repo_path = Some(repo_path.clone());
            }
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
            // A failed live export cannot establish a complete workspace
            // snapshot. Keep the previous TUI/export state instead of making
            // every issue in this repository disappear during a refresh.
            Err(error) if matches!(&error, BvrError::DoltExport { .. }) => return Err(error),
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

    if summary.successful_repos == 0 && summary.failed_repos > 0 {
        return Err(BvrError::InvalidArgument(format!(
            "workspace load failed for all repositories: {}",
            summary.failed_repo_names.join(", ")
        )));
    }

    Ok((issues, summary))
}

/// Deduplicate issues by ID, keeping the last occurrence (JSONL files
/// append updates, so later lines take precedence). Warns on duplicates.
fn deduplicate_issues(issues: Vec<Issue>) -> Vec<Issue> {
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(issues.len());
    let mut result = Vec::with_capacity(issues.len());
    for issue in issues {
        if let Some(&prev_idx) = seen.get(&issue.id) {
            warn(format!(
                "duplicate issue ID '{}': keeping later occurrence",
                issue.id
            ));
            let id = issue.id.clone();
            result[prev_idx] = issue;
            seen.insert(id, prev_idx);
        } else {
            seen.insert(issue.id.clone(), result.len());
            result.push(issue);
        }
    }
    result
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

    Ok(deduplicate_issues(issues))
}

/// Parse issues from JSONL text (e.g., from `git show` output).
pub fn parse_issues_from_text(text: &str) -> Result<Vec<Issue>> {
    let mut issues = Vec::new();
    for (line_no, raw_line) in text.lines().enumerate() {
        let trimmed = if line_no == 0 {
            raw_line.trim_start_matches('\u{feff}').trim()
        } else {
            raw_line.trim()
        };
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Issue>(trimmed) {
            Ok(mut issue) => {
                issue.status = issue.normalized_status();
                if let Err(error) = issue.validate() {
                    warn(format!(
                        "skipping invalid issue on line {}: {error}",
                        line_no + 1
                    ));
                    continue;
                }
                issues.push(issue);
            }
            Err(error) => {
                warn(format!(
                    "skipping malformed JSON on line {}: {error}",
                    line_no + 1
                ));
            }
        }
    }
    Ok(deduplicate_issues(issues))
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
    fn finds_preferred_file_with_multiple_candidates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads_dir = dir.path();
        std::fs::write(beads_dir.join("beads.jsonl"), "{}\n").expect("write beads");
        std::fs::write(beads_dir.join("issues.jsonl"), "{}\n").expect("write issues");
        std::fs::write(beads_dir.join("beads.base.jsonl"), "{}\n").expect("write base");

        let path = find_jsonl_path(beads_dir).expect("find path");
        assert!(
            path.ends_with("beads.jsonl"),
            "expected beads.jsonl to win, got: {}",
            path.display()
        );
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

    /// The temp root must not itself live under a repo that has a `.beads`
    /// ancestor: the upward directory search checks ancestors before the
    /// worktree fallback, so such an ancestor legitimately wins and the
    /// scenario under test never runs (seen when TMPDIR points inside a
    /// synced checkout on remote build workers).
    fn ancestor_beads_interferes(root: &std::path::Path) -> bool {
        root.ancestors().any(|a| a.join(".beads").is_dir())
    }

    /// Compare via canonicalize: on macOS the tempdir is handed out under
    /// `/var/...` while resolution yields `/private/var/...`.
    fn assert_same_dir(actual: &std::path::Path, expected: &std::path::Path) {
        assert_eq!(
            actual.canonicalize().expect("canonicalize actual"),
            expected.canonicalize().expect("canonicalize expected")
        );
    }

    #[test]
    fn get_beads_dir_falls_back_to_main_repo_for_git_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        if ancestor_beads_interferes(root) {
            eprintln!("skipping: tempdir has a .beads ancestor that wins the upward search");
            return;
        }
        let main_repo = root.join("main-repo");
        let worktree = root.join("worktree");
        let gitdir = main_repo.join(".git/worktrees/feature");

        std::fs::create_dir_all(main_repo.join(".beads")).expect("create main .beads");
        std::fs::create_dir_all(&gitdir).expect("create worktree gitdir");
        std::fs::create_dir_all(worktree.join("nested/path")).expect("create nested worktree");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect("write worktree .git file");
        std::fs::write(gitdir.join("commondir"), "../../\n").expect("write commondir");

        let beads_dir =
            get_beads_dir(Some(&worktree.join("nested/path"))).expect("find main repo .beads");
        assert_same_dir(&beads_dir, &main_repo.join(".beads"));
    }

    #[test]
    fn get_beads_dir_ignores_malformed_intermediate_git_file_during_worktree_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        if ancestor_beads_interferes(root) {
            eprintln!("skipping: tempdir has a .beads ancestor that wins the upward search");
            return;
        }
        let main_repo = root.join("main-repo");
        let worktree = root.join("worktree");
        let gitdir = main_repo.join(".git/worktrees/feature");
        let nested = worktree.join("nested/path");

        std::fs::create_dir_all(main_repo.join(".beads")).expect("create main .beads");
        std::fs::create_dir_all(&gitdir).expect("create worktree gitdir");
        std::fs::create_dir_all(&nested).expect("create nested worktree");
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect("write worktree .git file");
        std::fs::write(gitdir.join("commondir"), "../../\n").expect("write commondir");
        std::fs::write(worktree.join("nested/.git"), "not a gitdir pointer\n")
            .expect("write malformed nested .git file");

        let beads_dir = get_beads_dir(Some(&nested))
            .expect("skip malformed nested .git and find main repo .beads");
        assert_same_dir(&beads_dir, &main_repo.join(".beads"));
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
    fn find_jsonl_fallback_skips_empty_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads_dir = dir.path();
        std::fs::write(beads_dir.join("alpha.jsonl"), "").expect("write empty alpha");
        std::fs::write(beads_dir.join("zeta.jsonl"), "{}\n").expect("write zeta");

        let path = find_jsonl_path(beads_dir).expect("find fallback path");
        assert!(path.ends_with("zeta.jsonl"));
    }

    #[test]
    fn find_workspace_issue_paths_collects_enabled_repo_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/api/.beads")).expect("create api beads");
        std::fs::create_dir_all(root.join("apps/web/.beads")).expect("create web beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            concat!(
                "repos:\n",
                "  - name: api\n",
                "    path: services/api\n",
                "  - name: web\n",
                "    path: apps/web\n",
            ),
        )
        .expect("write workspace config");
        std::fs::write(root.join("services/api/.beads/issues.jsonl"), "{}\n")
            .expect("write api issues");
        std::fs::write(root.join("apps/web/.beads/issues.jsonl"), "{}\n").expect("write web");

        let mut paths =
            find_workspace_issue_paths(&root.join(".bv/workspace.yaml")).expect("watch paths");
        paths.sort();

        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("apps/web/.beads/issues.jsonl"));
        assert!(paths[1].ends_with("services/api/.beads/issues.jsonl"));
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
    fn load_workspace_config_applies_discovery_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("apps/web/.beads")).expect("create web beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            "discovery:\n  enabled: true\n",
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("apps/web/.beads/issues.jsonl"),
            "{\"id\":\"UI-1\",\"title\":\"UI\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write web issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");

        assert!(config.discovery.enabled);
        assert!(
            config
                .discovery
                .patterns
                .iter()
                .any(|pattern| pattern == "packages/*")
        );
        assert!(
            config
                .discovery
                .exclude
                .iter()
                .any(|pattern| pattern == "node_modules")
        );
        assert_eq!(config.discovery.max_depth, 2);
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].path, "apps/web");
        assert_eq!(config.repos[0].effective_prefix(), "web-");
    }

    #[test]
    fn load_workspace_config_reports_empty_discovery_with_guidance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        let config_path = root.join(".bv/workspace.yaml");
        std::fs::write(&config_path, "discovery:\n  enabled: true\n").expect("write config");

        let error = load_workspace_config(&config_path).expect_err("missing discovery repos");
        let message = error.to_string();
        assert!(message.contains("workspace discovery found no repositories"));
        assert!(message.contains("Patterns: ["));
        assert!(message.contains("defaults.beads_path"));
    }

    #[test]
    fn load_workspace_issues_discovers_repos_from_common_layouts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/api/.beads")).expect("create api .beads");
        std::fs::create_dir_all(root.join("apps/web/.beads")).expect("create web .beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            "discovery:\n  enabled: true\n",
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("services/api/.beads/issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"API Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");
        std::fs::write(
            root.join("apps/web/.beads/issues.jsonl"),
            "{\"id\":\"UI-1\",\"title\":\"Web UI\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write web issues");

        let (issues, summary) =
            load_workspace_issues_with_summary(&root.join(".bv/workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 2);
        assert_eq!(summary.successful_repos, 2);
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().any(|issue| issue.id == "api-AUTH-1"));
        assert!(issues.iter().any(|issue| issue.id == "web-UI-1"));
    }

    #[test]
    fn load_workspace_issues_discovers_workspace_root_repo_when_pattern_matches_dot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let root_name = workspace_root_repo_name(root);
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join(".beads")).expect("create root .beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            "discovery:\n  enabled: true\n  patterns: ['.']\n",
        )
        .expect("write workspace config");
        std::fs::write(
            root.join(".beads/issues.jsonl"),
            "{\"id\":\"ROOT-1\",\"title\":\"Workspace Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write root issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].path, ".");

        let (issues, summary) =
            load_workspace_issues_with_summary(&root.join(".bv/workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 1);
        assert_eq!(summary.successful_repos, 1);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].source_repo, root_name);
        assert_eq!(
            issues[0].id,
            format!("{}-ROOT-1", root_name.to_ascii_lowercase())
        );
    }

    #[test]
    fn load_workspace_issues_namespaces_explicit_workspace_root_repo_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let root_name = workspace_root_repo_name(root);
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join(".beads")).expect("create root .beads");
        std::fs::write(root.join(".bv/workspace.yaml"), "repos:\n  - path: ./\n")
            .expect("write workspace config");
        std::fs::write(
            root.join(".beads/issues.jsonl"),
            "{\"id\":\"ROOT-2\",\"title\":\"Explicit Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write root issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].effective_name(), root_name);
        assert_eq!(
            config.repos[0].effective_prefix(),
            format!("{}-", root_name.to_ascii_lowercase())
        );

        let (issues, summary) =
            load_workspace_issues_with_summary(&root.join(".bv/workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 1);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].source_repo, root_name);
        assert_eq!(
            issues[0].id,
            format!("{}-ROOT-2", issues[0].source_repo.to_ascii_lowercase())
        );
    }

    #[test]
    fn load_workspace_issues_namespaces_explicit_parent_segment_repo_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/.beads")).expect("create services .beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            "repos:\n  - path: services/api/..\n",
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("services/.beads/issues.jsonl"),
            "{\"id\":\"SRV-1\",\"title\":\"Service Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write services issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].effective_name(), "services");
        assert_eq!(config.repos[0].effective_prefix(), "services-");

        let (issues, summary) =
            load_workspace_issues_with_summary(&root.join(".bv/workspace.yaml"))
                .expect("load workspace issues");

        assert_eq!(summary.total_repos, 1);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].source_repo, "services");
        assert_eq!(issues[0].id, "services-SRV-1");
    }

    #[test]
    fn load_workspace_issues_applies_default_beads_path_to_explicit_and_discovered_repos() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/api/trackers")).expect("create api trackers");
        std::fs::create_dir_all(root.join("apps/web/trackers")).expect("create web trackers");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            concat!(
                "defaults:\n",
                "  beads_path: trackers\n",
                "discovery:\n",
                "  enabled: true\n",
                "repos:\n",
                "  - name: api\n",
                "    path: services/api\n",
            ),
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("services/api/trackers/issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"API Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");
        std::fs::write(
            root.join("apps/web/trackers/issues.jsonl"),
            "{\"id\":\"UI-1\",\"title\":\"Web UI\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write web issues");

        let mut paths =
            find_workspace_issue_paths(&root.join(".bv/workspace.yaml")).expect("watch paths");
        paths.sort();
        assert!(paths[0].ends_with("apps/web/trackers/issues.jsonl"));
        assert!(paths[1].ends_with("services/api/trackers/issues.jsonl"));

        let (issues, summary) =
            load_workspace_issues_with_summary(&root.join(".bv/workspace.yaml"))
                .expect("load workspace issues");
        assert_eq!(summary.total_repos, 2);
        assert!(issues.iter().any(|issue| issue.id == "api-AUTH-1"));
        assert!(issues.iter().any(|issue| issue.id == "web-UI-1"));
    }

    #[test]
    fn load_workspace_config_dedupes_explicit_repo_paths_with_dot_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/api/.beads")).expect("create api .beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            concat!(
                "discovery:\n",
                "  enabled: true\n",
                "repos:\n",
                "  - name: backend\n",
                "    path: services/./api\n",
                "    prefix: backend-\n",
            ),
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("services/api/.beads/issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"API Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");

        assert_eq!(
            config.repos.len(),
            1,
            "same repo should not be rediscovered"
        );
        assert_eq!(config.repos[0].name, "backend");
        assert_eq!(config.repos[0].path, "services/./api");
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
    fn load_workspace_issues_errors_when_all_repos_fail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let workspace_dir = root.join(".bv");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");

        std::fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - name: missing-api\n    path: services/api\n    prefix: api-\n  - name: missing-web\n    path: apps/web\n    prefix: web-\n",
        )
        .expect("write workspace config");

        let error = load_workspace_issues_with_summary(&workspace_dir.join("workspace.yaml"))
            .expect_err("all repo failures should bubble up");
        let message = error.to_string();

        assert!(message.contains("workspace load failed for all repositories"));
        assert!(message.contains("missing-api"));
        assert!(message.contains("missing-web"));
    }

    // --- qualify_id tests ---

    #[test]
    fn qualify_id_adds_prefix_when_missing() {
        assert_eq!(qualify_id("AUTH-1", "api-"), "api-AUTH-1");
    }

    #[test]
    fn qualify_id_skips_prefix_when_already_present() {
        assert_eq!(qualify_id("api-AUTH-1", "api-"), "api-AUTH-1");
    }

    #[test]
    fn qualify_id_case_insensitive_prefix_check() {
        assert_eq!(qualify_id("API-AUTH-1", "api-"), "API-AUTH-1");
    }

    // --- has_known_prefix tests ---

    #[test]
    fn has_known_prefix_matches() {
        let prefixes = vec!["api-".to_string(), "web-".to_string()];
        assert!(has_known_prefix("api-AUTH-1", &prefixes));
        assert!(has_known_prefix("web-UI-1", &prefixes));
    }

    #[test]
    fn has_known_prefix_no_match() {
        let prefixes = vec!["api-".to_string()];
        assert!(!has_known_prefix("AUTH-1", &prefixes));
    }

    #[test]
    fn has_known_prefix_case_insensitive() {
        let prefixes = vec!["API-".to_string()];
        assert!(has_known_prefix("api-AUTH-1", &prefixes));
    }

    // --- WorkspaceRepoConfig methods ---

    #[test]
    fn repo_config_is_enabled_default_true() {
        let repo = WorkspaceRepoConfig::default();
        assert!(repo.is_enabled());
    }

    #[test]
    fn repo_config_is_enabled_explicit_false() {
        let repo = WorkspaceRepoConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!repo.is_enabled());
    }

    #[test]
    fn repo_config_effective_name_from_name_field() {
        let repo = WorkspaceRepoConfig {
            name: "  my-repo  ".to_string(),
            path: "/some/path/other".to_string(),
            ..Default::default()
        };
        assert_eq!(repo.effective_name(), "my-repo");
    }

    #[test]
    fn repo_config_effective_name_from_path_when_name_empty() {
        let repo = WorkspaceRepoConfig {
            path: "/projects/my-app".to_string(),
            ..Default::default()
        };
        assert_eq!(repo.effective_name(), "my-app");
    }

    #[test]
    fn repo_config_effective_prefix_from_prefix_field() {
        let repo = WorkspaceRepoConfig {
            prefix: "  custom-  ".to_string(),
            ..Default::default()
        };
        assert_eq!(repo.effective_prefix(), "custom-");
    }

    #[test]
    fn repo_config_effective_prefix_generated_from_name() {
        let repo = WorkspaceRepoConfig {
            name: "MyApp".to_string(),
            ..Default::default()
        };
        assert_eq!(repo.effective_prefix(), "myapp-");
    }

    #[test]
    fn repo_config_effective_beads_path_explicit() {
        let repo = WorkspaceRepoConfig {
            beads_path: "  custom/path  ".to_string(),
            ..Default::default()
        };
        assert_eq!(repo.effective_beads_path(None), "custom/path");
    }

    #[test]
    fn repo_config_effective_beads_path_from_defaults() {
        let repo = WorkspaceRepoConfig::default();
        let defaults = WorkspaceDefaultsConfig {
            beads_path: "trackers".to_string(),
        };
        assert_eq!(repo.effective_beads_path(Some(&defaults)), "trackers");
    }

    #[test]
    fn repo_config_effective_beads_path_fallback() {
        let repo = WorkspaceRepoConfig::default();
        assert_eq!(repo.effective_beads_path(None), ".beads");
    }

    // --- normalize_path_for_display ---

    #[test]
    fn normalize_path_replaces_backslashes() {
        let path = Path::new("foo\\bar\\baz");
        assert_eq!(normalize_path_for_display(path), "foo/bar/baz");
    }

    // --- relative_path_matches_pattern ---

    #[test]
    fn relative_path_matches_wildcard() {
        assert!(relative_path_matches_pattern(Path::new("apps"), "*"));
    }

    #[test]
    fn relative_path_matches_nested_wildcard() {
        assert!(relative_path_matches_pattern(
            Path::new("packages/auth"),
            "packages/*"
        ));
    }

    #[test]
    fn relative_path_no_match_wrong_depth() {
        assert!(!relative_path_matches_pattern(
            Path::new("packages/auth/src"),
            "packages/*"
        ));
    }

    #[test]
    fn relative_path_matches_exact() {
        assert!(relative_path_matches_pattern(
            Path::new("services/api"),
            "services/api"
        ));
    }

    #[test]
    fn relative_path_no_match_different_segment() {
        assert!(!relative_path_matches_pattern(
            Path::new("services/web"),
            "services/api"
        ));
    }

    // --- is_excluded_workspace_path ---

    #[test]
    fn excluded_by_component_name() {
        let excludes = vec!["node_modules".to_string()];
        assert!(is_excluded_workspace_path(
            Path::new("node_modules"),
            &excludes
        ));
    }

    #[test]
    fn excluded_by_nested_component() {
        let excludes = vec![".git".to_string()];
        assert!(is_excluded_workspace_path(
            Path::new("project/.git"),
            &excludes
        ));
    }

    #[test]
    fn not_excluded_when_no_match() {
        let excludes = vec!["node_modules".to_string()];
        assert!(!is_excluded_workspace_path(
            Path::new("packages/auth"),
            &excludes
        ));
    }

    // --- resolve_workspace_root ---

    #[test]
    fn resolve_workspace_root_goes_up_two_levels() {
        let path = Path::new("/project/.bv/workspace.yaml");
        assert_eq!(resolve_workspace_root(path), PathBuf::from("/project"));
    }

    // --- parse_issues_from_text ---

    #[test]
    fn parse_issues_from_text_valid() {
        let text = "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n{\"id\":\"B\",\"title\":\"B\",\"status\":\"closed\",\"priority\":2,\"issue_type\":\"bug\"}\n";
        let issues = parse_issues_from_text(text).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "A");
        assert_eq!(issues[1].id, "B");
    }

    #[test]
    fn parse_issues_from_text_skips_empty_lines() {
        let text = "\n{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n\n";
        let issues = parse_issues_from_text(text).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn parse_issues_from_text_strips_bom() {
        let text = "\u{feff}{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n";
        let issues = parse_issues_from_text(text).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn parse_issues_from_text_skips_malformed() {
        let text = "not json\n{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n";
        let issues = parse_issues_from_text(text).unwrap();
        assert_eq!(issues.len(), 1);
    }

    // --- load_issues_from_file edge cases ---

    #[test]
    fn load_issues_skips_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issues.jsonl");
        std::fs::write(
            &path,
            "\n{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n\n",
        )
        .unwrap();
        let issues = load_issues_from_file(&path).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn load_issues_strips_bom_on_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issues.jsonl");
        std::fs::write(
            &path,
            "\u{feff}{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .unwrap();
        let issues = load_issues_from_file(&path).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn load_issues_skips_malformed_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issues.jsonl");
        std::fs::write(
            &path,
            "not json\n{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .unwrap();
        let issues = load_issues_from_file(&path).unwrap();
        assert_eq!(issues.len(), 1);
    }

    // --- find_jsonl_path edge cases ---

    #[test]
    fn find_jsonl_skips_backup_files() {
        let dir = tempfile::tempdir().unwrap();
        let beads_dir = dir.path();
        std::fs::write(beads_dir.join("issues.backup.jsonl"), "{}\n").unwrap();
        std::fs::write(beads_dir.join("issues.orig.jsonl"), "{}\n").unwrap();
        std::fs::write(beads_dir.join("beads.left.jsonl"), "{}\n").unwrap();
        std::fs::write(beads_dir.join("deletions.jsonl"), "{}\n").unwrap();
        std::fs::write(beads_dir.join("valid.jsonl"), "{}\n").unwrap();

        let path = find_jsonl_path(beads_dir).unwrap();
        assert!(path.ends_with("valid.jsonl"));
    }

    #[test]
    fn find_jsonl_error_when_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_jsonl_path(dir.path());
        assert!(result.is_err());
    }

    // --- load_sprints edge cases ---

    #[test]
    fn load_sprints_from_nonexistent_returns_empty() {
        let path = Path::new("/nonexistent/sprints.jsonl");
        let sprints = load_sprints_from_file(path).unwrap();
        assert!(sprints.is_empty());
    }

    #[test]
    fn load_sprints_skips_sprint_without_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sprints.jsonl");
        std::fs::write(
            &path,
            "{\"id\":\"\",\"name\":\"Bad Sprint\",\"bead_ids\":[]}\n{\"id\":\"s1\",\"name\":\"Good\",\"bead_ids\":[]}\n",
        )
        .unwrap();
        let sprints = load_sprints_from_file(&path).unwrap();
        assert_eq!(sprints.len(), 1);
        assert_eq!(sprints[0].id, "s1");
    }

    // --- WorkspaceConfig::validate ---

    #[test]
    fn workspace_validate_rejects_empty_repos() {
        let config = WorkspaceConfig::default();
        assert!(config.validate().is_err());
    }

    #[test]
    fn workspace_validate_rejects_all_disabled() {
        let config = WorkspaceConfig {
            repos: vec![WorkspaceRepoConfig {
                path: "api".to_string(),
                enabled: Some(false),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn workspace_validate_rejects_empty_path() {
        let config = WorkspaceConfig {
            repos: vec![WorkspaceRepoConfig {
                path: "  ".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    // --- namespace_workspace_issues ---

    #[test]
    fn namespace_issues_qualifies_comment_issue_ids() {
        let mut issues = vec![Issue {
            id: "A".to_string(),
            title: "T".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            comments: vec![crate::model::Comment {
                id: 1,
                issue_id: "A".to_string(),
                author: "dev".to_string(),
                text: "hello".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        namespace_workspace_issues(&mut issues, "api-", "api", &[]);
        assert_eq!(issues[0].comments[0].issue_id, "api-A");
    }

    #[test]
    fn namespace_issues_sets_source_repo() {
        let mut issues = vec![Issue {
            id: "A".to_string(),
            title: "T".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Default::default()
        }];
        namespace_workspace_issues(&mut issues, "api-", "my-api", &[]);
        assert_eq!(issues[0].source_repo, "my-api");
    }

    #[test]
    fn tracker_commands_keep_native_ids_that_already_carry_the_prefix() {
        let issue = |id: &str| Issue {
            id: id.to_string(),
            title: "T".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Default::default()
        };
        let mut issues = vec![issue("api-12"), issue("bd-7")];
        namespace_workspace_issues(&mut issues, "api-", "api", &[]);
        assert_eq!(issues[0].id, "api-12");
        assert_eq!(
            issues[0].claim_command(),
            "br update api-12 --status=in_progress"
        );
        assert_eq!(issues[1].id, "api-bd-7");
        assert_eq!(
            issues[1].claim_command(),
            "br update bd-7 --status=in_progress"
        );

        // A dashless prefix must not turn the ID into `-12`, which br would
        // parse as a flag.
        let mut dashless = vec![issue("api-12")];
        namespace_workspace_issues(&mut dashless, "api", "api", &[]);
        assert_eq!(dashless[0].show_command(), "br show api-12");
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

    #[test]
    fn load_workspace_config_rejects_duplicate_repo_path_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("services/api/.beads")).expect("create repo");

        let workspace_dir = root.join(".bv");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        let config_path = workspace_dir.join("workspace.yaml");
        std::fs::write(
            &config_path,
            concat!(
                "repos:\n",
                "  - path: services/api\n",
                "    prefix: api-\n",
                "  - path: services/./api\n",
                "    prefix: backend-\n",
            ),
        )
        .expect("write config");

        let error =
            load_workspace_config(&config_path).expect_err("duplicate repo aliases rejected");
        assert!(error.to_string().contains("duplicates repository path"));
    }

    #[test]
    fn load_workspace_config_rejects_duplicate_missing_repo_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let workspace_dir = root.join(".bv");
        std::fs::create_dir_all(&workspace_dir).expect("create .bv");
        let config_path = workspace_dir.join("workspace.yaml");
        std::fs::write(
            &config_path,
            concat!(
                "repos:\n",
                "  - path: services/api\n",
                "    prefix: api-\n",
                "  - path: services/./api\n",
                "    prefix: backend-\n",
            ),
        )
        .expect("write config");

        let error = load_workspace_config(&config_path)
            .expect_err("duplicate missing repo aliases rejected");
        assert!(error.to_string().contains("duplicates repository path"));
    }

    #[test]
    fn load_workspace_config_discovery_ignores_disabled_explicit_repo_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".bv")).expect("create .bv");
        std::fs::create_dir_all(root.join("services/api/.beads")).expect("create api .beads");
        std::fs::write(
            root.join(".bv/workspace.yaml"),
            concat!(
                "discovery:\n",
                "  enabled: true\n",
                "repos:\n",
                "  - name: disabled-api\n",
                "    path: services/./api\n",
                "    prefix: disabled-\n",
                "    enabled: false\n",
            ),
        )
        .expect("write workspace config");
        std::fs::write(
            root.join("services/api/.beads/issues.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"API Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        let config =
            load_workspace_config(&root.join(".bv/workspace.yaml")).expect("load workspace config");

        assert_eq!(
            config.repos.len(),
            2,
            "disabled explicit alias should not block discovery"
        );
        assert!(
            config
                .repos
                .iter()
                .any(|repo| repo.is_enabled() && repo.path == "services/api"),
            "discovery should still include the real repo path"
        );
    }

    #[test]
    fn deduplicate_issues_keeps_last_occurrence() {
        let issues = vec![
            Issue {
                id: "A".into(),
                title: "first".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
            Issue {
                id: "B".into(),
                title: "B".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
            Issue {
                id: "A".into(),
                title: "updated".into(),
                status: "closed".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
        ];
        let deduped = super::deduplicate_issues(issues);
        assert_eq!(deduped.len(), 2);
        let a = deduped.iter().find(|i| i.id == "A").unwrap();
        assert_eq!(a.title, "updated");
        assert_eq!(a.status, "closed");
    }

    #[test]
    fn deduplicate_issues_no_duplicates_is_noop() {
        let issues = vec![
            Issue {
                id: "A".into(),
                title: "A".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
            Issue {
                id: "B".into(),
                title: "B".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
        ];
        let deduped = super::deduplicate_issues(issues);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn deduplicate_issues_empty_input() {
        let deduped = super::deduplicate_issues(vec![]);
        assert!(deduped.is_empty());
    }

    #[test]
    fn deduplicate_issues_preserves_order() {
        let issues = vec![
            Issue {
                id: "C".into(),
                title: "C".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
            Issue {
                id: "A".into(),
                title: "A".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
            Issue {
                id: "B".into(),
                title: "B".into(),
                status: "open".into(),
                issue_type: "task".into(),
                ..Issue::default()
            },
        ];
        let deduped = super::deduplicate_issues(issues);
        let ids: Vec<&str> = deduped.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["C", "A", "B"]);
    }

    /// Build a br-schema SQLite database (the subset bvr reads) at `path`.
    fn write_br_sqlite(path: &Path) {
        let conn = rusqlite::Connection::open(path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE issues (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                design TEXT NOT NULL DEFAULT '', acceptance_criteria TEXT NOT NULL DEFAULT '',
                notes TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'open',
                priority INTEGER NOT NULL DEFAULT 2, issue_type TEXT NOT NULL DEFAULT 'task',
                assignee TEXT, estimated_minutes INTEGER, created_at DATETIME, updated_at DATETIME,
                closed_at DATETIME, due_at DATETIME, defer_until DATETIME, external_ref TEXT,
                source_repo TEXT NOT NULL DEFAULT '.', deleted_at DATETIME,
                ephemeral INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE dependencies (issue_id TEXT NOT NULL, depends_on_id TEXT NOT NULL,
                type TEXT NOT NULL DEFAULT 'blocks', created_at DATETIME, created_by TEXT NOT NULL DEFAULT '');
             CREATE TABLE labels (issue_id TEXT NOT NULL, label TEXT NOT NULL);
             CREATE TABLE comments (id INTEGER PRIMARY KEY AUTOINCREMENT, issue_id TEXT NOT NULL,
                author TEXT NOT NULL, text TEXT NOT NULL, created_at DATETIME);
             INSERT INTO issues (id, title, status, priority, issue_type, assignee, created_at,
                updated_at, due_at, defer_until)
             VALUES ('bd-1', 'Root task', 'open', 1, 'task', NULL, '2026-01-01T00:00:00Z',
                '2026-01-02T00:00:00Z', '2026-02-01T00:00:00Z', '2099-01-01T00:00:00+00:00');
             INSERT INTO issues (id, title, status, priority, issue_type, assignee, created_at, updated_at)
             VALUES ('bd-2', 'Child task', 'in_progress', 2, 'feature', 'alice',
                '2026-01-01 00:00:00', '2026-01-03 00:00:00');
             INSERT INTO issues (id, title, ephemeral) VALUES ('bd-wisp', 'Ephemeral wisp', 1);
             INSERT INTO dependencies (issue_id, depends_on_id, type) VALUES ('bd-2', 'bd-1', 'blocks');
             INSERT INTO labels (issue_id, label) VALUES ('bd-1', 'backend'), ('bd-1', 'api');
             INSERT INTO comments (issue_id, author, text, created_at)
             VALUES ('bd-1', 'bob', 'looks good', '2026-01-02T12:00:00Z');",
        )
        .expect("seed sqlite");
    }

    #[test]
    fn loads_issues_from_br_sqlite_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("beads.db");
        write_br_sqlite(&db);

        let issues = load_issues_from_sqlite(&db).expect("load sqlite");
        let ids: Vec<&str> = issues.iter().map(|issue| issue.id.as_str()).collect();
        assert_eq!(ids, vec!["bd-1", "bd-2"], "ephemeral issues are skipped");

        let root = &issues[0];
        assert_eq!(root.priority, 1);
        assert_eq!(root.labels, vec!["api".to_string(), "backend".to_string()]);
        assert!(root.due_date.is_some(), "due_at maps to due_date");
        assert!(root.defer_until.is_some());
        assert_eq!(root.comments.len(), 1);
        assert_eq!(root.comments[0].text, "looks good");

        let child = &issues[1];
        assert_eq!(child.assignee, "alice");
        assert!(
            child.created_at.is_some(),
            "space-separated timestamps parse"
        );
        assert_eq!(child.dependencies.len(), 1);
        assert_eq!(child.dependencies[0].depends_on_id, "bd-1");
        assert!(child.dependencies[0].is_blocking());
    }

    #[test]
    fn rejects_sqlite_without_issues_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("other.db");
        rusqlite::Connection::open(&db)
            .expect("open")
            .execute_batch("CREATE TABLE unrelated (x INTEGER);")
            .expect("create");
        assert!(load_issues_from_sqlite(&db).is_err());
    }

    #[test]
    fn sqlite_rejects_recursive_source_views_before_reading_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (table, columns) in [
            (
                "IsSuEs",
                "'bd-1' AS id, 'recursive issue' AS title, CAST(sum(x) AS TEXT) AS description",
            ),
            (
                "LaBeLs",
                "'bd-1' AS issue_id, CAST(sum(x) AS TEXT) AS label",
            ),
            (
                "DePeNdEnCiEs",
                "'bd-1' AS issue_id, CAST(sum(x) AS TEXT) AS depends_on_id",
            ),
            (
                "CoMmEnTs",
                "1 AS id, 'bd-1' AS issue_id, CAST(sum(x) AS TEXT) AS text",
            ),
        ] {
            let db = dir.path().join(format!("{table}.db"));
            let conn = rusqlite::Connection::open(&db).expect("open fixture");
            if !table.eq_ignore_ascii_case("issues") {
                conn.execute_batch(
                    "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
                     INSERT INTO issues VALUES ('bd-1', 'stored issue');",
                )
                .expect("create ordinary issues");
            }
            // Finite work keeps a regression bounded even if the guard breaks.
            conn.execute_batch(&format!(
                "CREATE VIEW {table} AS WITH RECURSIVE ticks(x) AS (
                    VALUES(1) UNION ALL SELECT x + 1 FROM ticks WHERE x < 50000
                 ) SELECT {columns} FROM ticks;"
            ))
            .expect("create recursive source view");
            drop(conn);
            let before = std::fs::read(&db).expect("read original fixture");

            let error = load_issues_from_sqlite(&db)
                .expect_err("source views must not be evaluated")
                .to_string();
            assert!(
                error.contains(&format!(
                    "{} must be a stored table",
                    table.to_ascii_lowercase()
                )),
                "{error}"
            );
            assert!(error.contains(&db.display().to_string()), "{error}");
            assert_eq!(std::fs::read(&db).expect("read fixture after load"), before);
        }
    }

    #[test]
    fn sqlite_rejects_virtual_source_tables_even_with_forged_rootpage() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (table, columns) in [
            ("issues", "id, title"),
            ("labels", "issue_id, label"),
            ("dependencies", "issue_id, depends_on_id"),
            ("comments", "id, issue_id, text"),
        ] {
            for forged_rootpage in [false, true] {
                let db = dir
                    .path()
                    .join(format!("{table}-forged-{forged_rootpage}.db"));
                let conn = rusqlite::Connection::open(&db).expect("open fixture");
                if table != "issues" {
                    conn.execute_batch(
                        "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
                         INSERT INTO issues VALUES ('bd-1', 'stored issue');",
                    )
                    .expect("create ordinary issues");
                }
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE {table} USING fts5({columns});"
                ))
                .expect("create virtual source");
                if forged_rootpage {
                    conn.execute_batch(&format!(
                        "PRAGMA writable_schema=ON;
                         UPDATE sqlite_schema SET rootpage=2 WHERE name='{table}';
                         PRAGMA writable_schema=OFF;"
                    ))
                    .expect("forge positive catalog rootpage");
                }
                drop(conn);

                let error = load_issues_from_sqlite(&db)
                    .expect_err("virtual source must be rejected before module access")
                    .to_string();
                if forged_rootpage {
                    assert!(error.contains("no such table"), "{error}");
                    assert!(error.contains(table), "{error}");
                } else {
                    assert!(
                        error.contains(&format!("{table} must be a stored table")),
                        "{error}"
                    );
                }
                assert!(error.contains(&db.display().to_string()), "{error}");
            }
        }
    }

    #[test]
    fn sqlite_accepts_stored_variant_schemas_and_unrelated_sql_objects() {
        let dir = tempfile::tempdir().expect("tempdir");
        for without_rowid in [false, true] {
            let db = dir.path().join(format!("stored-{without_rowid}.db"));
            let conn = rusqlite::Connection::open(&db).expect("open fixture");
            let table_options = if without_rowid { "WITHOUT ROWID" } else { "" };
            conn.execute_batch(&format!(
                "CREATE TABLE IsSuEs (
                    Id TEXT PRIMARY KEY, TiTlE TEXT NOT NULL CHECK(length(TiTlE) > 0),
                    search_title TEXT GENERATED ALWAYS AS (lower(TiTlE)) VIRTUAL
                 ) {table_options};
                 INSERT INTO IsSuEs (Id, TiTlE) VALUES ('bd-1', 'Ordinary stored issue');
                 CREATE INDEX issue_title ON IsSuEs (lower(TiTlE));
                 CREATE TABLE LaBeLs (Issue_Id TEXT, LaBeL TEXT);
                 INSERT INTO LaBeLs VALUES ('bd-1', 'backend');
                 CREATE TABLE DePeNdEnCiEs (Issue_Id TEXT, Depends_On_Id TEXT, Dependency_Type TEXT);
                 INSERT INTO DePeNdEnCiEs VALUES ('bd-1', 'bd-0', 'blocks');
                 CREATE TABLE CoMmEnTs (Id INTEGER, Issue_Id TEXT, Body TEXT);
                 INSERT INTO CoMmEnTs VALUES (7, 'bd-1', 'variant comment');
                 CREATE VIRTUAL TABLE issues_search USING fts5(id, title);
                 CREATE VIEW unrelated_recursive AS WITH RECURSIVE ticks(x) AS (
                    VALUES(1) UNION ALL SELECT x + 1 FROM ticks
                 ) SELECT x FROM ticks;"
            ))
            .expect("create legitimate variant schema");
            drop(conn);

            let issues = load_issues_from_sqlite(&db).expect("load stored variant schema");
            assert_eq!(issues.len(), 1);
            let issue = &issues[0];
            assert_eq!(issue.id, "bd-1");
            assert_eq!(issue.title, "Ordinary stored issue");
            assert_eq!(issue.status, "open");
            assert_eq!(issue.priority, 2);
            assert_eq!(issue.labels, ["backend"]);
            assert_eq!(issue.dependencies.len(), 1);
            assert_eq!(issue.dependencies[0].depends_on_id, "bd-0");
            assert!(issue.dependencies[0].is_blocking());
            assert_eq!(issue.comments.len(), 1);
            assert_eq!(issue.comments[0].id, 7);
            assert_eq!(issue.comments[0].text, "variant comment");
        }
    }

    #[test]
    fn sqlite_reader_disables_untrusted_schema_function_evaluation() {
        use rusqlite::{Connection, OpenFlags, functions::FunctionFlags};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("schema-function.db");
        let writer = Connection::open(&db).expect("open writer");
        let trusted_calls = Arc::new(AtomicUsize::new(0));
        let callback_trusted_calls = Arc::clone(&trusted_calls);
        writer
            .create_scalar_function(
                "application_only",
                1,
                FunctionFlags::SQLITE_DETERMINISTIC,
                move |context| {
                    callback_trusted_calls.fetch_add(1, Ordering::SeqCst);
                    context
                        .get::<String>(0)
                        .map(|title| format!("Generated {title}"))
                },
            )
            .expect("register writer function");
        writer
            .execute_batch(
                "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
                 INSERT INTO issues VALUES ('bd-1', 'stored issue');",
            )
            .expect("create ordinary stored issue");

        let mut reader = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open read-only reader");
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        reader
            .create_scalar_function(
                "application_only",
                1,
                FunctionFlags::SQLITE_DETERMINISTIC,
                move |context| {
                    callback_calls.fetch_add(1, Ordering::SeqCst);
                    context
                        .get::<String>(0)
                        .map(|title| format!("Generated {title}"))
                },
            )
            .expect("register reader function");
        let issues = read_issues_from_sqlite_connection(&mut reader, &db)
            .expect("configure reader and load ordinary issue");
        assert_eq!(issues[0].title, "stored issue");

        // CHECK expressions are not evaluated by SELECT, and table_info omits
        // generated columns. Read a VIRTUAL column explicitly on the configured
        // connection so this regression exercises the trusted-schema boundary.
        writer
            .execute_batch(
                "ALTER TABLE issues ADD COLUMN description TEXT
                 GENERATED ALWAYS AS (application_only(title)) VIRTUAL;",
            )
            .expect("add schema expression after the initial read finishes");
        let trusted_description: String = writer
            .query_row("SELECT description FROM issues", [], |row| row.get(0))
            .expect("trusted control evaluates the generated expression");
        assert_eq!(trusted_description, "Generated stored issue");
        assert_eq!(trusted_calls.load(Ordering::SeqCst), 1);

        let error = prepare_sqlite_read(&reader, "SELECT description FROM main.issues")
            .and_then(|mut statement| statement.query_row([], |row| row.get::<_, String>(0)))
            .expect_err("untrusted schema expressions must be rejected when read")
            .to_string();
        assert!(
            error.contains("unsafe use of application_only()"),
            "{error}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn sqlite_loads_comments_without_ids_in_issue_and_timestamp_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("comments-without-ids.db");
        let conn = rusqlite::Connection::open(&db).expect("open fixture");
        conn.execute_batch(
            "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO issues VALUES ('bd-2', 'Second issue'), ('bd-1', 'First issue');
             CREATE TABLE comments (issue_id TEXT, author TEXT, body TEXT, created_at TEXT);
             INSERT INTO comments VALUES
                ('bd-2', 'Dan', 'second later', '2026-01-04T00:00:00Z'),
                ('bd-1', 'Bea', 'first later', '2026-01-03T00:00:00Z'),
                ('bd-2', 'Cara', 'second earlier', '2026-01-02T00:00:00Z'),
                ('bd-1', 'Ada', 'first earlier', '2026-01-01T00:00:00Z');",
        )
        .expect("create comments variant without id column");
        drop(conn);

        let issues = load_issues_from_sqlite(&db).expect("load comments without ids");
        assert_eq!(issues.len(), 2);
        for (issue, expected_id, expected_comments) in [
            (
                &issues[0],
                "bd-1",
                [("Ada", "first earlier"), ("Bea", "first later")],
            ),
            (
                &issues[1],
                "bd-2",
                [("Cara", "second earlier"), ("Dan", "second later")],
            ),
        ] {
            assert_eq!(issue.id, expected_id);
            assert_eq!(issue.comments.len(), 2);
            for (comment, (author, text)) in issue.comments.iter().zip(expected_comments) {
                assert_eq!(comment.id, 0);
                assert_eq!(comment.issue_id, expected_id);
                assert_eq!(comment.author, author);
                assert_eq!(comment.text, text);
                assert!(comment.created_at.is_some());
            }
            assert!(issue.comments[0].created_at < issue.comments[1].created_at);
        }
    }

    #[test]
    fn sqlite_loads_minimal_comments_without_ids_or_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("minimal-comments.db");
        let conn = rusqlite::Connection::open(&db).expect("open fixture");
        conn.execute_batch(
            "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO issues VALUES ('bd-1', 'Stored issue');
             CREATE TABLE comments (issue_id TEXT, text TEXT);
             INSERT INTO comments VALUES ('bd-1', 'Minimal comment');",
        )
        .expect("create minimal comments schema");
        drop(conn);

        let issues = load_issues_from_sqlite(&db).expect("load minimal comments");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].comments.len(), 1);
        let comment = &issues[0].comments[0];
        assert_eq!(comment.id, 0);
        assert_eq!(comment.issue_id, "bd-1");
        assert_eq!(comment.text, "Minimal comment");
        assert!(comment.author.is_empty());
        assert!(comment.created_at.is_none());
    }

    #[test]
    fn sqlite_reads_issues_and_relationships_from_one_wal_snapshot() {
        use rusqlite::{
            Connection, OpenFlags,
            hooks::{AuthAction, AuthContext, Authorization},
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("concurrent-update.db");
        write_br_sqlite(&db);
        let writer = Connection::open(&db).expect("open writer");
        writer
            .execute_batch("PRAGMA journal_mode=WAL;")
            .expect("enable WAL");
        let mut reader = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open read-only reader");
        let updated = Arc::new(AtomicBool::new(false));
        let callback_updated = Arc::clone(&updated);
        reader
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = context.action
                    && table_name.eq_ignore_ascii_case("labels")
                    && !callback_updated.swap(true, Ordering::SeqCst)
                {
                    writer
                        .execute_batch(
                            "BEGIN IMMEDIATE;
                             UPDATE issues SET title='New root task' WHERE id='bd-1';
                             UPDATE labels SET label=upper(label);
                             COMMIT;",
                        )
                        .expect("commit update between issue and label reads");
                }
                Authorization::Allow
            }))
            .expect("install deterministic writer hook");

        let issues = read_issues_from_sqlite_connection(&mut reader, &db)
            .expect("read consistent snapshot during writer commit");
        assert!(updated.load(Ordering::SeqCst));
        assert_eq!(issues[0].title, "Root task");
        assert_eq!(issues[0].labels, ["api", "backend"]);
        drop(reader);

        let updated_issues = load_issues_from_sqlite(&db).expect("read next committed snapshot");
        assert_eq!(updated_issues[0].title, "New root task");
        assert_eq!(updated_issues[0].labels, ["API", "BACKEND"]);
    }

    #[test]
    fn sqlite_schema_validation_and_rows_share_one_wal_snapshot() {
        use rusqlite::{
            Connection, OpenFlags,
            hooks::{AuthAction, AuthContext, Authorization},
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("concurrent-schema.db");
        write_br_sqlite(&db);
        let writer = Connection::open(&db).expect("open writer");
        writer
            .execute_batch("PRAGMA journal_mode=WAL;")
            .expect("enable WAL");
        let mut reader = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open read-only reader");
        let replaced = Arc::new(AtomicBool::new(false));
        let callback_replaced = Arc::clone(&replaced);
        reader
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read {
                    table_name,
                    column_name,
                } = context.action
                    && table_name.eq_ignore_ascii_case("issues")
                    && column_name.eq_ignore_ascii_case("title")
                    && !callback_replaced.swap(true, Ordering::SeqCst)
                {
                    writer
                        .execute_batch(
                            "BEGIN IMMEDIATE;
                             ALTER TABLE issues RENAME TO stored_issues;
                             CREATE VIEW issues AS
                                SELECT 'view-1' AS id, 'Replaced after validation' AS title;
                             COMMIT;",
                        )
                        .expect("replace source after catalog validation");
                }
                Authorization::Allow
            }))
            .expect("install deterministic schema writer hook");

        let issues = read_issues_from_sqlite_connection(&mut reader, &db)
            .expect("finish reading the validated stored-table snapshot");
        assert!(replaced.load(Ordering::SeqCst));
        assert_eq!(issues[0].id, "bd-1");
        assert_eq!(issues[0].title, "Root task");
        drop(reader);

        let error = load_issues_from_sqlite(&db)
            .expect_err("next load must reject the newly committed view")
            .to_string();
        assert!(error.contains("issues must be a stored table"), "{error}");
    }

    fn set_mtime(path: &Path, secs: i64) {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
        File::options()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(time)
            .expect("set mtime");
    }

    #[test]
    fn select_issue_source_prefers_freshest_declared_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads = dir.path();
        std::fs::write(
            beads.join("metadata.json"),
            r#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
        )
        .expect("metadata");
        write_br_sqlite(&beads.join("beads.db"));
        std::fs::write(
            beads.join("issues.jsonl"),
            "{\"id\":\"J-1\",\"title\":\"From JSONL\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
        )
        .expect("jsonl");

        // Database newer than export: un-flushed br writes win.
        set_mtime(&beads.join("issues.jsonl"), 1_000_000);
        set_mtime(&beads.join("beads.db"), 2_000_000);
        assert_eq!(
            select_issue_source(beads).expect("select"),
            IssueSource::Sqlite(beads.join("beads.db"))
        );
        let issues = load_issues_from_beads_dir(beads).expect("load");
        assert_eq!(issues.len(), 2);

        // Export newer than database (git pull before import): JSONL wins.
        set_mtime(&beads.join("issues.jsonl"), 3_000_000);
        assert_eq!(
            select_issue_source(beads).expect("select"),
            IssueSource::Jsonl(beads.join("issues.jsonl"))
        );
        let issues = load_issues_from_beads_dir(beads).expect("load");
        assert_eq!(issues[0].id, "J-1");
    }

    #[test]
    fn select_issue_source_without_metadata_uses_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("beads.jsonl"),
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
        )
        .expect("jsonl");
        assert_eq!(
            select_issue_source(dir.path()).expect("select"),
            IssueSource::Jsonl(dir.path().join("beads.jsonl"))
        );
    }

    #[test]
    fn corrupt_database_falls_back_to_jsonl_export() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads = dir.path();
        std::fs::write(
            beads.join("metadata.json"),
            r#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
        )
        .expect("metadata");
        std::fs::write(
            beads.join("issues.jsonl"),
            "{\"id\":\"J-1\",\"title\":\"From JSONL\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
        )
        .expect("jsonl");
        std::fs::write(beads.join("beads.db"), b"not a sqlite database").expect("db");
        set_mtime(&beads.join("issues.jsonl"), 1_000_000);
        set_mtime(&beads.join("beads.db"), 2_000_000);

        let issues = load_issues_from_beads_dir(beads).expect("fallback load");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "J-1");
    }

    #[test]
    fn bd_workspace_never_reads_stray_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let beads = dir.path().join(".beads");
        std::fs::create_dir_all(beads.join("embeddeddolt")).expect("dolt dir");
        std::fs::write(
            beads.join("interactions.jsonl"),
            "{\"id\":\"x\",\"title\":\"x\"}\n",
        )
        .expect("stray");
        assert!(is_bd_workspace(&beads));
        assert_eq!(
            select_issue_source(&beads).expect("live source"),
            IssueSource::Dolt(std::fs::canonicalize(&beads).expect("canonical beads"))
        );
        assert!(!beads.join("issues.jsonl").exists());

        std::fs::write(
            beads.join("issues.jsonl"),
            "{\"id\":\"B-1\",\"title\":\"bd issue\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
        )
        .expect("export");
        assert_eq!(
            select_issue_source(&beads).expect("select"),
            IssueSource::Dolt(std::fs::canonicalize(&beads).expect("canonical beads"))
        );
    }

    #[test]
    fn bd_export_parser_accepts_empty_and_mixed_issue_memory_snapshots() {
        assert!(
            parse_bd_export(&b" \n\n"[..])
                .expect("empty snapshot")
                .is_empty()
        );
        let jsonl = concat!(
            "\u{feff}",
            "{\"_type\":\"memory\",\"key\":\"context\",\"value\":\"not an issue\"}\n",
            "{\"_type\":\"issue\",\"id\":\"LIVE-1\",\"title\":\"Live\",",
            "\"status\":\" OPEN \",\"issue_type\":\"task\"}\n",
        );
        let issues = parse_bd_export(jsonl.as_bytes()).expect("complete snapshot");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "LIVE-1");
        assert_eq!(issues[0].status, "open");
    }

    #[test]
    fn bd_export_parser_rejects_partial_invalid_and_unknown_records() {
        let valid = r#"{"id":"LIVE-1","title":"Live","status":"open","issue_type":"task"}"#;
        for invalid in [
            "{unfinished",
            r#"{"id":"BAD","title":"","status":"open","issue_type":"task"}"#,
            r#"{"_type":"unknown","id":"BAD","title":"Unknown","status":"open","issue_type":"task"}"#,
            r#"{"_type":null,"id":"BAD","title":"Unknown","status":"open","issue_type":"task"}"#,
            "[]",
            valid,
        ] {
            let jsonl = format!("{valid}\n{invalid}\n");
            let error = parse_bd_export(jsonl.as_bytes()).expect_err("invalid second record");
            assert!(
                error.contains("line 2"),
                "must reject the second record: {error}"
            );
        }
    }

    #[test]
    fn bd_export_parser_bounds_lines_and_rejects_invalid_utf8() {
        let oversized = vec![b'x'; MAX_LINE_BYTES + 1];
        let error = parse_bd_export(oversized.as_slice()).expect_err("oversized line");
        assert!(error.contains("exceeded"), "{error}");
        assert!(parse_bd_export(&b"\xff\n"[..]).is_err());
    }

    #[test]
    fn dolt_watch_initial_token_uses_the_loaded_snapshot_even_after_expiry() {
        let directory = tempfile::tempdir().expect("cache identity");
        let beads_dir = std::fs::canonicalize(directory.path()).expect("canonical path");
        let issues = vec![Issue {
            id: "LIVE-1".to_string(),
            title: "Already exported".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Default::default()
        }];
        let expected = crate::robot::compute_snapshot_hash(&issues);
        let entry = Arc::new(Mutex::new(Some(BdExportSnapshot {
            refresh_at: Instant::now() - BD_EXPORT_SUCCESS_TTL,
            result: Ok(issues),
        })));
        BD_EXPORT_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("cache")
            .insert(beads_dir.clone(), Arc::clone(&entry));

        assert_eq!(cached_dolt_snapshot_hash(&beads_dir), Some(expected));
        {
            let mut snapshot = entry.lock().expect("snapshot");
            snapshot.as_mut().expect("entry").result = Err("backend unavailable".to_string());
        }
        assert_eq!(cached_dolt_snapshot_hash(&beads_dir), None);
    }

    #[test]
    fn bd_workspace_detected_from_metadata_backend() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("metadata.json"), r#"{"backend":"Dolt"}"#)
            .expect("metadata");
        assert!(is_bd_workspace(dir.path()));
        let plain = tempfile::tempdir().expect("tempdir");
        assert!(!is_bd_workspace(plain.path()));
    }

    #[test]
    fn redirect_chain_resolves_to_target_beads_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("worktree/.beads");
        let shared = dir.path().join("main/.beads");
        std::fs::create_dir_all(&local).expect("local");
        std::fs::create_dir_all(&shared).expect("shared");
        std::fs::write(local.join("redirect"), "../../main/.beads\n").expect("redirect");

        let resolved = resolve_beads_redirect(&local).expect("resolve");
        assert_eq!(
            std::fs::canonicalize(resolved).expect("canon"),
            std::fs::canonicalize(&shared).expect("canon")
        );

        // No redirect file: unchanged.
        assert_eq!(resolve_beads_redirect(&shared).expect("resolve"), shared);
    }

    #[test]
    fn redirect_errors_on_loop_and_bad_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a/.beads");
        let b = dir.path().join("b/.beads");
        std::fs::create_dir_all(&a).expect("a");
        std::fs::create_dir_all(&b).expect("b");
        std::fs::write(a.join("redirect"), b.display().to_string()).expect("a->b");
        std::fs::write(b.join("redirect"), a.display().to_string()).expect("b->a");
        let error = resolve_beads_redirect(&a).expect_err("loop");
        assert!(error.to_string().contains("loop"), "{error}");

        let c = dir.path().join("c/.beads");
        let not_beads = dir.path().join("elsewhere");
        std::fs::create_dir_all(&c).expect("c");
        std::fs::create_dir_all(&not_beads).expect("elsewhere");
        std::fs::write(c.join("redirect"), not_beads.display().to_string()).expect("c->x");
        let error = resolve_beads_redirect(&c).expect_err("bad target");
        assert!(error.to_string().contains(".beads"), "{error}");

        let d = dir.path().join("d/.beads");
        std::fs::create_dir_all(&d).expect("d");
        std::fs::write(
            d.join("redirect"),
            dir.path().join("missing/.beads").display().to_string(),
        )
        .expect("d->missing");
        assert!(resolve_beads_redirect(&d).is_err());
    }

    #[test]
    fn load_issues_from_path_dispatches_on_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("beads.db");
        write_br_sqlite(&db);
        assert_eq!(load_issues_from_path(&db).expect("sqlite").len(), 2);

        let jsonl = dir.path().join("x.jsonl");
        std::fs::write(
            &jsonl,
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
        )
        .expect("jsonl");
        assert_eq!(load_issues_from_path(&jsonl).expect("jsonl").len(), 1);

        // A directory is treated as a .beads directory (metadata-less here, so
        // the db alone is not declared and the JSONL file is found).
        assert_eq!(load_issues_from_path(dir.path()).expect("dir").len(), 1);
    }

    #[test]
    fn jsonl_due_at_alias_and_defer_until_load() {
        let issues = parse_issues_from_text(
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"issue_type\":\"task\",\
             \"due_at\":\"2026-03-01T00:00:00Z\",\"defer_until\":\"2026-04-01T00:00:00Z\"}",
        )
        .expect("parse");
        assert!(issues[0].due_date.is_some());
        assert!(issues[0].defer_until.is_some());
    }
}
