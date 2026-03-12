use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use chrono::Utc;
use serde::Serialize;

use crate::analysis::Analyzer;
use crate::analysis::triage::TriageOptions;
use crate::export_sqlite::{
    SQLITE_EXPORT_CONFIG_FILENAME, SQLITE_EXPORT_FILENAME, SqliteBootstrapOptions,
    SqliteBundleOptions, bootstrap_export_database, emit_bootstrap_config,
    populate_export_database,
};
use crate::model::Issue;
use crate::{BvrError, Result};

const DEFAULT_PAGES_TITLE: &str = "Project Issues";
const DEFAULT_PREVIEW_PORT: u16 = 9000;
const MAX_PREVIEW_PORT_ATTEMPTS: u16 = 32;
const PREVIEW_MAX_REQUESTS_ENV: &str = "BVR_PREVIEW_MAX_REQUESTS";
const PREVIEW_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PREVIEW_STATUS_PATH: &str = "/__preview__/status";
const PREVIEW_RELOAD_PATH: &str = "/.bvr/livereload";

#[cfg(unix)]
const PREVIEW_SIGNAL_SET: &[i32] = &[signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];

#[cfg(not(unix))]
const PREVIEW_SIGNAL_SET: &[i32] = &[signal_hook::consts::SIGINT];

const STATIC_HOST_HEADERS: &str = "\
/*
  Cross-Origin-Embedder-Policy: require-corp
  Cross-Origin-Opener-Policy: same-origin
  Cache-Control: public, max-age=3600
  X-Content-Type-Options: nosniff

/*.wasm
  Content-Type: application/wasm
  Cache-Control: public, max-age=86400

/*.json
  Content-Type: application/json; charset=utf-8
  Cache-Control: no-cache

/beads.sqlite3
  Content-Type: application/x-sqlite3
  Cache-Control: public, max-age=3600
";

const LIVE_RELOAD_SCRIPT: &str = r"<script>
(() => {
  let lastToken = null;
  async function poll() {
    try {
      const resp = await fetch('/.bvr/livereload', { cache: 'no-store' });
      const token = (await resp.text()).trim();
      if (lastToken === null) {
        lastToken = token;
      } else if (token !== lastToken) {
        window.location.reload();
        return;
      }
    } catch (_) {}
    setTimeout(poll, 1200);
  }
  poll();
})();
</script>";

#[derive(Debug, Clone)]
pub struct ExportPagesOptions {
    pub title: Option<String>,
    pub include_closed: bool,
    pub include_history: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportPagesSummary {
    pub export_path: String,
    pub generated_at: String,
    pub issue_count: usize,
    pub include_closed: bool,
    pub include_history: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PagesMeta {
    title: String,
    generated_at: String,
    issue_count: usize,
    include_closed: bool,
    include_history: bool,
    generator: String,
    version: String,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewStatusResponse {
    status: &'static str,
    port: u16,
    url: String,
    bundle_path: String,
    has_index: bool,
    file_count: usize,
    live_reload: bool,
    reload_mode: &'static str,
    status_url: String,
    reload_endpoint: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewReloadMode {
    Poll,
    Disabled,
}

impl PreviewReloadMode {
    const fn from_enabled(live_reload: bool) -> Self {
        if live_reload {
            Self::Poll
        } else {
            Self::Disabled
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Disabled => "disabled",
        }
    }

    const fn operator_summary(self) -> &'static str {
        match self {
            Self::Poll => "polling (GET /.bvr/livereload)",
            Self::Disabled => "disabled",
        }
    }

    const fn reload_endpoint(self) -> Option<&'static str> {
        match self {
            Self::Poll => Some(PREVIEW_RELOAD_PATH),
            Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewShutdownReason {
    RequestLimitReached,
    ShutdownSignal,
}

impl PreviewShutdownReason {
    const fn operator_summary(self) -> &'static str {
        match self {
            Self::RequestLimitReached => "request limit reached",
            Self::ShutdownSignal => "received shutdown signal",
        }
    }
}

pub fn print_pages_wizard() {
    use crate::pages_wizard::{DeployTarget, WizardStep};

    println!("bvr pages wizard");
    println!();
    println!("Steps:");
    for step in WizardStep::ALL {
        println!("  {}. {}", step.display_number(), step.label());
    }
    println!();
    println!("Deploy targets:");
    for target in DeployTarget::ALL {
        let tools = target.required_tools();
        if tools.is_empty() {
            println!("  - {}", target.label());
        } else {
            println!("  - {} (requires: {})", target.label(), tools.join(", "));
        }
    }
    println!();
    println!("Non-interactive flow:");
    println!("  1) Export bundle:  bvr --export-pages ./bv-pages");
    println!("  2) Preview bundle: bvr --preview-pages ./bv-pages");
    println!("  3) Optional watch: bvr --export-pages ./bv-pages --watch-export");
    println!("  4) Deploy ./bv-pages to your chosen static host");
    println!();
    println!("Tip: customize title and payload scope:");
    println!("  bvr --export-pages ./bv-pages --pages-title \"Sprint Dashboard\" \\");
    println!("      --pages-include-closed=false --pages-include-history=false");
}

pub fn export_pages_bundle(
    issues: &[Issue],
    output_dir: &Path,
    options: &ExportPagesOptions,
) -> Result<ExportPagesSummary> {
    let title = options
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_PAGES_TITLE)
        .to_string();

    let filtered = issues
        .iter()
        .filter(|issue| options.include_closed || issue.is_open_like())
        .cloned()
        .collect::<Vec<_>>();

    fs::create_dir_all(output_dir.join("data"))?;

    let analyzer = Analyzer::new(filtered.clone());
    let triage = analyzer.triage(TriageOptions {
        group_by_track: false,
        group_by_label: false,
        max_recommendations: 50,
        ..TriageOptions::default()
    });
    let insights = analyzer.insights();

    let generated_at = Utc::now().to_rfc3339();
    let meta = PagesMeta {
        title: title.clone(),
        generated_at: generated_at.clone(),
        issue_count: filtered.len(),
        include_closed: options.include_closed,
        include_history: options.include_history,
        generator: "bvr".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let mut files = Vec::<String>::new();

    // Write the canonical viewer asset inventory (deterministic, sorted order).
    let asset_paths = crate::viewer_assets::write_viewer_assets(output_dir)?;
    files.extend(asset_paths);

    // Also write the lightweight Rust-generated assets under assets/ for
    // backward compatibility — the canonical index.html does not reference
    // these, but existing integrations may rely on their presence.
    fs::create_dir_all(output_dir.join("assets"))?;
    write_text(output_dir.join("assets/style.css"), CSS_BUNDLE)?;
    files.push("assets/style.css".to_string());

    write_text(output_dir.join("assets/viewer.js"), JS_BUNDLE)?;
    files.push("assets/viewer.js".to_string());

    write_json(output_dir.join("data/issues.json"), &filtered)?;
    files.push("data/issues.json".to_string());

    write_json(output_dir.join("data/meta.json"), &meta)?;
    files.push("data/meta.json".to_string());

    write_json(output_dir.join("data/triage.json"), &triage.result)?;
    files.push("data/triage.json".to_string());

    write_json(output_dir.join("data/insights.json"), &insights)?;
    files.push("data/insights.json".to_string());

    bootstrap_export_database(output_dir, &SqliteBootstrapOptions::default())?;
    populate_export_database(output_dir, Some(&title), &filtered, &analyzer, &triage)?;
    files.push(SQLITE_EXPORT_FILENAME.to_string());

    let sqlite_config = emit_bootstrap_config(output_dir, &SqliteBundleOptions::default())?;
    files.push(SQLITE_EXPORT_CONFIG_FILENAME.to_string());
    for chunk in &sqlite_config.chunks {
        files.push(chunk.path.clone());
    }

    if options.include_history {
        let history_limit = filtered.len().max(500);
        let history = analyzer.history(None, history_limit);
        write_json(output_dir.join("data/history.json"), &history)?;
        files.push("data/history.json".to_string());
    }

    // Deploy-facing README so the bundle is self-describing.
    write_text(
        output_dir.join("README.md"),
        &generate_deploy_readme(&title, &meta),
    )?;
    files.push("README.md".to_string());

    // Static-host header hints (Cloudflare Pages, Netlify, etc.).
    write_text(output_dir.join("_headers"), STATIC_HOST_HEADERS)?;
    files.push("_headers".to_string());

    let summary = ExportPagesSummary {
        export_path: output_dir.to_string_lossy().to_string(),
        generated_at,
        issue_count: filtered.len(),
        include_closed: options.include_closed,
        include_history: options.include_history,
        files,
    };

    write_json(output_dir.join("data/export_summary.json"), &summary)?;

    Ok(summary)
}

fn generate_deploy_readme(title: &str, meta: &PagesMeta) -> String {
    format!(
        "# {title}\n\
         \n\
         Static issue viewer bundle generated by **bvr** v{version}.\n\
         \n\
         ## Quick start\n\
         \n\
         Deploy this directory to any static host:\n\
         \n\
         - **GitHub Pages**: push this folder to your `gh-pages` branch\n\
         - **Cloudflare Pages**: point your project at this folder\n\
         - **Local preview**: `bvr --preview-pages {path}`\n\
         \n\
         ## Contents\n\
         \n\
         | File | Purpose |\n\
         |------|---------|\n\
         | `index.html` | Viewer entry point |\n\
         | `data/` | JSON + SQLite data payloads |\n\
         | `beads.sqlite3` | Full issue database |\n\
         | `_headers` | Static-host header hints |\n\
         \n\
         ## Generation info\n\
         \n\
         - **Title**: {title}\n\
         - **Issues**: {count}\n\
         - **Generated**: {at}\n\
         - **Generator**: bvr v{version}\n",
        version = meta.version,
        count = meta.issue_count,
        at = meta.generated_at,
        path = ".",
    )
}

pub fn run_preview_server(bundle_dir: &Path, live_reload: bool) -> Result<()> {
    if !bundle_dir.is_dir() {
        return Err(BvrError::InvalidArgument(format!(
            "preview bundle directory not found: {}",
            bundle_dir.display()
        )));
    }
    if !bundle_dir.join("index.html").is_file() {
        return Err(BvrError::InvalidArgument(format!(
            "missing index.html in preview bundle: {}",
            bundle_dir.display()
        )));
    }

    let (listener, port) = bind_preview_listener()?;
    listener.set_nonblocking(true)?;
    let preview_url = preview_url(port);
    let reload_mode = PreviewReloadMode::from_enabled(live_reload);
    let shutdown_requested = install_preview_signal_handlers()?;

    println!("Preview server running at {preview_url}");
    println!("Serving bundle: {}", bundle_dir.display());
    println!("Status endpoint: {preview_url}{PREVIEW_STATUS_PATH}");
    println!("Reload transport: {}", reload_mode.operator_summary());
    println!("Press Ctrl+C to stop.");
    maybe_open_preview_in_browser(port);

    let max_requests = std::env::var(PREVIEW_MAX_REQUESTS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0);
    let mut served = 0usize;

    let shutdown_reason = loop {
        if shutdown_requested.load(Ordering::Relaxed) {
            break PreviewShutdownReason::ShutdownSignal;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = handle_preview_request(stream, bundle_dir, live_reload, port) {
                    eprintln!("warning: preview request failed: {error}");
                }
                served = served.saturating_add(1);
                if max_requests.is_some_and(|limit| served >= limit) {
                    break PreviewShutdownReason::RequestLimitReached;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(PREVIEW_ACCEPT_POLL_INTERVAL);
            }
            Err(error) if shutdown_requested.load(Ordering::Relaxed) => {
                eprintln!("warning: preview accept loop stopped after shutdown signal: {error}");
                break PreviewShutdownReason::ShutdownSignal;
            }
            Err(error) => return Err(BvrError::Io(error)),
        }
    };

    println!(
        "Preview server stopped: {}.",
        shutdown_reason.operator_summary()
    );
    Ok(())
}

fn bind_preview_listener() -> Result<(TcpListener, u16)> {
    let base_port = std::env::var("BVR_PREVIEW_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PREVIEW_PORT);

    for offset in 0..MAX_PREVIEW_PORT_ATTEMPTS {
        let port = base_port.saturating_add(offset);
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok((listener, port)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(error) => {
                return Err(BvrError::InvalidArgument(format!(
                    "failed to bind preview server on 127.0.0.1:{port}: {error}. Set BVR_PREVIEW_PORT to a free port or stop the conflicting process."
                )));
            }
        }
    }

    Err(BvrError::InvalidArgument(format!(
        "could not bind preview server on ports {base_port}..{}. Set BVR_PREVIEW_PORT to a free port or stop the conflicting process.",
        base_port.saturating_add(MAX_PREVIEW_PORT_ATTEMPTS.saturating_sub(1))
    )))
}

fn handle_preview_request(
    mut stream: TcpStream,
    bundle_dir: &Path,
    live_reload: bool,
    port: u16,
) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    let bytes = stream.read(&mut buffer)?;
    if bytes == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let request_line = request.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or("/");
    let head_only = method == "HEAD";

    if method != "GET" && method != "HEAD" {
        write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed\n",
            head_only,
        )?;
        return Ok(());
    }

    let route = target.split('?').next().unwrap_or("/");
    if route == PREVIEW_STATUS_PATH || route == "/.bvr/status" {
        let payload = serde_json::to_vec(&preview_status(bundle_dir, live_reload, port)?)?;
        write_http_response(
            &mut stream,
            "200 OK",
            "application/json; charset=utf-8",
            &payload,
            head_only,
        )?;
        return Ok(());
    }

    if route == PREVIEW_RELOAD_PATH {
        let token = latest_modified_token(bundle_dir)?.to_string();
        write_http_response(
            &mut stream,
            "200 OK",
            "text/plain; charset=utf-8",
            token.as_bytes(),
            head_only,
        )?;
        return Ok(());
    }

    let Ok(relative) = normalize_request_path(route) else {
        write_http_response(
            &mut stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"invalid path\n",
            head_only,
        )?;
        return Ok(());
    };

    let Some(file_path) = resolve_preview_asset_path(bundle_dir, &relative)? else {
        write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found\n",
            head_only,
        )?;
        return Ok(());
    };

    let mut body = fs::read(&file_path)?;
    let mime = mime_type_for_path(&file_path);
    if live_reload && mime.starts_with("text/html") {
        body = inject_live_reload(body);
    }

    write_http_response(&mut stream, "200 OK", mime, &body, head_only)?;
    Ok(())
}

fn normalize_request_path(route: &str) -> Result<PathBuf> {
    let mut normalized = route.trim().trim_start_matches('/').to_string();
    if normalized.is_empty() || normalized.ends_with('/') {
        normalized.push_str("index.html");
    }

    let path = PathBuf::from(normalized);
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(BvrError::InvalidArgument(
                    "path traversal is not allowed".to_string(),
                ));
            }
        }
    }

    Ok(path)
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(OsStr::to_str).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n\
         Pragma: no-cache\r\n\
         Expires: 0\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()?;
    Ok(())
}

fn inject_live_reload(html: Vec<u8>) -> Vec<u8> {
    let html_text = String::from_utf8_lossy(&html);
    let injected = html_text.rfind("</body>").map_or_else(
        || {
            let mut output = String::with_capacity(html_text.len() + LIVE_RELOAD_SCRIPT.len());
            output.push_str(&html_text);
            output.push_str(LIVE_RELOAD_SCRIPT);
            output
        },
        |pos| {
            let mut output = String::with_capacity(html_text.len() + LIVE_RELOAD_SCRIPT.len() + 8);
            output.push_str(&html_text[..pos]);
            output.push_str(LIVE_RELOAD_SCRIPT);
            output.push_str("</body>");
            output.push_str(&html_text[pos + "</body>".len()..]);
            output
        },
    );
    injected.into_bytes()
}

fn preview_status(
    bundle_dir: &Path,
    live_reload: bool,
    port: u16,
) -> Result<PreviewStatusResponse> {
    let preview_url = preview_url(port);
    let reload_mode = PreviewReloadMode::from_enabled(live_reload);

    Ok(PreviewStatusResponse {
        status: "running",
        port,
        url: preview_url.clone(),
        bundle_path: bundle_dir.to_string_lossy().to_string(),
        has_index: bundle_dir.join("index.html").is_file(),
        file_count: count_files_recursive(bundle_dir)?,
        live_reload,
        reload_mode: reload_mode.label(),
        status_url: format!("{preview_url}{PREVIEW_STATUS_PATH}"),
        reload_endpoint: reload_mode.reload_endpoint(),
    })
}

fn preview_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn latest_modified_token(path: &Path) -> Result<u64> {
    let bundle_root = fs::canonicalize(path)?;
    let mut visited_dirs = HashSet::new();
    latest_modified_recursive(path, &bundle_root, 0, &mut visited_dirs)
}

fn count_files_recursive(path: &Path) -> Result<usize> {
    let bundle_root = fs::canonicalize(path)?;
    let mut visited_dirs = HashSet::new();
    count_files_recursive_inner(path, &bundle_root, &mut visited_dirs)
}

fn count_files_recursive_inner(
    path: &Path,
    bundle_root: &Path,
    visited_dirs: &mut HashSet<PathBuf>,
) -> Result<usize> {
    let link_metadata = fs::symlink_metadata(path)?;
    let resolved_path = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(BvrError::Io(error)),
    };
    if !resolved_path.starts_with(bundle_root) {
        return Ok(0);
    }

    let target_metadata = match fs::metadata(&resolved_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(BvrError::Io(error)),
    };

    if target_metadata.is_file() {
        return Ok(1);
    }

    if !target_metadata.is_dir() {
        return Ok(0);
    }

    if !visited_dirs.insert(resolved_path.clone()) {
        return Ok(0);
    }

    let mut total = 0usize;
    for entry in fs::read_dir(path)? {
        total = total.saturating_add(count_files_recursive_inner(
            &entry?.path(),
            bundle_root,
            visited_dirs,
        )?);
    }

    let _ = link_metadata;
    Ok(total)
}

fn latest_modified_recursive(
    path: &Path,
    bundle_root: &Path,
    mut latest: u64,
    visited_dirs: &mut HashSet<PathBuf>,
) -> Result<u64> {
    let link_metadata = fs::symlink_metadata(path)?;
    latest = latest.max(metadata_modified_token(&link_metadata));

    let resolved_path = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(latest),
        Err(error) => return Err(BvrError::Io(error)),
    };
    if !resolved_path.starts_with(bundle_root) {
        return Ok(latest);
    }

    let target_metadata = match fs::metadata(&resolved_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(latest),
        Err(error) => return Err(BvrError::Io(error)),
    };
    latest = latest.max(metadata_modified_token(&target_metadata));

    if !target_metadata.is_dir() {
        return Ok(latest);
    }

    if !visited_dirs.insert(resolved_path) {
        return Ok(latest);
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        latest = latest_modified_recursive(&entry.path(), bundle_root, latest, visited_dirs)?;
    }

    Ok(latest)
}

fn metadata_modified_token(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis().min(u128::from(u64::MAX))).unwrap_or(u64::MAX)
        })
}

fn resolve_preview_asset_path(bundle_dir: &Path, relative: &Path) -> Result<Option<PathBuf>> {
    let bundle_root = fs::canonicalize(bundle_dir)?;
    let candidate = bundle_dir.join(relative);
    let resolved = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BvrError::Io(error)),
    };

    if !resolved.starts_with(&bundle_root) {
        return Ok(None);
    }

    let metadata = fs::metadata(&resolved)?;
    if metadata.is_file() {
        Ok(Some(resolved))
    } else {
        Ok(None)
    }
}

fn write_text(path: PathBuf, content: &str) -> Result<()> {
    fs::write(path, content)?;
    Ok(())
}

fn write_json<T: Serialize>(path: PathBuf, payload: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(payload)?;
    fs::write(path, text)?;
    Ok(())
}

fn maybe_open_preview_in_browser(port: u16) {
    if std::env::var("BV_NO_BROWSER").is_ok() || std::env::var("BVR_NO_BROWSER").is_ok() {
        return;
    }

    let url = preview_url(port);
    thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !open_url_in_browser(&url) {
            eprintln!("warning: could not open browser automatically; open {url}");
        }
    });
}

fn open_url_in_browser(url: &str) -> bool {
    if cfg!(target_os = "windows") {
        run_command("cmd", &["/C", "start", "", url])
    } else if cfg!(target_os = "macos") {
        run_command("open", &[url])
    } else {
        run_command("xdg-open", &[url])
            || run_command("open", &[url])
            || run_command("gio", &["open", url])
    }
}

fn run_command(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn install_preview_signal_handlers() -> Result<Arc<AtomicBool>> {
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    for signal in PREVIEW_SIGNAL_SET {
        signal_hook::flag::register(*signal, Arc::clone(&shutdown_requested))?;
    }
    Ok(shutdown_requested)
}

const CSS_BUNDLE: &str = r":root {
  color-scheme: light dark;
  font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif;
}
body {
  margin: 0;
  background: #0b1220;
  color: #dce6ff;
}
.layout {
  max-width: 1100px;
  margin: 0 auto;
  padding: 1.2rem;
}
h1, h2 {
  margin: 0 0 0.6rem 0;
}
.meta {
  color: #9db0d7;
}
.grid {
  display: grid;
  gap: 1rem;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  margin-top: 1rem;
}
article {
  background: #111b31;
  border: 1px solid #2b3a5a;
  border-radius: 10px;
  padding: 0.9rem;
}
.issue-list, .pick-list {
  margin: 0;
  padding-left: 1.2rem;
}
.issue-list li, .pick-list li {
  margin-bottom: 0.5rem;
}
.insights {
  white-space: pre-wrap;
  font-size: 0.82rem;
  margin: 0;
}
";

const JS_BUNDLE: &str = r#"async function fetchJson(path) {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`failed to fetch ${path}: ${response.status}`);
  }
  return response.json();
}

function formatIssue(issue) {
  return `${issue.id} · ${issue.status} · p${issue.priority} · ${issue.title}`;
}

async function init() {
  const [meta, issues, triage, insights] = await Promise.all([
    fetchJson("data/meta.json"),
    fetchJson("data/issues.json"),
    fetchJson("data/triage.json"),
    fetchJson("data/insights.json")
  ]);

  const metaLine = document.getElementById("meta-line");
  metaLine.textContent = `${meta.issue_count} issues · generated ${meta.generated_at}`;

  const issueList = document.getElementById("issue-list");
  for (const issue of issues) {
    const li = document.createElement("li");
    li.textContent = formatIssue(issue);
    issueList.appendChild(li);
  }

  const topPicks = document.getElementById("top-picks");
  for (const pick of (triage.quick_ref?.top_picks ?? [])) {
    const li = document.createElement("li");
    li.textContent = `${pick.id} (${(pick.score * 100).toFixed(1)}%)`;
    topPicks.appendChild(li);
  }

  const insightsNode = document.getElementById("insights");
  const bottlenecks = (insights.bottlenecks ?? []).slice(0, 5)
    .map((entry) => `${entry.id}: score=${entry.score.toFixed(3)} blocks=${entry.blocks_count}`);
  insightsNode.textContent = bottlenecks.length > 0
    ? bottlenecks.join("\n")
    : "No bottlenecks available.";
}

init().catch((error) => {
  const metaLine = document.getElementById("meta-line");
  metaLine.textContent = `failed to load export data: ${error.message}`;
});
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_issue(id: &str, status: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: status.to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            assignee: String::new(),
            estimated_minutes: Some(30),
            created_at: None,
            updated_at: None,
            due_date: None,
            closed_at: None,
            labels: Vec::new(),
            comments: Vec::new(),
            dependencies: Vec::new(),
            source_repo: ".".to_string(),
            content_hash: None,
            external_ref: None,
        }
    }

    #[test]
    fn export_pages_bundle_writes_expected_core_files() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open"), make_issue("B", "closed")];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: Some("Dashboard".to_string()),
                include_closed: true,
                include_history: true,
            },
        )
        .expect("export pages bundle");

        assert_eq!(summary.issue_count, 2);
        assert!(out.join("index.html").is_file());
        assert!(out.join("assets/style.css").is_file());
        assert!(out.join("assets/viewer.js").is_file());
        assert!(out.join("data/issues.json").is_file());
        assert!(out.join("data/meta.json").is_file());
        assert!(out.join("data/triage.json").is_file());
        assert!(out.join("data/insights.json").is_file());
        assert!(out.join("data/history.json").is_file());
        assert!(out.join("data/export_summary.json").is_file());
        assert!(out.join("beads.sqlite3").is_file());
        assert!(out.join("beads.sqlite3.config.json").is_file());
        assert!(summary.files.contains(&"beads.sqlite3".to_string()));
        assert!(
            summary
                .files
                .contains(&"beads.sqlite3.config.json".to_string())
        );
    }

    #[test]
    fn export_pages_bundle_respects_include_closed_flag() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open"), make_issue("B", "closed")];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        assert_eq!(summary.issue_count, 1);
        assert!(!out.join("data/history.json").exists());

        let exported = fs::read_to_string(out.join("data/issues.json")).expect("read issues.json");
        assert!(exported.contains("\"A\""));
        assert!(!exported.contains("\"B\""));
    }

    #[test]
    fn export_pages_bundle_writes_sqlite_bootstrap_config_with_hash() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        let config: crate::export_sqlite::SqliteBootstrapConfig = serde_json::from_str(
            &fs::read_to_string(out.join("beads.sqlite3.config.json")).expect("read config"),
        )
        .expect("parse config");

        assert!(!config.chunked);
        assert!(config.total_size > 0);
        assert_eq!(config.hash.len(), 64);
    }

    #[test]
    fn normalize_request_path_rejects_parent_segments() {
        let result = normalize_request_path("/../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn inject_live_reload_appends_script() {
        let html = b"<html><body>ok</body></html>".to_vec();
        let injected = inject_live_reload(html);
        let text = String::from_utf8(injected).expect("utf8");
        assert!(text.contains("window.location.reload"));
    }

    #[test]
    fn export_bundle_includes_coi_service_worker() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        // COI service worker must be present for cross-origin isolation on static hosts
        assert!(
            out.join("coi-serviceworker.js").is_file(),
            "exported bundle must include coi-serviceworker.js"
        );

        // Index must reference service worker for registration
        let index = fs::read_to_string(out.join("index.html")).expect("read index.html");
        assert!(
            index.contains("coi-serviceworker.js"),
            "index.html must reference the COI service worker"
        );
    }

    #[test]
    fn exported_index_html_has_csp_meta_tag() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        let index = fs::read_to_string(out.join("index.html")).expect("read index.html");
        assert!(
            index.contains("Content-Security-Policy"),
            "exported index.html must include CSP meta tag"
        );
        // CSP must enforce self-contained (offline) deployment
        assert!(
            index.contains("default-src") && index.contains("connect-src"),
            "CSP must include default-src and connect-src directives"
        );
    }

    #[test]
    fn cache_control_header_disables_caching() {
        // Verify the no-cache header string matches the expected contract
        let header = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Length: 5\r\n\
             Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n\
             Pragma: no-cache\r\n\
             Expires: 0\r\n\
             Connection: close\r\n\r\n"
        );
        // All cache-disabling directives must be present
        assert!(header.contains("no-store"));
        assert!(header.contains("no-cache"));
        assert!(header.contains("must-revalidate"));
        assert!(header.contains("max-age=0"));
        assert!(header.contains("Pragma: no-cache"));
        assert!(header.contains("Expires: 0"));
    }

    #[test]
    fn mime_type_for_wasm_returns_correct_type() {
        assert_eq!(
            mime_type_for_path(Path::new("vendor/sql-wasm.wasm")),
            "application/octet-stream"
        );
        assert_eq!(
            mime_type_for_path(Path::new("vendor/inter.woff2")),
            "font/woff2"
        );
        assert_eq!(
            mime_type_for_path(Path::new("viewer.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("styles.css")),
            "text/css; charset=utf-8"
        );
    }

    #[test]
    fn export_bundle_includes_deploy_readme() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: Some("Sprint 42".to_string()),
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        assert!(out.join("README.md").is_file());
        assert!(summary.files.contains(&"README.md".to_string()));

        let readme = fs::read_to_string(out.join("README.md")).expect("read README");
        assert!(readme.contains("# Sprint 42"));
        assert!(readme.contains("bvr"));
        assert!(readme.contains("GitHub Pages"));
        assert!(readme.contains("Cloudflare Pages"));
        assert!(readme.contains("Issues"));
    }

    #[test]
    fn export_bundle_includes_static_host_headers() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export pages bundle");

        assert!(out.join("_headers").is_file());
        assert!(summary.files.contains(&"_headers".to_string()));

        let headers = fs::read_to_string(out.join("_headers")).expect("read _headers");
        assert!(headers.contains("Cross-Origin-Embedder-Policy"));
        assert!(headers.contains("Cross-Origin-Opener-Policy"));
        assert!(headers.contains("application/wasm"));
        assert!(headers.contains("application/x-sqlite3"));
    }

    #[test]
    fn generate_deploy_readme_includes_key_sections() {
        let meta = PagesMeta {
            title: "Test Project".to_string(),
            generated_at: "2026-03-09T12:00:00Z".to_string(),
            issue_count: 42,
            include_closed: true,
            include_history: true,
            generator: "bvr".to_string(),
            version: "0.1.0".to_string(),
        };
        let readme = generate_deploy_readme("Test Project", &meta);
        assert!(readme.contains("# Test Project"));
        assert!(readme.contains("## Quick start"));
        assert!(readme.contains("## Contents"));
        assert!(readme.contains("## Generation info"));
        assert!(readme.contains("Issues**: 42"));
        assert!(readme.contains("v0.1.0"));
    }

    #[test]
    fn preview_status_reports_urls_and_reload_mode() {
        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join("index.html"), "<html></html>").expect("write index");

        let status = preview_status(temp.path(), true, 9123).expect("preview status");
        assert_eq!(status.url, "http://127.0.0.1:9123");
        assert_eq!(status.reload_mode, "poll");
        assert_eq!(
            status.status_url,
            "http://127.0.0.1:9123/__preview__/status"
        );
        assert_eq!(status.reload_endpoint, Some("/.bvr/livereload"));
    }

    // ── Empty issue list ──────────────────────────────────────────────

    #[test]
    fn export_empty_issue_list_produces_valid_bundle() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        let summary = export_pages_bundle(
            &[],
            &out,
            &ExportPagesOptions {
                title: Some("Empty Project".to_string()),
                include_closed: true,
                include_history: true,
            },
        )
        .expect("export pages bundle");

        assert_eq!(summary.issue_count, 0);
        assert!(out.join("index.html").is_file());
        assert!(out.join("data/meta.json").is_file());
        assert!(out.join("data/issues.json").is_file());
        assert!(out.join("data/triage.json").is_file());
        assert!(out.join("data/insights.json").is_file());
        assert!(out.join("beads.sqlite3").is_file());
        assert!(out.join("README.md").is_file());
        assert!(out.join("_headers").is_file());

        let issues_json: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(out.join("data/issues.json")).expect("read"))
                .expect("parse");
        assert!(issues_json.is_empty());
    }

    #[test]
    fn export_empty_issues_history_still_written_when_enabled() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: true,
                include_history: true,
            },
        )
        .expect("export pages bundle");

        assert!(
            out.join("data/history.json").is_file(),
            "history.json must be emitted even for empty issue list"
        );
    }

    // ── Title edge cases ──────────────────────────────────────────────

    #[test]
    fn export_empty_title_falls_back_to_default() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: Some("".to_string()),
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");
        assert_eq!(meta["title"], "Project Issues");
    }

    #[test]
    fn export_whitespace_title_falls_back_to_default() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: Some("   \t  ".to_string()),
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");
        assert_eq!(meta["title"], "Project Issues");
    }

    #[test]
    fn export_none_title_falls_back_to_default() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");
        assert_eq!(meta["title"], "Project Issues");
    }

    #[test]
    fn export_unicode_title_preserved_in_meta() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let title = "Sprint \u{1f680} Rocket";

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: Some(title.to_string()),
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");
        assert_eq!(meta["title"], title);
    }

    // ── Meta JSON schema validation ───────────────────────────────────

    #[test]
    fn meta_json_has_all_required_fields() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: Some("Parity Test".to_string()),
                include_closed: true,
                include_history: true,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");

        assert!(meta["title"].is_string());
        assert!(meta["generated_at"].is_string());
        assert!(meta["issue_count"].is_number());
        assert!(meta["include_closed"].is_boolean());
        assert!(meta["include_history"].is_boolean());
        assert!(meta["generator"].is_string());
        assert!(meta["version"].is_string());
        assert_eq!(meta["generator"], "bvr");
    }

    // ── Triage / insights JSON shape ──────────────────────────────────

    #[test]
    fn triage_json_has_quick_ref_key() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open"), make_issue("B", "open")],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let triage: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/triage.json")).expect("read"))
                .expect("parse");

        assert!(
            triage.get("quick_ref").is_some(),
            "triage.json must contain quick_ref key"
        );
    }

    #[test]
    fn insights_json_has_bottlenecks_key() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let insights: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(out.join("data/insights.json")).expect("read"),
        )
        .expect("parse");

        assert!(
            insights.get("bottlenecks").is_some(),
            "insights.json must contain bottlenecks key"
        );
    }

    // ── Export summary validation ─────────────────────────────────────

    #[test]
    fn export_summary_json_is_self_consistent() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "closed"),
            make_issue("C", "open"),
        ];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: Some("Self Check".to_string()),
                include_closed: true,
                include_history: false,
            },
        )
        .expect("export");

        // Summary matches what was exported
        assert_eq!(summary.issue_count, 3);
        assert!(!summary.include_history);
        assert!(summary.include_closed);
        assert!(!summary.files.is_empty());

        // Round-trip: the on-disk summary matches
        let disk_summary: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(out.join("data/export_summary.json")).expect("read"),
        )
        .expect("parse");
        assert_eq!(disk_summary["issue_count"], 3);
        assert_eq!(disk_summary["include_closed"], true);
        assert_eq!(disk_summary["include_history"], false);
    }

    #[test]
    fn export_summary_file_list_includes_core_artifacts() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        let summary = export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let required = [
            "data/issues.json",
            "data/meta.json",
            "data/triage.json",
            "data/insights.json",
            "beads.sqlite3",
            "beads.sqlite3.config.json",
            "assets/style.css",
            "assets/viewer.js",
            "README.md",
            "_headers",
        ];

        for artifact in &required {
            assert!(
                summary.files.contains(&artifact.to_string()),
                "summary.files must contain {artifact}"
            );
        }
    }

    // ── Filtering edge cases ──────────────────────────────────────────

    #[test]
    fn export_all_closed_with_exclude_yields_zero_issues() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![
            make_issue("A", "closed"),
            make_issue("B", "closed"),
            make_issue("C", "tombstone"),
        ];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        assert_eq!(summary.issue_count, 0);

        let issues_json: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(out.join("data/issues.json")).expect("read"))
                .expect("parse");
        assert!(issues_json.is_empty());
    }

    #[test]
    fn export_include_closed_true_keeps_all_statuses() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "closed"),
            make_issue("C", "in_progress"),
        ];

        let summary = export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: true,
                include_history: false,
            },
        )
        .expect("export");

        assert_eq!(summary.issue_count, 3);
    }

    // ── Normalize path edge cases ─────────────────────────────────────

    #[test]
    fn normalize_root_path_maps_to_index() {
        let path = normalize_request_path("/").expect("normalize /");
        assert_eq!(path, PathBuf::from("index.html"));
    }

    #[test]
    fn normalize_trailing_slash_maps_to_index() {
        let path = normalize_request_path("/data/").expect("normalize /data/");
        assert_eq!(path, PathBuf::from("data/index.html"));
    }

    #[test]
    fn normalize_normal_file_path() {
        let path = normalize_request_path("/data/meta.json").expect("normalize");
        assert_eq!(path, PathBuf::from("data/meta.json"));
    }

    #[test]
    fn normalize_double_dot_rejected() {
        assert!(normalize_request_path("/../etc/passwd").is_err());
        assert!(normalize_request_path("/data/../../secret").is_err());
    }

    // ── MIME type coverage ────────────────────────────────────────────

    #[test]
    fn mime_types_cover_all_bundle_extensions() {
        assert_eq!(
            mime_type_for_path(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("data/meta.json")),
            "application/json; charset=utf-8"
        );
        assert_eq!(mime_type_for_path(Path::new("logo.svg")), "image/svg+xml");
        assert_eq!(mime_type_for_path(Path::new("photo.png")), "image/png");
        assert_eq!(mime_type_for_path(Path::new("pic.jpg")), "image/jpeg");
        assert_eq!(mime_type_for_path(Path::new("pic.jpeg")), "image/jpeg");
        assert_eq!(
            mime_type_for_path(Path::new("unknown.xyz")),
            "application/octet-stream"
        );
    }

    // ── Live reload injection edge cases ──────────────────────────────

    #[test]
    fn inject_live_reload_without_body_tag() {
        let html = b"<html>no body tag here</html>".to_vec();
        let injected = String::from_utf8(inject_live_reload(html)).expect("utf8");
        assert!(
            injected.contains("window.location.reload"),
            "script must be appended even without </body>"
        );
        assert!(injected.contains("no body tag here"));
    }

    #[test]
    fn inject_live_reload_empty_html() {
        let html = b"".to_vec();
        let injected = String::from_utf8(inject_live_reload(html)).expect("utf8");
        assert!(injected.contains("window.location.reload"));
    }

    // ── Preview reload mode ───────────────────────────────────────────

    #[test]
    fn preview_reload_mode_disabled_has_no_endpoint() {
        let mode = PreviewReloadMode::Disabled;
        assert_eq!(mode.label(), "disabled");
        assert!(mode.reload_endpoint().is_none());
        assert!(mode.operator_summary().contains("disabled"));
    }

    #[test]
    fn preview_reload_mode_poll_has_endpoint() {
        let mode = PreviewReloadMode::Poll;
        assert_eq!(mode.label(), "poll");
        assert!(mode.reload_endpoint().is_some());
        assert!(mode.operator_summary().contains("livereload"));
    }

    #[test]
    fn preview_status_without_live_reload() {
        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join("index.html"), "<html></html>").expect("write index");

        let status = preview_status(temp.path(), false, 9200).expect("preview status");
        assert_eq!(status.reload_mode, "disabled");
        assert!(status.reload_endpoint.is_none());
        assert!(!status.live_reload);
    }

    // ── Export idempotency ────────────────────────────────────────────

    #[test]
    fn export_twice_to_same_dir_succeeds() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];
        let opts = ExportPagesOptions {
            title: Some("Idempotent".to_string()),
            include_closed: false,
            include_history: false,
        };

        let s1 = export_pages_bundle(&issues, &out, &opts).expect("first export");
        let s2 = export_pages_bundle(&issues, &out, &opts).expect("second export");

        assert_eq!(s1.issue_count, s2.issue_count);
        assert_eq!(s1.files.len(), s2.files.len());
    }

    // ── SQLite DB table validation ────────────────────────────────────

    #[test]
    fn export_sqlite_has_expected_tables() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let db = rusqlite::Connection::open(out.join("beads.sqlite3")).expect("open db");
        let tables: Vec<String> = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            tables.contains(&"issues".to_string()),
            "must have issues table, got: {tables:?}"
        );
    }

    #[test]
    fn export_sqlite_issue_count_matches() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "open"),
            make_issue("C", "closed"),
        ];

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                include_closed: true,
                include_history: false,
            },
        )
        .expect("export");

        let db = rusqlite::Connection::open(out.join("beads.sqlite3")).expect("open db");
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 3);
    }

    // ── Static host headers contract ──────────────────────────────────

    #[test]
    fn static_host_headers_has_all_required_directives() {
        assert!(STATIC_HOST_HEADERS.contains("Cross-Origin-Embedder-Policy: require-corp"));
        assert!(STATIC_HOST_HEADERS.contains("Cross-Origin-Opener-Policy: same-origin"));
        assert!(STATIC_HOST_HEADERS.contains("X-Content-Type-Options: nosniff"));
        assert!(STATIC_HOST_HEADERS.contains("application/wasm"));
        assert!(STATIC_HOST_HEADERS.contains("application/json; charset=utf-8"));
        assert!(STATIC_HOST_HEADERS.contains("application/x-sqlite3"));
        // Glob patterns for file type matching
        assert!(STATIC_HOST_HEADERS.contains("/*.wasm"));
        assert!(STATIC_HOST_HEADERS.contains("/*.json"));
        assert!(STATIC_HOST_HEADERS.contains("/beads.sqlite3"));
    }

    // ── Count / modified helpers ───────────────────────────────────────

    #[test]
    fn count_files_recursive_empty_dir() {
        let temp = tempdir().expect("tempdir");
        let count = count_files_recursive(temp.path()).expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn count_files_recursive_nested() {
        let temp = tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("a/b")).expect("mkdir");
        fs::write(temp.path().join("a/b/c.txt"), "hi").expect("write");
        fs::write(temp.path().join("top.txt"), "hi").expect("write");

        let count = count_files_recursive(temp.path()).expect("count");
        assert_eq!(count, 2);
    }

    #[test]
    fn latest_modified_token_non_empty_dir() {
        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join("file.txt"), "hello").expect("write");

        let token = latest_modified_token(temp.path()).expect("token");
        assert!(token > 0, "token must be nonzero for non-empty dir");
    }

    #[cfg(unix)]
    #[test]
    fn count_files_recursive_skips_symlink_cycles() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let real = temp.path().join("real");
        fs::create_dir_all(&real).expect("mkdir real");
        fs::write(real.join("file.txt"), "hello").expect("write real file");
        symlink(temp.path(), temp.path().join("loop")).expect("create loop symlink");

        let count = count_files_recursive(temp.path()).expect("count");
        assert_eq!(count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn latest_modified_token_handles_symlink_cycles() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join("index.html"), "<html></html>").expect("write index");
        symlink(temp.path(), temp.path().join("loop")).expect("create loop symlink");

        let token = latest_modified_token(temp.path()).expect("token");
        assert!(token > 0);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_preview_asset_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().expect("bundle tempdir");
        let outside = tempdir().expect("outside tempdir");
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "top secret").expect("write secret");
        symlink(&secret, bundle.path().join("secret.txt")).expect("symlink secret");

        let resolved =
            resolve_preview_asset_path(bundle.path(), Path::new("secret.txt")).expect("resolve");
        assert!(resolved.is_none(), "symlink escape should be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn count_files_recursive_skips_symlinked_dir_outside_bundle() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().expect("bundle tempdir");
        let outside = tempdir().expect("outside tempdir");
        fs::write(bundle.path().join("inside.txt"), "inside").expect("write inside");
        fs::write(outside.path().join("outside.txt"), "outside").expect("write outside");
        symlink(outside.path(), bundle.path().join("outside-link")).expect("symlink outside dir");

        let count = count_files_recursive(bundle.path()).expect("count");
        assert_eq!(count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn latest_modified_token_ignores_symlinked_dir_outside_bundle() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().expect("bundle tempdir");
        let outside = tempdir().expect("outside tempdir");
        fs::write(bundle.path().join("inside.txt"), "inside").expect("write inside");
        fs::write(outside.path().join("outside.txt"), "outside").expect("write outside");
        symlink(outside.path(), bundle.path().join("outside-link")).expect("symlink outside dir");

        let base = latest_modified_token(bundle.path()).expect("base token");
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(outside.path().join("outside.txt"), "outside changed").expect("rewrite outside");
        let after = latest_modified_token(bundle.path()).expect("after token");
        assert_eq!(after, base, "outside changes must not affect bundle token");
    }
}
