//! E2E tests for workspace discovery and history mode workflows.
//!
//! These tests exercise the real `bvr` binary with temp repo layouts and git
//! histories, verifying discovery semantics, workspace aggregation, prefix
//! namespacing, history correlation, and representative failure modes.
//!
//! Coverage contract (bd-7oo.3.4):
//! - Workspace: discovery from nested dir, pattern matching, exclusion, prefix
//!   namespacing, cross-repo deps, partial load on failure, explicit --workspace.
//! - History: --robot-history shape, --history-since filtering, --min-confidence
//!   filtering, --bead-history single-issue, zero-result cases, nested dir.
//! - Failure: ambiguous workspace, missing beads, corrupt JSONL, invalid flags.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bvr_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr"))
}

fn bvr() -> Command {
    Command::new(bvr_bin())
}

fn run_json(args: &[&str], dir: &Path) -> Value {
    let output = bvr()
        .current_dir(dir)
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).expect("valid JSON from bvr")
}

fn run_json_env(args: &[&str], dir: &Path, envs: &[(&str, &str)]) -> Value {
    let mut cmd = bvr();
    cmd.current_dir(dir);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON from bvr")
}

fn run_failing(args: &[&str], dir: &Path) -> String {
    let output = bvr()
        .current_dir(dir)
        .args(args)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    String::from_utf8_lossy(&output).to_string()
}

fn run_git(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut cmd = ProcessCommand::new("git");
    cmd.current_dir(dir);
    cmd.args(args);
    cmd.env("GIT_AUTHOR_NAME", "Test");
    cmd.env("GIT_AUTHOR_EMAIL", "test@test.com");
    cmd.env("GIT_COMMITTER_NAME", "Test");
    cmd.env("GIT_COMMITTER_EMAIL", "test@test.com");
    if let Some(d) = date {
        cmd.env("GIT_AUTHOR_DATE", d);
        cmd.env("GIT_COMMITTER_DATE", d);
    }
    let out = cmd.output().expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_beads(dir: &Path, content: &str) {
    let beads_dir = dir.join(".beads");
    fs::create_dir_all(&beads_dir).expect("mkdir .beads");
    fs::write(beads_dir.join("beads.jsonl"), content).expect("write beads.jsonl");
}

fn issue_line(id: &str, title: &str, status: &str, priority: u8) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","status":"{status}","priority":{priority},"issue_type":"task"}}"#
    )
}

fn issue_line_with_dep(
    id: &str,
    title: &str,
    status: &str,
    priority: u8,
    depends_on: &str,
) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","status":"{status}","priority":{priority},"issue_type":"task","dependencies":[{{"depends_on_id":"{depends_on}","type":"blocks"}}]}}"#
    )
}

fn setup_workspace(root: &Path, repos: &[(&str, &str)]) -> PathBuf {
    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");

    let mut yaml_repos = String::new();
    for (name, _) in repos {
        yaml_repos.push_str(&format!(
            "  - name: {name}\n    path: {name}\n    prefix: \"{name}-\"\n"
        ));
    }
    let config = format!("name: test-workspace\nrepos:\n{yaml_repos}");
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write workspace.yaml");

    for (name, content) in repos {
        write_beads(&root.join(name), content);
    }

    bv_dir.join("workspace.yaml")
}

fn setup_workspace_with_discovery(root: &Path, layout: &[(&str, &str)]) -> PathBuf {
    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");

    let config =
        "name: discovery-workspace\nrepos: []\ndiscovery:\n  enabled: true\n  max_depth: 2\n";
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write workspace.yaml");

    for (subpath, content) in layout {
        write_beads(&root.join(subpath), content);
    }

    bv_dir.join("workspace.yaml")
}

// ===================================================================
// WORKSPACE DISCOVERY E2E
// ===================================================================

#[test]
fn workspace_explicit_repos_aggregate_and_namespace_ids() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!(
        "{}\n{}\n",
        issue_line("AUTH-1", "Auth feature", "open", 1),
        issue_line("AUTH-2", "Token refresh", "open", 2),
    );
    let web_beads = format!(
        "{}\n{}\n",
        issue_line("UI-1", "Dashboard", "open", 1),
        issue_line_with_dep("UI-2", "Login page", "blocked", 2, "AUTH-1"),
    );

    let ws_path = setup_workspace(root, &[("api", &api_beads), ("web", &web_beads)]);

    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    // Verify namespaced IDs appear in triage
    let triage = &json["triage"];
    assert!(triage.is_object(), "triage key missing");
    let quick_ref = &triage["quick_ref"];
    assert_eq!(quick_ref["total_open"], 4, "should see 4 open issues total");

    // Collect all issue IDs from all triage sections
    let mut all_ids = Vec::<String>::new();
    for section in &["recommendations", "quick_wins", "blockers_to_clear"] {
        if let Some(arr) = triage[*section].as_array() {
            for item in arr {
                if let Some(id) = item["id"].as_str() {
                    all_ids.push(id.to_string());
                }
            }
        }
    }
    if let Some(picks) = quick_ref["top_picks"].as_array() {
        for pick in picks {
            if let Some(id) = pick["id"].as_str() {
                all_ids.push(id.to_string());
            }
        }
    }
    assert!(
        all_ids.iter().any(|id| id.starts_with("api-")),
        "expected api- prefix in IDs: {all_ids:?}"
    );
    assert!(
        all_ids.iter().any(|id| id.starts_with("web-")),
        "expected web- prefix in IDs: {all_ids:?}"
    );
}

#[test]
fn workspace_cross_repo_dependency_is_resolved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("AUTH-1", "Auth service", "open", 1));
    // web's UI-2 depends on api's AUTH-1 via cross-repo prefix
    let web_beads = format!(
        "{}\n{}\n",
        issue_line("UI-1", "Dashboard", "open", 1),
        issue_line_with_dep("UI-2", "Login page", "blocked", 2, "api-AUTH-1"),
    );

    let ws_path = setup_workspace(root, &[("api", &api_beads), ("web", &web_beads)]);

    // Use --robot-graph to verify the cross-repo edge exists
    let json = run_json(
        &["--robot-graph", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let edges = json["edges"].as_u64().unwrap_or(0);
    assert!(
        edges >= 1,
        "should have cross-repo dependency edge, got {edges}"
    );

    // Also verify via triage that the workspace aggregates all issues
    let triage_json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );
    let total = triage_json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        total >= 2,
        "workspace should aggregate issues from both repos, got {total}"
    );
}

#[test]
fn workspace_discovery_finds_repos_by_pattern() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let svc_beads = format!("{}\n", issue_line("SVC-1", "Service task", "open", 1));
    let pkg_beads = format!("{}\n", issue_line("PKG-1", "Package task", "open", 1));

    let ws_path = setup_workspace_with_discovery(
        root,
        &[("svc-alpha", &svc_beads), ("packages/lib-a", &pkg_beads)],
    );

    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert!(total >= 2, "discovery should find both repos, got {total}");
}

#[test]
fn workspace_discovery_can_include_workspace_root_repo() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("workspace root name")
        .to_ascii_lowercase();

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    fs::write(
        bv_dir.join("workspace.yaml"),
        "name: root-repo\nrepos: []\ndiscovery:\n  enabled: true\n  patterns: ['.']\n",
    )
    .expect("write config");

    let root_beads = format!("{}\n", issue_line("ROOT-1", "Root task", "open", 1));
    write_beads(root, &root_beads);

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "workspace root repo should be discovered");

    let top_picks = json["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        top_picks
            .iter()
            .any(|pick| pick["id"].as_str() == Some(&format!("{root_name}-ROOT-1"))),
        "workspace root issue should be namespaced and triaged: {top_picks:?}"
    );
}

#[test]
fn workspace_explicit_root_repo_path_uses_workspace_name_for_namespacing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("workspace root name")
        .to_ascii_lowercase();

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    fs::write(
        bv_dir.join("workspace.yaml"),
        "name: explicit-root\nrepos:\n  - path: ./\n",
    )
    .expect("write config");

    let root_beads = format!(
        "{}\n",
        issue_line("ROOT-2", "Root explicit task", "open", 1)
    );
    write_beads(root, &root_beads);

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "explicit workspace root repo should load");

    let top_picks = json["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        top_picks
            .iter()
            .any(|pick| pick["id"].as_str() == Some(&format!("{root_name}-ROOT-2"))),
        "explicit root repo should use workspace directory name instead of '.': {top_picks:?}"
    );
}

#[test]
fn workspace_explicit_parent_segment_repo_path_uses_resolved_repo_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    fs::write(
        bv_dir.join("workspace.yaml"),
        "name: explicit-parent\nrepos:\n  - path: services/api/..\n",
    )
    .expect("write config");

    let service_beads = format!("{}\n", issue_line("SRV-1", "Service task", "open", 1));
    write_beads(&root.join("services"), &service_beads);

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "explicit parent-segment repo path should load");

    let top_picks = json["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        top_picks
            .iter()
            .any(|pick| pick["id"].as_str() == Some("services-SRV-1")),
        "explicit parent-segment repo path should use resolved repo basename: {top_picks:?}"
    );
}

#[test]
fn workspace_discovery_excludes_node_modules() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Create a repo under node_modules that should be excluded
    let real_beads = format!("{}\n", issue_line("REAL-1", "Real task", "open", 1));
    let excluded_beads = format!("{}\n", issue_line("BAD-1", "Should not appear", "open", 1));

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    fs::write(
        bv_dir.join("workspace.yaml"),
        "name: test\nrepos: []\ndiscovery:\n  enabled: true\n  max_depth: 3\n",
    )
    .expect("write config");

    write_beads(&root.join("app"), &real_beads);
    write_beads(&root.join("node_modules/bad-pkg"), &excluded_beads);

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        total, 1,
        "node_modules repo should be excluded, got {total}"
    );

    // Verify the excluded ID doesn't appear
    let empty_recs = vec![];
    let recs = json["triage"]["recommendations"]
        .as_array()
        .unwrap_or(&empty_recs);
    for rec in recs {
        let id = rec["id"].as_str().unwrap_or_default();
        assert!(
            !id.contains("BAD"),
            "node_modules issue should not appear: {id}"
        );
    }
}

#[test]
fn workspace_partial_load_continues_when_one_repo_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let good_beads = format!("{}\n", issue_line("GOOD-1", "Good task", "open", 1));
    // Create corrupt JSONL for the bad repo
    let bad_beads = "this is not valid json\n";

    let ws_path = setup_workspace(root, &[("good", &good_beads), ("bad", bad_beads)]);
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        total >= 1,
        "should load good repo despite bad repo failure, got {total}"
    );
}

#[test]
fn workspace_discovery_from_nested_subdir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let beads = format!("{}\n", issue_line("NEST-1", "Nested discovery", "open", 1));
    setup_workspace_with_discovery(root, &[("svc-one", &beads)]);

    // Run from a deeply nested directory inside a repo
    let nested = root.join("svc-one/src/deeply/nested");
    fs::create_dir_all(&nested).expect("mkdir nested");

    let json = run_json(&["--robot-triage"], &nested);
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        total >= 1,
        "should discover workspace from nested dir, got {total}"
    );
}

#[test]
fn workspace_discovery_from_workspace_root_without_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("API-1", "API task", "open", 1));
    let web_beads = format!("{}\n", issue_line("WEB-1", "Web task", "open", 2));
    setup_workspace_with_discovery(
        root,
        &[("services/api", &api_beads), ("apps/web", &web_beads)],
    );

    let json = run_json(&["--robot-triage"], root);
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        total, 2,
        "workspace root should auto-discover .bv/workspace.yaml"
    );
}

#[test]
fn workspace_explicit_flag_overrides_discovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Create two workspace configs: one at root, one explicit
    let root_beads = format!("{}\n", issue_line("ROOT-1", "Root task", "open", 1));
    setup_workspace_with_discovery(root, &[("root-svc", &root_beads)]);

    // Create a second workspace in a subdir
    let alt_dir = root.join("alt-workspace");
    let alt_beads = format!(
        "{}\n{}\n",
        issue_line("ALT-1", "Alt task 1", "open", 1),
        issue_line("ALT-2", "Alt task 2", "open", 2),
    );
    let alt_ws = setup_workspace(&alt_dir, &[("alt-svc", &alt_beads)]);

    // Explicit --workspace should use alt, not root
    let json = run_json(
        &["--robot-triage", "--workspace", &alt_ws.to_string_lossy()],
        root,
    );
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 2, "explicit workspace should override discovery");
}

#[test]
fn workspace_disabled_repo_is_excluded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let active_beads = format!("{}\n", issue_line("ACT-1", "Active", "open", 1));
    let disabled_beads = format!("{}\n", issue_line("DIS-1", "Disabled", "open", 1));

    write_beads(&root.join("active"), &active_beads);
    write_beads(&root.join("disabled"), &disabled_beads);

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    let config = "name: test\nrepos:\n  - name: active\n    path: active\n    prefix: \"act-\"\n  - name: disabled\n    path: disabled\n    prefix: \"dis-\"\n    enabled: false\n";
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "disabled repo should not contribute issues");
}

#[test]
fn workspace_discovery_ignores_disabled_explicit_repo_alias() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("AUTH-1", "API task", "open", 1));
    write_beads(&root.join("services/api"), &api_beads);

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    let config = concat!(
        "name: disabled-alias\n",
        "discovery:\n",
        "  enabled: true\n",
        "repos:\n",
        "  - name: disabled-api\n",
        "    path: services/./api\n",
        "    prefix: \"disabled-\"\n",
        "    enabled: false\n",
    );
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        total, 1,
        "disabled explicit alias should not suppress discovery"
    );

    let top_picks = json["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        top_picks.iter().all(|pick| pick["id"]
            .as_str()
            .is_some_and(|id| !id.starts_with("disabled-"))),
        "disabled explicit alias should not namespace the discovered repo: {top_picks:?}"
    );
}

#[test]
fn workspace_defaults_beads_path_applies_to_all_repos() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Use custom beads path "trackers" instead of ".beads"
    let beads_content = format!("{}\n", issue_line("CUST-1", "Custom path", "open", 1));
    let tracker_dir = root.join("svc/trackers");
    fs::create_dir_all(&tracker_dir).expect("mkdir trackers");
    fs::write(tracker_dir.join("beads.jsonl"), &beads_content).expect("write beads");

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    let config = "name: custom-path\ndefaults:\n  beads_path: trackers\nrepos:\n  - name: svc\n    path: svc\n    prefix: \"svc-\"\n";
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(
        total, 1,
        "custom beads_path should find issues in trackers/"
    );
}

#[test]
fn workspace_discovery_dedupes_explicit_repo_path_aliases() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("AUTH-1", "API task", "open", 1));
    write_beads(&root.join("services/api"), &api_beads);

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    fs::write(
        bv_dir.join("workspace.yaml"),
        concat!(
            "name: dedupe\n",
            "discovery:\n",
            "  enabled: true\n",
            "repos:\n",
            "  - name: backend\n",
            "    path: services/./api\n",
            "    prefix: \"backend-\"\n",
        ),
    )
    .expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let json = run_json(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "same repo alias should not be loaded twice");

    let top_picks = json["triage"]["quick_ref"]["top_picks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        top_picks.iter().all(|pick| pick["id"]
            .as_str()
            .is_some_and(|id| !id.starts_with("api-"))),
        "discovery should not create a second synthetic repo alias: {top_picks:?}"
    );
}

// ===================================================================
// WORKSPACE FAILURE MODES
// ===================================================================

#[test]
fn workspace_no_repos_found_returns_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    // Discovery enabled but no repos have .beads/ directories
    fs::write(
        bv_dir.join("workspace.yaml"),
        "name: empty\nrepos: []\ndiscovery:\n  enabled: true\n",
    )
    .expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let stderr = run_failing(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );
    assert!(
        stderr.contains("discovery found no repositories") || stderr.contains("no enabled"),
        "expected discovery error: {stderr}"
    );
}

#[test]
fn workspace_duplicate_prefix_returns_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let beads = format!("{}\n", issue_line("X-1", "Test", "open", 1));
    write_beads(&root.join("repo1"), &beads);
    write_beads(&root.join("repo2"), &beads);

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    let config = "name: dup\nrepos:\n  - name: repo1\n    path: repo1\n    prefix: \"dup-\"\n  - name: repo2\n    path: repo2\n    prefix: \"dup-\"\n";
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let stderr = run_failing(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );
    assert!(
        stderr.contains("duplicate prefix"),
        "expected duplicate prefix error: {stderr}"
    );
}

#[test]
fn workspace_duplicate_repo_alias_returns_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("AUTH-1", "API task", "open", 1));
    write_beads(&root.join("services/api"), &api_beads);

    let bv_dir = root.join(".bv");
    fs::create_dir_all(&bv_dir).expect("mkdir .bv");
    let config = concat!(
        "name: dup-alias\n",
        "repos:\n",
        "  - name: api\n",
        "    path: services/api\n",
        "    prefix: \"api-\"\n",
        "  - name: backend\n",
        "    path: services/./api\n",
        "    prefix: \"backend-\"\n",
    );
    fs::write(bv_dir.join("workspace.yaml"), config).expect("write config");

    let ws_path = bv_dir.join("workspace.yaml");
    let stderr = run_failing(
        &["--robot-triage", "--workspace", &ws_path.to_string_lossy()],
        root,
    );
    assert!(
        stderr.contains("duplicates repository path"),
        "expected duplicate repo alias error: {stderr}"
    );
}

// ===================================================================
// HISTORY E2E
// ===================================================================

fn setup_git_repo_with_history(root: &Path) {
    run_git(root, &["init"], None);
    fs::create_dir_all(root.join(".beads")).expect("mkdir .beads");
    fs::create_dir_all(root.join("src")).expect("mkdir src");

    // Commit 1: seed issues
    let seed = format!(
        "{}\n{}\n{}\n",
        issue_line("H-1", "First issue", "open", 1),
        issue_line("H-2", "Second issue", "open", 2),
        issue_line("H-3", "Third issue", "open", 3),
    );
    fs::write(root.join(".beads/beads.jsonl"), &seed).expect("write seed");
    run_git(root, &["add", ".beads/beads.jsonl"], None);
    run_git(
        root,
        &["commit", "-m", "seed H-1 H-2 H-3"],
        Some("2024-06-01T00:00:00Z"),
    );

    // Commit 2: claim H-1, add code
    let update1 = format!(
        "{}\n{}\n{}\n",
        issue_line("H-1", "First issue", "in_progress", 1),
        issue_line("H-2", "Second issue", "open", 2),
        issue_line("H-3", "Third issue", "open", 3),
    );
    fs::write(root.join(".beads/beads.jsonl"), &update1).expect("write update1");
    fs::write(root.join("src/feature.rs"), "// H-1 implementation\n").expect("write code");
    run_git(root, &["add", ".beads/beads.jsonl", "src/feature.rs"], None);
    run_git(
        root,
        &["commit", "-m", "claim H-1"],
        Some("2024-06-10T00:00:00Z"),
    );

    // Commit 3: close H-1
    let update2 = format!(
        "{}\n{}\n{}\n",
        issue_line("H-1", "First issue", "closed", 1),
        issue_line("H-2", "Second issue", "open", 2),
        issue_line("H-3", "Third issue", "open", 3),
    );
    fs::write(root.join(".beads/beads.jsonl"), &update2).expect("write update2");
    fs::write(root.join("src/feature.rs"), "// H-1 complete\n").expect("write code update");
    run_git(root, &["add", ".beads/beads.jsonl", "src/feature.rs"], None);
    run_git(
        root,
        &["commit", "-m", "close H-1"],
        Some("2024-06-20T00:00:00Z"),
    );

    // Commit 4: unrelated code change (no bead reference)
    fs::write(root.join("src/utils.rs"), "// utility\n").expect("write utils");
    run_git(root, &["add", "src/utils.rs"], None);
    run_git(
        root,
        &["commit", "-m", "add utilities"],
        Some("2024-06-25T00:00:00Z"),
    );
}

fn setup_workspace_repo_with_history(
    repo_root: &Path,
    bead_id: &str,
    title: &str,
    code_path: &str,
    seed_date: &str,
    close_date: &str,
) {
    run_git(repo_root, &["init"], None);
    fs::create_dir_all(repo_root.join(".beads")).expect("mkdir .beads");
    let code_file = repo_root.join(code_path);
    if let Some(parent) = code_file.parent() {
        fs::create_dir_all(parent).expect("mkdir code parent");
    }

    let seed = format!("{}\n", issue_line(bead_id, title, "open", 1));
    fs::write(repo_root.join(".beads/beads.jsonl"), &seed).expect("write seed");
    run_git(repo_root, &["add", ".beads/beads.jsonl"], None);
    run_git(
        repo_root,
        &["commit", "-m", &format!("seed {bead_id}")],
        Some(seed_date),
    );

    let closed = format!("{}\n", issue_line(bead_id, title, "closed", 1));
    fs::write(repo_root.join(".beads/beads.jsonl"), &closed).expect("write closed");
    fs::write(&code_file, format!("// completed {bead_id}\n")).expect("write code");
    run_git(repo_root, &["add", ".beads/beads.jsonl", code_path], None);
    run_git(
        repo_root,
        &["commit", "-m", &format!("close {bead_id}")],
        Some(close_date),
    );
}

#[test]
fn history_output_has_valid_envelope() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(&["--robot-history", "--history-limit", "20"], temp.path());

    // Envelope fields
    assert!(json["generated_at"].is_string(), "missing generated_at");
    assert!(json["data_hash"].is_string(), "missing data_hash");

    // History-specific fields
    assert!(json["git_range"].is_string(), "missing git_range");
    assert!(json["stats"].is_object(), "missing stats");
    assert!(
        json["histories"].is_object(),
        "missing histories map: {json}"
    );
}

#[test]
fn workspace_history_correlates_commits_from_each_repo_git_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let workspace = setup_workspace(
        root,
        &[
            (
                "api",
                &format!("{}\n", issue_line("AUTH-1", "Auth work", "closed", 1)),
            ),
            (
                "web",
                &format!("{}\n", issue_line("UI-1", "Web work", "closed", 1)),
            ),
        ],
    );

    setup_workspace_repo_with_history(
        &root.join("api"),
        "AUTH-1",
        "Auth work",
        "src/api.rs",
        "2024-07-01T00:00:00Z",
        "2024-07-03T00:00:00Z",
    );
    setup_workspace_repo_with_history(
        &root.join("web"),
        "UI-1",
        "Web work",
        "src/web.rs",
        "2024-07-02T00:00:00Z",
        "2024-07-04T00:00:00Z",
    );

    let json = run_json(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--workspace",
            &workspace.to_string_lossy(),
        ],
        root,
    );

    let api_commits = json["histories"]["api-AUTH-1"]["commits"]
        .as_array()
        .expect("api commits");
    let web_commits = json["histories"]["web-UI-1"]["commits"]
        .as_array()
        .expect("web commits");

    assert!(
        api_commits.iter().any(|commit| commit["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("AUTH-1"))),
        "api history should include commits from api repo: {api_commits:?}"
    );
    assert!(
        web_commits.iter().any(|commit| commit["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("UI-1"))),
        "web history should include commits from web repo: {web_commits:?}"
    );
    assert!(
        json["latest_commit_sha"]
            .as_str()
            .is_some_and(|sha| !sha.is_empty()),
        "workspace history should expose a latest commit sha"
    );
}

#[test]
fn history_contains_correlated_commits_for_referenced_bead() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(&["--robot-history", "--history-limit", "20"], temp.path());
    let histories = &json["histories"];

    // H-1 should have commits (seed + claim + close = 3 commits referencing it)
    let h1 = &histories["H-1"];
    assert!(h1.is_object(), "H-1 not in histories");
    let commits = h1["commits"].as_array().expect("H-1 commits");
    assert!(
        commits.len() >= 2,
        "H-1 should have at least 2 correlated commits, got {}",
        commits.len()
    );

    // Verify commit messages
    let messages: Vec<&str> = commits
        .iter()
        .filter_map(|c| c["message"].as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("H-1")),
        "expected commit referencing H-1: {messages:?}"
    );
}

#[test]
fn history_since_filters_older_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    // All commits
    let all = run_json(&["--robot-history", "--history-limit", "20"], temp.path());
    let all_stats = &all["stats"];

    // Filter to only after June 15
    let filtered = run_json(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--history-since",
            "2024-06-15T00:00:00Z",
        ],
        temp.path(),
    );

    assert_eq!(
        filtered["git_range"],
        "since 2024-06-15T00:00:00Z, last 20 commits"
    );

    let filtered_total = filtered["stats"]["total_commits"].as_u64().unwrap_or(0);
    let all_total = all_stats["total_commits"].as_u64().unwrap_or(0);
    assert!(
        filtered_total < all_total,
        "filtered ({filtered_total}) should have fewer commits than all ({all_total})"
    );
}

#[test]
fn history_min_confidence_removes_low_confidence_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let all = run_json(&["--robot-history", "--history-limit", "20"], temp.path());
    let filtered = run_json(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--min-confidence",
            "0.9",
        ],
        temp.path(),
    );

    // With high confidence filter, should have fewer or equal commits
    let all_total = all["stats"]["total_commits"].as_u64().unwrap_or(0);
    let filtered_total = filtered["stats"]["total_commits"].as_u64().unwrap_or(0);
    assert!(
        filtered_total <= all_total,
        "high-confidence filter should not increase commit count"
    );

    // All remaining commits should meet confidence threshold
    if let Some(histories) = filtered["histories"].as_object() {
        for (_bead_id, bead_data) in histories {
            if let Some(commits) = bead_data["commits"].as_array() {
                for commit in commits {
                    let confidence = commit["confidence"].as_f64().unwrap_or(0.0);
                    assert!(
                        confidence >= 0.9,
                        "commit below confidence threshold: {confidence}"
                    );
                }
            }
        }
    }
}

#[test]
fn history_bead_history_filters_to_single_issue() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(
        &[
            "--robot-history",
            "--bead-history",
            "H-2",
            "--history-limit",
            "20",
        ],
        temp.path(),
    );

    assert_eq!(
        json["bead_history"], "H-2",
        "bead_history should reflect filter"
    );
}

#[test]
fn history_from_nested_dir_with_beads_dir_env() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    setup_git_repo_with_history(root);

    let nested = root.join("src");
    let beads_dir = root.join(".beads");

    let json = run_json_env(
        &["--robot-history", "--history-limit", "20"],
        &nested,
        &[("BEADS_DIR", &beads_dir.to_string_lossy())],
    );

    assert!(
        json["histories"].is_object(),
        "should find histories from nested dir"
    );
    let histories = json["histories"].as_object().unwrap();
    assert!(
        !histories.is_empty(),
        "should have at least one bead with history"
    );
}

#[test]
fn history_future_since_returns_zero_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(
        &[
            "--robot-history",
            "--history-limit",
            "20",
            "--history-since",
            "2030-01-01T00:00:00Z",
        ],
        temp.path(),
    );

    let total = json["stats"]["total_commits"].as_u64().unwrap_or(0);
    assert_eq!(total, 0, "future date should yield 0 commits, got {total}");
}

#[test]
fn history_limit_caps_returned_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(&["--robot-history", "--history-limit", "1"], temp.path());

    let total = json["stats"]["total_commits"].as_u64().unwrap_or(u64::MAX);
    assert!(total <= 1, "history-limit=1 should cap to 1, got {total}");
}

#[test]
fn history_stats_has_method_distribution() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(&["--robot-history", "--history-limit", "20"], temp.path());
    let stats = &json["stats"];
    let dist = &stats["method_distribution"];
    assert!(
        dist.is_object(),
        "stats.method_distribution should be an object"
    );

    // At least one method should have nonzero count
    let has_method = dist.as_object().map_or(false, |m| {
        m.values()
            .any(|v| v.as_u64().unwrap_or(0) > 0 || v.as_i64().unwrap_or(0) > 0)
    });
    assert!(has_method, "should have at least one correlation method");
}

#[test]
fn history_commit_index_maps_sha_to_beads() {
    let temp = tempfile::tempdir().expect("tempdir");
    setup_git_repo_with_history(temp.path());

    let json = run_json(&["--robot-history", "--history-limit", "20"], temp.path());
    let commit_index = &json["commit_index"];
    assert!(
        commit_index.is_object(),
        "commit_index should be an object: {json}"
    );

    // Each key should be a SHA, each value an array of bead IDs
    if let Some(idx) = commit_index.as_object() {
        for (sha, beads) in idx {
            assert!(sha.len() >= 7, "SHA too short: {sha}");
            assert!(beads.is_array(), "commit_index values should be arrays");
        }
    }
}

// ===================================================================
// COMBINED WORKSPACE + HISTORY
// ===================================================================

#[test]
fn workspace_triage_with_beads_file_bypasses_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Create workspace with 2 repos
    let api_beads = format!("{}\n", issue_line("X-1", "WS issue", "open", 1));
    setup_workspace(root, &[("api", &api_beads)]);

    // Also create a standalone beads file
    let standalone = format!(
        "{}\n{}\n{}\n",
        issue_line("S-1", "Standalone 1", "open", 1),
        issue_line("S-2", "Standalone 2", "open", 2),
        issue_line("S-3", "Standalone 3", "open", 3),
    );
    let standalone_path = root.join("standalone.jsonl");
    fs::write(&standalone_path, &standalone).expect("write standalone");

    // --beads-file should bypass workspace entirely
    let json = run_json(
        &[
            "--robot-triage",
            "--beads-file",
            &standalone_path.to_string_lossy(),
        ],
        root,
    );

    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 3, "--beads-file should bypass workspace");
}

// ===================================================================
// DISCOVERY EDGE CASES
// ===================================================================

#[test]
fn single_repo_mode_without_workspace_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // Just .beads/ in cwd, no .bv/workspace.yaml
    let beads = format!(
        "{}\n{}\n",
        issue_line("SOLO-1", "Solo task", "open", 1),
        issue_line("SOLO-2", "Solo blocked", "blocked", 2),
    );
    write_beads(root, &beads);

    let json = run_json(&["--robot-triage"], root);
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 2, "single repo mode should work without workspace");
}

#[test]
fn single_repo_discovery_walks_up_from_nested_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let beads = format!("{}\n", issue_line("UP-1", "Walk up", "open", 1));
    write_beads(root, &beads);

    let nested = root.join("src/deeply/nested/dir");
    fs::create_dir_all(&nested).expect("mkdir nested");

    let json = run_json(&["--robot-triage"], &nested);
    let total = json["triage"]["quick_ref"]["total_open"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(total, 1, "should walk up from nested dir to find .beads/");
}

#[test]
fn missing_beads_dir_returns_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    // The upward .beads search checks every ancestor, so a tempdir living
    // inside a beads repo (remote build workers point TMPDIR at a synced
    // checkout) finds real sources and the "missing" scenario never exists.
    if root.ancestors().any(|a| a.join(".beads").is_dir()) {
        eprintln!("skipping: tempdir has a .beads ancestor that wins the upward search");
        return;
    }

    // No .beads/ directory at all
    let stderr = run_failing(&["--robot-triage"], root);
    assert!(
        stderr.contains(".beads") || stderr.contains("beads"),
        "should mention missing beads dir: {stderr}"
    );
}

// ===================================================================
// HISTORY WITH WORKSPACE
// ===================================================================

#[test]
fn workspace_robot_next_shows_namespaced_id() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!(
        "{}\n{}\n",
        issue_line("TASK-1", "High priority", "open", 0),
        issue_line("TASK-2", "Low priority", "open", 4),
    );
    let ws_path = setup_workspace(root, &[("backend", &api_beads)]);

    let json = run_json(
        &["--robot-next", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let id = json["id"].as_str().expect("id field");
    assert!(
        id.starts_with("backend-"),
        "robot-next ID should be namespaced: {id}"
    );
}

#[test]
fn workspace_robot_graph_shows_cross_repo_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let api_beads = format!("{}\n", issue_line("AUTH-1", "Auth", "open", 1));
    let web_beads = format!(
        "{}\n",
        issue_line_with_dep("UI-1", "Login", "blocked", 2, "api-AUTH-1"),
    );

    let ws_path = setup_workspace(root, &[("api", &api_beads), ("web", &web_beads)]);
    let json = run_json(
        &["--robot-graph", "--workspace", &ws_path.to_string_lossy()],
        root,
    );

    let edges = json["edges"].as_u64().unwrap_or(0);
    assert!(edges >= 1, "should have cross-repo edge, got {edges}");
}

// A fake backend is scoped to each child process. These tests must never invoke
// an installed bd or change the process-wide environment of parallel tests.
#[cfg(unix)]
mod dolt_backend {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    struct FakeDoltRepo {
        directory: tempfile::TempDir,
    }

    impl FakeDoltRepo {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("Dolt fixture directory");
            let root = directory.path();
            fs::create_dir_all(root.join(".beads/dolt")).expect("Dolt marker directory");
            fs::create_dir(root.join("bin")).expect("fake executable directory");
            fs::write(root.join(".beads/metadata.json"), r#"{"backend":"dolt"}"#)
                .expect("Dolt metadata");
            fs::write(root.join("live.jsonl"), "").expect("initial live snapshot");
            let executable = root.join("bin/bd");
            fs::write(
                &executable,
                r#"#!/bin/sh
beads_db=unset
if [ -n "$BEADS_DB" ]; then beads_db="$BEADS_DB"; fi
bd_db=unset
if [ -n "$BD_DB" ]; then bd_db="$BD_DB"; fi
printf '%s|%s|%s|%s|%s\n' "$*" "$PWD" "$BEADS_DIR" "$beads_db" "$bd_db" >> "$BVR_TEST_BD_CALLS"
if [ "$#" -ne 1 ] || [ "$1" != export ]; then
    printf 'expected a read-only bd export on stdout\n' >&2
    exit 64
fi
payload="$BEADS_DIR/../live.jsonl"
if [ -n "$BVR_TEST_BD_PAYLOAD" ]; then payload="$BVR_TEST_BD_PAYLOAD"; fi
case "$BVR_TEST_BD_MODE" in
    fail)
        /bin/cat "$payload"
        printf 'fake bd export failed after partial output\n' >&2
        exit 7
        ;;
    hang)
        printf '%s\n' "$$" > "$BVR_TEST_BD_PID"
        exec /bin/sleep 30
        ;;
    hold-open)
        /bin/sleep 30 &
        printf '%s\n' "$!" > "$BVR_TEST_BD_DESCENDANT_PID"
        ;;
    noisy)
        n=0
        while [ "$n" -lt 4096 ]; do
            printf 'fake bd diagnostic: enough stderr to exceed the capacity of an unread output pipe\n' >&2
            n=$((n + 1))
        done
        ;;
esac
/bin/cat "$payload"
"#,
            )
            .expect("fake bd executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("make fake bd executable");
            Self { directory }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn command(&self) -> Command {
            let mut command = bvr();
            command
                .current_dir(self.root())
                .env_remove("BEADS_DIR")
                .env_remove("BEADS_DB")
                .env_remove("BD_DB")
                .env_remove("BV_DATA_SOURCE")
                .env_remove("BVR_TEST_BD_MODE")
                .env_remove("BVR_TEST_BD_PAYLOAD")
                .env("PATH", self.root().join("bin"))
                .env("BVR_TEST_BD_CALLS", self.root().join("calls.log"))
                .env("BVR_TEST_BD_PID", self.root().join("child.pid"))
                .env(
                    "BVR_TEST_BD_DESCENDANT_PID",
                    self.root().join("descendant.pid"),
                )
                .timeout(Duration::from_secs(12));
            command
        }

        fn write_live(&self, content: &str) {
            fs::write(self.root().join("live.jsonl"), content).expect("live backend snapshot");
        }

        fn write_compatibility_snapshot(&self) -> String {
            let content = format!("{}\n", issue_line("STALE-1", "Stale snapshot", "open", 1));
            fs::write(self.root().join(".beads/issues.jsonl"), &content)
                .expect("compatibility snapshot");
            content
        }

        fn assert_compatibility_snapshot(&self, expected: &str) {
            assert_eq!(
                fs::read_to_string(self.root().join(".beads/issues.jsonl"))
                    .expect("compatibility snapshot still exists"),
                expected,
                "live reads must not modify the compatibility export"
            );
        }

        fn calls(&self) -> String {
            fs::read_to_string(self.root().join("calls.log")).unwrap_or_default()
        }
    }

    impl Drop for FakeDoltRepo {
        fn drop(&mut self) {
            // Only the hold-open case records a still-live descendant. The
            // direct child in the timeout case is reaped and never killed again.
            if let Ok(pid) = fs::read_to_string(self.root().join("descendant.pid")) {
                let _ = ProcessCommand::new("/bin/kill")
                    .args(["-KILL", pid.trim()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }

    fn robot_value(command: &mut Command) -> Value {
        let output = command
            .arg("--robot-triage")
            .assert()
            .success()
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout)
            .expect("stdout contains exactly one robot JSON value")
    }

    fn assert_top_pick(value: &Value, id: &str) {
        assert!(
            value["triage"]["quick_ref"]["top_picks"]
                .as_array()
                .expect("triage top picks")
                .iter()
                .any(|pick| pick["id"] == id),
            "expected live issue {id} in {value}"
        );
    }

    #[test]
    fn dolt_reads_live_stdout_over_stale_jsonl_on_every_new_process() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        fixture.write_live(&issue_line("LIVE-1", "First live state", "open", 1));
        assert_top_pick(&robot_value(&mut fixture.command()), "LIVE-1");

        fixture.write_live(&issue_line("LIVE-2", "Second live state", "open", 1));
        let value = robot_value(fixture.command().args(["--db", ".beads"]));
        assert_top_pick(&value, "LIVE-2");
        assert!(!value.to_string().contains("STALE-1"));
        assert_eq!(fixture.calls().lines().count(), 2);
        fixture.assert_compatibility_snapshot(&stale);
    }

    #[test]
    fn dolt_needs_no_compatibility_file_and_accepts_an_empty_live_database() {
        let fixture = FakeDoltRepo::new();
        fixture.write_live(&issue_line("LIVE-1", "Live only", "open", 1));
        assert_top_pick(&robot_value(&mut fixture.command()), "LIVE-1");
        assert!(!fixture.root().join(".beads/issues.jsonl").exists());

        let stale = fixture.write_compatibility_snapshot();
        fixture.write_live("");
        let value = robot_value(&mut fixture.command());
        assert_eq!(value["triage"]["quick_ref"]["total_open"], 0);
        assert!(
            value["triage"]["quick_ref"]["top_picks"]
                .as_array()
                .expect("empty-store top picks")
                .is_empty()
        );
        fixture.assert_compatibility_snapshot(&stale);
    }

    #[test]
    fn dolt_skips_only_explicit_memory_records() {
        let fixture = FakeDoltRepo::new();
        fixture.write_live(&format!(
            "{{\"_type\":\"memory\",\"key\":\"context\",\"value\":\"not an issue\"}}\n{}\n",
            issue_line("LIVE-1", "Real issue", "open", 1)
        ));
        let value = robot_value(&mut fixture.command());
        assert_top_pick(&value, "LIVE-1");
        assert_eq!(value["triage"]["quick_ref"]["total_open"], 1);
    }

    #[test]
    fn dolt_rejects_corrupt_partial_invalid_and_unknown_records_without_stale_fallback() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        let valid = issue_line("LIVE-1", "Valid prefix", "open", 1);
        for payload in [
            "not JSON\n".to_string(),
            format!("{valid}\n{{unfinished\n"),
            r#"{"id":"INVALID","title":"","status":"open","issue_type":"task"}"#.to_string(),
            concat!(
                r#"{"_type":"unknown","id":"UNKNOWN","title":"Unknown record kind","#,
                r#""status":"open","issue_type":"task"}"#,
            )
            .to_string(),
        ] {
            fixture.write_live(&payload);
            let output = fixture
                .command()
                .arg("--robot-triage")
                .assert()
                .failure()
                .get_output()
                .clone();
            assert!(
                output.stdout.is_empty(),
                "failed live reads must not emit partial robot JSON"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("bd export"),
                "the live backend error must remain visible in robot mode: {stderr}"
            );
            fixture.assert_compatibility_snapshot(&stale);
        }
    }

    #[test]
    fn dolt_nonzero_exit_is_visible_in_robot_mode_and_preserves_the_snapshot() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        fixture.write_live(&issue_line("PARTIAL-1", "Partial result", "open", 1));
        let output = fixture
            .command()
            .env("BVR_TEST_BD_MODE", "fail")
            .env("BV_ROBOT", "1")
            .arg("--robot-triage")
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("bd export"),
            "backend error must be visible: {stderr}"
        );
        assert!(
            stderr.contains("BV_DATA_SOURCE=jsonl"),
            "error should explain the explicit offline option: {stderr}"
        );
        fixture.assert_compatibility_snapshot(&stale);
    }

    #[test]
    fn dolt_missing_executable_fails_instead_of_returning_stale_data() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        let empty_bin = fixture.root().join("empty-bin");
        fs::create_dir(&empty_bin).expect("empty executable search path");
        let output = fixture
            .command()
            .env("PATH", &empty_bin)
            .arg("--robot-triage")
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("bd export"));
        assert!(fixture.calls().is_empty());
        fixture.assert_compatibility_snapshot(&stale);
    }

    #[test]
    fn dolt_hung_export_is_bounded_and_the_child_is_reaped() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        let started = Instant::now();
        let output = fixture
            .command()
            .env("BVR_TEST_BD_MODE", "hang")
            .arg("--robot-triage")
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "backend timeout should precede the outer test deadline"
        );
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("bd export") && stderr.contains("timed out"),
            "expected a visible bounded timeout: {stderr}"
        );
        let pid = fs::read_to_string(fixture.root().join("child.pid")).expect("fake child pid");
        assert!(
            !ProcessCommand::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("check fake child lifetime")
                .success(),
            "the timed-out fake backend must not remain running or unreaped"
        );
        fixture.assert_compatibility_snapshot(&stale);
    }

    #[test]
    fn dolt_large_stderr_cannot_deadlock_or_corrupt_robot_stdout() {
        let fixture = FakeDoltRepo::new();
        fixture.write_live(&issue_line("LIVE-1", "Noisy backend", "open", 1));
        let value = robot_value(fixture.command().env("BVR_TEST_BD_MODE", "noisy"));
        assert_top_pick(&value, "LIVE-1");
        assert!(!value.to_string().contains("fake bd diagnostic"));
    }

    #[test]
    fn dolt_success_does_not_wait_for_descendants_holding_output_descriptors() {
        let fixture = FakeDoltRepo::new();
        fixture.write_live(&issue_line("LIVE-1", "Exited exporter", "open", 1));
        let started = Instant::now();
        let value = robot_value(fixture.command().env("BVR_TEST_BD_MODE", "hold-open"));
        assert_top_pick(&value, "LIVE-1");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "an exited exporter must not wait for inherited output descriptors"
        );
    }

    #[test]
    fn dolt_explicit_jsonl_sources_bypass_the_backend() {
        let fixture = FakeDoltRepo::new();
        let stale = fixture.write_compatibility_snapshot();
        for flag in ["--beads-file", "--db"] {
            let value = robot_value(fixture.command().args([flag, ".beads/issues.jsonl"]));
            assert_top_pick(&value, "STALE-1");
        }
        let value = robot_value(
            fixture
                .command()
                .env("BEADS_DB", fixture.root().join(".beads/issues.jsonl")),
        );
        assert_top_pick(&value, "STALE-1");
        let value = robot_value(fixture.command().env("BV_DATA_SOURCE", "jsonl"));
        assert_top_pick(&value, "STALE-1");
        assert!(
            fixture.calls().is_empty(),
            "explicit file reads must not invoke bd"
        );
        fixture.assert_compatibility_snapshot(&stale);

        let missing = FakeDoltRepo::new();
        fs::write(missing.root().join(".beads/beads.jsonl"), &stale)
            .expect("unrelated compatibility filename");
        missing
            .command()
            .env("BV_DATA_SOURCE", "jsonl")
            .arg("--robot-triage")
            .assert()
            .failure();
        assert!(missing.calls().is_empty());
    }

    #[test]
    fn dolt_redirect_uses_the_resolved_workspace_and_removes_database_overrides() {
        let fixture = FakeDoltRepo::new();
        fixture.write_live(&issue_line("LIVE-1", "Redirected live issue", "open", 1));
        let linked = fixture.root().join("linked");
        fs::create_dir_all(linked.join(".beads")).expect("redirected checkout");
        let resolved =
            fs::canonicalize(fixture.root().join(".beads")).expect("canonical beads path");
        fs::write(
            linked.join(".beads/redirect"),
            resolved.to_string_lossy().as_bytes(),
        )
        .expect("beads redirect");
        let value = robot_value(
            fixture
                .command()
                .current_dir(&linked)
                .env("BEADS_DB", linked.join("missing.db"))
                .env("BD_DB", linked.join("missing-bd.db")),
        );
        assert_top_pick(&value, "LIVE-1");
        assert_eq!(
            fixture.calls(),
            format!(
                "export|{}|{}|unset|unset\n",
                resolved.parent().expect("repo root").display(),
                resolved.display()
            )
        );
    }

    #[test]
    fn dolt_workspace_cache_keeps_independent_repositories_separate() {
        let fixture = FakeDoltRepo::new();
        let ws_path = setup_workspace(fixture.root(), &[("api", ""), ("web", "")]);
        for (repo, id) in [("api", "API-1"), ("web", "WEB-1")] {
            let repo_path = fixture.root().join(repo);
            fs::write(repo_path.join(".beads/metadata.json"), r#"{"backend":"dolt"}"#)
                .expect("workspace Dolt metadata");
            fs::write(repo_path.join("live.jsonl"), issue_line(id, repo, "open", 1))
                .expect("independent live database");
        }
        let value = robot_value(fixture.command().arg("--workspace").arg(ws_path));
        assert_top_pick(&value, "api-API-1");
        assert_top_pick(&value, "web-WEB-1");
        assert_eq!(value["triage"]["quick_ref"]["total_open"], 2);
        assert_eq!(fixture.calls().lines().count(), 2);
    }

    #[test]
    fn dolt_workspace_fails_instead_of_emitting_partial_live_data() {
        // Exercise both strict-record rejection and a backend process failure.
        for failed_payload in [Some("not JSON\n"), None] {
            let fixture = FakeDoltRepo::new();
            let ws_path = setup_workspace(fixture.root(), &[("api", ""), ("web", "")]);
            for repo in ["api", "web"] {
                fs::write(
                    fixture.root().join(repo).join(".beads/metadata.json"),
                    r#"{"backend":"dolt"}"#,
                )
                .expect("workspace Dolt metadata");
            }
            fs::write(
                fixture.root().join("api/live.jsonl"),
                issue_line("API-1", "Valid live issue", "open", 1),
            )
            .expect("valid first live repository");
            if let Some(payload) = failed_payload {
                fs::write(fixture.root().join("web/live.jsonl"), payload)
                    .expect("malformed second live repository");
            }
            // Otherwise the absent file makes the second fake bd exit nonzero.
            let output = fixture
                .command()
                .arg("--workspace")
                .arg(ws_path)
                .arg("--robot-triage")
                .assert()
                .failure()
                .get_output()
                .clone();
            assert!(
                output.stdout.is_empty(),
                "a workspace must not publish only its successful live repositories"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("bd export") && stderr.contains("web"),
                "the failed live repository must remain visible to the caller: {stderr}"
            );
            assert_eq!(
                fixture.calls().lines().count(),
                2,
                "the fixture must load one valid repository before the live failure"
            );
        }
    }

    #[test]
    fn sqlite_metadata_outvotes_leftover_dolt_directory_and_explicit_sqlite_stays_direct() {
        let fixture = FakeDoltRepo::new();
        let database = fixture.root().join(".beads/beads.db");
        rusqlite::Connection::open(&database)
            .expect("fixture SQLite database")
            .execute_batch(
                "CREATE TABLE issues (id TEXT PRIMARY KEY, title TEXT);
                 INSERT INTO issues VALUES ('SQLITE-1', 'Current SQLite issue');",
            )
            .expect("seed SQLite fixture");
        fs::write(
            fixture.root().join(".beads/metadata.json"),
            r#"{"backend":"sqlite","database":"beads.db","jsonl_export":"issues.jsonl"}"#,
        )
        .expect("authoritative SQLite metadata");
        assert_top_pick(&robot_value(&mut fixture.command()), "SQLITE-1");
        assert_top_pick(
            &robot_value(fixture.command().arg("--beads-file").arg(database)),
            "SQLITE-1",
        );
        assert!(
            fixture.calls().is_empty(),
            "SQLite must not invoke a leftover Dolt backend"
        );
    }
}
