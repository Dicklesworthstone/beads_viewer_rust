use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

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

fn run_watch_export_with_update(repo_dir: &Path, args: &[&str], update: impl FnOnce()) -> String {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut child = ProcessCommand::new(bvr_bin)
        .current_dir(repo_dir)
        .args([
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--no-hooks",
        ])
        .args(args)
        .env_remove("BEADS_DIR")
        .env_remove("BEADS_DB")
        .env_remove("BD_DB")
        .env_remove("BV_DATA_SOURCE")
        .env("BVR_WATCH_MAX_LOOPS", "20")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "20")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn bounded watch export");
    let stderr = child.stderr.take().expect("watch stderr");
    let (startup_tx, startup_rx) = mpsc::channel();
    let stderr_reader = thread::spawn(move || {
        let mut captured = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("read watch stderr");
            if line.starts_with("Watching ") {
                let _ = startup_tx.send(());
            }
            captured.push_str(&line);
            captured.push('\n');
        }
        captured
    });

    if startup_rx.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = stderr_reader.join().expect("watch stderr reader");
        panic!("watch did not start within its deadline: {stderr}");
    }
    update();

    let deadline = Instant::now() + Duration::from_secs(10);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll watch process") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break (child.wait().expect("reap timed out watch process"), true);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stderr = stderr_reader.join().expect("watch stderr reader");
    assert!(!timed_out, "watch exceeded its deadline: {stderr}");
    assert!(status.success(), "watch export failed: {stderr}");
    stderr
}

fn watch_detail_snapshot(description: &str, comment: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "id": "WATCH-1",
            "title": "Stable issue",
            "status": "open",
            "priority": 1,
            "issue_type": "task",
            "description": description,
            "updated_at": "2026-10-10T10:00:00Z",
            "comments": [{
                "id": 1,
                "issue_id": "WATCH-1",
                "author": "fixture",
                "text": comment,
                "created_at": "2026-10-10T10:00:00Z"
            }]
        })
    )
}

fn assert_watch_bundle_details(repo_dir: &Path, description: &str, comment: &str) {
    let issues: Value = serde_json::from_slice(
        &fs::read(repo_dir.join("pages-out/data/issues.json")).expect("exported JSON snapshot"),
    )
    .expect("valid exported JSON");
    assert_eq!(issues.as_array().expect("issue array").len(), 1);
    assert_eq!(issues[0]["id"], "WATCH-1");
    assert_eq!(issues[0]["description"], description);
    assert_eq!(issues[0]["comments"][0]["text"], comment);
    let database = Connection::open(repo_dir.join("pages-out/beads.sqlite3"))
        .expect("open actual exported database");
    let stored_description: String = database
        .query_row(
            "SELECT description FROM issues WHERE id = 'WATCH-1'",
            [],
            |row| row.get(0),
        )
        .expect("exported issue description");
    let stored_comment: String = database
        .query_row(
            "SELECT text FROM comments WHERE issue_id = 'WATCH-1'",
            [],
            |row| row.get(0),
        )
        .expect("exported issue comment");
    assert_eq!(stored_description, description);
    assert_eq!(stored_comment, comment);
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
#[cfg(unix)]
fn watch_export_detects_source_changes_during_initial_export() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir(root.join(".beads")).expect("create beads directory");
    fs::write(
        root.join(".beads/issues.jsonl"),
        watch_detail_snapshot("Before export", "Before comment"),
    )
    .expect("initial source");
    let updated = watch_detail_snapshot("During export", "During comment");
    fs::write(root.join("updated.jsonl"), &updated).expect("updated source fixture");
    write_hooks(
        root,
        r#"hooks:
  pre-export:
    - name: update-source-once
      command: 'printf x >> hook-calls; if [ ! -f source-updated ]; then cp updated.jsonl .beads/issues.jsonl && printf done > source-updated; fi'
  post-export:
    - name: retain-first-snapshot
      command: 'if [ ! -f initial-export.json ]; then cp "$BV_EXPORT_PATH/data/issues.json" initial-export.json; fi'
"#,
    );

    let output = bvr_cmd(root)
        .args([
            "--db",
            ".beads",
            "--export-pages",
            "pages-out",
            "--watch-export",
        ])
        .env_remove("BEADS_DIR")
        .env_remove("BEADS_DB")
        .env_remove("BD_DB")
        .env_remove("BV_DATA_SOURCE")
        .env("BVR_WATCH_MAX_LOOPS", "8")
        .env("BVR_WATCH_INTERVAL_MS", "100")
        .env("BVR_WATCH_DEBOUNCE_MS", "20")
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .get_output()
        .clone();
    let initial: Value = serde_json::from_slice(
        &fs::read(root.join("initial-export.json")).expect("initial exported snapshot"),
    )
    .expect("valid initial snapshot");
    assert_eq!(initial[0]["description"], "Before export");
    assert_eq!(initial[0]["comments"][0]["text"], "Before comment");
    assert_eq!(
        fs::read_to_string(root.join(".beads/issues.jsonl")).expect("unchanged later source"),
        updated,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("watch: regenerated"),
        "an edit during the initial export must trigger a later export: {stderr}"
    );
    assert_watch_bundle_details(root, "During export", "During comment");
    assert_eq!(
        fs::read_to_string(root.join("hook-calls")).expect("hook invocation count"),
        "xx",
        "the initial export and one changed snapshot should each run hooks once"
    );
}

#[test]
fn watch_export_preserves_label_and_repo_scope_after_refresh() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir(root.join(".beads")).expect("create beads directory");
    let source = root.join(".beads/issues.jsonl");
    let mut rows = vec![
        serde_json::json!({
            "id": "API-1", "title": "Selected issue", "status": "open",
            "priority": 1, "issue_type": "task", "source_repo": "api",
            "labels": ["publish"], "description": "Before refresh",
            "dependencies": [{"issue_id": "API-1", "depends_on_id": "API-DEP", "type": "blocks"}]
        }),
        serde_json::json!({
            "id": "API-DEP", "title": "Unlabeled dependency", "status": "open",
            "priority": 1, "issue_type": "task", "source_repo": "api"
        }),
        serde_json::json!({
            "id": "API-OTHER", "title": "Unrelated issue", "status": "open",
            "priority": 1, "issue_type": "task", "source_repo": "api", "labels": ["internal"]
        }),
        serde_json::json!({
            "id": "WEB-1", "title": "Other repository", "status": "open",
            "priority": 1, "issue_type": "task", "source_repo": "web", "labels": ["publish"]
        }),
    ];
    let encode = |rows: &[Value]| {
        use std::fmt::Write as _;

        let mut snapshot = String::new();
        for row in rows {
            writeln!(snapshot, "{row}").expect("serialize JSONL fixture");
        }
        snapshot
    };
    fs::write(&source, encode(&rows)).expect("initial scoped source");
    let expected_ids = vec!["API-1".to_string(), "API-DEP".to_string()];
    let stderr =
        run_watch_export_with_update(root, &["--repo", "api", "--label", "publish"], || {
            let initial: Value = serde_json::from_slice(
                &fs::read(root.join("pages-out/data/issues.json")).expect("initial scoped export"),
            )
            .expect("initial issues JSON");
            let mut initial_ids = initial
                .as_array()
                .expect("initial array")
                .iter()
                .map(|row| row["id"].as_str().expect("issue ID").to_string())
                .collect::<Vec<_>>();
            initial_ids.sort();
            assert_eq!(initial_ids, expected_ids);
            rows[0]["description"] = Value::String("After refresh".to_string());
            fs::write(&source, encode(&rows)).expect("update matching issue after watch startup");
        });
    assert!(stderr.contains("watch: regenerated"), "{stderr}");
    let final_issues: Value = serde_json::from_slice(
        &fs::read(root.join("pages-out/data/issues.json")).expect("refreshed scoped export"),
    )
    .expect("final issues JSON");
    let mut final_ids = final_issues
        .as_array()
        .expect("final array")
        .iter()
        .map(|row| row["id"].as_str().expect("issue ID").to_string())
        .collect::<Vec<_>>();
    final_ids.sort();
    assert_eq!(final_ids, expected_ids, "refresh must preserve both scopes");
    let database = Connection::open(root.join("pages-out/beads.sqlite3"))
        .expect("open refreshed SQLite export");
    let mut query = database
        .prepare("SELECT id FROM issues ORDER BY id")
        .expect("issue IDs query");
    let ids = query
        .query_map([], |row| row.get::<_, String>(0))
        .expect("exported issue IDs")
        .collect::<Result<Vec<_>, _>>()
        .expect("read exported issue IDs");
    assert_eq!(
        ids, expected_ids,
        "SQLite export must retain the same scope"
    );
    let description: String = database
        .query_row(
            "SELECT description FROM issues WHERE id = 'API-1'",
            [],
            |row| row.get(0),
        )
        .expect("updated scoped issue");
    assert_eq!(description, "After refresh");
}

#[test]
fn watch_export_detects_preserved_mtime_jsonl_fallback_updates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir(root.join(".beads")).expect("create beads directory");
    let source = root.join(".beads/snapshot.jsonl");
    let database_path = root.join(".beads/beads.db");
    fs::write(
        root.join(".beads/metadata.json"),
        r#"{"database":"beads.db","jsonl_export":"snapshot.jsonl"}"#,
    )
    .expect("declared source metadata");
    let initial = watch_detail_snapshot("Older detail", "Older comment");
    let updated = watch_detail_snapshot("Newer detail", "Newer comment");
    assert_eq!(
        initial.len(),
        updated.len(),
        "only the content fingerprint may change"
    );
    fs::write(&source, &initial).expect("fallback source");
    fs::write(&database_path, b"not a SQLite database").expect("bad preferred database");
    let source_time = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let database_time = std::time::SystemTime::now() + Duration::from_secs(86_400);
    fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("open source timestamp")
        .set_modified(source_time)
        .expect("set older source timestamp");
    fs::OpenOptions::new()
        .write(true)
        .open(&database_path)
        .expect("open database timestamp")
        .set_modified(database_time)
        .expect("set preferred database timestamp");

    let stderr = run_watch_export_with_update(root, &["--db", ".beads"], || {
        assert_watch_bundle_details(root, "Older detail", "Older comment");
        fs::write(&source, &updated).expect("rewrite effective fallback data");
        fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .expect("restore source timestamp")
            .set_modified(source_time)
            .expect("preserve fallback mtime");
    });
    assert!(
        stderr.contains("falling back to the JSONL export"),
        "{stderr}"
    );
    assert!(stderr.contains("watch: regenerated"), "{stderr}");
    assert_watch_bundle_details(root, "Newer detail", "Newer comment");
    assert_eq!(
        fs::metadata(&source)
            .expect("source metadata")
            .modified()
            .expect("source mtime"),
        source_time
    );
    assert_eq!(
        fs::read(&database_path).expect("database remains untouched"),
        b"not a SQLite database"
    );
    assert!(
        fs::metadata(&database_path)
            .expect("database metadata")
            .modified()
            .expect("database mtime")
            > source_time
    );

    let strict = bvr_cmd(root)
        .args([
            "--db",
            ".beads",
            "--export-pages",
            "strict-out",
            "--watch-export",
        ])
        .env_remove("BEADS_DIR")
        .env_remove("BEADS_DB")
        .env_remove("BD_DB")
        .env("BV_DATA_SOURCE", "sqlite")
        .timeout(Duration::from_secs(10))
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    assert!(
        !String::from_utf8_lossy(&strict.stderr).contains("falling back"),
        "explicit SQLite mode must reject the bad database"
    );
    assert!(
        !root.join("strict-out").exists(),
        "strict failure must not export JSONL data"
    );
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

mod publication {
    use super::*;
    use bvr::export_sqlite::{SqliteBootstrapConfig, SqliteBundleOptions, emit_bootstrap_config};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    fn export_command(root: &Path) -> Command {
        let mut command = bvr_cmd(root);
        command
            .args(["--export-pages", "pages-out"])
            .env_remove("BEADS_DIR")
            .env_remove("BEADS_DB")
            .env_remove("BD_DB")
            .env_remove("BV_DATA_SOURCE")
            .timeout(Duration::from_secs(20));
        command
    }

    fn update_source(root: &Path) {
        fs::write(
            root.join(".beads/beads.jsonl"),
            "{\"id\":\"NEW-1\",\"title\":\"Updated issue\",\"description\":\"New export contents\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("update export source");
    }

    fn snapshot_files(bundle: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut paths = BTreeSet::new();
        collect_files_recursive(bundle, bundle, &mut paths);
        paths
            .into_iter()
            .map(|path| {
                let bytes = fs::read(bundle.join(&path)).expect("snapshot published file");
                (path, bytes)
            })
            .collect()
    }

    fn assert_unchanged(bundle: &Path, before: &BTreeMap<String, Vec<u8>>) {
        let after = snapshot_files(bundle);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "a failed export changed the published file inventory"
        );
        for (path, bytes) in before {
            assert!(
                after.get(path) == Some(bytes),
                "a failed export changed published bytes in {path}"
            );
        }
    }

    fn assert_updated_bundle(bundle: &Path) {
        let issues: Value = serde_json::from_slice(
            &fs::read(bundle.join("data/issues.json")).expect("published issue JSON"),
        )
        .expect("valid published issues");
        assert_eq!(issues.as_array().expect("issue list").len(), 1);
        assert_eq!(issues[0]["id"], "NEW-1");
        let connection =
            Connection::open(bundle.join("beads.sqlite3")).expect("published database");
        let id: String = connection
            .query_row("SELECT id FROM issues", [], |row| row.get(0))
            .expect("published issue row");
        assert_eq!(id, "NEW-1");
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("database integrity check");
        assert_eq!(integrity, "ok");
        drop(connection);

        let bytes = fs::read(bundle.join("beads.sqlite3")).expect("complete database bytes");
        let config: SqliteBootstrapConfig = serde_json::from_slice(
            &fs::read(bundle.join("beads.sqlite3.config.json")).expect("database config"),
        )
        .expect("valid database config");
        assert_eq!(
            config.total_size,
            u64::try_from(bytes.len()).expect("database size")
        );
        assert_eq!(config.hash, format!("{:x}", Sha256::digest(&bytes)));
        assert!(
            !config.chunked,
            "small replacement database should be a single file"
        );
        let summary: Value = serde_json::from_slice(
            &fs::read(bundle.join("data/export_summary.json")).expect("published summary"),
        )
        .expect("valid published summary");
        assert_eq!(summary["export_path"].as_str(), bundle.to_str());
    }

    #[test]
    fn export_pages_locked_database_preserves_bundle_and_recovers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_test_beads(root);
        export_command(root).arg("--no-hooks").assert().success();
        let bundle = root.join("pages-out");
        let before = snapshot_files(&bundle);
        let database = Connection::open(bundle.join("beads.sqlite3")).expect("lock database");
        database
            .execute_batch("BEGIN EXCLUSIVE")
            .expect("hold database lock");
        update_source(root);

        export_command(root)
            .arg("--no-hooks")
            .assert()
            .failure()
            .stderr(predicate::str::contains("database is locked"));
        assert_unchanged(&bundle, &before);

        database
            .execute_batch("ROLLBACK")
            .expect("release database lock");
        drop(database);
        export_command(root).arg("--no-hooks").assert().success();
        assert_updated_bundle(&bundle);
    }

    #[test]
    fn export_pages_unsupported_schema_preserves_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_test_beads(root);
        export_command(root).arg("--no-hooks").assert().success();
        let bundle = root.join("pages-out");
        let database = Connection::open(bundle.join("beads.sqlite3")).expect("existing database");
        database
            .pragma_update(None, "user_version", 99)
            .expect("unsupported schema version");
        drop(database);
        let before = snapshot_files(&bundle);
        update_source(root);

        export_command(root)
            .arg("--no-hooks")
            .assert()
            .failure()
            .stderr(predicate::str::contains("schema version 99"));
        assert_unchanged(&bundle, &before);
    }

    #[test]
    fn export_pages_wal_database_is_rejected_without_changing_published_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_test_beads(root);
        export_command(root).arg("--no-hooks").assert().success();
        let bundle = root.join("pages-out");
        let database = Connection::open(bundle.join("beads.sqlite3")).expect("existing database");
        database
            .pragma_update(None, "journal_mode", "WAL")
            .expect("external WAL conversion");
        drop(database);
        let before = snapshot_files(&bundle);
        update_source(root);

        export_command(root)
            .arg("--no-hooks")
            .assert()
            .failure()
            .stderr(predicate::str::contains("requires DELETE journal mode"));
        assert_unchanged(&bundle, &before);
    }

    #[test]
    fn export_pages_regeneration_preserves_custom_files_and_removes_stale_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_test_beads(root);
        write_hooks(
            root,
            "hooks:\n  pre-export:\n    - name: preserve-user-file\n      command: 'mkdir -p \"$BV_EXPORT_PATH\" && printf preserved > \"$BV_EXPORT_PATH/pre-hook.txt\"'\n",
        );
        export_command(root).assert().success();
        let bundle = root.join("pages-out");
        assert!(bundle.join("data/history.json").is_file());
        // Exercise a real prior chunk inventory while keeping the fixture small.
        let old_config = emit_bootstrap_config(
            &bundle,
            &SqliteBundleOptions {
                chunk_threshold_bytes: 0,
                chunk_size_bytes: 4096,
            },
        )
        .expect("emit prior chunked database");
        assert!(old_config.chunked);
        assert!(!old_config.chunks.is_empty());
        fs::create_dir_all(bundle.join(".git")).expect("output repository metadata");
        let custom_files = [
            (".git/config", "repository metadata"),
            ("CNAME", "issues.example.test"),
            ("data/custom.json", "{\"user\":true}"),
            ("assets/custom.css", "body { color: teal; }"),
            ("chunks/custom.bin", "unrelated chunk-directory content"),
            ("pre-hook.txt", "preserved"),
        ];
        for (path, bytes) in custom_files {
            if path != "pre-hook.txt" {
                fs::write(bundle.join(path), bytes).expect("write custom output file");
            }
        }
        let wasm =
            fs::read(bundle.join("vendor/sql-wasm.wasm")).expect("original SQLite WebAssembly");
        update_source(root);

        export_command(root)
            .arg("--pages-include-history=false")
            .assert()
            .success();

        for (path, bytes) in custom_files {
            assert_eq!(
                fs::read(bundle.join(path)).expect("preserved custom output file"),
                bytes.as_bytes(),
                "regeneration must preserve {path}"
            );
        }
        assert!(!bundle.join("data/history.json").exists());
        for chunk in old_config.chunks {
            assert!(
                !bundle.join(&chunk.path).exists(),
                "stale chunk {} survived",
                chunk.path
            );
        }
        assert_eq!(
            fs::read(bundle.join("vendor/sql-wasm.wasm")).expect("published WebAssembly"),
            wasm
        );
        assert_updated_bundle(&bundle);
    }

    #[test]
    fn export_pages_directory_collision_preserves_existing_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write_test_beads(root);
        export_command(root).arg("--no-hooks").assert().success();
        let bundle = root.join("pages-out");
        fs::rename(bundle.join("README.md"), root.join("saved-readme.md"))
            .expect("retain original readme");
        fs::create_dir(bundle.join("README.md")).expect("conflicting user directory");
        fs::write(bundle.join("README.md/user-file.txt"), b"keep this file")
            .expect("conflicting directory contents");
        let before = snapshot_files(&bundle);
        update_source(root);

        export_command(root).arg("--no-hooks").assert().failure();
        assert_unchanged(&bundle, &before);
        assert!(bundle.join("README.md").is_dir());
    }

    #[test]
    #[cfg(unix)]
    fn export_pages_managed_symlinks_do_not_modify_external_paths() {
        use std::os::unix::fs::symlink;

        for relative in ["data", "beads.sqlite3", "viewer.js"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path();
            write_test_beads(root);
            export_command(root).arg("--no-hooks").assert().success();
            let bundle = root.join("pages-out");
            let outside = root.join("outside");
            fs::create_dir(&outside).expect("outside directory");
            let target = outside.join("original");
            fs::rename(bundle.join(relative), &target).expect("retain original managed path");
            let before = snapshot_files(&outside);
            symlink(&target, bundle.join(relative)).expect("managed symlink");
            update_source(root);

            export_command(root).arg("--no-hooks").assert().failure();
            assert_unchanged(&outside, &before);
            assert_eq!(
                fs::read_link(bundle.join(relative)).expect("retained symlink"),
                target
            );
        }
    }
}

mod preview_transfers {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::net::{Shutdown, SocketAddr};
    use std::process::Child;

    // Keep every new socket/process test bounded, including failure paths: a
    // failed body assertion must not leave a preview server running in CI.
    struct PreviewProcess {
        child: Child,
        stdout: Option<thread::JoinHandle<String>>,
        stderr: Option<thread::JoinHandle<String>>,
        port: u16,
    }

    impl PreviewProcess {
        fn start(bundle: &Path, max_requests: usize) -> Self {
            Self::start_with_live_reload(bundle, max_requests, false)
        }

        fn start_with_live_reload(bundle: &Path, max_requests: usize, live_reload: bool) -> Self {
            let probe = TcpListener::bind(("127.0.0.1", 0)).expect("reserve preview port");
            let port = probe.local_addr().expect("preview address").port();
            drop(probe);
            let mut command = ProcessCommand::new(assert_cmd::cargo::cargo_bin!("bvr"));
            command.arg("--preview-pages").arg(bundle);
            if !live_reload {
                command.arg("--no-live-reload");
            }
            let mut child = command
                .env("BVR_PREVIEW_PORT", port.to_string())
                .env("BVR_PREVIEW_MAX_REQUESTS", max_requests.to_string())
                .env("BV_NO_BROWSER", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn preview");
            let stdout = child.stdout.take().expect("preview stdout");
            let stderr = child.stderr.take().expect("preview stderr");
            let (ready_tx, ready_rx) = mpsc::channel();
            let stdout = thread::spawn(move || {
                let mut captured = String::new();
                let mut actual_port = None;
                for line in BufReader::new(stdout).lines() {
                    let line = line.expect("read preview stdout");
                    if let Some(raw) =
                        line.strip_prefix("Preview server running at http://127.0.0.1:")
                    {
                        actual_port = raw.parse::<u16>().ok();
                    }
                    if line == "Press Ctrl+C to stop." {
                        let _ = ready_tx.send(actual_port.expect("advertised preview port"));
                    }
                    captured.push_str(&line);
                    captured.push('\n');
                }
                captured
            });
            let stderr = thread::spawn(move || {
                let mut captured = String::new();
                BufReader::new(stderr)
                    .read_to_string(&mut captured)
                    .expect("read preview stderr");
                captured
            });
            let mut server = Self {
                child,
                stdout: Some(stdout),
                stderr: Some(stderr),
                port: 0,
            };
            match ready_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(port) => server.port = port,
                Err(error) => {
                    let _ = server.child.kill();
                    let _ = server.child.wait();
                    let (stdout, stderr) = server.take_logs();
                    panic!("preview startup failed: {error}\n{stdout}\n{stderr}");
                }
            }
            server
        }

        fn take_logs(&mut self) -> (String, String) {
            let stdout = self
                .stdout
                .take()
                .map_or_else(String::new, |reader| reader.join().expect("stdout reader"));
            let stderr = self
                .stderr
                .take()
                .map_or_else(String::new, |reader| reader.join().expect("stderr reader"));
            (stdout, stderr)
        }

        fn finish(&mut self, reason: &str, timeout: Duration) -> String {
            let deadline = Instant::now() + timeout;
            let (status, timed_out) = loop {
                if let Some(status) = self.child.try_wait().expect("poll preview") {
                    break (status, false);
                }
                if Instant::now() >= deadline {
                    let _ = self.child.kill();
                    break (self.child.wait().expect("reap timed out preview"), true);
                }
                thread::sleep(Duration::from_millis(20));
            };
            let (stdout, stderr) = self.take_logs();
            assert!(
                !timed_out,
                "preview shutdown exceeded {timeout:?}\n{stdout}\n{stderr}"
            );
            assert!(
                status.success(),
                "preview exited with {status}\n{stdout}\n{stderr}"
            );
            assert!(
                stdout.contains(reason),
                "missing shutdown reason {reason:?}\n{stdout}\n{stderr}"
            );
            stderr
        }

        fn finish_requests(&mut self) {
            let stderr = self.finish("request limit reached", Duration::from_secs(5));
            assert!(
                !stderr.contains("preview request failed"),
                "unexpected transfer failure: {stderr}"
            );
        }

        #[cfg(unix)]
        fn terminate(&mut self) {
            let status = ProcessCommand::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .expect("send SIGTERM");
            assert!(status.success(), "failed to send SIGTERM");
            self.finish("received shutdown signal", Duration::from_secs(3));
        }
    }

    impl Drop for PreviewProcess {
        fn drop(&mut self) {
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
            if let Some(reader) = self.stdout.take() {
                let _ = reader.join();
            }
            if let Some(reader) = self.stderr.take() {
                let _ = reader.join();
            }
        }
    }

    struct BinaryResponse {
        headers: String,
        body: Vec<u8>,
    }

    impl BinaryResponse {
        fn header(&self, name: &str) -> &str {
            self.headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.trim()))
                .unwrap_or_else(|| panic!("missing {name} header: {}", self.headers))
        }

        fn assert_body(&self, expected: &[u8]) {
            assert_eq!(
                self.body.len(),
                expected.len(),
                "preview truncated the response body"
            );
            assert!(self.body == expected, "preview corrupted the response body");
            assert_eq!(Sha256::digest(&self.body), Sha256::digest(expected));
        }
    }

    fn connect(port: u16) -> TcpStream {
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(5),
        )
        .expect("connect preview");
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("write timeout");
        stream
    }

    fn request(port: u16, method: &str, path: &str) -> TcpStream {
        let mut stream = connect(port);
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("send request");
        stream
    }

    fn read_response(stream: &mut TcpStream, head: bool, paced: bool) -> BinaryResponse {
        let response = read_complete_response(stream, head, paced);
        assert!(
            response.headers.starts_with("HTTP/1.1 200 OK\r\n"),
            "unexpected headers: {}",
            response.headers
        );
        response
    }

    fn read_error_response(stream: &mut TcpStream, status: &str) -> BinaryResponse {
        let response = read_complete_response(stream, false, false);
        assert!(
            response.headers.starts_with(&format!("HTTP/1.1 {status} ")),
            "unexpected error response: {}",
            response.headers
        );
        response
    }

    fn read_complete_response(stream: &mut TcpStream, head: bool, paced: bool) -> BinaryResponse {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut response = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            assert!(
                Instant::now() < deadline,
                "preview response exceeded its deadline"
            );
            let bytes = stream
                .read(&mut buffer)
                .expect("read binary preview response");
            if bytes == 0 {
                break;
            }
            response.extend_from_slice(&buffer[..bytes]);
            if paced {
                thread::sleep(Duration::from_millis(1));
            }
        }
        let header_end = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .expect("HTTP response headers")
            + 4;
        let body = response.split_off(header_end);
        let headers = String::from_utf8(response).expect("UTF-8 HTTP headers");
        let response = BinaryResponse { headers, body };
        let advertised: usize = response
            .header("Content-Length")
            .parse()
            .expect("numeric Content-Length");
        assert_eq!(
            response.body.len(),
            if head { 0 } else { advertised },
            "advertised Content-Length does not match the delivered body"
        );
        response
    }

    fn expect_no_response(stream: &mut TcpStream) {
        expect_no_response_for(stream, Duration::from_millis(100));
    }

    fn expect_no_response_for(stream: &mut TcpStream, duration: Duration) {
        stream
            .set_read_timeout(Some(duration))
            .expect("short response probe timeout");
        let mut byte = [0];
        let result = stream.read(&mut byte);
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("restore response timeout");
        assert!(
            matches!(result, Err(ref error)
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "preview replied before the request could be served: {result:?}, byte={byte:?}"
        );
    }

    fn fragmented_request(port: u16, fragments: &[&[u8]]) -> TcpStream {
        let mut stream = connect(port);
        let (last, prefix) = fragments.split_last().expect("request fragments");
        for fragment in prefix {
            stream.write_all(fragment).expect("send request fragment");
            expect_no_response(&mut stream);
        }
        stream.write_all(last).expect("complete request headers");
        stream
    }

    fn write_bundle(bundle: &Path) {
        fs::create_dir_all(bundle.join("chunks")).expect("create bundle");
        fs::write(
            bundle.join("index.html"),
            b"<!doctype html><html><body>transfer test</body></html>\n",
        )
        .expect("write index");
    }

    fn export_large_bundle(root: &Path) -> std::path::PathBuf {
        fs::create_dir_all(root.join(".beads")).expect("create beads");
        let mut input = fs::File::create(root.join(".beads/beads.jsonl")).expect("beads input");
        let description = "preview database payload ".repeat(4096);
        for index in 0..64 {
            serde_json::to_writer(&mut input, &serde_json::json!({
                "id": format!("TRANSFER-{index}"), "title": format!("Transfer issue {index}"),
                "status": "open", "priority": 2, "issue_type": "task", "description": description,
            })).expect("write issue");
            writeln!(input).expect("issue newline");
        }
        drop(input);
        Command::new(assert_cmd::cargo::cargo_bin!("bvr"))
            .current_dir(root)
            .args([
                "--export-pages",
                "bundle",
                "--no-hooks",
                "--pages-include-history=false",
            ])
            .env_remove("BEADS_DIR")
            .env_remove("BEADS_DB")
            .env_remove("BD_DB")
            .env_remove("BV_DATA_SOURCE")
            .timeout(Duration::from_secs(45))
            .assert()
            .success();
        root.join("bundle")
    }

    fn payload(size: usize) -> Vec<u8> {
        (0..size).map(|offset| (offset % 251) as u8).collect()
    }

    fn read_headers(stream: &mut TcpStream) {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            assert!(header.len() < 8192, "oversized response headers");
            let mut byte = [0];
            stream.read_exact(&mut byte).expect("read response header");
            header.push(byte[0]);
        }
        assert!(header.starts_with(b"HTTP/1.1 200 OK\r\n"));
    }

    #[test]
    fn preview_pages_waits_for_complete_fragmented_get_head_and_status_requests() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = payload(64 * 1024 + 17);
        fs::write(temp.path().join("chunks/fragmented.bin"), &expected).expect("write asset");
        let mut server = PreviewProcess::start(temp.path(), 3);

        let mut get = fragmented_request(
            server.port,
            &[
                b"G",
                b"ET /chunks/frag",
                b"mented.bin HT",
                b"TP/1.1\r\nHost: local",
                b"host\r\n\r",
                b"\n",
            ],
        );
        read_response(&mut get, false, false).assert_body(&expected);

        let mut head = fragmented_request(
            server.port,
            &[
                b"H",
                b"EAD /chunks/fragmented.bin HTTP/1.",
                b"0\r\nHost: localhost\r\n",
                b"\r",
                b"\n",
            ],
        );
        let head = read_response(&mut head, true, false);
        assert_eq!(head.header("Content-Length"), expected.len().to_string());

        let mut status = fragmented_request(
            server.port,
            &[
                b"GET /__preview__/sta",
                b"tus HTTP/1.1\r",
                b"\nHost: localhost\r\n",
                b"\r",
                b"\n",
            ],
        );
        let status = read_response(&mut status, false, false);
        let status: serde_json::Value =
            serde_json::from_slice(&status.body).expect("status response JSON");
        assert_eq!(status["status"], "running");
        assert_eq!(status["port"], server.port);
        assert_eq!(status["live_reload"], false);
        server.finish_requests();
    }

    #[test]
    fn preview_pages_rejects_malformed_and_incomplete_requests_then_recovers() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = fs::read(temp.path().join("index.html")).expect("index");
        let cases: &[(&[u8], &str)] = &[
            (b"GET /\r\n\r\n", "400"),
            (b"GET / HTTP/1.1 extra\r\n\r\n", "400"),
            (b"GET http://localhost/ HTTP/1.1\r\n\r\n", "400"),
            (b"GET / HTTP/2.0\r\n\r\n", "400"),
            (b"GET / HTTP/1.1\r\nBad Header: value\r\n\r\n", "400"),
            (b"GET / HTTP/1.1\r\nMissing-Colon\r\n\r\n", "400"),
            (b"POST / HTTP/1.1\r\nHost: localhost\r\n\r\n", "405"),
            (b"POST /\r\n\r\n", "400"),
        ];
        let mut server = PreviewProcess::start(temp.path(), cases.len() * 2 + 2);
        for &(bytes, status) in cases {
            let mut stream = connect(server.port);
            stream.write_all(bytes).expect("send invalid request");
            read_error_response(&mut stream, status);
            read_response(&mut request(server.port, "GET", "/"), false, false)
                .assert_body(&expected);
        }

        let mut incomplete = connect(server.port);
        incomplete
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .expect("send incomplete headers");
        incomplete
            .shutdown(Shutdown::Write)
            .expect("finish incomplete request");
        read_error_response(&mut incomplete, "400");
        read_response(&mut request(server.port, "GET", "/"), false, false).assert_body(&expected);
        server.finish_requests();
    }

    #[test]
    fn preview_pages_enforces_header_limit_without_rejecting_http10_or_obs_text() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = fs::read(temp.path().join("index.html")).expect("index");
        let mut server = PreviewProcess::start(temp.path(), 3);

        // The cap includes the terminator, and opaque header values must not
        // force the entire HTTP header block to be valid UTF-8.
        let mut at_limit = b"GET / HTTP/1.0\r\nX-Bytes: \xff\x80\r\nX-Pad: ".to_vec();
        at_limit.resize(8192 - 4, b'x');
        at_limit.extend_from_slice(b"\r\n\r\n");
        let mut valid = connect(server.port);
        valid.write_all(&at_limit).expect("send headers at limit");
        read_response(&mut valid, false, false).assert_body(&expected);

        // An unterminated block at the cap can no longer fit its terminator.
        // Send no surplus bytes that would turn connection close into a reset.
        let mut oversized = b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Pad: ".to_vec();
        oversized.resize(8192, b'x');
        let mut invalid = connect(server.port);
        invalid
            .write_all(&oversized)
            .expect("send oversized headers");
        read_error_response(&mut invalid, "431");
        read_response(&mut request(server.port, "GET", "/"), false, false).assert_body(&expected);
        server.finish_requests();
    }

    #[test]
    fn preview_pages_fragmented_headers_share_one_read_deadline() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = fs::read(temp.path().join("index.html")).expect("index");
        let mut server = PreviewProcess::start(temp.path(), 2);
        let mut drip = connect(server.port);
        let started = Instant::now();
        drip.write_all(b"GET / HTTP/1.1\r\nX-Drip: ")
            .expect("start incomplete headers");
        for _ in 0..3 {
            thread::sleep(Duration::from_secs(1));
            drip.write_all(b"x").expect("send another header fragment");
            expect_no_response(&mut drip);
        }
        // A per-read reset would extend the server deadline to more than eight
        // seconds. The original five-second budget must expire first.
        drip.set_read_timeout(Some(Duration::from_secs(4)))
            .expect("bounded deadline response");
        read_error_response(&mut drip, "408");
        assert!(
            started.elapsed() >= Duration::from_secs(4)
                && started.elapsed() < Duration::from_secs(8),
            "fragments changed the five-second request deadline: {:?}",
            started.elapsed()
        );
        read_response(&mut request(server.port, "GET", "/"), false, false).assert_body(&expected);
        server.finish_requests();
    }

    #[cfg(unix)]
    #[test]
    fn preview_pages_sigterm_interrupts_incomplete_request_headers() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let mut server = PreviewProcess::start(temp.path(), 0);
        let mut incomplete = connect(server.port);
        incomplete
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .expect("send incomplete headers");
        expect_no_response(&mut incomplete);
        server.terminate();
        drop(incomplete);
    }

    #[test]
    fn preview_pages_serves_large_files_completely_to_slow_readers() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = payload(4 * 1024 * 1024);
        fs::write(temp.path().join("chunks/00000.bin"), &expected).expect("write large chunk");
        let mut server = PreviewProcess::start(temp.path(), 1);
        let mut stream = request(server.port, "GET", "/chunks/00000.bin");
        // Reproduce the reporter's backpressure, then keep draining in small
        // paced reads instead of allowing a single eager read to mask it.
        thread::sleep(Duration::from_millis(300));
        let response = read_response(&mut stream, false, true);
        assert_eq!(response.header("Content-Type"), "application/octet-stream");
        response.assert_body(&expected);
        server.finish_requests();
    }

    #[test]
    fn preview_pages_completes_overlapping_large_requests() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let first = payload(4 * 1024 * 1024);
        let second = payload(2 * 1024 * 1024 + 17);
        fs::write(temp.path().join("chunks/first.bin"), &first).expect("first asset");
        fs::write(temp.path().join("chunks/second.bin"), &second).expect("second asset");
        let mut server = PreviewProcess::start(temp.path(), 2);
        let mut first_stream = request(server.port, "GET", "/chunks/first.bin");
        let mut second_stream = request(server.port, "GET", "/chunks/second.bin");
        // Both clients remain live and drain independently, so overlapping
        // browser requests must each receive their complete response.
        thread::scope(|scope| {
            let second_response = scope.spawn(|| read_response(&mut second_stream, false, false));
            thread::sleep(Duration::from_millis(300));
            read_response(&mut first_stream, false, true).assert_body(&first);
            second_response
                .join()
                .expect("second reader")
                .assert_body(&second);
        });
        server.finish_requests();
    }

    #[test]
    fn preview_pages_serves_assets_status_and_reload_while_clients_wait() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = payload(32 * 1024 + 17);
        fs::write(temp.path().join("chunks/available.bin"), &expected).expect("write asset");
        // Reserve room for both waiting clients plus three independent requests.
        // The empty connection does not count; completing the partial request
        // and a final HEAD request reach the limit of five successful requests.
        let mut server = PreviewProcess::start_with_live_reload(temp.path(), 5, true);
        let idle = connect(server.port);
        let mut partial = connect(server.port);
        partial
            .write_all(b"GET /chunks/available.bin HTTP/1.1\r\nHost: localhost\r\n")
            .expect("send incomplete request");
        expect_no_response(&mut partial);

        let started = Instant::now();
        let mut asset = request(server.port, "GET", "/chunks/available.bin");
        asset
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("asset response timeout");
        read_response(&mut asset, false, false).assert_body(&expected);

        let mut status = request(server.port, "GET", "/__preview__/status");
        status
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("status response timeout");
        let status = read_response(&mut status, false, false);
        let status: serde_json::Value =
            serde_json::from_slice(&status.body).expect("status response JSON");
        assert_eq!(status["status"], "running");
        assert_eq!(status["live_reload"], true);
        assert_eq!(status["reload_mode"], "poll");

        let mut reload = request(server.port, "GET", "/.bvr/livereload");
        reload
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("reload response timeout");
        let reload = read_response(&mut reload, false, false);
        assert_eq!(reload.body.len(), 64, "reload fingerprint length");
        assert!(reload.body.iter().all(u8::is_ascii_hexdigit));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "waiting clients held up independent requests: {:?}",
            started.elapsed()
        );

        drop(idle);
        partial
            .write_all(b"\r\n")
            .expect("complete waiting request");
        read_response(&mut partial, false, false).assert_body(&expected);
        let head = read_response(
            &mut request(server.port, "HEAD", "/chunks/available.bin"),
            true,
            false,
        );
        assert_eq!(head.header("Content-Length"), expected.len().to_string());
        server.finish_requests();
    }

    #[test]
    fn preview_pages_serves_other_requests_during_a_stalled_large_response() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let first = payload(16 * 1024 * 1024);
        let second = payload(4 * 1024 * 1024 + 17);
        fs::write(temp.path().join("chunks/stalled.bin"), &first).expect("first asset");
        fs::write(temp.path().join("chunks/independent.bin"), &second).expect("second asset");
        let mut server = PreviewProcess::start(temp.path(), 3);
        let mut stalled = request(server.port, "GET", "/chunks/stalled.bin");
        let mut byte = [0];
        assert_eq!(stalled.peek(&mut byte).expect("first response began"), 1);
        // A watch rebuild may overwrite the same file while this reader is
        // stalled. The response that already began must keep its original bytes.
        fs::write(
            temp.path().join("chunks/stalled.bin"),
            vec![0xa5; first.len()],
        )
        .expect("rewrite asset during transfer");

        // Do not drain the first response: it is larger than normal TCP socket
        // buffers. Other workers must finish without waiting for that reader.
        let started = Instant::now();
        let mut independent = request(server.port, "GET", "/chunks/independent.bin");
        independent
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("independent response timeout");
        read_response(&mut independent, false, false).assert_body(&second);
        let mut status = request(server.port, "GET", "/__preview__/status");
        status
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("status response timeout");
        let status = read_response(&mut status, false, false);
        let status: serde_json::Value =
            serde_json::from_slice(&status.body).expect("status response JSON");
        assert_eq!(status["status"], "running");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "stalled response held up independent requests: {:?}",
            started.elapsed()
        );

        read_response(&mut stalled, false, false).assert_body(&first);
        server.finish_requests();
    }

    #[test]
    fn preview_pages_reserves_request_limit_slots_and_releases_failed_connections() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = fs::read(temp.path().join("index.html")).expect("index");
        fs::File::create(temp.path().join("large.bin"))
            .expect("large file")
            .set_len(16 * 1024 * 1024)
            .expect("large file size");

        for interrupted in [false, true] {
            let mut server = PreviewProcess::start(temp.path(), 1);
            let mut occupied = if interrupted {
                request(server.port, "GET", "/large.bin")
            } else {
                connect(server.port)
            };
            if interrupted {
                read_headers(&mut occupied);
            }
            let mut queued = [
                request(server.port, "GET", "/"),
                request(server.port, "GET", "/"),
            ];
            for client in &mut queued {
                expect_no_response(client);
            }

            occupied
                .shutdown(Shutdown::Both)
                .expect("release reserved connection");
            drop(occupied);
            let stderr = server.finish("request limit reached", Duration::from_secs(5));
            assert_eq!(
                stderr.contains("preview request failed"),
                interrupted,
                "unexpected connection outcome: {stderr}"
            );

            // One queued request may use the released reservation. The other
            // stays unaccepted and ends with EOF/reset when the listener closes.
            let mut delivered = 0;
            for client in &mut queued {
                client
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("closed connection timeout");
                let mut byte = [0];
                match client.peek(&mut byte) {
                    Ok(0) => {}
                    Ok(_) => {
                        read_response(client, false, false).assert_body(&expected);
                        delivered += 1;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) => {}
                    Err(error) => panic!("queued request did not finish: {error}"),
                }
            }
            assert_eq!(
                delivered, 1,
                "request limit must allow exactly one completed response"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn preview_pages_bounds_active_workers_and_interrupts_all_on_sigterm() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let expected = fs::read(temp.path().join("index.html")).expect("index");
        fs::File::create(temp.path().join("large.bin"))
            .expect("large file")
            .set_len(16 * 1024 * 1024)
            .expect("large file size");
        let mut server = PreviewProcess::start(temp.path(), 0);
        let mut blocked = Vec::new();
        for _ in 0..8 {
            let mut writer = request(server.port, "GET", "/large.bin");
            // Header arrival acknowledges admission to a worker, while the
            // undrained body keeps that worker occupied under backpressure.
            read_headers(&mut writer);
            blocked.push(writer);
        }

        let mut ninth = request(server.port, "GET", "/");
        expect_no_response_for(&mut ninth, Duration::from_millis(500));
        let released = blocked.pop().expect("one occupied worker");
        released
            .shutdown(Shutdown::Both)
            .expect("release worker capacity");
        drop(released);
        ninth
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("released capacity timeout");
        read_response(&mut ninth, false, false).assert_body(&expected);

        let mut partial = connect(server.port);
        partial
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .expect("occupy remaining worker with incomplete headers");
        expect_no_response(&mut partial);
        server.terminate();
        // Keep all peers open until shutdown has interrupted and joined both
        // the seven blocked writers and the incomplete request reader.
        drop(blocked);
        drop(partial);
    }

    #[test]
    fn preview_pages_preserves_exported_database_chunks_and_wasm() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = export_large_bundle(temp.path());
        let config_bytes =
            fs::read(bundle.join("beads.sqlite3.config.json")).expect("bootstrap config");
        let config: bvr::export_sqlite::SqliteBootstrapConfig =
            serde_json::from_slice(&config_bytes).expect("config schema");
        assert!(
            config.chunked && config.chunks.len() > 1,
            "fixture must exercise actual chunked export"
        );
        let mut server = PreviewProcess::start(&bundle, config.chunks.len() + 6);
        read_response(
            &mut request(server.port, "GET", "/beads.sqlite3.config.json"),
            false,
            false,
        )
        .assert_body(&config_bytes);
        let database = read_response(
            &mut request(server.port, "GET", "/beads.sqlite3"),
            false,
            false,
        );
        assert_eq!(database.header("Content-Type"), "application/x-sqlite3");
        assert_eq!(database.body.len() as u64, config.total_size);
        assert_eq!(format!("{:x}", Sha256::digest(&database.body)), config.hash);
        database.assert_body(&fs::read(bundle.join("beads.sqlite3")).expect("database on disk"));
        let mut reassembled = Vec::new();
        for chunk in &config.chunks {
            let response = read_response(
                &mut request(server.port, "GET", &format!("/{}", chunk.path)),
                false,
                false,
            );
            assert_eq!(response.body.len() as u64, chunk.size);
            assert_eq!(format!("{:x}", Sha256::digest(&response.body)), chunk.hash);
            response.assert_body(&fs::read(bundle.join(&chunk.path)).expect("exported chunk"));
            reassembled.extend_from_slice(&response.body);
        }
        database.assert_body(&reassembled);
        let reassembled_path = temp.path().join("served.sqlite3");
        fs::write(&reassembled_path, &reassembled).expect("save served database");
        let conn = Connection::open(&reassembled_path).expect("open served database");
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("database integrity");
        assert_eq!(integrity, "ok");
        let count: i64 = conn
            .query_row("SELECT count(*) FROM issues", [], |row| row.get(0))
            .expect("served issue count");
        assert_eq!(count, 64);
        for (path, mime) in [
            ("vendor/sql-wasm.wasm", "application/wasm"),
            (
                "vendor/mermaid.min.js",
                "application/javascript; charset=utf-8",
            ),
        ] {
            let expected = fs::read(bundle.join(path)).expect("embedded vendor asset");
            assert!(
                expected.len() > 327_680,
                "asset must exceed reported truncation boundary"
            );
            let response = read_response(
                &mut request(server.port, "GET", &format!("/{path}")),
                false,
                false,
            );
            assert_eq!(response.header("Content-Type"), mime);
            response.assert_body(&expected);
        }
        for path in ["beads.sqlite3", "vendor/sql-wasm.wasm"] {
            let response = read_response(
                &mut request(server.port, "HEAD", &format!("/{path}")),
                true,
                false,
            );
            assert_eq!(
                response
                    .header("Content-Length")
                    .parse::<u64>()
                    .expect("HEAD length"),
                fs::metadata(bundle.join(path))
                    .expect("asset metadata")
                    .len()
            );
        }
        server.finish_requests();
    }

    #[test]
    fn preview_pages_recovers_after_interrupted_large_response() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        fs::File::create(temp.path().join("large.bin"))
            .expect("large file")
            .set_len(32 * 1024 * 1024)
            .expect("large file size");
        let mut server = PreviewProcess::start(temp.path(), 1);
        let mut interrupted = request(server.port, "GET", "/large.bin");
        read_headers(&mut interrupted);
        let mut prefix = [0; 1024];
        interrupted.read_exact(&mut prefix).expect("response began");
        interrupted
            .shutdown(Shutdown::Both)
            .expect("interrupt transfer");
        drop(interrupted);
        read_response(&mut request(server.port, "GET", "/"), false, false)
            .assert_body(&fs::read(temp.path().join("index.html")).expect("index"));
        let stderr = server.finish("request limit reached", Duration::from_secs(5));
        assert!(
            stderr.contains("preview request failed"),
            "interrupted response should be reported: {stderr}"
        );
    }

    #[test]
    fn preview_pages_times_out_idle_connections_and_serves_next_request() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let mut server = PreviewProcess::start(temp.path(), 1);
        let mut idle = connect(server.port);
        let started = Instant::now();
        let mut byte = [0];
        assert_eq!(idle.read(&mut byte).expect("idle connection closure"), 0);
        assert!(
            started.elapsed() >= Duration::from_secs(4),
            "idle connection closed before the five-second read timeout"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "idle connection exceeded its read timeout allowance"
        );
        read_response(&mut request(server.port, "GET", "/"), false, false)
            .assert_body(&fs::read(temp.path().join("index.html")).expect("index"));
        let stderr = server.finish("request limit reached", Duration::from_secs(5));
        assert!(
            stderr.contains("preview request failed"),
            "read timeout should be reported: {stderr}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_pages_sigterm_interrupts_idle_connection() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        let mut server = PreviewProcess::start(temp.path(), 0);
        let idle = connect(server.port);
        thread::sleep(Duration::from_millis(300));
        server.terminate();
        drop(idle);
    }

    #[cfg(unix)]
    #[test]
    fn preview_pages_sigterm_interrupts_blocked_large_response() {
        let _guard = preview_test_guard();
        let temp = tempfile::tempdir().expect("tempdir");
        write_bundle(temp.path());
        fs::File::create(temp.path().join("large.bin"))
            .expect("large file")
            .set_len(32 * 1024 * 1024)
            .expect("large file size");
        let mut server = PreviewProcess::start(temp.path(), 0);
        let mut stalled = request(server.port, "GET", "/large.bin");
        read_headers(&mut stalled);
        thread::sleep(Duration::from_millis(300));
        server.terminate();
        drop(stalled);
    }
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
                .args([
                    "--export-pages",
                    "pages-out",
                    "--watch-export",
                    "--no-hooks",
                ])
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
        assert_eq!(
            fixture.issues()[0]["comments"][0]["text"],
            "Initial comment"
        );
        fixture.assert_compatibility_snapshot_unchanged();
    }
}
