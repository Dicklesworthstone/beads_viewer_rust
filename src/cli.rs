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
    }
}
