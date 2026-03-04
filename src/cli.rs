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
    pub export_md: Option<PathBuf>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub no_hooks: bool,

    #[arg(long)]
    pub as_of: Option<String>,

    #[arg(long, action = ArgAction::SetTrue)]
    pub force_full_analysis: bool,

    #[arg(long, hide = true)]
    pub beads_file: Option<PathBuf>,

    #[arg(long, hide = true)]
    pub repo_path: Option<PathBuf>,
}

impl Cli {
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
    }
}
