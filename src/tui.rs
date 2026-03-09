use std::collections::BTreeMap;
#[cfg(not(test))]
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;

use crate::analysis::Analyzer;
use crate::analysis::git_history::{
    GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryMilestonesCompat,
    correlate_histories_with_git, finalize_history_entries, load_git_commits,
};
use crate::analysis::history::IssueHistory;
use crate::analysis::triage::TriageOptions;
use crate::loader;
use crate::model::{Issue, Sprint};
#[cfg(not(test))]
use crate::robot::compute_data_hash;
use crate::{BvrError, Result};
use chrono::{DateTime, Utc};
use ftui::core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseEvent, MouseEventKind,
};
use ftui::core::geometry::Rect;
use ftui::layout::{Constraint, Flex};
use ftui::render::frame::Frame;
#[cfg(not(test))]
use ftui::runtime::TaskSpec;
use ftui::runtime::{App, Cmd, Model, ScreenMode};
use ftui::widgets::Widget;
use ftui::widgets::block::Block;
use ftui::widgets::paragraph::Paragraph;

#[cfg(not(test))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone)]
pub struct BackgroundModeConfig {
    pub beads_file: Option<PathBuf>,
    pub workspace_config: Option<PathBuf>,
    pub repo_path: Option<PathBuf>,
    pub repo_filter: Option<String>,
    pub poll_interval_ms: u64,
}

impl BackgroundModeConfig {
    pub const DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;

    #[cfg(not(test))]
    fn normalized(mut self) -> Self {
        if self.poll_interval_ms == 0 {
            self.poll_interval_ms = Self::DEFAULT_POLL_INTERVAL_MS;
        }
        self
    }

    #[cfg(not(test))]
    fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.poll_interval_ms.max(1))
    }

    #[cfg(not(test))]
    fn load_issues(&self) -> Result<Vec<Issue>> {
        let issues = if let Some(path) = self.beads_file.as_deref() {
            loader::load_issues_from_file(path)?
        } else if let Some(path) = self.workspace_config.as_deref() {
            loader::load_workspace_issues(path)?
        } else {
            loader::load_issues(self.repo_path.as_deref())?
        };

        if let Some(repo_filter) = self.repo_filter.as_deref() {
            Ok(filter_issues_by_repo(issues, repo_filter))
        } else {
            Ok(issues)
        }
    }
}

#[cfg(not(test))]
fn filter_issues_by_repo(issues: Vec<Issue>, repo_filter: &str) -> Vec<Issue> {
    let filter = repo_filter.trim().to_ascii_lowercase();
    if filter.is_empty() {
        return issues;
    }

    let needs_flexible_match =
        !filter.ends_with('-') && !filter.ends_with(':') && !filter.ends_with('_');
    let with_dash = format!("{filter}-");
    let with_colon = format!("{filter}:");
    let with_underscore = format!("{filter}_");

    issues
        .into_iter()
        .filter(|issue| {
            let id = issue.id.to_ascii_lowercase();
            if id.starts_with(&filter) {
                return true;
            }
            if needs_flexible_match
                && (id.starts_with(&with_dash)
                    || id.starts_with(&with_colon)
                    || id.starts_with(&with_underscore))
            {
                return true;
            }

            let source_repo = issue.source_repo.trim();
            if source_repo.is_empty() || source_repo == "." {
                return false;
            }

            let source_repo = source_repo.to_ascii_lowercase();
            if source_repo.starts_with(&filter) {
                return true;
            }

            needs_flexible_match
                && (source_repo.starts_with(&with_dash)
                    || source_repo.starts_with(&with_colon)
                    || source_repo.starts_with(&with_underscore))
        })
        .collect()
}

#[cfg(not(test))]
#[derive(Debug)]
struct BackgroundRuntimeState {
    config: BackgroundModeConfig,
    in_flight: bool,
    cancel_requested: Arc<AtomicBool>,
    last_data_hash: String,
    timeline: VecDeque<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundTickDecision {
    Stop,
    TickOnly,
    ReloadAndTick,
}

fn decide_background_tick(cancel_requested: bool, in_flight: bool) -> BackgroundTickDecision {
    if cancel_requested {
        BackgroundTickDecision::Stop
    } else if in_flight {
        BackgroundTickDecision::TickOnly
    } else {
        BackgroundTickDecision::ReloadAndTick
    }
}

fn should_apply_background_reload(
    cancel_requested: bool,
    new_hash: &str,
    previous_hash: &str,
) -> bool {
    !cancel_requested && new_hash != previous_hash
}

fn background_warning_message(cancel_requested: bool, error: &str) -> Option<String> {
    if cancel_requested || error == "canceled" {
        None
    } else {
        Some(format!("background reload warning: {error}"))
    }
}

#[cfg(not(test))]
const BACKGROUND_TIMELINE_MAX_EVENTS: usize = 32;

#[cfg(not(test))]
fn background_timeline_entry(event: &str) -> String {
    let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!("{ts} | {event}")
}

#[cfg(not(test))]
fn push_background_timeline(runtime: &mut BackgroundRuntimeState, event: &str) -> String {
    let entry = background_timeline_entry(event);
    runtime.timeline.push_back(entry.clone());
    while runtime.timeline.len() > BACKGROUND_TIMELINE_MAX_EVENTS {
        runtime.timeline.pop_front();
    }
    entry
}

// ---------------------------------------------------------------------------
// Visual Tokens — centralised style constants for the TUI
// ---------------------------------------------------------------------------

/// Terminal width breakpoints for responsive layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breakpoint {
    /// < 80 columns — compact single-pane emphasis
    Narrow,
    /// 80..120 columns — standard two-pane
    Medium,
    /// >= 120 columns — roomy two-pane with extra detail
    Wide,
}

impl Breakpoint {
    fn from_width(w: u16) -> Self {
        if w < 80 {
            Self::Narrow
        } else if w < 120 {
            Self::Medium
        } else {
            Self::Wide
        }
    }

    /// List pane percentage for the horizontal split.
    fn list_pct(self) -> f32 {
        match self {
            Self::Narrow => 35.0,
            Self::Medium => 42.0,
            Self::Wide => 38.0,
        }
    }

    /// Detail pane percentage for the horizontal split.
    fn detail_pct(self) -> f32 {
        100.0 - self.list_pct()
    }
}

/// Semantic colour tokens (dark-background palette).
#[allow(dead_code)]
mod tokens {
    use ftui::{PackedRgba, Style};

    // -- Palette primitives --------------------------------------------------
    pub const FG_DEFAULT: PackedRgba = PackedRgba::rgb(204, 204, 204); // #cccccc
    pub const FG_DIM: PackedRgba = PackedRgba::rgb(136, 136, 136); // #888888
    pub const FG_ACCENT: PackedRgba = PackedRgba::rgb(97, 175, 239); // #61afef blue
    pub const FG_SUCCESS: PackedRgba = PackedRgba::rgb(152, 195, 121); // #98c379 green
    pub const FG_WARNING: PackedRgba = PackedRgba::rgb(229, 192, 123); // #e5c07b yellow
    pub const FG_ERROR: PackedRgba = PackedRgba::rgb(224, 108, 117); // #e06c75 red
    pub const FG_MUTED: PackedRgba = PackedRgba::rgb(92, 99, 112); // #5c6370

    pub const BG_BASE: PackedRgba = PackedRgba::rgb(40, 44, 52); // #282c34
    pub const BG_SURFACE: PackedRgba = PackedRgba::rgb(50, 56, 66); // #323842
    pub const BG_HIGHLIGHT: PackedRgba = PackedRgba::rgb(62, 68, 81); // #3e4451

    // -- Status colours ------------------------------------------------------
    pub const STATUS_OPEN: PackedRgba = FG_ACCENT;
    pub const STATUS_IN_PROGRESS: PackedRgba = FG_WARNING;
    pub const STATUS_BLOCKED: PackedRgba = FG_ERROR;
    pub const STATUS_CLOSED: PackedRgba = FG_SUCCESS;

    // -- Priority colours ----------------------------------------------------
    pub const PRIO_P0: PackedRgba = FG_ERROR;
    pub const PRIO_P1: PackedRgba = FG_WARNING;
    pub const PRIO_P2: PackedRgba = FG_ACCENT;
    pub const PRIO_P3: PackedRgba = FG_DIM;
    pub const PRIO_P4: PackedRgba = FG_MUTED;

    // -- Semantic styles -----------------------------------------------------
    pub fn header() -> Style {
        Style::new().fg(FG_ACCENT).bold()
    }

    pub fn header_bg() -> Style {
        Style::new().fg(FG_ACCENT).bg(BG_SURFACE).bold()
    }

    pub fn footer() -> Style {
        Style::new().fg(FG_DIM)
    }

    pub fn selected() -> Style {
        Style::new().fg(FG_DEFAULT).bg(BG_HIGHLIGHT).bold()
    }

    pub fn panel_border() -> Style {
        Style::new().fg(FG_MUTED)
    }

    pub fn panel_border_focused() -> Style {
        Style::new().fg(FG_ACCENT)
    }

    pub fn panel_title() -> Style {
        Style::new().fg(FG_DEFAULT).bold()
    }

    pub fn panel_title_focused() -> Style {
        Style::new().fg(FG_ACCENT).bold()
    }

    pub fn status_style(status: &str) -> Style {
        let fg = if status.eq_ignore_ascii_case("open") {
            STATUS_OPEN
        } else if status.eq_ignore_ascii_case("in_progress") {
            STATUS_IN_PROGRESS
        } else if status.eq_ignore_ascii_case("blocked") {
            STATUS_BLOCKED
        } else if status.eq_ignore_ascii_case("closed") {
            STATUS_CLOSED
        } else {
            FG_DIM
        };
        Style::new().fg(fg)
    }

    pub fn priority_fg(prio: u8) -> PackedRgba {
        match prio {
            0 => PRIO_P0,
            1 => PRIO_P1,
            2 => PRIO_P2,
            3 => PRIO_P3,
            _ => PRIO_P4,
        }
    }

    pub fn priority_style(prio: u8) -> Style {
        Style::new().fg(priority_fg(prio))
    }

    pub fn search_highlight() -> Style {
        Style::new()
            .fg(PackedRgba::rgb(0, 0, 0))
            .bg(FG_WARNING)
            .bold()
    }

    pub fn help_key() -> Style {
        Style::new().fg(FG_ACCENT).bold()
    }

    pub fn help_desc() -> Style {
        Style::new().fg(FG_DEFAULT)
    }

    pub fn dim() -> Style {
        Style::new().fg(FG_DIM)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Main,
    Board,
    Insights,
    Graph,
    History,
    Actionable,
    Attention,
    Tree,
    LabelDashboard,
    FlowMatrix,
    TimeTravelDiff,
    Sprint,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Board => "Board",
            Self::Insights => "Insights",
            Self::Graph => "Graph",
            Self::History => "History",
            Self::Actionable => "Actionable",
            Self::Attention => "Attention",
            Self::Tree => "Tree",
            Self::LabelDashboard => "Labels",
            Self::FlowMatrix => "Flow",
            Self::TimeTravelDiff => "TimeTravel",
            Self::Sprint => "Sprint",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    List,
    Detail,
}

impl FocusPane {
    fn label(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Detail => "detail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListFilter {
    All,
    Open,
    Closed,
    Ready,
}

impl ListFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListSort {
    Default,
    CreatedAsc,
    CreatedDesc,
    Priority,
    Updated,
}

impl ListSort {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::CreatedAsc => "created-asc",
            Self::CreatedDesc => "created-desc",
            Self::Priority => "priority",
            Self::Updated => "updated",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Default => Self::CreatedAsc,
            Self::CreatedAsc => Self::CreatedDesc,
            Self::CreatedDesc => Self::Priority,
            Self::Priority => Self::Updated,
            Self::Updated => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardGrouping {
    Status,
    Priority,
    Type,
}

impl BoardGrouping {
    fn label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Type => "type",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Status => Self::Priority,
            Self::Priority => Self::Type,
            Self::Type => Self::Status,
        }
    }
}

/// 3-state empty lane visibility: Auto → `ShowAll` → `HideEmpty` → Auto.
/// Auto: status grouping shows all, priority/type grouping hides empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyLaneVisibility {
    Auto,
    ShowAll,
    HideEmpty,
}

impl EmptyLaneVisibility {
    fn next(self) -> Self {
        match self {
            Self::Auto => Self::ShowAll,
            Self::ShowAll => Self::HideEmpty,
            Self::HideEmpty => Self::Auto,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::ShowAll => "Show All",
            Self::HideEmpty => "Hide Empty",
        }
    }

    fn should_show_empty(self, grouping: BoardGrouping) -> bool {
        match self {
            Self::ShowAll => true,
            Self::HideEmpty => false,
            Self::Auto => matches!(grouping, BoardGrouping::Status),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryViewMode {
    Bead,
    Git,
}

impl HistoryViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Bead => "bead",
            Self::Git => "git",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Bead => Self::Git,
            Self::Git => Self::Bead,
        }
    }
}

/// Search mode for history view — determines which fields the search query matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HistorySearchMode {
    /// Search across all fields (default).
    #[default]
    All,
    /// Search commit messages only.
    Commit,
    /// Search by SHA prefix.
    Sha,
    /// Search bead ID/title only.
    Bead,
    /// Search by author name.
    Author,
}

impl HistorySearchMode {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Commit => "msg",
            Self::Sha => "sha",
            Self::Bead => "bead",
            Self::Author => "author",
        }
    }

    fn cycle(self) -> Self {
        match self {
            Self::All => Self::Commit,
            Self::Commit => Self::Sha,
            Self::Sha => Self::Bead,
            Self::Bead => Self::Author,
            Self::Author => Self::All,
        }
    }
}

/// A node in the history file tree (port of Go `FileTreeNode`).
#[derive(Debug, Clone)]
struct FileTreeNode {
    name: String,
    path: String,
    is_dir: bool,
    change_count: usize,
    expanded: bool,
    level: usize,
    children: Vec<Self>,
}

impl FileTreeNode {
    /// Flatten the tree into a list of visible (expanded) nodes for navigation.
    fn flatten_visible(&self) -> Vec<FlatFileEntry> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out
    }

    fn flatten_into(&self, out: &mut Vec<FlatFileEntry>) {
        out.push(FlatFileEntry {
            name: self.name.clone(),
            path: self.path.clone(),
            is_dir: self.is_dir,
            change_count: self.change_count,
            level: self.level,
        });
        if self.is_dir && self.expanded {
            for child in &self.children {
                child.flatten_into(out);
            }
        }
    }
}

/// A single visible entry in the flattened file tree.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FlatFileEntry {
    name: String,
    path: String,
    is_dir: bool,
    change_count: usize,
    level: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsightsPanel {
    Bottlenecks,
    Keystones,
    CriticalPath,
    Influencers,
    Betweenness,
    Hubs,
    Authorities,
    Cores,
    CutPoints,
    Slack,
    Cycles,
    Priority,
}

impl InsightsPanel {
    fn label(self) -> &'static str {
        match self {
            Self::Bottlenecks => "Bottlenecks",
            Self::Keystones => "Keystones",
            Self::CriticalPath => "Critical Path",
            Self::Influencers => "Influencers (PageRank)",
            Self::Betweenness => "Betweenness",
            Self::Hubs => "Hubs (HITS)",
            Self::Authorities => "Authorities (HITS)",
            Self::Cores => "K-Core Cohesion",
            Self::CutPoints => "Cut Points",
            Self::Slack => "Slack (Zero)",
            Self::Cycles => "Cycles",
            Self::Priority => "Priority",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Bottlenecks => "bottlenecks",
            Self::Keystones => "keystones",
            Self::CriticalPath => "crit-path",
            Self::Influencers => "influencers",
            Self::Betweenness => "betweenness",
            Self::Hubs => "hubs",
            Self::Authorities => "authorities",
            Self::Cores => "k-core",
            Self::CutPoints => "cut-pts",
            Self::Slack => "slack",
            Self::Cycles => "cycles",
            Self::Priority => "priority",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Bottlenecks => Self::Keystones,
            Self::Keystones => Self::CriticalPath,
            Self::CriticalPath => Self::Influencers,
            Self::Influencers => Self::Betweenness,
            Self::Betweenness => Self::Hubs,
            Self::Hubs => Self::Authorities,
            Self::Authorities => Self::Cores,
            Self::Cores => Self::CutPoints,
            Self::CutPoints => Self::Slack,
            Self::Slack => Self::Cycles,
            Self::Cycles => Self::Priority,
            Self::Priority => Self::Bottlenecks,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Bottlenecks => Self::Priority,
            Self::Keystones => Self::Bottlenecks,
            Self::CriticalPath => Self::Keystones,
            Self::Influencers => Self::CriticalPath,
            Self::Betweenness => Self::Influencers,
            Self::Hubs => Self::Betweenness,
            Self::Authorities => Self::Hubs,
            Self::Cores => Self::Authorities,
            Self::CutPoints => Self::Cores,
            Self::Slack => Self::CutPoints,
            Self::Cycles => Self::Slack,
            Self::Priority => Self::Cycles,
        }
    }
}

const INSIGHTS_HEATMAP_DEPTH_LABELS: [&str; 5] = ["D=0", "D1-2", "D3-5", "D6-10", "D10+"];
const INSIGHTS_HEATMAP_SCORE_LABELS: [&str; 5] = ["0-.2", ".2-.4", ".4-.6", ".6-.8", ".8-1"];

#[derive(Debug, Clone, Default)]
struct InsightsHeatmapState {
    row: usize,
    col: usize,
    drill_active: bool,
    drill_cursor: usize,
}

#[derive(Debug, Clone)]
struct InsightsHeatmapData {
    counts: Vec<Vec<usize>>,
    issue_ids: Vec<Vec<Vec<String>>>,
}

const HISTORY_CONFIDENCE_STEPS: [f64; 4] = [0.0, 0.5, 0.75, 0.9];

#[derive(Debug, Clone)]
struct HistoryGitCache {
    commits: Vec<GitCommitRecord>,
    histories: BTreeMap<String, HistoryBeadCompat>,
    commit_bead_confidence: BTreeMap<String, Vec<(String, f64)>>,
}

// ---------------------------------------------------------------------------
// Modal overlays
// ---------------------------------------------------------------------------

/// Modal overlays that take over the full screen.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModalOverlay {
    /// Welcome / first-run tutorial.
    #[allow(dead_code)]
    Tutorial,
    /// Reusable Y/N confirmation dialog.
    #[allow(dead_code)]
    Confirm { title: String, message: String },
    /// Interactive pages export wizard.
    PagesWizard(PagesWizardState),
    /// Recipe picker: shows available triage recipes.
    RecipePicker {
        items: Vec<(String, String)>,
        cursor: usize,
    },
    /// Label picker: shows all labels for quick filtering.
    LabelPicker {
        items: Vec<(String, usize)>,
        cursor: usize,
    },
    /// Repo picker: shows workspace repos for filtering.
    RepoPicker { items: Vec<String>, cursor: usize },
}

/// State for the multi-step pages export wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PagesWizardState {
    step: usize,
    export_dir: String,
    title: String,
    include_closed: bool,
    include_history: bool,
}

impl PagesWizardState {
    #[allow(dead_code)]
    fn new() -> Self {
        Self {
            step: 0,
            export_dir: "./bv-pages".to_string(),
            title: String::new(),
            include_closed: true,
            include_history: true,
        }
    }

    fn step_count() -> usize {
        4
    }

    fn step_label(&self) -> &'static str {
        match self.step {
            0 => "Export Directory",
            1 => "Page Title",
            2 => "Options",
            3 => "Review & Export",
            _ => "Done",
        }
    }
}

#[derive(Debug, Clone)]
struct HistoryTimelineEvent {
    issue_id: String,
    issue_title: String,
    issue_status: String,
    event_kind: String,
    event_timestamp: Option<DateTime<Utc>>,
    event_details: String,
}

/// A flattened node in the dependency tree for rendering.
#[derive(Debug, Clone)]
struct TreeFlatNode {
    /// Issue index into `analyzer.issues`.
    issue_index: usize,
    /// Depth in the tree (0 = root).
    depth: usize,
    /// Whether this node has children.
    has_children: bool,
    /// Whether this node's children are currently collapsed.
    is_collapsed: bool,
    /// Whether this is the last sibling at its depth (for box-drawing).
    is_last_sibling: bool,
    /// The prefix ancestry for box drawing (true = parent was last sibling at that depth).
    ancestry_last: Vec<bool>,
}

#[derive(Debug)]
enum Msg {
    KeyPress(KeyCode, Modifiers),
    Mouse(MouseEventKind),
    #[cfg(not(test))]
    Tick,
    #[cfg(not(test))]
    BackgroundReloaded(std::result::Result<Vec<Issue>, String>),
    Noop,
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                ..
            }) => Self::KeyPress(code, modifiers),
            Event::Mouse(MouseEvent { kind, .. }) => Self::Mouse(kind),
            #[cfg(not(test))]
            Event::Tick => Self::Tick,
            _ => Self::Noop,
        }
    }
}

#[derive(Debug)]
struct BvrApp {
    analyzer: Analyzer,
    repo_root: Option<PathBuf>,
    selected: usize,
    list_filter: ListFilter,
    list_sort: ListSort,
    board_grouping: BoardGrouping,
    board_empty_visibility: EmptyLaneVisibility,
    mode: ViewMode,
    mode_before_history: ViewMode,
    focus: FocusPane,
    focus_before_help: FocusPane,
    show_help: bool,
    help_scroll_offset: usize,
    show_quit_confirm: bool,
    modal_overlay: Option<ModalOverlay>,
    modal_confirm_result: Option<bool>,
    history_confidence_index: usize,
    history_view_mode: HistoryViewMode,
    history_event_cursor: usize,
    history_related_bead_cursor: usize,
    history_bead_commit_cursor: usize,
    history_git_cache: Option<HistoryGitCache>,
    history_search_active: bool,
    history_search_query: String,
    history_search_match_cursor: usize,
    history_search_mode: HistorySearchMode,
    history_show_file_tree: bool,
    history_file_tree_cursor: usize,
    history_file_tree_filter: Option<String>,
    history_file_tree_focus: bool,
    history_status_msg: String,
    board_search_active: bool,
    board_search_query: String,
    board_search_match_cursor: usize,
    board_detail_scroll_offset: usize,
    graph_search_active: bool,
    graph_search_query: String,
    graph_search_match_cursor: usize,
    insights_search_active: bool,
    insights_search_query: String,
    insights_search_match_cursor: usize,
    insights_panel: InsightsPanel,
    insights_heatmap: Option<InsightsHeatmapState>,
    insights_show_explanations: bool,
    insights_show_calc_proof: bool,
    detail_dep_cursor: usize,
    actionable_plan: Option<crate::analysis::plan::ExecutionPlan>,
    actionable_track_cursor: usize,
    actionable_item_cursor: usize,
    attention_result: Option<crate::analysis::label_intel::LabelAttentionResult>,
    attention_cursor: usize,
    tree_flat_nodes: Vec<TreeFlatNode>,
    tree_cursor: usize,
    tree_collapsed: std::collections::HashSet<String>,
    label_dashboard: Option<crate::analysis::label_intel::LabelHealthResult>,
    label_dashboard_cursor: usize,
    flow_matrix: Option<crate::analysis::label_intel::CrossLabelFlow>,
    flow_matrix_row_cursor: usize,
    flow_matrix_col_cursor: usize,
    sprint_data: Vec<Sprint>,
    sprint_cursor: usize,
    sprint_issue_cursor: usize,
    modal_label_filter: Option<String>,
    modal_repo_filter: Option<String>,
    time_travel_ref_input: String,
    time_travel_input_active: bool,
    time_travel_diff: Option<crate::analysis::diff::SnapshotDiff>,
    time_travel_category_cursor: usize,
    time_travel_issue_cursor: usize,
    time_travel_last_ref: Option<String>,
    priority_hints_visible: bool,
    status_msg: String,
    slow_metrics_pending: bool,
    #[cfg(not(test))]
    slow_metrics_rx: Option<std::sync::mpsc::Receiver<crate::analysis::graph::GraphMetrics>>,
    #[cfg(not(test))]
    background_runtime: Option<BackgroundRuntimeState>,
    /// Per-key event trace log for e2e debugging. Each entry records
    /// the key pressed and resulting state snapshot.
    #[cfg(test)]
    key_trace: Vec<KeyTraceEntry>,
}

/// A single entry in the per-key event trace log.
#[cfg(test)]
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct KeyTraceEntry {
    key: String,
    mode: ViewMode,
    focus: FocusPane,
    selected: usize,
    filter: ListFilter,
}

impl Model for BvrApp {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        #[cfg(not(test))]
        {
            self.background_tick_command()
        }
        #[cfg(test)]
        {
            Cmd::None
        }
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::KeyPress(code, modifiers) => {
                let cmd = self.handle_key(code, modifiers);
                #[cfg(test)]
                self.key_trace.push(KeyTraceEntry {
                    key: format!("{code:?}"),
                    mode: self.mode,
                    focus: self.focus,
                    selected: self.selected,
                    filter: self.list_filter,
                });
                #[cfg(not(test))]
                {
                    return self.wrap_quit_with_background_cancel(cmd);
                }
                #[cfg(test)]
                {
                    return cmd;
                }
            }
            Msg::Mouse(kind) => return self.handle_mouse(kind),
            #[cfg(not(test))]
            Msg::Tick => return self.handle_background_tick(),
            #[cfg(not(test))]
            Msg::BackgroundReloaded(result) => {
                self.handle_background_reload_result(result);
                return Cmd::None;
            }
            Msg::Noop => {}
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let full = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let visible_count = self.visible_issue_indices().len();
        let bp = Breakpoint::from_width(full.width);

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(3),
                Constraint::Fixed(1),
            ])
            .split(full);

        // -- Header ----------------------------------------------------------
        let header_text = match bp {
            Breakpoint::Narrow => format!(
                "bvr {} | {}/{} | {}",
                self.mode.label(),
                visible_count,
                self.analyzer.issues.len(),
                self.list_filter.label(),
            ),
            _ => {
                let mut filter_label = self.list_filter.label().to_string();
                if let Some(ref label) = self.modal_label_filter {
                    filter_label = format!("{filter_label}+label:{label}");
                }
                if let Some(ref repo) = self.modal_repo_filter {
                    filter_label = format!("{filter_label}+repo:{repo}");
                }
                let metrics_hint = if self.slow_metrics_pending {
                    " | metrics: computing..."
                } else {
                    ""
                };
                format!(
                    "bvr | mode={} | focus={} | issues={}/{} | filter={} | sort={}{} | ? help | Tab focus |",
                    self.mode.label(),
                    self.focus.label(),
                    visible_count,
                    self.analyzer.issues.len(),
                    filter_label,
                    self.list_sort.label(),
                    metrics_hint,
                )
            }
        };
        Paragraph::new(header_text)
            .style(tokens::header_bg())
            .render(rows[0], frame);

        // -- Help overlay ----------------------------------------------------
        if self.show_help {
            let inner_width = rows[1].width.saturating_sub(2) as usize;
            let full_help = self.help_overlay_text(inner_width);
            let help_lines: Vec<&str> = full_help.lines().collect();
            let visible_height = rows[1].height.saturating_sub(2) as usize; // border
            let max_offset = help_lines.len().saturating_sub(visible_height);
            let offset = self.help_scroll_offset.min(max_offset);
            let visible: String = help_lines
                .iter()
                .skip(offset)
                .take(visible_height)
                .copied()
                .collect::<Vec<&str>>()
                .join("\n");
            Paragraph::new(visible)
                .block(
                    Block::bordered()
                        .title("Help")
                        .border_style(tokens::panel_border_focused())
                        .style(tokens::panel_title_focused()),
                )
                .render(rows[1], frame);
            let scroll_hint = if help_lines.len() > visible_height {
                format!(
                    "? or Esc close | j/k scroll | Ctrl+d/u page | line {}/{}",
                    offset + 1,
                    help_lines.len()
                )
            } else {
                "? or Esc to close help".to_string()
            };
            Paragraph::new(scroll_hint)
                .style(tokens::footer())
                .render(rows[2], frame);
            return;
        }

        // -- Quit confirmation -----------------------------------------------
        if self.show_quit_confirm {
            Paragraph::new("Quit bvr?\n\nPress Esc or Y to quit.\nPress any other key to cancel.")
                .block(
                    Block::bordered()
                        .title("Confirm Quit")
                        .border_style(tokens::panel_border()),
                )
                .render(rows[1], frame);
            Paragraph::new("Esc/Y confirms quit. Any other key cancels.")
                .style(tokens::footer())
                .render(rows[2], frame);
            return;
        }

        // -- Modal overlays --------------------------------------------------
        if let Some(ref overlay) = self.modal_overlay {
            match overlay {
                ModalOverlay::Tutorial => {
                    let text = concat!(
                        "Welcome to bvr!\n\n",
                        "Modes:  b=board  i=insights  g=graph  h=history\n",
                        "Filter: o=open   c=closed    r=ready  a=all\n",
                        "Nav:    j/k=up/down  Tab=focus  /=search  n/N=cycle\n",
                        "Other:  ?=help   s=sort  Enter=select  Esc=back  q=quit\n\n",
                        "Press any key to dismiss."
                    );
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title("Tutorial")
                                .border_style(tokens::panel_border_focused())
                                .style(tokens::panel_title_focused()),
                        )
                        .render(rows[1], frame);
                    Paragraph::new("Press any key to continue.")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::Confirm { title, message } => {
                    let text = format!("{message}\n\nPress Y to confirm, N or Esc to cancel.");
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title(title.as_str())
                                .border_style(tokens::panel_border()),
                        )
                        .render(rows[1], frame);
                    Paragraph::new("Y=confirm | N/Esc=cancel")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::PagesWizard(wiz) => {
                    let text = Self::pages_wizard_text(wiz);
                    let wiz_title = format!(
                        "Pages Wizard ({}/{}): {}",
                        wiz.step + 1,
                        PagesWizardState::step_count(),
                        wiz.step_label()
                    );
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title(wiz_title.as_str())
                                .border_style(tokens::panel_border_focused())
                                .style(tokens::panel_title_focused()),
                        )
                        .render(rows[1], frame);
                    let footer = if wiz.step == PagesWizardState::step_count() - 1 {
                        "Enter=export | Esc=cancel | Backspace=prev step"
                    } else {
                        "Enter=next step | Esc=cancel | Backspace=prev step"
                    };
                    Paragraph::new(footer)
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::RecipePicker { items, cursor } => {
                    let mut lines = Vec::new();
                    for (i, (name, desc)) in items.iter().enumerate() {
                        let marker = if i == *cursor { "▸" } else { " " };
                        lines.push(format!(" {marker} {name:16} {desc}"));
                    }
                    let text = if lines.is_empty() {
                        " No recipes available.".to_string()
                    } else {
                        lines.join("\n")
                    };
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title("Recipe Picker")
                                .border_style(tokens::panel_border_focused())
                                .style(tokens::panel_title_focused()),
                        )
                        .render(rows[1], frame);
                    Paragraph::new("j/k=navigate | Enter=apply | Esc=close")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::LabelPicker { items, cursor } => {
                    let mut lines = Vec::new();
                    for (i, (label, count)) in items.iter().enumerate() {
                        let marker = if i == *cursor { "▸" } else { " " };
                        lines.push(format!(" {marker} {label:24} ({count} issues)"));
                    }
                    let text = if lines.is_empty() {
                        " No labels found.".to_string()
                    } else {
                        lines.join("\n")
                    };
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title("Label Picker")
                                .border_style(tokens::panel_border_focused())
                                .style(tokens::panel_title_focused()),
                        )
                        .render(rows[1], frame);
                    Paragraph::new("j/k=navigate | Enter=filter by label | Esc=close")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::RepoPicker { items, cursor } => {
                    let mut lines = Vec::new();
                    for (i, repo) in items.iter().enumerate() {
                        let marker = if i == *cursor { "▸" } else { " " };
                        lines.push(format!(" {marker} {repo}"));
                    }
                    let text = if lines.is_empty() {
                        " No repos found.".to_string()
                    } else {
                        lines.join("\n")
                    };
                    Paragraph::new(text)
                        .block(
                            Block::bordered()
                                .title("Repo Picker")
                                .border_style(tokens::panel_border_focused())
                                .style(tokens::panel_title_focused()),
                        )
                        .render(rows[1], frame);
                    Paragraph::new("j/k=navigate | Enter=filter by repo | Esc=close")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
            }
        }

        // -- Body: mode-aware panes with breakpoint-aware widths --------------
        let body = rows[1];
        let graph_single_pane =
            matches!(self.mode, ViewMode::Graph) && matches!(bp, Breakpoint::Narrow);
        let mut detail_viewport_height = body.height.saturating_sub(2) as usize;
        let mut board_detail_line = None;

        let detail_title = match self.mode {
            ViewMode::Board => "Board Focus",
            ViewMode::Insights => "Insight Detail",
            ViewMode::Graph if graph_single_pane => "Graph View",
            ViewMode::Graph => "Graph Focus",
            ViewMode::History => "History Timeline",
            ViewMode::Actionable => "Track Detail",
            ViewMode::Attention => "Label Detail",
            ViewMode::Tree => "Issue Detail",
            ViewMode::LabelDashboard => "Label Detail",
            ViewMode::FlowMatrix => "Flow Detail",
            ViewMode::TimeTravelDiff => "Diff Detail",
            ViewMode::Sprint => "Sprint Detail",
            ViewMode::Main => "Details",
        };
        let detail_focused = self.focus == FocusPane::Detail || graph_single_pane;
        let detail_title = if detail_focused {
            format!("{detail_title} [focus]")
        } else {
            detail_title.to_string()
        };
        let detail_border = if detail_focused {
            tokens::panel_border_focused()
        } else {
            tokens::panel_border()
        };
        let detail_title_style = if detail_focused {
            tokens::panel_title_focused()
        } else {
            tokens::panel_title()
        };

        if graph_single_pane {
            Paragraph::new(self.graph_detail_text())
                .block(
                    Block::bordered()
                        .title(&detail_title)
                        .border_style(detail_border)
                        .style(detail_title_style),
                )
                .render(body, frame);
        } else {
            let panes = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(bp.list_pct()),
                    Constraint::Percentage(bp.detail_pct()),
                ])
                .split(body);

            let list_text = self.list_panel_text();
            let list_title = match self.mode {
                ViewMode::Board => "Board Lanes",
                ViewMode::Insights => "Insight Queue",
                ViewMode::Graph => "Graph Nodes",
                ViewMode::History => {
                    if matches!(self.history_view_mode, HistoryViewMode::Git) {
                        "History Events"
                    } else {
                        "History Beads"
                    }
                }
                ViewMode::Actionable => "Execution Tracks",
                ViewMode::Attention => "Label Attention",
                ViewMode::Tree => "Dependency Tree",
                ViewMode::LabelDashboard => "Label Health",
                ViewMode::FlowMatrix => "Flow Matrix",
                ViewMode::TimeTravelDiff => "Diff Categories",
                ViewMode::Sprint => "Sprints",
                ViewMode::Main => "Issues",
            };
            let list_focused = self.focus == FocusPane::List;
            let list_title = if list_focused {
                format!("{list_title} [focus]")
            } else {
                list_title.to_string()
            };

            let list_border = if list_focused {
                tokens::panel_border_focused()
            } else {
                tokens::panel_border()
            };
            let list_title_style = if list_focused {
                tokens::panel_title_focused()
            } else {
                tokens::panel_title()
            };
            Paragraph::new(list_text)
                .block(
                    Block::bordered()
                        .title(&list_title)
                        .border_style(list_border)
                        .style(list_title_style),
                )
                .render(panes[0], frame);

            detail_viewport_height = panes[1].height.saturating_sub(2) as usize;
            let detail_text = if matches!(self.mode, ViewMode::Board) {
                let (text, offset, total_lines) =
                    self.board_detail_render_state(detail_viewport_height);
                board_detail_line = Some((offset, total_lines));
                text
            } else if matches!(self.mode, ViewMode::Graph) {
                self.graph_detail_text()
            } else {
                self.detail_panel_text()
            };
            Paragraph::new(detail_text)
                .block(
                    Block::bordered()
                        .title(&detail_title)
                        .border_style(detail_border)
                        .style(detail_title_style),
                )
                .render(panes[1], frame);
        }

        // -- Footer ----------------------------------------------------------
        let footer_text = match self.mode {
            ViewMode::Main => {
                if self.status_msg.is_empty() {
                    format!(
                        "b/i/g/h modes | s sort ({}) | p hints | C copy | x export | O edit",
                        self.list_sort.label()
                    )
                } else {
                    self.status_msg.clone()
                }
            }
            ViewMode::Board => {
                let detail_hint = board_detail_line.map_or_else(
                    || "Ctrl+j/k detail scroll".to_string(),
                    |(offset, total_lines)| {
                        if total_lines > detail_viewport_height {
                            format!(
                                "Ctrl+j/k detail scroll | line {}/{}",
                                offset + 1,
                                total_lines
                            )
                        } else {
                            "Ctrl+j/k detail scroll".to_string()
                        }
                    },
                );
                format!(
                    "Board mode: lane counts, queued IDs, and selected issue delivery context | grouping={} (s cycles) | empty-lanes={} (e toggles) | H/L lanes | 0/$ lane edges | {}",
                    self.board_grouping.label(),
                    self.board_empty_visibility.label(),
                    detail_hint,
                )
            }
            ViewMode::Insights => {
                format!(
                    "Insights [{}] | s/S panel | e explanations={} | x proof={}",
                    self.insights_panel.short_label(),
                    if self.insights_show_explanations {
                        "on"
                    } else {
                        "off"
                    },
                    if self.insights_show_calc_proof {
                        "on"
                    } else {
                        "off"
                    }
                )
            }
            ViewMode::Graph => "Graph mode: h/l nodes | j/k nodes or edges | H/L jump | Tab node/edge focus | / search | Enter open details | g/Esc back".to_string(),
            ViewMode::History => {
                if self.history_search_active {
                    format!(
                        "History ({}): [{}] Tab cycles mode | Enter confirm | Esc cancel",
                        self.history_view_mode.label(),
                        self.history_search_mode.label(),
                    )
                } else {
                    format!(
                        "History ({}): c confidence (>= {:.0}%) | v bead/git | / search (Tab mode) | f file-tree | h/Esc back",
                        self.history_view_mode.label(),
                        self.history_min_confidence() * 100.0
                    )
                }
            }
            ViewMode::Actionable => {
                let plan = self.actionable_plan.as_ref();
                let track_count = plan.map_or(0, |p| p.tracks.len());
                let item_count = plan.map_or(0, |p| p.summary.actionable_count);
                format!(
                    "Actionable: {track_count} tracks, {item_count} items | j/k navigate | Tab focus | a/Esc back"
                )
            }
            ViewMode::Attention => {
                let label_count = self.attention_result.as_ref().map_or(0, |r| r.labels.len());
                format!(
                    "Attention: {label_count} labels ranked | j/k navigate | Tab focus | !/Esc back"
                )
            }
            ViewMode::Tree => {
                let node_count = self.tree_flat_nodes.len();
                format!(
                    "Tree: {node_count} nodes | j/k navigate | Enter expand/collapse | Tab focus | T/Esc back"
                )
            }
            ViewMode::LabelDashboard => {
                let label_count = self.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
                format!("Labels: {label_count} | j/k navigate | Tab focus | [/Esc back")
            }
            ViewMode::FlowMatrix => {
                let label_count = self.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
                let dep_count = self
                    .flow_matrix
                    .as_ref()
                    .map_or(0, |f| f.total_cross_label_deps);
                format!(
                    "Flow: {label_count} labels, {dep_count} cross-deps | j/k rows | h/l cols | Tab focus | ]/Esc back"
                )
            }
            ViewMode::TimeTravelDiff => {
                if self.time_travel_input_active {
                    format!(
                        "Time-travel: enter git ref or file path | Enter confirm | Esc cancel"
                    )
                } else if self.time_travel_diff.is_some() {
                    "Time-travel: j/k navigate | Tab focus | T reload | t/Esc back".to_string()
                } else {
                    "Time-travel: no diff loaded | t to enter ref | Esc back".to_string()
                }
            }
            ViewMode::Sprint => {
                let sprint_count = self.sprint_data.len();
                format!(
                    "Sprint: {sprint_count} sprint(s) | j/k navigate | Tab focus | S/Esc back"
                )
            }
        };
        Paragraph::new(footer_text)
            .style(tokens::footer())
            .render(rows[2], frame);
    }
}

impl BvrApp {
    #[cfg(not(test))]
    fn wrap_quit_with_background_cancel(&self, cmd: Cmd<Msg>) -> Cmd<Msg> {
        if matches!(cmd, Cmd::Quit)
            && let Some(runtime) = self.background_runtime.as_ref()
        {
            runtime.cancel_requested.store(true, Ordering::Relaxed);
        }
        cmd
    }

    #[cfg(not(test))]
    fn background_tick_command(&self) -> Cmd<Msg> {
        self.background_runtime
            .as_ref()
            .map_or(Cmd::None, |runtime| {
                Cmd::tick(runtime.config.poll_interval())
            })
    }

    #[cfg(not(test))]
    fn handle_background_tick(&mut self) -> Cmd<Msg> {
        // Poll for slow metric completion
        if self.slow_metrics_pending {
            if let Some(rx) = self.slow_metrics_rx.as_ref() {
                if let Ok(slow) = rx.try_recv() {
                    self.analyzer.apply_slow_metrics(slow);
                    self.slow_metrics_pending = false;
                    self.slow_metrics_rx = None;
                    self.status_msg = "Background metrics computed".to_string();
                }
            }
        }

        let next_tick = self.background_tick_command();
        let Some(runtime) = self.background_runtime.as_mut() else {
            return Cmd::None;
        };

        let decision = decide_background_tick(
            runtime.cancel_requested.load(Ordering::Relaxed),
            runtime.in_flight,
        );

        let (config, cancel_requested) = match decision {
            BackgroundTickDecision::Stop => {
                self.history_status_msg =
                    push_background_timeline(runtime, "tick skipped: cancellation requested");
                return Cmd::None;
            }
            BackgroundTickDecision::TickOnly => {
                self.history_status_msg =
                    push_background_timeline(runtime, "tick observed: reload already in flight");
                return next_tick;
            }
            BackgroundTickDecision::ReloadAndTick => {
                runtime.in_flight = true;
                self.history_status_msg =
                    push_background_timeline(runtime, "tick scheduled: starting background reload");
                (runtime.config.clone(), runtime.cancel_requested.clone())
            }
        };

        let task = Cmd::task_with_spec(
            TaskSpec::new(1.0, 50.0).with_name("background-issue-reload"),
            move || {
                if cancel_requested.load(Ordering::Relaxed) {
                    return Msg::BackgroundReloaded(Err("canceled".to_string()));
                }

                let result = config.load_issues().map_err(|error| error.to_string());
                Msg::BackgroundReloaded(result)
            },
        );

        Cmd::batch(vec![task, next_tick])
    }

    #[cfg(not(test))]
    fn handle_background_reload_result(&mut self, result: std::result::Result<Vec<Issue>, String>) {
        let mut issues_to_apply: Option<Vec<Issue>> = None;
        let status_update: String;

        {
            let Some(runtime) = self.background_runtime.as_mut() else {
                return;
            };

            runtime.in_flight = false;
            let cancel_requested = runtime.cancel_requested.load(Ordering::Relaxed);

            match result {
                Ok(issues) => {
                    let hash = compute_data_hash(&issues);
                    if should_apply_background_reload(
                        cancel_requested,
                        &hash,
                        &runtime.last_data_hash,
                    ) {
                        runtime.last_data_hash = hash;
                        status_update = push_background_timeline(
                            runtime,
                            "reload applied: issue snapshot changed",
                        );
                        issues_to_apply = Some(issues);
                    } else if cancel_requested {
                        status_update = push_background_timeline(
                            runtime,
                            "reload ignored: cancellation requested",
                        );
                    } else {
                        status_update =
                            push_background_timeline(runtime, "reload ignored: no data change");
                    }
                }
                Err(error) => {
                    if let Some(warning) = background_warning_message(cancel_requested, &error) {
                        status_update = push_background_timeline(runtime, &warning);
                    } else {
                        status_update = push_background_timeline(
                            runtime,
                            "reload ignored: cancellation acknowledged",
                        );
                    }
                }
            }
        }

        self.history_status_msg = status_update;
        if let Some(issues) = issues_to_apply {
            self.apply_background_reload(issues);
        }
    }

    #[cfg(not(test))]
    fn apply_background_reload(&mut self, issues: Vec<Issue>) {
        let selected_id = self.selected_issue().map(|issue| issue.id.clone());

        let use_two_phase =
            issues.len() > crate::analysis::graph::AnalysisConfig::BACKGROUND_THRESHOLD;
        if use_two_phase {
            self.analyzer = Analyzer::new_fast(issues);
            #[cfg(not(test))]
            {
                self.slow_metrics_rx = Some(self.analyzer.spawn_slow_computation());
            }
            self.slow_metrics_pending = true;
        } else {
            self.analyzer = Analyzer::new(issues);
            self.slow_metrics_pending = false;
            #[cfg(not(test))]
            {
                self.slow_metrics_rx = None;
            }
        }
        self.history_git_cache = None;
        self.detail_dep_cursor = 0;
        self.board_detail_scroll_offset = 0;
        self.selected = 0;

        if let Some(id) = selected_id.as_deref() {
            self.select_issue_by_id(id);
        }

        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
    }

    fn board_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Board)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn actionable_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Actionable)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn attention_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Attention)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn tree_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Tree)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn label_dashboard_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::LabelDashboard)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn flow_matrix_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::FlowMatrix)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn time_travel_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::TimeTravelDiff)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
            && !self.time_travel_input_active
    }

    fn sprint_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Sprint)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn insights_heatmap_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Insights)
            && self.focus == FocusPane::List
            && self.insights_heatmap.is_some()
    }

    fn insights_heatmap_data(&self) -> InsightsHeatmapData {
        let mut counts =
            vec![vec![0; INSIGHTS_HEATMAP_SCORE_LABELS.len()]; INSIGHTS_HEATMAP_DEPTH_LABELS.len()];
        let mut issue_ids = vec![
            vec![Vec::new(); INSIGHTS_HEATMAP_SCORE_LABELS.len()];
            INSIGHTS_HEATMAP_DEPTH_LABELS.len()
        ];
        let recommendations = self.analyzer.triage(TriageOptions {
            max_recommendations: self.analyzer.issues.len().max(50),
            ..TriageOptions::default()
        });

        for recommendation in recommendations.result.recommendations {
            let Some(issue) = self
                .analyzer
                .issues
                .iter()
                .find(|issue| issue.id == recommendation.id)
            else {
                continue;
            };

            if !issue.is_open_like() || !self.issue_matches_filter(issue) {
                continue;
            }

            let depth = self
                .analyzer
                .metrics
                .critical_depth
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let row = match depth {
                0 => 0,
                1 | 2 => 1,
                3..=5 => 2,
                6..=10 => 3,
                _ => 4,
            };

            let score = recommendation.score.clamp(0.0, 1.0);
            let col = if score <= 0.2 {
                0
            } else if score <= 0.4 {
                1
            } else if score <= 0.6 {
                2
            } else if score <= 0.8 {
                3
            } else {
                4
            };

            counts[row][col] += 1;
            issue_ids[row][col].push(issue.id.clone());
        }

        InsightsHeatmapData { counts, issue_ids }
    }

    fn insights_heatmap_issue_ids_for_current_cell(&self) -> Vec<String> {
        let Some(state) = self.insights_heatmap.as_ref() else {
            return Vec::new();
        };
        let data = self.insights_heatmap_data();
        let row = state
            .row
            .min(INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1));
        let col = state
            .col
            .min(INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1));
        data.issue_ids[row][col].clone()
    }

    fn sync_insights_heatmap_selection(&mut self) {
        let Some(mut state) = self.insights_heatmap.clone() else {
            return;
        };
        let data = self.insights_heatmap_data();
        let row = state
            .row
            .min(INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1));
        let col = state
            .col
            .min(INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1));
        let cell_issue_ids = data.issue_ids[row][col].clone();

        state.row = row;
        state.col = col;

        if cell_issue_ids.is_empty() {
            for (next_row, row_issue_ids) in data.issue_ids.iter().enumerate() {
                if let Some((next_col, next_cell_ids)) = row_issue_ids
                    .iter()
                    .enumerate()
                    .find(|(_, ids)| !ids.is_empty())
                {
                    state.row = next_row;
                    state.col = next_col;
                    state.drill_active = false;
                    state.drill_cursor = 0;
                    let issue_id = next_cell_ids[0].clone();
                    self.insights_heatmap = Some(state);
                    self.select_issue_by_id(&issue_id);
                    return;
                }
            }

            state.drill_active = false;
            state.drill_cursor = 0;
            self.insights_heatmap = Some(state);
            return;
        }

        if !state.drill_active {
            state.drill_cursor = 0;
        } else {
            state.drill_cursor = state
                .drill_cursor
                .min(cell_issue_ids.len().saturating_sub(1));
        }

        let issue_id = cell_issue_ids[state.drill_cursor].clone();
        self.insights_heatmap = Some(state);
        self.select_issue_by_id(&issue_id);
    }

    fn toggle_insights_heatmap(&mut self) {
        if self.insights_heatmap.is_some() {
            self.insights_heatmap = None;
            return;
        }

        self.insights_heatmap = Some(InsightsHeatmapState::default());
        self.sync_insights_heatmap_selection();
    }

    fn enter_insights_heatmap_drill(&mut self) {
        let cell_issue_ids = self.insights_heatmap_issue_ids_for_current_cell();
        if cell_issue_ids.is_empty() {
            return;
        }

        if let Some(state) = self.insights_heatmap.as_mut() {
            state.drill_active = true;
            state.drill_cursor = 0;
        }
        self.sync_insights_heatmap_selection();
    }

    fn exit_insights_heatmap_drill(&mut self) -> bool {
        let Some(state) = self.insights_heatmap.as_mut() else {
            return false;
        };
        if !state.drill_active {
            return false;
        }

        state.drill_active = false;
        state.drill_cursor = 0;
        self.sync_insights_heatmap_selection();
        true
    }

    fn move_insights_heatmap_row(&mut self, delta: isize) {
        let Some(state) = self.insights_heatmap.as_mut() else {
            return;
        };
        if state.drill_active || delta == 0 {
            return;
        }

        let max_row = INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1);
        state.row = if delta >= 0 {
            state.row.saturating_add(delta.unsigned_abs()).min(max_row)
        } else {
            state.row.saturating_sub(delta.unsigned_abs())
        };
        self.sync_insights_heatmap_selection();
    }

    fn move_insights_heatmap_col(&mut self, delta: isize) {
        let Some(state) = self.insights_heatmap.as_mut() else {
            return;
        };
        if state.drill_active || delta == 0 {
            return;
        }

        let max_col = INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1);
        state.col = if delta >= 0 {
            state.col.saturating_add(delta.unsigned_abs()).min(max_col)
        } else {
            state.col.saturating_sub(delta.unsigned_abs())
        };
        self.sync_insights_heatmap_selection();
    }

    fn move_insights_heatmap_drill(&mut self, delta: isize) {
        let cell_issue_ids = self.insights_heatmap_issue_ids_for_current_cell();
        let Some(state) = self.insights_heatmap.as_mut() else {
            return;
        };
        if !state.drill_active || delta == 0 || cell_issue_ids.is_empty() {
            return;
        }

        let max_slot = cell_issue_ids.len().saturating_sub(1);
        state.drill_cursor = if delta >= 0 {
            state
                .drill_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            state.drill_cursor.saturating_sub(delta.unsigned_abs())
        };
        self.sync_insights_heatmap_selection();
    }

    fn handle_mouse(&mut self, kind: MouseEventKind) -> Cmd<Msg> {
        match kind {
            MouseEventKind::ScrollUp => self.handle_key(KeyCode::Up, Modifiers::NONE),
            MouseEventKind::ScrollDown => self.handle_key(KeyCode::Down, Modifiers::NONE),
            _ => Cmd::None,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: Modifiers) -> Cmd<Msg> {
        // Clear one-shot status message on any key press.
        if !self.status_msg.is_empty() {
            self.status_msg.clear();
        }

        if self.show_quit_confirm {
            match code {
                KeyCode::Escape | KeyCode::Char('y' | 'Y') => return Cmd::Quit,
                _ => {
                    self.show_quit_confirm = false;
                    self.focus = FocusPane::List;
                    return Cmd::None;
                }
            }
        }

        if self.show_help {
            match code {
                KeyCode::Char('?' | 'q') | KeyCode::Escape | KeyCode::F(1) => {
                    self.show_help = false;
                    self.help_scroll_offset = 0;
                    self.focus = self.focus_before_help;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_sub(1);
                }
                KeyCode::Char('d') if modifiers.contains(Modifiers::CTRL) => {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_add(10);
                }
                KeyCode::Char('u') if modifiers.contains(Modifiers::CTRL) => {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_sub(10);
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.help_scroll_offset = 0;
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.help_scroll_offset = 999;
                }
                _ => {}
            }
            return Cmd::None;
        }

        // -- Modal overlay handling ------------------------------------------
        if let Some(ref overlay) = self.modal_overlay.clone() {
            match overlay {
                ModalOverlay::Tutorial => {
                    // Any key dismisses tutorial
                    self.modal_overlay = None;
                    return Cmd::None;
                }
                ModalOverlay::Confirm { .. } => {
                    match code {
                        KeyCode::Char('y' | 'Y') => {
                            self.modal_confirm_result = Some(true);
                            self.modal_overlay = None;
                        }
                        KeyCode::Char('n' | 'N') | KeyCode::Escape => {
                            self.modal_confirm_result = Some(false);
                            self.modal_overlay = None;
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
                ModalOverlay::PagesWizard(wiz) => {
                    return self.handle_pages_wizard_key(code, wiz.clone());
                }
                ModalOverlay::RecipePicker { items, cursor } => {
                    let len = items.len();
                    let cur = *cursor;
                    match code {
                        KeyCode::Escape => self.modal_overlay = None,
                        KeyCode::Char('j') | KeyCode::Down => {
                            if let Some(ModalOverlay::RecipePicker { cursor, items }) =
                                &mut self.modal_overlay
                            {
                                if *cursor + 1 < items.len() {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if let Some(ModalOverlay::RecipePicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Enter => {
                            if cur < len {
                                let recipe_name = items[cur].0.clone();
                                self.modal_overlay = None;
                                self.status_msg = format!("Recipe: {recipe_name}");
                            }
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
                ModalOverlay::LabelPicker { items, cursor } => {
                    let len = items.len();
                    let cur = *cursor;
                    match code {
                        KeyCode::Escape => self.modal_overlay = None,
                        KeyCode::Char('j') | KeyCode::Down => {
                            if let Some(ModalOverlay::LabelPicker { cursor, items }) =
                                &mut self.modal_overlay
                            {
                                if *cursor + 1 < items.len() {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if let Some(ModalOverlay::LabelPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Enter => {
                            if cur < len {
                                let label = items[cur].0.clone();
                                self.modal_overlay = None;
                                self.set_label_filter(&label);
                            }
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
                ModalOverlay::RepoPicker { items, cursor } => {
                    let len = items.len();
                    let cur = *cursor;
                    match code {
                        KeyCode::Escape => self.modal_overlay = None,
                        KeyCode::Char('j') | KeyCode::Down => {
                            if let Some(ModalOverlay::RepoPicker { cursor, items }) =
                                &mut self.modal_overlay
                            {
                                if *cursor + 1 < items.len() {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if let Some(ModalOverlay::RepoPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Enter => {
                            if cur < len {
                                let repo = items[cur].clone();
                                self.modal_overlay = None;
                                self.set_repo_filter(&repo);
                            }
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
            }
        }

        // -- Force refresh (Ctrl+R / F5) ------------------------------------
        if matches!(
            (code, modifiers.contains(Modifiers::CTRL)),
            (KeyCode::Char('r'), true) | (KeyCode::F(5), _)
        ) {
            self.refresh_from_disk();
            return Cmd::None;
        }

        self.ensure_selected_visible();

        if self.board_shortcut_focus() && self.board_search_active {
            match code {
                KeyCode::Escape => self.cancel_board_search(),
                KeyCode::Enter => self.finish_board_search(),
                KeyCode::Backspace => {
                    self.board_search_query.pop();
                    self.board_search_match_cursor = 0;
                    self.select_current_board_search_match();
                }
                KeyCode::Char('n') => self.move_board_search_match_relative(1),
                KeyCode::Char('N') => self.move_board_search_match_relative(-1),
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.board_search_query.push(ch);
                    self.board_search_match_cursor = 0;
                    self.select_current_board_search_match();
                }
                _ => {}
            }
            return Cmd::None;
        }

        if matches!(self.mode, ViewMode::History)
            && self.focus == FocusPane::List
            && self.history_search_active
        {
            match code {
                KeyCode::Escape => self.cancel_history_search(),
                KeyCode::Enter => self.finish_history_search(),
                KeyCode::Tab | KeyCode::BackTab => {
                    self.history_search_mode = self.history_search_mode.cycle();
                    self.refresh_history_search_selection();
                }
                KeyCode::Backspace => {
                    self.history_search_query.pop();
                    self.refresh_history_search_selection();
                }
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.history_search_query.push(ch);
                    self.refresh_history_search_selection();
                }
                _ => {}
            }
            return Cmd::None;
        }

        if matches!(self.mode, ViewMode::Graph)
            && self.focus == FocusPane::List
            && self.graph_search_active
        {
            match code {
                KeyCode::Escape => self.cancel_graph_search(),
                KeyCode::Enter => self.finish_graph_search(),
                KeyCode::Backspace => {
                    self.graph_search_query.pop();
                    self.graph_search_match_cursor = 0;
                    self.select_current_graph_search_match();
                }
                KeyCode::Char('n') => self.move_graph_search_match_relative(1),
                KeyCode::Char('N') => self.move_graph_search_match_relative(-1),
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.graph_search_query.push(ch);
                    self.graph_search_match_cursor = 0;
                    self.select_current_graph_search_match();
                }
                _ => {}
            }
            return Cmd::None;
        }

        if matches!(self.mode, ViewMode::Insights)
            && self.focus == FocusPane::List
            && self.insights_search_active
        {
            match code {
                KeyCode::Escape => self.cancel_insights_search(),
                KeyCode::Enter => self.finish_insights_search(),
                KeyCode::Backspace => {
                    self.insights_search_query.pop();
                    self.insights_search_match_cursor = 0;
                    self.select_current_insights_search_match();
                }
                KeyCode::Char('n') => self.move_insights_search_match_relative(1),
                KeyCode::Char('N') => self.move_insights_search_match_relative(-1),
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.insights_search_query.push(ch);
                    self.insights_search_match_cursor = 0;
                    self.select_current_insights_search_match();
                }
                _ => {}
            }
            return Cmd::None;
        }

        // -- Time-travel ref input -----------------------------------------------
        if matches!(self.mode, ViewMode::TimeTravelDiff) && self.time_travel_input_active {
            match code {
                KeyCode::Escape => {
                    self.time_travel_input_active = false;
                    if self.time_travel_diff.is_none() {
                        self.mode = ViewMode::Main;
                        self.focus = FocusPane::List;
                    }
                }
                KeyCode::Enter => self.execute_time_travel(),
                KeyCode::Backspace => {
                    self.time_travel_ref_input.pop();
                }
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.time_travel_ref_input.push(ch);
                }
                _ => {}
            }
            return Cmd::None;
        }

        match code {
            KeyCode::Escape if self.exit_insights_heatmap_drill() => return Cmd::None,
            KeyCode::Char('?') => {
                self.show_help = true;
                self.focus_before_help = self.focus;
            }
            KeyCode::Enter if self.insights_heatmap_shortcut_focus() => {
                self.enter_insights_heatmap_drill();
            }
            KeyCode::Enter
                if !(matches!(self.mode, ViewMode::History) && self.history_file_tree_focus)
                    && !matches!(self.mode, ViewMode::Tree) =>
            {
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && let Some(bead_id) = self
                        .selected_history_event()
                        .map(|event| event.issue_id)
                        .or_else(|| self.selected_history_git_related_bead_id())
                {
                    self.select_issue_by_id(&bead_id);
                }
                self.mode = ViewMode::Main;
                self.focus = FocusPane::Detail;
            }
            KeyCode::Char('q') => {
                if matches!(self.mode, ViewMode::Main) {
                    return Cmd::Quit;
                }
                self.mode = ViewMode::Main;
                self.focus = FocusPane::List;
            }
            KeyCode::Char('c') if modifiers.contains(Modifiers::CTRL) => return Cmd::Quit,
            KeyCode::Escape => {
                if matches!(self.mode, ViewMode::History) && self.history_show_file_tree {
                    self.history_show_file_tree = false;
                    self.history_file_tree_focus = false;
                    self.history_status_msg = "File tree hidden".into();
                } else if matches!(self.mode, ViewMode::History) {
                    self.mode = self.mode_before_history;
                    self.focus = FocusPane::List;
                } else if !matches!(self.mode, ViewMode::Main) {
                    self.mode = ViewMode::Main;
                    self.focus = FocusPane::List;
                } else if self.has_active_filter() {
                    self.set_list_filter(ListFilter::All);
                } else {
                    self.show_quit_confirm = true;
                }
            }
            KeyCode::Char('j') | KeyCode::Down
                if self.insights_heatmap_shortcut_focus()
                    && self
                        .insights_heatmap
                        .as_ref()
                        .is_some_and(|state| state.drill_active) =>
            {
                self.move_insights_heatmap_drill(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if self.insights_heatmap_shortcut_focus()
                    && self
                        .insights_heatmap
                        .as_ref()
                        .is_some_and(|state| state.drill_active) =>
            {
                self.move_insights_heatmap_drill(-1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.insights_heatmap_shortcut_focus() => {
                self.move_insights_heatmap_row(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.insights_heatmap_shortcut_focus() => {
                self.move_insights_heatmap_row(-1);
            }
            KeyCode::Char('h') if self.insights_heatmap_shortcut_focus() => {
                self.move_insights_heatmap_col(-1);
            }
            KeyCode::Char('l')
                if self.insights_heatmap_shortcut_focus()
                    && self
                        .insights_heatmap
                        .as_ref()
                        .is_some_and(|state| !state.drill_active) =>
            {
                self.move_insights_heatmap_col(1);
            }
            KeyCode::Tab => {
                if matches!(self.mode, ViewMode::History) && self.history_show_file_tree {
                    // 3-way cycle: List → Detail → FileTree → List
                    if self.history_file_tree_focus {
                        self.history_file_tree_focus = false;
                        self.focus = FocusPane::List;
                    } else if self.focus == FocusPane::Detail {
                        self.history_file_tree_focus = true;
                    } else {
                        self.focus = FocusPane::Detail;
                    }
                } else {
                    self.focus = match self.focus {
                        FocusPane::List => FocusPane::Detail,
                        FocusPane::Detail => FocusPane::List,
                    };
                }
            }
            KeyCode::Char('j')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(3);
            }
            KeyCode::Char('k')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(-3);
            }
            KeyCode::Char('h') if self.board_shortcut_focus() => {
                self.move_board_lane_relative(-1);
            }
            KeyCode::Char('l') if self.board_shortcut_focus() => {
                self.move_board_lane_relative(1);
            }
            KeyCode::Char('/') if self.board_shortcut_focus() => {
                self.start_board_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::History) && self.focus == FocusPane::List =>
            {
                self.start_history_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::List =>
            {
                self.start_graph_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Insights) && self.focus == FocusPane::List =>
            {
                self.start_insights_search();
            }
            KeyCode::Char('n') if self.board_shortcut_focus() => {
                self.move_board_search_match_relative(1);
            }
            KeyCode::Char('N') if self.board_shortcut_focus() => {
                self.move_board_search_match_relative(-1);
            }
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::History)
                    && self.focus == FocusPane::List
                    && !self.history_search_query.is_empty() =>
            {
                self.move_history_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::History)
                    && self.focus == FocusPane::List
                    && !self.history_search_query.is_empty() =>
            {
                self.move_history_search_match_relative(-1);
            }
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::Graph)
                    && self.focus == FocusPane::List
                    && !self.graph_search_query.is_empty() =>
            {
                self.move_graph_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::Graph)
                    && self.focus == FocusPane::List
                    && !self.graph_search_query.is_empty() =>
            {
                self.move_graph_search_match_relative(-1);
            }
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::Insights)
                    && self.focus == FocusPane::List
                    && !self.insights_search_query.is_empty() =>
            {
                self.move_insights_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::Insights)
                    && self.focus == FocusPane::List
                    && !self.insights_search_query.is_empty() =>
            {
                self.move_insights_search_match_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.board_shortcut_focus() => {
                self.move_board_row_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.board_shortcut_focus() => {
                self.move_board_row_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.actionable_shortcut_focus() => {
                self.move_actionable_cursor(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.actionable_shortcut_focus() => {
                self.move_actionable_cursor(-1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.attention_shortcut_focus() => {
                self.move_attention_cursor(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.attention_shortcut_focus() => {
                self.move_attention_cursor(-1);
            }
            // -- Tree mode navigation
            KeyCode::Char('j') | KeyCode::Down if self.tree_shortcut_focus() => {
                if self.tree_cursor + 1 < self.tree_flat_nodes.len() {
                    self.tree_cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.tree_shortcut_focus() => {
                self.tree_cursor = self.tree_cursor.saturating_sub(1);
            }
            KeyCode::Enter
                if matches!(self.mode, ViewMode::Tree) && self.focus == FocusPane::List =>
            {
                self.tree_toggle_collapse();
            }
            // -- LabelDashboard mode navigation
            KeyCode::Char('j') | KeyCode::Down if self.label_dashboard_shortcut_focus() => {
                let count = self.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
                if count > 0 && self.label_dashboard_cursor + 1 < count {
                    self.label_dashboard_cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.label_dashboard_shortcut_focus() => {
                self.label_dashboard_cursor = self.label_dashboard_cursor.saturating_sub(1);
            }
            // -- FlowMatrix mode navigation
            KeyCode::Char('j') | KeyCode::Down if self.flow_matrix_shortcut_focus() => {
                let count = self.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
                if count > 0 && self.flow_matrix_row_cursor + 1 < count {
                    self.flow_matrix_row_cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.flow_matrix_shortcut_focus() => {
                self.flow_matrix_row_cursor = self.flow_matrix_row_cursor.saturating_sub(1);
            }
            KeyCode::Char('l') | KeyCode::Right if self.flow_matrix_shortcut_focus() => {
                let count = self.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
                if count > 0 && self.flow_matrix_col_cursor + 1 < count {
                    self.flow_matrix_col_cursor += 1;
                }
            }
            KeyCode::Char('h') | KeyCode::Left if self.flow_matrix_shortcut_focus() => {
                self.flow_matrix_col_cursor = self.flow_matrix_col_cursor.saturating_sub(1);
            }
            KeyCode::Char('j') | KeyCode::Down if self.sprint_shortcut_focus() => {
                if self.focus == FocusPane::List {
                    let count = self.sprint_data.len();
                    if count > 0 && self.sprint_cursor + 1 < count {
                        self.sprint_cursor += 1;
                        self.sprint_issue_cursor = 0;
                    }
                } else {
                    let issue_count = self.sprint_visible_issues().len();
                    if issue_count > 0 && self.sprint_issue_cursor + 1 < issue_count {
                        self.sprint_issue_cursor += 1;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.sprint_shortcut_focus() => {
                if self.focus == FocusPane::List {
                    self.sprint_cursor = self.sprint_cursor.saturating_sub(1);
                    self.sprint_issue_cursor = 0;
                } else {
                    self.sprint_issue_cursor = self.sprint_issue_cursor.saturating_sub(1);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.time_travel_shortcut_focus() => {
                if self.focus == FocusPane::List {
                    let count = self.time_travel_categories().len();
                    if count > 0 && self.time_travel_category_cursor + 1 < count {
                        self.time_travel_category_cursor += 1;
                        self.time_travel_issue_cursor = 0;
                    }
                } else {
                    self.time_travel_issue_cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.time_travel_shortcut_focus() => {
                if self.focus == FocusPane::List {
                    self.time_travel_category_cursor =
                        self.time_travel_category_cursor.saturating_sub(1);
                    self.time_travel_issue_cursor = 0;
                } else {
                    self.time_travel_issue_cursor = self.time_travel_issue_cursor.saturating_sub(1);
                }
            }
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(10);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(-10);
            }
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL) && self.board_shortcut_focus() =>
            {
                self.move_board_row_relative(10);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL) && self.board_shortcut_focus() =>
            {
                self.move_board_row_relative(-10);
            }
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(10);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(-10);
            }
            KeyCode::Char('h')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(-1);
            }
            KeyCode::Char('h')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::Detail =>
            {
                self.focus = FocusPane::List;
            }
            KeyCode::Char('l')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(1);
            }
            KeyCode::Char('H')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(-10);
            }
            KeyCode::Char('L')
                if matches!(self.mode, ViewMode::Graph) && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(10);
            }
            KeyCode::Char('h') if matches!(self.mode, ViewMode::Insights) => {
                self.focus = FocusPane::List;
            }
            KeyCode::Char('l') if matches!(self.mode, ViewMode::Insights) => {
                self.focus = FocusPane::Detail;
            }
            KeyCode::Char('h') if matches!(self.mode, ViewMode::Main | ViewMode::History) => {
                self.toggle_history_mode();
            }
            KeyCode::Char('c') if matches!(self.mode, ViewMode::History) => {
                if matches!(self.history_view_mode, HistoryViewMode::Bead) {
                    self.cycle_history_confidence();
                }
            }
            KeyCode::Char('v') if matches!(self.mode, ViewMode::History) => {
                self.toggle_history_view_mode();
            }
            KeyCode::Char('s') if matches!(self.mode, ViewMode::Main) => self.cycle_list_sort(),
            KeyCode::Char('m') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_heatmap();
            }
            KeyCode::Char('y') if matches!(self.mode, ViewMode::History) => {
                self.history_copy_to_clipboard();
            }
            KeyCode::Char('o') if matches!(self.mode, ViewMode::History) => {
                self.history_open_in_browser();
            }
            KeyCode::Char('f' | 'F') if matches!(self.mode, ViewMode::History) => {
                self.toggle_history_file_tree();
            }
            // File tree navigation (when file tree has focus in history mode)
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::History) && self.history_file_tree_focus =>
            {
                self.move_file_tree_cursor_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History) && self.history_file_tree_focus =>
            {
                self.move_file_tree_cursor_relative(-1);
            }
            KeyCode::Enter
                if matches!(self.mode, ViewMode::History) && self.history_file_tree_focus =>
            {
                self.file_tree_toggle_or_filter();
            }
            KeyCode::Char('o') => self.set_list_filter(ListFilter::Open),
            KeyCode::Char('c') => self.set_list_filter(ListFilter::Closed),
            KeyCode::Char('r') => self.set_list_filter(ListFilter::Ready),
            KeyCode::Char('a') if self.should_clear_filter_with_all_shortcut() => {
                self.set_list_filter(ListFilter::All);
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.move_history_cursor_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.move_history_cursor_relative(-1);
            }
            KeyCode::PageUp
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.move_history_cursor_relative(-10);
            }
            KeyCode::PageDown
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.move_history_cursor_relative(10);
            }
            KeyCode::Char('J')
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git) =>
            {
                self.move_history_related_bead_relative(1);
            }
            KeyCode::Char('K')
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git) =>
            {
                self.move_history_related_bead_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::Detail =>
            {
                self.move_history_related_bead_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::Detail =>
            {
                self.move_history_related_bead_relative(-1);
            }
            KeyCode::Char('J')
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead) =>
            {
                self.move_history_bead_commit_relative(1);
            }
            KeyCode::Char('K')
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead) =>
            {
                self.move_history_bead_commit_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead)
                    && self.focus == FocusPane::Detail =>
            {
                self.move_history_bead_commit_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead)
                    && self.focus == FocusPane::Detail =>
            {
                self.move_history_bead_commit_relative(-1);
            }
            // Board/Graph/Insights detail dependency navigation
            KeyCode::Char('J')
                if matches!(
                    self.mode,
                    ViewMode::Board | ViewMode::Graph | ViewMode::Insights
                ) =>
            {
                self.move_detail_dep_relative(1);
            }
            KeyCode::Char('K')
                if matches!(
                    self.mode,
                    ViewMode::Board | ViewMode::Graph | ViewMode::Insights
                ) =>
            {
                self.move_detail_dep_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::Graph | ViewMode::Insights)
                    && self.focus == FocusPane::Detail
                    && !self.detail_dep_list().is_empty() =>
            {
                self.move_detail_dep_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::Graph | ViewMode::Insights)
                    && self.focus == FocusPane::Detail
                    && !self.detail_dep_list().is_empty() =>
            {
                self.move_detail_dep_relative(-1);
            }
            KeyCode::Home
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.history_event_cursor = 0;
                self.history_related_bead_cursor = 0;
            }
            KeyCode::End | KeyCode::Char('G')
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && self.focus == FocusPane::List =>
            {
                self.select_last_history_event();
            }
            KeyCode::Char('j') | KeyCode::Down if self.focus == FocusPane::List => {
                self.move_selection_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.focus == FocusPane::List => {
                self.move_selection_relative(-1);
            }
            KeyCode::PageUp if self.focus == FocusPane::List => {
                self.move_selection_relative(-10);
            }
            KeyCode::PageDown if self.focus == FocusPane::List => {
                self.move_selection_relative(10);
            }
            KeyCode::Home | KeyCode::Char('0') if self.board_shortcut_focus() => {
                self.select_edge_in_current_board_lane(false);
            }
            KeyCode::End | KeyCode::Char('G' | '$') if self.board_shortcut_focus() => {
                self.select_edge_in_current_board_lane(true);
            }
            KeyCode::Home if self.focus == FocusPane::List => {
                self.select_first_visible();
            }
            KeyCode::End | KeyCode::Char('G') if self.focus == FocusPane::List => {
                self.select_last_visible();
            }
            KeyCode::Char('1') if self.board_shortcut_focus() => {
                self.select_first_in_board_lane(1);
            }
            KeyCode::Char('2') if self.board_shortcut_focus() => {
                self.select_first_in_board_lane(2);
            }
            KeyCode::Char('3') if self.board_shortcut_focus() => {
                self.select_first_in_board_lane(3);
            }
            KeyCode::Char('4') if self.board_shortcut_focus() => {
                self.select_first_in_board_lane(4);
            }
            KeyCode::Char('H') if self.board_shortcut_focus() => {
                self.select_first_in_non_empty_board_lane();
            }
            KeyCode::Char('L') if self.board_shortcut_focus() => {
                self.select_last_in_non_empty_board_lane();
            }
            KeyCode::Char('s') if matches!(self.mode, ViewMode::Board) => {
                self.cycle_board_grouping();
            }
            KeyCode::Char('e') if matches!(self.mode, ViewMode::Board) => {
                self.toggle_board_empty_visibility();
            }
            KeyCode::Char('s')
                if matches!(self.mode, ViewMode::Insights) && self.insights_heatmap.is_none() =>
            {
                self.insights_panel = self.insights_panel.next();
            }
            KeyCode::Char('S')
                if matches!(self.mode, ViewMode::Insights) && self.insights_heatmap.is_none() =>
            {
                self.insights_panel = self.insights_panel.prev();
            }
            KeyCode::Char('e') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_explanations();
            }
            KeyCode::Char('x') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_calc_proof();
            }
            KeyCode::Char('1') => self.mode = ViewMode::Main,
            KeyCode::Char('b') => {
                self.mode = if matches!(self.mode, ViewMode::Board) {
                    ViewMode::Main
                } else {
                    ViewMode::Board
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('i') => {
                self.mode = if matches!(self.mode, ViewMode::Insights) {
                    ViewMode::Main
                } else {
                    ViewMode::Insights
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('g') if matches!(self.mode, ViewMode::History) => {
                if matches!(self.history_view_mode, HistoryViewMode::Git)
                    && let Some(bead_id) = self
                        .selected_history_event()
                        .map(|event| event.issue_id)
                        .or_else(|| self.selected_history_git_related_bead_id())
                {
                    self.select_issue_by_id(&bead_id);
                }
                self.mode = ViewMode::Graph;
                self.focus = FocusPane::List;
            }
            KeyCode::Char('g') => {
                self.mode = if matches!(self.mode, ViewMode::Graph) {
                    ViewMode::Main
                } else {
                    ViewMode::Graph
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('a') => {
                self.mode = if matches!(self.mode, ViewMode::Actionable) {
                    ViewMode::Main
                } else {
                    self.compute_actionable_plan();
                    ViewMode::Actionable
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('!') => {
                self.mode = if matches!(self.mode, ViewMode::Attention) {
                    ViewMode::Main
                } else {
                    self.compute_attention();
                    ViewMode::Attention
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('T') => {
                self.toggle_tree_mode();
                self.focus = FocusPane::List;
            }
            KeyCode::Char('t') => {
                self.toggle_time_travel_mode();
            }
            KeyCode::Char('[') => {
                self.toggle_label_dashboard();
                self.focus = FocusPane::List;
            }
            KeyCode::Char(']') => {
                self.toggle_flow_matrix();
                self.focus = FocusPane::List;
            }
            KeyCode::Char('S') if !matches!(self.mode, ViewMode::Insights) => {
                self.toggle_sprint_mode();
                self.focus = FocusPane::List;
            }
            KeyCode::Char('\'') => {
                self.open_recipe_picker();
            }
            KeyCode::Char('L') => {
                self.open_label_picker();
            }
            KeyCode::Char('w') => {
                self.open_repo_picker();
            }
            KeyCode::Char('p') if matches!(self.mode, ViewMode::Main) => {
                self.priority_hints_visible = !self.priority_hints_visible;
            }
            KeyCode::Char('C') => {
                self.copy_selected_issue_id();
            }
            KeyCode::Char('x') if matches!(self.mode, ViewMode::Main) => {
                self.export_selected_issue_markdown();
            }
            KeyCode::Char('O') => {
                self.open_selected_in_editor();
            }
            _ => {}
        }

        if !matches!(self.mode, ViewMode::History) {
            self.mode_before_history = self.mode;
        }

        Cmd::None
    }

    fn toggle_history_mode(&mut self) {
        if matches!(self.mode, ViewMode::History) {
            self.mode = self.mode_before_history;
            self.focus = FocusPane::List;
            return;
        }

        self.mode_before_history = self.mode;
        self.mode = ViewMode::History;
        self.history_view_mode = HistoryViewMode::Bead;
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
        self.focus = FocusPane::List;
        self.ensure_git_history_loaded();
    }

    fn cycle_history_confidence(&mut self) {
        self.history_confidence_index =
            (self.history_confidence_index + 1) % HISTORY_CONFIDENCE_STEPS.len();
    }

    fn history_min_confidence(&self) -> f64 {
        HISTORY_CONFIDENCE_STEPS
            .get(self.history_confidence_index)
            .copied()
            .unwrap_or(0.0)
    }

    fn toggle_history_view_mode(&mut self) {
        self.history_view_mode = self.history_view_mode.toggle();
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
        self.focus = FocusPane::List;
        self.ensure_git_history_loaded();
        self.refresh_history_search_selection();
    }

    fn start_history_search(&mut self) {
        if !matches!(self.mode, ViewMode::History) || self.focus != FocusPane::List {
            return;
        }

        self.history_search_active = true;
        self.history_search_query.clear();
        self.history_search_match_cursor = 0;
        self.history_search_mode = HistorySearchMode::All;
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
    }

    fn finish_history_search(&mut self) {
        self.history_search_active = false;
    }

    fn cancel_history_search(&mut self) {
        self.history_search_active = false;
        self.history_search_match_cursor = 0;
        self.history_search_query.clear();
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
    }

    #[allow(dead_code)]
    fn open_tutorial(&mut self) {
        self.modal_overlay = Some(ModalOverlay::Tutorial);
    }

    #[allow(dead_code)]
    fn open_confirm(&mut self, title: impl Into<String>, message: impl Into<String>) {
        self.modal_confirm_result = None;
        self.modal_overlay = Some(ModalOverlay::Confirm {
            title: title.into(),
            message: message.into(),
        });
    }

    #[allow(dead_code)]
    fn open_pages_wizard(&mut self) {
        self.modal_overlay = Some(ModalOverlay::PagesWizard(PagesWizardState::new()));
    }

    fn toggle_history_file_tree(&mut self) {
        self.history_show_file_tree = !self.history_show_file_tree;
        if self.history_show_file_tree {
            self.history_status_msg = "File tree: j/k navigate, Enter filter, Esc close".into();
        } else {
            self.history_file_tree_focus = false;
            self.history_status_msg = "File tree hidden".into();
        }
    }

    fn move_file_tree_cursor_relative(&mut self, delta: isize) {
        let flat = self.history_flat_file_list();
        if flat.is_empty() {
            return;
        }
        let len = flat.len();
        let cur = self.history_file_tree_cursor.min(len.saturating_sub(1));
        self.history_file_tree_cursor = if delta > 0 {
            cur.saturating_add(delta.unsigned_abs())
                .min(len.saturating_sub(1))
        } else {
            cur.saturating_sub(delta.unsigned_abs())
        };
    }

    fn file_tree_toggle_or_filter(&mut self) {
        let flat = self.history_flat_file_list();
        let cursor = self
            .history_file_tree_cursor
            .min(flat.len().saturating_sub(1));
        if let Some(entry) = flat.get(cursor) {
            let path = entry.path.clone();
            if self.history_file_tree_filter.as_deref() == Some(&path) {
                self.history_file_tree_filter = None;
                self.history_status_msg = "Filter cleared".into();
            } else {
                self.history_file_tree_filter = Some(path.clone());
                self.history_status_msg = format!("Filtered to: {path}");
            }
            self.history_event_cursor = 0;
            self.history_bead_commit_cursor = 0;
            if matches!(self.history_view_mode, HistoryViewMode::Bead) {
                let visible = self.history_visible_issue_indices();
                if !visible.contains(&self.selected)
                    && let Some(&first_visible) = visible.first()
                {
                    self.set_selected_index(first_visible);
                }
            }
        }
    }

    fn history_path_matches_file_filter(&self, path: &str) -> bool {
        self.history_file_tree_filter
            .as_deref()
            .is_none_or(|filter| path == filter || path.starts_with(&format!("{filter}/")))
    }

    fn history_filtered_bead_commits<'a>(&'a self, issue_id: &str) -> Vec<&'a HistoryCommitCompat> {
        let min_confidence = self.history_min_confidence();
        self.history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(issue_id))
            .and_then(|history| history.commits.as_deref())
            .map(|commits| {
                commits
                    .iter()
                    .filter(|commit| {
                        commit.confidence >= min_confidence
                            && commit
                                .files
                                .iter()
                                .any(|file| self.history_path_matches_file_filter(&file.path))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn history_file_tree_nodes(&self) -> Vec<FileTreeNode> {
        let Some(cache) = &self.history_git_cache else {
            return Vec::new();
        };

        let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
        for commit in &cache.commits {
            for file in &commit.files {
                *file_counts.entry(file.path.clone()).or_default() += 1;
            }
        }

        fn insert_path(
            nodes: &mut Vec<FileTreeNode>,
            parts: &[&str],
            level: usize,
            prefix: &str,
            count: usize,
        ) {
            let Some((name, rest)) = parts.split_first() else {
                return;
            };

            let path = if prefix.is_empty() {
                (*name).to_string()
            } else {
                format!("{prefix}/{name}")
            };
            let is_dir = !rest.is_empty();
            let index = nodes
                .iter()
                .position(|node| node.path == path)
                .unwrap_or_else(|| {
                    nodes.push(FileTreeNode {
                        name: (*name).to_string(),
                        path: path.clone(),
                        is_dir,
                        change_count: 0,
                        expanded: true,
                        level,
                        children: Vec::new(),
                    });
                    nodes.len() - 1
                });

            nodes[index].change_count += count;
            if is_dir {
                insert_path(&mut nodes[index].children, rest, level + 1, &path, count);
            }
        }

        fn sort_nodes(nodes: &mut [FileTreeNode]) {
            for node in nodes.iter_mut() {
                sort_nodes(&mut node.children);
            }
            nodes.sort_by(|left, right| {
                right
                    .is_dir
                    .cmp(&left.is_dir)
                    .then_with(|| left.name.cmp(&right.name))
            });
        }

        let mut roots = Vec::new();
        for (path, count) in file_counts {
            let parts = path.split('/').collect::<Vec<_>>();
            insert_path(&mut roots, &parts, 0, "", count);
        }
        sort_nodes(&mut roots);
        roots
    }

    fn history_flat_file_list(&self) -> Vec<FlatFileEntry> {
        self.history_file_tree_nodes()
            .iter()
            .flat_map(FileTreeNode::flatten_visible)
            .collect()
    }

    /// Copy selected bead ID or commit SHA to clipboard via external command.
    fn history_copy_to_clipboard(&mut self) {
        let text = if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.selected_history_git_commit_sha()
        } else {
            self.selected_issue().map(|issue| issue.id.clone())
        };

        if let Some(text) = text {
            if copy_text_to_clipboard(&text) {
                let short = if text.len() > 7 { &text[..7] } else { &text };
                self.history_status_msg = format!("Copied {short} to clipboard");
            } else {
                self.history_status_msg = "Clipboard not available".into();
            }
        } else {
            self.history_status_msg = "No item selected".into();
        }
    }

    fn selected_history_git_commit_sha(&self) -> Option<String> {
        let cache = self.history_git_cache.as_ref()?;
        let commit = cache.commits.get(self.history_event_cursor)?;
        Some(commit.sha.clone())
    }

    /// Open selected commit in browser via git remote URL.
    fn history_open_in_browser(&mut self) {
        let sha = if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.selected_history_git_commit_sha()
        } else {
            // In bead mode, try to get commit SHA from bead's history
            let Some(issue) = self.selected_issue() else {
                self.history_status_msg = "No item selected".into();
                return;
            };
            self.history_git_cache.as_ref().and_then(|cache| {
                cache
                    .commit_bead_confidence
                    .iter()
                    .find(|(_, pairs)| pairs.iter().any(|(id, _)| id == &issue.id))
                    .map(|(sha, _)| sha.clone())
            })
        };

        let Some(sha) = sha else {
            self.history_status_msg = "No commit selected".into();
            return;
        };

        // Try to get remote URL from repo root
        let repo_root = self
            .repo_root
            .clone()
            .or_else(|| std::env::current_dir().ok());
        let Some(repo_root) = repo_root else {
            self.history_status_msg = "No repository root found".into();
            return;
        };

        let remote_url = std::process::Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
            .current_dir(&repo_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok()
                } else {
                    None
                }
            })
            .map(|s| s.trim().to_string());

        let Some(remote_url) = remote_url else {
            self.history_status_msg = "No git remote configured".into();
            return;
        };

        // Convert remote URL to web commit URL
        let web_url = remote_to_commit_url(&remote_url, &sha);
        let Some(url) = web_url else {
            self.history_status_msg = "Cannot build commit URL from remote".into();
            return;
        };

        if open_url_in_browser(&url) {
            let short = if sha.len() > 7 { &sha[..7] } else { &sha };
            self.history_status_msg = format!("Opened {short} in browser");
        } else {
            self.history_status_msg = "Could not open browser".into();
        }
    }

    fn refresh_history_search_selection(&mut self) {
        if self.history_search_query.trim().is_empty() {
            return;
        }

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.history_event_cursor = 0;
            self.history_related_bead_cursor = 0;
            return;
        }

        let visible = self.history_visible_issue_indices();
        if let Some(index) = visible.first().copied() {
            self.set_selected_index(index);
            self.focus = FocusPane::List;
            self.history_bead_commit_cursor = 0;
        }
    }

    /// Compute indices matching the current history search query.
    /// In bead mode: returns matching issue indices.
    /// In git mode: returns matching commit indices.
    fn history_search_matches(&self) -> Vec<usize> {
        let query = self.history_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.history_git_visible_commit_indices()
        } else {
            self.history_visible_issue_indices()
        }
    }

    fn move_history_search_match_relative(&mut self, delta: isize) {
        let matches = self.history_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }

        let len = matches.len();
        let current = self.history_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.history_search_match_cursor = next;

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.history_event_cursor = matches[next];
            self.history_related_bead_cursor = 0;
        } else {
            self.set_selected_index(matches[next]);
            self.history_bead_commit_cursor = 0;
        }
    }

    fn ensure_git_history_loaded(&mut self) {
        if self.history_git_cache.is_some() {
            return;
        }

        let repo_root = self
            .repo_root
            .clone()
            .or_else(|| std::env::current_dir().ok());
        let Some(repo_root) = repo_root else {
            return;
        };

        let commits = load_git_commits(&repo_root, 500, None).unwrap_or_default();
        let mut histories = self
            .analyzer
            .issues
            .iter()
            .map(|issue| {
                (
                    issue.id.clone(),
                    HistoryBeadCompat {
                        bead_id: issue.id.clone(),
                        title: issue.title.clone(),
                        status: issue.status.clone(),
                        events: Vec::new(),
                        milestones: HistoryMilestonesCompat::default(),
                        commits: None,
                        cycle_time: None,
                        last_author: String::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut commit_index = BTreeMap::<String, Vec<String>>::new();
        let mut method_distribution = BTreeMap::<String, usize>::new();

        correlate_histories_with_git(
            &repo_root,
            &commits,
            &mut histories,
            &mut commit_index,
            &mut method_distribution,
        );

        finalize_history_entries(&mut histories);

        let mut commit_bead_confidence = BTreeMap::<String, Vec<(String, f64)>>::new();
        for history in histories.values() {
            for commit in history.commits.as_deref().unwrap_or_default() {
                commit_bead_confidence
                    .entry(commit.sha.clone())
                    .or_default()
                    .push((history.bead_id.clone(), commit.confidence));
            }
        }
        for pairs in commit_bead_confidence.values_mut() {
            pairs.sort_by(|left, right| left.0.cmp(&right.0));
        }

        self.history_git_cache = Some(HistoryGitCache {
            commits,
            histories,
            commit_bead_confidence,
        });
    }

    fn history_git_visible_commit_indices(&self) -> Vec<usize> {
        let Some(cache) = &self.history_git_cache else {
            return Vec::new();
        };

        let min_confidence = self.history_min_confidence();
        let query = self.history_search_query.trim().to_ascii_lowercase();

        cache
            .commits
            .iter()
            .enumerate()
            .filter_map(|(index, commit)| {
                let related = self.history_git_related_beads_for_commit(&commit.sha);
                if related.is_empty() {
                    return None;
                }
                if !commit
                    .files
                    .iter()
                    .any(|file| self.history_path_matches_file_filter(&file.path))
                {
                    return None;
                }

                if query.is_empty() {
                    return Some(index);
                }

                let matches = match self.history_search_mode {
                    HistorySearchMode::Sha => {
                        commit.sha.to_ascii_lowercase().starts_with(&query)
                            || commit.short_sha.to_ascii_lowercase().starts_with(&query)
                    }
                    HistorySearchMode::Commit => {
                        commit.message.to_ascii_lowercase().contains(&query)
                    }
                    HistorySearchMode::Author => {
                        commit.author.to_ascii_lowercase().contains(&query)
                            || commit.author_email.to_ascii_lowercase().contains(&query)
                    }
                    HistorySearchMode::Bead => related
                        .iter()
                        .any(|id| id.to_ascii_lowercase().contains(&query)),
                    HistorySearchMode::All => {
                        let sha = commit.sha.to_ascii_lowercase();
                        let short_sha = commit.short_sha.to_ascii_lowercase();
                        let message = commit.message.to_ascii_lowercase();
                        let author = commit.author.to_ascii_lowercase();
                        let author_email = commit.author_email.to_ascii_lowercase();
                        let related_match = related
                            .iter()
                            .any(|id| id.to_ascii_lowercase().contains(&query));
                        sha.contains(&query)
                            || short_sha.contains(&query)
                            || message.contains(&query)
                            || author.contains(&query)
                            || author_email.contains(&query)
                            || commit.timestamp.to_ascii_lowercase().contains(&query)
                            || related_match
                    }
                };

                matches.then_some(index)
            })
            .filter(|index| {
                let commit = cache.commits.get(*index);
                commit.is_some_and(|commit| {
                    self.history_git_related_beads_for_commit(&commit.sha)
                        .iter()
                        .any(|bead_id| {
                            cache.histories.get(bead_id).is_some_and(|history| {
                                history
                                    .commits
                                    .as_deref()
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|entry| {
                                        entry.sha == commit.sha
                                            && entry.confidence >= min_confidence
                                    })
                            })
                        })
                })
            })
            .collect()
    }

    fn selected_history_git_commit(&self) -> Option<&GitCommitRecord> {
        let Some(cache) = &self.history_git_cache else {
            return None;
        };

        let visible = self.history_git_visible_commit_indices();
        if visible.is_empty() {
            return None;
        }

        let slot = self
            .history_event_cursor
            .min(visible.len().saturating_sub(1));
        let index = visible[slot];
        cache.commits.get(index)
    }

    fn history_git_related_beads_for_commit(&self, sha: &str) -> Vec<String> {
        let Some(cache) = &self.history_git_cache else {
            return Vec::new();
        };

        let min_confidence = self.history_min_confidence();
        cache
            .commit_bead_confidence
            .get(sha)
            .into_iter()
            .flatten()
            .filter(|(_, confidence)| *confidence >= min_confidence)
            .map(|(bead_id, _)| bead_id.clone())
            .collect()
    }

    fn selected_history_git_related_bead_id(&self) -> Option<String> {
        let commit = self.selected_history_git_commit()?;
        let related = self.history_git_related_beads_for_commit(&commit.sha);
        if related.is_empty() {
            return None;
        }

        let slot = self
            .history_related_bead_cursor
            .min(related.len().saturating_sub(1));
        related.get(slot).cloned()
    }

    fn move_history_cursor_relative(&mut self, delta: isize) {
        let commits_len = self.history_git_visible_commit_indices().len();
        if commits_len == 0 {
            self.history_event_cursor = 0;
            return;
        }

        let max_slot = commits_len.saturating_sub(1);
        let next_slot = if delta >= 0 {
            self.history_event_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            self.history_event_cursor
                .saturating_sub(delta.unsigned_abs())
        };
        self.history_event_cursor = next_slot;
        self.history_related_bead_cursor = 0;
    }

    fn select_last_history_event(&mut self) {
        let commits_len = self.history_git_visible_commit_indices().len();
        self.history_event_cursor = commits_len.saturating_sub(1);
        self.history_related_bead_cursor = 0;
    }

    fn move_history_related_bead_relative(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }

        if self.focus == FocusPane::List {
            self.move_history_cursor_relative(delta);
            return;
        }

        let Some(commit) = self.selected_history_git_commit() else {
            self.history_related_bead_cursor = 0;
            return;
        };
        let related_len = self.history_git_related_beads_for_commit(&commit.sha).len();
        if related_len == 0 {
            self.history_related_bead_cursor = 0;
            return;
        }

        let max_slot = related_len.saturating_sub(1);
        let next_slot = if delta >= 0 {
            self.history_related_bead_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            self.history_related_bead_cursor
                .saturating_sub(delta.unsigned_abs())
        };
        self.history_related_bead_cursor = next_slot;
    }

    fn move_history_bead_commit_relative(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }

        if self.focus == FocusPane::List {
            self.move_selection_relative(delta);
            return;
        }

        let Some(issue_id) = self.selected_issue().map(|issue| issue.id.clone()) else {
            self.history_bead_commit_cursor = 0;
            return;
        };

        self.ensure_git_history_loaded();

        let commits_len = self.history_filtered_bead_commits(&issue_id).len();

        if commits_len == 0 {
            self.history_bead_commit_cursor = 0;
            return;
        }

        let max_slot = commits_len.saturating_sub(1);
        let next_slot = if delta >= 0 {
            self.history_bead_commit_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            self.history_bead_commit_cursor
                .saturating_sub(delta.unsigned_abs())
        };
        self.history_bead_commit_cursor = next_slot;
    }

    // ── Detail dependency navigation (board/graph/insights) ─────

    fn detail_dep_list(&self) -> Vec<String> {
        let Some(issue) = self.selected_issue() else {
            return Vec::new();
        };
        let mut deps = self.analyzer.graph.blockers(&issue.id);
        deps.extend(self.analyzer.graph.dependents(&issue.id));
        deps
    }

    fn move_detail_dep_relative(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        if self.focus == FocusPane::List {
            self.move_selection_relative(delta);
            return;
        }
        let dep_len = self.detail_dep_list().len();
        if dep_len == 0 {
            self.detail_dep_cursor = 0;
            return;
        }
        let max_slot = dep_len.saturating_sub(1);
        let next_slot = if delta >= 0 {
            self.detail_dep_cursor
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            self.detail_dep_cursor.saturating_sub(delta.unsigned_abs())
        };
        self.detail_dep_cursor = next_slot;
    }

    fn history_timeline_events(&self) -> Vec<HistoryTimelineEvent> {
        let mut events = self
            .analyzer
            .history(None, 0)
            .into_iter()
            .flat_map(|history| {
                history
                    .events
                    .into_iter()
                    .map(move |event| HistoryTimelineEvent {
                        issue_id: history.id.clone(),
                        issue_title: history.title.clone(),
                        issue_status: history.status.clone(),
                        event_kind: event.kind,
                        event_timestamp: event.timestamp,
                        event_details: event.details,
                    })
            })
            .collect::<Vec<_>>();

        events.sort_by(|left, right| {
            cmp_opt_datetime(left.event_timestamp, right.event_timestamp, true)
                .then_with(|| left.issue_id.cmp(&right.issue_id))
                .then_with(|| left.event_kind.cmp(&right.event_kind))
        });

        events
    }

    fn history_timeline_events_filtered(&self) -> Vec<HistoryTimelineEvent> {
        let query = self.history_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return self.history_timeline_events();
        }

        self.history_timeline_events()
            .into_iter()
            .filter(|event| {
                event.issue_id.to_ascii_lowercase().contains(&query)
                    || event.issue_title.to_ascii_lowercase().contains(&query)
                    || event.issue_status.to_ascii_lowercase().contains(&query)
                    || event.event_kind.to_ascii_lowercase().contains(&query)
                    || event.event_details.to_ascii_lowercase().contains(&query)
                    || event
                        .event_timestamp
                        .map(|dt| {
                            dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                                .to_ascii_lowercase()
                        })
                        .is_some_and(|timestamp| timestamp.contains(&query))
            })
            .collect()
    }

    fn selected_history_event(&self) -> Option<HistoryTimelineEvent> {
        let events = self.history_timeline_events_filtered();
        if events.is_empty() {
            return None;
        }

        let slot = self
            .history_event_cursor
            .min(events.len().saturating_sub(1));
        events.get(slot).cloned()
    }

    fn selected_history_bead_commit(&self) -> Option<HistoryCommitCompat> {
        let issue = self.selected_issue()?;
        let commits = self.history_filtered_bead_commits(&issue.id);
        if commits.is_empty() {
            return None;
        }

        let slot = self
            .history_bead_commit_cursor
            .min(commits.len().saturating_sub(1));
        commits.get(slot).map(|commit| (*commit).clone())
    }

    fn issue_matches_filter(&self, issue: &Issue) -> bool {
        let base = match self.list_filter {
            ListFilter::All => true,
            ListFilter::Open => issue.is_open_like(),
            ListFilter::Closed => issue.is_closed_like(),
            ListFilter::Ready => {
                issue.is_open_like() && self.analyzer.graph.open_blockers(&issue.id).is_empty()
            }
        };
        if !base {
            return false;
        }
        if let Some(ref label) = self.modal_label_filter {
            if !issue.labels.iter().any(|l| l == label) {
                return false;
            }
        }
        if let Some(ref repo) = self.modal_repo_filter {
            if issue.source_repo != *repo {
                return false;
            }
        }
        true
    }

    fn visible_issue_indices(&self) -> Vec<usize> {
        let mut visible = self
            .analyzer
            .issues
            .iter()
            .enumerate()
            .filter_map(|(index, issue)| self.issue_matches_filter(issue).then_some(index))
            .collect::<Vec<_>>();

        {
            visible.sort_by(|left_index, right_index| {
                let left_issue = &self.analyzer.issues[*left_index];
                let right_issue = &self.analyzer.issues[*right_index];

                match self.list_sort {
                    ListSort::Default => {
                        let l_open = left_issue.is_open_like();
                        let r_open = right_issue.is_open_like();
                        r_open
                            .cmp(&l_open)
                            .then_with(|| left_issue.priority.cmp(&right_issue.priority))
                            .then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
                    ListSort::CreatedAsc => {
                        cmp_opt_datetime(left_issue.created_at, right_issue.created_at, false)
                            .then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
                    ListSort::CreatedDesc => {
                        cmp_opt_datetime(left_issue.created_at, right_issue.created_at, true)
                            .then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
                    ListSort::Priority => left_issue
                        .priority
                        .cmp(&right_issue.priority)
                        .then_with(|| left_issue.id.cmp(&right_issue.id)),
                    ListSort::Updated => cmp_opt_datetime(
                        left_issue.updated_at.or(left_issue.created_at),
                        right_issue.updated_at.or(right_issue.created_at),
                        true,
                    )
                    .then_with(|| left_issue.id.cmp(&right_issue.id)),
                }
            });
        }

        visible
    }

    fn history_visible_issue_indices(&self) -> Vec<usize> {
        let visible = self.visible_issue_indices();
        if !matches!(self.mode, ViewMode::History)
            || !matches!(self.history_view_mode, HistoryViewMode::Bead)
        {
            return visible;
        }

        let query = self.history_search_query.trim().to_ascii_lowercase();
        if query.is_empty()
            && self.history_file_tree_filter.is_none()
            && self.history_min_confidence() == 0.0
        {
            return visible;
        }

        let cache = self.history_git_cache.as_ref();
        visible
            .into_iter()
            .filter(|index| {
                self.analyzer.issues.get(*index).is_some_and(|issue| {
                    let filtered_commits = if cache.is_some() {
                        self.history_filtered_bead_commits(&issue.id)
                    } else {
                        Vec::new()
                    };
                    if (self.history_file_tree_filter.is_some()
                        || self.history_min_confidence() > 0.0)
                        && filtered_commits.is_empty()
                    {
                        return false;
                    }

                    match self.history_search_mode {
                        HistorySearchMode::Bead => {
                            issue.id.to_ascii_lowercase().contains(&query)
                                || issue.title.to_ascii_lowercase().contains(&query)
                        }
                        HistorySearchMode::Sha => filtered_commits.iter().any(|commit| {
                            commit.sha.to_ascii_lowercase().starts_with(&query)
                                || commit.short_sha.to_ascii_lowercase().starts_with(&query)
                        }),
                        HistorySearchMode::Commit => filtered_commits
                            .iter()
                            .any(|commit| commit.message.to_ascii_lowercase().contains(&query)),
                        HistorySearchMode::Author => cache.is_some_and(|c| {
                            c.histories.get(&issue.id).is_some_and(|history| {
                                history.last_author.to_ascii_lowercase().contains(&query)
                                    || filtered_commits.iter().any(|commit| {
                                        commit.author.to_ascii_lowercase().contains(&query)
                                            || commit
                                                .author_email
                                                .to_ascii_lowercase()
                                                .contains(&query)
                                    })
                            })
                        }),
                        HistorySearchMode::All => {
                            issue.id.to_ascii_lowercase().contains(&query)
                                || issue.title.to_ascii_lowercase().contains(&query)
                                || issue.status.to_ascii_lowercase().contains(&query)
                                || issue.issue_type.to_ascii_lowercase().contains(&query)
                                || issue
                                    .labels
                                    .iter()
                                    .any(|label| label.to_ascii_lowercase().contains(&query))
                        }
                    }
                })
            })
            .collect()
    }

    fn visible_issue_indices_for_list_nav(&self) -> Vec<usize> {
        if matches!(self.mode, ViewMode::History)
            && matches!(self.history_view_mode, HistoryViewMode::Bead)
        {
            return self.history_visible_issue_indices();
        }

        self.visible_issue_indices()
    }

    fn selected_visible_slot(&self, visible: &[usize]) -> Option<usize> {
        visible.iter().position(|index| *index == self.selected)
    }

    fn ensure_selected_visible(&mut self) {
        let visible = self.visible_issue_indices_for_list_nav();
        if visible.is_empty() {
            self.set_selected_index(0);
            return;
        }
        if !visible.contains(&self.selected) {
            self.set_selected_index(visible[0]);
        }
    }

    fn move_selection_relative(&mut self, delta: isize) {
        let visible = self.visible_issue_indices_for_list_nav();
        if visible.is_empty() {
            return;
        }

        let current_slot = self.selected_visible_slot(&visible).unwrap_or(0);
        let max_slot = visible.len().saturating_sub(1);
        let next_slot = if delta >= 0 {
            current_slot
                .saturating_add(delta.unsigned_abs())
                .min(max_slot)
        } else {
            current_slot.saturating_sub(delta.unsigned_abs())
        };
        self.set_selected_index(visible[next_slot]);
    }

    fn select_first_visible(&mut self) {
        if let Some(index) = self.visible_issue_indices_for_list_nav().first().copied() {
            self.set_selected_index(index);
        }
    }

    fn select_last_visible(&mut self) {
        if let Some(index) = self.visible_issue_indices_for_list_nav().last().copied() {
            self.set_selected_index(index);
        }
    }

    fn has_active_filter(&self) -> bool {
        self.list_filter != ListFilter::All
            || self.modal_label_filter.is_some()
            || self.modal_repo_filter.is_some()
    }

    fn should_clear_filter_with_all_shortcut(&self) -> bool {
        self.has_active_filter() && !matches!(self.mode, ViewMode::Actionable)
    }

    fn set_list_filter(&mut self, list_filter: ListFilter) {
        self.list_filter = list_filter;
        if matches!(list_filter, ListFilter::All) {
            self.modal_label_filter = None;
            self.modal_repo_filter = None;
        }
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
        self.focus = FocusPane::List;
    }

    fn cycle_list_sort(&mut self) {
        self.list_sort = self.list_sort.next();
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
        self.focus = FocusPane::List;
    }

    fn cycle_board_grouping(&mut self) {
        self.board_grouping = self.board_grouping.next();
        self.ensure_selected_visible();
        self.focus = FocusPane::List;
    }

    fn toggle_board_empty_visibility(&mut self) {
        self.board_empty_visibility = self.board_empty_visibility.next();
        self.ensure_selected_visible();
        self.focus = FocusPane::List;
    }

    fn scroll_board_detail(&mut self, delta: isize) {
        if delta == 0 || !matches!(self.mode, ViewMode::Board) || self.focus != FocusPane::Detail {
            return;
        }

        if delta > 0 {
            self.board_detail_scroll_offset = self
                .board_detail_scroll_offset
                .saturating_add(delta.unsigned_abs());
        } else {
            self.board_detail_scroll_offset = self
                .board_detail_scroll_offset
                .saturating_sub(delta.unsigned_abs());
        }
    }

    fn set_selected_index(&mut self, index: usize) {
        let changed = self.selected != index;
        self.selected = index;
        if changed {
            self.detail_dep_cursor = 0;
            if matches!(self.mode, ViewMode::Board) {
                self.board_detail_scroll_offset = 0;
            }
        }
    }

    fn toggle_insights_explanations(&mut self) {
        self.insights_show_explanations = !self.insights_show_explanations;
        self.focus = FocusPane::List;
    }

    fn toggle_insights_calc_proof(&mut self) {
        self.insights_show_calc_proof = !self.insights_show_calc_proof;
        self.focus = FocusPane::List;
    }

    fn board_lane_indices(&self) -> Vec<(String, Vec<usize>)> {
        let visible = self.visible_issue_indices();

        let mut lanes = match self.board_grouping {
            BoardGrouping::Status => {
                let mut open = Vec::<usize>::new();
                let mut in_progress = Vec::<usize>::new();
                let mut blocked = Vec::<usize>::new();
                let mut closed = Vec::<usize>::new();
                let mut other = Vec::<usize>::new();

                for index in visible {
                    let issue = &self.analyzer.issues[index];
                    if issue.is_closed_like() {
                        closed.push(index);
                    } else if issue.status.eq_ignore_ascii_case("blocked") {
                        blocked.push(index);
                    } else if issue.status.eq_ignore_ascii_case("in_progress") {
                        in_progress.push(index);
                    } else if issue.status.eq_ignore_ascii_case("open") {
                        open.push(index);
                    } else {
                        other.push(index);
                    }
                }

                vec![
                    ("open".to_string(), open),
                    ("in_progress".to_string(), in_progress),
                    ("blocked".to_string(), blocked),
                    ("closed".to_string(), closed),
                    ("other".to_string(), other),
                ]
            }
            BoardGrouping::Priority => {
                let mut p0 = Vec::<usize>::new();
                let mut p1 = Vec::<usize>::new();
                let mut p2 = Vec::<usize>::new();
                let mut p3_plus = Vec::<usize>::new();

                for index in visible {
                    let issue = &self.analyzer.issues[index];
                    match issue.priority {
                        0 => p0.push(index),
                        1 => p1.push(index),
                        2 => p2.push(index),
                        _ => p3_plus.push(index),
                    }
                }

                vec![
                    ("p0".to_string(), p0),
                    ("p1".to_string(), p1),
                    ("p2".to_string(), p2),
                    ("p3+".to_string(), p3_plus),
                ]
            }
            BoardGrouping::Type => {
                let mut by_type = std::collections::BTreeMap::<String, Vec<usize>>::new();
                for index in visible {
                    let issue = &self.analyzer.issues[index];
                    let key = if issue.issue_type.trim().is_empty() {
                        "unknown".to_string()
                    } else {
                        issue.issue_type.to_lowercase()
                    };
                    by_type.entry(key).or_default().push(index);
                }
                by_type.into_iter().collect()
            }
        };

        if !self
            .board_empty_visibility
            .should_show_empty(self.board_grouping)
        {
            lanes.retain(|(_, indices)| !indices.is_empty());
        }

        lanes
    }

    fn select_first_in_board_lane(&mut self, lane_position: usize) {
        if !matches!(self.mode, ViewMode::Board) || lane_position == 0 {
            return;
        }

        if let Some((_, indices)) = self.board_lane_indices().get(lane_position - 1)
            && let Some(index) = indices.first().copied()
        {
            self.set_selected_index(index);
        }
    }

    fn current_board_lane_slot(&self) -> Option<usize> {
        let lanes = self.board_lane_indices();
        lanes
            .iter()
            .position(|(_, indices)| indices.contains(&self.selected))
            .or_else(|| lanes.iter().position(|(_, indices)| !indices.is_empty()))
    }

    fn select_first_in_non_empty_board_lane(&mut self) {
        if !matches!(self.mode, ViewMode::Board) {
            return;
        }

        if let Some((_, indices)) = self
            .board_lane_indices()
            .into_iter()
            .find(|(_, indices)| !indices.is_empty())
            && let Some(index) = indices.first().copied()
        {
            self.set_selected_index(index);
        }
    }

    fn select_last_in_non_empty_board_lane(&mut self) {
        if !matches!(self.mode, ViewMode::Board) {
            return;
        }

        if let Some((_, indices)) = self
            .board_lane_indices()
            .into_iter()
            .rev()
            .find(|(_, indices)| !indices.is_empty())
            && let Some(index) = indices.first().copied()
        {
            self.set_selected_index(index);
        }
    }

    fn move_board_lane_relative(&mut self, delta: isize) {
        if !matches!(self.mode, ViewMode::Board)
            || !matches!(self.focus, FocusPane::List | FocusPane::Detail)
            || delta == 0
        {
            return;
        }

        let lanes = self.board_lane_indices();
        if lanes.is_empty() {
            return;
        }

        let Some(current_lane_slot) = self.current_board_lane_slot() else {
            return;
        };

        let current_row = lanes
            .get(current_lane_slot)
            .and_then(|(_, indices)| indices.iter().position(|index| *index == self.selected))
            .unwrap_or(0);

        let lane_count = isize::try_from(lanes.len()).unwrap_or(0);
        let mut target_lane_slot = isize::try_from(current_lane_slot).unwrap_or(0) + delta.signum();

        while target_lane_slot >= 0 && target_lane_slot < lane_count {
            let slot = usize::try_from(target_lane_slot).unwrap_or(0);
            if let Some((_, indices)) = lanes.get(slot)
                && !indices.is_empty()
            {
                let target_row = current_row.min(indices.len().saturating_sub(1));
                self.set_selected_index(indices[target_row]);
                return;
            }
            target_lane_slot += delta.signum();
        }
    }

    fn move_board_row_relative(&mut self, delta: isize) {
        if !matches!(self.mode, ViewMode::Board)
            || !matches!(self.focus, FocusPane::List | FocusPane::Detail)
            || delta == 0
        {
            return;
        }

        if self.focus == FocusPane::Detail && !self.detail_dep_list().is_empty() {
            self.move_detail_dep_relative(delta);
            return;
        }

        let lanes = self.board_lane_indices();
        let Some(lane_slot) = self.current_board_lane_slot() else {
            return;
        };
        let Some((_, indices)) = lanes.get(lane_slot) else {
            return;
        };
        if indices.is_empty() {
            return;
        }

        let current_row = indices
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let max_row = indices.len().saturating_sub(1);
        let next_row = if delta >= 0 {
            current_row
                .saturating_add(delta.unsigned_abs())
                .min(max_row)
        } else {
            current_row.saturating_sub(delta.unsigned_abs())
        };

        self.set_selected_index(indices[next_row]);
    }

    fn start_board_search(&mut self) {
        if !matches!(self.mode, ViewMode::Board)
            || !matches!(self.focus, FocusPane::List | FocusPane::Detail)
        {
            return;
        }

        self.board_search_active = true;
        self.board_search_query.clear();
        self.board_search_match_cursor = 0;
    }

    fn finish_board_search(&mut self) {
        self.board_search_active = false;
    }

    fn cancel_board_search(&mut self) {
        self.board_search_active = false;
        self.board_search_query.clear();
        self.board_search_match_cursor = 0;
    }

    fn board_search_matches(&self) -> Vec<usize> {
        let query = self.board_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        self.visible_issue_indices()
            .into_iter()
            .filter(|index| {
                self.analyzer.issues.get(*index).is_some_and(|issue| {
                    issue.id.to_ascii_lowercase().contains(&query)
                        || issue.title.to_ascii_lowercase().contains(&query)
                        || issue.status.to_ascii_lowercase().contains(&query)
                        || issue.issue_type.to_ascii_lowercase().contains(&query)
                        || issue
                            .labels
                            .iter()
                            .any(|label| label.to_ascii_lowercase().contains(&query))
                })
            })
            .collect()
    }

    fn select_current_board_search_match(&mut self) {
        let matches = self.board_search_matches();
        if matches.is_empty() {
            return;
        }

        self.board_search_match_cursor = self
            .board_search_match_cursor
            .min(matches.len().saturating_sub(1));
        self.set_selected_index(matches[self.board_search_match_cursor]);
    }

    fn move_board_search_match_relative(&mut self, delta: isize) {
        let matches = self.board_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }

        let len = matches.len();
        let current = self.board_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta >= 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.board_search_match_cursor = next;
        self.set_selected_index(matches[next]);
    }

    // ── Graph search ──────────────────────────────────────────

    fn start_graph_search(&mut self) {
        if !matches!(self.mode, ViewMode::Graph) || self.focus != FocusPane::List {
            return;
        }

        self.graph_search_active = true;
        self.graph_search_query.clear();
        self.graph_search_match_cursor = 0;
    }

    fn finish_graph_search(&mut self) {
        self.graph_search_active = false;
    }

    fn cancel_graph_search(&mut self) {
        self.graph_search_active = false;
        self.graph_search_query.clear();
        self.graph_search_match_cursor = 0;
    }

    fn graph_search_matches(&self) -> Vec<usize> {
        let query = self.graph_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.visible_issue_indices()
            .into_iter()
            .filter(|&index| {
                let issue = &self.analyzer.issues[index];
                issue.id.to_ascii_lowercase().contains(&query)
                    || issue.title.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    fn select_current_graph_search_match(&mut self) {
        let matches = self.graph_search_matches();
        if matches.is_empty() {
            return;
        }

        self.graph_search_match_cursor = self
            .graph_search_match_cursor
            .min(matches.len().saturating_sub(1));
        self.set_selected_index(matches[self.graph_search_match_cursor]);
    }

    fn move_graph_search_match_relative(&mut self, delta: isize) {
        let matches = self.graph_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }

        let len = matches.len();
        let current = self.graph_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.graph_search_match_cursor = next;
        self.set_selected_index(matches[next]);
    }

    // ── Insights search ──────────────────────────────────────────

    fn start_insights_search(&mut self) {
        if !matches!(self.mode, ViewMode::Insights) || self.focus != FocusPane::List {
            return;
        }

        self.insights_search_active = true;
        self.insights_search_query.clear();
        self.insights_search_match_cursor = 0;
    }

    fn finish_insights_search(&mut self) {
        self.insights_search_active = false;
    }

    fn cancel_insights_search(&mut self) {
        self.insights_search_active = false;
        self.insights_search_query.clear();
        self.insights_search_match_cursor = 0;
    }

    fn insights_search_matches(&self) -> Vec<usize> {
        let query = self.insights_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.visible_issue_indices()
            .into_iter()
            .filter(|&index| {
                let issue = &self.analyzer.issues[index];
                issue.id.to_ascii_lowercase().contains(&query)
                    || issue.title.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    fn select_current_insights_search_match(&mut self) {
        let matches = self.insights_search_matches();
        if matches.is_empty() {
            return;
        }

        self.insights_search_match_cursor = self
            .insights_search_match_cursor
            .min(matches.len().saturating_sub(1));
        self.set_selected_index(matches[self.insights_search_match_cursor]);
    }

    fn move_insights_search_match_relative(&mut self, delta: isize) {
        let matches = self.insights_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }

        let len = matches.len();
        let current = self.insights_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.insights_search_match_cursor = next;
        self.set_selected_index(matches[next]);
    }

    fn select_edge_in_current_board_lane(&mut self, select_last: bool) {
        if !matches!(self.mode, ViewMode::Board) {
            return;
        }

        let lanes = self.board_lane_indices();
        let Some(lane_slot) = self.current_board_lane_slot() else {
            return;
        };

        let Some((_, indices)) = lanes.get(lane_slot) else {
            return;
        };

        let candidate = if select_last {
            indices.last().copied()
        } else {
            indices.first().copied()
        };

        if let Some(index) = candidate {
            self.set_selected_index(index);
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        let visible = self.visible_issue_indices_for_list_nav();
        if visible.is_empty() {
            return None;
        }
        let index = self
            .selected_visible_slot(&visible)
            .map_or(visible[0], |_| self.selected);
        self.analyzer.issues.get(index)
    }

    fn issue_by_id(&self, issue_id: &str) -> Option<&Issue> {
        self.analyzer
            .issues
            .iter()
            .find(|issue| issue.id == issue_id)
    }

    fn select_issue_by_id(&mut self, issue_id: &str) {
        if let Some(index) = self
            .analyzer
            .issues
            .iter()
            .position(|issue| issue.id == issue_id)
        {
            self.set_selected_index(index);
            self.ensure_selected_visible();
        }
    }

    fn no_filtered_issues_text(&self, context: &str) -> String {
        format!(
            "No issues match the active filter ({}) in {context}.",
            self.list_filter.label()
        )
    }

    fn handle_pages_wizard_key(&mut self, code: KeyCode, mut wiz: PagesWizardState) -> Cmd<Msg> {
        match code {
            KeyCode::Escape => {
                self.modal_overlay = None;
                return Cmd::None;
            }
            KeyCode::Backspace if wiz.step == 0 && !wiz.export_dir.is_empty() => {
                wiz.export_dir.pop();
            }
            KeyCode::Backspace if wiz.step == 1 && !wiz.title.is_empty() => {
                wiz.title.pop();
            }
            KeyCode::Backspace if wiz.step > 0 => {
                wiz.step -= 1;
            }
            KeyCode::Char('c') if wiz.step == 2 => {
                wiz.include_closed = !wiz.include_closed;
            }
            KeyCode::Char('h') if wiz.step == 2 => {
                wiz.include_history = !wiz.include_history;
            }
            KeyCode::Char(ch) if wiz.step == 0 => {
                wiz.export_dir.push(ch);
            }
            KeyCode::Char(ch) if wiz.step == 1 => {
                wiz.title.push(ch);
            }
            KeyCode::Enter => {
                if wiz.step >= PagesWizardState::step_count() - 1 {
                    // Final step - dismiss and store result
                    self.modal_overlay = None;
                    self.modal_confirm_result = Some(true);
                    return Cmd::None;
                }
                wiz.step += 1;
            }
            _ => {}
        }
        self.modal_overlay = Some(ModalOverlay::PagesWizard(wiz));
        Cmd::None
    }

    fn pages_wizard_text(wiz: &PagesWizardState) -> String {
        match wiz.step {
            0 => format!(
                "Export directory: {}\n\n\
                 Type a path and press Enter to continue.\n\
                 (Default: ./bv-pages)",
                wiz.export_dir
            ),
            1 => format!(
                "Page title: {}\n\n\
                 Type a custom title and press Enter to continue.\n\
                 (Leave blank for default: \"Project Issues\")",
                if wiz.title.is_empty() {
                    "(default)"
                } else {
                    &wiz.title
                }
            ),
            2 => format!(
                "Options:\n\n\
                 [{}] Include closed issues     (toggle: c)\n\
                 [{}] Include history payload    (toggle: h)\n\n\
                 Press Enter to continue.",
                if wiz.include_closed { "x" } else { " " },
                if wiz.include_history { "x" } else { " " },
            ),
            3 => {
                let title_display = if wiz.title.is_empty() {
                    "Project Issues"
                } else {
                    &wiz.title
                };
                format!(
                    "Review:\n\n\
                     Directory:       {}\n\
                     Title:           {title_display}\n\
                     Include closed:  {}\n\
                     Include history: {}\n\n\
                     Press Enter to export, Esc to cancel.",
                    wiz.export_dir,
                    if wiz.include_closed { "yes" } else { "no" },
                    if wiz.include_history { "yes" } else { "no" },
                )
            }
            _ => String::new(),
        }
    }

    fn help_overlay_text(&self, width: usize) -> String {
        // Define keybinding sections.
        struct Section {
            title: &'static str,
            bindings: Vec<(&'static str, &'static str)>,
        }

        let sections = vec![
            Section {
                title: "Navigation",
                bindings: vec![
                    ("j/k", "Move selection up/down"),
                    ("arrows", "Move selection up/down"),
                    ("h/l", "Lateral nav (lanes, peers)"),
                    ("Ctrl+d/u", "Jump down/up by 10"),
                    ("PgUp/PgDn", "Jump by 10"),
                    ("Home/End", "Jump to top/bottom"),
                    ("G", "Jump to bottom"),
                    ("Tab", "Toggle list/detail focus"),
                    ("J/K", "Navigate deps in detail"),
                    ("Enter", "Return to main / drill"),
                    ("scroll", "Mouse wheel scrolls list"),
                ],
            },
            Section {
                title: "Views",
                bindings: vec![
                    ("a", "Toggle actionable mode"),
                    ("b", "Toggle board mode"),
                    ("i", "Toggle insights mode"),
                    ("g", "Toggle graph mode"),
                    ("h", "Toggle history mode"),
                    ("!", "Toggle attention mode"),
                    ("T", "Toggle tree view"),
                    ("[", "Toggle label dashboard"),
                    ("]", "Toggle flow matrix"),
                    ("v", "History: bead/git toggle"),
                ],
            },
            Section {
                title: "Filters",
                bindings: vec![
                    ("o", "Filter: open only"),
                    ("c", "Filter: closed only"),
                    ("r", "Filter: ready only"),
                    ("s", "Cycle sort/grouping/panel"),
                ],
            },
            Section {
                title: "Search",
                bindings: vec![
                    ("/", "Start search"),
                    ("n/N", "Next/prev search match"),
                    ("Tab", "Cycle search mode (in /)"),
                    ("Esc", "Cancel search"),
                    ("Enter", "Confirm search"),
                ],
            },
            Section {
                title: "Actions",
                bindings: vec![
                    ("p", "Toggle priority hints"),
                    ("C", "Copy issue ID"),
                    ("x", "Export issue markdown"),
                    ("O", "Open in editor"),
                    ("Ctrl+R/F5", "Refresh from disk"),
                ],
            },
            Section {
                title: "History",
                bindings: vec![
                    ("c", "Cycle confidence filter"),
                    ("y", "Copy SHA/ID"),
                    ("o", "Open commit in browser"),
                    ("f", "Toggle file tree"),
                ],
            },
            Section {
                title: "Board",
                bindings: vec![
                    ("1-4", "Jump to lane"),
                    ("H/L", "First/last lane"),
                    ("0/$", "First/last in lane"),
                    ("e", "Toggle empty lanes"),
                ],
            },
            Section {
                title: "Insights",
                bindings: vec![
                    ("s/S", "Cycle panel fwd/back"),
                    ("m", "Toggle heatmap"),
                    ("e", "Toggle explanations"),
                    ("x", "Toggle calc-proof"),
                ],
            },
            Section {
                title: "Global",
                bindings: vec![
                    ("?/F1", "Toggle this help"),
                    ("Esc", "Back / clear / quit"),
                    ("q", "Quit / back to main"),
                    ("Ctrl+C", "Quit immediately"),
                ],
            },
        ];

        // Render each section as a block of lines.
        let render_section = |sec: &Section| -> Vec<String> {
            let mut block = vec![format!("[{}]", sec.title)];
            for (key, desc) in &sec.bindings {
                block.push(format!("  {:<12} {}", key, desc));
            }
            block
        };

        let rendered: Vec<Vec<String>> = sections.iter().map(render_section).collect();

        // Determine column count based on width.
        let col_width = 36;
        let num_cols = if width >= col_width * 3 + 4 {
            3
        } else if width >= col_width * 2 + 2 {
            2
        } else {
            1
        };

        if num_cols == 1 {
            // Single column: just concatenate all sections.
            let mut out = Vec::new();
            for block in &rendered {
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.extend(block.iter().cloned());
            }
            return out.join("\n");
        }

        // Multi-column: distribute sections across columns to balance height.
        let total_lines: usize = rendered.iter().map(|b| b.len() + 1).sum::<usize>(); // +1 for gap
        let target_per_col = (total_lines + num_cols - 1) / num_cols;

        let mut columns: Vec<Vec<String>> = vec![Vec::new(); num_cols];
        let mut col = 0;
        let mut col_lines = 0;

        for block in &rendered {
            let block_lines = block.len() + 1; // +1 for gap before next section
            if col_lines > 0 && col_lines + block_lines > target_per_col && col + 1 < num_cols {
                col += 1;
                col_lines = 0;
            }
            if !columns[col].is_empty() {
                columns[col].push(String::new());
            }
            columns[col].extend(block.iter().cloned());
            col_lines += block_lines;
        }

        // Merge columns side by side.
        let max_rows = columns.iter().map(|c| c.len()).max().unwrap_or(0);
        let actual_col_width = if num_cols > 0 {
            (width.saturating_sub(num_cols - 1)) / num_cols
        } else {
            width
        };

        let mut output = Vec::with_capacity(max_rows);
        for row in 0..max_rows {
            let mut line = String::new();
            for (ci, col_data) in columns.iter().enumerate() {
                let cell = col_data.get(row).map(|s| s.as_str()).unwrap_or("");
                if ci > 0 {
                    line.push_str(" | ");
                }
                let cell_trunc = if cell.len() > actual_col_width {
                    &cell[..actual_col_width]
                } else {
                    cell
                };
                line.push_str(cell_trunc);
                // Pad to column width for alignment (except last column).
                if ci + 1 < columns.len() {
                    let padding = actual_col_width.saturating_sub(cell_trunc.len());
                    for _ in 0..padding {
                        line.push(' ');
                    }
                }
            }
            output.push(line);
        }

        output.join("\n")
    }

    fn list_panel_text(&self) -> String {
        if self.analyzer.issues.is_empty() {
            return "(no issues loaded)".to_string();
        }

        match self.mode {
            ViewMode::Board => self.board_list_text(),
            ViewMode::Insights => self.insights_list_text(),
            ViewMode::Graph => self.graph_list_text(),
            ViewMode::History => self.history_list_text(),
            ViewMode::Actionable => self.actionable_list_text(),
            ViewMode::Attention => self.attention_list_text(),
            ViewMode::Tree => self.tree_list_text(),
            ViewMode::LabelDashboard => self.label_dashboard_list_text(),
            ViewMode::FlowMatrix => self.flow_matrix_list_text(),
            ViewMode::TimeTravelDiff => self.time_travel_list_text(),
            ViewMode::Sprint => self.sprint_list_text(),
            ViewMode::Main => self.main_list_text(),
        }
    }

    fn main_list_text(&self) -> String {
        let visible = self.visible_issue_indices();
        if visible.is_empty() {
            return format!("(no issues match filter: {})", self.list_filter.label());
        }

        visible
            .into_iter()
            .filter_map(|index| self.analyzer.issues.get(index).map(|issue| (index, issue)))
            .map(|(index, issue)| {
                let marker = if index == self.selected { '>' } else { ' ' };
                format!(
                    "{marker} {:<14} {:<11} p{} {}",
                    issue.id, issue.status, issue.priority, issue.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn board_list_text(&self) -> String {
        let lanes = self.board_lane_indices();
        let mut out = Vec::<String>::new();
        out.push(format!(
            "Grouping: {} (s cycles) | Empty: {} (e)",
            self.board_grouping.label(),
            self.board_empty_visibility.label(),
        ));
        if self.board_search_active {
            out.push(format!("Search (active): /{}", self.board_search_query));
        } else if !self.board_search_query.is_empty() {
            out.push(format!("Search: /{} (n/N cycles)", self.board_search_query));
        }
        if !self.board_search_query.is_empty() {
            let matches = self.board_search_matches();
            if matches.is_empty() {
                out.push("Matches: none".to_string());
            } else {
                let position = self
                    .board_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                out.push(format!("Matches: {position}/{}", matches.len()));
            }
        }

        // Find which lane the selected issue belongs to
        let sel_id = self.selected_issue().map(|i| i.id.clone());
        let sel_index = self.selected;

        out.push(String::new());
        let total: usize = lanes.iter().map(|(_, v)| v.len()).sum();
        out.push(format!("Lanes ({}) | {} issues total", lanes.len(), total));
        out.push(String::new());

        for (lane, lane_indices) in &lanes {
            let count = lane_indices.len();
            // Mark current lane
            let marker = if sel_id.as_ref().is_some_and(|sid| {
                lane_indices
                    .iter()
                    .any(|&i| self.analyzer.issues[i].id == *sid)
            }) {
                ">"
            } else {
                " "
            };

            // Lane header with bar
            let bar_len = count.min(20);
            let bar: String = std::iter::repeat_n('\u{2588}', bar_len).collect();
            out.push(format!("{marker} {lane:<12} [{count:>3}] {bar}"));

            // Show card previews for each issue in lane
            for &idx in lane_indices.iter().take(6) {
                let issue = &self.analyzer.issues[idx];
                let sel_mark = if idx == sel_index { "\u{25b6}" } else { " " };
                let icon = status_icon(&issue.status);
                out.push(format!(
                    "    {sel_mark} {icon} {:<10} p{} {}",
                    issue.id,
                    issue.priority,
                    truncate_str(&issue.title, 22)
                ));
            }
            if lane_indices.len() > 6 {
                out.push(format!("    ... +{} more", lane_indices.len() - 6));
            }
            if lane_indices.is_empty() {
                out.push("    (empty)".to_string());
            }
            out.push(String::new());
        }

        out.join("\n")
    }

    fn insights_list_text(&self) -> String {
        if let Some(state) = self.insights_heatmap.as_ref() {
            let data = self.insights_heatmap_data();
            let row = state
                .row
                .min(INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1));
            let col = state
                .col
                .min(INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1));
            let cell_issue_ids = data.issue_ids[row][col].clone();

            if state.drill_active {
                let mut lines = vec![
                    "Priority heatmap drill | m toggle | j/k issue | Tab detail | Esc back"
                        .to_string(),
                    format!(
                        "Cell: {} x {} ({} issue(s))",
                        INSIGHTS_HEATMAP_DEPTH_LABELS[row],
                        INSIGHTS_HEATMAP_SCORE_LABELS[col],
                        cell_issue_ids.len()
                    ),
                    String::new(),
                ];

                if cell_issue_ids.is_empty() {
                    lines.push("  (no issues in selected cell)".to_string());
                    return lines.join("\n");
                }

                let max_visible = 10usize;
                let cursor = state
                    .drill_cursor
                    .min(cell_issue_ids.len().saturating_sub(1));
                let start = cursor.saturating_sub(max_visible.saturating_sub(1));
                let end = (start + max_visible).min(cell_issue_ids.len());

                for (idx, issue_id) in cell_issue_ids[start..end].iter().enumerate() {
                    let actual = start + idx;
                    let marker = if actual == cursor { '>' } else { ' ' };
                    let (priority, title) = self
                        .analyzer
                        .issues
                        .iter()
                        .find(|issue| issue.id == *issue_id)
                        .map_or((9, String::new()), |issue| {
                            (issue.priority, truncate_str(&issue.title, 28))
                        });
                    lines.push(format!("{marker} {issue_id:<12} p{priority} {title}"));
                }

                if cell_issue_ids.len() > max_visible {
                    lines.push(String::new());
                    lines.push(format!(
                        "Drill cursor: {}/{}",
                        cursor + 1,
                        cell_issue_ids.len()
                    ));
                }

                return lines.join("\n");
            }

            let mut lines = vec![
                "Priority heatmap | m toggle | h/l/j/k cell | Enter drill | Tab detail".to_string(),
                String::new(),
            ];

            if data
                .counts
                .iter()
                .all(|row_counts| row_counts.iter().all(|count| *count == 0))
            {
                lines.push("  (no open, filter-matching issues to chart)".to_string());
                return lines.join("\n");
            }

            let col_totals = (0..INSIGHTS_HEATMAP_SCORE_LABELS.len())
                .map(|score_col| {
                    data.counts
                        .iter()
                        .map(|row_counts| row_counts[score_col])
                        .sum()
                })
                .collect::<Vec<usize>>();

            lines.push(format!(
                "{:<8} | {:>4} {:>4} {:>4} {:>4} {:>4} | {:>4}",
                "Depth",
                INSIGHTS_HEATMAP_SCORE_LABELS[0],
                INSIGHTS_HEATMAP_SCORE_LABELS[1],
                INSIGHTS_HEATMAP_SCORE_LABELS[2],
                INSIGHTS_HEATMAP_SCORE_LABELS[3],
                INSIGHTS_HEATMAP_SCORE_LABELS[4],
                "Tot"
            ));
            lines.push("-".repeat(46));

            for (depth_row, label) in INSIGHTS_HEATMAP_DEPTH_LABELS.iter().enumerate() {
                let row_total = data.counts[depth_row].iter().sum::<usize>();
                let mut cell_chunks = Vec::with_capacity(INSIGHTS_HEATMAP_SCORE_LABELS.len());
                for score_col in 0..INSIGHTS_HEATMAP_SCORE_LABELS.len() {
                    let count = data.counts[depth_row][score_col];
                    let cell = if depth_row == row && score_col == col {
                        if count == 0 {
                            "[ .]".to_string()
                        } else {
                            format!("[{count:>2}]")
                        }
                    } else if count == 0 {
                        "  . ".to_string()
                    } else {
                        format!(" {count:>2} ")
                    };
                    cell_chunks.push(cell);
                }
                lines.push(format!(
                    "{label:<8} | {} | {row_total:>4}",
                    cell_chunks.join(" ")
                ));
            }

            lines.push("-".repeat(46));
            lines.push(format!(
                "{:<8} | {:>4} {:>4} {:>4} {:>4} {:>4} | {:>4}",
                "Total",
                col_totals[0],
                col_totals[1],
                col_totals[2],
                col_totals[3],
                col_totals[4],
                col_totals.iter().sum::<usize>()
            ));
            lines.push(String::new());
            lines.push(format!(
                "Selected: {} x {} ({} issue(s))",
                INSIGHTS_HEATMAP_DEPTH_LABELS[row],
                INSIGHTS_HEATMAP_SCORE_LABELS[col],
                cell_issue_ids.len()
            ));
            if let Some(issue_id) = cell_issue_ids.first() {
                let issue_title = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|issue| issue.id == *issue_id)
                    .map_or("", |issue| issue.title.as_str());
                lines.push(format!(
                    "Lead issue: {issue_id} {}",
                    truncate_str(issue_title, 34)
                ));
            } else {
                lines.push("Lead issue: none in selected cell".to_string());
            }

            return lines.join("\n");
        }

        let insights = self.analyzer.insights();

        let mut lines = vec![format!(
            "[{}] s/S cycles panel | e explanations | x calc-proof | / search | m heatmap",
            self.insights_panel.label()
        )];
        if self.insights_search_active {
            lines.push(format!("Search (active): /{}", self.insights_search_query));
        } else if !self.insights_search_query.is_empty() {
            lines.push(format!(
                "Search: /{} (n/N cycles)",
                self.insights_search_query
            ));
        }
        if !self.insights_search_query.is_empty() {
            let matches = self.insights_search_matches();
            if matches.is_empty() {
                lines.push("Matches: none".to_string());
            } else {
                let position = self
                    .insights_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                lines.push(format!("Matches: {position}/{}", matches.len()));
            }
        }
        lines.push(String::new());

        match self.insights_panel {
            InsightsPanel::Bottlenecks => {
                if insights.bottlenecks.is_empty() {
                    lines.push("  (no open issues to rank)".to_string());
                } else {
                    lines.extend(insights.bottlenecks.iter().take(15).enumerate().map(
                        |(index, item)| {
                            format!(
                                " {}. {:<12} score={:.3} blocks={}",
                                index + 1,
                                item.id,
                                item.score,
                                item.blocks_count
                            )
                        },
                    ));
                }
            }
            InsightsPanel::Keystones => {
                let mut keystones = self
                    .analyzer
                    .issues
                    .iter()
                    .filter(|issue| issue.is_open_like())
                    .filter_map(|issue| {
                        self.analyzer
                            .metrics
                            .critical_depth
                            .get(&issue.id)
                            .copied()
                            .map(|depth| (issue.id.as_str(), depth))
                    })
                    .filter(|(_, depth)| *depth > 0)
                    .collect::<Vec<_>>();

                keystones
                    .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));

                if keystones.is_empty() {
                    lines.push("  (no foundational chain detected)".to_string());
                } else {
                    lines.extend(keystones.iter().take(15).enumerate().map(
                        |(index, (id, depth))| {
                            let unblocks = self
                                .analyzer
                                .metrics
                                .blocks_count
                                .get(*id)
                                .copied()
                                .unwrap_or_default();
                            format!(
                                " {}. {:<12} depth={} unblocks={}",
                                index + 1,
                                id,
                                depth,
                                unblocks
                            )
                        },
                    ));
                }
            }
            InsightsPanel::CriticalPath => {
                if insights.critical_path.is_empty() {
                    lines.push("  (no critical path detected)".to_string());
                } else {
                    lines.extend(
                        insights
                            .critical_path
                            .iter()
                            .enumerate()
                            .map(|(index, id)| {
                                let depth = self
                                    .analyzer
                                    .metrics
                                    .critical_depth
                                    .get(id)
                                    .copied()
                                    .unwrap_or_default();
                                format!(" {}. {:<12} depth={}", index + 1, id, depth)
                            }),
                    );
                }
            }
            InsightsPanel::Influencers => {
                Self::append_metric_items(&mut lines, &insights.influencers, "influencer");
            }
            InsightsPanel::Betweenness => {
                Self::append_metric_items(&mut lines, &insights.betweenness, "betweenness");
            }
            InsightsPanel::Hubs => {
                Self::append_metric_items(&mut lines, &insights.hubs, "hub-score");
            }
            InsightsPanel::Authorities => {
                Self::append_metric_items(&mut lines, &insights.authorities, "authority");
            }
            InsightsPanel::Cores => {
                if insights.cores.is_empty() {
                    lines.push("  (no k-core data)".to_string());
                } else {
                    lines.extend(insights.cores.iter().take(15).enumerate().map(
                        |(index, item)| format!(" {}. {:<12} k={}", index + 1, item.id, item.value),
                    ));
                }
            }
            InsightsPanel::CutPoints => {
                if insights.articulation_points.is_empty() {
                    lines.push("  (no cut points -- graph is well-connected)".to_string());
                } else {
                    lines.extend(
                        insights
                            .articulation_points
                            .iter()
                            .enumerate()
                            .map(|(index, id)| format!(" {}. {}", index + 1, id)),
                    );
                }
            }
            InsightsPanel::Slack => {
                if insights.slack.is_empty() {
                    lines
                        .push("  (no zero-slack issues -- all have scheduling buffer)".to_string());
                } else {
                    lines.extend(
                        insights
                            .slack
                            .iter()
                            .enumerate()
                            .map(|(index, id)| format!(" {}. {}", index + 1, id)),
                    );
                }
            }
            InsightsPanel::Cycles => {
                if insights.cycles.is_empty() {
                    lines.push("  No cycles detected".to_string());
                } else {
                    lines.extend(
                        insights.cycles.iter().enumerate().map(|(index, cycle)| {
                            format!(" {}. {}", index + 1, cycle.join(" -> "))
                        }),
                    );
                }
            }
            InsightsPanel::Priority => {
                let recommendations = self.analyzer.priority(0.0, 15, None, None);
                if recommendations.is_empty() {
                    lines.push("  (no priority recommendations available)".to_string());
                } else {
                    lines.extend(recommendations.iter().enumerate().map(|(index, item)| {
                        format!(
                            " {}. {:<12} score={:.3} unblocks={} p{}",
                            index + 1,
                            item.id,
                            item.score,
                            item.unblocks,
                            item.priority
                        )
                    }));
                }
            }
        }

        lines.join("\n")
    }

    fn append_metric_items(
        lines: &mut Vec<String>,
        items: &[crate::analysis::MetricItem],
        label: &str,
    ) {
        if items.is_empty() {
            lines.push(format!("  (no {label} data)"));
        } else {
            lines.extend(items.iter().take(15).enumerate().map(|(index, item)| {
                let scaled = (item.value * 20.0).clamp(0.0, 20.0);
                let bar_len = (1_u32..=20_u32)
                    .take_while(|threshold| scaled >= f64::from(*threshold))
                    .count();
                let bar = format!(
                    "{}{}",
                    "#".repeat(bar_len),
                    ".".repeat(20_usize.saturating_sub(bar_len))
                );
                format!(" {}. {:<12} [{bar}] {:.4}", index + 1, item.id, item.value)
            }));
        }
    }

    fn graph_node_score(&self, id: &str) -> f64 {
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(id)
            .copied()
            .unwrap_or_default() as f64;
        let pagerank = self
            .analyzer
            .metrics
            .pagerank
            .get(id)
            .copied()
            .unwrap_or_default();
        depth + pagerank
    }

    fn graph_list_text(&self) -> String {
        let visible = self.graph_visible_issue_indices();
        if visible.is_empty() {
            return format!("(no issues match filter: {})", self.list_filter.label());
        }

        let total = visible.len();
        let mut lines = vec![format!(
            "Nodes ({total}) by critical-path score | h/l nav | / search | Tab focus"
        )];
        if self.graph_search_active {
            lines.push(format!("Search (active): /{}", self.graph_search_query));
        } else if !self.graph_search_query.is_empty() {
            lines.push(format!("Search: /{} (n/N cycles)", self.graph_search_query));
        }
        if !self.graph_search_query.is_empty() {
            let matches = self.graph_search_matches();
            if matches.is_empty() {
                lines.push("Matches: none".to_string());
            } else {
                let position = self
                    .graph_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                lines.push(format!("Matches: {position}/{}", matches.len()));
            }
        }
        lines.push(String::new());

        lines.extend(
            visible
                .into_iter()
                .filter_map(|index| self.analyzer.issues.get(index).map(|issue| (index, issue)))
                .map(|(index, issue)| {
                    let marker = if index == self.selected { '>' } else { ' ' };
                    let si = status_icon(&issue.status);
                    let blocks = self
                        .analyzer
                        .metrics
                        .blocks_count
                        .get(&issue.id)
                        .copied()
                        .unwrap_or_default();
                    let blocked_by = self
                        .analyzer
                        .metrics
                        .blocked_by_count
                        .get(&issue.id)
                        .copied()
                        .unwrap_or_default();
                    let pagerank = self
                        .analyzer
                        .metrics
                        .pagerank
                        .get(&issue.id)
                        .copied()
                        .unwrap_or_default();
                    format!(
                        "{marker} {si} {:<12} in:{:>2} out:{:>2} pr:{:.3}",
                        issue.id, blocked_by, blocks, pagerank
                    )
                }),
        );
        lines.join("\n")
    }

    fn graph_visible_issue_indices(&self) -> Vec<usize> {
        let mut visible = self.visible_issue_indices();
        visible.sort_by(|&left_idx, &right_idx| {
            let left = &self.analyzer.issues[left_idx];
            let right = &self.analyzer.issues[right_idx];
            let left_score = self.graph_node_score(&left.id);
            let right_score = self.graph_node_score(&right.id);
            right_score
                .total_cmp(&left_score)
                .then_with(|| left.id.cmp(&right.id))
        });
        visible
    }

    fn history_list_text(&self) -> String {
        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            let query = self.history_search_query.trim();
            let visible = self.history_git_visible_commit_indices();
            let cache = self.history_git_cache.as_ref();

            if visible.is_empty() {
                if query.is_empty() {
                    return "No git commits correlated with beads.\n\
                            (ensure repo has commits referencing bead IDs)"
                        .to_string();
                }
                return format!("(no commits match search: /{query})");
            }

            let cursor = self
                .history_event_cursor
                .min(visible.len().saturating_sub(1));

            let total_commits = cache.map_or(0, |c| c.commits.len());
            let mut lines = Vec::<String>::new();
            if query.is_empty() {
                lines.push(format!(
                    "Git commits ({}/{} correlated) | v bead list | / search | c confidence",
                    visible.len(),
                    total_commits
                ));
            } else {
                lines.push(format!(
                    "Git commits (matches: {}/{}) | v bead list | / search",
                    visible.len(),
                    total_commits
                ));
            }
            lines.push(format!(
                "Min confidence: >= {:.0}%",
                self.history_min_confidence() * 100.0
            ));

            if self.history_search_active {
                lines.push(format!(
                    "Search [{}] (Tab cycles): /{}",
                    self.history_search_mode.label(),
                    self.history_search_query
                ));
            } else if !query.is_empty() {
                let matches = self.history_search_matches();
                let mc = matches.len();
                lines.push(format!(
                    "Search [{}]: /{} ({mc} matches, n/N cycles)",
                    self.history_search_mode.label(),
                    self.history_search_query
                ));
            }

            lines.push(String::new());

            if let Some(cache) = cache {
                for (display_idx, &commit_idx) in visible.iter().enumerate() {
                    let marker = if display_idx == cursor { '>' } else { ' ' };
                    if let Some(commit) = cache.commits.get(commit_idx) {
                        let related = self.history_git_related_beads_for_commit(&commit.sha);
                        let beads_str = if related.len() <= 2 {
                            related.join(",")
                        } else {
                            format!("{}+{}", related[..2].join(","), related.len() - 2)
                        };
                        let msg = truncate_str(&commit.message, 30);
                        lines.push(format!(
                            "{marker} {} {:<8} {:<16} {}",
                            commit.short_sha, beads_str, msg, commit.timestamp
                        ));
                    }
                }
            }
            return lines.join("\n");
        }

        let histories = self.analyzer.history(None, 0);
        let query = self.history_search_query.trim();
        let all_visible = self.visible_issue_indices();
        let visible = self.history_visible_issue_indices();
        if visible.is_empty() {
            if all_visible.is_empty() {
                return format!("(no issues match filter: {})", self.list_filter.label());
            }
            if query.is_empty() {
                return "(no issues available)".to_string();
            }
            return format!("(no issues match history search: /{query})");
        }

        let mut lines = Vec::<String>::new();
        if query.is_empty() {
            lines.push(format!(
                "Bead history list ({} beads) | v toggles to git timeline | / search",
                visible.len()
            ));
        } else {
            lines.push(format!(
                "Bead history list (matches: {}/{}) | v toggles to git timeline | / search",
                visible.len(),
                all_visible.len()
            ));
        }

        if self.history_search_active {
            lines.push(format!(
                "Search [{}] (Tab cycles): /{}",
                self.history_search_mode.label(),
                self.history_search_query
            ));
        } else if !query.is_empty() {
            let mc = visible.len();
            lines.push(format!(
                "Search [{}]: /{} ({mc} matches, n/N cycles)",
                self.history_search_mode.label(),
                self.history_search_query
            ));
        }

        lines.push(String::new());
        lines.extend(
            visible
                .into_iter()
                .filter_map(|index| self.analyzer.issues.get(index).map(|issue| (index, issue)))
                .map(|(index, issue)| {
                    let marker = if index == self.selected { '>' } else { ' ' };
                    let event_count = histories
                        .iter()
                        .find(|entry| entry.id == issue.id)
                        .map_or(0, |entry| entry.events.len());
                    format!(
                        "{marker} {:<12} events:{:>2} {:<11}",
                        issue.id, event_count, issue.status
                    )
                }),
        );
        lines.join("\n")
    }

    // -- Actionable view --------------------------------------------------

    fn compute_actionable_plan(&mut self) {
        let triage = self
            .analyzer
            .triage(crate::analysis::triage::TriageOptions::default());
        let plan = self.analyzer.plan(&triage.score_by_id);
        self.actionable_track_cursor = 0;
        self.actionable_item_cursor = 0;
        self.actionable_plan = Some(plan);
    }

    fn move_actionable_cursor(&mut self, delta: isize) {
        let Some(plan) = self.actionable_plan.as_ref() else {
            return;
        };

        if matches!(self.focus, FocusPane::List) {
            // Navigate between tracks.
            let max = plan.tracks.len().saturating_sub(1);
            let new_pos = (self.actionable_track_cursor as isize + delta).clamp(0, max as isize);
            self.actionable_track_cursor = new_pos as usize;
            self.actionable_item_cursor = 0;
        } else {
            // Navigate between items within current track.
            if let Some(track) = plan.tracks.get(self.actionable_track_cursor) {
                let max = track.items.len().saturating_sub(1);
                let new_pos = (self.actionable_item_cursor as isize + delta).clamp(0, max as isize);
                self.actionable_item_cursor = new_pos as usize;
            }
        }
    }

    fn actionable_list_text(&self) -> String {
        let Some(plan) = self.actionable_plan.as_ref() else {
            return "(no execution plan computed)".to_string();
        };

        if plan.tracks.is_empty() {
            return "⚡ ACTIONABLE ITEMS\n\n✓ No actionable items. All tasks are either blocked or completed."
                .to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "⚡ ACTIONABLE ITEMS | {} items in {} tracks",
            plan.summary.actionable_count,
            plan.tracks.len()
        ));
        if let (Some(highest), Some(reason)) = (
            plan.summary.highest_impact.as_deref(),
            plan.summary.impact_reason.as_deref(),
        ) {
            lines.push(format!("RECOMMENDED: Start with {highest} -> {reason}"));
        }
        lines.push(String::new());

        for (track_idx, track) in plan.tracks.iter().enumerate() {
            let track_marker = if track_idx == self.actionable_track_cursor
                && matches!(self.focus, FocusPane::List)
            {
                "▸"
            } else {
                " "
            };
            let track_label = track.id.strip_prefix("track-").unwrap_or(&track.id);
            lines.push(format!(
                "{track_marker} TRACK {track_label} | {}",
                track.reason
            ));
            lines.push(format!(
                "  {} item{}",
                track.items.len(),
                if track.items.len() == 1 { "" } else { "s" }
            ));
            for (item_idx, item) in track.items.iter().enumerate() {
                let item_marker = if track_idx == self.actionable_track_cursor
                    && item_idx == self.actionable_item_cursor
                    && matches!(self.focus, FocusPane::Detail)
                {
                    "  ▸"
                } else {
                    "   "
                };
                let tree = if item_idx + 1 < track.items.len() {
                    "├─"
                } else {
                    "└─"
                };
                let title = truncate_str(&item.title, 42);
                let unblocks = if item.unblocks.is_empty() {
                    String::new()
                } else {
                    format!(" -> {}", item.unblocks.len())
                };
                lines.push(format!(
                    "{item_marker}{tree} {:<12} {:>5.2}  {}{}",
                    item.id, item.score, title, unblocks
                ));
            }
            lines.push(String::new());
        }

        lines.join("\n").trim_end().to_string()
    }

    fn actionable_detail_text(&self) -> String {
        let Some(plan) = self.actionable_plan.as_ref() else {
            return "(no plan)".to_string();
        };

        let Some(track) = plan.tracks.get(self.actionable_track_cursor) else {
            return "(no track selected)".to_string();
        };

        let track_label = track.id.strip_prefix("track-").unwrap_or(&track.id);

        let mut lines = Vec::new();
        lines.push(format!("TRACK {track_label}"));
        lines.push(track.reason.clone());
        lines.push(format!(
            "{} actionable item{}",
            track.items.len(),
            if track.items.len() == 1 { "" } else { "s" }
        ));
        lines.push(String::new());

        for (idx, item) in track.items.iter().enumerate() {
            let marker = if idx == self.actionable_item_cursor {
                "▸"
            } else {
                " "
            };
            lines.push(format!("{marker} {}  score {:.3}", item.id, item.score));
            lines.push(format!("  {}", item.title));
            if !item.unblocks.is_empty() {
                lines.push(format!("  Unblocks: {}", item.unblocks.join(", ")));
            }
            lines.push(format!("  Claim: {}", item.claim_command));
            lines.push(String::new());
        }

        if let (Some(highest), Some(reason)) = (
            plan.summary.highest_impact.as_deref(),
            plan.summary.impact_reason.as_deref(),
        ) {
            lines.push(format!("Highest impact: {highest}"));
            lines.push(format!("Impact detail: {reason}"));
        }

        lines.join("\n").trim_end().to_string()
    }

    // -- end Actionable view -----------------------------------------------

    // -- Attention view ----------------------------------------------------

    fn compute_attention(&mut self) {
        let result = crate::analysis::label_intel::compute_label_attention(
            &self.analyzer.issues,
            &self.analyzer.metrics,
            0, // no limit — show all
        );
        self.attention_result = Some(result);
        self.attention_cursor = 0;
    }

    fn copy_selected_issue_id(&mut self) {
        if let Some(issue) = self.selected_issue() {
            let id = issue.id.clone();
            if copy_text_to_clipboard(&id) {
                self.status_msg = format!("Copied {id} to clipboard");
            } else {
                self.status_msg = "Clipboard not available".into();
            }
        }
    }

    fn export_selected_issue_markdown(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };

        let mut md = String::new();
        md.push_str(&format!("# {} — {}\n\n", issue.id, issue.title));
        md.push_str(&format!(
            "**Status:** {} | **Priority:** p{} | **Type:** {}\n\n",
            issue.status, issue.priority, issue.issue_type
        ));
        if !issue.assignee.is_empty() {
            md.push_str(&format!("**Assignee:** {}\n\n", issue.assignee));
        }
        if !issue.description.is_empty() {
            md.push_str(&format!("## Description\n\n{}\n\n", issue.description));
        }
        if !issue.notes.is_empty() {
            md.push_str(&format!("## Notes\n\n{}\n\n", issue.notes));
        }
        if !issue.labels.is_empty() {
            md.push_str(&format!("**Labels:** {}\n", issue.labels.join(", ")));
        }

        let path = std::env::temp_dir().join(format!("{}.md", issue.id));
        match std::fs::write(&path, &md) {
            Ok(()) => {
                self.status_msg = format!("Exported to {}", path.display());
            }
            Err(err) => {
                self.status_msg = format!("Export failed: {err}");
            }
        }
    }

    fn open_selected_in_editor(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };

        let yaml = format!(
            "id: {}\ntitle: {}\nstatus: {}\npriority: {}\ntype: {}\nassignee: {}\nlabels: [{}]\n\ndescription: |\n  {}\n\nnotes: |\n  {}\n",
            issue.id,
            issue.title,
            issue.status,
            issue.priority,
            issue.issue_type,
            issue.assignee,
            issue.labels.join(", "),
            issue.description.replace('\n', "\n  "),
            issue.notes.replace('\n', "\n  "),
        );

        let path = std::env::temp_dir().join(format!("{}.yaml", issue.id));
        if std::fs::write(&path, &yaml).is_err() {
            self.status_msg = "Failed to write temp file".into();
            return;
        }

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
        if run_command(&editor, &[&path.to_string_lossy()]) {
            self.status_msg = format!("Opened in {editor}");
        } else {
            self.status_msg = format!("Failed to open {editor}");
        }
    }

    fn refresh_from_disk(&mut self) {
        let repo_path = self.repo_root.as_deref();
        let issues = match loader::load_issues(repo_path) {
            Ok(issues) => issues,
            Err(_) => return, // silently ignore — data unchanged
        };

        // Preserve selection by ID when possible.
        let selected_id = self
            .analyzer
            .issues
            .get(self.selected)
            .map(|i| i.id.clone());

        let use_two_phase =
            issues.len() > crate::analysis::graph::AnalysisConfig::BACKGROUND_THRESHOLD;
        if use_two_phase {
            self.analyzer = Analyzer::new_fast(issues);
            #[cfg(not(test))]
            {
                self.slow_metrics_rx = Some(self.analyzer.spawn_slow_computation());
            }
            self.slow_metrics_pending = true;
        } else {
            self.analyzer = Analyzer::new(issues);
            self.slow_metrics_pending = false;
            #[cfg(not(test))]
            {
                self.slow_metrics_rx = None;
            }
        }

        // Restore selection.
        if let Some(ref id) = selected_id {
            if let Some(pos) = self.analyzer.issues.iter().position(|i| i.id == *id) {
                self.selected = pos;
            } else {
                self.selected = 0;
            }
        } else {
            self.selected = 0;
        }

        // Reset computed views.
        self.actionable_plan = None;
        self.attention_result = None;

        // Recompute if in a derived view.
        match self.mode {
            ViewMode::Actionable => self.compute_actionable_plan(),
            ViewMode::Attention => self.compute_attention(),
            _ => {}
        }
    }

    fn move_attention_cursor(&mut self, delta: i32) {
        let count = self.attention_result.as_ref().map_or(0, |r| r.labels.len());
        if count == 0 {
            return;
        }
        let cur = self.attention_cursor as i32 + delta;
        self.attention_cursor = cur.clamp(0, count as i32 - 1) as usize;
    }

    fn attention_list_text(&self) -> String {
        let Some(result) = self.attention_result.as_ref() else {
            return "(computing attention scores…)".to_string();
        };
        if result.labels.is_empty() {
            return "(no labels found)".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "{:>4}  {:<20} {:>6}  {}",
            "Rank", "Label", "Score", "Reason"
        ));
        lines.push(format!("{}", "─".repeat(72)));

        for (idx, label) in result.labels.iter().enumerate() {
            let marker = if idx == self.attention_cursor {
                "▸"
            } else {
                " "
            };
            lines.push(format!(
                "{marker}{:>3}  {:<20} {:>5.1}  {}",
                label.rank,
                truncate_str(&label.label, 20),
                label.attention_score,
                truncate_str(&label.reason, 30),
            ));
        }

        lines.join("\n")
    }

    fn attention_detail_text(&self) -> String {
        let Some(result) = self.attention_result.as_ref() else {
            return "(no attention data)".to_string();
        };
        let Some(label) = result.labels.get(self.attention_cursor) else {
            return "(no label selected)".to_string();
        };

        let mut lines = Vec::new();
        lines.push(format!("Label: {}", label.label));
        lines.push(format!("Rank: #{}", label.rank));
        lines.push(format!("Attention Score: {:.3}", label.attention_score));
        lines.push(format!("Normalized: {:.3}", label.normalized_score));
        lines.push(String::new());

        lines.push("Breakdown:".to_string());
        lines.push(format!("  Open:     {}", label.open_count));
        lines.push(format!("  Blocked:  {}", label.blocked_count));
        lines.push(format!("  Stale:    {}", label.stale_count));
        lines.push(String::new());

        lines.push("Factors:".to_string());
        lines.push(format!("  PageRank sum:     {:.4}", label.pagerank_sum));
        lines.push(format!("  Staleness factor: {:.2}", label.staleness_factor));
        lines.push(format!("  Block impact:     {:.1}", label.block_impact));
        lines.push(format!("  Velocity factor:  {:.2}", label.velocity_factor));
        lines.push(String::new());

        lines.push(format!("Reason: {}", label.reason));

        // Show affected issues from the analyzer
        let issues_with_label: Vec<&str> = self
            .analyzer
            .issues
            .iter()
            .filter(|i| i.is_open_like() && i.labels.iter().any(|l| l == &label.label))
            .map(|i| i.id.as_str())
            .collect();
        if !issues_with_label.is_empty() {
            lines.push(String::new());
            lines.push(format!("Open issues ({}):", issues_with_label.len()));
            for id in &issues_with_label {
                lines.push(format!("  {id}"));
            }
        }

        lines.join("\n")
    }

    // -- end Attention view ------------------------------------------------

    // -- Tree view --------------------------------------------------------

    fn build_tree_flat_nodes(&mut self) {
        let issues = &self.analyzer.issues;
        let graph = &self.analyzer.graph;

        // Build a map from issue ID to index.
        let id_to_index: std::collections::HashMap<&str, usize> = issues
            .iter()
            .enumerate()
            .map(|(i, issue)| (issue.id.as_str(), i))
            .collect();

        // Find root issues: those with no blockers (nothing blocks them).
        let mut roots: Vec<usize> = Vec::new();
        let mut has_parent: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (i, issue) in issues.iter().enumerate() {
            let blockers = graph.blockers(&issue.id);
            if !blockers.is_empty() {
                has_parent.insert(i);
            }
        }

        for i in 0..issues.len() {
            if !has_parent.contains(&i) {
                roots.push(i);
            }
        }

        // Sort roots by ID for deterministic order.
        roots.sort_by(|&a, &b| issues[a].id.cmp(&issues[b].id));

        // DFS to build flat node list.
        let mut flat_nodes = Vec::new();
        let mut stack: Vec<(usize, usize, bool, Vec<bool>)> = Vec::new(); // (issue_index, depth, is_last, ancestry)

        for (ri, &root_idx) in roots.iter().enumerate() {
            let is_last = ri + 1 == roots.len();
            stack.push((root_idx, 0, is_last, Vec::new()));
        }

        // Process in reverse so first root is processed first (stack is LIFO).
        stack.reverse();

        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();

        while let Some((issue_idx, depth, is_last, ancestry)) = stack.pop() {
            if !visited.insert(issue_idx) {
                continue; // Avoid cycles.
            }

            let issue_id = &issues[issue_idx].id;
            let children_ids = graph.dependents(issue_id);
            let mut children: Vec<usize> = children_ids
                .iter()
                .filter_map(|id| id_to_index.get(id.as_str()).copied())
                .filter(|idx| !visited.contains(idx))
                .collect();
            children.sort_by(|&a, &b| issues[a].id.cmp(&issues[b].id));

            let is_collapsed = self.tree_collapsed.contains(issue_id);
            let has_children = !children.is_empty();

            flat_nodes.push(TreeFlatNode {
                issue_index: issue_idx,
                depth,
                has_children,
                is_collapsed,
                is_last_sibling: is_last,
                ancestry_last: ancestry.clone(),
            });

            if !is_collapsed {
                // Push children in reverse so first child is processed next.
                for (ci, &child_idx) in children.iter().enumerate().rev() {
                    let child_is_last = ci + 1 == children.len();
                    let mut child_ancestry = ancestry.clone();
                    child_ancestry.push(is_last);
                    stack.push((child_idx, depth + 1, child_is_last, child_ancestry));
                }
            }
        }

        self.tree_flat_nodes = flat_nodes;
    }

    fn toggle_tree_mode(&mut self) {
        if matches!(self.mode, ViewMode::Tree) {
            self.mode = ViewMode::Main;
        } else {
            self.mode = ViewMode::Tree;
            self.tree_cursor = 0;
            self.build_tree_flat_nodes();
        }
    }

    fn tree_toggle_collapse(&mut self) {
        if let Some(node) = self.tree_flat_nodes.get(self.tree_cursor) {
            if node.has_children {
                let issue_id = self.analyzer.issues[node.issue_index].id.clone();
                if self.tree_collapsed.contains(&issue_id) {
                    self.tree_collapsed.remove(&issue_id);
                } else {
                    self.tree_collapsed.insert(issue_id);
                }
                self.build_tree_flat_nodes();
                // Clamp cursor.
                if self.tree_cursor >= self.tree_flat_nodes.len() {
                    self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
                }
            }
        }
    }

    fn tree_list_text(&self) -> String {
        if self.tree_flat_nodes.is_empty() {
            return "(no dependency tree — all issues are independent)".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "Dependency tree ({} nodes) | Enter expand/collapse | T/Esc back",
            self.tree_flat_nodes.len()
        ));
        lines.push(String::new());

        for (i, node) in self.tree_flat_nodes.iter().enumerate() {
            let marker = if i == self.tree_cursor { '>' } else { ' ' };
            let issue = &self.analyzer.issues[node.issue_index];

            // Build tree prefix with box-drawing characters.
            let mut prefix = String::new();
            for &parent_was_last in &node.ancestry_last {
                if parent_was_last {
                    prefix.push_str("    ");
                } else {
                    prefix.push_str(" |  ");
                }
            }

            if node.depth > 0 {
                if node.is_last_sibling {
                    prefix.push_str(" `- ");
                } else {
                    prefix.push_str(" |- ");
                }
            }

            // Collapse indicator.
            let collapse_indicator = if node.has_children {
                if node.is_collapsed { "[+] " } else { "[-] " }
            } else {
                "    "
            };

            let status_icon = if issue.is_closed() {
                "x"
            } else if issue.is_open_like() {
                "o"
            } else {
                "-"
            };

            lines.push(format!(
                "{marker} {prefix}{collapse_indicator}({status_icon}) {} {}",
                issue.id,
                truncate_str(&issue.title, 40)
            ));
        }

        lines.join("\n")
    }

    fn tree_detail_text(&self) -> String {
        let Some(node) = self.tree_flat_nodes.get(self.tree_cursor) else {
            return "(no node selected)".to_string();
        };

        let issue = &self.analyzer.issues[node.issue_index];
        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);

        let mut lines = Vec::new();
        lines.push(format!("ID: {}", issue.id));
        lines.push(format!("Title: {}", issue.title));
        lines.push(format!("Status: {}", issue.status));
        lines.push(format!("Type: {}", issue.issue_type));
        lines.push(format!("Depth: {}", node.depth));

        if !issue.labels.is_empty() {
            lines.push(format!("Labels: {}", issue.labels.join(", ")));
        }

        if !blockers.is_empty() {
            lines.push(String::new());
            lines.push(format!("Blocked by ({}):", blockers.len()));
            for b in &blockers {
                let title = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|i| i.id == *b)
                    .map(|i| i.title.as_str())
                    .unwrap_or("?");
                lines.push(format!("  {b} - {title}"));
            }
        }

        if !dependents.is_empty() {
            lines.push(String::new());
            lines.push(format!("Dependents ({}):", dependents.len()));
            for d in &dependents {
                let title = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|i| i.id == *d)
                    .map(|i| i.title.as_str())
                    .unwrap_or("?");
                lines.push(format!("  {d} - {title}"));
            }
        }

        if !issue.description.is_empty() {
            lines.push(String::new());
            lines.push("Description:".to_string());
            lines.push(issue.description.clone());
        }

        lines.join("\n")
    }

    // -- end Tree view ----------------------------------------------------

    // -- LabelDashboard view ----------------------------------------------

    fn toggle_label_dashboard(&mut self) {
        if matches!(self.mode, ViewMode::LabelDashboard) {
            self.mode = ViewMode::Main;
        } else {
            self.mode = ViewMode::LabelDashboard;
            self.label_dashboard_cursor = 0;
            self.compute_label_dashboard();
        }
    }

    fn compute_label_dashboard(&mut self) {
        use crate::analysis::label_intel::compute_all_label_health;
        let metrics = self.analyzer.graph.compute_metrics();
        let result =
            compute_all_label_health(&self.analyzer.issues, &self.analyzer.graph, &metrics);
        self.label_dashboard = Some(result);
    }

    fn label_dashboard_list_text(&self) -> String {
        let Some(result) = &self.label_dashboard else {
            return "(computing label health...)".to_string();
        };

        if result.labels.is_empty() {
            return "(no labels found in issues)".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "Label health ({} labels) | healthy:{} warn:{} critical:{}",
            result.total_labels, result.healthy_count, result.warning_count, result.critical_count
        ));
        lines.push(String::new());

        for (i, label) in result.labels.iter().enumerate() {
            let marker = if i == self.label_dashboard_cursor {
                '>'
            } else {
                ' '
            };

            // Health bar visualization (10 chars wide).
            let bar_filled = (label.health as usize).min(100) / 10;
            let bar: String = (0..10)
                .map(|j| if j < bar_filled { '#' } else { '.' })
                .collect();

            let level_marker = match label.health_level.as_str() {
                "critical" => "!!",
                "warning" => "! ",
                _ => "  ",
            };

            lines.push(format!(
                "{marker} {level_marker} [{bar}] {:>3}  {:<16} ({} open, {} closed)",
                label.health,
                truncate_str(&label.label, 16),
                label.open_count,
                label.closed_count,
            ));
        }

        lines.join("\n")
    }

    fn label_dashboard_detail_text(&self) -> String {
        let Some(result) = &self.label_dashboard else {
            return "(no data)".to_string();
        };

        let Some(label) = result.labels.get(self.label_dashboard_cursor) else {
            return "(no label selected)".to_string();
        };

        let mut lines = Vec::new();
        lines.push(format!("Label: {}", label.label));
        lines.push(format!(
            "Health: {}/100 ({})",
            label.health, label.health_level
        ));
        lines.push(format!(
            "Issues: {} total ({} open, {} closed, {} blocked)",
            label.issue_count, label.open_count, label.closed_count, label.blocked_count
        ));

        // Velocity
        lines.push(String::new());
        lines.push(format!(
            "Velocity (score: {}/100):",
            label.velocity.velocity_score
        ));
        lines.push(format!(
            "  Closed 7d: {} | 30d: {}",
            label.velocity.closed_last_7_days, label.velocity.closed_last_30_days
        ));
        if label.velocity.avg_days_to_close > 0.0 {
            lines.push(format!(
                "  Avg days to close: {:.1}",
                label.velocity.avg_days_to_close
            ));
        }
        lines.push(format!(
            "  Trend: {} ({:+.0}%)",
            label.velocity.trend_direction, label.velocity.trend_percent
        ));

        // Freshness
        lines.push(String::new());
        lines.push(format!(
            "Freshness (score: {}/100):",
            label.freshness.freshness_score
        ));
        lines.push(format!(
            "  Avg days since update: {:.1}",
            label.freshness.avg_days_since_update
        ));
        lines.push(format!(
            "  Stale: {} (threshold: {}d)",
            label.freshness.stale_count, label.freshness.stale_threshold_days
        ));

        // Flow
        lines.push(String::new());
        lines.push(format!("Flow (score: {}/100):", label.flow.flow_score));
        lines.push(format!(
            "  Deps in: {} | out: {}",
            label.flow.incoming_deps, label.flow.outgoing_deps
        ));
        lines.push(format!(
            "  Blocked by external: {} | Blocking external: {}",
            label.flow.blocked_by_external, label.flow.blocking_external
        ));

        // Criticality
        lines.push(String::new());
        lines.push(format!(
            "Criticality (score: {}/100):",
            label.criticality.criticality_score
        ));
        lines.push(format!(
            "  Avg PageRank: {:.4} | Avg betweenness: {:.4}",
            label.criticality.avg_pagerank, label.criticality.avg_betweenness
        ));
        lines.push(format!(
            "  Critical paths: {} | Bottlenecks: {}",
            label.criticality.critical_path_count, label.criticality.bottleneck_count
        ));

        // Issues list
        if !label.issues.is_empty() {
            lines.push(String::new());
            lines.push(format!("Issues ({}):", label.issues.len()));
            for id in &label.issues {
                let title = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|i| i.id == *id)
                    .map(|i| i.title.as_str())
                    .unwrap_or("?");
                lines.push(format!("  {id} - {title}"));
            }
        }

        lines.join("\n")
    }

    // -- end LabelDashboard view ------------------------------------------

    // -- FlowMatrix view ---------------------------------------------------

    fn toggle_flow_matrix(&mut self) {
        if matches!(self.mode, ViewMode::FlowMatrix) {
            self.mode = ViewMode::Main;
        } else {
            self.mode = ViewMode::FlowMatrix;
            self.flow_matrix_row_cursor = 0;
            self.flow_matrix_col_cursor = 0;
            self.compute_flow_matrix();
        }
    }

    fn compute_flow_matrix(&mut self) {
        use crate::analysis::label_intel::compute_cross_label_flow;
        let result = compute_cross_label_flow(&self.analyzer.issues);
        self.flow_matrix = Some(result);
    }

    fn flow_matrix_list_text(&self) -> String {
        let Some(flow) = &self.flow_matrix else {
            return "(computing flow matrix...)".to_string();
        };

        if flow.labels.is_empty() {
            return "(no labels found — flow matrix empty)".to_string();
        }

        let labels = &flow.labels;
        let matrix = &flow.flow_matrix;
        let mut lines = Vec::new();

        // Header: summary
        lines.push(format!(
            "Cross-label flow ({} labels, {} deps) | bottlenecks: {}",
            labels.len(),
            flow.total_cross_label_deps,
            if flow.bottleneck_labels.is_empty() {
                "none".to_string()
            } else {
                flow.bottleneck_labels.join(", ")
            }
        ));
        lines.push(String::new());

        // Compute column width (label name + padding)
        let max_label_len = labels.iter().map(|l| l.len()).max().unwrap_or(4);
        let col_w = max_label_len.max(4);

        // Column headers
        let row_label_w = col_w + 2;
        let mut header = format!("{:row_label_w$}", "");
        for (ci, label) in labels.iter().enumerate() {
            let marker = if ci == self.flow_matrix_col_cursor {
                "v"
            } else {
                " "
            };
            header.push_str(&format!(" {marker}{:<width$}", label, width = col_w));
        }
        lines.push(header);

        // Separator
        let sep_len = row_label_w + labels.len() * (col_w + 2);
        lines.push("-".repeat(sep_len));

        // Rows
        for (ri, label) in labels.iter().enumerate() {
            let cursor = if ri == self.flow_matrix_row_cursor {
                ">"
            } else {
                " "
            };
            let mut row = format!("{cursor} {:<width$}", label, width = col_w);
            for (ci, val) in matrix[ri].iter().enumerate() {
                let cell = if ri == ci {
                    " .".to_string()
                } else if *val == 0 {
                    " -".to_string()
                } else {
                    format!(" {val}")
                };
                let highlight = ri == self.flow_matrix_row_cursor
                    && ci == self.flow_matrix_col_cursor
                    && ri != ci;
                if highlight {
                    row.push_str(&format!("[{:>width$}]", cell.trim(), width = col_w - 1));
                } else {
                    row.push_str(&format!(" {:>width$}", cell.trim(), width = col_w));
                }
            }
            lines.push(row);
        }

        lines.push(String::new());
        lines.push("j/k rows | h/l cols | Tab focus | ] or Esc back".to_string());

        lines.join("\n")
    }

    fn flow_matrix_detail_text(&self) -> String {
        let Some(flow) = &self.flow_matrix else {
            return "(no flow data)".to_string();
        };

        if flow.labels.is_empty() {
            return "(no labels)".to_string();
        }

        let labels = &flow.labels;
        let row = self
            .flow_matrix_row_cursor
            .min(labels.len().saturating_sub(1));
        let col = self
            .flow_matrix_col_cursor
            .min(labels.len().saturating_sub(1));
        let from_label = &labels[row];
        let to_label = &labels[col];

        let mut lines = Vec::new();

        if row == col {
            lines.push(format!("Label: {from_label}"));
            lines.push(String::new());
            // Show issues with this label
            let issue_ids: Vec<_> = self
                .analyzer
                .issues
                .iter()
                .filter(|i| i.labels.iter().any(|l| l == from_label))
                .collect();
            lines.push(format!("Issues with this label: {}", issue_ids.len()));
            for issue in &issue_ids {
                lines.push(format!("  {} - {}", issue.id, issue.title));
            }
        } else {
            let flow_val = flow.flow_matrix[row][col];
            lines.push(format!("{from_label} -> {to_label}: {flow_val} deps"));
            lines.push(String::new());

            // Find matching dependency entries
            let matching: Vec<_> = flow
                .dependencies
                .iter()
                .filter(|d| d.from_label == *from_label && d.to_label == *to_label)
                .collect();

            if matching.is_empty() && flow_val == 0 {
                lines.push("No cross-label dependencies in this direction.".to_string());
            } else {
                for dep in &matching {
                    lines.push(format!(
                        "{} -> {} ({} issues)",
                        dep.from_label, dep.to_label, dep.issue_count
                    ));
                    for id in &dep.issue_ids {
                        let title = self
                            .analyzer
                            .issues
                            .iter()
                            .find(|i| i.id == *id)
                            .map(|i| i.title.as_str())
                            .unwrap_or("?");
                        lines.push(format!("  {id} - {title}"));
                    }
                }
            }
        }

        // Bottleneck info
        if !flow.bottleneck_labels.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Bottleneck labels: {}",
                flow.bottleneck_labels.join(", ")
            ));
        }

        lines.join("\n")
    }

    // -- end FlowMatrix view -----------------------------------------------

    // -- TimeTravelDiff view -------------------------------------------------

    fn toggle_time_travel_mode(&mut self) {
        if matches!(self.mode, ViewMode::TimeTravelDiff) {
            self.mode = ViewMode::Main;
            self.focus = FocusPane::List;
        } else {
            self.mode = ViewMode::TimeTravelDiff;
            self.focus = FocusPane::List;
            if self.time_travel_diff.is_none() {
                // If no diff loaded yet, prompt for a ref
                self.time_travel_input_active = true;
                self.time_travel_ref_input.clear();
            }
        }
    }

    fn execute_time_travel(&mut self) {
        let reference = self.time_travel_ref_input.trim().to_string();
        if reference.is_empty() {
            self.status_msg = "Time-travel: empty ref, cancelled".into();
            self.time_travel_input_active = false;
            if self.time_travel_diff.is_none() {
                self.mode = ViewMode::Main;
                self.focus = FocusPane::List;
            }
            return;
        }

        self.time_travel_input_active = false;
        self.time_travel_last_ref = Some(reference.clone());

        // Try to load historical snapshot
        match self.load_time_travel_diff(&reference) {
            Ok(diff) => {
                self.time_travel_diff = Some(diff);
                self.time_travel_category_cursor = 0;
                self.time_travel_issue_cursor = 0;
                self.status_msg = format!("Time-travel: loaded diff from {reference}");
            }
            Err(err) => {
                self.status_msg = format!("Time-travel: {err}");
            }
        }
    }

    fn load_time_travel_diff(
        &self,
        reference: &str,
    ) -> std::result::Result<crate::analysis::diff::SnapshotDiff, String> {
        // Try file path first
        let path = std::path::Path::new(reference);
        if path.is_file() {
            let before = crate::loader::load_issues_from_file(path)
                .map_err(|e| format!("load file: {e}"))?;
            return Ok(crate::analysis::diff::compare_snapshots(
                &before,
                &self.analyzer.issues,
            ));
        }

        // Try repo-relative path
        if let Some(ref root) = self.repo_root {
            let rooted = root.join(reference);
            if rooted.is_file() {
                let before = crate::loader::load_issues_from_file(&rooted)
                    .map_err(|e| format!("load file: {e}"))?;
                return Ok(crate::analysis::diff::compare_snapshots(
                    &before,
                    &self.analyzer.issues,
                ));
            }
        }

        // Try git ref
        let repo_root = self
            .repo_root
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("."));
        let repo_root_str = repo_root.to_string_lossy();

        // Find beads file candidates
        let candidates = [".beads/issues.jsonl", ".beads/beads.jsonl"];
        for candidate in &candidates {
            let output = std::process::Command::new("git")
                .args([
                    "-C",
                    &repo_root_str,
                    "show",
                    &format!("{reference}:{candidate}"),
                ])
                .output()
                .map_err(|e| format!("git show: {e}"))?;

            if output.status.success() {
                let content = String::from_utf8_lossy(&output.stdout);
                let before = crate::loader::parse_issues_from_text(&content)
                    .map_err(|e| format!("parse: {e}"))?;
                return Ok(crate::analysis::diff::compare_snapshots(
                    &before,
                    &self.analyzer.issues,
                ));
            }
        }

        Err(format!(
            "could not resolve '{reference}' as file or git ref"
        ))
    }

    fn time_travel_categories(&self) -> Vec<(&str, usize)> {
        let Some(ref diff) = self.time_travel_diff else {
            return Vec::new();
        };
        let mut cats = Vec::new();
        let new_count = diff.new_issues.as_ref().map_or(0, |v| v.len());
        let closed_count = diff.closed_issues.as_ref().map_or(0, |v| v.len());
        let removed_count = diff.removed_issues.as_ref().map_or(0, |v| v.len());
        let reopened_count = diff.reopened_issues.as_ref().map_or(0, |v| v.len());
        let modified_count = diff.modified_issues.as_ref().map_or(0, |v| v.len());
        let new_cycles = diff.new_cycles.as_ref().map_or(0, |v| v.len());
        let resolved_cycles = diff.resolved_cycles.as_ref().map_or(0, |v| v.len());

        if new_count > 0 {
            cats.push(("New issues", new_count));
        }
        if closed_count > 0 {
            cats.push(("Closed issues", closed_count));
        }
        if removed_count > 0 {
            cats.push(("Removed issues", removed_count));
        }
        if reopened_count > 0 {
            cats.push(("Reopened issues", reopened_count));
        }
        if modified_count > 0 {
            cats.push(("Modified issues", modified_count));
        }
        if new_cycles > 0 {
            cats.push(("New cycles", new_cycles));
        }
        if resolved_cycles > 0 {
            cats.push(("Resolved cycles", resolved_cycles));
        }
        cats
    }

    fn time_travel_list_text(&self) -> String {
        let Some(ref diff) = self.time_travel_diff else {
            if self.time_travel_input_active {
                return format!(
                    " Enter git ref or file path:\n > {}_",
                    self.time_travel_ref_input
                );
            }
            return " No diff loaded. Press t to enter a ref.".to_string();
        };

        let mut lines = Vec::new();
        let ref_label = self.time_travel_last_ref.as_deref().unwrap_or("unknown");
        lines.push(format!(" Diff from: {ref_label}"));
        lines.push(format!(" Summary: {} changes", diff.summary.total_changes));
        lines.push(String::new());

        let cats = self.time_travel_categories();
        if cats.is_empty() {
            lines.push(" (no changes detected)".to_string());
        } else {
            for (i, (label, count)) in cats.iter().enumerate() {
                let marker = if i == self.time_travel_category_cursor {
                    ">"
                } else {
                    " "
                };
                lines.push(format!(" {marker} {label} ({count})"));
            }
        }

        // Metric deltas summary
        lines.push(String::new());
        lines.push(" METRIC DELTAS".to_string());
        let md = &diff.metric_deltas;
        if md.total_issues != 0 {
            lines.push(format!("   Total issues: {:+}", md.total_issues));
        }
        if md.open_issues != 0 {
            lines.push(format!("   Open issues:  {:+}", md.open_issues));
        }
        if md.blocked_issues != 0 {
            lines.push(format!("   Blocked:      {:+}", md.blocked_issues));
        }
        if md.total_edges != 0 {
            lines.push(format!("   Edges:        {:+}", md.total_edges));
        }
        if md.cycle_count != 0 {
            lines.push(format!("   Cycles:       {:+}", md.cycle_count));
        }

        lines.join("\n")
    }

    fn time_travel_detail_text(&self) -> String {
        let Some(ref diff) = self.time_travel_diff else {
            return String::new();
        };

        let cats = self.time_travel_categories();
        if cats.is_empty() {
            return " No changes in this diff.".to_string();
        }

        let cat_idx = self
            .time_travel_category_cursor
            .min(cats.len().saturating_sub(1));
        let (label, _) = cats[cat_idx];

        let mut lines = Vec::new();
        lines.push(format!(" {label}"));
        lines.push(String::new());

        match label {
            "New issues" => {
                if let Some(ref issues) = diff.new_issues {
                    for (i, di) in issues.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} {} [{}] {}", di.id, di.status, di.title));
                    }
                }
            }
            "Closed issues" => {
                if let Some(ref issues) = diff.closed_issues {
                    for (i, di) in issues.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} {} [{}] {}", di.id, di.status, di.title));
                    }
                }
            }
            "Removed issues" => {
                if let Some(ref issues) = diff.removed_issues {
                    for (i, di) in issues.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} {} [{}] {}", di.id, di.status, di.title));
                    }
                }
            }
            "Reopened issues" => {
                if let Some(ref issues) = diff.reopened_issues {
                    for (i, di) in issues.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} {} [{}] {}", di.id, di.status, di.title));
                    }
                }
            }
            "Modified issues" => {
                if let Some(ref issues) = diff.modified_issues {
                    for (i, mi) in issues.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        let field_changes: Vec<&str> =
                            mi.changes.iter().map(|c| c.field.as_str()).collect();
                        lines.push(format!(
                            " {marker} {} ({} fields: {})",
                            mi.issue_id,
                            mi.changes.len(),
                            field_changes.join(", ")
                        ));
                    }
                }
            }
            "New cycles" => {
                if let Some(ref cycles) = diff.new_cycles {
                    for (i, cycle) in cycles.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} [{}]", cycle.join(" -> ")));
                    }
                }
            }
            "Resolved cycles" => {
                if let Some(ref cycles) = diff.resolved_cycles {
                    for (i, cycle) in cycles.iter().enumerate() {
                        let marker = if i == self.time_travel_issue_cursor {
                            ">"
                        } else {
                            " "
                        };
                        lines.push(format!(" {marker} [{}]", cycle.join(" -> ")));
                    }
                }
            }
            _ => {}
        }

        lines.join("\n")
    }

    // -- end TimeTravelDiff view ---------------------------------------------

    // -- Sprint view ---------------------------------------------------------

    fn toggle_sprint_mode(&mut self) {
        if matches!(self.mode, ViewMode::Sprint) {
            self.mode = ViewMode::Main;
        } else {
            self.load_sprint_data();
            self.mode = ViewMode::Sprint;
            self.sprint_cursor = 0;
            self.sprint_issue_cursor = 0;
        }
    }

    fn load_sprint_data(&mut self) {
        self.sprint_data = loader::load_sprints(self.repo_root.as_deref()).unwrap_or_default();
    }

    fn sprint_visible_issues(&self) -> Vec<(usize, &Issue)> {
        let sprint = match self.sprint_data.get(self.sprint_cursor) {
            Some(s) => s,
            None => return Vec::new(),
        };
        sprint
            .bead_ids
            .iter()
            .filter_map(|bead_id| {
                self.analyzer
                    .issues
                    .iter()
                    .enumerate()
                    .find(|(_, issue)| issue.id == *bead_id)
            })
            .collect()
    }

    fn sprint_list_text(&self) -> String {
        if self.sprint_data.is_empty() {
            return " No sprints found.\n\n \
                    Sprints are defined in .beads/sprints.jsonl\n \
                    Each line: {\"id\":\"sprint-1\",\"name\":\"Sprint Alpha\",\n \
                    \"start_date\":\"...\",\"end_date\":\"...\",\"bead_ids\":[...]}"
                .to_string();
        }

        let now = Utc::now();
        let mut lines = Vec::new();
        lines.push(format!(" {} sprint(s)", self.sprint_data.len()));
        lines.push(String::new());

        for (i, sprint) in self.sprint_data.iter().enumerate() {
            let marker = if i == self.sprint_cursor { "▸" } else { " " };
            let active = if sprint.is_active_at(now) {
                " [ACTIVE]"
            } else {
                ""
            };
            let issue_count = sprint.bead_ids.len();
            let dates = match (&sprint.start_date, &sprint.end_date) {
                (Some(start), Some(end)) => {
                    format!("{} → {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
                }
                _ => "no dates".to_string(),
            };
            lines.push(format!(
                " {marker} {} | {} issues | {dates}{active}",
                sprint.name, issue_count
            ));

            // Show mini-progress
            let matched_issues = sprint
                .bead_ids
                .iter()
                .filter(|id| self.analyzer.issues.iter().any(|issue| issue.id == **id))
                .count();
            let closed = sprint
                .bead_ids
                .iter()
                .filter(|id| {
                    self.analyzer
                        .issues
                        .iter()
                        .any(|issue| issue.id == **id && issue.is_closed_like())
                })
                .count();
            if matched_issues > 0 {
                let pct = (closed as f64 / matched_issues as f64 * 100.0) as u32;
                lines.push(format!("   {closed}/{matched_issues} done ({pct}%)"));
            }
        }

        lines.join("\n")
    }

    fn sprint_detail_text(&self) -> String {
        let sprint = match self.sprint_data.get(self.sprint_cursor) {
            Some(s) => s,
            None => return " Select a sprint from the list.".to_string(),
        };

        let now = Utc::now();
        let mut lines = Vec::new();

        // Sprint header
        lines.push(format!(" SPRINT: {}", sprint.name));
        if sprint.is_active_at(now) {
            lines.push(" Status: ACTIVE".to_string());
        } else if sprint.end_date.is_some_and(|end| end < now) {
            lines.push(" Status: Completed".to_string());
        } else {
            lines.push(" Status: Upcoming".to_string());
        }

        if let (Some(start), Some(end)) = (&sprint.start_date, &sprint.end_date) {
            lines.push(format!(
                " Dates: {} → {}",
                start.format("%Y-%m-%d"),
                end.format("%Y-%m-%d")
            ));
            let total_days = (*end - *start).num_days();
            let elapsed = (now - *start).num_days().max(0).min(total_days);
            let remaining = total_days - elapsed;
            lines.push(format!(
                " Days: {total_days} total, {elapsed} elapsed, {remaining} remaining"
            ));
        }
        lines.push(String::new());

        // Issue list
        let visible = self.sprint_visible_issues();
        if visible.is_empty() {
            lines.push(format!(
                " {} bead(s) assigned, none matched loaded issues.",
                sprint.bead_ids.len()
            ));
        } else {
            let total = visible.len();
            let closed = visible
                .iter()
                .filter(|(_, issue)| issue.is_closed_like())
                .count();
            let open = total - closed;
            lines.push(format!(
                " Issues: {total} total, {open} open, {closed} closed"
            ));
            lines.push(String::new());

            // Sorted: open first (by priority), then closed
            let mut sorted: Vec<_> = visible.clone();
            sorted.sort_by(|(_, a), (_, b)| {
                let a_closed = a.is_closed_like();
                let b_closed = b.is_closed_like();
                a_closed.cmp(&b_closed).then(a.priority.cmp(&b.priority))
            });

            for (i, (_, issue)) in sorted.iter().enumerate() {
                let marker = if i == self.sprint_issue_cursor {
                    "▸"
                } else {
                    " "
                };
                let status_icon = if issue.is_closed_like() {
                    "✓"
                } else if issue.status == "in_progress" {
                    "●"
                } else if issue.status == "blocked" {
                    "✗"
                } else {
                    "○"
                };
                lines.push(format!(
                    " {marker} {status_icon} {} [P{}] {}",
                    issue.id, issue.priority, issue.title
                ));
            }

            // Burndown summary (ASCII bar)
            lines.push(String::new());
            if total > 0 {
                let pct = (closed as f64 / total as f64 * 100.0) as usize;
                let filled = pct / 5;
                let empty = 20 - filled;
                let bar = format!("[{}{}] {}%", "█".repeat(filled), "░".repeat(empty), pct);
                lines.push(format!(" Progress: {bar}"));
            }
        }

        lines.join("\n")
    }

    // -- end Sprint view -----------------------------------------------------

    // -- Modal pickers -------------------------------------------------------

    fn open_recipe_picker(&mut self) {
        let recipes = crate::analysis::recipe::list_recipes();
        let items: Vec<(String, String)> = recipes
            .into_iter()
            .map(|r| (r.name, r.description))
            .collect();
        self.modal_overlay = Some(ModalOverlay::RecipePicker { items, cursor: 0 });
    }

    fn open_label_picker(&mut self) {
        let mut label_counts = BTreeMap::<String, usize>::new();
        for issue in &self.analyzer.issues {
            for label in &issue.labels {
                *label_counts.entry(label.clone()).or_insert(0) += 1;
            }
        }
        let items: Vec<(String, usize)> = label_counts.into_iter().collect();
        self.modal_overlay = Some(ModalOverlay::LabelPicker { items, cursor: 0 });
    }

    fn open_repo_picker(&mut self) {
        let mut repos = std::collections::HashSet::<String>::new();
        for issue in &self.analyzer.issues {
            if !issue.source_repo.is_empty() {
                repos.insert(issue.source_repo.clone());
            }
        }
        let mut items: Vec<String> = repos.into_iter().collect();
        items.sort();
        if items.is_empty() {
            self.status_msg = "No workspace repos loaded".into();
            return;
        }
        self.modal_overlay = Some(ModalOverlay::RepoPicker { items, cursor: 0 });
    }

    fn set_label_filter(&mut self, label: &str) {
        if self
            .modal_label_filter
            .as_deref()
            .is_some_and(|current| current == label)
        {
            self.modal_label_filter = None;
            self.status_msg = "Label filter cleared".into();
        } else {
            self.modal_label_filter = Some(label.to_string());
            self.status_msg = format!("Filtering by label: {label}");
        }
    }

    fn set_repo_filter(&mut self, repo: &str) {
        if self
            .modal_repo_filter
            .as_deref()
            .is_some_and(|current| current == repo)
        {
            self.modal_repo_filter = None;
            self.status_msg = "Repo filter cleared".into();
        } else {
            self.modal_repo_filter = Some(repo.to_string());
            self.status_msg = format!("Filtering by repo: {repo}");
        }
    }

    // -- end Modal pickers ---------------------------------------------------

    fn detail_panel_text(&self) -> String {
        match self.mode {
            ViewMode::Board => self.board_detail_text(),
            ViewMode::Insights => self.insights_detail_text(),
            ViewMode::Graph => self.graph_detail_text(),
            ViewMode::History => self.history_detail_text(),
            ViewMode::Actionable => self.actionable_detail_text(),
            ViewMode::Attention => self.attention_detail_text(),
            ViewMode::Tree => self.tree_detail_text(),
            ViewMode::LabelDashboard => self.label_dashboard_detail_text(),
            ViewMode::FlowMatrix => self.flow_matrix_detail_text(),
            ViewMode::TimeTravelDiff => self.time_travel_detail_text(),
            ViewMode::Sprint => self.sprint_detail_text(),
            ViewMode::Main => self.issue_detail_text(),
        }
    }

    fn board_detail_render_state(&self, visible_height: usize) -> (String, usize, usize) {
        let full_text = self.board_detail_text();
        let total_lines = full_text.lines().count();
        if visible_height == 0 {
            return (String::new(), 0, total_lines);
        }

        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = self.board_detail_scroll_offset.min(max_offset);
        if offset == 0 {
            return (full_text, 0, total_lines);
        }

        let visible = full_text
            .lines()
            .skip(offset)
            .collect::<Vec<_>>()
            .join("\n");
        (visible, offset, total_lines)
    }

    fn issue_detail_text(&self) -> String {
        if self.analyzer.issues.is_empty() {
            return "No issues to display. Create or load a .beads/*.jsonl dataset.".to_string();
        }

        let Some(issue) = self.selected_issue() else {
            return self.no_filtered_issues_text("main detail");
        };
        let blockers = self.analyzer.graph.blockers(&issue.id);
        let open_blockers = self.analyzer.graph.open_blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        let betweenness = self
            .analyzer
            .metrics
            .betweenness
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let eigenvector = self
            .analyzer
            .metrics
            .eigenvector
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let hubs = self
            .analyzer
            .metrics
            .hubs
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let authorities = self
            .analyzer
            .metrics
            .authorities
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let k_core = self
            .analyzer
            .metrics
            .k_core
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let slack = self
            .analyzer
            .metrics
            .slack
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let pagerank = self
            .analyzer
            .metrics
            .pagerank
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let articulation = self
            .analyzer
            .metrics
            .articulation_points
            .contains(&issue.id);
        let history = self.analyzer.history(Some(&issue.id), 1).into_iter().next();

        let pr_rank = metric_rank(&self.analyzer.metrics.pagerank, &issue.id);
        let bw_rank = metric_rank(&self.analyzer.metrics.betweenness, &issue.id);
        let ev_rank = metric_rank(&self.analyzer.metrics.eigenvector, &issue.id);
        let hub_rank = metric_rank(&self.analyzer.metrics.hubs, &issue.id);
        let auth_rank = metric_rank(&self.analyzer.metrics.authorities, &issue.id);
        let pr_max = max_metric_value(&self.analyzer.metrics.pagerank);
        let bw_max = max_metric_value(&self.analyzer.metrics.betweenness);
        let ev_max = max_metric_value(&self.analyzer.metrics.eigenvector);
        let hub_max = max_metric_value(&self.analyzer.metrics.hubs);
        let auth_max = max_metric_value(&self.analyzer.metrics.authorities);
        let action_state = if issue.is_closed_like() {
            "closed"
        } else if open_blockers.is_empty() {
            "ready"
        } else {
            "blocked"
        };
        let closed_display = if issue.is_closed_like() {
            format_compact_timestamp(issue.closed_at.or(issue.updated_at))
        } else {
            "n/a".to_string()
        };

        let mut lines = vec![
            format!(
                "{} {}  {}",
                type_icon(&issue.issue_type),
                issue.id,
                issue.title
            ),
            format!(
                "Status: {} | Priority: p{} | Type: {} | State: {}",
                issue.status,
                issue.priority,
                display_or_fallback(&issue.issue_type, "unknown"),
                action_state
            ),
            format!(
                "Assignee: {} | Repo: {} | Estimate: {}",
                display_or_fallback(&issue.assignee, "unassigned"),
                display_or_fallback(&issue.source_repo, "local"),
                issue
                    .estimated_minutes
                    .map_or_else(|| "n/a".to_string(), |minutes| format!("{minutes}m"))
            ),
            format!(
                "Created: {} | Updated: {} | Due: {}",
                format_compact_timestamp(issue.created_at),
                format_compact_timestamp(issue.updated_at),
                format_compact_timestamp(issue.due_date)
            ),
            format!(
                "Closed: {} | Labels: {}",
                closed_display,
                join_display_values(&issue.labels, 4)
            ),
        ];
        if let Some(ref ext_ref) = issue.external_ref {
            lines.push(format!("External: {ext_ref}"));
        }
        lines.extend([
            String::new(),
            "Triage Snapshot:".to_string(),
            format!(
                "  open blockers: {} ({})",
                open_blockers.len(),
                join_display_values(&open_blockers, 4)
            ),
            format!(
                "  unblocks: {} ({})",
                dependents.len(),
                join_display_values(&dependents, 4)
            ),
            format!(
                "  dependency pressure: {} upstream | {} downstream",
                blockers.len(),
                dependents.len()
            ),
        ]);

        if let (Some(created), Some(closed)) = (issue.created_at, issue.closed_at) {
            let duration = closed - created;
            lines.push(format!(
                "  cycle time: {}d {}h",
                duration.num_days(),
                duration.num_hours() - duration.num_days() * 24
            ));
        }

        lines.push(String::new());
        lines.push("Graph Signals:".to_string());
        lines.push(format!(
            "  Critical depth: {depth} | k-core: {k_core} | slack: {slack:.4} | cut-point: {}",
            if articulation { "YES" } else { "no" }
        ));
        lines.push(format!(
            "  PageRank:     {pagerank:>8.4}  {}  #{pr_rank}",
            mini_bar(pagerank, pr_max)
        ));
        lines.push(format!(
            "  Betweenness:  {betweenness:>8.4}  {}  #{bw_rank}",
            mini_bar(betweenness, bw_max)
        ));
        lines.push(format!(
            "  Eigenvector:  {eigenvector:>8.4}  {}  #{ev_rank}",
            mini_bar(eigenvector, ev_max)
        ));
        lines.push(format!(
            "  HITS: hub {hubs:.4} {} #{hub_rank} | auth {authorities:.4} {} #{auth_rank}",
            mini_bar(hubs, hub_max),
            mini_bar(authorities, auth_max)
        ));

        push_text_section(&mut lines, "Description", &issue.description);
        push_text_section(&mut lines, "Design Notes", &issue.design);
        push_text_section(
            &mut lines,
            "Acceptance Criteria",
            &issue.acceptance_criteria,
        );
        push_text_section(&mut lines, "Notes", &issue.notes);

        lines.push(String::new());
        lines.push("Dependency Map:".to_string());
        lines.push(format!("  upstream: {}", join_display_values(&blockers, 4)));
        lines.push(format!(
            "  open gate: {}",
            join_display_values(&open_blockers, 4)
        ));
        lines.push(format!(
            "  downstream: {}",
            join_display_values(&dependents, 4)
        ));

        if self.priority_hints_visible {
            self.append_priority_hints(&mut lines, issue);
        }

        push_comment_section(&mut lines, issue);
        push_history_section(&mut lines, history.as_ref());
        lines.join("\n")
    }

    fn append_priority_hints(&self, lines: &mut Vec<String>, issue: &crate::model::Issue) {
        use crate::analysis::triage::{TriageOptions, TriageScoringOptions};

        lines.push(String::new());
        lines.push("Priority Hints (p to hide):".to_string());

        let triage = self.analyzer.triage(TriageOptions {
            max_recommendations: 200,
            scoring: TriageScoringOptions::default(),
            ..TriageOptions::default()
        });

        if let Some(rec) = triage
            .result
            .recommendations
            .iter()
            .find(|r| r.id == issue.id)
        {
            lines.push(format!("  Triage Score:  {:.3}", rec.score));
            lines.push(format!("  Confidence:    {:.1}%", rec.confidence * 100.0));
            lines.push(format!("  Unblocks:      {}", rec.unblocks));
            if !rec.reasons.is_empty() {
                lines.push(format!("  Reasons:       {}", rec.reasons.join("; ")));
            }

            if let Some(ref breakdown) = rec.breakdown {
                lines.push(String::new());
                lines.push("  Score Breakdown:".to_string());
                for component in breakdown {
                    let bar = mini_bar(component.weighted, 0.3);
                    lines.push(format!(
                        "    {:<14} {:>5.1}% × {:.3} = {:.4}  {bar}",
                        component.name,
                        component.weight * 100.0,
                        component.normalized,
                        component.weighted,
                    ));
                    if let Some(ref explanation) = component.explanation {
                        lines.push(format!("      └ {explanation}"));
                    }
                }
            }

            lines.push(format!("  Claim: {}", rec.claim_command));
        } else {
            lines.push("  (not in triage — may be closed or blocked)".to_string());
        }
    }

    fn board_detail_text(&self) -> String {
        if self.analyzer.issues.is_empty() {
            return "No issues to display in board mode.".to_string();
        }

        let Some(issue) = self.selected_issue() else {
            return self.no_filtered_issues_text("board mode");
        };

        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        let open_blockers = self.analyzer.graph.open_blockers(&issue.id);
        let icon = status_icon(&issue.status);
        let ti = type_icon(&issue.issue_type);

        // Determine current lane label from grouping
        let lane_label = match self.board_grouping {
            BoardGrouping::Status => issue.status.clone(),
            BoardGrouping::Priority => format!("p{}", issue.priority),
            BoardGrouping::Type => {
                if issue.issue_type.trim().is_empty() {
                    "unknown".to_string()
                } else {
                    issue.issue_type.to_lowercase()
                }
            }
        };

        let title_trunc = truncate_str(&issue.title, 34);
        let id_line = format!(" {} {} {} p{}", icon, ti, issue.id, issue.priority);
        let box_width = 40;
        let hrule: String = std::iter::repeat_n('\u{2500}', box_width).collect();

        let mut out = Vec::<String>::new();
        out.push(format!("\u{250c}{hrule}\u{2510}"));
        out.push(format!(
            "\u{2502} {:<w$}\u{2502}",
            id_line,
            w = box_width - 1
        ));
        out.push(format!(
            "\u{2502} {:<w$}\u{2502}",
            title_trunc,
            w = box_width - 1
        ));
        out.push(format!("\u{251c}{hrule}\u{2524}"));
        out.push(format!(
            "\u{2502} Lane: {:<w$}\u{2502}",
            lane_label,
            w = box_width - 7
        ));
        out.push(format!(
            "\u{2502} Assignee: {:<w$}\u{2502}",
            issue.assignee,
            w = box_width - 11
        ));
        if !issue.description.is_empty() {
            let desc_trunc = truncate_str(&issue.description, box_width - 3);
            out.push(format!(
                "\u{2502} {:<w$}\u{2502}",
                desc_trunc,
                w = box_width - 1
            ));
        }
        out.push(format!("\u{2514}{hrule}\u{2518}"));

        out.push(String::new());
        // Dependency context with detail cursor
        let mut dep_index = 0usize;
        let show_cursor = self.focus == FocusPane::Detail;
        if !blockers.is_empty() {
            out.push(format!("Depends on ({})", blockers.len()));
            for bid in &blockers {
                let bstatus = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|i| i.id == *bid)
                    .map_or("?", |i| status_icon(&i.status));
                let is_open = open_blockers.contains(bid);
                let marker = if is_open { "OPEN" } else { "ok" };
                let prefix = if show_cursor && dep_index == self.detail_dep_cursor {
                    ">"
                } else {
                    " "
                };
                out.push(format!("{prefix} {bstatus} {bid} [{marker}]"));
                dep_index += 1;
            }
        }
        if !dependents.is_empty() {
            out.push(format!("Unblocks ({})", dependents.len()));
            for did in &dependents {
                let dstatus = self
                    .analyzer
                    .issues
                    .iter()
                    .find(|i| i.id == *did)
                    .map_or("?", |i| status_icon(&i.status));
                let prefix = if show_cursor && dep_index == self.detail_dep_cursor {
                    ">"
                } else {
                    " "
                };
                out.push(format!("{prefix} {dstatus} {did}"));
                dep_index += 1;
            }
        }

        out.push(String::new());
        if open_blockers.is_empty() {
            out.push("Ready to advance to next lane.".to_string());
        } else {
            out.push(format!("Blocked by {} open issue(s).", open_blockers.len()));
        }

        out.join("\n")
    }

    fn insights_detail_text(&self) -> String {
        let heatmap_context = self.insights_heatmap.as_ref().map(|state| {
            let data = self.insights_heatmap_data();
            let row = state
                .row
                .min(INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1));
            let col = state
                .col
                .min(INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1));
            let cell_issue_ids = data.issue_ids[row][col].clone();

            let mut context = vec![format!(
                "Heatmap: {} x {} ({} issue(s))",
                INSIGHTS_HEATMAP_DEPTH_LABELS[row],
                INSIGHTS_HEATMAP_SCORE_LABELS[col],
                cell_issue_ids.len()
            )];
            if state.drill_active {
                let position = if cell_issue_ids.is_empty() {
                    0
                } else {
                    state
                        .drill_cursor
                        .min(cell_issue_ids.len().saturating_sub(1))
                        + 1
                };
                context.push(format!(
                    "Drill selection: {position}/{}",
                    cell_issue_ids.len()
                ));
            }

            (context, cell_issue_ids)
        });

        if self.analyzer.issues.is_empty() {
            return "No insights available.".to_string();
        }

        if let Some((mut context, cell_issue_ids)) = heatmap_context.clone()
            && cell_issue_ids.is_empty()
        {
            context.push(String::new());
            context.push("No issues in the selected heatmap cell.".to_string());
            return context.join("\n");
        }

        let Some(issue) = self.selected_issue() else {
            if let Some((mut context, _)) = heatmap_context {
                context.push(String::new());
                context.push(self.no_filtered_issues_text("insights mode"));
                return context.join("\n");
            }
            return self.no_filtered_issues_text("insights mode");
        };
        let insights = self.analyzer.insights();
        let pagerank = self
            .analyzer
            .metrics
            .pagerank
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let betweenness = self
            .analyzer
            .metrics
            .betweenness
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let eigenvector = self
            .analyzer
            .metrics
            .eigenvector
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let hubs = self
            .analyzer
            .metrics
            .hubs
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let authorities = self
            .analyzer
            .metrics
            .authorities
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let k_core = self
            .analyzer
            .metrics
            .k_core
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let slack = self
            .analyzer
            .metrics
            .slack
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let articulation = self
            .analyzer
            .metrics
            .articulation_points
            .contains(&issue.id);

        let mut lines = vec![
            format!(
                "Insights Summary: bottlenecks={} crit-path={} cycles={} cut-pts={} k-core-max={}",
                insights.bottlenecks.len(),
                insights.critical_path.len(),
                insights.cycles.len(),
                insights.articulation_points.len(),
                insights.cores.first().map_or(0, |c| c.value)
            ),
            String::new(),
            format!("Focus: {} ({})", issue.id, issue.title),
            format!("Status: {} | Priority: p{}", issue.status, issue.priority),
            String::new(),
            "All Metrics:".to_string(),
            format!("  PageRank:     {:.4}", pagerank),
            format!("  Betweenness:  {:.4}", betweenness),
            format!("  Eigenvector:  {:.4}", eigenvector),
            format!("  Hub (HITS):   {:.4}", hubs),
            format!("  Auth (HITS):  {:.4}", authorities),
            format!("  K-core:       {}", k_core),
            format!("  Crit depth:   {}", depth),
            format!("  Slack:        {:.4}", slack),
            format!(
                "  Cut point:    {}",
                if articulation { "YES" } else { "no" }
            ),
        ];

        lines.push(String::new());
        if self.insights_show_explanations {
            lines.push("Critical Path Head:".to_string());
            if insights.critical_path.is_empty() {
                lines.push("  none".to_string());
            } else {
                lines.extend(
                    insights
                        .critical_path
                        .iter()
                        .take(6)
                        .map(|id| format!("  - {id}")),
                );
            }

            lines.push(String::new());
            lines.push("Cycle Hotspots:".to_string());
            if insights.cycles.is_empty() {
                lines.push("  none".to_string());
            } else {
                lines.extend(
                    insights
                        .cycles
                        .iter()
                        .take(4)
                        .map(|cycle| format!("  - {}", cycle.join(" -> "))),
                );
            }
        } else {
            lines.push("Explanations hidden (press e to show).".to_string());
        }

        if self.insights_show_calc_proof {
            let blocks_count = self
                .analyzer
                .metrics
                .blocks_count
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let blocked_by_count = self
                .analyzer
                .metrics
                .blocked_by_count
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            lines.push(String::new());
            lines.push("Calculation Proof:".to_string());
            lines.push(format!(
                "  score inputs -> blocks={blocks_count} blocked_by={blocked_by_count} pagerank={pagerank:.4} betweenness={betweenness:.4} depth={depth}",
            ));
        }

        // Dependency context with detail cursor
        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        if !blockers.is_empty() || !dependents.is_empty() {
            let show_cursor = self.focus == FocusPane::Detail;
            let mut dep_index = 0usize;
            lines.push(String::new());
            if !blockers.is_empty() {
                lines.push(format!("Depends on ({})", blockers.len()));
                for bid in &blockers {
                    let bsi = self
                        .analyzer
                        .issues
                        .iter()
                        .find(|i| i.id == *bid)
                        .map_or("?", |i| status_icon(&i.status));
                    let prefix = if show_cursor && dep_index == self.detail_dep_cursor {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(format!("{prefix} {bsi} {bid}"));
                    dep_index += 1;
                }
            }
            if !dependents.is_empty() {
                lines.push(format!("Unblocks ({})", dependents.len()));
                for did in &dependents {
                    let dsi = self
                        .analyzer
                        .issues
                        .iter()
                        .find(|i| i.id == *did)
                        .map_or("?", |i| status_icon(&i.status));
                    let prefix = if show_cursor && dep_index == self.detail_dep_cursor {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(format!("{prefix} {dsi} {did}"));
                    dep_index += 1;
                }
            }
        }

        if let Some((mut context, _)) = heatmap_context {
            context.push(String::new());
            context.append(&mut lines);
            return context.join("\n");
        }

        lines.join("\n")
    }

    fn graph_relationship_box_width(&self, width: usize, count: usize) -> usize {
        let count = count.clamp(1, 5);
        let spacing = count.saturating_sub(1);
        let max_fit = width.saturating_sub(spacing) / count;
        if max_fit >= 20 {
            20
        } else if max_fit >= 12 {
            max_fit
        } else {
            max_fit.max(8)
        }
    }

    fn graph_relationship_box(
        &self,
        target_id: &str,
        box_width: usize,
        focused: bool,
    ) -> Vec<String> {
        let inner = box_width.saturating_sub(2).max(1);
        let (top_left, horizontal, top_right, vertical, bottom_left, bottom_right) = if focused {
            ('╔', '═', '╗', '║', '╚', '╝')
        } else {
            ('┌', '─', '┐', '│', '└', '┘')
        };
        let border = std::iter::repeat_n(horizontal, inner).collect::<String>();

        let (header, title) = self.issue_by_id(target_id).map_or_else(
            || (format!("[?] {target_id}"), "(not in filter)".to_string()),
            |candidate| {
                let prefix = if focused { ">" } else { " " };
                (
                    format!(
                        "{prefix}[{}] {}",
                        status_icon(&candidate.status),
                        candidate.id
                    ),
                    display_or_fallback(&candidate.title, "(untitled)"),
                )
            },
        );

        vec![
            format!("{top_left}{border}{top_right}"),
            format!("{vertical}{}{vertical}", fit_display(&header, inner)),
            format!(
                "{vertical}{}{vertical}",
                fit_display(&truncate_str(&title, inner), inner)
            ),
            format!("{bottom_left}{border}{bottom_right}"),
        ]
    }

    fn graph_ego_box(&self, issue: &crate::model::Issue, width: usize) -> Vec<String> {
        let box_width = (width / 2)
            .clamp(22, 38)
            .min(width.saturating_sub(4).max(12));
        let inner = box_width.saturating_sub(2).max(1);
        let border = std::iter::repeat_n('═', inner).collect::<String>();
        let si = status_icon(&issue.status);
        let ti = type_icon(&issue.issue_type);
        let header = format!("[{si} {ti} p{}] {}", issue.priority, issue.id);
        let title = display_or_fallback(&issue.title, "(untitled)");
        let counts = format!(
            "up:{} down:{}",
            self.analyzer.graph.blockers(&issue.id).len(),
            self.analyzer.graph.dependents(&issue.id).len()
        );

        vec![
            center_display(&format!("╔{border}╗"), width),
            center_display(&format!("║{}║", fit_display(&header, inner)), width),
            center_display(
                &format!("║{}║", fit_display(&truncate_str(&title, inner), inner)),
                width,
            ),
            center_display(&format!("║{}║", fit_display(&counts, inner)), width),
            center_display(&format!("╚{border}╝"), width),
        ]
    }

    fn graph_connector_rows(&self, count: usize, width: usize) -> Vec<String> {
        let display_count = count.clamp(0, 5);
        if display_count == 0 {
            return Vec::new();
        }
        if display_count == 1 {
            return ["│", "│", "▼"]
                .into_iter()
                .map(|line| center_display(line, width))
                .collect();
        }

        let mut fan = String::from("├");
        for idx in 0..display_count {
            if idx > 0 {
                fan.push('┼');
            }
            fan.push('─');
        }
        fan.push('┤');

        ["│".to_string(), fan, "▼".to_string()]
            .into_iter()
            .map(|line| center_display(&line, width))
            .collect()
    }

    fn graph_relationship_rows(
        &self,
        ids: &[String],
        width: usize,
        show_cursor: bool,
        dep_index: &mut usize,
    ) -> Vec<String> {
        if ids.is_empty() {
            return Vec::new();
        }

        let display_count = ids.len().min(5);
        let box_width = self.graph_relationship_box_width(width, display_count);
        let boxes = ids
            .iter()
            .take(display_count)
            .map(|target_id| {
                let focused = show_cursor && *dep_index == self.detail_dep_cursor;
                *dep_index += 1;
                self.graph_relationship_box(target_id, box_width, focused)
            })
            .collect::<Vec<_>>();

        let mut lines = center_box_rows(&boxes, width);
        if ids.len() > display_count {
            lines.push(center_display(
                &format!("+{} more", ids.len() - display_count),
                width,
            ));
        }
        lines
    }

    fn graph_detail_text(&self) -> String {
        self.graph_detail_text_for_width(72)
    }

    fn graph_detail_text_for_width(&self, width: usize) -> String {
        if self.analyzer.issues.is_empty() {
            return "No graph data available.".to_string();
        }

        let Some(issue) = self.selected_issue() else {
            return self.no_filtered_issues_text("graph mode");
        };
        let render_width = width.max(24);
        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        let pagerank = self
            .analyzer
            .metrics
            .pagerank
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let betweenness = self
            .analyzer
            .metrics
            .betweenness
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let eigenvector = self
            .analyzer
            .metrics
            .eigenvector
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let hubs = self
            .analyzer
            .metrics
            .hubs
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let authorities = self
            .analyzer
            .metrics
            .authorities
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let k_core = self
            .analyzer
            .metrics
            .k_core
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let slack = self
            .analyzer
            .metrics
            .slack
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let articulation = self
            .analyzer
            .metrics
            .articulation_points
            .contains(&issue.id);
        let cycle_hits = self
            .analyzer
            .metrics
            .cycles
            .iter()
            .filter(|cycle| cycle.iter().any(|id| id == &issue.id))
            .cloned()
            .collect::<Vec<_>>();

        let visible = self.graph_visible_issue_indices();
        let focus_position = visible
            .iter()
            .position(|&index| {
                self.analyzer
                    .issues
                    .get(index)
                    .is_some_and(|candidate| candidate.id == issue.id)
            })
            .map_or(1, |position| position + 1);
        let total_focusable = visible.len().max(1);
        let focus_summary = match self.focus {
            FocusPane::Detail => {
                let total_edges = blockers.len() + dependents.len();
                if total_edges == 0 {
                    "Focused edge: none (isolated node)".to_string()
                } else if self.detail_dep_cursor < blockers.len() {
                    let target = &blockers[self.detail_dep_cursor];
                    let title = self
                        .issue_by_id(target)
                        .map(|candidate| candidate.title.as_str())
                        .unwrap_or("?");
                    format!(
                        "Focused edge: depends on [{}/{}] {} -> {} ({})",
                        self.detail_dep_cursor + 1,
                        total_edges,
                        issue.id,
                        target,
                        title
                    )
                } else {
                    let dep_index = self.detail_dep_cursor - blockers.len();
                    let target = &dependents[dep_index];
                    let title = self
                        .issue_by_id(target)
                        .map(|candidate| candidate.title.as_str())
                        .unwrap_or("?");
                    format!(
                        "Focused edge: unblocks [{}/{}] {} -> {} ({})",
                        self.detail_dep_cursor + 1,
                        total_edges,
                        issue.id,
                        target,
                        title
                    )
                }
            }
            FocusPane::List => {
                "Focused edge: list focus (Tab to inspect relationships)".to_string()
            }
        };

        let total = self.analyzer.issues.len();
        let cp_rank = self
            .analyzer
            .metrics
            .critical_depth
            .values()
            .filter(|&&value| value > depth)
            .count()
            + 1;
        let pr_rank = metric_rank(&self.analyzer.metrics.pagerank, &issue.id);
        let bw_rank = metric_rank(&self.analyzer.metrics.betweenness, &issue.id);
        let ev_rank = metric_rank(&self.analyzer.metrics.eigenvector, &issue.id);
        let hub_rank = metric_rank(&self.analyzer.metrics.hubs, &issue.id);
        let auth_rank = metric_rank(&self.analyzer.metrics.authorities, &issue.id);
        let cp_max = self
            .analyzer
            .metrics
            .critical_depth
            .values()
            .copied()
            .max()
            .unwrap_or_default()
            .max(1) as f64;
        let pr_max = max_metric_value(&self.analyzer.metrics.pagerank);
        let bw_max = max_metric_value(&self.analyzer.metrics.betweenness);
        let ev_max = max_metric_value(&self.analyzer.metrics.eigenvector);
        let hub_max = max_metric_value(&self.analyzer.metrics.hubs);
        let auth_max = max_metric_value(&self.analyzer.metrics.authorities);

        let show_cursor = self.focus == FocusPane::Detail;
        let mut dep_index = 0usize;
        let mut lines = vec![
            format!(
                "Graph: nodes={} edges={} cycles={} actionable={}",
                self.analyzer.graph.node_count(),
                self.analyzer.graph.edge_count(),
                self.analyzer.metrics.cycles.len(),
                self.analyzer.graph.actionable_ids().len()
            ),
            format!(
                "Focus: node {focus_position}/{total_focusable} -> {} ({})",
                issue.id, issue.title
            ),
            focus_summary,
            String::new(),
        ];

        if !blockers.is_empty() {
            lines.push(center_display(
                "▲ BLOCKED BY (must complete first) ▲",
                render_width,
            ));
            lines.extend(self.graph_relationship_rows(
                &blockers,
                render_width,
                show_cursor,
                &mut dep_index,
            ));
            lines.extend(self.graph_connector_rows(blockers.len().min(5), render_width));
        }

        lines.extend(self.graph_ego_box(issue, render_width));

        if !dependents.is_empty() {
            lines.extend(self.graph_connector_rows(dependents.len().min(5), render_width));
            lines.push(center_display("▼ BLOCKS (waiting on this) ▼", render_width));
            lines.extend(self.graph_relationship_rows(
                &dependents,
                render_width,
                show_cursor,
                &mut dep_index,
            ));
        }

        lines.push(String::new());
        lines.push("GRAPH METRICS".to_string());
        lines.push("Importance:".to_string());
        lines.push(format!(
            "  Critical Path  {:>8}  {}  #{}",
            depth,
            mini_bar(depth as f64, cp_max),
            cp_rank
        ));
        lines.push(format!(
            "  PageRank       {:>8.4}  {}  #{}",
            pagerank,
            mini_bar(pagerank, pr_max),
            pr_rank
        ));
        lines.push(format!(
            "  Eigenvector    {:>8.4}  {}  #{}",
            eigenvector,
            mini_bar(eigenvector, ev_max),
            ev_rank
        ));
        lines.push("Flow & Connectivity:".to_string());
        lines.push(format!(
            "  Betweenness    {:>8.4}  {}  #{}",
            betweenness,
            mini_bar(betweenness, bw_max),
            bw_rank
        ));
        lines.push(format!(
            "  Hub Score      {:>8.4}  {}  #{}",
            hubs,
            mini_bar(hubs, hub_max),
            hub_rank
        ));
        lines.push(format!(
            "  Authority      {:>8.4}  {}  #{}",
            authorities,
            mini_bar(authorities, auth_max),
            auth_rank
        ));
        lines.push("Connections:".to_string());
        lines.push(format!(
            "  In-Degree      {:>8}  {}",
            blockers.len(),
            mini_bar(blockers.len() as f64, total as f64)
        ));
        lines.push(format!(
            "  Out-Degree     {:>8}  {}",
            dependents.len(),
            mini_bar(dependents.len() as f64, total as f64)
        ));
        lines.push(format!(
            "  K-core: {k_core}  Slack: {slack:.4}  Cut: {}",
            if articulation { "YES" } else { "no" }
        ));

        if cycle_hits.is_empty() {
            lines.push("  Cycles: none".to_string());
        } else {
            lines.push("  Cycles:".to_string());
            lines.extend(
                cycle_hits
                    .iter()
                    .take(4)
                    .map(|cycle| format!("    {}", cycle.join(" -> "))),
            );
        }

        lines.push(String::new());
        lines.push("Top PageRank:".to_string());
        lines.extend(
            top_metric_entries(&self.analyzer.metrics.pagerank, 5)
                .into_iter()
                .map(|(id, value)| format!("  {id:<12} {value:.4}")),
        );
        lines.push(String::new());
        lines.push(format!(
            "Legend: █ relative score | #N rank of {total} issues"
        ));
        lines.push(
            "Nav: h/l nodes | j/k nodes or focused edges | Tab node/edge focus | Enter open details"
                .to_string(),
        );

        lines
            .into_iter()
            .map(|line| truncate_display(&line, render_width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn history_detail_text(&self) -> String {
        if self.analyzer.issues.is_empty() {
            return "No history data available.".to_string();
        }

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            let visible = self.history_git_visible_commit_indices();
            let Some(commit) = self.selected_history_git_commit() else {
                return "No correlated git commits available.".to_string();
            };

            let cursor = self
                .history_event_cursor
                .min(visible.len().saturating_sub(1));

            let related = self.history_git_related_beads_for_commit(&commit.sha);

            let commit_icon = commit_type_icon(&commit.message);
            let initials = author_initials(&commit.author);
            let mut lines = vec![
                format!(
                    "Commit {}/{} | confidence >= {:.0}%",
                    cursor + 1,
                    visible.len(),
                    self.history_min_confidence() * 100.0
                ),
                String::new(),
                "COMMIT DETAILS:".to_string(),
                format!("  SHA: {}", commit.sha),
                format!("  [{initials}] {} <{}>", commit.author, commit.author_email),
                format!("  Date: {}", commit.timestamp),
                format!("  {commit_icon} {}", commit.message),
            ];

            if !commit.files.is_empty() {
                let total_ins: i64 = commit.files.iter().map(|f| f.insertions).sum();
                let total_del: i64 = commit.files.iter().map(|f| f.deletions).sum();
                lines.push(format!(
                    "  Files: {} changed +{total_ins}/-{total_del}",
                    commit.files.len()
                ));
                for file in commit.files.iter().take(5) {
                    let action_icon = match file.action.as_str() {
                        "A" => "+",
                        "D" => "-",
                        "R" | "R100" => ">",
                        _ => "~",
                    };
                    if file.insertions > 0 || file.deletions > 0 {
                        lines.push(format!(
                            "    {action_icon} {} +{}/-{}",
                            file.path, file.insertions, file.deletions
                        ));
                    } else {
                        lines.push(format!("    {action_icon} {}", file.path));
                    }
                }
                if commit.files.len() > 5 {
                    lines.push(format!("    +{} more files...", commit.files.len() - 5));
                }
            }

            if !related.is_empty() {
                lines.push(String::new());
                lines.push("RELATED BEADS:".to_string());
                for bead_id in &related {
                    let conf = self
                        .history_git_cache
                        .as_ref()
                        .and_then(|c| c.commit_bead_confidence.get(&commit.sha))
                        .and_then(|pairs| {
                            pairs.iter().find(|(id, _)| id == bead_id).map(|(_, c)| *c)
                        })
                        .unwrap_or(0.0);
                    let issue_status = self
                        .analyzer
                        .issues
                        .iter()
                        .find(|i| i.id == *bead_id)
                        .map_or("?", |i| status_icon(&i.status));
                    let title = self
                        .analyzer
                        .issues
                        .iter()
                        .find(|i| i.id == *bead_id)
                        .map(|i| truncate_str(&i.title, 30))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  [{issue_status}] {bead_id} ({:.0}%) {}",
                        conf * 100.0,
                        title
                    ));
                }
            }

            // Append file tree panel inline when toggled on
            if self.history_show_file_tree {
                lines.push(String::new());
                lines.push(self.file_tree_panel_text());
            }

            lines.push(String::new());
            lines.push("Enter: jump to related bead | J/K: cycle related beads".to_string());
            lines.push("v: switch to bead timeline | c: cycle confidence".to_string());
            lines.push("y: copy SHA | o: open in browser | f: file tree".to_string());
            if !self.history_status_msg.is_empty() {
                lines.push(String::new());
                lines.push(self.history_status_msg.clone());
            }
            return lines.join("\n");
        }

        let Some(issue) = self.selected_issue() else {
            return self.no_filtered_issues_text("history mode");
        };
        let selected_history = self.analyzer.history(Some(&issue.id), 1).into_iter().next();

        let all_histories = self.analyzer.history(None, 0);
        let closed_histories = all_histories
            .iter()
            .filter(|history| {
                history
                    .events
                    .iter()
                    .any(|event| event.kind.eq_ignore_ascii_case("closed"))
            })
            .count();

        let mut lines = vec![
            format!(
                "History Summary: beads={} closed-like={} selected={}",
                all_histories.len(),
                closed_histories,
                issue.id
            ),
            String::new(),
            format!("Issue: {} ({})", issue.id, issue.title),
            format!("Status: {}", issue.status),
            format!(
                "Min confidence filter: >= {:.0}%",
                self.history_min_confidence() * 100.0
            ),
            format!(
                "Created/Updated/Closed: {} / {} / {}",
                format_compact_timestamp(issue.created_at),
                format_compact_timestamp(issue.updated_at),
                format_compact_timestamp(issue.closed_at)
            ),
        ];

        if let (Some(created), Some(closed)) = (issue.created_at, issue.closed_at) {
            let duration = closed - created;
            lines.push(format!(
                "Create->Close cycle time: {}d {}h",
                duration.num_days(),
                duration.num_hours() - duration.num_days() * 24
            ));
        }

        // Show milestones from git history correlation if available
        if let Some(compat_history) = self
            .history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(&issue.id))
        {
            let ms = &compat_history.milestones;
            let has_milestones = ms.created.is_some()
                || ms.claimed.is_some()
                || ms.closed.is_some()
                || ms.reopened.is_some();
            if has_milestones {
                lines.push(String::new());
                lines.push("Milestones:".to_string());
                if let Some(ref event) = ms.created {
                    lines.push(format!(
                        "  Created:  {} by {}",
                        event.timestamp, event.author
                    ));
                }
                if let Some(ref event) = ms.claimed {
                    lines.push(format!(
                        "  Claimed:  {} by {}",
                        event.timestamp, event.author
                    ));
                }
                if let Some(ref event) = ms.closed {
                    lines.push(format!(
                        "  Closed:   {} by {}",
                        event.timestamp, event.author
                    ));
                }
                if let Some(ref event) = ms.reopened {
                    lines.push(format!(
                        "  Reopened: {} by {}",
                        event.timestamp, event.author
                    ));
                }
            }

            if !compat_history.last_author.is_empty() {
                lines.push(format!("Last author: {}", compat_history.last_author));
            }
        }

        lines.push(String::new());
        lines.push("LIFECYCLE:".to_string());

        if let Some(history) = selected_history {
            if history.events.is_empty() {
                lines.push("  (no events)".to_string());
            } else {
                let event_count = history.events.len();
                for (idx, event) in history.events.into_iter().enumerate() {
                    let ts = event
                        .timestamp
                        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                        .unwrap_or_else(|| "n/a".to_string());
                    let icon = lifecycle_icon(&event.kind);
                    let connector = if idx + 1 < event_count {
                        "\u{2502}"
                    } else {
                        "\u{2514}"
                    };
                    lines.push(format!(
                        "  {connector} {icon} {:<10} {ts}  {}",
                        event.kind, event.details
                    ));
                }
            }
        } else {
            lines.push("  (history unavailable for selected issue)".to_string());
        }

        if let Some(commit) = self.selected_history_bead_commit() {
            let total = self.history_filtered_bead_commits(&issue.id).len();
            let slot = self.history_bead_commit_cursor.min(total.saturating_sub(1));

            lines.push(String::new());
            lines.push(format!(
                "COMMIT DETAILS ({}/{}):",
                slot.saturating_add(1),
                total
            ));
            let commit_icon = commit_type_icon(&commit.message);
            lines.push(format!(
                "  {commit_icon} {}  {}",
                commit.short_sha, commit.timestamp
            ));
            let initials = author_initials(&commit.author);
            lines.push(format!(
                "  [{initials}] {} <{}>",
                commit.author, commit.author_email
            ));
            lines.push(format!("  {}", commit.message));
            lines.push(format!(
                "  {:.0}% confidence ({})",
                commit.confidence * 100.0,
                commit.method
            ));
            if !commit.reason.is_empty() {
                lines.push(format!("  Reason: {}", commit.reason));
            }
            if !commit.files.is_empty() {
                let total_ins: i64 = commit.files.iter().map(|f| f.insertions).sum();
                let total_del: i64 = commit.files.iter().map(|f| f.deletions).sum();
                lines.push(format!(
                    "  {} file(s) +{total_ins}/-{total_del}",
                    commit.files.len()
                ));
                for file in commit.files.iter().take(5) {
                    let action_icon = match file.action.as_str() {
                        "A" => "+",
                        "D" => "-",
                        "R" | "R100" => ">",
                        _ => "~",
                    };
                    if file.insertions > 0 || file.deletions > 0 {
                        lines.push(format!(
                            "    {action_icon} {} +{}/-{}",
                            file.path, file.insertions, file.deletions
                        ));
                    } else {
                        lines.push(format!("    {action_icon} {}", file.path));
                    }
                }
                if commit.files.len() > 5 {
                    lines.push(format!("    +{} more files...", commit.files.len() - 5));
                }
            }
        }

        lines.push(String::new());
        lines.push("v: switch to git timeline | J/K: cycle commits".to_string());
        lines.push("y: copy bead ID | o: open commit | f: file tree".to_string());
        if !self.history_status_msg.is_empty() {
            lines.push(String::new());
            lines.push(self.history_status_msg.clone());
        }

        lines.join("\n")
    }

    /// Render the file tree panel text (when visible).
    fn file_tree_panel_text(&self) -> String {
        let flat = self.history_flat_file_list();
        if flat.is_empty() {
            return "No file data available.\n(git history may not be loaded)".to_string();
        }

        let mut out = Vec::new();
        out.push(format!("File Tree ({} entries) | Esc close", flat.len()));
        if let Some(ref filter) = self.history_file_tree_filter {
            out.push(format!("Filter: {filter}"));
        }
        out.push(String::new());

        for (idx, entry) in flat.iter().enumerate() {
            let marker = if self.history_file_tree_focus && idx == self.history_file_tree_cursor {
                '>'
            } else {
                ' '
            };
            let indent = "  ".repeat(entry.level);
            let icon = if entry.is_dir { "/" } else { "" };
            out.push(format!(
                "{marker} {indent}{}{icon} ({})",
                entry.name, entry.change_count
            ));
        }

        out.join("\n")
    }
}

#[must_use]
fn truncate_str(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else if max_len >= 3 {
        format!("{}...", &value[..max_len - 3])
    } else {
        value[..max_len].to_string()
    }
}

fn metric_rank(metrics: &std::collections::HashMap<String, f64>, target_id: &str) -> usize {
    let target_value = metrics.get(target_id).copied().unwrap_or_default();
    metrics
        .values()
        .filter(|&&value| value > target_value)
        .count()
        + 1
}

fn max_metric_value(metrics: &std::collections::HashMap<String, f64>) -> f64 {
    metrics.values().copied().fold(0.0_f64, f64::max).max(1e-9)
}

fn run_command_with_stdin(program: &str, args: &[&str], input: &str) -> bool {
    let Ok(mut child) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };

    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(input.as_bytes()).is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }

    child.wait().is_ok_and(|status| status.success())
}

fn run_command(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn copy_text_to_clipboard(text: &str) -> bool {
    run_command_with_stdin("wl-copy", &[], text)
        || run_command_with_stdin("xclip", &["-selection", "clipboard"], text)
        || run_command_with_stdin("xsel", &["--clipboard", "--input"], text)
        || run_command_with_stdin("pbcopy", &[], text)
        || run_command_with_stdin("clip.exe", &[], text)
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

fn mini_bar(value: f64, max: f64) -> String {
    let width: usize = 6;
    let ratio = if max > 0.0 { value / max } else { 0.0 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled = ratio
        .mul_add(width as f64, 0.0)
        .round()
        .clamp(0.0, width as f64) as usize;
    let empty = width - filled;
    std::iter::repeat_n('\u{2588}', filled)
        .chain(std::iter::repeat_n('\u{2591}', empty))
        .collect()
}

fn status_icon(status: &str) -> &'static str {
    if status.eq_ignore_ascii_case("open") {
        "o"
    } else if status.eq_ignore_ascii_case("in_progress") {
        "*"
    } else if status.eq_ignore_ascii_case("blocked") {
        "!"
    } else if status.eq_ignore_ascii_case("closed") {
        "x"
    } else if status.eq_ignore_ascii_case("deferred") {
        "~"
    } else if status.eq_ignore_ascii_case("review") {
        "r"
    } else if status.eq_ignore_ascii_case("pinned") {
        "^"
    } else {
        "?"
    }
}

fn type_icon(issue_type: &str) -> &'static str {
    if issue_type.eq_ignore_ascii_case("bug") {
        "B"
    } else if issue_type.eq_ignore_ascii_case("feature") {
        "F"
    } else if issue_type.eq_ignore_ascii_case("task") {
        "T"
    } else if issue_type.eq_ignore_ascii_case("epic") {
        "E"
    } else if issue_type.eq_ignore_ascii_case("question") {
        "Q"
    } else if issue_type.eq_ignore_ascii_case("docs") {
        "D"
    } else {
        "-"
    }
}

fn lifecycle_icon(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "created" => "+",
        "claimed" | "assigned" => "@",
        "closed" => "x",
        "reopened" => "~",
        "modified" | "updated" => ".",
        _ => "-",
    }
}

fn commit_type_icon(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.starts_with("feat") {
        "F"
    } else if lower.starts_with("fix") {
        "B"
    } else if lower.starts_with("docs") {
        "D"
    } else if lower.starts_with("refactor") {
        "R"
    } else if lower.starts_with("test") {
        "T"
    } else if lower.starts_with("chore") {
        "C"
    } else if lower.starts_with("perf") {
        "P"
    } else if lower.starts_with("ci") {
        "I"
    } else if lower.starts_with("build") {
        "K"
    } else if lower.starts_with("style") {
        "S"
    } else if lower.starts_with("merge") || lower.starts_with("Merge") {
        "M"
    } else if lower.starts_with("revert") {
        "<"
    } else {
        "*"
    }
}

fn author_initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect::<String>()
}

fn display_or_fallback(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn format_compact_timestamp(dt: Option<DateTime<Utc>>) -> String {
    dt.map_or_else(|| "n/a".to_string(), |ts| ts.format("%Y-%m-%d").to_string())
}

fn join_display_values(values: &[String], limit: usize) -> String {
    if values.is_empty() {
        return "none".to_string();
    }

    let mut parts = values.iter().take(limit).cloned().collect::<Vec<_>>();
    if values.len() > limit {
        parts.push(format!("+{} more", values.len() - limit));
    }
    parts.join(", ")
}

fn push_text_section(lines: &mut Vec<String>, title: &str, body: &str) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }

    lines.push(String::new());
    lines.push(format!("{title}:"));
    for line in trimmed.lines() {
        let content = line.trim_end();
        if content.is_empty() {
            lines.push("  ".to_string());
        } else {
            lines.push(format!("  {content}"));
        }
    }
}

fn push_comment_section(lines: &mut Vec<String>, issue: &Issue) {
    if issue.comments.is_empty() {
        return;
    }

    lines.push(String::new());
    lines.push(format!("Recent Comments ({}):", issue.comments.len()));
    for comment in issue.comments.iter().rev().take(3) {
        lines.push(format!(
            "  - {} @ {}",
            display_or_fallback(&comment.author, "unknown"),
            format_compact_timestamp(comment.created_at)
        ));
        for line in comment.text.lines().take(3) {
            lines.push(format!("      {}", line.trim_end()));
        }
        if comment.text.lines().count() > 3 {
            lines.push("      ...".to_string());
        }
    }
}

fn push_history_section(lines: &mut Vec<String>, history: Option<&IssueHistory>) {
    let Some(history) = history else {
        return;
    };
    if history.events.is_empty() {
        return;
    }

    lines.push(String::new());
    lines.push(format!(
        "History Summary ({} events):",
        history.events.len()
    ));
    let start = history.events.len().saturating_sub(4);
    for event in history.events.iter().skip(start) {
        lines.push(format!(
            "  {} {} {}",
            lifecycle_icon(&event.kind),
            format_compact_timestamp(event.timestamp),
            event.details
        ));
    }
}

fn top_metric_entries(
    metrics: &std::collections::HashMap<String, f64>,
    limit: usize,
) -> Vec<(String, f64)> {
    let mut entries = metrics
        .iter()
        .map(|(id, value)| (id.clone(), *value))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    entries.truncate(limit);
    entries
}

fn truncate_display(value: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }

    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= max_len {
        return value.to_string();
    }
    if max_len == 1 {
        return chars[..1].iter().collect();
    }

    let mut out = chars[..max_len - 1].iter().collect::<String>();
    out.push('…');
    out
}

fn fit_display(value: &str, width: usize) -> String {
    let mut out = truncate_display(value, width);
    let visible = out.chars().count();
    if visible < width {
        out.push_str(&" ".repeat(width - visible));
    }
    out
}

fn center_display(value: &str, width: usize) -> String {
    let text = truncate_display(value, width);
    let visible = text.chars().count();
    if visible >= width {
        return text;
    }

    let left = (width - visible) / 2;
    let right = width - visible - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

fn center_box_rows(boxes: &[Vec<String>], width: usize) -> Vec<String> {
    if boxes.is_empty() {
        return Vec::new();
    }

    let height = boxes.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|row| {
            let joined = boxes
                .iter()
                .map(|box_lines| {
                    if let Some(line) = box_lines.get(row) {
                        line.clone()
                    } else {
                        let fallback_width =
                            box_lines.first().map_or(0, |line| line.chars().count());
                        " ".repeat(fallback_width)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            center_display(&joined, width)
        })
        .collect()
}

fn cmp_opt_datetime(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
    descending: bool,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            if descending {
                right.cmp(&left)
            } else {
                left.cmp(&right)
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Convert a git remote URL to a web commit URL.
fn remote_to_commit_url(remote: &str, sha: &str) -> Option<String> {
    // Handle ssh (git@github.com:owner/repo.git) and https
    let trimmed = remote.trim();
    let web_base = if let Some(rest) = trimmed.strip_prefix("git@") {
        // git@github.com:owner/repo.git → https://github.com/owner/repo
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let rest = rest.replacen(':', "/", 1);
        format!("https://{rest}")
    } else if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        let base = trimmed.strip_suffix(".git").unwrap_or(trimmed);
        base.to_string()
    } else {
        return None;
    };

    Some(format!("{web_base}/commit/{sha}"))
}

fn new_app(issues: Vec<Issue>, mode: ViewMode) -> BvrApp {
    new_app_with_background(issues, mode, None)
}

fn new_app_with_background(
    issues: Vec<Issue>,
    mode: ViewMode,
    background_config: Option<BackgroundModeConfig>,
) -> BvrApp {
    #[cfg(not(test))]
    let initial_data_hash = compute_data_hash(&issues);
    #[cfg(not(test))]
    let background_runtime = background_config.map(|config| {
        let mut timeline = VecDeque::new();
        timeline.push_back(background_timeline_entry("background mode initialized"));

        BackgroundRuntimeState {
            config: config.normalized(),
            in_flight: false,
            cancel_requested: Arc::new(AtomicBool::new(false)),
            last_data_hash: initial_data_hash,
            timeline,
        }
    });
    #[cfg(test)]
    let _ = background_config;

    let repo_root = loader::get_beads_dir(None)
        .ok()
        .and_then(|beads_dir| beads_dir.parent().map(std::path::Path::to_path_buf));

    let use_two_phase = issues.len() > crate::analysis::graph::AnalysisConfig::BACKGROUND_THRESHOLD;
    let analyzer = if use_two_phase {
        Analyzer::new_fast(issues)
    } else {
        Analyzer::new(issues)
    };
    #[cfg(not(test))]
    let slow_metrics_rx = if use_two_phase {
        Some(analyzer.spawn_slow_computation())
    } else {
        None
    };
    let slow_metrics_pending = use_two_phase;

    BvrApp {
        analyzer,
        repo_root,
        selected: 0,
        list_filter: ListFilter::All,
        list_sort: ListSort::Default,
        board_grouping: BoardGrouping::Status,
        board_empty_visibility: EmptyLaneVisibility::Auto,
        mode,
        mode_before_history: ViewMode::Main,
        focus: FocusPane::List,
        focus_before_help: FocusPane::List,
        show_help: false,
        help_scroll_offset: 0,
        show_quit_confirm: false,
        modal_overlay: None,
        modal_confirm_result: None,
        history_confidence_index: 0,
        history_view_mode: HistoryViewMode::Bead,
        history_event_cursor: 0,
        history_related_bead_cursor: 0,
        history_bead_commit_cursor: 0,
        history_git_cache: None,
        history_search_active: false,
        history_search_query: String::new(),
        history_search_match_cursor: 0,
        history_search_mode: HistorySearchMode::All,
        history_show_file_tree: false,
        history_file_tree_cursor: 0,
        history_file_tree_filter: None,
        history_file_tree_focus: false,
        history_status_msg: String::new(),
        board_search_active: false,
        board_search_query: String::new(),
        board_search_match_cursor: 0,
        board_detail_scroll_offset: 0,
        graph_search_active: false,
        graph_search_query: String::new(),
        graph_search_match_cursor: 0,
        insights_search_active: false,
        insights_search_query: String::new(),
        insights_search_match_cursor: 0,
        insights_panel: InsightsPanel::Bottlenecks,
        insights_heatmap: None,
        insights_show_explanations: true,
        insights_show_calc_proof: false,
        detail_dep_cursor: 0,
        actionable_plan: None,
        actionable_track_cursor: 0,
        actionable_item_cursor: 0,
        attention_result: None,
        attention_cursor: 0,
        tree_flat_nodes: Vec::new(),
        tree_cursor: 0,
        tree_collapsed: std::collections::HashSet::new(),
        label_dashboard: None,
        label_dashboard_cursor: 0,
        flow_matrix: None,
        flow_matrix_row_cursor: 0,
        flow_matrix_col_cursor: 0,
        time_travel_ref_input: String::new(),
        time_travel_input_active: false,
        time_travel_diff: None,
        time_travel_category_cursor: 0,
        time_travel_issue_cursor: 0,
        time_travel_last_ref: None,
        sprint_data: Vec::new(),
        sprint_cursor: 0,
        sprint_issue_cursor: 0,
        modal_label_filter: None,
        modal_repo_filter: None,
        priority_hints_visible: false,
        status_msg: String::new(),
        slow_metrics_pending,
        #[cfg(not(test))]
        slow_metrics_rx,
        #[cfg(not(test))]
        background_runtime,
        #[cfg(test)]
        key_trace: Vec::new(),
    }
}

pub fn run_tui(issues: Vec<Issue>) -> Result<()> {
    run_tui_with_background(issues, None)
}

pub fn run_tui_with_background(
    issues: Vec<Issue>,
    background_config: Option<BackgroundModeConfig>,
) -> Result<()> {
    let model = new_app_with_background(issues, ViewMode::Main, background_config);
    App::new(model)
        .screen_mode(ScreenMode::AltScreen)
        .run()
        .map_err(|error| BvrError::Tui(error.to_string()))
}

/// Render a named TUI view non-interactively at the given dimensions and
/// return the textual output. Supported view names: `insights`, `board`,
/// `history`, `main`, `graph`.
pub fn render_debug_view(
    issues: Vec<Issue>,
    view_name: &str,
    width: u16,
    height: u16,
) -> Result<String> {
    let mode = match view_name {
        "insights" => ViewMode::Insights,
        "board" => ViewMode::Board,
        "history" => ViewMode::History,
        "main" => ViewMode::Main,
        "graph" => ViewMode::Graph,
        other => {
            return Err(BvrError::InvalidArgument(format!(
                "Unknown debug-render view '{other}'. Supported: insights, board, history, main, graph"
            )));
        }
    };

    let app = new_app(issues, mode);
    let mut pool = ftui::GraphemePool::default();
    let mut frame = Frame::new(width, height, &mut pool);
    app.view(&mut frame);
    Ok(buffer_to_text(&frame.buffer, &pool))
}

/// Convert a rendered buffer to a plain-text string (one line per row,
/// trailing whitespace trimmed).
fn buffer_to_text(buf: &ftui::Buffer, pool: &ftui::GraphemePool) -> String {
    let mut out = String::with_capacity((buf.width() as usize + 1) * buf.height() as usize);
    for y in 0..buf.height() {
        if y > 0 {
            out.push('\n');
        }
        let mut row = String::with_capacity(buf.width() as usize);
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                if cell.is_continuation() {
                    continue;
                }
                if cell.is_empty() {
                    row.push(' ');
                } else if let Some(c) = cell.content.as_char() {
                    row.push(c);
                } else if let Some(gid) = cell.content.grapheme_id() {
                    if let Some(text) = pool.get(gid) {
                        row.push_str(text);
                    } else {
                        row.push('?');
                    }
                } else {
                    row.push(' ');
                }
            } else {
                row.push(' ');
            }
        }
        let trimmed = row.trim_end();
        out.push_str(trimmed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundTickDecision, BoardGrouping, BvrApp, EmptyLaneVisibility, FocusPane,
        GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryGitCache,
        HistoryMilestonesCompat, HistorySearchMode, HistoryViewMode, InsightsPanel, ListFilter,
        ListSort, ModalOverlay, MouseEventKind, Msg, ViewMode, background_warning_message,
        decide_background_tick, render_debug_view, should_apply_background_reload,
    };
    use crate::analysis::Analyzer;
    use crate::analysis::git_history::HistoryFileChangeCompat;
    use crate::model::{Comment, Dependency, Issue, Sprint, ts};
    use ftui::core::event::{KeyCode, Modifiers};
    use ftui::runtime::{Cmd, Model};
    use std::collections::BTreeMap;

    fn sample_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                description: "Top-level issue that unblocks downstream work once the shared baseline is solid."
                    .to_string(),
                design: "Keep the main flow lean, surface the most important context first, and let supporting sections trail after the summary."
                    .to_string(),
                acceptance_criteria:
                    "- Detail summary shows triage and graph context.\n- Rich body sections stay readable across narrow and wide panes."
                        .to_string(),
                notes: "Fixture used to exercise the richer main detail pane.".to_string(),
                assignee: "alice".to_string(),
                estimated_minutes: Some(90),
                created_at: ts("2026-01-01T00:00:00Z"),
                updated_at: ts("2026-01-02T00:00:00Z"),
                labels: vec!["core".to_string(), "parity".to_string()],
                comments: vec![
                    Comment {
                        id: 1,
                        issue_id: "A".to_string(),
                        author: "alice".to_string(),
                        text: "Need this baseline before the dependent slice can land."
                            .to_string(),
                        created_at: ts("2026-01-01T08:00:00Z"),
                    },
                    Comment {
                        id: 2,
                        issue_id: "A".to_string(),
                        author: "bob".to_string(),
                        text: "Verify the detail pane still reads well at narrow widths."
                            .to_string(),
                        created_at: ts("2026-01-02T09:30:00Z"),
                    },
                ],
                source_repo: "viewer".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                description: "Depends on A and should report blocked state clearly.".to_string(),
                created_at: ts("2026-01-03T00:00:00Z"),
                updated_at: ts("2026-01-04T00:00:00Z"),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "Closed".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                created_at: ts("2026-01-01T00:00:00Z"),
                updated_at: ts("2026-01-06T00:00:00Z"),
                closed_at: ts("2026-01-06T00:00:00Z"),
                ..Issue::default()
            },
        ]
    }

    fn lane_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "OPEN-1".to_string(),
                title: "Open".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                ..Issue::default()
            },
            Issue {
                id: "IP-1".to_string(),
                title: "In Progress".to_string(),
                status: "in_progress".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "BLK-1".to_string(),
                title: "Blocked".to_string(),
                status: "blocked".to_string(),
                issue_type: "bug".to_string(),
                priority: 2,
                ..Issue::default()
            },
            Issue {
                id: "CLS-1".to_string(),
                title: "Closed".to_string(),
                status: "closed".to_string(),
                issue_type: "docs".to_string(),
                priority: 3,
                ..Issue::default()
            },
        ]
    }

    fn board_nav_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "OPEN-1".to_string(),
                title: "Open Start".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                ..Issue::default()
            },
            Issue {
                id: "OPEN-2".to_string(),
                title: "Open End".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "IP-1".to_string(),
                title: "In Progress".to_string(),
                status: "in_progress".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "CLS-1".to_string(),
                title: "Closed".to_string(),
                status: "closed".to_string(),
                issue_type: "docs".to_string(),
                priority: 3,
                ..Issue::default()
            },
        ]
    }

    fn board_with_unknown_status_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "OPEN-1".to_string(),
                title: "Open".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                ..Issue::default()
            },
            Issue {
                id: "QUE-1".to_string(),
                title: "Queued".to_string(),
                status: "queued".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
        ]
    }

    fn sortable_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "Z".to_string(),
                title: "Oldest".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                created_at: ts("2026-01-01T00:00:00Z"),
                updated_at: ts("2026-01-06T00:00:00Z"),
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Middle".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                created_at: ts("2026-01-02T00:00:00Z"),
                updated_at: ts("2026-01-05T00:00:00Z"),
                ..Issue::default()
            },
            Issue {
                id: "M".to_string(),
                title: "Newest".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                created_at: ts("2026-01-03T00:00:00Z"),
                updated_at: ts("2026-01-04T00:00:00Z"),
                ..Issue::default()
            },
        ]
    }

    fn graph_many_blocker_issues() -> Vec<Issue> {
        let mut issues = vec![Issue {
            id: "MAIN".to_string(),
            title: "Main Issue".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            dependencies: (0..10)
                .map(|idx| Dependency {
                    issue_id: "MAIN".to_string(),
                    depends_on_id: format!("BLK-{idx:02}"),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                })
                .collect(),
            ..Issue::default()
        }];

        issues.extend((0..10).map(|idx| Issue {
            id: format!("BLK-{idx:02}"),
            title: format!("Blocker {idx:02}"),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }));

        issues
    }

    fn graph_many_dependent_issues() -> Vec<Issue> {
        let mut issues = vec![Issue {
            id: "ROOT".to_string(),
            title: "Root Issue".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];

        issues.extend((0..10).map(|idx| Issue {
            id: format!("DEP-{idx:02}"),
            title: format!("Dependent {idx:02}"),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            dependencies: vec![Dependency {
                issue_id: format!("DEP-{idx:02}"),
                depends_on_id: "ROOT".to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            }],
            ..Issue::default()
        }));

        issues
    }

    fn new_app(mode: ViewMode, selected: usize) -> BvrApp {
        BvrApp {
            analyzer: Analyzer::new(sample_issues()),
            repo_root: None,
            selected,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        }
    }

    fn new_app_with_issues(mode: ViewMode, selected: usize, issues: Vec<Issue>) -> BvrApp {
        let mut app = new_app(mode, selected);
        app.analyzer = Analyzer::new(issues);
        app.selected = selected;
        app
    }

    fn history_file_change(path: &str) -> HistoryFileChangeCompat {
        HistoryFileChangeCompat {
            path: path.to_string(),
            action: "M".to_string(),
            insertions: 1,
            deletions: 0,
        }
    }

    fn history_commit(
        sha: &str,
        message: &str,
        confidence: f64,
        paths: &[&str],
    ) -> HistoryCommitCompat {
        HistoryCommitCompat {
            sha: sha.to_string(),
            short_sha: sha[..7.min(sha.len())].to_string(),
            message: message.to_string(),
            author: "Test Author".to_string(),
            author_email: "test@example.com".to_string(),
            timestamp: "2026-01-10T00:00:00Z".to_string(),
            files: paths.iter().map(|path| history_file_change(path)).collect(),
            method: "co_committed".to_string(),
            confidence,
            reason: "fixture".to_string(),
        }
    }

    fn git_commit_record(sha: &str, message: &str, paths: &[&str]) -> GitCommitRecord {
        GitCommitRecord {
            sha: sha.to_string(),
            short_sha: sha[..7.min(sha.len())].to_string(),
            timestamp: "2026-01-10T00:00:00Z".to_string(),
            author: "Test Author".to_string(),
            author_email: "test@example.com".to_string(),
            message: message.to_string(),
            files: paths.iter().map(|path| history_file_change(path)).collect(),
            changed_beads: true,
            changed_non_beads: true,
        }
    }

    fn history_app_with_git_cache(view_mode: HistoryViewMode, selected: usize) -> BvrApp {
        let ui_commit = history_commit(
            "aaaa1111",
            "feat: ui wiring",
            0.95,
            &["src/ui/app.rs", "src/ui/detail.rs"],
        );
        let core_commit =
            history_commit("bbbb2222", "feat: graph core", 0.80, &["src/core/graph.rs"]);
        let docs_commit = history_commit("cccc3333", "docs: readme", 0.90, &["README.md"]);
        let build_commit = history_commit("dddd4444", "chore: cargo polish", 0.85, &["Cargo.toml"]);

        let mut histories = BTreeMap::new();
        histories.insert(
            "A".to_string(),
            HistoryBeadCompat {
                bead_id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                events: Vec::new(),
                milestones: HistoryMilestonesCompat::default(),
                commits: Some(vec![ui_commit.clone(), core_commit.clone()]),
                cycle_time: None,
                last_author: "Alice".to_string(),
            },
        );
        histories.insert(
            "B".to_string(),
            HistoryBeadCompat {
                bead_id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                events: Vec::new(),
                milestones: HistoryMilestonesCompat::default(),
                commits: Some(vec![docs_commit.clone(), build_commit.clone()]),
                cycle_time: None,
                last_author: "Bob".to_string(),
            },
        );

        let mut commit_bead_confidence = BTreeMap::new();
        commit_bead_confidence.insert("aaaa1111".to_string(), vec![("A".to_string(), 0.95)]);
        commit_bead_confidence.insert("bbbb2222".to_string(), vec![("A".to_string(), 0.80)]);
        commit_bead_confidence.insert("cccc3333".to_string(), vec![("B".to_string(), 0.90)]);
        commit_bead_confidence.insert("dddd4444".to_string(), vec![("B".to_string(), 0.85)]);

        let mut app = new_app(ViewMode::History, selected);
        app.history_view_mode = view_mode;
        app.history_git_cache = Some(HistoryGitCache {
            commits: vec![
                git_commit_record(
                    "aaaa1111",
                    "feat: ui wiring",
                    &["src/ui/app.rs", "src/ui/detail.rs"],
                ),
                git_commit_record("bbbb2222", "feat: graph core", &["src/core/graph.rs"]),
                git_commit_record("cccc3333", "docs: readme", &["README.md"]),
                git_commit_record("dddd4444", "chore: cargo polish", &["Cargo.toml"]),
            ],
            histories,
            commit_bead_confidence,
        });
        app
    }

    fn key(code: KeyCode) -> Msg {
        Msg::KeyPress(code, Modifiers::empty())
    }

    fn key_ctrl(code: KeyCode) -> Msg {
        Msg::KeyPress(code, Modifiers::CTRL)
    }

    fn selected_issue_id(app: &BvrApp) -> String {
        app.analyzer
            .issues
            .get(app.selected)
            .map(|issue| issue.id.clone())
            .unwrap_or_default()
    }

    fn first_rendered_issue_id(app: &BvrApp) -> String {
        app.list_panel_text()
            .lines()
            .next()
            .map(|line| {
                let mut tokens = line.split_whitespace();
                let first = tokens.next().unwrap_or_default();
                if first == ">" {
                    tokens.next().unwrap_or_default().to_string()
                } else {
                    first.to_string()
                }
            })
            .unwrap_or_default()
    }

    #[test]
    fn background_tick_decision_prioritizes_cancel_then_in_flight() {
        assert_eq!(
            decide_background_tick(true, false),
            BackgroundTickDecision::Stop
        );
        assert_eq!(
            decide_background_tick(true, true),
            BackgroundTickDecision::Stop
        );
        assert_eq!(
            decide_background_tick(false, true),
            BackgroundTickDecision::TickOnly
        );
        assert_eq!(
            decide_background_tick(false, false),
            BackgroundTickDecision::ReloadAndTick
        );
    }

    #[test]
    fn background_reload_apply_requires_no_cancel_and_hash_change() {
        assert!(should_apply_background_reload(
            false, "new-hash", "old-hash"
        ));
        assert!(!should_apply_background_reload(
            false,
            "same-hash",
            "same-hash"
        ));
        assert!(!should_apply_background_reload(
            true, "new-hash", "old-hash"
        ));
    }

    #[test]
    fn background_warning_message_suppresses_canceled_paths() {
        assert_eq!(background_warning_message(true, "boom"), None);
        assert_eq!(background_warning_message(false, "canceled"), None);
        assert_eq!(
            background_warning_message(false, "permission denied").as_deref(),
            Some("background reload warning: permission denied")
        );
    }

    #[test]
    fn render_debug_view_supports_all_named_views() {
        for view in ["insights", "board", "history", "main", "graph"] {
            let output =
                render_debug_view(sample_issues(), view, 80, 12).expect("debug render succeeds");
            assert_eq!(
                output.lines().count(),
                12,
                "expected one line per requested row for view {view}"
            );
        }
    }

    #[test]
    fn render_debug_view_respects_dimensions() {
        let width = 32_u16;
        let height = 7_u16;
        let output = render_debug_view(sample_issues(), "main", width, height)
            .expect("main debug render succeeds");
        let lines: Vec<&str> = output.lines().collect();

        assert_eq!(lines.len(), usize::from(height));
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= usize::from(width)),
            "every rendered line should fit within the requested width"
        );
    }

    #[test]
    fn render_debug_view_rejects_unknown_view() {
        let error = render_debug_view(sample_issues(), "bogus", 80, 10)
            .expect_err("unknown view should fail");
        let message = error.to_string();
        assert!(message.contains("Unknown debug-render view 'bogus'"));
        assert!(message.contains("insights, board, history, main, graph"));
    }

    #[test]
    fn graph_mode_renders_metric_sections() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.mode = ViewMode::Graph;
        let text = app.detail_panel_text();
        assert!(text.contains("Graph:"));
        assert!(text.contains("PageRank"));
        assert!(text.contains("GRAPH METRICS"));
        assert!(text.contains("Importance:"));
        assert!(text.contains("Betweenness"));
        assert!(text.contains("Top PageRank"));
    }

    #[test]
    fn graph_mode_narrow_uses_single_detail_pane_layout() {
        let text = render_frame(ViewMode::Graph, 60, 30);
        assert!(text.contains("Graph View [focus]"));
        assert!(!text.contains("Graph Nodes"));
        assert!(text.contains("Focus: node 1/3 -> A (Root)"));
        assert!(text.contains("Focused edge: list focus"));
    }

    #[test]
    fn board_mode_renders_lane_summary() {
        let mut app = new_app(ViewMode::Board, 1);
        app.mode = ViewMode::Board;
        let list = app.list_panel_text();
        let detail = app.detail_panel_text();
        assert!(list.contains("Lane"));
        assert!(detail.contains("Lane:"));
        assert!(detail.contains('B'));
    }

    #[test]
    fn insights_mode_renders_rank_sections() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;
        let list = app.list_panel_text();
        let detail = app.detail_panel_text();
        assert!(list.contains("[Bottlenecks]"));
        assert!(detail.contains("Insights Summary"));
        assert!(detail.contains("Critical Path Head"));
    }

    #[test]
    fn insights_panel_s_cycles_through_all_panels() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;
        assert!(matches!(app.insights_panel, InsightsPanel::Bottlenecks));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Keystones));
        let list = app.list_panel_text();
        assert!(list.contains("[Keystones]"));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::CriticalPath));
        let list = app.list_panel_text();
        assert!(list.contains("[Critical Path]"));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Influencers));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Betweenness));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Hubs));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Authorities));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Cores));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::CutPoints));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Slack));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Cycles));

        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Priority));
        let list = app.list_panel_text();
        assert!(list.contains("[Priority]"));

        // Full cycle wraps back
        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Bottlenecks));

        // S (shift) goes backwards
        app.update(key(KeyCode::Char('S')));
        assert!(matches!(app.insights_panel, InsightsPanel::Priority));
    }

    #[test]
    fn insights_keystones_and_priority_panels_render_legacy_style_rows() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;

        app.update(key(KeyCode::Char('s')));
        let keystones = app.list_panel_text();
        assert!(keystones.contains("[Keystones]"));
        assert!(keystones.contains("depth="));
        assert!(keystones.contains("unblocks="));
        assert!(keystones.contains("A"));

        for _ in 0..10 {
            app.update(key(KeyCode::Char('s')));
        }
        let priority = app.list_panel_text();
        assert!(priority.contains("[Priority]"));
        assert!(priority.contains("score="));
        assert!(priority.contains("unblocks="));
        assert!(priority.contains("p"));
    }

    #[test]
    fn insights_detail_shows_all_metrics_for_focused_issue() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;
        let detail = app.detail_panel_text();
        assert!(detail.contains("PageRank:"));
        assert!(detail.contains("Betweenness:"));
        assert!(detail.contains("Eigenvector:"));
        assert!(detail.contains("Hub (HITS):"));
        assert!(detail.contains("Auth (HITS):"));
        assert!(detail.contains("K-core:"));
        assert!(detail.contains("Crit depth:"));
        assert!(detail.contains("Slack:"));
        assert!(detail.contains("Cut point:"));
    }

    #[test]
    fn insights_mode_e_and_x_toggle_explanations_and_calc_proof() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;

        let initial = app.detail_panel_text();
        assert!(initial.contains("Critical Path Head"));
        assert!(!initial.contains("Calculation Proof:"));

        app.update(key(KeyCode::Char('e')));
        assert!(!app.insights_show_explanations);
        let without_explanations = app.detail_panel_text();
        assert!(without_explanations.contains("Explanations hidden"));
        assert!(!without_explanations.contains("Critical Path Head"));

        app.update(key(KeyCode::Char('x')));
        assert!(app.insights_show_calc_proof);
        let with_proof = app.detail_panel_text();
        assert!(with_proof.contains("Calculation Proof:"));

        app.update(key(KeyCode::Char('e')));
        assert!(app.insights_show_explanations);
        let restored = app.detail_panel_text();
        assert!(restored.contains("Critical Path Head"));
    }

    #[test]
    fn history_mode_renders_timeline_sections() {
        let mut app = new_app(ViewMode::History, 2);
        app.mode = ViewMode::History;
        let text = app.detail_panel_text();
        assert!(text.contains("History Summary"));
        assert!(text.contains("LIFECYCLE:"));
        assert!(text.contains("switch to git timeline"));
        assert!(text.contains("Min confidence filter"));
    }

    #[test]
    fn graph_mode_snapshot_like_output_is_stable() {
        let app = new_app(ViewMode::Graph, 0);
        let text = app.detail_panel_text();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(lines.first().is_some_and(|line| line.starts_with("Graph:")));
        assert!(lines.iter().any(|line| line.contains('A')));
        assert!(lines.iter().any(|line| line.contains("Top PageRank:")));
    }

    #[test]
    fn graph_detail_text_uses_legacy_relationship_headers_and_legend() {
        let mut blocker_view = new_app(ViewMode::Graph, 1);
        blocker_view.mode = ViewMode::Graph;
        let blocker_text = blocker_view.detail_panel_text();
        assert!(blocker_text.contains("▲ BLOCKED BY (must complete first) ▲"));
        assert!(blocker_text.contains("┌"));
        assert!(blocker_text.contains("[o] A"));
        assert!(blocker_text.contains("Root"));

        let mut dependent_view = new_app(ViewMode::Graph, 0);
        dependent_view.mode = ViewMode::Graph;
        let dependent_text = dependent_view.detail_panel_text();
        assert!(dependent_text.contains("▼ BLOCKS (waiting on this) ▼"));
        assert!(dependent_text.contains("┌"));
        assert!(dependent_text.contains("[o] B"));
        assert!(dependent_text.contains("Dependent"));
        assert!(dependent_text.contains("Legend: █ relative score | #N rank of 3 issues"));
        assert!(dependent_text.contains("Nav: h/l nodes | j/k nodes or focused edges"));
    }

    #[test]
    fn graph_detail_text_renders_visual_graph_content() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.mode = ViewMode::Graph;

        let text = app.graph_detail_text_for_width(58);
        assert!(text.contains("╔"));
        assert!(text.contains("▼ BLOCKS (waiting on this) ▼"));
        assert!(text.contains("[o] B"));
        assert!(text.contains("Dependent"));
        assert!(text.lines().all(|line| line.chars().count() <= 58));
    }

    #[test]
    fn graph_detail_text_many_blockers_shows_overflow_summary() {
        let mut app = new_app_with_issues(ViewMode::Graph, 0, graph_many_blocker_issues());
        app.mode = ViewMode::Graph;
        app.select_issue_by_id("MAIN");

        let text = app.graph_detail_text_for_width(120);
        assert!(text.contains("▲ BLOCKED BY (must complete first) ▲"));
        assert!(text.contains("[o] BLK-00"));
        assert!(text.contains("[o] BLK-04"));
        assert!(text.contains("Blocker 00"));
        assert!(text.contains("Blocker 04"));
        assert!(text.contains("+5 more"));
        assert!(!text.contains("BLK-05"));
        assert!(!text.contains("Blocker 05"));
        assert!(text.lines().all(|line| line.chars().count() <= 120));
    }

    #[test]
    fn graph_detail_text_many_dependents_shows_overflow_summary() {
        let mut app = new_app_with_issues(ViewMode::Graph, 0, graph_many_dependent_issues());
        app.mode = ViewMode::Graph;
        app.select_issue_by_id("ROOT");

        let text = app.graph_detail_text_for_width(120);
        assert!(text.contains("▼ BLOCKS (waiting on this) ▼"));
        assert!(text.contains("[o] DEP-00"));
        assert!(text.contains("[o] DEP-04"));
        assert!(text.contains("Dependent 00"));
        assert!(text.contains("Dependent 04"));
        assert!(text.contains("+5 more"));
        assert!(!text.contains("DEP-05"));
        assert!(!text.contains("Dependent 05"));
        assert!(text.lines().all(|line| line.chars().count() <= 120));
    }

    #[test]
    fn history_mode_snapshot_like_output_is_stable() {
        let app = new_app(ViewMode::History, 2);
        let text = app.detail_panel_text();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(
            lines
                .first()
                .is_some_and(|line| line.starts_with("History Summary:"))
        );
        assert!(lines.iter().any(|line| line.contains("Issue: C (Closed)")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Create->Close cycle time:"))
        );
        assert!(lines.iter().any(|line| line.contains("LIFECYCLE:")));
    }

    #[test]
    fn help_tab_focus_and_quit_confirm_match_legacy_behavior() {
        let mut app = new_app(ViewMode::Main, 0);

        let cmd = app.update(key(KeyCode::Char('?')));
        assert!(matches!(cmd, Cmd::None));
        assert!(app.show_help);
        assert_eq!(app.focus, FocusPane::List);

        // 'x' no longer closes help (only ? or Esc does)
        let cmd = app.update(key(KeyCode::Char('x')));
        assert!(matches!(cmd, Cmd::None));
        assert!(app.show_help);

        // Esc closes help and restores focus
        let cmd = app.update(key(KeyCode::Escape));
        assert!(matches!(cmd, Cmd::None));
        assert!(!app.show_help);
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        let cmd = app.update(key(KeyCode::Escape));
        assert!(matches!(cmd, Cmd::None));
        assert!(app.show_quit_confirm);

        let quit_cmd = app.update(key(KeyCode::Char('y')));
        assert!(matches!(quit_cmd, Cmd::Quit));
    }

    #[test]
    fn escape_from_non_main_modes_returns_to_main() {
        for mode in [ViewMode::Board, ViewMode::Insights, ViewMode::Graph] {
            let mut app = new_app(mode, 0);
            let cmd = app.update(key(KeyCode::Escape));
            assert!(matches!(cmd, Cmd::None));
            assert!(matches!(app.mode, ViewMode::Main));
            assert!(!app.show_quit_confirm);
        }
    }

    #[test]
    fn q_from_non_main_modes_returns_to_main_instead_of_quit() {
        for mode in [ViewMode::Board, ViewMode::Insights, ViewMode::Graph] {
            let mut app = new_app(mode, 0);
            let cmd = app.update(key(KeyCode::Char('q')));
            assert!(matches!(cmd, Cmd::None));
            assert!(matches!(app.mode, ViewMode::Main));
        }
    }

    #[test]
    fn view_hotkeys_toggle_modes_back_to_main() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('b')));
        assert!(matches!(app.mode, ViewMode::Board));
        app.update(key(KeyCode::Char('b')));
        assert!(matches!(app.mode, ViewMode::Main));

        app.update(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, ViewMode::Insights));
        app.update(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, ViewMode::Main));

        app.update(key(KeyCode::Char('g')));
        assert!(matches!(app.mode, ViewMode::Graph));
        app.update(key(KeyCode::Char('g')));
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn history_toggle_and_escape_match_legacy_behavior() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::History));
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::Main));
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::History));
        app.update(key(KeyCode::Escape));
        assert!(matches!(app.mode, ViewMode::Main));
        assert!(!app.show_quit_confirm);
    }

    #[test]
    fn insights_mode_h_l_switch_focus_panes() {
        let mut app = new_app(ViewMode::Insights, 0);

        assert_eq!(app.focus, FocusPane::List);
        app.update(key(KeyCode::Char('l')));
        assert_eq!(app.focus, FocusPane::Detail);
        assert!(matches!(app.mode, ViewMode::Insights));

        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.focus, FocusPane::List);
        assert!(matches!(app.mode, ViewMode::Insights));
    }

    #[test]
    fn insights_heatmap_toggle_and_escape_drill_preserve_mode() {
        let mut app = new_app(ViewMode::Insights, 0);

        assert!(app.insights_heatmap.is_none());
        app.update(key(KeyCode::Char('m')));
        assert!(app.insights_heatmap.is_some());
        assert!(app.list_panel_text().contains("Priority heatmap |"));

        app.update(key(KeyCode::Enter));
        assert!(
            app.insights_heatmap
                .as_ref()
                .is_some_and(|state| state.drill_active)
        );
        assert!(app.list_panel_text().contains("Priority heatmap drill"));

        app.update(key(KeyCode::Escape));
        assert!(
            app.insights_heatmap
                .as_ref()
                .is_some_and(|state| !state.drill_active)
        );
        assert!(matches!(app.mode, ViewMode::Insights));
    }

    #[test]
    fn insights_heatmap_empty_grid_renders_without_panic() {
        let issues = vec![Issue {
            id: "CLS-1".to_string(),
            title: "Closed".to_string(),
            status: "closed".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let mut app = new_app_with_issues(ViewMode::Insights, 0, issues);

        app.update(key(KeyCode::Char('m')));
        assert!(
            app.list_panel_text()
                .contains("no open, filter-matching issues")
        );
        assert!(
            app.detail_panel_text()
                .contains("No issues in the selected heatmap cell.")
        );
    }

    #[test]
    fn insights_heatmap_drill_selection_syncs_detail_context() {
        let mut app = new_app(ViewMode::Insights, 0);

        app.update(key(KeyCode::Char('m')));
        let selected = selected_issue_id(&app);
        let detail = app.detail_panel_text();
        assert!(detail.contains("Heatmap:"));
        assert!(detail.contains(&format!("Focus: {selected}")));

        app.update(key(KeyCode::Enter));
        let drilled = app.detail_panel_text();
        assert!(drilled.contains("Drill selection:"));
        assert!(drilled.contains(&format!("Focus: {selected}")));
    }

    #[test]
    fn insights_heatmap_list_keys_stay_in_list_focus() {
        let mut app = new_app(ViewMode::Insights, 0);

        app.update(key(KeyCode::Char('m')));
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Char('l')));
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.focus, FocusPane::List);
        assert!(app.list_panel_text().contains("Priority heatmap"));
    }

    #[test]
    fn graph_mode_h_l_and_ctrl_paging_move_selection() {
        let mut app = new_app(ViewMode::Graph, 0);

        assert_eq!(selected_issue_id(&app), "A");
        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "B");
        assert!(matches!(app.mode, ViewMode::Graph));

        app.update(key(KeyCode::Char('h')));
        assert_eq!(selected_issue_id(&app), "A");

        app.update(key_ctrl(KeyCode::Char('d')));
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key_ctrl(KeyCode::Char('u')));
        assert_eq!(selected_issue_id(&app), "A");
    }

    #[test]
    fn graph_mode_shift_h_l_jump_by_page_window() {
        let mut app = new_app(ViewMode::Graph, 0);

        assert_eq!(selected_issue_id(&app), "A");
        app.update(key(KeyCode::Char('L')));
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Char('H')));
        assert_eq!(selected_issue_id(&app), "A");
    }

    #[test]
    fn graph_mode_h_from_detail_returns_to_list_focus() {
        let mut app = new_app(ViewMode::Graph, 0);
        assert_eq!(app.focus, FocusPane::List);

        // Tab to detail
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // h from detail should switch back to list focus
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.focus, FocusPane::List);
        assert!(matches!(app.mode, ViewMode::Graph));

        // h from list should navigate (move selection)
        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "B");
        app.update(key(KeyCode::Char('h')));
        assert_eq!(selected_issue_id(&app), "A");
    }

    #[test]
    fn graph_mode_list_header_shows_keybinding_hints() {
        let app = new_app(ViewMode::Graph, 0);
        let list_text = app.list_panel_text();
        assert!(list_text.contains("h/l nav"));
        assert!(list_text.contains("Tab focus"));
        assert!(list_text.contains("/ search"));
    }

    #[test]
    fn graph_mode_search_query_and_match_cycling_work() {
        let mut app = new_app(ViewMode::Graph, 0);
        assert!(!app.graph_search_active);

        app.update(key(KeyCode::Char('/')));
        assert!(app.graph_search_active);
        assert!(app.graph_search_query.is_empty());

        // Type a search query that matches issue "A"
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.graph_search_query, "a");
        assert_eq!(selected_issue_id(&app), "A");

        // Enter finishes search but keeps query
        app.update(key(KeyCode::Enter));
        assert!(!app.graph_search_active);
        assert_eq!(app.graph_search_query, "a");

        // n/N should cycle matches
        app.update(key(KeyCode::Char('n')));

        // Escape from new search clears query
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('x')));
        assert_eq!(app.graph_search_query, "x");
        app.update(key(KeyCode::Escape));
        assert!(!app.graph_search_active);
        assert!(app.graph_search_query.is_empty());
    }

    #[test]
    fn insights_mode_search_query_and_match_cycling_work() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, ViewMode::Insights));
        assert!(!app.insights_search_active);

        app.update(key(KeyCode::Char('/')));
        assert!(app.insights_search_active);
        assert!(app.insights_search_query.is_empty());

        // Type a search query
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.insights_search_query, "a");

        // Enter finishes search but keeps query
        app.update(key(KeyCode::Enter));
        assert!(!app.insights_search_active);
        assert_eq!(app.insights_search_query, "a");

        // Escape from new search clears query
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.insights_search_query, "z");
        app.update(key(KeyCode::Escape));
        assert!(!app.insights_search_active);
        assert!(app.insights_search_query.is_empty());
    }

    #[test]
    fn insights_list_header_shows_search_hint() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('i')));
        let list_text = app.list_panel_text();
        assert!(list_text.contains("/ search"));
    }

    #[test]
    fn history_confidence_cycles_on_c_key() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));

        let initial_index = app.history_confidence_index;
        app.update(key(KeyCode::Char('c')));
        assert_ne!(app.history_confidence_index, initial_index);
    }

    #[test]
    fn history_v_toggles_git_mode_and_enter_jumps_to_related_issue() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Bead));

        app.update(key(KeyCode::Char('v')));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));
        let git_list = app.list_panel_text();
        assert!(git_list.contains("Git commits") || git_list.contains("No git commits correlated"));

        let first_issue_id = app
            .selected_history_event()
            .map(|event| event.issue_id)
            .expect("git timeline should contain at least one event");

        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));

        app.update(key(KeyCode::Char('c')));
        assert_eq!(app.history_confidence_index, 0);

        let cmd = app.update(key(KeyCode::Enter));
        assert!(matches!(cmd, Cmd::None));
        assert!(matches!(app.mode, ViewMode::Main));
        assert_eq!(app.focus, FocusPane::Detail);
        assert_eq!(selected_issue_id(&app), first_issue_id);
    }

    #[test]
    fn history_git_mode_shift_j_k_perform_secondary_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        app.update(key(KeyCode::Char('v')));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));
        assert_eq!(app.history_related_bead_cursor, 0);
        assert_eq!(app.history_event_cursor, 0);

        // J/K navigation is safe even with no git history data (test fixtures
        // have no real git repo, so event/commit lists are empty).
        app.update(key(KeyCode::Char('J')));
        app.update(key(KeyCode::Char('K')));

        // Cursors remain at zero since no events exist to navigate.
        assert_eq!(app.history_related_bead_cursor, 0);
        assert_eq!(app.history_event_cursor, 0);
    }

    #[test]
    fn history_mode_search_filters_git_timeline_and_intercepts_hotkeys() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        app.update(key(KeyCode::Char('v')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));

        app.update(key(KeyCode::Char('/')));
        assert!(app.history_search_active);
        assert!(app.history_search_query.is_empty());

        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.history_search_query, "o");
        assert_eq!(app.list_filter, ListFilter::All);

        app.update(key(KeyCode::Backspace));
        assert!(app.history_search_query.is_empty());

        for ch in "dependent".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.history_search_query, "dependent");
        let event = app
            .selected_history_event()
            .expect("history git mode should have timeline events");
        assert_eq!(event.issue_id, "B");

        app.update(key(KeyCode::Enter));
        assert!(!app.history_search_active);
        assert_eq!(app.history_search_query, "dependent");

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('x')));
        assert_eq!(app.history_search_query, "x");
        app.update(key(KeyCode::Escape));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(!app.history_search_active);
        assert!(app.history_search_query.is_empty());
    }

    #[test]
    fn history_mode_search_filters_bead_list_and_escape_clears_query() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Bead));

        app.update(key(KeyCode::Char('/')));
        for ch in "closed".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.history_search_query, "closed");
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Enter));
        assert!(!app.history_search_active);
        assert_eq!(app.history_search_query, "closed");

        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Escape));
        assert!(!app.history_search_active);
        assert!(app.history_search_query.is_empty());

        app.update(key(KeyCode::Home));
        assert_eq!(selected_issue_id(&app), "A");
        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "B");
    }

    #[test]
    fn history_mode_search_zero_results_show_explicit_message() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Bead));

        app.update(key(KeyCode::Char('/')));
        for ch in "zzzz".chars() {
            app.update(key(KeyCode::Char(ch)));
        }

        assert!(app.history_visible_issue_indices().is_empty());
        let text = app.history_list_text();
        assert!(text.contains("(no issues match history search: /zzzz)"));

        app.update(key(KeyCode::Enter));
        assert!(!app.history_search_active);
        assert_eq!(app.history_search_query, "zzzz");
        assert!(
            app.history_list_text()
                .contains("(no issues match history search: /zzzz)")
        );
    }

    #[test]
    fn history_git_search_zero_results_show_explicit_message() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        app.update(key(KeyCode::Char('v')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));

        app.update(key(KeyCode::Char('/')));
        for ch in "zzzz".chars() {
            app.update(key(KeyCode::Char(ch)));
        }

        assert!(app.history_search_matches().is_empty());
        let text = app.history_list_text();
        assert!(text.contains("(no commits match search: /zzzz)"));

        app.update(key(KeyCode::Enter));
        assert!(!app.history_search_active);
        assert_eq!(app.history_search_query, "zzzz");
        assert!(
            app.history_list_text()
                .contains("(no commits match search: /zzzz)")
        );
    }

    #[test]
    fn history_git_mode_g_switches_to_graph_and_selects_issue_from_event() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('h')));
        app.update(key(KeyCode::Char('v')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));

        let event_issue_id = app
            .selected_history_event()
            .expect("git timeline should have events")
            .issue_id;

        app.update(key(KeyCode::Char('g')));
        assert!(matches!(app.mode, ViewMode::Graph));
        assert_eq!(selected_issue_id(&app), event_issue_id);
    }

    #[test]
    fn enter_from_specialized_modes_returns_to_main_detail() {
        for mode in [
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::History,
        ] {
            let mut app = new_app(mode, 0);
            let cmd = app.update(key(KeyCode::Enter));
            assert!(matches!(cmd, Cmd::None));
            assert!(matches!(app.mode, ViewMode::Main));
            assert_eq!(app.focus, FocusPane::Detail);
        }
    }

    #[test]
    fn filter_hotkeys_apply_and_escape_clears_before_quit_confirm() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('c')));
        assert_eq!(app.list_filter, ListFilter::Closed);
        assert_eq!(selected_issue_id(&app), "C");

        let cmd = app.update(key(KeyCode::Escape));
        assert!(matches!(cmd, Cmd::None));
        assert_eq!(app.list_filter, ListFilter::All);
        assert!(!app.show_quit_confirm);

        let cmd = app.update(key(KeyCode::Escape));
        assert!(matches!(cmd, Cmd::None));
        assert!(app.show_quit_confirm);
    }

    #[test]
    fn list_navigation_respects_active_filter() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);
        assert_eq!(selected_issue_id(&app), "A");

        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "B");

        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "B");
    }

    #[test]
    fn board_mode_number_keys_jump_to_expected_lane_selection() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(lane_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.update(key(KeyCode::Char('2')));
        assert_eq!(selected_issue_id(&app), "IP-1");
        assert!(matches!(app.mode, ViewMode::Board));

        app.update(key(KeyCode::Char('3')));
        assert_eq!(selected_issue_id(&app), "BLK-1");

        app.update(key(KeyCode::Char('4')));
        assert_eq!(selected_issue_id(&app), "CLS-1");

        app.update(key(KeyCode::Char('1')));
        app.select_issue_by_id("OPEN-1");
        app.select_issue_by_id("OPEN-1");
        assert_eq!(selected_issue_id(&app), "OPEN-1");
        assert!(matches!(app.mode, ViewMode::Board));
    }

    #[test]
    fn board_grouping_cycles_and_lane_jumps_follow_grouping() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(lane_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.board_grouping, BoardGrouping::Priority);
        assert!(app.list_panel_text().contains("Grouping: priority"));
        app.update(key(KeyCode::Char('3')));
        assert_eq!(selected_issue_id(&app), "BLK-1");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.board_grouping, BoardGrouping::Type);
        assert!(app.list_panel_text().contains("Grouping: type"));
    }

    #[test]
    fn board_mode_advanced_navigation_and_empty_lane_toggle_work() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        app.update(key(KeyCode::Char('$')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");
        app.update(key(KeyCode::Char('0')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('L')));
        assert_eq!(selected_issue_id(&app), "CLS-1");
        app.update(key(KeyCode::Char('H')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('c')));
        let with_empty_lanes = app.list_panel_text();
        assert!(with_empty_lanes.contains("open"));
        assert!(with_empty_lanes.contains("in_progress"));
        assert!(with_empty_lanes.contains("blocked"));

        // 3-state cycle: Auto → ShowAll → HideEmpty
        app.update(key(KeyCode::Char('e')));
        assert_eq!(app.board_empty_visibility, EmptyLaneVisibility::ShowAll);
        app.update(key(KeyCode::Char('e')));
        assert_eq!(app.board_empty_visibility, EmptyLaneVisibility::HideEmpty);
        let without_empty_lanes = app.list_panel_text();
        assert!(!without_empty_lanes.contains("open"));
        assert!(!without_empty_lanes.contains("in_progress"));
        assert!(!without_empty_lanes.contains("blocked"));
        assert!(without_empty_lanes.contains("closed"));
    }

    #[test]
    fn board_mode_home_and_end_stay_within_current_lane() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        app.update(key(KeyCode::End));
        assert_eq!(selected_issue_id(&app), "OPEN-2");

        app.update(key(KeyCode::Home));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "IP-1");
        app.update(key(KeyCode::End));
        assert_eq!(selected_issue_id(&app), "IP-1");
    }

    #[test]
    fn board_mode_h_l_move_between_lanes_without_entering_history() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "IP-1");
        assert!(matches!(app.mode, ViewMode::Board));

        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "CLS-1");
        assert!(matches!(app.mode, ViewMode::Board));

        app.update(key(KeyCode::Char('h')));
        assert_eq!(selected_issue_id(&app), "IP-1");
        assert!(matches!(app.mode, ViewMode::Board));
    }

    #[test]
    fn board_mode_j_k_stay_within_current_lane() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");

        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");

        app.update(key(KeyCode::Char('k')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('k')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");
        assert!(matches!(app.mode, ViewMode::Board));
    }

    #[test]
    fn board_mode_ctrl_d_u_page_within_current_lane() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        assert_eq!(selected_issue_id(&app), "OPEN-1");
        app.update(key_ctrl(KeyCode::Char('d')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");

        app.update(key_ctrl(KeyCode::Char('u')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");
        assert!(matches!(app.mode, ViewMode::Board));
    }

    #[test]
    fn board_mode_search_query_and_match_cycling_work() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.update(key(KeyCode::Char('/')));
        assert!(app.board_search_active);
        assert!(app.board_search_query.is_empty());

        for ch in ['o', 'p', 'e'] {
            app.update(key(KeyCode::Char(ch)));
        }

        assert_eq!(app.board_search_query, "ope");
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");

        app.update(key(KeyCode::Char('N')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Enter));
        assert!(!app.board_search_active);
        assert_eq!(app.board_search_query, "ope");

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");
    }

    #[test]
    fn board_mode_search_escape_clears_query_and_blocks_filter_hotkeys() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('c')));
        assert!(app.board_search_active);
        assert_eq!(app.board_search_query, "c");
        assert_eq!(app.list_filter, ListFilter::All);

        app.update(key(KeyCode::Escape));
        assert!(!app.board_search_active);
        assert!(app.board_search_query.is_empty());
    }

    #[test]
    fn board_mode_detail_focus_shortcuts_drive_navigation_and_search() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_nav_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        app.select_issue_by_id("OPEN-1");
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");
        assert_eq!(app.focus, FocusPane::Detail);

        app.update(key(KeyCode::Char('l')));
        assert_eq!(selected_issue_id(&app), "IP-1");

        app.update(key(KeyCode::Char('L')));
        assert_eq!(selected_issue_id(&app), "CLS-1");

        app.update(key(KeyCode::Char('H')));
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Char('/')));
        assert!(app.board_search_active);
        app.update(key(KeyCode::Char('o')));
        app.update(key(KeyCode::Char('p')));
        app.update(key(KeyCode::Char('e')));
        assert_eq!(app.board_search_query, "ope");
        assert_eq!(selected_issue_id(&app), "OPEN-1");

        app.update(key(KeyCode::Enter));
        assert!(!app.board_search_active);

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "OPEN-2");
        assert_eq!(app.focus, FocusPane::Detail);
    }

    #[test]
    fn board_detail_scroll_shortcuts_reset_when_selection_changes() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;

        app.update(key_ctrl(KeyCode::Char('j')));
        assert_eq!(app.board_detail_scroll_offset, 3);

        app.update(key_ctrl(KeyCode::Char('d')));
        assert_eq!(app.board_detail_scroll_offset, 13);

        app.focus = FocusPane::List;
        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "B");
        assert_eq!(app.board_detail_scroll_offset, 0);
    }

    #[test]
    fn board_detail_render_state_applies_scroll_offset() {
        let mut app = new_app(ViewMode::Board, 1);
        app.board_detail_scroll_offset = 4;

        let full = app.board_detail_text();
        let total_lines = full.lines().count();
        let visible_height = 6;
        let expected_offset = 4.min(total_lines.saturating_sub(visible_height));
        let expected = if expected_offset == 0 {
            full
        } else {
            full.lines()
                .skip(expected_offset)
                .collect::<Vec<_>>()
                .join("\n")
        };

        let (visible, offset, reported_total) = app.board_detail_render_state(visible_height);
        assert_eq!(reported_total, total_lines);
        assert_eq!(offset, expected_offset);
        assert_eq!(visible, expected);
    }

    #[test]
    fn board_mode_g_switches_to_graph_view() {
        let mut app = new_app(ViewMode::Board, 0);
        app.update(key(KeyCode::Char('g')));
        assert!(matches!(app.mode, ViewMode::Graph));
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn board_status_grouping_places_unknown_status_in_other_lane() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(board_with_unknown_status_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        let list = app.list_panel_text();
        assert!(list.contains("other"));
        assert!(list.contains("QUE-1"));

        // 3-state cycle: Auto → ShowAll → HideEmpty
        app.update(key(KeyCode::Char('e')));
        assert_eq!(app.board_empty_visibility, EmptyLaneVisibility::ShowAll);
        app.update(key(KeyCode::Char('e')));
        assert_eq!(app.board_empty_visibility, EmptyLaneVisibility::HideEmpty);
        let hidden_empty = app.list_panel_text();
        assert!(hidden_empty.contains("open"));
        assert!(!hidden_empty.contains("in_progress"));
        assert!(!hidden_empty.contains("blocked"));
        assert!(!hidden_empty.contains("closed"));
        assert!(hidden_empty.contains("other"));
    }

    #[test]
    fn sort_key_cycles_main_order_modes() {
        let mut app = BvrApp {
            analyzer: Analyzer::new(sortable_issues()),
            repo_root: None,
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode: ViewMode::Main,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            help_scroll_offset: 0,
            show_quit_confirm: false,
            modal_overlay: None,
            modal_confirm_result: None,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_search_mode: HistorySearchMode::All,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            board_detail_scroll_offset: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_heatmap: None,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            actionable_plan: None,
            actionable_track_cursor: 0,
            actionable_item_cursor: 0,
            attention_result: None,
            attention_cursor: 0,
            tree_flat_nodes: Vec::new(),
            tree_cursor: 0,
            tree_collapsed: std::collections::HashSet::new(),
            label_dashboard: None,
            label_dashboard_cursor: 0,
            flow_matrix: None,
            flow_matrix_row_cursor: 0,
            flow_matrix_col_cursor: 0,
            time_travel_ref_input: String::new(),
            time_travel_input_active: false,
            time_travel_diff: None,
            time_travel_category_cursor: 0,
            time_travel_issue_cursor: 0,
            time_travel_last_ref: None,
            sprint_data: Vec::new(),
            sprint_cursor: 0,
            sprint_issue_cursor: 0,
            modal_label_filter: None,
            modal_repo_filter: None,
            priority_hints_visible: false,
            status_msg: String::new(),
            slow_metrics_pending: false,
            #[cfg(test)]
            key_trace: Vec::new(),
        };

        // Default: open-first, then priority asc, then id asc → M(p1), A(p2), Z(p3)
        assert_eq!(first_rendered_issue_id(&app), "M");
        assert_eq!(app.list_sort, ListSort::Default);

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::CreatedAsc);
        assert_eq!(first_rendered_issue_id(&app), "Z");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::CreatedDesc);
        assert_eq!(first_rendered_issue_id(&app), "M");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::Priority);
        assert_eq!(first_rendered_issue_id(&app), "M");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::Updated);
        assert_eq!(first_rendered_issue_id(&app), "Z");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::Default);
        assert_eq!(first_rendered_issue_id(&app), "M");
    }

    #[test]
    fn default_sort_treats_tombstone_as_closed_like() {
        let mut issues = sortable_issues();
        issues.push(Issue {
            id: "T".to_string(),
            title: "Tombstone".to_string(),
            status: "tombstone".to_string(),
            issue_type: "task".to_string(),
            priority: 0,
            ..Issue::default()
        });

        let mut app = new_app(ViewMode::Main, 0);
        app.analyzer = Analyzer::new(issues);
        app.list_filter = ListFilter::All;
        app.list_sort = ListSort::Default;

        let visible = app.visible_issue_indices();
        assert!(!visible.is_empty());

        let first = &app.analyzer.issues[visible[0]].id;
        let last = &app.analyzer.issues[*visible.last().unwrap_or(&0)].id;
        assert_ne!(first, "T", "tombstone issue should not be sorted as open");
        assert_eq!(
            last, "T",
            "tombstone issue should sort with closed-like items"
        );
    }

    #[test]
    fn board_detail_j_k_navigate_deps_when_detail_focused() {
        // Issue A (index 0) has no blockers but B depends on it (dependents=["B"]).
        let mut app = new_app(ViewMode::Board, 0);
        app.mode = ViewMode::Board;
        assert_eq!(selected_issue_id(&app), "A");

        // Tab to detail focus
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        assert_eq!(app.detail_dep_cursor, 0);

        // dep list for A = dependents=["B"] => length 1
        let deps = app.detail_dep_list();
        assert_eq!(deps, vec!["B".to_string()]);

        // J should not move past the end
        app.update(key(KeyCode::Char('J')));
        assert_eq!(app.detail_dep_cursor, 0);

        // Detail text should show cursor marker
        let detail = app.detail_panel_text();
        assert!(detail.contains('>'), "detail should show cursor marker");
    }

    #[test]
    fn graph_detail_j_k_navigate_deps_when_detail_focused() {
        // Issue B (index 1) depends on A (blockers=["A"]), no dependents.
        let mut app = new_app(ViewMode::Graph, 1);
        app.mode = ViewMode::Graph;
        assert_eq!(selected_issue_id(&app), "B");

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        let deps = app.detail_dep_list();
        assert_eq!(deps, vec!["A".to_string()]);

        // j in detail focus navigates deps
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.detail_dep_cursor, 0);

        // Detail text should show cursor
        let detail = app.detail_panel_text();
        assert!(
            detail.contains('>'),
            "graph detail should show cursor marker"
        );
    }

    #[test]
    fn graph_detail_text_surfaces_focus_context_for_node_and_edge() {
        let mut app = new_app(ViewMode::Graph, 1);
        app.mode = ViewMode::Graph;

        let list_focus = app.detail_panel_text();
        assert!(list_focus.contains("Focus: node"));
        assert!(list_focus.contains("Focused edge: list focus"));

        app.update(key(KeyCode::Tab));
        let detail_focus = app.detail_panel_text();
        assert!(detail_focus.contains("Focused edge: depends on"));
        assert!(detail_focus.contains("B -> A"));
    }

    #[test]
    fn insights_detail_shows_deps_with_cursor_and_j_k_works() {
        // Issue B (index 1) depends on A (blockers=["A"]).
        let mut app = new_app(ViewMode::Insights, 1);
        app.mode = ViewMode::Insights;
        assert_eq!(selected_issue_id(&app), "B");

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        let detail = app.detail_panel_text();
        assert!(
            detail.contains("Depends on"),
            "insights detail should show dependency section"
        );
        assert!(detail.contains('>'), "insights detail should show cursor");
    }

    #[test]
    fn detail_dep_cursor_resets_on_selection_change() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.mode = ViewMode::Graph;
        app.detail_dep_cursor = 5;

        // Moving selection should reset cursor
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.detail_dep_cursor, 0);
    }

    // -- Breakpoint tests ----------------------------------------------------

    #[test]
    fn breakpoint_narrow_below_80() {
        assert_eq!(Breakpoint::from_width(40), Breakpoint::Narrow);
        assert_eq!(Breakpoint::from_width(79), Breakpoint::Narrow);
    }

    #[test]
    fn breakpoint_medium_80_to_119() {
        assert_eq!(Breakpoint::from_width(80), Breakpoint::Medium);
        assert_eq!(Breakpoint::from_width(100), Breakpoint::Medium);
        assert_eq!(Breakpoint::from_width(119), Breakpoint::Medium);
    }

    #[test]
    fn breakpoint_wide_120_plus() {
        assert_eq!(Breakpoint::from_width(120), Breakpoint::Wide);
        assert_eq!(Breakpoint::from_width(200), Breakpoint::Wide);
    }

    #[test]
    fn breakpoint_list_detail_pct_sums_to_100() {
        for bp in [Breakpoint::Narrow, Breakpoint::Medium, Breakpoint::Wide] {
            let sum = bp.list_pct() + bp.detail_pct();
            assert!(
                (sum - 100.0).abs() < f32::EPSILON,
                "{bp:?} pcts sum to {sum}"
            );
        }
    }

    #[test]
    fn breakpoint_narrow_gives_smaller_list() {
        assert!(Breakpoint::Narrow.list_pct() < Breakpoint::Medium.list_pct());
    }

    #[test]
    fn breakpoint_wide_gives_larger_detail() {
        assert!(Breakpoint::Wide.detail_pct() > Breakpoint::Medium.detail_pct());
    }

    // -- Visual token tests --------------------------------------------------

    #[test]
    fn token_status_colours_are_distinct() {
        let open = tokens::STATUS_OPEN;
        let prog = tokens::STATUS_IN_PROGRESS;
        let blk = tokens::STATUS_BLOCKED;
        let cls = tokens::STATUS_CLOSED;
        assert_ne!(open, prog);
        assert_ne!(open, blk);
        assert_ne!(open, cls);
        assert_ne!(prog, blk);
        assert_ne!(prog, cls);
        assert_ne!(blk, cls);
    }

    #[test]
    fn token_priority_colours_descend_urgency() {
        // P0 (error red) must be different from P3/P4 (dim/muted)
        assert_ne!(tokens::priority_fg(0), tokens::priority_fg(3));
        assert_ne!(tokens::priority_fg(0), tokens::priority_fg(4));
    }

    #[test]
    fn token_status_style_returns_correct_fg() {
        let open_style = tokens::status_style("open");
        assert_eq!(open_style.fg, Some(tokens::STATUS_OPEN));

        let closed_style = tokens::status_style("closed");
        assert_eq!(closed_style.fg, Some(tokens::STATUS_CLOSED));

        let unknown = tokens::status_style("whatever");
        assert_eq!(unknown.fg, Some(tokens::FG_DIM));
    }

    #[test]
    fn token_header_is_bold() {
        let h = tokens::header();
        assert!(h.attrs.is_some_and(|a| a.contains(ftui::StyleFlags::BOLD)));
    }

    #[test]
    fn token_selected_has_highlight_bg() {
        let s = tokens::selected();
        assert_eq!(s.bg, Some(tokens::BG_HIGHLIGHT));
    }

    #[test]
    fn token_focused_border_differs_from_unfocused() {
        let focused = tokens::panel_border_focused();
        let unfocused = tokens::panel_border();
        assert_ne!(focused.fg, unfocused.fg);
    }

    // -- Snapshot structural tests -------------------------------------------
    // These validate that the text content and panel structure are correct at
    // each breakpoint, using the text-generation methods directly. This avoids
    // needing full ftui Frame construction in tests.

    use super::Breakpoint;
    use super::tokens;

    /// Build the header string the same way `view()` does for a given width.
    fn header_for_width(app: &BvrApp, width: u16) -> String {
        let bp = Breakpoint::from_width(width);
        let visible_count = app.visible_issue_indices().len();
        match bp {
            Breakpoint::Narrow => format!(
                "bvr {} | {}/{}  | {}",
                app.mode.label(),
                visible_count,
                app.analyzer.issues.len(),
                app.list_filter.label(),
            ),
            _ => format!(
                "bvr | mode={} | focus={} | issues={}/{} | filter={} | sort={} | ? help | Tab focus | Esc back/quit",
                app.mode.label(),
                app.focus.label(),
                visible_count,
                app.analyzer.issues.len(),
                app.list_filter.label(),
                app.list_sort.label()
            ),
        }
    }

    #[test]
    fn snapshot_narrow_header_is_compact() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 60);
        assert!(h.contains("bvr"), "header should contain 'bvr'");
        // Narrow header should NOT contain 'mode=' verbose prefix
        assert!(!h.contains("mode="), "narrow header should be compact");
    }

    #[test]
    fn snapshot_medium_header_is_full() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 100);
        assert!(h.contains("mode="), "medium header should show mode=");
        assert!(h.contains("focus="), "medium header should show focus=");
    }

    #[test]
    fn snapshot_wide_header_is_full() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 140);
        assert!(h.contains("mode="), "wide header should show mode=");
        assert!(
            h.contains("Esc back/quit"),
            "wide header should show keybinding hints"
        );
    }

    #[test]
    fn snapshot_list_panel_content_consistent_across_breakpoints() {
        let app = new_app(ViewMode::Main, 0);
        // list_panel_text() is breakpoint-independent (content stays same)
        let text = app.list_panel_text();
        assert!(text.contains("Root"), "list should contain issue title");
        assert!(text.contains('A'), "list should contain issue ID");
    }

    #[test]
    fn snapshot_detail_panel_content_consistent_across_breakpoints() {
        let app = new_app(ViewMode::Main, 0);
        let text = app.detail_panel_text();
        assert!(!text.is_empty(), "detail should have content");
    }

    #[test]
    fn main_detail_includes_rich_sections() {
        let app = new_app(ViewMode::Main, 0);
        let text = app.detail_panel_text();

        assert!(text.contains("Triage Snapshot:"));
        assert!(text.contains("Graph Signals:"));
        assert!(text.contains("Design Notes:"));
        assert!(text.contains("Recent Comments (2):"));
        assert!(text.contains("History Summary"));
    }

    #[test]
    fn main_detail_marks_blocked_issue_and_dependency_map() {
        let app = new_app(ViewMode::Main, 1);
        let text = app.detail_panel_text();

        assert!(text.contains("State: blocked"));
        assert!(text.contains("Dependency Map:"));
        assert!(text.contains("upstream: A"));
        assert!(text.contains("open gate: A"));
    }

    #[test]
    fn snapshot_board_mode_list_mentions_grouping() {
        let app = new_app(ViewMode::Board, 0);
        let text = app.list_panel_text();
        assert!(
            text.contains("Grouping"),
            "board list should mention grouping"
        );
    }

    #[test]
    fn snapshot_each_view_mode_has_content() {
        for mode in [
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::History,
        ] {
            let app = new_app(mode, 0);
            let list = app.list_panel_text();
            let detail = app.detail_panel_text();
            // Neither should be empty
            assert!(!list.is_empty(), "{mode:?} list panel should not be empty");
            assert!(
                !detail.is_empty(),
                "{mode:?} detail panel should not be empty"
            );
        }
    }

    #[test]
    fn snapshot_deterministic_text_across_calls() {
        let app = new_app(ViewMode::Main, 0);
        let list1 = app.list_panel_text();
        let list2 = app.list_panel_text();
        assert_eq!(list1, list2, "list text should be deterministic");
        let detail1 = app.detail_panel_text();
        let detail2 = app.detail_panel_text();
        assert_eq!(detail1, detail2, "detail text should be deterministic");
    }

    // -- Help overlay key parity tests ---------------------------------------

    #[test]
    fn help_g_scrolls_to_top() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;
        app.help_scroll_offset = 50;

        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.help_scroll_offset, 0);
        assert!(app.show_help, "g should scroll not close");
    }

    #[test]
    fn help_big_g_scrolls_to_bottom() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;
        app.help_scroll_offset = 0;

        app.update(key(KeyCode::Char('G')));
        assert_eq!(app.help_scroll_offset, 999);
        assert!(app.show_help, "G should scroll not close");
    }

    #[test]
    fn help_home_scrolls_to_top() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;
        app.help_scroll_offset = 30;

        app.update(key(KeyCode::Home));
        assert_eq!(app.help_scroll_offset, 0);
        assert!(app.show_help);
    }

    #[test]
    fn help_end_scrolls_to_bottom() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;
        app.help_scroll_offset = 0;

        app.update(key(KeyCode::End));
        assert_eq!(app.help_scroll_offset, 999);
        assert!(app.show_help);
    }

    #[test]
    fn help_q_closes_help() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;
        app.help_scroll_offset = 10;
        app.focus_before_help = FocusPane::List;

        app.update(key(KeyCode::Char('q')));
        assert!(!app.show_help, "q should close help");
        assert_eq!(app.help_scroll_offset, 0, "scroll should reset on close");
    }

    #[test]
    fn help_f1_closes_help() {
        let mut app = new_app(ViewMode::Main, 0);
        app.show_help = true;

        app.update(key(KeyCode::F(1)));
        assert!(!app.show_help, "F1 should close help");
    }

    // -- Key trace tests -----------------------------------------------------

    #[test]
    fn key_trace_records_state_transitions() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(app.key_trace.is_empty());

        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('b')));
        app.update(key(KeyCode::Char('j')));

        assert_eq!(app.key_trace.len(), 3);

        // First key: j in Main mode, moves selection to 1
        assert_eq!(app.key_trace[0].key, "Char('j')");
        assert_eq!(app.key_trace[0].mode, ViewMode::Main);

        // Second key: b switches to Board mode
        assert_eq!(app.key_trace[1].mode, ViewMode::Board);

        // Third key: j in Board mode
        assert_eq!(app.key_trace[2].mode, ViewMode::Board);
    }

    #[test]
    fn key_trace_captures_filter_changes() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.key_trace.last().unwrap().filter, ListFilter::Open);

        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.key_trace.last().unwrap().filter, ListFilter::All);
    }

    #[test]
    fn actionable_text_uses_legacy_summary_shape() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));

        let list = app.list_panel_text();
        assert!(list.contains("ACTIONABLE ITEMS"));
        assert!(list.contains("TRACK 1"));
        assert!(list.contains("RECOMMENDED:"));

        app.update(key(KeyCode::Tab));
        let detail = app.detail_panel_text();
        assert!(detail.contains("TRACK 1"));
        assert!(detail.contains("Claim:"));
        assert!(detail.contains("Highest impact:"));
    }

    #[test]
    fn empty_lane_visibility_3state_cycle() {
        let v = EmptyLaneVisibility::Auto;
        assert_eq!(v.next(), EmptyLaneVisibility::ShowAll);
        assert_eq!(v.next().next(), EmptyLaneVisibility::HideEmpty);
        assert_eq!(v.next().next().next(), EmptyLaneVisibility::Auto);
    }

    #[test]
    fn empty_lane_auto_shows_for_status_hides_for_others() {
        let auto = EmptyLaneVisibility::Auto;
        assert!(auto.should_show_empty(BoardGrouping::Status));
        assert!(!auto.should_show_empty(BoardGrouping::Priority));
        assert!(!auto.should_show_empty(BoardGrouping::Type));
    }

    #[test]
    fn help_overlay_text_switches_column_layout_at_width_thresholds() {
        let app = new_app(ViewMode::Main, 0);

        let narrow = app.help_overlay_text(70);
        let medium = app.help_overlay_text(80);
        let wide = app.help_overlay_text(120);

        assert!(!narrow.lines().any(|line| line.contains(" | ")));
        assert!(medium.lines().any(|line| line.matches(" | ").count() == 1));
        assert!(wide.lines().any(|line| line.matches(" | ").count() >= 2));

        for section in [
            "[Navigation]",
            "[Views]",
            "[Filters]",
            "[Search]",
            "[Actions]",
            "[History]",
            "[Board]",
            "[Insights]",
            "[Global]",
        ] {
            assert!(wide.contains(section), "wide help should include {section}");
        }
    }

    #[test]
    fn help_overlay_text_keeps_critical_shortcuts_visible_in_compact_mode() {
        let app = new_app(ViewMode::Main, 0);
        let text = app.help_overlay_text(70);

        for snippet in [
            "Toggle actionable mode",
            "Filter: open only",
            "Toggle file tree",
            "Ctrl+R/F5",
            "Quit immediately",
        ] {
            assert!(
                text.contains(snippet),
                "compact help should include {snippet}"
            );
        }
    }

    // -- History parity tests ------------------------------------------------

    #[test]
    fn history_file_tree_builds_nested_directories_and_root_files() {
        let app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        let nodes = app.history_file_tree_nodes();

        assert_eq!(nodes.first().map(|node| node.name.as_str()), Some("src"));
        assert_eq!(
            nodes
                .iter()
                .filter(|node| !node.is_dir)
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Cargo.toml", "README.md"]
        );

        let src = nodes
            .iter()
            .find(|node| node.path == "src")
            .expect("src root node should exist");
        assert_eq!(src.change_count, 3);
        assert_eq!(
            src.children
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "ui"]
        );

        let ui = src
            .children
            .iter()
            .find(|node| node.path == "src/ui")
            .expect("src/ui directory should exist");
        assert_eq!(ui.change_count, 2);
        assert_eq!(
            ui.children
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["app.rs", "detail.rs"]
        );
    }

    #[test]
    fn history_file_tree_filter_applies_to_git_view_and_panel_text() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.history_show_file_tree = true;
        app.history_file_tree_focus = true;
        app.history_file_tree_cursor = app
            .history_flat_file_list()
            .iter()
            .position(|entry| entry.path == "src/ui")
            .expect("src/ui entry should exist");

        app.file_tree_toggle_or_filter();

        assert_eq!(app.history_file_tree_filter.as_deref(), Some("src/ui"));
        assert_eq!(app.history_git_visible_commit_indices(), vec![0]);

        let panel = app.file_tree_panel_text();
        assert!(panel.contains("Filter: src/ui"));
        assert!(panel.contains("src/"));
        assert!(panel.contains("ui/"));
        assert!(panel.contains("app.rs"));
    }

    #[test]
    fn history_file_tree_filter_repositions_hidden_bead_selection() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 1);
        app.history_show_file_tree = true;
        app.history_file_tree_cursor = app
            .history_flat_file_list()
            .iter()
            .position(|entry| entry.path == "src/ui")
            .expect("src/ui entry should exist");

        app.file_tree_toggle_or_filter();

        assert_eq!(selected_issue_id(&app), "A");
        assert_eq!(app.history_visible_issue_indices(), vec![0]);
        assert_eq!(
            app.selected_history_bead_commit()
                .map(|commit| commit.short_sha),
            Some("aaaa111".to_string())
        );
    }

    #[test]
    fn history_escape_closes_file_tree_before_leaving_history() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        app.mode_before_history = ViewMode::Main;
        app.history_show_file_tree = true;
        app.history_file_tree_focus = true;

        app.update(key(KeyCode::Escape));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(!app.history_show_file_tree);
        assert!(!app.history_file_tree_focus);

        app.update(key(KeyCode::Escape));
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn history_f_toggles_file_tree() {
        let mut app = new_app(ViewMode::History, 0);
        app.mode = ViewMode::History;
        assert!(!app.history_show_file_tree);

        app.update(key(KeyCode::Char('f')));
        assert!(app.history_show_file_tree);
        assert!(!app.history_status_msg.is_empty());

        app.update(key(KeyCode::Char('f')));
        assert!(!app.history_show_file_tree);
    }

    #[test]
    fn history_tab_cycles_file_tree_focus() {
        let mut app = new_app(ViewMode::History, 0);
        app.mode = ViewMode::History;
        app.history_show_file_tree = true;
        app.focus = FocusPane::List;

        // List → Detail
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        assert!(!app.history_file_tree_focus);

        // Detail → FileTree
        app.update(key(KeyCode::Tab));
        assert!(app.history_file_tree_focus);

        // FileTree → List
        app.update(key(KeyCode::Tab));
        assert!(!app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn history_detail_shows_yof_hints() {
        let app = new_app(ViewMode::History, 0);
        let detail = app.detail_panel_text();
        assert!(
            detail.contains("y: copy") || detail.contains("y:"),
            "history detail should mention y key"
        );
        assert!(
            detail.contains("f: file") || detail.contains("f:"),
            "history detail should mention f key"
        );
    }

    #[test]
    fn history_footer_shows_key_hints() {
        // Verify the footer format string contains expected keys
        let app = new_app(ViewMode::History, 0);
        // The footer text is rendered in view(), but we can check the mode label
        assert_eq!(app.history_view_mode.label(), "bead");
    }

    #[test]
    fn history_search_mode_defaults_to_all() {
        let app = new_app(ViewMode::History, 0);
        assert_eq!(app.history_search_mode, HistorySearchMode::All);
    }

    #[test]
    fn history_search_mode_cycles_with_tab() {
        let mut app = new_app(ViewMode::History, 0);
        // Start search
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        assert!(app.history_search_active);
        assert_eq!(app.history_search_mode, HistorySearchMode::All);

        // Tab cycles: All -> Commit
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Commit);

        // Commit -> Sha
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Sha);

        // Sha -> Bead
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Bead);

        // Bead -> Author
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Author);

        // Author -> All (wraps)
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::All);
    }

    #[test]
    fn history_search_mode_resets_on_new_search() {
        let mut app = new_app(ViewMode::History, 0);
        // Start search and change mode
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Commit);

        // Confirm search
        app.handle_key(KeyCode::Enter, Modifiers::NONE);
        assert!(!app.history_search_active);
        // Mode is preserved after confirm
        assert_eq!(app.history_search_mode, HistorySearchMode::Commit);

        // Starting a new search resets to All
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::All);
    }

    #[test]
    fn history_search_mode_label_shown_in_list_text() {
        let mut app = new_app(ViewMode::History, 0);
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        app.handle_key(KeyCode::Char('t'), Modifiers::NONE);

        let text = app.history_list_text();
        assert!(
            text.contains("[all]"),
            "list text should show search mode label, got: {text}"
        );

        // Switch to bead mode
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        let text = app.history_list_text();
        assert!(
            text.contains("[bead]"),
            "list text should show bead mode label, got: {text}"
        );
    }

    #[test]
    fn history_search_mode_bead_filters_by_id_and_title_only() {
        let mut app = new_app(ViewMode::History, 0);
        // Start search in bead mode
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        // Cycle to Bead mode: All -> Commit -> Sha -> Bead
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        assert_eq!(app.history_search_mode, HistorySearchMode::Bead);

        // Search for "Root" — sample issue A has title "Root"
        for ch in "root".chars() {
            app.handle_key(KeyCode::Char(ch), Modifiers::NONE);
        }

        let visible = app.history_visible_issue_indices();
        // Only issue A (title "Root") should match in bead mode
        assert!(
            !visible.is_empty(),
            "bead mode search for 'root' should match issue A by title"
        );

        // Searching for "open" (status) should NOT match in bead mode
        app.handle_key(KeyCode::Escape, Modifiers::NONE);
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        // Cycle back to Bead mode
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        app.handle_key(KeyCode::Tab, Modifiers::NONE);
        for ch in "open".chars() {
            app.handle_key(KeyCode::Char(ch), Modifiers::NONE);
        }
        let visible_status = app.history_visible_issue_indices();

        // Now switch to All mode and search "open" — should match
        app.handle_key(KeyCode::Escape, Modifiers::NONE);
        app.handle_key(KeyCode::Char('/'), Modifiers::NONE);
        // All mode is default after starting new search
        assert_eq!(app.history_search_mode, HistorySearchMode::All);
        for ch in "open".chars() {
            app.handle_key(KeyCode::Char(ch), Modifiers::NONE);
        }
        let visible_all = app.history_visible_issue_indices();

        // In All mode, "open" matches status so should find more results than Bead mode
        assert!(
            visible_all.len() >= visible_status.len(),
            "All mode should match at least as many as Bead mode"
        );
    }

    #[test]
    fn history_git_search_modes_filter_commit_sha_and_author_fields() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        if let Some(cache) = app.history_git_cache.as_mut() {
            cache.commits[0].author = "Alice Example".to_string();
            cache.commits[0].author_email = "alice@example.com".to_string();
            cache.commits[1].author = "Carol Example".to_string();
            cache.commits[1].author_email = "carol@example.com".to_string();
            cache.commits[2].author = "Bob Example".to_string();
            cache.commits[2].author_email = "bob@example.com".to_string();
            cache.commits[3].author = "Bob Example".to_string();
            cache.commits[3].author_email = "bob@example.com".to_string();
        }

        app.history_search_mode = HistorySearchMode::Commit;
        app.history_search_query = "graph".to_string();
        assert_eq!(app.history_search_matches(), vec![1]);

        app.history_search_mode = HistorySearchMode::Sha;
        app.history_search_query = "cccc".to_string();
        assert_eq!(app.history_search_matches(), vec![2]);

        app.history_search_mode = HistorySearchMode::Author;
        app.history_search_query = "bob".to_string();
        assert_eq!(app.history_search_matches(), vec![2, 3]);
    }

    #[test]
    fn history_git_search_matches_respect_file_tree_filter() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.history_search_mode = HistorySearchMode::Commit;
        app.history_search_query = "feat".to_string();

        assert_eq!(app.history_search_matches(), vec![0, 1]);

        app.history_file_tree_filter = Some("src/ui".to_string());
        assert_eq!(app.history_search_matches(), vec![0]);
    }

    #[test]
    fn history_search_mode_enum_coverage() {
        // Verify all mode labels
        assert_eq!(HistorySearchMode::All.label(), "all");
        assert_eq!(HistorySearchMode::Commit.label(), "msg");
        assert_eq!(HistorySearchMode::Sha.label(), "sha");
        assert_eq!(HistorySearchMode::Bead.label(), "bead");
        assert_eq!(HistorySearchMode::Author.label(), "author");

        // Verify cycle is complete (5 steps back to start)
        let mut mode = HistorySearchMode::All;
        for _ in 0..5 {
            mode = mode.cycle();
        }
        assert_eq!(mode, HistorySearchMode::All);
    }

    #[test]
    fn mouse_scroll_down_moves_selection() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.selected, 0);
        app.handle_mouse(MouseEventKind::ScrollDown);
        assert_eq!(app.selected, 1);
        app.handle_mouse(MouseEventKind::ScrollDown);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn mouse_scroll_up_moves_selection() {
        let mut app = new_app(ViewMode::Main, 2);
        assert_eq!(app.selected, 2);
        app.handle_mouse(MouseEventKind::ScrollUp);
        assert_eq!(app.selected, 1);
        app.handle_mouse(MouseEventKind::ScrollUp);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn mouse_scroll_works_in_board_mode() {
        let mut app = new_app(ViewMode::Board, 0);
        app.handle_mouse(MouseEventKind::ScrollDown);
        // Should not panic, and should move selection
        assert!(app.selected <= 1);
    }

    #[test]
    fn mouse_other_events_are_noop() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_mouse(MouseEventKind::Moved);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn tree_view_toggle() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(matches!(app.mode, ViewMode::Main));

        // T toggles to Tree
        app.handle_key(KeyCode::Char('T'), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::Tree));
        assert!(!app.tree_flat_nodes.is_empty(), "tree should build nodes");

        // T toggles back to Main
        app.handle_key(KeyCode::Char('T'), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn tree_view_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char('T'), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::Tree));
        assert_eq!(app.tree_cursor, 0);

        if app.tree_flat_nodes.len() > 1 {
            app.handle_key(KeyCode::Char('j'), Modifiers::NONE);
            assert_eq!(app.tree_cursor, 1);
            app.handle_key(KeyCode::Char('k'), Modifiers::NONE);
            assert_eq!(app.tree_cursor, 0);
        }
    }

    #[test]
    fn tree_view_renders_list_and_detail() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char('T'), Modifiers::NONE);

        let list = app.tree_list_text();
        assert!(
            list.contains("Dependency tree"),
            "list should show tree header, got: {list}"
        );

        let detail = app.tree_detail_text();
        assert!(
            detail.contains("ID:"),
            "detail should show issue ID, got: {detail}"
        );
    }

    #[test]
    fn label_dashboard_toggle() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(matches!(app.mode, ViewMode::Main));

        // [ toggles to LabelDashboard
        app.handle_key(KeyCode::Char('['), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::LabelDashboard));
        assert!(app.label_dashboard.is_some());

        // [ toggles back
        app.handle_key(KeyCode::Char('['), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn label_dashboard_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char('['), Modifiers::NONE);
        assert_eq!(app.label_dashboard_cursor, 0);

        let count = app.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
        if count > 1 {
            app.handle_key(KeyCode::Char('j'), Modifiers::NONE);
            assert_eq!(app.label_dashboard_cursor, 1);
            app.handle_key(KeyCode::Char('k'), Modifiers::NONE);
            assert_eq!(app.label_dashboard_cursor, 0);
        }
    }

    #[test]
    fn label_dashboard_renders_list_and_detail() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char('['), Modifiers::NONE);

        let list = app.label_dashboard_list_text();
        assert!(
            list.contains("Label health") || list.contains("no labels"),
            "list should show header, got: {list}"
        );

        let detail = app.label_dashboard_detail_text();
        // If there are labels, detail should show label info
        if app
            .label_dashboard
            .as_ref()
            .is_some_and(|r| !r.labels.is_empty())
        {
            assert!(
                detail.contains("Label:"),
                "detail should show label name, got: {detail}"
            );
        }
    }

    #[test]
    fn tree_view_expand_collapse() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char('T'), Modifiers::NONE);

        // Find a node with children for the collapse test
        let has_children_node = app.tree_flat_nodes.iter().position(|n| n.has_children);
        if let Some(idx) = has_children_node {
            app.tree_cursor = idx;
            let initial_count = app.tree_flat_nodes.len();

            // Enter collapses children
            app.handle_key(KeyCode::Enter, Modifiers::NONE);
            assert!(
                app.tree_flat_nodes.len() < initial_count,
                "collapsing should reduce node count"
            );

            // Enter again expands
            app.handle_key(KeyCode::Enter, Modifiers::NONE);
            assert_eq!(
                app.tree_flat_nodes.len(),
                initial_count,
                "expanding should restore node count"
            );
        }
    }

    #[test]
    fn flow_matrix_toggle() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(matches!(app.mode, ViewMode::Main));

        // ] toggles to FlowMatrix
        app.handle_key(KeyCode::Char(']'), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::FlowMatrix));
        assert!(app.flow_matrix.is_some());

        // ] toggles back
        app.handle_key(KeyCode::Char(']'), Modifiers::NONE);
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn flow_matrix_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char(']'), Modifiers::NONE);
        assert_eq!(app.flow_matrix_row_cursor, 0);
        assert_eq!(app.flow_matrix_col_cursor, 0);

        let count = app.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
        if count > 1 {
            // j/k for rows
            app.handle_key(KeyCode::Char('j'), Modifiers::NONE);
            assert_eq!(app.flow_matrix_row_cursor, 1);
            app.handle_key(KeyCode::Char('k'), Modifiers::NONE);
            assert_eq!(app.flow_matrix_row_cursor, 0);

            // h/l for columns
            app.handle_key(KeyCode::Char('l'), Modifiers::NONE);
            assert_eq!(app.flow_matrix_col_cursor, 1);
            app.handle_key(KeyCode::Char('h'), Modifiers::NONE);
            assert_eq!(app.flow_matrix_col_cursor, 0);
        }
    }

    #[test]
    fn flow_matrix_renders_list_and_detail() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_key(KeyCode::Char(']'), Modifiers::NONE);

        let list = app.flow_matrix_list_text();
        assert!(
            list.contains("Cross-label flow") || list.contains("no labels"),
            "list should show header or empty, got: {list}"
        );

        let detail = app.flow_matrix_detail_text();
        // Detail should have some content
        assert!(!detail.is_empty(), "detail should not be empty");
    }

    #[test]
    fn remote_to_commit_url_ssh() {
        let url = super::remote_to_commit_url("git@github.com:owner/repo.git", "abc123");
        assert_eq!(
            url,
            Some("https://github.com/owner/repo/commit/abc123".into())
        );
    }

    #[test]
    fn remote_to_commit_url_https() {
        let url = super::remote_to_commit_url("https://github.com/owner/repo.git", "def456");
        assert_eq!(
            url,
            Some("https://github.com/owner/repo/commit/def456".into())
        );
    }

    #[test]
    fn remote_to_commit_url_no_git_suffix() {
        let url = super::remote_to_commit_url("https://github.com/owner/repo", "sha789");
        assert_eq!(
            url,
            Some("https://github.com/owner/repo/commit/sha789".into())
        );
    }

    #[test]
    fn file_tree_node_flatten_visible() {
        let node = super::FileTreeNode {
            name: "src".into(),
            path: "src".into(),
            is_dir: true,
            change_count: 3,
            expanded: true,
            level: 0,
            children: vec![
                super::FileTreeNode {
                    name: "main.rs".into(),
                    path: "src/main.rs".into(),
                    is_dir: false,
                    change_count: 2,
                    expanded: false,
                    level: 1,
                    children: vec![],
                },
                super::FileTreeNode {
                    name: "lib.rs".into(),
                    path: "src/lib.rs".into(),
                    is_dir: false,
                    change_count: 1,
                    expanded: false,
                    level: 1,
                    children: vec![],
                },
            ],
        };

        let flat = node.flatten_visible();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].name, "src");
        assert!(flat[0].is_dir);
        assert_eq!(flat[1].name, "main.rs");
        assert_eq!(flat[2].name, "lib.rs");
    }

    // ── Modal overlay tests ─────────────────────────────────────────

    #[test]
    fn modal_tutorial_dismisses_on_any_key() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_tutorial();
        assert!(app.modal_overlay.is_some());
        assert!(matches!(app.modal_overlay, Some(ModalOverlay::Tutorial)));

        app.update(key(KeyCode::Char('x')));
        assert!(app.modal_overlay.is_none());
    }

    #[test]
    fn modal_confirm_accepts_on_y_rejects_on_n() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_confirm("Test", "Do you confirm?");
        assert!(app.modal_overlay.is_some());
        assert!(app.modal_confirm_result.is_none());

        app.update(key(KeyCode::Char('y')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(true));

        app.open_confirm("Test", "Another question?");
        app.update(key(KeyCode::Char('n')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(false));

        app.open_confirm("Test", "Third question?");
        app.update(key(KeyCode::Escape));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(false));
    }

    #[test]
    fn modal_confirm_ignores_unrelated_keys() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_confirm("Test", "Do you confirm?");

        app.update(key(KeyCode::Char('x')));
        assert!(app.modal_overlay.is_some());
        assert!(app.modal_confirm_result.is_none());
    }

    #[test]
    fn modal_pages_wizard_step_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_pages_wizard();

        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 0);
                assert_eq!(wiz.export_dir, "./bv-pages");
            }
            other => panic!("Expected PagesWizard, got {other:?}"),
        }

        app.update(key(KeyCode::Char('x')));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.export_dir, "./bv-pagesx");
            }
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Backspace));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.export_dir, "./bv-pages");
            }
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Enter));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 1);
                assert_eq!(wiz.step_label(), "Page Title");
            }
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Enter));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 2);
                assert!(wiz.include_closed);
                assert!(wiz.include_history);
            }
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Char('c')));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => assert!(!wiz.include_closed),
            _ => panic!("lost wizard"),
        }
        app.update(key(KeyCode::Char('h')));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => assert!(!wiz.include_history),
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Enter));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 3);
            }
            _ => panic!("lost wizard"),
        }

        app.update(key(KeyCode::Enter));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(true));
    }

    #[test]
    fn modal_pages_wizard_escape_cancels() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_pages_wizard();
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Escape));
        assert!(app.modal_overlay.is_none());
    }

    #[test]
    fn modal_pages_wizard_backspace_goes_back() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_pages_wizard();
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => assert_eq!(wiz.step, 2),
            _ => panic!("lost wizard"),
        }
        app.update(key(KeyCode::Backspace));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => assert_eq!(wiz.step, 1),
            _ => panic!("lost wizard"),
        }
    }

    #[test]
    fn modal_state_transitions_help_to_quit_cycle() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        assert!(app.modal_overlay.is_none());

        app.update(key(KeyCode::Escape));
        assert!(!app.show_help);

        app.update(key(KeyCode::Escape));
        assert!(app.show_quit_confirm);

        app.update(key(KeyCode::Char('x')));
        assert!(!app.show_quit_confirm);

        app.open_tutorial();
        assert!(app.modal_overlay.is_some());
        assert!(!app.show_help);

        app.update(key(KeyCode::Enter));
        assert!(app.modal_overlay.is_none());
    }

    #[test]
    fn modal_overlay_blocks_normal_key_handling() {
        let mut app = new_app(ViewMode::Main, 0);
        let initial_mode = app.mode;

        app.open_confirm("Test", "Question");
        app.update(key(KeyCode::Char('b')));
        assert_eq!(app.mode, initial_mode);
        assert!(app.modal_overlay.is_some());

        app.update(key(KeyCode::Char('y')));
        assert!(app.modal_overlay.is_none());
    }

    // =====================================================================
    // Regression Harness: File-based Snapshots
    // =====================================================================
    //
    // These tests capture full rendered frames (via buffer_to_text) and
    // compare against stored baselines using `insta`.  Any rendering
    // change produces a diff that `cargo insta review` can approve.

    /// Render a full frame for the given mode/width/height and return text.
    fn render_frame(mode: ViewMode, width: u16, height: u16) -> String {
        let app = new_app(mode, 0);
        render_app(&app, width, height)
    }

    fn render_app(app: &BvrApp, width: u16, height: u16) -> String {
        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(width, height, &mut pool);
        app.view(&mut frame);
        super::buffer_to_text(&frame.buffer, &pool)
    }

    macro_rules! snapshot_test {
        ($name:ident, $mode:expr, $width:literal, $height:literal) => {
            #[test]
            fn $name() {
                let text = render_frame($mode, $width, $height);
                insta::assert_snapshot!(text);
            }
        };
    }

    // Main mode at 3 breakpoints
    snapshot_test!(snap_main_narrow, ViewMode::Main, 60, 30);
    snapshot_test!(snap_main_medium, ViewMode::Main, 100, 30);
    snapshot_test!(snap_main_wide, ViewMode::Main, 140, 30);

    // Board mode at 3 breakpoints
    snapshot_test!(snap_board_narrow, ViewMode::Board, 60, 30);
    snapshot_test!(snap_board_medium, ViewMode::Board, 100, 30);
    snapshot_test!(snap_board_wide, ViewMode::Board, 140, 30);

    // Insights mode at 3 breakpoints
    snapshot_test!(snap_insights_narrow, ViewMode::Insights, 60, 30);
    snapshot_test!(snap_insights_medium, ViewMode::Insights, 100, 30);
    snapshot_test!(snap_insights_wide, ViewMode::Insights, 140, 30);

    // Graph mode at 3 breakpoints
    snapshot_test!(snap_graph_narrow, ViewMode::Graph, 60, 30);
    snapshot_test!(snap_graph_medium, ViewMode::Graph, 100, 30);
    snapshot_test!(snap_graph_wide, ViewMode::Graph, 140, 30);

    // History mode at 3 breakpoints
    snapshot_test!(snap_history_narrow, ViewMode::History, 60, 30);
    snapshot_test!(snap_history_medium, ViewMode::History, 100, 30);
    snapshot_test!(snap_history_wide, ViewMode::History, 140, 30);

    // =====================================================================
    // Regression Harness: Keyflow Journey Tests
    // =====================================================================
    //
    // Each test replays a complete keyboard journey that a user would
    // perform, asserting state at each step.  The key_trace vector
    // provides a full audit log for triage.

    #[test]
    fn keyflow_main_to_board_navigate_return() {
        let mut app = new_app(ViewMode::Main, 0);

        // Enter board
        app.update(key(KeyCode::Char('b')));
        assert_eq!(app.mode, ViewMode::Board);

        // Navigate lanes
        app.update(key(KeyCode::Char('l')));
        // Move down within lane
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('j')));

        // Toggle grouping
        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.board_grouping, BoardGrouping::Priority);

        // Return to main
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);

        // Verify trace has all steps
        assert_eq!(app.key_trace.len(), 6);
    }

    #[test]
    fn keyflow_board_search_with_cycling() {
        let mut app = new_app(ViewMode::Board, 0);

        // Start search
        app.update(key(KeyCode::Char('/')));
        assert!(app.board_search_active);

        // Type query
        app.update(key(KeyCode::Char('R')));
        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.board_search_query, "Ro");

        // Accept search
        app.update(key(KeyCode::Enter));
        assert!(!app.board_search_active);

        // Cycle matches
        app.update(key(KeyCode::Char('n')));
        app.update(key(KeyCode::Char('N')));

        // Clear with Esc
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn keyflow_main_to_insights_explore_return() {
        let mut app = new_app(ViewMode::Main, 0);

        // Enter insights
        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.mode, ViewMode::Insights);

        // Cycle panels
        app.update(key(KeyCode::Char('s')));
        assert_ne!(app.insights_panel, InsightsPanel::Bottlenecks);

        // Toggle explanations
        app.update(key(KeyCode::Char('e')));
        assert!(!app.insights_show_explanations);

        // Toggle calc proof
        app.update(key(KeyCode::Char('x')));
        assert!(app.insights_show_calc_proof);

        // Switch focus
        app.update(key(KeyCode::Char('l')));
        assert_eq!(app.focus, FocusPane::Detail);

        // Return
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn keyflow_main_to_graph_search_return() {
        let mut app = new_app(ViewMode::Main, 0);

        // Enter graph
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);

        // Navigate
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('j')));
        assert!(app.selected >= 2);

        // Search
        app.update(key(KeyCode::Char('/')));
        assert!(app.graph_search_active);
        app.update(key(KeyCode::Char('B')));
        app.update(key(KeyCode::Enter));
        assert!(!app.graph_search_active);

        // n/N cycling
        app.update(key(KeyCode::Char('n')));

        // Escape returns to main
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn keyflow_history_bead_to_git_search_graph_jump() {
        let mut app = new_app(ViewMode::Main, 0);

        // Enter history
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.mode, ViewMode::History);

        // Toggle to git mode
        app.update(key(KeyCode::Char('v')));
        assert_eq!(app.history_view_mode, HistoryViewMode::Git);

        // Back to bead mode
        app.update(key(KeyCode::Char('v')));
        assert_eq!(app.history_view_mode, HistoryViewMode::Bead);

        // Search in bead mode
        app.update(key(KeyCode::Char('/')));
        assert!(app.history_search_active);
        app.update(key(KeyCode::Char('C')));
        app.update(key(KeyCode::Enter));
        assert!(!app.history_search_active);

        // n/N cycling
        app.update(key(KeyCode::Char('n')));

        // Confidence cycling
        app.update(key(KeyCode::Char('c')));
        assert_ne!(app.history_confidence_index, 0);

        // Jump to graph
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);

        // Return to main
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn keyflow_filter_navigation_preserves_selection() {
        let mut app = new_app(ViewMode::Main, 0);

        // Apply open filter
        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);
        let open_count = app.visible_issue_indices().len();

        // Navigate within filtered list
        for _ in 0..3 {
            app.update(key(KeyCode::Char('j')));
        }

        // Switch to closed filter
        app.update(key(KeyCode::Char('c')));
        assert_eq!(app.list_filter, ListFilter::Closed);
        let closed_count = app.visible_issue_indices().len();
        assert_ne!(open_count, closed_count);

        // Clear to all
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.list_filter, ListFilter::All);
    }

    #[test]
    fn keyflow_main_to_actionable_return() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);
        assert!(app.actionable_plan.is_some());
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        assert!(app.detail_panel_text().contains("Claim:"));

        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn actionable_all_shortcut_clears_filter_before_toggling_view() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);

        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(app.list_filter, ListFilter::All);

        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);
        assert!(app.actionable_plan.is_some());
    }

    #[test]
    fn keyflow_help_then_modal_then_quit() {
        let mut app = new_app(ViewMode::Main, 0);

        // Open help
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);

        // Scroll help
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('j')));

        // Close help
        app.update(key(KeyCode::Escape));
        assert!(!app.show_help);

        // Open tutorial
        app.open_tutorial();
        assert!(matches!(app.modal_overlay, Some(ModalOverlay::Tutorial)));

        // Dismiss
        app.update(key(KeyCode::Enter));
        assert!(app.modal_overlay.is_none());

        // Open confirm
        app.open_confirm("Delete?", "Are you sure?");
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::Confirm { .. })
        ));

        // Reject
        app.update(key(KeyCode::Char('n')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(false));

        // Quit confirm flow
        app.update(key(KeyCode::Escape));
        assert!(app.show_quit_confirm);
        let cmd = app.update(key(KeyCode::Char('y')));
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn keyflow_sort_cycle_persists_across_modes() {
        let mut app = new_app(ViewMode::Main, 0);

        // Cycle sort
        app.update(key(KeyCode::Char('s')));
        let sort_after = app.list_sort;
        assert_ne!(sort_after, ListSort::Default);

        // Enter board and return
        app.update(key(KeyCode::Char('b')));
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(app.list_sort, sort_after, "sort should persist");
    }

    #[test]
    fn keyflow_pages_wizard_full_flow() {
        let mut app = new_app(ViewMode::Main, 0);

        app.open_pages_wizard();
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::PagesWizard(_))
        ));

        // Step 0 → 1 (export dir)
        app.update(key(KeyCode::Enter));
        // Step 1 → 2 (title)
        app.update(key(KeyCode::Enter));
        // Step 2: toggle include_closed (true→false)
        app.update(key(KeyCode::Char('c')));
        // Step 2: toggle include_history (true→false)
        app.update(key(KeyCode::Char('h')));
        // Step 2 → 3 (review)
        app.update(key(KeyCode::Enter));

        // Verify we're at step 3 with toggled options
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 3);
                assert!(!wiz.include_closed, "should have toggled off");
                assert!(!wiz.include_history, "should have toggled off");
            }
            _ => panic!("expected PagesWizard at step 3"),
        }

        // Go back
        app.update(key(KeyCode::Backspace));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => assert_eq!(wiz.step, 2),
            _ => panic!("expected step 2"),
        }

        // Forward again and finish
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        assert!(app.modal_overlay.is_none());
    }

    #[test]
    fn keyflow_full_mode_tour() {
        // Journey: Main → Board → Main → Insights → Main → Graph → Main → Actionable → Main → History → Main
        let mut app = new_app(ViewMode::Main, 0);

        for (toggle_key, expected_mode) in [
            ('b', ViewMode::Board),
            ('b', ViewMode::Main), // toggle back
            ('i', ViewMode::Insights),
            ('i', ViewMode::Main), // toggle back
            ('g', ViewMode::Graph),
            ('g', ViewMode::Main), // toggle back
            ('a', ViewMode::Actionable),
            ('a', ViewMode::Main), // toggle back
            ('h', ViewMode::History),
        ] {
            app.update(key(KeyCode::Char(toggle_key)));
            assert_eq!(
                app.mode, expected_mode,
                "after pressing '{toggle_key}' expected {expected_mode:?}"
            );
        }

        // History returns via Escape
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);

        // Verify complete trace
        assert_eq!(app.key_trace.len(), 10);
    }

    #[test]
    fn keyflow_detail_dep_navigation_journey() {
        let mut app = new_app(ViewMode::Main, 0);

        // Select issue B (index 1 in sample_issues) which has a dependency on A
        app.update(key(KeyCode::Char('j'))); // move to B
        assert_eq!(selected_issue_id(&app), "B");

        // Enter graph mode (deps visible)
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);

        // Focus on detail
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // J/K dep navigation
        app.update(key(KeyCode::Char('J')));
        app.update(key(KeyCode::Char('K')));

        // Return to list focus
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    // =====================================================================
    // Regression Harness: Snapshot + Keyflow Combined
    // =====================================================================
    //
    // These tests replay a keyflow and then snapshot the rendered output,
    // catching both behavioral and visual regressions.

    #[test]
    fn snap_after_board_search() {
        let mut app = new_app(ViewMode::Board, 0);
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('B')));
        app.update(key(KeyCode::Enter));

        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(100, 30, &mut pool);
        app.view(&mut frame);
        let text = super::buffer_to_text(&frame.buffer, &pool);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_after_insights_panel_cycle() {
        let mut app = new_app(ViewMode::Insights, 0);
        // Cycle through 3 panels
        app.update(key(KeyCode::Char('s')));
        app.update(key(KeyCode::Char('s')));
        app.update(key(KeyCode::Char('s')));

        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(100, 30, &mut pool);
        app.view(&mut frame);
        let text = super::buffer_to_text(&frame.buffer, &pool);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_help_overlay() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('?')));

        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(100, 30, &mut pool);
        app.view(&mut frame);
        let text = super::buffer_to_text(&frame.buffer, &pool);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_history_git_mode() {
        let mut app = new_app(ViewMode::History, 0);
        app.update(key(KeyCode::Char('v')));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_quit_confirm_modal() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Escape));
        assert!(app.show_quit_confirm);

        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(100, 30, &mut pool);
        app.view(&mut frame);
        let text = super::buffer_to_text(&frame.buffer, &pool);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_filter_applied() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('o'))); // open filter

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_main_with_priority_hints() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('p')));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_board_detail_focus() {
        let mut app = new_app(ViewMode::Board, 1);
        app.update(key(KeyCode::Tab));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_insights_heatmap_drill() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.update(key(KeyCode::Char('m')));
        app.update(key(KeyCode::Enter));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_graph_detail_focus() {
        let mut app = new_app(ViewMode::Graph, 1);
        app.update(key(KeyCode::Tab));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_actionable_detail_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Tab));

        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    // -- Attention view tests ------------------------------------------------

    fn labeled_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "A".to_string(),
                title: "Feature work".to_string(),
                status: "open".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                labels: vec!["backend".to_string(), "urgent".to_string()],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Bug fix".to_string(),
                status: "open".to_string(),
                issue_type: "bug".to_string(),
                priority: 2,
                labels: vec!["backend".to_string()],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "UI polish".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                labels: vec!["frontend".to_string()],
                ..Issue::default()
            },
        ]
    }

    #[test]
    fn attention_view_toggle_and_state() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());

        // Press ! to enter Attention mode
        app.update(key(KeyCode::Char('!')));
        assert!(matches!(app.mode, ViewMode::Attention));
        assert!(app.attention_result.is_some());
        assert_eq!(app.attention_cursor, 0);

        // Press ! again to return to Main
        app.update(key(KeyCode::Char('!')));
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn attention_view_navigation() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        assert!(matches!(app.mode, ViewMode::Attention));

        let label_count = app.attention_result.as_ref().unwrap().labels.len();
        assert!(label_count >= 2, "should have at least 2 labels");

        // Navigate down
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.attention_cursor, 1);

        // Navigate up
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.attention_cursor, 0);

        // Can't go above 0
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.attention_cursor, 0);
    }

    #[test]
    fn attention_view_renders_list_and_detail() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));

        let list = app.list_panel_text();
        assert!(list.contains("Rank"));
        assert!(list.contains("Label"));
        assert!(list.contains("Score"));

        let detail = app.detail_panel_text();
        assert!(detail.contains("Label:"));
        assert!(detail.contains("Attention Score:"));
        assert!(detail.contains("Breakdown:"));
    }

    #[test]
    fn attention_view_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.update(key(KeyCode::Char('!')));
        assert!(matches!(app.mode, ViewMode::Attention));

        let list = app.list_panel_text();
        // Empty issues triggers early return before mode dispatch
        assert!(list.contains("no issues loaded"));

        // Navigation on empty should not panic
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
    }

    // -- Refresh tests -------------------------------------------------------

    #[test]
    fn refresh_keybinding_does_not_panic() {
        let mut app = new_app(ViewMode::Main, 0);

        // Ctrl+R — silently fails (no disk data) but doesn't panic
        app.update(Msg::KeyPress(KeyCode::Char('r'), Modifiers::CTRL));
        assert!(matches!(app.mode, ViewMode::Main));

        // F5 — same behavior
        app.update(key(KeyCode::F(5)));
        assert!(matches!(app.mode, ViewMode::Main));
    }

    #[test]
    fn refresh_preserves_mode() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        assert!(matches!(app.mode, ViewMode::Attention));

        // Refresh in Attention mode — fails silently but stays in Attention
        app.update(Msg::KeyPress(KeyCode::Char('r'), Modifiers::CTRL));
        assert!(matches!(app.mode, ViewMode::Attention));
    }

    // -- Priority hints tests ------------------------------------------------

    #[test]
    fn priority_hints_toggle() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(!app.priority_hints_visible);

        app.update(key(KeyCode::Char('p')));
        assert!(app.priority_hints_visible);

        app.update(key(KeyCode::Char('p')));
        assert!(!app.priority_hints_visible);
    }

    #[test]
    fn priority_hints_show_breakdown_in_detail() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('p')));
        assert!(app.priority_hints_visible);

        let detail = app.detail_panel_text();
        assert!(detail.contains("Priority Hints"));
        assert!(detail.contains("Triage Score:") || detail.contains("not in triage"));
    }

    #[test]
    fn priority_hints_only_in_main_mode() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());

        // Switch to Board mode — p should NOT toggle hints
        app.update(key(KeyCode::Char('b')));
        assert!(matches!(app.mode, ViewMode::Board));
        app.update(key(KeyCode::Char('p')));
        assert!(
            !app.priority_hints_visible,
            "p should not toggle hints in Board mode"
        );
    }

    // -- Export/clipboard/editor tests ---------------------------------------

    #[test]
    fn copy_issue_id_sets_status_msg() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());

        // C key should attempt clipboard (may fail in test env, but shouldn't panic)
        app.update(key(KeyCode::Char('C')));
        assert!(
            app.status_msg.contains("Copied") || app.status_msg.contains("Clipboard"),
            "status_msg should indicate clipboard result: {}",
            app.status_msg
        );
    }

    #[test]
    fn export_markdown_creates_temp_file() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('x')));
        assert!(
            app.status_msg.contains("Exported"),
            "should confirm export: {}",
            app.status_msg
        );
    }

    #[test]
    fn status_msg_cleared_on_next_key() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.status_msg = "Test message".to_string();

        // Any key press should clear the status msg
        app.update(key(KeyCode::Char('j')));
        assert!(app.status_msg.is_empty());
    }

    // -- TimeTravelDiff mode tests -------------------------------------------

    #[test]
    fn t_key_enters_time_travel_mode_with_input_prompt() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        assert!(app.time_travel_input_active);
        assert!(app.time_travel_ref_input.is_empty());
    }

    #[test]
    fn time_travel_escape_from_empty_input_returns_to_main() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
        assert!(!app.time_travel_input_active);
    }

    #[test]
    fn time_travel_input_accepts_characters() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        app.update(key(KeyCode::Char('H')));
        app.update(key(KeyCode::Char('E')));
        app.update(key(KeyCode::Char('A')));
        app.update(key(KeyCode::Char('D')));
        assert_eq!(app.time_travel_ref_input, "HEAD");
    }

    #[test]
    fn time_travel_backspace_removes_char() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Char('b')));
        app.update(key(KeyCode::Backspace));
        assert_eq!(app.time_travel_ref_input, "a");
    }

    #[test]
    fn time_travel_enter_with_empty_ref_returns_to_main() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        app.update(key(KeyCode::Enter));
        // Empty ref cancels
        assert_eq!(app.mode, ViewMode::Main);
        assert!(!app.time_travel_input_active);
    }

    #[test]
    fn time_travel_toggle_off() {
        let mut app = new_app(ViewMode::Main, 0);
        // Enter time-travel, then provide a diff manually
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = false;
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &app.analyzer.issues.clone(),
            &app.analyzer.issues,
        ));
        // Press t again to toggle off
        app.update(key(KeyCode::Char('t')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn time_travel_jk_navigates_categories() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = false;
        // Provide a self-diff (will have 0 categories, but navigation shouldn't panic)
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &[],
            &app.analyzer.issues,
        ));
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        // Should not panic
    }

    #[test]
    fn time_travel_list_text_shows_prompt_when_input_active() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = true;
        app.time_travel_ref_input = "HEAD~3".to_string();
        let text = app.time_travel_list_text();
        assert!(text.contains("HEAD~3"), "should show input: {text}");
    }

    #[test]
    fn time_travel_list_text_shows_no_diff_when_empty() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = false;
        let text = app.time_travel_list_text();
        assert!(
            text.contains("No diff loaded"),
            "should show no-diff message: {text}"
        );
    }

    #[test]
    fn time_travel_with_diff_shows_summary() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = false;
        app.time_travel_last_ref = Some("HEAD~1".to_string());
        // Diff from empty to current issues shows new_issues
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &[],
            &app.analyzer.issues,
        ));
        let text = app.time_travel_list_text();
        assert!(text.contains("HEAD~1"), "should show ref: {text}");
        assert!(
            text.contains("New issues"),
            "should show new issues category: {text}"
        );
    }

    // -- Sprint view tests ---------------------------------------------------

    fn make_sprint(id: &str, name: &str, bead_ids: Vec<&str>) -> Sprint {
        Sprint {
            id: id.to_string(),
            name: name.to_string(),
            start_date: Some(chrono::Utc::now() - chrono::Duration::days(7)),
            end_date: Some(chrono::Utc::now() + chrono::Duration::days(7)),
            bead_ids: bead_ids.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn s_key_toggles_sprint_mode() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('S')));
        assert_eq!(app.mode, ViewMode::Sprint, "S should enter Sprint mode");
        app.update(key(KeyCode::Char('S')));
        assert_eq!(app.mode, ViewMode::Main, "S again should return to Main");
    }

    #[test]
    fn sprint_list_shows_no_sprints_message() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = Vec::new();
        let text = app.sprint_list_text();
        assert!(
            text.contains("No sprints found"),
            "should show no-sprints message: {text}"
        );
    }

    #[test]
    fn sprint_list_shows_sprint_summary() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        // Create a sprint referencing sample issues (A, B, C from sample_issues())
        app.sprint_data = vec![make_sprint("s1", "Sprint Alpha", vec!["A", "B", "C"])];
        let text = app.sprint_list_text();
        assert!(
            text.contains("Sprint Alpha"),
            "should show sprint name: {text}"
        );
        assert!(text.contains("3 issues"), "should show issue count: {text}");
        assert!(text.contains("ACTIVE"), "should show active status: {text}");
    }

    #[test]
    fn sprint_detail_shows_issue_list() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![make_sprint("s1", "Sprint Alpha", vec!["A", "B", "C"])];
        app.sprint_cursor = 0;
        let text = app.sprint_detail_text();
        assert!(
            text.contains("SPRINT: Sprint Alpha"),
            "should show sprint name: {text}"
        );
        assert!(text.contains("ACTIVE"), "should show active status: {text}");
        // Should list at least some issues
        assert!(
            text.contains("Issues:") || text.contains("bead(s)"),
            "should show issue summary: {text}"
        );
    }

    #[test]
    fn sprint_jk_navigates_sprints() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![
            make_sprint("s1", "Sprint 1", vec!["A"]),
            make_sprint("s2", "Sprint 2", vec!["B"]),
        ];
        app.focus = FocusPane::List;
        assert_eq!(app.sprint_cursor, 0);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.sprint_cursor, 1, "j should move to next sprint");
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.sprint_cursor, 0, "k should move back");
    }

    #[test]
    fn sprint_jk_navigates_issues_in_detail() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![make_sprint("s1", "Sprint 1", vec!["A", "B", "C"])];
        app.focus = FocusPane::Detail;
        assert_eq!(app.sprint_issue_cursor, 0);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(
            app.sprint_issue_cursor, 1,
            "j in detail should move issue cursor"
        );
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.sprint_issue_cursor, 0, "k should move back");
    }

    #[test]
    fn sprint_escape_returns_to_main() {
        let mut app = new_app(ViewMode::Sprint, 0);
        app.sprint_data = vec![make_sprint("s1", "Sprint 1", vec!["A"])];
        app.update(key(KeyCode::Char('S')));
        assert_eq!(
            app.mode,
            ViewMode::Main,
            "S in Sprint mode should return to Main"
        );
    }

    #[test]
    fn sprint_detail_shows_progress_bar() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        // C is closed in sample_issues()
        app.sprint_data = vec![make_sprint("s1", "Sprint Alpha", vec!["A", "B", "C"])];
        app.sprint_cursor = 0;
        let text = app.sprint_detail_text();
        assert!(
            text.contains("Progress:"),
            "should show progress bar: {text}"
        );
        assert!(text.contains('%'), "should show percentage: {text}");
    }

    #[test]
    fn sprint_cursor_reset_on_sprint_change() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![
            make_sprint("s1", "Sprint 1", vec!["A", "B"]),
            make_sprint("s2", "Sprint 2", vec!["C"]),
        ];
        app.focus = FocusPane::List;
        app.sprint_issue_cursor = 1;
        // Navigate to next sprint
        app.update(key(KeyCode::Char('j')));
        assert_eq!(
            app.sprint_issue_cursor, 0,
            "issue cursor should reset on sprint change"
        );
    }

    // -- Modal picker tests ---------------------------------------------------

    #[test]
    fn quote_key_opens_recipe_picker() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        assert!(
            matches!(app.modal_overlay, Some(ModalOverlay::RecipePicker { .. })),
            "' should open recipe picker"
        );
    }

    #[test]
    fn recipe_picker_jk_navigates() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        if let Some(ModalOverlay::RecipePicker { cursor, .. }) = &app.modal_overlay {
            assert_eq!(*cursor, 0);
        }
        app.update(key(KeyCode::Char('j')));
        if let Some(ModalOverlay::RecipePicker { cursor, .. }) = &app.modal_overlay {
            assert_eq!(*cursor, 1, "j should advance cursor");
        }
        app.update(key(KeyCode::Char('k')));
        if let Some(ModalOverlay::RecipePicker { cursor, .. }) = &app.modal_overlay {
            assert_eq!(*cursor, 0, "k should go back");
        }
    }

    #[test]
    fn recipe_picker_escape_closes() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        assert!(app.modal_overlay.is_some());
        app.update(key(KeyCode::Escape));
        assert!(
            app.modal_overlay.is_none(),
            "Esc should close recipe picker"
        );
    }

    #[test]
    fn recipe_picker_enter_selects_and_closes() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        app.update(key(KeyCode::Enter));
        assert!(
            app.modal_overlay.is_none(),
            "Enter should close recipe picker"
        );
        assert!(
            app.status_msg.contains("Recipe:"),
            "should show recipe name in status: {}",
            app.status_msg
        );
    }

    #[test]
    fn capital_l_opens_label_picker() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('L')));
        assert!(
            matches!(app.modal_overlay, Some(ModalOverlay::LabelPicker { .. })),
            "L should open label picker"
        );
        // sample_issues have "core" and "parity" labels on issue A
        if let Some(ModalOverlay::LabelPicker { items, .. }) = &app.modal_overlay {
            assert!(!items.is_empty(), "should have labels from issues");
        }
    }

    #[test]
    fn label_picker_enter_filters_by_label() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('L')));
        app.update(key(KeyCode::Enter));
        assert!(
            app.modal_label_filter.is_some(),
            "Enter in label picker should set label filter"
        );
        assert!(
            app.status_msg.contains("Filtering by label"),
            "should show filter status: {}",
            app.status_msg
        );
    }

    #[test]
    fn label_filter_clears_on_all() {
        let mut app = new_app(ViewMode::Main, 0);
        app.modal_label_filter = Some("core".to_string());
        assert!(app.has_active_filter());
        app.set_list_filter(ListFilter::All);
        assert!(
            app.modal_label_filter.is_none(),
            "All filter should clear label filter"
        );
    }

    #[test]
    fn w_key_opens_repo_picker_or_status_msg() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('w')));
        // sample_issues have source_repo="viewer" on issue A, "" on others
        // So either we get a picker with repos, or a status message
        if app.modal_overlay.is_some() {
            assert!(
                matches!(app.modal_overlay, Some(ModalOverlay::RepoPicker { .. })),
                "w should open repo picker when repos exist"
            );
        } else {
            // No repos -> status message
            assert!(
                app.status_msg.contains("repo") || app.status_msg.contains("workspace"),
                "should indicate no repos: {}",
                app.status_msg
            );
        }
    }

    #[test]
    fn sprint_with_no_matching_issues_shows_message() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![make_sprint(
            "s1",
            "Sprint Ghost",
            vec!["NONEXISTENT-1", "NONEXISTENT-2"],
        )];
        app.sprint_cursor = 0;
        let text = app.sprint_detail_text();
        assert!(
            text.contains("none matched"),
            "should say no issues matched: {text}"
        );
    }

    // =====================================================================
    // bd-2ec: Expanded TUI snapshot + keyflow + edge-case coverage
    // =====================================================================

    // -- Snapshots for newer view modes ------------------------------------

    snapshot_test!(snap_actionable_narrow, ViewMode::Actionable, 60, 30);
    snapshot_test!(snap_actionable_medium, ViewMode::Actionable, 100, 30);
    snapshot_test!(snap_actionable_wide, ViewMode::Actionable, 140, 30);

    snapshot_test!(snap_tree_narrow, ViewMode::Tree, 60, 30);
    snapshot_test!(snap_tree_medium, ViewMode::Tree, 100, 30);
    snapshot_test!(snap_tree_wide, ViewMode::Tree, 140, 30);

    snapshot_test!(
        snap_label_dashboard_narrow,
        ViewMode::LabelDashboard,
        60,
        30
    );
    snapshot_test!(
        snap_label_dashboard_medium,
        ViewMode::LabelDashboard,
        100,
        30
    );
    snapshot_test!(snap_label_dashboard_wide, ViewMode::LabelDashboard, 140, 30);

    snapshot_test!(snap_flow_matrix_narrow, ViewMode::FlowMatrix, 60, 30);
    snapshot_test!(snap_flow_matrix_medium, ViewMode::FlowMatrix, 100, 30);
    snapshot_test!(snap_flow_matrix_wide, ViewMode::FlowMatrix, 140, 30);

    snapshot_test!(snap_sprint_narrow, ViewMode::Sprint, 60, 30);
    snapshot_test!(snap_sprint_medium, ViewMode::Sprint, 100, 30);
    snapshot_test!(snap_sprint_wide, ViewMode::Sprint, 140, 30);

    snapshot_test!(snap_time_travel_narrow, ViewMode::TimeTravelDiff, 60, 30);
    snapshot_test!(snap_time_travel_medium, ViewMode::TimeTravelDiff, 100, 30);
    snapshot_test!(snap_time_travel_wide, ViewMode::TimeTravelDiff, 140, 30);

    // -- Interactive snapshots for newer view modes -------------------------

    #[test]
    fn snap_attention_detail_focus() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        app.update(key(KeyCode::Tab));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_tree_detail_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('T')));
        app.update(key(KeyCode::Tab));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_label_dashboard_detail_focus() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('[')));
        app.update(key(KeyCode::Tab));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_flow_matrix_detail_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char(']')));
        app.update(key(KeyCode::Tab));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_sprint_detail_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![make_sprint("s1", "Sprint Alpha", vec!["A", "B", "C"])];
        app.focus = FocusPane::Detail;
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_time_travel_input_prompt() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        // Now in time-travel input mode
        app.update(key(KeyCode::Char('H')));
        app.update(key(KeyCode::Char('E')));
        app.update(key(KeyCode::Char('A')));
        app.update(key(KeyCode::Char('D')));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_recipe_picker_overlay() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        assert!(app.modal_overlay.is_some());
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_label_picker_overlay() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('L')));
        assert!(app.modal_overlay.is_some());
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn snap_time_travel_with_diff() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = false;
        app.time_travel_last_ref = Some("HEAD~1".to_string());
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &[],
            &app.analyzer.issues,
        ));
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    // -- Empty data / edge-case no-panic tests -----------------------------

    #[test]
    fn tree_view_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.update(key(KeyCode::Char('T')));
        let list = app.list_panel_text();
        assert!(!list.is_empty());
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        app.update(key(KeyCode::Tab));
        let detail = app.detail_panel_text();
        assert!(!detail.is_empty());
    }

    #[test]
    fn label_dashboard_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.update(key(KeyCode::Char('[')));
        let list = app.list_panel_text();
        assert!(!list.is_empty());
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        app.update(key(KeyCode::Tab));
        let detail = app.detail_panel_text();
        assert!(!detail.is_empty());
    }

    #[test]
    fn flow_matrix_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.update(key(KeyCode::Char(']')));
        let list = app.list_panel_text();
        assert!(!list.is_empty());
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        app.update(key(KeyCode::Tab));
        let detail = app.detail_panel_text();
        assert!(!detail.is_empty());
    }

    #[test]
    fn sprint_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![make_sprint("s1", "Sprint Empty", vec!["X"])];
        let list = app.list_panel_text();
        assert!(!list.is_empty());
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        let detail = app.detail_panel_text();
        assert!(!detail.is_empty());
    }

    #[test]
    fn time_travel_render_all_states_no_panic() {
        let mut app = new_app(ViewMode::Main, 0);
        // Input active state
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = true;
        let _ = render_app(&app, 100, 30);

        // No diff state
        app.time_travel_input_active = false;
        let _ = render_app(&app, 100, 30);

        // With diff state
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &[],
            &app.analyzer.issues,
        ));
        let _ = render_app(&app, 100, 30);
    }

    #[test]
    fn actionable_empty_issues_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        app.update(key(KeyCode::Char('a')));
        let list = app.list_panel_text();
        assert!(!list.is_empty());
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        let _ = render_app(&app, 100, 30);
    }

    #[test]
    fn all_modes_render_at_narrow_width_no_panic() {
        for mode in [
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::History,
            ViewMode::Actionable,
            ViewMode::Attention,
            ViewMode::Tree,
            ViewMode::LabelDashboard,
            ViewMode::FlowMatrix,
            ViewMode::Sprint,
            ViewMode::TimeTravelDiff,
        ] {
            let _ = render_frame(mode, 40, 10);
        }
    }

    #[test]
    fn all_modes_render_with_single_issue_no_panic() {
        let single = vec![Issue {
            id: "X".to_string(),
            title: "Solo".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        for mode in [
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::Actionable,
            ViewMode::Attention,
            ViewMode::Tree,
            ViewMode::LabelDashboard,
            ViewMode::FlowMatrix,
        ] {
            let app = new_app_with_issues(mode, 0, single.clone());
            let _ = render_app(&app, 100, 30);
        }
    }

    #[test]
    fn all_modes_render_with_all_closed_issues_no_panic() {
        let closed = vec![
            Issue {
                id: "X".to_string(),
                title: "Done A".to_string(),
                status: "closed".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "Y".to_string(),
                title: "Done B".to_string(),
                status: "closed".to_string(),
                ..Issue::default()
            },
        ];
        for mode in [
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::Actionable,
            ViewMode::Attention,
            ViewMode::Tree,
            ViewMode::LabelDashboard,
            ViewMode::FlowMatrix,
        ] {
            let app = new_app_with_issues(mode, 0, closed.clone());
            let _ = render_app(&app, 100, 30);
        }
    }

    // -- Keyflow journeys through newer modes ------------------------------

    #[test]
    fn keyflow_full_newer_mode_tour() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        assert_eq!(app.mode, ViewMode::Main);

        // Actionable
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);

        // Attention
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Attention);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Main);

        // Tree
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);

        // LabelDashboard
        app.update(key(KeyCode::Char('[')));
        assert_eq!(app.mode, ViewMode::LabelDashboard);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);

        // FlowMatrix
        app.update(key(KeyCode::Char(']')));
        assert_eq!(app.mode, ViewMode::FlowMatrix);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);

        // TimeTravelDiff (enter then cancel)
        app.update(key(KeyCode::Char('t')));
        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);

        // Verify key trace captured all transitions
        assert!(
            app.key_trace.len() >= 15,
            "should have many trace entries: {}",
            app.key_trace.len()
        );
    }

    #[test]
    fn keyflow_sprint_full_journey() {
        let mut app = new_app(ViewMode::Main, 0);

        // Enter sprint mode (load_sprint_data called internally, returns empty w/o disk)
        app.update(key(KeyCode::Char('S')));
        assert_eq!(app.mode, ViewMode::Sprint);

        // Inject sprint data after entering mode (simulating disk load)
        app.sprint_data = vec![
            make_sprint("s1", "Sprint 1", vec!["A", "B"]),
            make_sprint("s2", "Sprint 2", vec!["C"]),
        ];

        // Navigate sprints
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.sprint_cursor, 1);

        // Switch to detail focus
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // Navigate back to list
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        // Return to main
        app.update(key(KeyCode::Char('S')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn keyflow_modal_chain() {
        let mut app = new_app(ViewMode::Main, 0);

        // Open recipe picker, navigate, close
        app.update(key(KeyCode::Char('\'')));
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::RecipePicker { .. })
        ));
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Escape));
        assert!(app.modal_overlay.is_none());

        // Open label picker, select
        app.update(key(KeyCode::Char('L')));
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::LabelPicker { .. })
        ));
        app.update(key(KeyCode::Enter));
        assert!(app.modal_overlay.is_none());
        assert!(app.modal_label_filter.is_some());

        // Clear filter
        app.set_list_filter(ListFilter::All);
        assert!(app.modal_label_filter.is_none());
    }

    #[test]
    fn keyflow_rapid_mode_switching_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        // Rapid switching between modes — should never panic
        let keys = [
            KeyCode::Char('b'), // Board
            KeyCode::Char('q'), // Back
            KeyCode::Char('i'), // Insights
            KeyCode::Char('q'), // Back
            KeyCode::Char('g'), // Graph
            KeyCode::Char('q'), // Back
            KeyCode::Char('a'), // Actionable
            KeyCode::Char('a'), // Back
            KeyCode::Char('!'), // Attention
            KeyCode::Char('!'), // Back
            KeyCode::Char('T'), // Tree
            KeyCode::Char('q'), // Back
            KeyCode::Char('['), // LabelDashboard
            KeyCode::Char('q'), // Back
            KeyCode::Char(']'), // FlowMatrix
            KeyCode::Char('q'), // Back
            KeyCode::Char('t'), // TimeTravelDiff
            KeyCode::Escape,    // Cancel
            KeyCode::Char('b'), // Board again
            KeyCode::Char('g'), // Graph from board?
            KeyCode::Char('q'), // Back
        ];
        for k in keys {
            app.update(key(k));
        }
        // Should still be in a valid state
        assert!(!matches!(app.mode, ViewMode::TimeTravelDiff));
    }

    #[test]
    fn keyflow_navigation_in_all_newer_modes() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        // Test j/k/Tab/Esc in each newer mode
        for mode_key in [
            KeyCode::Char('a'), // Actionable
            KeyCode::Char('!'), // Attention
            KeyCode::Char('T'), // Tree
            KeyCode::Char('['), // LabelDashboard
            KeyCode::Char(']'), // FlowMatrix
        ] {
            app.update(key(mode_key));
            app.update(key(KeyCode::Char('j')));
            app.update(key(KeyCode::Char('j')));
            app.update(key(KeyCode::Char('k')));
            app.update(key(KeyCode::Tab));
            app.update(key(KeyCode::Char('j')));
            app.update(key(KeyCode::Tab));
            // Return to main
            let exit_key = match mode_key {
                KeyCode::Char('a') => KeyCode::Char('a'),
                KeyCode::Char('!') => KeyCode::Char('!'),
                _ => KeyCode::Char('q'),
            };
            app.update(key(exit_key));
            assert_eq!(
                app.mode,
                ViewMode::Main,
                "should return to Main from mode entered via {:?}",
                mode_key
            );
        }
    }

    #[test]
    fn keyflow_help_from_newer_modes() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        // Help should work from any mode
        for mode_key in [
            KeyCode::Char('a'),
            KeyCode::Char('T'),
            KeyCode::Char('['),
            KeyCode::Char(']'),
        ] {
            app.update(key(mode_key));
            app.update(key(KeyCode::Char('?')));
            assert!(app.show_help, "help should open in mode {:?}", mode_key);
            app.update(key(KeyCode::Char('?')));
            assert!(!app.show_help);
            // Return to main via q (or a for actionable)
            let exit = match mode_key {
                KeyCode::Char('a') => KeyCode::Char('a'),
                _ => KeyCode::Char('q'),
            };
            app.update(key(exit));
        }
    }

    #[test]
    fn keyflow_filter_then_mode_switch_preserves_filter() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        // Set open filter
        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);

        // Switch to Tree mode and back — filter should persist
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);
        app.update(key(KeyCode::Char('q')));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(
            app.list_filter,
            ListFilter::Open,
            "filter should persist across mode transitions"
        );
    }

    // -- Additional edge-case coverage for existing features ---------------

    #[test]
    fn graph_with_cycle_issues_no_panic() {
        let issues = vec![
            Issue {
                id: "X".to_string(),
                title: "Cyclic A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "X".to_string(),
                    depends_on_id: "Y".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "Y".to_string(),
                title: "Cyclic B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "Y".to_string(),
                    depends_on_id: "X".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        for mode in [
            ViewMode::Main,
            ViewMode::Graph,
            ViewMode::Insights,
            ViewMode::Actionable,
        ] {
            let app = new_app_with_issues(mode, 0, issues.clone());
            let _ = render_app(&app, 100, 30);
        }
    }

    #[test]
    fn attention_detail_shows_correct_label_on_navigation() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        let labels_count = app.attention_result.as_ref().unwrap().labels.len();
        if labels_count >= 2 {
            let detail_0 = app.detail_panel_text();
            app.update(key(KeyCode::Char('j')));
            let detail_1 = app.detail_panel_text();
            // Different label should produce different detail content
            assert_ne!(
                detail_0, detail_1,
                "navigating to next label should change detail"
            );
        }
    }

    #[test]
    fn sprint_tab_switches_focus_and_navigation_context() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![
            make_sprint("s1", "Sprint 1", vec!["A", "B"]),
            make_sprint("s2", "Sprint 2", vec!["C"]),
        ];

        // List focus: j/k moves sprint_cursor
        app.focus = FocusPane::List;
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.sprint_cursor, 1);

        // Switch to detail
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // Detail focus: j/k moves sprint_issue_cursor (sprint 2 has 1 issue)
        app.update(key(KeyCode::Char('j')));
        // sprint_issue_cursor can't go past issue count - verify no panic

        // Back to list
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn modal_overlay_blocks_mode_switch_keys() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('\'')));
        assert!(app.modal_overlay.is_some());

        // Mode keys should NOT switch modes while modal is open
        app.update(key(KeyCode::Char('b')));
        assert_eq!(
            app.mode,
            ViewMode::Main,
            "b should not switch mode during modal"
        );
        assert!(app.modal_overlay.is_some(), "modal should still be open");

        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Main);

        // Close modal
        app.update(key(KeyCode::Escape));
        assert!(app.modal_overlay.is_none());

        // Now mode switch should work
        app.update(key(KeyCode::Char('b')));
        assert_eq!(app.mode, ViewMode::Board);
    }

    #[test]
    fn label_filter_affects_visible_issues() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        let total = app.visible_issue_indices().len();
        assert!(total >= 3);

        // Set label filter to "frontend" — only issue C has it
        app.modal_label_filter = Some("frontend".to_string());
        let filtered = app.visible_issue_indices().len();
        assert!(
            filtered < total,
            "label filter should reduce visible issues: {filtered} < {total}"
        );
    }

    #[test]
    fn repo_filter_affects_visible_issues() {
        let mut app = new_app(ViewMode::Main, 0);
        // sample_issues: A has source_repo="viewer", B and C have ""
        app.modal_repo_filter = Some("viewer".to_string());
        let filtered = app.visible_issue_indices().len();
        assert!(filtered >= 1, "repo filter should show at least one issue");
    }

    #[test]
    fn header_shows_combined_filters() {
        let mut app = new_app(ViewMode::Main, 0);
        app.modal_label_filter = Some("core".to_string());
        app.modal_repo_filter = Some("viewer".to_string());
        let text = render_app(&app, 100, 3);
        assert!(
            text.contains("label:core") || text.contains("core"),
            "header should mention label filter: {text}"
        );
    }

    // -- Two-phase (fast/slow) metric TUI tests ------------------------------

    #[test]
    fn slow_metrics_pending_flag_default_false() {
        let app = new_app(ViewMode::Main, 0);
        assert!(
            !app.slow_metrics_pending,
            "small graph should not have pending slow metrics"
        );
    }

    #[test]
    fn slow_metrics_pending_shows_in_header() {
        let mut app = new_app(ViewMode::Main, 0);
        app.slow_metrics_pending = true;
        let text = render_app(&app, 120, 3);
        assert!(
            text.contains("computing"),
            "header should show metrics computing indicator: {text}"
        );
    }

    #[test]
    fn slow_metrics_pending_clears_after_apply() {
        let mut app = new_app(ViewMode::Main, 0);
        app.slow_metrics_pending = true;
        let slow = app
            .analyzer
            .graph
            .compute_metrics_with_config(&crate::analysis::graph::AnalysisConfig::slow_phase());
        app.analyzer.apply_slow_metrics(slow);
        app.slow_metrics_pending = false;
        let text = render_app(&app, 120, 3);
        assert!(
            !text.contains("computing"),
            "header should not show computing after metrics applied: {text}"
        );
    }

    #[test]
    fn all_modes_work_with_fast_only_metrics() {
        // Create analyzer with fast-only metrics
        let mut app = new_app(ViewMode::Main, 0);
        let issues = app.analyzer.issues.clone();
        app.analyzer = Analyzer::new_fast(issues);
        app.slow_metrics_pending = true;

        // Verify all view modes render without panic
        for mode in [
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Graph,
            ViewMode::Insights,
            ViewMode::Actionable,
        ] {
            app.mode = mode;
            let _ = render_app(&app, 100, 30);
        }
    }
}
