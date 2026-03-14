//! CLI and model integration tests for bd-1wv.
//!
//! Tests exercise CLI-level behavior through the binary:
//! - Exit codes for drift checking (--robot-drift without baseline)
//! - Flag parsing for --related-min-relevance / --related-max-results
//! - Status semantics via --robot-triage output filtering
//! - content_hash / external_ref model serialization
//! - Edge cases: empty input, Unicode titles, conflicting flags

mod test_utils;

use assert_cmd::Command;
use bvr::analysis::drift::{Baseline, BaselineGraphStats, BaselineTopMetrics};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bvr() -> Command {
    let bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    Command::new(bin)
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

// ============================================================================
// Drift exit codes (--robot-drift)
// ============================================================================

#[test]
fn drift_without_baseline_exits_nonzero() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");
    let tmp = tempfile::tempdir().expect("temp dir");

    // Point --repo-path at an empty temp dir (no baseline)
    bvr()
        .args([
            "--robot-drift",
            "--beads-file",
            beads_path.to_str().unwrap(),
            "--repo-path",
            tmp.path().to_str().unwrap(),
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
