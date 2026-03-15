//! End-to-end robot command matrix tests.
//!
//! Runs every robot command against test fixtures and validates:
//! - Exit code is 0
//! - Output is valid JSON
//! - Envelope fields are present (`generated_at`, `data_hash`)
//! - Command-specific payload fields exist
//!
//! Generates per-scenario diagnostics on failure.

mod test_utils;

use assert_cmd::Command;
use serde_json::Value;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use test_utils::{JsonType, assert_valid_envelope, validate_fields, validate_type_at};
use toon::toon_to_json;

fn bvr() -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    Command::new(bvr_bin)
}

const FIXTURE: &str = "tests/testdata/minimal.jsonl";
const COMPLEX_FIXTURE: &str = "tests/testdata/synthetic_complex.jsonl";
const E2E_ARTIFACT_DIR_ENV: &str = "BVR_E2E_ARTIFACT_DIR";

fn sanitize_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "scenario".to_string()
    } else {
        trimmed.to_string()
    }
}

fn shell_quote(token: &str) -> String {
    if token.is_empty() {
        return "''".to_string();
    }
    if token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        token.to_string()
    } else {
        format!("'{}'", token.replace('\'', "'\"'\"'"))
    }
}

fn args_fingerprint(args: &[String], fixture: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    args.hash(&mut hasher);
    fixture.hash(&mut hasher);
    hasher.finish()
}

fn write_artifact_bundle(
    root: &Path,
    scenario: &str,
    args: &[String],
    fixture: &str,
    output: &Output,
) {
    let scenario = sanitize_component(scenario);
    let fingerprint = args_fingerprint(args, fixture);
    let bundle_dir = root.join(format!("{scenario}-{fingerprint:016x}"));
    if let Err(error) = fs::create_dir_all(&bundle_dir) {
        eprintln!(
            "warning: could not create e2e artifact dir {}: {error}",
            bundle_dir.display()
        );
        return;
    }

    let args_rendered = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<String>>()
        .join(" ");
    let replay = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\ncargo run -- {args_rendered} --beads-file {}\n",
        shell_quote(fixture)
    );
    let meta = json!({
        "scenario": scenario,
        "fixture": fixture,
        "args": args,
        "exit_code": output.status.code(),
        "success": output.status.success(),
    });

    if let Err(error) = fs::write(bundle_dir.join("command.sh"), replay) {
        eprintln!("warning: could not write replay script: {error}");
    }
    if let Err(error) = fs::write(bundle_dir.join("stdout.txt"), &output.stdout) {
        eprintln!("warning: could not write stdout artifact: {error}");
    }
    if let Err(error) = fs::write(bundle_dir.join("stderr.txt"), &output.stderr) {
        eprintln!("warning: could not write stderr artifact: {error}");
    }
    if let Err(error) = fs::write(
        bundle_dir.join("meta.json"),
        serde_json::to_vec_pretty(&meta).unwrap_or_default(),
    ) {
        eprintln!("warning: could not write metadata artifact: {error}");
    }
}

fn maybe_write_artifact_bundle(scenario: &str, args: &[String], fixture: &str, output: &Output) {
    let Some(root) = std::env::var_os(E2E_ARTIFACT_DIR_ENV).map(PathBuf::from) else {
        return;
    };
    write_artifact_bundle(&root, scenario, args, fixture, output);
}

fn run_bvr_with_artifacts(args: &[String], fixture: &str, scenario: &str) -> Output {
    let output = bvr()
        .args(args)
        .arg("--beads-file")
        .arg(fixture)
        .output()
        .expect("failed to execute bvr");
    maybe_write_artifact_bundle(scenario, args, fixture, &output);
    output
}

/// Helper: run a robot command and return parsed JSON output.
fn run_robot(args: &[&str], fixture: &str) -> Value {
    let args_owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let scenario = format!(
        "robot-{}",
        args.first()
            .copied()
            .unwrap_or("unknown")
            .trim_start_matches("--")
    );
    let output = run_bvr_with_artifacts(&args_owned, fixture, &scenario);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Command {:?} failed (exit={}).\nstdout: {stdout}\nstderr: {stderr}",
        args,
        output.status.code().unwrap_or(-1)
    );

    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Command {args:?} produced invalid JSON: {e}\nOutput: {stdout}"))
}

fn run_robot_raw(args: &[&str], fixture: &str) -> Output {
    bvr()
        .args(args)
        .arg("--beads-file")
        .arg(fixture)
        .output()
        .expect("failed to execute bvr")
}

fn decode_toon(stdout: &[u8]) -> Value {
    let toon = std::str::from_utf8(stdout).expect("utf8 toon payload");
    let json = toon_to_json(toon).expect("decode toon payload");
    serde_json::from_str(&json).expect("decoded toon json")
}

#[test]
fn artifact_bundle_writes_replay_stdout_stderr_and_meta() {
    let temp = tempfile::tempdir().expect("tempdir");
    let args = vec![
        "--robot-triage".to_string(),
        "--robot-max-results".to_string(),
        "1".to_string(),
    ];
    let fixture = "tests/testdata/minimal.jsonl";
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            "printf 'stdout payload'; printf 'stderr payload' 1>&2",
        ])
        .output()
        .expect("command output");

    write_artifact_bundle(temp.path(), "artifact smoke", &args, fixture, &output);

    let mut command_file_count = 0usize;
    for entry in fs::read_dir(temp.path()).expect("artifact root listing") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        assert!(path.join("command.sh").exists());
        assert!(path.join("stdout.txt").exists());
        assert!(path.join("stderr.txt").exists());
        assert!(path.join("meta.json").exists());

        let replay = fs::read_to_string(path.join("command.sh")).expect("command script");
        assert!(replay.contains("cargo run --"));
        assert!(replay.contains("--robot-triage"));

        let stdout = fs::read_to_string(path.join("stdout.txt")).expect("stdout file");
        let stderr = fs::read_to_string(path.join("stderr.txt")).expect("stderr file");
        assert!(stdout.contains("stdout payload"));
        assert!(stderr.contains("stderr payload"));

        let meta = fs::read_to_string(path.join("meta.json")).expect("meta file");
        let value: Value = serde_json::from_str(&meta).expect("meta json");
        assert_eq!(value["fixture"], fixture);
        assert_eq!(value["success"], true);
        command_file_count += 1;
    }

    assert_eq!(
        command_file_count, 1,
        "expected one artifact bundle directory"
    );
}

// =========================================================================
// Robot Command Matrix
// =========================================================================

#[test]
fn e2e_robot_triage() {
    let json = run_robot(&["--robot-triage"], FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["triage"], "").is_empty());
    assert!(validate_type_at(&json, "triage.recommendations", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_next() {
    let json = run_robot(&["--robot-next"], FIXTURE);
    // robot-next has flat top-level fields, not a nested "recommendation" wrapper
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&json, &["id", "title", "score"], "").is_empty());
}

#[test]
fn e2e_robot_next_toon_round_trips_via_library_decoder() {
    let output = run_robot_raw(&["--robot-next", "--format", "toon"], FIXTURE);
    assert!(
        output.status.success(),
        "toon command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim_start().starts_with('{'),
        "expected TOON output, got JSON-like payload: {stdout}"
    );

    let decoded = decode_toon(&output.stdout);
    assert!(validate_fields(&decoded, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&decoded, &["id", "title", "score"], "").is_empty());
}

#[test]
fn e2e_robot_next_honors_output_format_env_and_cli_override() {
    let env_output = bvr()
        .env("BV_OUTPUT_FORMAT", "toon")
        .args(["--robot-next", "--beads-file", FIXTURE])
        .output()
        .expect("run env-driven toon");
    assert!(
        env_output.status.success(),
        "env-driven toon failed: {}",
        String::from_utf8_lossy(&env_output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&env_output.stdout)
            .trim_start()
            .starts_with('{'),
        "expected BV_OUTPUT_FORMAT=toon to emit TOON"
    );
    let decoded_env = decode_toon(&env_output.stdout);
    assert!(validate_fields(&decoded_env, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&decoded_env, &["id", "title", "score"], "").is_empty());

    let override_output = bvr()
        .env("BV_OUTPUT_FORMAT", "toon")
        .args(["--robot-next", "--format", "json", "--beads-file", FIXTURE])
        .output()
        .expect("run explicit json override");
    assert!(
        override_output.status.success(),
        "json override failed: {}",
        String::from_utf8_lossy(&override_output.stderr)
    );
    let override_json: Value =
        serde_json::from_slice(&override_output.stdout).expect("override json payload");
    assert!(validate_fields(&override_json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&override_json, &["id", "title", "score"], "").is_empty());
}

#[test]
fn e2e_robot_next_toon_stats_can_be_enabled_via_env() {
    let output = bvr()
        .env("TOON_STATS", "1")
        .args(["--robot-next", "--format", "toon", "--beads-file", FIXTURE])
        .output()
        .expect("run toon stats env");
    assert!(
        output.status.success(),
        "toon stats env failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("TOON"));
    assert!(stderr.contains("savings"));
}

#[test]
fn e2e_robot_next_toon_does_not_require_external_tru_binary() {
    let output = bvr()
        .env("PATH", "")
        .args(["--robot-next", "--format", "toon", "--beads-file", FIXTURE])
        .output()
        .expect("run toon without PATH");
    assert!(
        output.status.success(),
        "toon without PATH failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let decoded = decode_toon(&output.stdout);
    assert!(validate_fields(&decoded, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&decoded, &["id", "title", "score"], "").is_empty());
}

#[test]
fn e2e_robot_plan() {
    let json = run_robot(&["--robot-plan"], FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["plan"], "").is_empty());
}

#[test]
fn e2e_robot_insights() {
    let json = run_robot(&["--robot-insights"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["insights"], "").is_empty());
    assert!(validate_type_at(&json, "insights.bottlenecks", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_priority() {
    let json = run_robot(&["--robot-priority"], FIXTURE);
    // robot-priority has flat top-level fields
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&json, &["recommendations", "status", "summary"], "").is_empty());
    assert!(validate_type_at(&json, "recommendations", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_graph_json() {
    let json = run_robot(&["--robot-graph"], FIXTURE);
    // robot-graph has data_hash but no generated_at; has nodes, edges, format
    assert!(validate_fields(&json, &["data_hash", "nodes", "edges", "format"], "").is_empty());
}

#[test]
fn e2e_robot_graph_dot() {
    let output = bvr()
        .args(["--robot-graph", "--graph-format", "dot"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("digraph"),
        "DOT output should contain 'digraph'"
    );
}

#[test]
fn e2e_robot_graph_mermaid() {
    let output = bvr()
        .args(["--robot-graph", "--graph-format", "mermaid"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("graph"),
        "Mermaid output should contain 'graph'"
    );
}

#[test]
fn e2e_robot_diff() {
    // robot-diff requires --diff-since; uses from_data_hash/to_data_hash instead of data_hash
    let json = run_robot(
        &[
            "--robot-diff",
            "--diff-since",
            "tests/testdata/all_closed.jsonl",
        ],
        FIXTURE,
    );
    assert!(
        validate_fields(
            &json,
            &["generated_at", "from_data_hash", "to_data_hash", "diff"],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_suggest() {
    let json = run_robot(&["--robot-suggest"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["suggestions"], "").is_empty());
}

#[test]
fn e2e_robot_alerts() {
    let json = run_robot(&["--robot-alerts"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["alerts"], "").is_empty());
}

#[test]
fn e2e_robot_history() {
    let json = run_robot(&["--robot-history"], FIXTURE);
    // robot-history has flat top-level fields
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(
        validate_fields(
            &json,
            &["histories", "commit_index", "stats", "git_range"],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_forecast() {
    // robot-forecast requires a days argument
    let json = run_robot(&["--robot-forecast", "7"], FIXTURE);
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&json, &["forecasts", "forecast_count", "agents"], "").is_empty());
    assert!(validate_type_at(&json, "forecasts", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_capacity() {
    let json = run_robot(&["--robot-capacity"], FIXTURE);
    // robot-capacity has flat top-level fields
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&json, &["total_minutes", "estimated_days", "agents"], "").is_empty());
}

#[test]
fn e2e_robot_burndown() {
    // robot-burndown requires a sprint id; standard fixtures lack sprint data.
    // Validate the command correctly errors for a nonexistent sprint.
    let output = bvr()
        .args(["--robot-burndown", "nonexistent"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(
        !output.status.success(),
        "burndown should fail for nonexistent sprint"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("sprint not found")
            || combined.contains("invalid argument")
            || combined.contains("not found"),
        "output should mention error: stderr={stderr} stdout={stdout}"
    );
}

#[test]
fn e2e_robot_metrics() {
    let json = run_robot(&["--robot-metrics"], FIXTURE);
    assert!(validate_fields(&json, &["timing"], "").is_empty());
}

#[test]
fn e2e_robot_search() {
    let json = run_robot(
        &["--robot-search", "--search", "parity", "--search-limit", "5"],
        COMPLEX_FIXTURE,
    );
    assert_valid_envelope(&json);
    assert!(
        validate_fields(&json, &["query", "limit", "mode", "results"], "").is_empty()
    );
    assert!(validate_type_at(&json, "results", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_label_health() {
    let json = run_robot(&["--robot-label-health"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["result"], "").is_empty());
}

#[test]
fn e2e_robot_label_flow() {
    let json = run_robot(&["--robot-label-flow"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["flow"], "").is_empty());
}

#[test]
fn e2e_robot_label_attention() {
    let json = run_robot(
        &["--robot-label-attention", "--attention-limit", "3"],
        COMPLEX_FIXTURE,
    );
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["limit", "result"], "").is_empty());
}

#[test]
fn e2e_robot_recipes() {
    let json = run_robot(&["--robot-recipes"], FIXTURE);
    assert!(
        validate_fields(&json, &["generated_at", "output_format", "version"], "").is_empty()
    );
    assert!(validate_fields(&json, &["recipes"], "").is_empty());
    assert!(validate_type_at(&json, "recipes", JsonType::Array).is_empty());
}

#[test]
fn e2e_profile_startup_human() {
    let output = bvr()
        .args(["--profile-startup"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Startup Profile"),
        "human profile should contain header"
    );
    assert!(
        stdout.contains("Load JSONL"),
        "human profile should contain timing phases"
    );
}

#[test]
fn e2e_profile_startup_json() {
    let json = run_robot(&["--profile-startup", "--profile-json"], FIXTURE);
    assert!(validate_fields(&json, &["generated_at", "data_hash", "profile"], "").is_empty());
    let profile = &json["profile"];
    assert!(
        validate_fields(
            profile,
            &[
                "node_count",
                "edge_count",
                "density",
                "load_jsonl",
                "build_graph",
                "total"
            ],
            "profile"
        )
        .is_empty()
    );
    assert!(validate_type_at(&json, "recommendations", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_help() {
    let output = bvr()
        .arg("--robot-help")
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "robot-help should produce output");
}

#[test]
fn e2e_robot_docs() {
    // robot-docs requires a valid topic argument
    let json = run_robot(&["--robot-docs", "guide"], FIXTURE);
    assert!(validate_fields(&json, &["generated_at", "topic", "guide"], "").is_empty());
}

#[test]
fn e2e_robot_schema() {
    let json = run_robot(&["--robot-schema"], FIXTURE);
    assert!(validate_fields(&json, &["schema_version", "commands"], "").is_empty());
}

// =========================================================================
// Debug-render TUI e2e
// =========================================================================

#[test]
fn e2e_debug_render_all_views() {
    for view in ["main", "board", "insights", "graph", "history"] {
        let args = vec![
            "--debug-render".to_string(),
            view.to_string(),
            "--debug-width".to_string(),
            "100".to_string(),
            "--debug-height".to_string(),
            "30".to_string(),
        ];
        let output = run_bvr_with_artifacts(
            &args,
            FIXTURE,
            &format!("debug-render-view-{view}-w100-h30"),
        );
        assert!(
            output.status.success(),
            "debug-render {view} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.trim().is_empty(),
            "debug-render {view} should produce output"
        );
    }
}

#[test]
fn e2e_debug_render_narrow_and_wide() {
    for width in [40, 80, 120, 180] {
        let args = vec![
            "--debug-render".to_string(),
            "main".to_string(),
            "--debug-width".to_string(),
            width.to_string(),
            "--debug-height".to_string(),
            "20".to_string(),
        ];
        let output =
            run_bvr_with_artifacts(&args, FIXTURE, &format!("debug-render-main-width-{width}"));
        assert!(
            output.status.success(),
            "debug-render at width {width} failed"
        );
    }
}

// =========================================================================
// Export e2e
// =========================================================================

#[test]
fn e2e_export_md() {
    let tmp = tempfile::tempdir().unwrap();
    let md_path = tmp.path().join("export.md");
    let output = bvr()
        .args(["--export-md", md_path.to_str().unwrap(), "--no-hooks"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    assert!(md_path.exists(), "export-md should create the file");
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("# "),
        "md export should contain markdown headers"
    );
}

#[test]
fn e2e_priority_brief() {
    let tmp = tempfile::tempdir().unwrap();
    let brief_path = tmp.path().join("brief.md");
    let output = bvr()
        .args(["--priority-brief", brief_path.to_str().unwrap()])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    assert!(brief_path.exists(), "priority-brief should create the file");
    let content = std::fs::read_to_string(&brief_path).unwrap();
    assert!(content.contains("Priority Brief"));
}

#[test]
fn e2e_agent_brief() {
    let tmp = tempfile::tempdir().unwrap();
    let output = bvr()
        .args(["--agent-brief", tmp.path().to_str().unwrap()])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    assert!(tmp.path().join("brief.md").exists());
    assert!(tmp.path().join("triage.json").exists());
    assert!(tmp.path().join("meta.json").exists());
}

// =========================================================================
// Error handling e2e
// =========================================================================

#[test]
fn e2e_missing_beads_file_errors_gracefully() {
    let output = bvr()
        .arg("--robot-triage")
        .arg("--beads-file")
        .arg("/nonexistent/path.jsonl")
        .output()
        .expect("failed to execute");
    assert!(!output.status.success(), "should fail for missing file");
}

#[test]
fn e2e_debug_render_unknown_view_errors() {
    let output = bvr()
        .args(["--debug-render", "nonexistent"])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(!output.status.success());
}

// =========================================================================
// Cross-fixture consistency
// =========================================================================

#[test]
fn e2e_all_fixtures_produce_valid_triage() {
    let fixtures = [
        "tests/testdata/minimal.jsonl",
        "tests/testdata/single_issue.jsonl",
        "tests/testdata/all_closed.jsonl",
        "tests/testdata/synthetic_complex.jsonl",
    ];
    for fixture in &fixtures {
        let json = run_robot(&["--robot-triage"], fixture);
        assert_valid_envelope(&json);
        assert!(
            validate_fields(&json, &["triage"], "").is_empty(),
            "fixture {fixture} failed triage validation"
        );
    }
}

#[test]
fn e2e_empty_fixture_returns_zero_open() {
    let json = run_robot(&["--robot-triage"], "tests/testdata/empty.jsonl");
    assert_valid_envelope(&json);
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(total, 0, "empty fixture should have 0 open issues");
}
