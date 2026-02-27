use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use serde_json::Value;

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

fn run_bvr_json_owned(flags: &[String], beads_file: &str) -> Value {
    let root = repo_root();
    let beads_path = root.join(beads_file);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.args(flags);
    command.arg("--beads-file").arg(&beads_path);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn run_bvr_json_from_path(flags: &[&str], beads_path: &std::path::Path) -> Value {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.args(flags);
    command.arg("--beads-file").arg(beads_path);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn run_bvr_json_in_dir(flags: &[&str], dir: &std::path::Path) -> Value {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(dir);
    command.args(flags);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

fn load_fixture(path: &str) -> Value {
    let root = repo_root();
    let fixture_path = root.join(path);
    let fixture_text = fs::read_to_string(fixture_path).expect("fixture file");
    serde_json::from_str(&fixture_text).expect("fixture json")
}

#[test]
fn robot_triage_conforms_to_fixture_core_fields() {
    let root = repo_root();
    let fixture_path = root.join("tests/conformance/fixtures/go_outputs/bvr.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("fixture file");
    let fixture: Value = serde_json::from_str(&fixture_text).expect("fixture json");

    let actual = run_bvr_json(&["--robot-triage"], "tests/testdata/minimal.jsonl");

    assert_eq!(
        actual["triage"]["quick_ref"]["total_open"],
        fixture["triage"]["triage"]["quick_ref"]["open_count"]
    );
    assert_eq!(
        actual["triage"]["quick_ref"]["total_actionable"],
        fixture["triage"]["triage"]["quick_ref"]["actionable_count"]
    );
    assert_eq!(
        actual["triage"]["quick_ref"]["top_picks"][0]["id"],
        fixture["triage"]["triage"]["quick_ref"]["top_picks"][0]["id"]
    );
}

#[test]
fn robot_plan_is_deterministic_for_minimal_fixture() {
    let first = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    let second = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");

    assert_eq!(first["plan"], second["plan"]);
}

#[test]
fn robot_plan_and_priority_publish_full_status_matrix() {
    let plan = run_bvr_json(&["--robot-plan"], "tests/testdata/minimal.jsonl");
    let priority = run_bvr_json(&["--robot-priority"], "tests/testdata/minimal.jsonl");

    for key in [
        "PageRank",
        "Betweenness",
        "Eigenvector",
        "HITS",
        "Critical",
        "Cycles",
        "KCore",
        "Articulation",
        "Slack",
    ] {
        assert_eq!(plan["status"][key]["state"], "computed");
        assert_eq!(priority["status"][key]["state"], "computed");
    }
}

#[test]
fn robot_priority_respects_max_results_filter() {
    let output = run_bvr_json(
        &["--robot-priority", "--robot-max-results", "1"],
        "tests/testdata/synthetic_complex.jsonl",
    );

    let count = output["recommendations"]
        .as_array()
        .expect("recommendations array")
        .len();
    assert_eq!(count, 1);
}

#[test]
fn robot_history_supports_filter_and_limit() {
    let history = run_bvr_json(
        &["--robot-history", "--history-limit", "1"],
        "tests/testdata/minimal.jsonl",
    );
    let history_len = history["histories"]
        .as_object()
        .expect("histories map")
        .len();
    assert_eq!(history_len, 1);
    assert!(history.get("history_count").is_none());
    assert!(history.get("histories_timeline").is_none());

    let bead = run_bvr_json(&["--bead-history", "A"], "tests/testdata/minimal.jsonl");
    assert_eq!(bead["history_count"], 1);
    assert_eq!(bead["histories_timeline"][0]["id"], "A");
}

#[test]
fn robot_forecast_returns_expected_payload() {
    let output = run_bvr_json(
        &["--robot-forecast", "all", "--forecast-agents", "2"],
        "tests/testdata/minimal.jsonl",
    );

    assert_eq!(output["agents"], 2);
    assert_eq!(output["forecast_count"], 2);
    assert_eq!(
        output["forecasts"]
            .as_array()
            .expect("forecasts array")
            .len(),
        2
    );
    assert!(output.get("filters").is_none());
    assert!(output["forecasts"][0]["eta_date_low"].is_string());
    assert!(output["forecasts"][0]["eta_date_high"].is_string());
    assert!(output["forecasts"][0]["velocity_minutes_per_day"].is_number());
    assert!(output.get("summary").is_some());
}

#[test]
fn robot_diff_compares_snapshots() {
    let root = repo_root();
    let before = root.join("tests/testdata/minimal.jsonl");
    let flags = vec![
        "--robot-diff".to_string(),
        "--diff-since".to_string(),
        before.to_string_lossy().to_string(),
    ];

    let output = run_bvr_json_owned(&flags, "tests/testdata/synthetic_complex.jsonl");
    let new_issues = output["diff"]["summary"]["issues_added"]
        .as_u64()
        .expect("diff.summary.issues_added");
    assert!(new_issues > 0);
}

#[test]
fn robot_forecast_core_fields_match_legacy_fixture() {
    let root = repo_root();
    let fixture_path = root.join("tests/conformance/fixtures/go_outputs/bvr_extended.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("extended fixture file");
    let fixture: Value = serde_json::from_str(&fixture_text).expect("extended fixture json");

    let actual = run_bvr_json(
        &["--robot-forecast", "all", "--forecast-agents", "2"],
        "tests/testdata/synthetic_complex.jsonl",
    );

    assert_eq!(actual["agents"], fixture["forecast"]["agents"]);
    assert_eq!(
        actual["forecast_count"],
        fixture["forecast"]["forecast_count"]
    );
    assert!(actual.get("filters").is_none());
    assert!(actual["summary"].is_object());

    let actual_items = actual["forecasts"].as_array().expect("actual forecasts");
    let fixture_items = fixture["forecast"]["forecasts"]
        .as_array()
        .expect("fixture forecasts");
    assert_eq!(actual_items.len(), fixture_items.len());

    for (actual_item, fixture_item) in actual_items.iter().zip(fixture_items.iter()) {
        assert_eq!(actual_item["issue_id"], fixture_item["issue_id"]);
        assert_eq!(
            actual_item["estimated_minutes"],
            fixture_item["estimated_minutes"]
        );
        let actual_velocity = actual_item["velocity_minutes_per_day"]
            .as_f64()
            .expect("actual velocity");
        let fixture_velocity = fixture_item["velocity_minutes_per_day"]
            .as_f64()
            .expect("fixture velocity");
        assert!((actual_velocity - fixture_velocity).abs() < 1e-9);
        assert_eq!(actual_item["factors"], fixture_item["factors"]);
        assert!(actual_item["eta_date"].is_string());
        assert!(actual_item["eta_date_low"].is_string());
        assert!(actual_item["eta_date_high"].is_string());
    }

    assert_eq!(
        actual["summary"]["total_minutes"],
        fixture["forecast"]["summary"]["total_minutes"]
    );
    let actual_total_days = actual["summary"]["total_days"]
        .as_f64()
        .expect("actual total_days");
    let fixture_total_days = fixture["forecast"]["summary"]["total_days"]
        .as_f64()
        .expect("fixture total_days");
    assert!((actual_total_days - fixture_total_days).abs() < 1e-9);

    let actual_avg_conf = actual["summary"]["avg_confidence"]
        .as_f64()
        .expect("actual avg_confidence");
    let fixture_avg_conf = fixture["forecast"]["summary"]["avg_confidence"]
        .as_f64()
        .expect("fixture avg_confidence");
    assert!((actual_avg_conf - fixture_avg_conf).abs() < 1e-9);

    // Envelope metadata parity: output_format and version fields
    assert_eq!(
        actual["output_format"].as_str().expect("output_format"),
        "json",
        "forecast output_format must be 'json'"
    );
    assert!(
        actual["version"]
            .as_str()
            .expect("version")
            .starts_with('v'),
        "forecast version must start with 'v'"
    );
}

#[test]
fn robot_diff_core_fields_match_legacy_fixture() {
    let root = repo_root();
    let fixture_path = root.join("tests/conformance/fixtures/go_outputs/bvr_extended.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("extended fixture file");
    let fixture: Value = serde_json::from_str(&fixture_text).expect("extended fixture json");

    let actual = run_bvr_json(
        &[
            "--robot-diff",
            "--diff-since",
            "tests/testdata/minimal.jsonl",
        ],
        "tests/testdata/synthetic_complex.jsonl",
    );

    assert!(actual["diff"]["from_timestamp"].is_string());
    assert!(actual["diff"]["to_timestamp"].is_string());
    assert_eq!(
        actual["diff"]["summary"]["issues_added"],
        fixture["diff"]["diff"]["summary"]["issues_added"]
    );
    assert_eq!(
        actual["diff"]["summary"]["issues_removed"],
        fixture["diff"]["diff"]["summary"]["issues_removed"]
    );
    assert_eq!(
        actual["diff"]["metric_deltas"]["total_issues"],
        fixture["diff"]["diff"]["metric_deltas"]["total_issues"]
    );
    assert_eq!(
        actual["diff"]["metric_deltas"]["cycle_count"],
        fixture["diff"]["diff"]["metric_deltas"]["cycle_count"]
    );

    let actual_new = actual["diff"]["new_issues"]
        .as_array()
        .expect("actual new_issues");
    let fixture_new = fixture["diff"]["diff"]["new_issues"]
        .as_array()
        .expect("fixture new_issues");
    assert_eq!(actual_new.len(), fixture_new.len());

    for (actual_issue, fixture_issue) in actual_new.iter().zip(fixture_new.iter()) {
        assert_eq!(actual_issue["id"], fixture_issue["id"]);
        assert_eq!(actual_issue["status"], fixture_issue["status"]);
        assert_eq!(actual_issue["priority"], fixture_issue["priority"]);
        assert_eq!(actual_issue["issue_type"], fixture_issue["issue_type"]);
        assert_eq!(actual_issue["created_at"], fixture_issue["created_at"]);
        assert_eq!(actual_issue["updated_at"], fixture_issue["updated_at"]);
        if fixture_issue.get("assignee").is_some() {
            assert_eq!(actual_issue["assignee"], fixture_issue["assignee"]);
        }
        if fixture_issue.get("labels").is_some() {
            assert_eq!(actual_issue["labels"], fixture_issue["labels"]);
        }
        if fixture_issue.get("dependencies").is_some() {
            assert_eq!(actual_issue["dependencies"], fixture_issue["dependencies"]);
        }
        if fixture_issue.get("comments").is_some() {
            assert_eq!(actual_issue["comments"], fixture_issue["comments"]);
        }
        assert!(actual_issue.get("design").is_none());
        assert!(actual_issue.get("acceptance_criteria").is_none());
        assert!(actual_issue.get("notes").is_none());
        assert!(actual_issue.get("source_repo").is_none());
    }
}

#[test]
fn robot_history_core_fields_match_legacy_fixture() {
    let root = repo_root();
    let fixture_path = root.join("tests/conformance/fixtures/go_outputs/bvr_extended.json");
    let fixture_text = fs::read_to_string(fixture_path).expect("extended fixture file");
    let fixture: Value = serde_json::from_str(&fixture_text).expect("extended fixture json");

    let actual = run_bvr_json(
        &["--robot-history", "--history-limit", "20"],
        "tests/testdata/synthetic_complex.jsonl",
    );

    assert!(actual.get("bead_history").is_none());
    assert!(actual.get("history_count").is_none());
    assert!(actual.get("histories_timeline").is_none());

    assert_eq!(
        actual["stats"]["total_beads"],
        fixture["history"]["stats"]["total_beads"]
    );
    assert!(actual["histories"].is_object());

    // Validate each history entry has the expected shape.
    let histories = actual["histories"].as_object().expect("histories object");
    for (bead_id, entry) in histories {
        assert!(
            entry["bead_id"].is_string(),
            "{bead_id}: bead_id should be a string"
        );
        assert!(
            entry["events"].is_array(),
            "{bead_id}: events should be an array"
        );
        assert!(
            entry["milestones"].is_object(),
            "{bead_id}: milestones should be an object"
        );

        // Milestones should NOT have null-valued keys (skip_serializing_if).
        let milestones = entry["milestones"].as_object().unwrap();
        for (ms_key, ms_val) in milestones {
            assert!(
                !ms_val.is_null(),
                "{bead_id}: milestone {ms_key} should not be null"
            );
        }
    }
}

#[test]
fn robot_capacity_core_fields_match_legacy_fixture() {
    let fixture = load_fixture("tests/conformance/fixtures/go_outputs/bvr_extended.json");

    let actual = run_bvr_json(
        &["--robot-capacity", "--agents", "3"],
        "tests/testdata/synthetic_complex.jsonl",
    );

    assert_eq!(
        actual["open_issue_count"],
        fixture["capacity"]["open_issue_count"]
    );
    assert_eq!(
        actual["total_minutes"],
        fixture["capacity"]["total_minutes"]
    );
    assert_eq!(
        actual["serial_minutes"],
        fixture["capacity"]["serial_minutes"]
    );
    assert_eq!(
        actual["parallel_minutes"],
        fixture["capacity"]["parallel_minutes"]
    );
    assert_eq!(
        actual["critical_path_length"],
        fixture["capacity"]["critical_path_length"]
    );
    assert_eq!(
        actual["actionable_count"],
        fixture["capacity"]["actionable_count"]
    );
    assert_eq!(
        actual["critical_path"],
        fixture["capacity"]["critical_path"]
    );
    assert_eq!(actual["actionable"], fixture["capacity"]["actionable"]);

    let actual_estimated_days = actual["estimated_days"]
        .as_f64()
        .expect("actual estimated_days");
    let fixture_estimated_days = fixture["capacity"]["estimated_days"]
        .as_f64()
        .expect("fixture estimated_days");
    assert!((actual_estimated_days - fixture_estimated_days).abs() < 1e-9);

    // Envelope metadata parity: output_format and version fields
    assert_eq!(
        actual["output_format"].as_str().expect("output_format"),
        "json",
        "capacity output_format must be 'json'"
    );
    assert!(
        actual["version"]
            .as_str()
            .expect("version")
            .starts_with('v'),
        "capacity version must start with 'v'"
    );

    let label_scoped = run_bvr_json(
        &[
            "--robot-capacity",
            "--capacity-label",
            "backend",
            "--agents",
            "1",
        ],
        "tests/testdata/synthetic_complex.jsonl",
    );
    assert_eq!(
        label_scoped["open_issue_count"],
        fixture["capacity_by_label"]["open_issue_count"]
    );
    assert_eq!(
        label_scoped["total_minutes"],
        fixture["capacity_by_label"]["total_minutes"]
    );
    assert_eq!(
        label_scoped["estimated_days"],
        fixture["capacity_by_label"]["estimated_days"]
    );
    assert_eq!(label_scoped["label"], fixture["capacity_by_label"]["label"]);
    assert_eq!(
        label_scoped["actionable_count"],
        fixture["capacity_by_label"]["actionable_count"]
    );
}

#[test]
fn robot_adversarial_triage_core_fields_match_legacy_fixture() {
    let fixture = load_fixture("tests/conformance/fixtures/go_outputs/bvr_adversarial.json");
    let actual = run_bvr_json(
        &["--robot-triage"],
        "tests/testdata/adversarial_parity.jsonl",
    );

    assert_eq!(
        actual["triage"]["quick_ref"]["total_open"],
        fixture["triage"]["triage"]["quick_ref"]["open_count"]
    );
    assert_eq!(
        actual["triage"]["quick_ref"]["total_actionable"],
        fixture["triage"]["triage"]["quick_ref"]["actionable_count"]
    );

    let mut actual_ids = actual["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .expect("actual top picks")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    actual_ids.sort_unstable();

    let mut fixture_ids = fixture["triage"]["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .expect("fixture top picks")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    fixture_ids.sort_unstable();

    assert_eq!(actual_ids, fixture_ids);
}

#[test]
fn robot_adversarial_plan_forecast_history_diff_core_fields_match_legacy_fixture() {
    let fixture = load_fixture("tests/conformance/fixtures/go_outputs/bvr_adversarial.json");

    let plan = run_bvr_json(&["--robot-plan"], "tests/testdata/adversarial_parity.jsonl");
    assert_eq!(
        plan["plan"]["tracks"]
            .as_array()
            .expect("actual tracks")
            .len(),
        fixture["plan"]["plan"]["tracks"]
            .as_array()
            .expect("fixture tracks")
            .len()
    );
    assert_eq!(
        plan["plan"]["summary"]["highest_impact"],
        fixture["plan"]["plan"]["summary"]["highest_impact"]
    );

    let forecast = run_bvr_json(
        &["--robot-forecast", "all", "--forecast-agents", "2"],
        "tests/testdata/adversarial_parity.jsonl",
    );
    assert_eq!(
        forecast["forecast_count"],
        fixture["forecast"]["forecast_count"]
    );
    assert_eq!(
        forecast["summary"]["total_minutes"],
        fixture["forecast"]["summary"]["total_minutes"]
    );
    let actual_forecast_ids = forecast["forecasts"]
        .as_array()
        .expect("actual forecasts")
        .iter()
        .filter_map(|item| item["issue_id"].as_str())
        .collect::<Vec<_>>();
    let fixture_forecast_ids = fixture["forecast"]["forecasts"]
        .as_array()
        .expect("fixture forecasts")
        .iter()
        .filter_map(|item| item["issue_id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(actual_forecast_ids, fixture_forecast_ids);

    let history = run_bvr_json(
        &["--robot-history", "--history-limit", "20"],
        "tests/testdata/adversarial_parity.jsonl",
    );
    assert_eq!(
        history["stats"]["total_beads"],
        fixture["history"]["stats"]["total_beads"]
    );
    assert_eq!(
        history["stats"]["total_commits"],
        fixture["history"]["stats"]["total_commits"]
    );
    assert_eq!(
        history["histories"]
            .as_object()
            .expect("actual histories")
            .len(),
        fixture["history"]["histories"]
            .as_object()
            .expect("fixture histories")
            .len()
    );

    let diff = run_bvr_json(
        &[
            "--robot-diff",
            "--diff-since",
            "tests/testdata/minimal.jsonl",
        ],
        "tests/testdata/adversarial_parity.jsonl",
    );
    assert_eq!(diff["diff"]["summary"], fixture["diff"]["diff"]["summary"]);
    assert_eq!(
        diff["diff"]["metric_deltas"]["total_issues"],
        fixture["diff"]["diff"]["metric_deltas"]["total_issues"]
    );
    assert_eq!(
        diff["diff"]["new_issues"]
            .as_array()
            .expect("actual new issues")
            .len(),
        fixture["diff"]["diff"]["new_issues"]
            .as_array()
            .expect("fixture new issues")
            .len()
    );
}

#[test]
fn robot_graph_json_supports_root_depth_and_label_filters() {
    let temp = tempfile::tempdir().expect("tempdir");
    let beads_path = temp.path().join("graph.jsonl");
    fs::write(
        &beads_path,
        concat!(
            "{\"id\":\"A\",\"title\":\"Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"labels\":[\"api\"]}\n",
            "{\"id\":\"B\",\"title\":\"Middle\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"labels\":[\"api\"],\"dependencies\":[{\"depends_on_id\":\"A\",\"type\":\"blocks\"}]}\n",
            "{\"id\":\"C\",\"title\":\"Leaf\",\"status\":\"open\",\"priority\":3,\"issue_type\":\"task\",\"labels\":[\"cli\"],\"dependencies\":[{\"depends_on_id\":\"B\",\"type\":\"blocks\"}]}\n"
        ),
    )
    .expect("write beads");

    let full = run_bvr_json_from_path(&["--robot-graph"], &beads_path);
    assert_eq!(full["format"], "json");
    assert_eq!(full["nodes"], 3);
    assert_eq!(full["edges"], 2);
    assert!(full["adjacency"].is_object());

    let filtered = run_bvr_json_from_path(
        &["--robot-graph", "--graph-root", "C", "--graph-depth", "1"],
        &beads_path,
    );
    assert_eq!(filtered["format"], "json");
    assert_eq!(filtered["nodes"], 2);
    assert_eq!(filtered["filters_applied"]["root"], "C");
    assert_eq!(filtered["filters_applied"]["depth"], "1");

    let ids = filtered["adjacency"]["nodes"]
        .as_array()
        .expect("adjacency nodes")
        .iter()
        .filter_map(|node| node["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"B"));
    assert!(ids.contains(&"C"));
    assert!(!ids.contains(&"A"));

    let labeled = run_bvr_json_from_path(&["--robot-graph", "--label", "api"], &beads_path);
    assert_eq!(labeled["format"], "json");
    assert_eq!(labeled["nodes"], 2);
    assert_eq!(labeled["filters_applied"]["label"], "api");
}

#[test]
fn robot_graph_dot_and_mermaid_emit_expected_markers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let beads_path = temp.path().join("graph.jsonl");
    fs::write(
        &beads_path,
        concat!(
            "{\"id\":\"A\",\"title\":\"Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"B\",\"title\":\"Child\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"A\",\"type\":\"blocks\"}]}\n"
        ),
    )
    .expect("write beads");

    let dot = run_bvr_json_from_path(&["--robot-graph", "--graph-format", "dot"], &beads_path);
    assert_eq!(dot["format"], "dot");
    let dot_graph = dot["graph"].as_str().expect("dot graph");
    assert!(dot_graph.contains("digraph G {"));
    assert!(dot_graph.contains("rankdir=LR;"));

    let mermaid =
        run_bvr_json_from_path(&["--robot-graph", "--graph-format", "mermaid"], &beads_path);
    assert_eq!(mermaid["format"], "mermaid");
    let mermaid_graph = mermaid["graph"].as_str().expect("mermaid graph");
    assert!(mermaid_graph.contains("graph TD"));
    assert!(mermaid_graph.contains("classDef"));
}

#[test]
fn robot_parity_slice_surfaces_bd_3q0_across_graph_insights_and_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let beads_path = temp.path().join("bd-3q0-parity.jsonl");
    fs::write(
        &beads_path,
        concat!(
            "{\"id\":\"bd-3q0\",\"title\":\"Primary blocker\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"feature\",\"created_at\":\"2026-02-18T03:00:00Z\",\"updated_at\":\"2026-02-18T03:05:00Z\",\"labels\":[\"parity\",\"tui\"]}\n",
            "{\"id\":\"bd-3q1\",\"title\":\"Blocked follow-on\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"2026-02-18T03:01:00Z\",\"updated_at\":\"2026-02-18T03:06:00Z\",\"labels\":[\"parity\"],\"dependencies\":[{\"depends_on_id\":\"bd-3q0\",\"type\":\"blocks\"}]}\n",
            "{\"id\":\"bd-3q2\",\"title\":\"Independent slice\",\"status\":\"open\",\"priority\":3,\"issue_type\":\"task\",\"created_at\":\"2026-02-18T03:02:00Z\",\"updated_at\":\"2026-02-18T03:07:00Z\",\"labels\":[\"graph\"]}\n"
        ),
    )
    .expect("write beads");

    let graph = run_bvr_json_from_path(&["--robot-graph"], &beads_path);
    assert_eq!(graph["format"], "json");
    assert_eq!(graph["nodes"], 3);
    let edges = graph["adjacency"]["edges"]
        .as_array()
        .expect("graph adjacency edges");
    assert!(edges.iter().any(|edge| {
        edge["from"] == "bd-3q1" && edge["to"] == "bd-3q0" && edge["type"] == "blocks"
    }));

    let insights = run_bvr_json_from_path(&["--robot-insights"], &beads_path);
    assert_eq!(insights["insights"]["bottlenecks"][0]["id"], "bd-3q0");
    assert_eq!(insights["insights"]["bottlenecks"][0]["blocks_count"], 1);
    assert_eq!(insights["insights"]["critical_path"][0], "bd-3q0");

    let bead_history = run_bvr_json_from_path(&["--bead-history", "bd-3q1"], &beads_path);
    assert_eq!(bead_history["history_count"], 1);
    let timeline_events = bead_history["histories_timeline"][0]["events"]
        .as_array()
        .expect("timeline events");
    assert!(
        timeline_events.iter().any(|event| {
            event["kind"] == "dependency" && event["details"] == "Blocked by bd-3q0"
        })
    );
    let history_events = bead_history["histories"]["bd-3q1"]["events"]
        .as_array()
        .expect("history events");
    assert!(history_events.iter().any(|event| {
        event["event_type"] == "dependency" && event["commit_message"] == "Blocked by bd-3q0"
    }));
}

#[test]
fn robot_history_correlates_git_commits_and_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();

    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::create_dir_all(repo_dir.join("pkg")).expect("mkdir pkg");

    let run_git = |args: &[&str]| {
        let mut command = std::process::Command::new("git");
        command.current_dir(repo_dir);
        command.args(args);
        command.env("GIT_AUTHOR_NAME", "Test");
        command.env("GIT_AUTHOR_EMAIL", "test@example.com");
        command.env("GIT_COMMITTER_NAME", "Test");
        command.env("GIT_COMMITTER_EMAIL", "test@example.com");
        let output = command.output().expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run_git(&["init"]);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    run_git(&["add", ".beads/beads.jsonl"]);
    run_git(&["commit", "-m", "seed HIST-1"]);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"in_progress\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    fs::write(
        repo_dir.join("pkg/work.go"),
        "package pkg\n\n// work in progress\n",
    )
    .expect("write work.go");
    run_git(&["add", ".beads/beads.jsonl", "pkg/work.go"]);
    run_git(&["commit", "-m", "claim HIST-1 with code"]);

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"HIST-1\",\"title\":\"History bead\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");
    fs::write(
        repo_dir.join("pkg/work.go"),
        "package pkg\n\n// finished work\nfunc Done() {}\n",
    )
    .expect("write work.go");
    run_git(&["add", ".beads/beads.jsonl", "pkg/work.go"]);
    run_git(&["commit", "-m", "close HIST-1"]);

    let payload = run_bvr_json_in_dir(&["--robot-history"], repo_dir);

    assert!(payload["latest_commit_sha"].as_str().is_some());
    assert_eq!(payload["stats"]["total_beads"], 1);
    assert_eq!(payload["stats"]["beads_with_commits"], 1);
    assert!(
        payload["stats"]["method_distribution"]["co_committed"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );

    let commits = payload["histories"]["HIST-1"]["commits"]
        .as_array()
        .expect("commits array");
    assert!(!commits.is_empty());

    let has_path_hint = commits.iter().any(|commit| {
        commit["files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["path"] == "pkg/work.go"))
    });
    assert!(has_path_hint, "expected pkg/work.go path hint in commits");

    assert!(payload["histories"]["HIST-1"]["milestones"]["closed"].is_object());
    assert!(payload["commit_index"].is_object());
}

#[test]
fn robot_capacity_estimated_days_drops_with_more_agents() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":480,\"labels\":[\"backend\"]}\n",
            "{\"id\":\"B\",\"title\":\"B\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":480,\"labels\":[\"backend\"]}\n",
            "{\"id\":\"C\",\"title\":\"C\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":480,\"labels\":[\"frontend\"]}\n"
        ),
    )
    .expect("write beads");

    let one = run_bvr_json_in_dir(&["--robot-capacity", "--agents", "1"], repo_dir);
    let three = run_bvr_json_in_dir(&["--robot-capacity", "--agents", "3"], repo_dir);

    assert_eq!(one["open_issue_count"], 3);
    assert_eq!(three["open_issue_count"], 3);
    assert!(one["total_minutes"].as_i64().unwrap_or_default() > 0);
    assert_eq!(one["total_minutes"], three["total_minutes"]);
    assert!(
        three["estimated_days"].as_f64().unwrap_or(f64::INFINITY)
            < one["estimated_days"].as_f64().unwrap_or(f64::INFINITY)
    );

    let backend = run_bvr_json_in_dir(
        &[
            "--robot-capacity",
            "--capacity-label",
            "backend",
            "--agents",
            "1",
        ],
        repo_dir,
    );
    assert_eq!(backend["label"], "backend");
    assert_eq!(backend["open_issue_count"], 2);

    let backend_forecast = run_bvr_json_in_dir(
        &[
            "--robot-forecast",
            "all",
            "--forecast-label",
            "backend",
            "--forecast-agents",
            "1",
        ],
        repo_dir,
    );
    assert_eq!(
        backend["total_minutes"],
        backend_forecast["summary"]["total_minutes"]
    );

    let spaced_label = run_bvr_json_in_dir(
        &[
            "--robot-capacity",
            "--capacity-label",
            " backend ",
            "--agents",
            "1",
        ],
        repo_dir,
    );
    assert_eq!(spaced_label["label"], " backend ");
    assert_eq!(spaced_label["open_issue_count"], 0);
    assert_eq!(spaced_label["total_minutes"], 0);
}

#[test]
fn robot_forecast_supports_sprint_filter_for_all() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120,\"labels\":[\"backend\"]}\n",
            "{\"id\":\"B\",\"title\":\"B\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120,\"labels\":[\"backend\"]}\n",
            "{\"id\":\"C\",\"title\":\"C\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120,\"labels\":[\"backend\"]}\n"
        ),
    )
    .expect("write beads");

    fs::write(
        repo_dir.join(".beads/sprints.jsonl"),
        "{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"bead_ids\":[\"A\"]}\n",
    )
    .expect("write sprints");

    let payload = run_bvr_json_in_dir(
        &[
            "--robot-forecast",
            "all",
            "--forecast-label",
            "backend",
            "--forecast-sprint",
            "sprint-1",
            "--forecast-agents",
            "2",
        ],
        repo_dir,
    );

    assert_eq!(payload["filters"]["label"], "backend");
    assert_eq!(payload["filters"]["sprint"], "sprint-1");
    assert_eq!(payload["agents"], 2);
    assert_eq!(payload["forecast_count"], 1);
    assert_eq!(payload["forecasts"][0]["issue_id"], "A");
}

#[test]
fn robot_forecast_single_issue_ignores_label_and_sprint_membership() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120,\"labels\":[\"backend\"]}\n",
            "{\"id\":\"B\",\"title\":\"B\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120,\"labels\":[\"api\"]}\n"
        ),
    )
    .expect("write beads");

    fs::write(
        repo_dir.join(".beads/sprints.jsonl"),
        "{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"bead_ids\":[\"A\"]}\n",
    )
    .expect("write sprints");

    let payload = run_bvr_json_in_dir(
        &[
            "--robot-forecast",
            "B",
            "--forecast-label",
            "backend",
            "--forecast-sprint",
            "sprint-1",
        ],
        repo_dir,
    );

    assert_eq!(payload["forecast_count"], 1);
    assert_eq!(payload["forecasts"][0]["issue_id"], "B");
}

#[test]
fn robot_burndown_current_sprint_matches_legacy_shape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    let now = chrono::Utc::now();
    let start =
        chrono::DateTime::parse_from_rfc3339(&format!("{}T00:00:00Z", now.format("%Y-%m-%d")))
            .expect("start")
            .with_timezone(&chrono::Utc)
            .checked_sub_signed(chrono::Duration::days(1))
            .expect("start minus one");
    let end = start + chrono::Duration::days(4);

    let closed1 = (start + chrono::Duration::hours(1)).to_rfc3339();
    let closed2 = (start + chrono::Duration::hours(2)).to_rfc3339();
    let t0 = start.to_rfc3339();

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        format!(
            concat!(
                "{{\"id\":\"A\",\"title\":\"Done 1\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{t0}\",\"updated_at\":\"{t0}\",\"closed_at\":\"{closed1}\"}}\n",
                "{{\"id\":\"B\",\"title\":\"Done 2\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{t0}\",\"updated_at\":\"{t0}\",\"closed_at\":\"{closed2}\"}}\n",
                "{{\"id\":\"C\",\"title\":\"Open\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{t0}\",\"updated_at\":\"{t0}\"}}\n"
            ),
            t0 = t0,
            closed1 = closed1,
            closed2 = closed2
        ),
    )
    .expect("write beads");

    fs::write(
        repo_dir.join(".beads/sprints.jsonl"),
        format!(
            "{{\"id\":\"sprint-1\",\"name\":\"Sprint 1\",\"start_date\":\"{}\",\"end_date\":\"{}\",\"bead_ids\":[\"A\",\"B\",\"C\"]}}\n",
            start.to_rfc3339(),
            end.to_rfc3339(),
        ),
    )
    .expect("write sprints");

    let payload = run_bvr_json_in_dir(&["--robot-burndown", "current"], repo_dir);

    assert_eq!(payload["sprint_id"], "sprint-1");
    assert_eq!(payload["total_issues"], 3);
    assert_eq!(payload["completed_issues"], 2);
    assert_eq!(payload["remaining_issues"], 1);
    assert!(payload["elapsed_days"].as_u64().unwrap_or_default() > 0);
    assert!(payload["total_days"].as_u64().unwrap_or_default() > 0);

    let daily_points = payload["daily_points"].as_array().expect("daily points");
    let elapsed_days = payload["elapsed_days"].as_u64().expect("elapsed days");
    assert_eq!(
        u64::try_from(daily_points.len()).unwrap_or(u64::MAX),
        elapsed_days
    );

    let last = daily_points.last().expect("last daily point");
    assert_eq!(last["completed"], 2);
    assert_eq!(last["remaining"], 1);

    let ideal_line = payload["ideal_line"].as_array().expect("ideal line");
    assert_eq!(
        u64::try_from(ideal_line.len()).unwrap_or(u64::MAX),
        payload["total_days"].as_u64().unwrap_or_default() + 1
    );
    assert_eq!(
        ideal_line
            .last()
            .and_then(|entry| entry["remaining"].as_i64())
            .unwrap_or_default(),
        0
    );

    // Envelope metadata
    assert_eq!(
        payload["output_format"].as_str().expect("output_format"),
        "json"
    );
    assert!(
        payload["version"]
            .as_str()
            .expect("version")
            .starts_with('v')
    );
}

#[test]
fn robot_burndown_closed_issue_without_closed_at_still_counts_as_completed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    let now = chrono::Utc::now();
    let start =
        chrono::DateTime::parse_from_rfc3339(&format!("{}T00:00:00Z", now.format("%Y-%m-%d")))
            .expect("start")
            .with_timezone(&chrono::Utc)
            .checked_sub_signed(chrono::Duration::days(1))
            .expect("start minus one");
    let end = start + chrono::Duration::days(2);
    let t0 = start.to_rfc3339();

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        format!(
            concat!(
                "{{\"id\":\"A\",\"title\":\"Closed no timestamp\",\"status\":\"closed\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{t0}\"}}\n",
                "{{\"id\":\"B\",\"title\":\"Open\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{t0}\"}}\n"
            ),
            t0 = t0
        ),
    )
    .expect("write beads");

    fs::write(
        repo_dir.join(".beads/sprints.jsonl"),
        format!(
            "{{\"id\":\"sprint-2\",\"name\":\"Sprint 2\",\"start_date\":\"{}\",\"end_date\":\"{}\",\"bead_ids\":[\"A\",\"B\"]}}\n",
            start.to_rfc3339(),
            end.to_rfc3339(),
        ),
    )
    .expect("write sprints");

    let payload = run_bvr_json_in_dir(&["--robot-burndown", "current"], repo_dir);
    assert_eq!(payload["completed_issues"], 1);

    let last = payload["daily_points"]
        .as_array()
        .expect("daily points")
        .last()
        .expect("last daily point")
        .clone();
    assert_eq!(last["completed"], 1);
    assert_eq!(last["remaining"], 1);
}

#[test]
fn robot_capacity_treats_tombstone_as_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"Open\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120}\n",
            "{\"id\":\"B\",\"title\":\"Archived\",\"status\":\"tombstone\",\"priority\":1,\"issue_type\":\"task\",\"estimated_minutes\":120}\n"
        ),
    )
    .expect("write beads");

    let payload = run_bvr_json_in_dir(&["--robot-capacity"], repo_dir);
    assert_eq!(payload["open_issue_count"], 1);
    assert_eq!(payload["actionable_count"], 1);
}

#[test]
fn robot_suggest_contract_and_hash_stability() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"A\",\"title\":\"Login OAuth bug\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"description\":\"OAuth login fails with 500 in auth handler\",\"labels\":[\"auth\"]}\n",
            "{\"id\":\"B\",\"title\":\"OAuth login failure\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"description\":\"Login via OAuth returns error; auth flow seems broken\",\"labels\":[\"auth\"]}\n",
            "{\"id\":\"cycle-a\",\"title\":\"Cycle A\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"cycle-b\",\"type\":\"blocks\"}]}\n",
            "{\"id\":\"cycle-b\",\"title\":\"Cycle B\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"cycle-a\",\"type\":\"blocks\"}]}\n"
        ),
    )
    .expect("write beads");

    let first = run_bvr_json_in_dir(&["--robot-suggest"], repo_dir);
    assert!(
        first["generated_at"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    assert!(first["data_hash"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(
        first["usage_hints"]
            .as_array()
            .is_some_and(|hints| !hints.is_empty())
    );

    let suggestions = first["suggestions"]["suggestions"]
        .as_array()
        .expect("suggestions array");
    let total = first["suggestions"]["stats"]["total"]
        .as_u64()
        .expect("stats total");
    assert_eq!(u64::try_from(suggestions.len()).unwrap_or(u64::MAX), total);

    let second = run_bvr_json_in_dir(&["--robot-suggest"], repo_dir);
    assert_eq!(first["data_hash"], second["data_hash"]);
}

#[test]
fn robot_suggest_filters_work() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"cycle-a\",\"title\":\"Cycle A\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"cycle-b\",\"type\":\"blocks\"}]}\n",
            "{\"id\":\"cycle-b\",\"title\":\"Cycle B\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"depends_on_id\":\"cycle-a\",\"type\":\"blocks\"}]}\n",
            "{\"id\":\"dep-1\",\"title\":\"Users database migration\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"description\":\"migration for users table and database schema\",\"labels\":[\"backend\"]}\n",
            "{\"id\":\"dep-2\",\"title\":\"Database users schema update\",\"status\":\"open\",\"priority\":3,\"issue_type\":\"task\",\"description\":\"users table migration and schema adjustments\",\"labels\":[\"backend\"]}\n"
        ),
    )
    .expect("write beads");

    let cycle_only = run_bvr_json_in_dir(&["--robot-suggest", "--suggest-type", "cycle"], repo_dir);
    let cycle_suggestions = cycle_only["suggestions"]["suggestions"]
        .as_array()
        .expect("cycle suggestions");
    assert!(!cycle_suggestions.is_empty());
    assert!(
        cycle_suggestions
            .iter()
            .all(|entry| entry["type"] == "cycle_warning")
    );

    let high_conf = run_bvr_json_in_dir(
        &["--robot-suggest", "--suggest-confidence", "0.9"],
        repo_dir,
    );
    let high_conf_suggestions = high_conf["suggestions"]["suggestions"]
        .as_array()
        .expect("high confidence suggestions");
    assert!(
        high_conf_suggestions
            .iter()
            .all(|entry| entry["confidence"].as_f64().unwrap_or_default() >= 0.9)
    );

    let bead_filtered =
        run_bvr_json_in_dir(&["--robot-suggest", "--suggest-bead", "cycle-a"], repo_dir);
    let bead_suggestions = bead_filtered["suggestions"]["suggestions"]
        .as_array()
        .expect("bead-filtered suggestions");
    assert!(!bead_suggestions.is_empty());
    assert!(
        bead_suggestions.iter().all(|entry| {
            entry["target_bead"] == "cycle-a" || entry["related_bead"] == "cycle-a"
        })
    );
}

#[test]
fn robot_triage_single_issue_returns_valid_output() {
    let actual = run_bvr_json(&["--robot-triage"], "tests/testdata/single_issue.jsonl");

    assert_eq!(actual["triage"]["quick_ref"]["total_open"], 1);
    assert_eq!(actual["triage"]["quick_ref"]["total_actionable"], 1);
    assert!(actual["triage"]["recommendations"].is_array());
    // Single issue with no deps should have exactly one recommendation.
    assert!(
        actual["triage"]["recommendations"]
            .as_array()
            .unwrap()
            .len()
            <= 1
    );
}

#[test]
fn robot_triage_all_closed_returns_zero_open() {
    let actual = run_bvr_json(&["--robot-triage"], "tests/testdata/all_closed.jsonl");

    assert_eq!(actual["triage"]["quick_ref"]["total_open"], 0);
    assert_eq!(actual["triage"]["quick_ref"]["total_actionable"], 0);
    // No open issues means no recommendations.
    assert!(
        actual["triage"]["recommendations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn robot_suggest_single_issue_has_no_duplicates() {
    let actual = run_bvr_json(&["--robot-suggest"], "tests/testdata/single_issue.jsonl");

    assert!(actual["suggestions"]["suggestions"].is_array());
    let suggestions = actual["suggestions"]["suggestions"].as_array().unwrap();
    // A single issue can't have duplicates.
    for suggestion in suggestions {
        assert_ne!(
            suggestion["type"].as_str().unwrap(),
            "potential_duplicate",
            "single issue should not produce duplicate suggestions"
        );
    }
}

#[test]
fn robot_plan_all_closed_has_empty_tracks() {
    let actual = run_bvr_json(&["--robot-plan"], "tests/testdata/all_closed.jsonl");

    assert!(actual["plan"]["tracks"].is_array());
    // With all issues closed, there are no actionable open tracks.
    let tracks = actual["plan"]["tracks"].as_array().unwrap();
    assert!(
        !tracks.iter().any(|track| track["issues"]
            .as_array()
            .is_some_and(|issues| !issues.is_empty())),
        "all-closed input should produce no non-empty tracks"
    );
}

#[test]
fn robot_insights_single_issue_returns_valid_metrics() {
    let actual = run_bvr_json(&["--robot-insights"], "tests/testdata/single_issue.jsonl");

    assert!(actual["insights"]["bottlenecks"].is_array());
    assert!(actual["insights"]["critical_path"].is_array());
    assert!(actual["insights"]["cycles"].is_array());
    // Single issue with no deps should have no cycles.
    assert!(actual["insights"]["cycles"].as_array().unwrap().is_empty());
}

#[test]
fn robot_history_single_issue_returns_valid_structure() {
    let actual = run_bvr_json(&["--robot-history"], "tests/testdata/single_issue.jsonl");

    assert_eq!(actual["stats"]["total_beads"], 1);
    assert!(actual["histories"].is_object());
    let histories = actual["histories"].as_object().unwrap();
    assert_eq!(histories.len(), 1);
    assert!(histories.contains_key("SOLO-1"));

    let entry = &histories["SOLO-1"];
    assert_eq!(entry["bead_id"], "SOLO-1");
    assert!(entry["events"].is_array());
    assert!(entry["milestones"].is_object());
}

#[test]
fn robot_forecast_all_closed_returns_empty_forecasts() {
    let actual = run_bvr_json(
        &["--robot-forecast", "all", "--forecast-agents", "1"],
        "tests/testdata/all_closed.jsonl",
    );

    assert_eq!(actual["forecast_count"], 0);
    assert!(actual["forecasts"].as_array().unwrap().is_empty());
}

#[test]
fn robot_graph_single_issue_returns_single_node() {
    let actual = run_bvr_json(
        &["--robot-graph", "--graph-format", "json"],
        "tests/testdata/single_issue.jsonl",
    );

    // Graph output has top-level "nodes" as a count, and "adjacency.nodes" as a list.
    assert_eq!(actual["nodes"], 1);
    assert_eq!(actual["edges"], 0);
}

#[test]
fn robot_suggest_rejects_invalid_type() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        "{\"id\":\"A\",\"title\":\"A\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write beads");

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(repo_dir);
    command.args(["--robot-suggest", "--suggest-type", "nope"]);
    command.assert().failure();
}

#[test]
fn robot_triage_empty_fixture_returns_zero_open_and_actionable() {
    let actual = run_bvr_json(&["--robot-triage"], "tests/testdata/empty.jsonl");

    assert_eq!(actual["triage"]["quick_ref"]["total_open"], 0);
    assert_eq!(actual["triage"]["quick_ref"]["total_actionable"], 0);
    assert!(
        actual["triage"]["recommendations"]
            .as_array()
            .expect("recommendations array")
            .is_empty()
    );
    assert!(
        actual["triage"]["quick_wins"]
            .as_array()
            .expect("quick_wins array")
            .is_empty()
    );
}

#[test]
fn robot_boundary_fixture_keeps_closed_items_out_of_recommendations() {
    let triage = run_bvr_json(
        &["--robot-triage"],
        "tests/testdata/boundary_conditions.jsonl",
    );

    assert_eq!(triage["triage"]["quick_ref"]["total_open"], 8);
    let recommendations = triage["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert!(recommendations.iter().any(|entry| entry["id"] == "BND-001"));
    assert!(
        recommendations
            .iter()
            .all(|entry| entry["id"] != "BND-008" && entry["id"] != "BND-009")
    );

    let priority = run_bvr_json(
        &["--robot-priority", "--robot-max-results", "10"],
        "tests/testdata/boundary_conditions.jsonl",
    );
    let priority_recommendations = priority["recommendations"]
        .as_array()
        .expect("priority recommendations");
    assert!(!priority_recommendations.is_empty());
    for rec in priority_recommendations {
        let score = rec["score"].as_f64().expect("score");
        let confidence = rec["confidence"].as_f64().expect("confidence");
        assert!((0.0..=1.0).contains(&score));
        assert!((0.0..=1.0).contains(&confidence));
    }
}

#[test]
fn robot_large_graph_fixture_reports_expected_graph_size() {
    let graph = run_bvr_json(
        &["--robot-graph", "--graph-format", "json"],
        "tests/testdata/large_graph_40.jsonl",
    );
    assert_eq!(graph["nodes"], 40);
    assert_eq!(graph["edges"], 39);

    let triage = run_bvr_json(
        &["--robot-triage", "--robot-max-results", "5"],
        "tests/testdata/large_graph_40.jsonl",
    );
    assert_eq!(triage["triage"]["quick_ref"]["total_open"], 40);
    assert_eq!(triage["triage"]["quick_ref"]["total_actionable"], 1);

    let recommendations = triage["triage"]["recommendations"]
        .as_array()
        .expect("recommendations array");
    assert!(!recommendations.is_empty());
    assert!(recommendations.len() <= 5);
    assert_eq!(recommendations[0]["id"], "LG-001");
}

#[test]
fn robot_adversarial_stress_fixture_surfaces_cycles_and_cascades() {
    let insights = run_bvr_json(
        &["--robot-insights"],
        "tests/testdata/adversarial_stress.jsonl",
    );
    let cycles = insights["insights"]["cycles"].as_array().expect("cycles");
    assert!(!cycles.is_empty());

    let alerts = run_bvr_json(
        &["--robot-alerts", "--alert-type", "blocking_cascade"],
        "tests/testdata/adversarial_stress.jsonl",
    );
    let cascade_alerts = alerts["alerts"].as_array().expect("alerts");
    assert!(!cascade_alerts.is_empty());
    assert!(
        cascade_alerts
            .iter()
            .all(|entry| entry["type"] == "blocking_cascade")
    );

    let suggest = run_bvr_json(
        &["--robot-suggest", "--suggest-type", "cycle"],
        "tests/testdata/adversarial_stress.jsonl",
    );
    let cycle_suggestions = suggest["suggestions"]["suggestions"]
        .as_array()
        .expect("cycle suggestions");
    assert!(!cycle_suggestions.is_empty());
    assert!(
        cycle_suggestions
            .iter()
            .all(|entry| entry["type"] == "cycle_warning")
    );
}

#[test]
fn robot_burndown_core_fields_match_legacy_fixture() {
    let fixture = load_fixture("tests/conformance/fixtures/go_outputs/bvr_extended.json");
    let expected = &fixture["burndown"];

    let root = repo_root();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let beads_dir = repo_dir.join(".beads");
    fs::create_dir_all(&beads_dir).expect("mkdir beads");

    let beads_data = fs::read_to_string(root.join("tests/testdata/synthetic_complex.jsonl"))
        .expect("read beads");
    fs::write(beads_dir.join("beads.jsonl"), beads_data).expect("write beads");

    let sprints_data = fs::read_to_string(root.join("tests/testdata/sprints_synthetic.jsonl"))
        .expect("read sprints");
    fs::write(beads_dir.join("sprints.jsonl"), sprints_data).expect("write sprints");

    let actual = run_bvr_json_in_dir(&["--robot-burndown", "sprint-1"], repo_dir);

    // Core scalar fields
    assert_eq!(actual["sprint_id"], expected["sprint_id"]);
    assert_eq!(actual["sprint_name"], expected["sprint_name"]);
    assert_eq!(actual["total_issues"], expected["total_issues"]);
    assert_eq!(actual["completed_issues"], expected["completed_issues"]);
    assert_eq!(actual["remaining_issues"], expected["remaining_issues"]);
    assert_eq!(actual["total_days"], expected["total_days"]);
    assert_eq!(actual["on_track"], expected["on_track"]);

    // Envelope metadata
    assert_eq!(
        actual["output_format"].as_str().expect("output_format"),
        "json"
    );
    assert!(
        actual["version"]
            .as_str()
            .expect("version")
            .starts_with('v')
    );

    // Burn rates should be close (both compute from same data)
    let actual_ideal = actual["ideal_burn_rate"].as_f64().expect("ideal_burn_rate");
    let expected_ideal = expected["ideal_burn_rate"]
        .as_f64()
        .expect("expected ideal");
    assert!(
        (actual_ideal - expected_ideal).abs() < 0.01,
        "ideal_burn_rate: actual={actual_ideal} expected={expected_ideal}"
    );

    // Array lengths
    let daily_len = actual["daily_points"]
        .as_array()
        .expect("daily_points")
        .len();
    let expected_daily_len = expected["daily_points"]
        .as_array()
        .expect("expected daily_points")
        .len();
    assert_eq!(daily_len, expected_daily_len);

    let ideal_len = actual["ideal_line"].as_array().expect("ideal_line").len();
    let expected_ideal_len = expected["ideal_line"]
        .as_array()
        .expect("expected ideal_line")
        .len();
    assert_eq!(ideal_len, expected_ideal_len);

    // Date fields should be present and parseable
    assert!(actual["start_date"].as_str().is_some());
    assert!(actual["end_date"].as_str().is_some());
    assert!(actual["generated_at"].as_str().is_some());
    assert!(actual["data_hash"].as_str().is_some());
}
