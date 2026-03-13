use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;

fn write_minimal_beads(repo_dir: &Path) {
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"description\":\"export path\"}\n",
    )
    .expect("write beads.jsonl");
}

fn write_hooks(repo_dir: &Path, yaml: &str) {
    fs::create_dir_all(repo_dir.join(".bv")).expect("create .bv");
    fs::write(repo_dir.join(".bv/hooks.yaml"), yaml).expect("write hooks.yaml");
}

fn bvr_cmd(repo_dir: &Path) -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(repo_dir);
    command
}

#[test]
fn export_md_writes_markdown_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .assert()
        .success();

    let report = fs::read_to_string(repo_dir.join("report.md")).expect("read report.md");
    assert!(report.contains("# Beads Export"));
    assert!(report.contains("BD-1 Ship export"));
}

#[test]
fn export_md_pre_hook_failure_blocks_export() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    write_hooks(
        repo_dir,
        "hooks:\n  pre-export:\n    - name: fail-fast\n      command: 'exit 7'\n",
    );

    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .assert()
        .failure()
        .stderr(predicate::str::contains("pre-export hook"));

    assert!(
        !repo_dir.join("report.md").exists(),
        "report should not exist when pre-export hook fails"
    );
}

#[test]
fn export_md_post_hook_failure_warns_but_succeeds() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    write_hooks(
        repo_dir,
        "hooks:\n  post-export:\n    - name: fail-late\n      command: 'exit 9'\n      on_error: fail\n",
    );

    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .assert()
        .success()
        .stderr(predicate::str::contains("post-export hook failed"));

    assert!(repo_dir.join("report.md").exists());
}

#[test]
fn export_md_no_hooks_skips_hook_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    write_hooks(
        repo_dir,
        "hooks:\n  pre-export:\n    - name: marker\n      command: 'echo ran > hook-ran.txt'\n",
    );

    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .arg("--no-hooks")
        .assert()
        .success();

    assert!(repo_dir.join("report.md").exists());
    assert!(!repo_dir.join("hook-ran.txt").exists());
}

#[test]
fn export_md_hooks_receive_context_env_vars() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    write_hooks(
        repo_dir,
        r#"hooks:
  pre-export:
    - name: env-check
      command: 'printf "%s\n" "$CUSTOM_LINE" > env-line.txt'
      env:
        CUSTOM_LINE: '${BV_EXPORT_PATH}|$BV_ISSUE_COUNT'
"#,
    );

    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .assert()
        .success();

    let line = fs::read_to_string(repo_dir.join("env-line.txt")).expect("read env-line.txt");
    // BV_EXPORT_PATH is now always absolute (resolved against cwd).
    let expected = format!("{}|1", repo_dir.join("report.md").display());
    assert_eq!(line.trim(), expected);
}

#[test]
fn export_md_hook_timeout_marks_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_minimal_beads(repo_dir);

    write_hooks(
        repo_dir,
        "hooks:\n  pre-export:\n    - name: slow\n      command: 'sleep 2'\n      timeout: 10ms\n",
    );

    let started = Instant::now();
    bvr_cmd(repo_dir)
        .arg("--export-md")
        .arg("report.md")
        .assert()
        .failure()
        .stderr(predicate::str::contains("timeout"));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "timeout hook should fail quickly; took {elapsed:?}"
    );
}

#[test]
fn export_md_hooks_run_from_repo_path_when_invoked_elsewhere() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path().join("repo");
    let caller_dir = temp.path().join("caller");
    let export_path = repo_dir.join("report.md");
    fs::create_dir_all(&repo_dir).expect("create repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    write_minimal_beads(&repo_dir);

    write_hooks(
        &repo_dir,
        "hooks:\n  pre-export:\n    - name: relative-cwd\n      command: 'printf repo > hook-ran.txt'\n",
    );

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command
        .current_dir(&caller_dir)
        .arg("--repo-path")
        .arg(&repo_dir)
        .arg("--export-md")
        .arg(&export_path);

    command.assert().success();

    assert!(export_path.exists());
    assert!(
        repo_dir.join("hook-ran.txt").exists(),
        "hook should run in repo dir when --repo-path is used"
    );
    assert!(
        !caller_dir.join("hook-ran.txt").exists(),
        "hook should not run in caller current dir"
    );
}
