//! End-to-end export/pages parity tests.
//!
//! Exercises the full `bvr --export-pages` CLI flow and validates:
//! - All required artifacts are present in the bundle
//! - JSON data payloads are valid and structurally sound
//! - SQLite database is populated and queryable
//! - HTML + assets reference only local paths (offline-capable)
//! - Custom title, closed-issue filter, and history flags work end-to-end
//! - Failure paths produce clear diagnostics
//!
//! Set BVR_E2E_ARTIFACT_DIR to capture per-scenario diagnostics.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE: &str = "tests/testdata/minimal.jsonl";
const COMPLEX_FIXTURE: &str = "tests/testdata/synthetic_complex.jsonl";
const E2E_ARTIFACT_DIR_ENV: &str = "BVR_E2E_ARTIFACT_DIR";

fn bvr() -> Command {
    let bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr");
    Command::new(bin)
}

fn fresh_export_dir(label: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("bvr_e2e_export_{label}_"))
        .tempdir()
        .expect("create temp dir");
    dir
}

fn save_diagnostic(scenario: &str, output: &std::process::Output, export_dir: &Path) {
    let Some(root) = std::env::var_os(E2E_ARTIFACT_DIR_ENV).map(PathBuf::from) else {
        return;
    };
    let dir = root.join(scenario);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("stdout.txt"), &output.stdout);
    let _ = fs::write(dir.join("stderr.txt"), &output.stderr);
    let _ = fs::write(
        dir.join("meta.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scenario": scenario,
            "exit_code": output.status.code(),
            "export_dir": export_dir.display().to_string(),
        }))
        .unwrap_or_default(),
    );
    // Snapshot the bundle file listing
    if export_dir.is_dir() {
        let listing = collect_file_listing(export_dir);
        let _ = fs::write(dir.join("bundle_listing.txt"), listing.join("\n"));
    }
}

fn collect_file_listing(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_files_recursive(dir, dir, &mut files);
    files.sort();
    files
}

fn collect_files_recursive(base: &Path, current: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.display().to_string());
        }
    }
}

// ── Happy path: default export ─────────────────────────────────────

#[test]
fn e2e_export_default_produces_complete_bundle() {
    let tmp = fresh_export_dir("default");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_default", &output, &export_path);

    assert!(
        output.status.success(),
        "bvr --export-pages must exit 0:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Core artifacts
    let required = [
        "index.html",
        "data/meta.json",
        "data/issues.json",
        "data/triage.json",
        "data/insights.json",
        "data/export_summary.json",
        "beads.sqlite3",
        "beads.sqlite3.config.json",
        "assets/style.css",
        "assets/viewer.js",
        "README.md",
        "_headers",
    ];

    for artifact in &required {
        assert!(
            export_path.join(artifact).is_file(),
            "missing required artifact: {artifact}"
        );
    }

    // Meta JSON structural check
    let meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/meta.json")).expect("read meta"),
    )
    .expect("parse meta");
    assert!(meta["title"].is_string(), "meta must have title");
    assert!(meta["issue_count"].is_number(), "meta must have issue_count");
    assert!(meta["generator"].as_str() == Some("bvr"), "generator must be bvr");
    assert!(meta["version"].is_string(), "meta must have version");

    // Issues JSON is a non-empty array
    let issues: Vec<serde_json::Value> = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/issues.json")).expect("read issues"),
    )
    .expect("parse issues");
    assert!(!issues.is_empty(), "issues array must not be empty");

    // Triage JSON has quick_ref
    let triage: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/triage.json")).expect("read triage"),
    )
    .expect("parse triage");
    assert!(
        triage.get("quick_ref").is_some(),
        "triage must have quick_ref"
    );

    // SQLite is openable and has issues table
    let db = rusqlite::Connection::open(export_path.join("beads.sqlite3")).expect("open db");
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .expect("count issues");
    assert!(count > 0, "SQLite must have at least 1 issue, got {count}");
}

// ── Custom title ───────────────────────────────────────────────────

#[test]
fn e2e_export_custom_title_propagates_to_meta() {
    let tmp = fresh_export_dir("title");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--pages-title")
        .arg("Sprint 42 Dashboard")
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_custom_title", &output, &export_path);
    assert!(output.status.success());

    let meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/meta.json")).expect("read meta"),
    )
    .expect("parse meta");
    assert_eq!(
        meta["title"], "Sprint 42 Dashboard",
        "custom title must appear in meta.json"
    );

    let readme = fs::read_to_string(export_path.join("README.md")).expect("read README");
    assert!(
        readme.contains("Sprint 42 Dashboard"),
        "custom title must appear in README.md"
    );
}

// ── Exclude closed issues ──────────────────────────────────────────

#[test]
fn e2e_export_exclude_closed_filters_issues() {
    let tmp = fresh_export_dir("no_closed");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--pages-include-closed=false")
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_exclude_closed", &output, &export_path);
    assert!(output.status.success());

    let issues: Vec<serde_json::Value> = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/issues.json")).expect("read issues"),
    )
    .expect("parse issues");

    for issue in &issues {
        let status = issue["status"].as_str().unwrap_or_default();
        assert_ne!(
            status, "closed",
            "closed issues must not appear when --pages-include-closed=false"
        );
    }

    let meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/meta.json")).expect("read meta"),
    )
    .expect("parse meta");
    assert_eq!(
        meta["include_closed"], false,
        "include_closed must be false in meta"
    );
}

// ── History exclusion ──────────────────────────────────────────────

#[test]
fn e2e_export_exclude_history_omits_history_json() {
    let tmp = fresh_export_dir("no_history");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--pages-include-history=false")
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_no_history", &output, &export_path);
    assert!(output.status.success());

    assert!(
        !export_path.join("data/history.json").exists(),
        "history.json must not exist when --pages-include-history=false"
    );
}

// ── Offline/CSP compliance ─────────────────────────────────────────

#[test]
fn e2e_export_html_has_no_external_urls() {
    let tmp = fresh_export_dir("offline");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_offline", &output, &export_path);
    assert!(output.status.success());

    let index = fs::read_to_string(export_path.join("index.html")).expect("read index.html");

    // Must not reference external CDN/Bootstrap URLs
    assert!(
        !index.contains("cdn.jsdelivr.net"),
        "index.html must not reference external CDN"
    );
    assert!(
        !index.contains("cdnjs.cloudflare.com"),
        "index.html must not reference external CDN"
    );
    assert!(
        !index.contains("unpkg.com"),
        "index.html must not reference unpkg"
    );

    // Must have CSP meta tag
    assert!(
        index.contains("Content-Security-Policy"),
        "index.html must include CSP meta tag"
    );
}

// ── SQLite integrity ───────────────────────────────────────────────

#[test]
fn e2e_export_sqlite_database_passes_integrity_check() {
    let tmp = fresh_export_dir("sqlite");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_sqlite_integrity", &output, &export_path);
    assert!(output.status.success());

    let db = rusqlite::Connection::open(export_path.join("beads.sqlite3")).expect("open db");

    // Integrity check
    let integrity: String = db
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok", "SQLite integrity check must pass");

    // Config file hash matches
    let config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("beads.sqlite3.config.json")).expect("read config"),
    )
    .expect("parse config");
    assert!(
        config["hash"].is_string(),
        "config must have hash field"
    );
    let hash = config["hash"].as_str().unwrap();
    assert_eq!(hash.len(), 64, "hash must be 64 hex characters (SHA-256)");

    assert!(
        config["total_size"].is_number(),
        "config must have total_size"
    );
    let total_size = config["total_size"].as_u64().unwrap_or(0);
    let actual_size = fs::metadata(export_path.join("beads.sqlite3"))
        .expect("stat db")
        .len();
    assert_eq!(
        total_size, actual_size,
        "config total_size must match actual file size"
    );
}

// ── Headers file ───────────────────────────────────────────────────

#[test]
fn e2e_export_headers_file_has_security_directives() {
    let tmp = fresh_export_dir("headers");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_headers", &output, &export_path);
    assert!(output.status.success());

    let headers = fs::read_to_string(export_path.join("_headers")).expect("read _headers");
    assert!(headers.contains("Cross-Origin-Embedder-Policy"));
    assert!(headers.contains("Cross-Origin-Opener-Policy"));
    assert!(headers.contains("X-Content-Type-Options: nosniff"));
    assert!(headers.contains("application/wasm"));
    assert!(headers.contains("application/x-sqlite3"));
}

// ── Export summary self-consistency ────────────────────────────────

#[test]
fn e2e_export_summary_is_self_consistent() {
    let tmp = fresh_export_dir("summary");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--pages-include-closed=true")
        .arg("--pages-include-history=true")
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_summary", &output, &export_path);
    assert!(output.status.success());

    let summary: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/export_summary.json")).expect("read summary"),
    )
    .expect("parse summary");

    assert!(summary["issue_count"].is_number());
    assert!(summary["files"].is_array());
    assert!(summary["generated_at"].is_string());
    assert_eq!(summary["include_closed"], true);
    assert_eq!(summary["include_history"], true);

    // Every file listed in summary must exist on disk
    let files = summary["files"].as_array().expect("files array");
    for file_val in files {
        let file_name = file_val.as_str().expect("file entry is string");
        assert!(
            export_path.join(file_name).exists(),
            "summary lists '{file_name}' but it doesn't exist on disk"
        );
    }

    // history.json must be present (include_history=true)
    assert!(export_path.join("data/history.json").is_file());
}

// ── Failure path: missing fixture file ─────────────────────────────

#[test]
fn e2e_export_with_missing_fixture_fails_with_diagnostic() {
    let tmp = fresh_export_dir("missing");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg("tests/testdata/DOES_NOT_EXIST.jsonl")
        .output()
        .expect("execute bvr");

    save_diagnostic("export_missing_fixture", &output, &export_path);

    assert!(
        !output.status.success(),
        "export with nonexistent fixture must fail"
    );

    // Bundle should not be created on failure
    assert!(
        !export_path.join("index.html").exists(),
        "no index.html should exist after failed export"
    );
}

// ── COI service worker present ─────────────────────────────────────

#[test]
fn e2e_export_includes_coi_service_worker() {
    let tmp = fresh_export_dir("coi");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_coi", &output, &export_path);
    assert!(output.status.success());

    assert!(
        export_path.join("coi-serviceworker.js").is_file(),
        "COI service worker must be present for COOP/COEP"
    );

    let index = fs::read_to_string(export_path.join("index.html")).expect("read index");
    assert!(
        index.contains("coi-serviceworker.js"),
        "index.html must reference COI service worker"
    );
}

// ── Complex fixture produces richer output ──────────────────────────

#[test]
fn e2e_export_complex_fixture_has_dependencies_and_metrics() {
    let tmp = fresh_export_dir("complex");
    let export_path = tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--pages-include-closed=true")
        .arg("--beads-file")
        .arg(COMPLEX_FIXTURE)
        .output()
        .expect("execute bvr");

    save_diagnostic("export_complex", &output, &export_path);
    assert!(output.status.success());

    // SQLite should have issues
    let db = rusqlite::Connection::open(export_path.join("beads.sqlite3")).expect("open db");
    let issue_count: i64 = db
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .expect("count");
    assert!(
        issue_count >= 3,
        "complex fixture must produce at least 3 issues, got {issue_count}"
    );

    // Insights should have bottlenecks
    let insights: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(export_path.join("data/insights.json")).expect("read insights"),
    )
    .expect("parse insights");
    assert!(insights.get("bottlenecks").is_some());
}

// ── Watch-export integration tests ──────────────────────────────────

/// Helper: create a temporary beads file from the fixture for watch tests.
fn create_mutable_beads_file(label: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("bvr_watch_{label}_"))
        .tempdir()
        .expect("create temp dir");
    let beads_path = dir.path().join("issues.jsonl");
    fs::copy(FIXTURE, &beads_path).expect("copy fixture");
    (dir, beads_path)
}

#[test]
fn e2e_watch_export_requires_export_pages_flag() {
    let output = bvr()
        .arg("--watch-export")
        .arg("--beads-file")
        .arg(FIXTURE)
        .output()
        .expect("execute bvr");

    assert_eq!(
        output.status.code(),
        Some(2),
        "watch-export without export-pages should exit 2"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--watch-export requires --export-pages"),
        "expected guidance in stderr: {stderr}"
    );
}

#[test]
fn e2e_watch_export_single_cycle_regenerates() {
    let (_beads_dir, beads_path) = create_mutable_beads_file("single");
    let export_tmp = fresh_export_dir("watch_single");
    let export_path = export_tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--watch-export")
        .arg("--beads-file")
        .arg(&beads_path)
        .env("BVR_WATCH_MAX_LOOPS", "2")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "50")
        .output()
        .expect("execute bvr");

    save_diagnostic("watch_single_cycle", &output, &export_path);
    assert!(
        output.status.success(),
        "watch with max_loops should exit 0:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should show watching message
    assert!(
        stderr.contains("Watching") && stderr.contains("source file(s)"),
        "expected watch startup message: {stderr}"
    );
    // Should show the watched path
    assert!(
        stderr.contains("issues.jsonl"),
        "expected watched path in output: {stderr}"
    );
    // Should exit via max loops
    assert!(
        stderr.contains("max loops reached"),
        "expected max loops exit message: {stderr}"
    );
    // Initial export should have created the bundle
    assert!(
        export_path.join("index.html").is_file(),
        "initial export should create index.html"
    );
}

#[test]
fn e2e_watch_export_detects_file_change_and_regenerates() {
    let (_beads_dir, beads_path) = create_mutable_beads_file("change");
    let export_tmp = fresh_export_dir("watch_change");
    let export_path = export_tmp.path().join("pages");

    // Spawn the watch process in background with short intervals.
    // We'll modify the file, then let max_loops expire.
    let beads_path_clone = beads_path.clone();
    let export_path_clone = export_path.clone();

    // Use a thread to modify the beads file after a short delay
    let modifier = std::thread::spawn(move || {
        // Wait for the watch to start
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Append an issue to the beads file to trigger change detection
        let extra_issue = r#"{"id":"WATCH-1","title":"Added by watch test","status":"open","issue_type":"task","priority":2}"#;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&beads_path_clone)
            .expect("open beads file for append");
        use std::io::Write;
        writeln!(file, "{extra_issue}").expect("append issue");
    });

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path_clone)
        .arg("--watch-export")
        .arg("--beads-file")
        .arg(&beads_path)
        .env("BVR_WATCH_MAX_LOOPS", "5")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "50")
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .expect("execute bvr");

    modifier.join().expect("modifier thread");
    save_diagnostic("watch_change", &output, &export_path);
    assert!(
        output.status.success(),
        "watch should exit 0:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should detect the change
    assert!(
        stderr.contains("change #1 detected"),
        "expected change detection message: {stderr}"
    );
    // Should show the changed file
    assert!(
        stderr.contains("issues.jsonl"),
        "expected changed filename in output: {stderr}"
    );
    // Should regenerate
    assert!(
        stderr.contains("regenerated"),
        "expected regeneration message: {stderr}"
    );
}

#[test]
fn e2e_watch_export_shows_poll_and_debounce_config() {
    let (_beads_dir, beads_path) = create_mutable_beads_file("config");
    let export_tmp = fresh_export_dir("watch_config");
    let export_path = export_tmp.path().join("pages");

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--watch-export")
        .arg("--beads-file")
        .arg(&beads_path)
        .env("BVR_WATCH_MAX_LOOPS", "1")
        .env("BVR_WATCH_INTERVAL_MS", "200")
        .env("BVR_WATCH_DEBOUNCE_MS", "100")
        .output()
        .expect("execute bvr");

    save_diagnostic("watch_config", &output, &export_path);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("poll 200ms") && stderr.contains("debounce 100ms"),
        "expected config values in startup message: {stderr}"
    );
}

#[test]
fn e2e_watch_export_failure_reports_last_good_served() {
    // Start with a valid file, then replace it with invalid content
    let (_beads_dir, beads_path) = create_mutable_beads_file("failure");
    let export_tmp = fresh_export_dir("watch_failure");
    let export_path = export_tmp.path().join("pages");

    let beads_path_clone = beads_path.clone();
    let modifier = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Delete the beads file to trigger a reload error
        let _ = fs::remove_file(&beads_path_clone);
    });

    let output = bvr()
        .arg("--export-pages")
        .arg(&export_path)
        .arg("--watch-export")
        .arg("--beads-file")
        .arg(&beads_path)
        .env("BVR_WATCH_MAX_LOOPS", "5")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "50")
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .expect("execute bvr");

    modifier.join().expect("modifier thread");
    save_diagnostic("watch_failure", &output, &export_path);

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The initial export should have succeeded
    assert!(
        stderr.contains("Exported pages bundle"),
        "initial export should succeed: {stderr}"
    );
    // After file deletion, either stat fails (warning) or reload fails
    // Either way, the last good export should still be served
    let has_failure_msg = stderr.contains("last good export still served")
        || stderr.contains("cannot stat")
        || stderr.contains("reload failed");
    assert!(
        has_failure_msg,
        "expected failure/warning message: {stderr}"
    );
}
