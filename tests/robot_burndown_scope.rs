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
fn robot_burndown_includes_scope_changes_from_git_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let beads_dir = repo_dir.join(".beads");
    fs::create_dir_all(&beads_dir).expect("mkdir beads");

    fs::write(
        beads_dir.join("beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"Alpha\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"B\",\"title\":\"Beta\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\"}\n"
        ),
    )
    .expect("write beads");

    let now = chrono::Utc::now();
    let start = (now - chrono::Duration::days(1)).to_rfc3339();
    let end = (now + chrono::Duration::days(1)).to_rfc3339();

    let sprint_v1 = format!(
        "{{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"start_date\":\"{start}\",\"end_date\":\"{end}\",\"bead_ids\":[\"A\"]}}\n"
    );
    fs::write(beads_dir.join("sprints.jsonl"), sprint_v1).expect("write sprint v1");

    run_git(repo_dir, &["init"], None);
    run_git(
        repo_dir,
        &["add", ".beads/beads.jsonl", ".beads/sprints.jsonl"],
        None,
    );
    run_git(repo_dir, &["commit", "-m", "init sprint"], None);

    let sprint_v2 = format!(
        "{{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"start_date\":\"{start}\",\"end_date\":\"{end}\",\"bead_ids\":[\"A\",\"B\"]}}\n"
    );
    fs::write(beads_dir.join("sprints.jsonl"), sprint_v2).expect("write sprint v2");
    run_git(repo_dir, &["add", ".beads/sprints.jsonl"], None);
    run_git(repo_dir, &["commit", "-m", "add B to sprint"], None);

    let sprint_v3 = format!(
        "{{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"start_date\":\"{start}\",\"end_date\":\"{end}\",\"bead_ids\":[\"B\"]}}\n"
    );
    fs::write(beads_dir.join("sprints.jsonl"), sprint_v3).expect("write sprint v3");
    run_git(repo_dir, &["add", ".beads/sprints.jsonl"], None);
    run_git(repo_dir, &["commit", "-m", "remove A from sprint"], None);

    let payload = run_bvr_json_in_dir(&["--robot-burndown", "sprint-1"], repo_dir);
    assert_eq!(payload["sprint_id"], "sprint-1");
    assert!(
        payload["generated_at"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );

    let scope_changes = payload["scope_changes"].as_array().expect("scope_changes");
    assert_eq!(scope_changes.len(), 2, "scope_changes={scope_changes:?}");
    assert_eq!(scope_changes[0]["issue_id"], "B");
    assert_eq!(scope_changes[0]["action"], "added");
    assert_eq!(scope_changes[1]["issue_id"], "A");
    assert_eq!(scope_changes[1]["action"], "removed");
}
