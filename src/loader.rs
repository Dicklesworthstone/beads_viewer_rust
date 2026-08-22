use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

    if let Some(beads_dir) = find_beads_dir_from(&root) {
        return Ok(beads_dir);
    }

    if let Some(beads_dir) = find_worktree_main_repo_beads_dir_from(&root) {
        return Ok(beads_dir);
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
    let beads_dir = get_beads_dir(repo_path)?;
    let path = find_jsonl_path(&beads_dir)?;
    load_issues_from_file(&path)
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

        match find_jsonl_path(&beads_dir) {
            Ok(jsonl_path) => paths.push(jsonl_path),
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
}
