use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock};
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
#[allow(dead_code)]
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
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle must carry project_health summary data inside data/triage.json.",
        rationale: "The shipped viewer runtime already renders graph and velocity health directly from the exported triage payload.",
        provenance: &[
            "legacy_beads_viewer_code/beads_viewer/tests/e2e/export_pages_test.go::TestExportPages_TriageJSON",
            "legacy_beads_viewer_code/beads_viewer/pkg/export/viewer_assets/index.html::project_health dashboard widgets",
        ],
        observe_now: observe_triage_project_health,
    },
    LegacyExportContractItem {
        id: "quick-ref-count-fields",
        class: LegacyExportParityClass::MustHaveParityNow,
        description: "The export bundle must include legacy quick_ref count fields such as open_count, actionable_count, blocked_count, and in_progress_count.",
        rationale: "Legacy incremental tests and the current static dashboard both consume richer quick_ref counters than the reduced Rust payload previously exported.",
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
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set preview write timeout");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set preview read timeout");
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

fn preview_test_guard() -> MutexGuard<'static, ()> {
    static PREVIEW_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    PREVIEW_TEST_MUTEX
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_watch_startup(child: &mut std::process::Child) -> String {
    let stderr = child.stderr.as_mut().expect("watch stderr");
    let mut captured = String::new();
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];

    for _ in 0..16 {
        line.clear();
        loop {
            let bytes = stderr.read(&mut byte).expect("read watch startup");
            assert!(bytes > 0, "watch process exited before entering watch mode");
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        let line = String::from_utf8_lossy(&line);
        assert!(
            !line.is_empty(),
            "watch process exited before entering watch mode"
        );
        captured.push_str(&line);
        if line.starts_with("Watching ") {
            return captured;
        }
    }

    panic!("watch process did not advertise watch mode");
}

fn wait_for_preview_startup(child: &mut std::process::Child) -> (String, u16) {
    let stdout = child.stdout.as_mut().expect("preview stdout");
    let mut captured = String::new();
    let mut line = Vec::new();
    let mut preview_port = None;
    let mut byte = [0_u8; 1];

    for _ in 0..16 {
        line.clear();
        loop {
            let bytes = stdout.read(&mut byte).expect("read preview startup");
            assert!(
                bytes > 0,
                "preview server exited before advertising its port"
            );
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        let line = String::from_utf8_lossy(&line);
        assert!(
            !line.is_empty(),
            "preview server exited before advertising its port"
        );
        captured.push_str(&line);
        if preview_port.is_none() {
            preview_port = line
                .trim()
                .strip_prefix("Preview server running at http://127.0.0.1:")
                .and_then(|value| value.parse::<u16>().ok());
        }
        if line.trim() == "Press Ctrl+C to stop." {
            if let Some(port) = preview_port {
                return (captured, port);
            }
        }
    }

    panic!("failed to capture preview startup output: {captured}");
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
            LegacyExportParityClass::NonGoalForRustParity => {}
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
fn export_pages_uses_workspace_root_hooks_when_repo_path_discovers_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = temp.path().join("caller");
    let export_path = repo_dir.join("pages-out");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    write_test_beads(&repo_dir);
    write_hooks(
        &workspace_root,
        "hooks:\n  pre-export:\n    - name: workspace-root\n      command: 'printf workspace > hook-ran.txt'\n",
    );

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command
        .current_dir(&caller_dir)
        .arg("--repo-path")
        .arg(&nested_dir)
        .arg("--export-pages")
        .arg(&export_path);

    command.assert().success();

    assert!(export_path.join("index.html").exists());
    assert!(
        workspace_root.join("hook-ran.txt").exists(),
        "workspace root hook should run when --repo-path discovers a workspace"
    );
    assert!(
        !repo_dir.join("hook-ran.txt").exists(),
        "nested repo should not become the hook working directory"
    );
}

#[test]
fn export_pages_relative_export_path_uses_workspace_root_when_repo_path_discovers_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let nested_dir = repo_dir.join("src");
    let caller_dir = temp.path().join("caller");

    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::create_dir_all(&nested_dir).expect("create nested repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    write_test_beads(&repo_dir);
    write_hooks(
        &workspace_root,
        r#"hooks:
  pre-export:
    - name: export-path-check
      command: 'printf "%s\n" "$BV_EXPORT_PATH" > export-path.txt'
"#,
    );

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command
        .current_dir(&caller_dir)
        .arg("--repo-path")
        .arg(&nested_dir)
        .arg("--export-pages")
        .arg("pages-out");

    command.assert().success();

    assert!(workspace_root.join("pages-out/index.html").exists());
    assert!(
        !caller_dir.join("pages-out/index.html").exists(),
        "relative export path should resolve from workspace root"
    );

    let export_path =
        fs::read_to_string(workspace_root.join("export-path.txt")).expect("read export path");
    assert_eq!(
        export_path.trim(),
        workspace_root.join("pages-out").display().to_string()
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
    let _guard = preview_test_guard();
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
fn preview_pages_relative_path_uses_workspace_root_when_repo_path_discovers_workspace() {
    let _guard = preview_test_guard();
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let repo_dir = workspace_root.join("services/api");
    let caller_dir = temp.path().join("caller");
    let bundle_dir = workspace_root.join("bundle");
    fs::create_dir_all(&repo_dir).expect("create repo dir");
    fs::create_dir_all(&caller_dir).expect("create caller dir");
    fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace .bv");
    fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n",
    )
    .expect("write workspace config");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>workspace preview</body></html>",
    )
    .expect("write preview bundle");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(&caller_dir)
        .args([
            "--preview-pages",
            "bundle",
            "--repo-path",
            repo_dir.to_str().expect("repo path"),
        ])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/");
    assert!(
        response.contains("200 OK"),
        "expected preview request to succeed after workspace-root resolution: {response}"
    );
    assert!(
        preview_response_body(&response).contains("workspace preview"),
        "expected preview body from workspace-root bundle: {response}"
    );

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = format!(
        "{}{}",
        startup_stdout,
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stdout.contains(&format!("Serving bundle: {}", bundle_dir.display())));
    assert!(stdout.contains("Preview server running at http://127.0.0.1:"));
}

#[test]
fn watch_export_regenerates_after_change_and_keeps_repo_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_repo_scoped_beads(repo_dir);
    let beads_path = repo_dir.join(".beads/beads.jsonl");
    let beads_path_for_modifier = beads_path.clone();
    let (modifier_start_tx, modifier_start_rx) = mpsc::channel();

    let modifier = thread::spawn(move || {
        modifier_start_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watch startup signal");
        thread::sleep(Duration::from_millis(350));
        let mut beads = fs::OpenOptions::new()
            .append(true)
            .open(&beads_path_for_modifier)
            .expect("open beads for append");
        beads
            .write_all(
                b"{\"id\":\"BD-ALPHA-2\",\"title\":\"Alpha Two\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"source_repo\":\"alpha\"}\n",
            )
            .expect("append issue");
        beads.flush().expect("flush append");
    });

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--repo",
            "alpha",
        ])
        .env("BVR_WATCH_MAX_LOOPS", "40")
        .env("BVR_WATCH_INTERVAL_MS", "200")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn bvr watch export");

    let startup_stderr = wait_for_watch_startup(&mut child);
    modifier_start_tx
        .send(())
        .expect("send watch startup signal");
    let output = child.wait_with_output().expect("wait for watch process");
    modifier.join().expect("modifier thread");
    assert!(
        output.status.success(),
        "watch export failed with stderr: {}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = format!(
        "{}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );
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
    let workspace_path = repo_dir.join(".bv/workspace.yaml");
    let workspace_path_for_modifier = workspace_path.clone();
    let (modifier_start_tx, modifier_start_rx) = mpsc::channel();

    let modifier = thread::spawn(move || {
        modifier_start_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watch startup signal");
        thread::sleep(Duration::from_millis(350));
        fs::write(
            &workspace_path_for_modifier,
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
        .expect("update workspace");
    });

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--workspace",
            ".",
        ])
        .env("BVR_WATCH_MAX_LOOPS", "40")
        .env("BVR_WATCH_INTERVAL_MS", "200")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn workspace watch export");

    let startup_stderr = wait_for_watch_startup(&mut child);
    modifier_start_tx
        .send(())
        .expect("send watch startup signal");
    let output = child.wait_with_output().expect("wait for workspace watch");
    modifier.join().expect("modifier thread");
    assert!(
        output.status.success(),
        "workspace watch export failed with stderr: {}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = format!(
        "{}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stderr.contains("watch: regenerated"),
        "expected regeneration log in stderr, got: {stderr}"
    );

    let issues = fs::read_to_string(repo_dir.join("pages-out/data/issues.json")).expect("issues");
    assert!(issues.contains("api-AUTH-1"));
    assert!(issues.contains("web-UI-1"));
    assert!(
        stderr.contains(".bv/workspace.yaml"),
        "expected workspace config to be watched, got: {stderr}"
    );
}

#[test]
fn watch_export_regenerates_after_delete_and_recreate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    write_test_beads(repo_dir);
    let beads_path = repo_dir.join(".beads/beads.jsonl");
    let beads_path_for_modifier = beads_path.clone();
    let (modifier_start_tx, modifier_start_rx) = mpsc::channel();

    let modifier = thread::spawn(move || {
        modifier_start_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watch startup signal");
        thread::sleep(Duration::from_millis(350));
        fs::remove_file(&beads_path_for_modifier).expect("remove beads file");
        thread::sleep(Duration::from_millis(350));
        fs::write(
            &beads_path_for_modifier,
            concat!(
                "{\"id\":\"BD-1\",\"title\":\"Recreated One\",\"status\":\"open\",\"priority\":1,",
                "\"issue_type\":\"task\",\"source_repo\":\".\"}\n",
                "{\"id\":\"BD-2\",\"title\":\"Recreated Two\",\"status\":\"open\",\"priority\":2,",
                "\"issue_type\":\"task\",\"source_repo\":\".\"}\n",
            ),
        )
        .expect("rewrite beads file");
    });

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--export-pages", "pages-out", "--watch-export"])
        .env("BVR_WATCH_MAX_LOOPS", "40")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "40")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn watch export");

    let startup_stderr = wait_for_watch_startup(&mut child);
    modifier_start_tx
        .send(())
        .expect("send watch startup signal");
    let output = child.wait_with_output().expect("wait for watch process");
    modifier.join().expect("modifier thread");
    assert!(
        output.status.success(),
        "watch export failed with stderr: {}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = format!(
        "{}{}",
        startup_stderr,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stderr.contains("watch: change #"),
        "expected change detection after delete+recreate: {stderr}"
    );
    assert!(
        stderr.contains("watch: regenerated"),
        "expected regeneration after recreate: {stderr}"
    );

    let issues = fs::read_to_string(repo_dir.join("pages-out/data/issues.json")).expect("issues");
    assert!(issues.contains("BD-2"), "expected recreated data in export");
}

#[test]
fn preview_pages_reports_session_diagnostics() {
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/");
    assert!(
        response.contains("200 OK"),
        "expected preview GET to succeed: {response}"
    );

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = format!(
        "{}{}",
        startup_stdout,
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stdout.contains(&format!(
        "Preview server running at http://127.0.0.1:{actual_port}"
    )));
    assert!(stdout.contains("Serving bundle:"));
    assert!(stdout.contains("Status endpoint:"));
    assert!(stdout.contains("Reload transport: polling"));
    assert!(stdout.contains("Preview server stopped: request limit reached."));
}

#[test]
fn preview_pages_request_limit_ignores_empty_preconnects() {
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "2")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);

    let preconnect = TcpStream::connect(("127.0.0.1", actual_port))
        .expect("connect preview server for empty preconnect");
    drop(preconnect);
    thread::sleep(Duration::from_millis(50));

    let response = preview_request(actual_port, "/");
    assert!(
        response.contains("200 OK"),
        "expected first GET request to succeed after empty preconnect: {response}"
    );

    let second_response = preview_request(actual_port, "/__preview__/status");
    assert!(
        second_response.contains("200 OK"),
        "expected second GET request to succeed after empty preconnect: {second_response}"
    );

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn preview_pages_status_endpoint_reports_bundle_metadata() {
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/__preview__/status");
    let body = preview_response_body(&response);
    let payload = serde_json::from_str::<Value>(body).expect("decode status json");
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["port"], actual_port);
    assert_eq!(payload["url"], format!("http://127.0.0.1:{actual_port}"));
    assert_eq!(payload["has_index"], true);
    assert_eq!(payload["live_reload"], true);
    assert_eq!(payload["reload_mode"], "poll");
    assert_eq!(payload["file_count"], 2);
    assert_eq!(
        payload["status_url"],
        format!("http://127.0.0.1:{actual_port}/__preview__/status")
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
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle", "--no-live-reload"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/");
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

#[test]
fn preview_pages_no_live_reload_disables_reload_endpoint() {
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle", "--no-live-reload"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/.bvr/livereload");
    assert!(
        response.contains("404 Not Found"),
        "reload endpoint should be disabled when live reload is off: {response}"
    );

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview reload request failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn preview_pages_serves_directory_index_without_trailing_slash() {
    let _guard = preview_test_guard();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    let bundle_dir = repo_dir.join("bundle");
    fs::create_dir_all(bundle_dir.join("docs")).expect("create docs dir");
    fs::write(
        bundle_dir.join("index.html"),
        "<!doctype html><html><body>root</body></html>",
    )
    .expect("write root index");
    fs::write(
        bundle_dir.join("docs/index.html"),
        "<!doctype html><html><body>docs page</body></html>",
    )
    .expect("write docs index");

    let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe port");
    let port = probe.local_addr().expect("probe addr").port();
    drop(probe);

    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BVR_PREVIEW_MAX_REQUESTS", "1")
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/docs");
    assert!(
        response.contains("200 OK"),
        "expected /docs to resolve to docs/index.html: {response}"
    );
    let body = preview_response_body(&response);
    assert!(body.contains("docs page"));

    let output = child.wait_with_output().expect("wait for preview");
    assert!(
        output.status.success(),
        "preview directory request failed with stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn preview_pages_handles_sigterm_gracefully() {
    let _guard = preview_test_guard();
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
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args(["--preview-pages", "bundle"])
        .env("BVR_PREVIEW_PORT", port.to_string())
        .env("BV_NO_BROWSER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn preview");
    let (_startup_stdout, actual_port) = wait_for_preview_startup(&mut child);
    let response = preview_request(actual_port, "/");
    assert!(
        response.contains("200 OK"),
        "expected preview GET to succeed: {response}"
    );

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

#[cfg(unix)]
mod dolt_watch {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Output;

    struct FakeDoltWatch {
        directory: tempfile::TempDir,
        compatibility_snapshot: String,
    }

    impl FakeDoltWatch {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("Dolt watch fixture");
            let root = directory.path();
            fs::create_dir_all(root.join(".beads/dolt")).expect("Dolt marker directory");
            fs::create_dir(root.join("bin")).expect("fake backend executable directory");
            fs::write(root.join(".beads/metadata.json"), r#"{"backend":"dolt"}"#)
                .expect("Dolt metadata");
            let compatibility_snapshot = snapshot("Offline detail", "Offline comment");
            fs::write(root.join(".beads/issues.jsonl"), &compatibility_snapshot)
                .expect("offline compatibility snapshot");
            fs::write(
                root.join("initial.jsonl"),
                snapshot("Initial detail", "Initial comment"),
            )
            .expect("first live snapshot");
            fs::write(
                root.join("updated.jsonl"),
                snapshot("Changed detail", "Changed comment"),
            )
            .expect("later live snapshot");
            let executable = root.join("bin/bd");
            fs::write(
                &executable,
                r#"#!/bin/sh
if [ "$#" -ne 1 ] || [ "$1" != export ]; then
    printf 'expected plain bd export on stdout\n' >&2
    exit 64
fi
if [ "$BVR_TEST_DOLT_MODE" = recover-workspace ]; then
    case "$BEADS_DIR" in
        */api/.beads) repo=api ;;
        */web/.beads) repo=web ;;
        *) printf 'unexpected workspace backend\n' >&2; exit 64 ;;
    esac
    count=0
    counter="$BVR_TEST_DOLT_ROOT/$repo.calls"
    if [ -f "$counter" ]; then count=$(/bin/cat "$counter"); fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$counter"
    if [ "$repo" = api ]; then
        if [ "$count" -eq 1 ]; then
            /bin/cat "$BVR_TEST_DOLT_ROOT/api-first.jsonl"
        else
            /bin/cat "$BVR_TEST_DOLT_ROOT/api-updated.jsonl"
        fi
    elif [ "$count" -eq 2 ]; then
        printf 'fake web backend temporarily unavailable\n' >&2
        exit 7
    else
        /bin/cat "$BVR_TEST_DOLT_ROOT/web.jsonl"
    fi
    exit
fi
count=0
if [ -f "$BVR_TEST_DOLT_ROOT/calls" ]; then
    count=$(/bin/cat "$BVR_TEST_DOLT_ROOT/calls")
fi
count=$((count + 1))
printf '%s\n' "$count" > "$BVR_TEST_DOLT_ROOT/calls"
if [ "$count" -gt 1 ]; then
    case "$BVR_TEST_DOLT_MODE" in
        change)
            /bin/cat "$BVR_TEST_DOLT_ROOT/updated.jsonl"
            exit 0
            ;;
        fail)
            printf 'partial invalid output\n'
            printf 'fake live backend became unavailable\n' >&2
            exit 7
            ;;
    esac
fi
/bin/cat "$BVR_TEST_DOLT_ROOT/initial.jsonl"
"#,
            )
            .expect("fake bd for watch");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("make watch backend executable");
            Self {
                directory,
                compatibility_snapshot,
            }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn run(&self, mode: &str, explicit_directory: bool) -> Output {
            let mut command = bvr_cmd(self.root());
            command
                .args(["--export-pages", "pages-out", "--watch-export", "--no-hooks"])
                .env_remove("BEADS_DIR")
                .env_remove("BEADS_DB")
                .env_remove("BD_DB")
                .env_remove("BV_DATA_SOURCE")
                .env("PATH", self.root().join("bin"))
                .env("BVR_TEST_DOLT_ROOT", self.root())
                .env("BVR_TEST_DOLT_MODE", mode)
                .env("BVR_WATCH_MAX_LOOPS", "24")
                .env("BVR_WATCH_INTERVAL_MS", "200")
                .env("BVR_WATCH_DEBOUNCE_MS", "50")
                .timeout(Duration::from_secs(18));
            if explicit_directory {
                command.args(["--db", ".beads"]);
            }
            command.assert().success().get_output().clone()
        }

        fn issues(&self) -> Value {
            serde_json::from_slice(
                &fs::read(self.root().join("pages-out/data/issues.json"))
                    .expect("exported live issues"),
            )
            .expect("valid exported issues JSON")
        }

        fn calls(&self) -> usize {
            fs::read_to_string(self.root().join("calls"))
                .expect("backend invocation count")
                .trim()
                .parse()
                .expect("numeric invocation count")
        }

        fn assert_compatibility_snapshot_unchanged(&self) {
            assert_eq!(
                fs::read_to_string(self.root().join(".beads/issues.jsonl"))
                    .expect("offline snapshot still exists"),
                self.compatibility_snapshot,
                "watch reads must not replace the compatibility export"
            );
        }
    }

    fn snapshot(description: &str, comment: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "id": "DOLT-1",
                "title": "Stable title",
                "status": "open",
                "priority": 1,
                "issue_type": "task",
                "created_at": "2026-10-09T10:00:00Z",
                "updated_at": "2026-10-09T10:00:00Z",
                "description": description,
                "comments": [{
                    "id": 1,
                    "issue_id": "DOLT-1",
                    "author": "fixture",
                    "text": comment,
                    "created_at": "2026-10-09T10:00:00Z"
                }]
            })
        )
    }

    #[test]
    fn dolt_watch_exports_description_and_comment_changes_without_jsonl_updates() {
        // Both discovery and an explicit directory must watch the live source.
        // Every issue field in the public robot data hash stays unchanged.
        for explicit_directory in [false, true] {
            let fixture = FakeDoltWatch::new();
            let output = fixture.run("change", explicit_directory);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("watch: regenerated"),
                "live backend changes must regenerate the export: {stderr}"
            );
            let issues = fixture.issues();
            let rows = issues.as_array().expect("exported issue rows");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["id"], "DOLT-1");
            assert_eq!(rows[0]["title"], "Stable title");
            assert_eq!(rows[0]["status"], "open");
            assert_eq!(rows[0]["issue_type"], "task");
            assert_eq!(rows[0]["description"], "Changed detail");
            assert_eq!(rows[0]["comments"][0]["text"], "Changed comment");
            assert_eq!(rows[0]["updated_at"], "2026-10-09T10:00:00Z");
            assert!(fixture.calls() >= 2, "live backend must be refreshed");
            fixture.assert_compatibility_snapshot_unchanged();
        }
    }

    #[test]
    fn dolt_watch_unchanged_snapshots_are_quiet_and_backend_reads_are_cached() {
        let fixture = FakeDoltWatch::new();
        let output = fixture.run("same", false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("max loops reached"),
            "watch should complete its bounded poll loop: {stderr}"
        );
        assert!(
            !stderr.contains("watch: regenerated") && !stderr.contains("change #"),
            "unchanged live content must not trigger regeneration: {stderr}"
        );
        let calls = fixture.calls();
        assert!(
            calls >= 2,
            "the unchanged-snapshot case must include at least one real backend refresh"
        );
        assert!(
            calls < 12,
            "the cache must coalesce repeated loads across 24 polls, got {calls} exports"
        );
        assert_eq!(fixture.issues()[0]["description"], "Initial detail");
        fixture.assert_compatibility_snapshot_unchanged();
    }

    #[test]
    fn dolt_watch_retries_a_pending_workspace_change_after_an_unchanged_repo_recovers() {
        let fixture = FakeDoltWatch::new();
        let root = fixture.root();
        fs::create_dir(root.join(".bv")).expect("workspace configuration directory");
        fs::write(
            root.join(".bv/workspace.yaml"),
            concat!(
                "name: dolt-recovery\nrepos:\n",
                "  - name: api\n    path: api\n    prefix: api-\n",
                "  - name: web\n    path: web\n    prefix: web-\n",
            ),
        )
        .expect("two-repository workspace");
        for repo in ["api", "web"] {
            let beads_dir = root.join(repo).join(".beads");
            fs::create_dir_all(&beads_dir).expect("workspace beads directory");
            fs::write(beads_dir.join("metadata.json"), r#"{"backend":"dolt"}"#)
                .expect("live workspace metadata");
        }
        fs::write(
            root.join("api-first.jsonl"),
            snapshot("A1", "Stable API comment"),
        )
        .expect("initial API snapshot");
        fs::write(
            root.join("api-updated.jsonl"),
            snapshot("A2", "Stable API comment"),
        )
        .expect("changed API snapshot");
        fs::write(root.join("web.jsonl"), snapshot("B1", "Stable web comment"))
            .expect("unchanging web snapshot");

        // API changes once. Web fails its second export and then recovers with
        // its original content after the 30-second failure cache expires.
        // Neither backend changes any further content to trigger a fresh token.
        let output = bvr_cmd(root)
            .args([
                "--export-pages",
                "pages-out",
                "--watch-export",
                "--no-hooks",
                "--workspace",
                ".bv/workspace.yaml",
            ])
            .env_remove("BEADS_DIR")
            .env_remove("BEADS_DB")
            .env_remove("BD_DB")
            .env_remove("BV_DATA_SOURCE")
            .env("PATH", root.join("bin"))
            .env("BVR_TEST_DOLT_ROOT", root)
            .env("BVR_TEST_DOLT_MODE", "recover-workspace")
            .env("BVR_WATCH_MAX_LOOPS", "120")
            .env("BVR_WATCH_INTERVAL_MS", "300")
            .env("BVR_WATCH_DEBOUNCE_MS", "50")
            .timeout(Duration::from_secs(45))
            .assert()
            .success()
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("reload failed"),
            "the test must encounter a failed complete-workspace reload: {stderr}"
        );
        assert!(
            stderr.contains("watch: regenerated"),
            "a pending change must be published after the unchanged repo recovers: {stderr}"
        );
        let web_calls: usize = fs::read_to_string(root.join("web.calls"))
            .expect("web backend call counter")
            .trim()
            .parse()
            .expect("numeric web call count");
        assert!(
            web_calls >= 3,
            "the web backend must recover after its second call fails"
        );
        let issues = fixture.issues();
        let rows = issues.as_array().expect("complete workspace export");
        assert_eq!(rows.len(), 2, "both live repositories must remain present");
        let api = rows
            .iter()
            .find(|row| row["id"] == "api-DOLT-1")
            .expect("exported API issue");
        let web = rows
            .iter()
            .find(|row| row["id"] == "web-DOLT-1")
            .expect("exported web issue");
        assert_eq!(api["description"], "A2");
        assert_eq!(web["description"], "B1");
        assert_eq!(api["updated_at"], "2026-10-09T10:00:00Z");
        assert_eq!(web["updated_at"], "2026-10-09T10:00:00Z");
        for repo in ["api", "web"] {
            assert!(
                !root.join(repo).join(".beads/issues.jsonl").exists(),
                "live recovery must not manufacture a compatibility snapshot"
            );
        }
    }

    #[test]
    fn dolt_watch_backend_failure_keeps_last_good_export_and_backs_off() {
        let fixture = FakeDoltWatch::new();
        let output = fixture.run("fail", false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("bd export"),
            "a failed refresh must be visible while the last export stays available: {stderr}"
        );
        assert!(
            !stderr.contains("watch: regenerated"),
            "a failed live read must not publish stale or partial data: {stderr}"
        );
        assert_eq!(
            fixture.calls(),
            2,
            "one initial success and one failed refresh should enter the failure backoff"
        );
        assert_eq!(fixture.issues()[0]["description"], "Initial detail");
        assert_eq!(fixture.issues()[0]["comments"][0]["text"], "Initial comment");
        fixture.assert_compatibility_snapshot_unchanged();
    }
}
