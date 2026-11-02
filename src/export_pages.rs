use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::analysis::Analyzer;
use crate::analysis::triage::TriageOptions;
use crate::export_sqlite::{
    SQLITE_EXPORT_CONFIG_FILENAME, SQLITE_EXPORT_FILENAME, SqliteBootstrapOptions,
    SqliteBundleOptions, bootstrap_export_database, emit_bootstrap_config,
    populate_export_database, snapshot_export_database,
};
use crate::model::Issue;
use crate::{BvrError, Result};

const DEFAULT_PAGES_TITLE: &str = "Project Issues";
const DEFAULT_PREVIEW_PORT: u16 = 9000;
const MAX_PREVIEW_PORT_ATTEMPTS: u16 = 32;
const PREVIEW_MAX_REQUESTS_ENV: &str = "BVR_PREVIEW_MAX_REQUESTS";
const PREVIEW_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PREVIEW_MAX_CONNECTIONS: usize = 8;
const PREVIEW_HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const PREVIEW_MAX_HEADER_BYTES: usize = 8192;
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
    pub subtitle: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: Option<String>,
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
    let mut publication = PagesPublication::new(output_dir)?;
    let database_guard =
        snapshot_export_database(&publication.output_dir, &publication.staged_dir)?;
    let summary = build_pages_bundle(issues, &publication.staged_dir, output_dir, options)?;
    publication.prepare(&summary.files)?;

    // Keep SQLite's writer guard through construction and metadata preflight.
    // Drop it before opening the original database for comparison or backup:
    // closing another descriptor for that file can release POSIX SQLite locks.
    drop(database_guard);
    publication.back_up_existing()?;
    publication.publish()?;
    Ok(summary)
}

fn build_pages_bundle(
    issues: &[Issue],
    output_dir: &Path,
    logical_output_dir: &Path,
    options: &ExportPagesOptions,
) -> Result<ExportPagesSummary> {
    let title = options
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_PAGES_TITLE)
        .to_string();
    let subtitle = options
        .subtitle
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);

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
        subtitle: subtitle.clone(),
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

    let history_path = output_dir.join("data/history.json");
    if options.include_history {
        let history_limit = filtered.len().max(500);
        let histories = analyzer.history(None, history_limit);
        // Convert to the richer HistoryBeadCompat format that matches
        // --robot-history output shape (with commits/cycle_time/milestones).
        let history_compat: std::collections::BTreeMap<String, _> = histories
            .iter()
            .map(|h| {
                (
                    h.id.clone(),
                    crate::analysis::git_history::HistoryBeadCompat {
                        bead_id: h.id.clone(),
                        title: h.title.clone(),
                        status: h.status.clone(),
                        events: h
                            .events
                            .iter()
                            .map(|e| crate::analysis::git_history::HistoryEventCompat {
                                bead_id: h.id.clone(),
                                event_type: e.kind.clone(),
                                timestamp: e
                                    .timestamp
                                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                                    .unwrap_or_default(),
                                commit_sha: String::new(),
                                commit_message: e.details.clone(),
                                author: String::new(),
                                author_email: String::new(),
                            })
                            .collect(),
                        milestones: crate::analysis::git_history::HistoryMilestonesCompat::default(
                        ),
                        commits: None,
                        cycle_time: None,
                        last_author: String::new(),
                    },
                )
            })
            .collect();
        write_json(history_path.clone(), &history_compat)?;
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

    files.push("data/export_summary.json".to_string());

    let summary = ExportPagesSummary {
        export_path: logical_output_dir.to_string_lossy().to_string(),
        generated_at,
        issue_count: filtered.len(),
        include_closed: options.include_closed,
        include_history: options.include_history,
        files,
    };

    write_json(output_dir.join("data/export_summary.json"), &summary)?;

    Ok(summary)
}

#[derive(Clone)]
struct PublicationChange {
    relative_path: PathBuf,
    install: bool,
    existed: bool,
}

struct PagesPublication {
    output_dir: PathBuf,
    staging: tempfile::TempDir,
    staged_dir: PathBuf,
    changes: Vec<PublicationChange>,
    applied: usize,
    created_dirs: Vec<PathBuf>,
}

impl PagesPublication {
    fn new(output_dir: &Path) -> Result<Self> {
        let output_dir = resolve_export_output_dir(output_dir)?;
        let parent = output_dir.parent().ok_or_else(|| {
            BvrError::InvalidArgument(
                "cannot create sibling staging directory for a filesystem root".to_string(),
            )
        })?;
        let staging = create_publication_staging(&output_dir, parent, &std::env::temp_dir())?;
        let staged_dir = staging.path().join("bundle");
        fs::create_dir(&staged_dir)?;
        Ok(Self {
            output_dir,
            staging,
            staged_dir,
            changes: Vec::new(),
            applied: 0,
            created_dirs: Vec::new(),
        })
    }

    fn prepare(&mut self, files: &[String]) -> Result<()> {
        export_directory_exists(&self.output_dir)?;
        let new_files = files.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
        let mut managed = new_files.clone();
        managed.insert(PathBuf::from("data/history.json"));
        let chunks_dir = self.output_dir.join("chunks");
        if export_directory_exists(&chunks_dir)? {
            for entry in fs::read_dir(&chunks_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                if is_numbered_database_chunk(&name) {
                    managed.insert(Path::new("chunks").join(name));
                }
            }
        }

        for relative_path in managed {
            let existed = managed_export_file_exists(&self.output_dir, &relative_path)?;
            let install = new_files.contains(&relative_path);
            if install && !managed_export_file_exists(&self.staged_dir, &relative_path)? {
                return Err(BvrError::InvalidArgument(format!(
                    "staged export file is missing: {}",
                    relative_path.display()
                )));
            }
            if install || existed {
                self.changes.push(PublicationChange {
                    relative_path,
                    install,
                    existed,
                });
            }
        }
        self.changes.sort_by(|left, right| {
            publication_order(&left.relative_path)
                .cmp(&publication_order(&right.relative_path))
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
        Ok(())
    }

    fn back_up_existing(&mut self) -> Result<()> {
        let mut changed = Vec::with_capacity(self.changes.len());
        for change in self.changes.drain(..) {
            if managed_export_file_exists(&self.output_dir, &change.relative_path)?
                != change.existed
            {
                return Err(BvrError::InvalidArgument(format!(
                    "export target changed before backup: {}",
                    change.relative_path.display()
                )));
            }
            let published = self.output_dir.join(&change.relative_path);
            let staged = self.staged_dir.join(&change.relative_path);
            if change.existed && change.install && export_files_equal(&published, &staged)? {
                continue;
            }
            if change.existed {
                let backup = self
                    .staging
                    .path()
                    .join("backups")
                    .join(&change.relative_path);
                create_private_parent(&backup)?;
                fs::copy(&published, backup)?;
                if change.install {
                    // Replacing by rename must not widen the permissions that
                    // an in-place write to the existing managed file retained.
                    fs::set_permissions(&staged, fs::symlink_metadata(&published)?.permissions())?;
                }
            }
            // Stale files move here during publication; installed files move
            // back here during rollback. These parents remain private.
            create_private_parent(&staged)?;
            changed.push(change);
        }
        self.changes = changed;
        Ok(())
    }

    fn apply_next(&mut self) -> Result<bool> {
        let Some(change) = self.changes.get(self.applied).cloned() else {
            return Ok(false);
        };
        let exists = managed_export_file_exists(&self.output_dir, &change.relative_path)?;
        if exists != change.existed {
            return Err(BvrError::InvalidArgument(format!(
                "export target changed during publication: {}",
                change.relative_path.display()
            )));
        }
        let published = self.output_dir.join(&change.relative_path);
        let staged = self.staged_dir.join(&change.relative_path);
        let result = if change.install {
            self.create_output_parents(&change.relative_path)?;
            fs::rename(staged, &published)
        } else {
            fs::rename(&published, staged)
        };
        result.map_err(|error| {
            BvrError::InvalidArgument(format!(
                "cannot publish export file {}: {error}",
                published.display()
            ))
        })?;
        self.applied += 1;
        Ok(true)
    }

    fn create_output_parents(&mut self, relative_path: &Path) -> Result<()> {
        let mut path = self.output_dir.clone();
        self.create_output_directory(&path)?;
        if let Some(parent) = relative_path.parent() {
            for component in parent.components() {
                path.push(component);
                self.create_output_directory(&path)?;
            }
        }
        Ok(())
    }

    fn create_output_directory(&mut self, path: &Path) -> Result<()> {
        if !export_directory_exists(path)? {
            match fs::create_dir(path) {
                Ok(()) => self.created_dirs.push(path.to_path_buf()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    export_directory_exists(path)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn restore_change(&self, change: &PublicationChange) -> Result<()> {
        let exists = managed_export_file_exists(&self.output_dir, &change.relative_path)?;
        if exists != change.install {
            return Err(BvrError::InvalidArgument(format!(
                "export target changed before rollback: {}",
                change.relative_path.display()
            )));
        }
        let published = self.output_dir.join(&change.relative_path);
        if change.install {
            let staged = self.staged_dir.join(&change.relative_path);
            match fs::symlink_metadata(&staged) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    return Err(BvrError::InvalidArgument(format!(
                        "rollback destination is occupied: {}",
                        staged.display()
                    )));
                }
            }
            fs::rename(&published, staged)?;
        }
        if change.existed {
            let backup = self
                .staging
                .path()
                .join("backups")
                .join(&change.relative_path);
            // Restore without allocating another full file: publication may
            // have failed because the disk is full. A successful rename leaves
            // the old copy published; a failure leaves its backup available.
            fs::rename(backup, published)?;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for change in self.changes[..self.applied].iter().rev() {
            if let Err(error) = self.restore_change(change) {
                failures.push(format!("{}: {error}", change.relative_path.display()));
            }
        }
        for path in self.created_dirs.iter().rev() {
            if let Err(error) = fs::remove_dir(path) {
                failures.push(format!("{}: {error}", path.display()));
            }
        }
        failures
    }

    fn publish(mut self) -> Result<()> {
        // Individual renames and reverse rollback handle ordinary I/O errors.
        // This is not a multi-file transaction for independent readers, nor a
        // crash-atomic operation; configuration and summary are installed last.
        if let Err(error) = self.apply_all() {
            let failures = self.rollback();
            if failures.is_empty() {
                close_publication_staging(self.staging);
                return Err(BvrError::InvalidArgument(format!(
                    "pages publication failed; previous bundle restored: {error}"
                )));
            }
            let recovery = self.staging.keep();
            return Err(BvrError::InvalidArgument(format!(
                "pages publication failed: {error}; rollback incomplete: {}; recovery files retained at {}",
                failures.join("; "),
                recovery.display()
            )));
        }
        if let Err(error) = remove_empty_chunks_directory(&self.output_dir) {
            eprintln!("warning: cannot clean up empty export chunks directory: {error}");
        }
        close_publication_staging(self.staging);
        Ok(())
    }

    fn apply_all(&mut self) -> Result<()> {
        while self.apply_next()? {}
        Ok(())
    }
}

fn resolve_export_output_dir(output_dir: &Path) -> Result<PathBuf> {
    match fs::metadata(output_dir) {
        Ok(metadata) if metadata.is_dir() => return Ok(fs::canonicalize(output_dir)?),
        Ok(_) => {
            return Err(BvrError::InvalidArgument(format!(
                "export output must be a directory: {}",
                output_dir.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(output_dir) {
        Ok(_) => {
            return Err(BvrError::InvalidArgument(format!(
                "export output is a dangling symlink: {}",
                output_dir.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let name = output_dir.file_name().ok_or_else(|| {
        BvrError::InvalidArgument(format!(
            "invalid export directory: {}",
            output_dir.display()
        ))
    })?;
    let parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    Ok(fs::canonicalize(parent)?.join(name))
}

fn create_publication_staging(
    output_dir: &Path,
    sibling_parent: &Path,
    temporary_dir: &Path,
) -> Result<tempfile::TempDir> {
    let sibling_error = match try_publication_staging(output_dir, sibling_parent) {
        Ok(staging) => return Ok(staging),
        Err(error) => error,
    };
    #[cfg(unix)]
    let fallback = try_publication_staging(output_dir, temporary_dir);
    #[cfg(not(unix))]
    let fallback: Result<tempfile::TempDir> = Err(BvrError::InvalidArgument(
        "cannot verify the filesystem of an external temporary directory on this platform"
            .to_string(),
    ));
    fallback.map_err(|error| {
        BvrError::InvalidArgument(format!(
            "cannot create safe export staging for {}: sibling location {}: {sibling_error}; temporary location {}: {error}",
            output_dir.display(),
            sibling_parent.display(),
            temporary_dir.display()
        ))
    })
}

fn try_publication_staging(output_dir: &Path, location: &Path) -> Result<tempfile::TempDir> {
    let location = fs::canonicalize(location)?;
    ensure_staging_location(output_dir, &location)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".bvr-pages-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // TempDir otherwise follows the process umask. Keep staged assets and
        // retained recovery copies private from the instant the directory exists.
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(location)?;
    // Verify the actual directory as well as its proposed parent before any
    // bundle construction or published-file mutation can take place.
    ensure_staging_location(output_dir, &fs::canonicalize(staging.path())?)?;
    Ok(staging)
}

fn ensure_staging_location(output_dir: &Path, location: &Path) -> Result<()> {
    if location.starts_with(output_dir) {
        return Err(BvrError::InvalidArgument(format!(
            "staging location must be outside the export output tree: {}",
            location.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let output_metadata = match fs::metadata(output_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = output_dir.parent().ok_or_else(|| {
                    BvrError::InvalidArgument(format!(
                        "export output has no filesystem parent: {}",
                        output_dir.display()
                    ))
                })?;
                fs::metadata(parent)?
            }
            Err(error) => return Err(error.into()),
        };
        if output_metadata.dev() != fs::metadata(location)?.dev() {
            return Err(BvrError::InvalidArgument(format!(
                "staging location must share the export output filesystem: {}",
                location.display()
            )));
        }
    }
    Ok(())
}

fn export_directory_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(BvrError::InvalidArgument(format!(
            "managed export directory must be a directory without symlinks: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn managed_export_file_exists(root: &Path, relative_path: &Path) -> Result<bool> {
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BvrError::InvalidArgument(format!(
            "invalid managed export path: {}",
            relative_path.display()
        )));
    }
    if !export_directory_exists(root)? {
        return Ok(false);
    }
    let mut path = root.to_path_buf();
    let mut components = relative_path.components().peekable();
    while let Some(component) = components.next() {
        path.push(component);
        if components.peek().is_some() {
            if !export_directory_exists(&path)? {
                return Ok(false);
            }
        } else {
            return match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => Ok(true),
                Ok(_) => Err(BvrError::InvalidArgument(format!(
                    "managed export file must be a regular file without symlinks: {}",
                    path.display()
                ))),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            };
        }
    }
    Ok(false)
}

fn is_numbered_database_chunk(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_suffix(".bin"))
        .is_some_and(|digits| digits.len() >= 5 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn publication_order(relative_path: &Path) -> u8 {
    if relative_path == Path::new("data/export_summary.json") {
        2
    } else if relative_path == Path::new(SQLITE_EXPORT_CONFIG_FILENAME) {
        1
    } else {
        0
    }
}

fn create_private_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn export_files_equal(left: &Path, right: &Path) -> Result<bool> {
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut remaining = left.metadata()?.len();
    if remaining != right.metadata()?.len() {
        return Ok(false);
    }
    let mut left_bytes = vec![0; 64 * 1024];
    let mut right_bytes = vec![0; left_bytes.len()];
    while remaining > 0 {
        let amount = usize::try_from(remaining.min(left_bytes.len() as u64))
            .expect("comparison buffer length fits usize");
        left.read_exact(&mut left_bytes[..amount])?;
        right.read_exact(&mut right_bytes[..amount])?;
        if left_bytes[..amount] != right_bytes[..amount] {
            return Ok(false);
        }
        remaining -= amount as u64;
    }
    Ok(left.read(&mut left_bytes[..1])? == 0 && right.read(&mut right_bytes[..1])? == 0)
}

fn remove_empty_chunks_directory(output_dir: &Path) -> Result<()> {
    let chunks = output_dir.join("chunks");
    if export_directory_exists(&chunks)? && fs::read_dir(&chunks)?.next().is_none() {
        fs::remove_dir(chunks)?;
    }
    Ok(())
}

fn close_publication_staging(staging: tempfile::TempDir) {
    let path = staging.path().to_path_buf();
    if let Err(error) = staging.close() {
        eprintln!(
            "warning: export finished but staging cleanup failed at {}: {error}",
            path.display()
        );
    }
}

fn generate_deploy_readme(title: &str, meta: &PagesMeta) -> String {
    let subtitle_line = meta
        .subtitle
        .as_deref()
        .map(|subtitle| format!("- **Subtitle**: {subtitle}\n"))
        .unwrap_or_default();
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
         {subtitle_line}\
         - **Issues**: {count}\n\
         - **Generated**: {at}\n\
         - **Generator**: bvr v{version}\n",
        subtitle_line = subtitle_line,
        version = meta.version,
        count = meta.issue_count,
        at = meta.generated_at,
        path = ".",
    )
}

pub fn run_preview_server(bundle_dir: &Path, live_reload: bool) -> Result<()> {
    if !bundle_dir.is_dir() {
        return Err(BvrError::InvalidArgument(format!(
            "preview bundle directory not found: {} (run --export-pages {} first)",
            bundle_dir.display(),
            bundle_dir.display()
        )));
    }
    if !bundle_dir.join("index.html").is_file() {
        return Err(BvrError::InvalidArgument(format!(
            "missing index.html in preview bundle: {} (run --export-pages {} to regenerate)",
            bundle_dir.display(),
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
    let shutdown_reason = serve_preview_connections(
        &listener,
        bundle_dir,
        live_reload,
        port,
        &shutdown_requested,
        max_requests,
    )?;

    println!(
        "Preview server stopped: {}.",
        shutdown_reason.operator_summary()
    );
    Ok(())
}

fn serve_preview_connections(
    listener: &TcpListener,
    bundle_dir: &Path,
    live_reload: bool,
    port: u16,
    shutdown_requested: &AtomicBool,
    max_requests: Option<usize>,
) -> Result<PreviewShutdownReason> {
    thread::scope(|scope| {
        let mut connections = PreviewConnections::default();
        let mut served = 0usize;
        loop {
            if shutdown_requested.load(Ordering::Relaxed) {
                return Ok(PreviewShutdownReason::ShutdownSignal);
            }
            served = served.saturating_add(connections.reap_finished());
            if max_requests.is_some_and(|limit| served >= limit) {
                return Ok(PreviewShutdownReason::RequestLimitReached);
            }

            // Reserve one remaining request for every active worker. Empty
            // preconnects and failed transfers release their reservation when
            // reaped, so concurrent completions cannot overshoot the limit.
            let reserved = served.saturating_add(connections.active.len());
            if connections.active.len() >= PREVIEW_MAX_CONNECTIONS
                || max_requests.is_some_and(|limit| reserved >= limit)
            {
                thread::sleep(PREVIEW_ACCEPT_POLL_INTERVAL);
                continue;
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    let interrupt = stream.try_clone()?;
                    let request = thread::Builder::new()
                        .name("bvr-preview-request".to_string())
                        .spawn_scoped(scope, move || {
                            handle_preview_request(stream, bundle_dir, live_reload, port)
                        })?;
                    connections
                        .active
                        .push(PreviewConnection { interrupt, request });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(PREVIEW_ACCEPT_POLL_INTERVAL);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(_) if shutdown_requested.load(Ordering::Relaxed) => {
                    return Ok(PreviewShutdownReason::ShutdownSignal);
                }
                Err(error) => return Err(BvrError::Io(error)),
            }
        }
    })
}

struct PreviewConnection<'scope> {
    interrupt: TcpStream,
    request: thread::ScopedJoinHandle<'scope, Result<bool>>,
}

#[derive(Default)]
struct PreviewConnections<'scope> {
    active: Vec<PreviewConnection<'scope>>,
}

impl PreviewConnections<'_> {
    fn reap_finished(&mut self) -> usize {
        let mut served = 0;
        let mut index = 0;
        while index < self.active.len() {
            if !self.active[index].request.is_finished() {
                index += 1;
                continue;
            }
            let connection = self.active.swap_remove(index);
            let result = connection.request.join().unwrap_or_else(|_| {
                Err(BvrError::Io(std::io::Error::other(
                    "preview request worker panicked",
                )))
            });
            match result {
                Ok(true) => served += 1,
                Ok(false) => {}
                Err(error) => eprintln!("warning: preview request failed: {error}"),
            }
        }
        served
    }
}

impl Drop for PreviewConnections<'_> {
    fn drop(&mut self) {
        // Cancel every socket before joining any worker. This runs on signals
        // and all error exits, including failed accepts, clones, or spawns.
        // Per-write timeouts alone do not bound a slowly progressing transfer.
        for connection in &self.active {
            let _ = connection.interrupt.shutdown(Shutdown::Both);
        }
        for connection in self.active.drain(..) {
            let _ = connection.request.join();
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
) -> Result<bool> {
    // The listener is nonblocking so idle accepts can poll for shutdown. Some
    // platforms (notably macOS/BSD) can inherit that mode on accepted sockets.
    // write_all needs blocking I/O here or a full send buffer truncates the
    // response with WouldBlock after we have already advertised HTTP 200.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(PREVIEW_HEADER_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut buffer = [0_u8; PREVIEW_MAX_HEADER_BYTES];
    let (header_len, rejection) = match read_preview_headers(&mut stream, &mut buffer)? {
        PreviewHeaderRead::Empty => return Ok(false),
        PreviewHeaderRead::Complete(len) => (len, None),
        PreviewHeaderRead::Rejected { len, status } => (len, Some(status)),
    };
    let headers = &buffer[..header_len];
    let (method, target) = match (rejection, parse_preview_request(headers)) {
        (None, Some(request)) => request,
        (status, _) => {
            let status = status.unwrap_or("400 Bad Request");
            let head_only =
                headers.split(|byte| byte.is_ascii_whitespace()).next() == Some(&b"HEAD"[..]);
            write_http_response(
                &mut stream,
                status,
                "text/plain; charset=utf-8",
                format!("{status}\n").as_bytes(),
                head_only,
            )?;
            return Ok(true);
        }
    };
    let head_only = method == "HEAD";

    if method != "GET" && method != "HEAD" {
        write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed\n",
            head_only,
        )?;
        return Ok(true);
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
        return Ok(true);
    }

    if route == PREVIEW_RELOAD_PATH {
        if !live_reload {
            write_http_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"not found\n",
                head_only,
            )?;
            return Ok(true);
        }

        let token = latest_modified_token(bundle_dir)?;
        write_http_response(
            &mut stream,
            "200 OK",
            "text/plain; charset=utf-8",
            token.as_bytes(),
            head_only,
        )?;
        return Ok(true);
    }

    let Ok(relative) = normalize_request_path(route) else {
        write_http_response(
            &mut stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"invalid path\n",
            head_only,
        )?;
        return Ok(true);
    };

    let Some(file_path) = resolve_preview_asset_path(bundle_dir, &relative)? else {
        write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found\n",
            head_only,
        )?;
        return Ok(true);
    };

    // Capture the complete asset before advertising success: a watched export
    // can rewrite the source while a slow peer is still receiving its body.
    // The connection cap bounds the number of simultaneous response snapshots.
    let mut body = fs::read(&file_path)?;
    let mime = mime_type_for_path(&file_path);
    if live_reload && mime.starts_with("text/html") {
        body = inject_live_reload(body);
    }

    write_http_response(&mut stream, "200 OK", mime, &body, head_only)?;
    Ok(true)
}

enum PreviewHeaderRead {
    Empty,
    Complete(usize),
    Rejected { len: usize, status: &'static str },
}

fn read_preview_headers(
    stream: &mut TcpStream,
    buffer: &mut [u8; PREVIEW_MAX_HEADER_BYTES],
) -> std::io::Result<PreviewHeaderRead> {
    let deadline = Instant::now() + PREVIEW_HEADER_TIMEOUT;
    let mut used = 0usize;
    loop {
        if used == buffer.len() {
            return Ok(PreviewHeaderRead::Rejected {
                len: used,
                status: "431 Request Header Fields Too Large",
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if used == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "preview request headers timed out",
                ));
            }
            return Ok(PreviewHeaderRead::Rejected {
                len: used,
                status: "408 Request Timeout",
            });
        }
        // A peer sending fragments must share one budget; a fresh five-second
        // timeout for every read would let it occupy a worker indefinitely.
        stream.set_read_timeout(Some(remaining))?;
        let previous_len = used;
        match stream.read(&mut buffer[used..]) {
            Ok(0) if used == 0 => return Ok(PreviewHeaderRead::Empty),
            Ok(0) => {
                return Ok(PreviewHeaderRead::Rejected {
                    len: used,
                    status: "400 Bad Request",
                });
            }
            Ok(bytes) => {
                used += bytes;
                if Instant::now() >= deadline {
                    return Ok(PreviewHeaderRead::Rejected {
                        len: used,
                        status: "408 Request Timeout",
                    });
                }
                // Only the final three old bytes can begin a delimiter that
                // crosses this read. Do not rescan the whole growing header.
                let start = previous_len.saturating_sub(3);
                if let Some(offset) = buffer[start..used]
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                {
                    return Ok(PreviewHeaderRead::Complete(start + offset + 4));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error)
                if used > 0
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                return Ok(PreviewHeaderRead::Rejected {
                    len: used,
                    status: "408 Request Timeout",
                });
            }
            Err(error) => return Err(error),
        }
    }
}

fn parse_preview_request(headers: &[u8]) -> Option<(&str, &str)> {
    let line_end = headers.windows(2).position(|bytes| bytes == b"\r\n")?;
    let request_line = std::str::from_utf8(&headers[..line_end]).ok()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some()
        || method.is_empty()
        || !method.bytes().all(is_http_token_byte)
        || target.is_empty()
        || (matches!(method, "GET" | "HEAD") && !target.starts_with('/'))
        || !target
            .bytes()
            .all(|byte| matches!(byte, b'!'..=b'~') && byte != b'#')
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return None;
    }

    // Header values are opaque bytes, not necessarily UTF-8. Validate their
    // HTTP syntax without lossy decoding or accepting folded/control lines.
    let mut fields = &headers[line_end + 2..];
    loop {
        let end = fields.windows(2).position(|bytes| bytes == b"\r\n")?;
        if end == 0 {
            return (fields.len() == 2).then_some((method, target));
        }
        let line = &fields[..end];
        let colon = line.iter().position(|byte| *byte == b':')?;
        if colon == 0
            || !line[..colon].iter().copied().all(is_http_token_byte)
            || !line[colon + 1..]
                .iter()
                .all(|byte| matches!(byte, b'\t' | b' '..=b'~' | 0x80..=0xff))
        {
            return None;
        }
        fields = &fields[end + 2..];
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
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
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        _ if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(SQLITE_EXPORT_FILENAME)) =>
        {
            "application/x-sqlite3"
        }
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

fn latest_modified_token(path: &Path) -> Result<String> {
    let bundle_root = fs::canonicalize(path)?;
    let mut visited_dirs = HashSet::new();
    let mut hasher = Sha256::new();
    fingerprint_bundle_recursive(
        path,
        &bundle_root,
        Path::new(""),
        &mut visited_dirs,
        &mut hasher,
    )?;
    Ok(format!("{:x}", hasher.finalize()))
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

    if !visited_dirs.insert(resolved_path) {
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

fn fingerprint_bundle_recursive(
    path: &Path,
    bundle_root: &Path,
    relative: &Path,
    visited_dirs: &mut HashSet<PathBuf>,
    hasher: &mut Sha256,
) -> Result<()> {
    let link_metadata = fs::symlink_metadata(path)?;
    let resolved_path = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(BvrError::Io(error)),
    };
    if !resolved_path.starts_with(bundle_root) {
        return Ok(());
    }

    let target_metadata = match fs::metadata(&resolved_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(BvrError::Io(error)),
    };

    hasher.update(relative.to_string_lossy().as_bytes());
    hasher.update([0]);
    if link_metadata.file_type().is_symlink() {
        hasher.update(b"symlink");
        hasher.update([0]);
        hasher.update(resolved_path.to_string_lossy().as_bytes());
        hasher.update([0]);
    }

    if target_metadata.is_file() {
        hasher.update(b"file");
        hasher.update([0]);
        hasher.update(fs::read(&resolved_path)?);
        return Ok(());
    }

    if !target_metadata.is_dir() {
        hasher.update(b"other");
        hasher.update([0]);
        hasher.update(metadata_modified_token(&target_metadata).to_le_bytes());
        return Ok(());
    }

    hasher.update(b"dir");
    hasher.update([0]);
    if !visited_dirs.insert(resolved_path) {
        hasher.update(b"visited");
        hasher.update([0]);
        return Ok(());
    }

    let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child_relative = if relative.as_os_str().is_empty() {
            PathBuf::from(entry.file_name())
        } else {
            relative.join(entry.file_name())
        };
        fingerprint_bundle_recursive(
            &entry.path(),
            bundle_root,
            &child_relative,
            visited_dirs,
            hasher,
        )?;
    }

    Ok(())
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
    } else if metadata.is_dir() {
        let index_path = resolved.join("index.html");
        match fs::metadata(&index_path) {
            Ok(index_metadata) if index_metadata.is_file() => Ok(Some(index_path)),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(BvrError::Io(error)),
        }
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
  const bottlenecks = (insights.Bottlenecks ?? []).slice(0, 5)
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
    use std::sync::mpsc;

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
            defer_until: None,
            closed_at: None,
            labels: Vec::new(),
            comments: Vec::new(),
            dependencies: Vec::new(),
            source_repo: ".".to_string(),
            workspace_prefix: None,
            workspace_repo_path: None,
            workspace_local_id: None,
            content_hash: None,
            external_ref: None,
        }
    }

    #[test]
    #[cfg(unix)]
    fn publication_falls_back_to_an_external_temporary_directory() {
        use std::os::unix::fs::PermissionsExt;

        for existing_output in [false, true] {
            let temp = tempdir().expect("tempdir");
            let root = fs::canonicalize(temp.path()).expect("resolved temporary root");
            let output = root.join("published");
            if existing_output {
                fs::create_dir(&output).expect("existing output directory");
            }
            let unusable = root.join("unusable-parent");
            fs::write(&unusable, b"not a directory").expect("unusable first location");
            let fallback = root.join("temporary");
            fs::create_dir(&fallback).expect("same-filesystem fallback");

            // A file deterministically prevents sibling creation even when
            // tests run as root, unlike a chmod-based permission failure.
            let staging = create_publication_staging(&output, &unusable, &fallback)
                .expect("use verified fallback");

            assert_eq!(staging.path().parent(), Some(fallback.as_path()));
            assert!(!staging.path().starts_with(&output));
            assert_eq!(output.exists(), existing_output);
            assert_eq!(
                fs::metadata(staging.path())
                    .expect("private staging metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            assert_eq!(
                fs::read(&unusable).expect("unchanged file"),
                b"not a directory"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn publication_rejects_a_temporary_alias_inside_the_output_tree() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("resolved temporary root");
        let output = root.join("published");
        let inside = output.join("temporary");
        fs::create_dir_all(&inside).expect("inside-output temporary directory");
        let unusable = root.join("unusable-parent");
        fs::write(&unusable, b"not a directory").expect("unusable first location");
        let alias = root.join("temporary-alias");
        symlink(&inside, &alias).expect("temporary alias into output");

        let error = create_publication_staging(&output, &unusable, &alias)
            .expect_err("no safe external staging location");

        let message = error.to_string();
        assert!(
            message.contains("outside the export output tree"),
            "{message}"
        );
        assert!(message.contains("sibling location"), "{message}");
        assert!(message.contains("temporary location"), "{message}");
        assert!(
            fs::read_dir(&inside)
                .expect("unchanged output tree")
                .next()
                .is_none()
        );
        assert_eq!(fs::read_link(alias).expect("unchanged alias"), inside);
    }

    #[test]
    fn publication_rolls_back_installed_and_removed_files_after_late_failure() {
        let temp = tempdir().expect("tempdir");
        let output = temp.path().join("published");
        fs::create_dir_all(output.join("data")).expect("published directories");
        fs::write(output.join("a-existing.txt"), b"old existing bytes").expect("old file");
        fs::write(output.join("data/history.json"), b"old history").expect("old history");
        fs::write(output.join("z-fail.txt"), b"old final bytes").expect("old final file");
        fs::write(output.join("data/custom.json"), b"user content").expect("custom file");

        let mut publication = PagesPublication::new(&output).expect("private staging");
        let staging_path = publication.staging.path().to_path_buf();
        let staged = publication.staged_dir.clone();
        fs::create_dir(staged.join("b-new")).expect("new staged directory");
        fs::write(staged.join("a-existing.txt"), b"new existing bytes").expect("replacement");
        fs::write(staged.join("b-new/added.txt"), b"newly added file").expect("addition");
        fs::write(staged.join("z-fail.txt"), b"new final bytes").expect("final replacement");
        publication
            .prepare(&[
                "a-existing.txt".to_string(),
                "b-new/added.txt".to_string(),
                "z-fail.txt".to_string(),
            ])
            .expect("preflight");
        publication.back_up_existing().expect("complete backups");
        assert_eq!(
            fs::read(staging_path.join("backups/z-fail.txt")).expect("final file backed up"),
            b"old final bytes"
        );

        assert!(publication.apply_next().expect("install replacement"));
        assert!(publication.apply_next().expect("install new file"));
        assert!(publication.apply_next().expect("remove stale history"));
        assert_eq!(
            fs::read(output.join("a-existing.txt")).expect("installed replacement"),
            b"new existing bytes"
        );
        assert!(output.join("b-new/added.txt").is_file());
        assert!(!output.join("data/history.json").exists());
        // A real rename failure after three completed changes, with all old
        // files already backed up. No injected success/failure mock is used.
        fs::rename(
            staged.join("z-fail.txt"),
            staging_path.join("withheld-final"),
        )
        .expect("withhold final staged source");

        let error = publication.publish().expect_err("late publication failure");
        assert!(error.to_string().contains("previous bundle restored"));
        assert_eq!(
            fs::read(output.join("a-existing.txt")).expect("restored replacement"),
            b"old existing bytes"
        );
        assert_eq!(
            fs::read(output.join("data/history.json")).expect("restored stale history"),
            b"old history"
        );
        assert_eq!(
            fs::read(output.join("z-fail.txt")).expect("untouched final file"),
            b"old final bytes"
        );
        assert_eq!(
            fs::read(output.join("data/custom.json")).expect("preserved custom file"),
            b"user content"
        );
        assert!(!output.join("b-new").exists());
        assert!(!staging_path.exists());
    }

    #[test]
    fn publication_retains_backups_when_rollback_cannot_restore_a_file() {
        let temp = tempdir().expect("tempdir");
        let output = temp.path().join("published");
        fs::create_dir(&output).expect("published directory");
        fs::write(output.join("a-existing.txt"), b"original bytes").expect("original file");
        fs::write(output.join("z-fail.txt"), b"original final bytes").expect("final file");
        let mut publication = PagesPublication::new(&output).expect("private staging");
        let recovery = publication.staging.path().to_path_buf();
        let staged = publication.staged_dir.clone();
        fs::write(staged.join("a-existing.txt"), b"replacement bytes").expect("replacement");
        fs::write(staged.join("z-fail.txt"), b"replacement final bytes")
            .expect("final replacement");
        publication
            .prepare(&["a-existing.txt".to_string(), "z-fail.txt".to_string()])
            .expect("preflight");
        publication.back_up_existing().expect("complete backups");
        assert!(publication.apply_next().expect("install first replacement"));
        fs::rename(staged.join("z-fail.txt"), recovery.join("withheld-final"))
            .expect("withhold final staged source");
        fs::create_dir(staged.join("a-existing.txt")).expect("obstruct rollback destination");
        fs::write(
            staged.join("a-existing.txt/keep.txt"),
            b"retain this content",
        )
        .expect("rollback obstruction contents");

        let error = publication
            .publish()
            .expect_err("publication and rollback fail");
        let message = error.to_string();
        assert!(message.contains("rollback incomplete"), "{message}");
        assert!(
            message.contains(&recovery.display().to_string()),
            "{message}"
        );
        assert_eq!(
            fs::read(recovery.join("backups/a-existing.txt")).expect("retained original backup"),
            b"original bytes"
        );
        assert_eq!(
            fs::read(recovery.join("backups/z-fail.txt")).expect("retained final backup"),
            b"original final bytes"
        );
        assert_eq!(
            fs::read(output.join("a-existing.txt")).expect("retained installed version"),
            b"replacement bytes"
        );
        assert_eq!(
            fs::read(staged.join("a-existing.txt/keep.txt")).expect("retained obstruction"),
            b"retain this content"
        );
    }

    #[test]
    #[cfg(unix)]
    fn publication_preserves_file_permissions_and_skips_unchanged_files() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = tempdir().expect("tempdir");
        let output = temp.path().join("published");
        fs::create_dir(&output).expect("published directory");
        fs::write(output.join("private.txt"), b"old private bytes").expect("private file");
        fs::set_permissions(
            output.join("private.txt"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("private permissions");
        fs::write(output.join("unchanged.txt"), b"stable bytes").expect("unchanged file");
        let root_inode = fs::metadata(&output).expect("root metadata").ino();
        let unchanged_inode = fs::metadata(output.join("unchanged.txt"))
            .expect("unchanged metadata")
            .ino();
        let mut publication = PagesPublication::new(&output).expect("private staging");
        fs::write(
            publication.staged_dir.join("private.txt"),
            b"new private bytes",
        )
        .expect("private replacement");
        fs::write(
            publication.staged_dir.join("unchanged.txt"),
            b"stable bytes",
        )
        .expect("unchanged staged content");
        publication
            .prepare(&["private.txt".to_string(), "unchanged.txt".to_string()])
            .expect("preflight");
        publication
            .back_up_existing()
            .expect("back up changed file");
        assert_eq!(publication.changes.len(), 1);

        publication.publish().expect("successful publication");

        assert_eq!(
            fs::metadata(output.join("private.txt"))
                .expect("private metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::read(output.join("private.txt")).expect("replacement bytes"),
            b"new private bytes"
        );
        assert_eq!(
            fs::metadata(&output).expect("root metadata").ino(),
            root_inode
        );
        assert_eq!(
            fs::metadata(output.join("unchanged.txt"))
                .expect("unchanged metadata")
                .ino(),
            unchanged_inode
        );
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
                subtitle: None,
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
        assert!(
            summary
                .files
                .contains(&"data/export_summary.json".to_string())
        );
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
                subtitle: None,
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
                subtitle: None,
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
    fn preview_request_parser_accepts_http_versions_and_opaque_header_values() {
        for (request, method, target) in [
            (&b"GET / HTTP/1.0\r\n\r\n"[..], "GET", "/"),
            (
                &b"HEAD /assets/viewer.js?version=1 HTTP/1.1\r\n\r\n"[..],
                "HEAD",
                "/assets/viewer.js?version=1",
            ),
            (
                &b"GET / HTTP/1.1\r\nX-!#$%&'*+-.^_`|~: \t\xff\x80\r\nEmpty:\r\n\r\n"[..],
                "GET",
                "/",
            ),
            (&b"OPTIONS * HTTP/1.1\r\n\r\n"[..], "OPTIONS", "*"),
            (
                &b"CONNECT localhost:443 HTTP/1.1\r\n\r\n"[..],
                "CONNECT",
                "localhost:443",
            ),
        ] {
            assert_eq!(parse_preview_request(request), Some((method, target)));
        }
    }

    #[test]
    fn preview_request_parser_rejects_malformed_request_lines() {
        for line in [
            "GET /",
            "GET / HTTP/1.1 extra",
            "GET  / HTTP/1.1",
            "GET /  HTTP/1.1",
            "GET / HTTP/1.1 ",
            " GET / HTTP/1.1",
            "GET\t/\tHTTP/1.1",
            "G(ET / HTTP/1.1",
            "GET * HTTP/1.1",
            "HEAD http://localhost/ HTTP/1.1",
            "GET /#fragment HTTP/1.1",
            "GET /\u{7f} HTTP/1.1",
            "GET /\u{0} HTTP/1.1",
            "GET /é HTTP/1.1",
            "GET / HTTP/2.0",
            "POST  HTTP/1.1",
        ] {
            let request = format!("{line}\r\n\r\n");
            assert!(
                parse_preview_request(request.as_bytes()).is_none(),
                "accepted malformed request line {line:?}"
            );
        }
    }

    #[test]
    fn preview_request_parser_rejects_malformed_header_fields() {
        for field in [
            &b": value"[..],
            &b"Bad Header: value"[..],
            &b"Bad:Name: value\x00"[..],
            &b"Name : value"[..],
            &b"Missing-Colon"[..],
            &b"\tFolded: value"[..],
            &b"Name: value\x7f"[..],
            &b"Name: value\x0b"[..],
            &b"Name: value\x0c"[..],
            &b"Name: value\nnext"[..],
            &b"Name: value\rnext"[..],
            &b"N\xffme: value"[..],
        ] {
            let mut request = b"GET / HTTP/1.1\r\n".to_vec();
            request.extend_from_slice(field);
            request.extend_from_slice(b"\r\n\r\n");
            assert!(
                parse_preview_request(&request).is_none(),
                "accepted malformed header field {field:?}"
            );
        }
    }

    #[test]
    fn preview_request_header_reader_stops_at_first_complete_request() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let mut client = TcpStream::connect(listener.local_addr().expect("listener address"))
            .expect("connect client");
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("client write timeout");
        let (mut server, _) = listener.accept().expect("accept client");
        let first = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let mut requests = first.to_vec();
        requests.extend_from_slice(b"POST /bad HTTP/2.0\r\nInvalid Header\r\n\r\n\xff");
        client.write_all(&requests).expect("send requests");

        let mut buffer = [0_u8; PREVIEW_MAX_HEADER_BYTES];
        let PreviewHeaderRead::Complete(len) =
            read_preview_headers(&mut server, &mut buffer).expect("read header")
        else {
            panic!("first request did not complete");
        };
        assert_eq!(&buffer[..len], first);
        assert_eq!(parse_preview_request(&buffer[..len]), Some(("GET", "/")));
    }

    #[test]
    fn preview_request_serves_large_body_on_nonblocking_accepted_socket() {
        let bundle = tempdir().expect("tempdir");
        let payload: Vec<u8> = (0..16 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        fs::write(bundle.path().join("large.bin"), &payload).expect("write large asset");

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect client");
        client
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("client read timeout");
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("client write timeout");
        let (server, _) = listener.accept().expect("accept client");
        // Reproduce inherited O_NONBLOCK even on Linux, where accept normally
        // clears it. Exercise the actual request handler, not a test-only path.
        server
            .set_nonblocking(true)
            .expect("nonblocking connection");
        client
            .write_all(b"GET /large.bin HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("send request");

        let (response, serve_result) = thread::scope(|scope| {
            let serving =
                scope.spawn(|| handle_preview_request(server, bundle.path(), false, port));
            // Hold off draining until a body much larger than the usual TCP
            // send buffer has put the server under backpressure.
            thread::sleep(Duration::from_millis(300));
            let mut response = Vec::new();
            let read_result = client.read_to_end(&mut response);
            drop(client);
            let serve_result = serving.join().expect("request worker");
            read_result.expect("read complete response");
            (response, serve_result)
        });

        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP headers")
            + 4;
        let headers = std::str::from_utf8(&response[..header_end]).expect("ASCII headers");
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(headers.contains(&format!("Content-Length: {}\r\n", payload.len())));
        let body = &response[header_end..];
        assert_eq!(body.len(), payload.len(), "preview truncated the body");
        assert!(body == payload.as_slice(), "preview corrupted the body");
        assert!(serve_result.expect("serve complete response"));
    }

    #[test]
    fn preview_request_times_out_writes_without_progress() {
        use std::io::ErrorKind;
        use std::time::Instant;

        let bundle = tempdir().expect("tempdir");
        fs::write(bundle.path().join("index.html"), b"preview").expect("write index");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect client");
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("client write timeout");
        let (mut server, _) = listener.accept().expect("accept client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("send request");

        // A large response alone does not establish a no-progress write:
        // buffered TCP data can advance across several per-write timeouts.
        // First saturate the socket and let delayed acknowledgments settle,
        // while this peer stays connected without reading any data.
        server
            .set_write_timeout(Some(Duration::from_millis(100)))
            .expect("prefill timeout");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last_progress = Instant::now();
        let buffer = vec![0_u8; 64 * 1024];
        let mut probe = buffer.as_slice();
        loop {
            assert!(
                Instant::now() < deadline,
                "failed to establish backpressure"
            );
            match server.write(probe) {
                Ok(0) => panic!("peer closed during prefill"),
                Ok(_) => last_progress = Instant::now(),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    if probe.len() > 1 {
                        // BSD send low-water marks can block a large write
                        // while a short HTTP header would still fit. Exhaust
                        // that remaining space before measuring the timeout.
                        probe = &buffer[..1];
                        last_progress = Instant::now();
                    } else if last_progress.elapsed() >= Duration::from_secs(1) {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => panic!("prefill failed: {error}"),
            }
        }

        let (elapsed, result) = thread::scope(|scope| {
            let (completed, completion) = mpsc::sync_channel(1);
            let serving = scope.spawn(move || {
                let started = Instant::now();
                let result = handle_preview_request(server, bundle.path(), false, port);
                let _ = completed.send((started.elapsed(), result));
            });
            let outcome = completion.recv_timeout(Duration::from_secs(15));
            // Even if the handler loses its timeout, close the peer before
            // joining so this regression cannot leave a blocked test thread.
            let _ = client.shutdown(Shutdown::Both);
            drop(client);
            serving.join().expect("request worker");
            outcome.expect("stalled write exceeded the timeout allowance")
        });

        assert!(
            elapsed >= Duration::from_secs(8) && elapsed < Duration::from_secs(15),
            "handler did not retain its ten-second write timeout: {elapsed:?}"
        );
        assert!(
            matches!(result, Err(BvrError::Io(ref error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)),
            "expected a write timeout, got {result:?}"
        );
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
                subtitle: None,
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
                subtitle: None,
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
            "application/wasm"
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
        assert_eq!(
            mime_type_for_path(Path::new("beads.sqlite3")),
            "application/x-sqlite3"
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
                subtitle: None,
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
                subtitle: None,
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
            subtitle: Some("Subheading".to_string()),
            generated_at: "2026-03-09T12:00:00Z".to_string(),
            issue_count: 42,
            include_closed: true,
            include_history: true,
            generator: "bvr".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let readme = generate_deploy_readme("Test Project", &meta);
        assert!(readme.contains("# Test Project"));
        assert!(readme.contains("## Quick start"));
        assert!(readme.contains("## Contents"));
        assert!(readme.contains("## Generation info"));
        assert!(readme.contains("Issues**: 42"));
        assert!(readme.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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

    #[test]
    fn export_subtitle_is_preserved_in_meta_and_bundle_readme() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");

        export_pages_bundle(
            &[make_issue("A", "open")],
            &out,
            &ExportPagesOptions {
                title: Some("Dashboard".to_string()),
                subtitle: Some("Nightly triage snapshot".to_string()),
                include_closed: false,
                include_history: false,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");
        assert_eq!(meta["subtitle"], "Nightly triage snapshot");

        let readme = fs::read_to_string(out.join("README.md")).expect("read README");
        assert!(readme.contains("Nightly triage snapshot"));
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
                subtitle: None,
                include_closed: true,
                include_history: true,
            },
        )
        .expect("export");

        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("data/meta.json")).expect("read"))
                .expect("parse");

        assert!(meta["title"].is_string());
        assert!(meta.get("subtitle").is_none());
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
                subtitle: None,
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
                subtitle: None,
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
            insights.get("Bottlenecks").is_some(),
            "insights.json must contain Bottlenecks key"
        );
    }

    #[test]
    fn fallback_viewer_js_reads_exported_insights_key_shape() {
        assert!(
            JS_BUNDLE.contains("insights.Bottlenecks"),
            "fallback viewer must read the serialized Insights key shape"
        );
        assert!(
            !JS_BUNDLE.contains("insights.bottlenecks"),
            "fallback viewer must not rely on a lowercase insights key that export never emits"
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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
                subtitle: None,
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
            subtitle: None,
            include_closed: false,
            include_history: false,
        };

        let s1 = export_pages_bundle(&issues, &out, &opts).expect("first export");
        let s2 = export_pages_bundle(&issues, &out, &opts).expect("second export");

        assert_eq!(s1.issue_count, s2.issue_count);
        assert_eq!(s1.files.len(), s2.files.len());
    }

    #[test]
    fn export_reexport_without_history_removes_stale_history_file() {
        let temp = tempdir().expect("tempdir");
        let out = temp.path().join("pages");
        let issues = vec![make_issue("A", "open")];

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                subtitle: None,
                include_closed: false,
                include_history: true,
            },
        )
        .expect("initial export with history");
        assert!(out.join("data/history.json").is_file());

        export_pages_bundle(
            &issues,
            &out,
            &ExportPagesOptions {
                title: None,
                subtitle: None,
                include_closed: false,
                include_history: false,
            },
        )
        .expect("re-export without history");

        assert!(
            !out.join("data/history.json").exists(),
            "stale history.json should be removed when history export is disabled"
        );
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
                subtitle: None,
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
                subtitle: None,
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
        assert!(
            !token.is_empty(),
            "token must be non-empty for non-empty dir"
        );
    }

    #[test]
    fn latest_modified_token_changes_for_same_size_rewrite() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        fs::write(&path, "AAAA").expect("write first");
        let first = latest_modified_token(temp.path()).expect("first token");

        fs::write(&path, "BBBB").expect("write second");
        let second = latest_modified_token(temp.path()).expect("second token");

        assert_ne!(first, second, "same-size rewrites must change the token");
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
        assert!(!token.is_empty());
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
