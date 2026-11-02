//! Stress tests exercising large-dataset and adversarial fixtures.
//!
//! Validates that bvr handles:
//! - 500-issue graphs with diverse topologies (chains, hubs, diamonds, cycles)
//! - Pathological dependency patterns (deep chains, convergence, divergence, overlapping cycles, self-deps)
//! - Malformed/edge-case metadata (empty strings, negative priority, unicode, huge estimates)
//!
//! Each fixture must load, parse, and produce valid JSON output without panics.

mod test_utils;

use assert_cmd::Command;
use serde_json::Value;

fn bvr() -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    Command::new(bvr_bin)
}

const LARGE_500: &str = "tests/testdata/stress_large_500.jsonl";
const PATHOLOGICAL: &str = "tests/testdata/pathological_deps.jsonl";
const MALFORMED: &str = "tests/testdata/malformed_metadata.jsonl";

/// Run a robot command against a fixture and return parsed JSON.
fn run_robot(args: &[&str], fixture: &str) -> Value {
    let output = bvr()
        .args(args)
        .arg("--beads-file")
        .arg(fixture)
        .output()
        .expect("failed to execute bvr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command {:?} against {fixture} failed (exit={}).\nstdout: {stdout}\nstderr: {stderr}",
        args,
        output.status.code().unwrap_or(-1)
    );

    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("Command {args:?} against {fixture} produced invalid JSON: {e}\nOutput: {stdout}")
    })
}

// =========================================================================
// stress_large_500.jsonl — 500-issue diverse topology graph
// =========================================================================

#[test]
fn stress_large_500_triage() {
    let json = run_robot(&["--robot-triage"], LARGE_500);
    let triage = &json["triage"];
    assert!(!triage["recommendations"].as_array().unwrap().is_empty());
    assert!(triage["quick_ref"]["total_open"].as_u64().unwrap() > 100);
}

#[test]
fn stress_large_500_insights() {
    let json = run_robot(&["--robot-insights"], LARGE_500);
    let insights = &json;
    // Should detect the 5 cycle groups
    let cycles = insights["Cycles"].as_array().unwrap();
    assert!(
        cycles.len() >= 5,
        "Expected at least 5 cycles, got {}",
        cycles.len()
    );
}

#[test]
fn stress_large_500_next() {
    let json = run_robot(&["--robot-next"], LARGE_500);
    assert!(json["id"].is_string());
    assert!(json["score"].is_f64() || json["score"].is_u64());
}

#[test]
fn stress_large_500_graph() {
    let json = run_robot(&["--robot-graph"], LARGE_500);
    assert_eq!(
        json["nodes"].as_u64().unwrap(),
        500,
        "Should have 500 graph nodes"
    );
    assert!(
        json["edges"].as_u64().unwrap() > 100,
        "Should have many edges"
    );
}

#[test]
fn stress_large_500_plan() {
    let json = run_robot(&["--robot-plan"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_suggest() {
    let json = run_robot(&["--robot-suggest"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_alerts() {
    let json = run_robot(&["--robot-alerts"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_history() {
    let json = run_robot(&["--robot-history"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_metrics() {
    let json = run_robot(&["--robot-metrics"], LARGE_500);
    assert!(json["data_hash"].is_string());
}

#[test]
fn stress_large_500_label_health() {
    let json = run_robot(&["--robot-label-health"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_label_flow() {
    let json = run_robot(&["--robot-label-flow"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_label_attention() {
    let json = run_robot(&["--robot-label-attention"], LARGE_500);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_large_500_search() {
    let json = run_robot(&["--robot-search", "--search", "chain"], LARGE_500);
    let results = json["results"].as_array().unwrap();
    assert!(!results.is_empty(), "Should find chain issues");
}

#[test]
fn stress_large_500_diff() {
    let json = run_robot(
        &[
            "--robot-diff",
            "--diff-since",
            "tests/testdata/minimal.jsonl",
        ],
        LARGE_500,
    );
    assert!(json["diff"].is_object());
}

#[test]
fn stress_large_500_forecast() {
    let json = run_robot(&["--robot-forecast", "7"], LARGE_500);
    assert!(json["forecasts"].is_array());
}

#[test]
fn stress_large_500_capacity() {
    let json = run_robot(&["--robot-capacity", "--agents", "3"], LARGE_500);
    assert!(json["total_minutes"].is_number());
}

/// Each layer shares both prerequisites from the previous layer. There are only
/// 53 issues, but enumerating every root-to-leaf path visits over 67 million paths.
fn write_shared_prerequisites(dir: &std::path::Path) -> std::path::PathBuf {
    use std::io::Write;

    let path = dir.join("shared-prerequisites.jsonl");
    let mut file = std::fs::File::create(&path).expect("create shared-prerequisite fixture");
    let mut previous = Vec::<String>::new();
    for layer in 0..=26 {
        let ids = if layer == 0 {
            vec!["L00-root".to_string()]
        } else {
            vec![format!("L{layer:02}-a"), format!("L{layer:02}-b")]
        };
        for id in &ids {
            let issue = serde_json::json!({
                "id": id,
                "title": id,
                "status": "open",
                "issue_type": "task",
                "priority": 2,
                "estimated_minutes": 60,
                "dependencies": previous.iter().map(|blocker| serde_json::json!({
                    "issue_id": id,
                    "depends_on_id": blocker,
                    "type": "blocks"
                })).collect::<Vec<_>>()
            });
            writeln!(file, "{issue}").expect("write shared-prerequisite issue");
        }
        previous = ids;
    }
    path
}

fn run_shared_prerequisite_robot(args: &[&str], fixture: &std::path::Path) -> Value {
    let output = bvr()
        .args(args)
        .arg("--beads-file")
        .arg(fixture)
        // This is a runaway-traversal guard, not a microbenchmark. The fixture
        // is deliberately small enough that ordinary CI has ample headroom.
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).expect("shared-prerequisite robot JSON")
}

#[test]
fn stress_shared_prerequisites_forecast_completes_with_exact_dependency_waits() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let path = write_shared_prerequisites(dir.path());
    let all = run_shared_prerequisite_robot(&["--robot-forecast", "all"], &path);
    assert_eq!(all["forecast_count"], 53);

    // All issues in a layer have the same own effort and completion date. With
    // no historical closures the velocity is 60 / 5 = 12 minutes per day.
    // Parallel prerequisites contribute their maximum wait, never their sum.
    let mut expected_days = 0.0;
    let mut total_minutes = 0_i64;
    for layer in 0..=26 {
        let depth = 27 - layer;
        let own_minutes = (60.0 * (1.0 + (f64::from(depth) / 10.0).min(1.0))) as i32;
        expected_days += f64::from(own_minutes) / 12.0;
        for suffix in if layer == 0 {
            &["root"][..]
        } else {
            &["a", "b"][..]
        } {
            let id = format!("L{layer:02}-{suffix}");
            let item = all["forecasts"]
                .as_array()
                .expect("forecast array")
                .iter()
                .find(|item| item["issue_id"] == id)
                .expect("forecast for each issue");
            assert_eq!(item["estimated_minutes"], own_minutes, "{id}");
            assert!(
                (item["estimated_days"].as_f64().unwrap() - expected_days).abs() < 1e-9,
                "dependency wait for {id}: {item}"
            );
            assert_eq!(item["velocity_minutes_per_day"], 12.0);
            total_minutes += i64::from(own_minutes);
        }
    }
    assert_eq!(all["summary"]["total_minutes"], total_minutes);

    let single = run_shared_prerequisite_robot(&["--robot-forecast", "L26-b"], &path);
    assert_eq!(single["forecast_count"], 1);
    let terminal = all["forecasts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["issue_id"] == "L26-b")
        .unwrap();
    for field in [
        "estimated_minutes",
        "estimated_days",
        "confidence",
        "velocity_minutes_per_day",
        "agents",
        "factors",
    ] {
        assert_eq!(single["forecasts"][0][field], terminal[field], "{field}");
    }
}

#[test]
fn stress_shared_prerequisites_capacity_completes_with_exact_critical_path() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let path = write_shared_prerequisites(dir.path());
    let capacity = run_shared_prerequisite_robot(&["--robot-capacity", "--agents", "3"], &path);
    let expected_path = std::iter::once("L00-root".to_string())
        .chain((1..=26).map(|layer| format!("L{layer:02}-a")))
        .collect::<Vec<_>>();
    assert_eq!(capacity["open_issue_count"], 53);
    assert_eq!(capacity["critical_path_length"], 27);
    assert_eq!(capacity["critical_path"], serde_json::json!(expected_path));
    assert_eq!(capacity["actionable"], serde_json::json!(["L00-root"]));

    let mut serial_minutes = 0_i32;
    for depth in 1..=27 {
        serial_minutes += (60.0 * (1.0 + (f64::from(depth) / 10.0).min(1.0))) as i32;
    }
    let total_minutes = serial_minutes * 2 - 120;
    let parallel_minutes = total_minutes - serial_minutes;
    assert_eq!(capacity["serial_minutes"], serial_minutes);
    assert_eq!(capacity["total_minutes"], total_minutes);
    assert_eq!(capacity["parallel_minutes"], parallel_minutes);
    assert_eq!(
        capacity["estimated_days"],
        f64::from(serial_minutes + parallel_minutes / 3) / 480.0
    );
}

#[test]
fn stress_large_500_profile() {
    let json = run_robot(&["--profile-startup", "--profile-json"], LARGE_500);
    let profile = &json["profile"];
    assert_eq!(profile["node_count"].as_u64().unwrap(), 500);
}

// =========================================================================
// pathological_deps.jsonl — Extreme dependency patterns
// =========================================================================

#[test]
fn stress_pathological_triage() {
    let json = run_robot(&["--robot-triage"], PATHOLOGICAL);
    assert!(json["triage"]["recommendations"].is_array());
}

#[test]
fn stress_pathological_insights_detects_cycles() {
    let json = run_robot(&["--robot-insights"], PATHOLOGICAL);
    let insights = &json;
    let cycles = insights["Cycles"].as_array().unwrap();
    // Overlapping cycles + bidirectional + long cycle + self-dep = many cycles expected
    assert!(
        cycles.len() >= 3,
        "Expected at least 3 cycles in pathological fixture, got {}",
        cycles.len()
    );
}

#[test]
fn stress_pathological_graph_deep_chain() {
    let json = run_robot(&["--robot-graph"], PATHOLOGICAL);
    let node_count = json["nodes"].as_u64().unwrap();
    assert!(node_count >= 200, "Expected 200+ nodes, got {node_count}");
}

#[test]
fn stress_pathological_bottleneck_convergence() {
    // The convergence sink (PD-150) depends on 50 issues — should be flagged
    let json = run_robot(&["--robot-insights"], PATHOLOGICAL);
    let insights = &json;
    let bottlenecks = insights["Bottlenecks"].as_array().unwrap();
    assert!(
        !bottlenecks.is_empty(),
        "Convergence bottleneck should be detected"
    );
}

#[test]
fn stress_pathological_suggest() {
    let json = run_robot(&["--robot-suggest"], PATHOLOGICAL);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_pathological_alerts() {
    let json = run_robot(&["--robot-alerts"], PATHOLOGICAL);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_pathological_search_cycle() {
    let json = run_robot(&["--robot-search", "--search", "cycle"], PATHOLOGICAL);
    let results = json["results"].as_array().unwrap();
    assert!(!results.is_empty(), "Should find cycle-related issues");
}

#[test]
fn stress_pathological_dangling_dep() {
    // PD-250 depends on non-existent GHOST-999 — should not crash
    let json = run_robot(&["--robot-triage"], PATHOLOGICAL);
    assert!(json["data_hash"].is_string());
}

#[test]
fn stress_pathological_self_dep() {
    // PD-210 depends on itself — should not crash
    let json = run_robot(&["--robot-graph"], PATHOLOGICAL);
    assert!(json["nodes"].is_u64());
}

// =========================================================================
// malformed_metadata.jsonl — Edge-case metadata values
// =========================================================================

#[test]
fn stress_malformed_triage() {
    let json = run_robot(&["--robot-triage"], MALFORMED);
    assert!(json["data_hash"].is_string());
}

#[test]
fn stress_malformed_insights() {
    let json = run_robot(&["--robot-insights"], MALFORMED);
    assert!(json["data_hash"].is_string());
}

#[test]
fn stress_malformed_graph() {
    let json = run_robot(&["--robot-graph"], MALFORMED);
    let node_count = json["nodes"].as_u64().unwrap();
    // Some malformed issues may be filtered; accept any count > 0
    assert!(node_count > 0, "Should have some graph nodes");
}

#[test]
fn stress_malformed_search_unicode() {
    let json = run_robot(&["--robot-search", "--search", "Ünïcödé"], MALFORMED);
    let results = json["results"].as_array().unwrap();
    assert!(!results.is_empty(), "Should find unicode-titled issues");
}

#[test]
fn stress_malformed_suggest() {
    let json = run_robot(&["--robot-suggest"], MALFORMED);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_malformed_alerts() {
    let json = run_robot(&["--robot-alerts"], MALFORMED);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_malformed_history() {
    let json = run_robot(&["--robot-history"], MALFORMED);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_malformed_label_health() {
    let json = run_robot(&["--robot-label-health"], MALFORMED);
    assert!(json["generated_at"].is_string());
}

#[test]
fn stress_malformed_profile() {
    let json = run_robot(&["--profile-startup", "--profile-json"], MALFORMED);
    let profile = &json["profile"];
    // Some malformed issues may not produce graph nodes
    assert!(profile["node_count"].as_u64().unwrap() > 0);
}
