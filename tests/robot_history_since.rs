use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use serde_json::Value;

fn run_bvr_json_in_dir(flags: &[&str], dir: &Path) -> Value {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(dir);
    command.args(flags);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn run_bvr_json_in_dir_with_env(flags: &[&str], dir: &Path, envs: &[(&str, &str)]) -> Value {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(dir);
    command.args(flags);
    for (key, value) in envs {
        command.env(key, value);
    }

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn run_git(repo_dir: &Path, args: &[&str], date: Option<&str>) {
    let mut command = ProcessCommand::new("git");
    command.current_dir(repo_dir);
    command.args(args);
    command.env("GIT_AUTHOR_NAME", "Test");
    command.env("GIT_AUTHOR_EMAIL", "test@example.com");
    command.env("GIT_COMMITTER_NAME", "Test");
    command.env("GIT_COMMITTER_EMAIL", "test@example.com");
    if let Some(date) = date {
        command.env("GIT_AUTHOR_DATE", date);
        command.env("GIT_COMMITTER_DATE", date);
    }
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn robot_history_since_filters_git_commit_correlation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("pkg")).expect("mkdir pkg");

    run_git(repo_dir, &["init"], None);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(repo_dir, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "seed HIST-1"],
        Some("2024-01-01T00:00:00Z"),
    );

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    fs::write(repo_dir.join("pkg/work.rs"), "// in progress\n").expect("write work");
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", "pkg/work.rs"],
        None,
    );
    run_git(
        repo_dir,
        &["commit", "-m", "claim HIST-1"],
        Some("2024-01-05T00:00:00Z"),
    );

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    fs::write(repo_dir.join("pkg/work.rs"), "// closed\n").expect("write work");
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", "pkg/work.rs"],
        None,
    );
    run_git(
        repo_dir,
        &["commit", "-m", "close HIST-1"],
        Some("2024-01-10T00:00:00Z"),
    );

    let all = run_bvr_json_in_dir(&["--robot-history", "--history-limit", "20"], repo_dir);
    let all_commits = all["histories"]["HIST-1"]["commits"]
        .as_array()
        .expect("all commits");
    assert!(all_commits.len() >= 3);

    let filtered = run_bvr_json_in_dir(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--history-since",
            "2024-01-06T00:00:00Z",
        ],
        repo_dir,
    );
    let filtered_commits = filtered["histories"]["HIST-1"]["commits"]
        .as_array()
        .expect("filtered commits");

    assert!(filtered_commits.len() < all_commits.len());
    assert_eq!(
        filtered["git_range"],
        "since 2024-01-06T00:00:00Z, last 20 commits"
    );
}

#[test]
fn robot_history_since_accepts_freeform_git_since_expressions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    run_git(repo_dir, &["init"], None);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(repo_dir, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "seed"],
        Some("2024-01-01T00:00:00Z"),
    );

    let output = run_bvr_json_in_dir(
        &[
            "--robot-history",
            "--history-since",
            "not-a-valid-history-since",
        ],
        repo_dir,
    );
    assert_eq!(
        output["git_range"],
        "since not-a-valid-history-since, last 500 commits"
    );
}

#[test]
fn robot_history_respects_min_confidence_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("pkg")).expect("mkdir pkg");

    run_git(repo_dir, &["init"], None);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(repo_dir, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "seed HIST-1"],
        Some("2024-01-01T00:00:00Z"),
    );

    fs::write(repo_dir.join("pkg/work.rs"), "// code only\n").expect("write code");
    run_git(repo_dir, &["add", "pkg/work.rs"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "refactor HIST-1 code-only"],
        Some("2024-01-02T00:00:00Z"),
    );

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    fs::write(repo_dir.join("pkg/work.rs"), "// co-committed\n").expect("write code");
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", "pkg/work.rs"],
        None,
    );
    run_git(
        repo_dir,
        &["commit", "-m", "claim HIST-1 with code"],
        Some("2024-01-03T00:00:00Z"),
    );

    let all = run_bvr_json_in_dir(&["--robot-history", "--history-limit", "20"], repo_dir);
    let all_commits = all["histories"]["HIST-1"]["commits"]
        .as_array()
        .expect("all commits");
    assert!(all_commits.len() >= 3);

    let filtered = run_bvr_json_in_dir(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--min-confidence",
            "0.9",
        ],
        repo_dir,
    );
    let filtered_commits = filtered["histories"]["HIST-1"]["commits"]
        .as_array()
        .expect("filtered commits");

    assert!(filtered_commits.len() < all_commits.len());
    assert_eq!(filtered_commits.len(), 1);
    assert!(
        filtered_commits
            .iter()
            .all(|commit| commit["confidence"].as_f64().unwrap_or_default() >= 0.9)
    );
    assert_eq!(
        filtered["stats"]["method_distribution"]["co_committed"],
        serde_json::json!(1)
    );
    assert!(filtered["stats"]["method_distribution"]["explicit_id"].is_null());
}

#[test]
fn robot_history_from_nested_dir_keeps_git_correlation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("nested/work")).expect("mkdir nested");

    run_git(repo_dir, &["init"], None);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-2\",\"title\":\"Nested history bead\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(repo_dir, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "seed HIST-2"],
        Some("2024-02-01T00:00:00Z"),
    );

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-2\",\"title\":\"Nested history bead\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads update");
    fs::write(repo_dir.join("nested/work/task.rs"), "// touch\n").expect("write nested file");
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", "nested/work/task.rs"],
        None,
    );
    run_git(
        repo_dir,
        &["commit", "-m", "claim HIST-2 from nested"],
        Some("2024-02-02T00:00:00Z"),
    );

    let nested_dir = repo_dir.join("nested/work");
    let beads_dir = repo_dir.join(".beads");
    let beads_dir_str = beads_dir.to_string_lossy().to_string();

    let output = run_bvr_json_in_dir_with_env(
        &["--robot-history", "--history-limit", "20"],
        &nested_dir,
        &[("BEADS_DIR", beads_dir_str.as_str())],
    );

    let commits = output["histories"]["HIST-2"]["commits"]
        .as_array()
        .expect("hist commits");
    assert!(!commits.is_empty());
    assert!(
        commits
            .iter()
            .any(|commit| commit["message"] == "claim HIST-2 from nested")
    );
}

#[test]
fn robot_history_from_nested_dir_uses_beads_diff_when_message_has_no_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("nested/work")).expect("mkdir nested");

    run_git(repo_dir, &["init"], None);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-3\",\"title\":\"Diff-derived history bead\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(repo_dir, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_dir,
        &["commit", "-m", "initial snapshot"],
        Some("2024-03-01T00:00:00Z"),
    );

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-3\",\"title\":\"Diff-derived history bead\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("update beads");
    fs::write(repo_dir.join("nested/work/task.rs"), "// code change\n").expect("write code");
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", "nested/work/task.rs"],
        None,
    );
    run_git(
        repo_dir,
        &["commit", "-m", "sync snapshots"],
        Some("2024-03-02T00:00:00Z"),
    );

    let nested_dir = repo_dir.join("nested/work");
    let beads_dir = repo_dir.join(".beads");
    let beads_dir_str = beads_dir.to_string_lossy().to_string();

    let output = run_bvr_json_in_dir_with_env(
        &["--robot-history", "--history-limit", "20"],
        &nested_dir,
        &[("BEADS_DIR", beads_dir_str.as_str())],
    );

    let commits = output["histories"]["HIST-3"]["commits"]
        .as_array()
        .expect("hist commits");
    assert!(
        commits.len() >= 2,
        "expected diff-derived commit correlation from beads.jsonl changes"
    );
    assert!(
        commits
            .iter()
            .any(|commit| commit["message"] == "sync snapshots")
    );
}

#[test]
fn robot_triage_from_nested_dir_discovers_parent_beads_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("nested/work")).expect("mkdir nested");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"B\",\"title\":\"B\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"A\",\"type\":\"blocks\"}]}\n"
        ),
    )
    .expect("write beads");

    let nested_dir = repo_dir.join("nested/work");
    let output = run_bvr_json_in_dir(&["--robot-triage"], &nested_dir);

    assert_eq!(output["triage"]["quick_ref"]["total_open"], 2);
    assert_eq!(output["triage"]["quick_ref"]["total_actionable"], 1);
    assert_eq!(output["triage"]["quick_ref"]["top_picks"][0]["id"], "A");
}
