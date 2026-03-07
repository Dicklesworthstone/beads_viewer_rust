use std::ffi::OsStr;
use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};

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

#[derive(Debug, Parser)]
#[command(
    name = "bvr",
    about = "Rust port of beads_viewer (bv)",
    disable_help_subcommand = true,
    disable_version_flag = true
)]
pub struct Cli {
    #[arg(short = 'V', long = "version", action = ArgAction::SetTrue)]
    pub version: bool,

    /// Check whether a newer bvr version is available.
    #[arg(long, action = ArgAction::SetTrue)]
    pub check_update: bool,

    /// Update bvr to the latest version.
    #[arg(long, action = ArgAction::SetTrue)]
    pub update: bool,

    /// Roll back the most recent update.
    #[arg(long, action = ArgAction::SetTrue)]
    pub rollback: bool,

    /// Skip update confirmation prompts (legacy compatibility flag).
    #[arg(long, action = ArgAction::SetTrue)]
    pub yes: bool,

    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_help: bool,

    #[arg(long)]
    pub robot_docs: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_schema: bool,

    #[arg(long)]
    pub schema_command: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub stats: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_next: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage_by_track: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_triage_by_label: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_plan: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_insights: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_priority: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    pub robot_alerts: bool,

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

    #[arg(long, default_value_t = 0.0)]
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

    #[arg(long = "min-confidence", default_value_t = 0.0)]
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

    #[arg(long, default_value_t = 10)]
    pub related_limit: usize,

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

    #[arg(long, action = ArgAction::SetTrue)]
    pub no_hooks: bool,

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
    pub fn is_operational_command(&self) -> bool {
        self.check_update || self.update || self.rollback || self.yes
    }

    #[must_use]
    pub fn is_robot_command(&self) -> bool {
        self.robot_help
            || self.robot_next
            || self.robot_triage
            || self.robot_triage_by_track
            || self.robot_triage_by_label
            || self.robot_plan
            || self.robot_insights
            || self.robot_priority
            || self.robot_alerts
            || self.robot_suggest
            || self.robot_diff
            || self.robot_history
            || self.robot_burndown.is_some()
            || self.robot_graph
            || self.robot_forecast.is_some()
            || self.robot_capacity
            || self.bead_history.is_some()
            || self.robot_docs.is_some()
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        Cli, OutputFormat, format_flag_was_explicit_in_args, resolve_output_format_choice,
    };

    #[test]
    fn parse_operational_flags() {
        let cli = Cli::parse_from(["bvr", "--check-update"]);
        assert!(cli.check_update);
        assert!(cli.is_operational_command());

        let cli = Cli::parse_from(["bvr", "--update", "--yes"]);
        assert!(cli.update);
        assert!(cli.yes);
        assert!(cli.is_operational_command());

        let cli = Cli::parse_from(["bvr", "--rollback"]);
        assert!(cli.rollback);
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
}
