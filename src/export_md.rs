use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;

use crate::model::Issue;
use crate::{BvrError, Result};

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize, Default)]
struct HooksFile {
    #[serde(default)]
    hooks: HookPhases,
}

#[derive(Debug, Deserialize, Default)]
struct HookPhases {
    #[serde(rename = "pre-export", default)]
    pre_export: Vec<HookSpec>,
    #[serde(rename = "post-export", default)]
    post_export: Vec<HookSpec>,
}

#[derive(Debug, Deserialize, Default)]
struct HookSpec {
    #[serde(default)]
    name: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    timeout: Option<HookTimeoutValue>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    on_error: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HookTimeoutValue {
    Text(String),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    PreExport,
    PostExport,
}

impl HookPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::PreExport => "pre-export",
            Self::PostExport => "post-export",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookOnError {
    Fail,
    Continue,
}

#[derive(Debug, Clone)]
struct HookRuntime {
    name: String,
    command: String,
    timeout: Duration,
    env: BTreeMap<String, String>,
    on_error: HookOnError,
}

#[derive(Debug, Clone)]
struct HookContext {
    export_path: String,
    export_format: String,
    issue_count: usize,
    timestamp: String,
}

impl HookContext {
    fn new(export_path: &Path, export_format: &str, issue_count: usize) -> Self {
        // Resolve to absolute so BV_EXPORT_PATH is valid regardless of the
        // hook's working directory (which may differ when --repo-path is used).
        let abs_path = if export_path.is_absolute() {
            export_path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(export_path)
        };
        Self {
            export_path: abs_path.to_string_lossy().to_string(),
            export_format: export_format.to_string(),
            issue_count,
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone)]
struct HookRunResult {
    name: String,
    success: bool,
    duration: Duration,
    error: Option<String>,
    stderr: String,
}

pub fn export_markdown_with_hooks(
    issues: &[Issue],
    export_path: &Path,
    no_hooks: bool,
    repo_path: Option<&Path>,
) -> Result<()> {
    run_export_with_hooks(
        export_path,
        "markdown",
        issues.len(),
        no_hooks,
        repo_path,
        || write_markdown_report(issues, export_path),
    )
}

pub fn run_export_with_hooks<T, F>(
    export_path: &Path,
    export_format: &str,
    issue_count: usize,
    no_hooks: bool,
    repo_path: Option<&Path>,
    export_fn: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let project_dir = resolve_project_dir(repo_path)?;
    let hook_context = HookContext::new(export_path, export_format, issue_count);

    let hooks = if no_hooks {
        HookPhases::default()
    } else {
        let hook_config_path = project_dir.join(".bv").join("hooks.yaml");
        match load_hooks(&hook_config_path) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("Warning: failed to load hooks: {error}");
                HookPhases::default()
            }
        }
    };

    let mut hook_results = Vec::<HookRunResult>::new();

    for hook in normalize_hooks(&hooks.pre_export, HookPhase::PreExport) {
        let result = run_hook(&hook, &hook_context, &project_dir)?;
        let should_fail = !result.success && hook.on_error == HookOnError::Fail;
        let error_message = result.error.clone();
        let hook_name = result.name.clone();
        hook_results.push(result);

        if should_fail {
            let reason = error_message.unwrap_or_else(|| "unknown error".to_string());
            return Err(BvrError::InvalidArgument(format!(
                "pre-export hook {hook_name:?} failed: {reason}"
            )));
        }
    }

    let output = export_fn()?;

    let mut post_failure = None::<String>;
    for hook in normalize_hooks(&hooks.post_export, HookPhase::PostExport) {
        let result = run_hook(&hook, &hook_context, &project_dir)?;
        if !result.success && hook.on_error == HookOnError::Fail && post_failure.is_none() {
            let reason = result
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            post_failure = Some(format!("hook {:?} failed: {reason}", result.name));
        }
        hook_results.push(result);
    }

    if let Some(error) = post_failure {
        eprintln!("Warning: post-export hook failed: {error}");
    }

    if !hook_results.is_empty() {
        println!("{}", format_hook_summary(&hook_results));
    }

    Ok(output)
}

fn resolve_project_dir(repo_path: Option<&Path>) -> Result<PathBuf> {
    let base = if let Some(path) = repo_path {
        path.to_path_buf()
    } else {
        std::env::current_dir()?
    };
    // Ensure absolute so hooks always run from a fully-resolved directory,
    // even when --repo-path is a relative path.
    if base.is_absolute() {
        Ok(base)
    } else {
        Ok(std::env::current_dir()?.join(base))
    }
}

fn load_hooks(path: &Path) -> Result<HookPhases> {
    if !path.exists() {
        return Ok(HookPhases::default());
    }

    let text = std::fs::read_to_string(path)?;
    let config = serde_yaml::from_str::<HooksFile>(&text).map_err(|error| {
        BvrError::InvalidArgument(format!("parsing {}: {error}", path.display()))
    })?;

    Ok(config.hooks)
}

fn normalize_hooks(hooks: &[HookSpec], phase: HookPhase) -> Vec<HookRuntime> {
    let mut normalized = Vec::<HookRuntime>::new();

    for (index, hook) in hooks.iter().enumerate() {
        if hook.command.trim().is_empty() {
            continue;
        }

        let name = if hook.name.trim().is_empty() {
            format!("{}-{}", phase.label(), index + 1)
        } else {
            hook.name.trim().to_string()
        };

        let timeout = parse_hook_timeout(hook.timeout.as_ref()).unwrap_or(DEFAULT_HOOK_TIMEOUT);

        let on_error = parse_on_error(&hook.on_error, phase);

        normalized.push(HookRuntime {
            name,
            command: hook.command.trim().to_string(),
            timeout,
            env: hook.env.clone(),
            on_error,
        });
    }

    normalized
}

fn parse_on_error(raw: &str, phase: HookPhase) -> HookOnError {
    let default = match phase {
        HookPhase::PreExport => HookOnError::Fail,
        HookPhase::PostExport => HookOnError::Continue,
    };

    match raw.trim().to_ascii_lowercase().as_str() {
        "fail" => HookOnError::Fail,
        "continue" => HookOnError::Continue,
        _ => default,
    }
}

fn parse_hook_timeout(raw: Option<&HookTimeoutValue>) -> Option<Duration> {
    let raw = raw?;

    match raw {
        HookTimeoutValue::Unsigned(value) => Some(Duration::from_secs(*value)),
        HookTimeoutValue::Signed(value) => u64::try_from(*value).ok().map(Duration::from_secs),
        HookTimeoutValue::Float(value) => {
            if *value > 0.0 {
                Some(Duration::from_secs_f64(*value))
            } else {
                None
            }
        }
        HookTimeoutValue::Text(value) => parse_timeout_text(value),
    }
}

fn parse_timeout_text(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let normalized = trimmed.to_ascii_lowercase();
    let units = [
        ("ms", 0.001_f64),
        ("s", 1.0_f64),
        ("m", 60.0_f64),
        ("h", 3600.0_f64),
    ];

    for (suffix, multiplier) in units {
        if let Some(number) = normalized.strip_suffix(suffix)
            && let Ok(value) = number.trim().parse::<f64>()
            && value > 0.0
        {
            return Some(Duration::from_secs_f64(value * multiplier));
        }
    }

    None
}

fn run_hook(
    hook: &HookRuntime,
    context: &HookContext,
    project_dir: &Path,
) -> Result<HookRunResult> {
    let (shell, shell_flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };

    let mut command = Command::new(shell);
    command.arg(shell_flag).arg(&hook.command);
    command.current_dir(project_dir);
    command.env_clear();
    command.envs(build_hook_env(hook, context));
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;

    let stdout_reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut bytes = Vec::<u8>::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut bytes = Vec::<u8>::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        })
    });

    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }

        if started.elapsed() > hook.timeout {
            let _ = child.kill();
            break (child.wait()?, true);
        }

        std::thread::sleep(Duration::from_millis(10));
    };
    let elapsed = started.elapsed();

    // If the hook timed out, do not join IO reader threads because children of the shell
    // may still hold inherited pipes open briefly; dropping the handles detaches the threads
    // and keeps timeout behavior bounded.
    let stderr = if timed_out {
        drop(stdout_reader);
        drop(stderr_reader);
        String::new()
    } else {
        let _ = stdout_reader.map_or_else(Vec::new, |reader| reader.join().unwrap_or_default());
        let stderr =
            stderr_reader.map_or_else(Vec::new, |reader| reader.join().unwrap_or_default());
        String::from_utf8_lossy(&stderr).trim().to_string()
    };

    let success = status.success() && !timed_out;
    let error = if timed_out {
        Some(format!("timeout after {}ms", hook.timeout.as_millis()))
    } else if status.success() {
        None
    } else {
        Some(format!("exit status {status}"))
    };

    Ok(HookRunResult {
        name: hook.name.clone(),
        success,
        duration: elapsed,
        error,
        stderr,
    })
}

fn build_hook_env(hook: &HookRuntime, context: &HookContext) -> BTreeMap<String, String> {
    let mut env = std::env::vars().collect::<BTreeMap<String, String>>();
    env.insert("BV_EXPORT_PATH".to_string(), context.export_path.clone());
    env.insert(
        "BV_EXPORT_FORMAT".to_string(),
        context.export_format.clone(),
    );
    env.insert(
        "BV_ISSUE_COUNT".to_string(),
        context.issue_count.to_string(),
    );
    env.insert("BV_TIMESTAMP".to_string(), context.timestamp.clone());

    let mut keys = hook.env.keys().cloned().collect::<Vec<_>>();
    keys.sort_unstable();

    for key in keys {
        let value = hook.env.get(&key).cloned().unwrap_or_default();
        let expanded = expand_env_like_shell(&value, &env);
        env.insert(key, expanded);
    }

    env
}

fn expand_env_like_shell(input: &str, env: &BTreeMap<String, String>) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut idx = 0usize;
    let mut output = String::new();

    while idx < chars.len() {
        if chars[idx] != '$' {
            output.push(chars[idx]);
            idx += 1;
            continue;
        }

        if idx + 1 >= chars.len() {
            output.push('$');
            idx += 1;
            continue;
        }

        if chars[idx + 1] == '{' {
            let mut cursor = idx + 2;
            while cursor < chars.len() && chars[cursor] != '}' {
                cursor += 1;
            }
            if cursor < chars.len() && chars[cursor] == '}' {
                let key = chars[idx + 2..cursor].iter().collect::<String>();
                output.push_str(env.get(&key).map_or("", String::as_str));
                idx = cursor + 1;
                continue;
            }
        }

        if !is_env_var_start(chars[idx + 1]) {
            output.push('$');
            idx += 1;
            continue;
        }

        let mut cursor = idx + 1;
        while cursor < chars.len() && is_env_var_char(chars[cursor]) {
            cursor += 1;
        }

        let key = chars[idx + 1..cursor].iter().collect::<String>();
        output.push_str(env.get(&key).map_or("", String::as_str));
        idx = cursor;
    }

    output
}

fn is_env_var_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_env_var_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn format_hook_summary(results: &[HookRunResult]) -> String {
    if results.is_empty() {
        return "No hooks executed".to_string();
    }

    let succeeded = results.iter().filter(|result| result.success).count();
    let failed = results.len().saturating_sub(succeeded);

    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "Hook execution: {succeeded} succeeded, {failed} failed"
    );

    for result in results {
        if result.success {
            let _ = writeln!(summary, "  [OK] {} ({:?})", result.name, result.duration);
            continue;
        }

        let reason = result
            .error
            .clone()
            .unwrap_or_else(|| "unknown error".to_string());
        let _ = writeln!(summary, "  [FAIL] {}: {}", result.name, reason);
        if !result.stderr.is_empty() {
            let _ = writeln!(
                summary,
                "         stderr: {}",
                truncate_runes(&result.stderr, 200)
            );
        }
    }

    summary.trim_end().to_string()
}

fn write_markdown_report(issues: &[Issue], export_path: &Path) -> Result<()> {
    if let Some(parent) = export_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let report = generate_markdown_report(issues);
    std::fs::write(export_path, report)?;
    Ok(())
}

fn generate_markdown_report(issues: &[Issue]) -> String {
    let mut sorted = issues.to_vec();
    sorted.sort_by(compare_issues_for_markdown);

    let mut open_count = 0usize;
    let mut in_progress_count = 0usize;
    let mut blocked_count = 0usize;
    let mut closed_count = 0usize;

    for issue in &sorted {
        let normalized = issue.status.trim().to_ascii_lowercase();
        if is_closed_like_status(&normalized) {
            closed_count += 1;
            continue;
        }

        match normalized.as_str() {
            "in_progress" => in_progress_count += 1,
            "blocked" => blocked_count += 1,
            _ => open_count += 1,
        }
    }

    let mut markdown = String::new();
    let _ = writeln!(markdown, "# Beads Export\n");
    let _ = writeln!(markdown, "*Generated: {}*\n", Utc::now().to_rfc3339());

    markdown.push_str("## Summary\n\n");
    markdown.push_str("| Metric | Count |\n");
    markdown.push_str("|---|---:|\n");
    let total_count = sorted.len();
    let _ = writeln!(markdown, "| Total | {total_count} |");
    let _ = writeln!(markdown, "| Open | {open_count} |");
    let _ = writeln!(markdown, "| In Progress | {in_progress_count} |");
    let _ = writeln!(markdown, "| Blocked | {blocked_count} |");
    let _ = writeln!(markdown, "| Closed | {closed_count} |\n");

    for issue in sorted {
        let _ = writeln!(markdown, "## {} {}\n", issue.id, issue.title);
        let _ = writeln!(markdown, "- Status: `{}`", issue.status);
        let _ = writeln!(markdown, "- Priority: `P{}`", issue.priority);
        let _ = writeln!(markdown, "- Type: `{}`", issue.issue_type);
        if !issue.assignee.trim().is_empty() {
            let _ = writeln!(markdown, "- Assignee: `{}`", issue.assignee.trim());
        }

        if !issue.labels.is_empty() {
            let labels = issue
                .labels
                .iter()
                .map(|label| format!("`{}`", label.trim()))
                .collect::<Vec<_>>();
            let _ = writeln!(markdown, "- Labels: {}", labels.join(", "));
        }

        markdown.push('\n');

        if !issue.description.trim().is_empty() {
            markdown.push_str("### Description\n\n");
            markdown.push_str(issue.description.trim());
            markdown.push_str("\n\n");
        }

        if !issue.acceptance_criteria.trim().is_empty() {
            markdown.push_str("### Acceptance Criteria\n\n");
            markdown.push_str(issue.acceptance_criteria.trim());
            markdown.push_str("\n\n");
        }

        if !issue.design.trim().is_empty() {
            markdown.push_str("### Design\n\n");
            markdown.push_str(issue.design.trim());
            markdown.push_str("\n\n");
        }

        if !issue.notes.trim().is_empty() {
            markdown.push_str("### Notes\n\n");
            markdown.push_str(issue.notes.trim());
            markdown.push_str("\n\n");
        }

        let dependencies = issue
            .dependencies
            .iter()
            .filter_map(|dep| {
                let depends_on = dep.depends_on_id.trim();
                if depends_on.is_empty() {
                    return None;
                }

                let dep_type = if dep.dep_type.trim().is_empty() {
                    "blocks"
                } else {
                    dep.dep_type.trim()
                };

                Some(format!("- `{depends_on}` (`{dep_type}`)"))
            })
            .collect::<Vec<_>>();

        if !dependencies.is_empty() {
            markdown.push_str("### Dependencies\n\n");
            markdown.push_str(&dependencies.join("\n"));
            markdown.push_str("\n\n");
        }

        if !issue.comments.is_empty() {
            markdown.push_str("### Comments\n\n");
            for comment in issue.comments {
                let author = if comment.author.trim().is_empty() {
                    "unknown"
                } else {
                    comment.author.trim()
                };
                let text = comment.text.trim();
                let _ = writeln!(markdown, "- **{author}**: {text}");
            }
            markdown.push('\n');
        }

        markdown.push_str("---\n\n");
    }

    markdown
}

fn compare_issues_for_markdown(left: &Issue, right: &Issue) -> Ordering {
    let left_closed = is_closed_like_status(&left.status.trim().to_ascii_lowercase());
    let right_closed = is_closed_like_status(&right.status.trim().to_ascii_lowercase());

    if left_closed != right_closed {
        return if left_closed {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }

    if left.priority != right.priority {
        return left.priority.cmp(&right.priority);
    }

    let left_created = left.created_at;
    let right_created = right.created_at;

    if left_created != right_created {
        return right_created.cmp(&left_created);
    }

    left.id.cmp(&right.id)
}

fn is_closed_like_status(status: &str) -> bool {
    matches!(status, "closed" | "tombstone")
}

fn truncate_runes(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let chars = input.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return input.to_string();
    }

    if max_chars <= 3 {
        return chars[..max_chars].iter().collect();
    }

    let mut output = chars[..max_chars - 3].iter().collect::<String>();
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timeout_supports_string_and_numeric_values() {
        assert_eq!(parse_timeout_text("5s"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_timeout_text("250ms"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(parse_timeout_text("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_timeout_text(""), None);
    }

    #[test]
    fn shell_like_expansion_expands_context_values() {
        let env = BTreeMap::from([
            ("BV_EXPORT_PATH".to_string(), "/tmp/out.md".to_string()),
            ("BV_ISSUE_COUNT".to_string(), "12".to_string()),
        ]);

        let expanded = expand_env_like_shell(
            "path=${BV_EXPORT_PATH} count=$BV_ISSUE_COUNT missing=$MISSING",
            &env,
        );

        assert_eq!(expanded, "path=/tmp/out.md count=12 missing=");
    }

    #[test]
    fn resolve_project_dir_returns_absolute_for_relative_input() {
        let result = resolve_project_dir(Some(Path::new("relative/path"))).unwrap();
        assert!(
            result.is_absolute(),
            "expected absolute path, got: {result:?}"
        );
        assert!(
            result.ends_with("relative/path"),
            "should preserve the relative suffix: {result:?}"
        );
    }

    #[test]
    fn resolve_project_dir_preserves_absolute_input() {
        let result = resolve_project_dir(Some(Path::new("/absolute/path"))).unwrap();
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn resolve_project_dir_falls_back_to_cwd_when_none() {
        let result = resolve_project_dir(None).unwrap();
        assert!(result.is_absolute());
    }

    #[test]
    fn hook_context_resolves_relative_export_path_to_absolute() {
        let ctx = HookContext::new(Path::new("output/report.md"), "markdown", 5);
        let path = Path::new(&ctx.export_path);
        assert!(
            path.is_absolute(),
            "BV_EXPORT_PATH should be absolute, got: {path:?}"
        );
        assert!(
            path.ends_with("output/report.md"),
            "should preserve the relative suffix: {path:?}"
        );
    }

    #[test]
    fn hook_context_preserves_absolute_export_path() {
        let ctx = HookContext::new(Path::new("/tmp/report.md"), "markdown", 5);
        assert_eq!(ctx.export_path, "/tmp/report.md");
    }

    #[test]
    fn parse_on_error_defaults_by_phase() {
        assert_eq!(parse_on_error("", HookPhase::PreExport), HookOnError::Fail);
        assert_eq!(
            parse_on_error("", HookPhase::PostExport),
            HookOnError::Continue
        );
        assert_eq!(
            parse_on_error("unknown", HookPhase::PreExport),
            HookOnError::Fail
        );
    }

    #[test]
    fn parse_on_error_recognizes_explicit_values() {
        assert_eq!(
            parse_on_error("fail", HookPhase::PostExport),
            HookOnError::Fail
        );
        assert_eq!(
            parse_on_error("continue", HookPhase::PreExport),
            HookOnError::Continue
        );
        assert_eq!(
            parse_on_error("  FAIL  ", HookPhase::PostExport),
            HookOnError::Fail
        );
    }

    #[test]
    fn parse_hook_timeout_handles_all_variants() {
        assert_eq!(
            parse_hook_timeout(Some(&HookTimeoutValue::Unsigned(10))),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            parse_hook_timeout(Some(&HookTimeoutValue::Signed(5))),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_hook_timeout(Some(&HookTimeoutValue::Signed(-1))),
            None
        );
        assert!(parse_hook_timeout(Some(&HookTimeoutValue::Float(2.5))).is_some());
        assert_eq!(
            parse_hook_timeout(Some(&HookTimeoutValue::Float(-1.0))),
            None
        );
        assert_eq!(
            parse_hook_timeout(Some(&HookTimeoutValue::Text("5s".to_string()))),
            Some(Duration::from_secs(5))
        );
        assert_eq!(parse_hook_timeout(None), None);
    }

    #[test]
    fn parse_timeout_text_supports_all_units() {
        assert_eq!(
            parse_timeout_text("100ms"),
            Some(Duration::from_millis(100))
        );
        assert_eq!(parse_timeout_text("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_timeout_text("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_timeout_text("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_timeout_text("0s"), None);
        assert_eq!(parse_timeout_text("abc"), None);
    }

    #[test]
    fn normalize_hooks_skips_empty_commands() {
        let specs = vec![
            HookSpec {
                name: "empty".to_string(),
                command: "   ".to_string(),
                ..HookSpec::default()
            },
            HookSpec {
                name: "valid".to_string(),
                command: "echo ok".to_string(),
                ..HookSpec::default()
            },
        ];

        let normalized = normalize_hooks(&specs, HookPhase::PreExport);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].name, "valid");
    }

    #[test]
    fn normalize_hooks_auto_generates_names() {
        let specs = vec![
            HookSpec {
                command: "echo a".to_string(),
                ..HookSpec::default()
            },
            HookSpec {
                command: "echo b".to_string(),
                ..HookSpec::default()
            },
        ];

        let normalized = normalize_hooks(&specs, HookPhase::PostExport);
        assert_eq!(normalized[0].name, "post-export-1");
        assert_eq!(normalized[1].name, "post-export-2");
    }

    #[test]
    fn is_closed_like_status_matches_closed_and_tombstone() {
        assert!(is_closed_like_status("closed"));
        assert!(is_closed_like_status("tombstone"));
        assert!(!is_closed_like_status("open"));
        assert!(!is_closed_like_status("in_progress"));
        assert!(!is_closed_like_status("blocked"));
    }

    #[test]
    fn truncate_runes_handles_edge_cases() {
        assert_eq!(truncate_runes("hello", 10), "hello");
        assert_eq!(truncate_runes("hello world", 8), "hello...");
        assert_eq!(truncate_runes("abc", 3), "abc");
        assert_eq!(truncate_runes("abcdef", 3), "abc");
        assert_eq!(truncate_runes("anything", 0), "");
    }

    #[test]
    fn format_hook_summary_reports_empty_hooks() {
        assert_eq!(format_hook_summary(&[]), "No hooks executed");
    }

    #[test]
    fn format_hook_summary_reports_success_and_failure() {
        let results = vec![
            HookRunResult {
                name: "lint".to_string(),
                success: true,
                duration: Duration::from_millis(100),
                error: None,
                stderr: String::new(),
            },
            HookRunResult {
                name: "deploy".to_string(),
                success: false,
                duration: Duration::from_millis(50),
                error: Some("exit status 1".to_string()),
                stderr: "connection refused".to_string(),
            },
        ];

        let summary = format_hook_summary(&results);
        assert!(summary.contains("1 succeeded, 1 failed"));
        assert!(summary.contains("[OK] lint"));
        assert!(summary.contains("[FAIL] deploy: exit status 1"));
        assert!(summary.contains("stderr: connection refused"));
    }

    #[test]
    fn generate_markdown_report_produces_expected_structure() {
        let issues = vec![
            Issue {
                id: "BD-1".to_string(),
                title: "Open task".to_string(),
                status: "open".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                description: "Do something".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "BD-2".to_string(),
                title: "Closed task".to_string(),
                status: "closed".to_string(),
                priority: 2,
                issue_type: "bug".to_string(),
                ..Issue::default()
            },
        ];

        let report = generate_markdown_report(&issues);
        assert!(report.contains("# Beads Export"));
        assert!(report.contains("| Total | 2 |"));
        assert!(report.contains("| Open | 1 |"));
        assert!(report.contains("| Closed | 1 |"));
        assert!(report.contains("## BD-1 Open task"));
        assert!(report.contains("## BD-2 Closed task"));
        assert!(report.contains("### Description"));
        assert!(report.contains("Do something"));
    }

    #[test]
    fn generate_markdown_report_sorts_open_before_closed() {
        let issues = vec![
            Issue {
                id: "CLOSED-1".to_string(),
                title: "Done".to_string(),
                status: "closed".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "OPEN-1".to_string(),
                title: "Active".to_string(),
                status: "open".to_string(),
                priority: 2,
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];

        let report = generate_markdown_report(&issues);
        let open_pos = report.find("OPEN-1").expect("should contain OPEN-1");
        let closed_pos = report.find("CLOSED-1").expect("should contain CLOSED-1");
        assert!(
            open_pos < closed_pos,
            "open issues should appear before closed"
        );
    }

    #[test]
    fn generate_markdown_report_handles_empty_issues() {
        let report = generate_markdown_report(&[]);
        assert!(report.contains("# Beads Export"));
        assert!(report.contains("| Total | 0 |"));
    }

    #[test]
    fn load_hooks_returns_default_for_missing_file() {
        let result = load_hooks(Path::new("/nonexistent/hooks.yaml")).unwrap();
        assert!(result.pre_export.is_empty());
        assert!(result.post_export.is_empty());
    }

    #[test]
    fn load_hooks_parses_valid_yaml() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hooks_path = temp.path().join("hooks.yaml");
        std::fs::write(
            &hooks_path,
            r#"
hooks:
  pre-export:
    - name: lint
      command: echo lint
      on_error: fail
  post-export:
    - command: echo done
"#,
        )
        .expect("write hooks file");

        let result = load_hooks(&hooks_path).unwrap();
        assert_eq!(result.pre_export.len(), 1);
        assert_eq!(result.pre_export[0].name, "lint");
        assert_eq!(result.post_export.len(), 1);
    }

    #[test]
    fn expand_env_handles_trailing_dollar_sign() {
        let env = BTreeMap::new();
        assert_eq!(expand_env_like_shell("price$", &env), "price$");
        assert_eq!(expand_env_like_shell("$", &env), "$");
    }

    #[test]
    fn expand_env_handles_dollar_followed_by_non_var_char() {
        let env = BTreeMap::new();
        assert_eq!(expand_env_like_shell("$1 $2", &env), "$1 $2");
    }

    #[test]
    fn hook_phase_labels_match_yaml_keys() {
        assert_eq!(HookPhase::PreExport.label(), "pre-export");
        assert_eq!(HookPhase::PostExport.label(), "post-export");
    }

    #[test]
    fn compare_issues_sorts_by_priority_within_same_status() {
        let high = Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            priority: 1,
            issue_type: "task".to_string(),
            ..Issue::default()
        };
        let low = Issue {
            id: "B".to_string(),
            title: "B".to_string(),
            status: "open".to_string(),
            priority: 3,
            issue_type: "task".to_string(),
            ..Issue::default()
        };

        assert_eq!(compare_issues_for_markdown(&high, &low), Ordering::Less);
        assert_eq!(compare_issues_for_markdown(&low, &high), Ordering::Greater);
    }
}
