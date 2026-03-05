use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn write_test_beads(repo_dir: &Path) {
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");
}

fn bvr_cmd(repo_dir: &Path) -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(repo_dir);
    command
}

fn run_robot_triage(repo_dir: &Path, extra_args: &[&str]) -> Value {
    let mut command = bvr_cmd(repo_dir);
    command.arg("--robot-triage");
    command.args(extra_args);
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice::<Value>(&output).expect("robot-triage json output")
}

#[test]
fn background_mode_flags_are_accepted_for_robot_commands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    let baseline = run_robot_triage(repo_dir, &[]);
    let with_background = run_robot_triage(repo_dir, &["--background-mode"]);
    let with_no_background = run_robot_triage(repo_dir, &["--no-background-mode"]);

    assert_eq!(baseline["data_hash"], with_background["data_hash"]);
    assert_eq!(baseline["data_hash"], with_no_background["data_hash"]);
}

#[test]
fn background_mode_flags_are_mutually_exclusive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    bvr_cmd(repo_dir)
        .args(["--background-mode", "--no-background-mode", "--robot-next"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("mutually exclusive"));
}
