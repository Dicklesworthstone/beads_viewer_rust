//! Pages wizard state model, config persistence, and validation.
//!
//! This module defines the state machine for the interactive `--pages` wizard,
//! including deploy target configuration, validation, and saved config support.
//! The actual interactive prompts are wired up in `main.rs`; this module is the
//! testable core that the interactive layer drives.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{BvrError, Result};

// ── Deploy target ──────────────────────────────────────────────────

/// Supported static-hosting deployment targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployTarget {
    Github,
    Cloudflare,
    Local,
}

impl DeployTarget {
    pub const ALL: [Self; 3] = [Self::Github, Self::Cloudflare, Self::Local];

    /// Human label for display in prompts.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub Pages",
            Self::Cloudflare => "Cloudflare Pages",
            Self::Local => "Local / custom static host",
        }
    }

    /// CLI tools required before deployment (empty for local).
    pub const fn required_tools(self) -> &'static [&'static str] {
        match self {
            Self::Github => &["gh"],
            Self::Cloudflare => &["wrangler"],
            Self::Local => &[],
        }
    }
}

impl fmt::Display for DeployTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ── Wizard step ────────────────────────────────────────────────────

/// Steps in the wizard state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WizardStep {
    /// Offer to load saved config from disk.
    LoadSaved = 0,
    /// Collect export options (closed, history, title).
    ExportOptions = 1,
    /// Choose deployment target.
    DeployTarget = 2,
    /// Collect target-specific settings.
    TargetConfig = 3,
    /// Verify prerequisites (CLI tools, auth).
    Prerequisites = 4,
    /// Perform the export.
    Export = 5,
    /// Offer local preview before deploy.
    Preview = 6,
    /// Deploy to target.
    Deploy = 7,
    /// Show success summary.
    Done = 8,
}

impl WizardStep {
    /// All steps in order.
    pub const ALL: [Self; 9] = [
        Self::LoadSaved,
        Self::ExportOptions,
        Self::DeployTarget,
        Self::TargetConfig,
        Self::Prerequisites,
        Self::Export,
        Self::Preview,
        Self::Deploy,
        Self::Done,
    ];

    /// Advance to the next step.
    pub fn next(self) -> Option<Self> {
        let idx = self as usize;
        Self::ALL.get(idx + 1).copied()
    }

    /// Go back to the previous user-configurable step.
    /// Export/Preview/Deploy/Done cannot be backed out of.
    pub fn back(self) -> Option<Self> {
        match self {
            Self::LoadSaved => None,
            Self::ExportOptions => Some(Self::LoadSaved),
            Self::DeployTarget => Some(Self::ExportOptions),
            Self::TargetConfig => Some(Self::DeployTarget),
            Self::Prerequisites => Some(Self::TargetConfig),
            // Cannot back out of execution steps
            Self::Export | Self::Preview | Self::Deploy | Self::Done => None,
        }
    }

    /// Whether this step can be cancelled (returns to caller).
    pub fn is_cancellable(self) -> bool {
        matches!(
            self,
            Self::LoadSaved
                | Self::ExportOptions
                | Self::DeployTarget
                | Self::TargetConfig
                | Self::Prerequisites
        )
    }

    /// Human label for progress display.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LoadSaved => "Load saved config",
            Self::ExportOptions => "Export options",
            Self::DeployTarget => "Deploy target",
            Self::TargetConfig => "Target settings",
            Self::Prerequisites => "Prerequisites",
            Self::Export => "Export",
            Self::Preview => "Preview",
            Self::Deploy => "Deploy",
            Self::Done => "Done",
        }
    }

    /// Step number for display (1-indexed).
    pub const fn display_number(self) -> usize {
        (self as usize) + 1
    }

    /// Total number of steps.
    pub const fn total() -> usize {
        Self::ALL.len()
    }
}

// ── Wizard config ──────────────────────────────────────────────────

/// Persistent wizard configuration, saved between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardConfig {
    /// Include closed issues in export.
    #[serde(default = "default_true")]
    pub include_closed: bool,
    /// Include git history for time-travel views.
    #[serde(default = "default_true")]
    pub include_history: bool,
    /// Custom title for the exported site.
    #[serde(default)]
    pub title: Option<String>,
    /// Subtitle for the exported site.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Deployment target.
    #[serde(default)]
    pub deploy_target: Option<DeployTarget>,
    /// Output directory for the export bundle.
    #[serde(default)]
    pub output_path: Option<PathBuf>,

    // GitHub-specific
    /// GitHub repo name (owner/repo format).
    #[serde(default)]
    pub github_repo: Option<String>,
    /// Whether to create a private GitHub repo.
    #[serde(default)]
    pub github_private: bool,
    /// GitHub repo description.
    #[serde(default)]
    pub github_description: Option<String>,

    // Cloudflare-specific
    /// Cloudflare Pages project name.
    #[serde(default)]
    pub cloudflare_project: Option<String>,
    /// Cloudflare Pages branch.
    #[serde(default)]
    pub cloudflare_branch: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for WizardConfig {
    fn default() -> Self {
        Self {
            include_closed: true,
            include_history: true,
            title: None,
            subtitle: None,
            deploy_target: None,
            output_path: None,
            github_repo: None,
            github_private: false,
            github_description: None,
            cloudflare_project: None,
            cloudflare_branch: None,
        }
    }
}

impl WizardConfig {
    /// Validate the config for completeness before export.
    pub fn validate_for_export(&self) -> Result<()> {
        if self.output_path.is_none() {
            return Err(BvrError::InvalidArgument(
                "output path is required for export".into(),
            ));
        }
        Ok(())
    }

    /// Validate the config for completeness before deployment.
    pub fn validate_for_deploy(&self) -> Result<()> {
        self.validate_for_export()?;
        let target = self.deploy_target.ok_or_else(|| {
            BvrError::InvalidArgument("deploy target is required".into())
        })?;
        match target {
            DeployTarget::Github => {
                if self.github_repo.as_ref().map_or(true, |r| r.is_empty()) {
                    return Err(BvrError::InvalidArgument(
                        "GitHub repo name is required (owner/repo format)".into(),
                    ));
                }
            }
            DeployTarget::Cloudflare => {
                if self
                    .cloudflare_project
                    .as_ref()
                    .map_or(true, |p| p.is_empty())
                {
                    return Err(BvrError::InvalidArgument(
                        "Cloudflare project name is required".into(),
                    ));
                }
            }
            DeployTarget::Local => {}
        }
        Ok(())
    }

    /// Clear target-specific fields when switching deploy target.
    pub fn clear_target_config(&mut self) {
        self.github_repo = None;
        self.github_private = false;
        self.github_description = None;
        self.cloudflare_project = None;
        self.cloudflare_branch = None;
    }
}

// ── Config persistence ─────────────────────────────────────────────

/// Default config directory: `~/.config/bvr/`.
fn config_dir() -> Option<PathBuf> {
    dirs_path().map(|d| d.join("bvr"))
}

/// Cross-platform config base path.
fn dirs_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
}

const WIZARD_CONFIG_FILENAME: &str = "pages-wizard.json";

/// Path to the saved wizard config file.
pub fn wizard_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(WIZARD_CONFIG_FILENAME))
}

/// Load a previously saved wizard config from disk.
pub fn load_wizard_config() -> Result<Option<WizardConfig>> {
    let path = match wizard_config_path() {
        Some(p) => p,
        None => return Ok(None),
    };
    if !path.is_file() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path).map_err(|e| {
        BvrError::InvalidArgument(format!("failed to read wizard config: {e}"))
    })?;
    let config: WizardConfig = serde_json::from_str(&contents).map_err(|e| {
        BvrError::InvalidArgument(format!("failed to parse wizard config: {e}"))
    })?;
    Ok(Some(config))
}

/// Save wizard config to disk for future reuse.
pub fn save_wizard_config(config: &WizardConfig) -> Result<()> {
    let path = match wizard_config_path() {
        Some(p) => p,
        None => {
            return Err(BvrError::InvalidArgument(
                "cannot determine config directory".into(),
            ))
        }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            BvrError::InvalidArgument(format!(
                "failed to create config directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| {
        BvrError::InvalidArgument(format!("failed to serialize wizard config: {e}"))
    })?;
    fs::write(&path, json).map_err(|e| {
        BvrError::InvalidArgument(format!("failed to write wizard config: {e}"))
    })?;
    Ok(())
}

/// Save wizard config to a specific path (for testing).
pub fn save_wizard_config_to(config: &WizardConfig, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            BvrError::InvalidArgument(format!("mkdir {}: {e}", parent.display()))
        })?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| {
        BvrError::InvalidArgument(format!("serialize: {e}"))
    })?;
    fs::write(path, json).map_err(|e| {
        BvrError::InvalidArgument(format!("write {}: {e}", path.display()))
    })?;
    Ok(())
}

/// Load wizard config from a specific path (for testing).
pub fn load_wizard_config_from(path: &Path) -> Result<Option<WizardConfig>> {
    if !path.is_file() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path).map_err(|e| {
        BvrError::InvalidArgument(format!("read {}: {e}", path.display()))
    })?;
    let config: WizardConfig = serde_json::from_str(&contents).map_err(|e| {
        BvrError::InvalidArgument(format!("parse {}: {e}", path.display()))
    })?;
    Ok(Some(config))
}

// ── Prerequisite checking ──────────────────────────────────────────

/// Result of checking prerequisites for a deploy target.
#[derive(Debug, Clone)]
pub struct PrereqResult {
    pub target: DeployTarget,
    pub missing_tools: Vec<String>,
    pub passed: bool,
}

/// Check whether required CLI tools are available on PATH.
pub fn check_prerequisites(target: DeployTarget) -> PrereqResult {
    let mut missing = Vec::new();
    for tool in target.required_tools() {
        if !is_tool_available(tool) {
            missing.push((*tool).to_string());
        }
    }
    PrereqResult {
        target,
        passed: missing.is_empty(),
        missing_tools: missing,
    }
}

fn is_tool_available(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Wizard state machine ───────────────────────────────────────────

/// Result of a wizard step interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    /// Advance to the next step.
    Next,
    /// Go back to the previous step.
    Back,
    /// User cancelled the wizard.
    Cancel,
}

/// The wizard state machine.
pub struct Wizard {
    pub config: WizardConfig,
    pub step: WizardStep,
    pub is_update: bool,
    beads_path: Option<PathBuf>,
}

impl Wizard {
    /// Create a new wizard with default config.
    pub fn new(beads_path: Option<PathBuf>) -> Self {
        Self {
            config: WizardConfig::default(),
            step: WizardStep::LoadSaved,
            is_update: false,
            beads_path,
        }
    }

    /// Create a wizard with a pre-loaded saved config.
    pub fn with_saved_config(config: WizardConfig, beads_path: Option<PathBuf>) -> Self {
        Self {
            config,
            step: WizardStep::Prerequisites,
            is_update: true,
            beads_path,
        }
    }

    /// Beads path, if provided.
    pub fn beads_path(&self) -> Option<&Path> {
        self.beads_path.as_deref()
    }

    /// Advance to the next step, returning None when done.
    pub fn advance(&mut self) -> Option<WizardStep> {
        if let Some(next) = self.step.next() {
            self.step = next;
            Some(next)
        } else {
            None
        }
    }

    /// Go back one step.
    pub fn go_back(&mut self) -> Option<WizardStep> {
        if let Some(prev) = self.step.back() {
            self.step = prev;
            Some(prev)
        } else {
            None
        }
    }

    /// Whether the current step can be cancelled.
    pub fn can_cancel(&self) -> bool {
        self.step.is_cancellable()
    }

    /// Whether the wizard has reached the done state.
    pub fn is_done(&self) -> bool {
        self.step == WizardStep::Done
    }

    /// Apply a step result to the wizard state.
    pub fn apply_result(&mut self, result: StepResult) -> WizardTransition {
        match result {
            StepResult::Next => {
                if let Some(next) = self.advance() {
                    WizardTransition::GoTo(next)
                } else {
                    WizardTransition::Finished
                }
            }
            StepResult::Back => {
                if let Some(prev) = self.go_back() {
                    WizardTransition::GoTo(prev)
                } else {
                    WizardTransition::StayOnCurrent
                }
            }
            StepResult::Cancel => WizardTransition::Cancelled,
        }
    }
}

/// Result of applying a step result to the wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardTransition {
    /// Move to a specific step.
    GoTo(WizardStep),
    /// Stay on the current step (back from first step).
    StayOnCurrent,
    /// Wizard completed successfully.
    Finished,
    /// User cancelled.
    Cancelled,
}

// ── Interactive wizard runner ───────────────────────────────────────

use std::io::{BufRead, Write as IoWrite};

/// Run the interactive pages wizard with the given I/O streams.
///
/// Uses `reader` for user input and `writer` for prompts/output.
/// The `beads_path` is the path to the beads file to export from.
/// The `export_fn` callback performs the actual export given the config.
/// The `preview_fn` callback starts a preview server for the given path.
///
/// Returns `Ok(Some(config))` on success, `Ok(None)` on cancel,
/// or `Err` on I/O or validation failure.
pub fn run_wizard_interactive<R, W, E, P>(
    reader: &mut R,
    writer: &mut W,
    beads_path: Option<PathBuf>,
    export_fn: E,
    preview_fn: P,
) -> Result<Option<WizardConfig>>
where
    R: BufRead,
    W: IoWrite,
    E: FnOnce(&WizardConfig) -> Result<()>,
    P: Fn(&Path) -> Result<()>,
{
    let mut export_fn = Some(export_fn);

    writeln!(writer, "╭──────────────────────────────────────╮").ok();
    writeln!(writer, "│  bvr pages wizard                    │").ok();
    writeln!(writer, "╰──────────────────────────────────────╯").ok();
    writeln!(writer).ok();

    // Step 1: Check for saved config
    let mut wizard = match load_wizard_config() {
        Ok(Some(saved)) => {
            writeln!(writer, "Found saved configuration.").ok();
            write!(writer, "Use saved config? [Y/n] ").ok();
            writer.flush().ok();
            let answer = read_line_trimmed(reader);
            if answer.is_empty() || answer.starts_with('y') || answer.starts_with('Y') {
                writeln!(writer, "  → Using saved config").ok();
                Wizard::with_saved_config(saved, beads_path)
            } else {
                Wizard::new(beads_path)
            }
        }
        _ => Wizard::new(beads_path),
    };

    // If starting fresh, advance past LoadSaved
    if wizard.step == WizardStep::LoadSaved {
        wizard.advance();
    }

    loop {
        match wizard.step {
            WizardStep::LoadSaved => {
                // Already handled above
                wizard.advance();
            }
            WizardStep::ExportOptions => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: {}",
                    wizard.step.display_number(),
                    WizardStep::total(),
                    wizard.step.label()
                )
                .ok();

                wizard.config.include_closed =
                    prompt_yes_no(reader, writer, "Include closed issues?", true);
                wizard.config.include_history =
                    prompt_yes_no(reader, writer, "Include git history?", true);
                wizard.config.title =
                    prompt_optional(reader, writer, "Custom title (empty = default)");
                wizard.config.subtitle =
                    prompt_optional(reader, writer, "Custom subtitle (empty = none)");

                wizard.advance();
            }
            WizardStep::DeployTarget => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: {}",
                    wizard.step.display_number(),
                    WizardStep::total(),
                    wizard.step.label()
                )
                .ok();

                writeln!(writer, "Where will you deploy?").ok();
                for (i, target) in DeployTarget::ALL.iter().enumerate() {
                    writeln!(writer, "  {}) {}", i + 1, target.label()).ok();
                }
                write!(writer, "Choice [1-3, or 'b' to go back]: ").ok();
                writer.flush().ok();
                let answer = read_line_trimmed(reader);

                if answer == "b" || answer == "B" {
                    wizard.go_back();
                    continue;
                }

                let choice = answer.parse::<usize>().unwrap_or(0);
                if choice >= 1 && choice <= 3 {
                    let target = DeployTarget::ALL[choice - 1];
                    if wizard.config.deploy_target != Some(target) {
                        wizard.config.clear_target_config();
                    }
                    wizard.config.deploy_target = Some(target);
                    wizard.advance();
                } else {
                    writeln!(writer, "Invalid choice, please enter 1, 2, or 3.").ok();
                    // Stay on current step
                }
            }
            WizardStep::TargetConfig => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: {}",
                    wizard.step.display_number(),
                    WizardStep::total(),
                    wizard.step.label()
                )
                .ok();

                match wizard.config.deploy_target {
                    Some(DeployTarget::Github) => {
                        wizard.config.github_repo =
                            prompt_required(reader, writer, "GitHub repo (owner/repo)");
                        if wizard.config.github_repo.is_none() {
                            wizard.go_back();
                            continue;
                        }
                        wizard.config.github_private =
                            prompt_yes_no(reader, writer, "Private repo?", false);
                        wizard.config.github_description =
                            prompt_optional(reader, writer, "Repo description (optional)");
                    }
                    Some(DeployTarget::Cloudflare) => {
                        wizard.config.cloudflare_project =
                            prompt_required(reader, writer, "Cloudflare project name");
                        if wizard.config.cloudflare_project.is_none() {
                            wizard.go_back();
                            continue;
                        }
                        wizard.config.cloudflare_branch = prompt_optional(
                            reader,
                            writer,
                            "Branch name (default: production)",
                        );
                    }
                    Some(DeployTarget::Local) | None => {
                        // Local needs output path
                    }
                }

                // Output path (all targets)
                write!(writer, "Output directory [./bv-pages]: ").ok();
                writer.flush().ok();
                let path = read_line_trimmed(reader);
                wizard.config.output_path = Some(PathBuf::from(if path.is_empty() {
                    "./bv-pages".to_string()
                } else {
                    path
                }));

                wizard.advance();
            }
            WizardStep::Prerequisites => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: {}",
                    wizard.step.display_number(),
                    WizardStep::total(),
                    wizard.step.label()
                )
                .ok();

                if let Some(target) = wizard.config.deploy_target {
                    let result = check_prerequisites(target);
                    if result.passed {
                        writeln!(writer, "  ✓ All prerequisites met for {target}").ok();
                    } else {
                        writeln!(writer, "  ✗ Missing tools: {}", result.missing_tools.join(", "))
                            .ok();
                        writeln!(writer, "  Install the missing tools and retry, or go back to choose a different target.").ok();
                        write!(writer, "  [r]etry / [b]ack / [c]ancel: ").ok();
                        writer.flush().ok();
                        let answer = read_line_trimmed(reader);
                        match answer.as_str() {
                            "b" | "B" => {
                                wizard.go_back();
                                continue;
                            }
                            "c" | "C" => return Ok(None),
                            _ => continue, // retry
                        }
                    }
                }

                // Validate config before proceeding
                match wizard.config.validate_for_export() {
                    Ok(()) => {
                        wizard.advance();
                    }
                    Err(e) => {
                        writeln!(writer, "  Config validation failed: {e}").ok();
                        wizard.go_back();
                        continue;
                    }
                }
            }
            WizardStep::Export => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: Exporting bundle...",
                    wizard.step.display_number(),
                    WizardStep::total(),
                )
                .ok();

                let do_export = export_fn.take().expect("export called only once");
                match do_export(&wizard.config) {
                    Ok(()) => {
                        writeln!(
                            writer,
                            "  ✓ Export complete: {}",
                            wizard
                                .config
                                .output_path
                                .as_deref()
                                .unwrap_or(Path::new("?"))
                                .display()
                        )
                        .ok();
                        wizard.advance();
                    }
                    Err(e) => {
                        writeln!(writer, "  ✗ Export failed: {e}").ok();
                        return Err(e);
                    }
                }
            }
            WizardStep::Preview => {
                writeln!(writer).ok();
                write!(writer, "Preview the export locally? [Y/n] ").ok();
                writer.flush().ok();
                let answer = read_line_trimmed(reader);
                if answer.is_empty() || answer.starts_with('y') || answer.starts_with('Y') {
                    if let Some(path) = wizard.config.output_path.as_deref() {
                        if let Err(e) = preview_fn(path) {
                            writeln!(writer, "  Preview error: {e}").ok();
                        }
                    }
                } else {
                    writeln!(writer, "  Skipping preview.").ok();
                }
                wizard.advance();
            }
            WizardStep::Deploy => {
                writeln!(writer).ok();
                writeln!(
                    writer,
                    "Step {}/{}: Deploy",
                    wizard.step.display_number(),
                    WizardStep::total(),
                )
                .ok();

                let target = wizard
                    .config
                    .deploy_target
                    .map_or("local".to_string(), |t| t.label().to_string());
                let output = wizard
                    .config
                    .output_path
                    .as_deref()
                    .unwrap_or(Path::new("./bv-pages"));

                writeln!(writer, "  Target: {target}").ok();
                writeln!(writer, "  Bundle: {}", output.display()).ok();

                match wizard.config.deploy_target {
                    Some(DeployTarget::Local) | None => {
                        writeln!(
                            writer,
                            "  Your bundle is ready at: {}",
                            output.display()
                        )
                        .ok();
                        writeln!(
                            writer,
                            "  Deploy it to any static host (Netlify, Vercel, S3, etc.)"
                        )
                        .ok();
                    }
                    Some(DeployTarget::Github) => {
                        let repo = wizard.config.github_repo.as_deref().unwrap_or("?");
                        writeln!(writer, "  Deploy to GitHub Pages: {repo}").ok();
                        writeln!(
                            writer,
                            "  Run: gh repo create {repo} --public --source={}",
                            output.display()
                        )
                        .ok();
                    }
                    Some(DeployTarget::Cloudflare) => {
                        let project = wizard
                            .config
                            .cloudflare_project
                            .as_deref()
                            .unwrap_or("?");
                        writeln!(writer, "  Deploy to Cloudflare Pages: {project}").ok();
                        writeln!(
                            writer,
                            "  Run: wrangler pages deploy {} --project-name={project}",
                            output.display()
                        )
                        .ok();
                    }
                }
                wizard.advance();
            }
            WizardStep::Done => {
                writeln!(writer).ok();
                writeln!(writer, "✓ Pages wizard complete!").ok();

                // Save config for reuse
                if let Err(e) = save_wizard_config(&wizard.config) {
                    writeln!(writer, "  (could not save config for reuse: {e})").ok();
                } else {
                    writeln!(writer, "  Config saved for next run.").ok();
                }

                return Ok(Some(wizard.config));
            }
        }
    }
}

fn read_line_trimmed<R: BufRead>(reader: &mut R) -> String {
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    line.trim().to_string()
}

fn prompt_yes_no<R: BufRead, W: IoWrite>(
    reader: &mut R,
    writer: &mut W,
    prompt: &str,
    default: bool,
) -> bool {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    write!(writer, "  {prompt} {hint} ").ok();
    writer.flush().ok();
    let answer = read_line_trimmed(reader);
    if answer.is_empty() {
        default
    } else {
        answer.starts_with('y') || answer.starts_with('Y')
    }
}

fn prompt_optional<R: BufRead, W: IoWrite>(
    reader: &mut R,
    writer: &mut W,
    prompt: &str,
) -> Option<String> {
    write!(writer, "  {prompt}: ").ok();
    writer.flush().ok();
    let answer = read_line_trimmed(reader);
    if answer.is_empty() { None } else { Some(answer) }
}

fn prompt_required<R: BufRead, W: IoWrite>(
    reader: &mut R,
    writer: &mut W,
    prompt: &str,
) -> Option<String> {
    write!(writer, "  {prompt}: ").ok();
    writer.flush().ok();
    let answer = read_line_trimmed(reader);
    if answer.is_empty() {
        writeln!(writer, "  (required, going back)").ok();
        None
    } else {
        Some(answer)
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── DeployTarget ───────────────────────────────────────────────

    #[test]
    fn deploy_target_all_has_three_variants() {
        assert_eq!(DeployTarget::ALL.len(), 3);
    }

    #[test]
    fn deploy_target_labels_are_non_empty() {
        for target in DeployTarget::ALL {
            assert!(!target.label().is_empty());
        }
    }

    #[test]
    fn deploy_target_github_requires_gh_tool() {
        assert_eq!(DeployTarget::Github.required_tools(), &["gh"]);
    }

    #[test]
    fn deploy_target_cloudflare_requires_wrangler() {
        assert_eq!(DeployTarget::Cloudflare.required_tools(), &["wrangler"]);
    }

    #[test]
    fn deploy_target_local_requires_no_tools() {
        assert!(DeployTarget::Local.required_tools().is_empty());
    }

    #[test]
    fn deploy_target_display_matches_label() {
        for target in DeployTarget::ALL {
            assert_eq!(format!("{target}"), target.label());
        }
    }

    #[test]
    fn deploy_target_serde_roundtrip() {
        for target in DeployTarget::ALL {
            let json = serde_json::to_string(&target).unwrap();
            let back: DeployTarget = serde_json::from_str(&json).unwrap();
            assert_eq!(target, back);
        }
    }

    // ── WizardStep ─────────────────────────────────────────────────

    #[test]
    fn wizard_step_ordering_is_sequential() {
        for (i, step) in WizardStep::ALL.iter().enumerate() {
            assert_eq!(*step as usize, i);
        }
    }

    #[test]
    fn wizard_step_next_advances_through_all() {
        let mut step = WizardStep::LoadSaved;
        let mut count = 1;
        while let Some(next) = step.next() {
            step = next;
            count += 1;
        }
        assert_eq!(count, WizardStep::total());
        assert_eq!(step, WizardStep::Done);
    }

    #[test]
    fn wizard_step_done_has_no_next() {
        assert_eq!(WizardStep::Done.next(), None);
    }

    #[test]
    fn wizard_step_back_from_first_is_none() {
        assert_eq!(WizardStep::LoadSaved.back(), None);
    }

    #[test]
    fn wizard_step_back_from_export_options_goes_to_load_saved() {
        assert_eq!(
            WizardStep::ExportOptions.back(),
            Some(WizardStep::LoadSaved)
        );
    }

    #[test]
    fn wizard_step_back_from_prerequisites_goes_to_target_config() {
        assert_eq!(
            WizardStep::Prerequisites.back(),
            Some(WizardStep::TargetConfig)
        );
    }

    #[test]
    fn wizard_step_execution_steps_cannot_go_back() {
        assert_eq!(WizardStep::Export.back(), None);
        assert_eq!(WizardStep::Preview.back(), None);
        assert_eq!(WizardStep::Deploy.back(), None);
        assert_eq!(WizardStep::Done.back(), None);
    }

    #[test]
    fn wizard_step_config_steps_are_cancellable() {
        assert!(WizardStep::LoadSaved.is_cancellable());
        assert!(WizardStep::ExportOptions.is_cancellable());
        assert!(WizardStep::DeployTarget.is_cancellable());
        assert!(WizardStep::TargetConfig.is_cancellable());
        assert!(WizardStep::Prerequisites.is_cancellable());
    }

    #[test]
    fn wizard_step_execution_steps_not_cancellable() {
        assert!(!WizardStep::Export.is_cancellable());
        assert!(!WizardStep::Preview.is_cancellable());
        assert!(!WizardStep::Deploy.is_cancellable());
        assert!(!WizardStep::Done.is_cancellable());
    }

    #[test]
    fn wizard_step_labels_are_non_empty() {
        for step in WizardStep::ALL {
            assert!(!step.label().is_empty(), "step {:?} has empty label", step);
        }
    }

    #[test]
    fn wizard_step_display_numbers_are_1_indexed() {
        for (i, step) in WizardStep::ALL.iter().enumerate() {
            assert_eq!(step.display_number(), i + 1);
        }
    }

    // ── WizardConfig defaults ──────────────────────────────────────

    #[test]
    fn wizard_config_default_includes_closed_and_history() {
        let config = WizardConfig::default();
        assert!(config.include_closed);
        assert!(config.include_history);
    }

    #[test]
    fn wizard_config_default_has_no_title() {
        let config = WizardConfig::default();
        assert!(config.title.is_none());
    }

    #[test]
    fn wizard_config_default_has_no_deploy_target() {
        let config = WizardConfig::default();
        assert!(config.deploy_target.is_none());
    }

    // ── WizardConfig validation ────────────────────────────────────

    #[test]
    fn validate_for_export_requires_output_path() {
        let config = WizardConfig::default();
        assert!(config.validate_for_export().is_err());
    }

    #[test]
    fn validate_for_export_passes_with_output_path() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        assert!(config.validate_for_export().is_ok());
    }

    #[test]
    fn validate_for_deploy_requires_deploy_target() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        assert!(config.validate_for_deploy().is_err());
    }

    #[test]
    fn validate_for_deploy_local_needs_only_output_path() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        config.deploy_target = Some(DeployTarget::Local);
        assert!(config.validate_for_deploy().is_ok());
    }

    #[test]
    fn validate_for_deploy_github_requires_repo_name() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        config.deploy_target = Some(DeployTarget::Github);
        assert!(config.validate_for_deploy().is_err());

        config.github_repo = Some("owner/repo".into());
        assert!(config.validate_for_deploy().is_ok());
    }

    #[test]
    fn validate_for_deploy_github_rejects_empty_repo() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        config.deploy_target = Some(DeployTarget::Github);
        config.github_repo = Some(String::new());
        assert!(config.validate_for_deploy().is_err());
    }

    #[test]
    fn validate_for_deploy_cloudflare_requires_project_name() {
        let mut config = WizardConfig::default();
        config.output_path = Some(PathBuf::from("./pages"));
        config.deploy_target = Some(DeployTarget::Cloudflare);
        assert!(config.validate_for_deploy().is_err());

        config.cloudflare_project = Some("my-project".into());
        assert!(config.validate_for_deploy().is_ok());
    }

    #[test]
    fn clear_target_config_resets_all_target_fields() {
        let mut config = WizardConfig::default();
        config.github_repo = Some("owner/repo".into());
        config.github_private = true;
        config.github_description = Some("desc".into());
        config.cloudflare_project = Some("proj".into());
        config.cloudflare_branch = Some("main".into());

        config.clear_target_config();
        assert!(config.github_repo.is_none());
        assert!(!config.github_private);
        assert!(config.github_description.is_none());
        assert!(config.cloudflare_project.is_none());
        assert!(config.cloudflare_branch.is_none());
    }

    // ── Config persistence ─────────────────────────────────────────

    #[test]
    fn config_serde_roundtrip() {
        let mut config = WizardConfig::default();
        config.title = Some("My Dashboard".into());
        config.deploy_target = Some(DeployTarget::Github);
        config.github_repo = Some("user/pages".into());
        config.output_path = Some(PathBuf::from("./out"));

        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: WizardConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.title.as_deref(), Some("My Dashboard"));
        assert_eq!(back.deploy_target, Some(DeployTarget::Github));
        assert_eq!(back.github_repo.as_deref(), Some("user/pages"));
    }

    #[test]
    fn config_deserialize_with_missing_fields_uses_defaults() {
        let json = r#"{"title": "Minimal"}"#;
        let config: WizardConfig = serde_json::from_str(json).unwrap();
        assert!(config.include_closed);
        assert!(config.include_history);
        assert!(config.deploy_target.is_none());
    }

    #[test]
    fn save_and_load_config_file_roundtrip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bvr/pages-wizard.json");

        let mut config = WizardConfig::default();
        config.title = Some("Test".into());
        config.deploy_target = Some(DeployTarget::Local);
        config.output_path = Some(PathBuf::from("./pages"));

        save_wizard_config_to(&config, &path).unwrap();
        let loaded = load_wizard_config_from(&path).unwrap().unwrap();
        assert_eq!(loaded.title.as_deref(), Some("Test"));
        assert_eq!(loaded.deploy_target, Some(DeployTarget::Local));
    }

    #[test]
    fn load_config_from_nonexistent_returns_none() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("does_not_exist.json");
        let result = load_wizard_config_from(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_config_from_invalid_json_returns_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        fs::write(&path, "not json").unwrap();
        assert!(load_wizard_config_from(&path).is_err());
    }

    // ── Wizard state machine ───────────────────────────────────────

    #[test]
    fn wizard_starts_at_load_saved() {
        let w = Wizard::new(None);
        assert_eq!(w.step, WizardStep::LoadSaved);
        assert!(!w.is_update);
    }

    #[test]
    fn wizard_with_saved_config_starts_at_prerequisites() {
        let w = Wizard::with_saved_config(WizardConfig::default(), None);
        assert_eq!(w.step, WizardStep::Prerequisites);
        assert!(w.is_update);
    }

    #[test]
    fn wizard_advance_walks_through_all_steps() {
        let mut w = Wizard::new(None);
        let mut visited = vec![w.step];
        while let Some(next) = w.advance() {
            visited.push(next);
        }
        assert_eq!(visited.len(), WizardStep::total());
        assert!(w.is_done());
    }

    #[test]
    fn wizard_go_back_from_deploy_target_to_export_options() {
        let mut w = Wizard::new(None);
        w.step = WizardStep::DeployTarget;
        let prev = w.go_back();
        assert_eq!(prev, Some(WizardStep::ExportOptions));
        assert_eq!(w.step, WizardStep::ExportOptions);
    }

    #[test]
    fn wizard_go_back_from_first_step_stays() {
        let mut w = Wizard::new(None);
        let prev = w.go_back();
        assert_eq!(prev, None);
        assert_eq!(w.step, WizardStep::LoadSaved);
    }

    #[test]
    fn wizard_cancel_from_config_step_returns_cancelled() {
        let mut w = Wizard::new(None);
        w.step = WizardStep::ExportOptions;
        assert!(w.can_cancel());
        let transition = w.apply_result(StepResult::Cancel);
        assert_eq!(transition, WizardTransition::Cancelled);
    }

    #[test]
    fn wizard_next_from_done_returns_finished() {
        let mut w = Wizard::new(None);
        w.step = WizardStep::Done;
        let transition = w.apply_result(StepResult::Next);
        assert_eq!(transition, WizardTransition::Finished);
    }

    #[test]
    fn wizard_back_from_first_stays_on_current() {
        let mut w = Wizard::new(None);
        let transition = w.apply_result(StepResult::Back);
        assert_eq!(transition, WizardTransition::StayOnCurrent);
    }

    #[test]
    fn wizard_next_advances_to_next_step() {
        let mut w = Wizard::new(None);
        let transition = w.apply_result(StepResult::Next);
        assert_eq!(transition, WizardTransition::GoTo(WizardStep::ExportOptions));
        assert_eq!(w.step, WizardStep::ExportOptions);
    }

    #[test]
    fn wizard_full_forward_journey() {
        let mut w = Wizard::new(None);
        let mut steps = vec![];
        loop {
            steps.push(w.step);
            match w.apply_result(StepResult::Next) {
                WizardTransition::GoTo(_) => {}
                WizardTransition::Finished => break,
                other => panic!("unexpected transition: {other:?}"),
            }
        }
        assert_eq!(steps.len(), WizardStep::total());
    }

    #[test]
    fn wizard_back_and_forward_cycle() {
        let mut w = Wizard::new(None);
        // Go to DeployTarget
        w.apply_result(StepResult::Next); // LoadSaved -> ExportOptions
        w.apply_result(StepResult::Next); // ExportOptions -> DeployTarget
        assert_eq!(w.step, WizardStep::DeployTarget);

        // Go back
        w.apply_result(StepResult::Back); // DeployTarget -> ExportOptions
        assert_eq!(w.step, WizardStep::ExportOptions);

        // Go forward again
        w.apply_result(StepResult::Next); // ExportOptions -> DeployTarget
        assert_eq!(w.step, WizardStep::DeployTarget);
    }

    #[test]
    fn wizard_beads_path_stored() {
        let w = Wizard::new(Some(PathBuf::from("/test/beads")));
        assert_eq!(w.beads_path(), Some(Path::new("/test/beads")));
    }

    #[test]
    fn wizard_beads_path_none() {
        let w = Wizard::new(None);
        assert!(w.beads_path().is_none());
    }

    // ── Prerequisite checking ──────────────────────────────────────

    #[test]
    fn prereq_local_always_passes() {
        let result = check_prerequisites(DeployTarget::Local);
        assert!(result.passed);
        assert!(result.missing_tools.is_empty());
    }

    #[test]
    fn prereq_result_has_correct_target() {
        for target in DeployTarget::ALL {
            let result = check_prerequisites(target);
            assert_eq!(result.target, target);
        }
    }

    // ── Interactive wizard tests ───────────────────────────────────

    /// Helper to run wizard with canned input and capture output.
    fn run_wizard_with_input(input: &str) -> (String, std::result::Result<Option<WizardConfig>, crate::BvrError>) {
        let mut reader = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut output = Vec::new();
        let export_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ec = export_called.clone();
        let result = run_wizard_interactive(
            &mut reader,
            &mut output,
            None,
            move |_config| {
                ec.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            |_path| Ok(()),
        );
        let text = String::from_utf8_lossy(&output).to_string();
        (text, result)
    }

    #[test]
    fn wizard_interactive_local_flow_completes() {
        // No saved config, accept defaults, choose local (3), give output path,
        // accept preview (yes), and continue through deploy
        let input = "y\ny\n\n\n3\n./test-out\ny\n\n";
        let (output, result) = run_wizard_with_input(input);
        assert!(
            result.is_ok(),
            "wizard should succeed, got: {result:?}\noutput: {output}"
        );
        let config = result.unwrap();
        assert!(config.is_some(), "wizard should return config");
        let config = config.unwrap();
        assert_eq!(config.deploy_target, Some(DeployTarget::Local));
        assert!(output.contains("Pages wizard complete"));
    }

    #[test]
    fn wizard_interactive_github_flow_collects_repo() {
        // Choose GitHub (1), provide repo name, not private, no description, output path
        let input = "y\ny\n\n\n1\nuser/my-pages\nn\n\n./gh-out\ny\n\n";
        let (output, result) = run_wizard_with_input(input);
        assert!(result.is_ok(), "output: {output}");
        let config = result.unwrap().unwrap();
        assert_eq!(config.deploy_target, Some(DeployTarget::Github));
        assert_eq!(config.github_repo.as_deref(), Some("user/my-pages"));
        assert!(!config.github_private);
    }

    #[test]
    fn wizard_interactive_cloudflare_flow_collects_project() {
        // Choose Cloudflare (2), provide project name, branch name, output path
        let input = "y\ny\n\n\n2\nmy-cf-project\nmain\n./cf-out\ny\n\n";
        let (output, result) = run_wizard_with_input(input);
        assert!(result.is_ok(), "output: {output}");
        let config = result.unwrap().unwrap();
        assert_eq!(config.deploy_target, Some(DeployTarget::Cloudflare));
        assert_eq!(
            config.cloudflare_project.as_deref(),
            Some("my-cf-project")
        );
    }

    #[test]
    fn wizard_interactive_shows_step_numbers() {
        let input = "y\ny\n\n\n3\n./out\ny\n\n";
        let (output, _) = run_wizard_with_input(input);
        assert!(output.contains("Step 2/9"), "expected step numbering: {output}");
    }

    #[test]
    fn wizard_interactive_skip_preview() {
        let input = "y\ny\n\n\n3\n./out\nn\n\n";
        let (output, result) = run_wizard_with_input(input);
        assert!(result.is_ok());
        assert!(
            output.contains("Skipping preview"),
            "expected skip msg: {output}"
        );
    }

    #[test]
    fn wizard_interactive_default_output_path() {
        // Leave output path empty to get default
        let input = "y\ny\n\n\n3\n\ny\n\n";
        let (_, result) = run_wizard_with_input(input);
        let config = result.unwrap().unwrap();
        assert_eq!(
            config.output_path,
            Some(PathBuf::from("./bv-pages"))
        );
    }

    #[test]
    fn wizard_interactive_custom_title() {
        let input = "y\ny\nMy Dashboard\n\n3\n./out\ny\n\n";
        let (_, result) = run_wizard_with_input(input);
        let config = result.unwrap().unwrap();
        assert_eq!(config.title.as_deref(), Some("My Dashboard"));
    }

    #[test]
    fn wizard_interactive_shows_deploy_instructions_github() {
        let input = "y\ny\n\n\n1\nowner/repo\nn\n\n./out\ny\n\n";
        let (output, _) = run_wizard_with_input(input);
        assert!(
            output.contains("gh repo create"),
            "expected gh instructions: {output}"
        );
    }

    #[test]
    fn wizard_interactive_shows_deploy_instructions_cloudflare() {
        let input = "y\ny\n\n\n2\nmy-proj\n\n./out\ny\n\n";
        let (output, _) = run_wizard_with_input(input);
        assert!(
            output.contains("wrangler pages deploy"),
            "expected wrangler instructions: {output}"
        );
    }

    #[test]
    fn wizard_interactive_shows_banner() {
        let input = "y\ny\n\n\n3\n./out\ny\n\n";
        let (output, _) = run_wizard_with_input(input);
        assert!(
            output.contains("bvr pages wizard"),
            "expected banner: {output}"
        );
    }
}
