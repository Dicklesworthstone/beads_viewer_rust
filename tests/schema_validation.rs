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
    assert!(validate_type_at(&output, "status", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "Bottlenecks", JsonType::Array).is_empty());
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
    assert!(validate_type_at(&output, "analysis_config", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "results", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "results.total_labels", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "results.labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "results.summaries", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

#[test]
fn robot_label_flow_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-label-flow"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "analysis_config", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "flow", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "flow.labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "flow.flow_matrix", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "flow.dependencies", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

#[test]
fn robot_label_attention_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-label-attention"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "labels", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "total_labels", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

#[test]
fn robot_label_attention_respects_limit() {
    let output = run_bvr_json(
        &["--robot-label-attention", "--attention-limit", "1"],
        "tests/testdata/synthetic_complex.jsonl",
    );
    assert_valid_version_envelope(&output);
    let labels = output["labels"].as_array().expect("labels array");
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

#[test]
fn robot_search_has_valid_envelope() {
    let output = run_bvr_json(
        &[
            "--robot-search",
            "--search",
            "parity",
            "--search-limit",
            "5",
        ],
        "tests/testdata/synthetic_complex.jsonl",
    );
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "query", JsonType::String).is_empty());
    assert!(validate_type_at(&output, "limit", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "mode", JsonType::String).is_empty());
    assert!(validate_type_at(&output, "results", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

// ============================================================================
// Recently-added field validation (bd-1ru.* parity work)
// ============================================================================

#[test]
fn robot_triage_has_usage_hints() {
    let output = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

#[test]
fn robot_plan_has_analysis_config() {
    let output = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    assert!(validate_type_at(&output, "analysis_config", JsonType::Object).is_empty());
}

#[test]
fn robot_plan_has_usage_hints() {
    let output = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    assert!(validate_type_at(&output, "usage_hints", JsonType::Array).is_empty());
}

#[test]
fn robot_insights_has_advanced_and_stats_fields() {
    let output = run_bvr_json(
        &["--robot-insights", "--robot-full-stats"],
        "tests/testdata/synthetic_complex.jsonl",
    );
    test_utils::assert_valid_envelope(&output);
    // full_stats is a map of issue-id -> metric node
    assert!(validate_type_at(&output, "full_stats", JsonType::Object).is_empty());
}

#[test]
fn robot_next_has_envelope_and_score_fields() {
    let output = run_bvr_json(&["--robot-next"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    // RobotNextOutput always has these fields (nullable)
    assert!(output.get("id").is_some(), "next output must have id field");
    assert!(
        output.get("score").is_some(),
        "next output must have score field"
    );
    assert!(
        output.get("reasons").is_some(),
        "next output must have reasons field"
    );
}

#[test]
fn robot_forecast_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-forecast", "all"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    assert!(validate_type_at(&output, "forecasts", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "forecast_count", JsonType::Number).is_empty());
}

#[test]
fn robot_burndown_has_valid_envelope() {
    // --robot-burndown requires a sprint target; use "current" which gracefully
    // returns an error or empty data when no sprints are configured.
    // We test via the binary directly to check the envelope shape.
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let result = std::process::Command::new(&bvr_bin)
        .args(["--robot-burndown", "current", "--beads-file"])
        .arg(&beads_path)
        .output()
        .expect("failed to run bvr");
    // If it succeeds (sprint found), validate the envelope
    if result.status.success() {
        let output: Value = serde_json::from_slice(&result.stdout).expect("valid JSON output");
        assert_valid_version_envelope(&output);
        assert!(validate_type_at(&output, "daily_points", JsonType::Array).is_empty());
    } else {
        // No sprints configured is expected; just verify it doesn't panic
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("sprint") || stderr.contains("error"),
            "expected sprint-related error, got: {stderr}"
        );
    }
}

#[test]
fn robot_history_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-history"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    // histories is a BTreeMap<String, HistoryBeadCompat> → JSON Object
    assert!(validate_type_at(&output, "histories", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "git_range", JsonType::String).is_empty());
    assert!(validate_type_at(&output, "stats", JsonType::Object).is_empty());
}

#[test]
fn robot_diff_has_valid_envelope() {
    let root = repo_root();
    let beads_path = root.join("tests/testdata/minimal.jsonl");
    // --robot-diff requires --diff-since; use the same file to get an empty diff
    let output = run_bvr_json(
        &["--robot-diff", "--diff-since", beads_path.to_str().unwrap()],
        "tests/testdata/minimal.jsonl",
    );
    test_utils::assert_valid_envelope(&output);
    assert!(output.get("from_data_hash").is_some());
    assert!(output.get("to_data_hash").is_some());
    assert!(output.get("resolved_revision").is_some());
    assert!(validate_type_at(&output, "diff", JsonType::Object).is_empty());
}

#[test]
fn robot_graph_json_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-graph"], "tests/testdata/minimal.jsonl");
    test_utils::assert_valid_envelope(&output);
    // nodes and edges are counts (usize), not arrays
    assert!(validate_type_at(&output, "nodes", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "edges", JsonType::Number).is_empty());
    assert!(validate_type_at(&output, "format", JsonType::String).is_empty());
    assert!(validate_type_at(&output, "explanation", JsonType::Object).is_empty());
}

#[test]
fn robot_metrics_has_valid_envelope() {
    let output = run_bvr_json(&["--robot-metrics"], "tests/testdata/minimal.jsonl");
    assert_valid_version_envelope(&output);
    // timing, cache are Vec<> → JSON Array
    assert!(validate_type_at(&output, "timing", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "cache", JsonType::Array).is_empty());
    assert!(validate_type_at(&output, "memory", JsonType::Object).is_empty());
}

#[test]
fn robot_search_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-search"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_label_health_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-label-health"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_recipes_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-recipes"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_capacity_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-capacity"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_forecast_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-forecast"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_burndown_schema_includes_versioned_envelope_fields() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-burndown"],
        "tests/testdata/sprints_synthetic.jsonl",
    );
    assert!(
        validate_type_at(&output, "schema.properties.output_format", JsonType::Object).is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.version", JsonType::Object).is_empty());
}

#[test]
fn robot_correlation_stats_schema_matches_flattened_output_shape() {
    let output = run_bvr_json(
        &[
            "--robot-schema",
            "--schema-command",
            "robot-correlation-stats",
        ],
        "tests/testdata/minimal.jsonl",
    );
    assert!(
        validate_type_at(
            &output,
            "schema.properties.total_feedback",
            JsonType::Object
        )
        .is_empty()
    );
    assert!(validate_type_at(&output, "schema.properties.confirmed", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.rejected", JsonType::Object).is_empty());
    assert!(
        validate_type_at(&output, "schema.properties.accuracy_rate", JsonType::Object).is_empty()
    );
    assert!(output["schema"]["properties"].get("stats").is_none());
}

#[test]
fn robot_orphans_schema_matches_flattened_output_shape() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-orphans"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(validate_type_at(&output, "schema.properties.stats", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.candidates", JsonType::Object).is_empty());
    assert!(output["schema"]["properties"].get("report").is_none());
}

#[test]
fn robot_impact_network_schema_matches_flattened_output_shape() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-impact-network"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(validate_type_at(&output, "schema.properties.bead_id", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.depth", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.network", JsonType::Object).is_empty());
    assert!(
        validate_type_at(&output, "schema.properties.top_connected", JsonType::Object).is_empty()
    );
    assert!(output["schema"]["properties"].get("result").is_none());
}

#[test]
fn robot_drift_schema_matches_flattened_output_shape() {
    let output = run_bvr_json(
        &["--robot-schema", "--schema-command", "robot-drift"],
        "tests/testdata/minimal.jsonl",
    );
    assert!(validate_type_at(&output, "schema.properties.has_drift", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.exit_code", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.summary", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.alerts", JsonType::Object).is_empty());
    assert!(validate_type_at(&output, "schema.properties.baseline", JsonType::Object).is_empty());
    assert!(output["schema"]["properties"].get("result").is_none());
}

// ============================================================================
// Determinism tests for additional robot modes
// ============================================================================

#[test]
fn robot_plan_deterministic() {
    let first = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Plan output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

#[test]
fn robot_insights_deterministic() {
    let first = run_bvr_json(&["--robot-insights"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-insights"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Insights output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

#[test]
fn robot_next_deterministic() {
    let first = run_bvr_json(&["--robot-next"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-next"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Next output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

#[test]
fn robot_alerts_deterministic() {
    let first = run_bvr_json(&["--robot-alerts"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-alerts"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Alerts output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

#[test]
fn robot_capacity_deterministic() {
    let first = run_bvr_json(&["--robot-capacity"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-capacity"], "tests/testdata/minimal.jsonl");

    let diffs = test_utils::compare_json_ignoring(&first, &second, "", &["generated_at"]);
    assert!(
        diffs.is_empty(),
        "Capacity output not deterministic:\n{}",
        test_utils::format_diffs_compact(&diffs)
    );
}

// ============================================================================
// Complex fixture validates richer output shapes
// ============================================================================

#[test]
fn robot_triage_complex_has_recommendations() {
    let output = run_bvr_json(
        &["--robot-triage"],
        "tests/testdata/synthetic_complex.jsonl",
    );
    test_utils::assert_valid_envelope(&output);
    let recs = output["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert!(
        !recs.is_empty(),
        "complex fixture must produce triage recommendations"
    );
}

#[test]
fn robot_plan_complex_has_tracks() {
    let output = run_bvr_json(&["--robot-plan"], "tests/testdata/synthetic_complex.jsonl");
    test_utils::assert_valid_envelope(&output);
    let tracks = output["plan"]["tracks"].as_array().expect("tracks array");
    assert!(
        !tracks.is_empty(),
        "complex fixture must produce execution tracks"
    );
}

#[test]
fn robot_insights_complex_has_bottlenecks() {
    let output = run_bvr_json(
        &["--robot-insights"],
        "tests/testdata/synthetic_complex.jsonl",
    );
    test_utils::assert_valid_envelope(&output);
    let bottlenecks = output["Bottlenecks"].as_array().expect("bottlenecks array");
    assert!(
        !bottlenecks.is_empty(),
        "complex fixture must produce bottleneck insights"
    );
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

// These exercise the advertised schemas against actual command output, including
// variants which do not share a root shape. Deliberately malformed copies must
// fail too: a permissive/empty schema must not make this contract suite pass.
mod advertised_work_contract {
    use std::fmt::Write as _;
    use std::path::Path;

    use assert_cmd::Command;
    use serde_json::{Value, json};

    fn run(dir: &Path, flags: &[&str]) -> Value {
        let binary = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
        let output = Command::new(binary)
            .current_dir(dir)
            .env_remove("BEADS_DB")
            .env_remove("BEADS_DIR")
            .env_remove("BV_DATA_SOURCE")
            .env_remove("BV_ROBOT_NOT_READY_LABELS")
            .args(flags)
            .args(["--format", "json"])
            .arg("--beads-file")
            .arg(dir.join(".beads/beads.jsonl"))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).expect("robot JSON")
    }

    fn fixture(issues: &[Value]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temporary fixture");
        std::fs::create_dir(dir.path().join(".beads")).expect("beads directory");
        let mut jsonl = String::new();
        for issue in issues {
            writeln!(&mut jsonl, "{issue}").expect("serialize fixture issue");
        }
        std::fs::write(dir.path().join(".beads/beads.jsonl"), jsonl).expect("write fixture");
        dir
    }

    fn readiness_fixture() -> tempfile::TempDir {
        fixture(&[
            json!({"id":"READY","title":"Ready task","status":"open","priority":1,"issue_type":"task","labels":["ready"]}),
            json!({"id":"ACTIVE","title":"Claimed task","status":"in_progress","priority":1,"issue_type":"task","assignee":"worker","labels":["active"],"defer_until":"2000-01-01T00:00:00Z"}),
            json!({"id":"WAIT","title":"Waiting task","status":"open","priority":1,"issue_type":"task","labels":["waiting"],"dependencies":[{"issue_id":"WAIT","depends_on_id":"READY","type":"blocks"}]}),
            json!({"id":"DEFER","title":"Later task","status":"open","priority":2,"issue_type":"task","labels":["later"],"defer_until":"2299-01-01T00:00:00Z"}),
            json!({"id":"PARK","title":"Parked task","status":"blocked","priority":1,"issue_type":"task","labels":["parked"]}),
            json!({"id":"DONE","title":"Finished task","status":"closed","priority":2,"issue_type":"task"}),
        ])
    }

    fn schema(dir: &Path, command: &str) -> Value {
        run(dir, &["--robot-schema", "--schema-command", command])["schema"].clone()
    }

    // A deliberately limited evaluator for the Draft 2020-12 assertions used
    // by these three schemas. Preflight rejects unknown assertions, formats,
    // and external references instead of silently treating them as passing.
    // date-time is asserted here in addition to the structural constraints.
    fn check_schema(schema: &Value, root: &Value) {
        if schema.is_boolean() {
            return;
        }
        let object = schema
            .as_object()
            .expect("schema must be an object or boolean");
        for (key, value) in object {
            match key.as_str() {
                "$schema" => assert_eq!(value, "https://json-schema.org/draft/2020-12/schema"),
                "$ref" => {
                    let pointer = value.as_str().expect("reference string");
                    let pointer = pointer.strip_prefix('#').expect("local reference only");
                    let target = root.pointer(pointer).expect("reference must resolve");
                    assert!(
                        target.is_object() || target.is_boolean(),
                        "reference must point to a schema: {value}"
                    );
                }
                "$defs" | "properties" => {
                    for child in value.as_object().expect("schema map").values() {
                        check_schema(child, root);
                    }
                }
                "items" | "additionalProperties" | "not" => check_schema(value, root),
                "oneOf" | "anyOf" => {
                    let branches = value.as_array().expect("schema alternatives");
                    assert_ne!(branches.as_slice(), [] as [Value; 0]);
                    for branch in branches {
                        check_schema(branch, root);
                    }
                }
                "type" => assert!(matches!(
                    value.as_str(),
                    Some("object" | "array" | "string" | "integer" | "number" | "boolean" | "null")
                )),
                "required" => assert!(
                    value
                        .as_array()
                        .expect("required array")
                        .iter()
                        .all(Value::is_string)
                ),
                "enum" => assert_ne!(
                    value.as_array().expect("enum array").as_slice(),
                    [] as [Value; 0]
                ),
                "minimum" => assert!(value.is_number()),
                "format" => assert_eq!(value, "date-time"),
                "const" | "title" | "description" => {}
                other => panic!("unsupported schema assertion: {other}"),
            }
        }
    }

    fn validate(schema: &Value, root: &Value, value: &Value, path: &str) -> Result<(), String> {
        if let Some(accept) = schema.as_bool() {
            return if accept {
                Ok(())
            } else {
                Err(format!("{path}: false schema"))
            };
        }
        if let Some(reference) = schema["$ref"].as_str() {
            let target = root
                .pointer(reference.strip_prefix('#').expect("local reference"))
                .expect("reference checked before evaluation");
            validate(target, root, value, path)?;
        }
        if let Some(kind) = schema["type"].as_str() {
            let matches = match kind {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
                "number" => value.is_number(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => unreachable!("type checked before evaluation"),
            };
            if !matches {
                return Err(format!("{path}: expected {kind}, got {value}"));
            }
        }
        if let Some(expected) = schema.get("const") {
            if value != expected {
                return Err(format!("{path}: expected constant {expected}"));
            }
        }
        if let Some(choices) = schema["enum"].as_array() {
            if !choices.contains(value) {
                return Err(format!("{path}: value outside enum"));
            }
        }
        if let (Some(minimum), Some(number)) = (schema["minimum"].as_f64(), value.as_f64()) {
            if number < minimum {
                return Err(format!("{path}: value below {minimum}"));
            }
        }
        if schema["format"] == "date-time" {
            if let Some(date) = value.as_str() {
                chrono::DateTime::parse_from_rfc3339(date)
                    .map_err(|error| format!("{path}: invalid date-time: {error}"))?;
            }
        }
        if let Some(object) = value.as_object() {
            if let Some(required) = schema["required"].as_array() {
                for key in required {
                    let key = key.as_str().expect("required key string");
                    if !object.contains_key(key) {
                        return Err(format!("{path}: missing required field {key}"));
                    }
                }
            }
            for (key, field) in object {
                if let Some(property) = schema["properties"].get(key) {
                    validate(property, root, field, &format!("{path}/{key}"))?;
                } else if let Some(additional) = schema.get("additionalProperties") {
                    validate(additional, root, field, &format!("{path}/{key}"))?;
                }
            }
        }
        if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
            for (index, item) in values.iter().enumerate() {
                validate(items, root, item, &format!("{path}/{index}"))?;
            }
        }
        for keyword in ["oneOf", "anyOf"] {
            if let Some(branches) = schema[keyword].as_array() {
                let matches = branches
                    .iter()
                    .filter(|branch| validate(branch, root, value, path).is_ok())
                    .count();
                if matches == 0 || (keyword == "oneOf" && matches != 1) {
                    return Err(format!("{path}: {keyword} matched {matches} branches"));
                }
            }
        }
        if let Some(negated) = schema.get("not") {
            if validate(negated, root, value, path).is_ok() {
                return Err(format!("{path}: matched forbidden schema"));
            }
        }
        Ok(())
    }

    fn assert_accepts(schema: &Value, output: &Value) {
        check_schema(schema, schema);
        assert_eq!(validate(schema, schema, output, "$"), Ok(()));
    }

    fn assert_rejects(schema: &Value, output: &Value, reason: &str) {
        check_schema(schema, schema);
        assert!(
            validate(schema, schema, output, "$").is_err(),
            "schema accepted {reason}: {output}"
        );
    }

    fn assert_bad_field(schema: &Value, output: &Value, pointer: &str, bad: Value) {
        let mut changed = output.clone();
        *changed
            .pointer_mut(pointer)
            .expect("field exists in real output") = bad;
        assert_rejects(schema, &changed, pointer);
    }

    fn assert_required(schema: &Value, output: &Value, object: &str, field: &str) {
        let mut changed = output.clone();
        assert!(
            changed
                .pointer_mut(object)
                .expect("object exists")
                .as_object_mut()
                .expect("object")
                .remove(field)
                .is_some()
        );
        assert_rejects(schema, &changed, &format!("missing {object}/{field}"));
    }

    #[test]
    fn full_triage_matches_schema_and_preserves_readiness_distinctions() {
        let fixture = readiness_fixture();
        let output = run(
            fixture.path(),
            &[
                "--robot-triage",
                "--robot-triage-by-label",
                "--robot-triage-by-track",
            ],
        );
        let schema = schema(fixture.path(), "robot-triage");
        let triage = &output["triage"];
        assert_eq!(triage["quick_ref"]["open_count"], 3);
        assert_eq!(triage["quick_ref"]["in_progress_count"], 1);
        assert_eq!(triage["quick_ref"]["actionable_count"], 2);
        assert_eq!(triage["quick_ref"]["not_actionable_count"], 3);
        assert_eq!(triage["quick_ref"]["top_picks"][0]["id"], "READY");
        assert!(triage["quick_ref"]["top_picks"][0].get("action").is_none());
        assert_eq!(triage["project_health"]["counts"]["not_closed"], 5);
        assert_eq!(triage["project_health"]["counts"]["dependency_blocked"], 3);
        assert_eq!(triage["project_health"]["counts"]["blocked"], 1);
        let recommendations = triage["recommendations"]
            .as_array()
            .expect("recommendations");
        assert_eq!(recommendations.len(), 2);
        assert!(
            recommendations
                .iter()
                .all(|rec| rec["id"] != "DEFER" && rec["id"] != "WAIT" && rec["id"] != "PARK")
        );
        for recommendation in recommendations {
            assert_eq!(recommendation["claimable"], recommendation["id"] == "READY");
        }
        // The future deferral is excluded; the elapsed one is retained on an
        // actionable recommendation without making already claimed work free.
        let deferred_index = recommendations
            .iter()
            .position(|rec| rec["id"] == "ACTIVE")
            .expect("elapsed deferral recommendation");
        assert!(recommendations[deferred_index]["defer_until"].is_string());
        assert!(
            recommendations
                .iter()
                .any(|rec| rec.get("defer_until").is_none())
        );
        let groups = triage["recommendations_by_label"]
            .as_array()
            .expect("groups");
        assert!(groups.iter().any(|group| group["top_pick"].is_null()));
        assert!(groups.iter().any(|group| group["top_pick"].is_object()));
        assert_accepts(&schema, &output);
        assert_bad_field(
            &schema,
            &output,
            "/triage/recommendations/0/claimable",
            json!("true"),
        );
        assert_required(&schema, &output, "/triage/recommendations/0", "claimable");
        let deferred_path = format!("/triage/recommendations/{deferred_index}/defer_until");
        assert_bad_field(&schema, &output, &deferred_path, Value::Null);
        assert_bad_field(&schema, &output, &deferred_path, json!("not a timestamp"));
        assert_bad_field(
            &schema,
            &output,
            "/triage/project_health/counts/dependency_blocked",
            json!("3"),
        );
        assert_bad_field(
            &schema,
            &output,
            "/triage/project_health/counts/not_closed",
            json!(-1),
        );
        assert_required(
            &schema,
            &output,
            "/triage/project_health/counts",
            "actionable",
        );
        assert_bad_field(
            &schema,
            &output,
            "/triage/project_health/counts/by_status/open",
            json!(1.5),
        );
        assert_bad_field(
            &schema,
            &output,
            "/triage/recommendations_by_label/0/top_pick",
            json!(false),
        );
    }

    #[test]
    fn brief_triage_matches_its_own_shape_and_rejects_ambiguous_payloads() {
        let fixture = readiness_fixture();
        let output = run(fixture.path(), &["--robot-triage", "--brief"]);
        let schema = schema(fixture.path(), "robot-triage");
        assert_eq!(output["brief"], true);
        assert!(output.get("triage").is_none());
        let recommendations = output["recommendations"]
            .as_array()
            .expect("brief recommendations");
        let ready = recommendations
            .iter()
            .find(|rec| rec["id"] == "READY")
            .expect("ready recommendation");
        assert_eq!(ready["unblocks"], json!(["WAIT"]));
        assert!(ready.get("assignee").is_none());
        let active = recommendations
            .iter()
            .find(|rec| rec["id"] == "ACTIVE")
            .expect("active recommendation");
        assert_eq!(active["assignee"], "worker");
        assert_eq!(active["claimable"], false);
        assert!(active.get("unblocks").is_none());
        assert_accepts(&schema, &output);
        assert_required(&schema, &output, "", "brief");
        assert_required(&schema, &output, "/recommendations/0", "claimable");
        assert_bad_field(&schema, &output, "/brief", json!(false));
        assert_bad_field(
            &schema,
            &output,
            "/recommendations/0/claimable",
            Value::Null,
        );
        let ready_index = recommendations
            .iter()
            .position(|rec| rec["id"] == "READY")
            .expect("ready index");
        assert_bad_field(
            &schema,
            &output,
            &format!("/recommendations/{ready_index}/unblocks"),
            json!(1),
        );
        let full = run(fixture.path(), &["--robot-triage"]);
        let mut hybrid = output;
        hybrid["triage"] = full["triage"].clone();
        hybrid["usage_hints"] = full["usage_hints"].clone();
        assert_rejects(&schema, &hybrid, "hybrid brief/full payload");
    }

    #[test]
    fn empty_triage_variants_match_schema_with_optional_fields_omitted() {
        let fixture = fixture(&[]);
        let schema = schema(fixture.path(), "robot-triage");
        let full = run(fixture.path(), &["--robot-triage"]);
        assert_eq!(full["triage"]["recommendations"], json!([]));
        assert_eq!(full["triage"]["project_health"]["counts"]["not_closed"], 0);
        assert!(full["triage"]["commands"].get("claim_top").is_none());
        assert_accepts(&schema, &full);
        let brief = run(fixture.path(), &["--robot-triage", "--brief"]);
        assert_eq!(brief["recommendations"], json!([]));
        assert!(brief.get("quick_wins").is_none());
        assert!(brief.get("blockers_to_clear").is_none());
        assert_accepts(&schema, &brief);
        assert_required(&schema, &brief, "", "recommendations");
        assert_required(
            &schema,
            &full,
            "/triage/project_health/counts",
            "not_closed",
        );
    }

    #[test]
    fn grouped_triage_commands_publish_the_same_full_and_brief_contract() {
        let fixture = readiness_fixture();
        for command in ["robot-triage-by-track", "robot-triage-by-label"] {
            let flag = format!("--{command}");
            let schema = schema(fixture.path(), command);
            let full = run(fixture.path(), &[&flag]);
            let brief = run(fixture.path(), &[&flag, "--brief"]);
            assert_accepts(&schema, &full);
            assert_accepts(&schema, &brief);
            assert_bad_field(
                &schema,
                &full,
                "/triage/recommendations/0/claimable",
                json!("yes"),
            );
            assert_required(&schema, &brief, "", "brief");
        }
    }

    #[test]
    fn next_schema_accepts_pick_and_no_pick_without_nullable_identifiers() {
        let fixture = readiness_fixture();
        let schema = schema(fixture.path(), "robot-next");
        let pick = run(fixture.path(), &["--robot-next"]);
        assert_eq!(pick["id"], "READY");
        assert!(pick.get("message").is_none());
        assert_accepts(&schema, &pick);
        assert_required(&schema, &pick, "", "claim_command");
        let waiting = self::fixture(&[
            json!({"id":"ACTIVE","title":"Already claimed","status":"in_progress","issue_type":"task"}),
            json!({"id":"DEFER","title":"Not ready yet","status":"open","issue_type":"task","defer_until":"2299-01-01T00:00:00Z"}),
            json!({"id":"BLOCK","title":"Blocked","status":"blocked","issue_type":"task"}),
        ]);
        let empty = run(waiting.path(), &["--robot-next"]);
        assert_eq!(empty["message"], "No actionable items available");
        for key in [
            "id",
            "title",
            "score",
            "reasons",
            "unblocks",
            "claim_command",
            "show_command",
        ] {
            assert!(empty.get(key).is_none(), "no-pick omits {key}");
        }
        assert_accepts(&schema, &empty);
        assert_required(&schema, &empty, "", "message");
        let mut null_id = empty.clone();
        null_id["id"] = Value::Null;
        assert_rejects(&schema, &null_id, "null pick identifier");
        let mut mixed = pick;
        mixed["message"] = empty["message"].clone();
        assert_rejects(&schema, &mixed, "pick with no-pick message");
    }

    #[test]
    fn burndown_schema_validates_risk_signals_and_optional_timestamps() {
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::days(30)).to_rfc3339();
        let fixture = fixture(&[
            json!({"id":"OLD","title":"Stalled blocker","status":"blocked","priority":1,"issue_type":"task","created_at":old,"updated_at":old}),
            json!({"id":"WAIT","title":"Sprint dependency","status":"open","priority":1,"issue_type":"task","created_at":old,"updated_at":old,"dependencies":[{"issue_id":"WAIT","depends_on_id":"OLD","type":"blocks"}]}),
            json!({"id":"UNDATED","title":"Critical without dates","status":"blocked","priority":0,"issue_type":"task"}),
            json!({"id":"DONE","title":"Finished","status":"closed","priority":2,"issue_type":"task","created_at":old,"closed_at":now.to_rfc3339()}),
        ]);
        let sprint = json!({"id":"sprint","name":"Contract sprint","start_date":(now - chrono::Duration::days(4)).to_rfc3339(),"end_date":(now + chrono::Duration::days(7)).to_rfc3339(),"bead_ids":["WAIT","UNDATED","DONE"]});
        std::fs::write(
            fixture.path().join(".beads/sprints.jsonl"),
            format!("{sprint}\n"),
        )
        .expect("sprint fixture");
        let output = run(fixture.path(), &["--robot-burndown", "sprint"]);
        let schema = schema(fixture.path(), "robot-burndown");
        assert!(
            output["daily_points"]
                .as_array()
                .is_some_and(|points| !points.is_empty())
        );
        assert!(
            output["ideal_line"]
                .as_array()
                .is_some_and(|points| !points.is_empty())
        );
        assert!(output.get("burndown").is_none());
        assert!(output.get("scope_changes").is_none());
        assert!(output["projected_complete"].is_string());
        let risks = output["at_risk"].as_array().expect("risks");
        assert_eq!(risks.len(), 2);
        assert_eq!(risks[0]["id"], "UNDATED");
        assert_eq!(risks[0]["signals"], json!(["critical_blocked"]));
        assert!(risks[0].get("since").is_none());
        assert_eq!(risks[1]["id"], "WAIT");
        assert_eq!(
            risks[1]["signals"],
            json!([
                "blocked_too_long",
                "no_activity",
                "critical_blocked",
                "blockers_not_closing"
            ])
        );
        assert!(risks[1]["since"].is_string());
        assert_accepts(&schema, &output);
        // The serializer also omits empty titles. The CLI loader requires
        // titles, so test that schema allowance separately from real output.
        let mut no_title = output.clone();
        no_title["at_risk"][0]
            .as_object_mut()
            .expect("risk object")
            .remove("title");
        assert_accepts(&schema, &no_title);
        assert_required(&schema, &output, "", "at_risk");
        assert_required(&schema, &output, "/at_risk/0", "signals");
        assert_bad_field(&schema, &output, "/at_risk/0/priority", json!("P0"));
        assert_bad_field(
            &schema,
            &output,
            "/at_risk/0/signals/0",
            json!("unknown_signal"),
        );
        assert_bad_field(&schema, &output, "/at_risk/1/since", Value::Null);
        assert_bad_field(&schema, &output, "/at_risk/1/since", json!("yesterday"));
        assert_bad_field(&schema, &output, "/daily_points/0/remaining", json!("2"));
        assert_bad_field(&schema, &output, "/projected_complete", Value::Null);
    }

    #[test]
    fn undated_sprint_matches_schema_without_null_dates_or_risk_items() {
        let fixture = fixture(&[]);
        std::fs::write(
            fixture.path().join(".beads/sprints.jsonl"),
            "{\"id\":\"sprint\",\"name\":\"Undated\",\"bead_ids\":[]}\n",
        )
        .expect("undated sprint");
        let output = run(fixture.path(), &["--robot-burndown", "sprint"]);
        let schema = schema(fixture.path(), "robot-burndown");
        for field in [
            "start_date",
            "end_date",
            "projected_complete",
            "scope_changes",
        ] {
            assert!(output.get(field).is_none(), "omitted optional {field}");
        }
        assert_eq!(output["at_risk"], json!([]));
        assert_eq!(output["daily_points"], json!([]));
        assert_accepts(&schema, &output);
        let mut null_date = output.clone();
        null_date["start_date"] = Value::Null;
        assert_rejects(&schema, &null_date, "null start_date");
        assert_required(&schema, &output, "", "daily_points");
    }

    #[test]
    fn contract_evaluator_applies_assertions_and_ref_siblings() {
        let schema = json!({
            "$defs":{"count":{"type":"integer","minimum":0}},
            "type":"object",
            "properties":{
                "count":{"$ref":"#/$defs/count","enum":[0,2]},
                "nullable":{"anyOf":[{"type":"null"},{"const":"ready"}]},
                "dates":{"type":"array","items":{"type":"string","format":"date-time"}}
            },
            "required":["count","nullable","dates"],
            "additionalProperties":false,
            "oneOf":[{"properties":{"count":{"const":0}}},{"properties":{"count":{"const":2}}}],
            "not":{"properties":{"nullable":{"const":"forbidden"}}}
        });
        let valid = json!({"count":2,"nullable":null,"dates":["2026-10-09T00:00:00Z"]});
        assert_accepts(&schema, &valid);
        for count in [json!(-1), json!(1), json!(1.5), json!("2")] {
            assert_bad_field(&schema, &valid, "/count", count);
        }
        assert_bad_field(&schema, &valid, "/nullable", json!(false));
        assert_bad_field(&schema, &valid, "/dates/0", json!("not a date"));
        assert_required(&schema, &valid, "", "nullable");
        let mut extra = valid;
        extra["extra"] = json!(true);
        assert_rejects(&schema, &extra, "unadvertised field");
        let ambiguous = json!({"oneOf":[{"type":"integer"},{"type":"number"}]});
        assert_rejects(&ambiguous, &json!(2), "two matching alternatives");
        let no_pick =
            json!({"type":"object","not":{"anyOf":[{"required":["id"]},{"required":["score"]}]}});
        assert_accepts(&no_pick, &json!({}));
        assert_rejects(&no_pick, &json!({"id":"READY"}), "first forbidden field");
        assert_rejects(&no_pick, &json!({"score":0.7}), "second forbidden field");
    }

    #[test]
    #[should_panic(expected = "unsupported schema assertion: pattern")]
    fn contract_evaluator_rejects_unhandled_assertions() {
        let schema = json!({"not":{"type":"string","pattern":"^ready$"}});
        assert_accepts(&schema, &json!("blocked"));
    }
}
