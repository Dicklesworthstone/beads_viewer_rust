//! CLI and model integration tests for bd-1wv.
//!
//! Tests exercise CLI-level behavior through the binary:
//! - Exit codes for drift checking (--robot-drift without baseline)
//! - Flag parsing for --related-min-relevance / --related-max-results
//! - Status semantics via --robot-triage output filtering
//! - `content_hash` / `external_ref` model serialization
//! - Edge cases: empty input, Unicode titles, conflicting flags

mod test_utils;

use assert_cmd::Command;
use bvr::analysis::drift::{Baseline, BaselineGraphStats, BaselineTopMetrics};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bvr() -> Command {
    let bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    Command::new(bin)
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let output = ProcessCommand::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_bvr_json(flags: &[&str], beads_file: &str) -> Value {
    let root = repo_root();
    let beads_path = root.join(beads_file);
    let mut cmd = bvr();
    cmd.args(flags);
    cmd.arg("--beads-file").arg(&beads_path);
    let output = cmd.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn run_bvr_json_with_path(flags: &[&str], beads_path: &std::path::Path) -> Value {
    let mut cmd = bvr();
    cmd.args(flags);
    cmd.arg("--beads-file").arg(beads_path);
    let output = cmd.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

// ============================================================================
// Drift exit codes (--robot-drift)
// ============================================================================

#[test]
fn drift_without_baseline_exits_nonzero() {
    let root = repo_root();
    let source_beads_path = root.join("tests/testdata/minimal.jsonl");
    let tmp = tempfile::tempdir().expect("temp dir");
    let beads_path = tmp.path().join("minimal.jsonl");
    fs::copy(&source_beads_path, &beads_path).expect("copy fixture into isolated project dir");

    // Use an isolated project dir so no saved baseline can be discovered.
    bvr()
        .args([
            "--robot-drift",
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("baseline"));
}

#[test]
fn drift_with_saved_baseline_exits_zero() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");
    let tmp = tempfile::tempdir().expect("temp dir");

    // First, save a baseline
    bvr()
        .args([
            "--save-baseline",
            "",
            "--beads-file",
            beads_path.to_str().unwrap(),
            "--repo-path",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Then check drift (same data → exit 0)
    let output = bvr()
        .args([
            "--robot-drift",
            "--beads-file",
            beads_path.to_str().unwrap(),
            "--repo-path",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert!(json.get("generated_at").is_some(), "envelope present");
    // DriftResult is flattened, so has_drift/alerts are at top level
    assert!(
        json.get("has_drift").is_some(),
        "drift has_drift field present"
    );
    assert!(json.get("alerts").is_some(), "drift alerts field present");
}

#[test]
fn baseline_info_reads_saved_baseline_without_issue_data() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let baseline = Baseline {
        version: 1,
        created_at: "2026-03-12T00:00:00Z".to_string(),
        description: "saved snapshot".to_string(),
        stats: BaselineGraphStats {
            node_count: 3,
            edge_count: 2,
            density: 0.3333,
            open_count: 2,
            closed_count: 1,
            blocked_count: 1,
            cycle_count: 0,
            actionable_count: 1,
        },
        top_metrics: BaselineTopMetrics {
            pagerank: Vec::new(),
            betweenness: Vec::new(),
            hubs: Vec::new(),
            authorities: Vec::new(),
        },
        cycles: Vec::new(),
    };

    let baseline_dir = tmp.path().join(".bv");
    fs::create_dir_all(&baseline_dir).expect("create .bv");
    fs::write(
        baseline_dir.join("baseline.json"),
        serde_json::to_vec_pretty(&baseline).expect("serialize baseline"),
    )
    .expect("write baseline");

    bvr()
        .args([
            "--baseline-info",
            "--repo-path",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Baseline info:"))
        .stdout(predicates::str::contains("saved snapshot"))
        .stdout(predicates::str::contains("Nodes: 3"));
}

#[test]
fn baseline_commands_use_workspace_root_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");

    bvr()
        .current_dir(&caller_dir)
        .args([
            "--save-baseline",
            "workspace snapshot",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("Baseline saved to"));

    assert!(
        workspace_root.join(".bv/baseline.json").exists(),
        "baseline should be saved at workspace root"
    );
    assert!(
        !repo_dir.join(".bv/baseline.json").exists(),
        "nested repo should not receive its own baseline file"
    );

    bvr()
        .current_dir(&caller_dir)
        .args([
            "--baseline-info",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("workspace snapshot"))
        .stdout(predicates::str::contains("Nodes: 1"));
}

#[test]
fn feedback_show_reads_saved_feedback_from_repo_path_when_invoked_elsewhere() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let repo_dir = tmp.path().join("repo");
    let caller_dir = tmp.path().join("caller");
    fs::create_dir_all(repo_dir.join(".bv")).expect("create repo .bv");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        repo_dir.join(".bv/feedback.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": "1.0",
            "events": [
                {
                    "issue_id": "BD-1",
                    "action": "accept",
                    "score": 0.9,
                    "timestamp": "2026-03-12T00:00:00Z",
                    "by": "cli",
                    "reason": ""
                }
            ],
            "adjustments": []
        }))
        .expect("serialize feedback"),
    )
    .expect("write feedback");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--feedback-show",
            "--format",
            "json",
            "--repo-path",
            repo_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(json["stats"]["total_accepted"], 1);
    assert_eq!(json["stats"]["total_ignored"], 0);
}

#[test]
fn feedback_show_reads_saved_feedback_from_beads_file_project_when_invoked_elsewhere() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let repo_dir = tmp.path().join("repo");
    let caller_dir = tmp.path().join("caller");
    let beads_dir = repo_dir.join(".beads");
    fs::create_dir_all(repo_dir.join(".bv")).expect("create repo .bv");
    fs::create_dir_all(&beads_dir).expect("create beads dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        beads_dir.join("beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");
    fs::write(
        repo_dir.join(".bv/feedback.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": "1.0",
            "events": [
                {
                    "issue_id": "BD-1",
                    "action": "ignore",
                    "score": 0.2,
                    "timestamp": "2026-03-12T00:00:00Z",
                    "by": "cli",
                    "reason": ""
                }
            ],
            "adjustments": []
        }))
        .expect("serialize feedback"),
    )
    .expect("write feedback");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--feedback-show",
            "--format",
            "json",
            "--beads-file",
            beads_dir.join("beads.jsonl").to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(json["stats"]["total_accepted"], 0);
    assert_eq!(json["stats"]["total_ignored"], 1);
}

#[test]
fn feedback_show_reads_saved_feedback_from_workspace_root_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");
    fs::write(
        workspace_root.join(".bv/feedback.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": "1.0",
            "events": [
                {
                    "issue_id": "BD-1",
                    "action": "accept",
                    "score": 0.9,
                    "timestamp": "2026-03-12T00:00:00Z",
                    "by": "cli",
                    "reason": ""
                }
            ],
            "adjustments": []
        }))
        .expect("serialize feedback"),
    )
    .expect("write feedback");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--feedback-show",
            "--format",
            "json",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(json["stats"]["total_accepted"], 1);
    assert_eq!(json["stats"]["total_ignored"], 0);
}

#[test]
fn correlation_stats_reads_feedback_from_workspace_root_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(workspace_root.join(".beads")).expect("create workspace .beads");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");
    fs::write(
        workspace_root.join(".beads/correlation_feedback.jsonl"),
        "{\"commit_sha\":\"abc123\",\"bead_id\":\"BD-1\",\"feedback_at\":\"2026-03-12T00:00:00Z\",\"feedback_by\":\"cli\",\"type\":\"confirm\",\"reason\":\"\",\"original_conf\":0.9}\n",
    )
    .expect("write correlation feedback");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--robot-correlation-stats",
            "--format",
            "json",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(json["total_feedback"], 1);
    assert_eq!(json["confirmed"], 1);
    assert_eq!(json["rejected"], 0);
}

#[test]
fn sprint_list_reads_sprints_from_workspace_root_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(workspace_root.join(".beads")).expect("create workspace .beads");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");
    fs::write(
        workspace_root.join(".beads/sprints.jsonl"),
        "{\"id\":\"sprint-1\",\"name\":\"Workspace Sprint\",\"bead_ids\":[\"BD-1\"]}\n",
    )
    .expect("write sprints");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--robot-sprint-list",
            "--format",
            "json",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(json["sprint_count"], 1);
    assert_eq!(json["sprints"][0]["id"], "sprint-1");
    assert_eq!(json["sprints"][0]["name"], "Workspace Sprint");
}

#[test]
fn robot_orphans_uses_workspace_root_history_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(workspace_root.join("apps/web/src")).expect("create orphan file dir");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship export\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads file");

    run_git(&workspace_root, &["init"]);
    run_git(&workspace_root, &["add", "."]);
    run_git(
        &workspace_root,
        &["commit", "-m", "initial workspace snapshot"],
    );

    fs::write(
        workspace_root.join("apps/web/src/orphan.js"),
        "export const orphan = true;\n",
    )
    .expect("write orphan file");
    run_git(&workspace_root, &["add", "."]);
    run_git(&workspace_root, &["commit", "-m", "add orphan web file"]);

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--robot-orphans",
            "--orphans-min-score",
            "0",
            "--format",
            "json",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert!(
        json["stats"]["total_commits"].as_u64().unwrap_or(0) >= 1,
        "workspace-root git history should be visible to robot-orphans"
    );
    assert!(
        json["candidates"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["files"]
                    .as_array()
                    .is_some_and(|files| files.iter().any(|file| file == "apps/web/src/orphan.js"))
            })),
        "robot-orphans should include the workspace-root orphan file when --repo-path discovers a workspace"
    );
}

// ============================================================================
// Related work flags (--related-min-relevance, --related-max-results)
// ============================================================================

#[test]
fn related_min_relevance_flag_parses() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/synthetic_complex.jsonl");

    // Use a very high min-relevance to get zero results
    let output = bvr()
        .args([
            "--robot-related",
            "bd-complex-1",
            "--related-min-relevance",
            "100",
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    // RelatedWorkResult is flattened: source_bead and related are at top level
    assert!(
        json.get("source_bead").is_some(),
        "source_bead field present"
    );
    assert!(json.get("related").is_some(), "related field present");
}

#[test]
fn related_max_results_limits_output() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/synthetic_complex.jsonl");

    let output = bvr()
        .args([
            "--robot-related",
            "bd-complex-1",
            "--related-max-results",
            "1",
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    // RelatedWorkResult is flattened: related is at top level
    if let Some(related) = json.get("related") {
        if let Some(arr) = related.as_array() {
            assert!(
                arr.len() <= 1,
                "max-results=1 but got {} results",
                arr.len()
            );
        }
    }
}

#[test]
fn related_invalid_bead_id_still_succeeds() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    // Nonexistent bead returns empty result, not failure
    let output = bvr()
        .args([
            "--robot-related",
            "bd-nonexistent-999",
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    // Flattened: source_bead is at top level
    assert!(json.get("source_bead").is_some());
    let related = json["related"].as_array().expect("related array");
    assert!(related.is_empty(), "nonexistent bead should have 0 related");
}

// ============================================================================
// Status semantics via CLI output (is_closed / is_tombstone / is_closed_like)
// ============================================================================

#[test]
fn triage_excludes_closed_and_tombstone_from_recommendations() {
    // all_closed.jsonl has only closed/tombstone issues
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/all_closed.jsonl");
    let recs = output["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert!(
        recs.is_empty(),
        "triage should not recommend closed/tombstone issues, got {} recs",
        recs.len()
    );
}

#[test]
fn triage_includes_open_statuses() {
    // minimal.jsonl has open issues
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    let recs = output["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert!(
        !recs.is_empty(),
        "triage should recommend open issues from minimal fixture"
    );
}

#[test]
fn robot_next_excludes_in_progress_from_top_pick() {
    let temp = tempfile::tempdir().expect("tempdir");
    let beads_path = temp.path().join("in_progress_actionable.jsonl");
    fs::write(
        &beads_path,
        concat!(
            "{\"id\":\"A\",\"title\":\"Claimed blocker\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"B\",\"title\":\"Blocked leaf\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",",
            "\"dependencies\":[{\"depends_on_id\":\"A\",\"type\":\"blocks\"}]}\n",
        ),
    )
    .expect("write fixture");

    let output = run_bvr_json_with_path(&["--robot-next"], &beads_path);
    // In-progress issues are excluded from top_picks to match legacy behavior
    // (top_picks surfaces new work, not already-claimed work). When the only
    // actionable issue is in_progress, robot-next returns a no-pick message.
    assert!(
        output["id"].is_null(),
        "robot-next should not pick an in_progress issue: {output}"
    );
}

// ============================================================================
// content_hash and external_ref model fields
// ============================================================================

#[test]
fn content_hash_not_serialized_in_robot_output() {
    // content_hash has skip_serializing — it should never appear in JSON output
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    let json_str = serde_json::to_string(&output).unwrap();
    assert!(
        !json_str.contains("content_hash"),
        "content_hash should never appear in JSON output"
    );
}

#[test]
fn external_ref_absent_when_not_set() {
    // Standard fixtures don't set external_ref, so it should be absent (skip_serializing_if)
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    let json_str = serde_json::to_string(&output).unwrap();
    // If no issue has external_ref set, it shouldn't appear
    // (skip_serializing_if = "Option::is_none")
    assert!(
        !json_str.contains("external_ref"),
        "external_ref should be absent when not set in fixture data"
    );
}

// ============================================================================
// Edge cases: empty input
// ============================================================================

#[test]
fn empty_beads_file_exits_cleanly() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/empty.jsonl");

    // Should not panic, may succeed with empty results or exit gracefully
    let result =
        std::process::Command::new(std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr"))
            .args(["--robot-triage", "--beads-file"])
            .arg(&beads_path)
            .output()
            .expect("run bvr");

    // Either succeeds with empty triage or exits with informative error
    if result.status.success() {
        let json: Value =
            serde_json::from_slice(&result.stdout).expect("valid JSON from empty input");
        let recs = json["triage"]["recommendations"]
            .as_array()
            .expect("recommendations");
        assert!(
            recs.is_empty(),
            "empty input should produce 0 recommendations"
        );
    }
    // Non-zero exit is acceptable for empty input, just shouldn't panic
}

#[test]
fn single_issue_triage_produces_one_recommendation() {
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/single_issue.jsonl");
    let recs = output["triage"]["recommendations"]
        .as_array()
        .expect("recommendations");
    assert_eq!(
        recs.len(),
        1,
        "single open issue should yield 1 recommendation"
    );
}

#[test]
fn as_of_uses_historical_jsonl_filename_when_current_filename_changed() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let repo_dir = tmp.path().join("repo");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");

    run_git(&repo_dir, &["init"]);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"OLD-1\",\"title\":\"Old issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write historical beads.jsonl");
    run_git(&repo_dir, &["add", "."]);
    run_git(&repo_dir, &["commit", "-m", "initial beads filename"]);

    fs::remove_file(repo_dir.join(".beads/beads.jsonl")).expect("remove old beads filename");
    fs::write(
        repo_dir.join(".beads/issues.jsonl"),
        "{\"id\":\"NEW-1\",\"title\":\"New issue\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\"}\n",
    )
    .expect("write current issues.jsonl");
    run_git(&repo_dir, &["add", "."]);
    run_git(&repo_dir, &["commit", "-m", "rename beads file"]);

    let output = bvr()
        .current_dir(&repo_dir)
        .args(["--robot-triage", "--as-of", "HEAD~1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    let recommendations = json["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert_eq!(
        recommendations.len(),
        1,
        "historical triage should load one issue"
    );
    assert_eq!(recommendations[0]["id"], "OLD-1");
}

// ============================================================================
// Flag parsing edge cases
// ============================================================================

#[test]
fn unknown_flag_exits_with_error() {
    bvr()
        .args(["--robot-triage", "--nonexistent-flag-xyz"])
        .assert()
        .failure();
}

#[test]
fn robot_search_without_query_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-search", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("search"));
}

#[test]
fn robot_search_with_whitespace_only_query_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-search", "--search", "   ", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("--search <query>"));
}

#[test]
fn robot_full_stats_without_insights_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-full-stats", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--robot-full-stats and --insight-limit require --robot-insights",
        ));
}

#[test]
fn insight_limit_without_robot_insights_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--insight-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--robot-full-stats and --insight-limit require --robot-insights",
        ));
}

#[test]
fn graph_modifier_without_graph_command_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--graph-root", "A", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "graph modifiers require --robot-graph or --export-graph <path>",
        ));
}

#[test]
fn graph_root_next_keeps_blockers_outside_the_subtree() {
    // C is a child of epic E but is blocked by B, which is outside E's tree.
    let tmp = tempfile::tempdir().expect("temp dir");
    let beads_path = tmp.path().join("issues.jsonl");
    fs::write(
        &beads_path,
        concat!(
            r#"{"id":"E","title":"Epic","status":"open","priority":1,"issue_type":"epic"}"#,
            "\n",
            r#"{"id":"B","title":"Outside blocker","status":"open","priority":2,"issue_type":"task"}"#,
            "\n",
            r#"{"id":"C","title":"Child","status":"open","priority":0,"issue_type":"task","dependencies":[{"depends_on_id":"E","type":"parent-child"},{"depends_on_id":"B","type":"blocks"}]}"#,
            "\n",
        ),
    )
    .expect("write beads file");

    let output = bvr()
        .args(["--robot-next", "--graph-root", "E", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let next: Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert_ne!(next["id"], "C", "blocked C offered as next work: {next}");
    assert!(next["id"].is_null(), "unexpected pick: {next}");
}

#[test]
fn search_modifier_without_robot_search_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--search-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "search modifiers require --robot-search",
        ));
}

#[test]
fn history_modifier_without_history_command_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--history-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "history modifiers require --robot-history or --bead-history <id>",
        ));
}

#[test]
fn alert_filter_without_robot_alerts_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--severity", "high", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "alert filters require --robot-alerts",
        ));
}

#[test]
fn suggest_filter_without_robot_suggest_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--suggest-type", "duplicates", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "suggest filters require --robot-suggest",
        ));
}

#[test]
fn relation_modifier_without_file_relations_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--relations-threshold", "0.7", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "relation modifiers require --robot-file-relations <path>",
        ));
}

#[test]
fn related_modifier_without_robot_related_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--related-max-results", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "related-work modifiers require --robot-related <id>",
        ));
}

#[test]
fn network_depth_without_impact_network_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--network-depth", "3", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--network-depth requires --robot-impact-network <id>",
        ));
}

#[test]
fn forecast_modifier_without_robot_forecast_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--forecast-label", "backend", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "forecast modifiers require --robot-forecast <id|all>",
        ));
}

#[test]
fn capacity_modifier_without_robot_capacity_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--capacity-label", "backend", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "capacity modifiers require --robot-capacity",
        ));
}

#[test]
fn attention_limit_without_robot_label_attention_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--attention-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--attention-limit requires --robot-label-attention",
        ));
}

#[test]
fn orphans_modifier_without_robot_orphans_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--orphans-min-score", "0", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--orphans-min-score requires --robot-orphans",
        ));
}

#[test]
fn file_beads_limit_without_robot_file_beads_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--file-beads-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--file-beads-limit requires --robot-file-beads <path>",
        ));
}

#[test]
fn hotspots_limit_without_robot_file_hotspots_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--hotspots-limit", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--hotspots-limit requires --robot-file-hotspots",
        ));
}

#[test]
fn search_query_without_robot_search_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--search", "alpha", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--search <query> requires --robot-search",
        ));
}

#[test]
fn related_include_closed_without_robot_related_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--related-include-closed", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--related-include-closed requires --robot-related <id>",
        ));
}

#[test]
fn correlation_by_without_correlation_action_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--correlation-by", "cli", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "correlation action modifiers require --robot-confirm-correlation or --robot-reject-correlation",
        ));
}

#[test]
fn recipe_without_emit_script_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--recipe", "actionable", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "script modifiers require --emit-script",
        ));
}

#[test]
fn profile_json_without_profile_startup_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--profile-json", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--profile-json requires --profile-startup",
        ));
}

#[test]
fn debug_width_without_debug_render_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--debug-width", "120", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "debug render dimensions require --debug-render <view>",
        ));
}

#[test]
fn pages_title_without_export_pages_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--pages-title", "Dashboard", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "pages export modifiers require --export-pages <dir>",
        ));
}

#[test]
fn no_live_reload_without_preview_or_pages_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--no-live-reload", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--no-live-reload requires --preview-pages <dir> or --pages",
        ));
}

#[test]
fn robot_min_confidence_without_robot_priority_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-min-confidence", "0.7", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "priority filters require --robot-priority",
        ));
}

#[test]
fn robot_by_label_without_robot_priority_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-by-label", "backend", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "priority filters require --robot-priority",
        ));
}

#[test]
fn robot_by_assignee_without_robot_priority_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-by-assignee", "alice", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "priority filters require --robot-priority",
        ));
}

#[test]
fn robot_max_results_without_recommendation_command_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-max-results", "5", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--robot-max-results requires a recommendation command",
        ));
}

#[test]
fn weight_preset_without_recommendation_scoring_command_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--weight-preset", "graph-heavy", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "--weight-preset requires a recommendation-scoring command",
        ));
}

#[test]
fn robot_schema_command_accepts_bare_command_name() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "search"],
        "tests/testdata/minimal.jsonl",
    );
    assert_eq!(output["command"], "robot-search");
    assert!(
        output["schema"].is_object(),
        "schema payload must be present"
    );
}

#[test]
fn robot_schema_command_accepts_flag_style_command_name() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    let output = bvr()
        .args([
            "--robot-schema",
            "--schema-command=--robot-search",
            "--beads-file",
        ])
        .arg(&beads_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert_eq!(json["command"], "robot-search");
    assert!(json["schema"].is_object(), "schema payload must be present");
}

#[test]
fn robot_schema_without_beads_file_succeeds() {
    let output = bvr()
        .args(["--robot-schema"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert!(
        json["commands"].is_object(),
        "schema commands map must be present"
    );
    assert_eq!(json["schema_version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn robot_schema_unknown_command_exits_with_listed_choices() {
    bvr()
        .args(["--robot-schema", "--schema-command", "definitely-not-real"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Unknown command: definitely-not-real",
        ))
        .stderr(predicates::str::contains("robot-search"))
        .stderr(predicates::str::contains("robot-triage"));
}

#[test]
fn robot_docs_without_beads_file_succeeds() {
    let output = bvr()
        .args(["--robot-docs", "guide"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert_eq!(json["topic"], "guide");
    assert!(json["guide"].is_object(), "guide payload must be present");
}

#[test]
fn robot_docs_invalid_topic_returns_error_payload_and_exit_code_two() {
    let output = bvr()
        .args(["--robot-docs", "definitely-not-real"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON output");
    assert_eq!(json["topic"], "definitely-not-real");
    assert!(
        json["error"]
            .as_str()
            .is_some_and(|error| error.contains("Unknown topic")),
        "invalid topic should explain the failure"
    );
    assert!(
        json["available_topics"]
            .as_array()
            .is_some_and(|topics| !topics.is_empty()),
        "invalid topic response should list the supported topics"
    );
}

#[test]
fn robot_help_with_beads_file_succeeds() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-help", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .success()
        .stdout(predicates::str::contains("--robot-triage"))
        .stdout(predicates::str::contains("--robot-schema"))
        .stdout(predicates::str::contains("--robot-docs"));
}

#[test]
fn robot_diff_without_diff_since_exits_with_error() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    bvr()
        .args(["--robot-diff", "--beads-file"])
        .arg(&beads_path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("diff-since"));
}

#[test]
fn robot_forecast_requires_value() {
    // --robot-forecast with no value should fail
    bvr().arg("--robot-forecast").assert().failure();
}

#[test]
fn robot_burndown_requires_value() {
    // --robot-burndown with no value should fail
    bvr().arg("--robot-burndown").assert().failure();
}

// ============================================================================
// Diff with self (zero-delta)
// ============================================================================

#[test]
fn diff_self_produces_zero_changes() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");

    let output = run_bvr_json(
        &["--robot-diff", "--diff-since", beads_path.to_str().unwrap()],
        "tests/testdata/minimal.jsonl",
    );

    assert!(output.get("diff").is_some(), "diff field must exist");
    let diff = &output["diff"];
    // Self-diff should show zero added/removed/modified
    if let Some(added) = diff.get("added") {
        if let Some(arr) = added.as_array() {
            assert!(arr.is_empty(), "self-diff should have 0 added issues");
        }
    }
    if let Some(removed) = diff.get("removed") {
        if let Some(arr) = removed.as_array() {
            assert!(arr.is_empty(), "self-diff should have 0 removed issues");
        }
    }
}

// ============================================================================
// Forecast and capacity with fixtures
// ============================================================================

#[test]
fn forecast_all_produces_valid_output() {
    let output = run_bvr_json(&["--robot-forecast", "all"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_version_envelope(&output);
    assert!(output.get("forecasts").is_some());
    assert!(output.get("forecast_count").is_some());
}

#[test]
fn capacity_with_multiple_agents() {
    let output = run_bvr_json(
        &["--robot-capacity", "--agents", "3"],
        "tests/testdata/minimal.jsonl",
    );
    test_utils::assert_valid_version_envelope(&output);
    let agents = output["agents"].as_u64().expect("agents field");
    assert_eq!(agents, 3, "capacity should reflect --agents=3");
}

// ============================================================================
// History output shape
// ============================================================================

#[test]
fn history_output_has_correct_structure() {
    let output = run_bvr_json(&["--robot-history"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);

    // histories is a map (Object), not array
    assert!(
        output["histories"].is_object(),
        "histories should be object"
    );
    assert!(output["stats"].is_object(), "stats should be object");
    assert!(
        output["git_range"].is_string(),
        "git_range should be string"
    );
    assert!(
        output["commit_index"].is_object(),
        "commit_index should be object"
    );
}

// ============================================================================
// Graph output shape
// ============================================================================

#[test]
fn graph_output_has_correct_structure() {
    let output = run_bvr_json(&["--robot-graph"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);

    assert!(output["nodes"].is_number(), "nodes should be a count");
    assert!(output["edges"].is_number(), "edges should be a count");
    assert!(output["format"].is_string(), "format should be string");
    assert!(
        output["explanation"].is_object(),
        "explanation should be object"
    );
}

// ============================================================================
// Metrics output shape
// ============================================================================

#[test]
fn metrics_output_has_correct_structure() {
    let output = run_bvr_json(&["--robot-metrics"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_version_envelope(&output);

    assert!(output["timing"].is_array(), "timing should be array");
    assert!(output["cache"].is_array(), "cache should be array");
    assert!(output["memory"].is_object(), "memory should be object");
}

// ============================================================================
// Unicode in fixture data
// ============================================================================

#[test]
fn triage_handles_unicode_titles() {
    // boundary_conditions.jsonl may have edge-case data
    let output = run_bvr_json(
        &["--robot-triage"],
        "tests/testdata/boundary_conditions.jsonl",
    );
    test_utils::assert_valid_envelope(&output);
    // Should not panic or produce invalid JSON
    let json_str = serde_json::to_string(&output).unwrap();
    assert!(!json_str.is_empty());
}

// ============================================================================
// Multiple robot flags should use first-wins
// ============================================================================

#[test]
fn robot_triage_and_plan_both_succeed_independently() {
    // Verify both produce valid output when run separately
    let triage = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    assert!(triage.get("triage").is_some());

    let plan = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    assert!(plan.get("plan").is_some());
}

#[test]
fn robot_triage_suppresses_loader_warnings_without_bv_robot_env() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let beads_path = tmp.path().join("malformed.jsonl");
    fs::write(
        &beads_path,
        "not json\n{\"id\":\"A\",\"title\":\"Valid\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write malformed fixture");

    let output = bvr()
        .args([
            "--robot-triage",
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .output()
        .expect("run bvr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "robot triage should succeed on mixed malformed input\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Warning:"),
        "robot mode should suppress loader warnings without requiring BV_ROBOT env\nstderr: {stderr}"
    );

    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(
        json.get("triage").is_some(),
        "triage payload should be present"
    );
}

#[test]
fn diff_since_auto_robot_diff_suppresses_loader_warnings() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let beads_path = tmp.path().join("malformed.jsonl");
    fs::write(
        &beads_path,
        "not json\n{\"id\":\"A\",\"title\":\"Valid\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write malformed fixture");

    let output = bvr()
        .args([
            "--diff-since",
            beads_path.to_str().unwrap(),
            "--beads-file",
            beads_path.to_str().unwrap(),
        ])
        .output()
        .expect("run bvr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "auto robot diff should succeed on mixed malformed input\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Warning:"),
        "auto-enabled diff mode should suppress loader warnings\nstderr: {stderr}"
    );

    let json: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(json.get("diff").is_some(), "diff payload should be present");
}

#[test]
fn robot_diff_git_ref_uses_workspace_history_when_repo_path_discovers_workspace() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let web_dir = workspace_root.join("apps/web");
    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(web_dir.join(".beads")).expect("create web .beads");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n    prefix: api-\n  - path: apps/web\n    prefix: web-\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"AUTH-1\",\"title\":\"API issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write api beads");
    fs::write(
        web_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"WEB-1\",\"title\":\"Web issue one\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write web beads");

    run_git(&workspace_root, &["init"]);
    run_git(&workspace_root, &["add", "."]);
    run_git(
        &workspace_root,
        &["commit", "-m", "initial workspace snapshot"],
    );

    fs::write(
        web_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"WEB-1\",\"title\":\"Web issue one\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"WEB-2\",\"title\":\"Web issue two\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n"
        ),
    )
    .expect("update web beads");
    run_git(&workspace_root, &["add", "."]);
    run_git(&workspace_root, &["commit", "-m", "add second web issue"]);

    let output = bvr()
        .args([
            "--robot-diff",
            "--diff-since",
            "HEAD~1",
            "--repo-path",
            repo_dir.to_str().unwrap(),
        ])
        .current_dir(&workspace_root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    let added = json["diff"]["new_issues"]
        .as_array()
        .expect("new_issues array");
    assert_eq!(
        added.len(),
        1,
        "workspace diff should only add the new web issue"
    );
    assert_eq!(added[0]["title"], "Web issue two");
}

#[test]
fn robot_diff_snapshot_prefers_workspace_relative_file_over_caller_cwd_shadow() {
    let tmp = tempfile::tempdir_in(repo_root()).expect("temp dir");
    let workspace_root = tmp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = tmp.path().join("caller");
    let caller_snapshots = caller_dir.join("snapshots");
    let workspace_snapshots = workspace_root.join("snapshots");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(repo_dir.join(".beads")).expect("create repo .beads");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_snapshots).expect("create caller snapshots");
    fs::create_dir_all(&workspace_snapshots).expect("create workspace snapshots");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"BD-1\",\"title\":\"Ship alpha\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"BD-2\",\"title\":\"Ship beta\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n"
        ),
    )
    .expect("write current beads file");
    fs::write(
        workspace_snapshots.join("before.jsonl"),
        "{\"id\":\"BD-1\",\"title\":\"Ship alpha\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write workspace snapshot");
    fs::write(
        caller_snapshots.join("before.jsonl"),
        concat!(
            "{\"id\":\"BD-1\",\"title\":\"Ship alpha\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"BD-2\",\"title\":\"Ship beta\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n"
        ),
    )
    .expect("write caller shadow snapshot");

    let output = bvr()
        .current_dir(&caller_dir)
        .args([
            "--robot-diff",
            "--diff-since",
            "snapshots/before.jsonl",
            "--format",
            "json",
            "--repo-path",
            nested_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid JSON");
    let added = json["diff"]["new_issues"]
        .as_array()
        .expect("new_issues array");
    assert_eq!(
        added.len(),
        1,
        "workspace-relative snapshot should beat caller cwd shadow"
    );
    // In workspace mode, issues are namespace-prefixed with the repo name
    assert_eq!(added[0]["id"], "api-BD-2");
    assert_eq!(added[0]["title"], "Ship beta");
}

// ============================================================================
// Large fixture stress
// ============================================================================

#[test]
fn large_fixture_triage_does_not_panic() {
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/large_graph_40.jsonl");
    test_utils::assert_valid_envelope(&output);
    let recs = output["triage"]["recommendations"]
        .as_array()
        .expect("recommendations");
    assert!(
        !recs.is_empty(),
        "40-node graph should produce recommendations"
    );
}

// ============================================================================
// --export-template (legacy Go text/template reports)
// ============================================================================

#[test]
fn export_template_renders_custom_markdown_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("report.tmpl");
    fs::write(
        &template,
        "# {{.Title}}\n{{range .Issues}}- {{.ID}} P{{.Priority}}{{if eq .Status \"closed\"}} done{{end}}\n{{end}}{{if .Graph}}graph{{else}}no graph{{end}} {{len .Issues}}\n",
    )
    .expect("write template");
    let out = dir.path().join("report.md");
    bvr()
        .arg("--beads-file")
        .arg(repo_root().join("tests/testdata/minimal.jsonl"))
        .arg("--export")
        .arg(&out)
        .arg("--export-template")
        .arg(&template)
        .arg("--export-include-graph=false")
        .arg("--no-hooks")
        .assert()
        .success();
    let report = fs::read_to_string(&out).expect("report written");
    assert!(report.starts_with("# Beads Export\n- "), "{report}");
    assert!(report.ends_with("- B P2\nno graph 2\n"), "{report}");
}

#[test]
fn export_template_rejects_bad_combinations_and_templates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("bad.tmpl");
    fs::write(&template, "{{.Missing}}").expect("write template");
    let fixture = repo_root().join("tests/testdata/minimal.jsonl");
    for (args, needle) in [
        (
            vec!["--robot-triage", "--export-template"],
            "requires --export or --export-md",
        ),
        (
            vec![
                "--export",
                "out.csv",
                "--export-format",
                "csv",
                "--export-include-graph=false",
                "--export-template",
            ],
            "custom templates require markdown export",
        ),
        (
            vec!["--export", "out.md", "--export-template"],
            "map has no entry for key \"Missing\"",
        ),
    ] {
        let output = bvr()
            .current_dir(dir.path())
            .arg("--beads-file")
            .arg(&fixture)
            .args(&args)
            .arg(&template)
            .output()
            .expect("run bvr");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
}
