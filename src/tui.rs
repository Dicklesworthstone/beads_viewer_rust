use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ftui::core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui::core::geometry::Rect;
use ftui::layout::{Constraint, Flex};
use ftui::render::frame::Frame;
use ftui::runtime::{App, Cmd, Model, ScreenMode};
use ftui::widgets::Widget;
use ftui::widgets::block::Block;
use ftui::widgets::paragraph::Paragraph;

use crate::analysis::Analyzer;
use crate::analysis::git_history::{
    GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryMilestonesCompat,
    correlate_histories_with_git, finalize_history_entries, load_git_commits,
};
use crate::loader;
use crate::model::Issue;
use crate::{BvrError, Result};

#[derive(Debug, Clone, Copy)]
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

const HISTORY_CONFIDENCE_STEPS: [f64; 4] = [0.0, 0.5, 0.75, 0.9];

#[derive(Debug, Clone)]
struct HistoryGitCache {
    commits: Vec<GitCommitRecord>,
    histories: BTreeMap<String, HistoryBeadCompat>,
    commit_index: BTreeMap<String, Vec<String>>,
    commit_bead_confidence: BTreeMap<String, Vec<(String, f64)>>,
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
    board_show_empty_lanes: bool,
    mode: ViewMode,
    mode_before_history: ViewMode,
    focus: FocusPane,
    focus_before_help: FocusPane,
    show_help: bool,
    show_quit_confirm: bool,
    history_confidence_index: usize,
    history_view_mode: HistoryViewMode,
    history_event_cursor: usize,
    history_related_bead_cursor: usize,
    history_bead_commit_cursor: usize,
    history_git_cache: Option<HistoryGitCache>,
    history_search_active: bool,
    history_search_query: String,
    board_search_active: bool,
    board_search_query: String,
    board_search_match_cursor: usize,
    insights_show_explanations: bool,
    insights_show_calc_proof: bool,
}

impl Model for BvrApp {
    type Message = Msg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::KeyPress(code, modifiers) => return self.handle_key(code, modifiers),
            Msg::Noop => {}
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let full = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let visible_count = self.visible_issue_indices().len();

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(3),
                Constraint::Fixed(1),
            ])
            .split(full);

        let header = Paragraph::new(format!(
            "bvr | mode={} | focus={} | issues={}/{} | filter={} | sort={} | ? help | Tab focus | Esc back/quit",
            self.mode.label(),
            self.focus.label(),
            visible_count,
            self.analyzer.issues.len(),
            self.list_filter.label(),
            self.list_sort.label()
        ));
        header.render(rows[0], frame);

        if self.show_help {
            Paragraph::new(self.help_overlay_text())
                .block(Block::bordered().title("Help"))
                .render(rows[1], frame);
            Paragraph::new("Press any key to close help.").render(rows[2], frame);
            return;
        }

        if self.show_quit_confirm {
            Paragraph::new("Quit bvr?\n\nPress Esc or Y to quit.\nPress any other key to cancel.")
                .block(Block::bordered().title("Confirm Quit"))
                .render(rows[1], frame);
            Paragraph::new("Esc/Y confirms quit. Any other key cancels.").render(rows[2], frame);
            return;
        }

        let body = rows[1];
        let panes = Flex::horizontal()
            .constraints([Constraint::Percentage(42.0), Constraint::Percentage(58.0)])
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
        let list_title = if self.focus == FocusPane::List {
            format!("{list_title} [focus]")
        } else {
            list_title.to_string()
        };

        Paragraph::new(list_text)
            .block(Block::bordered().title(&list_title))
            .render(panes[0], frame);

        let detail_text = self.detail_panel_text();
        let detail_title = match self.mode {
            ViewMode::Board => "Board Focus",
            ViewMode::Insights => "Insight Detail",
            ViewMode::Graph => "Graph Focus",
            ViewMode::History => "History Timeline",
            ViewMode::Main => "Details",
        };
        let detail_title = if self.focus == FocusPane::Detail {
            format!("{detail_title} [focus]")
        } else {
            detail_title.to_string()
        };
        Paragraph::new(detail_text)
            .block(Block::bordered().title(&detail_title))
            .render(panes[1], frame);

        let footer_text = match self.mode {
            ViewMode::Main => format!(
                "Main view mirrors bv split workflow. Press b/i/g/h for focused modes | s cycles sort ({})",
                self.list_sort.label()
            ),
            ViewMode::Board => {
                format!(
                    "Board mode: lane counts, queued IDs, and selected issue delivery context | grouping={} (s cycles) | empty-lanes={} (e toggles) | H/L lanes | 0/$ lane edges",
                    self.board_grouping.label(),
                    if self.board_show_empty_lanes {
                        "shown"
                    } else {
                        "hidden"
                    }
                )
            }
            ViewMode::Insights => {
                format!(
                    "Insights mode: bottlenecks, critical path pressure, and cycle risk hotspots | explanations={} (e) | calc-proof={} (x)",
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
                "History mode ({}): lifecycle timeline and event stream | c cycles confidence (bead mode) (>= {:.0}%) | v toggles bead/git | / search | h/Esc back",
                self.history_view_mode.label(),
                self.history_min_confidence() * 100.0
            ),
        };
        Paragraph::new(footer_text).render(rows[2], frame);
    }
}

impl BvrApp {
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
            self.show_help = false;
            self.focus = self.focus_before_help;
            return Cmd::None;
        }

        self.ensure_selected_visible();

        if matches!(self.mode, ViewMode::Board)
            && self.focus == FocusPane::List
            && self.board_search_active
        {
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

        match code {
            KeyCode::Char('?') => {
                self.show_help = true;
                self.focus_before_help = self.focus;
            }
            KeyCode::Enter => {
                if matches!(self.mode, ViewMode::History) {
                    if matches!(self.history_view_mode, HistoryViewMode::Git)
                        && let Some(bead_id) = self.selected_history_git_related_bead_id()
                    {
                        self.select_issue_by_id(&bead_id);
                    }
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
                self.focus = match self.focus {
                    FocusPane::List => FocusPane::Detail,
                    FocusPane::Detail => FocusPane::List,
                };
            }
            KeyCode::Char('h')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_lane_relative(-1);
            }
            KeyCode::Char('l')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_lane_relative(1);
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.start_board_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::History) && self.focus == FocusPane::List =>
            {
                self.start_history_search();
            }
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_search_match_relative(-1);
            }
            KeyCode::Char('j') | KeyCode::Down
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_row_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.move_board_row_relative(-1);
            }
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_board_row_relative(10);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
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
            KeyCode::Home | KeyCode::Char('0')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_edge_in_current_board_lane(false);
            }
            KeyCode::End | KeyCode::Char('G' | '$')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_edge_in_current_board_lane(true);
            }
            KeyCode::Home if self.focus == FocusPane::List => {
                self.select_first_visible();
            }
            KeyCode::End | KeyCode::Char('G') if self.focus == FocusPane::List => {
                self.select_last_visible();
            }
            KeyCode::Char('1')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_first_in_board_lane(1);
            }
            KeyCode::Char('2')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_first_in_board_lane(2);
            }
            KeyCode::Char('3')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_first_in_board_lane(3);
            }
            KeyCode::Char('4')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_first_in_board_lane(4);
            }
            KeyCode::Char('H')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_first_in_non_empty_board_lane();
            }
            KeyCode::Char('L')
                if matches!(self.mode, ViewMode::Board) && self.focus == FocusPane::List =>
            {
                self.select_last_in_non_empty_board_lane();
            }
            KeyCode::Char('s') if matches!(self.mode, ViewMode::Board) => {
                self.cycle_board_grouping();
            }
            KeyCode::Char('e') if matches!(self.mode, ViewMode::Board) => {
                self.toggle_board_show_empty_lanes();
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
                    && let Some(bead_id) = self.selected_history_git_related_bead_id()
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
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
    }

    fn finish_history_search(&mut self) {
        self.history_search_active = false;
    }

    fn cancel_history_search(&mut self) {
        self.history_search_active = false;
        self.history_search_query.clear();
        self.history_event_cursor = 0;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;
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

    fn ensure_git_history_loaded(&mut self) {
        if self.history_git_cache.is_some() {
            return;
        }

        let repo_root = self.repo_root.clone().or_else(|| std::env::current_dir().ok());
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
                        commits: Vec::new(),
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
            for commit in &history.commits {
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
            commit_index,
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
                            cache
                                .histories
                                .get(bead_id)
                                .is_some_and(|history| {
                                    history.commits.iter().any(|entry| {
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

        if !matches!(self.list_sort, ListSort::Default) {
            visible.sort_by(|left_index, right_index| {
                let left_issue = &self.analyzer.issues[*left_index];
                let right_issue = &self.analyzer.issues[*right_index];

                match self.list_sort {
                    ListSort::Default => left_issue.id.cmp(&right_issue.id),
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

    fn toggle_board_show_empty_lanes(&mut self) {
        self.board_show_empty_lanes = !self.board_show_empty_lanes;
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

        if !self.board_show_empty_lanes {
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
            self.focus = FocusPane::List;
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
            self.focus = FocusPane::List;
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
            self.focus = FocusPane::List;
        }
    }

    fn move_board_lane_relative(&mut self, delta: isize) {
        if !matches!(self.mode, ViewMode::Board) || self.focus != FocusPane::List || delta == 0 {
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
                self.focus = FocusPane::List;
                return;
            }
            target_lane_slot += delta.signum();
        }
    }

    fn move_board_row_relative(&mut self, delta: isize) {
        if !matches!(self.mode, ViewMode::Board) || self.focus != FocusPane::List || delta == 0 {
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
        self.focus = FocusPane::List;
    }

    fn start_board_search(&mut self) {
        if !matches!(self.mode, ViewMode::Board) || self.focus != FocusPane::List {
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
        self.focus = FocusPane::List;
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
        self.focus = FocusPane::List;
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
            self.focus = FocusPane::List;
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
            "  /               Board/History: search (board supports n/N match cycling)".to_string(),
            "  Tab              Toggle list/detail focus".to_string(),
            "  b/i/g/h          Toggle board/insights/graph/history".to_string(),
            "  Enter            Return to main detail pane".to_string(),
            "  o/c/r/a          Filter open/closed/ready/all".to_string(),
            "  s                Main: cycle sort | Board: cycle grouping".to_string(),
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
                if self.board_show_empty_lanes {
                    "shown"
                } else {
                    "hidden"
                }
            ));
            lines.push("Board: 1-4 jump lanes | H/L first/last lane".to_string());
            lines.push(
                "Board: 0/$ first/last issue in current lane | e toggle empty lanes".to_string(),
            );
        }

        if matches!(self.mode, ViewMode::Insights) {
            lines.push(String::new());
            lines.push(format!(
                "Insights toggles: explanations={} (e) | calc-proof={} (x)",
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
        let mut lane_rows = Vec::<String>::new();
        lane_rows.push(format!(
            "Grouping: {} (s cycles) | Empty lanes: {} (e toggles)",
            self.board_grouping.label(),
            if self.board_show_empty_lanes {
                "shown"
            } else {
                "hidden"
            }
        ));
        if self.board_search_active {
            lane_rows.push(format!("Search (active): /{}", self.board_search_query));
        } else if !self.board_search_query.is_empty() {
            lane_rows.push(format!("Search: /{} (n/N cycles)", self.board_search_query));
        }
        if !self.board_search_query.is_empty() {
            let matches = self.board_search_matches();
            if matches.is_empty() {
                lane_rows.push("Matches: none".to_string());
            } else {
                let position = self
                    .board_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                lane_rows.push(format!("Matches: {position}/{}", matches.len()));
            }
        }
        lane_rows.push(String::new());
        lane_rows.push("Lane          Count  Sample".to_string());
        lane_rows.push("----------------------------".to_string());

        for (lane, lane_indices) in lanes {
            let lane_issues = lane_indices
                .into_iter()
                .filter_map(|index| self.analyzer.issues.get(index))
                .map(|issue| issue.id.clone())
                .collect::<Vec<_>>();
            let sample = lane_issues
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            lane_rows.push(format!("{lane:<12} {:>5}  {sample}", lane_issues.len()));
        }

        lane_rows.push(String::new());
        lane_rows.push("Selected Queue Order:".to_string());
        lane_rows.extend(
            self.main_list_text()
                .lines()
                .map(std::string::ToString::to_string),
        );
        lane_rows.join("\n")
    }

    fn insights_list_text(&self) -> String {
        let insights = self.analyzer.insights();
        if insights.bottlenecks.is_empty() {
            return "No open issues to rank for bottlenecks.".to_string();
        }

        let mut lines = vec![format!(
            "Top Bottlenecks (score, blocks) | explanations={} (e) | calc-proof={} (x)",
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
        )];
        lines.extend(
            insights
                .bottlenecks
                .iter()
                .take(15)
                .enumerate()
                .map(|(index, item)| {
                    format!(
                        "{}. {:<12} score={:.3} blocks={}",
                        index + 1,
                        item.id,
                        item.score,
                        item.blocks_count
                    )
                }),
        );
        lines.join("\n")
    }

    fn graph_list_text(&self) -> String {
        let visible = self.visible_issue_indices();
        if visible.is_empty() {
            return format!("(no issues match filter: {})", self.list_filter.label());
        }

        visible
            .into_iter()
            .filter_map(|index| self.analyzer.issues.get(index).map(|issue| (index, issue)))
            .map(|(index, issue)| {
                let marker = if index == self.selected { '>' } else { ' ' };
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
                    "{marker} {:<12} in:{:>2} out:{:>2} pr:{:.3}",
                    issue.id, blocked_by, blocks, pagerank
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn history_list_text(&self) -> String {
        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            let query = self.history_search_query.trim();
            let events = self.history_timeline_events_filtered();
            if events.is_empty() {
                if query.is_empty() {
                    return "No timeline events available.".to_string();
                }
                return format!("(no timeline events match history search: /{query})");
            }

            let cursor = self
                .history_event_cursor
                .min(events.len().saturating_sub(1));

            let mut lines = Vec::<String>::new();
            if query.is_empty() {
                lines.push(format!(
                    "Event timeline ({} events) | v toggles to bead list | / search",
                    events.len()
                ));
            } else {
                let all_len = self.history_timeline_events().len();
                lines.push(format!(
                    "Event timeline (matches: {}/{all_len}) | v toggles to bead list | / search",
                    events.len()
                ));
            }

            if self.history_search_active {
                lines.push(format!("Search (active): /{}", self.history_search_query));
            } else if !query.is_empty() {
                lines.push(format!(
                    "Search: /{} (Esc clears)",
                    self.history_search_query
                ));
            }

            lines.push(String::new());
            lines.extend(events.iter().enumerate().map(|(index, event)| {
                let marker = if index == cursor { '>' } else { ' ' };
                let ts = event.event_timestamp.as_deref().unwrap_or("n/a");
                format!(
                    "{marker} {:<19} {:<10} {:<12} {}",
                    ts, event.event_kind, event.issue_id, event.event_details
                )
            }));
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
            lines.push(format!(
                "Search: /{} (Esc clears)",
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
        let lane_summary = self
            .board_lane_indices()
            .into_iter()
            .map(|(lane, indices)| format!("{lane}={}", indices.len()))
            .collect::<Vec<_>>()
            .join(" ");

        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        let open_blockers = self.analyzer.graph.open_blockers(&issue.id);

        [
            format!(
                "Lane Summary [{}]: {lane_summary}",
                self.board_grouping.label()
            ),
            String::new(),
            format!("Selected: {} ({})", issue.id, issue.title),
            format!("Current lane: {}", issue.status),
            format!(
                "Priority/Assignee: p{} / {}",
                issue.priority, issue.assignee
            ),
            format!("Depends on: {}", join_or_none(&blockers)),
            format!("Open blockers: {}", join_or_none(&open_blockers)),
            format!("Unblocks: {}", join_or_none(&dependents)),
            String::new(),
            "Next Action: move selected issue to next lane once open blockers are clear."
                .to_string(),
        ]
        .join("\n")
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
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or_default();

        let mut lines = vec![
            format!(
                "Insights Summary: bottlenecks={} critical_path={} cycles={} articulation={}",
                insights.bottlenecks.len(),
                insights.critical_path.len(),
                insights.cycles.len(),
                insights.articulation_points.len()
            ),
            String::new(),
            format!("Focus: {} ({})", issue.id, issue.title),
            format!("PageRank: {:.4}", pagerank),
            format!("Betweenness: {:.4}", betweenness),
            format!("Critical depth: {}", depth),
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
        let open_blockers = self.analyzer.graph.open_blockers(&issue.id);
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

        let mut lines = vec![
            format!(
                "Graph Summary: nodes={} edges={} cycles={} actionable={}",
                self.analyzer.graph.node_count(),
                self.analyzer.graph.edge_count(),
                self.analyzer.metrics.cycles.len(),
                self.analyzer.graph.actionable_ids().len()
            ),
            String::new(),
            format!("Focus: {} ({})", issue.id, issue.title),
            format!("Status/Priority: {} / p{}", issue.status, issue.priority),
            format!("Depends on: {}", join_or_none(&blockers)),
            format!("Open blockers: {}", join_or_none(&open_blockers)),
            format!("Direct dependents: {}", join_or_none(&dependents)),
            String::new(),
            format!("PageRank: {:.4}", pagerank),
            format!("Betweenness: {:.4}", betweenness),
            format!("Eigenvector: {:.4}", eigenvector),
            format!("Hubs/Authorities: {:.4} / {:.4}", hubs, authorities),
            format!("Critical depth: {}", depth),
            format!("K-core: {}", k_core),
            format!("Slack: {:.4}", slack),
            format!(
                "Articulation point: {}",
                if articulation { "yes" } else { "no" }
            ),
        ];

        if cycle_hits.is_empty() {
            lines.push("Cycle membership: none".to_string());
        } else {
            lines.push("Cycle membership:".to_string());
            lines.extend(
                cycle_hits
                    .iter()
                    .map(|cycle| format!("  - {}", cycle.join(" -> "))),
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
            let query = self.history_search_query.trim();
            let events = self.history_timeline_events_filtered();
            if events.is_empty() {
                if query.is_empty() {
                    return "No timeline events available.".to_string();
                }
                return format!("No timeline events match history search: /{query}");
            }

            let cursor = self
                .history_event_cursor
                .min(events.len().saturating_sub(1));
            let event = &events[cursor];

            let summary = if query.is_empty() {
                format!(
                    "History Summary: mode=git events={} selected_event={}/{}",
                    events.len(),
                    cursor + 1,
                    events.len()
                )
            } else {
                let all_len = self.history_timeline_events().len();
                format!(
                    "History Summary: mode=git search=/{query} matches={}/{all_len} selected_event={}/{}",
                    events.len(),
                    cursor + 1,
                    events.len()
                )
            };

            return [
                summary,
                String::new(),
                format!(
                    "Event: {} ({})",
                    event.event_kind,
                    event.event_timestamp.as_deref().unwrap_or("n/a")
                ),
                format!("Issue: {} ({})", event.issue_id, event.issue_title),
                format!("Issue status: {}", event.issue_status),
                format!("Details: {}", event.event_details),
                String::new(),
                "Enter: jump to issue detail in main mode".to_string(),
                "v: switch back to bead timeline".to_string(),
                "c: confidence filter applies only in bead mode".to_string(),
            ]
            .join("\n");
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

        lines.push(String::new());
        lines.push("Event Timeline:".to_string());

        if let Some(history) = selected_history {
            if history.events.is_empty() {
                lines.push("  (no events)".to_string());
            } else {
                lines.extend(history.events.into_iter().map(|event| {
                    let ts = event.timestamp.unwrap_or_else(|| "n/a".to_string());
                    format!("  {ts}  {:<10} {}", event.kind, event.details)
                }));
            }
        } else {
            lines.push("  (history unavailable for selected issue)".to_string());
        }

        lines.push(String::new());
        lines.push(
            "Git Correlation: use --robot-history for commit-level timeline and method stats."
                .to_string(),
        );

        lines.join("\n")
    }
}

#[must_use]
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

pub fn run_tui(issues: Vec<Issue>) -> Result<()> {
    let repo_root = loader::get_beads_dir(None)
        .ok()
        .and_then(|beads_dir| beads_dir.parent().map(|path| path.to_path_buf()));

    let model = BvrApp {
        analyzer: Analyzer::new(issues),
        repo_root,
        selected: 0,
        list_filter: ListFilter::All,
        list_sort: ListSort::Default,
        board_grouping: BoardGrouping::Status,
        board_show_empty_lanes: true,
        mode: ViewMode::Main,
        mode_before_history: ViewMode::Main,
        focus: FocusPane::List,
        focus_before_help: FocusPane::List,
        show_help: false,
        show_quit_confirm: false,
        history_confidence_index: 0,
        history_view_mode: HistoryViewMode::Bead,
        history_event_cursor: 0,
        history_related_bead_cursor: 0,
        history_bead_commit_cursor: 0,
        history_git_cache: None,
        history_search_active: false,
        history_search_query: String::new(),
        board_search_active: false,
        board_search_query: String::new(),
        board_search_match_cursor: 0,
        insights_show_explanations: true,
        insights_show_calc_proof: false,
    };

    App::new(model)
        .screen_mode(ScreenMode::AltScreen)
        .run()
        .map_err(|error| BvrError::Tui(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        BoardGrouping, BvrApp, FocusPane, HistoryViewMode, ListFilter, ListSort, Msg, ViewMode,
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
            board_show_empty_lanes: true,
            mode,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_related_bead_cursor: 0,
            history_bead_commit_cursor: 0,
            history_git_cache: None,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
    fn graph_mode_renders_metric_sections() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.mode = ViewMode::Graph;
        let text = app.detail_panel_text();
        assert!(text.contains("Graph Summary"));
        assert!(text.contains("PageRank"));
        assert!(text.contains("Top PageRank"));
    }

    #[test]
    fn board_mode_renders_lane_summary() {
        let mut app = new_app(ViewMode::Board, 1);
        app.mode = ViewMode::Board;
        let list = app.list_panel_text();
        let detail = app.detail_panel_text();
        assert!(list.contains("Lane"));
        assert!(detail.contains("Lane Summary"));
        assert!(detail.contains("Selected: B"));
    }

    #[test]
    fn insights_mode_renders_rank_sections() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.mode = ViewMode::Insights;
        let list = app.list_panel_text();
        let detail = app.detail_panel_text();
        assert!(list.contains("Top Bottlenecks"));
        assert!(detail.contains("Insights Summary"));
        assert!(detail.contains("Critical Path Head"));
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
        assert!(text.contains("Event Timeline"));
        assert!(text.contains("Git Correlation"));
        assert!(text.contains("Min confidence filter"));
    }

    #[test]
    fn graph_mode_snapshot_like_output_is_stable() {
        let app = new_app(ViewMode::Graph, 0);
        let text = app.detail_panel_text();
        let lines = text.lines().collect::<Vec<_>>();
        assert!(
            lines
                .first()
                .is_some_and(|line| line.starts_with("Graph Summary:"))
        );
        assert!(lines.iter().any(|line| line.contains("Focus: A (Root)")));
        assert!(lines.iter().any(|line| line.contains("Depends on: none")));
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
        assert!(lines.iter().any(|line| line.contains("Event Timeline:")));
    }

    #[test]
    fn help_tab_focus_and_quit_confirm_match_legacy_behavior() {
        let mut app = new_app(ViewMode::Main, 0);

        let cmd = app.update(key(KeyCode::Char('?')));
        assert!(matches!(cmd, Cmd::None));
        assert!(app.show_help);
        assert_eq!(app.focus, FocusPane::List);

        let cmd = app.update(key(KeyCode::Char('x')));
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
        assert!(app.list_panel_text().contains("Event timeline"));

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
        assert_eq!(app.history_event_cursor, 0);

        app.update(key(KeyCode::Char('J')));
        assert!(app.history_event_cursor >= 1);

        app.update(key(KeyCode::Char('K')));
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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

        app.update(key(KeyCode::Char('e')));
        assert!(!app.board_show_empty_lanes);
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Board,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
        };

        let list = app.list_panel_text();
        assert!(list.contains("other"));
        assert!(list.contains("QUE-1"));

        app.update(key(KeyCode::Char('e')));
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
            selected: 0,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_show_empty_lanes: true,
            mode: ViewMode::Main,
            mode_before_history: ViewMode::Main,
            focus: FocusPane::List,
            focus_before_help: FocusPane::List,
            show_help: false,
            show_quit_confirm: false,
            history_confidence_index: 0,
            history_view_mode: HistoryViewMode::Bead,
            history_event_cursor: 0,
            history_search_active: false,
            history_search_query: String::new(),
            board_search_active: false,
            board_search_query: String::new(),
            board_search_match_cursor: 0,
            insights_show_explanations: true,
            insights_show_calc_proof: false,
        };

        assert_eq!(first_rendered_issue_id(&app), "A");
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
        assert_eq!(first_rendered_issue_id(&app), "A");
    }
}
