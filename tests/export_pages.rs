use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;

// `bd-7oo.1.1` freezes the legacy export contract in one place. The point is not
// to cargo-cult every Go-era filename forever; it is to make the parity line
// explicit: which legacy behaviors are already satisfied, which gaps are
// intentionally deferred, and which legacy file-layout details are not part of
// the Rust contract going forward.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyExportParityClass {
    MustHaveParityNow,
    ExplicitlyDeferredParity,
    NonGoalForRustParity,
}

impl LegacyExportParityClass {
    const fn label(self) -> &'static str {
        match self {
            Self::MustHaveParityNow => "must-have-now",
            Self::ExplicitlyDeferredParity => "explicitly-deferred",
            Self::NonGoalForRustParity => "non-goal",
        }
    }

    const fn expected_present_now(self) -> bool {
        matches!(self, Self::MustHaveParityNow)
    }
}

#[derive(Debug)]
struct LegacyExportContractItem {
    id: &'static str,
    class: LegacyExportParityClass,
    description: &'static str,
    rationale: &'static str,
    provenance: &'static [&'static str],
    observe_now: fn(&ExportBundleObservation) -> bool,
}

#[derive(Debug)]
struct ExportBundleObservation {
    files: BTreeSet<String>,
    index_html: String,
    meta_json: Value,
    triage_json: Value,
    history_json: Option<Value>,
}

impl ExportBundleObservation {
    fn capture(export_dir: &Path) -> Self {
        let mut files = BTreeSet::new();
        collect_files_recursive(export_dir, export_dir, &mut files);

        Self {
            files,
            index_html: fs::read_to_string(export_dir.join("index.html")).expect("read index.html"),
            meta_json: serde_json::from_str(
                &fs::read_to_string(export_dir.join("data/meta.json"))
                    .expect("read data/meta.json"),
            )
            .expect("parse data/meta.json"),
            triage_json: serde_json::from_str(
                &fs::read_to_string(export_dir.join("data/triage.json"))
                    .expect("read data/triage.json"),
            )
            .expect("parse data/triage.json"),
            history_json: export_dir.join("data/history.json").is_file().then(|| {
                serde_json::from_str(
                    &fs::read_to_string(export_dir.join("data/history.json"))
                        .expect("read data/history.json"),
                )
                .expect("parse data/history.json")
            }),
        }
    }

    fn has_file(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn has_file_suffix(&self, suffix: &str) -> bool {
        self.files.iter().any(|path| path.ends_with(suffix))
    }
}

fn collect_files_recursive(root: &Path, current: &Path, files: &mut BTreeSet<String>) {
    for entry in fs::read_dir(current).expect("read export directory") {
        let entry = entry.expect("export dir entry");
        let path = entry.path();
        if entry.file_type().expect("export entry file type").is_dir() {
            collect_files_recursive(root, &path, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("export path under root")
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(relative);
    }
}

fn observe_index_html_root(obs: &ExportBundleObservation) -> bool {
    obs.has_file("index.html")
}

fn observe_meta_json(obs: &ExportBundleObservation) -> bool {
    obs.has_file("data/meta.json")
}

fn observe_meta_title_and_count_contract(obs: &ExportBundleObservation) -> bool {
    // meta.json carries the caller-supplied title; the canonical index.html has
    // its own static title ("Beads Viewer") and does not get patched at export
    // time — only meta.json is authoritative for the custom title.
    obs.meta_json.get("title").and_then(Value::as_str) == Some("Contract Fixture")
        && obs
            .meta_json
            .get("generated_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        && obs.meta_json.get("issue_count").and_then(Value::as_u64) == Some(2)
}

fn observe_triage_json(obs: &ExportBundleObservation) -> bool {
    obs.has_file("data/triage.json")
}

fn observe_triage_recommendation_shape(obs: &ExportBundleObservation) -> bool {
    obs.triage_json
        .get("recommendations")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
        && obs
            .triage_json
            .get("quick_ref")
            .and_then(|value| value.get("top_picks"))
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty())
}

fn observe_history_json(obs: &ExportBundleObservation) -> bool {
    obs.has_file("data/history.json")
}

fn observe_legacy_history_timeline_schema(obs: &ExportBundleObservation) -> bool {
    obs.history_json
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|object| object.contains_key("commits"))
}

fn observe_local_bootstrap_references(obs: &ExportBundleObservation) -> bool {
    // Canonical index.html uses root-level styles.css and viewer.js plus
    // vendor/* scripts — all local references, no CDN.
    obs.has_file("styles.css")
        && obs.has_file("viewer.js")
        && !obs.index_html.contains("http://")
        && !obs.index_html.contains("https://")
}

fn observe_no_external_bootstrap_urls(obs: &ExportBundleObservation) -> bool {
    !obs.index_html.contains("http://") && !obs.index_html.contains("https://")
}

fn observe_sqlite_database(obs: &ExportBundleObservation) -> bool {
    obs.has_file("beads.sqlite3")
}

fn observe_sqlite_config(obs: &ExportBundleObservation) -> bool {
    obs.has_file("beads.sqlite3.config.json")
}

fn observe_triage_project_health(obs: &ExportBundleObservation) -> bool {
    obs.triage_json.get("project_health").is_some()
}

fn observe_legacy_quick_ref_counts(obs: &ExportBundleObservation) -> bool {
    let quick_ref = obs.triage_json.get("quick_ref");
    quick_ref.is_some_and(|value| value.get("open_count").is_some())
        && quick_ref.is_some_and(|value| value.get("actionable_count").is_some())
        && quick_ref.is_some_and(|value| value.get("blocked_count").is_some())
        && quick_ref.is_some_and(|value| value.get("in_progress_count").is_some())
}

fn observe_graph_runtime_pack(obs: &ExportBundleObservation) -> bool {
    obs.has_file_suffix("graph.js")
        && obs.has_file_suffix(".wasm")
        && (obs.has_file_suffix("serviceworker.js") || obs.has_file_suffix("service-worker.js"))
}

fn observe_search_loader_assets(obs: &ExportBundleObservation) -> bool {
    obs.has_file_suffix("hybrid_scorer.js") && obs.has_file_suffix("wasm_loader.js")
}

fn observe_readme(obs: &ExportBundleObservation) -> bool {
    obs.has_file("README.md")
}

fn observe_cloudflare_headers(obs: &ExportBundleObservation) -> bool {
    obs.has_file("_headers")
}

fn observe_legacy_root_stylesheet(obs: &ExportBundleObservation) -> bool {
    obs.has_file("styles.css") || obs.index_html.contains("href=\"styles.css\"")
}

fn observe_legacy_root_viewer_script(obs: &ExportBundleObservation) -> bool {
    obs.has_file("viewer.js") || obs.index_html.contains("src=\"viewer.js\"")
}

static LEGACY_EXPORT_CONTRACT: &[LegacyExportContractItem] = &[
    LegacyExportContractItem {
        id: "index-html-entrypoint",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle must contain a root index.html entrypoint for preview and static hosts.",
        rationale: "Legacy preview, offline, and deploy flows all assume a root HTML landing page.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/cmd/bv/main.go::help text for --export-pages output",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_HTMLStructure",
        ],
        observe_now: observe_index_html_root,
    },
    LegacyExportContractItem {
        id: "meta-json",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle must emit data/meta.json for page metadata and generation context.",
        rationale: "Legacy export tests and viewer bootstrapping both expect a serialized metadata payload.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_IncludesHistoryAndRunsHooks",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_meta_json,
    },
    LegacyExportContractItem {
        id: "meta-title-and-count-contract",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The exported metadata and HTML title should carry title, generated_at, and issue_count coherently.",
        rationale: "Legacy incremental and Cloudflare tests treat title propagation and changing generation metadata as part of the operator-visible export contract.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_MetaJSON",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_cloudflare_test.go::TestCloudflare_CustomTitle",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_incremental_test.go::TestExportIncremental_AddNewIssues",
        ],
        observe_now: observe_meta_title_and_count_contract,
    },
    LegacyExportContractItem {
        id: "triage-json",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle must emit data/triage.json as the machine-readable planning payload.",
        rationale: "Legacy static pages surface recommendations and health from triage data rather than recomputing in-browser.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_TriageJSON",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_triage_json,
    },
    LegacyExportContractItem {
        id: "triage-recommendations-shape",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The current Rust parity surface must still export actionable recommendations and top picks in triage.json.",
        rationale: "Legacy export tests expect static pages to render useful planning output immediately, not an empty stub payload.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_TriageJSON",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_incremental_test.go::TestExportIncremental_AddNewIssues",
        ],
        observe_now: observe_triage_recommendation_shape,
    },
    LegacyExportContractItem {
        id: "history-json-when-enabled",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "When history export is enabled, the bundle must include data/history.json.",
        rationale: "Legacy time-travel and timeline views depend on an exported history payload, and the current Rust export already ships it.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_IncludesHistoryAndRunsHooks",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/history_timeline_e2e_test.go::TestTimelineExportStructure",
        ],
        observe_now: observe_history_json,
    },
    LegacyExportContractItem {
        id: "local-bootstrap-references",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The exported index.html must reference local stylesheet and viewer bootstrap assets.",
        rationale: "Static-host and preview flows must stay self-contained instead of depending on live asset servers.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_CSSPresent",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_JavaScriptFiles",
        ],
        observe_now: observe_local_bootstrap_references,
    },
    LegacyExportContractItem {
        id: "offline-bootstrap-without-network-urls",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The exported HTML bootstrap should remain self-contained and avoid hard-coded external URLs.",
        rationale: "Offline-capable static export is a user-facing promise, not just a test harness detail.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_NoExternalURLs",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_no_external_bootstrap_urls,
    },
    LegacyExportContractItem {
        id: "sqlite-database",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle now emits beads.sqlite3 for the static viewer workflow.",
        rationale: "Static export parity requires shipping the SQLite artifact itself rather than only reduced JSON sidecars.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_SQLiteDatabase",
            "legacy_beads_viewer_code/beads_viewer/pkg/export/integration_test.go::TestExportCreatesExpectedFiles",
        ],
        observe_now: observe_sqlite_database,
    },
    LegacyExportContractItem {
        id: "sqlite-bootstrap-config",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle now emits beads.sqlite3.config.json with deterministic bootstrap metadata.",
        rationale: "The viewer handoff contract needs an explicit config payload for file size, hash, and chunk metadata before later export work can build on it safely.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_SQLiteDatabase",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_cloudflare_test.go::TestCloudflare_SQLiteChunking",
        ],
        observe_now: observe_sqlite_config,
    },
    LegacyExportContractItem {
        id: "triage-project-health",
        class: LegacyExportParityClass::ExplicitlyDeferredParity,
        description: "Full parity still requires triage.json to carry project_health summary data.",
        rationale: "Legacy static pages expose graph and velocity health directly from the exported triage payload.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_TriageJSON",
            "legacy_beads_viewer_code/beads_viewer/pkg/export/viewer_assets/index.html::project_health dashboard widgets",
        ],
        observe_now: observe_triage_project_health,
    },
    LegacyExportContractItem {
        id: "quick-ref-count-fields",
        class: LegacyExportParityClass::ExplicitlyDeferredParity,
        description: "Full parity still requires legacy quick_ref count fields such as open_count, actionable_count, blocked_count, and in_progress_count.",
        rationale: "Legacy incremental tests and dashboards consume richer summary counters than the current reduced Rust quick_ref payload exports.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_incremental_test.go::TestExportIncremental_CloseIssues",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_incremental_test.go::TestExportIncremental_CloseBlockingIssue",
        ],
        observe_now: observe_legacy_quick_ref_counts,
    },
    LegacyExportContractItem {
        id: "history-timeline-schema",
        class: LegacyExportParityClass::ExplicitlyDeferredParity,
        description: "Full parity still requires the richer timeline-style history schema with commits and bead deltas.",
        rationale: "Legacy history export is a commit timeline payload, not just the reduced per-issue event list the current Rust exporter writes.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/history_timeline_e2e_test.go::TestTimelineExportStructure",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_IncludesHistoryAndRunsHooks",
        ],
        observe_now: observe_legacy_history_timeline_schema,
    },
    LegacyExportContractItem {
        id: "offline-graph-runtime-pack",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "Full parity still requires a local graph/search runtime pack, including graph JS, WASM, and service-worker support.",
        rationale: "Legacy offline export ships richer graph/search assets locally so the bundle works without network fetches.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_JavaScriptFiles",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_cloudflare_test.go::TestCloudflare_ServiceWorkerForCOI",
        ],
        observe_now: observe_graph_runtime_pack,
    },
    LegacyExportContractItem {
        id: "search-loader-assets",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "Full parity still requires the legacy offline search loader assets such as hybrid_scorer.js and wasm_loader.js.",
        rationale: "Legacy export tests treat the local search/runtime loader stack as part of the shipped viewer bundle.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_IncludesHistoryAndRunsHooks",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_search_loader_assets,
    },
    LegacyExportContractItem {
        id: "deploy-readme",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle emits a deploy-facing README.md with quickstart and generation metadata.",
        rationale: "Legacy export writes operator-oriented deployment context into the bundle itself for GitHub Pages workflows.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/cmd/bv/main.go::generateREADME",
            "legacy_beads_viewer_code/beads_viewer/README.md::Static site export bundle layout",
        ],
        observe_now: observe_readme,
    },
    LegacyExportContractItem {
        id: "cloudflare-headers",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle emits _headers with COOP/COEP, MIME, and cache directives for static hosts.",
        rationale: "Legacy Cloudflare export flow emits host-specific cache and content-type guidance as export artifacts.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_cloudflare_test.go::TestCloudflare_HeadersFileGenerated",
            "legacy_beads_viewer_code/beads_viewer/pkg/export/cloudflare.go::GenerateHeadersFile",
        ],
        observe_now: observe_cloudflare_headers,
    },
    LegacyExportContractItem {
        id: "legacy-root-stylesheet-name",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "Rust parity does not need to preserve the legacy root-level styles.css filename.",
        rationale: "The user-facing requirement is a local stylesheet reference, not the Go-era root path specifically.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_CSSPresent",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_legacy_root_stylesheet,
    },
    LegacyExportContractItem {
        id: "legacy-root-viewer-script-name",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "Rust parity does not need to preserve the legacy root-level viewer.js filename.",
        rationale: "The requirement is an equivalent local bootstrap script, not a byte-for-byte match to the Go bundle layout.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_JavaScriptFiles",
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_offline_test.go::TestOffline_CompleteBundleChecklist",
        ],
        observe_now: observe_legacy_root_viewer_script,
    },
];

fn write_test_beads(repo_dir: &Path) {
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"BD-OPEN\",\"title\":\"Open Issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
            "{\"id\":\"BD-CLOSED\",\"title\":\"Closed Issue\",\"status\":\"closed\",\"priority\":2,\"issue_type\":\"task\"}\n"
        ),
    )
    .expect("write beads file");
}

fn bvr_cmd(repo_dir: &Path) -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(repo_dir);
    command
}

fn write_repo_scoped_beads(repo_dir: &Path) {
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"BD-ALPHA-1\",\"title\":\"Alpha One\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"source_repo\":\"alpha\"}\n",
            "{\"id\":\"BD-BETA-1\",\"title\":\"Beta One\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"source_repo\":\"beta\"}\n"
        ),
    )
    .expect("write beads file");
}

fn write_hooks(repo_dir: &Path, yaml: &str) {
    fs::create_dir_all(repo_dir.join(".bv")).expect("create .bv");
    fs::write(repo_dir.join(".bv/hooks.yaml"), yaml).expect("write hooks");
}

fn preview_request(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect preview server");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write preview request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read preview response");
    response
}

fn preview_response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body)
        .trim()
}

#[test]
fn export_pages_writes_bundle_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    bvr_cmd(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--pages-title",
            "Sprint Dashboard",
        ])
        .assert()
        .success();

    let out = repo_dir.join("pages-out");
    assert!(out.join("index.html").is_file());
    assert!(out.join("assets/style.css").is_file());
    assert!(out.join("assets/viewer.js").is_file());
    assert!(out.join("data/issues.json").is_file());
    assert!(out.join("data/meta.json").is_file());
    assert!(out.join("data/triage.json").is_file());
    assert!(out.join("data/insights.json").is_file());
    assert!(out.join("data/history.json").is_file());
    assert!(out.join("data/export_summary.json").is_file());

    let meta = fs::read_to_string(out.join("data/meta.json")).expect("read meta.json");
    assert!(meta.contains("\"Sprint Dashboard\""));
}

#[test]
fn export_pages_populates_sqlite_database_with_core_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("create .beads");
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        concat!(
            "{\"id\":\"BD-ROOT\",\"title\":\"Root Issue\",\"description\":\"Export root\",\"design\":\"Keep SQLite rows deterministic\",\"acceptance_criteria\":\"Viewer can query the DB\",\"notes\":\"Needs a comment\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"labels\":[\"export\",\"sqlite\"],\"comments\":[{\"id\":1,\"issue_id\":\"BD-ROOT\",\"author\":\"alice\",\"text\":\"Populate the database rows\",\"created_at\":\"2026-03-08T00:00:00Z\"}],\"source_repo\":\"alpha\"}\n",
            "{\"id\":\"BD-CHILD\",\"title\":\"Child Issue\",\"status\":\"blocked\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{\"issue_id\":\"BD-CHILD\",\"depends_on_id\":\"BD-ROOT\",\"type\":\"blocks\",\"created_by\":\"tester\",\"created_at\":\"2026-03-08T01:00:00Z\"}],\"source_repo\":\"beta\"}\n"
        ),
    )
    .expect("write beads file");

    bvr_cmd(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--pages-title",
            "SQLite Fixture",
        ])
        .assert()
        .success();

    let db =
        Connection::open(repo_dir.join("pages-out/beads.sqlite3")).expect("open export sqlite");

    let issue_count: i64 = db
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .expect("query issues count");
    let dependency_count: i64 = db
        .query_row("SELECT COUNT(*) FROM dependencies", [], |row| row.get(0))
        .expect("query dependency count");
    let comment_count: i64 = db
        .query_row("SELECT COUNT(*) FROM comments", [], |row| row.get(0))
        .expect("query comment count");
    let metrics_count: i64 = db
        .query_row("SELECT COUNT(*) FROM issue_metrics", [], |row| row.get(0))
        .expect("query metrics count");
    let overview_count: i64 = db
        .query_row("SELECT COUNT(*) FROM issue_overview_mv", [], |row| {
            row.get(0)
        })
        .expect("query overview count");

    assert_eq!(issue_count, 2);
    assert_eq!(dependency_count, 1);
    assert_eq!(comment_count, 1);
    assert_eq!(metrics_count, 2);
    assert_eq!(overview_count, 2);

    let root = db
        .query_row(
            "
            SELECT source_repo, labels, dependent_count, comment_count
            FROM issue_overview_mv
            WHERE id = ?
            ",
            ["BD-ROOT"],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("query root overview");

    assert_eq!(root.0, "alpha");
    assert_eq!(root.1, "[\"export\",\"sqlite\"]");
    assert_eq!(root.2, 1);
    assert_eq!(root.3, 1);
}

#[test]
fn legacy_export_contract_inventory_is_unique_and_provenanced() {
    let mut ids = BTreeSet::new();
    let mut must_have = 0usize;
    let mut deferred = 0usize;
    let mut non_goal = 0usize;

    for item in LEGACY_EXPORT_CONTRACT {
        assert!(
            ids.insert(item.id),
            "duplicate legacy export contract id: {}",
            item.id
        );
        assert!(
            !item.description.trim().is_empty(),
            "contract item {} is missing a description",
            item.id
        );
        assert!(
            !item.rationale.trim().is_empty(),
            "contract item {} is missing rationale",
            item.id
        );
        assert!(
            !item.provenance.is_empty(),
            "contract item {} is missing legacy provenance",
            item.id
        );
        assert!(
            item.provenance.iter().all(|entry| {
                entry.contains("legacy_beads_viewer_code/beads_viewer/")
                    && (entry.contains("::") || entry.contains("README.md"))
            }),
            "contract item {} has non-specific provenance: {:?}",
            item.id,
            item.provenance
        );

        match item.class {
            LegacyExportParityClass::MustHaveParityNow => must_have += 1,
            LegacyExportParityClass::ExplicitlyDeferredParity => deferred += 1,
            LegacyExportParityClass::NonGoalForRustParity => non_goal += 1,
        }
    }

    assert!(
        must_have > 0,
        "contract should contain must-have parity items"
    );
    assert!(
        deferred > 0,
        "contract should contain explicitly deferred parity gaps"
    );
    // non-goal count may be zero once the canonical asset inventory is fully
    // shipped — the previous non-goals (root-level styles.css, viewer.js)
    // are now satisfied by the embedded viewer_assets module.
}

#[test]
fn export_pages_contract_makes_current_parity_status_explicit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    bvr_cmd(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--pages-title",
            "Contract Fixture",
        ])
        .assert()
        .success();

    let observations = ExportBundleObservation::capture(&repo_dir.join("pages-out"));
    let mut mismatches = Vec::new();

    for item in LEGACY_EXPORT_CONTRACT {
        let observed = (item.observe_now)(&observations);
        let expected = item.class.expected_present_now();
        if observed == expected {
            continue;
        }

        mismatches.push(format!(
            "[{}] {} expected present_now={} but observed={observed}. {}. Rationale: {}. Provenance: {}",
            item.class.label(),
            item.id,
            expected,
            item.description,
            item.rationale,
            item.provenance.join(" | "),
        ));
    }

    assert!(
        mismatches.is_empty(),
        "legacy export contract drifted from the documented Rust parity status:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn export_pages_can_exclude_closed_and_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    bvr_cmd(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--pages-include-closed=false",
            "--pages-include-history=false",
        ])
        .assert()
        .success();

    let out = repo_dir.join("pages-out");
    let issues = fs::read_to_string(out.join("data/issues.json")).expect("read issues.json");
    assert!(issues.contains("BD-OPEN"));
    assert!(!issues.contains("BD-CLOSED"));
    assert!(
        !out.join("data/history.json").exists(),
        "history should be omitted when --pages-include-history=false"
    );
}

#[test]
fn export_pages_runs_hooks_and_passes_export_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);
    write_hooks(
        repo_dir,
        concat!(
            "hooks:\n",
            "  pre-export:\n",
            "    - name: mark-pre\n",
            "      command: 'mkdir -p \"$BV_EXPORT_PATH\" && echo pre > \"$BV_EXPORT_PATH/pre-hook.txt\"'\n",
            "    - name: capture-env\n",
            "      command: 'printf \"%s\\n\" \"$BV_EXPORT_FORMAT|$BV_ISSUE_COUNT\" > \"$BV_EXPORT_PATH/hook-env.txt\"'\n",
            "  post-export:\n",
            "    - name: mark-post\n",
            "      command: 'echo post > \"$BV_EXPORT_PATH/post-hook.txt\"'\n",
        ),
    );

    bvr_cmd(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--pages-include-closed=false",
        ])
        .assert()
        .success();

    let out = repo_dir.join("pages-out");
    assert!(out.join("pre-hook.txt").is_file());
    assert!(out.join("post-hook.txt").is_file());
    let env_line = fs::read_to_string(out.join("hook-env.txt")).expect("read hook env");
    assert_eq!(env_line.trim(), "html|1");
}

#[test]
fn export_pages_pre_hook_failure_blocks_export() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);
    write_hooks(
        repo_dir,
        "hooks:\n  pre-export:\n    - name: fail-fast\n      command: 'exit 7'\n",
    );

    bvr_cmd(repo_dir)
        .args(["--export-pages", "pages-out"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pre-export hook"));

    assert!(
        !repo_dir.join("pages-out/index.html").exists(),
        "bundle should not be written when pre-export hook fails"
    );
}

#[test]
fn watch_export_requires_export_pages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);

    bvr_cmd(repo_dir)
        .arg("--watch-export")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--watch-export requires --export-pages <dir>",
        ));
}

#[test]
fn preview_pages_is_handled_before_issue_loading() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();

    bvr_cmd(repo_dir)
        .args(["--preview-pages", "missing-bundle"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "preview bundle directory not found",
        ))
        .stderr(predicate::str::contains("beads directory not found").not());
}

#[test]
fn watch_export_regenerates_after_change_and_keeps_repo_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_repo_scoped_beads(repo_dir);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--repo",
            "alpha",
        ])
        .env("BVR_WATCH_MAX_LOOPS", "8")
        .env("BVR_WATCH_INTERVAL_MS", "200")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn bvr watch export");

    thread::sleep(Duration::from_millis(350));
    let mut beads = fs::OpenOptions::new()
        .append(true)
        .open(repo_dir.join(".beads/beads.jsonl"))
        .expect("open beads for append");
    beads
        .write_all(
            b"{\"id\":\"BD-ALPHA-2\",\"title\":\"Alpha Two\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"source_repo\":\"alpha\"}\n",
        )
        .expect("append issue");
    beads.flush().expect("flush append");

    let output = child.wait_with_output().expect("wait for watch process");
    assert!(
        output.status.success(),
        "watch export failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("watch: regenerated"),
        "expected regeneration log in stderr, got: {stderr}"
    );

    let issues = fs::read_to_string(repo_dir.join("pages-out/data/issues.json")).expect("issues");
    assert!(issues.contains("BD-ALPHA-1"));
    assert!(issues.contains("BD-ALPHA-2"));
    assert!(
        !issues.contains("BD-BETA-1"),
        "repo filter should exclude non-target repos after watch refresh"
    );
}

#[test]
fn watch_export_regenerates_after_workspace_change() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".bv")).expect("create .bv");
    fs::create_dir_all(repo_dir.join("services/api/.beads")).expect("create api beads");
    fs::create_dir_all(repo_dir.join("apps/web/.beads")).expect("create web beads");
    fs::write(
        repo_dir.join(".bv/workspace.yaml"),
        concat!(
            "repos:\n",
            "  - name: api\n",
            "    path: services/api\n",
            "    prefix: api-\n",
            "  - name: web\n",
            "    path: apps/web\n",
            "    prefix: web-\n",
        ),
    )
    .expect("write workspace");
    fs::write(
        repo_dir.join("services/api/.beads/issues.jsonl"),
        "{\"id\":\"AUTH-1\",\"title\":\"API Auth\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
    )
    .expect("write api issues");
    fs::write(
        repo_dir.join("apps/web/.beads/issues.jsonl"),
        "{\"id\":\"UI-1\",\"title\":\"Web UI\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\"}\n",
    )
    .expect("write web issues");

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--workspace",
            ".",
        ])
        .env("BVR_WATCH_MAX_LOOPS", "8")
        .env("BVR_WATCH_INTERVAL_MS", "200")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn workspace watch export");

    thread::sleep(Duration::from_millis(350));
    let mut beads = fs::OpenOptions::new()
        .append(true)
        .open(repo_dir.join("apps/web/.beads/issues.jsonl"))
        .expect("open web beads for append");
    beads
        .write_all(
            b"{\"id\":\"UI-2\",\"title\":\"Web UI Two\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("append web issue");
    beads.flush().expect("flush append");

    let output = child.wait_with_output().expect("wait for workspace watch");
    assert!(
        output.status.success(),
        "workspace watch export failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("watch: regenerated"),
        "expected regeneration log in stderr, got: {stderr}"
    );

    let issues = fs::read_to_string(repo_dir.join("pages-out/data/issues.json")).expect("issues");
    assert!(issues.contains("api-AUTH-1"));
    assert!(issues.contains("web-UI-1"));
    assert!(issues.contains("web-UI-2"));
}

#[test]
fn preview_pages_reports_session_diagnostics() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let bundle_dir = repo_dir.join("bundle");
    fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>ok</body></html>",
    )
    .expect("write index");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");

    let mut connected = false;
    for _ in 0..40 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .expect("write request");
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(connected, "failed to connect to preview server");

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!(
        "Preview server running at http://127.0.0.1:{port}"
    )));
    assert!(stdout.contains("Serving bundle:"));
    assert!(stdout.contains("Status endpoint:"));
    assert!(stdout.contains("Reload transport: polling"));
    assert!(stdout.contains("Preview server stopped: request limit reached."));
}

#[test]
fn preview_pages_status_endpoint_reports_bundle_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let bundle_dir = repo_dir.join("bundle");
    fs::create_dir_all(bundle_dir.join("assets")).expect("create assets");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>ok</body></html>",
    )
    .expect("write index");
    fs::write(bundle_dir.join("assets/style.css"), "body{}").expect("write style");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "2")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");

    let mut response = None::<String>;
    for _ in 0..40 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            response = Some(preview_request(port, "/__preview__/status"));
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let response = response.expect("status response");
    let body = preview_response_body(&response);
    let payload = serde_json::from_str::<Value>(body).expect("decode status json");
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["port"], port);
    assert_eq!(payload["url"], format!("http://127.0.0.1:{port}"));
    assert_eq!(payload["has_index"], true);
    assert_eq!(payload["live_reload"], true);
    assert_eq!(payload["reload_mode"], "poll");
    assert_eq!(payload["file_count"], 2);
    assert_eq!(
        payload["status_url"],
        format!("http://127.0.0.1:{port}/__preview__/status")
    );
    assert_eq!(payload["reload_endpoint"], "/.bvr/livereload");
    assert!(
        payload["bundle_path"]
            .as_str()
            .is_some_and(|value| value.ends_with("bundle"))
    );

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview status request failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn preview_pages_no_live_reload_omits_reload_script() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let bundle_dir = repo_dir.join("bundle");
    fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>ok</body></html>",
    )
    .expect("write index");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle", "--no-live-reload"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "2")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");

    let mut response = None::<String>;
    for _ in 0..40 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            response = Some(preview_request(port, "/"));
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let response = response.expect("html response");
    let body = preview_response_body(&response);
    assert!(body.contains("<body>ok</body>"));
    assert!(!body.contains("window.location.reload"));

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview html request failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn preview_pages_handles_sigterm_gracefully() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let bundle_dir = repo_dir.join("bundle");
    fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>ok</body></html>",
    )
    .expect("write index");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");

    let mut connected = false;
    for _ in 0..40 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .expect("write request");
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(connected, "failed to connect to preview server");

    let status = ProcessCommand::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(status.success(), "failed to send SIGTERM");

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview did not shut down cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Preview server stopped: received shutdown signal."));
}
