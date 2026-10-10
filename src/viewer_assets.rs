//! Canonical viewer asset inventory for export bundles.
//!
//! All assets are embedded at compile time via `include_bytes!` and written
//! deterministically during export.  The manifest is sorted by output path
//! so that two exports of the same source produce identical file trees.

use std::fs;
use std::path::Path;

use crate::Result;

/// A single entry in the viewer asset inventory.
#[derive(Debug, Clone, Copy)]
pub struct AssetEntry {
    /// Relative path inside the export bundle (e.g. `"vendor/d3.v7.min.js"`).
    pub path: &'static str,
    /// Raw bytes of the asset.
    pub bytes: &'static [u8],
    /// MIME type for HTTP serving.
    pub content_type: &'static str,
}

// ---------------------------------------------------------------------------
// Embedded assets – sorted alphabetically by output path.
// ---------------------------------------------------------------------------

/// Full viewer asset inventory, sorted by path for deterministic output.
pub static ASSET_INVENTORY: &[AssetEntry] = &[
    AssetEntry {
        path: "charts.js",
        bytes: include_bytes!("../viewer_assets/charts.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "coi-serviceworker.js",
        bytes: include_bytes!("../viewer_assets/coi-serviceworker.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "graph-demo.html",
        bytes: include_bytes!("../viewer_assets/graph-demo.html"),
        content_type: "text/html; charset=utf-8",
    },
    AssetEntry {
        path: "graph.js",
        bytes: include_bytes!("../viewer_assets/graph.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "hybrid_scorer.js",
        bytes: include_bytes!("../viewer_assets/hybrid_scorer.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "index.html",
        bytes: include_bytes!("../viewer_assets/index.html"),
        content_type: "text/html; charset=utf-8",
    },
    AssetEntry {
        path: "styles.css",
        bytes: include_bytes!("../viewer_assets/styles.css"),
        content_type: "text/css; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/alpine-collapse.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/alpine-collapse.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/alpine.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/alpine.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/bv_graph.js",
        bytes: include_bytes!("../viewer_assets/vendor/bv_graph.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/bv_graph_bg.wasm",
        bytes: include_bytes!("../viewer_assets/vendor/bv_graph_bg.wasm"),
        content_type: "application/wasm",
    },
    AssetEntry {
        path: "vendor/chart.umd.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/chart.umd.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/d3.v7.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/d3.v7.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/dompurify.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/dompurify.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/force-graph.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/force-graph.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/inter-variable.woff2",
        bytes: include_bytes!("../viewer_assets/vendor/inter-variable.woff2"),
        content_type: "font/woff2",
    },
    AssetEntry {
        path: "vendor/jetbrains-mono-regular.woff2",
        bytes: include_bytes!("../viewer_assets/vendor/jetbrains-mono-regular.woff2"),
        content_type: "font/woff2",
    },
    AssetEntry {
        path: "vendor/marked.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/marked.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/mermaid.min.js",
        bytes: include_bytes!("../viewer_assets/vendor/mermaid.min.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/sql-wasm.js",
        bytes: include_bytes!("../viewer_assets/vendor/sql-wasm.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "vendor/sql-wasm.wasm",
        bytes: include_bytes!("../viewer_assets/vendor/sql-wasm.wasm"),
        content_type: "application/wasm",
    },
    AssetEntry {
        path: "vendor/tailwindcss.js",
        bytes: include_bytes!("../viewer_assets/vendor/tailwindcss.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "viewer.js",
        bytes: include_bytes!("../viewer_assets/viewer.js"),
        content_type: "application/javascript; charset=utf-8",
    },
    AssetEntry {
        path: "wasm_loader.js",
        bytes: include_bytes!("../viewer_assets/wasm_loader.js"),
        content_type: "application/javascript; charset=utf-8",
    },
];

/// Number of assets in the canonical inventory.
pub const ASSET_COUNT: usize = 24;

/// Write all viewer assets to `output_dir`, creating subdirectories as needed.
///
/// Files are written in manifest order (sorted by path) for deterministic
/// output.  Returns the list of relative paths written.
pub fn write_viewer_assets(output_dir: &Path) -> Result<Vec<String>> {
    let mut written = Vec::with_capacity(ASSET_INVENTORY.len());

    for entry in ASSET_INVENTORY {
        let dest = output_dir.join(entry.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, entry.bytes)?;
        written.push(entry.path.to_string());
    }

    Ok(written)
}

/// Look up an asset by its output path (for preview server).
pub fn lookup_asset(path: &str) -> Option<&'static AssetEntry> {
    ASSET_INVENTORY.iter().find(|e| e.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn inventory_count_matches_constant() {
        assert_eq!(
            ASSET_INVENTORY.len(),
            ASSET_COUNT,
            "ASSET_COUNT constant must match actual inventory length"
        );
    }

    #[test]
    fn inventory_paths_are_sorted() {
        let paths: Vec<&str> = ASSET_INVENTORY.iter().map(|e| e.path).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "inventory must be sorted by path");
    }

    #[test]
    fn inventory_paths_are_unique() {
        let paths: BTreeSet<&str> = ASSET_INVENTORY.iter().map(|e| e.path).collect();
        assert_eq!(
            paths.len(),
            ASSET_INVENTORY.len(),
            "inventory must not contain duplicate paths"
        );
    }

    #[test]
    fn inventory_has_no_empty_assets() {
        for entry in ASSET_INVENTORY {
            assert!(
                !entry.bytes.is_empty(),
                "asset {} must not be empty",
                entry.path
            );
        }
    }

    #[test]
    fn inventory_includes_index_html() {
        assert!(
            lookup_asset("index.html").is_some(),
            "inventory must include index.html"
        );
    }

    #[test]
    fn inventory_includes_core_viewer_files() {
        let expected = [
            "index.html",
            "viewer.js",
            "styles.css",
            "graph.js",
            "charts.js",
        ];
        for path in expected {
            assert!(
                lookup_asset(path).is_some(),
                "inventory must include {path}"
            );
        }
    }

    #[test]
    fn inventory_includes_vendor_libraries() {
        let expected_vendors = [
            "vendor/alpine.min.js",
            "vendor/d3.v7.min.js",
            "vendor/force-graph.min.js",
            "vendor/chart.umd.min.js",
            "vendor/marked.min.js",
            "vendor/mermaid.min.js",
            "vendor/dompurify.min.js",
            "vendor/sql-wasm.js",
            "vendor/sql-wasm.wasm",
            "vendor/tailwindcss.js",
            "vendor/bv_graph.js",
            "vendor/bv_graph_bg.wasm",
        ];
        for path in expected_vendors {
            assert!(
                lookup_asset(path).is_some(),
                "inventory must include {path}"
            );
        }
    }

    #[test]
    fn inventory_includes_fonts() {
        let expected_fonts = [
            "vendor/inter-variable.woff2",
            "vendor/jetbrains-mono-regular.woff2",
        ];
        for path in expected_fonts {
            assert!(
                lookup_asset(path).is_some(),
                "inventory must include {path}"
            );
        }
    }

    #[test]
    fn write_viewer_assets_creates_all_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let written = write_viewer_assets(temp.path()).expect("write assets");

        assert_eq!(written.len(), ASSET_COUNT);
        for path in &written {
            let file_path = temp.path().join(path);
            assert!(file_path.is_file(), "asset must exist: {path}");
            let content = std::fs::read(&file_path).expect("read file");
            assert!(!content.is_empty(), "asset must not be empty: {path}");
        }
    }

    #[test]
    fn write_viewer_assets_is_deterministic() {
        let temp1 = tempfile::tempdir().expect("tempdir1");
        let temp2 = tempfile::tempdir().expect("tempdir2");
        let written1 = write_viewer_assets(temp1.path()).expect("write1");
        let written2 = write_viewer_assets(temp2.path()).expect("write2");

        assert_eq!(written1, written2, "path lists must be identical");
        for path in &written1 {
            let bytes1 = std::fs::read(temp1.path().join(path)).expect("read1");
            let bytes2 = std::fs::read(temp2.path().join(path)).expect("read2");
            assert_eq!(bytes1, bytes2, "content must be identical for {path}");
        }
    }

    #[test]
    fn lookup_asset_returns_none_for_unknown() {
        assert!(lookup_asset("nonexistent.txt").is_none());
    }

    #[test]
    fn index_html_has_no_external_urls() {
        let index = lookup_asset("index.html").expect("index.html exists");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");
        assert!(
            !html.contains("http://"),
            "index.html must not reference http:// URLs"
        );
        assert!(
            !html.contains("https://"),
            "index.html must not reference https:// URLs"
        );
    }

    #[test]
    fn index_html_script_refs_resolve_to_inventory() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        // Extract src="..." and href="..." references to local asset files.
        let mut missing = Vec::new();
        for prefix in ["src=\"", "href=\""] {
            let mut search_from = 0;
            while let Some(start) = html[search_from..].find(prefix) {
                let attr_start = search_from + start;
                let abs_start = attr_start + prefix.len();
                if let Some(end) = html[abs_start..].find('"') {
                    let path = &html[abs_start..abs_start + end];
                    search_from = abs_start + end + 1;
                    // Skip fragment, data:, blob:, empty, or JS expression refs,
                    // including bound attributes (`:href="issuesHref"`).
                    if path.is_empty()
                        || (attr_start > 0 && html.as_bytes()[attr_start - 1] == b':')
                        || path.starts_with('#')
                        || path.starts_with("data:")
                        || path.starts_with("blob:")
                        || path.starts_with('\'')
                        || path.contains('+')
                    {
                        continue;
                    }
                    if lookup_asset(path).is_none() {
                        missing.push(path.to_string());
                    }
                } else {
                    break;
                }
            }
        }

        assert!(
            missing.is_empty(),
            "index.html references assets not in inventory: {missing:?}"
        );
    }

    #[test]
    fn content_security_policy_is_self_contained() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");
        // CSP meta tag restricts sources to self for offline-safe deployment
        assert!(html.contains("Content-Security-Policy"));
    }

    #[test]
    fn csp_directives_enforce_offline_safety() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        // Extract CSP content attribute value
        let csp_marker = "Content-Security-Policy";
        let csp_pos = html.find(csp_marker).expect("CSP meta tag must exist");
        let after_marker = &html[csp_pos..];
        let content_start = after_marker
            .find("content=\"")
            .expect("CSP must have content attribute");
        let content_value = &after_marker[content_start + 9..];
        let content_end = content_value.find('"').expect("CSP content must close");
        let csp = &content_value[..content_end];

        // All directives must use 'self' as the base origin
        let required_directives = [
            "default-src",
            "script-src",
            "style-src",
            "font-src",
            "img-src",
            "connect-src",
            "worker-src",
        ];
        for directive in &required_directives {
            assert!(
                csp.contains(directive),
                "CSP must include {directive} directive"
            );
        }

        // connect-src must be self-only (no external fetch allowed for offline)
        let connect_idx = csp.find("connect-src").unwrap();
        let connect_val = &csp[connect_idx..];
        let connect_end = connect_val.find(';').unwrap_or(connect_val.len());
        let connect_directive = &connect_val[..connect_end];
        assert!(
            !connect_directive.contains("http:") && !connect_directive.contains("https:"),
            "connect-src must not allow external URLs: {connect_directive}"
        );

        // font-src must be self-only (vendored fonts)
        let font_idx = csp.find("font-src").unwrap();
        let font_val = &csp[font_idx..];
        let font_end = font_val.find(';').unwrap_or(font_val.len());
        let font_directive = &font_val[..font_end];
        assert!(
            !font_directive.contains("http:") && !font_directive.contains("https:"),
            "font-src must not allow external URLs: {font_directive}"
        );

        // worker-src must include blob: (for WASM workers)
        let worker_idx = csp.find("worker-src").unwrap();
        let worker_val = &csp[worker_idx..];
        let worker_end = worker_val.find(';').unwrap_or(worker_val.len());
        let worker_directive = &worker_val[..worker_end];
        assert!(
            worker_directive.contains("blob:"),
            "worker-src must allow blob: for WASM workers: {worker_directive}"
        );
    }

    #[test]
    fn coi_service_worker_is_present_and_versioned() {
        let sw = lookup_asset("coi-serviceworker.js").expect("coi-serviceworker.js");
        let js = std::str::from_utf8(sw.bytes).expect("valid utf8");

        // Must define a versioned cache name
        assert!(
            js.contains("CACHE_NAME") || js.contains("cache"),
            "service worker must use a cache"
        );

        // Must inject cross-origin isolation headers
        assert!(
            js.contains("Cross-Origin-Embedder-Policy"),
            "service worker must set COEP header"
        );
        assert!(
            js.contains("Cross-Origin-Opener-Policy"),
            "service worker must set COOP header"
        );

        // Must handle fetch events
        assert!(
            js.contains("fetch"),
            "service worker must intercept fetch events"
        );
    }

    #[test]
    fn viewer_runtime_uses_vendored_sql_wasm_only() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("./vendor/sql-wasm.js"),
            "viewer runtime must load vendored sql-wasm.js"
        );
        assert!(
            js.contains("locateFile: file => `./vendor/${file}`"),
            "viewer runtime must resolve sql-wasm assets from the local vendor directory"
        );
        assert!(
            !js.contains("cdn.jsdelivr.net") && !js.contains("unpkg.com"),
            "viewer runtime must not fall back to external CDNs"
        );
    }

    #[test]
    fn graph_runtime_cleans_up_keyboard_shortcuts_on_reinit() {
        let graph = lookup_asset("graph.js").expect("graph.js");
        let js = std::str::from_utf8(graph.bytes).expect("valid utf8");

        assert!(
            js.contains("let keyboardShortcutHandler = null;"),
            "graph runtime must track a stable keyboard shortcut handler"
        );
        assert!(
            js.contains("document.addEventListener('keydown', keyboardShortcutHandler);"),
            "graph runtime must register keyboard shortcuts via the stable handler"
        );
        assert!(
            js.contains("document.removeEventListener('keydown', keyboardShortcutHandler);"),
            "graph runtime cleanup must remove keyboard shortcut listeners"
        );
    }

    #[test]
    fn graph_runtime_cleans_up_time_travel_styles_and_controls() {
        let graph = lookup_asset("graph.js").expect("graph.js");
        let js = std::str::from_utf8(graph.bytes).expect("valid utf8");

        assert!(
            js.contains("styleEl: null,"),
            "graph runtime must track the injected time-travel style element"
        );
        assert!(
            js.contains("timeTravelState.styleEl.remove();"),
            "graph runtime must remove leaked time-travel style elements during rebuild/cleanup"
        );
        assert!(
            js.contains("timeTravelState.controlsEl.remove();"),
            "graph runtime cleanup must remove time-travel controls"
        );
    }

    #[test]
    fn viewer_runtime_avoids_duplicate_graph_detail_surfaces() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("if (this.view === 'graph') return;"),
            "viewer runtime must keep the global graph click modal handler out of graph view"
        );
        assert!(
            js.contains("this.graphDetailNode = node;"),
            "viewer runtime must still route graph-view node clicks to the graph detail pane"
        );
    }

    #[test]
    fn viewer_runtime_binds_global_listeners_idempotently() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("globalListenersBound: false,"),
            "viewer runtime must track whether global listeners are already bound"
        );
        assert!(
            js.contains("if (!this.globalListenersBound) {"),
            "viewer runtime must guard global listener binding inside init()"
        );
        assert!(
            js.contains("this.globalListenersBound = true;"),
            "viewer runtime must mark global listeners as bound"
        );
        assert!(
            js.contains("hashChangeListenerBound: false,"),
            "viewer runtime must track hashchange listener binding separately"
        );
    }

    #[test]
    fn viewer_runtime_clears_graph_detail_state_when_leaving_graph_view() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("case 'issue':\n          // Issue detail view\n          this.view = ISSUE_BACKDROP_VIEWS.has(route.query.get('from'))\n            ? route.query.get('from')\n            : 'issues';\n          this.graphDetailNode = null;"),
            "viewer runtime must clear graph detail state before issue-detail transitions"
        );
        assert!(
            js.contains("case 'issues':\n          this.view = 'issues';\n          this.selectedIssue = null;\n          this.graphDetailNode = null;"),
            "viewer runtime must clear graph detail state before issue-list transitions"
        );
        assert!(
            js.contains("this.view = 'insights';\n          this.selectedIssue = null;\n          this.graphDetailNode = null;"),
            "viewer runtime must clear graph detail state when entering insights"
        );
        assert!(
            js.contains("this.view = 'dashboard';\n          this.selectedIssue = null;\n          this.graphDetailNode = null;"),
            "viewer runtime must clear graph detail state when returning to dashboard"
        );
    }

    #[test]
    fn viewer_runtime_tears_down_force_graph_on_route_exit_without_rebinding_bridge_listeners() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("graphBridgeListenersBound: false,"),
            "viewer runtime must track graph bridge listener binding separately from graph readiness"
        );
        assert!(
            js.contains("if (previousView === 'graph' && route.view !== 'graph') {\n        this.teardownForceGraph();\n      }"),
            "viewer runtime must tear down the force graph when leaving graph view"
        );
        assert!(
            js.contains("teardownForceGraph() {\n      if (this.forceGraphModule?.cleanup) {\n        this.forceGraphModule.cleanup();\n      }\n      this.forceGraphReady = false;"),
            "viewer runtime must call graph cleanup and reset graph readiness on teardown"
        );
        assert!(
            js.contains("if (!this.graphBridgeListenersBound) {\n          this.graphBridgeListenersBound = true;"),
            "viewer runtime must avoid rebinding graph bridge listeners after teardown"
        );
    }

    #[test]
    fn graph_runtime_cancels_deferred_callbacks_during_cleanup() {
        let graph = lookup_asset("graph.js").expect("graph.js");
        let js = std::str::from_utf8(graph.bytes).expect("valid utf8");

        assert!(
            js.contains("const pendingTimeouts = new Set();"),
            "graph runtime must track deferred timeout callbacks"
        );
        assert!(
            js.contains("function scheduleTimeout(callback, delay) {"),
            "graph runtime must route deferred callbacks through a tracked scheduler"
        );
        assert!(
            js.contains("function clearScheduledTimeouts() {"),
            "graph runtime must expose bulk timeout cleanup"
        );
        assert!(
            js.contains("scheduleTimeout(() => {\n            store.graph?.zoomToFit(400, 50);\n        }, 500);"),
            "graph runtime must guard delayed zoom-to-fit after teardown"
        );
        assert!(
            js.contains("export function cleanup() {\n    clearScheduledTimeouts();"),
            "graph cleanup must cancel deferred callbacks before tearing down graph state"
        );
        assert!(
            js.contains("export function cleanup() {\n    clearScheduledTimeouts();\n    document.removeEventListener('mousemove', positionTooltip);"),
            "graph cleanup must tear down tooltip listeners synchronously instead of scheduling fresh timeout work"
        );
    }

    #[test]
    fn viewer_runtime_uses_canonical_dashboard_route_when_closing_issue_modal() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("if (currentView === 'issues') {\n        navigateToIssues(this.filters, this.sort, this.searchQuery);\n      } else if (currentView === 'dashboard') {\n        navigateToDashboard();\n      } else {\n        navigate('/' + currentView);"),
            "viewer runtime must use the canonical dashboard route when closing issue modal from dashboard view"
        );
    }

    #[test]
    fn viewer_runtime_preserves_issue_backdrop_view_for_routed_issue_flows() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("const ISSUE_BACKDROP_VIEWS = new Set(['dashboard', 'issues', 'insights', 'graph']);"),
            "viewer runtime must define the allowed routed issue backdrop views"
        );
        assert!(
            js.contains("function navigateToIssue(id, backdropView = null) {"),
            "viewer runtime must allow routed issue navigation to carry backdrop context"
        );
        assert!(
            js.contains("const from = validBackdrop && validBackdrop !== 'issues'\n    ? `?from=${encodeURIComponent(validBackdrop)}`\n    : '';"),
            "viewer runtime must encode non-default backdrop views into the issue route"
        );
        assert!(
            js.contains("this.view = ISSUE_BACKDROP_VIEWS.has(route.query.get('from'))\n            ? route.query.get('from')\n            : 'issues';"),
            "viewer runtime must restore routed issue backdrop context from the route"
        );
        assert!(
            js.contains("showIssue(id) {\n      navigateToIssue(id, this.view);\n    },"),
            "viewer runtime must preserve the current view when opening routed issue detail"
        );
    }

    #[test]
    fn viewer_runtime_preserves_backdrop_for_mermaid_issue_navigation() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("const backdropView = this.view;"),
            "viewer runtime must capture the current backdrop view when wiring Mermaid issue links"
        );
        assert!(
            js.contains("navigateToIssue(id, backdropView);"),
            "viewer runtime must preserve backdrop context for Mermaid issue-to-issue navigation"
        );
        assert!(
            !js.contains("call window.beadsViewer.navigateToIssue("),
            "viewer runtime must not interpolate issue IDs into Mermaid callback source"
        );
    }

    #[test]
    fn viewer_runtime_limits_issue_nav_list_to_issues_view() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("function issueNavListForView(view, issues) {\n  return view === 'issues' ? issues.map(issue => issue.id) : [];\n}"),
            "viewer runtime must scope issue navigation lists to the issues view"
        );
        assert!(
            js.contains("this.issueNavList = issueNavListForView(this.view, this.issues);"),
            "viewer runtime must avoid seeding issue navigation from hidden issue-list data in other views"
        );
        assert!(
            js.contains("if (!this.issueNavList.length) {\n        return;\n      }"),
            "viewer runtime must not navigate through stale issue-list state when no valid issues-view navigation list exists"
        );
    }

    #[test]
    fn viewer_runtime_uses_backdrop_aware_issue_routes_for_modal_permalinks() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let viewer_js = std::str::from_utf8(viewer.bytes).expect("valid utf8");
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            viewer_js.contains("function issueRouteFor(id, backdropView = null) {"),
            "viewer runtime must centralize backdrop-aware issue route generation"
        );
        assert!(
            viewer_js.contains("navigate(issueRouteFor(id, backdropView));"),
            "viewer runtime must route imperative issue navigation through the shared issue route builder"
        );
        assert!(
            html.contains(":href=\"'#' + issueRouteFor(selectedIssue.id, view)\""),
            "issue modal permalink must preserve backdrop context through the shared issue route builder"
        );
    }

    #[test]
    fn viewer_runtime_preserves_routed_issue_urls_for_keyboard_dependency_navigation() {
        let viewer = lookup_asset("viewer.js").expect("viewer.js");
        let js = std::str::from_utf8(viewer.bytes).expect("valid utf8");

        assert!(
            js.contains("const route = parseRoute(window.location.hash);\n              if (route.view === 'issue') {\n                navigateToIssue(deps.blockedBy[0].id, this.view);\n              } else {\n                this.selectIssue(deps.blockedBy[0].id);\n              }"),
            "viewer runtime must preserve routed issue URLs when keyboard navigation jumps to blocker issues"
        );
        assert!(
            js.contains("const route = parseRoute(window.location.hash);\n              if (route.view === 'issue') {\n                navigateToIssue(deps.blocks[0].id, this.view);\n              } else {\n                this.selectIssue(deps.blocks[0].id);\n              }"),
            "viewer runtime must preserve routed issue URLs when keyboard navigation jumps to dependent issues"
        );
    }

    #[test]
    fn viewer_runtime_only_handles_escape_when_issue_modal_is_visible() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            html.contains("@keydown.escape.window=\"selectedIssue && closeIssue()\""),
            "issue modal Escape handling must be gated on a visible selected issue instead of always binding globally"
        );
    }

    #[test]
    fn viewer_runtime_only_handles_escape_when_keyboard_help_is_visible() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            html.contains(
                "@keydown.escape.window=\"showKeyboardHelp && (showKeyboardHelp = false)\""
            ),
            "keyboard help Escape handling must be gated on the help modal actually being visible"
        );
    }

    #[test]
    fn viewer_runtime_only_polls_diagnostics_memory_stats_while_panel_is_visible() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            html.contains("x-data=\"{ memStats: window.beadsViewer?.getWasmMemoryStats?.() || {}, memStatsPoll: null }\""),
            "diagnostics memory widget must track its polling interval explicitly"
        );
        assert!(
            html.contains("$watch('showDiagnostics', visible => {"),
            "diagnostics memory widget must watch the diagnostics panel visibility"
        );
        assert!(
            html.contains("if (visible && !memStatsPoll) {"),
            "diagnostics memory widget must only start polling when the panel becomes visible"
        );
        assert!(
            html.contains("} else if (!visible && memStatsPoll) {"),
            "diagnostics memory widget must stop polling when the panel is hidden"
        );
    }

    #[test]
    fn viewer_runtime_uses_alpine_managed_resize_binding_for_graph_detail_pane() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            html.contains("@resize.window=\"isMobile = window.innerWidth < 768\""),
            "graph detail pane must use Alpine-managed resize binding instead of a raw window resize listener"
        );
        assert!(
            !html.contains("x-init=\"window.addEventListener('resize', () => isMobile = window.innerWidth < 768)\""),
            "graph detail pane must not install an untracked global resize listener from x-init"
        );
    }

    #[test]
    fn viewer_runtime_scopes_cycle_navigator_window_events_to_insights_view() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        assert!(
            html.contains("$watch('view', newView => {"),
            "cycle navigator must reset itself when the active view changes"
        );
        assert!(
            html.contains("if (newView !== 'insights') {"),
            "cycle navigator must clear stale active state when leaving insights"
        );
        assert!(
            html.contains("@bv-graph:cycle-highlight-change.window=\"\n               if (view === 'insights') {"),
            "cycle navigator must ignore graph highlight events while insights is hidden"
        );
        assert!(
            html.contains("@bv-graph:cycle-navigator-reset.window=\"\n               if (view === 'insights') {"),
            "cycle navigator reset handling must be scoped to the visible insights view"
        );
    }

    #[test]
    fn index_html_registers_service_worker() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        // Must register the COI service worker
        assert!(
            html.contains("coi-serviceworker.js"),
            "index.html must reference the COI service worker"
        );
        assert!(
            html.contains("serviceWorker.register"),
            "index.html must register the service worker"
        );

        // Must have infinite-reload prevention (check for crossOriginIsolated)
        assert!(
            html.contains("crossOriginIsolated"),
            "index.html must check crossOriginIsolated to prevent reload loops"
        );
    }

    #[test]
    fn script_loading_order_preserves_dependencies() {
        let index = lookup_asset("index.html").expect("index.html");
        let html = std::str::from_utf8(index.bytes).expect("valid utf8");

        // Tailwind must load before body content (in <head>)
        let tailwind_pos = html
            .find("tailwindcss.js")
            .expect("tailwind must be present");
        let body_pos = html.find("<body").expect("body tag must exist");
        assert!(
            tailwind_pos < body_pos,
            "Tailwind CSS must load in <head> before <body>"
        );

        // Alpine must use defer (executes after non-deferred scripts like DOMPurify)
        let alpine_pos = html
            .find("alpine.min.js")
            .expect("Alpine.js must be present");
        // Find the <script tag that contains alpine.min.js
        let before_alpine = &html[..alpine_pos];
        let script_start = before_alpine.rfind("<script").expect("alpine script tag");
        let script_tag = &html[script_start..alpine_pos + 20];
        assert!(
            script_tag.contains("defer"),
            "Alpine.js must use defer attribute for correct load ordering"
        );
        // DOMPurify must be present (non-deferred, executes before Alpine)
        assert!(
            html.contains("dompurify.min.js"),
            "DOMPurify must be present"
        );

        // viewer.js (main app) must be last application script
        let viewer_pos = html.find("viewer.js").expect("viewer.js must be present");
        let charts_pos = html.find("charts.js").expect("charts.js must be present");
        assert!(
            viewer_pos > charts_pos,
            "viewer.js must load after charts.js"
        );

        // WASM assets must have loaders
        let wasm_loader_pos = html
            .find("wasm_loader.js")
            .expect("wasm_loader.js must be present");
        assert!(
            viewer_pos > wasm_loader_pos,
            "viewer.js must load after WASM loader"
        );
    }

    #[test]
    fn wasm_runtime_assets_are_paired() {
        // sql-wasm requires both .js and .wasm
        assert!(
            lookup_asset("vendor/sql-wasm.js").is_some(),
            "sql-wasm.js must be in inventory"
        );
        assert!(
            lookup_asset("vendor/sql-wasm.wasm").is_some(),
            "sql-wasm.wasm must be in inventory"
        );

        // bv_graph requires both .js and .wasm
        assert!(
            lookup_asset("vendor/bv_graph.js").is_some(),
            "bv_graph.js must be in inventory"
        );
        assert!(
            lookup_asset("vendor/bv_graph_bg.wasm").is_some(),
            "bv_graph_bg.wasm must be in inventory"
        );
    }

    #[test]
    fn asset_content_types_are_consistent() {
        for entry in ASSET_INVENTORY {
            let extension = std::path::Path::new(entry.path)
                .extension()
                .and_then(|ext| ext.to_str());
            // Every entry must have a non-empty content type
            assert!(
                !entry.content_type.is_empty(),
                "asset {} must have a content type",
                entry.path
            );
            // Verify extension-to-type consistency
            if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("woff2")) {
                assert_eq!(
                    entry.content_type, "font/woff2",
                    "WOFF2 files must have font/woff2 content type: {}",
                    entry.path
                );
            }
            if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("css")) {
                assert!(
                    entry.content_type.starts_with("text/css"),
                    "CSS files must have text/css content type: {} has {}",
                    entry.path,
                    entry.content_type
                );
            }
            if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("html")) {
                assert!(
                    entry.content_type.starts_with("text/html"),
                    "HTML files must have text/html content type: {} has {}",
                    entry.path,
                    entry.content_type
                );
            }
        }
    }
}
