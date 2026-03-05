use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::Utc;
use serde::Serialize;

use crate::analysis::Analyzer;
use crate::analysis::triage::TriageOptions;
use crate::model::Issue;
use crate::{BvrError, Result};

const DEFAULT_PAGES_TITLE: &str = "Project Issues";
const DEFAULT_PREVIEW_PORT: u16 = 9000;
const MAX_PREVIEW_PORT_ATTEMPTS: u16 = 32;
const PREVIEW_MAX_REQUESTS_ENV: &str = "BVR_PREVIEW_MAX_REQUESTS";

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

pub fn print_pages_wizard() {
    println!("bvr pages wizard (non-interactive)");
    println!();
    println!("Recommended flow:");
    println!("  1) Export bundle:  bvr --export-pages ./bv-pages");
    println!("  2) Preview bundle: bvr --preview-pages ./bv-pages");
    println!("  3) Optional watch: bvr --export-pages ./bv-pages --watch-export");
    println!("  4) Deploy ./bv-pages to GitHub Pages, Cloudflare Pages, or any static host");
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

    fs::create_dir_all(output_dir.join("assets"))?;
    fs::create_dir_all(output_dir.join("data"))?;

    let analyzer = Analyzer::new(filtered.clone());
    let triage = analyzer.triage(TriageOptions {
        group_by_track: false,
        group_by_label: false,
        max_recommendations: 50,
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

    write_text(
        output_dir.join("index.html"),
        &render_index_html(&title, options.include_history),
    )?;
    files.push("index.html".to_string());

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

    if options.include_history {
        let history_limit = filtered.len().max(500);
        let history = analyzer.history(None, history_limit);
        write_json(output_dir.join("data/history.json"), &history)?;
        files.push("data/history.json".to_string());
    }

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
    println!("Preview server running at http://127.0.0.1:{port}");
    println!("Serving bundle: {}", bundle_dir.display());
    println!(
        "Live reload: {}",
        if live_reload { "enabled" } else { "disabled" }
    );
    println!("Press Ctrl+C to stop.");

    let max_requests = std::env::var(PREVIEW_MAX_REQUESTS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0);
    let mut served = 0usize;

    loop {
        let (stream, _) = listener.accept()?;
        if let Err(error) = handle_preview_request(stream, bundle_dir, live_reload) {
            eprintln!("warning: preview request failed: {error}");
        }
        served = served.saturating_add(1);
        if max_requests.is_some_and(|limit| served >= limit) {
            return Ok(());
        }
    }
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
            Err(error) => return Err(BvrError::Io(error)),
        }
    }

    Err(BvrError::InvalidArgument(format!(
        "could not bind preview server on ports {base_port}..{}",
        base_port.saturating_add(MAX_PREVIEW_PORT_ATTEMPTS.saturating_sub(1))
    )))
}

fn handle_preview_request(
    mut stream: TcpStream,
    bundle_dir: &Path,
    live_reload: bool,
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
    if route == "/.bvr/livereload" {
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

    let file_path = bundle_dir.join(&relative);
    if !file_path.is_file() {
        write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found\n",
            head_only,
        )?;
        return Ok(());
    }

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
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
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

fn latest_modified_token(path: &Path) -> Result<u64> {
    latest_modified_recursive(path, 0)
}

fn latest_modified_recursive(path: &Path, mut latest: u64) -> Result<u64> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    latest = latest.max(modified);

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            latest = latest_modified_recursive(&entry.path(), latest)?;
        }
    }

    Ok(latest)
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

fn render_index_html(title: &str, include_history: bool) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <link rel="stylesheet" href="assets/style.css">
</head>
<body>
  <main class="layout">
    <header>
      <h1>{title}</h1>
      <p id="meta-line" class="meta">Loading export metadata...</p>
    </header>
    <section class="grid">
      <article>
        <h2>Issues</h2>
        <ul id="issue-list" class="issue-list"></ul>
      </article>
      <article>
        <h2>Triage Top Picks</h2>
        <ol id="top-picks" class="pick-list"></ol>
      </article>
      <article>
        <h2>Insights</h2>
        <pre id="insights" class="insights"></pre>
      </article>
    </section>
    <footer class="meta">
      <span>History payload: {history_state}</span>
    </footer>
  </main>
  <script src="assets/viewer.js"></script>
</body>
</html>
"#,
        history_state = if include_history {
            "included"
        } else {
            "excluded"
        }
    )
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
}
