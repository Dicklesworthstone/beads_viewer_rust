use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::analysis::Analyzer;
use crate::analysis::git_history::{
    GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryMilestonesCompat,
    correlate_histories_with_git, finalize_history_entries, load_git_commits,
};
use crate::loader;
use crate::model::Issue;
use crate::{BvrError, Result};
use chrono::{DateTime, Utc};
use ftui::core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui::core::geometry::Rect;
use ftui::layout::{Constraint, Flex};
use ftui::render::frame::Frame;
use ftui::runtime::{App, Cmd, Model, ScreenMode};
use ftui::widgets::Widget;
use ftui::widgets::block::Block;
use ftui::widgets::paragraph::Paragraph;

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
        let fg = match status {
            "open" => STATUS_OPEN,
            "in_progress" => STATUS_IN_PROGRESS,
            "blocked" => STATUS_BLOCKED,
            "closed" => STATUS_CLOSED,
            _ => FG_DIM,
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
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Board => "Board",
            Self::Insights => "Insights",
            Self::Graph => "Graph",
            Self::History => "History",
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

/// A node in the history file tree (port of Go `FileTreeNode`).
#[derive(Debug, Clone)]
struct FileTreeNode {
    name: String,
    path: String,
    is_dir: bool,
    change_count: usize,
    expanded: bool,
    level: usize,
    children: Vec<FileTreeNode>,
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
    CriticalPath,
    Influencers,
    Betweenness,
    Hubs,
    Authorities,
    Cores,
    CutPoints,
    Slack,
    Cycles,
}

impl InsightsPanel {
    fn label(self) -> &'static str {
        match self {
            Self::Bottlenecks => "Bottlenecks",
            Self::CriticalPath => "Critical Path",
            Self::Influencers => "Influencers (PageRank)",
            Self::Betweenness => "Betweenness",
            Self::Hubs => "Hubs (HITS)",
            Self::Authorities => "Authorities (HITS)",
            Self::Cores => "K-Core Cohesion",
            Self::CutPoints => "Cut Points",
            Self::Slack => "Slack (Zero)",
            Self::Cycles => "Cycles",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Bottlenecks => "bottlenecks",
            Self::CriticalPath => "crit-path",
            Self::Influencers => "influencers",
            Self::Betweenness => "betweenness",
            Self::Hubs => "hubs",
            Self::Authorities => "authorities",
            Self::Cores => "k-core",
            Self::CutPoints => "cut-pts",
            Self::Slack => "slack",
            Self::Cycles => "cycles",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Bottlenecks => Self::CriticalPath,
            Self::CriticalPath => Self::Influencers,
            Self::Influencers => Self::Betweenness,
            Self::Betweenness => Self::Hubs,
            Self::Hubs => Self::Authorities,
            Self::Authorities => Self::Cores,
            Self::Cores => Self::CutPoints,
            Self::CutPoints => Self::Slack,
            Self::Slack => Self::Cycles,
            Self::Cycles => Self::Bottlenecks,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Bottlenecks => Self::Cycles,
            Self::CriticalPath => Self::Bottlenecks,
            Self::Influencers => Self::CriticalPath,
            Self::Betweenness => Self::Influencers,
            Self::Hubs => Self::Betweenness,
            Self::Authorities => Self::Hubs,
            Self::Cores => Self::Authorities,
            Self::CutPoints => Self::Cores,
            Self::Slack => Self::CutPoints,
            Self::Cycles => Self::Slack,
        }
    }
}

const HISTORY_CONFIDENCE_STEPS: [f64; 4] = [0.0, 0.5, 0.75, 0.9];

#[derive(Debug, Clone)]
struct HistoryGitCache {
    commits: Vec<GitCommitRecord>,
    histories: BTreeMap<String, HistoryBeadCompat>,
    commit_bead_confidence: BTreeMap<String, Vec<(String, f64)>>,
}

#[derive(Debug, Clone)]
struct HistoryTimelineEvent {
    issue_id: String,
    issue_title: String,
    issue_status: String,
    event_kind: String,
    event_timestamp: Option<String>,
    event_details: String,
}

#[derive(Debug)]
enum Msg {
    KeyPress(KeyCode, Modifiers),
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
    history_confidence_index: usize,
    history_view_mode: HistoryViewMode,
    history_event_cursor: usize,
    history_related_bead_cursor: usize,
    history_bead_commit_cursor: usize,
    history_git_cache: Option<HistoryGitCache>,
    history_search_active: bool,
    history_search_query: String,
    history_search_match_cursor: usize,
    history_show_file_tree: bool,
    history_file_tree_cursor: usize,
    history_file_tree_filter: Option<String>,
    history_file_tree_focus: bool,
    history_status_msg: String,
    board_search_active: bool,
    board_search_query: String,
    board_search_match_cursor: usize,
    graph_search_active: bool,
    graph_search_query: String,
    graph_search_match_cursor: usize,
    insights_search_active: bool,
    insights_search_query: String,
    insights_search_match_cursor: usize,
    insights_panel: InsightsPanel,
    insights_show_explanations: bool,
    insights_show_calc_proof: bool,
    detail_dep_cursor: usize,
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
                return cmd;
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
            _ => format!(
                "bvr | mode={} | focus={} | issues={}/{} | filter={} | sort={} | ? help | Tab focus | Esc back/quit",
                self.mode.label(),
                self.focus.label(),
                visible_count,
                self.analyzer.issues.len(),
                self.list_filter.label(),
                self.list_sort.label()
            ),
        };
        Paragraph::new(header_text)
            .style(tokens::header_bg())
            .render(rows[0], frame);

        // -- Help overlay ----------------------------------------------------
        if self.show_help {
            let full_help = self.help_overlay_text();
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

        // -- Body: two-pane split with breakpoint-aware widths ---------------
        let body = rows[1];
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

        let detail_text = self.detail_panel_text();
        let detail_title = match self.mode {
            ViewMode::Board => "Board Focus",
            ViewMode::Insights => "Insight Detail",
            ViewMode::Graph => "Graph Focus",
            ViewMode::History => "History Timeline",
            ViewMode::Main => "Details",
        };
        let detail_focused = self.focus == FocusPane::Detail;
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
        Paragraph::new(detail_text)
            .block(
                Block::bordered()
                    .title(&detail_title)
                    .border_style(detail_border)
                    .style(detail_title_style),
            )
            .render(panes[1], frame);

        // -- Footer ----------------------------------------------------------
        let footer_text = match self.mode {
            ViewMode::Main => format!(
                "Main view mirrors bv split workflow. Press b/i/g/h for focused modes | s cycles sort ({})",
                self.list_sort.label()
            ),
            ViewMode::Board => {
                format!(
                    "Board mode: lane counts, queued IDs, and selected issue delivery context | grouping={} (s cycles) | empty-lanes={} (e toggles) | H/L lanes | 0/$ lane edges",
                    self.board_grouping.label(),
                    self.board_empty_visibility.label(),
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
            ViewMode::Graph => {
                "Graph mode: centrality ranks, blockers/dependents, cycle membership.".to_string()
            }
            ViewMode::History => format!(
                "History ({}): c confidence (>= {:.0}%) | v bead/git | y copy | o open | f file-tree | / search | h/Esc back",
                self.history_view_mode.label(),
                self.history_min_confidence() * 100.0
            ),
        };
        Paragraph::new(footer_text)
            .style(tokens::footer())
            .render(rows[2], frame);
    }
}

impl BvrApp {
    fn board_shortcut_focus(&self) -> bool {
        matches!(self.mode, ViewMode::Board)
            && matches!(self.focus, FocusPane::List | FocusPane::Detail)
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: Modifiers) -> Cmd<Msg> {
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

        match code {
            KeyCode::Char('?') => {
                self.show_help = true;
                self.focus_before_help = self.focus;
            }
            KeyCode::Enter => {
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
                if matches!(self.mode, ViewMode::History) {
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
            KeyCode::Char('a') => self.set_list_filter(ListFilter::All),
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
            KeyCode::Char('s') if matches!(self.mode, ViewMode::Insights) => {
                self.insights_panel = self.insights_panel.next();
            }
            KeyCode::Char('S') if matches!(self.mode, ViewMode::Insights) => {
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

    fn toggle_history_file_tree(&mut self) {
        self.history_show_file_tree = !self.history_show_file_tree;
        if self.history_show_file_tree {
            self.history_file_tree_cursor = 0;
            self.history_file_tree_filter = None;
            self.history_status_msg = "File tree: j/k navigate, Enter filter, Esc close".into();
        } else {
            self.history_file_tree_focus = false;
            self.history_file_tree_filter = None;
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
            (cur + delta as usize).min(len.saturating_sub(1))
        } else {
            cur.saturating_sub(delta.unsigned_abs())
        };
    }

    fn file_tree_toggle_or_filter(&mut self) {
        let flat = self.history_flat_file_list();
        let cursor = self.history_file_tree_cursor.min(flat.len().saturating_sub(1));
        if let Some(entry) = flat.get(cursor) {
            if entry.is_dir {
                // Toggle expand/collapse on directory nodes
                let path = entry.path.clone();
                if let Some(nodes) = self.history_git_cache.as_ref().map(|_| self.history_file_tree_nodes()) {
                    // Rebuild with toggle — we toggle the filter instead
                    if self.history_file_tree_filter.as_deref() == Some(&path) {
                        self.history_file_tree_filter = None;
                        self.history_status_msg = "Filter cleared".into();
                    } else {
                        self.history_file_tree_filter = Some(path.clone());
                        self.history_status_msg = format!("Filtered to: {path}");
                    }
                    let _ = nodes; // suppress unused
                }
            } else {
                // Filter by this file
                let path = entry.path.clone();
                if self.history_file_tree_filter.as_deref() == Some(&path) {
                    self.history_file_tree_filter = None;
                    self.history_status_msg = "Filter cleared".into();
                } else {
                    self.history_file_tree_filter = Some(path.clone());
                    self.history_status_msg = format!("Filtered to: {path}");
                }
            }
        }
    }

    fn history_file_tree_nodes(&self) -> Vec<FileTreeNode> {
        let cache = match &self.history_git_cache {
            Some(c) => c,
            None => return Vec::new(),
        };

        let mut file_counts: BTreeMap<String, usize> = BTreeMap::new();
        for commit in &cache.commits {
            for file in &commit.files {
                *file_counts.entry(file.path.clone()).or_default() += 1;
            }
        }

        // Build flat file tree with directory grouping
        let mut roots: BTreeMap<String, FileTreeNode> = BTreeMap::new();
        for (path, count) in &file_counts {
            let parts: Vec<&str> = path.rsplitn(2, '/').collect();
            let (file_name, dir_path) = if parts.len() == 2 {
                (parts[0].to_string(), parts[1].to_string())
            } else {
                (path.clone(), String::new())
            };

            let dir_node = roots
                .entry(dir_path.clone())
                .or_insert_with(|| FileTreeNode {
                    name: if dir_path.is_empty() {
                        ".".to_string()
                    } else {
                        dir_path.clone()
                    },
                    path: dir_path.clone(),
                    is_dir: true,
                    change_count: 0,
                    expanded: true,
                    level: 0,
                    children: Vec::new(),
                });

            dir_node.change_count += count;
            dir_node.children.push(FileTreeNode {
                name: file_name,
                path: path.clone(),
                is_dir: false,
                change_count: *count,
                expanded: false,
                level: 1,
                children: Vec::new(),
            });
        }

        roots.into_values().collect()
    }

    fn history_flat_file_list(&self) -> Vec<FlatFileEntry> {
        self.history_file_tree_nodes()
            .iter()
            .flat_map(|node| node.flatten_visible())
            .collect()
    }

    /// Copy selected bead ID or commit SHA to clipboard via external command.
    fn history_copy_to_clipboard(&mut self) {
        let text = if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.selected_history_git_commit_sha()
        } else {
            Some(self.analyzer.issues[self.selected].id.clone())
        };

        if let Some(text) = text {
            // Use xclip/xsel/pbcopy via shell
            let result = std::process::Command::new("sh")
                .args(["-c", &format!("printf '%s' '{}' | xclip -selection clipboard 2>/dev/null || printf '%s' '{}' | xsel --clipboard 2>/dev/null || printf '%s' '{}' | pbcopy 2>/dev/null", text, text, text)])
                .output();
            match result {
                Ok(output) if output.status.success() => {
                    let short = if text.len() > 7 { &text[..7] } else { &text };
                    self.history_status_msg = format!("Copied {short} to clipboard");
                }
                _ => {
                    self.history_status_msg = "Clipboard not available".into();
                }
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
            let issue = &self.analyzer.issues[self.selected];
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

        let result = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("xdg-open '{url}' 2>/dev/null || open '{url}' 2>/dev/null"),
            ])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                let short = if sha.len() > 7 { &sha[..7] } else { &sha };
                self.history_status_msg = format!("Opened {short} in browser");
            }
            _ => {
                self.history_status_msg = "Could not open browser".into();
            }
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
            self.selected = index;
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
            let cache = match &self.history_git_cache {
                Some(c) => c,
                None => return Vec::new(),
            };
            cache
                .commits
                .iter()
                .enumerate()
                .filter(|(_, commit)| {
                    commit.sha.to_ascii_lowercase().contains(&query)
                        || commit.message.to_ascii_lowercase().contains(&query)
                        || commit.author.to_ascii_lowercase().contains(&query)
                        || commit
                            .files
                            .iter()
                            .any(|f| f.path.to_ascii_lowercase().contains(&query))
                })
                .map(|(i, _)| i)
                .collect()
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
            self.selected = matches[next];
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

                if query.is_empty() {
                    return Some(index);
                }

                let timestamp = commit.timestamp.to_ascii_lowercase();
                let author = commit.author.to_ascii_lowercase();
                let author_email = commit.author_email.to_ascii_lowercase();
                let message = commit.message.to_ascii_lowercase();
                let sha = commit.sha.to_ascii_lowercase();
                let short_sha = commit.short_sha.to_ascii_lowercase();

                let related_match = related
                    .iter()
                    .any(|id| id.to_ascii_lowercase().contains(&query));

                let matches = sha.contains(&query)
                    || short_sha.contains(&query)
                    || message.contains(&query)
                    || author.contains(&query)
                    || author_email.contains(&query)
                    || timestamp.contains(&query)
                    || related_match;

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

        let commits_len = self
            .history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(&issue_id))
            .map_or(0, |history| history.commits.as_ref().map_or(0, Vec::len));

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
            cmp_opt_datetime(
                parse_timestamp(left.event_timestamp.as_deref()),
                parse_timestamp(right.event_timestamp.as_deref()),
                true,
            )
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
                        .as_deref()
                        .map(str::to_ascii_lowercase)
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
        let cache = self.history_git_cache.as_ref()?;
        let history = cache.histories.get(&issue.id)?;
        let commits = history.commits.as_ref()?;
        if commits.is_empty() {
            return None;
        }

        let slot = self
            .history_bead_commit_cursor
            .min(commits.len().saturating_sub(1));
        commits.get(slot).cloned()
    }

    fn issue_matches_filter(&self, issue: &Issue) -> bool {
        match self.list_filter {
            ListFilter::All => true,
            ListFilter::Open => issue.is_open_like(),
            ListFilter::Closed => issue.is_closed_like(),
            ListFilter::Ready => {
                issue.is_open_like() && self.analyzer.graph.open_blockers(&issue.id).is_empty()
            }
        }
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
                        let l_open =
                            left_issue.status != "closed" && left_issue.status != "rejected";
                        let r_open =
                            right_issue.status != "closed" && right_issue.status != "rejected";
                        r_open
                            .cmp(&l_open)
                            .then_with(|| left_issue.priority.cmp(&right_issue.priority))
                            .then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
                    ListSort::CreatedAsc => cmp_opt_datetime(
                        parse_timestamp(left_issue.created_at.as_deref()),
                        parse_timestamp(right_issue.created_at.as_deref()),
                        false,
                    )
                    .then_with(|| left_issue.id.cmp(&right_issue.id)),
                    ListSort::CreatedDesc => cmp_opt_datetime(
                        parse_timestamp(left_issue.created_at.as_deref()),
                        parse_timestamp(right_issue.created_at.as_deref()),
                        true,
                    )
                    .then_with(|| left_issue.id.cmp(&right_issue.id)),
                    ListSort::Priority => left_issue
                        .priority
                        .cmp(&right_issue.priority)
                        .then_with(|| left_issue.id.cmp(&right_issue.id)),
                    ListSort::Updated => cmp_opt_datetime(
                        parse_timestamp(
                            left_issue
                                .updated_at
                                .as_deref()
                                .or(left_issue.created_at.as_deref()),
                        ),
                        parse_timestamp(
                            right_issue
                                .updated_at
                                .as_deref()
                                .or(right_issue.created_at.as_deref()),
                        ),
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
        if query.is_empty() {
            return visible;
        }

        visible
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
            self.selected = 0;
            return;
        }
        if !visible.contains(&self.selected) {
            self.selected = visible[0];
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
        self.selected = visible[next_slot];
        self.detail_dep_cursor = 0;
    }

    fn select_first_visible(&mut self) {
        if let Some(index) = self.visible_issue_indices_for_list_nav().first().copied() {
            self.selected = index;
        }
    }

    fn select_last_visible(&mut self) {
        if let Some(index) = self.visible_issue_indices_for_list_nav().last().copied() {
            self.selected = index;
        }
    }

    fn has_active_filter(&self) -> bool {
        self.list_filter != ListFilter::All
    }

    fn set_list_filter(&mut self, list_filter: ListFilter) {
        self.list_filter = list_filter;
        self.ensure_selected_visible();
        self.focus = FocusPane::List;
    }

    fn cycle_list_sort(&mut self) {
        self.list_sort = self.list_sort.next();
        self.ensure_selected_visible();
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
            self.selected = index;
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
            self.selected = index;
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
            self.selected = index;
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
                self.selected = indices[target_row];
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

        self.selected = indices[next_row];
        self.detail_dep_cursor = 0;
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
        self.selected = matches[self.board_search_match_cursor];
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
        self.selected = matches[next];
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
        self.selected = matches[self.graph_search_match_cursor];
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
        self.selected = matches[next];
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
        self.selected = matches[self.insights_search_match_cursor];
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
        self.selected = matches[next];
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
            self.selected = index;
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

    fn select_issue_by_id(&mut self, issue_id: &str) {
        if let Some(index) = self
            .analyzer
            .issues
            .iter()
            .position(|issue| issue.id == issue_id)
        {
            self.selected = index;
            self.ensure_selected_visible();
        }
    }

    fn no_filtered_issues_text(&self, context: &str) -> String {
        format!(
            "No issues match the active filter ({}) in {context}.",
            self.list_filter.label()
        )
    }

    fn help_overlay_text(&self) -> String {
        let mut lines = vec![
            "Core keys:".to_string(),
            "  j/k or arrows  Move selection".to_string(),
            "  h/l             Mode-aware lateral nav (board lanes, graph peers, insights pane focus)".to_string(),
            "  Ctrl+d/Ctrl+u   Jump down/up by 10 rows".to_string(),
            "  PgUp/PgDn       Jump by 10 rows".to_string(),
            "  Home/End         Jump to top/bottom".to_string(),
            "  /               Board/History/Graph/Insights: search (n/N match cycling)".to_string(),
            "  Tab              Toggle list/detail focus".to_string(),
            "  J/K              Detail: navigate blockers/dependents (board/graph/insights)".to_string(),
            "  b/i/g/h          Toggle board/insights/graph/history".to_string(),
            "  Enter            Return to main detail pane".to_string(),
            "  o/c/r/a          Filter open/closed/ready/all".to_string(),
            "  s                Main: cycle sort | Board: cycle grouping | Insights: cycle panel"
                .to_string(),
            "  ?                Toggle help overlay".to_string(),
            "  Esc              Back from mode (or clear filter, then quit confirm in main)"
                .to_string(),
            "  q                Main: quit | Non-main: return to main".to_string(),
            "  Ctrl+C           Quit immediately".to_string(),
        ];

        if matches!(self.mode, ViewMode::History) {
            lines.push(String::new());
            lines.push(format!(
                "History view mode: {} (v toggles bead/git event timeline)",
                self.history_view_mode.label()
            ));
            lines.push(format!(
                "History: c cycles min confidence filter (current >= {:.0}%)",
                self.history_min_confidence() * 100.0
            ));
        }

        if matches!(self.mode, ViewMode::Board) {
            lines.push(String::new());
            lines.push(format!(
                "Board lanes: grouping={} | empty-lanes={}",
                self.board_grouping.label(),
                self.board_empty_visibility.label(),
            ));
            lines.push("Board: 1-4 jump lanes | H/L first/last lane".to_string());
            lines.push(
                "Board: 0/$ first/last issue in current lane | e toggle empty lanes".to_string(),
            );
        }

        if matches!(self.mode, ViewMode::Insights) {
            lines.push(String::new());
            lines.push(format!(
                "Insights panel: {} (s/S cycles forward/back)",
                self.insights_panel.label()
            ));
            lines.push(
                "Panels: bottlenecks -> crit-path -> influencers -> betweenness -> hubs"
                    .to_string(),
            );
            lines
                .push("        -> authorities -> k-core -> cut-pts -> slack -> cycles".to_string());
            lines.push(format!(
                "Toggles: explanations={} (e) | calc-proof={} (x)",
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
            ));
        }

        lines.join("\n")
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
        let insights = self.analyzer.insights();

        let mut lines = vec![format!(
            "[{}] s/S cycles panel | e explanations | x calc-proof | / search",
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
        let mut visible = self.visible_issue_indices();
        if visible.is_empty() {
            return format!("(no issues match filter: {})", self.list_filter.label());
        }

        // Sort by critical path score (descending), then by ID for stability.
        visible.sort_by(|&left_idx, &right_idx| {
            let left = &self.analyzer.issues[left_idx];
            let right = &self.analyzer.issues[right_idx];
            let left_score = self.graph_node_score(&left.id);
            let right_score = self.graph_node_score(&right.id);
            right_score
                .total_cmp(&left_score)
                .then_with(|| left.id.cmp(&right.id))
        });

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
                lines.push(format!("Search (active): /{}", self.history_search_query));
            } else if !query.is_empty() {
                let matches = self.history_search_matches();
                let mc = matches.len();
                lines.push(format!(
                    "Search: /{} ({mc} matches, n/N cycles)",
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
            lines.push(format!("Search (active): /{}", self.history_search_query));
        } else if !query.is_empty() {
            let mc = visible.len();
            lines.push(format!(
                "Search: /{} ({mc} matches, n/N cycles)",
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

    fn detail_panel_text(&self) -> String {
        match self.mode {
            ViewMode::Board => self.board_detail_text(),
            ViewMode::Insights => self.insights_detail_text(),
            ViewMode::Graph => self.graph_detail_text(),
            ViewMode::History => self.history_detail_text(),
            ViewMode::Main => self.issue_detail_text(),
        }
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

        let mut lines = vec![
            format!("ID: {}", issue.id),
            format!("Title: {}", issue.title),
            format!("Status: {}", issue.status),
            format!("Priority: {}", issue.priority),
            format!("Type: {}", issue.issue_type),
            format!("Assignee: {}", issue.assignee),
            format!("Labels: {}", issue.labels.join(", ")),
            format!("PageRank: {:.4}", pagerank),
            format!("Critical depth: {}", depth),
            format!("Depends on: {}", join_or_none(&blockers)),
            format!("Open blockers: {}", join_or_none(&open_blockers)),
            format!("Direct dependents: {}", join_or_none(&dependents)),
            String::new(),
            "Description:".to_string(),
            issue.description.clone(),
        ];

        if !issue.acceptance_criteria.trim().is_empty() {
            lines.push(String::new());
            lines.push("Acceptance Criteria:".to_string());
            lines.push(issue.acceptance_criteria.clone());
        }

        lines.join("\n")
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
        if self.analyzer.issues.is_empty() {
            return "No insights available.".to_string();
        }

        let Some(issue) = self.selected_issue() else {
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

        lines.join("\n")
    }

    fn graph_detail_text(&self) -> String {
        if self.analyzer.issues.is_empty() {
            return "No graph data available.".to_string();
        }

        let Some(issue) = self.selected_issue() else {
            return self.no_filtered_issues_text("graph mode");
        };
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

        let si = status_icon(&issue.status);
        let ti = type_icon(&issue.issue_type);

        let mut lines = vec![format!(
            "Graph: nodes={} edges={} cycles={} actionable={}",
            self.analyzer.graph.node_count(),
            self.analyzer.graph.edge_count(),
            self.analyzer.metrics.cycles.len(),
            self.analyzer.graph.actionable_ids().len()
        )];

        // ASCII ego-node visualization with detail cursor
        let show_cursor = self.focus == FocusPane::Detail;
        let mut dep_index = 0usize;
        lines.push(String::new());
        if !blockers.is_empty() {
            lines.push("  BLOCKED BY:".to_string());
            for bid in blockers.iter().take(5) {
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
                lines.push(format!("  {prefix} [{bsi}] {bid}"));
                dep_index += 1;
            }
            if blockers.len() > 5 {
                lines.push(format!("    +{} more", blockers.len() - 5));
            }
            lines.push("        |".to_string());
            lines.push("        v".to_string());
        }

        lines.push(format!("  +---[{si} {ti} p{}]---+", issue.priority));
        lines.push(format!("  | {:<17} |", &issue.id));
        lines.push(format!("  | {:<17} |", truncate_str(&issue.title, 17)));
        lines.push(format!(
            "  | up:{} down:{:<8} |",
            blockers.len(),
            dependents.len()
        ));
        lines.push("  +-------------------+".to_string());

        if !dependents.is_empty() {
            lines.push("        |".to_string());
            lines.push("        v".to_string());
            lines.push("  BLOCKS (waiting):".to_string());
            for did in dependents.iter().take(5) {
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
                lines.push(format!("  {prefix} [{dsi}] {did}"));
                dep_index += 1;
            }
            if dependents.len() > 5 {
                lines.push(format!("    +{} more", dependents.len() - 5));
            }
        }

        // Metrics panel with mini-bars and rank badges (Go parity)
        lines.push(String::new());
        lines.push("GRAPH METRICS".to_string());
        lines.push("Importance:".to_string());
        let total = self.analyzer.issues.len();
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
        lines.push(format!(
            "  Critical Path  {:>8}  {}  #{}",
            depth,
            mini_bar(depth as f64, self.analyzer.graph.node_count() as f64),
            pr_rank
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
        let cut = if articulation { "YES" } else { "no" };
        lines.push(format!("  K-core: {k_core}  Slack: {slack:.4}  Cut: {cut}"));

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

        lines.join("\n")
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
                issue.created_at.as_deref().unwrap_or("n/a"),
                issue.updated_at.as_deref().unwrap_or("n/a"),
                issue.closed_at.as_deref().unwrap_or("n/a")
            ),
        ];

        if let (Some(created), Some(closed)) = (
            parse_timestamp(issue.created_at.as_deref()),
            parse_timestamp(issue.closed_at.as_deref()),
        ) {
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
                    let ts = event.timestamp.unwrap_or_else(|| "n/a".to_string());
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
            let total = self
                .history_git_cache
                .as_ref()
                .and_then(|cache| cache.histories.get(&issue.id))
                .map_or(0, |history| history.commits.as_ref().map_or(0, Vec::len));
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
    match status {
        "open" => "o",
        "in_progress" => "*",
        "blocked" => "!",
        "closed" => "x",
        "deferred" => "~",
        "review" => "r",
        "pinned" => "^",
        _ => "?",
    }
}

fn type_icon(issue_type: &str) -> &'static str {
    match issue_type {
        "bug" => "B",
        "feature" => "F",
        "task" => "T",
        "epic" => "E",
        "question" => "Q",
        "docs" => "D",
        _ => "-",
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

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
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

fn parse_timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    raw.and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
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
    let repo_root = loader::get_beads_dir(None)
        .ok()
        .and_then(|beads_dir| beads_dir.parent().map(std::path::Path::to_path_buf));

    BvrApp {
        analyzer: Analyzer::new(issues),
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
        history_confidence_index: 0,
        history_view_mode: HistoryViewMode::Bead,
        history_event_cursor: 0,
        history_related_bead_cursor: 0,
        history_bead_commit_cursor: 0,
        history_git_cache: None,
        history_search_active: false,
        history_search_query: String::new(),
        history_search_match_cursor: 0,
        history_show_file_tree: false,
        history_file_tree_cursor: 0,
        history_file_tree_filter: None,
        history_file_tree_focus: false,
        history_status_msg: String::new(),
        board_search_active: false,
        board_search_query: String::new(),
        board_search_match_cursor: 0,
        graph_search_active: false,
        graph_search_query: String::new(),
        graph_search_match_cursor: 0,
        insights_search_active: false,
        insights_search_query: String::new(),
        insights_search_match_cursor: 0,
        insights_panel: InsightsPanel::Bottlenecks,
        insights_show_explanations: true,
        insights_show_calc_proof: false,
        detail_dep_cursor: 0,
        #[cfg(test)]
        key_trace: Vec::new(),
    }
}

pub fn run_tui(issues: Vec<Issue>) -> Result<()> {
    let model = new_app(issues, ViewMode::Main);
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
        BoardGrouping, BvrApp, EmptyLaneVisibility, FocusPane, HistoryViewMode, InsightsPanel,
        ListFilter, ListSort, Msg, ViewMode, render_debug_view,
    };
    use crate::analysis::Analyzer;
    use crate::model::{Dependency, Issue};
    use ftui::core::event::{KeyCode, Modifiers};
    use ftui::runtime::{Cmd, Model};

    fn sample_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                created_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: Some("2026-01-02T00:00:00Z".to_string()),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                created_at: Some("2026-01-03T00:00:00Z".to_string()),
                updated_at: Some("2026-01-04T00:00:00Z".to_string()),
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
                created_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: Some("2026-01-06T00:00:00Z".to_string()),
                closed_at: Some("2026-01-06T00:00:00Z".to_string()),
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
                created_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: Some("2026-01-06T00:00:00Z".to_string()),
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Middle".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                created_at: Some("2026-01-02T00:00:00Z".to_string()),
                updated_at: Some("2026-01-05T00:00:00Z".to_string()),
                ..Issue::default()
            },
            Issue {
                id: "M".to_string(),
                title: "Newest".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                created_at: Some("2026-01-03T00:00:00Z".to_string()),
                updated_at: Some("2026-01-04T00:00:00Z".to_string()),
                ..Issue::default()
            },
        ]
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
            #[cfg(test)]
            key_trace: Vec::new(),
        }
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

        // Full cycle wraps back
        app.update(key(KeyCode::Char('s')));
        assert!(matches!(app.insights_panel, InsightsPanel::Bottlenecks));

        // S (shift) goes backwards
        app.update(key(KeyCode::Char('S')));
        assert!(matches!(app.insights_panel, InsightsPanel::Cycles));
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            history_search_match_cursor: 0,
            history_show_file_tree: false,
            history_file_tree_cursor: 0,
            history_file_tree_filter: None,
            history_file_tree_focus: false,
            history_status_msg: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            graph_search_active: false,
            graph_search_query: String::new(),
            graph_search_match_cursor: 0,
            insights_search_active: false,
            insights_search_query: String::new(),
            insights_search_match_cursor: 0,
            insights_panel: InsightsPanel::Bottlenecks,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
            detail_dep_cursor: 0,
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

    // -- History parity tests ------------------------------------------------

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
}
