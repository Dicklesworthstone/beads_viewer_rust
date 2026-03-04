mod test_utils;

use assert_cmd::Command;
use serde_json::Value;
use std::path::PathBuf;
use test_utils::{JsonType, assert_valid_version_envelope, validate_type_at};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_bvr_json(flags: &[&str], beads_file: &str) -> Value {
    let root = repo_root();
    let beads_path = root.join(beads_file);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.args(flags);
    command.arg("--beads-file").arg(&beads_path);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

// ============================================================================
// Schema validation tests for robot output contracts
// ============================================================================

#[test]
fn robot_triage_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    // Triage uses older envelope format (no version)
    test_utils::assert_valid_envelope(&output);
    assert!(validate_type_at(&output, "triage", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "triage.quick_ref", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "triage.recommendations", JsonType::Array).is_empty());
}

#[test]
fn robot_plan_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    assert!(validate_type_at(&output, "plan", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "status", JsonType::Object).is_empty());
}

#[test]
fn robot_insights_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-insights"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    assert!(validate_type_at(&output, "insights", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "insights.status", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "insights.bottlenecks", JsonType::Array).is_empty());
}

#[test]
fn robot_alerts_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-alerts"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    assert!(validate_type_at(&output, "alerts", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "summary", JsonType::Object).is_empty());
}

#[test]
fn robot_suggest_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-suggest"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    assert!(validate_type_at(&output, "suggestions", JsonType::Object).is_empty());
}

#[test]
fn robot_capacity_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-capacity"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "agents", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "open_issue_count", JsonType::Number).is_empty());
}

#[test]
fn robot_label_health_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-label-health"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "result", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "result.total_labels", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "result.labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "result.summaries", JsonType::Array).is_empty());
}

#[test]
fn robot_label_flow_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-label-flow"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "flow", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "flow.labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "flow.flow_matrix", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "flow.dependencies", JsonType::Array).is_empty());
}

#[test]
fn robot_label_attention_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-label-attention"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "result", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "result.labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "result.total_labels", JsonType::Number).is_empty());
}

#[test]
fn robot_label_attention_respects_limit() {
    let output = run_bvr_json(
        &["--robot-label-attention", "--attention-limit", "1"],
        "tests/testdata/synthetic_complex.jsonl",
    );
    assert_valid_version_envelope(&output);
    let labels = output["result"]["labels"].as_array().expect("labels array");
    assert!(labels.len() <= 1);
}

#[test]
fn robot_correlation_stats_has_valid_envelope() {
    let output = run_bvr_json(
        &["--robot-correlation-stats"],
        "tests/testdata/minimal.jsonl",
    );
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "total_feedback", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "confirmed", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "rejected", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "accuracy_rate", JsonType::Number).is_empty());
}

#[test]
fn robot_file_hotspots_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-file-hotspots"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "hotspots", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "stats", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "stats.total_files", JsonType::Number).is_empty());
}

#[test]
fn robot_orphans_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-orphans"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "stats", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "candidates", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "stats.total_commits", JsonType::Number).is_empty());
}

// ============================================================================
// Comparator self-tests (verifying the test_utils module itself)
// ============================================================================

#[test]
fn comparator_detects_field_drift() {
    let expected = serde_json::json!({
        "generated_at": "2026-03-04T07:00:00Z",
        "data_hash": "abc123",
        "output_format": "json",
        "version": "v0.1.0",
        "total": 5,
        "items": [{"id": "A"}, {"id": "B"}]
    });
    let actual = serde_json::json!({
        "generated_at": "2026-03-04T08:00:00Z",
        "data_hash": "abc123",
        "output_format": "json",
        "version": "v0.1.0",
        "total": 5,
        "items": [{"id": "A"}, {"id": "B"}]
    });

    // Without ignoring: should find 1 diff (generated_at)
    let diffs = test_utils::compare_json(&expected, &actual, "", None);
    assert_eq!(diffs.len(), 1);

    // Ignoring generated_at: should be clean
    let diffs = test_utils::compare_json_ignoring(&expected, &actual, "", &["generated_at"]);
    assert!(diffs.is_empty());
}

#[test]
fn comparator_order_invariant_arrays() {
    let expected = serde_json::json!([
        {"id": "C", "score": 3},
        {"id": "A", "score": 1},
        {"id": "B", "score": 2}
    ]);
    let actual = serde_json::json!([
        {"id": "A", "score": 1},
        {"id": "B", "score": 2},
        {"id": "C", "score": 3}
    ]);

    // Strict: should differ (order matters)
    let strict = test_utils::compare_json(&expected, &actual, "", None);
    assert!(!strict.is_empty());

    // Sorted by id: should match
    let sorted = test_utils::compare_json(&expected, &actual, "", Some("id"));
    assert!(sorted.is_empty());
}

#[test]
fn robot_triage_deterministic() {
    let first = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Triage output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

#[test]
fn robot_label_health_deterministic() {
    let first = run_bvr_json(&["--robot-label-health"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-label-health"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(
        &first,
        &second,
        "",
        &["generated_at", "most_recent_update", "oldest_open_issue"],
    );
    assert!(
        diffs.is_empty(),
        "Label health output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}
