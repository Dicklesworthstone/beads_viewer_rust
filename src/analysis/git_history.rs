use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::diff::{FieldChange, detect_changes};
use crate::{BvrError, Result, model::Issue};

/// User-supplied bead ID patterns (`--id-pattern`), registered once at CLI
/// startup before any correlation runs.
static CUSTOM_ID_PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();

/// Register `--id-pattern` regexes for every message-based ID matcher
/// (commit correlation and orphan detection). Later calls are ignored.
pub fn set_custom_id_patterns(patterns: Vec<regex::Regex>) {
    let _ = CUSTOM_ID_PATTERNS.set(patterns);
}

/// The registered `--id-pattern` regexes (empty unless the flag was given).
pub fn custom_id_patterns() -> &'static [regex::Regex] {
    CUSTOM_ID_PATTERNS.get().map_or(&[], Vec::as_slice)
}

/// Candidate bead IDs that `patterns` find in `message`, in pattern then
/// match order: capture group 1 when the pattern defines one, else the whole
/// match. Matching is against the original-case message.
#[must_use]
pub fn ids_from_patterns(message: &str, patterns: &[regex::Regex]) -> Vec<String> {
    let mut ids = Vec::new();
    for pattern in patterns {
        for captures in pattern.captures_iter(message) {
            let id = captures
                .get(1)
                .filter(|group| !group.as_str().is_empty())
                .or_else(|| captures.get(0))
                .map(|group| group.as_str().to_string());
            if let Some(id) = id.filter(|id| !id.is_empty()) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

#[derive(Debug, Clone)]
pub struct GitCommitRecord {
    pub sha: String,
    pub short_sha: String,
    pub timestamp: String,
    pub author: String,
    pub author_email: String,
    pub message: String,
    pub files: Vec<HistoryFileChangeCompat>,
    pub changed_beads: bool,
    pub changed_non_beads: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HistoryMilestonesCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<HistoryEventCompat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed: Option<HistoryEventCompat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<HistoryEventCompat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reopened: Option<HistoryEventCompat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEventCompat {
    pub bead_id: String,
    pub event_type: String,
    pub timestamp: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub author: String,
    pub author_email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryBeadCompat {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub events: Vec<HistoryEventCompat>,
    pub milestones: HistoryMilestonesCompat,
    pub commits: Option<Vec<HistoryCommitCompat>>,
    pub cycle_time: Option<HistoryCycleCompat>,
    pub last_author: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryCommitCompat {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub timestamp: String,
    pub files: Vec<HistoryFileChangeCompat>,
    pub method: String,
    pub confidence: f64,
    pub reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub field_changes: Vec<FieldChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bead_diff_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryFileChangeCompat {
    pub path: String,
    pub action: String,
    pub insertions: i64,
    pub deletions: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryCycleCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_to_close: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_to_close: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_to_claim: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryStatsCompat {
    pub total_beads: usize,
    pub beads_with_commits: usize,
    pub total_commits: usize,
    pub unique_authors: usize,
    pub avg_commits_per_bead: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_cycle_time_days: Option<f64>,
    pub method_distribution: BTreeMap<String, usize>,
}

/// Cancellation belongs to one history request, never to the process or repo.
#[derive(Clone, Debug, Default)]
pub struct GitHistoryCancellation(Arc<AtomicBool>);

impl GitHistoryCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug)]
struct GitHistoryLimits {
    command_timeout: Duration,
    log_timeout: Duration,
    scan_timeout: Duration,
    max_stdout: u64,
    max_stderr: u64,
    max_output: u64,
    max_result_bytes: usize,
}

impl Default for GitHistoryLimits {
    fn default() -> Self {
        Self {
            command_timeout: Duration::from_secs(5),
            log_timeout: Duration::from_secs(10),
            scan_timeout: Duration::from_secs(30),
            max_stdout: 32 * 1024 * 1024,
            max_stderr: 128 * 1024,
            max_output: 256 * 1024 * 1024,
            max_result_bytes: 256 * 1024 * 1024,
        }
    }
}

/// One bounded request, shared by commit loading and correlation. Reuse it
/// across workspace repositories so the budget applies to the whole request.
pub struct GitHistoryScan {
    started: Instant,
    cancellation: GitHistoryCancellation,
    limits: GitHistoryLimits,
    output_bytes: u64,
    result_bytes: usize,
    git_program: OsString,
    #[cfg(test)]
    commands_started: usize,
}

impl Default for GitHistoryScan {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            cancellation: GitHistoryCancellation::default(),
            limits: GitHistoryLimits::default(),
            output_bytes: 0,
            result_bytes: 0,
            git_program: OsString::from("git"),
            #[cfg(test)]
            commands_started: 0,
        }
    }
}

struct GitHistoryOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn history_error(kind: std::io::ErrorKind, message: impl std::fmt::Display) -> BvrError {
    BvrError::Io(std::io::Error::new(kind, format!("git history: {message}")))
}

impl GitHistoryScan {
    #[must_use]
    pub fn cancellation_token(&self) -> GitHistoryCancellation {
        self.cancellation.clone()
    }

    fn check_progress(&self) -> Result<()> {
        if self.cancellation.0.load(Ordering::Relaxed) {
            return Err(history_error(
                std::io::ErrorKind::Interrupted,
                "request cancelled",
            ));
        }
        if self.started.elapsed() >= self.limits.scan_timeout {
            return Err(history_error(
                std::io::ErrorKind::TimedOut,
                format!(
                    "request exceeded {} seconds; reduce --history-limit or narrow --history-since",
                    self.limits.scan_timeout.as_secs_f64()
                ),
            ));
        }
        Ok(())
    }

    fn command(&self, repo_root: &Path) -> Command {
        let mut command = Command::new(&self.git_program);
        command
            .arg("--no-pager")
            .arg("--literal-pathspecs")
            .args(["-c", "core.fsmonitor=false"])
            .arg("-C")
            .arg(repo_root)
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .stdin(Stdio::null());
        command
    }

    fn check_output_lengths(&self, operation: &str, stdout: u64, stderr: u64) -> Result<()> {
        for (stream, bytes, limit) in [
            ("stdout", stdout, self.limits.max_stdout),
            ("stderr", stderr, self.limits.max_stderr),
        ] {
            if bytes > limit {
                return Err(history_error(
                    std::io::ErrorKind::InvalidData,
                    format!("{operation} {stream} exceeded {limit} bytes"),
                ));
            }
        }
        if self
            .output_bytes
            .saturating_add(stdout)
            .saturating_add(stderr)
            > self.limits.max_output
        {
            return Err(history_error(
                std::io::ErrorKind::InvalidData,
                format!(
                    "request output exceeded {} bytes; reduce --history-limit or narrow --history-since",
                    self.limits.max_output
                ),
            ));
        }
        Ok(())
    }

    fn wait_for_command(
        &self,
        child: &mut Child,
        stdout: &File,
        stderr: &File,
        operation: &str,
        started: Instant,
        command_timeout: Duration,
    ) -> Result<(ExitStatus, u64, u64)> {
        loop {
            self.check_progress()?;
            let status = child.try_wait()?;
            let stdout_len = stdout.metadata()?.len();
            let stderr_len = stderr.metadata()?.len();
            self.check_output_lengths(operation, stdout_len, stderr_len)?;
            if started.elapsed() >= command_timeout {
                return Err(history_error(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "{operation} timed out after {} seconds",
                        command_timeout.as_secs_f64()
                    ),
                ));
            }
            if let Some(status) = status {
                return Ok((status, stdout_len, stderr_len));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run(&mut self, command: Command, operation: &str) -> Result<GitHistoryOutput> {
        self.run_with_timeout(command, operation, self.limits.command_timeout)
    }

    fn run_with_timeout(
        &mut self,
        mut command: Command,
        operation: &str,
        command_timeout: Duration,
    ) -> Result<GitHistoryOutput> {
        self.check_progress()?;
        // Private files avoid unbounded in-memory output and pipe readers that
        // remain blocked when a descendant inherits Git's output handles.
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        command
            .stdout(Stdio::from(stdout.try_clone()?))
            .stderr(Stdio::from(stderr.try_clone()?));
        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| {
            history_error(
                error.kind(),
                format!("could not start {operation}: {error}"),
            )
        })?;
        #[cfg(test)]
        {
            self.commands_started += 1;
        }
        let (status, stdout_len, stderr_len) = match self.wait_for_command(
            &mut child,
            &stdout,
            &stderr,
            operation,
            started,
            command_timeout,
        ) {
            Ok(completed) => completed,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        self.output_bytes += stdout_len + stderr_len;
        stdout.rewind()?;
        stderr.rewind()?;
        let mut result = GitHistoryOutput {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        // Read exactly the completed snapshot, even if a descendant retains a
        // file handle. Every byte counted here belongs to the shared budget.
        stdout.take(stdout_len).read_to_end(&mut result.stdout)?;
        stderr.take(stderr_len).read_to_end(&mut result.stderr)?;
        self.check_progress()?;
        Ok(result)
    }

    fn command_error(operation: &str, output: &GitHistoryOutput) -> BvrError {
        let diagnostic = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4096)]);
        history_error(
            std::io::ErrorKind::Other,
            format!(
                "{operation} failed ({}): {}",
                output.status,
                diagnostic.trim()
            ),
        )
    }

    fn is_git_work_tree(&mut self, repo_root: &Path) -> Result<bool> {
        let mut command = self.command(repo_root);
        command.args(["rev-parse", "--is-inside-work-tree"]);
        let output = self.run(command, "git rev-parse")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not a git repository") {
                return Ok(false);
            }
            return Err(Self::command_error("git rev-parse", &output));
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(history_error(
                std::io::ErrorKind::InvalidData,
                "invalid work-tree probe output",
            )),
        }
    }

    pub fn repository_root(&mut self, path: &Path) -> Result<Option<PathBuf>> {
        let mut command = self.command(path);
        command.args(["rev-parse", "--show-toplevel"]);
        let output = self.run(command, "git rev-parse --show-toplevel")?;
        if !output.status.success() {
            if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
                return Ok(None);
            }
            return Err(Self::command_error(
                "git rev-parse --show-toplevel",
                &output,
            ));
        }
        let root = std::str::from_utf8(&output.stdout)
            .map_err(|error| {
                history_error(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid repository path: {error}"),
                )
            })?
            .trim_end_matches('\n');
        if root.is_empty() {
            return Err(history_error(
                std::io::ErrorKind::InvalidData,
                "empty repository path",
            ));
        }
        Ok(Some(PathBuf::from(root)))
    }

    pub fn head_sha(&mut self, repo_root: &Path) -> Result<Option<String>> {
        if !self.is_git_work_tree(repo_root)? {
            return Ok(None);
        }
        let mut command = self.command(repo_root);
        command.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
        let output = self.run(command, "git rev-parse HEAD")?;
        if output.status.success() {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok(Some(sha));
            }
            return Err(history_error(
                std::io::ErrorKind::InvalidData,
                "invalid HEAD commit ID",
            ));
        }
        // A symbolic HEAD whose branch has no ref is an ordinary unborn repo.
        // Other lookup errors must not masquerade as empty history.
        let mut symbolic = self.command(repo_root);
        symbolic.args(["symbolic-ref", "--quiet", "HEAD"]);
        let symbolic = self.run(symbolic, "git symbolic-ref HEAD")?;
        if symbolic.status.success() {
            let branch = String::from_utf8_lossy(&symbolic.stdout).trim().to_string();
            if branch.starts_with("refs/heads/") {
                let mut reference = self.command(repo_root);
                reference.args(["show-ref", "--verify", "--quiet", &branch]);
                let reference = self.run(reference, "git show-ref HEAD branch")?;
                if reference.status.code() == Some(1) && reference.stderr.is_empty() {
                    return Ok(None);
                }
                if !reference.status.success() {
                    return Err(Self::command_error("git show-ref HEAD branch", &reference));
                }
            }
        }
        Err(Self::command_error("git rev-parse HEAD", &output))
    }

    pub fn load_commits(
        &mut self,
        repo_root: &Path,
        limit: usize,
        history_since: Option<&str>,
    ) -> Result<Vec<GitCommitRecord>> {
        if !self.is_git_work_tree(repo_root)? {
            return Ok(Vec::new());
        }
        let mut command = self.command(repo_root);
        command.arg("log");
        if limit > 0 {
            command.arg(format!("-n{limit}"));
        }
        if let Some(since) = history_since {
            command.arg("--since").arg(since);
        }
        command.args([
            "--name-status",
            "-z",
            "--no-color",
            "--encoding=UTF-8",
            "--no-ext-diff",
            "--no-textconv",
            "--date=iso-strict",
            "--pretty=format:%H%x00%h%x00%cI%x00%an%x00%ae%x00%s%x00",
        ]);
        let output = self.run_with_timeout(command, "git log", self.limits.log_timeout)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not have any commits yet") {
                return Ok(Vec::new());
            }
            if let Some(since) = history_since {
                return Err(BvrError::InvalidArgument(format!(
                    "Error parsing --history-since '{since}': {}",
                    stderr.trim()
                )));
            }
            return Err(Self::command_error("git log", &output));
        }
        self.parse_commit_log(&output.stdout)
    }

    fn parse_commit_log(&self, output: &[u8]) -> Result<Vec<GitCommitRecord>> {
        self.check_progress()?;
        if !output.is_empty() && output.last() != Some(&0) {
            return Err(history_error(
                std::io::ErrorKind::InvalidData,
                "incomplete git log output",
            ));
        }
        let text = std::str::from_utf8(output).map_err(|error| {
            history_error(
                std::io::ErrorKind::InvalidData,
                format!("invalid git log text: {error}"),
            )
        })?;
        let mut fields = text.split('\0').peekable();
        let mut commits = Vec::new();
        while let Some(sha) = fields.next() {
            self.check_progress()?;
            if sha.is_empty() {
                continue;
            }
            if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(history_error(
                    std::io::ErrorKind::InvalidData,
                    "invalid git log commit ID",
                ));
            }
            let mut next_field = || {
                fields.next().ok_or_else(|| {
                    history_error(std::io::ErrorKind::InvalidData, "incomplete git log record")
                })
            };
            let short_sha = next_field()?.to_string();
            let timestamp = next_field()?.to_string();
            let author = next_field()?.to_string();
            let author_email = next_field()?.to_string();
            let message = next_field()?.to_string();
            let mut files = Vec::new();
            let mut changed_beads = false;
            let mut changed_non_beads = false;
            while fields.peek().is_some_and(|field| !field.is_empty()) {
                self.check_progress()?;
                let status = fields.next().unwrap_or_default().trim_start_matches('\n');
                let action = status.chars().next().ok_or_else(|| {
                    history_error(std::io::ErrorKind::InvalidData, "missing git file status")
                })?;
                if !matches!(action, 'A' | 'C' | 'D' | 'M' | 'R' | 'T' | 'U' | 'X' | 'B') {
                    return Err(history_error(
                        std::io::ErrorKind::InvalidData,
                        "invalid git file status",
                    ));
                }
                let source_path = fields.next().ok_or_else(|| {
                    history_error(std::io::ErrorKind::InvalidData, "missing git file path")
                })?;
                let path = if matches!(action, 'R' | 'C') {
                    fields.next().ok_or_else(|| {
                        history_error(
                            std::io::ErrorKind::InvalidData,
                            "missing git rename destination",
                        )
                    })?
                } else {
                    source_path
                };
                if path.is_empty() {
                    return Err(history_error(
                        std::io::ErrorKind::InvalidData,
                        "empty git file path",
                    ));
                }
                let is_beads = is_beads_jsonl_path(path);
                changed_beads |= is_beads;
                // Generated backup rotation is neither a live issue edit nor
                // a code change, but retain its paths in the complete file list.
                changed_non_beads |= !is_beads && !is_br_history_path(path);
                files.push(HistoryFileChangeCompat {
                    path: path.to_string(),
                    action: action.to_string(),
                    insertions: 0,
                    deletions: 0,
                });
            }
            files.sort_by(|left, right| left.path.cmp(&right.path));
            commits.push(GitCommitRecord {
                sha: sha.to_string(),
                short_sha,
                timestamp,
                author,
                author_email,
                message,
                files,
                changed_beads,
                changed_non_beads,
            });
        }
        self.check_progress()?;
        Ok(commits)
    }
}

pub fn load_git_commits(
    repo_root: &Path,
    limit: usize,
    history_since: Option<&str>,
) -> Result<Vec<GitCommitRecord>> {
    GitHistoryScan::default().load_commits(repo_root, limit, history_since)
}

pub fn correlate_histories_with_git(
    repo_root: &Path,
    commits: &[GitCommitRecord],
    histories_map: &mut BTreeMap<String, HistoryBeadCompat>,
    commit_index: &mut BTreeMap<String, Vec<String>>,
    method_distribution: &mut BTreeMap<String, usize>,
) -> Result<()> {
    correlate_histories_with_git_aliases(
        repo_root,
        commits,
        histories_map,
        commit_index,
        method_distribution,
        &BTreeMap::new(),
    )
}

/// Like [`correlate_histories_with_git`] but accepts `workspace_id_aliases`.
///
/// The alias map goes from **raw (unprefixed, lowercase)** bead IDs to their
/// canonical workspace-prefixed form.  This allows JSONL diffs from nested
/// workspace repos (which store unprefixed IDs) to be matched against the
/// prefixed IDs in the histories map.
pub fn correlate_histories_with_git_aliases(
    repo_root: &Path,
    commits: &[GitCommitRecord],
    histories_map: &mut BTreeMap<String, HistoryBeadCompat>,
    commit_index: &mut BTreeMap<String, Vec<String>>,
    method_distribution: &mut BTreeMap<String, usize>,
    workspace_id_aliases: &BTreeMap<String, String>,
) -> Result<()> {
    GitHistoryScan::default().correlate_histories(
        repo_root,
        commits,
        histories_map,
        commit_index,
        method_distribution,
        workspace_id_aliases,
    )
}

impl GitHistoryScan {
    /// Correlate all selected commits or return an error without publishing
    /// any additions to the caller's maps. No failed diff becomes empty data.
    pub fn correlate_histories(
        &mut self,
        repo_root: &Path,
        commits: &[GitCommitRecord],
        histories_map: &mut BTreeMap<String, HistoryBeadCompat>,
        commit_index: &mut BTreeMap<String, Vec<String>>,
        method_distribution: &mut BTreeMap<String, usize>,
        workspace_id_aliases: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.check_progress()?;
        let mut known_ids: BTreeMap<String, String> = histories_map
            .keys()
            .map(|id| (id.to_ascii_lowercase(), id.clone()))
            .collect();
        for (raw_lower, canonical) in workspace_id_aliases {
            self.check_progress()?;
            known_ids
                .entry(raw_lower.clone())
                .or_insert_with(|| canonical.clone());
        }

        let mut pending = Vec::<(String, HistoryCommitCompat)>::new();
        let mut pending_ids = BTreeSet::new();
        let mut result_bytes = self.result_bytes;
        for commit in commits {
            self.check_progress()?;
            let mut bead_ids = extract_ids_from_message(&commit.message, &known_ids);
            let bead_change_details = if commit.changed_beads {
                self.read_bead_change_details(repo_root, commit, &known_ids)?
            } else {
                BTreeMap::new()
            };
            // The parsed diff supplies both IDs and field changes, including
            // additions/deletions and records that do not deserialize as Issue.
            bead_ids.extend(bead_change_details.keys().cloned());
            let (method, confidence, reason) = if commit.changed_beads && commit.changed_non_beads {
                (
                    "co_committed",
                    0.95,
                    "Commit modified beads metadata and code paths together",
                )
            } else if commit.changed_beads {
                (
                    "explicit_id",
                    0.85,
                    "Commit references bead changes explicitly",
                )
            } else {
                ("explicit_id", 0.75, "Commit message references bead ID")
            };

            for bead_id in bead_ids {
                self.check_progress()?;
                let Some(history) = histories_map.get(&bead_id) else {
                    continue;
                };
                if history
                    .commits
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|entry| entry.sha == commit.sha)
                    || pending_ids.contains(&(bead_id.clone(), commit.sha.clone()))
                {
                    continue;
                }
                let (changes, diff_lines) = bead_change_details
                    .get(&bead_id)
                    .map_or((&[][..], &[][..]), |(changes, lines)| {
                        (changes.as_slice(), lines.as_slice())
                    });
                result_bytes = result_bytes.saturating_add(correlation_entry_bytes(
                    commit, &bead_id, method, reason, changes, diff_lines,
                ));
                if result_bytes > self.limits.max_result_bytes {
                    return Err(history_error(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "correlation data exceeded {} bytes; reduce --history-limit or narrow --history-since",
                            self.limits.max_result_bytes
                        ),
                    ));
                }
                pending_ids.insert((bead_id.clone(), commit.sha.clone()));
                pending.push((
                    bead_id,
                    HistoryCommitCompat {
                        sha: commit.sha.clone(),
                        short_sha: commit.short_sha.clone(),
                        message: commit.message.clone(),
                        author: commit.author.clone(),
                        author_email: commit.author_email.clone(),
                        timestamp: commit.timestamp.clone(),
                        files: commit.files.clone(),
                        method: method.to_string(),
                        confidence,
                        reason: reason.to_string(),
                        field_changes: changes.to_vec(),
                        bead_diff_lines: diff_lines.to_vec(),
                    },
                ));
            }
        }
        self.check_progress()?;
        self.result_bytes = result_bytes;
        for (bead_id, commit) in pending {
            let history = histories_map.get_mut(&bead_id).expect("known history ID");
            let ids = commit_index.entry(commit.sha.clone()).or_default();
            if !ids.contains(&bead_id) {
                ids.push(bead_id);
            }
            *method_distribution
                .entry(commit.method.clone())
                .or_insert(0) += 1;
            history.commits.get_or_insert_with(Vec::new).push(commit);
        }
        for ids in commit_index.values_mut() {
            ids.sort();
            ids.dedup();
        }
        Ok(())
    }
}

// File lists are copied into each bead's history. Charge that multiplication
// before cloning, as a small Git output can otherwise expand into a huge result.
fn correlation_entry_bytes(
    commit: &GitCommitRecord,
    bead_id: &str,
    method: &str,
    reason: &str,
    changes: &[FieldChange],
    diff_lines: &[String],
) -> usize {
    let strings = [
        commit.sha.as_str(),
        commit.short_sha.as_str(),
        commit.message.as_str(),
        commit.author.as_str(),
        commit.author_email.as_str(),
        commit.timestamp.as_str(),
        bead_id,
        method,
        reason,
    ];
    let base = strings
        .iter()
        .fold(std::mem::size_of::<HistoryCommitCompat>(), |size, value| {
            size.saturating_add(value.len())
        });
    let files = commit.files.iter().fold(base, |size, file| {
        size.saturating_add(std::mem::size_of::<HistoryFileChangeCompat>())
            .saturating_add(file.path.len())
            .saturating_add(file.action.len())
    });
    let fields = changes.iter().fold(files, |size, change| {
        size.saturating_add(std::mem::size_of::<FieldChange>())
            .saturating_add(change.field.len())
            .saturating_add(change.old_value.len())
            .saturating_add(change.new_value.len())
    });
    diff_lines.iter().fold(fields, |size, line| {
        size.saturating_add(std::mem::size_of::<String>())
            .saturating_add(line.len())
    })
}

/// Build a mapping from raw (unprefixed, lowercase) bead IDs to their canonical
/// workspace-prefixed form.
///
/// For each issue with a known workspace prefix, strip that prefix from the
/// issue ID to recover the raw ID. Older callers that only populated
/// `source_repo` still get a best-effort fallback of `lowercase(source_repo)-`.
pub fn build_workspace_id_aliases(issues: &[Issue]) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::<String, String>::new();

    for issue in issues {
        let prefix = issue
            .workspace_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .map(std::borrow::ToOwned::to_owned)
            .or_else(|| {
                let repo = issue.source_repo.trim();
                (!repo.is_empty()).then(|| format!("{repo}-"))
            });

        let Some(prefix) = prefix else {
            continue;
        };

        let id_lower = issue.id.to_ascii_lowercase();
        let prefix_lower = prefix.to_ascii_lowercase();
        if let Some(raw) = id_lower.strip_prefix(&prefix_lower) {
            if !raw.is_empty() {
                aliases
                    .entry(raw.to_string())
                    .or_insert_with(|| issue.id.clone());
            }
        }
    }

    aliases
}

pub fn extract_ids_from_message(
    message: &str,
    known_ids: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    extract_ids_from_message_with_patterns(message, known_ids, custom_id_patterns())
}

/// Known bead IDs referenced by `message`: every known ID that appears as a
/// whole token, plus IDs the custom `patterns` extract (case-insensitively
/// resolved against the known IDs).
#[must_use]
pub fn extract_ids_from_message_with_patterns(
    message: &str,
    known_ids: &BTreeMap<String, String>,
    patterns: &[regex::Regex],
) -> BTreeSet<String> {
    let lower = message.to_ascii_lowercase();
    let mut ids: BTreeSet<String> = known_ids
        .iter()
        .filter_map(|(lower_id, canonical)| {
            if contains_issue_id_token(&lower, lower_id) {
                Some(canonical.clone())
            } else {
                None
            }
        })
        .collect();
    for id in ids_from_patterns(message, patterns) {
        if let Some(canonical) = known_ids.get(&id.to_ascii_lowercase()) {
            ids.insert(canonical.clone());
        }
    }
    ids
}

fn contains_issue_id_token(message: &str, issue_id: &str) -> bool {
    if issue_id.is_empty() {
        return false;
    }

    message.match_indices(issue_id).any(|(start, _)| {
        let left = message[..start].chars().next_back();
        let right = message[start + issue_id.len()..].chars().next();

        let left_boundary = left.is_none_or(|ch| !is_issue_id_char(ch));
        let right_boundary = right.is_none_or(|ch| !is_issue_id_char(ch));

        left_boundary && right_boundary
    })
}

const fn is_issue_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'
}

type BeadChangeDetails = BTreeMap<String, (Vec<FieldChange>, Vec<String>)>;

impl GitHistoryScan {
    fn read_bead_change_details(
        &mut self,
        repo_root: &Path,
        commit: &GitCommitRecord,
        known_ids: &BTreeMap<String, String>,
    ) -> Result<BeadChangeDetails> {
        let paths = commit
            .files
            .iter()
            .filter(|file| is_beads_jsonl_path(&file.path));
        let mut command = self.command(repo_root);
        command.args([
            "show",
            "--format=",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--end-of-options",
        ]);
        command.arg(&commit.sha).arg("--");
        let mut has_paths = false;
        for file in paths {
            self.check_progress()?;
            command.arg(&file.path);
            has_paths = true;
        }
        if !has_paths {
            return Ok(BTreeMap::new());
        }
        // All selected paths share one diff read. Disable rename folding so a
        // pure move between selected metadata files still supplies JSON lines,
        // as the former per-file pathspec reads did.
        let operation = format!("git show {}", commit.sha);
        let output = self.run(command, &operation)?;
        if !output.status.success() {
            return Err(Self::command_error(&operation, &output));
        }
        self.parse_bead_change_details(&output.stdout, known_ids)
    }

    fn parse_bead_change_details(
        &self,
        output: &[u8],
        known_ids: &BTreeMap<String, String>,
    ) -> Result<BeadChangeDetails> {
        self.check_progress()?;
        let text = std::str::from_utf8(output).map_err(|error| {
            history_error(
                std::io::ErrorKind::InvalidData,
                format!("invalid git diff text: {error}"),
            )
        })?;
        let mut before = BTreeMap::<String, serde_json::Value>::new();
        let mut after = BTreeMap::<String, serde_json::Value>::new();
        for raw_line in text.lines() {
            self.check_progress()?;
            let line = raw_line.trim();
            if !(line.starts_with('+') || line.starts_with('-'))
                || line.starts_with("+++")
                || line.starts_with("---")
            {
                continue;
            }

            let content = line.trim_start_matches(['+', '-']).trim();
            if !(content.starts_with('{') && content.ends_with('}')) {
                continue;
            }

            let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
                continue;
            };
            let Some(raw_id) = value.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(canonical) = known_ids.get(&raw_id.to_ascii_lowercase()) else {
                continue;
            };

            if line.starts_with('-') {
                before.insert(canonical.clone(), value);
            } else {
                after.insert(canonical.clone(), value);
            }
        }

        let mut details = BTreeMap::<String, (Vec<FieldChange>, Vec<String>)>::new();
        let bead_ids = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for bead_id in bead_ids {
            self.check_progress()?;
            let field_changes = match (before.get(&bead_id), after.get(&bead_id)) {
                (Some(old_value), Some(new_value)) => {
                    match (
                        serde_json::from_value::<Issue>(old_value.clone()),
                        serde_json::from_value::<Issue>(new_value.clone()),
                    ) {
                        (Ok(old_issue), Ok(new_issue)) => detect_changes(&old_issue, &new_issue),
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            };

            let mut diff_lines = field_changes
                .iter()
                .flat_map(|change| {
                    [
                        format!("- {}: {}", change.field, change.old_value),
                        format!("+ {}: {}", change.field, change.new_value),
                    ]
                })
                .collect::<Vec<_>>();

            if diff_lines.is_empty() {
                if let Some(old_value) = before.get(&bead_id) {
                    diff_lines.push(format!("- issue: {}", summarize_bead_snapshot(old_value)));
                }
                if let Some(new_value) = after.get(&bead_id) {
                    diff_lines.push(format!("+ issue: {}", summarize_bead_snapshot(new_value)));
                }
            }

            details.insert(bead_id, (field_changes, diff_lines));
        }
        self.check_progress()?;
        Ok(details)
    }
}

fn summarize_bead_snapshot(value: &serde_json::Value) -> String {
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if title.is_empty() {
        format!("{id} [{status}]")
    } else {
        format!("{id} [{status}] {title}")
    }
}

fn is_beads_jsonl_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
    path.extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("jsonl"))
        && path
            .components()
            .any(|component| component.as_os_str() == ".beads")
        && !is_br_history_path(&normalized)
}

fn is_br_history_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut components = Path::new(&normalized).components();
    // br keeps generated snapshots and their metadata beneath .br_history.
    // Do not whitelist live filenames: metadata can select custom JSONL paths.
    components.any(|component| component.as_os_str() == ".beads")
        && components.any(|component| component.as_os_str() == ".br_history")
}

fn is_closed_like_status(status: &str) -> bool {
    matches!(status, "closed" | "tombstone")
}

pub fn finalize_history_entries(histories_map: &mut BTreeMap<String, HistoryBeadCompat>) {
    for history in histories_map.values_mut() {
        if let Some(commits) = history.commits.as_mut() {
            commits.sort_by(|left, right| {
                compare_timestamps(&left.timestamp, &right.timestamp)
                    .then_with(|| left.sha.cmp(&right.sha))
            });
        }

        if let Some(commits) = history.commits.as_ref().filter(|c| !c.is_empty()) {
            let mut events = commits
                .iter()
                .enumerate()
                .map(|(index, commit)| HistoryEventCompat {
                    bead_id: history.bead_id.clone(),
                    event_type: infer_event_type_from_commit(index, &commit.message),
                    timestamp: commit.timestamp.clone(),
                    commit_sha: commit.sha.clone(),
                    commit_message: commit.message.clone(),
                    author: commit.author.clone(),
                    author_email: commit.author_email.clone(),
                })
                .collect::<Vec<_>>();

            if !events.iter().any(|entry| entry.event_type == "created")
                && let Some(first) = commits.first()
            {
                events.insert(
                    0,
                    HistoryEventCompat {
                        bead_id: history.bead_id.clone(),
                        event_type: "created".to_string(),
                        timestamp: first.timestamp.clone(),
                        commit_sha: first.sha.clone(),
                        commit_message: first.message.clone(),
                        author: first.author.clone(),
                        author_email: first.author_email.clone(),
                    },
                );
            }

            if is_closed_like_status(&history.status.to_ascii_lowercase())
                && !events.iter().any(|entry| entry.event_type == "closed")
                && let Some(last) = commits.last()
            {
                events.push(HistoryEventCompat {
                    bead_id: history.bead_id.clone(),
                    event_type: "closed".to_string(),
                    timestamp: last.timestamp.clone(),
                    commit_sha: last.sha.clone(),
                    commit_message: last.message.clone(),
                    author: last.author.clone(),
                    author_email: last.author_email.clone(),
                });
            }

            events.sort_by(|left, right| {
                compare_timestamps(&left.timestamp, &right.timestamp)
                    .then_with(|| left.event_type.cmp(&right.event_type))
            });
            history.events = events;
        }

        history.milestones = HistoryMilestonesCompat {
            created: history
                .events
                .iter()
                .find(|event| event.event_type == "created")
                .cloned(),
            claimed: history
                .events
                .iter()
                .find(|event| event.event_type == "claimed")
                .cloned(),
            closed: history
                .events
                .iter()
                .find(|event| event.event_type == "closed")
                .cloned(),
            reopened: history
                .events
                .iter()
                .rev()
                .find(|event| event.event_type == "reopened")
                .cloned(),
        };

        let create_to_close = duration_between(
            history
                .milestones
                .created
                .as_ref()
                .map(|event| event.timestamp.as_str()),
            history
                .milestones
                .closed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
        );
        let claim_to_close = duration_between(
            history
                .milestones
                .claimed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
            history
                .milestones
                .closed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
        );
        let create_to_claim = duration_between(
            history
                .milestones
                .created
                .as_ref()
                .map(|event| event.timestamp.as_str()),
            history
                .milestones
                .claimed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
        );

        if create_to_close.is_some() || claim_to_close.is_some() || create_to_claim.is_some() {
            history.cycle_time = Some(HistoryCycleCompat {
                claim_to_close: claim_to_close.map(format_duration_compact),
                create_to_close: create_to_close.map(format_duration_compact),
                create_to_claim: create_to_claim.map(format_duration_compact),
            });
        }

        history.last_author = history
            .commits
            .as_ref()
            .and_then(|c| c.last())
            .map_or_else(String::new, |commit| commit.author.clone());

        // Normalize: empty Vec -> None (serializes as null, matching legacy)
        if history.commits.as_ref().is_some_and(Vec::is_empty) {
            history.commits = None;
        }
    }
}

fn infer_event_type_from_commit(index: usize, message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if has_word_token(&lower, "reopen") || has_word_token(&lower, "reopened") {
        "reopened".to_string()
    } else if has_word_token(&lower, "close") || has_word_token(&lower, "closed") {
        "closed".to_string()
    } else if has_word_token(&lower, "claim")
        || has_word_token(&lower, "claimed")
        || lower.contains("in_progress")
        || has_word_sequence(&lower, "in progress")
    {
        "claimed".to_string()
    } else if index == 0 {
        "created".to_string()
    } else {
        "modified".to_string()
    }
}

/// Check if `text` contains `token` delimited by non-alphanumeric boundaries.
/// This avoids false positives like "disclose" matching "close" or
/// "closedown" matching "closed".
fn has_word_token(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(start, _)| {
        let end = start + token.len();
        let left = start == 0 || !text.as_bytes()[start - 1].is_ascii_alphanumeric();
        let right = end == text.len() || !text.as_bytes()[end].is_ascii_alphanumeric();
        left && right
    })
}

/// Check if `text` contains the exact two-word sequence.
fn has_word_sequence(text: &str, sequence: &str) -> bool {
    text.contains(sequence)
}

fn compare_timestamps(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_rfc3339_utc(left), parse_rfc3339_utc(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn duration_between(start: Option<&str>, end: Option<&str>) -> Option<chrono::Duration> {
    let start = start.and_then(parse_rfc3339_utc)?;
    let end = end.and_then(parse_rfc3339_utc)?;
    let duration = end - start;
    if duration.num_seconds() >= 0 {
        Some(duration)
    } else {
        None
    }
}

fn format_duration_compact(duration: chrono::Duration) -> String {
    let days = duration.num_days();
    let hours = duration.num_hours() - days * 24;
    let minutes = duration.num_minutes() - duration.num_hours() * 60;
    format!("{days}d {hours}h {minutes}m")
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn run_history_git(repo: &Path, args: &[&str], date: &str) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=hooks-disabled",
            ])
            .args(args)
            .env("GIT_AUTHOR_NAME", "History regression")
            .env("GIT_AUTHOR_EMAIL", "history@example.invalid")
            .env("GIT_COMMITTER_NAME", "History regression")
            .env("GIT_COMMITTER_EMAIL", "history@example.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.join("empty-global-config"))
            .output()
            .expect("run temporary repository git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output UTF-8")
    }

    fn history_snapshot(id: &str, status: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "id": id, "title": format!("Issue {id}"), "status": status,
                "priority": 1, "issue_type": "task",
            })
        )
    }

    fn empty_histories(ids: &[&str]) -> BTreeMap<String, HistoryBeadCompat> {
        ids.iter()
            .map(|id| {
                (
                    (*id).to_string(),
                    HistoryBeadCompat {
                        bead_id: (*id).to_string(),
                        title: (*id).to_string(),
                        status: "open".to_string(),
                        events: Vec::new(),
                        milestones: HistoryMilestonesCompat::default(),
                        commits: None,
                        cycle_time: None,
                        last_author: String::new(),
                    },
                )
            })
            .collect()
    }

    fn small_history_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("history repository");
        let root = temp.path();
        run_history_git(root, &["init"], "2024-01-01T00:00:00Z");
        std::fs::create_dir_all(root.join(".beads")).expect("beads directory");
        std::fs::write(
            root.join(".beads/issues.jsonl"),
            history_snapshot("bd-one", "open"),
        )
        .expect("initial snapshot");
        run_history_git(root, &["add", "."], "2024-01-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "initial snapshot"],
            "2024-01-01T00:00:00Z",
        );
        std::fs::write(
            root.join(".beads/issues.jsonl"),
            history_snapshot("bd-one", "closed"),
        )
        .expect("updated snapshot");
        run_history_git(root, &["add", "."], "2024-02-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "close snapshot"],
            "2024-02-01T00:00:00Z",
        );
        temp
    }

    #[test]
    fn bounded_history_reads_each_commit_diff_once_and_preserves_correlations() {
        let temp = tempfile::tempdir().expect("history repository");
        let root = temp.path();
        run_history_git(root, &["init"], "2024-01-01T00:00:00Z");
        let paths: Vec<_> = (0..8)
            .map(|index| format!("service-{index}/.beads/issues.jsonl"))
            .collect();
        for (index, path) in paths.iter().enumerate() {
            std::fs::create_dir_all(root.join(path).parent().unwrap())
                .expect("nested beads directory");
            let mut snapshot = history_snapshot(&format!("bd-{index}"), "open");
            if index == 0 {
                snapshot.push_str(&history_snapshot("bd-removed", "open"));
            }
            std::fs::write(root.join(path), snapshot).expect("initial snapshot");
        }
        run_history_git(root, &["add", "."], "2024-01-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "seed snapshots"],
            "2024-01-01T00:00:00Z",
        );
        for (index, path) in paths.iter().enumerate() {
            let mut snapshot = history_snapshot(&format!("bd-{index}"), "closed");
            if index == 0 {
                snapshot.push_str(&history_snapshot("bd-new", "open"));
            }
            std::fs::write(root.join(path), snapshot).expect("updated snapshot");
        }
        std::fs::write(
            root.join("code.jsonl"),
            history_snapshot("bd-decoy", "open"),
        )
        .expect("non-beads JSON must not produce correlation");
        run_history_git(root, &["add", "."], "2024-02-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "sync snapshots; bd-note"],
            "2024-02-01T00:00:00Z",
        );
        std::fs::write(root.join("notes.txt"), "code-only change\n").expect("code change");
        run_history_git(root, &["add", "."], "2024-03-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "followup bd-note"],
            "2024-03-01T00:00:00Z",
        );

        let mut scan = GitHistoryScan::default();
        let commits = scan.load_commits(root, 0, None).expect("complete log");
        assert_eq!(commits.len(), 3);
        let mut histories = empty_histories(&[
            "workspace-bd-0",
            "bd-1",
            "bd-2",
            "bd-3",
            "bd-4",
            "bd-5",
            "bd-6",
            "bd-7",
            "bd-new",
            "bd-removed",
            "bd-note",
            "bd-decoy",
        ]);
        let aliases = BTreeMap::from([("bd-0".to_string(), "workspace-bd-0".to_string())]);
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        scan.correlate_histories(
            root,
            &commits,
            &mut histories,
            &mut index,
            &mut methods,
            &aliases,
        )
        .expect("complete correlation");
        assert_eq!(
            scan.commands_started, 4,
            "probe + log + one diff for each of two metadata commits"
        );
        assert_eq!(index.len(), 3);
        assert_eq!(
            methods,
            BTreeMap::from([
                ("co_committed".to_string(), 11),
                ("explicit_id".to_string(), 10)
            ])
        );
        assert!(histories["bd-decoy"].commits.is_none());
        let updated = &histories["workspace-bd-0"].commits.as_ref().unwrap()[0];
        assert_eq!(updated.message, "sync snapshots; bd-note");
        assert_eq!(updated.method, "co_committed");
        assert!((updated.confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(
            updated.files.len(),
            9,
            "retain the full commit file list, including code"
        );
        assert_eq!(updated.field_changes.len(), 1);
        assert_eq!(updated.field_changes[0].field, "status");
        assert_eq!(
            updated.bead_diff_lines,
            ["- status: open", "+ status: closed"]
        );
        let removed = &histories["bd-removed"].commits.as_ref().unwrap()[0];
        assert_eq!(
            removed.bead_diff_lines,
            ["- issue: bd-removed [open] Issue bd-removed"]
        );
        let added = &histories["bd-new"].commits.as_ref().unwrap()[0];
        assert_eq!(
            added.bead_diff_lines,
            ["+ issue: bd-new [open] Issue bd-new"]
        );
        let message_only = &histories["bd-note"].commits.as_ref().unwrap()[0];
        assert_eq!(message_only.method, "explicit_id");
        assert!((message_only.confidence - 0.75).abs() < f64::EPSILON);
        let initial = &histories["workspace-bd-0"].commits.as_ref().unwrap()[1];
        assert!((initial.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn bounded_history_ignores_generated_backups_without_losing_live_changes() {
        let temp = tempfile::tempdir().expect("history repository");
        let root = temp.path();
        run_history_git(root, &["init"], "2024-01-01T00:00:00Z");
        let live_paths = [
            (".beads/issues.jsonl", "bd-issues"),
            (".beads/beads.jsonl", "bd-beads"),
            (".beads/beads.base.jsonl", "bd-base"),
            (".beads/custom.jsonl", "bd-custom"),
            (".beads/exports/current board.jsonl", "bd-nested"),
            ("services/api/.beads/issues.jsonl", "bd-workspace"),
        ];
        for (path, id) in live_paths {
            std::fs::create_dir_all(root.join(path).parent().unwrap())
                .expect("live metadata directory");
            std::fs::write(root.join(path), history_snapshot(id, "open"))
                .expect("initial live issues");
        }
        run_history_git(root, &["add", "."], "2024-01-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "seed live issues"],
            "2024-01-01T00:00:00Z",
        );

        let backup_paths = [
            ".beads/.br_history/issues.20240101.jsonl",
            ".beads/.br_history/issues.20240101.meta.json",
            "services/api/.beads/.br_history/issues.20240101.jsonl",
            "services/api/.beads/.br_history/issues.20240101.meta.json",
        ];
        let archived_snapshot = |status: &str| {
            format!(
                "{}\n{}",
                serde_json::json!({
                    "id": "bd-issues", "title": "Archived issue ".repeat(4096),
                    "status": status, "priority": 4, "issue_type": "bug",
                }),
                history_snapshot("bd-backup-only", status),
            )
        };
        let initial_archive = archived_snapshot("in_progress");
        assert!(initial_archive.len() > 8 * 1024);
        for path in backup_paths {
            std::fs::create_dir_all(root.join(path).parent().unwrap())
                .expect("generated history directory");
            let contents = if path.ends_with(".jsonl") {
                initial_archive.as_str()
            } else {
                "{\"generation\":1}\n"
            };
            std::fs::write(root.join(path), contents).expect("generated backup");
        }
        run_history_git(root, &["add", "."], "2024-02-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "rotate generated backups"],
            "2024-02-01T00:00:00Z",
        );

        for (path, id) in live_paths {
            std::fs::write(root.join(path), history_snapshot(id, "closed"))
                .expect("updated live issues");
        }
        let updated_archive = archived_snapshot("tombstone");
        for path in backup_paths {
            let contents = if path.ends_with(".jsonl") {
                updated_archive.as_str()
            } else {
                "{\"generation\":2}\n"
            };
            std::fs::write(root.join(path), contents).expect("rotated backup");
        }
        run_history_git(root, &["add", "."], "2024-03-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "update live issues and rotate backups"],
            "2024-03-01T00:00:00Z",
        );
        for path in backup_paths {
            std::fs::remove_file(root.join(path)).expect("prune generated backup");
        }
        run_history_git(root, &["add", "."], "2024-04-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "prune generated backups; bd-note"],
            "2024-04-01T00:00:00Z",
        );

        let mut scan = GitHistoryScan::default();
        // Live diffs and the complete log fit, but any archive read exceeds
        // this cap. Skipping only backup-only commits would still fail here.
        scan.limits.max_stdout = 8 * 1024;
        let commits = scan.load_commits(root, 500, None).expect("complete log");
        assert_eq!(commits.len(), 4);
        for index in [0, 2] {
            assert!(!commits[index].changed_beads);
            assert!(!commits[index].changed_non_beads);
            assert_eq!(commits[index].files.len(), backup_paths.len());
        }
        for index in [1, 3] {
            assert!(commits[index].changed_beads);
            assert!(!commits[index].changed_non_beads);
        }
        let mut ids = live_paths.iter().map(|(_, id)| *id).collect::<Vec<_>>();
        ids.extend(["bd-backup-only", "bd-note"]);
        let mut histories = empty_histories(&ids);
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        scan.correlate_histories(
            root,
            &commits,
            &mut histories,
            &mut index,
            &mut methods,
            &BTreeMap::new(),
        )
        .expect("archives must not consume the live diff budget");
        assert_eq!(scan.commands_started, 4, "probe + log + two live diffs");
        assert_eq!(index.len(), 3);
        assert!(!index.contains_key(&commits[2].sha));
        assert!(histories["bd-backup-only"].commits.is_none());
        for (_, id) in live_paths {
            let related = histories[id].commits.as_ref().expect("live history");
            assert_eq!(related.len(), 2);
            let updated = &related[0];
            assert_eq!(updated.sha, commits[1].sha);
            assert_eq!(updated.field_changes.len(), 1);
            assert_eq!(updated.field_changes[0].field, "status");
            assert_eq!(
                updated.bead_diff_lines,
                ["- status: open", "+ status: closed"]
            );
            assert_eq!(updated.method, "explicit_id");
            assert!((updated.confidence - 0.85).abs() < f64::EPSILON);
            assert_eq!(updated.files.len(), live_paths.len() + backup_paths.len());
        }
        let note = &histories["bd-note"].commits.as_ref().unwrap()[0];
        assert_eq!(note.sha, commits[0].sha);
        assert_eq!(note.field_changes.len(), 0);
        assert_eq!(note.bead_diff_lines.len(), 0);
        assert!((note.confidence - 0.75).abs() < f64::EPSILON);
        assert_eq!(
            methods,
            BTreeMap::from([("explicit_id".to_string(), live_paths.len() * 2 + 1)])
        );
    }

    #[test]
    fn bounded_history_keeps_nonrepo_empty_repo_and_requested_range() {
        let empty = tempfile::tempdir().expect("empty directory");
        let mut scan = GitHistoryScan::default();
        assert_eq!(scan.repository_root(empty.path()).unwrap(), None);
        assert_eq!(scan.head_sha(empty.path()).unwrap(), None);
        assert_eq!(load_git_commits(empty.path(), 500, None).unwrap().len(), 0);
        run_history_git(empty.path(), &["init"], "2024-01-01T00:00:00Z");
        assert_eq!(
            scan.repository_root(empty.path()).unwrap(),
            Some(empty.path().canonicalize().unwrap())
        );
        assert_eq!(scan.head_sha(empty.path()).unwrap(), None);
        assert_eq!(load_git_commits(empty.path(), 500, None).unwrap().len(), 0);
        run_history_git(
            empty.path(),
            &["commit", "--allow-empty", "-m", "empty initial commit"],
            "2024-01-01T00:00:00Z",
        );
        let no_files = load_git_commits(empty.path(), 0, None).unwrap();
        assert_eq!(no_files.len(), 1);
        assert_eq!(no_files[0].files.len(), 0);
        assert_eq!(
            scan.head_sha(empty.path()).unwrap(),
            Some(no_files[0].sha.clone())
        );
        let repo = small_history_repo();
        assert_eq!(load_git_commits(repo.path(), 0, None).unwrap().len(), 2);
        let limited = load_git_commits(repo.path(), 1, None).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].message, "close snapshot");
        assert_eq!(
            scan.head_sha(repo.path()).unwrap(),
            Some(limited[0].sha.clone())
        );
        let since = load_git_commits(repo.path(), 0, Some("2024-01-15T00:00:00Z")).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].sha, limited[0].sha);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_preserves_literal_paths_and_rename_destinations() {
        let temp = tempfile::tempdir().expect("history repository");
        let root = temp.path();
        run_history_git(root, &["init"], "2024-01-01T00:00:00Z");
        run_history_git(
            root,
            &["config", "diff.renames", "true"],
            "2024-01-01T00:00:00Z",
        );
        std::fs::create_dir_all(root.join("services/é/.beads")).expect("beads directory");
        let old_path = "services/é/.beads/board[1].jsonl";
        let new_path = "services/é/.beads/renamed \"board\"\t[2].jsonl";
        let other_path = "services/é/.beads/board1.jsonl";
        std::fs::write(root.join(old_path), history_snapshot("bd-one", "open")).expect("snapshot");
        std::fs::write(root.join(other_path), history_snapshot("bd-other", "open")).expect("decoy");
        std::fs::write(root.join("old code.txt"), "code\n").expect("code file");
        run_history_git(root, &["add", "."], "2024-01-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "initial snapshots"],
            "2024-01-01T00:00:00Z",
        );
        std::fs::rename(root.join(old_path), root.join(new_path)).expect("rename metadata");
        std::fs::rename(root.join("old code.txt"), root.join("new code.txt")).expect("rename code");
        run_history_git(root, &["add", "."], "2024-02-01T00:00:00Z");
        run_history_git(
            root,
            &["commit", "-m", "rename snapshots é \u{1e} \u{1f}"],
            "2024-02-01T00:00:00Z",
        );
        run_history_git(
            root,
            &["config", "color.ui", "always"],
            "2024-02-01T00:00:00Z",
        );
        run_history_git(
            root,
            &["config", "i18n.logOutputEncoding", "ISO-8859-1"],
            "2024-02-01T00:00:00Z",
        );
        let mut scan = GitHistoryScan::default();
        let commits = scan.load_commits(root, 1, None).expect("rename log");
        assert_eq!(commits[0].message, "rename snapshots é \u{1e} \u{1f}");
        assert_eq!(
            commits[0]
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.action.as_str()))
                .collect::<Vec<_>>(),
            [("new code.txt", "R"), (new_path, "R")]
        );
        let mut histories = empty_histories(&["bd-one", "bd-other"]);
        scan.correlate_histories(
            root,
            &commits,
            &mut histories,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("literal path diff");
        let related = histories["bd-one"]
            .commits
            .as_ref()
            .expect("renamed metadata still correlates");
        assert_eq!(related.len(), 1);
        assert_eq!(
            related[0].bead_diff_lines,
            ["+ issue: bd-one [open] Issue bd-one"]
        );
        assert!(
            histories["bd-other"].commits.is_none(),
            "glob-like filename is a literal pathspec"
        );
    }

    #[test]
    fn bounded_history_diff_failure_leaves_all_maps_unchanged() {
        let repo = small_history_repo();
        let mut scan = GitHistoryScan::default();
        let mut commits = scan.load_commits(repo.path(), 1, None).unwrap();
        let mut missing = commits[0].clone();
        missing.sha = "0000000000000000000000000000000000000000".to_string();
        commits.push(missing);
        let mut histories = empty_histories(&["bd-one"]);
        let before = serde_json::to_value(&histories).unwrap();
        let mut index = BTreeMap::from([("existing".to_string(), vec!["bd-one".to_string()])]);
        let mut methods = BTreeMap::from([("existing".to_string(), 1)]);
        let error = scan
            .correlate_histories(
                repo.path(),
                &commits,
                &mut histories,
                &mut index,
                &mut methods,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("git show 0000000000000000000000000000000000000000 failed")
        );
        assert_eq!(serde_json::to_value(&histories).unwrap(), before);
        assert_eq!(
            index,
            BTreeMap::from([("existing".to_string(), vec!["bd-one".to_string()])])
        );
        assert_eq!(methods, BTreeMap::from([("existing".to_string(), 1)]));
    }

    #[test]
    fn bounded_history_corrupt_head_is_not_an_unborn_repository() {
        let repo = small_history_repo();
        std::fs::write(
            repo.path().join(".git/HEAD"),
            "0000000000000000000000000000000000000000\n",
        )
        .expect("corrupt detached HEAD in temporary repository");
        let mut scan = GitHistoryScan::default();
        let error = scan.head_sha(repo.path()).unwrap_err();
        assert!(error.to_string().contains("git rev-parse HEAD failed"));
        assert!(scan.load_commits(repo.path(), 500, None).is_err());
    }

    #[test]
    fn bounded_history_aggregate_output_budget_spans_load_and_correlation() {
        let repo = small_history_repo();
        let mut scan = GitHistoryScan::default();
        let commits = scan.load_commits(repo.path(), 1, None).unwrap();
        scan.limits.max_output = scan.output_bytes;
        let mut histories = empty_histories(&["bd-one"]);
        let before = serde_json::to_value(&histories).unwrap();
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        let error = scan
            .correlate_histories(
                repo.path(),
                &commits,
                &mut histories,
                &mut index,
                &mut methods,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("request output exceeded"));
        assert!(error.to_string().contains("--history-limit"));
        assert_eq!(serde_json::to_value(&histories).unwrap(), before);
        assert_eq!(index.len(), 0);
        assert_eq!(methods.len(), 0);
    }

    #[test]
    fn bounded_history_rejects_result_amplification_before_publishing() {
        let repo = small_history_repo();
        let mut scan = GitHistoryScan::default();
        let commits = scan.load_commits(repo.path(), 1, None).unwrap();
        scan.limits.max_result_bytes = 1;
        let mut histories = empty_histories(&["bd-one"]);
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        let error = scan
            .correlate_histories(
                repo.path(),
                &commits,
                &mut histories,
                &mut index,
                &mut methods,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("correlation data exceeded"));
        assert!(histories["bd-one"].commits.is_none());
        assert_eq!(index.len(), 0);
        assert_eq!(methods.len(), 0);
    }

    #[cfg(unix)]
    fn fake_history_git(directory: &Path, script: &str) -> OsString {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("fake-git");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("fake Git script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("executable script");
        path.into_os_string()
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_kills_a_stalled_git_command() {
        let temp = tempfile::tempdir().unwrap();
        let mut scan = GitHistoryScan::default();
        scan.git_program = fake_history_git(temp.path(), "while :; do :; done");
        scan.limits.command_timeout = Duration::from_millis(50);
        let start = Instant::now();
        let error = scan.load_commits(temp.path(), 500, None).unwrap_err();
        assert!(
            matches!(&error, BvrError::Io(error) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(error.to_string().contains("git rev-parse timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(scan.commands_started, 1);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_cancellation_interrupts_an_active_git_command() {
        let temp = tempfile::tempdir().unwrap();
        let mut scan = GitHistoryScan::default();
        scan.git_program = fake_history_git(temp.path(), "while :; do :; done");
        let token = scan.cancellation_token();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            token.cancel();
        });
        let start = Instant::now();
        let error = scan.load_commits(temp.path(), 500, None).unwrap_err();
        cancel.join().expect("cancellation thread");
        assert!(
            matches!(&error, BvrError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted)
        );
        assert!(error.to_string().contains("request cancelled"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_enforces_both_output_stream_limits() {
        for stream in ["stdout", "stderr"] {
            let temp = tempfile::tempdir().unwrap();
            let mut scan = GitHistoryScan::default();
            let redirect = if stream == "stderr" { " >&2" } else { "" };
            scan.git_program = fake_history_git(
                temp.path(),
                &format!("while :; do printf '0123456789abcdef'{redirect}; done"),
            );
            scan.limits.max_stdout = 1024;
            scan.limits.max_stderr = 1024;
            let start = Instant::now();
            let error = scan.load_commits(temp.path(), 500, None).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("{stream} exceeded 1024 bytes")),
                "{error}"
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_aggregate_deadline_spans_load_and_correlation() {
        let repo = small_history_repo();
        let tools = tempfile::tempdir().unwrap();
        let mut scan = GitHistoryScan::default();
        scan.git_program = fake_history_git(
            tools.path(),
            "for arg do\n  if [ \"$arg\" = show ]; then while :; do :; done; fi\ndone\nexec git \"$@\"",
        );
        let commits = scan.load_commits(repo.path(), 1, None).unwrap();
        // Preserve the original request start. Give the following subprocess a
        // small remaining aggregate budget while its own limit stays at 5s.
        scan.limits.scan_timeout = scan.started.elapsed() + Duration::from_millis(50);
        let mut histories = empty_histories(&["bd-one"]);
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        let start = Instant::now();
        let error = scan
            .correlate_histories(
                repo.path(),
                &commits,
                &mut histories,
                &mut index,
                &mut methods,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("request exceeded"));
        assert!(error.to_string().contains("--history-limit"));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(histories["bd-one"].commits.is_none());
        assert_eq!(index.len(), 0);
        assert_eq!(methods.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_history_log_timeout_still_consumes_the_aggregate_budget() {
        let repo = small_history_repo();
        let tools = tempfile::tempdir().unwrap();
        let program = fake_history_git(
            tools.path(),
            "for arg do\n  if [ \"$arg\" = log ]; then while :; do :; done; fi\ndone\nexec git \"$@\"",
        );
        for aggregate_expires_first in [false, true] {
            let mut scan = GitHistoryScan::default();
            scan.git_program = program.clone();
            assert!(scan.repository_root(repo.path()).unwrap().is_some());
            scan.limits.log_timeout = if aggregate_expires_first {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(100)
            };
            scan.limits.scan_timeout = scan.started.elapsed()
                + if aggregate_expires_first {
                    Duration::from_millis(150)
                } else {
                    Duration::from_secs(2)
                };
            let started = Instant::now();
            let error = scan.load_commits(repo.path(), 500, None).unwrap_err();
            let expected = if aggregate_expires_first {
                "request exceeded"
            } else {
                "git log timed out"
            };
            assert!(error.to_string().contains(expected), "{error}");
            assert_eq!(
                scan.commands_started, 3,
                "root lookup, work-tree probe, then stalled log"
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }

    #[test]
    fn bounded_history_checks_cancellation_during_parsing_and_before_publication() {
        let mut scan = GitHistoryScan::default();
        scan.cancellation_token().cancel();
        assert!(
            scan.parse_commit_log(b"ignored")
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(
            scan.parse_bead_change_details(b"+{}", &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        let mut histories = empty_histories(&["bd-one"]);
        let mut index = BTreeMap::new();
        let mut methods = BTreeMap::new();
        assert!(
            scan.correlate_histories(
                Path::new("."),
                &[],
                &mut histories,
                &mut index,
                &mut methods,
                &BTreeMap::new()
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
        assert_eq!(scan.commands_started, 0);
    }

    #[test]
    fn has_word_token_rejects_substring_match() {
        // "disclose" should NOT match "close"
        assert!(!has_word_token("disclose the issue", "close"));
        // "exclaim" should NOT match "claim"
        assert!(!has_word_token("exclaim loudly", "claim"));
        // "reopen" embedded in "unreopened" should not match at word boundary
        assert!(!has_word_token("unreopened", "reopen"));
        // Suffix continuations should not match either
        assert!(!has_word_token("closedown the task", "closed"));
        assert!(!has_word_token("claimer picked it up", "claim"));
    }

    #[test]
    fn has_word_token_accepts_word_boundary() {
        assert!(has_word_token("close the issue", "close"));
        assert!(has_word_token("closed bd-123", "closed"));
        assert!(has_word_token("claim this task", "claim"));
        assert!(has_word_token("reopen bd-456", "reopen"));
        assert!(has_word_token("reopened bd-456", "reopened"));
        // At start of text
        assert!(has_word_token("close", "close"));
        // After punctuation
        assert!(has_word_token("[close] bd-789", "close"));
    }

    #[test]
    fn infer_event_type_close_vs_disclose() {
        assert_eq!(infer_event_type_from_commit(1, "close bd-123"), "closed");
        assert_eq!(infer_event_type_from_commit(1, "Closed bd-123"), "closed");
        // "disclose" should NOT trigger "closed"
        assert_eq!(
            infer_event_type_from_commit(1, "disclose internal details"),
            "modified"
        );
        assert_eq!(
            infer_event_type_from_commit(1, "closedown remaining tasks"),
            "modified"
        );
    }

    #[test]
    fn infer_event_type_claim_vs_exclaim() {
        assert_eq!(infer_event_type_from_commit(1, "claim bd-abc"), "claimed");
        assert_eq!(
            infer_event_type_from_commit(1, "set status to in_progress"),
            "claimed"
        );
        assert_eq!(
            infer_event_type_from_commit(1, "mark in progress"),
            "claimed"
        );
        // "exclaim" should NOT trigger "claimed"
        assert_eq!(
            infer_event_type_from_commit(1, "exclaim about progress"),
            "modified"
        );
        assert_eq!(
            infer_event_type_from_commit(1, "claimer rotation updated"),
            "modified"
        );
    }

    #[test]
    fn infer_event_type_reopen_vs_embedded() {
        assert_eq!(infer_event_type_from_commit(1, "reopen bd-xyz"), "reopened");
        assert_eq!(
            infer_event_type_from_commit(1, "Reopened the issue"),
            "reopened"
        );
    }

    #[test]
    fn infer_event_type_index_zero_fallback() {
        // Index 0 with no keyword match => "created"
        assert_eq!(infer_event_type_from_commit(0, "initial setup"), "created");
        // Index >0 with no keyword match => "modified"
        assert_eq!(infer_event_type_from_commit(1, "update readme"), "modified");
    }

    #[test]
    fn none_commits_serialize_as_null() {
        let history = HistoryBeadCompat {
            bead_id: "bd-test".to_string(),
            title: "Test".to_string(),
            status: "open".to_string(),
            events: vec![],
            milestones: HistoryMilestonesCompat::default(),
            commits: None,
            cycle_time: None,
            last_author: String::new(),
        };
        let json = serde_json::to_value(&history).unwrap();
        // commits and cycle_time are always present (as null) to match
        // legacy Go output shape.
        assert!(
            json.get("commits").is_some_and(serde_json::Value::is_null),
            "None commits should serialize as null"
        );
        assert!(
            json.get("cycle_time")
                .is_some_and(serde_json::Value::is_null),
            "None cycle_time should serialize as null"
        );
    }

    #[test]
    fn some_commits_serialize_as_array() {
        let history = HistoryBeadCompat {
            bead_id: "bd-test".to_string(),
            title: "Test".to_string(),
            status: "open".to_string(),
            events: vec![],
            milestones: HistoryMilestonesCompat::default(),
            commits: Some(vec![HistoryCommitCompat {
                sha: "abc123".to_string(),
                short_sha: "abc".to_string(),
                message: "test commit".to_string(),
                author: "tester".to_string(),
                author_email: "test@example.com".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
                files: vec![],
                method: "log".to_string(),
                confidence: 1.0,
                reason: "test".to_string(),
                field_changes: vec![],
                bead_diff_lines: vec![],
            }]),
            cycle_time: None,
            last_author: String::new(),
        };
        let json = serde_json::to_value(&history).unwrap();
        assert!(
            json["commits"].is_array(),
            "Some commits should serialize as array"
        );
        assert_eq!(json["commits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn milestones_omit_null_fields() {
        let milestones = HistoryMilestonesCompat {
            created: Some(HistoryEventCompat {
                bead_id: "bd-test".to_string(),
                event_type: "created".to_string(),
                timestamp: "2024-01-01T00:00:00Z".to_string(),
                commit_sha: "abc".to_string(),
                commit_message: "init".to_string(),
                author: "tester".to_string(),
                author_email: "test@example.com".to_string(),
            }),
            claimed: None,
            closed: None,
            reopened: None,
        };
        let json = serde_json::to_value(&milestones).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("created"), "created should be present");
        assert!(
            !obj.contains_key("claimed"),
            "None claimed should be omitted"
        );
        assert!(!obj.contains_key("closed"), "None closed should be omitted");
        assert!(
            !obj.contains_key("reopened"),
            "None reopened should be omitted"
        );
    }

    #[test]
    fn cycle_time_omits_null_fields() {
        let cycle = HistoryCycleCompat {
            create_to_close: Some("2d 3h 0m".to_string()),
            claim_to_close: None,
            create_to_claim: None,
        };
        let json = serde_json::to_value(&cycle).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("create_to_close"));
        assert!(
            !obj.contains_key("claim_to_close"),
            "None should be omitted"
        );
        assert!(
            !obj.contains_key("create_to_claim"),
            "None should be omitted"
        );
    }

    #[test]
    fn history_stats_omit_absent_avg_cycle_time_days() {
        let stats = HistoryStatsCompat {
            total_beads: 2,
            beads_with_commits: 0,
            total_commits: 0,
            unique_authors: 0,
            avg_commits_per_bead: 0.0,
            avg_cycle_time_days: None,
            method_distribution: BTreeMap::new(),
        };

        let json = serde_json::to_value(&stats).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            !obj.contains_key("avg_cycle_time_days"),
            "None avg_cycle_time_days should be omitted"
        );
    }

    #[test]
    fn is_beads_jsonl_path_accepts_nested_workspace_beads_files() {
        assert!(is_beads_jsonl_path("services/api/.beads/issues.jsonl"));
        assert!(is_beads_jsonl_path("apps\\web\\.beads\\beads.jsonl"));
        assert!(!is_beads_jsonl_path("services/api/beads/issues.jsonl"));
        assert!(!is_beads_jsonl_path("services/api/.beads/issues.json"));
    }

    #[test]
    fn is_beads_jsonl_path_excludes_generated_history_with_exact_components() {
        for path in [
            ".beads/.br_history/issues.20260101.jsonl",
            "services/api/.beads/.br_history/older/issues.jsonl",
            "apps\\web\\.beads\\.br_history\\beads.jsonl",
        ] {
            assert!(is_br_history_path(path), "{path}");
            assert!(!is_beads_jsonl_path(path), "{path}");
        }
        assert!(is_br_history_path(".beads/.br_history/snapshot.meta.json"));
        for path in [
            ".br_history/project/.beads/issues.jsonl",
            ".beads/.br_history-exports/custom.jsonl",
            ".beads/custom/.br_history.jsonl",
        ] {
            assert!(!is_br_history_path(path), "{path}");
            assert!(is_beads_jsonl_path(path), "{path}");
        }
    }

    #[test]
    fn ids_from_patterns_prefers_capture_group_then_whole_match() {
        let grouped = regex::Regex::new(r"ticket\s+(bh-[a-z0-9]{5})").unwrap();
        let whole = regex::Regex::new(r"bh-[a-z0-9]{5}").unwrap();
        let message = "ticket bh-8g6cj landed; follow-up bh-x1y2z";
        assert_eq!(
            ids_from_patterns(message, &[grouped, whole]),
            vec!["bh-8g6cj".to_string(), "bh-x1y2z".to_string()]
        );
        assert!(ids_from_patterns("nothing here", &[]).is_empty());
    }

    #[test]
    fn custom_patterns_resolve_ids_glued_to_other_text() {
        // `bh-8g6cj` is glued to `fix:` text, so the whole-token matcher
        // misses it; the custom pattern still extracts it.
        let mut known = BTreeMap::new();
        known.insert("bh-8g6cj".to_string(), "BH-8g6cj".to_string());
        let message = "fix(ui):bh-8g6cj_followup";
        assert!(extract_ids_from_message_with_patterns(message, &known, &[]).is_empty());
        let pattern = regex::Regex::new(r"(bh-[0-9a-z]{5})_").unwrap();
        let ids = extract_ids_from_message_with_patterns(message, &known, &[pattern]);
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec!["BH-8g6cj"]);
    }

    #[test]
    fn custom_patterns_ignore_unknown_ids() {
        let known = BTreeMap::new();
        let pattern = regex::Regex::new(r"bh-[a-z0-9]{5}").unwrap();
        assert!(extract_ids_from_message_with_patterns("bh-zzzzz", &known, &[pattern]).is_empty());
    }

    #[test]
    fn build_workspace_id_aliases_maps_raw_to_prefixed() {
        let issues = vec![
            Issue {
                id: "api-bd-1234".to_string(),
                source_repo: "api".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "web-bd-5678".to_string(),
                source_repo: "web".to_string(),
                ..Issue::default()
            },
        ];

        let aliases = build_workspace_id_aliases(&issues);
        assert_eq!(aliases.get("bd-1234").unwrap(), "api-bd-1234");
        assert_eq!(aliases.get("bd-5678").unwrap(), "web-bd-5678");
    }

    #[test]
    fn build_workspace_id_aliases_skips_non_workspace_issues() {
        let issues = vec![Issue {
            id: "bd-abcd".to_string(),
            source_repo: String::new(),
            ..Issue::default()
        }];

        let aliases = build_workspace_id_aliases(&issues);
        assert!(aliases.is_empty());
    }

    #[test]
    fn build_workspace_id_aliases_handles_case_insensitive_prefix() {
        let issues = vec![Issue {
            id: "API-bd-1234".to_string(),
            source_repo: "API".to_string(),
            ..Issue::default()
        }];

        let aliases = build_workspace_id_aliases(&issues);
        assert_eq!(aliases.get("bd-1234").unwrap(), "API-bd-1234");
    }

    #[test]
    fn build_workspace_id_aliases_first_wins_on_collision() {
        let issues = vec![
            Issue {
                id: "api-bd-same".to_string(),
                source_repo: "api".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "web-bd-same".to_string(),
                source_repo: "web".to_string(),
                ..Issue::default()
            },
        ];

        let aliases = build_workspace_id_aliases(&issues);
        // First insertion wins (BTreeMap::entry + or_insert)
        assert_eq!(aliases.get("bd-same").unwrap(), "api-bd-same");
    }

    #[test]
    fn build_workspace_id_aliases_uses_workspace_prefix_when_repo_name_differs() {
        let issues = vec![Issue {
            id: "api-bd-1234".to_string(),
            source_repo: "payments-service".to_string(),
            workspace_prefix: Some("api-".to_string()),
            ..Issue::default()
        }];

        let aliases = build_workspace_id_aliases(&issues);
        assert_eq!(aliases.get("bd-1234").unwrap(), "api-bd-1234");
    }

    #[test]
    fn correlate_with_aliases_matches_raw_ids_from_commit_messages() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "api-bd-1234".to_string(),
            HistoryBeadCompat {
                bead_id: "api-bd-1234".to_string(),
                title: "Test issue".to_string(),
                status: "open".to_string(),
                events: Vec::new(),
                milestones: HistoryMilestonesCompat::default(),
                commits: None,
                cycle_time: None,
                last_author: String::new(),
            },
        );

        let commits = vec![GitCommitRecord {
            sha: "abc123".to_string(),
            short_sha: "abc".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            author: "dev".to_string(),
            author_email: "dev@example.com".to_string(),
            message: "fix: resolve bd-1234 bug".to_string(),
            files: Vec::new(),
            changed_beads: false,
            changed_non_beads: true,
        }];

        let aliases: BTreeMap<String, String> =
            std::iter::once(("bd-1234".to_string(), "api-bd-1234".to_string())).collect();

        let mut commit_index = BTreeMap::new();
        let mut method_dist = BTreeMap::new();

        // Without aliases, the raw ID "bd-1234" won't match "api-bd-1234"
        correlate_histories_with_git_aliases(
            Path::new("."),
            &commits,
            &mut histories,
            &mut commit_index,
            &mut method_dist,
            &aliases,
        )
        .expect("message-only correlation");

        let history = histories.get("api-bd-1234").unwrap();
        assert!(
            history.commits.as_ref().is_some_and(|c| !c.is_empty()),
            "raw ID bd-1234 in commit message should match prefixed api-bd-1234 via alias"
        );
    }
}

pub fn compute_history_stats(
    histories_map: &BTreeMap<String, HistoryBeadCompat>,
    commit_index: &BTreeMap<String, Vec<String>>,
    method_distribution: BTreeMap<String, usize>,
) -> HistoryStatsCompat {
    let total_beads = histories_map.len();
    let beads_with_commits = histories_map
        .values()
        .filter(|history| history.commits.as_ref().is_some_and(|c| !c.is_empty()))
        .count();
    let total_commits = commit_index.len();

    let mut authors = BTreeSet::<String>::new();
    let mut claim_to_close_days = Vec::<f64>::new();

    for history in histories_map.values() {
        for commit in history.commits.as_deref().unwrap_or_default() {
            if !commit.author.is_empty() {
                authors.insert(commit.author.clone());
            }
        }
        for event in &history.events {
            if !event.author.is_empty() {
                authors.insert(event.author.clone());
            }
        }

        if let Some(duration) = duration_between(
            history
                .milestones
                .claimed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
            history
                .milestones
                .closed
                .as_ref()
                .map(|event| event.timestamp.as_str()),
        ) {
            let seconds_i32 = i32::try_from(duration.num_seconds()).unwrap_or(i32::MAX);
            const SECS_PER_DAY: f64 = 86_400.0;
            claim_to_close_days.push(f64::from(seconds_i32) / SECS_PER_DAY);
        }
    }

    let avg_commits_per_bead = if beads_with_commits == 0 {
        0.0
    } else {
        let total_commits_u32 = u32::try_from(total_commits).unwrap_or(u32::MAX);
        let beads_with_commits_u32 = u32::try_from(beads_with_commits).unwrap_or(u32::MAX);
        f64::from(total_commits_u32) / f64::from(beads_with_commits_u32)
    };

    let avg_cycle_time_days = if claim_to_close_days.is_empty() {
        None
    } else {
        let count_u32 = u32::try_from(claim_to_close_days.len()).unwrap_or(u32::MAX);
        Some(claim_to_close_days.iter().sum::<f64>() / f64::from(count_u32))
    };

    HistoryStatsCompat {
        total_beads,
        beads_with_commits,
        total_commits,
        unique_authors: authors.len(),
        avg_commits_per_bead,
        avg_cycle_time_days,
        method_distribution,
    }
}
