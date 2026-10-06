use std::ffi::OsStr;
use std::path::PathBuf;

use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};

fn parse_confidence(s: &str) -> Result<f64, String> {
    let value: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(format!(
            "confidence must be between 0.0 and 1.0, got {value}"
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Json,
    Toon,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum GraphFormat {
    #[default]
    Json,
    Dot,
    Mermaid,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum GraphPreset {
    #[default]
    Compact,
    Roomy,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum GraphStyle {
    #[default]
    Force,
    Grid,
}

/// Operational subcommands.
///
/// `--check-update` stays the non-mutating availability check; `upgrade`
/// (issue #23) is the mutating counterpart that actually installs the latest
/// release through the project's supported distribution mechanism
/// (cargo / crates.io).
#[derive(Debug, Clone, Subcommand)]
pub enum BvrCommand {
    /// Install the latest released bvr version (via `cargo install`).
    Upgrade {
        /// Report what would be installed and the exact command, then exit
        /// without changing anything.
        #[arg(long, action = ArgAction::SetTrue)]
        dry_run: bool,
    },
}

#[derive(Debug, Parser)]
#[command(
    name = "bvr",
    about = "Rust port of beads_viewer (bv)",
    disable_help_subcommand = true,
    disable_version_flag = true
)]
pub struct Cli {
    /// Operational verbs (`bvr upgrade`). The robot/query surface stays
    /// flag-based; subcommands are reserved for actions users type by hand.
    #[command(subcommand)]
    pub command: Option<BvrCommand>,

    #[arg(short = 'V', long = "version", action = ArgAction::SetTrue)]
    pub version: bool,

    /// Check whether a newer bvr version is available.
    #[arg(long, action = ArgAction::SetTrue)]
    pub check_update: bool,

    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_help: bool,

    #[arg(long)]
    pub robot_docs: Option<String>,

    /// Machine-readable manifest of every robot command, output format,
    /// environment variable, and exit code.
    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_capabilities: bool,

    /// TUI color theme: light, dark, or auto (detect the terminal background
    /// from `COLORFGBG`). Overrides `BV_THEME`.
    #[arg(long)]
    pub theme: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_schema: bool,

    #[arg(long)]
    pub schema_command: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub stats: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_next: bool,

    #[arg(long, visible_alias = "robot-orient", action = ArgAction::SetTrue)]
    pub robot_overview: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage_by_track: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage_by_label: bool,

    /// Compact --robot-triage output: only decision-relevant fields (id,
    /// title, status, assignee, score, blockers, unblocks, claim commands).
    #[arg(long, action = ArgAction::SetTrue)]
    pub brief: bool,

    /// Comma-separated labels marking a bead not-ready: excluded from
    /// claimable --robot-next/--robot-triage top picks
    /// (env: `BV_ROBOT_NOT_READY_LABELS`).
    #[arg(long)]
    pub robot_not_ready_labels: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_plan: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_insights: bool,

    /// Include full per-node metric maps in robot-insights output.
    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_full_stats: bool,

    /// Maximum items per insight category (bottlenecks, influencers, etc.).
    #[arg(long, default_value_t = 20)]
    pub insight_limit: usize,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_priority: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_alerts: bool,

    /// Emit economics projections (burn rate, cost-to-complete, cost-of-delay).
    /// Requires `--economics-overlay <path>` or `BVR_ECONOMICS_OVERLAY` env var
    /// pointing at a JSON file with `hourly_rate` and `hours_per_day`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_economics: bool,

    /// Path to a JSON economics overlay file. Overrides `BVR_ECONOMICS_OVERLAY`
    /// when both are set. Required for `--robot-economics` unless the env var
    /// is set. Schema: `{"hourly_rate": f64, "hours_per_day": f64,
    /// "budget_envelope": f64?, "throughput_window_days": u32?,
    /// "currency": String?}`.
    #[arg(long)]
    pub economics_overlay: Option<std::path::PathBuf>,

    /// Emit delivery posture classification (flow mix, urgency profile,
    /// milestone pressure). No overlay required.
    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_delivery: bool,

    #[arg(long)]
    pub severity: Option<String>,

    #[arg(long)]
    pub alert_type: Option<String>,

    #[arg(long)]
    pub alert_label: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_suggest: bool,

    #[arg(long)]
    pub suggest_type: Option<String>,

    #[arg(long, default_value_t = 0.0, value_parser = parse_confidence)]
    pub suggest_confidence: f64,

    #[arg(long)]
    pub suggest_bead: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_diff: bool,

    #[arg(long)]
    pub diff_since: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_history: bool,

    #[arg(long)]
    pub bead_history: Option<String>,

    #[arg(long, default_value_t = 500)]
    pub history_limit: usize,

    #[arg(long)]
    pub history_since: Option<String>,

    #[arg(long = "min-confidence", default_value_t = 0.0, value_parser = parse_confidence)]
    pub history_min_confidence: f64,

    #[arg(long)]
    pub robot_burndown: Option<String>,

    #[arg(long)]
    pub robot_forecast: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_graph: bool,

    #[arg(long, value_enum, default_value_t = GraphFormat::Json)]
    pub graph_format: GraphFormat,

    #[arg(long)]
    pub graph_root: Option<String>,

    #[arg(long, default_value_t = 0)]
    pub graph_depth: usize,

    #[arg(long, value_enum, default_value_t = GraphPreset::Compact)]
    pub graph_preset: GraphPreset,

    #[arg(long, value_enum, default_value_t = GraphStyle::Force)]
    pub graph_style: GraphStyle,

    #[arg(long)]
    pub graph_title: Option<String>,

    #[arg(long)]
    pub export_graph: Option<PathBuf>,

    #[arg(long)]
    pub forecast_label: Option<String>,

    #[arg(long)]
    pub forecast_sprint: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub forecast_agents: usize,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_capacity: bool,

    #[arg(long = "agents", default_value_t = 1)]
    pub capacity_agents: usize,

    #[arg(long)]
    pub capacity_label: Option<String>,

    #[arg(long, default_value_t = 10)]
    pub robot_max_results: usize,

    #[arg(long, default_value_t = 0.0)]
    pub robot_min_confidence: f64,

    #[arg(long)]
    pub robot_by_label: Option<String>,

    #[arg(long)]
    pub robot_by_assignee: Option<String>,

    #[arg(long)]
    pub label: Option<String>,

    #[arg(long)]
    pub workspace: Option<PathBuf>,

    #[arg(short = 'r', long)]
    pub repo: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_sprint_list: bool,

    #[arg(long)]
    pub robot_sprint_show: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_metrics: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_label_health: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_label_flow: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_label_attention: bool,

    #[arg(long, default_value_t = 0)]
    pub attention_limit: usize,

    #[arg(long)]
    pub robot_explain_correlation: Option<String>,

    #[arg(long)]
    pub robot_confirm_correlation: Option<String>,

    #[arg(long)]
    pub robot_reject_correlation: Option<String>,

    #[arg(long)]
    pub correlation_by: Option<String>,

    #[arg(long)]
    pub correlation_reason: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_correlation_stats: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_orphans: bool,

    #[arg(long, default_value_t = 30)]
    pub orphans_min_score: u32,

    #[arg(long)]
    pub robot_file_beads: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub file_beads_limit: usize,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_file_hotspots: bool,

    #[arg(long, default_value_t = 10)]
    pub hotspots_limit: usize,

    #[arg(long)]
    pub robot_impact: Option<String>,

    #[arg(long)]
    pub robot_file_relations: Option<String>,

    #[arg(long, default_value_t = 0.5)]
    pub relations_threshold: f64,

    #[arg(long, default_value_t = 10)]
    pub relations_limit: usize,

    #[arg(long)]
    pub robot_related: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub related_min_relevance: u32,

    #[arg(long, default_value_t = 10)]
    pub related_max_results: usize,

    #[arg(long)]
    pub robot_blocker_chain: Option<String>,

    #[arg(long)]
    pub robot_impact_network: Option<String>,

    #[arg(long, default_value_t = 2)]
    pub network_depth: usize,

    #[arg(long)]
    pub robot_causality: Option<String>,

    #[arg(long)]
    pub save_baseline: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_drift: bool,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_search: bool,

    #[arg(long, default_value_t = 10)]
    pub search_limit: usize,

    #[arg(long)]
    pub search_mode: Option<String>,

    #[arg(long)]
    pub search_preset: Option<String>,

    #[arg(long)]
    pub search_weights: Option<String>,

    /// List available triage recipes.
    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_recipes: bool,

    /// Apply a named recipe to filter/sort recommendations.
    #[arg(long)]
    pub recipe: Option<String>,

    /// Scoring weight preset (default, graph-heavy, priority-first, quick-wins, risk-averse).
    #[arg(long)]
    pub weight_preset: Option<String>,

    /// Emit a shell script for the top recommendations.
    #[arg(long, action = ArgAction::SetTrue)]
    pub emit_script: bool,

    /// Number of recommendations to include in emitted script (default 5).
    #[arg(long, default_value_t = 5)]
    pub script_limit: usize,

    /// Shell format for emitted script: bash (default), fish, zsh.
    #[arg(long, default_value = "bash")]
    pub script_format: String,

    /// Record positive feedback for a recommendation.
    #[arg(long)]
    pub feedback_accept: Option<String>,

    /// Record negative feedback (ignore) for a recommendation.
    #[arg(long)]
    pub feedback_ignore: Option<String>,

    /// Show feedback statistics.
    #[arg(long, action = ArgAction::SetTrue)]
    pub feedback_show: bool,

    /// Reset all recorded feedback.
    #[arg(long, action = ArgAction::SetTrue)]
    pub feedback_reset: bool,

    /// Generate a priority brief as markdown and write to the given path.
    #[arg(long)]
    pub priority_brief: Option<PathBuf>,

    /// Generate an agent brief bundle in the given directory.
    #[arg(long)]
    pub agent_brief: Option<PathBuf>,

    /// Export static pages bundle to directory.
    #[arg(long)]
    pub export_pages: Option<PathBuf>,

    /// Preview an existing static pages bundle from directory.
    #[arg(long)]
    pub preview_pages: Option<PathBuf>,

    /// Watch beads file changes and auto-regenerate pages export.
    #[arg(long, action = ArgAction::SetTrue)]
    pub watch_export: bool,

    /// Launch pages deployment wizard.
    #[arg(long, action = ArgAction::SetTrue)]
    pub pages: bool,

    /// Include closed issues in exported pages bundle (default: true).
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub pages_include_closed: bool,

    /// Include history payload in exported pages bundle (default: true).
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub pages_include_history: bool,

    /// Custom title for exported pages bundle.
    #[arg(long)]
    pub pages_title: Option<String>,

    /// Custom subtitle for exported pages bundle.
    #[arg(long)]
    pub pages_subtitle: Option<String>,

    /// Disable live reload when previewing pages.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_live_reload: bool,

    /// Enable experimental background snapshot loading (TUI only).
    #[arg(long, action = ArgAction::SetTrue)]
    pub background_mode: bool,

    /// Disable experimental background snapshot loading (TUI only).
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_background_mode: bool,

    #[arg(long)]
    pub export_md: Option<PathBuf>,

    /// Export a report to this path (format chosen by --export-format).
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// Report format for --export: markdown (default), json, csv, or mermaid.
    #[arg(long)]
    pub export_format: Option<String>,

    /// Include dependency context in the --export report (default: true
    /// except for csv). Use `--export-include-graph=false` to omit it.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub export_include_graph: Option<bool>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub no_hooks: bool,

    /// Start the TUI in the given view instead of Main.
    /// Supported: main, board, insights, graph, history, actionable,
    /// attention, tree, labels, flow, timediff, sprint.
    #[arg(long)]
    pub view: Option<String>,

    /// Start the TUI with a list-status filter applied.
    /// Supported: all, open, in-progress, blocked, closed, ready.
    #[arg(long)]
    pub list_filter: Option<String>,

    /// Render a named TUI view non-interactively and output to stdout.
    /// Supported views: insights, board, history, main, graph.
    #[arg(long)]
    pub debug_render: Option<String>,

    /// Width in columns for debug render (default 180).
    #[arg(long, default_value_t = 180)]
    pub debug_width: u16,

    /// Height in rows for debug render (default 50).
    #[arg(long, default_value_t = 50)]
    pub debug_height: u16,

    /// Check agent file blurb status.
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_check: bool,

    /// Add beads workflow blurb to agent file (creates AGENTS.md if needed).
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_add: bool,

    /// Update blurb to current version in agent file.
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_update: bool,

    /// Remove blurb from agent file.
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_remove: bool,

    /// Dry-run mode for agents commands (show what would change without writing).
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_dry_run: bool,

    /// Skip confirmation prompts for agents commands (legacy compatibility flag).
    #[arg(long, action = ArgAction::SetTrue)]
    pub agents_force: bool,

    #[arg(long)]
    pub as_of: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub force_full_analysis: bool,

    /// Output detailed startup timing profile for diagnostics.
    #[arg(long, action = ArgAction::SetTrue)]
    pub profile_startup: bool,

    /// Output profile in JSON format (use with --profile-startup).
    #[arg(long, action = ArgAction::SetTrue)]
    pub profile_json: bool,

    /// Bypass disk cache for this invocation.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_cache: bool,

    /// Legacy compatibility alias for `--beads-file`.
    #[arg(long)]
    pub db: Option<PathBuf>,

    /// Show baseline metadata (when it was saved, description, stats).
    #[arg(long, action = ArgAction::SetTrue)]
    pub baseline_info: bool,

    /// Compare current state against saved baseline with human-readable output.
    #[arg(long, action = ArgAction::SetTrue)]
    pub check_drift: bool,

    /// Include closed issues in related work discovery.
    #[arg(long, action = ArgAction::SetTrue)]
    pub related_include_closed: bool,

    #[arg(long, hide = true)]
    pub beads_file: Option<PathBuf>,

    #[arg(long, hide = true)]
    pub repo_path: Option<PathBuf>,
}

impl Cli {
    pub fn resolve_output_format(&self) -> std::result::Result<OutputFormat, String> {
        let cli_explicit = format_flag_was_explicit_in_args(std::env::args_os().skip(1));
        resolve_output_format_choice(
            self.format,
            cli_explicit,
            std::env::var("BV_OUTPUT_FORMAT").ok().as_deref(),
            std::env::var("TOON_DEFAULT_FORMAT").ok().as_deref(),
        )
    }

    #[must_use]
    pub fn resolve_stats_flag(&self) -> bool {
        self.stats || std::env::var("TOON_STATS").is_ok_and(|value| value.trim() == "1")
    }

    #[must_use]
    pub fn resolve_search_preset(&self) -> Option<String> {
        resolve_optional_string_choice(
            self.search_preset.as_deref(),
            std::env::var("BV_SEARCH_PRESET").ok().as_deref(),
        )
    }

    #[must_use]
    pub fn is_operational_command(&self) -> bool {
        self.check_update || self.command.is_some()
    }

    #[must_use]
    pub fn is_robot_command(&self) -> bool {
        self.robot_help
            || self.robot_next
            || self.robot_overview
            || self.robot_triage
            || self.robot_triage_by_track
            || self.robot_triage_by_label
            || self.robot_plan
            || self.robot_insights
            || self.robot_priority
            || self.robot_alerts
            || self.robot_economics
            || self.robot_delivery
            || self.robot_suggest
            || self.robot_diff
            || self.robot_history
            || self.robot_burndown.is_some()
            || self.robot_graph
            || self.robot_forecast.is_some()
            || self.robot_capacity
            || self.bead_history.is_some()
            || self.robot_docs.is_some()
            || self.robot_capabilities
            || self.robot_schema
            || self.robot_sprint_list
            || self.robot_sprint_show.is_some()
            || self.robot_metrics
            || self.robot_label_health
            || self.robot_label_flow
            || self.robot_label_attention
            || self.robot_explain_correlation.is_some()
            || self.robot_confirm_correlation.is_some()
            || self.robot_reject_correlation.is_some()
            || self.robot_correlation_stats
            || self.robot_orphans
            || self.robot_file_beads.is_some()
            || self.robot_file_hotspots
            || self.robot_impact.is_some()
            || self.robot_file_relations.is_some()
            || self.robot_related.is_some()
            || self.robot_blocker_chain.is_some()
            || self.robot_impact_network.is_some()
            || self.robot_causality.is_some()
            || self.save_baseline.is_some()
            || self.robot_drift
            || self.check_drift
            || self.robot_search
            || self.robot_recipes
            || self.emit_script
            || self.feedback_show
            || self.feedback_accept.is_some()
            || self.feedback_ignore.is_some()
            || self.feedback_reset
            || self.priority_brief.is_some()
            || self.agent_brief.is_some()
            || self.profile_startup
    }

    #[must_use]
    pub fn is_agents_command(&self) -> bool {
        self.agents_check
            || self.agents_add
            || self.agents_update
            || self.agents_remove
            || self.agents_dry_run
            || self.agents_force
    }
}

fn resolve_output_format_choice(
    cli_format: OutputFormat,
    cli_explicit: bool,
    bv_output_format: Option<&str>,
    toon_default_format: Option<&str>,
) -> std::result::Result<OutputFormat, String> {
    if cli_explicit {
        return Ok(cli_format);
    }

    for (source, raw) in [
        ("BV_OUTPUT_FORMAT", bv_output_format),
        ("TOON_DEFAULT_FORMAT", toon_default_format),
    ] {
        let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };

        return OutputFormat::from_str(raw, true)
            .map_err(|_| format!("invalid {source} value {raw:?} (expected json|toon)"));
    }

    Ok(cli_format)
}

fn resolve_optional_string_choice(
    cli_value: Option<&str>,
    env_value: Option<&str>,
) -> Option<String> {
    cli_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .or_else(|| {
            env_value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(std::string::ToString::to_string)
        })
}

fn format_flag_was_explicit_in_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter().any(|arg| {
        let text = arg.as_ref().to_string_lossy();
        text == "--format" || text.starts_with("--format=")
    })
}

// ---------------------------------------------------------------------------
// Agent-intent invocation aliases
//
// Agents routinely guess subcommand-style forms (`bvr triage --json`,
// `bvr robot-next`, `bvr search "oauth login" --limit 5`, `bvr graph mermaid`).
// Legacy `bv` accepts these; `rewrite_agent_intent_args` maps them onto the
// canonical flag surface before clap parses, so the flag contract stays the
// single source of truth. Tokens it does not recognise pass through untouched
// so clap still reports genuine typos.
// ---------------------------------------------------------------------------

/// Robot commands taking no value, addressable as `bvr <name>` or
/// `bvr robot-<name>`.
const AGENT_BOOL_COMMANDS: &[(&str, &str)] = &[
    ("triage", "--robot-triage"),
    ("recommend", "--robot-triage"),
    ("recommendations", "--robot-triage"),
    ("next", "--robot-next"),
    ("pick", "--robot-next"),
    ("overview", "--robot-overview"),
    ("orient", "--robot-overview"),
    ("plan", "--robot-plan"),
    ("insights", "--robot-insights"),
    ("insight", "--robot-insights"),
    ("analysis", "--robot-insights"),
    ("analyze", "--robot-insights"),
    ("priority", "--robot-priority"),
    ("priorities", "--robot-priority"),
    ("alerts", "--robot-alerts"),
    ("suggest", "--robot-suggest"),
    ("suggestions", "--robot-suggest"),
    ("recipes", "--robot-recipes"),
    ("metrics", "--robot-metrics"),
    ("capabilities", "--robot-capabilities"),
    ("capability", "--robot-capabilities"),
    ("manifest", "--robot-capabilities"),
    ("labels", "--robot-label-health"),
    ("label-health", "--robot-label-health"),
    ("label-flow", "--robot-label-flow"),
    ("label-attention", "--robot-label-attention"),
    ("hotspots", "--robot-file-hotspots"),
    ("file-hotspots", "--robot-file-hotspots"),
    ("sprints", "--robot-sprint-list"),
    ("sprint-list", "--robot-sprint-list"),
    ("capacity", "--robot-capacity"),
    ("orphans", "--robot-orphans"),
    ("correlation-stats", "--robot-correlation-stats"),
    ("triage-by-track", "--robot-triage-by-track"),
    ("triage-by-label", "--robot-triage-by-label"),
    ("economics", "--robot-economics"),
    ("delivery", "--robot-delivery"),
];

/// Robot commands whose first positional is their value:
/// (name, value flag, default when no positional is given).
const AGENT_VALUE_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("file-beads", "--robot-file-beads", None),
    ("file-relations", "--robot-file-relations", None),
    ("impact", "--robot-impact", None),
    ("related", "--robot-related", None),
    ("blockers", "--robot-blocker-chain", None),
    ("blocker-chain", "--robot-blocker-chain", None),
    ("impact-network", "--robot-impact-network", Some("all")),
    ("causality", "--robot-causality", None),
    ("sprint", "--robot-sprint-show", None),
    ("sprint-show", "--robot-sprint-show", None),
    ("forecast", "--robot-forecast", Some("all")),
    ("burndown", "--robot-burndown", Some("current")),
    ("explain-correlation", "--robot-explain-correlation", None),
    ("confirm-correlation", "--robot-confirm-correlation", None),
    ("reject-correlation", "--robot-reject-correlation", None),
];

fn is_positional(arg: &str) -> bool {
    !arg.is_empty() && !arg.starts_with('-')
}

fn is_output_format(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "json" | "toon")
}

/// The flag `--limit` means for a given command context.
fn limit_flag_for(context: &str) -> &'static str {
    match context {
        "search" => "--search-limit",
        "label-attention" => "--attention-limit",
        "file-beads" => "--file-beads-limit",
        "hotspots" | "file-hotspots" => "--hotspots-limit",
        "history" => "--history-limit",
        "related" => "--related-max-results",
        "file-relations" => "--relations-limit",
        "emit-script" => "--script-limit",
        _ => "--robot-max-results",
    }
}

/// Rewrite agent output/limit aliases inside an argument list:
/// `--json`/`--toon`/`--output <fmt>`/`-o <fmt>` → `--format <fmt>`,
/// `--limit <n>` → the context's limit flag, `--name` → `--label`.
fn rewrite_flag_aliases(args: &[String], context: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let next = args.get(index + 1).map(String::as_str);
        match arg {
            "--json" | "--json=true" | "--json=false" | "--toon=false" => {
                out.extend(["--format".to_string(), "json".to_string()]);
            }
            "--toon" | "--toon=true" => {
                out.extend(["--format".to_string(), "toon".to_string()]);
            }
            "--output" | "-o" if next.is_some_and(is_output_format) => {
                out.extend([
                    "--format".to_string(),
                    next.unwrap_or_default().to_ascii_lowercase(),
                ]);
                index += 1;
            }
            "--limit" => out.push(limit_flag_for(context).to_string()),
            "--name" => out.push("--label".to_string()),
            _ => {
                if let Some(value) = arg
                    .strip_prefix("--output=")
                    .or_else(|| arg.strip_prefix("-o="))
                    .filter(|value| is_output_format(value))
                {
                    out.push(format!("--format={}", value.to_ascii_lowercase()));
                } else if let Some(value) = arg.strip_prefix("--limit=") {
                    out.push(format!("{}={value}", limit_flag_for(context)));
                } else if let Some(value) = arg.strip_prefix("--name=") {
                    out.push(format!("--label={value}"));
                } else {
                    out.push(arg.to_string());
                }
            }
        }
        index += 1;
    }
    out
}

fn has_structured_output_alias(args: &[String]) -> bool {
    args.iter().enumerate().any(|(index, arg)| {
        let lower = arg.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "--json" | "--json=true" | "--json=false" | "--toon" | "--toon=true" | "--toon=false"
        ) || lower
            .strip_prefix("--output=")
            .is_some_and(is_output_format)
            || lower.strip_prefix("-o=").is_some_and(is_output_format)
            || ((lower == "--output" || lower == "-o")
                && args.get(index + 1).is_some_and(|v| is_output_format(v)))
    })
}

/// True when the args already select a primary action (a robot command or a
/// non-robot verb such as export/pages/version), so `--json` alone must not
/// imply `--robot-triage`.
fn has_primary_action(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let name = arg.split('=').next().unwrap_or_default();
        (name.starts_with("--robot-")
            && !matches!(
                name,
                "--robot-max-results"
                    | "--robot-min-confidence"
                    | "--robot-by-label"
                    | "--robot-by-assignee"
                    | "--robot-not-ready-labels"
                    | "--robot-full-stats"
            ))
            || matches!(
                name,
                "--version"
                    | "-V"
                    | "--help"
                    | "-h"
                    | "--check-update"
                    | "--pages"
                    | "--export-pages"
                    | "--preview-pages"
                    | "--export"
                    | "--export-md"
                    | "--export-graph"
                    | "--priority-brief"
                    | "--agent-brief"
                    | "--emit-script"
                    | "--search"
                    | "--save-baseline"
                    | "--baseline-info"
                    | "--check-drift"
                    | "--feedback-show"
                    | "--feedback-reset"
                    | "--feedback-accept"
                    | "--feedback-ignore"
                    | "--debug-render"
                    | "--agents-check"
                    | "--agents-add"
                    | "--agents-update"
                    | "--agents-remove"
                    | "--stats"
            )
    })
}

/// Map agent-style invocations onto the canonical flag surface. `args`
/// excludes the program name.
#[must_use]
pub fn rewrite_agent_intent_args(args: &[String]) -> Vec<String> {
    if args.is_empty() {
        return Vec::new();
    }

    // The command word is the first positional that is not a flag's value,
    // so `bvr --workspace w.yaml next` works as well as `bvr next`.
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if is_positional(arg) {
            let command = arg.trim().to_ascii_lowercase();
            let bare = command.strip_prefix("robot-").unwrap_or(&command);
            if let Some(rewritten) = rewrite_command(bare, &command, &args[index + 1..]) {
                let mut out = rewrite_flag_aliases(&args[..index], bare);
                out.extend(rewritten);
                return out;
            }
            return args.to_vec();
        }
        let flag_takes_next = arg.starts_with("--") && !arg.contains('=') && takes_value(arg)
            || matches!(arg, "-r" | "-l" | "--limit" | "--output" | "-o");
        index += if flag_takes_next { 2 } else { 1 };
    }

    let rewritten = rewrite_flag_aliases(args, "");
    if has_structured_output_alias(args) && !has_primary_action(&rewritten) {
        let mut out = vec!["--robot-triage".to_string()];
        out.extend(rewritten);
        return out;
    }
    rewritten
}

fn rewrite_command(bare: &str, command: &str, rest: &[String]) -> Option<Vec<String>> {
    let with_prefix = |prefix: Vec<String>, context: &str, rest: &[String]| {
        let mut out = prefix;
        out.extend(rewrite_flag_aliases(rest, context));
        out
    };
    let take_positional = |rest: &[String]| -> (Option<String>, Vec<String>) {
        // The first positional (skipping leading output/limit aliases) is
        // the command's value; everything else is passed through.
        let mut remaining = rest.to_vec();
        let position = remaining.iter().position(|arg| is_positional(arg));
        let value = position.and_then(|index| {
            let prior_takes_value =
                index > 0 && matches!(remaining[index - 1].as_str(), "--limit" | "--output" | "-o");
            (!prior_takes_value).then(|| remaining.remove(index))
        });
        (value, remaining)
    };

    if let Some((_, flag)) = AGENT_BOOL_COMMANDS.iter().find(|(name, _)| *name == bare) {
        return Some(with_prefix(vec![(*flag).to_string()], bare, rest));
    }

    if let Some((_, flag, default)) = AGENT_VALUE_COMMANDS
        .iter()
        .find(|(name, _, _)| *name == bare)
    {
        let (value, remaining) = take_positional(rest);
        let mut prefix = vec![(*flag).to_string()];
        match value.or_else(|| default.map(str::to_string)) {
            Some(value) => prefix.push(value),
            // No value: leave the flag last so clap reports the missing value.
            None => {
                let mut out = rewrite_flag_aliases(&remaining, bare);
                out.push((*flag).to_string());
                return Some(out);
            }
        }
        return Some(with_prefix(prefix, bare, &remaining));
    }

    match bare {
        "help" if command == "robot-help" => {
            if has_structured_output_alias(rest) {
                Some(with_prefix(
                    vec!["--robot-docs".to_string(), "guide".to_string()],
                    "docs",
                    rest,
                ))
            } else {
                Some(with_prefix(vec!["--robot-help".to_string()], "help", rest))
            }
        }
        "docs" | "doc" => {
            let (topic, remaining) = take_positional(rest);
            Some(with_prefix(
                vec![
                    "--robot-docs".to_string(),
                    topic.unwrap_or_else(|| "guide".to_string()),
                ],
                "docs",
                &remaining,
            ))
        }
        "schema" | "schemas" => {
            let (target, remaining) = take_positional(rest);
            let mut prefix = vec!["--robot-schema".to_string()];
            if let Some(target) = target {
                let target = target.to_ascii_lowercase();
                prefix.push("--schema-command".to_string());
                prefix.push(if target.starts_with("robot-") {
                    target
                } else {
                    format!("robot-{target}")
                });
            }
            Some(with_prefix(prefix, "schema", &remaining))
        }
        "search" | "find" => {
            let mut query = Vec::new();
            let mut remaining = Vec::new();
            let mut index = 0;
            while index < rest.len() {
                let arg = &rest[index];
                if is_positional(arg) {
                    query.push(arg.clone());
                } else {
                    remaining.push(arg.clone());
                    if matches!(arg.as_str(), "--limit" | "--output" | "-o")
                        || (arg.starts_with("--") && !arg.contains('=') && takes_value(arg))
                    {
                        if let Some(value) = rest.get(index + 1) {
                            remaining.push(value.clone());
                            index += 1;
                        }
                    }
                }
                index += 1;
            }
            let mut prefix = Vec::new();
            if !query.is_empty() {
                prefix.push("--search".to_string());
                prefix.push(query.join(" "));
            }
            prefix.push("--robot-search".to_string());
            Some(with_prefix(prefix, "search", &remaining))
        }
        "graph" => {
            let mut prefix = vec!["--robot-graph".to_string()];
            let mut remaining = rest.to_vec();
            if let Some(index) = remaining.iter().position(|arg| is_positional(arg))
                && matches!(
                    remaining[index].to_ascii_lowercase().as_str(),
                    "json" | "dot" | "mermaid"
                )
            {
                let format = remaining.remove(index).to_ascii_lowercase();
                prefix.push("--graph-format".to_string());
                prefix.push(format);
            }
            Some(with_prefix(prefix, "graph", &remaining))
        }
        "diff" | "changes" => {
            let (since, remaining) = take_positional(rest);
            let mut prefix = vec!["--robot-diff".to_string()];
            if let Some(since) = since {
                prefix.push("--diff-since".to_string());
                prefix.push(since);
            }
            Some(with_prefix(prefix, "diff", &remaining))
        }
        "history" => {
            let (bead, remaining) = take_positional(rest);
            let mut prefix = vec!["--robot-history".to_string()];
            if let Some(bead) = bead {
                prefix.push("--bead-history".to_string());
                prefix.push(bead);
            }
            Some(with_prefix(prefix, "history", &remaining))
        }
        "drift" => Some(with_prefix(
            vec!["--check-drift".to_string(), "--robot-drift".to_string()],
            "drift",
            rest,
        )),
        "self-update" | "selfupdate" => {
            let mut out = vec!["upgrade".to_string()];
            out.extend(rest.iter().cloned());
            Some(out)
        }
        _ => None,
    }
}

/// Whether a long flag consumes the following argument (used to keep flag
/// values out of a free-text search query).
fn takes_value(flag: &str) -> bool {
    let name = flag.trim_start_matches('-').replace('-', "_");
    Cli::command()
        .get_arguments()
        .find(|arg| arg.get_id().as_str() == name)
        .is_some_and(|arg| {
            arg.get_action().takes_values()
                && arg
                    .get_num_args()
                    .is_none_or(|range| range.min_values() > 0)
        })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        Cli, OutputFormat, format_flag_was_explicit_in_args, resolve_optional_string_choice,
        resolve_output_format_choice,
    };

    fn rw(args: &[&str]) -> Vec<String> {
        let owned: Vec<String> = args.iter().map(ToString::to_string).collect();
        super::rewrite_agent_intent_args(&owned)
    }

    #[test]
    fn agent_intent_bare_and_robot_prefixed_commands() {
        assert_eq!(
            rw(&["triage", "--json"]),
            ["--robot-triage", "--format", "json"]
        );
        assert_eq!(
            rw(&["robot-next", "--toon"]),
            ["--robot-next", "--format", "toon"]
        );
        assert_eq!(
            rw(&["plan", "-o", "json"]),
            ["--robot-plan", "--format", "json"]
        );
        assert_eq!(rw(&["capabilities"]), ["--robot-capabilities"]);
        assert_eq!(
            rw(&["triage", "--limit", "5", "--name", "api"]),
            [
                "--robot-triage",
                "--robot-max-results",
                "5",
                "--label",
                "api"
            ]
        );
    }

    #[test]
    fn agent_intent_value_commands_take_first_positional() {
        assert_eq!(rw(&["forecast"]), ["--robot-forecast", "all"]);
        assert_eq!(
            rw(&["forecast", "bd-1", "--json"]),
            ["--robot-forecast", "bd-1", "--format", "json"]
        );
        assert_eq!(rw(&["burndown"]), ["--robot-burndown", "current"]);
        assert_eq!(
            rw(&["related", "--json", "bd-7"]),
            ["--robot-related", "bd-7", "--format", "json"]
        );
        // Missing required value: flag left last so clap reports it.
        assert_eq!(
            rw(&["blockers", "--json"]),
            ["--format", "json", "--robot-blocker-chain"]
        );
        assert_eq!(
            rw(&["diff", "HEAD~3"]),
            ["--robot-diff", "--diff-since", "HEAD~3"]
        );
        assert_eq!(
            rw(&["history", "bd-2"]),
            ["--robot-history", "--bead-history", "bd-2"]
        );
        assert_eq!(rw(&["history"]), ["--robot-history"]);
    }

    #[test]
    fn agent_intent_search_graph_docs_schema() {
        assert_eq!(
            rw(&["search", "oauth", "login", "--limit", "3", "--json"]),
            [
                "--search",
                "oauth login",
                "--robot-search",
                "--search-limit",
                "3",
                "--format",
                "json"
            ]
        );
        assert_eq!(
            rw(&["search", "--search-mode", "hybrid", "auth"]),
            [
                "--search",
                "auth",
                "--robot-search",
                "--search-mode",
                "hybrid"
            ]
        );
        assert_eq!(
            rw(&["graph", "mermaid"]),
            ["--robot-graph", "--graph-format", "mermaid"]
        );
        assert_eq!(rw(&["docs"]), ["--robot-docs", "guide"]);
        assert_eq!(rw(&["docs", "env"]), ["--robot-docs", "env"]);
        assert_eq!(
            rw(&["schema", "triage"]),
            ["--robot-schema", "--schema-command", "robot-triage"]
        );
        assert_eq!(rw(&["drift"]), ["--check-drift", "--robot-drift"]);
    }

    #[test]
    fn agent_intent_json_alone_implies_triage_only_without_primary_action() {
        assert_eq!(rw(&["--json"]), ["--robot-triage", "--format", "json"]);
        assert_eq!(
            rw(&["--robot-plan", "--json"]),
            ["--robot-plan", "--format", "json"]
        );
        assert_eq!(
            rw(&["--json", "--robot-max-results", "3"]),
            [
                "--robot-triage",
                "--format",
                "json",
                "--robot-max-results",
                "3"
            ]
        );
        assert_eq!(
            rw(&["--export", "r.json", "--json"]),
            ["--export", "r.json", "--format", "json"]
        );
    }

    #[test]
    fn agent_intent_leaves_unknown_and_canonical_args_alone() {
        assert_eq!(rw(&["upgrade", "--dry-run"]), ["upgrade", "--dry-run"]);
        assert_eq!(rw(&["self-update"]), ["upgrade"]);
        assert_eq!(
            rw(&["--workspace", "w.yaml", "next", "--json"]),
            ["--workspace", "w.yaml", "--robot-next", "--format", "json"]
        );
        assert_eq!(rw(&["--beads-file", "triage"]), ["--beads-file", "triage"]);
        assert_eq!(rw(&["--robot-triage"]), ["--robot-triage"]);
        assert_eq!(rw(&["frobnicate"]), ["frobnicate"]);
        assert_eq!(rw(&[]), Vec::<String>::new());
    }

    #[test]
    fn agent_intent_rewrites_parse_with_clap() {
        for args in [
            vec!["triage", "--json"],
            vec!["search", "auth", "--limit", "2"],
            vec!["forecast", "--json"],
            vec!["graph", "dot"],
            vec!["schema", "next"],
            vec!["--json"],
        ] {
            let mut argv = vec!["bvr".to_string()];
            argv.extend(rw(&args));
            Cli::try_parse_from(&argv).unwrap_or_else(|error| panic!("{args:?}: {error}"));
        }
    }

    #[test]
    fn parse_operational_flags() {
        let cli = Cli::parse_from(["bvr", "--check-update"]);
        assert!(cli.check_update);
        assert!(cli.is_operational_command());
    }

    #[test]
    fn parse_agents_force_as_agents_command() {
        let cli = Cli::parse_from(["bvr", "--agents-force"]);
        assert!(cli.agents_force);
        assert!(cli.is_agents_command());
    }

    #[test]
    fn parse_pages_flags() {
        let cli = Cli::parse_from([
            "bvr",
            "--export-pages",
            "bundle",
            "--watch-export",
            "--pages-title",
            "Dashboard",
            "--pages-subtitle",
            "Triage View",
            "--pages-include-closed=false",
            "--pages-include-history=false",
        ]);

        assert_eq!(
            cli.export_pages
                .as_deref()
                .and_then(std::path::Path::to_str),
            Some("bundle")
        );
        assert!(cli.watch_export);
        assert_eq!(cli.pages_title.as_deref(), Some("Dashboard"));
        assert_eq!(cli.pages_subtitle.as_deref(), Some("Triage View"));
        assert!(!cli.pages_include_closed);
        assert!(!cli.pages_include_history);
    }

    #[test]
    fn parse_background_mode_flags() {
        let cli = Cli::parse_from(["bvr", "--background-mode", "--no-background-mode"]);
        assert!(cli.background_mode);
        assert!(cli.no_background_mode);
    }

    #[test]
    fn explicit_format_flag_detected_with_split_syntax() {
        assert!(format_flag_was_explicit_in_args([
            "--robot-next",
            "--format",
            "toon"
        ]));
    }

    #[test]
    fn explicit_format_flag_detected_with_equals_syntax() {
        assert!(format_flag_was_explicit_in_args([
            "--robot-next",
            "--format=toon"
        ]));
    }

    #[test]
    fn resolve_output_format_uses_env_when_cli_flag_absent() {
        let resolved = resolve_output_format_choice(OutputFormat::Json, false, Some("toon"), None)
            .expect("format");
        assert!(matches!(resolved, OutputFormat::Toon));
    }

    #[test]
    fn resolve_output_format_prefers_cli_when_flag_explicit() {
        let resolved = resolve_output_format_choice(OutputFormat::Json, true, Some("toon"), None)
            .expect("format");
        assert!(matches!(resolved, OutputFormat::Json));
    }

    #[test]
    fn resolve_output_format_falls_back_to_secondary_env() {
        let resolved = resolve_output_format_choice(OutputFormat::Json, false, None, Some("toon"))
            .expect("format");
        assert!(matches!(resolved, OutputFormat::Toon));
    }

    #[test]
    fn resolve_output_format_rejects_invalid_env_values() {
        let error = resolve_output_format_choice(OutputFormat::Json, false, Some("yaml"), None)
            .expect_err("invalid env should fail");
        assert!(error.contains("BV_OUTPUT_FORMAT"));
        assert!(error.contains("json|toon"));
    }

    #[test]
    fn resolve_search_preset_uses_env_when_cli_flag_absent() {
        let resolved = resolve_optional_string_choice(None, Some("impact-first"));
        assert_eq!(resolved.as_deref(), Some("impact-first"));
    }

    #[test]
    fn resolve_search_preset_prefers_cli_over_env() {
        let resolved = resolve_optional_string_choice(Some("text-only"), Some("impact-first"));
        assert_eq!(resolved.as_deref(), Some("text-only"));
    }

    #[test]
    fn resolve_search_preset_ignores_blank_values() {
        let resolved = resolve_optional_string_choice(Some("   "), Some("  "));
        assert_eq!(resolved, None);
    }

    #[test]
    fn parse_no_cache_flag() {
        let cli = Cli::parse_from(["bvr", "--no-cache", "--robot-triage"]);
        assert!(cli.no_cache);
    }

    #[test]
    fn parse_db_flag() {
        let cli = Cli::parse_from(["bvr", "--db", "/tmp/test.jsonl", "--robot-triage"]);
        assert_eq!(
            cli.db.as_deref().and_then(std::path::Path::to_str),
            Some("/tmp/test.jsonl")
        );
    }

    #[test]
    fn parse_baseline_info_flag() {
        let cli = Cli::parse_from(["bvr", "--baseline-info"]);
        assert!(cli.baseline_info);
        // baseline_info doesn't need issues loaded, so it's not a robot command
        assert!(!cli.is_robot_command());
    }

    #[test]
    fn parse_check_drift_flag() {
        let cli = Cli::parse_from(["bvr", "--check-drift"]);
        assert!(cli.check_drift);
        assert!(cli.is_robot_command());
    }

    #[test]
    fn parse_related_include_closed_flag() {
        let cli = Cli::parse_from(["bvr", "--robot-related", "bd-1", "--related-include-closed"]);
        assert!(cli.related_include_closed);
    }

    #[test]
    fn robot_orient_is_alias_for_robot_overview() {
        let overview = Cli::parse_from(["bvr", "--robot-overview"]);
        let orient = Cli::parse_from(["bvr", "--robot-orient"]);
        assert!(overview.robot_overview);
        assert!(orient.robot_overview);
    }
}
