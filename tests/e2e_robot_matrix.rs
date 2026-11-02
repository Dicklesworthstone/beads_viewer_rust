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
    let binary_path =
        std::env::var("CARGO_BIN_EXE_bvr").unwrap_or_else(|_| "target/debug/bvr".into());
    let fixture_snapshot_name = Path::new(fixture)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| "fixture.jsonl".to_string(), ToString::to_string);
    let fixture_snapshot_path = bundle_dir.join(&fixture_snapshot_name);
    let replay_fixture = if fs::copy(fixture, &fixture_snapshot_path).is_ok() {
        format!("$(dirname \"$0\")/{fixture_snapshot_name}")
    } else {
        shell_quote(fixture)
    };
    let replay = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nBVR_BIN=\"${{BVR_BIN:-{}}}\"\n\"$BVR_BIN\" {args_rendered} --beads-file {replay_fixture}\n",
        shell_quote(&binary_path),
    );
    let meta = json!({
        "scenario": scenario,
        "fixture": fixture,
        "args": args,
        "args_rendered": args_rendered,
        "cwd": std::env::current_dir().ok().map(|path| path.display().to_string()),
        "binary_path": binary_path,
        "exit_code": output.status.code(),
        "success": output.status.success(),
        "stdout_bytes": output.stdout.len(),
        "stderr_bytes": output.stderr.len(),
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
        assert!(replay.contains("BVR_BIN="));
        assert!(replay.contains("--robot-triage"));
        assert!(path.join("minimal.jsonl").exists());

        let stdout = fs::read_to_string(path.join("stdout.txt")).expect("stdout file");
        let stderr = fs::read_to_string(path.join("stderr.txt")).expect("stderr file");
        assert!(stdout.contains("stdout payload"));
        assert!(stderr.contains("stderr payload"));

        let meta = fs::read_to_string(path.join("meta.json")).expect("meta file");
        let value: Value = serde_json::from_str(&meta).expect("meta json");
        assert_eq!(value["fixture"], fixture);
        assert_eq!(value["success"], true);
        assert!(value["binary_path"].is_string());
        assert!(value["stdout_bytes"].is_number());
        assert!(value["stderr_bytes"].is_number());
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
    assert_eq!(
        json["triage"]["commands"]["refresh_triage"],
        "bvr --robot-triage"
    );
}

#[test]
fn e2e_robot_next() {
    let json = run_robot(&["--robot-next"], FIXTURE);
    // robot-next has flat top-level fields, not a nested "recommendation" wrapper
    assert!(validate_fields(&json, &["generated_at", "data_hash"], "").is_empty());
    assert!(validate_fields(&json, &["id", "title", "score"], "").is_empty());
}

#[test]
fn e2e_robot_overview() {
    let json = run_robot(&["--robot-overview"], FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["summary", "commands"], "").is_empty());
    assert!(
        validate_type_at(&json, "summary.open_issues", JsonType::Number).is_empty(),
        "overview summary should expose numeric counts: {json}"
    );
    assert_eq!(json["commands"]["next"], "bvr --robot-next");
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
    assert!(validate_fields(&json, &["Bottlenecks"], "").is_empty());
    assert!(validate_type_at(&json, "Bottlenecks", JsonType::Array).is_empty());
}

mod advanced_planning {
    use super::{Value, fs, json, run_robot};

    #[derive(Clone)]
    struct Fixture {
        ids: Vec<String>,
        closed: Vec<bool>,
        edges: Vec<Vec<bool>>,
        nonblocking: Vec<(usize, usize, &'static str)>,
    }

    impl Fixture {
        fn new(ids: &[&str], edges: &[(usize, usize)]) -> Self {
            let mut graph = Self {
                ids: ids.iter().map(|id| (*id).to_string()).collect(),
                closed: vec![false; ids.len()],
                edges: vec![vec![false; ids.len()]; ids.len()],
                nonblocking: Vec::new(),
            };
            for &(from, to) in edges {
                graph.edges[from][to] = true;
            }
            graph
        }

        fn run(&self, reverse: bool) -> Value {
            let temp = tempfile::tempdir().expect("advanced planning fixture directory");
            let path = temp.path().join("issues.jsonl");
            let mut records = Vec::new();
            for (to, id) in self.ids.iter().enumerate() {
                let mut dependencies = Vec::new();
                for (from, blocker) in self.ids.iter().enumerate() {
                    if self.edges[from][to] {
                        // Every accepted blocking type must retain the same
                        // direction: the suggested `from` blocks `to`.
                        let kind =
                            ["blocks", "waits-for", "conditional-blocks", ""][(from + to) % 4];
                        dependencies.push(json!({
                            "issue_id": id, "depends_on_id": blocker, "type": kind
                        }));
                    }
                }
                for &(from, target, kind) in &self.nonblocking {
                    if target == to {
                        dependencies.push(json!({
                            "issue_id": id, "depends_on_id": self.ids[from], "type": kind
                        }));
                    }
                }
                if reverse {
                    dependencies.reverse();
                }
                records.push(json!({
                    "id": id,
                    "title": id,
                    "status": if self.closed[to] { "closed" } else { "open" },
                    "issue_type": "task",
                    "priority": 2,
                    "dependencies": dependencies
                }));
            }
            if reverse {
                records.reverse();
            }
            let contents = records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(&path, format!("{contents}\n")).expect("write advanced planning fixture");
            let output = run_robot(&["--robot-insights"], path.to_str().expect("fixture path"));
            output["advanced_insights"].clone()
        }

        // These small-fixture oracles deliberately use matrix transitive
        // closure and exhaustive DAG paths, independent of production SCC and
        // topological algorithms. No reflexive edges are added: a true diagonal
        // means the input actually contains a cycle through that vertex.
        fn reachability(&self, open_only: bool) -> Vec<Vec<bool>> {
            let mut reachable = self.edges.clone();
            for from in 0..self.ids.len() {
                for to in 0..self.ids.len() {
                    if open_only && (self.closed[from] || self.closed[to]) {
                        reachable[from][to] = false;
                    }
                }
            }
            for via in 0..self.ids.len() {
                for from in 0..self.ids.len() {
                    for to in 0..self.ids.len() {
                        if reachable[from][via] && reachable[via][to] {
                            reachable[from][to] = true;
                        }
                    }
                }
            }
            reachable
        }

        fn cyclic_components(&self) -> usize {
            let reachable = self.reachability(false);
            (0..self.ids.len())
                .filter(|&node| {
                    reachable[node][node]
                        && (0..node).all(|other| !reachable[node][other] || !reachable[other][node])
                })
                .count()
        }

        fn open_components(&self) -> usize {
            let mut visited = self.closed.clone();
            let mut count = 0;
            for start in 0..self.ids.len() {
                if visited[start] {
                    continue;
                }
                count += 1;
                visited[start] = true;
                let mut pending = vec![start];
                while let Some(node) = pending.pop() {
                    for neighbor in 0..self.ids.len() {
                        if !visited[neighbor]
                            && (self.edges[node][neighbor] || self.edges[neighbor][node])
                        {
                            visited[neighbor] = true;
                            pending.push(neighbor);
                        }
                    }
                }
            }
            count
        }

        fn open_depth(&self) -> Option<usize> {
            fn longest_from(graph: &Fixture, node: usize) -> usize {
                1 + (0..graph.ids.len())
                    .filter(|&next| !graph.closed[next] && graph.edges[node][next])
                    .map(|next| longest_from(graph, next))
                    .max()
                    .unwrap_or(0)
            }

            let reachable = self.reachability(true);
            if (0..self.ids.len()).any(|node| reachable[node][node]) {
                return None;
            }
            Some(
                (0..self.ids.len())
                    .filter(|&node| !self.closed[node])
                    .map(|node| longest_from(self, node))
                    .max()
                    .unwrap_or(0),
            )
        }

        fn edge_indices(&self, item: &Value, from: &str, to: &str) -> (usize, usize) {
            let lookup = |field: &str| {
                let id = item[field].as_str().expect("edge endpoint is a string");
                self.ids
                    .iter()
                    .position(|known| known == id)
                    .expect("known endpoint")
            };
            let edge = (lookup(from), lookup(to));
            assert!(
                self.edges[edge.0][edge.1],
                "suggestion must name a real remaining edge: {item}"
            );
            edge
        }

        fn assert_cycle_replay(&self, advanced: &Value) {
            let result = &advanced["cycle_break"];
            let mut residual = self.clone();
            assert_eq!(result["cycles_before"], residual.cyclic_components());
            for cut in result["suggestions"].as_array().expect("cycle suggestions") {
                let before = residual.cyclic_components();
                let (from, to) = residual.edge_indices(cut, "from", "to");
                assert!(
                    residual.reachability(false)[to][from],
                    "cut must lie on a remaining cycle"
                );
                residual.edges[from][to] = false;
                assert_eq!(
                    cut["cycles_broken"],
                    before.saturating_sub(residual.cyclic_components()),
                    "{cut}"
                );
            }
            assert_eq!(
                result["estimated_cycles_after"],
                residual.cyclic_components()
            );
        }

        fn assert_parallel_replay(&self, advanced: &Value) {
            let cuts = &advanced["parallel_cut"];
            let gains = &advanced["parallel_gain"];
            let before = self.open_depth().unwrap_or(0);
            assert_eq!(cuts["current_serial_depth"], before);
            assert_eq!(gains["current_max_chain"], before);
            assert_eq!(gains["current_components"], self.open_components());

            let mut residual = self.clone();
            for cut in cuts["cuts"].as_array().expect("parallel cuts") {
                let previous = residual.open_depth().expect("parallel cuts require a DAG");
                let (from, to) = residual.edge_indices(cut, "from", "to");
                assert!(!self.closed[from] && !self.closed[to]);
                residual.edges[from][to] = false;
                let after = residual
                    .open_depth()
                    .expect("deleting edges preserves acyclicity");
                assert_eq!(cut["depth_reduction"], previous - after, "{cut}");
            }
            assert_eq!(
                cuts["estimated_depth_after"],
                residual.open_depth().unwrap_or(0)
            );

            for gain in gains["gains"].as_array().expect("parallel gains") {
                let mut single = self.clone();
                let (from, to) = single.edge_indices(gain, "edge_from", "edge_to");
                assert!(!self.closed[from] && !self.closed[to]);
                single.edges[from][to] = false;
                let after = single.open_depth().expect("parallel gains require a DAG");
                assert_eq!(gain["new_max_chain"], after, "{gain}");
                assert_eq!(gain["new_components"], single.open_components(), "{gain}");
                let expected = f64::from(u32::try_from(before - after).unwrap())
                    / f64::from(u32::try_from(before).unwrap());
                assert!(
                    (gain["parallelism_delta"].as_f64().unwrap() - expected).abs() < 1e-12,
                    "{gain}"
                );
            }
        }
    }

    #[test]
    fn e2e_advanced_cycle_break_replays_overlapping_cycles() {
        let graph = Fixture::new(
            &["A", "B", "C"],
            &[(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)],
        );
        let advanced = graph.run(false);
        graph.assert_cycle_replay(&advanced);
        assert_eq!(advanced["cycle_break"]["estimated_cycles_after"], 0);
        assert!(
            advanced["cycle_break"]["suggestions"]
                .as_array()
                .unwrap()
                .len()
                > 1
        );
    }

    #[test]
    fn e2e_advanced_cycle_break_budget_reports_actual_remaining_cycles() {
        let edges = (0..7)
            .flat_map(|from| {
                (0..7)
                    .filter(move |&to| from != to)
                    .map(move |to| (from, to))
            })
            .collect::<Vec<_>>();
        let graph = Fixture::new(&["A", "B", "C", "D", "E", "F", "G"], &edges);
        let advanced = graph.run(false);
        graph.assert_cycle_replay(&advanced);
        assert_eq!(
            advanced["cycle_break"]["suggestions"]
                .as_array()
                .unwrap()
                .len(),
            10
        );
        // Every pair has both directions, requiring at least 21 removals.
        assert!(
            advanced["cycle_break"]["estimated_cycles_after"]
                .as_u64()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn e2e_advanced_diamond_does_not_claim_single_edge_parallel_gain() {
        let graph = Fixture::new(&["A", "B", "C", "D"], &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        let advanced = graph.run(false);
        graph.assert_parallel_replay(&advanced);
        assert_eq!(advanced["parallel_gain"]["current_max_chain"], 3);
        let gains = advanced["parallel_gain"]["gains"].as_array().unwrap();
        assert_eq!(gains.len(), 4);
        for gain in gains {
            assert_eq!(gain["new_max_chain"], 3);
            assert_eq!(gain["parallelism_delta"], 0.0);
        }
    }

    #[test]
    fn e2e_advanced_parallel_cut_reports_cumulative_residual_depth() {
        let graph = Fixture::new(&["A", "B", "C", "D"], &[(0, 1), (1, 2), (2, 3)]);
        let advanced = graph.run(false);
        graph.assert_parallel_replay(&advanced);
        assert_eq!(advanced["parallel_cut"]["current_serial_depth"], 4);
        assert_eq!(advanced["parallel_cut"]["cuts"][0]["from"], "B");
        assert_eq!(advanced["parallel_cut"]["cuts"][0]["to"], "C");
        assert_eq!(advanced["parallel_cut"]["cuts"][0]["depth_reduction"], 2);
        assert_eq!(advanced["parallel_cut"]["estimated_depth_after"], 1);
    }

    #[test]
    fn e2e_advanced_parallel_scope_excludes_closed_and_nonblocking_edges() {
        let mut graph = Fixture::new(
            &["A", "B", "X1", "X2", "X3", "X4", "X5"],
            &[(0, 1), (2, 3), (3, 4), (4, 5), (5, 6)],
        );
        graph.closed[2..].fill(true);
        graph.nonblocking = vec![(1, 0, "related"), (1, 0, "parent-child")];
        let advanced = graph.run(false);
        graph.assert_parallel_replay(&advanced);
        assert_eq!(advanced["parallel_cut"]["current_serial_depth"], 2);
        assert_eq!(
            advanced["parallel_cut"]["cuts"].as_array().unwrap().len(),
            1
        );
        assert_eq!(advanced["parallel_cut"]["estimated_depth_after"], 1);
    }

    #[test]
    fn e2e_advanced_closed_cycles_do_not_hide_open_parallel_work() {
        let mut graph = Fixture::new(&["A", "B", "X", "Y"], &[(0, 1), (2, 3), (3, 2)]);
        graph.closed[2..].fill(true);
        let advanced = graph.run(false);
        graph.assert_cycle_replay(&advanced);
        graph.assert_parallel_replay(&advanced);
        assert_eq!(advanced["cycle_break"]["cycles_before"], 1);
        assert_eq!(advanced["parallel_cut"]["current_serial_depth"], 2);
        assert_eq!(advanced["parallel_cut"]["estimated_depth_after"], 1);
    }

    #[test]
    fn e2e_advanced_suggestions_are_stable_under_input_reordering() {
        for graph in [
            Fixture::new(&["A", "B", "C", "D"], &[(0, 1), (0, 2), (1, 3), (2, 3)]),
            Fixture::new(
                &["A", "B", "C"],
                &[(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)],
            ),
        ] {
            let forward = graph.run(false);
            let reverse = graph.run(true);
            for field in ["cycle_break", "parallel_cut", "parallel_gain"] {
                assert_eq!(forward[field], reverse[field], "{field}");
            }
        }
    }
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
        &[
            "--robot-search",
            "--search",
            "parity",
            "--search-limit",
            "5",
        ],
        COMPLEX_FIXTURE,
    );
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &["query", "limit", "mode", "results", "usage_hints"],
            ""
        )
        .is_empty()
    );
    assert!(validate_type_at(&json, "results", JsonType::Array).is_empty());
}

#[test]
fn e2e_robot_search_min_score_filters_and_echoes_threshold() {
    let all = run_robot(
        &[
            "--robot-search",
            "--search",
            "github database",
            "--search-limit",
            "50",
        ],
        COMPLEX_FIXTURE,
    );
    assert!(
        all.get("min_score").is_none(),
        "min_score only appears when set: {all}"
    );
    let all_count = all["results"].as_array().expect("results array").len();
    assert!(
        all_count > 0,
        "fixture should partially match the query: {all}"
    );

    let strict = run_robot(
        &[
            "--robot-search",
            "--search",
            "github database",
            "--search-limit",
            "50",
            "--search-min-score",
            "0.9",
        ],
        COMPLEX_FIXTURE,
    );
    assert_eq!(strict["min_score"], 0.9);
    let strict_count = strict["results"].as_array().expect("results array").len();
    assert!(
        strict_count < all_count,
        "a high similarity floor must drop partial matches ({strict_count} vs {all_count})"
    );
}

#[test]
fn e2e_robot_search_min_score_rejects_out_of_range_values() {
    for bad in ["1.5", "-2", "nan", "abc"] {
        let output = bvr()
            .args([
                "--robot-search",
                "--search",
                "parity",
                "--search-min-score",
                bad,
            ])
            .arg("--beads-file")
            .arg(COMPLEX_FIXTURE)
            .output()
            .expect("run bvr");
        assert_eq!(output.status.code(), Some(2), "{bad} should be rejected");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid --search-min-score"),
            "stderr should explain the rejection for {bad}"
        );
    }
}

#[test]
fn e2e_invalid_id_pattern_exits_with_usage_error() {
    let output = bvr()
        .args(["--robot-history", "--id-pattern", "bh-("])
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("run bvr");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid --id-pattern"));
}

#[test]
fn e2e_robot_search_hybrid_without_lexical_match_returns_no_results() {
    let json = run_robot(
        &[
            "--robot-search",
            "--search",
            "zzzznotfound",
            "--search-mode",
            "hybrid",
            "--search-limit",
            "5",
        ],
        FIXTURE,
    );
    assert_valid_envelope(&json);
    let results = json["results"].as_array().expect("results array");
    assert!(
        results.is_empty(),
        "hybrid search should not return unrelated issues when the query has no lexical matches: {json}"
    );
}

#[test]
fn e2e_robot_search_honors_search_preset_env() {
    let output = bvr()
        .env("BV_SEARCH_PRESET", "text-only")
        .args([
            "--robot-search",
            "--search",
            "parity",
            "--search-mode",
            "hybrid",
            "--beads-file",
            COMPLEX_FIXTURE,
        ])
        .output()
        .expect("failed to execute bvr");
    assert!(
        output.status.success(),
        "env-driven search preset failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: Value = serde_json::from_slice(&output.stdout).expect("search json payload");
    assert_valid_envelope(&json);
    assert_eq!(json["preset"], "text-only");
    assert_eq!(json["weights"]["text"], 1.0);
    assert_eq!(json["weights"]["pagerank"], 0.0);
}

#[test]
fn e2e_robot_label_health() {
    let json = run_robot(&["--robot-label-health"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["analysis_config", "results", "usage_hints"], "").is_empty());
}

#[test]
fn e2e_robot_label_flow() {
    let json = run_robot(&["--robot-label-flow"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["analysis_config", "flow", "usage_hints"], "").is_empty());
}

#[test]
fn e2e_robot_label_attention() {
    let json = run_robot(
        &["--robot-label-attention", "--attention-limit", "3"],
        COMPLEX_FIXTURE,
    );
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &["limit", "labels", "total_labels", "usage_hints"],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_recipes() {
    let json = run_robot(&["--robot-recipes"], FIXTURE);
    assert!(validate_fields(&json, &["generated_at", "output_format", "version"], "").is_empty());
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
// Robot commands: causal / file-intel / blocker-chain / correlation
// =========================================================================

#[test]
fn e2e_robot_blocker_chain() {
    let json = run_robot(&["--robot-blocker-chain", "bd-101"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &[
                "target_id",
                "chain_length",
                "is_blocked",
                "has_cycle",
                "root_blockers"
            ],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_causality() {
    let json = run_robot(&["--robot-causality", "bd-101"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(validate_fields(&json, &["chain", "insights"], "").is_empty());
    // chain and insights are objects, not arrays
    assert!(validate_type_at(&json, "chain", JsonType::Object).is_empty());
    assert!(validate_type_at(&json, "insights", JsonType::Object).is_empty());
}

#[test]
fn e2e_robot_file_beads() {
    let json = run_robot(&["--robot-file-beads", "src/main.rs"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &["file_path", "total_beads", "open_beads", "closed_beads"],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_impact() {
    let json = run_robot(&["--robot-impact", "bd-101"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &[
                "risk_level",
                "risk_score",
                "summary",
                "files",
                "affected_beads"
            ],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_file_relations() {
    let json = run_robot(&["--robot-file-relations", "src/main.rs"], COMPLEX_FIXTURE);
    assert_valid_envelope(&json);
    assert!(
        validate_fields(
            &json,
            &["source_file", "related_files", "total_commits_for_source"],
            ""
        )
        .is_empty()
    );
}

#[test]
fn e2e_robot_explain_correlation_rejects_invalid_format() {
    let output = bvr()
        .args(["--robot-explain-correlation", "INVALID"])
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(
        !output.status.success(),
        "explain-correlation should reject arguments without SHA:beadID format"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("SHA:beadID") || combined.contains("Expected format"),
        "should mention expected format: stderr={stderr} stdout={stdout}"
    );
}

#[test]
fn e2e_robot_confirm_correlation_rejects_invalid_format() {
    let output = bvr()
        .args(["--robot-confirm-correlation", "INVALID"])
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(
        !output.status.success(),
        "confirm-correlation should reject arguments without SHA:beadID format"
    );
}

#[test]
fn e2e_robot_reject_correlation_rejects_invalid_format() {
    let output = bvr()
        .args(["--robot-reject-correlation", "INVALID"])
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("failed to execute");
    assert!(
        !output.status.success(),
        "reject-correlation should reject arguments without SHA:beadID format"
    );
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
    assert!(
        json["triage"]["commands"].get("claim_top").is_none(),
        "empty fixture should omit claim_top when there is no recommendation: {json}"
    );
    assert!(
        json["triage"]["commands"].get("show_top").is_none(),
        "empty fixture should omit show_top when there is no recommendation: {json}"
    );
}
