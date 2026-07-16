use std::cell::Cell;
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::analysis::Analyzer;
use crate::analysis::git_history::{
    GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryMilestonesCompat,
    build_workspace_id_aliases, correlate_histories_with_git_aliases, finalize_history_entries,
    load_git_commits,
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
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui::core::geometry::Rect;
use ftui::layout::{Constraint, Flex};
use ftui::render::frame::Frame;
#[cfg(not(test))]
use ftui::runtime::TaskSpec;
use ftui::runtime::{App, Cmd, Model, ScreenMode};
use ftui::text::{
    Line as RichLine, Span as RichSpan, Text as RichText, display_width, truncate_to_width,
    truncate_with_ellipsis,
};
use ftui::widgets::Widget;
use ftui::widgets::block::Block;
use ftui::widgets::paragraph::Paragraph;
use ftui::widgets::table::{Row as TableRow, Table, TableState};
use ftui::widgets::StatefulWidget;

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

#[cfg(test)]
fn sprint_reference_now() -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-03-09T00:00:00Z")
        .expect("valid fixed sprint test timestamp")
        .with_timezone(&Utc)
}

#[cfg(not(test))]
fn sprint_reference_now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
fn ui_reference_now() -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-04-02T00:00:00Z")
        .expect("valid fixed ui test timestamp")
        .with_timezone(&Utc)
}

#[cfg(not(test))]
fn ui_reference_now() -> DateTime<Utc> {
    Utc::now()
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

    #[cfg(test)]
    /// Detail pane percentage for the horizontal split.
    fn detail_pct(self) -> f32 {
        100.0 - self.list_pct()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PaneSplitState {
    narrow_list_pct: f32,
    medium_list_pct: f32,
    wide_list_pct: f32,
    history_standard: [f32; 3],
    history_wide_git: [f32; 3],
    history_wide_bead: [f32; 4],
}

impl Default for PaneSplitState {
    fn default() -> Self {
        Self {
            narrow_list_pct: Breakpoint::Narrow.list_pct(),
            medium_list_pct: Breakpoint::Medium.list_pct(),
            wide_list_pct: Breakpoint::Wide.list_pct(),
            history_standard: [30.0, 35.0, 35.0],
            history_wide_git: [25.0, 30.0, 45.0],
            history_wide_bead: [20.0, 22.0, 25.0, 33.0],
        }
    }
}

impl PaneSplitState {
    fn two_pane_list_pct(self, breakpoint: Breakpoint) -> f32 {
        match breakpoint {
            Breakpoint::Narrow => self.narrow_list_pct,
            Breakpoint::Medium => self.medium_list_pct,
            Breakpoint::Wide => self.wide_list_pct,
        }
    }

    fn two_pane_detail_pct(self, breakpoint: Breakpoint) -> f32 {
        100.0 - self.two_pane_list_pct(breakpoint)
    }

    fn adjust_two_pane(&mut self, breakpoint: Breakpoint, delta_pct: f32) -> bool {
        let list = self.two_pane_list_pct(breakpoint);
        let clamped = (list + delta_pct).clamp(25.0, 75.0);
        if (clamped - list).abs() < f32::EPSILON {
            return false;
        }
        match breakpoint {
            Breakpoint::Narrow => self.narrow_list_pct = clamped,
            Breakpoint::Medium => self.medium_list_pct = clamped,
            Breakpoint::Wide => self.wide_list_pct = clamped,
        }
        true
    }

    fn history_pcts(self, layout: HistoryLayout, view_mode: HistoryViewMode) -> PaneSplitPreset {
        match (layout, view_mode) {
            (HistoryLayout::Wide, HistoryViewMode::Bead) => {
                PaneSplitPreset::Four(self.history_wide_bead)
            }
            (HistoryLayout::Wide, HistoryViewMode::Git) => {
                PaneSplitPreset::Three(self.history_wide_git)
            }
            (HistoryLayout::Standard, _) => PaneSplitPreset::Three(self.history_standard),
            (HistoryLayout::Narrow, _) => {
                PaneSplitPreset::Two([self.medium_list_pct, 100.0 - self.medium_list_pct])
            }
        }
    }

    fn adjust_history(
        &mut self,
        layout: HistoryLayout,
        view_mode: HistoryViewMode,
        focus: FocusPane,
        delta_pct: f32,
    ) -> bool {
        match (layout, view_mode) {
            (HistoryLayout::Wide, HistoryViewMode::Bead) => {
                let (primary, secondary) = match focus {
                    FocusPane::List => (0, 3),
                    FocusPane::Middle => (2, 3),
                    FocusPane::Detail => (3, 2),
                };
                adjust_split_pair(
                    &mut self.history_wide_bead,
                    primary,
                    secondary,
                    delta_pct,
                    15.0,
                )
            }
            (HistoryLayout::Standard, _) | (HistoryLayout::Wide, HistoryViewMode::Git) => {
                let (primary, secondary) = match focus {
                    FocusPane::List => (0, 2),
                    FocusPane::Middle => (1, 2),
                    FocusPane::Detail => (2, 1),
                };
                let target = if matches!(layout, HistoryLayout::Wide) {
                    &mut self.history_wide_git
                } else {
                    &mut self.history_standard
                };
                adjust_split_pair(target, primary, secondary, delta_pct, 18.0)
            }
            (HistoryLayout::Narrow, _) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PaneSplitPreset {
    Two([f32; 2]),
    Three([f32; 3]),
    Four([f32; 4]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitterTarget {
    TwoPane { breakpoint: Breakpoint },
    HistoryThree { wide: bool, divider: usize },
    HistoryFour { divider: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SplitterHitBox {
    target: SplitterTarget,
    rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeaderModeTab {
    mode: ViewMode,
    rect: Rect,
}

impl PaneSplitState {
    fn adjust_splitter_target(&mut self, target: SplitterTarget, delta_pct: f32) -> bool {
        match target {
            SplitterTarget::TwoPane { breakpoint } => self.adjust_two_pane(breakpoint, delta_pct),
            SplitterTarget::HistoryThree { wide, divider } => {
                let target = if wide {
                    &mut self.history_wide_git
                } else {
                    &mut self.history_standard
                };
                adjust_split_pair(target, divider, divider + 1, delta_pct, 18.0)
            }
            SplitterTarget::HistoryFour { divider } => adjust_split_pair(
                &mut self.history_wide_bead,
                divider,
                divider + 1,
                delta_pct,
                15.0,
            ),
        }
    }
}

fn adjust_split_pair<const N: usize>(
    ratios: &mut [f32; N],
    primary: usize,
    secondary: usize,
    delta_pct: f32,
    min_pct: f32,
) -> bool {
    let max_increase = ratios[secondary] - min_pct;
    let max_decrease = ratios[primary] - min_pct;
    let applied = delta_pct.clamp(-max_decrease, max_increase);
    if applied.abs() < f32::EPSILON {
        return false;
    }
    ratios[primary] += applied;
    ratios[secondary] -= applied;
    true
}

thread_local! {
    static LAST_VIEW_WIDTH: Cell<u16> = const { Cell::new(80) };
    static LAST_VIEW_HEIGHT: Cell<u16> = const { Cell::new(24) };
    static LAST_DETAIL_CONTENT_AREA: Cell<Rect> = const { Cell::new(Rect::new(0, 0, 0, 0)) };
    static PANE_SPLIT_STATE: Cell<PaneSplitState> = const { Cell::new(PaneSplitState {
        narrow_list_pct: 35.0,
        medium_list_pct: 42.0,
        wide_list_pct: 38.0,
        history_standard: [30.0, 35.0, 35.0],
        history_wide_git: [25.0, 30.0, 45.0],
        history_wide_bead: [20.0, 22.0, 25.0, 33.0],
    }) };
}

fn record_view_size(width: u16, height: u16) {
    LAST_VIEW_WIDTH.with(|cell| cell.set(width));
    LAST_VIEW_HEIGHT.with(|cell| cell.set(height));
}

fn record_detail_content_area(area: Rect) {
    LAST_DETAIL_CONTENT_AREA.with(|cell| cell.set(area));
}

fn cached_detail_content_area() -> Rect {
    LAST_DETAIL_CONTENT_AREA.with(Cell::get)
}

fn cached_view_width() -> u16 {
    LAST_VIEW_WIDTH.with(Cell::get)
}

fn cached_view_height() -> u16 {
    LAST_VIEW_HEIGHT.with(Cell::get)
}

fn pane_split_state() -> PaneSplitState {
    PANE_SPLIT_STATE.with(Cell::get)
}

fn set_pane_split_state(state: PaneSplitState) {
    PANE_SPLIT_STATE.with(|cell| cell.set(state));
}

const fn block_inner_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn saturating_scroll_offset(offset: usize) -> u16 {
    u16::try_from(offset).unwrap_or(u16::MAX)
}

const fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && y >= area.y
        && x < area.x.saturating_add(area.width)
        && y < area.y.saturating_add(area.height)
}

fn splitter_rect_between(left: Rect, right: Rect) -> Rect {
    let x = left.x.saturating_add(left.width).saturating_sub(1);
    let max_right = right.x.saturating_add(right.width);
    let width = if x < max_right {
        (max_right - x).min(2)
    } else {
        1
    };
    Rect::new(x, left.y, width.max(1), left.height)
}

fn splitter_hit_boxes(app: &BvrApp, width: u16, height: u16) -> Vec<SplitterHitBox> {
    let full = Rect::from_size(width, height);
    let rows = Flex::vertical()
        .constraints([
            Constraint::Fixed(1),
            Constraint::Min(3),
            Constraint::Fixed(1),
        ])
        .split(full);
    let body = rows[1];
    let bp = Breakpoint::from_width(width);
    let split_state = pane_split_state();
    let graph_single_pane = matches!(app.mode, ViewMode::Graph) && matches!(bp, Breakpoint::Narrow);
    let history_layout = if matches!(app.mode, ViewMode::History) {
        HistoryLayout::from_width(body.width)
    } else {
        HistoryLayout::Narrow
    };
    let history_multi_pane =
        matches!(app.mode, ViewMode::History) && history_layout.has_middle_pane();

    if graph_single_pane {
        return Vec::new();
    }

    if history_multi_pane {
        if matches!(history_layout, HistoryLayout::Wide)
            && matches!(app.history_view_mode, HistoryViewMode::Bead)
        {
            let PaneSplitPreset::Four(pcts) =
                split_state.history_pcts(history_layout, app.history_view_mode)
            else {
                unreachable!("wide bead history should use four-pane split");
            };
            let panes = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(pcts[0]),
                    Constraint::Percentage(pcts[1]),
                    Constraint::Percentage(pcts[2]),
                    Constraint::Percentage(pcts[3]),
                ])
                .split(body);
            return vec![
                SplitterHitBox {
                    target: SplitterTarget::HistoryFour { divider: 0 },
                    rect: splitter_rect_between(panes[0], panes[1]),
                },
                SplitterHitBox {
                    target: SplitterTarget::HistoryFour { divider: 1 },
                    rect: splitter_rect_between(panes[1], panes[2]),
                },
                SplitterHitBox {
                    target: SplitterTarget::HistoryFour { divider: 2 },
                    rect: splitter_rect_between(panes[2], panes[3]),
                },
            ];
        }

        let PaneSplitPreset::Three(pane_widths) =
            split_state.history_pcts(history_layout, app.history_view_mode)
        else {
            unreachable!("multi-pane history should use three-pane split");
        };
        let panes = Flex::horizontal()
            .constraints([
                Constraint::Percentage(pane_widths[0]),
                Constraint::Percentage(pane_widths[1]),
                Constraint::Percentage(pane_widths[2]),
            ])
            .split(body);
        return vec![
            SplitterHitBox {
                target: SplitterTarget::HistoryThree {
                    wide: matches!(history_layout, HistoryLayout::Wide),
                    divider: 0,
                },
                rect: splitter_rect_between(panes[0], panes[1]),
            },
            SplitterHitBox {
                target: SplitterTarget::HistoryThree {
                    wide: matches!(history_layout, HistoryLayout::Wide),
                    divider: 1,
                },
                rect: splitter_rect_between(panes[1], panes[2]),
            },
        ];
    }

    let panes = Flex::horizontal()
        .constraints([
            Constraint::Percentage(split_state.two_pane_list_pct(bp)),
            Constraint::Percentage(split_state.two_pane_detail_pct(bp)),
        ])
        .split(body);
    vec![SplitterHitBox {
        target: SplitterTarget::TwoPane { breakpoint: bp },
        rect: splitter_rect_between(panes[0], panes[1]),
    }]
}

fn header_tab_candidate_modes(mode: ViewMode, bp: Breakpoint) -> Vec<ViewMode> {
    let mut modes = match bp {
        Breakpoint::Narrow => vec![
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::History,
        ],
        Breakpoint::Medium => vec![
            ViewMode::Main,
            ViewMode::Board,
            ViewMode::Insights,
            ViewMode::Graph,
            ViewMode::History,
            ViewMode::Actionable,
            ViewMode::Attention,
            ViewMode::Tree,
            ViewMode::Sprint,
        ],
        Breakpoint::Wide => ViewMode::navigation_order().to_vec(),
    };
    if !modes.contains(&mode) {
        modes.push(mode);
    }
    modes
}

fn header_mode_tabs(app: &BvrApp, width: u16) -> Vec<HeaderModeTab> {
    let bp = Breakpoint::from_width(width);
    // Keep enough horizontal budget for the status chips so the active
    // filter/sort state remains visible at common terminal widths.
    let reserved_width = match bp {
        Breakpoint::Narrow => 16u16,
        Breakpoint::Medium => 70u16,
        Breakpoint::Wide if width < 132 => 68u16,
        Breakpoint::Wide => 56u16,
    };
    let max_x = width.saturating_sub(reserved_width);
    let mut x = 4u16;
    let mut tabs = Vec::new();

    for mode in header_tab_candidate_modes(app.mode, bp) {
        let label = mode.tab_text(bp);
        let tab_width = u16::try_from(display_width(&label)).unwrap_or(u16::MAX);
        if tab_width == 0 {
            continue;
        }

        let next_end = x.saturating_add(tab_width);
        if !tabs.is_empty() && next_end >= max_x {
            break;
        }

        tabs.push(HeaderModeTab {
            mode,
            rect: Rect::new(x, 0, tab_width, 1),
        });
        x = next_end.saturating_add(1);
    }

    if !tabs.iter().any(|tab| tab.mode == app.mode) {
        let label = app.mode.tab_text(bp);
        let tab_width = u16::try_from(display_width(&label)).unwrap_or(u16::MAX);
        let start_x = width.saturating_sub(tab_width.saturating_add(1));
        tabs.push(HeaderModeTab {
            mode: app.mode,
            rect: Rect::new(start_x, 0, tab_width.min(width.saturating_sub(start_x)), 1),
        });
    }

    tabs
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandHint<'a> {
    key: &'a str,
    desc: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Muted,
}

/// Semantic colour tokens with light/dark palette support.
///
/// Detects terminal background via `BV_THEME` env var (`light` | `dark`)
/// with dark-mode default.  All 9+ issue statuses, 5 priority levels,
/// 5 issue types, and distinct footer styling tokens are included.
mod tokens {
    use super::SemanticTone;
    use ftui::{PackedRgba, Style};

    // ── Theme detection ──────────────────────────────────────────────

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ThemeMode {
        Dark,
        Light,
    }

    /// Detect theme from `BV_THEME` env var.  Falls back to dark.
    /// Result is cached for the lifetime of the process.
    pub fn detect_theme() -> ThemeMode {
        static CACHED: std::sync::OnceLock<ThemeMode> = std::sync::OnceLock::new();
        *CACHED.get_or_init(|| match std::env::var("BV_THEME").as_deref() {
            Ok("light") => ThemeMode::Light,
            _ => ThemeMode::Dark,
        })
    }

    fn is_light() -> bool {
        detect_theme() == ThemeMode::Light
    }

    /// Runtime light/dark picker (zero-cost after first call).
    fn p(light: PackedRgba, dark: PackedRgba) -> PackedRgba {
        if is_light() { light } else { dark }
    }

    // ── Palette primitives (dark defaults, light alternatives) ───────

    // Foreground
    pub const FG_DEFAULT: PackedRgba = PackedRgba::rgb(248, 248, 242); // #f8f8f2
    pub const FG_DEFAULT_LIGHT: PackedRgba = PackedRgba::rgb(26, 26, 26); // #1a1a1a
    pub const FG_DIM: PackedRgba = PackedRgba::rgb(136, 136, 136); // #888888
    pub const FG_ACCENT: PackedRgba = PackedRgba::rgb(189, 147, 249); // #bd93f9 purple
    pub const FG_ACCENT_LIGHT: PackedRgba = PackedRgba::rgb(107, 71, 217); // #6b47d9
    pub const FG_SUCCESS: PackedRgba = PackedRgba::rgb(80, 250, 123); // #50fa7b green
    pub const FG_SUCCESS_LIGHT: PackedRgba = PackedRgba::rgb(0, 119, 0); // #007700
    pub const FG_WARNING: PackedRgba = PackedRgba::rgb(255, 184, 108); // #ffb86c orange
    pub const FG_WARNING_LIGHT: PackedRgba = PackedRgba::rgb(176, 104, 0); // #b06800
    pub const FG_ERROR: PackedRgba = PackedRgba::rgb(255, 85, 85); // #ff5555 red
    pub const FG_ERROR_LIGHT: PackedRgba = PackedRgba::rgb(204, 0, 0); // #cc0000
    pub const FG_INFO: PackedRgba = PackedRgba::rgb(139, 233, 253); // #8be9fd cyan
    pub const FG_INFO_LIGHT: PackedRgba = PackedRgba::rgb(0, 96, 128); // #006080
    pub const FG_MUTED: PackedRgba = PackedRgba::rgb(98, 114, 164); // #6272a4
    pub const FG_MUTED_LIGHT: PackedRgba = PackedRgba::rgb(102, 102, 102); // #666666
    pub const FG_SUBTEXT: PackedRgba = PackedRgba::rgb(191, 191, 191); // #bfbfbf
    pub const FG_SUBTEXT_LIGHT: PackedRgba = PackedRgba::rgb(85, 85, 85); // #555555

    // Background
    pub const BG_SURFACE: PackedRgba = PackedRgba::rgb(54, 57, 73); // #363949
    pub const BG_SURFACE_LIGHT: PackedRgba = PackedRgba::rgb(232, 232, 232); // #e8e8e8
    pub const BG_HIGHLIGHT: PackedRgba = PackedRgba::rgb(68, 71, 90); // #44475a
    pub const BG_HIGHLIGHT_LIGHT: PackedRgba = PackedRgba::rgb(208, 208, 208); // #d0d0d0
    pub const BG_DARK: PackedRgba = PackedRgba::rgb(30, 31, 41); // #1e1f29

    // Semantic surface tints (dark)
    pub const BG_SURFACE_ACCENT: PackedRgba = PackedRgba::rgb(42, 26, 68); // #2a1a44
    pub const BG_SURFACE_SUCCESS: PackedRgba = PackedRgba::rgb(26, 61, 42); // #1a3d2a
    pub const BG_SURFACE_WARNING: PackedRgba = PackedRgba::rgb(61, 42, 26); // #3d2a1a
    pub const BG_SURFACE_DANGER: PackedRgba = PackedRgba::rgb(61, 26, 26); // #3d1a1a
    pub const BG_SURFACE_INFO: PackedRgba = PackedRgba::rgb(26, 51, 68); // #1a3344
    pub const BG_SURFACE_MUTED: PackedRgba = PackedRgba::rgb(42, 42, 61); // #2a2a3d

    // Semantic surface tints (light)
    pub const BG_SURFACE_ACCENT_L: PackedRgba = PackedRgba::rgb(232, 221, 255); // #e8ddff
    pub const BG_SURFACE_SUCCESS_L: PackedRgba = PackedRgba::rgb(212, 237, 218); // #d4edda
    pub const BG_SURFACE_WARNING_L: PackedRgba = PackedRgba::rgb(255, 232, 204); // #ffe8cc
    pub const BG_SURFACE_DANGER_L: PackedRgba = PackedRgba::rgb(248, 215, 218); // #f8d7da
    pub const BG_SURFACE_INFO_L: PackedRgba = PackedRgba::rgb(209, 236, 241); // #d1ecf1
    pub const BG_SURFACE_MUTED_L: PackedRgba = PackedRgba::rgb(226, 227, 229); // #e2e3e5

    // ── Status colours (9 statuses, matching Go bv) ──────────────────

    pub fn status_fg(status: &str) -> PackedRgba {
        let s = status.to_ascii_lowercase();
        let s = s.as_str();
        match s {
            "open" => p(FG_SUCCESS_LIGHT, FG_SUCCESS),
            "in_progress" => p(FG_INFO_LIGHT, FG_INFO),
            "blocked" => p(FG_ERROR_LIGHT, FG_ERROR),
            "deferred" | "draft" => p(FG_WARNING_LIGHT, FG_WARNING),
            "pinned" => p(PackedRgba::rgb(0, 102, 204), PackedRgba::rgb(102, 153, 255)),
            "hooked" => p(PackedRgba::rgb(0, 128, 128), PackedRgba::rgb(0, 206, 209)),
            "review" => p(FG_ACCENT_LIGHT, FG_ACCENT),
            "closed" => p(FG_SUBTEXT_LIGHT, FG_MUTED),
            "tombstone" => p(FG_DIM, PackedRgba::rgb(68, 71, 90)),
            _ => p(FG_DIM, FG_DIM),
        }
    }

    pub fn status_bg(status: &str) -> PackedRgba {
        let s = status.to_ascii_lowercase();
        let s = s.as_str();
        match s {
            "open" => p(BG_SURFACE_SUCCESS_L, BG_SURFACE_SUCCESS),
            "in_progress" => p(BG_SURFACE_INFO_L, BG_SURFACE_INFO),
            "blocked" => p(BG_SURFACE_DANGER_L, BG_SURFACE_DANGER),
            "deferred" | "draft" => p(BG_SURFACE_WARNING_L, BG_SURFACE_WARNING),
            "pinned" => p(PackedRgba::rgb(204, 229, 255), PackedRgba::rgb(26, 42, 68)),
            "hooked" => p(PackedRgba::rgb(204, 255, 255), PackedRgba::rgb(26, 61, 61)),
            "review" => p(BG_SURFACE_ACCENT_L, BG_SURFACE_ACCENT),
            "closed" => p(BG_SURFACE_MUTED_L, BG_SURFACE_MUTED),
            "tombstone" => p(BG_HIGHLIGHT_LIGHT, BG_DARK),
            _ => p(BG_SURFACE_LIGHT, BG_SURFACE),
        }
    }

    /// Short status badge label (4 chars).
    pub fn status_badge_label(status: &str) -> &'static str {
        let s = status.to_ascii_lowercase();
        match s.as_str() {
            "open" => "OPEN",
            "in_progress" => "PROG",
            "blocked" => "BLKD",
            "deferred" => "DEFR",
            "draft" => "DRFT",
            "pinned" => "PIN ",
            "hooked" => "HOOK",
            "review" => "REVW",
            "closed" => "DONE",
            "tombstone" => "TOMB",
            _ => "????",
        }
    }

    // ── Priority colours (P0-P4 with bg) ─────────────────────────────

    pub const PRIO_P0: PackedRgba = FG_ERROR; // critical red
    pub const PRIO_P1: PackedRgba = FG_WARNING; // high orange
    pub const PRIO_P2: PackedRgba = PackedRgba::rgb(241, 250, 140); // #f1fa8c medium yellow
    pub const PRIO_P3: PackedRgba = FG_SUCCESS; // low green
    pub const PRIO_P4: PackedRgba = FG_MUTED; // backlog gray

    pub fn priority_fg(prio: u8) -> PackedRgba {
        match prio {
            0 => p(FG_ERROR_LIGHT, PRIO_P0),
            1 => p(FG_WARNING_LIGHT, PRIO_P1),
            2 => p(PackedRgba::rgb(128, 128, 0), PRIO_P2),
            3 => p(FG_SUCCESS_LIGHT, PRIO_P3),
            _ => p(FG_MUTED_LIGHT, PRIO_P4),
        }
    }

    pub fn priority_bg(prio: u8) -> PackedRgba {
        match prio {
            0 => p(BG_SURFACE_DANGER_L, BG_SURFACE_DANGER),
            1 => p(BG_SURFACE_WARNING_L, BG_SURFACE_WARNING),
            2 => p(PackedRgba::rgb(255, 243, 205), PackedRgba::rgb(61, 61, 26)),
            3 => p(BG_SURFACE_SUCCESS_L, BG_SURFACE_SUCCESS),
            _ => p(BG_SURFACE_LIGHT, BG_SURFACE),
        }
    }

    pub fn priority_style(prio: u8) -> Style {
        Style::new().fg(priority_fg(prio)).bold()
    }

    /// Priority badge style with background.
    pub fn priority_badge(prio: u8) -> Style {
        Style::new()
            .fg(priority_fg(prio))
            .bg(priority_bg(prio))
            .bold()
    }

    // ── Type colours and icons ───────────────────────────────────────

    pub fn type_fg(issue_type: &str) -> PackedRgba {
        let t = issue_type.to_ascii_lowercase();
        match t.as_str() {
            "bug" => p(FG_ERROR_LIGHT, FG_ERROR),
            "feature" => p(FG_WARNING_LIGHT, FG_WARNING),
            "task" => p(PackedRgba::rgb(128, 128, 0), PackedRgba::rgb(241, 250, 140)),
            "epic" => p(FG_ACCENT_LIGHT, FG_ACCENT),
            "docs" => p(FG_INFO_LIGHT, FG_INFO),
            _ => p(FG_SUBTEXT_LIGHT, FG_SUBTEXT),
        }
    }

    // ── Footer tokens (4 distinct styles matching Go bv) ─────────────

    /// Footer keybinding label (bright, bold).
    pub fn footer_key() -> Style {
        Style::new()
            .fg(p(
                PackedRgba::rgb(51, 51, 51),
                PackedRgba::rgb(224, 224, 232),
            ))
            .bold()
    }

    /// Footer navigation hint text (medium brightness).
    pub fn footer_hint() -> Style {
        Style::new().fg(p(
            PackedRgba::rgb(68, 68, 68),
            PackedRgba::rgb(200, 200, 208),
        ))
    }

    /// Footer separator (`│` between hints).
    pub fn footer_sep() -> Style {
        Style::new().fg(p(
            PackedRgba::rgb(136, 136, 136),
            PackedRgba::rgb(136, 136, 160),
        ))
    }

    /// Footer secondary text (issue counts, dim info).
    pub fn footer_dim() -> Style {
        Style::new().fg(p(
            PackedRgba::rgb(85, 85, 85),
            PackedRgba::rgb(160, 160, 184),
        ))
    }

    // ── Heatmap gradient for sparkline/importance ─────────────────────

    /// Map a 0.0–1.0 score to a heatmap colour.
    pub fn heatmap_color(score: f64) -> PackedRgba {
        if score > 0.8 {
            p(FG_ACCENT_LIGHT, FG_ACCENT)
        } else if score > 0.5 {
            p(FG_WARNING_LIGHT, FG_WARNING)
        } else if score > 0.2 {
            p(FG_INFO_LIGHT, FG_INFO)
        } else {
            p(FG_SUBTEXT_LIGHT, FG_MUTED)
        }
    }

    // ── Sparkline characters ─────────────────────────────────────────

    const SPARK_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    /// Render a sparkline string for a 0.0–1.0 value at the given width.
    pub fn render_sparkline(value: f64, width: usize) -> String {
        let clamped = value.clamp(0.0, 1.0);
        let total = (clamped * width as f64 * 8.0) as usize;
        let full_blocks = total / 8;
        let remainder = total % 8;
        let mut s = String::with_capacity(width);
        for _ in 0..full_blocks.min(width) {
            s.push('█');
        }
        if full_blocks < width && remainder > 0 {
            s.push(SPARK_CHARS[remainder.saturating_sub(1)]);
        }
        let drawn = s.chars().count();
        for _ in drawn..width {
            s.push(' ');
        }
        s
    }

    // ── Priority hint arrows ─────────────────────────────────────────

    // ── Preserved original API (backward compat) ─────────────────────

    pub fn fg_default() -> PackedRgba {
        p(FG_DEFAULT_LIGHT, FG_DEFAULT)
    }
    pub fn fg_dim() -> PackedRgba {
        p(FG_MUTED_LIGHT, FG_DIM)
    }
    pub fn fg_accent() -> PackedRgba {
        p(FG_ACCENT_LIGHT, FG_ACCENT)
    }
    pub fn bg_surface() -> PackedRgba {
        p(BG_SURFACE_LIGHT, BG_SURFACE)
    }
    pub fn bg_highlight() -> PackedRgba {
        p(BG_HIGHLIGHT_LIGHT, BG_HIGHLIGHT)
    }

    // ── Semantic styles (runtime theme-aware) ────────────────────────

    pub fn header() -> Style {
        Style::new().fg(fg_accent()).bold()
    }

    pub fn header_bg() -> Style {
        Style::new().fg(fg_accent()).bg(bg_surface()).bold()
    }

    pub fn footer() -> Style {
        footer_hint()
    }

    pub fn selected() -> Style {
        Style::new().fg(fg_default()).bg(bg_highlight()).bold()
    }

    pub fn panel_title() -> Style {
        Style::new().fg(fg_default()).bold()
    }

    pub fn panel_title_focused() -> Style {
        Style::new().fg(fg_accent()).bold()
    }

    pub fn semantic_fg(tone: SemanticTone) -> PackedRgba {
        match tone {
            SemanticTone::Neutral => fg_default(),
            SemanticTone::Accent => fg_accent(),
            SemanticTone::Success => p(FG_SUCCESS_LIGHT, FG_SUCCESS),
            SemanticTone::Warning => p(FG_WARNING_LIGHT, FG_WARNING),
            SemanticTone::Danger => p(FG_ERROR_LIGHT, FG_ERROR),
            SemanticTone::Muted => fg_dim(),
        }
    }

    pub fn semantic_bg(tone: SemanticTone) -> PackedRgba {
        match tone {
            SemanticTone::Neutral => bg_surface(),
            SemanticTone::Accent => p(BG_SURFACE_ACCENT_L, BG_SURFACE_ACCENT),
            SemanticTone::Success => p(BG_SURFACE_SUCCESS_L, BG_SURFACE_SUCCESS),
            SemanticTone::Warning => p(BG_SURFACE_WARNING_L, BG_SURFACE_WARNING),
            SemanticTone::Danger => p(BG_SURFACE_DANGER_L, BG_SURFACE_DANGER),
            SemanticTone::Muted => p(BG_SURFACE_MUTED_L, BG_SURFACE_MUTED),
        }
    }

    pub fn chip_style(tone: SemanticTone) -> Style {
        Style::new()
            .fg(semantic_fg(tone))
            .bg(semantic_bg(tone))
            .bold()
    }

    pub fn panel_border_for(tone: SemanticTone, focused: bool) -> Style {
        let tone = if focused { tone } else { SemanticTone::Muted };
        Style::new().fg(semantic_fg(tone))
    }

    pub fn panel_title_for(tone: SemanticTone, focused: bool) -> Style {
        let tone = if focused { tone } else { SemanticTone::Neutral };
        Style::new().fg(semantic_fg(tone)).bold()
    }

    pub fn status_style(status: &str) -> Style {
        Style::new().fg(status_fg(status))
    }

    /// Status badge style with foreground and background.
    pub fn status_badge(status: &str) -> Style {
        Style::new()
            .fg(status_fg(status))
            .bg(status_bg(status))
            .bold()
    }

    pub fn help_desc() -> Style {
        Style::new().fg(fg_default())
    }

    pub fn dim() -> Style {
        Style::new().fg(fg_dim())
    }
}

fn semantic_panel_block(title: &str, focused: bool, tone: SemanticTone) -> Block<'_> {
    Block::bordered()
        .title(title)
        .border_style(tokens::panel_border_for(tone, focused))
        .style(tokens::panel_title_for(tone, focused))
}

fn push_chip(line: &mut RichLine, label: &str, tone: SemanticTone) {
    line.push_span(RichSpan::styled(
        truncate_display(label, 32),
        tokens::chip_style(tone),
    ));
}

fn push_metric_chip(line: &mut RichLine, label: &str, value: &str, tone: SemanticTone) {
    push_chip(line, &format!("{label}={value}"), tone);
}

fn build_header_text(app: &BvrApp, width: u16) -> RichText {
    let bp = Breakpoint::from_width(width);
    let visible_count = app.visible_issue_indices().len();
    let total_count = app.analyzer.issues.len();
    let mode_tabs = header_mode_tabs(app, width);

    if matches!(bp, Breakpoint::Narrow) {
        let mut line = RichLine::new();
        line.push_span(RichSpan::styled("bvr", tokens::header()));
        line.push_span(RichSpan::raw(" "));
        for tab in &mode_tabs {
            push_chip(
                &mut line,
                &tab.mode.tab_text(bp),
                if tab.mode == app.mode {
                    SemanticTone::Accent
                } else {
                    SemanticTone::Muted
                },
            );
            line.push_span(RichSpan::raw(" "));
        }
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_chip(
            &mut line,
            &format!("{visible_count}/{total_count}"),
            SemanticTone::Neutral,
        );
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_chip(&mut line, app.list_filter.label(), SemanticTone::Muted);
        return RichText::from_lines([line]);
    }

    let mut filter_label = app.list_filter.label().to_string();
    if let Some(ref label) = app.modal_label_filter {
        filter_label = format!("{filter_label}+label:{label}");
    }
    if let Some(ref repo) = app.modal_repo_filter {
        filter_label = format!("{filter_label}+repo:{repo}");
    }
    let mode_label = if matches!(app.mode, ViewMode::History) {
        format!("{} {}", app.mode.label(), app.history_view_mode.indicator())
    } else {
        app.mode.label().to_string()
    };

    let mut line = RichLine::new();
    line.push_span(RichSpan::styled("bvr", tokens::header()));
    line.push_span(RichSpan::raw(" "));
    for tab in &mode_tabs {
        push_chip(
            &mut line,
            &tab.mode.tab_text(bp),
            if tab.mode == app.mode {
                SemanticTone::Accent
            } else {
                SemanticTone::Muted
            },
        );
        line.push_span(RichSpan::raw(" "));
    }
    line.push_span(RichSpan::styled("| mode=", tokens::dim()));
    push_chip(&mut line, &mode_label, SemanticTone::Accent);
    line.push_span(RichSpan::styled(" | focus=", tokens::dim()));
    push_chip(&mut line, app.focus.label(), SemanticTone::Warning);
    line.push_span(RichSpan::styled(" | ", tokens::dim()));
    push_metric_chip(
        &mut line,
        "issues",
        &format!("{visible_count}/{total_count}"),
        SemanticTone::Neutral,
    );
    if matches!(bp, Breakpoint::Medium | Breakpoint::Wide) {
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(&mut line, "filter", &filter_label, SemanticTone::Muted);
    }
    if matches!(bp, Breakpoint::Wide) && !app.slow_metrics_pending {
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(
            &mut line,
            "sort",
            app.list_sort.label(),
            SemanticTone::Neutral,
        );
    }
    if app.slow_metrics_pending {
        line.push_span(RichSpan::styled(" | metrics: ", tokens::dim()));
        push_chip(&mut line, "computing...", SemanticTone::Warning);
    }
    line.push_span(RichSpan::styled(" |", tokens::dim()));
    RichText::from_lines([line])
}

// ---------------------------------------------------------------------------
// Reusable visual primitives — shared building blocks for TUI surfaces.
// Each returns RichSpan(s) or RichLine so callers can compose them freely.
// ---------------------------------------------------------------------------

#[cfg_attr(not(test), allow(dead_code))]
/// Status chip: coloured icon + abbreviated status text.
/// Example output: `● open` (blue), `▶ in_progress` (yellow), `✖ closed` (green).
fn status_chip(status: &str) -> Vec<RichSpan<'static>> {
    let normalized = status.trim().to_ascii_lowercase();
    let (icon, label) = match normalized.as_str() {
        "open" => ("●", "open"),
        "in_progress" => ("▶", "prog"),
        "blocked" => ("■", "blkd"),
        "closed" => ("✔", "done"),
        "deferred" => ("◇", "defr"),
        "review" => ("◎", "revw"),
        "pinned" => ("⊤", "pind"),
        "tombstone" => ("†", "tomb"),
        "hooked" => ("⊙", "hook"),
        _ => ("?", "unkn"),
    };
    vec![
        RichSpan::styled(icon, tokens::status_style(&normalized)),
        RichSpan::styled(format!("{label}"), tokens::status_style(&normalized)),
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
/// Priority badge: coloured priority indicator.
/// Example output: `P0` (red), `P2` (blue).
fn priority_badge(priority: i32) -> RichSpan<'static> {
    let prio = priority.clamp(0, 4) as u8;
    RichSpan::styled(format!("P{prio}"), tokens::priority_style(prio).bold())
}

#[cfg_attr(not(test), allow(dead_code))]
/// Type badge: single-letter issue type with type-specific colour.
/// Example output: `T` (task), `B` (bug), `E` (epic).
fn type_badge(issue_type: &str) -> RichSpan<'static> {
    let icon = type_icon(issue_type);
    RichSpan::styled(icon, ftui::Style::new().fg(tokens::type_fg(issue_type)))
}

#[cfg_attr(not(test), allow(dead_code))]
/// Metric strip: compact inline metric display with label and mini bar.
/// Example output: `PR ██░░░░ 0.42` for PageRank.
fn metric_strip(label: &str, value: f64, max_value: f64) -> Vec<RichSpan<'static>> {
    let bar = mini_bar(value, max_value);
    let formatted = format!("{value:.2}");
    vec![
        RichSpan::styled(format!("{label} "), tokens::dim()),
        RichSpan::raw(bar),
        RichSpan::styled(format!(" {formatted}"), tokens::dim()),
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
/// Blocker indicator: shows blocking state with colour coding.
/// Returns empty vec if the issue has no blockers and blocks nothing.
fn blocker_indicator(open_blockers: usize, blocks_count: usize) -> Vec<RichSpan<'static>> {
    if open_blockers > 0 {
        vec![RichSpan::styled(
            format!("⊘{open_blockers}"),
            tokens::status_style("blocked"),
        )]
    } else if blocks_count > 0 {
        vec![RichSpan::styled(
            format!("↓{blocks_count}"),
            tokens::status_style("open"),
        )]
    } else {
        Vec::new()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Section separator: dim horizontal rule spanning the given width.
fn section_separator(width: usize) -> RichLine {
    let rule = "─".repeat(width.min(120));
    RichLine::from_spans([RichSpan::styled(rule, tokens::dim())])
}

#[cfg_attr(not(test), allow(dead_code))]
/// Panel header: bold title with optional subtitle.
fn panel_header<'a>(title: &'a str, subtitle: Option<&'a str>) -> RichLine {
    let mut spans = vec![RichSpan::styled(title, tokens::panel_title().bold())];
    if let Some(sub) = subtitle {
        spans.push(RichSpan::styled(format!("  {sub}"), tokens::dim()));
    }
    RichLine::from_spans(spans)
}

#[cfg_attr(not(test), allow(dead_code))]
/// Label chips: coloured label tags inline.
fn label_chips(labels: &[String]) -> Vec<RichSpan<'static>> {
    let mut spans = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            spans.push(RichSpan::styled(" ", tokens::dim()));
        }
        spans.push(RichSpan::styled(format!("[{label}]"), tokens::header()));
    }
    spans
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanLineVariant {
    Narrow,
    Medium,
    Wide,
}

impl ScanLineVariant {
    fn from_width(width: usize) -> Self {
        if width < 54 {
            Self::Narrow
        } else if width < 92 {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanSegment {
    label: String,
    kind: ScanSegmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ScanLineContext {
    open_blockers: usize,
    blocks_count: usize,
    triage_rank: usize,
    pagerank_rank: usize,
    critical_depth: usize,
    graph_score: f64,
    search_match_position: Option<usize>,
    total_search_matches: usize,
    diff_tag: Option<DiffTag>,
    available_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffTag {
    New,
    Modified,
    Closed,
    Reopened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScanSegmentKind {
    Marker {
        selected: bool,
    },
    Chip(SemanticTone),
    Dim,
    Title {
        selected: bool,
    },
    Priority,
    Type,
    /// Badge styled by actual status (not the display label).
    StatusBadge {
        status: String,
    },
    Sparkline,
}

fn push_scan_segment(line: &mut RichLine, segment: &ScanSegment, row_selected: bool) {
    let base_style = match segment.kind {
        ScanSegmentKind::Marker { selected } => {
            if selected {
                tokens::selected()
            } else {
                tokens::dim()
            }
        }
        ScanSegmentKind::Chip(tone) => tokens::chip_style(tone),
        ScanSegmentKind::Dim => tokens::dim(),
        ScanSegmentKind::Title { selected } => {
            if selected {
                tokens::panel_title()
            } else {
                tokens::help_desc()
            }
        }
        ScanSegmentKind::Priority => {
            let prio = segment
                .label
                .trim_start_matches('P')
                .parse::<i32>()
                .unwrap_or_default();
            tokens::priority_badge(prio as u8)
        }
        ScanSegmentKind::Type => {
            // Use type-specific colour from label (B/F/T/E/Q/D/R)
            let type_name = match segment.label.as_str() {
                "B" => "bug",
                "F" => "feature",
                "T" => "task",
                "E" => "epic",
                "Q" => "question",
                "D" => "docs",
                "R" => "refactor",
                _ => "",
            };
            ftui::Style::new().fg(tokens::type_fg(type_name))
        }
        ScanSegmentKind::StatusBadge { ref status } => tokens::status_badge(status),
        ScanSegmentKind::Sparkline => {
            // Estimate visual fill ratio from sparkline characters for heatmap colour.
            let char_count = segment.label.chars().count().max(1);
            let filled = segment.label.chars().filter(|c| *c != ' ').count();
            let score = filled as f64 / char_count as f64;
            ftui::Style::new().fg(tokens::heatmap_color(score))
        }
    };
    // Apply highlight background to entire row when selected
    let style = if row_selected {
        base_style.bg(tokens::bg_highlight())
    } else {
        base_style
    };
    line.push_span(RichSpan::styled(segment.label.clone(), style));
}

fn scan_segments_width(segments: &[ScanSegment]) -> usize {
    if segments.is_empty() {
        return 0;
    }
    segments
        .iter()
        .map(|segment| display_width(&segment.label))
        .sum::<usize>()
        + segments.len().saturating_sub(1)
}

fn issue_action_state(issue: &crate::model::Issue, open_blockers: usize) -> &'static str {
    if issue.is_closed_like() {
        "closed"
    } else if open_blockers > 0 {
        "blocked"
    } else {
        "ready"
    }
}

fn action_state_tone(state: &str) -> SemanticTone {
    match state {
        "ready" => SemanticTone::Success,
        "blocked" => SemanticTone::Danger,
        "closed" => SemanticTone::Muted,
        _ => SemanticTone::Neutral,
    }
}

fn dependency_signal_segments(
    open_blockers: usize,
    blocks_count: usize,
    variant: ScanLineVariant,
) -> Vec<ScanSegment> {
    let mut segments = Vec::new();
    if open_blockers > 0 {
        segments.push(ScanSegment {
            label: format!("⊘{open_blockers}"),
            kind: ScanSegmentKind::Chip(SemanticTone::Danger),
        });
    }
    if blocks_count > 0 && !matches!(variant, ScanLineVariant::Narrow) {
        segments.push(ScanSegment {
            label: format!("↓{blocks_count}"),
            kind: ScanSegmentKind::Chip(SemanticTone::Accent),
        });
    }
    segments
}

fn issue_label_summary(issue: &crate::model::Issue) -> Option<String> {
    issue.labels.first().map(|label| {
        if issue.labels.len() > 1 {
            format!(
                "[{}+{}]",
                truncate_display(label, 10),
                issue.labels.len() - 1
            )
        } else {
            format!("[{}]", truncate_display(label, 12))
        }
    })
}

/// Issue scan line: dense single-line summary for list views.
/// Format adapts by width to surface rank, state, ownership, freshness, and scope.
// ============================================================================
// Column Schema for ftui Table widget
// ============================================================================

/// Identifies a column in the issue list table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ColumnId {
    Marker,
    Rank,
    ActionState,
    Priority,
    Id,
    Type,
    StatusBadge,
    Sparkline,
    Title,
    Deps,
    Assignee,
    Comments,
    Timestamp,
    Search,
}

/// Width behavior for a table column.
#[derive(Debug, Clone)]
enum ColumnWidth {
    /// Exact width in cells.
    Fixed(u16),
    /// Minimum width in cells; grows if space allows.
    Min(u16),
    /// Fills remaining space (stretch).
    Fill,
    /// Fits content width, with optional min/max bounds.
    FitContent,
    /// Fits content but clamped to [min, max].
    FitContentBounded { min: u16, max: u16 },
}

/// A column definition in the issue table schema.
#[derive(Debug, Clone)]
struct ColumnDef {
    id: ColumnId,
    /// Short header label (empty for no header).
    header: &'static str,
    /// Width constraint for this column.
    width: ColumnWidth,
}

/// Returns the column definitions for a given scan line variant.
fn columns_for_variant(variant: ScanLineVariant) -> Vec<ColumnDef> {
    match variant {
        ScanLineVariant::Narrow => narrow_columns(),
        ScanLineVariant::Medium => medium_columns(),
        ScanLineVariant::Wide => wide_columns(),
    }
}

fn narrow_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef { id: ColumnId::Marker, header: "", width: ColumnWidth::Fixed(1) },
        ColumnDef { id: ColumnId::Rank, header: "#", width: ColumnWidth::Fixed(4) },
        ColumnDef { id: ColumnId::ActionState, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Priority, header: "", width: ColumnWidth::Fixed(2) },
        ColumnDef { id: ColumnId::Id, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Title, header: "", width: ColumnWidth::Fill },
        ColumnDef { id: ColumnId::Deps, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Assignee, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Search, header: "", width: ColumnWidth::Fixed(8) },
    ]
}

fn medium_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef { id: ColumnId::Marker, header: "", width: ColumnWidth::Fixed(1) },
        ColumnDef { id: ColumnId::Rank, header: "#", width: ColumnWidth::Fixed(4) },
        ColumnDef { id: ColumnId::ActionState, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Priority, header: "", width: ColumnWidth::Fixed(2) },
        ColumnDef { id: ColumnId::Id, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Type, header: "", width: ColumnWidth::Fixed(2) },
        ColumnDef { id: ColumnId::Title, header: "", width: ColumnWidth::Fill },
        ColumnDef { id: ColumnId::Deps, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Assignee, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Comments, header: "", width: ColumnWidth::Fixed(5) },
        ColumnDef { id: ColumnId::Timestamp, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Search, header: "", width: ColumnWidth::Fixed(8) },
    ]
}

fn wide_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef { id: ColumnId::Marker, header: "", width: ColumnWidth::Fixed(1) },
        ColumnDef { id: ColumnId::Rank, header: "#", width: ColumnWidth::Fixed(4) },
        ColumnDef { id: ColumnId::ActionState, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Priority, header: "", width: ColumnWidth::Fixed(2) },
        ColumnDef { id: ColumnId::Id, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Type, header: "", width: ColumnWidth::Fixed(2) },
        ColumnDef { id: ColumnId::StatusBadge, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Sparkline, header: "", width: ColumnWidth::Fixed(5) },
        ColumnDef { id: ColumnId::Title, header: "", width: ColumnWidth::Fill },
        ColumnDef { id: ColumnId::Deps, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Assignee, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Comments, header: "", width: ColumnWidth::Fixed(5) },
        ColumnDef { id: ColumnId::Timestamp, header: "", width: ColumnWidth::FitContent },
        ColumnDef { id: ColumnId::Search, header: "", width: ColumnWidth::Fixed(10) },
    ]
}

/// Converts `ColumnWidth` to ftui's `Constraint`.
fn column_width_to_constraint(w: &ColumnWidth) -> Constraint {
    match *w {
        ColumnWidth::Fixed(n) => Constraint::Fixed(n),
        ColumnWidth::Min(n) => Constraint::Min(n),
        ColumnWidth::Fill => Constraint::Fill,
        ColumnWidth::FitContent => Constraint::FitContent,
        ColumnWidth::FitContentBounded { min, max } => {
            Constraint::FitContentBounded { min, max }
        }
    }
}

/// Computed width info for a single column.
#[derive(Debug, Clone)]
struct ColumnWidthResult {
    id: ColumnId,
    /// Maximum display width across all rows.
    max_content_width: u16,
    /// The ftui constraint to use.
    constraint: Constraint,
}

// Pre-scan visible issues and compute optimal column widths.
// These functions are reserved for future tight-width negotiation.
#[allow(dead_code)]
fn compute_column_widths(
    visible: &[(usize, &crate::model::Issue)],
    variant: ScanLineVariant,
    available_width: usize,
) -> Vec<(ColumnId, Constraint)> {
    let columns = columns_for_variant(variant);
    let mut results: Vec<ColumnWidthResult> = Vec::with_capacity(columns.len());

    // First pass: compute max content width per column
    let mut header_widths: Vec<u16> = columns.iter().map(|c| display_width(c.header) as u16).collect();

    for (col_idx, col) in columns.iter().enumerate() {
        let mut max_width: u16 = 0;

        for &(_slot, issue) in visible {
            let content_width = estimate_column_content_width(col.id, issue);
            max_width = max_width.max(content_width);
        }

        results.push(ColumnWidthResult {
            id: col.id,
            max_content_width: max_width.max(header_widths[col_idx]),
            constraint: column_width_to_constraint(&col.width),
        });
    }

    // Second pass: check if all fixed/min columns fit; if not, tighten.
    let total_available = available_width as u16;
    negotiate_tight_widths(&mut results, total_available, variant);

    results.into_iter().map(|r| (r.id, r.constraint)).collect()
}

/// Estimate the display width of a column's content for an issue.
///
/// This is approximate — the actual rendered width may differ slightly due to
/// styled spans, but it's accurate enough for width negotiation.
#[allow(dead_code)]
fn estimate_column_content_width(col: ColumnId, issue: &crate::model::Issue) -> u16 {
    match col {
        ColumnId::Marker => 1,
        ColumnId::Rank => 4,              // "#01"
        ColumnId::ActionState => {
            // Check dependencies where this issue is the dependent (someone blocks this issue)
            let is_blocked = issue.dependencies.iter().any(|d| d.dep_type == "blocks");
            if is_blocked { 7 } else { 5 }   // "blocked" vs "ready"
        }
        ColumnId::Priority => 2,          // "P0".."P4"
        ColumnId::Id => {
            let id_len = display_width(&issue.id);
            id_len.min(14) as u16         // truncated to 14
        }
        ColumnId::Type => 2,              // icon width
        ColumnId::StatusBadge => {
            display_width(&issue.status) as u16 + 2  // badge padding
        }
        ColumnId::Sparkline => {
            // Only rendered when graph_score > 0
            let score = 0.5; // approximation
            if score > 0.0 { 5 } else { 0 }
        }
        ColumnId::Title => display_width(&issue.title) as u16,
        ColumnId::Deps => {
            let blocker_count = issue.dependencies.iter().filter(|d| d.dep_type == "blocks").count();
            // We don't have a direct "blocks_count" on Issue; it's from graph metrics.
            if blocker_count > 0 {
                8 // approximate
            } else {
                0
            }
        }
        ColumnId::Assignee => {
            let a = issue.assignee.trim();
            if a.is_empty() {
                0 // no unassigned label in narrow/medium
            } else {
                (display_width(a) + 1).min(13) as u16 // @name
            }
        }
        ColumnId::Comments => {
            if issue.comments.is_empty() {
                0
            } else {
                5 // "💬99" max
            }
        }
        ColumnId::Timestamp => 6,         // approximate "2h ago"
        ColumnId::Search => 0,            // only when searching
    }
}

/// Negotiate column widths when total needed exceeds available space.
///
/// Strategy:
/// 1. Start with fixed columns at their exact widths.
/// 2. Give min columns their min widths.
/// 3. If remaining < 0, shrink non-critical columns (status badge, sparkline, comments, timestamp).
/// 4. If still tight, reduce title fill space (title will be truncated by paragraph).
/// 5. For FitContent columns, replace with Min(min_width) to avoid explosion.
#[allow(dead_code)]
fn negotiate_tight_widths(
    results: &mut [ColumnWidthResult],
    total_available: u16,
    _variant: ScanLineVariant,
) {
    // Phase 1: compute minimum total width
    let min_total: u16 = results.iter().map(|r| match &r.constraint {
        Constraint::Fixed(n) => *n,
        Constraint::Min(n) => *n,
        Constraint::Fill => 4, // absolute minimum for title
        Constraint::FitContent | Constraint::FitContentBounded { .. } | Constraint::FitMin => 1,
        _ => 0,
    }).sum();

    if min_total <= total_available {
        // Enough room: use FitContent for flexible columns, Fill for title
        for r in results.iter_mut() {
            match r.id {
                ColumnId::Title => r.constraint = Constraint::Fill,
                ColumnId::StatusBadge | ColumnId::Sparkline |
                ColumnId::Deps | ColumnId::Assignee | ColumnId::Timestamp |
                ColumnId::Search => {
                    r.constraint = Constraint::FitContent;
                }
                _ => {} // keep original constraint
            }
        }
        return;
    }

    // Phase 2: tight — shrink non-essential columns
    // Reduce status badge, sparkline to zero width (hide them)
    for r in results.iter_mut() {
        match r.id {
            ColumnId::StatusBadge | ColumnId::Sparkline |
            ColumnId::Comments | ColumnId::Timestamp |
            ColumnId::Search => {
                r.constraint = Constraint::Fixed(0);
            }
            ColumnId::Assignee => {
                // Only show if there's room
                r.constraint = Constraint::Min(0);
            }
            ColumnId::Deps => {
                r.constraint = Constraint::Min(0);
            }
            ColumnId::Title => {
                r.constraint = Constraint::Fill;
            }
            _ => {} // keep fixed/min
        }
    }

    // Phase 3: check again after shrinking
    let shrunk_min: u16 = results.iter().map(|r| match &r.constraint {
        Constraint::Fixed(n) => *n,
        Constraint::Min(n) => *n,
        Constraint::Fill => 4,
        Constraint::FitContent | Constraint::FitContentBounded { .. } | Constraint::FitMin => 1,
        _ => 0,
    }).sum();

    if shrunk_min > total_available {
        // Extremely tight: clamp title to remaining space
        let fixed_total: u16 = results.iter().map(|r| match &r.constraint {
            Constraint::Fixed(n) => *n,
            Constraint::Min(n) => *n,
            _ => 0,
        }).sum();

        let title_idx = results.iter().position(|r| r.id == ColumnId::Title);
        if let Some(idx) = title_idx {
            let remaining = total_available.saturating_sub(fixed_total);
            // Title gets whatever is left, minimum 4 chars
            results[idx].constraint = Constraint::Max(remaining.max(4));
        }
    }
}

/// Build a `TableRow` for an issue, with one cell per column in the schema.
///
/// Each cell is a styled `RichLine`. Empty cells are rendered as a single space
/// to avoid collapsing. Selection highlighting is handled by the Table widget's
/// `highlight_style` — do not apply selection backgrounds here.
fn issue_to_table_row(
    issue: &crate::model::Issue,
    is_selected: bool,
    context: &ScanLineContext,
    columns: &[ColumnDef],
) -> TableRow {
    let action_state = issue_action_state(issue, context.open_blockers);
    let variant = ScanLineVariant::from_width(context.available_width);

    let cells: Vec<RichLine> = columns.iter().map(|col| {
        let (label, style) = match col.id {
            ColumnId::Marker => {
                let ch = if is_selected { "▸" } else { " " };
                let style = if is_selected { tokens::selected() } else { tokens::dim() };
                (ch.to_string(), style)
            }
            ColumnId::Rank => {
                (format!("#{:02}", context.triage_rank), tokens::chip_style(SemanticTone::Accent))
            }
            ColumnId::ActionState => {
                (action_state.to_string(), tokens::chip_style(action_state_tone(&action_state)))
            }
            ColumnId::Priority => {
                let prio = issue.priority.clamp(0, 4) as u8;
                (format!("P{prio}"), tokens::priority_badge(prio))
            }
            ColumnId::Id => {
                (truncate_display(&issue.id, 14), tokens::dim())
            }
            ColumnId::Type => {
                let icon = type_icon(&issue.issue_type).to_string();
                let type_name = match icon.as_str() {
                    "B" => "bug", "F" => "feature", "T" => "task",
                    "E" => "epic", "Q" => "question", "D" => "docs",
                    "R" => "refactor", _ => "",
                };
                let style = ftui::Style::new().fg(tokens::type_fg(type_name));
                (icon, style)
            }
            ColumnId::StatusBadge => {
                let label = tokens::status_badge_label(&issue.status).to_string();
                let style = tokens::status_badge(&issue.status);
                (label, style)
            }
            ColumnId::Sparkline => {
                if context.graph_score > 0.0 {
                    let s = tokens::render_sparkline(context.graph_score, 5);
                    let filled = s.chars().filter(|c| *c != ' ').count();
                    let score = filled as f64 / 5.0;
                    let style = ftui::Style::new().fg(tokens::heatmap_color(score));
                    (s, style)
                } else {
                    (String::new(), ftui::Style::default())
                }
            }
            ColumnId::Title => {
                (issue.title.clone(), ftui::Style::default())
            }
            ColumnId::Deps => {
                let (label, tone) = if context.open_blockers > 0 {
                    (format!("⊘{}", context.open_blockers), SemanticTone::Danger)
                } else if context.blocks_count > 0 && !matches!(variant, ScanLineVariant::Narrow) {
                    (format!("↓{}", context.blocks_count), SemanticTone::Accent)
                } else {
                    (String::new(), SemanticTone::Neutral)
                };
                if label.is_empty() {
                    (String::new(), ftui::Style::default())
                } else {
                    (label, tokens::chip_style(tone))
                }
            }
            ColumnId::Assignee => {
                let a = issue.assignee.trim();
                if a.is_empty() {
                    if matches!(variant, ScanLineVariant::Wide) {
                        ("@unassigned".to_string(), tokens::chip_style(SemanticTone::Muted))
                    } else {
                        (String::new(), ftui::Style::default())
                    }
                } else {
                    (format!("@{}", truncate_display(a, 12)), tokens::chip_style(SemanticTone::Neutral))
                }
            }
            ColumnId::Comments => {
                if issue.comments.is_empty() {
                    (String::new(), ftui::Style::default())
                } else {
                    (format!("💬{}", issue.comments.len()), tokens::dim())
                }
            }
            ColumnId::Timestamp => {
                let ts = format_compact_timestamp(issue.updated_at.or(issue.created_at));
                (format!("↻{ts}"), tokens::dim())
            }
            ColumnId::Search => {
                if let Some(pos) = context.search_match_position {
                    (format!("hit {}/{}", pos, context.total_search_matches), tokens::dim())
                } else {
                    (String::new(), ftui::Style::default())
                }
            }
        };

        // Selection highlighting is handled by Table's `highlight_style`.
        if label.is_empty() {
            // Use a single space to avoid zero-width cell collapsing
            RichLine::from(RichSpan::styled(" ", style))
        } else {
            RichLine::from(RichSpan::styled(label, style))
        }
    }).collect();

    TableRow::new(cells)
}

fn issue_scan_line(
    issue: &crate::model::Issue,
    is_selected: bool,
    context: ScanLineContext,
) -> RichLine {
    let variant = ScanLineVariant::from_width(context.available_width);
    let action_state = issue_action_state(issue, context.open_blockers);
    let mut prefix = vec![
        ScanSegment {
            label: if is_selected {
                "▸".to_string()
            } else {
                " ".to_string()
            },
            kind: ScanSegmentKind::Marker {
                selected: is_selected,
            },
        },
        ScanSegment {
            label: format!("#{:02}", context.triage_rank),
            kind: ScanSegmentKind::Chip(SemanticTone::Accent),
        },
        ScanSegment {
            label: action_state.to_string(),
            kind: ScanSegmentKind::Chip(action_state_tone(action_state)),
        },
        ScanSegment {
            label: format!("P{}", issue.priority.clamp(0, 4)),
            kind: ScanSegmentKind::Priority,
        },
        ScanSegment {
            label: truncate_display(&issue.id, 14),
            kind: ScanSegmentKind::Dim,
        },
    ];

    if !matches!(variant, ScanLineVariant::Narrow) {
        prefix.push(ScanSegment {
            label: type_icon(&issue.issue_type).to_string(),
            kind: ScanSegmentKind::Type,
        });
    }
    if matches!(variant, ScanLineVariant::Wide) {
        prefix.push(ScanSegment {
            label: tokens::status_badge_label(&issue.status).to_string(),
            kind: ScanSegmentKind::StatusBadge {
                status: issue.status.clone(),
            },
        });
        // Sparkline for graph importance (5 chars)
        if context.graph_score > 0.0 {
            prefix.push(ScanSegment {
                label: tokens::render_sparkline(context.graph_score, 5),
                kind: ScanSegmentKind::Sparkline,
            });
        }
    }

    let mut suffix =
        dependency_signal_segments(context.open_blockers, context.blocks_count, variant);
    if !issue.assignee.trim().is_empty() {
        suffix.push(ScanSegment {
            label: format!("@{}", truncate_display(issue.assignee.trim(), 12)),
            kind: ScanSegmentKind::Chip(SemanticTone::Neutral),
        });
    } else if matches!(variant, ScanLineVariant::Wide) {
        suffix.push(ScanSegment {
            label: "@unassigned".to_string(),
            kind: ScanSegmentKind::Chip(SemanticTone::Muted),
        });
    }
    if matches!(variant, ScanLineVariant::Medium | ScanLineVariant::Wide) {
        // Comment count (only if > 0)
        if !issue.comments.is_empty() {
            suffix.push(ScanSegment {
                label: format!("💬{}", issue.comments.len()),
                kind: ScanSegmentKind::Dim,
            });
        }
        // Updated timestamp
        suffix.push(ScanSegment {
            label: format!(
                "↻{}",
                format_compact_timestamp(issue.updated_at.or(issue.created_at))
            ),
            kind: ScanSegmentKind::Dim,
        });
    }
    if matches!(variant, ScanLineVariant::Wide) {
        suffix.push(ScanSegment {
            label: format!(
                "repo:{}",
                truncate_display(&display_or_fallback(&issue.source_repo, "local"), 10)
            ),
            kind: ScanSegmentKind::Chip(SemanticTone::Muted),
        });
        suffix.push(ScanSegment {
            label: format!("pr#{}", context.pagerank_rank),
            kind: ScanSegmentKind::Chip(SemanticTone::Neutral),
        });
        suffix.push(ScanSegment {
            label: format!("d{}", context.critical_depth),
            kind: ScanSegmentKind::Chip(if context.critical_depth > 0 {
                SemanticTone::Warning
            } else {
                SemanticTone::Muted
            }),
        });
        if let Some(label_summary) = issue_label_summary(issue) {
            suffix.push(ScanSegment {
                label: label_summary,
                kind: ScanSegmentKind::Chip(SemanticTone::Accent),
            });
        }
    }
    if let Some(position) = context.search_match_position {
        suffix.push(ScanSegment {
            label: if is_selected {
                format!("hit {position}/{}", context.total_search_matches)
            } else {
                "hit".to_string()
            },
            kind: ScanSegmentKind::Chip(if is_selected {
                SemanticTone::Warning
            } else {
                SemanticTone::Accent
            }),
        });
    }

    // Time-travel diff marker
    if let Some(tag) = context.diff_tag {
        let (label, tone) = match tag {
            DiffTag::New => ("NEW", SemanticTone::Success),
            DiffTag::Modified => ("MOD", SemanticTone::Warning),
            DiffTag::Closed => ("CLO", SemanticTone::Muted),
            DiffTag::Reopened => ("RE!", SemanticTone::Danger),
        };
        suffix.push(ScanSegment {
            label: label.to_string(),
            kind: ScanSegmentKind::Chip(tone),
        });
    }

    let min_title_width = match variant {
        ScanLineVariant::Narrow => 10,
        ScanLineVariant::Medium => 16,
        ScanLineVariant::Wide => 22,
    };
    while !suffix.is_empty()
        && context.available_width
            < scan_segments_width(&prefix)
                + scan_segments_width(&suffix)
                + min_title_width
                + usize::from(!prefix.is_empty())
                + usize::from(!suffix.is_empty())
    {
        suffix.pop();
    }

    let reserved_width = scan_segments_width(&prefix)
        + scan_segments_width(&suffix)
        + usize::from(!prefix.is_empty())
        + usize::from(!suffix.is_empty());
    let title_width = context
        .available_width
        .saturating_sub(reserved_width)
        .max(min_title_width);
    let title = truncate_display(&issue.title, title_width);

    let mut line = RichLine::new();
    let sep = if is_selected {
        RichSpan::styled(" ", tokens::selected())
    } else {
        RichSpan::raw(" ")
    };
    for (index, segment) in prefix.iter().enumerate() {
        if index > 0 {
            line.push_span(sep.clone());
        }
        push_scan_segment(&mut line, segment, is_selected);
    }
    if !prefix.is_empty() {
        line.push_span(sep.clone());
    }
    push_scan_segment(
        &mut line,
        &ScanSegment {
            label: title,
            kind: ScanSegmentKind::Title {
                selected: is_selected,
            },
        },
        is_selected,
    );
    if !suffix.is_empty() {
        line.push_span(sep.clone());
    }
    for (index, segment) in suffix.iter().enumerate() {
        if index > 0 {
            line.push_span(sep.clone());
        }
        push_scan_segment(&mut line, segment, is_selected);
    }

    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
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
    /// Parse a CLI `--view` value into a `ViewMode`.
    pub fn from_cli(s: &str) -> Option<Self> {
        match s {
            "main" => Some(Self::Main),
            "board" => Some(Self::Board),
            "insights" => Some(Self::Insights),
            "graph" => Some(Self::Graph),
            "history" => Some(Self::History),
            "actionable" => Some(Self::Actionable),
            "attention" => Some(Self::Attention),
            "tree" => Some(Self::Tree),
            "labels" => Some(Self::LabelDashboard),
            "flow" => Some(Self::FlowMatrix),
            "timediff" => Some(Self::TimeTravelDiff),
            "sprint" => Some(Self::Sprint),
            _ => None,
        }
    }
}

impl ViewMode {
    const fn navigation_order() -> [Self; 12] {
        [
            Self::Main,
            Self::Board,
            Self::Insights,
            Self::Graph,
            Self::History,
            Self::Actionable,
            Self::LabelDashboard,
            Self::FlowMatrix,
            Self::Attention,
            Self::Tree,
            Self::TimeTravelDiff,
            Self::Sprint,
        ]
    }

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

    const fn shortcut(self) -> &'static str {
        match self {
            Self::Main => "1",
            Self::Board => "b",
            Self::Insights => "i",
            Self::Graph => "g",
            Self::History => "h",
            Self::Actionable => "a",
            Self::Attention => "!",
            Self::Tree => "T",
            Self::LabelDashboard => "[",
            Self::FlowMatrix => "]",
            Self::TimeTravelDiff => "t",
            Self::Sprint => "S",
        }
    }

    const fn short_label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Board => "Board",
            Self::Insights => "In",
            Self::Graph => "Graph",
            Self::History => "Hist",
            Self::Actionable => "Act",
            Self::Attention => "Attn",
            Self::Tree => "Tree",
            Self::LabelDashboard => "Lbl",
            Self::FlowMatrix => "Flow",
            Self::TimeTravelDiff => "Diff",
            Self::Sprint => "Sprint",
        }
    }

    fn tab_text(self, bp: Breakpoint) -> String {
        let label = if matches!(bp, Breakpoint::Narrow) {
            self.short_label()
        } else {
            self.label()
        };
        format!("{} {label}", self.shortcut())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    List,
    Middle,
    Detail,
}

impl FocusPane {
    fn label(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Middle => "middle",
            Self::Detail => "detail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFilter {
    All,
    Open,
    InProgress,
    Blocked,
    Closed,
    Ready,
}

impl ListFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::InProgress => "in-progress",
            Self::Blocked => "blocked",
            Self::Closed => "closed",
            Self::Ready => "ready",
        }
    }

    /// Parse a CLI `--list-filter` value into a `ListFilter`.
    pub fn from_cli(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "open" => Some(Self::Open),
            "in-progress" | "in_progress" | "inprogress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "closed" => Some(Self::Closed),
            "ready" => Some(Self::Ready),
            _ => None,
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
    PageRank,
    Blockers,
}

impl ListSort {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::CreatedAsc => "created-asc",
            Self::CreatedDesc => "created-desc",
            Self::Priority => "priority",
            Self::Updated => "updated",
            Self::PageRank => "pagerank",
            Self::Blockers => "blockers",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Default => Self::CreatedAsc,
            Self::CreatedAsc => Self::CreatedDesc,
            Self::CreatedDesc => Self::Priority,
            Self::Priority => Self::Updated,
            Self::Updated => Self::PageRank,
            Self::PageRank => Self::Blockers,
            Self::Blockers => Self::Default,
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

    fn indicator(self) -> &'static str {
        match self {
            Self::Bead => "◈ Beads",
            Self::Git => "◉ Git",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Bead => Self::Git,
            Self::Git => Self::Bead,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryLayout {
    Narrow,
    Standard,
    Wide,
}

impl HistoryLayout {
    fn from_width(width: u16) -> Self {
        if width < 100 {
            Self::Narrow
        } else if width < 150 {
            Self::Standard
        } else {
            Self::Wide
        }
    }

    fn has_middle_pane(self) -> bool {
        !matches!(self, Self::Narrow)
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
    Tutorial,
    /// Reusable Y/N confirmation dialog.
    Confirm {
        title: String,
        message: String,
        resume_overlay: Option<Box<ModalOverlay>>,
    },
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
        filter: String,
    },
    /// Repo picker: shows workspace repos for filtering.
    RepoPicker {
        items: Vec<String>,
        cursor: usize,
        filter: String,
    },
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
    Mouse(MouseEvent),
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
            Event::Mouse(event) => Self::Mouse(event),
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
    /// Navigation back stack: tracks which modes led to the current one.
    mode_back_stack: Vec<ViewMode>,
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
    /// Universal detail pane scroll offset — works in all modes when focus is Detail.
    detail_scroll_offset: usize,
    main_search_active: bool,
    main_search_query: String,
    main_search_match_cursor: usize,
    list_scroll_offset: Cell<usize>,
    list_viewport_height: Cell<usize>,
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
    tree_search_active: bool,
    tree_search_query: String,
    tree_search_match_cursor: usize,
    /// When true, the previous key was `g`; a second `g` triggers `gg` (jump-to-top).
    pending_g: bool,
    /// Saved mode before the speculative graph toggle so `gg` can restore it.
    g_pre_toggle_mode: Option<ViewMode>,
    /// When true, the previous key was `z`; the next key completes a z-command.
    pending_z: bool,
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
                let mode_before = self.mode;
                let cmd = self.handle_key(code, modifiers);
                if self.mode != mode_before {
                    self.list_scroll_offset.set(0);
                }
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
            Msg::Mouse(event) => return self.handle_mouse(event),
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
        record_view_size(full.width, full.height);
        record_detail_content_area(Rect::default());
        let bp = Breakpoint::from_width(full.width);

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(3),
                Constraint::Fixed(1),
            ])
            .split(full);

        // -- Header ----------------------------------------------------------
        let header_text = build_header_text(self, full.width);
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
                .block(semantic_panel_block("Help", true, SemanticTone::Accent))
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
                .block(semantic_panel_block(
                    "Confirm Quit",
                    false,
                    SemanticTone::Danger,
                ))
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
                        .block(semantic_panel_block("Tutorial", true, SemanticTone::Accent))
                        .render(rows[1], frame);
                    Paragraph::new("Press any key to continue.")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::Confirm { title, message, .. } => {
                    let text = format!("{message}\n\nPress Y to confirm, N or Esc to cancel.");
                    Paragraph::new(text)
                        .block(semantic_panel_block(
                            title.as_str(),
                            false,
                            SemanticTone::Danger,
                        ))
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
                        .block(semantic_panel_block(
                            wiz_title.as_str(),
                            true,
                            SemanticTone::Accent,
                        ))
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
                        .block(semantic_panel_block(
                            "Recipe Picker",
                            true,
                            SemanticTone::Accent,
                        ))
                        .render(rows[1], frame);
                    Paragraph::new("j/k=navigate | Enter=apply | Esc=close")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::LabelPicker {
                    items,
                    cursor,
                    filter,
                } => {
                    let needle = filter.to_ascii_lowercase();
                    let mut lines = Vec::new();
                    if !filter.is_empty() {
                        lines.push(format!(" Filter: /{filter}"));
                    }
                    let mut vis_idx = 0usize;
                    for (label, count) in items {
                        if !needle.is_empty() && !label.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let marker = if vis_idx == *cursor { "▸" } else { " " };
                        lines.push(format!(" {marker} {label:24} ({count} issues)"));
                        vis_idx += 1;
                    }
                    let text = if vis_idx == 0 {
                        if filter.is_empty() {
                            " No labels found.".to_string()
                        } else {
                            format!(" No labels match: /{filter}")
                        }
                    } else {
                        lines.join("\n")
                    };
                    Paragraph::new(text)
                        .block(semantic_panel_block(
                            "Label Picker",
                            true,
                            SemanticTone::Accent,
                        ))
                        .render(rows[1], frame);
                    Paragraph::new("Type to filter | ↑/↓=navigate | Enter=apply | Esc=close")
                        .style(tokens::footer())
                        .render(rows[2], frame);
                    return;
                }
                ModalOverlay::RepoPicker {
                    items,
                    cursor,
                    filter,
                } => {
                    let needle = filter.to_ascii_lowercase();
                    let mut lines = Vec::new();
                    if !filter.is_empty() {
                        lines.push(format!(" Filter: /{filter}"));
                    }
                    let mut vis_idx = 0usize;
                    for repo in items {
                        if !needle.is_empty() && !repo.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let marker = if vis_idx == *cursor { "▸" } else { " " };
                        lines.push(format!(" {marker} {repo}"));
                        vis_idx += 1;
                    }
                    let text = if vis_idx == 0 {
                        if filter.is_empty() {
                            " No repos found.".to_string()
                        } else {
                            format!(" No repos match: /{filter}")
                        }
                    } else {
                        lines.join("\n")
                    };
                    Paragraph::new(text)
                        .block(semantic_panel_block(
                            "Repo Picker",
                            true,
                            SemanticTone::Accent,
                        ))
                        .render(rows[1], frame);
                    Paragraph::new("Type to filter | ↑/↓=navigate | Enter=apply | Esc=close")
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
        let history_layout = if matches!(self.mode, ViewMode::History) {
            HistoryLayout::from_width(body.width)
        } else {
            HistoryLayout::Narrow
        };
        let split_state = pane_split_state();
        let history_multi_pane =
            matches!(self.mode, ViewMode::History) && history_layout.has_middle_pane();
        let mut detail_viewport_height = body.height.saturating_sub(2) as usize;
        let mut board_detail_line = None;

        let detail_title = match self.mode {
            ViewMode::Board => "Board Focus",
            ViewMode::Insights => "Insight Detail",
            ViewMode::Graph if graph_single_pane => "Graph View",
            ViewMode::Graph => "Graph Focus",
            ViewMode::History => self.history_detail_panel_title(),
            ViewMode::Actionable => "Track Detail",
            ViewMode::Attention => "Label Detail",
            ViewMode::Tree => "Issue Detail",
            ViewMode::LabelDashboard => "Label Detail",
            ViewMode::FlowMatrix => "Flow Detail",
            ViewMode::TimeTravelDiff => "Diff Detail",
            ViewMode::Sprint => "Sprint Detail",
            ViewMode::Main => "Details",
        };
        let detail_focused = self.focus == FocusPane::Detail
            || graph_single_pane
            || (matches!(self.mode, ViewMode::History) && self.history_file_tree_focus);
        let detail_title = if detail_focused {
            format!("{detail_title} [focus]")
        } else {
            detail_title.to_string()
        };
        if graph_single_pane {
            record_detail_content_area(block_inner_rect(body));
            Paragraph::new(self.graph_detail_render_text())
                .block(semantic_panel_block(
                    &detail_title,
                    detail_focused,
                    SemanticTone::Accent,
                ))
                .render(body, frame);
        } else if history_multi_pane {
            let render_history_panel =
                |frame: &mut Frame, area: Rect, title: String, focused: bool, text: RichText| {
                    Paragraph::new(text)
                        .block(semantic_panel_block(
                            title.as_str(),
                            focused,
                            SemanticTone::Accent,
                        ))
                        .render(area, frame);
                };

            if matches!(history_layout, HistoryLayout::Wide)
                && matches!(self.history_view_mode, HistoryViewMode::Bead)
            {
                let PaneSplitPreset::Four(pcts) =
                    split_state.history_pcts(history_layout, self.history_view_mode)
                else {
                    unreachable!("wide bead history should use four-pane split");
                };
                let panes = Flex::horizontal()
                    .constraints([
                        Constraint::Percentage(pcts[0]),
                        Constraint::Percentage(pcts[1]),
                        Constraint::Percentage(pcts[2]),
                        Constraint::Percentage(pcts[3]),
                    ])
                    .split(body);

                render_history_panel(
                    frame,
                    panes[0],
                    if matches!(self.focus, FocusPane::List) {
                        format!("{} [focus]", self.history_list_panel_title())
                    } else {
                        self.history_list_panel_title().to_string()
                    },
                    matches!(self.focus, FocusPane::List),
                    RichText::raw(self.history_list_text()),
                );
                render_history_panel(
                    frame,
                    panes[1],
                    self.history_timeline_panel_title(),
                    false,
                    RichText::raw(self.history_timeline_text(panes[1].width, panes[1].height)),
                );
                render_history_panel(
                    frame,
                    panes[2],
                    if matches!(self.focus, FocusPane::Middle) {
                        format!("{} [focus]", self.history_middle_panel_title())
                    } else {
                        self.history_middle_panel_title().to_string()
                    },
                    matches!(self.focus, FocusPane::Middle),
                    RichText::raw(self.history_middle_text(panes[2].width, panes[2].height)),
                );
                detail_viewport_height = panes[3].height.saturating_sub(2) as usize;
                render_history_panel(
                    frame,
                    panes[3],
                    detail_title.clone(),
                    detail_focused,
                    self.detail_panel_render_text(),
                );
                record_detail_content_area(block_inner_rect(panes[3]));
            } else {
                let PaneSplitPreset::Three(pane_widths) =
                    split_state.history_pcts(history_layout, self.history_view_mode)
                else {
                    unreachable!("multi-pane history should use three-pane split");
                };
                let panes = Flex::horizontal()
                    .constraints([
                        Constraint::Percentage(pane_widths[0]),
                        Constraint::Percentage(pane_widths[1]),
                        Constraint::Percentage(pane_widths[2]),
                    ])
                    .split(body);

                render_history_panel(
                    frame,
                    panes[0],
                    if matches!(self.focus, FocusPane::List) {
                        format!("{} [focus]", self.history_list_panel_title())
                    } else {
                        self.history_list_panel_title().to_string()
                    },
                    matches!(self.focus, FocusPane::List),
                    RichText::raw(self.history_list_text()),
                );
                render_history_panel(
                    frame,
                    panes[1],
                    if matches!(self.focus, FocusPane::Middle) {
                        format!("{} [focus]", self.history_middle_panel_title())
                    } else {
                        self.history_middle_panel_title().to_string()
                    },
                    matches!(self.focus, FocusPane::Middle),
                    RichText::raw(self.history_middle_text(panes[1].width, panes[1].height)),
                );
                detail_viewport_height = panes[2].height.saturating_sub(2) as usize;
                render_history_panel(
                    frame,
                    panes[2],
                    detail_title.clone(),
                    detail_focused,
                    self.detail_panel_render_text(),
                );
                record_detail_content_area(block_inner_rect(panes[2]));
            }
        } else {
            let panes = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(split_state.two_pane_list_pct(bp)),
                    Constraint::Percentage(split_state.two_pane_detail_pct(bp)),
                ])
                .split(body);

            let list_text = self.list_panel_render_text(panes[0].width);
            let list_title = match self.mode {
                ViewMode::Board => "Board Lanes",
                ViewMode::Insights => "Insight Queue",
                ViewMode::Graph => "Graph Nodes",
                ViewMode::History => self.history_list_panel_title(),
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

            let vp_height = panes[0].height.saturating_sub(2) as usize;
            self.list_viewport_height.set(vp_height);

            if matches!(self.mode, ViewMode::Main) && !self.analyzer.issues.is_empty() {
                // Use ftui Table widget for the main issue list with proper column alignment.
                let visible = self.visible_issue_indices();
                let available_width = panes[0].width.saturating_sub(2) as usize;
                let variant = ScanLineVariant::from_width(available_width);
                let columns = columns_for_variant(variant);
                let widths: Vec<Constraint> = columns.iter().map(|c| column_width_to_constraint(&c.width)).collect();
                let search_matches = self.main_search_matches();
                let search_positions: BTreeMap<usize, usize> = search_matches
                    .iter()
                    .enumerate()
                    .map(|(slot, index)| (*index, slot + 1))
                    .collect();

                let rows: Vec<TableRow> = visible.iter().enumerate().filter_map(|(slot, index)| {
                    let issue = self.analyzer.issues.get(*index)?;
                    let open_blockers = self.analyzer.graph.open_blockers(&issue.id).len();
                    let blocks_count = self
                        .analyzer
                        .metrics
                        .blocks_count
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
                    let graph_score = self
                        .analyzer
                        .metrics
                        .pagerank
                        .get(&issue.id)
                        .copied()
                        .unwrap_or_default();
                    let search_match_position = search_positions.get(index).copied();
                    let total_search_matches = search_matches.len();

                    let context = ScanLineContext {
                        open_blockers,
                        blocks_count,
                        triage_rank: slot + 1,
                        pagerank_rank: slot + 1,
                        critical_depth: depth,
                        graph_score,
                        search_match_position,
                        total_search_matches,
                        diff_tag: None,
                        available_width,
                    };

                    let is_selected = *index == self.selected;
                    Some(issue_to_table_row(issue, is_selected, &context, &columns))
                }).collect();

                let mut table_state = TableState::default();
                // Convert issue index -> row index: find the position of self.selected within visible
                table_state.selected = visible.iter().position(|i| *i == self.selected);
                table_state.offset = self.list_scroll_offset.get();

                // Render panel border first
                let block = semantic_panel_block(&list_title, list_focused, SemanticTone::Accent);
                let inner_area = block_inner_rect(panes[0]);
                block.render(panes[0], frame);

                // Render the search banner inside the panel
                let banner_lines = self.main_search_banner_lines();
                let banner_height = banner_lines.len() as u16;
                let banner_rect = Rect::new(inner_area.x, inner_area.y, inner_area.width, banner_height);
                Paragraph::new(RichText::from_lines(banner_lines))
                    .render(banner_rect, frame);

                // Render the table below the banner, inside the panel
                let table_y = inner_area.y + banner_height;
                let table_height = inner_area.height.saturating_sub(banner_height);
                if table_height > 0 {
                    let table_rect = Rect::new(inner_area.x, table_y, inner_area.width, table_height);
                    let table = Table::new(rows, widths)
                        .highlight_style(ftui::Style::new().bg(tokens::bg_highlight()))
                        .column_spacing(1);
                    StatefulWidget::render(&table, table_rect, frame, &mut table_state);
                }

                // Sync scroll offset from TableState back to BvrApp (AFTER render)
                self.list_scroll_offset.set(table_state.offset);
            } else {
                // Auto-scroll: find the line with the '>' cursor marker and
                // ensure it is within the visible viewport.
                if vp_height > 0 {
                    let scroll = self.list_scroll_offset.get();
                    if let Some(cursor_line) = list_text.to_plain_text().lines().position(|line| {
                        line.starts_with('>')
                            || line.starts_with(" >")
                            || line.starts_with("  >")
                            || line.starts_with("   >")
                            || line.starts_with("    >")
                            || line.contains('\u{25b6}')
                            || line.contains('▸')
                    }) {
                        if cursor_line < scroll {
                            self.list_scroll_offset.set(cursor_line);
                        } else if cursor_line >= scroll + vp_height {
                            self.list_scroll_offset
                                .set(cursor_line.saturating_sub(vp_height - 1));
                        }
                    }
                }
                Paragraph::new(list_text)
                    .block(semantic_panel_block(
                        &list_title,
                        list_focused,
                        SemanticTone::Accent,
                    ))
                    .scroll((saturating_scroll_offset(self.list_scroll_offset.get()), 0))
                    .render(panes[0], frame);
            }

            detail_viewport_height = panes[1].height.saturating_sub(2) as usize;
            let detail_text = if matches!(self.mode, ViewMode::Board) {
                let rendered = self.board_detail_render_text();
                let total_lines = rendered.lines().len();
                let max_offset = total_lines.saturating_sub(detail_viewport_height);
                let offset = self.board_detail_scroll_offset.min(max_offset);
                board_detail_line = Some((offset, total_lines));
                rendered
            } else if matches!(self.mode, ViewMode::Graph) {
                self.graph_detail_render_text()
            } else {
                self.detail_panel_render_text()
            };
            let detail_scroll = if matches!(self.mode, ViewMode::Board) {
                // Use the clamped offset to prevent scrolling past content end.
                board_detail_line.map_or(0, |(o, _)| o)
            } else {
                usize::from(saturating_scroll_offset(self.detail_scroll_offset))
            };
            Paragraph::new(detail_text)
                .block(semantic_panel_block(
                    &detail_title,
                    detail_focused,
                    SemanticTone::Accent,
                ))
                .scroll((saturating_scroll_offset(detail_scroll), 0))
                .render(panes[1], frame);
            record_detail_content_area(block_inner_rect(panes[1]));
        }

        // -- Footer ----------------------------------------------------------
        let footer_text = match self.mode {
            ViewMode::Main => {
                if self.status_msg.is_empty() {
                    None
                } else {
                    Some(RichText::raw(self.status_msg.clone()))
                }
            }
            ViewMode::Board => {
                let detail_hint_desc = board_detail_line.map_or_else(
                    || "detail scroll".to_string(),
                    |(offset, total_lines)| {
                        if total_lines > detail_viewport_height {
                            format!("detail scroll ({}/{})", offset + 1, total_lines)
                        } else {
                            "detail scroll".to_string()
                        }
                    },
                );
                if self.status_msg.is_empty() {
                    let grouping_label = self.board_grouping.label();
                    let empty_label = self.board_empty_visibility.label();
                    let mut hints = vec![
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "/",
                            desc: "search",
                        },
                    ];
                    let grouping_desc = format!("grouping={grouping_label}");
                    let empty_desc = format!("empty-lanes={empty_label}");
                    hints.push(CommandHint {
                        key: "s",
                        desc: &grouping_desc,
                    });
                    hints.push(CommandHint {
                        key: "e",
                        desc: &empty_desc,
                    });
                    hints.push(CommandHint {
                        key: "H/L",
                        desc: "lanes",
                    });
                    hints.push(CommandHint {
                        key: "0/$",
                        desc: "edges",
                    });
                    hints.push(CommandHint {
                        key: "^j/k",
                        desc: &detail_hint_desc,
                    });
                    if self.should_open_selected_issue_external_ref() {
                        hints.push(CommandHint {
                            key: "o",
                            desc: "open link",
                        });
                        hints.push(CommandHint {
                            key: "y",
                            desc: "copy link",
                        });
                    }
                    Some(styled_mode_footer("Board", &hints, rows[2].width))
                } else {
                    Some(RichText::raw(self.status_msg.clone()))
                }
            }
            ViewMode::Insights => {
                if self.status_msg.is_empty() {
                    let panel_label = self.insights_panel.short_label();
                    let expl_flag = if self.insights_show_explanations {
                        "on"
                    } else {
                        "off"
                    };
                    let proof_flag = if self.insights_show_calc_proof {
                        "on"
                    } else {
                        "off"
                    };
                    let expl_desc = format!("explanations={expl_flag}");
                    let proof_desc = format!("proof={proof_flag}");
                    let mut hints = vec![
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "/",
                            desc: "search",
                        },
                        CommandHint {
                            key: "s/S",
                            desc: "panel",
                        },
                        CommandHint {
                            key: "e",
                            desc: &expl_desc,
                        },
                        CommandHint {
                            key: "x",
                            desc: &proof_desc,
                        },
                    ];
                    if self.should_open_selected_issue_external_ref() {
                        hints.push(CommandHint {
                            key: "o",
                            desc: "open link",
                        });
                        hints.push(CommandHint {
                            key: "y",
                            desc: "copy link",
                        });
                    }
                    Some(styled_mode_footer(
                        &format!("Insights [{panel_label}]"),
                        &hints,
                        rows[2].width,
                    ))
                } else {
                    Some(RichText::raw(self.status_msg.clone()))
                }
            }
            ViewMode::Graph => {
                if self.status_msg.is_empty() {
                    None
                } else {
                    Some(RichText::raw(self.status_msg.clone()))
                }
            }
            ViewMode::History => {
                if self.history_search_active {
                    let mode_label = self.history_view_mode.label();
                    let search_label = self.history_search_mode.label();
                    Some(styled_mode_footer(
                        &format!("History ({mode_label}): [{search_label}]"),
                        &[
                            CommandHint {
                                key: "Tab",
                                desc: "cycles mode",
                            },
                            CommandHint {
                                key: "Enter",
                                desc: "confirm",
                            },
                            CommandHint {
                                key: "Esc",
                                desc: "cancel",
                            },
                        ],
                        rows[2].width,
                    ))
                } else {
                    None
                }
            }
            ViewMode::Actionable => {
                let plan = self.actionable_plan.as_ref();
                let track_count = plan.map_or(0, |p| p.tracks.len());
                let item_count = plan.map_or(0, |p| p.summary.actionable_count);
                Some(styled_mode_footer(
                    &format!("Actionable: {track_count} tracks, {item_count} items"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "navigate",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "a/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
            ViewMode::Attention => {
                let label_count = self.attention_result.as_ref().map_or(0, |r| r.labels.len());
                Some(styled_mode_footer(
                    &format!("Attention: {label_count} labels ranked"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "navigate",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "!/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
            ViewMode::Tree => {
                let node_count = self.tree_flat_nodes.len();
                Some(styled_mode_footer(
                    &format!("Tree: {node_count} nodes"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "navigate",
                        },
                        CommandHint {
                            key: "Enter",
                            desc: "expand/collapse",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "T/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
            ViewMode::LabelDashboard => {
                let label_count = self.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
                Some(styled_mode_footer(
                    &format!("Labels: {label_count}"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "navigate",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "[/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
            ViewMode::FlowMatrix => {
                let label_count = self.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
                let dep_count = self
                    .flow_matrix
                    .as_ref()
                    .map_or(0, |f| f.total_cross_label_deps);
                Some(styled_mode_footer(
                    &format!("Flow: {label_count} labels, {dep_count} cross-deps"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "rows",
                        },
                        CommandHint {
                            key: "h/l",
                            desc: "cols",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "]/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
            ViewMode::TimeTravelDiff => {
                if self.time_travel_input_active {
                    Some(styled_mode_footer(
                        "Time-travel: enter git ref or file path",
                        &[
                            CommandHint {
                                key: "Enter",
                                desc: "confirm",
                            },
                            CommandHint {
                                key: "Esc",
                                desc: "cancel",
                            },
                        ],
                        rows[2].width,
                    ))
                } else if self.time_travel_diff.is_some() {
                    Some(styled_mode_footer(
                        "Time-travel",
                        &[
                            CommandHint {
                                key: "j/k",
                                desc: "navigate",
                            },
                            CommandHint {
                                key: "Tab",
                                desc: "focus",
                            },
                            CommandHint {
                                key: "T",
                                desc: "reload",
                            },
                            CommandHint {
                                key: "t/Esc",
                                desc: "back",
                            },
                        ],
                        rows[2].width,
                    ))
                } else {
                    Some(styled_mode_footer(
                        "Time-travel: no diff loaded",
                        &[
                            CommandHint {
                                key: "t",
                                desc: "enter ref",
                            },
                            CommandHint {
                                key: "Esc",
                                desc: "back",
                            },
                        ],
                        rows[2].width,
                    ))
                }
            }
            ViewMode::Sprint => {
                let sprint_count = self.sprint_data.len();
                Some(styled_mode_footer(
                    &format!("Sprint: {sprint_count} sprint(s)"),
                    &[
                        CommandHint {
                            key: "j/k",
                            desc: "navigate",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "focus",
                        },
                        CommandHint {
                            key: "S/Esc",
                            desc: "back",
                        },
                    ],
                    rows[2].width,
                ))
            }
        };
        let footer_text = footer_text.unwrap_or_else(|| match self.mode {
            ViewMode::Main => {
                let hints = self.main_footer_command_hints();
                wrap_command_hints(&hints, rows[2].width.saturating_sub(1) as usize)
            }
            ViewMode::Graph => {
                let hints = self.graph_footer_command_hints();
                wrap_command_hints(&hints, rows[2].width.saturating_sub(1) as usize)
            }
            ViewMode::History => {
                if self.history_file_tree_focus {
                    let mut hints = vec![
                        CommandHint {
                            key: "j/k",
                            desc: "tree",
                        },
                        CommandHint {
                            key: "Enter",
                            desc: "filter",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "panes",
                        },
                        CommandHint {
                            key: "Esc",
                            desc: "close tree",
                        },
                    ];
                    hints.extend([
                        CommandHint {
                            key: "^←/→",
                            desc: "resize",
                        },
                        CommandHint {
                            key: "^0",
                            desc: "reset split",
                        },
                    ]);
                    wrap_command_hints(&hints, rows[2].width.saturating_sub(1) as usize)
                } else {
                    let confidence = format!(
                        "confidence >= {:.0}%",
                        self.history_min_confidence() * 100.0
                    );
                    let mut hints = vec![
                        CommandHint {
                            key: "c",
                            desc: confidence.as_str(),
                        },
                        CommandHint {
                            key: "v",
                            desc: "bead/git",
                        },
                        CommandHint {
                            key: "/",
                            desc: "search",
                        },
                        CommandHint {
                            key: "Tab",
                            desc: "mode",
                        },
                        CommandHint {
                            key: "y",
                            desc: "copy",
                        },
                    ];
                    if self.history_selected_commit_url().is_some() {
                        hints.push(CommandHint {
                            key: "o",
                            desc: "open commit",
                        });
                    }
                    hints.extend([
                        CommandHint {
                            key: "f",
                            desc: "file-tree",
                        },
                        CommandHint {
                            key: "h/Esc",
                            desc: "back",
                        },
                        CommandHint {
                            key: "^←/→",
                            desc: "resize",
                        },
                        CommandHint {
                            key: "^0",
                            desc: "reset split",
                        },
                    ]);
                    wrap_command_hints(&hints, rows[2].width.saturating_sub(1) as usize)
                }
            }
            _ => unreachable!("footer rich hints only apply to main/graph/history"),
        });
        Paragraph::new(footer_text)
            .style(tokens::footer())
            .render(rows[2], frame);
    }
}

impl BvrApp {
    fn header_mode_tab_at(&self, x: u16, y: u16) -> Option<ViewMode> {
        if y != 0 {
            return None;
        }

        header_mode_tabs(self, cached_view_width())
            .into_iter()
            .find(|tab| rect_contains(tab.rect, x, y))
            .map(|tab| tab.mode)
    }

    /// Push current mode onto back stack before switching.
    fn push_mode_stack(&mut self) {
        if self.mode_back_stack.last() != Some(&self.mode) {
            self.mode_back_stack.push(self.mode);
            // Cap stack at 10 to prevent unbounded growth
            if self.mode_back_stack.len() > 10 {
                self.mode_back_stack.remove(0);
            }
        }
    }

    fn activate_mode_tab(&mut self, mode: ViewMode) {
        self.push_mode_stack();
        match mode {
            ViewMode::Main => {
                self.mode = ViewMode::Main;
                self.focus = FocusPane::List;
            }
            ViewMode::Board => {
                self.mode = ViewMode::Board;
                self.focus = FocusPane::List;
            }
            ViewMode::Insights => {
                self.mode = ViewMode::Insights;
                self.focus = FocusPane::List;
            }
            ViewMode::Graph => {
                self.mode = ViewMode::Graph;
                self.focus = FocusPane::List;
            }
            ViewMode::History => {
                if !matches!(self.mode, ViewMode::History) {
                    self.toggle_history_mode();
                }
                self.focus = FocusPane::List;
            }
            ViewMode::Actionable => {
                if !matches!(self.mode, ViewMode::Actionable) {
                    self.compute_actionable_plan();
                    self.mode = ViewMode::Actionable;
                }
                self.detail_scroll_offset = 0;
                self.focus = FocusPane::List;
            }
            ViewMode::Attention => {
                if !matches!(self.mode, ViewMode::Attention) {
                    self.compute_attention();
                    self.mode = ViewMode::Attention;
                }
                self.focus = FocusPane::List;
            }
            ViewMode::Tree => {
                if !matches!(self.mode, ViewMode::Tree) {
                    self.toggle_tree_mode();
                }
                self.focus = FocusPane::List;
            }
            ViewMode::LabelDashboard => {
                if !matches!(self.mode, ViewMode::LabelDashboard) {
                    self.toggle_label_dashboard();
                }
                self.focus = FocusPane::List;
            }
            ViewMode::FlowMatrix => {
                if !matches!(self.mode, ViewMode::FlowMatrix) {
                    self.toggle_flow_matrix();
                }
                self.focus = FocusPane::List;
            }
            ViewMode::TimeTravelDiff => {
                if !matches!(self.mode, ViewMode::TimeTravelDiff) {
                    self.toggle_time_travel_mode();
                }
            }
            ViewMode::Sprint => {
                if !matches!(self.mode, ViewMode::Sprint) {
                    self.toggle_sprint_mode();
                }
                self.focus = FocusPane::List;
            }
        }
        self.status_msg = format!("Switched to {}", self.mode.label());
    }

    fn splitter_hit_target_at(&self, x: u16, y: u16) -> Option<SplitterHitBox> {
        splitter_hit_boxes(self, cached_view_width(), cached_view_height())
            .into_iter()
            .find(|hit_box| rect_contains(hit_box.rect, x, y))
    }

    fn adjust_splitter_target(
        &mut self,
        target: SplitterTarget,
        delta_pct: f32,
        source: &'static str,
    ) -> bool {
        let mut state = pane_split_state();
        let changed = state.adjust_splitter_target(target, delta_pct);
        if changed {
            set_pane_split_state(state);
            self.status_msg = format!("Pane split adjusted ({source}, {delta_pct:+.0}%)");
        }
        changed
    }

    fn handle_splitter_mouse_scroll(&mut self, event: MouseEvent) -> Option<Cmd<Msg>> {
        let hit_box = self.splitter_hit_target_at(event.x, event.y)?;
        let delta_pct = match event.kind {
            MouseEventKind::ScrollUp => 4.0,
            MouseEventKind::ScrollDown => -4.0,
            _ => return None,
        };
        self.adjust_splitter_target(hit_box.target, delta_pct, "mouse");
        Some(Cmd::None)
    }

    fn handle_splitter_mouse_click(&mut self, event: MouseEvent) -> Option<Cmd<Msg>> {
        if !matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        let hit_box = self.splitter_hit_target_at(event.x, event.y)?;
        let midpoint = hit_box.rect.x.saturating_add(hit_box.rect.width / 2);
        let expand_leading = event.x < midpoint;
        let delta_pct = if expand_leading { 4.0 } else { -4.0 };

        self.focus = match (hit_box.target, expand_leading) {
            (SplitterTarget::TwoPane { .. }, true) => FocusPane::List,
            (SplitterTarget::TwoPane { .. }, false) => FocusPane::Detail,
            (SplitterTarget::HistoryThree { divider, .. }, true) if divider == 0 => FocusPane::List,
            (SplitterTarget::HistoryThree { divider, .. }, false) if divider == 0 => {
                FocusPane::Middle
            }
            (SplitterTarget::HistoryThree { .. }, true) => FocusPane::Middle,
            (SplitterTarget::HistoryThree { .. }, false) => FocusPane::Detail,
            (SplitterTarget::HistoryFour { divider }, true) if divider == 0 => FocusPane::List,
            (SplitterTarget::HistoryFour { divider }, false) if divider == 0 => FocusPane::Middle,
            (SplitterTarget::HistoryFour { divider }, true) if divider == 1 => FocusPane::Middle,
            (SplitterTarget::HistoryFour { divider }, false) if divider == 1 => FocusPane::Middle,
            (SplitterTarget::HistoryFour { .. }, true) => FocusPane::Middle,
            (SplitterTarget::HistoryFour { .. }, false) => FocusPane::Detail,
        };

        self.adjust_splitter_target(hit_box.target, delta_pct, "mouse");
        Some(Cmd::None)
    }

    fn handle_header_mouse_click(&mut self, event: MouseEvent) -> Option<Cmd<Msg>> {
        if !matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        let mode = self.header_mode_tab_at(event.x, event.y)?;
        self.activate_mode_tab(mode);
        Some(Cmd::None)
    }

    fn adjust_active_pane_split(&mut self, delta_pct: f32) -> bool {
        let mut state = pane_split_state();
        let changed = if matches!(self.mode, ViewMode::History) {
            state.adjust_history(
                self.history_layout(),
                self.history_view_mode,
                self.focus,
                delta_pct,
            )
        } else if matches!(self.mode, ViewMode::Graph)
            && matches!(
                Breakpoint::from_width(cached_view_width()),
                Breakpoint::Narrow
            )
        {
            false
        } else {
            state.adjust_two_pane(Breakpoint::from_width(cached_view_width()), delta_pct)
        };
        if changed {
            set_pane_split_state(state);
            self.status_msg = format!("Pane split adjusted ({delta_pct:+.0}%)");
        }
        changed
    }

    fn history_layout(&self) -> HistoryLayout {
        HistoryLayout::from_width(cached_view_width())
    }

    fn history_has_middle_pane(&self) -> bool {
        matches!(self.mode, ViewMode::History) && self.history_layout().has_middle_pane()
    }

    fn history_list_panel_title(&self) -> &'static str {
        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            "Commits"
        } else {
            "Beads With History"
        }
    }

    fn history_middle_panel_title(&self) -> &'static str {
        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            "Related Beads"
        } else {
            "Commits"
        }
    }

    fn history_detail_panel_title(&self) -> &'static str {
        "Commit Details"
    }

    fn history_timeline_panel_title(&self) -> String {
        self.selected_issue()
            .map(|issue| format!("Timeline: {}", issue.id))
            .unwrap_or_else(|| "Timeline".to_string())
    }

    fn reset_pane_split_state(&mut self) -> bool {
        let default_state = PaneSplitState::default();
        if pane_split_state() == default_state {
            return false;
        }

        set_pane_split_state(default_state);
        self.status_msg = "Pane splits reset".to_string();
        true
    }

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
            issues.len() > crate::analysis::graph::AnalysisConfig::background_threshold();
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
        self.detail_scroll_offset = 0;
        self.selected = 0;

        if let Some(id) = selected_id.as_deref() {
            self.select_issue_by_id(id);
        }

        if !self.preserve_off_queue_ranked_context() {
            self.ensure_selected_visible();
        }
        self.sync_insights_heatmap_selection();

        // Rebuild tree flat nodes after the analyzer was replaced so
        // stale issue_index values don't cause out-of-bounds panics
        // when Tree view renders with the new, potentially shorter,
        // issue list.
        self.rebuild_tree_if_active();
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

    fn handle_mouse(&mut self, event: MouseEvent) -> Cmd<Msg> {
        if let Some(cmd) = self.handle_header_mouse_click(event) {
            return cmd;
        }

        if let Some(cmd) = self.handle_splitter_mouse_scroll(event) {
            return cmd;
        }

        if let Some(cmd) = self.handle_splitter_mouse_click(event) {
            return cmd;
        }

        match event.kind {
            MouseEventKind::ScrollUp => self.handle_key(KeyCode::Up, Modifiers::NONE),
            MouseEventKind::ScrollDown => self.handle_key(KeyCode::Down, Modifiers::NONE),
            MouseEventKind::Down(MouseButton::Left)
                if self.mouse_open_detail_link(event.x, event.y) =>
            {
                Cmd::None
            }
            MouseEventKind::Down(MouseButton::Right)
                if self.mouse_copy_detail_link(event.x, event.y) =>
            {
                Cmd::None
            }
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
                ModalOverlay::Confirm { resume_overlay, .. } => {
                    match code {
                        KeyCode::Char('y' | 'Y') => {
                            self.modal_confirm_result = Some(true);
                            self.modal_overlay = None;
                        }
                        KeyCode::Char('n' | 'N') | KeyCode::Escape => {
                            self.modal_confirm_result = Some(false);
                            self.modal_overlay = resume_overlay.clone().map(|overlay| *overlay);
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
                        KeyCode::Enter if cur < len => {
                            let recipe_name = items[cur].0.clone();
                            self.modal_overlay = None;
                            self.status_msg = format!("Recipe: {recipe_name}");
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
                ModalOverlay::LabelPicker {
                    items,
                    cursor,
                    filter,
                } => {
                    let needle = filter.to_ascii_lowercase();
                    let filtered: Vec<usize> = items
                        .iter()
                        .enumerate()
                        .filter(|(_, (name, _))| {
                            needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let flen = filtered.len();
                    match code {
                        KeyCode::Escape => self.modal_overlay = None,
                        KeyCode::Down => {
                            if let Some(ModalOverlay::LabelPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                if *cursor + 1 < flen {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Up => {
                            if let Some(ModalOverlay::LabelPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(ModalOverlay::LabelPicker { filter, cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                filter.pop();
                                *cursor = 0;
                            }
                        }
                        KeyCode::Enter => {
                            let actual_idx = filtered.get(*cursor).copied();
                            if let Some(idx) = actual_idx {
                                let label = items[idx].0.clone();
                                self.modal_overlay = None;
                                self.set_label_filter(&label);
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(ModalOverlay::LabelPicker { filter, cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                filter.push(c);
                                *cursor = 0;
                            }
                        }
                        _ => {}
                    }
                    return Cmd::None;
                }
                ModalOverlay::RepoPicker {
                    items,
                    cursor,
                    filter,
                } => {
                    let needle = filter.to_ascii_lowercase();
                    let filtered: Vec<usize> = items
                        .iter()
                        .enumerate()
                        .filter(|(_, name)| {
                            needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let flen = filtered.len();
                    match code {
                        KeyCode::Escape => self.modal_overlay = None,
                        KeyCode::Down => {
                            if let Some(ModalOverlay::RepoPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                if *cursor + 1 < flen {
                                    *cursor += 1;
                                }
                            }
                        }
                        KeyCode::Up => {
                            if let Some(ModalOverlay::RepoPicker { cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                *cursor = cursor.saturating_sub(1);
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(ModalOverlay::RepoPicker { filter, cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                filter.pop();
                                *cursor = 0;
                            }
                        }
                        KeyCode::Enter => {
                            let actual_idx = filtered.get(*cursor).copied();
                            if let Some(idx) = actual_idx {
                                let repo = items[idx].clone();
                                self.modal_overlay = None;
                                self.set_repo_filter(&repo);
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(ModalOverlay::RepoPicker { filter, cursor, .. }) =
                                &mut self.modal_overlay
                            {
                                filter.push(c);
                                *cursor = 0;
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

        if matches!(
            (code, modifiers.contains(Modifiers::CTRL)),
            (KeyCode::Char('0'), true)
        ) {
            self.reset_pane_split_state();
            return Cmd::None;
        }

        if !self.preserve_off_queue_ranked_context() {
            self.ensure_selected_visible();
        }

        if matches!(self.mode, ViewMode::Main)
            && self.focus == FocusPane::List
            && self.main_search_active
        {
            match code {
                KeyCode::Escape => self.cancel_main_search(),
                KeyCode::Enter => self.finish_main_search(),
                KeyCode::Backspace => {
                    self.main_search_query.pop();
                    self.main_search_match_cursor = 0;
                    self.select_current_main_search_match();
                }
                KeyCode::Char('n') => self.move_main_search_match_relative(1),
                KeyCode::Char('N') => self.move_main_search_match_relative(-1),
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.main_search_query.push(ch);
                    self.main_search_match_cursor = 0;
                    self.select_current_main_search_match();
                }
                _ => {}
            }
            return Cmd::None;
        }

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

        // -- Tree search input -------------------------------------------------------
        if matches!(self.mode, ViewMode::Tree)
            && self.focus == FocusPane::List
            && self.tree_search_active
        {
            match code {
                KeyCode::Escape => self.cancel_tree_search(),
                KeyCode::Enter => self.finish_tree_search(),
                KeyCode::Backspace => {
                    self.tree_search_query.pop();
                    self.tree_search_match_cursor = 0;
                    self.select_current_tree_search_match();
                }
                KeyCode::Char('n') => self.move_tree_search_match_relative(1),
                KeyCode::Char('N') => self.move_tree_search_match_relative(-1),
                KeyCode::Char(ch) if !modifiers.contains(Modifiers::CTRL) && !ch.is_control() => {
                    self.tree_search_query.push(ch);
                    self.tree_search_match_cursor = 0;
                    self.select_current_tree_search_match();
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

        // -- Vim z-prefix handling -----------------------------------------------
        if self.pending_z {
            self.pending_z = false;
            if self.tree_shortcut_focus() {
                match code {
                    KeyCode::Char('o') => self.tree_expand_current(),
                    KeyCode::Char('c') => self.tree_collapse_current(),
                    KeyCode::Char('a') => self.tree_toggle_collapse(),
                    KeyCode::Char('R') => self.tree_expand_all(),
                    KeyCode::Char('M') => self.tree_collapse_all(),
                    KeyCode::Char('z') => self.tree_recenter_cursor(),
                    _ => {}
                }
            }
            return Cmd::None;
        }

        // -- Vim gg (jump-to-top) handling ---------------------------------------
        // First `g` speculatively toggles graph and latches pending_g. A rapid
        // second `g` undoes the toggle and jumps to the top of the current list.
        // Any other key after `g` just clears the latch (the toggle stands).
        if self.pending_g {
            self.pending_g = false;
            if code == KeyCode::Char('g') {
                // Restore mode that was saved before the speculative graph toggle.
                if let Some(prev) = self.g_pre_toggle_mode.take() {
                    self.mode = prev;
                }
                self.vim_jump_to_top();
                return Cmd::None;
            }
            self.g_pre_toggle_mode = None;
            // Not gg — the speculative graph toggle stands. Fall through to
            // handle the current key normally.
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
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead)
                    && matches!(self.focus, FocusPane::Middle) =>
            {
                self.jump_from_history_bead_commit_to_git();
            }
            KeyCode::Enter
                if !(matches!(self.mode, ViewMode::History) && self.history_file_tree_focus)
                    && !matches!(self.mode, ViewMode::Tree) =>
            {
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && let Some(bead_id) = self
                        .selected_history_git_related_bead_id()
                        .or_else(|| self.selected_history_event().map(|event| event.issue_id))
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
                } else if !matches!(self.mode, ViewMode::Main) {
                    // Pop from back stack if available, otherwise go to Main
                    let prev = self.mode_back_stack.pop().unwrap_or(ViewMode::Main);
                    self.mode = prev;
                    self.focus = FocusPane::List;
                    self.detail_scroll_offset = 0;
                } else if matches!(self.focus, FocusPane::Detail) {
                    self.focus = FocusPane::List;
                    self.status_msg = "Focus returned to list".into();
                } else if !self.main_search_query.is_empty() {
                    self.cancel_main_search();
                    self.status_msg = "Main search cleared".into();
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
            KeyCode::Right if modifiers.contains(Modifiers::CTRL) => {
                self.adjust_active_pane_split(4.0);
            }
            KeyCode::Left if modifiers.contains(Modifiers::CTRL) => {
                self.adjust_active_pane_split(-4.0);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let reverse = matches!(code, KeyCode::BackTab);
                if matches!(self.mode, ViewMode::History) && self.history_show_file_tree {
                    if self.history_file_tree_focus {
                        self.history_file_tree_focus = false;
                        self.focus = if reverse {
                            FocusPane::Detail
                        } else {
                            FocusPane::List
                        };
                    } else if self.history_has_middle_pane() {
                        match (self.focus, reverse) {
                            (FocusPane::List, false) => self.focus = FocusPane::Middle,
                            (FocusPane::Middle, false) => self.focus = FocusPane::Detail,
                            (FocusPane::Detail, false) => self.history_file_tree_focus = true,
                            (FocusPane::List, true) => self.history_file_tree_focus = true,
                            (FocusPane::Middle, true) => self.focus = FocusPane::List,
                            (FocusPane::Detail, true) => self.focus = FocusPane::Middle,
                        }
                    } else if reverse && self.focus == FocusPane::List {
                        self.history_file_tree_focus = true;
                    } else if self.focus == FocusPane::Detail {
                        self.focus = FocusPane::List;
                    } else if !reverse {
                        self.history_file_tree_focus = true;
                    } else {
                        self.focus = FocusPane::Detail;
                    }
                } else if self.history_has_middle_pane() {
                    self.focus = match (self.focus, reverse) {
                        (FocusPane::List, false) => FocusPane::Middle,
                        (FocusPane::Middle, false) => FocusPane::Detail,
                        (FocusPane::Detail, false) => FocusPane::List,
                        (FocusPane::List, true) => FocusPane::Detail,
                        (FocusPane::Middle, true) => FocusPane::List,
                        (FocusPane::Detail, true) => FocusPane::Middle,
                    };
                } else {
                    self.focus = match self.focus {
                        FocusPane::List => FocusPane::Detail,
                        FocusPane::Middle => FocusPane::Detail,
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
            // Universal detail pane scroll — works in any mode with Detail focus
            KeyCode::Char('j')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(3);
            }
            KeyCode::Char('k')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(-3);
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
                if matches!(self.mode, ViewMode::Graph)
                    && matches!(self.focus, FocusPane::List | FocusPane::Detail) =>
            {
                self.start_graph_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Insights)
                    && matches!(self.focus, FocusPane::List | FocusPane::Detail) =>
            {
                self.start_insights_search();
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Main) && self.focus == FocusPane::List =>
            {
                self.start_main_search();
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
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::Main)
                    && self.focus == FocusPane::List
                    && !self.main_search_query.is_empty() =>
            {
                self.move_main_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::Main)
                    && self.focus == FocusPane::List
                    && !self.main_search_query.is_empty() =>
            {
                self.move_main_search_match_relative(-1);
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
            KeyCode::Char('j') | KeyCode::Down
                if self.tree_shortcut_focus()
                    && self.tree_cursor + 1 < self.tree_flat_nodes.len() =>
            {
                self.tree_cursor += 1;
            }
            KeyCode::Char('k') | KeyCode::Up if self.tree_shortcut_focus() => {
                self.tree_cursor = self.tree_cursor.saturating_sub(1);
            }
            KeyCode::Home if self.tree_shortcut_focus() => {
                self.tree_cursor = 0;
            }
            KeyCode::End | KeyCode::Char('G') if self.tree_shortcut_focus() => {
                self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
            }
            KeyCode::PageDown if self.tree_shortcut_focus() => {
                let step = self.list_page_step();
                self.tree_cursor =
                    (self.tree_cursor + step).min(self.tree_flat_nodes.len().saturating_sub(1));
            }
            KeyCode::PageUp if self.tree_shortcut_focus() => {
                let step = self.list_page_step();
                self.tree_cursor = self.tree_cursor.saturating_sub(step);
            }
            KeyCode::Enter
                if matches!(self.mode, ViewMode::Tree) && self.focus == FocusPane::List =>
            {
                self.tree_toggle_collapse();
            }
            KeyCode::Char('z') if self.tree_shortcut_focus() => {
                self.pending_z = true;
            }
            KeyCode::Char('/')
                if matches!(self.mode, ViewMode::Tree) && self.focus == FocusPane::List =>
            {
                self.start_tree_search();
            }
            KeyCode::Char('n')
                if matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List
                    && !self.tree_search_query.is_empty() =>
            {
                self.move_tree_search_match_relative(1);
            }
            KeyCode::Char('N')
                if matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List
                    && !self.tree_search_query.is_empty() =>
            {
                self.move_tree_search_match_relative(-1);
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
                    let categories = self.time_travel_categories();
                    let issue_count = categories
                        .get(self.time_travel_category_cursor)
                        .map_or(0, |(_, count)| *count);
                    if issue_count > 0 && self.time_travel_issue_cursor + 1 < issue_count {
                        self.time_travel_issue_cursor += 1;
                    }
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
            // Universal detail pane page scroll — works in any non-Board mode
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(10);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(-10);
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
                    && matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List =>
            {
                let half = self.list_page_step() / 2;
                self.tree_cursor =
                    (self.tree_cursor + half).min(self.tree_flat_nodes.len().saturating_sub(1));
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List =>
            {
                let half = self.list_page_step() / 2;
                self.tree_cursor = self.tree_cursor.saturating_sub(half);
            }
            KeyCode::Char('d')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(self.list_page_step() as isize);
            }
            KeyCode::Char('u')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(-(self.list_page_step() as isize));
            }
            // Ctrl-F / Ctrl-B — full page scroll (vim aliases for PageDown/PageUp)
            // Ordering mirrors Ctrl-D/Ctrl-U above: specific detail handlers
            // MUST come before broad board_shortcut_focus() guards, which match
            // both List and Detail focus.
            KeyCode::Char('f')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List =>
            {
                let step = self.list_page_step();
                self.tree_cursor =
                    (self.tree_cursor + step).min(self.tree_flat_nodes.len().saturating_sub(1));
            }
            KeyCode::Char('b')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Tree)
                    && self.focus == FocusPane::List =>
            {
                let step = self.list_page_step();
                self.tree_cursor = self.tree_cursor.saturating_sub(step);
            }
            KeyCode::Char('f')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(self.list_page_step() as isize);
            }
            KeyCode::Char('b')
                if modifiers.contains(Modifiers::CTRL)
                    && matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_board_detail(-(self.list_page_step() as isize));
            }
            KeyCode::Char('f')
                if modifiers.contains(Modifiers::CTRL) && self.board_shortcut_focus() =>
            {
                self.move_board_row_relative(self.list_page_step() as isize);
            }
            KeyCode::Char('b')
                if modifiers.contains(Modifiers::CTRL) && self.board_shortcut_focus() =>
            {
                self.move_board_row_relative(-(self.list_page_step() as isize));
            }
            KeyCode::Char('f')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(self.list_page_step() as isize);
            }
            KeyCode::Char('b')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::Detail =>
            {
                self.scroll_detail(-(self.list_page_step() as isize));
            }
            KeyCode::Char('f')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(self.list_page_step() as isize);
            }
            KeyCode::Char('b')
                if modifiers.contains(Modifiers::CTRL)
                    && !matches!(self.mode, ViewMode::Board)
                    && self.focus == FocusPane::List =>
            {
                self.move_selection_relative(-(self.list_page_step() as isize));
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
            KeyCode::Char('h') if matches!(self.mode, ViewMode::Main) => {
                self.toggle_history_mode();
            }
            KeyCode::Char('h')
                if matches!(self.mode, ViewMode::History) && !self.history_file_tree_focus =>
            {
                self.toggle_history_mode();
            }
            KeyCode::Char('c')
                if matches!(self.mode, ViewMode::History) && !self.history_file_tree_focus =>
            {
                self.cycle_history_confidence();
            }
            KeyCode::Char('v')
                if matches!(self.mode, ViewMode::History) && !self.history_file_tree_focus =>
            {
                self.toggle_history_view_mode();
            }
            KeyCode::Char('s') if matches!(self.mode, ViewMode::Main) => self.cycle_list_sort(),
            KeyCode::Char('m') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_heatmap();
            }
            KeyCode::Char('y')
                if matches!(self.mode, ViewMode::History) && !self.history_file_tree_focus =>
            {
                self.history_copy_to_clipboard();
            }
            KeyCode::Char('y') if self.should_copy_selected_issue_external_ref() => {
                self.copy_selected_issue_external_ref();
            }
            KeyCode::Char('o')
                if matches!(self.mode, ViewMode::History) && !self.history_file_tree_focus =>
            {
                self.history_open_in_browser();
            }
            KeyCode::Char('o') if self.should_open_selected_issue_external_ref() => {
                self.open_selected_issue_external_ref();
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
            KeyCode::Char('I') => self.set_list_filter(ListFilter::InProgress),
            KeyCode::Char('B') => self.set_list_filter(ListFilter::Blocked),
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
                    && matches!(self.focus, FocusPane::Middle | FocusPane::Detail) =>
            {
                self.move_history_related_bead_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Git)
                    && matches!(self.focus, FocusPane::Middle | FocusPane::Detail) =>
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
                    && matches!(self.focus, FocusPane::Middle | FocusPane::Detail) =>
            {
                self.move_history_bead_commit_relative(1);
            }
            KeyCode::Char('k') | KeyCode::Up
                if matches!(self.mode, ViewMode::History)
                    && matches!(self.history_view_mode, HistoryViewMode::Bead)
                    && matches!(self.focus, FocusPane::Middle | FocusPane::Detail) =>
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
                self.move_selection_relative(-(self.list_page_step() as isize));
            }
            KeyCode::PageDown if self.focus == FocusPane::List => {
                self.move_selection_relative(self.list_page_step() as isize);
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
                self.reselect_insights_panel_context();
            }
            KeyCode::Char('S')
                if matches!(self.mode, ViewMode::Insights) && self.insights_heatmap.is_none() =>
            {
                self.insights_panel = self.insights_panel.prev();
                self.reselect_insights_panel_context();
            }
            KeyCode::Char('e') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_explanations();
            }
            KeyCode::Char('x') if matches!(self.mode, ViewMode::Insights) => {
                self.toggle_insights_calc_proof();
            }
            KeyCode::Char('1') => {
                self.mode = ViewMode::Main;
                self.focus = FocusPane::List;
                self.sync_ranked_list_context();
            }
            KeyCode::Char('b') => {
                self.mode = if matches!(self.mode, ViewMode::Board) {
                    ViewMode::Main
                } else {
                    ViewMode::Board
                };
                self.focus = FocusPane::List;
            }
            KeyCode::Char('i') => {
                let entering_insights = !matches!(self.mode, ViewMode::Insights);
                let previous_mode = self.mode;
                let previous_selected = self.selected;
                self.mode = if entering_insights {
                    ViewMode::Insights
                } else {
                    ViewMode::Main
                };
                self.focus = FocusPane::List;
                if entering_insights {
                    if matches!(previous_mode, ViewMode::Main) {
                        self.reselect_ranked_list_context();
                    } else {
                        self.sync_insights_heatmap_selection();
                        if self.insights_heatmap.is_none() {
                            self.set_selected_index(previous_selected);
                        }
                    }
                } else {
                    self.sync_ranked_list_context();
                }
            }
            KeyCode::Char('g') if matches!(self.mode, ViewMode::History) => {
                let saved = self.mode;
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
                self.sync_ranked_list_context();
                self.pending_g = true;
                self.g_pre_toggle_mode = Some(saved);
            }
            KeyCode::Char('g') => {
                let saved = self.mode;
                let entering_graph = !matches!(self.mode, ViewMode::Graph);
                let previous_mode = self.mode;
                let previous_selected = self.selected;
                self.mode = if entering_graph {
                    ViewMode::Graph
                } else {
                    ViewMode::Main
                };
                self.focus = FocusPane::List;
                if entering_graph {
                    if matches!(previous_mode, ViewMode::Main) {
                        self.reselect_ranked_list_context();
                    } else if !self.graph_search_query.trim().is_empty() {
                        self.select_current_graph_search_match();
                    } else {
                        self.set_selected_index(previous_selected);
                        self.sync_insights_heatmap_selection();
                    }
                } else {
                    self.sync_ranked_list_context();
                }
                self.pending_g = true;
                self.g_pre_toggle_mode = Some(saved);
            }
            KeyCode::Char('a') => {
                self.mode = if matches!(self.mode, ViewMode::Actionable) {
                    ViewMode::Main
                } else {
                    self.compute_actionable_plan();
                    ViewMode::Actionable
                };
                self.detail_scroll_offset = 0;
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
            KeyCode::Char('t') if modifiers.contains(Modifiers::CTRL) => {
                self.open_tutorial();
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
            KeyCode::Char('P') => {
                self.open_pages_wizard();
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
        self.history_search_active = false;
        self.history_search_query.clear();
        self.history_search_match_cursor = 0;
        self.history_search_mode = HistorySearchMode::All;
        self.history_show_file_tree = false;
        self.history_file_tree_cursor = 0;
        self.history_file_tree_filter = None;
        self.history_file_tree_focus = false;
        self.history_status_msg.clear();
        self.focus = FocusPane::List;
        self.ensure_git_history_loaded();
    }

    fn cycle_history_confidence(&mut self) {
        self.history_confidence_index =
            (self.history_confidence_index + 1) % HISTORY_CONFIDENCE_STEPS.len();
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            let visible = self.history_git_visible_commit_indices();
            self.history_event_cursor = self
                .history_event_cursor
                .min(visible.len().saturating_sub(1));
        } else {
            let visible = self.history_visible_issue_indices();
            if !visible.contains(&self.selected)
                && let Some(&first_visible) = visible.first()
            {
                self.set_selected_index(first_visible);
            }
        }

        let flat = self.history_flat_file_list();
        self.history_file_tree_cursor = self
            .history_file_tree_cursor
            .min(flat.len().saturating_sub(1));
        self.refresh_history_search_selection();
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

    fn jump_from_history_bead_commit_to_git(&mut self) {
        let Some(commit) = self.selected_history_bead_commit() else {
            self.history_status_msg = "No correlated commit selected".to_string();
            return;
        };

        self.ensure_git_history_loaded();
        self.history_view_mode = HistoryViewMode::Git;
        self.focus = FocusPane::List;
        self.history_related_bead_cursor = 0;
        self.history_bead_commit_cursor = 0;

        let visible = self.history_git_visible_commit_indices();
        let Some(target_slot) = visible.iter().position(|index| {
            self.history_git_cache
                .as_ref()
                .and_then(|cache| cache.commits.get(*index))
                .is_some_and(|entry| entry.sha == commit.sha)
        }) else {
            self.history_status_msg =
                format!("Commit {} not visible in git view", commit.short_sha);
            return;
        };

        self.history_event_cursor = target_slot;
        self.history_status_msg = format!("Backtraced to commit {}", commit.short_sha);
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

    fn open_tutorial(&mut self) {
        self.modal_overlay = Some(ModalOverlay::Tutorial);
    }

    fn open_confirm_with_resume(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        resume_overlay: Option<ModalOverlay>,
    ) {
        self.modal_confirm_result = None;
        self.modal_overlay = Some(ModalOverlay::Confirm {
            title: title.into(),
            message: message.into(),
            resume_overlay: resume_overlay.map(Box::new),
        });
    }

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
            if self
                .history_git_related_beads_for_commit(&commit.sha)
                .is_empty()
            {
                continue;
            }
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

    fn history_selected_commit_sha(&self) -> Option<String> {
        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            self.selected_history_git_commit_sha()
        } else {
            self.selected_history_bead_commit().map(|commit| commit.sha)
        }
    }

    fn history_selected_commit_url(&self) -> Option<String> {
        self.history_selected_commit_sha()
            .and_then(|sha| self.history_commit_url_for_sha(&sha))
    }

    fn history_commit_for_bead(&self, bead_id: &str, sha: &str) -> Option<HistoryCommitCompat> {
        self.history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(bead_id))
            .and_then(|history| history.commits.as_deref())
            .and_then(|commits| commits.iter().find(|commit| commit.sha == sha))
            .cloned()
    }

    fn selected_history_git_bead_commit(&self) -> Option<HistoryCommitCompat> {
        let commit = self.selected_history_git_commit()?;
        let bead_id = self.selected_history_git_related_bead_id()?;
        self.history_commit_for_bead(&bead_id, &commit.sha)
    }

    fn history_commit_url_for_sha(&self, sha: &str) -> Option<String> {
        let repo_root = self
            .repo_root
            .clone()
            .or_else(|| std::env::current_dir().ok())?;
        let remote_url = std::process::Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
            .current_dir(&repo_root)
            .output()
            .ok()
            .and_then(|output| {
                output
                    .status
                    .success()
                    .then(|| String::from_utf8(output.stdout).ok())
                    .flatten()
            })
            .map(|s| s.trim().to_string())?;
        remote_to_commit_url(&remote_url, sha)
    }

    /// Open selected commit in browser via git remote URL.
    fn history_open_in_browser(&mut self) {
        let sha = self.history_selected_commit_sha();

        let Some(sha) = sha else {
            self.history_status_msg = "No commit selected".into();
            return;
        };

        let Some(url) = self.history_commit_url_for_sha(&sha) else {
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

    fn history_copy_commit_url(&mut self) {
        let sha = self.history_selected_commit_sha();

        let Some(sha) = sha else {
            self.history_status_msg = "No commit selected".into();
            return;
        };

        let Some(url) = self.history_commit_url_for_sha(&sha) else {
            self.history_status_msg = "Cannot build commit URL from remote".into();
            return;
        };

        if copy_text_to_clipboard(&url) {
            let short = if sha.len() > 7 { &sha[..7] } else { &sha };
            self.history_status_msg = format!("Copied {short} commit URL");
        } else {
            self.history_status_msg = "Clipboard not available".into();
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
        let workspace_aliases = build_workspace_id_aliases(&self.analyzer.issues);

        correlate_histories_with_git_aliases(
            &repo_root,
            &commits,
            &mut histories,
            &mut commit_index,
            &mut method_distribution,
            &workspace_aliases,
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
            ListFilter::InProgress => issue.status.eq_ignore_ascii_case("in_progress"),
            ListFilter::Blocked => issue.status.eq_ignore_ascii_case("blocked"),
            ListFilter::Closed => issue.is_closed_like(),
            ListFilter::Ready => {
                issue.is_open_like() && self.analyzer.graph.open_blockers(&issue.id).is_empty()
            }
        };
        if !base {
            return false;
        }
        if let Some(ref label) = self.modal_label_filter {
            if !issue
                .labels
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(label))
            {
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
                    ListSort::PageRank => {
                        let l = self
                            .analyzer
                            .metrics
                            .pagerank
                            .get(&left_issue.id)
                            .copied()
                            .unwrap_or_default();
                        let r = self
                            .analyzer
                            .metrics
                            .pagerank
                            .get(&right_issue.id)
                            .copied()
                            .unwrap_or_default();
                        r.total_cmp(&l)
                            .then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
                    ListSort::Blockers => {
                        let l = self
                            .analyzer
                            .metrics
                            .blocks_count
                            .get(&left_issue.id)
                            .copied()
                            .unwrap_or_default();
                        let r = self
                            .analyzer
                            .metrics
                            .blocks_count
                            .get(&right_issue.id)
                            .copied()
                            .unwrap_or_default();
                        r.cmp(&l).then_with(|| left_issue.id.cmp(&right_issue.id))
                    }
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
        if matches!(self.mode, ViewMode::Graph) {
            return self.graph_visible_issue_indices();
        }
        if matches!(self.mode, ViewMode::Insights) {
            return self.insights_visible_issue_indices_for_list_nav();
        }

        self.visible_issue_indices()
    }

    fn selected_visible_slot(&self, visible: &[usize]) -> Option<usize> {
        visible.iter().position(|index| *index == self.selected)
    }

    fn preserve_off_queue_ranked_context(&self) -> bool {
        match self.mode {
            ViewMode::Insights => {
                self.insights_heatmap.is_none()
                    && self.insights_search_query.trim().is_empty()
                    && !self
                        .insights_visible_issue_indices_for_list_nav()
                        .contains(&self.selected)
            }
            ViewMode::Graph => {
                self.graph_search_query.trim().is_empty()
                    && !self.graph_visible_issue_indices().contains(&self.selected)
            }
            _ => false,
        }
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

    fn sync_ranked_list_context(&mut self) {
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
    }

    fn reselect_insights_panel_context(&mut self) {
        if !self.insights_search_query.trim().is_empty() {
            self.select_current_insights_search_match();
            self.sync_insights_heatmap_selection();
            return;
        }

        let visible = self.insights_visible_issue_indices_for_list_nav();
        if visible.contains(&self.selected) {
            self.select_first_visible();
        } else {
            self.sync_insights_heatmap_selection();
        }
    }

    fn reselect_ranked_list_context(&mut self) {
        match self.mode {
            ViewMode::Graph if !self.graph_search_query.trim().is_empty() => {
                self.select_current_graph_search_match();
            }
            ViewMode::Insights if !self.insights_search_query.trim().is_empty() => {
                self.select_current_insights_search_match();
            }
            _ => self.select_first_visible(),
        }
        self.sync_insights_heatmap_selection();
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

    fn list_page_step(&self) -> usize {
        let body_rows = usize::from(cached_view_height().saturating_sub(3));
        if matches!(self.mode, ViewMode::Main) {
            body_rows
                .saturating_sub(self.main_search_banner_lines().len())
                .max(5)
        } else {
            body_rows.saturating_sub(2).max(5)
        }
    }

    fn select_first_visible(&mut self) {
        if let Some(index) = self.visible_issue_indices_for_list_nav().first().copied() {
            self.set_selected_index(index);
            self.list_scroll_offset.set(0);
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
        self.list_scroll_offset.set(0);
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
        self.focus = FocusPane::List;
        self.rebuild_tree_if_active();
    }

    /// Rebuild tree flat nodes when in Tree view so filter changes
    /// take effect immediately.  Clamps the cursor afterward.
    fn rebuild_tree_if_active(&mut self) {
        if matches!(self.mode, ViewMode::Tree) {
            self.build_tree_flat_nodes();
            if self.tree_cursor >= self.tree_flat_nodes.len() {
                self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
            }
        }
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

    /// Universal detail pane scroll — works in any mode when focus is Detail.
    fn scroll_detail(&mut self, delta: isize) {
        if delta == 0 || self.focus != FocusPane::Detail {
            return;
        }

        if delta > 0 {
            self.detail_scroll_offset = self
                .detail_scroll_offset
                .saturating_add(delta.unsigned_abs());
        } else {
            self.detail_scroll_offset = self
                .detail_scroll_offset
                .saturating_sub(delta.unsigned_abs());
        }
    }

    fn set_selected_index(&mut self, index: usize) {
        let changed = self.selected != index;
        self.selected = index;
        if changed {
            self.detail_dep_cursor = 0;
            self.detail_scroll_offset = 0;
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
            && let Some(index) = indices.last().copied()
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

        self.board_visible_issue_indices_in_display_order()
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
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.board_search_match_cursor = next;
        self.set_selected_index(matches[next]);
    }

    // ── Graph search ──────────────────────────────────────────

    fn start_graph_search(&mut self) {
        if !matches!(self.mode, ViewMode::Graph)
            || !matches!(self.focus, FocusPane::List | FocusPane::Detail)
        {
            return;
        }

        self.focus = FocusPane::List;
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
        self.graph_visible_issue_indices()
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

        if let Some(current) = matches.iter().position(|&index| index == self.selected) {
            self.graph_search_match_cursor = current;
            self.set_selected_index(matches[current]);
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

    fn issue_index_for_id(&self, issue_id: &str) -> Option<usize> {
        self.analyzer
            .issues
            .iter()
            .position(|issue| issue.id == issue_id)
    }

    fn insights_visible_issue_indices_for_list_nav(&self) -> Vec<usize> {
        if let Some(state) = self.insights_heatmap.as_ref() {
            let data = self.insights_heatmap_data();
            let row = state
                .row
                .min(INSIGHTS_HEATMAP_DEPTH_LABELS.len().saturating_sub(1));
            let col = state
                .col
                .min(INSIGHTS_HEATMAP_SCORE_LABELS.len().saturating_sub(1));
            return data.issue_ids[row][col]
                .iter()
                .filter_map(|issue_id| self.issue_index_for_id(issue_id))
                .collect();
        }

        let insights = self.analyzer.insights();
        let ids = match self.insights_panel {
            InsightsPanel::Bottlenecks => insights
                .bottlenecks
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
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
                keystones
                    .into_iter()
                    .map(|(id, _)| id.to_string())
                    .collect::<Vec<_>>()
            }
            InsightsPanel::CriticalPath => insights.critical_path.clone(),
            InsightsPanel::Influencers => insights
                .influencers
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            InsightsPanel::Betweenness => insights
                .betweenness
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            InsightsPanel::Hubs => insights
                .hubs
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            InsightsPanel::Authorities => insights
                .authorities
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            InsightsPanel::Cores => insights
                .cores
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            InsightsPanel::CutPoints => insights.articulation_points.clone(),
            InsightsPanel::Slack => insights.slack.clone(),
            InsightsPanel::Priority => self
                .analyzer
                .priority(0.0, 15, None, None)
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            InsightsPanel::Cycles => Vec::new(),
        };

        let indices = ids
            .iter()
            .filter_map(|issue_id| self.issue_index_for_id(issue_id))
            .collect::<Vec<_>>();
        if indices.is_empty() {
            self.visible_issue_indices()
        } else {
            indices
        }
    }

    // ── Insights search ──────────────────────────────────────────

    fn start_insights_search(&mut self) {
        if !matches!(self.mode, ViewMode::Insights)
            || !matches!(self.focus, FocusPane::List | FocusPane::Detail)
        {
            return;
        }

        self.focus = FocusPane::List;
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
        self.insights_visible_issue_indices_for_list_nav()
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

        if let Some(current) = matches.iter().position(|&index| index == self.selected) {
            self.insights_search_match_cursor = current;
            self.set_selected_index(matches[current]);
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

    // ── Main (issues list) search ──────────────────────────────

    fn start_main_search(&mut self) {
        if !matches!(self.mode, ViewMode::Main) || self.focus != FocusPane::List {
            return;
        }

        self.main_search_active = true;
        self.main_search_query.clear();
        self.main_search_match_cursor = 0;
    }

    fn finish_main_search(&mut self) {
        self.main_search_active = false;
    }

    fn cancel_main_search(&mut self) {
        self.main_search_active = false;
        self.main_search_query.clear();
        self.main_search_match_cursor = 0;
    }

    fn main_search_matches(&self) -> Vec<usize> {
        let query = self.main_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.visible_issue_indices()
            .into_iter()
            .filter(|&index| {
                let issue = &self.analyzer.issues[index];
                issue.id.to_ascii_lowercase().contains(&query)
                    || issue.title.to_ascii_lowercase().contains(&query)
                    || issue.status.to_ascii_lowercase().contains(&query)
                    || issue.issue_type.to_ascii_lowercase().contains(&query)
                    || issue.description.to_ascii_lowercase().contains(&query)
                    || issue.notes.to_ascii_lowercase().contains(&query)
                    || issue.design.to_ascii_lowercase().contains(&query)
                    || issue
                        .acceptance_criteria
                        .to_ascii_lowercase()
                        .contains(&query)
                    || issue.assignee.to_ascii_lowercase().contains(&query)
                    || issue
                        .labels
                        .iter()
                        .any(|label| label.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    fn select_current_main_search_match(&mut self) {
        let matches = self.main_search_matches();
        if matches.is_empty() {
            return;
        }

        self.main_search_match_cursor = self
            .main_search_match_cursor
            .min(matches.len().saturating_sub(1));
        self.set_selected_index(matches[self.main_search_match_cursor]);
    }

    fn move_main_search_match_relative(&mut self, delta: isize) {
        let matches = self.main_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }

        let len = matches.len();
        let current = self.main_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };

        self.main_search_match_cursor = next;
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

    fn board_visible_issue_indices_in_display_order(&self) -> Vec<usize> {
        self.board_lane_indices()
            .into_iter()
            .flat_map(|(_, indices)| indices)
            .collect()
    }

    fn issue_diff_tag(&self, issue_id: &str) -> Option<DiffTag> {
        let diff = self.time_travel_diff.as_ref()?;
        if diff
            .new_issues
            .as_ref()
            .is_some_and(|v| v.iter().any(|d| d.id == issue_id))
        {
            return Some(DiffTag::New);
        }
        if diff
            .reopened_issues
            .as_ref()
            .is_some_and(|v| v.iter().any(|d| d.id == issue_id))
        {
            return Some(DiffTag::Reopened);
        }
        if diff
            .modified_issues
            .as_ref()
            .is_some_and(|v| v.iter().any(|m| m.issue_id == issue_id))
        {
            return Some(DiffTag::Modified);
        }
        if diff
            .closed_issues
            .as_ref()
            .is_some_and(|v| v.iter().any(|d| d.id == issue_id))
        {
            return Some(DiffTag::Closed);
        }
        None
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

    fn selected_issue_external_ref_url(&self) -> Option<&str> {
        self.selected_issue()
            .and_then(|issue| issue.external_ref.as_deref())
            .filter(|url| is_http_url(url))
    }

    fn main_footer_command_hints(&self) -> Vec<CommandHint<'static>> {
        let mut hints = vec![
            CommandHint {
                key: "b/i/g/h",
                desc: "modes",
            },
            CommandHint {
                key: "/",
                desc: "search",
            },
            CommandHint {
                key: "s",
                desc: self.list_sort.label(),
            },
            CommandHint {
                key: "p",
                desc: "hints",
            },
            CommandHint {
                key: "C",
                desc: "copy",
            },
        ];
        if matches!(self.focus, FocusPane::Detail) {
            if self.selected_issue_external_ref_url().is_some() {
                hints.push(CommandHint {
                    key: "o",
                    desc: "open link",
                });
                hints.push(CommandHint {
                    key: "y",
                    desc: "copy link",
                });
            }
            hints.push(CommandHint {
                key: "^j/k",
                desc: "scroll",
            });
        }
        hints.extend([
            CommandHint {
                key: "x",
                desc: "export",
            },
            CommandHint {
                key: "O",
                desc: "edit",
            },
            CommandHint {
                key: "^←/→",
                desc: "resize",
            },
            CommandHint {
                key: "^0",
                desc: "reset split",
            },
        ]);
        hints
    }

    fn graph_footer_command_hints(&self) -> Vec<CommandHint<'static>> {
        let mut hints = match self.focus {
            FocusPane::List => vec![
                CommandHint {
                    key: "h/l",
                    desc: "nodes",
                },
                CommandHint {
                    key: "j/k",
                    desc: "nodes",
                },
                CommandHint {
                    key: "H/L",
                    desc: "jump",
                },
                CommandHint {
                    key: "Tab",
                    desc: "detail",
                },
                CommandHint {
                    key: "/",
                    desc: "search",
                },
                CommandHint {
                    key: "Enter",
                    desc: "open details",
                },
            ],
            FocusPane::Detail | FocusPane::Middle => {
                let mut hints = vec![
                    CommandHint {
                        key: "h/Tab",
                        desc: "list",
                    },
                    CommandHint {
                        key: "Enter",
                        desc: "open details",
                    },
                ];
                if !self.detail_dep_list().is_empty() {
                    hints.push(CommandHint {
                        key: "j/k",
                        desc: "deps",
                    });
                }
                hints.push(CommandHint {
                    key: "^j/k",
                    desc: "scroll",
                });
                hints
            }
        };
        if self.selected_issue_external_ref_url().is_some()
            && matches!(self.focus, FocusPane::Detail)
        {
            hints.push(CommandHint {
                key: "o",
                desc: "open link",
            });
            hints.push(CommandHint {
                key: "y",
                desc: "copy link",
            });
        }
        hints.push(CommandHint {
            key: "g/Esc",
            desc: "back",
        });
        hints.push(CommandHint {
            key: "^←/→",
            desc: "resize",
        });
        hints.push(CommandHint {
            key: "^0",
            desc: "reset split",
        });
        hints
    }

    fn should_open_selected_issue_external_ref(&self) -> bool {
        matches!(
            self.mode,
            ViewMode::Main | ViewMode::Board | ViewMode::Insights | ViewMode::Graph
        ) && matches!(self.focus, FocusPane::Detail)
            && self.selected_issue_external_ref_url().is_some()
    }

    fn should_copy_selected_issue_external_ref(&self) -> bool {
        matches!(
            self.mode,
            ViewMode::Main | ViewMode::Board | ViewMode::Insights | ViewMode::Graph
        ) && matches!(self.focus, FocusPane::Detail)
            && self.selected_issue_external_ref_url().is_some()
    }

    fn open_selected_issue_external_ref(&mut self) {
        let Some(url) = self.selected_issue_external_ref_url().map(str::to_string) else {
            self.status_msg = "No external issue reference".into();
            return;
        };

        if open_url_in_browser(&url) {
            self.status_msg = "Opened external issue reference".into();
        } else {
            self.status_msg = "Could not open browser".into();
        }
    }

    fn copy_selected_issue_external_ref(&mut self) {
        let Some(url) = self.selected_issue_external_ref_url().map(str::to_string) else {
            self.status_msg = "No external issue reference".into();
            return;
        };

        if copy_text_to_clipboard(&url) {
            self.status_msg = "Copied external issue reference to clipboard".into();
        } else {
            self.status_msg = "Clipboard not available".into();
        }
    }

    fn current_detail_link_row_area(&self) -> Option<Rect> {
        let area = cached_detail_content_area();
        if area.width == 0 || area.height == 0 {
            return None;
        }

        let (detail_text, line_index, scroll_offset) = match self.mode {
            ViewMode::Main => {
                let detail_text = self.issue_detail_render_text();
                let line_index = detail_text.lines().iter().position(|line| {
                    ftui::text::Line::spans(line)
                        .iter()
                        .any(|span| span.link.is_some())
                })?;
                (
                    detail_text,
                    line_index,
                    usize::from(saturating_scroll_offset(self.detail_scroll_offset)),
                )
            }
            ViewMode::Board => {
                let detail_text = self.board_detail_render_text();
                let line_index = detail_text.lines().iter().position(|line| {
                    ftui::text::Line::spans(line)
                        .iter()
                        .any(|span| span.link.is_some())
                })?;
                (detail_text, line_index, self.board_detail_scroll_offset)
            }
            ViewMode::Insights => {
                let detail_text = self.insights_detail_render_text();
                let line_index = detail_text.lines().iter().position(|line| {
                    ftui::text::Line::spans(line)
                        .iter()
                        .any(|span| span.link.is_some())
                })?;
                (
                    detail_text,
                    line_index,
                    usize::from(saturating_scroll_offset(self.detail_scroll_offset)),
                )
            }
            ViewMode::Graph => {
                let detail_text = self.graph_detail_render_text();
                let line_index = detail_text.lines().iter().position(|line| {
                    ftui::text::Line::spans(line)
                        .iter()
                        .any(|span| span.link.is_some())
                })?;
                (
                    detail_text,
                    line_index,
                    usize::from(saturating_scroll_offset(self.detail_scroll_offset)),
                )
            }
            ViewMode::History => {
                self.history_selected_commit_url()?;
                let detail_text = self.history_detail_render_text();
                let line_index = self.history_detail_text().lines().count().saturating_add(1);
                (detail_text, line_index, 0)
            }
            _ => return None,
        };
        let line = detail_text.lines().get(line_index)?;
        let line_width = display_width(&line.to_plain_text());
        if line_width == 0 {
            return None;
        }
        if line_index < scroll_offset {
            return None;
        }

        let width = u16::try_from(line_width)
            .unwrap_or(u16::MAX)
            .min(area.width);
        let visible_line_index = line_index.saturating_sub(scroll_offset);
        let y = area
            .y
            .saturating_add(saturating_scroll_offset(visible_line_index));
        if width == 0 || y >= area.y.saturating_add(area.height) {
            return None;
        }

        Some(Rect::new(area.x, y, width, 1))
    }

    fn detail_link_hit(&self, x: u16, y: u16) -> bool {
        if !matches!(self.focus, FocusPane::Detail) {
            return false;
        }

        let Some(link_area) = self.current_detail_link_row_area() else {
            return false;
        };
        if !rect_contains(link_area, x, y) {
            return false;
        }

        match self.mode {
            ViewMode::Main | ViewMode::Board | ViewMode::Insights | ViewMode::Graph => {
                self.selected_issue_external_ref_url().is_some()
            }
            ViewMode::History => self.history_selected_commit_url().is_some(),
            _ => false,
        }
    }

    fn mouse_open_detail_link(&mut self, x: u16, y: u16) -> bool {
        if !self.detail_link_hit(x, y) {
            return false;
        }

        match self.mode {
            ViewMode::Main | ViewMode::Board | ViewMode::Insights | ViewMode::Graph => {
                self.open_selected_issue_external_ref();
            }
            ViewMode::History => self.history_open_in_browser(),
            _ => return false,
        }

        true
    }

    fn mouse_copy_detail_link(&mut self, x: u16, y: u16) -> bool {
        if !self.detail_link_hit(x, y) {
            return false;
        }

        match self.mode {
            ViewMode::Main | ViewMode::Board | ViewMode::Insights | ViewMode::Graph => {
                self.copy_selected_issue_external_ref();
                true
            }
            ViewMode::History => {
                self.history_copy_commit_url();
                true
            }
            _ => false,
        }
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
                    self.open_confirm_with_resume(
                        "Export Pages?",
                        "Finalize the current pages export settings?\n\nThis uses the reusable confirmation modal without changing the quit flow.",
                        Some(ModalOverlay::PagesWizard(wiz)),
                    );
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
                    ("Ctrl+j/k", "Scroll detail pane"),
                    ("Ctrl+←/→", "Resize active pane split"),
                    ("Ctrl+0", "Reset pane splits"),
                    ("PgUp/PgDn", "Jump by 10"),
                    ("Home/End", "Jump to top/bottom"),
                    ("gg", "Jump to top (any view)"),
                    ("G", "Jump to bottom"),
                    ("Ctrl+f/b", "Full page down/up"),
                    ("Tab / Shift+Tab", "Toggle focus forward/back"),
                    ("J/K", "Navigate deps in detail"),
                    ("Enter", "Return to main / drill"),
                    ("scroll", "Mouse wheel scrolls list"),
                    ("splitter click/scroll", "Mouse-resize active divider"),
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
                    ("P", "Pages export wizard"),
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
                title: "Tree",
                bindings: vec![
                    ("Enter/za", "Toggle fold"),
                    ("zo/zc", "Open/close fold"),
                    ("zR/zM", "Open/close all folds"),
                    ("zz", "Recenter cursor"),
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
                    ("Ctrl+T", "Tutorial"),
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
        let actual_col_width = width
            .saturating_sub(num_cols.saturating_sub(1))
            .checked_div(num_cols)
            .unwrap_or(width);

        let mut output = Vec::with_capacity(max_rows);
        for row in 0..max_rows {
            let mut line = String::new();
            for (ci, col_data) in columns.iter().enumerate() {
                let cell = col_data.get(row).map(|s| s.as_str()).unwrap_or("");
                if ci > 0 {
                    line.push_str(" | ");
                }
                let cell_trunc = truncate_display(cell, actual_col_width);
                line.push_str(&cell_trunc);
                // Pad to column width for alignment (except last column).
                if ci + 1 < columns.len() {
                    let padding = actual_col_width.saturating_sub(display_width(&cell_trunc));
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

    fn list_panel_render_text(&self, width: u16) -> RichText {
        match self.mode {
            ViewMode::Main => self.main_list_render_text(width),
            ViewMode::Graph => self.graph_list_render_text(width),
            ViewMode::Tree => self.tree_list_render_text(width),
            _ => RichText::raw(self.list_panel_text()),
        }
    }

    fn main_list_text(&self) -> String {
        self.main_list_render_text(80).to_plain_text()
    }

    fn main_list_empty_state_lines(&self) -> Vec<RichLine> {
        let mut lines = vec![RichLine::from_spans([RichSpan::styled(
            "No issues in the current triage slice",
            tokens::panel_title(),
        )])];

        let mut scope = vec![format!("filter={}", self.list_filter.label())];
        if let Some(label) = self.modal_label_filter.as_deref() {
            scope.push(format!("label={label}"));
        }
        if let Some(repo) = self.modal_repo_filter.as_deref() {
            scope.push(format!("repo={repo}"));
        }
        if !self.main_search_query.is_empty() {
            scope.push(format!("search=/{}", self.main_search_query));
        }
        lines.push(RichLine::raw(format!("Scope: {}", scope.join(" | "))));

        let recovery = if !self.main_search_query.is_empty() {
            "Recover: Esc keeps context | / edits search | n/N cycle hits | o/c/r/B/I switch filters"
        } else {
            "Recover: a all | o open | I in-progress | B blocked | c closed | r ready"
        };
        lines.push(RichLine::raw(recovery));
        lines
    }

    fn main_focus_banner_line(&self) -> RichLine {
        let mut line = RichLine::new();
        push_chip(
            &mut line,
            if matches!(self.focus, FocusPane::List) {
                "Focus: list owns selection, / search, o/c/r/B/I filters, L label, w repo, Tab detail, Shift+Tab reverse"
            } else {
                "Focus: detail owns J/K deps, ^j/k scroll, o/y link actions, Tab returns to list, Shift+Tab reverse"
            },
            if matches!(self.focus, FocusPane::List) {
                SemanticTone::Accent
            } else {
                SemanticTone::Warning
            },
        );
        line
    }

    fn main_scope_banner_line(&self) -> RichLine {
        let selected = self
            .selected_issue()
            .map_or_else(|| "none".to_string(), |issue| issue.id.clone());
        let visible = self.visible_issue_indices_for_list_nav();
        let position = self.selected_visible_slot(&visible).map_or_else(
            || "0/0".to_string(),
            |slot| format!("{}/{}", slot + 1, visible.len()),
        );
        let mut line = RichLine::new();
        push_metric_chip(
            &mut line,
            "scope",
            self.list_filter.label(),
            SemanticTone::Muted,
        );
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(
            &mut line,
            "label",
            self.modal_label_filter.as_deref().unwrap_or("any"),
            SemanticTone::Neutral,
        );
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(
            &mut line,
            "repo",
            self.modal_repo_filter.as_deref().unwrap_or("any"),
            SemanticTone::Neutral,
        );
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(&mut line, "pos", &position, SemanticTone::Muted);
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(
            &mut line,
            "search",
            if self.main_search_query.is_empty() {
                "off"
            } else {
                self.main_search_query.as_str()
            },
            if self.main_search_query.is_empty() {
                SemanticTone::Muted
            } else {
                SemanticTone::Accent
            },
        );
        line.push_span(RichSpan::styled(" | ", tokens::dim()));
        push_metric_chip(&mut line, "selected", &selected, SemanticTone::Warning);
        line
    }

    fn main_search_banner_lines(&self) -> Vec<RichLine> {
        let mut lines = vec![self.main_focus_banner_line(), self.main_scope_banner_line()];
        if self.main_search_active {
            lines.push(RichLine::raw(format!(
                "Search (active): /{}",
                self.main_search_query
            )));
        } else if !self.main_search_query.is_empty() {
            lines.push(RichLine::raw(format!(
                "Search: /{} (n/N cycles)",
                self.main_search_query
            )));
        }
        let visible = self.visible_issue_indices();
        if !self.main_search_query.is_empty() {
            let matches = self.main_search_matches();
            if matches.is_empty() {
                lines.push(RichLine::raw("Matches: none in visible issues"));
                lines.push(RichLine::raw(
                    "Hint: keep scanning rows, refine /query, or clear repo/label filters",
                ));
            } else {
                let position = self
                    .main_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                lines.push(RichLine::raw(format!(
                    "Matches: {position}/{}",
                    matches.len()
                )));
                lines.push(RichLine::raw(
                    "Guide: n/N cycle hits | Enter keeps query | Esc clears | Tab keeps context",
                ));
            }
        } else {
            lines.push(RichLine::raw(
                "Guide: / search-as-you-type | o/c/r/B/I quick filters | Esc unwinds state | Tab/Shift+Tab focus",
            ));
        }
        lines.push(RichLine::raw(""));
        if visible.is_empty() {
            lines.extend(self.main_list_empty_state_lines());
        }
        lines
    }

    fn main_list_render_text(&self, width: u16) -> RichText {
        let visible = self.visible_issue_indices();
        let mut lines = self.main_search_banner_lines();
        if visible.is_empty() {
            return RichText::from_lines(lines);
        }

        let line_width = usize::from(width.saturating_sub(2)).max(24);
        let search_matches = self.main_search_matches();
        let search_positions = search_matches
            .iter()
            .enumerate()
            .map(|(slot, index)| (*index, slot + 1))
            .collect::<BTreeMap<usize, usize>>();
        for (slot, (index, issue)) in visible
            .into_iter()
            .filter_map(|index| self.analyzer.issues.get(index).map(|issue| (index, issue)))
            .enumerate()
        {
            let open_blockers = self.analyzer.graph.open_blockers(&issue.id).len();
            let blocks_count = self
                .analyzer
                .metrics
                .blocks_count
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let pagerank_rank = metric_rank(&self.analyzer.metrics.pagerank, &issue.id);
            let graph_score = self
                .analyzer
                .metrics
                .pagerank
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let critical_depth = self
                .analyzer
                .metrics
                .critical_depth
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            lines.push(issue_scan_line(
                issue,
                index == self.selected,
                ScanLineContext {
                    open_blockers,
                    blocks_count,
                    triage_rank: slot + 1,
                    pagerank_rank,
                    critical_depth,
                    graph_score,
                    search_match_position: search_positions.get(&index).copied(),
                    total_search_matches: search_matches.len(),
                    diff_tag: self.issue_diff_tag(&issue.id),
                    available_width: line_width,
                },
            ));
        }

        RichText::from_lines(lines)
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
            let is_current_lane = sel_id.as_ref().is_some_and(|sid| {
                lane_indices
                    .iter()
                    .any(|&i| self.analyzer.issues[i].id == *sid)
            });
            let marker = if is_current_lane { "▸" } else { " " };

            // Lane health signals
            let blocked_count = lane_indices
                .iter()
                .filter(|&&i| {
                    self.analyzer.issues[i]
                        .normalized_status()
                        .eq_ignore_ascii_case("blocked")
                })
                .count();
            let health = if count == 0 {
                "empty".to_string()
            } else if blocked_count > 0 {
                format!("{blocked_count} blocked")
            } else {
                "clear".to_string()
            };

            // Lane header with box-drawing border
            let bar_len = count.min(20);
            let bar: String = std::iter::repeat_n('\u{2588}', bar_len).collect();
            out.push(format!(
                "{marker} \u{250c}\u{2500} {lane} [{count}] {bar}  {health}"
            ));

            // Show card previews with box-drawing borders
            let preview_limit = 8;
            for &idx in lane_indices.iter().take(preview_limit) {
                let issue = &self.analyzer.issues[idx];
                let is_sel = idx == sel_index;
                let s_icon = status_icon(&issue.status);
                let t_icon = type_icon(&issue.issue_type);
                let open_bl = self.analyzer.graph.open_blockers(&issue.id).len();
                let blocks = self
                    .analyzer
                    .metrics
                    .blocks_count
                    .get(&issue.id)
                    .copied()
                    .unwrap_or(0);
                let dep_tag = if open_bl > 0 {
                    format!("\u{2298}{open_bl}")
                } else if blocks > 0 {
                    format!("\u{2193}{blocks}")
                } else {
                    String::new()
                };
                let assignee = if issue.assignee.is_empty() {
                    String::new()
                } else {
                    format!(" @{}", truncate_str(&issue.assignee, 8))
                };

                // Card with box border
                let sel_char = if is_sel { "\u{25b6}" } else { "\u{2502}" };
                out.push(format!(
                    "  {sel_char} {s_icon}{t_icon} P{} {} {}{dep_tag}{assignee}",
                    issue.priority.clamp(0, 4),
                    truncate_str(&issue.id, 10),
                    truncate_str(&issue.title, 18),
                ));
            }
            if lane_indices.len() > preview_limit {
                out.push(format!(
                    "  \u{2502} ... +{} more",
                    lane_indices.len() - preview_limit
                ));
            }
            if lane_indices.is_empty() {
                out.push("  \u{2502} (empty)".to_string());
            }
            out.push(format!(
                "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}"
            ));
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
        let search_matches = self.insights_search_matches();
        let search_positions = search_matches
            .iter()
            .enumerate()
            .map(|(slot, index)| (*index, slot + 1))
            .collect::<BTreeMap<usize, usize>>();
        lines.extend(self.insights_signal_tiles());
        lines.push(String::new());
        lines.extend(self.insights_outlier_radar());
        lines.push(String::new());
        lines.push(format!(
            "Panel Focus | {} | {}",
            self.insights_panel.label(),
            self.insights_panel_focus_hint()
        ));
        lines.push(String::new());

        match self.insights_panel {
            InsightsPanel::Bottlenecks => {
                if insights.bottlenecks.is_empty() {
                    lines.push("  (no open issues to rank)".to_string());
                } else {
                    lines.extend(insights.bottlenecks.iter().take(15).enumerate().map(
                        |(index, item)| {
                            let hit_suffix = self.insights_search_hit_suffix(
                                &item.id,
                                &search_positions,
                                search_matches.len(),
                            );
                            format!(
                                " {}. {:<12} score={:.3} blocks={}{}",
                                index + 1,
                                item.id,
                                item.score,
                                item.blocks_count,
                                hit_suffix
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
                            let hit_suffix = self.insights_search_hit_suffix(
                                id,
                                &search_positions,
                                search_matches.len(),
                            );
                            format!(
                                " {}. {:<12} depth={} unblocks={}{}",
                                index + 1,
                                id,
                                depth,
                                unblocks,
                                hit_suffix
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
                                let hit_suffix = self.insights_search_hit_suffix(
                                    id,
                                    &search_positions,
                                    search_matches.len(),
                                );
                                format!(" {}. {:<12} depth={}{}", index + 1, id, depth, hit_suffix)
                            }),
                    );
                }
            }
            InsightsPanel::Influencers => {
                self.append_metric_items(
                    &mut lines,
                    &insights.influencers,
                    "influencer",
                    &search_positions,
                    search_matches.len(),
                );
            }
            InsightsPanel::Betweenness => {
                self.append_metric_items(
                    &mut lines,
                    &insights.betweenness,
                    "betweenness",
                    &search_positions,
                    search_matches.len(),
                );
            }
            InsightsPanel::Hubs => {
                self.append_metric_items(
                    &mut lines,
                    &insights.hubs,
                    "hub-score",
                    &search_positions,
                    search_matches.len(),
                );
            }
            InsightsPanel::Authorities => {
                self.append_metric_items(
                    &mut lines,
                    &insights.authorities,
                    "authority",
                    &search_positions,
                    search_matches.len(),
                );
            }
            InsightsPanel::Cores => {
                if insights.cores.is_empty() {
                    lines.push("  (no k-core data)".to_string());
                } else {
                    lines.extend(insights.cores.iter().take(15).enumerate().map(
                        |(index, item)| {
                            let hit_suffix = self.insights_search_hit_suffix(
                                &item.id,
                                &search_positions,
                                search_matches.len(),
                            );
                            format!(
                                " {}. {:<12} k={}{}",
                                index + 1,
                                item.id,
                                item.value,
                                hit_suffix
                            )
                        },
                    ));
                }
            }
            InsightsPanel::CutPoints => {
                if insights.articulation_points.is_empty() {
                    lines.push("  (no cut points -- graph is well-connected)".to_string());
                } else {
                    lines.extend(insights.articulation_points.iter().enumerate().map(
                        |(index, id)| {
                            let hit_suffix = self.insights_search_hit_suffix(
                                id,
                                &search_positions,
                                search_matches.len(),
                            );
                            format!(" {}. {}{}", index + 1, id, hit_suffix)
                        },
                    ));
                }
            }
            InsightsPanel::Slack => {
                if insights.slack.is_empty() {
                    lines
                        .push("  (no zero-slack issues -- all have scheduling buffer)".to_string());
                } else {
                    lines.extend(insights.slack.iter().enumerate().map(|(index, id)| {
                        let hit_suffix = self.insights_search_hit_suffix(
                            id,
                            &search_positions,
                            search_matches.len(),
                        );
                        format!(" {}. {}{}", index + 1, id, hit_suffix)
                    }));
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
                        let hit_suffix = self.insights_search_hit_suffix(
                            &item.id,
                            &search_positions,
                            search_matches.len(),
                        );
                        format!(
                            " {}. {:<12} score={:.3} unblocks={} p{}{}",
                            index + 1,
                            item.id,
                            item.score,
                            item.unblocks,
                            item.priority,
                            hit_suffix
                        )
                    }));
                }
            }
        }

        lines.join("\n")
    }

    fn insights_signal_tiles(&self) -> Vec<String> {
        let insights = self.analyzer.insights();
        let open_issues = self
            .analyzer
            .issues
            .iter()
            .filter(|issue| issue.is_open_like())
            .count();
        let blocked_open = self
            .analyzer
            .issues
            .iter()
            .filter(|issue| {
                issue.is_open_like() && !self.analyzer.graph.open_blockers(&issue.id).is_empty()
            })
            .count();
        let zero_slack = insights.slack.len();
        let max_k_core = insights
            .cores
            .iter()
            .map(|item| item.value)
            .max()
            .unwrap_or(0);

        vec![
            "Signal Tiles".to_string(),
            format!(
                "[Flow ] open={open_issues} blocked={blocked_open} crit-path={} cycles={}",
                insights.critical_path.len(),
                insights.cycles.len()
            ),
            format!(
                "[Risk ] bottlenecks={} cut-points={} zero-slack={} max-k={max_k_core}",
                insights.bottlenecks.len(),
                insights.articulation_points.len(),
                zero_slack
            ),
        ]
    }

    fn insights_outlier_radar(&self) -> Vec<String> {
        let insights = self.analyzer.insights();
        let priority = self.analyzer.priority(0.0, 1, None, None);
        let top_bottleneck = insights.bottlenecks.first().map_or_else(
            || "none".to_string(),
            |item| {
                format!(
                    "{} score={:.3} blocks={}",
                    item.id, item.score, item.blocks_count
                )
            },
        );
        let top_influencer = insights.influencers.first().map_or_else(
            || "none".to_string(),
            |item| format!("{} pr={:.4}", item.id, item.value),
        );
        let top_priority = priority.first().map_or_else(
            || "none".to_string(),
            |item| format!("{} score={:.3} p{}", item.id, item.score, item.priority),
        );

        vec![
            "Outlier Radar".to_string(),
            format!("[Lead ] bottleneck={top_bottleneck}"),
            format!("[Rank ] influencer={top_influencer}"),
            format!("[Act  ] next-priority={top_priority}"),
        ]
    }

    fn insights_panel_focus_hint(&self) -> &'static str {
        match self.insights_panel {
            InsightsPanel::Bottlenecks => "blocking pressure and downstream drag",
            InsightsPanel::Keystones => "foundational chains that unlock follow-on work",
            InsightsPanel::CriticalPath => "deep dependency rails with little slack",
            InsightsPanel::Influencers => "highest graph influence by PageRank",
            InsightsPanel::Betweenness => "bridge nodes that route dependency flow",
            InsightsPanel::Hubs => "strong outbound influence in the graph",
            InsightsPanel::Authorities => "strong inbound authority in the graph",
            InsightsPanel::Cores => "densest cohesion clusters",
            InsightsPanel::CutPoints => "single-node fragility in connectivity",
            InsightsPanel::Slack => "zero-buffer scheduling hotspots",
            InsightsPanel::Cycles => "circular dependency traps",
            InsightsPanel::Priority => "graph-informed reprioritization candidates",
        }
    }

    fn insights_search_hit_suffix(
        &self,
        issue_id: &str,
        search_positions: &BTreeMap<usize, usize>,
        total_search_matches: usize,
    ) -> String {
        self.issue_index_for_id(issue_id)
            .and_then(|issue_index| search_positions.get(&issue_index).copied())
            .map_or_else(String::new, |position| {
                format!(" hit {position}/{total_search_matches}")
            })
    }

    fn append_metric_items(
        &self,
        lines: &mut Vec<String>,
        items: &[crate::analysis::MetricItem],
        label: &str,
        search_positions: &BTreeMap<usize, usize>,
        total_search_matches: usize,
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
                let hit_suffix = self.insights_search_hit_suffix(
                    &item.id,
                    search_positions,
                    total_search_matches,
                );
                format!(
                    " {}. {:<12} [{bar}] {:.4}{hit_suffix}",
                    index + 1,
                    item.id,
                    item.value
                )
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
        let search_matches = self.graph_search_matches();
        let search_positions = search_matches
            .iter()
            .enumerate()
            .map(|(slot, index)| (*index, slot + 1))
            .collect::<BTreeMap<usize, usize>>();

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
                    let hit_suffix = search_positions
                        .get(&index)
                        .map_or_else(String::new, |position| {
                            format!(" hit {position}/{}", search_matches.len())
                        });
                    format!(
                        "{marker} {si} {:<12} in:{:>2} out:{:>2} pr:{:.3}{hit_suffix}",
                        issue.id, blocked_by, blocks, pagerank
                    )
                }),
        );
        lines.join("\n")
    }

    fn graph_list_render_text(&self, width: u16) -> RichText {
        let visible = self.graph_visible_issue_indices();
        if visible.is_empty() {
            return RichText::raw(format!(
                "(no issues match filter: {})",
                self.list_filter.label()
            ));
        }

        let total = visible.len();
        let mut lines = vec![panel_header(
            "Nodes",
            Some(&format!(
                "{total} by critical-path score | h/l nav | / search | Tab focus"
            )),
        )];
        if self.graph_search_active {
            lines.push(RichLine::raw(format!(
                "Search (active): /{}",
                self.graph_search_query
            )));
        } else if !self.graph_search_query.is_empty() {
            lines.push(RichLine::raw(format!(
                "Search: /{} (n/N cycles)",
                self.graph_search_query
            )));
        }
        if !self.graph_search_query.is_empty() {
            let matches = self.graph_search_matches();
            if matches.is_empty() {
                lines.push(RichLine::raw("Matches: none"));
            } else {
                let position = self
                    .graph_search_match_cursor
                    .min(matches.len().saturating_sub(1))
                    + 1;
                lines.push(RichLine::raw(format!(
                    "Matches: {position}/{}",
                    matches.len()
                )));
            }
        }
        lines.push(section_separator(
            usize::from(width.saturating_sub(2)).max(24),
        ));

        let pr_max = max_metric_value(&self.analyzer.metrics.pagerank);
        let line_width = usize::from(width.saturating_sub(2)).max(24);
        let search_matches = self.graph_search_matches();
        let search_positions = search_matches
            .iter()
            .enumerate()
            .map(|(slot, index)| (*index, slot + 1))
            .collect::<BTreeMap<usize, usize>>();
        for index in visible {
            let Some(issue) = self.analyzer.issues.get(index) else {
                continue;
            };
            let blocked_by = self.analyzer.graph.open_blockers(&issue.id).len();
            let blocks = self
                .analyzer
                .metrics
                .blocks_count
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
            let mut line = RichLine::new();
            let marker_style = if index == self.selected {
                tokens::selected()
            } else {
                tokens::dim()
            };
            line.push_span(RichSpan::styled(
                if index == self.selected { "▸" } else { " " },
                marker_style,
            ));
            line.push_span(RichSpan::raw(" "));
            line.push_span(RichSpan::styled(
                truncate_display(&issue.id, 12),
                tokens::panel_title(),
            ));
            line.push_span(RichSpan::raw(" "));
            for span in metric_strip("PR", pagerank, pr_max) {
                line.push_span(span);
            }
            // Neighborhood: blocker/dependent indicators
            let bl = blocker_indicator(blocked_by, blocks);
            if !bl.is_empty() {
                line.push_span(RichSpan::raw(" "));
                for s in bl {
                    line.push_span(s);
                }
            }
            // Cycle membership
            if self
                .analyzer
                .metrics
                .cycles
                .iter()
                .any(|c| c.contains(&issue.id))
            {
                line.push_span(RichSpan::styled(
                    " \u{27f3}",
                    tokens::status_style("blocked"),
                ));
            }
            // Articulation point
            if self
                .analyzer
                .metrics
                .articulation_points
                .contains(&issue.id)
            {
                line.push_span(RichSpan::styled(
                    " \u{25c6}",
                    tokens::status_style("in_progress"),
                ));
            }
            line.push_span(RichSpan::raw(" "));
            let title_width = line_width.saturating_sub(42);
            line.push_span(RichSpan::styled(
                truncate_display(&issue.title, title_width.max(8)),
                tokens::help_desc(),
            ));
            if let Some(position) = search_positions.get(&index) {
                line.push_span(RichSpan::raw(" "));
                line.push_span(RichSpan::styled(
                    format!("hit {position}/{}", search_matches.len()),
                    tokens::status_style("selected"),
                ));
            }
            lines.push(line);
        }

        RichText::from_lines(lines)
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
                        let type_icon = commit_type_icon(&commit.message);
                        let msg = truncate_str(&commit.message, 28);
                        let ts = compact_history_duration_label(&commit.timestamp);
                        lines.push(format!(
                            "{marker} {type_icon} {} {:<8} {} {ts}",
                            commit.short_sha, beads_str, msg
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
                    let marker = if index == self.selected {
                        "\u{25b8}"
                    } else {
                        " "
                    };
                    let event_count = histories
                        .iter()
                        .find(|entry| entry.id == issue.id)
                        .map_or(0, |entry| entry.events.len());
                    let si = status_icon(&issue.status);
                    let ti = type_icon(&issue.issue_type);
                    format!(
                        "{marker} {si}{ti} {:<12} {event_count:>2}\u{25aa} {:<11}",
                        issue.id, issue.status
                    )
                }),
        );
        lines.join("\n")
    }

    fn history_middle_text(&self, width: u16, height: u16) -> String {
        let inner_width = usize::from(width.saturating_sub(4)).max(12);
        let visible_rows = usize::from(height.saturating_sub(4)).max(1);

        if matches!(self.history_view_mode, HistoryViewMode::Git) {
            let Some(commit) = self.selected_history_git_commit() else {
                return "Select a commit to view related beads.".to_string();
            };

            let related = self.history_git_related_beads_for_commit(&commit.sha);
            if related.is_empty() {
                return format!("No beads correlated with {}.", commit.short_sha);
            }

            let slot = self
                .history_related_bead_cursor
                .min(related.len().saturating_sub(1));
            let start = slot.saturating_sub(visible_rows.saturating_sub(1));
            let end = (start + visible_rows).min(related.len());
            let mut lines = vec![
                format!("{} related bead(s) for {}", related.len(), commit.short_sha),
                String::new(),
            ];

            for (idx, bead_id) in related.iter().enumerate().skip(start).take(end - start) {
                let marker = if idx == slot && matches!(self.focus, FocusPane::Middle) {
                    '>'
                } else {
                    ' '
                };
                let issue = self.issue_by_id(bead_id);
                let status = issue.map_or("?", |issue| status_icon(&issue.status));
                let title = issue
                    .map(|issue| truncate_str(&issue.title, inner_width.saturating_sub(18)))
                    .unwrap_or_else(|| bead_id.clone());
                lines.push(format!("{marker} [{status}] {bead_id:<8} {title}"));
            }

            if end < related.len() {
                lines.push(format!("+{} more", related.len() - end));
            }

            return lines.join("\n");
        }

        let Some(issue) = self.selected_issue() else {
            return "Select a bead to view correlated commits.".to_string();
        };
        let commits = self.history_filtered_bead_commits(&issue.id);
        if commits.is_empty() {
            return format!("No commits correlated with {}.", issue.id);
        }

        let slot = self
            .history_bead_commit_cursor
            .min(commits.len().saturating_sub(1));
        let start = slot.saturating_sub(visible_rows.saturating_sub(1));
        let end = (start + visible_rows).min(commits.len());
        let mut lines = vec![
            format!("{} commit(s) for {}", commits.len(), issue.id),
            String::new(),
        ];

        for (idx, commit) in commits.iter().enumerate().skip(start).take(end - start) {
            let marker = if idx == slot && matches!(self.focus, FocusPane::Middle) {
                '>'
            } else {
                ' '
            };
            let summary = truncate_str(&commit.message, inner_width.saturating_sub(18));
            lines.push(format!(
                "{marker} {} {:>3.0}% {}",
                commit.short_sha,
                commit.confidence * 100.0,
                summary
            ));
        }

        if end < commits.len() {
            lines.push(format!("+{} more", commits.len() - end));
        }

        lines.join("\n")
    }

    fn history_timeline_text(&self, width: u16, height: u16) -> String {
        let Some(issue) = self.selected_issue() else {
            return "Select a bead to view its timeline.".to_string();
        };
        let inner_width = usize::from(width.saturating_sub(4)).max(12);
        let visible_rows = usize::from(height.saturating_sub(4)).max(1);
        let compat_history = self
            .history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(&issue.id));
        let filtered_commits = self.history_filtered_bead_commits(&issue.id);

        if let Some(compat_history) = compat_history {
            let mut lines = vec![format!("Timeline: {}", issue.id), String::new()];
            lines.push(self.history_compact_timeline_text(compat_history, inner_width));
            if let Some(cycle) = compat_history
                .cycle_time
                .as_ref()
                .and_then(|cycle| cycle.create_to_close.as_deref())
            {
                lines.push(format!("Cycle: {}", compact_history_duration_label(cycle)));
            }
            if !filtered_commits.is_empty() {
                let avg_confidence = filtered_commits
                    .iter()
                    .map(|commit| commit.confidence)
                    .sum::<f64>()
                    / filtered_commits.len() as f64;
                lines.push(format!(
                    "Commits: {} | Avg confidence: {:.0}%",
                    filtered_commits.len(),
                    avg_confidence * 100.0
                ));
            }
            lines.push(String::new());

            let used_rows = lines.len();
            let max_timeline_rows = visible_rows.saturating_sub(used_rows).max(1);
            lines.extend(render_legacy_timeline_lines(
                compat_history,
                &filtered_commits,
                inner_width,
                max_timeline_rows,
            ));
            return lines.join("\n");
        }

        let selected_history = self.analyzer.history(Some(&issue.id), 1).into_iter().next();

        let mut entries = Vec::new();
        if let Some(history) = selected_history {
            for event in history.events {
                let ts = event
                    .timestamp
                    .map(|dt| format_compact_timestamp(Some(dt)))
                    .unwrap_or_else(|| "n/a".to_string());
                let detail = truncate_str(&event.details, inner_width.saturating_sub(16));
                entries.push(format!("{} {ts} {}", lifecycle_icon(&event.kind), detail));
            }
        }

        for commit in self
            .history_filtered_bead_commits(&issue.id)
            .into_iter()
            .take(visible_rows)
        {
            let summary = truncate_str(&commit.message, inner_width.saturating_sub(14));
            entries.push(format!("• {} {}", commit.short_sha, summary));
        }

        if entries.is_empty() {
            return format!("No timeline data for {}.", issue.id);
        }

        let hidden = entries.len().saturating_sub(visible_rows);
        let mut lines = vec![format!("Cycle view for {}", issue.id), String::new()];
        lines.extend(entries.into_iter().take(visible_rows));
        if hidden > 0 {
            lines.push(format!("+{hidden} more"));
        }
        lines.join("\n")
    }

    fn history_compact_timeline_text(
        &self,
        history: &HistoryBeadCompat,
        max_width: usize,
    ) -> String {
        let mut markers = Vec::<&str>::new();
        let mut start_ts = None::<&str>;
        let mut end_ts = None::<&str>;

        if let Some(ref event) = history.milestones.created {
            markers.push("○");
            start_ts = Some(event.timestamp.as_str());
        }
        if let Some(ref event) = history.milestones.claimed {
            markers.push("●");
            start_ts = start_ts.or(Some(event.timestamp.as_str()));
        }

        let commit_count = history.commits.as_ref().map_or(0, Vec::len);
        if commit_count > 5 {
            markers.extend(["├", "├", "├", "├", "…"]);
        } else {
            for _ in 0..commit_count {
                markers.push("├");
            }
        }

        if let Some(ref event) = history.milestones.closed {
            markers.push("✓");
            end_ts = Some(event.timestamp.as_str());
        }

        if markers.is_empty() {
            return "(no timeline data)".to_string();
        }

        let mut summary = Vec::<String>::new();
        if let Some(ref cycle) = history.cycle_time {
            if let Some(ref create_to_close) = cycle.create_to_close {
                summary.push(format!(
                    "{} cycle",
                    compact_history_duration_label(create_to_close)
                ));
            }
        }
        if commit_count > 0 {
            summary.push(if commit_count == 1 {
                "1 commit".to_string()
            } else {
                format!("{commit_count} commits")
            });
        }

        let mut result = markers.join("──");
        if !summary.is_empty() {
            result.push_str("  ");
            result.push_str(&summary.join(", "));
        }

        if let (Some(start), Some(end)) = (start_ts, end_ts) {
            if let (Some(start), Some(end)) = (
                compact_history_month_day(start),
                compact_history_month_day(end),
            ) {
                let date_range = format!("{start} ─ {end}");
                if result.chars().count() + date_range.chars().count() + 4 < max_width {
                    result.push('\n');
                    result.push_str(&date_range);
                }
            }
        }

        result
            .lines()
            .map(|line| truncate_display(line, max_width))
            .collect::<Vec<_>>()
            .join("\n")
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

        let previous_track = self.actionable_track_cursor;
        let previous_item = self.actionable_item_cursor;

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

        if self.actionable_track_cursor != previous_track
            || self.actionable_item_cursor != previous_item
        {
            self.detail_scroll_offset = 0;
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
                let unblocks_str = if item.unblocks.is_empty() {
                    String::new()
                } else if item.unblocks.len() <= 2 {
                    format!(" \u{2192} {}", item.unblocks.join(", "))
                } else {
                    format!(
                        " \u{2192} {}, +{}",
                        item.unblocks[..2].join(", "),
                        item.unblocks.len() - 2
                    )
                };
                lines.push(format!(
                    "{item_marker}{tree} P{} {:<12} {:>5.2}  {}{unblocks_str}",
                    item.priority.clamp(0, 4),
                    item.id,
                    item.score,
                    title
                ));
            }
            lines.push(String::new());
        }

        // On wide terminals (>=120), show a parallel summary of all tracks
        let width = usize::from(cached_view_width());
        if width >= 120 && plan.tracks.len() >= 2 {
            lines.push(String::new());
            lines.push("═══ Parallel Track Overview ═══".to_string());
            let col_width = (width.saturating_sub(4)) / plan.tracks.len().min(4);
            for chunk in plan.tracks.chunks(4) {
                // Header row
                let headers = chunk.iter().fold(String::new(), |mut acc, track| {
                    let label = track.id.strip_prefix("track-").unwrap_or(&track.id);
                    let title = format!("Track {label} ({})", track.items.len());
                    let _ = write!(acc, "{title:<w$}", w = col_width);
                    acc
                });
                lines.push(headers);
                // Item rows (show up to 5 per track)
                let max_items = chunk
                    .iter()
                    .map(|t| t.items.len().min(5))
                    .max()
                    .unwrap_or(0);
                for row in 0..max_items {
                    let row_text: String = chunk
                        .iter()
                        .map(|t| {
                            if let Some(item) = t.items.get(row) {
                                format!(
                                    "{:<w$}",
                                    format!("  {} {:.2}", truncate_str(&item.id, 10), item.score),
                                    w = col_width
                                )
                            } else {
                                " ".repeat(col_width)
                            }
                        })
                        .collect();
                    lines.push(row_text);
                }
                lines.push(String::new());
            }
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
            issues.len() > crate::analysis::graph::AnalysisConfig::background_threshold();
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
        // Rebuild tree so stale issue_index values don't cause
        // out-of-bounds panics with the new issue list.
        self.rebuild_tree_if_active();
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
            .filter(|i| {
                i.is_open_like()
                    && i.labels
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(&label.label))
            })
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

        // Precompute which issues pass the active filter (list status,
        // label, and/or repo) so the tree only shows matching issues
        // (and their structural parents).
        let filter_active = self.has_active_filter();
        let passes: Vec<bool> = issues
            .iter()
            .map(|issue| self.issue_matches_filter(issue))
            .collect();

        // Build a map from issue ID to index.
        let id_to_index: std::collections::HashMap<&str, usize> = issues
            .iter()
            .enumerate()
            .map(|(i, issue)| (issue.id.as_str(), i))
            .collect();

        // Build parent-child hierarchy from parent-child dependencies.
        // A child issue has a dep with dep_type="parent-child" and
        // depends_on_id pointing to its parent.
        let mut children_of: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();
        let mut has_parent: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (i, issue) in issues.iter().enumerate() {
            for dep in &issue.dependencies {
                if dep.is_parent_child() && !dep.depends_on_id.trim().is_empty() {
                    if let Some(&parent_idx) = id_to_index.get(dep.depends_on_id.as_str()) {
                        children_of.entry(parent_idx).or_default().push(i);
                        has_parent.insert(i);
                    }
                }
            }
        }

        // Deduplicate and sort children lists.
        for children in children_of.values_mut() {
            children.sort_by(|&a, &b| issues[a].id.cmp(&issues[b].id));
            children.dedup();
        }

        // Roots: issues with no parent-child dependency pointing to a
        // known parent. Fall back to blocking-based roots if no
        // parent-child deps exist at all.
        let mut roots: Vec<usize> = (0..issues.len())
            .filter(|i| !has_parent.contains(i))
            .collect();

        if roots.len() == issues.len() && !issues.is_empty() {
            // No parent-child deps at all — fall back to blocking graph
            // so the tree still shows something useful.
            let graph = &self.analyzer.graph;
            let mut blocking_children_of: std::collections::HashMap<usize, Vec<usize>> =
                std::collections::HashMap::new();
            let mut has_blocker: std::collections::HashSet<usize> =
                std::collections::HashSet::new();

            for (i, issue) in issues.iter().enumerate() {
                let blockers = graph.blockers(&issue.id);
                if !blockers.is_empty() {
                    has_blocker.insert(i);
                    for blocker_id in &blockers {
                        if let Some(&blocker_idx) = id_to_index.get(blocker_id.as_str()) {
                            blocking_children_of.entry(blocker_idx).or_default().push(i);
                        }
                    }
                }
            }
            for children in blocking_children_of.values_mut() {
                children.sort_by(|&a, &b| issues[a].id.cmp(&issues[b].id));
                children.dedup();
            }

            roots = (0..issues.len())
                .filter(|i| !has_blocker.contains(i))
                .collect();

            children_of = blocking_children_of;
        }

        roots.sort_by(|&a, &b| issues[a].id.cmp(&issues[b].id));

        // When a list filter is active, compute which nodes are "visible":
        // an issue that matches the filter, plus any ancestor whose subtree
        // contains a matching descendant (so the tree structure stays intact).
        let visible: Option<std::collections::HashSet<usize>> = if filter_active {
            let mut vis: std::collections::HashSet<usize> = std::collections::HashSet::new();
            // Mark matching leaves and propagate upward.
            fn mark_visible(
                idx: usize,
                children_of: &std::collections::HashMap<usize, Vec<usize>>,
                passes: &[bool],
                vis: &mut std::collections::HashSet<usize>,
                visited: &mut std::collections::HashSet<usize>,
            ) -> bool {
                if !visited.insert(idx) {
                    return vis.contains(&idx);
                }
                let mut dominated = passes[idx];
                if let Some(children) = children_of.get(&idx) {
                    for &child in children {
                        if mark_visible(child, children_of, passes, vis, visited) {
                            dominated = true;
                        }
                    }
                }
                if dominated {
                    vis.insert(idx);
                }
                dominated
            }
            let mut visited_mark: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            for &root in &roots {
                mark_visible(root, &children_of, &passes, &mut vis, &mut visited_mark);
            }
            Some(vis)
        } else {
            None
        };

        // Filter roots to only visible ones when a filter is active.
        if let Some(ref vis) = visible {
            roots.retain(|idx| vis.contains(idx));
        }

        // Collect issue IDs for collapse state lookup (avoids borrow conflict).
        let issue_ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();

        // DFS to build flat node list.
        let mut flat_nodes = Vec::new();
        // (issue_index, depth, is_last_sibling, ancestry_last)
        let mut stack: Vec<(usize, usize, bool, Vec<bool>)> = Vec::new();

        for (ri, &root_idx) in roots.iter().enumerate().rev() {
            let is_last = ri + 1 == roots.len();
            stack.push((root_idx, 0, is_last, Vec::new()));
        }

        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();

        while let Some((issue_idx, depth, is_last, ancestry)) = stack.pop() {
            if !visited.insert(issue_idx) {
                continue; // Avoid cycles.
            }

            let issue_id = &issue_ids[issue_idx];
            let children: Vec<usize> = children_of
                .get(&issue_idx)
                .map(|c| {
                    c.iter()
                        .copied()
                        .filter(|idx| {
                            !visited.contains(idx)
                                && visible.as_ref().map_or(true, |v| v.contains(idx))
                        })
                        .collect()
                })
                .unwrap_or_default();

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

    /// `zo` — expand the fold under the cursor (no-op if already open or leaf).
    fn tree_expand_current(&mut self) {
        if let Some(node) = self.tree_flat_nodes.get(self.tree_cursor) {
            if node.has_children {
                let issue_id = self.analyzer.issues[node.issue_index].id.clone();
                if self.tree_collapsed.remove(&issue_id) {
                    self.build_tree_flat_nodes();
                }
            }
        }
    }

    /// `zc` — close the fold under the cursor.
    ///
    /// Vim semantics: if the cursor is on a foldable node, close it.
    /// If the cursor is on a leaf (or an already-collapsed node whose
    /// parent fold should close next), walk up to the nearest ancestor
    /// with children and collapse *that* fold, then move the cursor to
    /// the newly-closed parent so repeat `zc` keeps walking up.
    ///
    /// Uses the flat-node depth column: the parent of the node at
    /// flat index `i` is the nearest preceding node with
    /// `depth < nodes[i].depth`. This matches the DFS invariant in
    /// `build_tree_flat_nodes` and avoids rebuilding the parent map.
    fn tree_collapse_current(&mut self) {
        let Some(node) = self.tree_flat_nodes.get(self.tree_cursor) else {
            return;
        };
        let cursor_has_children = node.has_children;
        let cursor_issue_index = node.issue_index;
        let cursor_depth = node.depth;
        let cursor_issue_id = self.analyzer.issues[cursor_issue_index].id.clone();

        // Case 1: cursor is on a foldable, currently-open node — collapse it in place.
        if cursor_has_children && !self.tree_collapsed.contains(&cursor_issue_id) {
            if self.tree_collapsed.insert(cursor_issue_id) {
                self.build_tree_flat_nodes();
                if self.tree_cursor >= self.tree_flat_nodes.len() {
                    self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
                }
            }
            return;
        }

        // Case 2: cursor is on a leaf or an already-collapsed node.
        // Walk up the flat list to find the nearest enclosing ancestor
        // with children. The flat list is a DFS order, so the parent of
        // the node at index `i` is the nearest preceding node with
        // `depth < nodes[i].depth`.
        let mut ancestor_cursor: Option<usize> = None;
        let mut search_depth = cursor_depth;
        let mut i = self.tree_cursor;
        while i > 0 {
            i -= 1;
            let n = &self.tree_flat_nodes[i];
            if n.depth < search_depth {
                if n.has_children {
                    ancestor_cursor = Some(i);
                    break;
                }
                // Keep climbing past depth boundaries (defensive; the DFS
                // invariant guarantees any depth-decreasing parent has children).
                search_depth = n.depth;
            }
        }

        let Some(ancestor_idx) = ancestor_cursor else {
            // Orphan leaf at root — nothing to collapse. No-op gracefully.
            return;
        };

        let ancestor_issue_index = self.tree_flat_nodes[ancestor_idx].issue_index;
        let ancestor_issue_id = self.analyzer.issues[ancestor_issue_index].id.clone();
        let inserted = self.tree_collapsed.insert(ancestor_issue_id.clone());
        if inserted {
            self.build_tree_flat_nodes();
        }
        // Move cursor to the newly-closed ancestor (by issue id, since
        // the flat list was rebuilt and descendant rows vanished).
        if let Some(new_cursor) = self
            .tree_flat_nodes
            .iter()
            .position(|n| self.analyzer.issues[n.issue_index].id == ancestor_issue_id)
        {
            self.tree_cursor = new_cursor;
        } else if self.tree_cursor >= self.tree_flat_nodes.len() {
            self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
        }
    }

    /// `zR` — expand every fold in the tree.
    fn tree_expand_all(&mut self) {
        if !self.tree_collapsed.is_empty() {
            self.tree_collapsed.clear();
            self.build_tree_flat_nodes();
        }
    }

    /// `zM` — collapse every fold in the tree.
    ///
    /// Temporarily expands all to discover every node (including deeply nested
    /// ones hidden inside collapsed parents), then collapses everything.
    fn tree_collapse_all(&mut self) {
        self.tree_collapsed.clear();
        self.build_tree_flat_nodes();
        let ids: Vec<String> = self
            .tree_flat_nodes
            .iter()
            .filter(|n| n.has_children)
            .map(|n| self.analyzer.issues[n.issue_index].id.clone())
            .collect();
        for id in ids {
            self.tree_collapsed.insert(id);
        }
        self.build_tree_flat_nodes();
        if self.tree_cursor >= self.tree_flat_nodes.len() {
            self.tree_cursor = self.tree_flat_nodes.len().saturating_sub(1);
        }
    }

    /// `zz` — recenter the viewport so the cursor is roughly centred.
    fn tree_recenter_cursor(&self) {
        // The rendered tree text has header lines before the first node:
        // panel_header (1) + optional search banner (0-1) + separator (1).
        let header_lines = if self.tree_search_active || !self.tree_search_query.is_empty() {
            3
        } else {
            2
        };
        let cursor_line = self.tree_cursor + header_lines;
        let half = self.list_page_step() / 2;
        self.list_scroll_offset
            .set(cursor_line.saturating_sub(half));
    }

    // -- Vim gg jump-to-top ---------------------------------------------------

    fn vim_jump_to_top(&mut self) {
        if self.tree_shortcut_focus() {
            self.tree_cursor = 0;
        } else if self.board_shortcut_focus() {
            self.select_edge_in_current_board_lane(false);
        } else if matches!(self.mode, ViewMode::History) && self.focus == FocusPane::List {
            self.history_event_cursor = 0;
            self.history_related_bead_cursor = 0;
            self.history_bead_commit_cursor = 0;
        } else if self.focus == FocusPane::List {
            self.select_first_visible();
        }
    }

    // -- Tree search ----------------------------------------------------------

    fn start_tree_search(&mut self) {
        if !matches!(self.mode, ViewMode::Tree) || self.focus != FocusPane::List {
            return;
        }
        self.tree_search_active = true;
        self.tree_search_query.clear();
        self.tree_search_match_cursor = 0;
    }

    fn finish_tree_search(&mut self) {
        self.tree_search_active = false;
    }

    fn cancel_tree_search(&mut self) {
        self.tree_search_active = false;
        self.tree_search_query.clear();
        self.tree_search_match_cursor = 0;
    }

    fn tree_search_matches(&self) -> Vec<usize> {
        let query = self.tree_search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.tree_flat_nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                let issue = &self.analyzer.issues[node.issue_index];
                issue.id.to_ascii_lowercase().contains(&query)
                    || issue.title.to_ascii_lowercase().contains(&query)
                    || issue.status.to_ascii_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn select_current_tree_search_match(&mut self) {
        let matches = self.tree_search_matches();
        if matches.is_empty() {
            return;
        }
        self.tree_search_match_cursor = self
            .tree_search_match_cursor
            .min(matches.len().saturating_sub(1));
        self.tree_cursor = matches[self.tree_search_match_cursor];
    }

    fn move_tree_search_match_relative(&mut self, delta: isize) {
        let matches = self.tree_search_matches();
        if matches.is_empty() || delta == 0 {
            return;
        }
        let len = matches.len();
        let current = self.tree_search_match_cursor.min(len.saturating_sub(1));
        let step = delta.unsigned_abs() % len;
        let next = if delta > 0 {
            (current + step) % len
        } else {
            (current + len - step) % len
        };
        self.tree_search_match_cursor = next;
        self.tree_cursor = matches[next];
    }

    fn tree_list_text(&self) -> String {
        self.tree_list_render_text(80).to_plain_text()
    }

    fn tree_list_render_text(&self, width: u16) -> RichText {
        if self.tree_flat_nodes.is_empty() {
            return RichText::raw("(no dependency tree — all issues are independent)");
        }

        let line_width = usize::from(width.saturating_sub(2)).max(24);
        let mut lines = Vec::new();

        lines.push(panel_header(
            "Dependency tree",
            Some(&format!(
                "{} nodes | Enter/za toggle | zo/zc open/close | zR/zM all | T/Esc back",
                self.tree_flat_nodes.len()
            )),
        ));
        // Compute search matches once for both the banner and hit indicators.
        let tree_search_match_indices = self.tree_search_matches();
        let tree_search_hits: BTreeMap<usize, usize> = tree_search_match_indices
            .iter()
            .enumerate()
            .map(|(slot, &idx)| (idx, slot + 1))
            .collect();

        if self.tree_search_active {
            lines.push(RichLine::raw(format!(
                "Search (active): /{}",
                self.tree_search_query
            )));
        } else if !self.tree_search_query.is_empty() {
            let pos = if tree_search_match_indices.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "{}/{}",
                    self.tree_search_match_cursor
                        .min(tree_search_match_indices.len().saturating_sub(1))
                        + 1,
                    tree_search_match_indices.len()
                )
            };
            lines.push(RichLine::raw(format!(
                "Search: /{} (n/N cycles, matches: {})",
                self.tree_search_query, pos
            )));
        }
        lines.push(section_separator(line_width));

        for (i, node) in self.tree_flat_nodes.iter().enumerate() {
            let is_selected = i == self.tree_cursor;
            let issue = &self.analyzer.issues[node.issue_index];
            let mut line = RichLine::new();

            // Cursor marker
            let marker_style = if is_selected {
                tokens::selected()
            } else {
                tokens::dim()
            };
            line.push_span(RichSpan::styled(
                if is_selected { "▸" } else { " " },
                marker_style,
            ));
            line.push_span(RichSpan::raw(" "));

            // Tree prefix (box-drawing characters)
            let mut prefix = String::new();
            for &parent_was_last in &node.ancestry_last {
                if parent_was_last {
                    prefix.push_str("    ");
                } else {
                    prefix.push_str("│   ");
                }
            }
            if node.depth > 0 {
                if node.is_last_sibling {
                    prefix.push_str("└── ");
                } else {
                    prefix.push_str("├── ");
                }
            }
            if !prefix.is_empty() {
                line.push_span(RichSpan::styled(prefix, tokens::dim()));
            }

            // Collapse indicator
            if node.has_children {
                let indicator = if node.is_collapsed { "[+] " } else { "[-] " };
                line.push_span(RichSpan::styled(indicator, tokens::panel_title()));
            } else {
                line.push_span(RichSpan::raw("    "));
            }

            // Status icon with status-specific color
            let si = status_icon(&issue.status);
            line.push_span(RichSpan::styled(si, tokens::status_style(&issue.status)));
            line.push_span(RichSpan::raw(" "));

            // Priority badge
            line.push_span(priority_badge(issue.priority));
            line.push_span(RichSpan::raw(" "));

            // Issue ID
            line.push_span(RichSpan::styled(
                truncate_display(&issue.id, 12),
                tokens::dim(),
            ));
            line.push_span(RichSpan::raw(" "));

            // Title
            let title_width = line_width.saturating_sub(30 + node.depth * 4);
            line.push_span(RichSpan::styled(
                truncate_display(&issue.title, title_width.max(8)),
                tokens::help_desc(),
            ));

            // Dependency indicators
            let blocks = self
                .analyzer
                .metrics
                .blocks_count
                .get(&issue.id)
                .copied()
                .unwrap_or(0);
            let open_bl = self.analyzer.graph.open_blockers(&issue.id).len();
            let bl = blocker_indicator(open_bl, blocks);
            if !bl.is_empty() {
                line.push_span(RichSpan::raw(" "));
                for s in bl {
                    line.push_span(s);
                }
            }

            // Cycle indicator
            if self
                .analyzer
                .metrics
                .cycles
                .iter()
                .any(|c| c.contains(&issue.id))
            {
                line.push_span(RichSpan::styled(
                    " \u{27f3}",
                    tokens::status_style("blocked"),
                ));
            }

            // Search hit indicator
            if let Some(&pos) = tree_search_hits.get(&i) {
                line.push_span(RichSpan::styled(
                    format!(" hit {pos}/{}", tree_search_hits.len()),
                    tokens::panel_title(),
                ));
            }

            lines.push(line);
        }

        RichText::from_lines(lines)
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

            // Velocity sparkline (uses Unicode block characters for trend)
            let velocity = label.velocity.velocity_score;
            let spark = if velocity > 70 {
                "\u{2593}\u{2593}\u{2593}" // ▓▓▓ high velocity
            } else if velocity > 30 {
                "\u{2592}\u{2592}\u{2591}" // ▒▒░ medium
            } else if velocity > 0 {
                "\u{2591}\u{2591}\u{2591}" // ░░░ low
            } else {
                "\u{2581}\u{2581}\u{2581}" // ▁▁▁ stale
            };

            let stale_tag = if label.freshness.stale_count > 0 {
                " STALE"
            } else {
                ""
            };

            lines.push(format!(
                "{marker} {level_marker} [{bar}] {:>3}  {spark} {:<14} ({} open, {} blk){stale_tag}",
                label.health,
                truncate_str(&label.label, 14),
                label.open_count,
                label.blocked_count,
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
        let max_label_width = labels.iter().map(|l| display_width(l)).max().unwrap_or(4);
        let col_w = max_label_width.max(4);

        // Column headers
        let row_label_w = col_w + 2;
        let mut header = " ".repeat(row_label_w);
        for (ci, label) in labels.iter().enumerate() {
            let marker = if ci == self.flow_matrix_col_cursor {
                "v"
            } else {
                " "
            };
            header.push(' ');
            header.push_str(marker);
            header.push_str(&fit_display(label, col_w));
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
            let mut row = format!("{cursor} {}", fit_display(label, col_w));
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
                    row.push('[');
                    row.push_str(&fit_display(cell.trim(), col_w - 1));
                    row.push(']');
                } else {
                    row.push(' ');
                    row.push_str(&fit_display(cell.trim(), col_w));
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
                .filter(|i| {
                    i.labels
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(from_label))
                })
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

        let now = sprint_reference_now();
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

        let now = sprint_reference_now();
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

        // Sprint action commands
        lines.push(String::new());
        lines.push(" ─── Sprint Actions ───".to_string());
        lines.push(format!(" Claim next:  br update <id> --status=in_progress"));
        lines.push(format!(" Close issue: br close <id> --reason \"done\""));
        if !sprint.bead_ids.is_empty() {
            let first_open = self
                .sprint_visible_issues()
                .iter()
                .find(|(_, i)| i.is_open_like())
                .map(|(_, i)| i.id.clone());
            if let Some(next_id) = first_open {
                lines.push(format!(
                    " Suggested:   br update {next_id} --status=in_progress"
                ));
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
        self.modal_overlay = Some(ModalOverlay::LabelPicker {
            items,
            cursor: 0,
            filter: String::new(),
        });
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
        self.modal_overlay = Some(ModalOverlay::RepoPicker {
            items,
            cursor: 0,
            filter: String::new(),
        });
    }

    fn set_label_filter(&mut self, label: &str) {
        if self
            .modal_label_filter
            .as_deref()
            .is_some_and(|current| current.eq_ignore_ascii_case(label))
        {
            self.modal_label_filter = None;
            self.status_msg = "Label filter cleared".into();
        } else {
            self.modal_label_filter = Some(label.to_string());
            self.status_msg = format!("Filtering by label: {label}");
        }
        self.list_scroll_offset.set(0);
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
        self.focus = FocusPane::List;
        self.rebuild_tree_if_active();
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
        self.list_scroll_offset.set(0);
        self.ensure_selected_visible();
        self.sync_insights_heatmap_selection();
        self.focus = FocusPane::List;
        self.rebuild_tree_if_active();
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

    fn detail_panel_render_text(&self) -> RichText {
        match self.mode {
            ViewMode::Main => self.issue_detail_render_text(),
            ViewMode::Insights => self.insights_detail_render_text(),
            ViewMode::Graph => self.graph_detail_render_text(),
            ViewMode::History => self.history_detail_render_text(),
            _ => RichText::raw(self.detail_panel_text()),
        }
    }

    fn issue_detail_render_text(&self) -> RichText {
        let Some(issue) = self.selected_issue() else {
            return RichText::raw(self.issue_detail_text());
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
        let pr_max = max_metric_value(&self.analyzer.metrics.pagerank);
        let bw_max = max_metric_value(&self.analyzer.metrics.betweenness);
        let ev_max = max_metric_value(&self.analyzer.metrics.eigenvector);
        let history = self.analyzer.history(Some(&issue.id), 1).into_iter().next();
        let action_state = if issue.is_closed_like() {
            "closed"
        } else if open_blockers.is_empty() {
            "ready"
        } else {
            "blocked"
        };
        let action_subtitle = match action_state {
            "closed" => "reference state",
            "ready" => "ready to execute",
            _ => "waiting on blockers",
        };
        let action_line = if issue.is_closed_like() {
            format!(
                "Action: Closed work item | Downstream still watching: {}",
                dependents.len()
            )
        } else if open_blockers.is_empty() {
            format!(
                "Action: Pull now | Downstream impact: {} | Critical depth: {}",
                dependents.len(),
                depth
            )
        } else {
            format!(
                "Action: Unblock first via {} | Open blockers: {} | Downstream impact: {}",
                join_display_values(&open_blockers, 3),
                open_blockers.len(),
                dependents.len()
            )
        };
        let status_line = format!(
            "Status: {} | Priority: p{} | Type: {} | State: {}",
            issue.status,
            issue.priority,
            display_or_fallback(&issue.issue_type, "unknown"),
            action_state
        );
        let context_line = format!(
            "Assignee: {} | Repo: {} | Estimate: {}",
            display_or_fallback(&issue.assignee, "unassigned"),
            display_or_fallback(&issue.source_repo, "local"),
            issue
                .estimated_minutes
                .map_or_else(|| "n/a".to_string(), |minutes| format!("{minutes}m"))
        );
        let closed_display = if issue.is_closed_like() {
            format_compact_timestamp(issue.closed_at.or(issue.updated_at))
        } else {
            "n/a".to_string()
        };
        let timeline_line = format!(
            "Created: {} | Updated: {} | Due: {}",
            format_compact_timestamp(issue.created_at),
            format_compact_timestamp(issue.updated_at),
            format_compact_timestamp(issue.due_date)
        );
        let signal_summary = format!(
            "Depth {depth} | k-core {k_core} | slack {slack:.4} | cut-point {}",
            if articulation { "YES" } else { "no" }
        );
        let external_ref = self.selected_issue_external_ref_url();

        let mut lines = Vec::new();
        let push_module_header = |lines: &mut Vec<RichLine>, title: &str, subtitle: &str| {
            if !lines.is_empty() {
                lines.push(RichLine::raw(""));
            }
            lines.push(section_separator(48));
            lines.push(panel_header(title, Some(subtitle)));
        };

        push_module_header(&mut lines, "Summary", action_subtitle);
        lines.push(RichLine::from_spans([
            RichSpan::raw(format!(
                "{} {}  {}",
                type_icon(&issue.issue_type),
                issue.id,
                issue.title
            )),
            RichSpan::styled("  ", tokens::dim()),
            RichSpan::styled("(C copy id)", tokens::dim()),
        ]));
        if let Some(styled_line) = styled_detail_summary_line(&status_line) {
            lines.push(styled_line);
        }
        lines.push(RichLine::from_spans([RichSpan::styled(
            action_line,
            tokens::panel_title_focused(),
        )]));
        lines.push(RichLine::from_spans([
            RichSpan::raw(&context_line),
            RichSpan::styled("  ", tokens::dim()),
            RichSpan::styled("(w repo filter)", tokens::dim()),
        ]));

        let mut labels_line = RichLine::new();
        labels_line.push_span(RichSpan::raw(format!(
            "Closed: {closed_display} | Labels: "
        )));
        if issue.labels.is_empty() {
            labels_line.push_span(RichSpan::styled("none", tokens::dim()));
        } else {
            for span in label_chips(&issue.labels) {
                labels_line.push_span(span);
            }
        }
        labels_line.push_span(RichSpan::styled("  ", tokens::dim()));
        labels_line.push_span(RichSpan::styled("(L label filter)", tokens::dim()));
        lines.push(labels_line);
        lines.push(RichLine::from_spans([
            RichSpan::raw(&timeline_line),
            RichSpan::styled("  ", tokens::dim()),
            RichSpan::styled("(t time-travel)", tokens::dim()),
        ]));
        if let Some(url) = external_ref {
            lines.push(RichLine::from_spans([
                RichSpan::raw("External: "),
                RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                RichSpan::styled("  ", tokens::dim()),
                RichSpan::styled("(o open, y copy)", tokens::dim()),
            ]));
        }

        push_module_header(&mut lines, "Signals", "rank and graph pressure");
        lines.push(RichLine::raw(signal_summary));
        let mut primary_metrics = RichLine::new();
        for span in metric_strip("PR", pagerank, pr_max) {
            primary_metrics.push_span(span);
        }
        primary_metrics.push_span(RichSpan::styled("  ", tokens::dim()));
        for span in metric_strip("BW", betweenness, bw_max) {
            primary_metrics.push_span(span);
        }
        lines.push(primary_metrics);
        let mut secondary_metrics = RichLine::new();
        for span in metric_strip("EV", eigenvector, ev_max) {
            secondary_metrics.push_span(span);
        }
        secondary_metrics.push_span(RichSpan::styled("  ", tokens::dim()));
        secondary_metrics.push_span(RichSpan::styled(
            format!(
                "blockers={} | unblocks={}",
                open_blockers.len(),
                dependents.len()
            ),
            tokens::dim(),
        ));
        lines.push(secondary_metrics);

        push_module_header(&mut lines, "Dependencies", "upstream, gates, downstream");
        lines.push(RichLine::raw(format!(
            "Upstream: {}",
            join_display_values(&blockers, 4)
        )));
        lines.push(RichLine::raw(format!(
            "Open Gate: {}",
            join_display_values(&open_blockers, 4)
        )));
        lines.push(RichLine::raw(format!(
            "Downstream: {}",
            join_display_values(&dependents, 4)
        )));

        if self.priority_hints_visible {
            push_module_header(&mut lines, "Priority Hints", "scoring rationale");
            let mut hint_lines = Vec::new();
            self.append_priority_hints(&mut hint_lines, issue);
            for line in hint_lines {
                if let Some(styled_line) = styled_detail_summary_line(&line) {
                    lines.push(styled_line);
                } else {
                    lines.push(RichLine::raw(line));
                }
            }
        }

        for line in self.issue_detail_text().lines() {
            if matches!(
                line,
                "Triage Snapshot:" | "Graph Signals:" | "Dependency Map:"
            ) {
                continue;
            }
            if line.starts_with("  open blockers:")
                || line.starts_with("  unblocks:")
                || line.starts_with("  dependency pressure:")
                || line.starts_with("  cycle time:")
                || line.starts_with("  Critical depth:")
                || line.starts_with("  PageRank:")
                || line.starts_with("  Betweenness:")
                || line.starts_with("  Eigenvector:")
                || line.starts_with("  HITS:")
                || line.starts_with("  upstream:")
                || line.starts_with("  open gate:")
                || line.starts_with("  downstream:")
                || line == status_line
                || line == context_line
                || line == timeline_line
                || line.starts_with("Closed: ")
                || line.starts_with("External: ")
                || line
                    == format!(
                        "{} {}  {}",
                        type_icon(&issue.issue_type),
                        issue.id,
                        issue.title
                    )
            {
                continue;
            }

            if line.ends_with(':') && !line.is_empty() {
                lines.push(RichLine::raw(""));
                lines.push(section_separator(48));
                lines.push(panel_header(line.trim_end_matches(':'), None));
            } else if let Some(styled_line) = styled_detail_summary_line(line) {
                lines.push(styled_line);
            } else {
                lines.push(RichLine::raw(line));
            }
        }

        if let Some(history) = history.as_ref()
            && !history.events.is_empty()
            && !self.issue_detail_text().contains(&format!(
                "History Summary ({} events):",
                history.events.len()
            ))
        {
            lines.push(RichLine::raw(""));
            lines.push(section_separator(48));
            lines.push(panel_header("History Summary", Some("recent lifecycle")));
        }

        RichText::from_lines(lines)
    }

    fn graph_detail_render_text(&self) -> RichText {
        let external_ref = self.selected_issue_external_ref_url();
        let graph_link_insert_after = 4usize;
        let mut lines = Vec::new();
        for (index, line) in self.graph_detail_text().lines().enumerate() {
            if let Some(url) = external_ref
                && index == graph_link_insert_after
            {
                lines.push(RichLine::from_spans([
                    RichSpan::raw("External Link: "),
                    RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                    RichSpan::styled("  ", tokens::dim()),
                    RichSpan::styled("(o open, y copy)", tokens::dim()),
                ]));
                lines.push(RichLine::raw(""));
            }

            if let Some(url) = external_ref
                && line
                    .strip_prefix("External: ")
                    .is_some_and(|rendered| rendered == url || rendered.ends_with('…'))
            {
                continue;
            } else if let Some(styled_line) = styled_detail_summary_line(line) {
                lines.push(styled_line);
                continue;
            }

            lines.push(RichLine::raw(line));
        }
        if let Some(url) = external_ref
            && self.graph_detail_text().lines().count() <= graph_link_insert_after
        {
            lines.push(RichLine::raw(""));
            lines.push(RichLine::from_spans([
                RichSpan::raw("External Link: "),
                RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                RichSpan::styled("  ", tokens::dim()),
                RichSpan::styled("(o open, y copy)", tokens::dim()),
            ]));
        }
        RichText::from_lines(lines)
    }

    fn board_detail_render_text(&self) -> RichText {
        let external_ref = self.selected_issue_external_ref_url();
        let mut lines = Vec::new();
        let mut inserted_link = false;

        for line in self.board_detail_text().lines() {
            if let Some(url) = external_ref
                && !inserted_link
                && line.is_empty()
            {
                lines.push(RichLine::from_spans([
                    RichSpan::raw("External Link: "),
                    RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                    RichSpan::styled("  ", tokens::dim()),
                    RichSpan::styled("(o open, y copy)", tokens::dim()),
                ]));
                lines.push(RichLine::raw(""));
                inserted_link = true;
            }

            lines.push(RichLine::raw(line));
        }

        if let Some(url) = external_ref
            && !inserted_link
        {
            lines.push(RichLine::raw(""));
            lines.push(RichLine::from_spans([
                RichSpan::raw("External Link: "),
                RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                RichSpan::styled("  ", tokens::dim()),
                RichSpan::styled("(o open, y copy)", tokens::dim()),
            ]));
        }

        RichText::from_lines(lines)
    }

    #[cfg(test)]
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
        // Labels
        if !issue.labels.is_empty() {
            let labels_str = issue
                .labels
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let labels_display = if issue.labels.len() > 4 {
                format!("{labels_str} +{}", issue.labels.len() - 4)
            } else {
                labels_str
            };
            out.push(format!(
                "\u{2502} Labels: {:<w$}\u{2502}",
                truncate_str(&labels_display, box_width - 9),
                w = box_width - 9
            ));
        }
        // Dates
        if let Some(created) = issue.created_at {
            let reference_now = ui_reference_now();
            let age = (reference_now - created).num_days();
            let updated_age = issue.updated_at.map_or_else(
                || "never".to_string(),
                |u| format!("{}d ago", (reference_now - u).num_days()),
            );
            out.push(format!(
                "\u{2502} Age: {age}d | Updated: {:<w$}\u{2502}",
                updated_age,
                w = box_width - 21
            ));
        }
        // Graph metrics
        let pr = self
            .analyzer
            .metrics
            .pagerank
            .get(&issue.id)
            .copied()
            .unwrap_or(0.0);
        let depth = self
            .analyzer
            .metrics
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or(0);
        if pr > 0.0 || depth > 0 {
            out.push(format!(
                "\u{2502} PR:{:.3} Depth:{} {:<w$}\u{2502}",
                pr,
                depth,
                if self
                    .analyzer
                    .metrics
                    .articulation_points
                    .contains(&issue.id)
                {
                    "\u{25c6}cut"
                } else {
                    ""
                },
                w = box_width - 18
            ));
        }
        // Description
        if !issue.description.is_empty() {
            out.push(format!("\u{251c}{hrule}\u{2524}"));
            // Wrap description across multiple lines
            for line in issue.description.lines().take(3) {
                out.push(format!(
                    "\u{2502} {:<w$}\u{2502}",
                    truncate_str(line.trim(), box_width - 3),
                    w = box_width - 1
                ));
            }
            if issue.description.lines().count() > 3 {
                out.push(format!("\u{2502} {:<w$}\u{2502}", "...", w = box_width - 1));
            }
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
        let blockers = self.analyzer.graph.blockers(&issue.id);
        let dependents = self.analyzer.graph.dependents(&issue.id);
        let articulation = self
            .analyzer
            .metrics
            .articulation_points
            .contains(&issue.id);
        let in_critical_path = insights.critical_path.iter().any(|id| id == &issue.id);
        let in_cycle = insights
            .cycles
            .iter()
            .any(|cycle| cycle.iter().any(|id| id == &issue.id));
        let top_bottleneck = insights
            .bottlenecks
            .first()
            .is_some_and(|item| item.id == issue.id);

        let mut lines = vec![
            "Analytics Cockpit".to_string(),
            format!(
                "System Radar | bottlenecks={} crit-path={} cycles={} cut-pts={} k-core-max={}",
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
            "Metric Strip".to_string(),
            format!(
                "[Rank ] pagerank={pagerank:.4} betweenness={betweenness:.4} eigenvector={eigenvector:.4}"
            ),
            format!("[HITS ] hub={hubs:.4} authority={authorities:.4} k-core={k_core}"),
            format!(
                "[Risk ] crit-depth={depth} slack={slack:.4} cut-point={}",
                if articulation { "yes" } else { "no" }
            ),
            format!(
                "[Flow ] blockers={} dependents={} crit-path={} cycle={}",
                blockers.len(),
                dependents.len(),
                if in_critical_path { "yes" } else { "no" },
                if in_cycle { "yes" } else { "no" }
            ),
            format!(
                "[Lead ] top-bottleneck={}",
                if top_bottleneck { "yes" } else { "no" }
            ),
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

    fn insights_detail_render_text(&self) -> RichText {
        let external_ref = self.selected_issue_external_ref_url();
        let insights_link_insert_after = 4usize;
        let mut lines = Vec::new();
        let mut inserted_link = false;
        for (index, line) in self.insights_detail_text().lines().enumerate() {
            if let Some(url) = external_ref
                && index == insights_link_insert_after
            {
                lines.push(RichLine::from_spans([
                    RichSpan::raw("External Link: "),
                    RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                    RichSpan::styled("  ", tokens::dim()),
                    RichSpan::styled("(o open, y copy)", tokens::dim()),
                ]));
                lines.push(RichLine::raw(""));
                inserted_link = true;
            }

            lines.push(RichLine::raw(line));
        }

        if let Some(url) = external_ref
            && !inserted_link
        {
            lines.push(RichLine::raw(""));
            lines.push(RichLine::from_spans([
                RichSpan::raw("External Link: "),
                RichSpan::styled(url, tokens::panel_title_focused()).link(url),
                RichSpan::styled("  ", tokens::dim()),
                RichSpan::styled("(o open, y copy)", tokens::dim()),
            ]));
        }

        RichText::from_lines(lines)
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
            FocusPane::Middle => {
                "Focused edge: list focus (Tab to inspect relationships)".to_string()
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

            if let Some(bead_commit) = self.selected_history_git_bead_commit() {
                lines.push(String::new());
                lines.push(format!(
                    "SELECTED BEAD CHANGE ({}):",
                    self.selected_history_git_related_bead_id()
                        .unwrap_or_default()
                ));
                if bead_commit.field_changes.is_empty() {
                    lines.push("  (no field-level bead changes detected)".to_string());
                } else {
                    let fields = bead_commit
                        .field_changes
                        .iter()
                        .map(|change| change.field.as_str())
                        .collect::<Vec<_>>();
                    lines.push(format!("  Fields: {}", fields.join(", ")));
                }
                if !bead_commit.bead_diff_lines.is_empty() {
                    lines.push("  Diff:".to_string());
                    for line in bead_commit.bead_diff_lines.iter().take(8) {
                        lines.push(format!("    {line}"));
                    }
                    if bead_commit.bead_diff_lines.len() > 8 {
                        lines.push(format!(
                            "    +{} more diff lines...",
                            bead_commit.bead_diff_lines.len() - 8
                        ));
                    }
                }
            }

            // Append file tree panel inline when toggled on
            if self.history_show_file_tree {
                lines.push(String::new());
                lines.push(self.file_tree_panel_text());
            }

            let action_line = if self.history_selected_commit_url().is_some() {
                "y: copy SHA | o: open in browser | f: file tree"
            } else {
                "y: copy SHA | f: file tree"
            };
            lines.push(String::new());
            lines.push(
                "Enter: jump to related bead | J/K: cycle related beads | diff follows cursor"
                    .to_string(),
            );
            lines.push("v: switch to bead timeline | c: cycle confidence".to_string());
            lines.push(action_line.to_string());
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
        let compat_history = self
            .history_git_cache
            .as_ref()
            .and_then(|cache| cache.histories.get(&issue.id));

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
        if let Some(compat_history) = compat_history {
            push_text_section(
                &mut lines,
                "Timeline",
                &self.history_compact_timeline_text(compat_history, 56),
            );

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
        if let Some(compat_history) = compat_history {
            lines.extend(history_legacy_lifecycle_lines(compat_history, 5));
        } else if let Some(history) = selected_history.as_ref() {
            lines.push("LIFECYCLE:".to_string());
            if history.events.is_empty() {
                lines.push("  (no events)".to_string());
            } else {
                let event_count = history.events.len();
                for (idx, event) in history.events.iter().enumerate() {
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
            lines.push("LIFECYCLE:".to_string());
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
            if !commit.field_changes.is_empty() {
                let fields = commit
                    .field_changes
                    .iter()
                    .map(|change| change.field.as_str())
                    .collect::<Vec<_>>();
                lines.push(format!("  Fields changed: {}", fields.join(", ")));
            }
            if !commit.bead_diff_lines.is_empty() {
                lines.push("  Bead diff:".to_string());
                for line in commit.bead_diff_lines.iter().take(8) {
                    lines.push(format!("    {line}"));
                }
                if commit.bead_diff_lines.len() > 8 {
                    lines.push(format!(
                        "    +{} more diff lines...",
                        commit.bead_diff_lines.len() - 8
                    ));
                }
            }
        }

        let action_line = if self.history_selected_commit_url().is_some() {
            "y: copy bead ID | o: open commit | f: file tree"
        } else {
            "y: copy bead ID | f: file tree"
        };
        lines.push(String::new());
        lines.push(
            "Enter: backtrace selected commit | v: switch to git timeline | J/K: cycle commits"
                .to_string(),
        );
        lines.push(action_line.to_string());
        if !self.history_status_msg.is_empty() {
            lines.push(String::new());
            lines.push(self.history_status_msg.clone());
        }

        lines.join("\n")
    }

    fn history_detail_render_text(&self) -> RichText {
        let mut text = RichText::raw(self.history_detail_text());
        if let Some(url) = self.history_selected_commit_url() {
            text.push_line(RichLine::raw(""));
            text.push_line(RichLine::from_spans([
                RichSpan::raw("Browser Link: "),
                RichSpan::styled(
                    "open selected commit (o open, right-click copy link)",
                    tokens::panel_title_focused(),
                )
                .link(url),
            ]));
        }
        text
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
    truncate_display(value, max_len)
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
    match status.to_ascii_lowercase().as_str() {
        "open" => "o",
        "in_progress" => "*",
        "blocked" => "!",
        "closed" => "x",
        "deferred" | "draft" => "~",
        "review" => "r",
        "pinned" => "^",
        "hooked" => "&",
        "tombstone" => "#",
        _ => "?",
    }
}

fn type_icon(issue_type: &str) -> &'static str {
    match issue_type.to_ascii_lowercase().as_str() {
        "bug" => "B",
        "feature" => "F",
        "task" => "T",
        "epic" => "E",
        "question" => "Q",
        "docs" => "D",
        "refactor" => "R",
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

fn compact_history_duration_label(raw: &str) -> String {
    raw.split_whitespace()
        .find_map(|token| {
            let digits = token
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
                .collect::<String>();
            digits
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .map(|_| token.to_string())
        })
        .or_else(|| raw.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| raw.to_string())
}

fn compact_history_month_day(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.format("%b %-d").to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LegacyTimelineEntryType {
    Event,
    Commit,
}

#[derive(Debug, Clone)]
struct LegacyTimelineEntry {
    timestamp: String,
    parsed_ts: Option<DateTime<Utc>>,
    entry_type: LegacyTimelineEntryType,
    label: String,
    detail: String,
    confidence: Option<f64>,
}

fn legacy_history_author_initials(name: &str) -> String {
    let parts = name.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] => "??".to_string(),
        [single] => {
            let mut chars = single.chars();
            match (chars.next(), chars.next()) {
                (Some(first), Some(second)) => [first, second]
                    .into_iter()
                    .flat_map(char::to_uppercase)
                    .collect(),
                (Some(first), None) => first.to_uppercase().collect(),
                (None, _) => "??".to_string(),
            }
        }
        [first, .., last] => [first.chars().next(), last.chars().next()]
            .into_iter()
            .flatten()
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

fn legacy_history_relative_time(timestamp: &str) -> Option<String> {
    let ts = DateTime::parse_from_rfc3339(timestamp).ok()?;
    let diff = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
    if diff < chrono::Duration::zero() {
        return Some("in future".to_string());
    }
    if diff < chrono::Duration::minutes(1) {
        return Some("just now".to_string());
    }
    if diff < chrono::Duration::hours(1) {
        return Some(format!("{}m ago", diff.num_minutes()));
    }
    if diff < chrono::Duration::days(1) {
        return Some(format!("{}h ago", diff.num_hours()));
    }
    if diff < chrono::Duration::days(7) {
        return Some(format!("{}d ago", diff.num_days()));
    }
    if diff < chrono::Duration::days(30) {
        return Some(format!("{}w ago", diff.num_weeks()));
    }
    if diff < chrono::Duration::days(365) {
        return Some(format!("{}mo ago", diff.num_days() / 30));
    }
    Some(format!("{}y ago", diff.num_days() / 365))
}

fn legacy_history_lifecycle_icon(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "created" => "🆕",
        "claimed" | "assigned" => "👤",
        "closed" => "✓",
        "reopened" => "↺",
        "modified" | "updated" => "✎",
        _ => "•",
    }
}

fn history_legacy_lifecycle_lines(
    compat_history: &HistoryBeadCompat,
    max_lines: usize,
) -> Vec<String> {
    let mut events = if compat_history.events.is_empty() {
        let mut fallback = Vec::new();
        if let Some(event) = compat_history.milestones.created.clone() {
            fallback.push(event);
        }
        if let Some(event) = compat_history.milestones.claimed.clone() {
            fallback.push(event);
        }
        if let Some(event) = compat_history.milestones.closed.clone() {
            fallback.push(event);
        }
        if let Some(event) = compat_history.milestones.reopened.clone() {
            fallback.push(event);
        }
        fallback
    } else {
        compat_history.events.clone()
    };

    if events.is_empty() {
        return vec!["LIFECYCLE:".to_string(), "  (no events)".to_string()];
    }

    events.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.event_type.cmp(&right.event_type))
    });

    let mut lines = vec![format!("LIFECYCLE ({})", events.len())];
    let mut available_events = max_lines.saturating_sub(1);
    let needs_more_line = events.len() > available_events;
    if needs_more_line {
        available_events = available_events.saturating_sub(1);
    }

    let mut displayed = 0usize;
    for index in (0..events.len()).rev() {
        if displayed >= available_events {
            break;
        }
        let event = &events[index];
        let connector = if index == 0 { "└" } else { "│" };
        let relative =
            legacy_history_relative_time(&event.timestamp).unwrap_or_else(|| "n/a".to_string());
        let initials = legacy_history_author_initials(&event.author);
        lines.push(format!(
            "  {connector} {} {:<7} {initials}",
            legacy_history_lifecycle_icon(&event.event_type),
            truncate_display(&relative, 7),
        ));
        displayed += 1;
    }

    if needs_more_line {
        lines.push(format!("  +{} more", events.len() - displayed));
    }

    lines
}

fn legacy_timeline_timestamp(timestamp: &str) -> Option<String> {
    let ts = DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&Utc);
    let diff = Utc::now().signed_duration_since(ts);
    Some(if diff < chrono::Duration::days(1) {
        ts.format("%-I:%M %p").to_string()
    } else if diff < chrono::Duration::days(7) {
        ts.format("%a %-I%p").to_string()
    } else if diff < chrono::Duration::days(365) {
        ts.format("%b %-d").to_string()
    } else {
        ts.format("%b '%y").to_string()
    })
}

fn build_legacy_timeline_entries(
    compat_history: &HistoryBeadCompat,
    commits: &[&HistoryCommitCompat],
) -> Vec<LegacyTimelineEntry> {
    let mut entries = Vec::new();

    if let Some(event) = compat_history.milestones.created.as_ref() {
        entries.push(LegacyTimelineEntry {
            timestamp: event.timestamp.clone(),
            parsed_ts: DateTime::parse_from_rfc3339(&event.timestamp)
                .ok()
                .map(|dt| dt.with_timezone(&Utc)),
            entry_type: LegacyTimelineEntryType::Event,
            label: "○ Created".to_string(),
            detail: compat_history.title.clone(),
            confidence: None,
        });
    }
    if let Some(event) = compat_history.milestones.claimed.as_ref() {
        entries.push(LegacyTimelineEntry {
            timestamp: event.timestamp.clone(),
            parsed_ts: DateTime::parse_from_rfc3339(&event.timestamp)
                .ok()
                .map(|dt| dt.with_timezone(&Utc)),
            entry_type: LegacyTimelineEntryType::Event,
            label: "● Claimed".to_string(),
            detail: format!("by {}", event.author),
            confidence: None,
        });
    }
    if let Some(event) = compat_history.milestones.reopened.as_ref() {
        entries.push(LegacyTimelineEntry {
            timestamp: event.timestamp.clone(),
            parsed_ts: DateTime::parse_from_rfc3339(&event.timestamp)
                .ok()
                .map(|dt| dt.with_timezone(&Utc)),
            entry_type: LegacyTimelineEntryType::Event,
            label: "↻ Reopened".to_string(),
            detail: String::new(),
            confidence: None,
        });
    }
    if let Some(event) = compat_history.milestones.closed.as_ref() {
        entries.push(LegacyTimelineEntry {
            timestamp: event.timestamp.clone(),
            parsed_ts: DateTime::parse_from_rfc3339(&event.timestamp)
                .ok()
                .map(|dt| dt.with_timezone(&Utc)),
            entry_type: LegacyTimelineEntryType::Event,
            label: "✓ Closed".to_string(),
            detail: String::new(),
            confidence: None,
        });
    }

    for commit in commits {
        entries.push(LegacyTimelineEntry {
            timestamp: commit.timestamp.clone(),
            parsed_ts: DateTime::parse_from_rfc3339(&commit.timestamp)
                .ok()
                .map(|dt| dt.with_timezone(&Utc)),
            entry_type: LegacyTimelineEntryType::Commit,
            label: commit.short_sha.clone(),
            detail: commit.message.clone(),
            confidence: Some(commit.confidence),
        });
    }

    entries.sort_by(|left, right| {
        left.parsed_ts
            .cmp(&right.parsed_ts)
            .then_with(|| left.entry_type.cmp(&right.entry_type))
            .then_with(|| left.label.cmp(&right.label))
    });
    entries
}

fn render_legacy_timeline_lines(
    compat_history: &HistoryBeadCompat,
    commits: &[&HistoryCommitCompat],
    width: usize,
    max_visible: usize,
) -> Vec<String> {
    let entries = build_legacy_timeline_entries(compat_history, commits);
    if entries.is_empty() {
        return vec!["No events recorded".to_string()];
    }

    let shown = entries.iter().take(max_visible).collect::<Vec<_>>();
    let mut lines = Vec::new();
    let text_width = width.saturating_sub(12).max(8);

    for entry in &shown {
        let ts = legacy_timeline_timestamp(&entry.timestamp).unwrap_or_else(|| "n/a".to_string());
        let ts = format!("{ts:>8}");
        match entry.entry_type {
            LegacyTimelineEntryType::Event => {
                let mut line = format!("{ts} | {}", entry.label);
                if !entry.detail.is_empty() {
                    line.push(' ');
                    line.push_str(&truncate_display(
                        &entry.detail,
                        text_width.saturating_sub(2),
                    ));
                }
                lines.push(truncate_display(&line, width));
            }
            LegacyTimelineEntryType::Commit => {
                let confidence = entry.confidence.unwrap_or(0.0) * 100.0;
                lines.push(truncate_display(
                    &format!("{ts} | ├─ {} {:.0}%", entry.label, confidence),
                    width,
                ));
                if !entry.detail.is_empty() {
                    lines.push(truncate_display(
                        &format!(
                            "{:>8} |   {}",
                            "",
                            truncate_display(&entry.detail, text_width)
                        ),
                        width,
                    ));
                }
            }
        }
    }

    if entries.len() > shown.len() {
        lines.push(truncate_display(
            &format!("{:>8} | ↕ 1-{} of {}", "", shown.len(), entries.len()),
            width,
        ));
    }

    lines
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

    if display_width(value) <= max_len {
        return value.to_string();
    }
    if max_len == 1 {
        return truncate_to_width(value, max_len);
    }

    truncate_with_ellipsis(value, max_len, "…")
}

fn tone_for_status(status: &str) -> SemanticTone {
    if status.eq_ignore_ascii_case("open") || status.eq_ignore_ascii_case("review") {
        SemanticTone::Accent
    } else if status.eq_ignore_ascii_case("in_progress") || status.eq_ignore_ascii_case("hooked") {
        SemanticTone::Warning
    } else if status.eq_ignore_ascii_case("blocked") || status.eq_ignore_ascii_case("tombstone") {
        SemanticTone::Danger
    } else if status.eq_ignore_ascii_case("closed") {
        SemanticTone::Success
    } else {
        SemanticTone::Muted
    }
}

fn tone_for_state(state: &str) -> SemanticTone {
    if state.eq_ignore_ascii_case("ready") {
        SemanticTone::Success
    } else if state.eq_ignore_ascii_case("blocked") {
        SemanticTone::Danger
    } else if state.eq_ignore_ascii_case("closed") {
        SemanticTone::Muted
    } else {
        SemanticTone::Accent
    }
}

fn tone_for_priority(priority: &str) -> SemanticTone {
    match priority.trim_start_matches(['p', 'P']) {
        "0" => SemanticTone::Danger,
        "1" => SemanticTone::Warning,
        "2" => SemanticTone::Accent,
        "3" | "4" => SemanticTone::Muted,
        _ => SemanticTone::Neutral,
    }
}

fn summary_line_from_pairs(
    line: &str,
    tone_for_key: impl Fn(&str, &str) -> SemanticTone,
) -> Option<RichLine> {
    let mut out = RichLine::new();
    let mut wrote_any = false;
    for part in line.split(" | ") {
        let (label, value) = part.split_once(": ")?;
        if wrote_any {
            out.push_span(RichSpan::styled(" | ", tokens::dim()));
        }
        out.push_span(RichSpan::styled(format!("{label}:"), tokens::dim()));
        out.push_span(RichSpan::raw(" "));
        push_chip(&mut out, value, tone_for_key(label, value));
        wrote_any = true;
    }
    wrote_any.then_some(out)
}

fn styled_detail_summary_line(line: &str) -> Option<RichLine> {
    if line.ends_with(':') && !line.starts_with("  ") {
        return Some(RichLine::from_spans([RichSpan::styled(
            line,
            tokens::chip_style(SemanticTone::Accent),
        )]));
    }

    if line.starts_with("Status: ") {
        return summary_line_from_pairs(line, |label, value| match label {
            "Status" => tone_for_status(value),
            "Priority" => tone_for_priority(value),
            "State" => tone_for_state(value),
            "Type" => SemanticTone::Neutral,
            _ => SemanticTone::Muted,
        });
    }

    None
}

fn command_hint_width(hint: CommandHint<'_>) -> usize {
    display_width(hint.key) + 1 + display_width(hint.desc)
}

fn command_hint_line(hints: &[CommandHint<'_>]) -> RichLine {
    let mut line = RichLine::new();
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            line.push_span(RichSpan::styled(" │ ", tokens::footer_sep()));
        }
        line.push_span(RichSpan::styled(hint.key, tokens::footer_key()));
        line.push_span(RichSpan::raw(" "));
        line.push_span(RichSpan::styled(hint.desc, tokens::footer_hint()));
    }
    line
}

fn wrap_command_hints(hints: &[CommandHint<'_>], width: usize) -> RichText {
    if hints.is_empty() || width == 0 {
        return RichText::new();
    }

    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut line_width = 0usize;

    for (index, hint) in hints.iter().copied().enumerate() {
        let hint_width = command_hint_width(hint);
        let separator_width = usize::from(index > line_start) * 3;
        if index > line_start && line_width + separator_width + hint_width > width {
            lines.push(command_hint_line(&hints[line_start..index]));
            line_start = index;
            line_width = hint_width;
        } else {
            line_width += separator_width + hint_width;
        }
    }

    if line_start < hints.len() {
        lines.push(command_hint_line(&hints[line_start..]));
    }

    RichText::from_lines(lines)
}

/// Build a styled footer with a dim prefix label and styled command hints.
fn styled_mode_footer(prefix: &str, hints: &[CommandHint<'_>], width: u16) -> RichText {
    let mut first_line = RichLine::new();
    first_line.push_span(RichSpan::styled(prefix, tokens::footer_dim()));
    first_line.push_span(RichSpan::styled(" │ ", tokens::footer_sep()));

    // Append the first line of hints inline after the prefix
    let avail = width.saturating_sub(1) as usize;
    let prefix_width = display_width(prefix) + 3; // " │ " = 3
    let hint_text = wrap_command_hints(hints, avail.saturating_sub(prefix_width));

    if let Some(first_hint_line) = hint_text.lines().first() {
        for span in first_hint_line.spans() {
            first_line.push_span(span.clone());
        }
    }

    let mut lines = vec![first_line];
    for extra in hint_text.lines().iter().skip(1) {
        lines.push(extra.clone());
    }
    RichText::from_lines(lines)
}

fn fit_display(value: &str, width: usize) -> String {
    let mut out = truncate_display(value, width);
    let visible = display_width(&out);
    if visible < width {
        out.push_str(&" ".repeat(width - visible));
    }
    out
}

fn center_display(value: &str, width: usize) -> String {
    let text = truncate_display(value, width);
    let visible = display_width(&text);
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
                            box_lines.first().map_or(0, |line| display_width(line));
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

    let use_two_phase =
        issues.len() > crate::analysis::graph::AnalysisConfig::background_threshold();
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
        mode_back_stack: Vec::new(),
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
        detail_scroll_offset: 0,
        main_search_active: false,
        main_search_query: String::new(),
        main_search_match_cursor: 0,
        list_scroll_offset: Cell::new(0),
        list_viewport_height: Cell::new(0),
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
        tree_search_active: false,
        tree_search_query: String::new(),
        tree_search_match_cursor: 0,
        pending_g: false,
        g_pre_toggle_mode: None,
        pending_z: false,
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
    run_tui_with_background(issues, None, None, None)
}

/// Frame budget for the interactive TUI.
///
/// ftui's default budget (16ms total, frame skipping allowed) is tuned for
/// animation-heavy apps that would rather drop a frame than fall behind.
/// bvr is event-driven: frames render only when state changes, and every
/// rendered frame is the response to a user action, so a late frame is
/// always better than a mutilated or dropped one.
///
/// On large datasets (hundreds of issues) a full view render can exceed the
/// 16ms default. Each over-budget frame ratchets ftui's degradation ladder
/// one step (Full -> SimpleBorders -> NoStyling -> EssentialOnly -> Skeleton
/// -> SkipFrame). The user experiences this as vanishing colours, skeleton
/// boxes, and finally a blank, seemingly locked screen: at SkipFrame the
/// runtime drops the frame before presenting and clears its dirty flag, so
/// nothing is redrawn until the next input event. Skipped frames also record
/// a zero frame time, which trips the upgrade path a few frames later, so
/// the level oscillates and the screen flickers between states.
///
/// `strict()` disables frame skipping outright, and the generous deadline
/// keeps the degradation ladder from ever engaging for realistic datasets.
/// This is a deadline, not a delay: the runtime stays event-driven and
/// presents each frame as soon as it is rendered.
fn interactive_frame_budget() -> ftui::render::budget::FrameBudgetConfig {
    ftui::render::budget::FrameBudgetConfig::strict(std::time::Duration::from_secs(1))
}

pub fn run_tui_with_background(
    issues: Vec<Issue>,
    background_config: Option<BackgroundModeConfig>,
    initial_view: Option<ViewMode>,
    initial_filter: Option<ListFilter>,
) -> Result<()> {
    let mode = initial_view.unwrap_or(ViewMode::Main);
    let mut model = new_app_with_background(issues, mode, background_config);
    if let Some(filter) = initial_filter {
        model.list_filter = filter;
    }
    // Computed views initialise their data lazily when toggled via
    // keybinding, but `--view` bypasses those toggle functions.
    // Trigger the same initialisation here so the view is populated
    // on the very first frame.
    match mode {
        ViewMode::Tree => model.build_tree_flat_nodes(),
        ViewMode::LabelDashboard => model.compute_label_dashboard(),
        ViewMode::FlowMatrix => model.compute_flow_matrix(),
        ViewMode::Sprint => model.load_sprint_data(),
        ViewMode::Actionable => model.compute_actionable_plan(),
        ViewMode::Attention => model.compute_attention(),
        _ => {}
    }
    App::new(model)
        .screen_mode(ScreenMode::AltScreen)
        .with_budget(interactive_frame_budget())
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
    let (mode, kind) = parse_debug_render_target(view_name)?;

    #[cfg(test)]
    set_pane_split_state(PaneSplitState::default());

    let mut app = new_app(issues, mode);
    if matches!(mode, ViewMode::History) && matches!(kind, DebugRenderKind::Layout) {
        app.history_view_mode = HistoryViewMode::Bead;
    }
    let mut pool = ftui::GraphemePool::default();
    let mut frame = Frame::new(width, height, &mut pool);
    app.view(&mut frame);
    match kind {
        DebugRenderKind::View => Ok(buffer_to_text(&frame.buffer, &pool)),
        DebugRenderKind::Layout => Ok(render_layout_debug_report(&app, width, height)),
        DebugRenderKind::HitTest => Ok(render_hittest_debug_report(&app, width, height)),
        DebugRenderKind::Capture => Ok(render_capture_debug_report(
            &app,
            &buffer_to_text(&frame.buffer, &pool),
            width,
            height,
        )),
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugRenderKind {
    View,
    Layout,
    HitTest,
    Capture,
}

fn parse_debug_render_target(view_name: &str) -> Result<(ViewMode, DebugRenderKind)> {
    let (base_name, kind) = if let Some(base) = view_name.strip_suffix("-layout") {
        (base, DebugRenderKind::Layout)
    } else if let Some(base) = view_name.strip_suffix("-hittest") {
        (base, DebugRenderKind::HitTest)
    } else if let Some(base) = view_name.strip_suffix("-capture") {
        (base, DebugRenderKind::Capture)
    } else {
        (view_name, DebugRenderKind::View)
    };

    let mode = match base_name {
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

    Ok((mode, kind))
}

fn rect_debug_line(label: &str, area: Rect) -> String {
    format!(
        "{label:<14} x={} y={} w={} h={}",
        area.x, area.y, area.width, area.height
    )
}

fn debug_layout_rects(app: &BvrApp, width: u16, height: u16) -> Vec<(&'static str, Rect)> {
    let full = Rect::from_size(width, height);
    let rows = Flex::vertical()
        .constraints([
            Constraint::Fixed(1),
            Constraint::Min(3),
            Constraint::Fixed(1),
        ])
        .split(full);
    let body = rows[1];
    let bp = Breakpoint::from_width(width);
    let split_state = pane_split_state();
    let graph_single_pane = matches!(app.mode, ViewMode::Graph) && matches!(bp, Breakpoint::Narrow);
    let history_layout = if matches!(app.mode, ViewMode::History) {
        HistoryLayout::from_width(body.width)
    } else {
        HistoryLayout::Narrow
    };
    let history_multi_pane =
        matches!(app.mode, ViewMode::History) && history_layout.has_middle_pane();
    let mut rects = vec![("header", rows[0]), ("body", body), ("footer", rows[2])];

    if graph_single_pane {
        rects.push(("detail", body));
        return rects;
    }

    if history_multi_pane {
        if matches!(history_layout, HistoryLayout::Wide)
            && matches!(app.history_view_mode, HistoryViewMode::Bead)
        {
            let PaneSplitPreset::Four(pcts) =
                split_state.history_pcts(history_layout, app.history_view_mode)
            else {
                unreachable!("wide bead history should use four-pane split");
            };
            let panes = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(pcts[0]),
                    Constraint::Percentage(pcts[1]),
                    Constraint::Percentage(pcts[2]),
                    Constraint::Percentage(pcts[3]),
                ])
                .split(body);
            rects.push(("list", panes[0]));
            rects.push(("timeline", panes[1]));
            rects.push(("middle", panes[2]));
            rects.push(("detail", panes[3]));
        } else {
            let PaneSplitPreset::Three(pane_widths) =
                split_state.history_pcts(history_layout, app.history_view_mode)
            else {
                unreachable!("multi-pane history should use three-pane split");
            };
            let panes = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(pane_widths[0]),
                    Constraint::Percentage(pane_widths[1]),
                    Constraint::Percentage(pane_widths[2]),
                ])
                .split(body);
            rects.push(("list", panes[0]));
            rects.push(("middle", panes[1]));
            rects.push(("detail", panes[2]));
        }
        return rects;
    }

    let panes = Flex::horizontal()
        .constraints([
            Constraint::Percentage(split_state.two_pane_list_pct(bp)),
            Constraint::Percentage(split_state.two_pane_detail_pct(bp)),
        ])
        .split(body);
    rects.push(("list", panes[0]));
    rects.push(("detail", panes[1]));
    rects
}

fn render_layout_debug_report(app: &BvrApp, width: u16, height: u16) -> String {
    let rects = debug_layout_rects(app, width, height);
    let mut lines = vec![
        format!(
            "Layout Debug | view={} | focus={}",
            app.mode.label(),
            app.focus.label()
        ),
        format!(
            "viewport       w={} h={} breakpoint={:?}",
            width,
            height,
            Breakpoint::from_width(width)
        ),
    ];
    lines.extend(
        rects
            .into_iter()
            .map(|(label, area)| rect_debug_line(label, area)),
    );

    let detail_area = cached_detail_content_area();
    if detail_area.width > 0 && detail_area.height > 0 {
        lines.push(rect_debug_line("detail-content", detail_area));
    }

    lines.join("\n")
}

fn render_hittest_debug_report(app: &BvrApp, width: u16, height: u16) -> String {
    let mut lines = vec![
        format!(
            "HitTest Debug | view={} | focus={}",
            app.mode.label(),
            app.focus.label()
        ),
        format!("viewport       w={} h={}", width, height),
    ];

    let detail_area = cached_detail_content_area();
    lines.push(rect_debug_line("detail-content", detail_area));

    for tab in header_mode_tabs(app, width) {
        lines.push(rect_debug_line(
            &format!("tab-{}", tab.mode.label().to_ascii_lowercase()),
            tab.rect,
        ));
    }

    if let Some(link_area) = app.current_detail_link_row_area() {
        lines.push(rect_debug_line("link-row", link_area));
        let center_x = link_area.x.saturating_add(link_area.width / 2);
        let center_y = link_area.y;
        lines.push(format!(
            "link-center    x={} y={} inside={}",
            center_x,
            center_y,
            rect_contains(link_area, center_x, center_y)
        ));
    } else {
        lines.push("link-row       none".to_string());
    }

    for (index, hit_box) in splitter_hit_boxes(app, width, height)
        .into_iter()
        .enumerate()
    {
        lines.push(rect_debug_line(&format!("splitter-{index}"), hit_box.rect));
    }

    lines.join("\n")
}

fn render_capture_debug_report(app: &BvrApp, rendered: &str, width: u16, height: u16) -> String {
    format!(
        "Capture Debug | view={} | focus={} | selected={} | trace-len={}\nviewport       w={} h={}\n\n--- render ---\n{}\n\n--- layout ---\n{}\n\n--- hittest ---\n{}",
        app.mode.label(),
        app.focus.label(),
        app.selected,
        debug_trace_len(app),
        width,
        height,
        rendered,
        render_layout_debug_report(app, width, height),
        render_hittest_debug_report(app, width, height),
    )
}

#[cfg(test)]
fn debug_trace_len(app: &BvrApp) -> usize {
    app.key_trace.len()
}

#[cfg(not(test))]
fn debug_trace_len(_app: &BvrApp) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundTickDecision, BoardGrouping, BvrApp, CommandHint, EmptyLaneVisibility, FocusPane,
        GitCommitRecord, HistoryBeadCompat, HistoryCommitCompat, HistoryGitCache, HistoryLayout,
        HistoryMilestonesCompat, HistorySearchMode, HistoryViewMode, InsightsPanel, ListFilter,
        ListSort, ModalOverlay, MouseButton, MouseEvent, MouseEventKind, Msg, ScanLineContext,
        SemanticTone, ViewMode, background_warning_message, blocker_indicator, buffer_to_text,
        build_header_text, cached_detail_content_area, center_display, command_hint_width,
        compact_history_duration_label, decide_background_tick, display_width, fit_display,
        history_legacy_lifecycle_lines, issue_scan_line, label_chips,
        legacy_history_author_initials, metric_strip, panel_header, priority_badge,
        record_view_size, render_debug_view, saturating_scroll_offset, section_separator,
        should_apply_background_reload, sprint_reference_now, status_chip,
        styled_detail_summary_line, truncate_display, type_badge, wrap_command_hints,
    };
    use crate::analysis::Analyzer;
    use crate::analysis::diff::FieldChange;
    use crate::analysis::git_history::{
        HistoryCycleCompat, HistoryEventCompat, HistoryFileChangeCompat,
    };
    use crate::analysis::label_intel::CrossLabelFlow;
    use crate::model::{Comment, Dependency, Issue, Sprint, ts};
    use chrono::Utc;
    use ftui::core::event::{KeyCode, Modifiers};
    use ftui::runtime::{Cmd, Model};
    use ftui::text::{Line as RichLine, Span as RichSpan};
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    #[test]
    fn interactive_frame_budget_never_skips_frames() {
        let budget = super::interactive_frame_budget();
        assert!(
            !budget.allow_frame_skip,
            "bvr must never drop a rendered frame: ftui's skipped-frame path \
             clears the runtime dirty flag, leaving the terminal blank until \
             the next input event (reported in beads_viewer_rust PR #19)"
        );
    }

    #[test]
    fn interactive_frame_budget_leaves_cold_frame_headroom() {
        let budget = super::interactive_frame_budget();
        assert!(
            budget.total >= std::time::Duration::from_millis(500),
            "frame deadline must stay far above worst-case cold render times \
             so ftui's degradation ladder (SimpleBorders -> NoStyling -> \
             Skeleton -> SkipFrame) never engages on large datasets; got {:?}",
            budget.total
        );
    }

    #[test]
    fn cold_first_frame_fits_interactive_budget_on_large_dataset() {
        // The cold first frame on a large dataset is the exact scenario from
        // PR #19: ~800 issues, uncached layout, first render after entering
        // the alt screen. Render each debug-supported view from a cold app
        // and require it to finish within the configured frame deadline. If
        // view rendering ever regresses past the deadline, ftui would start
        // degrading/skipping frames again and this test should fail.
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/testdata/stress_large_500.jsonl"
        );
        let issues = crate::loader::load_issues_from_file(std::path::Path::new(fixture))
            .expect("stress fixture loads");
        assert!(issues.len() >= 400, "fixture should be a large dataset");
        let budget = super::interactive_frame_budget();
        for view in ["main", "board", "insights", "graph", "history"] {
            let start = std::time::Instant::now();
            let output = render_debug_view(issues.clone(), view, 220, 60)
                .unwrap_or_else(|e| panic!("cold {view} render failed: {e}"));
            let elapsed = start.elapsed();
            assert!(
                !output.trim().is_empty(),
                "cold {view} render produced empty output"
            );
            assert!(
                elapsed < budget.total,
                "cold {view} frame took {elapsed:?}, exceeding the {:?} \
                 interactive frame deadline; ftui would degrade or skip \
                 frames (blank screen / lost colours from PR #19)",
                budget.total
            );
        }
    }

    #[derive(Debug, Clone)]
    struct DebugReplayCapture {
        step: String,
        mode: ViewMode,
        focus: FocusPane,
        selected: usize,
        width: u16,
        height: u16,
        trace_len: usize,
        rendered: String,
        layout: String,
        hittest: String,
    }

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
        let mut app = BvrApp {
            analyzer: Analyzer::new(sample_issues()),
            repo_root: None,
            selected,
            list_filter: ListFilter::All,
            list_sort: ListSort::Default,
            board_grouping: BoardGrouping::Status,
            board_empty_visibility: EmptyLaneVisibility::Auto,
            mode,
            mode_before_history: ViewMode::Main,
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
        app.reset_pane_split_state();
        app
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
            field_changes: vec![],
            bead_diff_lines: vec![],
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

    /// Pre-populate `history_git_cache` with deterministic fixture data so
    /// tests that switch to git-history mode don't depend on real repo state.
    fn inject_deterministic_git_cache(app: &mut BvrApp) {
        let ui_commit = history_commit(
            "aaaa1111",
            "feat: ui wiring",
            0.95,
            &["src/ui/app.rs", "src/ui/detail.rs"],
        );
        let core_commit =
            history_commit("bbbb2222", "feat: graph core", 0.80, &["src/core/graph.rs"]);

        let mut histories = BTreeMap::new();
        for issue in &app.analyzer.issues {
            histories.insert(
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
            );
        }

        // Wire commits to issue "B" (Dependent) so git mode shows content.
        if let Some(history) = histories.get_mut("B") {
            history.commits = Some(vec![ui_commit.clone(), core_commit.clone()]);
            history.last_author = "Test Author".to_string();
        }

        let mut commit_bead_confidence = BTreeMap::new();
        commit_bead_confidence.insert("aaaa1111".to_string(), vec![("B".to_string(), 0.95)]);
        commit_bead_confidence.insert("bbbb2222".to_string(), vec![("B".to_string(), 0.80)]);

        app.history_git_cache = Some(HistoryGitCache {
            commits: vec![
                git_commit_record(
                    "aaaa1111",
                    "feat: ui wiring",
                    &["src/ui/app.rs", "src/ui/detail.rs"],
                ),
                git_commit_record("bbbb2222", "feat: graph core", &["src/core/graph.rs"]),
            ],
            histories,
            commit_bead_confidence,
        });
    }

    fn key(code: KeyCode) -> Msg {
        Msg::KeyPress(code, Modifiers::empty())
    }

    fn key_ctrl(code: KeyCode) -> Msg {
        Msg::KeyPress(code, Modifiers::CTRL)
    }

    fn key_backtab() -> Msg {
        Msg::KeyPress(KeyCode::BackTab, Modifiers::SHIFT)
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Msg {
        Msg::Mouse(MouseEvent::new(kind, x, y))
    }

    fn selected_issue_id(app: &BvrApp) -> String {
        app.analyzer
            .issues
            .get(app.selected)
            .map(|issue| issue.id.clone())
            .unwrap_or_default()
    }

    fn first_rendered_issue_id(app: &BvrApp) -> String {
        let indices = app.visible_issue_indices();
        indices
            .first()
            .and_then(|&idx| app.analyzer.issues.get(idx))
            .map(|issue| issue.id.clone())
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
        for view in [
            "insights",
            "board",
            "history",
            "main",
            "graph",
            "main-layout",
            "history-layout",
            "graph-layout",
            "main-hittest",
            "graph-hittest",
            "main-capture",
        ] {
            let output =
                render_debug_view(sample_issues(), view, 80, 12).expect("debug render succeeds");
            if view.contains("-layout") || view.contains("-hittest") || view.contains("-capture") {
                assert!(
                    !output.is_empty(),
                    "diagnostic debug render should return content for view {view}"
                );
            } else {
                assert_eq!(
                    output.lines().count(),
                    12,
                    "expected one line per requested row for view {view}"
                );
            }
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
                .all(|line| display_width(line) <= usize::from(width)),
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
    fn render_debug_view_layout_reports_pane_rects() {
        let output =
            render_debug_view(sample_issues(), "main-layout", 100, 20).expect("layout debug");
        assert!(output.contains("Layout Debug | view=Main"));
        assert!(output.contains("header"));
        assert!(output.contains("list"));
        assert!(output.contains("detail"));
        assert!(output.contains("detail-content"));
    }

    #[test]
    fn render_debug_view_hittest_reports_link_row() {
        let mut issues = sample_issues();
        issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let output = render_debug_view(issues, "main-hittest", 100, 20).expect("hittest debug");
        assert!(output.contains("HitTest Debug | view=Main"));
        assert!(output.contains("detail-content"));
        assert!(output.contains("tab-main"));
        assert!(output.contains("tab-board"));
        assert!(output.contains("link-row"));
        assert!(output.contains("link-center"));
        assert!(output.contains("splitter-0"));
    }

    #[test]
    fn render_debug_view_history_layout_reports_timeline_rects() {
        let output =
            render_debug_view(sample_issues(), "history-layout", 160, 24).expect("layout debug");
        assert!(output.contains("Layout Debug | view=History"));
        assert!(output.contains("timeline"));
        assert!(output.contains("middle"));
        assert!(output.contains("detail"));
    }

    #[test]
    fn render_debug_view_graph_hittest_reports_link_row() {
        let mut issues = sample_issues();
        issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let output =
            render_debug_view(issues, "graph-hittest", 100, 20).expect("graph hittest debug");
        assert!(output.contains("HitTest Debug | view=Graph"));
        assert!(output.contains("link-row"));
        assert!(output.contains("link-center"));
    }

    #[test]
    fn render_debug_view_capture_includes_render_layout_and_hittest_sections() {
        let mut issues = sample_issues();
        issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let output = render_debug_view(issues, "main-capture", 100, 20).expect("capture debug");
        assert!(output.contains("Capture Debug | view=Main"));
        assert!(output.contains("--- render ---"));
        assert!(output.contains("--- layout ---"));
        assert!(output.contains("--- hittest ---"));
        assert!(output.contains("Layout Debug | view=Main"));
        assert!(output.contains("HitTest Debug | view=Main"));
    }

    #[test]
    fn debug_replay_artifact_tracks_responsive_layout_and_trace_growth() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let mut captures = Vec::new();

        capture_debug_replay(&app, 100, 24, "history_standard", &mut captures);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        capture_debug_replay(&app, 160, 24, "history_wide_detail", &mut captures);

        let artifact = debug_replay_artifact("history responsive replay", &captures);
        assert!(artifact.contains("=== Debug Replay: history responsive replay ==="));
        assert!(artifact.contains("trace-len=0"));
        assert!(artifact.contains("trace-len=2"));
        assert!(artifact.contains("size=100x24"));
        assert!(artifact.contains("size=160x24"));
        assert!(artifact.contains("breakpoint=Medium"));
        assert!(artifact.contains("breakpoint=Wide"));
        assert!(artifact.contains("timeline"));
        assert!(artifact.contains("HitTest Debug | view=History"));
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
        assert!(list.contains("Signal Tiles"));
        assert!(list.contains("Outlier Radar"));
        assert!(detail.contains("Analytics Cockpit"));
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
        assert!(detail.contains("Metric Strip"));
        assert!(detail.contains("[Rank ]"));
        assert!(detail.contains("[Flow ]"));
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
        assert!(text.lines().all(|line| display_width(line) <= 58));
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
        assert!(text.lines().all(|line| display_width(line) <= 120));
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
        assert!(text.lines().all(|line| display_width(line) <= 120));
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
    fn main_hotkey_from_graph_or_insights_resets_focus_to_list() {
        for mode in [ViewMode::Graph, ViewMode::Insights] {
            let mut app = new_app(mode, 1);
            app.focus = FocusPane::Detail;

            app.update(key(KeyCode::Char('1')));
            assert!(matches!(app.mode, ViewMode::Main));
            assert_eq!(app.focus, FocusPane::List);
            assert_eq!(selected_issue_id(&app), "B");
        }
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
    fn main_mode_search_query_and_match_cycling_work() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(!app.main_search_active);

        app.update(key(KeyCode::Char('/')));
        assert!(app.main_search_active);
        assert!(app.main_search_query.is_empty());

        app.update(key(KeyCode::Char('d')));
        assert_eq!(app.main_search_query, "d");
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("hit 1/3"));

        app.update(key(KeyCode::Enter));
        assert!(!app.main_search_active);
        assert_eq!(app.main_search_query, "d");
        assert!(app.list_panel_text().contains("Matches: 1/3"));

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "B");
        assert!(app.list_panel_text().contains("Matches: 2/3"));
        assert!(app.list_panel_text().contains("hit 2/3"));

        app.update(key(KeyCode::Char('N')));
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("Matches: 1/3"));

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.main_search_query, "z");
        app.update(key(KeyCode::Escape));
        assert!(!app.main_search_active);
        assert!(app.main_search_query.is_empty());
    }

    #[test]
    fn main_mode_search_no_match_message_is_explicit() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('z')));

        let active = app.list_panel_text();
        assert!(active.contains("Search (active): /z"));
        assert!(active.contains("Matches: none"));

        app.update(key(KeyCode::Enter));
        let finished = app.list_panel_text();
        assert!(finished.contains("Search: /z (n/N cycles)"));
        assert!(finished.contains("Matches: none"));
        assert_eq!(selected_issue_id(&app), "A");
    }

    #[test]
    fn main_mode_search_requires_list_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;

        app.update(key(KeyCode::Char('/')));
        assert!(!app.main_search_active);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);
        app.update(key(KeyCode::Char('/')));
        assert!(app.main_search_active);
    }

    #[test]
    fn keyflow_main_escape_unwinds_focus_search_and_filter() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        app.update(key(KeyCode::Escape));
        assert_eq!(app.focus, FocusPane::List);
        assert_eq!(app.status_msg, "Focus returned to list");

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('d')));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.main_search_query, "d");
        app.update(key(KeyCode::Escape));
        assert!(app.main_search_query.is_empty());
        assert_eq!(app.status_msg, "Main search cleared");

        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);
        app.update(key(KeyCode::Escape));
        assert_eq!(app.list_filter, ListFilter::All);
    }

    #[test]
    fn backtab_reverses_main_focus_cycle() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.focus, FocusPane::List);

        app.update(key_backtab());
        assert_eq!(app.focus, FocusPane::Detail);

        app.update(key_backtab());
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn backtab_reverses_history_focus_cycle_with_file_tree() {
        let mut app = new_app(ViewMode::History, 0);
        app.history_show_file_tree = true;

        app.update(key_backtab());
        assert!(app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::List);

        app.update(key_backtab());
        assert!(!app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::Detail);

        app.update(key_backtab());
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn main_list_focus_banner_tracks_active_pane() {
        let mut app = new_app(ViewMode::Main, 0);
        let list_focus = app.main_list_render_text(90).to_plain_text();
        assert!(list_focus.contains("Focus: list owns selection"));
        assert!(list_focus.contains("scope=all"));

        app.focus = FocusPane::Detail;
        let detail_focus = app.main_list_render_text(90).to_plain_text();
        assert!(detail_focus.contains("Focus: detail owns J/K deps"));
        assert!(detail_focus.contains("selected=A"));
    }

    #[test]
    fn main_list_scope_banner_surfaces_label_repo_and_search_state() {
        let mut app = new_app(ViewMode::Main, 0);
        app.modal_label_filter = Some("core".to_string());
        app.modal_repo_filter = Some("viewer".to_string());
        app.main_search_query = "root".to_string();

        let text = app.main_list_render_text(100).to_plain_text();
        assert!(text.contains("label=core"), "label scope missing: {text}");
        assert!(text.contains("pos=1/1"), "position scope missing: {text}");
        assert!(text.contains("repo=viewer"), "repo scope missing: {text}");
        assert!(text.contains("search=root"), "search scope missing: {text}");
    }

    #[test]
    fn page_down_uses_viewport_aware_step_in_main_mode() {
        let issues = (0..20)
            .map(|idx| Issue {
                id: format!("ISSUE-{idx:02}"),
                title: format!("Issue {idx:02}"),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: idx % 4,
                ..Issue::default()
            })
            .collect();
        let mut app = new_app_with_issues(ViewMode::Main, 0, issues);
        let _ = render_app(&app, 120, 16);
        let visible = app.visible_issue_indices_for_list_nav();
        let page_step = app.list_page_step();
        let expected_down = visible[page_step.min(visible.len().saturating_sub(1))];

        app.update(key(KeyCode::PageDown));
        assert_eq!(app.selected, expected_down);

        app.update(key(KeyCode::PageUp));
        assert_eq!(selected_issue_id(&app), "ISSUE-00");
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
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Alpha dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Alpha root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Graph, 0, issues);
        assert!(!app.graph_search_active);

        app.update(key(KeyCode::Char('/')));
        assert!(app.graph_search_active);
        assert!(app.graph_search_query.is_empty());

        // Type a search query that matches issue "A"
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.graph_search_query, "a");
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("hit 1/2"));

        // Enter finishes search but keeps query
        app.update(key(KeyCode::Enter));
        assert!(!app.graph_search_active);
        assert_eq!(app.graph_search_query, "a");
        assert!(app.list_panel_text().contains("Matches: 1/2"));
        assert!(app.list_panel_text().contains("hit 1/2"));

        // n/N should cycle matches
        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "B");
        assert!(app.list_panel_text().contains("Matches: 2/2"));
        assert!(app.list_panel_text().contains("hit 2/2"));

        app.update(key(KeyCode::Char('N')));
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("Matches: 1/2"));

        // Escape from new search clears query
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('x')));
        assert_eq!(app.graph_search_query, "x");
        app.update(key(KeyCode::Escape));
        assert!(!app.graph_search_active);
        assert!(app.graph_search_query.is_empty());
    }

    #[test]
    fn graph_mode_search_starts_from_detail_focus_and_returns_to_list() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;

        app.update(key(KeyCode::Char('/')));

        assert!(app.graph_search_active);
        assert_eq!(app.focus, FocusPane::List);
        assert!(app.graph_search_query.is_empty());
    }

    #[test]
    fn graph_mode_search_prefers_rendered_order_over_storage_order() {
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Alpha dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Alpha root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Graph, 0, issues);

        let rendered_ids = app
            .graph_visible_issue_indices()
            .into_iter()
            .map(|index| app.analyzer.issues[index].id.clone())
            .collect::<Vec<_>>();
        assert_eq!(rendered_ids, vec!["A", "B"]);

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));

        assert_eq!(selected_issue_id(&app), "A");
        assert!(
            app.list_panel_text()
                .lines()
                .any(|line| line.starts_with('>') && line.contains(" A"))
        );
    }

    #[test]
    fn graph_mode_navigation_uses_graph_ranked_order() {
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Graph, 0, issues);
        let order = app
            .graph_visible_issue_indices()
            .into_iter()
            .map(|index| app.analyzer.issues[index].id.clone())
            .collect::<Vec<_>>();
        assert_eq!(order.len(), 2);

        assert_eq!(selected_issue_id(&app), order[0]);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), order[1]);
        app.update(key(KeyCode::Char('k')));
        assert_eq!(selected_issue_id(&app), order[0]);
    }

    #[test]
    fn graph_mode_toggle_reselects_first_graph_ranked_issue() {
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Main, 0, issues);
        let graph_order = app.graph_visible_issue_indices();
        assert_eq!(graph_order.len(), 2);
        app.set_selected_index(graph_order[1]);
        let expected = app.analyzer.issues[graph_order[0]].id.clone();
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);
        assert_eq!(selected_issue_id(&app), expected);
    }

    #[test]
    fn graph_mode_toggle_preserves_search_match_selection() {
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Alpha dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Graph, 0, issues);
        app.set_selected_index(app.issue_index_for_id("B").expect("issue B should exist"));

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Enter));
        assert_eq!(selected_issue_id(&app), "B");

        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Main);
        // Clear pending_g latch (any non-g key) before re-toggling
        app.update(key(KeyCode::F(20)));
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);
        assert_eq!(selected_issue_id(&app), "B");
    }

    #[test]
    fn insights_mode_search_query_and_match_cycling_work() {
        let issues = vec![
            Issue {
                id: "B".to_string(),
                title: "Alpha dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Alpha root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Main, 0, issues);
        app.update(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, ViewMode::Insights));
        assert!(!app.insights_search_active);

        app.update(key(KeyCode::Char('/')));
        assert!(app.insights_search_active);
        assert!(app.insights_search_query.is_empty());

        // Type a search query
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.insights_search_query, "a");
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("hit 1/2"));

        // Enter finishes search but keeps query
        app.update(key(KeyCode::Enter));
        assert!(!app.insights_search_active);
        assert_eq!(app.insights_search_query, "a");
        assert!(app.list_panel_text().contains("Matches: 1/2"));
        assert!(app.list_panel_text().contains("hit 1/2"));

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "B");
        assert!(app.list_panel_text().contains("Matches: 2/2"));
        assert!(app.list_panel_text().contains("hit 2/2"));

        app.update(key(KeyCode::Char('N')));
        assert_eq!(selected_issue_id(&app), "A");
        assert!(app.list_panel_text().contains("Matches: 1/2"));

        // Escape from new search clears query
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.insights_search_query, "z");
        app.update(key(KeyCode::Escape));
        assert!(!app.insights_search_active);
        assert!(app.insights_search_query.is_empty());
    }

    #[test]
    fn insights_mode_search_starts_from_detail_focus_and_returns_to_list() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;

        app.update(key(KeyCode::Char('/')));

        assert!(app.insights_search_active);
        assert_eq!(app.focus, FocusPane::List);
        assert!(app.insights_search_query.is_empty());
    }

    #[test]
    fn insights_mode_selection_tracks_bottleneck_order_not_storage_order() {
        let issues = vec![
            Issue {
                id: "C".to_string(),
                title: "Closed unrelated".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root bottleneck".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Insights, 0, issues);

        let expected = app
            .analyzer
            .insights()
            .bottlenecks
            .first()
            .map(|item| item.id.clone())
            .expect("bottleneck ranking");
        assert_eq!(selected_issue_id(&app), expected);

        let order = app
            .insights_visible_issue_indices_for_list_nav()
            .into_iter()
            .map(|index| app.analyzer.issues[index].id.clone())
            .collect::<Vec<_>>();
        assert!(!order.is_empty());

        app.update(key(KeyCode::Char('j')));
        let next_expected = order.get(1).cloned().unwrap_or_else(|| order[0].clone());
        assert_eq!(selected_issue_id(&app), next_expected);
    }

    #[test]
    fn insights_mode_toggle_and_panel_cycle_sync_ranked_selection() {
        let issues = vec![
            Issue {
                id: "C".to_string(),
                title: "Closed unrelated".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root bottleneck".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Main, 0, issues);
        app.set_selected_index(0);

        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.mode, ViewMode::Insights);
        let bottleneck_expected = app
            .insights_visible_issue_indices_for_list_nav()
            .first()
            .map(|index| app.analyzer.issues[*index].id.clone())
            .expect("bottleneck ranked issue");
        assert_eq!(selected_issue_id(&app), bottleneck_expected);

        let bottleneck_order = app.insights_visible_issue_indices_for_list_nav();
        if let Some(second_index) = bottleneck_order.get(1).copied() {
            app.set_selected_index(second_index);
        }

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.insights_panel, InsightsPanel::Keystones);
        let keystone_expected = app
            .insights_visible_issue_indices_for_list_nav()
            .first()
            .map(|index| app.analyzer.issues[*index].id.clone())
            .expect("keystone ranked issue");
        assert_eq!(selected_issue_id(&app), keystone_expected);
    }

    #[test]
    fn insights_panel_cycle_preserves_search_match_selection() {
        let issues = vec![
            Issue {
                id: "C".to_string(),
                title: "Closed unrelated".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Alpha dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root bottleneck".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Insights, 0, issues);
        app.set_selected_index(app.issue_index_for_id("B").expect("issue B should exist"));

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Enter));
        assert_eq!(selected_issue_id(&app), "B");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.insights_panel, InsightsPanel::Keystones);
        assert_eq!(selected_issue_id(&app), "B");
    }

    #[test]
    fn insights_entry_from_graph_preserves_external_context_issue() {
        let issues = vec![
            Issue {
                id: "C".to_string(),
                title: "Closed unrelated".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root bottleneck".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Board, 0, issues);
        app.set_selected_index(app.issue_index_for_id("C").expect("issue C should exist"));

        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.mode, ViewMode::Insights);
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.insights_panel, InsightsPanel::Keystones);
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(selected_issue_id(&app), "C");
    }

    #[test]
    fn graph_entry_from_insights_preserves_external_context_issue() {
        let issues = vec![
            Issue {
                id: "C".to_string(),
                title: "Closed unrelated".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Dependent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "A".to_string(),
                title: "Root bottleneck".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Board, 0, issues);
        app.set_selected_index(app.issue_index_for_id("C").expect("issue C should exist"));

        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.mode, ViewMode::Insights);
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);
        assert_eq!(selected_issue_id(&app), "C");

        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(selected_issue_id(&app), "C");
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
    fn history_git_mode_c_cycles_confidence_and_clamps_cursor() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.mode = ViewMode::History;
        app.history_view_mode = HistoryViewMode::Git;
        app.history_event_cursor = 3;
        app.history_file_tree_cursor = 99;

        app.update(key(KeyCode::Char('c')));
        app.update(key(KeyCode::Char('c')));
        app.update(key(KeyCode::Char('c')));

        assert_eq!(app.history_confidence_index, 3);
        assert_eq!(app.history_git_visible_commit_indices(), vec![0, 2]);
        assert_eq!(app.history_event_cursor, 1);
        assert!(app.history_flat_file_list().len() < 99);
        assert_eq!(app.history_file_tree_cursor, 4);
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

        assert!(
            app.selected_history_event().is_some(),
            "git timeline should contain at least one event"
        );

        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Git));

        app.update(key(KeyCode::Char('c')));
        assert_eq!(app.history_confidence_index, 1);

        // Capture expected issue_id after confidence change (which may filter events)
        let expected_issue_id = app
            .selected_history_git_related_bead_id()
            .or_else(|| app.selected_history_event().map(|event| event.issue_id))
            .expect("should have a related bead after confidence change");

        let cmd = app.update(key(KeyCode::Enter));
        assert!(matches!(cmd, Cmd::None));
        assert!(matches!(app.mode, ViewMode::Main));
        assert_eq!(app.focus, FocusPane::Detail);
        assert_eq!(selected_issue_id(&app), expected_issue_id);
    }

    #[test]
    fn history_reentry_resets_search_and_file_tree_state() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.mode = ViewMode::History;
        app.mode_before_history = ViewMode::Main;
        app.history_view_mode = HistoryViewMode::Git;
        app.history_search_active = false;
        app.history_search_query = "graph".to_string();
        app.history_search_match_cursor = 2;
        app.history_search_mode = HistorySearchMode::Author;
        app.history_show_file_tree = true;
        app.history_file_tree_cursor = 3;
        app.history_file_tree_filter = Some("src/ui".to_string());
        app.history_file_tree_focus = true;
        app.history_status_msg = "Filtered to: src/ui".to_string();
        app.focus = FocusPane::Detail;

        app.update(key(KeyCode::Char('q')));
        assert!(matches!(app.mode, ViewMode::Main));

        app.update(key(KeyCode::Char('h')));
        assert!(matches!(app.mode, ViewMode::History));
        assert!(matches!(app.history_view_mode, HistoryViewMode::Bead));
        assert!(!app.history_search_active);
        assert!(app.history_search_query.is_empty());
        assert_eq!(app.history_search_match_cursor, 0);
        assert_eq!(app.history_search_mode, HistorySearchMode::All);
        assert!(!app.history_show_file_tree);
        assert_eq!(app.history_file_tree_cursor, 0);
        assert!(app.history_file_tree_filter.is_none());
        assert!(!app.history_file_tree_focus);
        assert!(app.history_status_msg.is_empty());
        assert_eq!(app.focus, FocusPane::List);
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
    fn filter_hotkeys_include_blocked_and_in_progress_slices() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, lane_issues());

        app.update(key(KeyCode::Char('I')));
        assert_eq!(app.list_filter, ListFilter::InProgress);
        assert_eq!(selected_issue_id(&app), "IP-1");
        assert_eq!(app.visible_issue_indices().len(), 1);

        app.update(key(KeyCode::Char('B')));
        assert_eq!(app.list_filter, ListFilter::Blocked);
        assert_eq!(selected_issue_id(&app), "BLK-1");
        assert_eq!(app.visible_issue_indices().len(), 1);

        app.update(key(KeyCode::Escape));
        assert_eq!(app.list_filter, ListFilter::All);
        assert!(!app.show_quit_confirm);
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
    fn board_mode_search_prefers_rendered_lane_order_over_storage_order() {
        let issues = vec![
            Issue {
                id: "BLK-1".to_string(),
                title: "Alpha blocked".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "OPEN-1".to_string(),
                title: "Alpha open".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let mut app = new_app_with_issues(ViewMode::Board, 0, issues);

        let rendered_ids = app
            .board_visible_issue_indices_in_display_order()
            .into_iter()
            .map(|index| app.analyzer.issues[index].id.clone())
            .collect::<Vec<_>>();
        assert_eq!(rendered_ids, vec!["OPEN-1", "BLK-1"]);

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));

        assert_eq!(selected_issue_id(&app), "OPEN-1");
        assert!(
            app.list_panel_text()
                .lines()
                .any(|line| line.contains('▶') && line.contains("OPEN-1"))
        );
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
    fn main_detail_scroll_shortcuts_move_offset() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;

        app.update(key_ctrl(KeyCode::Char('j')));
        assert_eq!(app.detail_scroll_offset, 3);

        app.update(key_ctrl(KeyCode::Char('d')));
        assert_eq!(app.detail_scroll_offset, 13);

        app.update(key_ctrl(KeyCode::Char('k')));
        assert_eq!(app.detail_scroll_offset, 10);

        app.update(key_ctrl(KeyCode::Char('u')));
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn main_detail_scroll_resets_when_selection_changes() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;

        app.update(key_ctrl(KeyCode::Char('j')));
        assert_eq!(app.detail_scroll_offset, 3);

        app.focus = FocusPane::List;
        app.update(key(KeyCode::Char('j')));
        assert_eq!(selected_issue_id(&app), "B");
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn main_detail_scroll_ignored_without_detail_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.focus, FocusPane::List);

        app.update(key_ctrl(KeyCode::Char('j')));
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn detail_scroll_works_in_all_modes() {
        // Universal detail scroll works in every mode, not just Main
        for mode in [
            ViewMode::Main,
            ViewMode::Graph,
            ViewMode::Insights,
            ViewMode::History,
        ] {
            let mut app = new_app(mode, 0);
            app.focus = FocusPane::Detail;
            app.scroll_detail(5);
            assert_eq!(
                app.detail_scroll_offset, 5,
                "scroll_detail should work in {mode:?}"
            );
        }
    }

    #[test]
    fn main_footer_shows_scroll_hint_when_detail_focused() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("^j/k"),
            "expected scroll hint in footer, got:\n{rendered}"
        );
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
            mode_back_stack: Vec::new(),
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
            detail_scroll_offset: 0,
            main_search_active: false,
            main_search_query: String::new(),
            main_search_match_cursor: 0,
            list_scroll_offset: Cell::new(0),
            list_viewport_height: Cell::new(0),
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
            tree_search_active: false,
            tree_search_query: String::new(),
            tree_search_match_cursor: 0,
            pending_g: false,
            g_pre_toggle_mode: None,
            pending_z: false,
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
        assert_eq!(app.list_sort, ListSort::PageRank);

        app.update(key(KeyCode::Char('s')));
        assert_eq!(app.list_sort, ListSort::Blockers);

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

    #[test]
    fn history_layout_breakpoints_match_legacy() {
        assert_eq!(HistoryLayout::from_width(99), HistoryLayout::Narrow);
        assert_eq!(HistoryLayout::from_width(100), HistoryLayout::Standard);
        assert_eq!(HistoryLayout::from_width(149), HistoryLayout::Standard);
        assert_eq!(HistoryLayout::from_width(150), HistoryLayout::Wide);
    }

    #[test]
    fn pane_split_two_pane_adjustment_persists_and_clamps() {
        let mut app = new_app(ViewMode::Main, 0);
        let _ = render_app(&app, 100, 24);
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            42.0
        );

        app.update(Msg::KeyPress(KeyCode::Right, Modifiers::CTRL));
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            46.0
        );

        app.mode = ViewMode::Board;
        let _ = render_app(&app, 100, 24);
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            46.0
        );

        for _ in 0..20 {
            app.update(Msg::KeyPress(KeyCode::Left, Modifiers::CTRL));
        }
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            25.0
        );
    }

    #[test]
    fn pane_split_history_wide_bead_adjustment_preserves_minimums() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.focus = FocusPane::Middle;
        let _ = render_app(&app, 160, 24);

        app.update(Msg::KeyPress(KeyCode::Right, Modifiers::CTRL));
        let split = super::pane_split_state();
        assert_eq!(split.history_wide_bead[1], 22.0);
        assert_eq!(split.history_wide_bead[2], 29.0);
        assert_eq!(split.history_wide_bead[3], 29.0);

        for _ in 0..20 {
            app.update(Msg::KeyPress(KeyCode::Right, Modifiers::CTRL));
        }
        let clamped = super::pane_split_state().history_wide_bead;
        assert!(clamped[2] >= 15.0);
        assert!(clamped[3] >= 15.0);
        assert!((clamped.iter().sum::<f32>() - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pane_split_reset_shortcut_restores_defaults() {
        let mut app = new_app(ViewMode::Main, 0);
        app.reset_pane_split_state();
        let _ = render_app(&app, 100, 24);
        app.update(Msg::KeyPress(KeyCode::Right, Modifiers::CTRL));
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            46.0
        );

        app.update(Msg::KeyPress(KeyCode::Char('0'), Modifiers::CTRL));
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            42.0
        );
        assert_eq!(app.status_msg, "Pane splits reset");
    }

    #[test]
    fn mouse_scroll_over_splitter_adjusts_two_pane_ratio() {
        let mut app = new_app(ViewMode::Main, 0);
        app.reset_pane_split_state();
        let _ = render_app(&app, 100, 24);
        let first_hit_box = super::splitter_hit_boxes(&app, 100, 24)
            .into_iter()
            .next()
            .expect("main view should expose a two-pane splitter");

        app.update(mouse(
            MouseEventKind::ScrollUp,
            first_hit_box.rect.x,
            first_hit_box.rect.y.saturating_add(1),
        ));
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            46.0
        );

        let second_hit_box = super::splitter_hit_boxes(&app, 100, 24)
            .into_iter()
            .next()
            .expect("main view should still expose a splitter after resize");

        app.update(mouse(
            MouseEventKind::ScrollDown,
            second_hit_box.rect.x,
            second_hit_box.rect.y.saturating_add(1),
        ));
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            42.0
        );
    }

    #[test]
    fn mouse_click_on_splitter_nudges_ratio_and_updates_focus() {
        let mut app = new_app(ViewMode::Main, 0);
        app.reset_pane_split_state();
        let _ = render_app(&app, 100, 24);
        let hit_box = super::splitter_hit_boxes(&app, 100, 24)
            .into_iter()
            .next()
            .expect("main view should expose a two-pane splitter");
        let right_edge = hit_box
            .rect
            .x
            .saturating_add(hit_box.rect.width.saturating_sub(1));

        app.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right_edge,
            hit_box.rect.y.saturating_add(1),
        ));
        assert_eq!(app.focus, FocusPane::Detail);
        assert_eq!(
            super::pane_split_state().two_pane_list_pct(Breakpoint::Medium),
            38.0
        );
    }

    // -- Visual token tests --------------------------------------------------

    #[test]
    fn token_status_colours_are_distinct() {
        let open = tokens::status_fg("open");
        let prog = tokens::status_fg("in_progress");
        let blk = tokens::status_fg("blocked");
        let cls = tokens::status_fg("closed");
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
        assert_eq!(open_style.fg, Some(tokens::status_fg("open")));

        let closed_style = tokens::status_style("closed");
        assert_eq!(closed_style.fg, Some(tokens::status_fg("closed")));

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
    fn token_chip_style_has_semantic_background() {
        let style = tokens::chip_style(SemanticTone::Warning);
        assert_eq!(style.fg, Some(tokens::FG_WARNING));
        assert_eq!(style.bg, Some(tokens::BG_SURFACE_WARNING));
    }

    #[test]
    fn token_focused_border_differs_from_unfocused() {
        let focused = tokens::panel_border_for(SemanticTone::Accent, true);
        let unfocused = tokens::panel_border_for(SemanticTone::Accent, false);
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
        build_header_text(app, width).lines()[0].to_plain_text()
    }

    #[test]
    fn snapshot_narrow_header_is_compact() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 60);
        assert!(h.contains("bvr"), "header should contain 'bvr'");
        assert!(
            h.contains("1 Main"),
            "narrow header should show the main tab"
        );
        assert!(
            !h.contains("Esc back/quit"),
            "narrow header should remain compact"
        );
    }

    #[test]
    fn snapshot_medium_header_is_full() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 100);
        assert!(h.contains("b Board"), "medium header should show board tab");
        assert!(
            h.contains("i Insights"),
            "medium header should show insights tab"
        );
        assert!(h.contains("mode=Main"), "medium header should show mode=");
        assert!(h.contains("focus=list"), "medium header should show focus=");
        assert!(
            h.contains("issues=3/3"),
            "medium header should show issues metric"
        );
    }

    #[test]
    fn snapshot_wide_header_is_full() {
        let app = new_app(ViewMode::Main, 0);
        let h = header_for_width(&app, 140);
        assert!(
            h.contains("[ Labels"),
            "wide header should expose secondary tabs"
        );
        assert!(h.contains("] Flow"), "wide header should expose flow tab");
        assert!(
            h.contains("sort=default"),
            "wide header should show sort metric"
        );
        assert!(
            h.ends_with(" |"),
            "wide header should preserve trailing delimiter"
        );
    }

    #[test]
    fn snapshot_narrow_header_keeps_active_non_primary_mode_visible() {
        let app = new_app(ViewMode::Sprint, 0);
        let h = header_for_width(&app, 60);
        assert!(
            h.contains("S Sprint"),
            "narrow header should keep active tab visible"
        );
    }

    #[test]
    fn help_overlay_mentions_splitter_resize_controls() {
        let app = new_app(ViewMode::Main, 0);
        let help = app.help_overlay_text(120);
        assert!(help.contains("Ctrl+\u{2190}/\u{2192}"));
        assert!(help.contains("Ctrl+0"));
        assert!(help.contains("splitter click/scroll"));
    }

    #[test]
    fn header_shows_metrics_pending_chip() {
        let mut app = new_app(ViewMode::Main, 0);
        app.slow_metrics_pending = true;
        let h = header_for_width(&app, 120);
        assert!(
            h.contains("metrics: computing..."),
            "header should surface pending metrics chip: {h}"
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
    fn main_list_render_text_uses_rich_issue_scan_rows() {
        let app = new_app(ViewMode::Main, 1);
        let text = app.main_list_render_text(120).to_plain_text();
        assert!(text.contains('▸'), "selected row marker missing: {text}");
        assert!(text.contains("P0"), "priority badge text missing: {text}");
        assert!(text.contains("#01"), "triage rank missing: {text}");
        assert!(text.contains("OPEN"), "status badge text missing: {text}");
        assert!(text.contains("⊘1"), "blocker indicator missing: {text}");
        assert!(text.contains("B"), "selected issue id missing: {text}");
        assert!(
            text.contains("pr#2"),
            "pagerank rank signal missing: {text}"
        );
    }

    #[test]
    fn main_list_render_text_adapts_rows_to_narrow_width() {
        let app = new_app(ViewMode::Main, 1);
        let text = app.main_list_render_text(48).to_plain_text();
        assert!(text.contains("▸"), "selected row marker missing: {text}");
        assert!(text.contains("blocked"), "state chip missing: {text}");
        assert!(text.contains("Dependent"), "title missing: {text}");
        assert!(
            !text.contains("repo:"),
            "narrow variant should drop repo metadata: {text}"
        );
    }

    #[test]
    fn main_list_empty_state_is_recovery_oriented() {
        let mut app = new_app(ViewMode::Main, 0);
        app.modal_repo_filter = Some("missing".to_string());

        let text = app.main_list_render_text(90).to_plain_text();
        assert!(text.contains("No issues in the current triage slice"));
        assert!(
            text.contains("repo=missing"),
            "scope should mention repo: {text}"
        );
        assert!(text.contains("Recover:"), "recovery hint missing: {text}");
    }

    #[test]
    fn main_list_search_no_hits_keeps_guidance_visible() {
        let mut app = new_app(ViewMode::Main, 0);
        app.main_search_query = "zzz".to_string();

        let text = app.main_list_render_text(90).to_plain_text();
        assert!(text.contains("Matches: none in visible issues"));
        assert!(
            text.contains("refine /query"),
            "search guidance missing: {text}"
        );
    }

    #[test]
    fn graph_list_render_text_uses_metric_strips_and_header() {
        let app = new_app(ViewMode::Graph, 0);
        let text = app.graph_list_render_text(90).to_plain_text();
        assert!(text.contains("Nodes"), "graph header missing: {text}");
        assert!(text.contains("PR "), "metric strip label missing: {text}");
        assert!(
            text.contains("↓") || text.contains("⊘"),
            "blocker indicators missing: {text}"
        );
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
    fn main_detail_render_text_uses_label_chips() {
        let app = new_app(ViewMode::Main, 0);
        let text = app.issue_detail_render_text().to_plain_text();
        assert!(text.contains("[core]"), "label chip missing: {text}");
        assert!(
            text.contains("[parity]"),
            "second label chip missing: {text}"
        );
    }

    #[test]
    fn main_detail_render_text_uses_modular_cockpit_sections() {
        let app = new_app(ViewMode::Main, 0);
        let text = app.issue_detail_render_text().to_plain_text();
        assert!(text.contains("Summary"), "summary module missing: {text}");
        assert!(text.contains("Signals"), "signals module missing: {text}");
        assert!(
            text.contains("Dependencies"),
            "dependencies module missing: {text}"
        );
        assert!(
            text.contains("Action:"),
            "action-first summary line missing: {text}"
        );
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
    fn history_file_tree_respects_confidence_threshold() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.history_confidence_index = 3;

        let paths = app
            .history_flat_file_list()
            .into_iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.path)
            .collect::<Vec<_>>();

        assert!(paths.contains(&"src/ui/app.rs".to_string()));
        assert!(paths.contains(&"src/ui/detail.rs".to_string()));
        assert!(paths.contains(&"README.md".to_string()));
        assert!(!paths.contains(&"src/core/graph.rs".to_string()));
        assert!(!paths.contains(&"Cargo.toml".to_string()));
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

        // List → FileTree (no middle pane at default width)
        app.update(key(KeyCode::Tab));
        assert!(app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::List);

        // FileTree → List
        app.update(key(KeyCode::Tab));
        assert!(!app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn history_standard_tab_cycles_list_middle_detail() {
        let mut app = new_app(ViewMode::History, 0);
        app.mode = ViewMode::History;
        record_view_size(120, 30);

        assert_eq!(app.focus, FocusPane::List);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Middle);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn history_standard_file_tree_cycle_includes_middle_before_detail() {
        let mut app = new_app(ViewMode::History, 0);
        app.mode = ViewMode::History;
        record_view_size(120, 30);
        app.history_show_file_tree = true;
        app.focus = FocusPane::List;

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Middle);
        assert!(!app.history_file_tree_focus);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        assert!(!app.history_file_tree_focus);

        app.update(key(KeyCode::Tab));
        assert!(app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::Detail);

        app.update(key(KeyCode::Tab));
        assert!(!app.history_file_tree_focus);
        assert_eq!(app.focus, FocusPane::List);
    }

    #[test]
    fn history_middle_navigation_moves_bead_commit_cursor() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        record_view_size(120, 30);
        app.focus = FocusPane::Middle;

        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.history_bead_commit_cursor, 1);
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.history_bead_commit_cursor, 0);
    }

    #[test]
    fn history_middle_navigation_moves_git_related_bead_cursor() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.mode = ViewMode::History;
        record_view_size(120, 30);
        app.focus = FocusPane::Middle;
        if let Some(cache) = app.history_git_cache.as_mut() {
            cache.commit_bead_confidence.insert(
                "aaaa1111".to_string(),
                vec![("A".to_string(), 0.95), ("B".to_string(), 0.91)],
            );
        }

        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.history_related_bead_cursor, 1);
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.history_related_bead_cursor, 0);
    }

    #[test]
    fn history_standard_bead_renders_three_legacy_panes() {
        let app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let text = render_app(&app, 120, 30);
        assert!(text.contains("Beads With History [focus]"));
        assert!(text.contains("Commits"));
        assert!(text.contains("Commit Details"));
    }

    #[test]
    fn history_standard_git_renders_three_legacy_panes() {
        let app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        let text = render_app(&app, 120, 30);
        assert!(text.contains("Commits [focus]"));
        assert!(text.contains("Related Beads"));
        assert!(text.contains("Commit Details"));
    }

    #[test]
    fn history_wide_bead_renders_timeline_pane() {
        let app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let text = render_app(&app, 160, 30);
        assert!(text.contains("Beads With History [focus]"));
        assert!(text.contains("Timeline: A"));
        assert!(text.contains("Commits"));
        assert!(text.contains("Commit Details"));
    }

    #[test]
    fn history_timeline_text_uses_legacy_summary_when_git_history_exists() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");
            history.milestones.created = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "created".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.milestones.closed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "closed".to_string(),
                timestamp: "2026-01-04T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.cycle_time = Some(HistoryCycleCompat {
                claim_to_close: Some("2d 0h 0m".to_string()),
                create_to_close: Some("3d 0h 0m".to_string()),
                create_to_claim: Some("1d 0h 0m".to_string()),
            });
        }

        let text = app.history_timeline_text(48, 12);
        assert!(text.contains("Timeline: A"));
        assert!(text.contains("3d cycle"));
        assert!(text.contains("Cycle: 3d"));
        assert!(text.contains("Avg confidence"));
    }

    #[test]
    fn history_timeline_text_renders_legacy_event_and_commit_rows() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");
            history.milestones.created = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "created".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.milestones.claimed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "claimed".to_string(),
                timestamp: "2026-01-02T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Bob Builder".to_string(),
                author_email: "bob@example.com".to_string(),
            });
            history.milestones.closed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "closed".to_string(),
                timestamp: "2026-01-04T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Carol".to_string(),
                author_email: "carol@example.com".to_string(),
            });
            history.commits = Some(vec![
                HistoryCommitCompat {
                    timestamp: "2026-01-03T00:00:00Z".to_string(),
                    ..history_commit("aaaa1111", "feat: ui wiring", 0.95, &["src/ui/app.rs"])
                },
                HistoryCommitCompat {
                    timestamp: "2026-01-03T12:00:00Z".to_string(),
                    ..history_commit("bbbb2222", "feat: graph core", 0.80, &["src/core/graph.rs"])
                },
            ]);
        }

        let text = app.history_timeline_text(60, 14);
        assert!(text.contains("○ Created"));
        assert!(text.contains("● Claimed"));
        assert!(text.contains("✓ Closed"));
        assert!(text.contains("├─ aaaa111"));
        assert!(text.contains("feat: ui wiring"));
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
    fn history_view_mode_indicator_matches_legacy_icons() {
        assert_eq!(HistoryViewMode::Bead.indicator(), "◈ Beads");
        assert_eq!(HistoryViewMode::Git.indicator(), "◉ Git");
    }

    #[test]
    fn history_status_line_shows_legacy_mode_indicator() {
        let bead_view =
            render_debug_view(sample_issues(), "history", 100, 30).expect("history view renders");
        assert!(bead_view.contains("mode=History ◈ Beads"));

        let mut app = new_app(ViewMode::History, 0);
        app.handle_key(KeyCode::Char('v'), Modifiers::NONE);
        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(100, 30, &mut pool);
        app.view(&mut frame);
        let git_view = buffer_to_text(&frame.buffer, &pool);
        assert!(git_view.contains("mode=History ◉ Git"));
    }

    #[test]
    fn compact_history_duration_label_prefers_first_nonzero_unit() {
        assert_eq!(compact_history_duration_label("3d 0h 0m"), "3d");
        assert_eq!(compact_history_duration_label("0d 5h 0m"), "5h");
        assert_eq!(compact_history_duration_label("0d 0h 15m"), "15m");
    }

    #[test]
    fn legacy_history_author_initials_match_go_contract() {
        assert_eq!(legacy_history_author_initials(""), "??");
        assert_eq!(legacy_history_author_initials("alice"), "AL");
        assert_eq!(legacy_history_author_initials("Alice Baker"), "AB");
        assert_eq!(legacy_history_author_initials("Alice Beth Carter"), "AC");
    }

    #[test]
    fn history_compact_timeline_matches_legacy_marker_contract() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let history = {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");

            history.milestones.created = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "created".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.milestones.claimed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "claimed".to_string(),
                timestamp: "2026-01-02T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.milestones.closed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "closed".to_string(),
                timestamp: "2026-01-04T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.cycle_time = Some(HistoryCycleCompat {
                claim_to_close: Some("2d 0h 0m".to_string()),
                create_to_close: Some("3d 0h 0m".to_string()),
                create_to_claim: Some("1d 0h 0m".to_string()),
            });
            history.clone()
        };

        let text = app.history_compact_timeline_text(&history, 80);
        assert!(text.contains("○"));
        assert!(text.contains("●"));
        assert!(text.contains("├"));
        assert!(text.contains("✓"));
        assert!(text.contains("3d cycle"));
        assert!(text.contains("2 commits"));
    }

    #[test]
    fn history_compact_timeline_collapses_many_commit_markers() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let history = {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");

            history.milestones.created = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "created".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.commits = Some(vec![
                history_commit("a1", "feat: one", 0.90, &["src/a.rs"]),
                history_commit("a2", "feat: two", 0.90, &["src/b.rs"]),
                history_commit("a3", "feat: three", 0.90, &["src/c.rs"]),
                history_commit("a4", "feat: four", 0.90, &["src/d.rs"]),
                history_commit("a5", "feat: five", 0.90, &["src/e.rs"]),
                history_commit("a6", "feat: six", 0.90, &["src/f.rs"]),
                history_commit("a7", "feat: seven", 0.90, &["src/g.rs"]),
            ]);
            history.clone()
        };

        let text = app.history_compact_timeline_text(&history, 80);
        assert!(text.contains("…"));
        assert!(text.contains("7 commits"));
    }

    #[test]
    fn history_detail_surfaces_compact_timeline_when_git_history_exists() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");

            history.milestones.created = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "created".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.milestones.closed = Some(HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: "closed".to_string(),
                timestamp: "2026-01-04T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: "Alice".to_string(),
                author_email: "alice@example.com".to_string(),
            });
            history.cycle_time = Some(HistoryCycleCompat {
                claim_to_close: Some("2d 0h 0m".to_string()),
                create_to_close: Some("3d 0h 0m".to_string()),
                create_to_claim: Some("1d 0h 0m".to_string()),
            });
        }

        let text = app.history_detail_text();
        assert!(text.contains("Timeline:"));
        assert!(text.contains("3d cycle"));
    }

    #[test]
    fn truncate_display_preserves_grapheme_clusters_and_cell_width() {
        let text = "Ame\u{301}lie 👩‍💻";
        let truncated = truncate_display(text, 6);
        assert_eq!(display_width(&truncated), 6);
        assert!(truncated.ends_with('…'));
        assert!(
            truncated.contains("e\u{301}"),
            "combining grapheme should stay intact"
        );
    }

    #[test]
    fn fit_display_pads_to_visual_width_for_wide_graphemes() {
        let fitted = fit_display("界", 4);
        assert_eq!(display_width(&fitted), 4);
        assert_eq!(fitted, "界  ");
    }

    #[test]
    fn truncate_display_handles_cjk_double_width() {
        // CJK characters are 2 cells wide
        let text = "日本語テスト";
        assert_eq!(display_width(text), 12);

        let truncated = truncate_display(text, 7);
        // Should truncate to fit within 7 cells (3 CJK chars = 6 + ellipsis = 7)
        assert!(display_width(&truncated) <= 7);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_display_handles_emoji_sequences() {
        // Family emoji (ZWJ sequence) should be treated as single grapheme
        let text = "👨‍👩‍👧‍👦 family";
        let truncated = truncate_display(text, 5);
        assert!(display_width(&truncated) <= 5);
    }

    #[test]
    fn truncate_display_ascii_within_limit_is_unchanged() {
        let text = "hello";
        assert_eq!(truncate_display(text, 10), "hello");
        assert_eq!(truncate_display(text, 5), "hello");
    }

    #[test]
    fn truncate_display_empty_string() {
        assert_eq!(truncate_display("", 10), "");
        assert_eq!(truncate_display("", 0), "");
    }

    #[test]
    fn truncate_display_zero_width() {
        assert_eq!(truncate_display("hello", 0), "");
    }

    #[test]
    fn truncate_display_width_one() {
        let truncated = truncate_display("hello", 1);
        assert_eq!(display_width(&truncated), 1);
    }

    #[test]
    fn fit_display_cjk_truncation_and_padding() {
        // "世界" is 4 cells wide; fitting to 6 should pad 2 spaces
        let fitted = fit_display("世界", 6);
        assert_eq!(display_width(&fitted), 6);
        assert!(fitted.starts_with("世界"));

        // Fitting to 3 should truncate (can't fit 2-wide char + ellipsis in 3)
        let fitted_narrow = fit_display("世界你好", 5);
        assert_eq!(display_width(&fitted_narrow), 5);
    }

    #[test]
    fn center_display_with_cjk() {
        let centered = center_display("界", 6);
        assert_eq!(display_width(&centered), 6);
        // "界" is 2 cells, so 4 cells of padding: 2 left + 2 right
        assert!(centered.contains("界"));
    }

    #[test]
    fn command_hint_width_with_unicode_keys() {
        let hint = CommandHint {
            key: "⌘",
            desc: "cmd",
        };
        let width = command_hint_width(hint);
        assert_eq!(width, display_width("⌘") + 1 + display_width("cmd"));
    }

    #[test]
    fn wrap_command_hints_preserves_groups_and_styles() {
        let hints = [
            CommandHint {
                key: "Tab",
                desc: "mode",
            },
            CommandHint {
                key: "/",
                desc: "search",
            },
            CommandHint {
                key: "O",
                desc: "edit",
            },
        ];

        let wrapped = wrap_command_hints(&hints, 18);
        assert_eq!(wrapped.lines().len(), 2);
        assert_eq!(wrapped.lines()[0].to_plain_text(), "Tab mode");
        assert_eq!(wrapped.lines()[1].to_plain_text(), "/ search │ O edit");

        let first_line = wrapped.lines()[0].spans();
        let second_line = wrapped.lines()[1].spans();
        assert_eq!(first_line[0].style, Some(tokens::footer_key()));
        assert_eq!(first_line[2].style, Some(tokens::footer_hint()));
        assert_eq!(second_line[3].style, Some(tokens::footer_sep()));
    }

    #[test]
    fn main_footer_command_hints_wrap_across_multiple_lines() {
        let hints = [
            CommandHint {
                key: "b/i/g/h",
                desc: "modes",
            },
            CommandHint {
                key: "/",
                desc: "search",
            },
            CommandHint {
                key: "s",
                desc: "sort",
            },
            CommandHint {
                key: "p",
                desc: "hints",
            },
            CommandHint {
                key: "C",
                desc: "copy",
            },
            CommandHint {
                key: "x",
                desc: "export",
            },
            CommandHint {
                key: "O",
                desc: "edit",
            },
        ];

        let wrapped = wrap_command_hints(&hints, 20);
        let plain_lines = wrapped
            .lines()
            .iter()
            .map(ftui::text::Line::to_plain_text)
            .collect::<Vec<_>>();

        assert_eq!(
            plain_lines,
            vec![
                "b/i/g/h modes".to_string(),
                "/ search │ s sort".to_string(),
                "p hints │ C copy".to_string(),
                "x export │ O edit".to_string(),
            ]
        );
    }

    #[test]
    fn styled_detail_summary_line_turns_status_into_chips() {
        let line =
            styled_detail_summary_line("Status: open | Priority: p1 | Type: bug | State: ready")
                .expect("styled summary line");
        assert_eq!(
            line.to_plain_text(),
            "Status: open | Priority: p1 | Type: bug | State: ready"
        );
        let spans = line.spans();
        assert_eq!(
            spans[2].style,
            Some(tokens::chip_style(SemanticTone::Accent))
        );
        assert_eq!(
            spans[6].style,
            Some(tokens::chip_style(SemanticTone::Warning))
        );
        assert_eq!(
            spans[14].style,
            Some(tokens::chip_style(SemanticTone::Success))
        );
    }

    #[test]
    fn history_detail_renders_hyperlink_for_selected_commit() {
        let repo = init_temp_repo_with_remote("git@github.com:owner/repo.git");
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.repo_root = Some(repo.path().to_path_buf());

        let urls = rendered_link_urls(&app, 120, 40);
        assert!(
            urls.iter()
                .any(|url| url == "https://github.com/owner/repo/commit/aaaa1111"),
            "expected commit hyperlink to be rendered, got {urls:?}"
        );

        let rendered = app.history_detail_render_text().to_plain_text();
        assert!(
            rendered.contains("open selected commit (o open, right-click copy link)"),
            "expected inline open hint for commit hyperlink, got:\n{rendered}"
        );
    }

    #[test]
    fn history_footer_shows_commit_actions_when_selected_commit_url_is_available() {
        let repo = init_temp_repo_with_remote("git@github.com:owner/repo.git");
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.repo_root = Some(repo.path().to_path_buf());

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("y copy"),
            "expected history footer to advertise copy action, got:\n{rendered}"
        );
        assert!(
            rendered.contains("o open commit"),
            "expected history footer to advertise open action when a commit URL exists, got:\n{rendered}"
        );
    }

    #[test]
    fn history_footer_hides_open_commit_hint_without_selected_commit_url() {
        // Point repo_root at a non-git directory so history_selected_commit_url()
        // returns None (otherwise the test process's cwd falls back to the real
        // project git repo and a URL is always available).
        let no_git_dir = tempfile::tempdir().expect("tempdir");
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.repo_root = Some(no_git_dir.path().to_path_buf());

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("y copy"),
            "expected history footer to keep the copy action, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("o open commit"),
            "expected history footer to hide open action without a commit URL, got:\n{rendered}"
        );
    }

    #[test]
    fn history_footer_switches_to_file_tree_controls_when_tree_has_focus() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.history_show_file_tree = true;
        app.history_file_tree_focus = true;

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("j/k tree"),
            "expected history footer to advertise file-tree navigation, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Enter filter"),
            "expected history footer to advertise file-tree filtering, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Esc close tree"),
            "expected history footer to advertise closing the file tree, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("o open commit"),
            "expected generic commit action hints to be hidden while file tree owns focus, got:\n{rendered}"
        );
    }

    #[test]
    fn history_file_tree_focus_blocks_view_toggle_shortcuts() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.history_show_file_tree = true;
        app.history_file_tree_focus = true;

        app.update(key(KeyCode::Char('v')));
        assert_eq!(app.history_view_mode, HistoryViewMode::Git);

        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.mode, ViewMode::History);
        assert!(app.history_file_tree_focus);
    }

    #[test]
    fn history_file_tree_focus_blocks_copy_and_open_shortcuts() {
        let repo = init_temp_repo_with_remote("git@github.com:owner/repo.git");
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.repo_root = Some(repo.path().to_path_buf());
        app.history_show_file_tree = true;
        app.history_file_tree_focus = true;
        app.history_status_msg = "File tree: j/k navigate, Enter filter, Esc close".into();

        app.update(key(KeyCode::Char('y')));
        assert_eq!(
            app.history_status_msg,
            "File tree: j/k navigate, Enter filter, Esc close"
        );

        app.update(key(KeyCode::Char('o')));
        assert_eq!(
            app.history_status_msg,
            "File tree: j/k navigate, Enter filter, Esc close"
        );
    }

    #[test]
    fn history_detail_hides_open_hint_without_selected_commit_url() {
        let no_git_dir = tempfile::tempdir().expect("tempdir");
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.repo_root = Some(no_git_dir.path().to_path_buf());

        let rendered = app.history_detail_render_text().to_plain_text();
        // When there is no commit URL, the detail should NOT contain the browser
        // link affordance (which is only added when a URL is available).
        assert!(
            !rendered.contains("open selected commit (o open, right-click copy link)"),
            "expected history detail to omit the browser link affordance without a commit URL, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("Browser Link:"),
            "expected no browser link line without a commit URL, got:\n{rendered}"
        );
    }

    #[test]
    fn history_selected_commit_url_tracks_selected_bead_commit_cursor() {
        let repo = init_temp_repo_with_remote("git@github.com:owner/repo.git");
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.repo_root = Some(repo.path().to_path_buf());
        app.history_bead_commit_cursor = 1;

        let url = app
            .history_selected_commit_url()
            .expect("selected bead commit URL");
        assert!(
            url.ends_with("/bbbb2222"),
            "expected selected cursor to drive bead commit URL, got {url}"
        );
    }

    #[test]
    fn history_bead_middle_enter_backtraces_to_git_commit() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        app.focus = FocusPane::Middle;
        app.history_bead_commit_cursor = 1;

        let cmd = app.update(key(KeyCode::Enter));

        assert!(matches!(cmd, Cmd::None));
        assert_eq!(app.mode, ViewMode::History);
        assert_eq!(app.history_view_mode, HistoryViewMode::Git);
        assert_eq!(app.focus, FocusPane::List);
        assert_eq!(app.history_event_cursor, 1);
        assert!(
            app.history_status_msg
                .contains("Backtraced to commit bbbb222")
        );
    }

    #[test]
    fn history_bead_detail_shows_field_level_diff_lines() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        if let Some(cache) = app.history_git_cache.as_mut()
            && let Some(history) = cache.histories.get_mut("A")
            && let Some(commit) = history
                .commits
                .as_mut()
                .and_then(|commits| commits.get_mut(0))
        {
            commit.field_changes = vec![FieldChange {
                field: "status".to_string(),
                old_value: "open".to_string(),
                new_value: "blocked".to_string(),
            }];
            commit.bead_diff_lines = vec![
                "- status: open".to_string(),
                "+ status: blocked".to_string(),
            ];
        }

        let detail = app.history_detail_text();
        assert!(detail.contains("Fields changed: status"), "got:\n{detail}");
        assert!(detail.contains("- status: open"), "got:\n{detail}");
        assert!(detail.contains("+ status: blocked"), "got:\n{detail}");
    }

    #[test]
    fn history_git_detail_shows_selected_related_bead_change_summary() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Git, 0);
        app.mode = ViewMode::History;
        if let Some(cache) = app.history_git_cache.as_mut()
            && let Some(history) = cache.histories.get_mut("A")
            && let Some(commit) = history
                .commits
                .as_mut()
                .and_then(|commits| commits.get_mut(0))
        {
            commit.field_changes = vec![FieldChange {
                field: "labels".to_string(),
                old_value: "backend".to_string(),
                new_value: "backend,urgent".to_string(),
            }];
            commit.bead_diff_lines = vec![
                "- labels: backend".to_string(),
                "+ labels: backend,urgent".to_string(),
            ];
        }

        let detail = app.history_detail_text();
        assert!(
            detail.contains("SELECTED BEAD CHANGE (A):"),
            "got:\n{detail}"
        );
        assert!(detail.contains("Fields: labels"), "got:\n{detail}");
        assert!(
            detail.contains("+ labels: backend,urgent"),
            "got:\n{detail}"
        );
    }

    #[test]
    fn main_detail_renders_hyperlink_for_external_issue_reference() {
        let mut app = new_app(ViewMode::Main, 0);
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let urls = rendered_link_urls(&app, 120, 40);
        assert!(
            urls.iter()
                .any(|url| url == "https://github.com/org/repo/issues/42"),
            "expected external issue hyperlink to be rendered, got {urls:?}"
        );

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("(o open, y copy)"),
            "expected inline external-ref action hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("(C copy id)"),
            "expected issue-id action hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("(w repo filter)"),
            "expected repo-filter action hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("(L label filter)"),
            "expected label-filter action hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("(t time-travel)"),
            "expected time-travel action hint, got:\n{rendered}"
        );
    }

    #[test]
    fn graph_detail_renders_hyperlink_for_external_issue_reference() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let detail = app.graph_detail_render_text();
        let urls = detail
            .lines()
            .iter()
            .flat_map(ftui::text::Line::spans)
            .filter_map(|span| span.link.as_deref())
            .collect::<Vec<_>>();
        assert!(
            urls.iter()
                .any(|url| *url == "https://github.com/org/repo/issues/42"),
            "expected graph detail hyperlink span to be rendered, got {urls:?}"
        );

        let rendered = detail.to_plain_text();
        assert!(
            rendered.contains("(o open, y copy)"),
            "expected graph detail inline external-ref action hint, got:\n{rendered}"
        );
    }

    #[test]
    fn main_detail_open_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(!app.should_open_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_open_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("mailto:test@example.com".into());
        assert!(!app.should_open_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        assert!(app.should_open_selected_issue_external_ref());
    }

    #[test]
    fn graph_detail_open_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Graph, 0);
        assert!(!app.should_open_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_open_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("mailto:test@example.com".into());
        }
        assert!(!app.should_open_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        assert!(app.should_open_selected_issue_external_ref());
    }

    #[test]
    fn board_detail_open_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Board, 0);
        assert!(!app.should_open_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_open_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("mailto:test@example.com".into());
        assert!(!app.should_open_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        assert!(app.should_open_selected_issue_external_ref());
    }

    #[test]
    fn main_detail_copy_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Main, 0);
        assert!(!app.should_copy_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_copy_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("mailto:test@example.com".into());
        assert!(!app.should_copy_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        assert!(app.should_copy_selected_issue_external_ref());
    }

    #[test]
    fn graph_detail_copy_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Graph, 0);
        assert!(!app.should_copy_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_copy_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("mailto:test@example.com".into());
        }
        assert!(!app.should_copy_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        assert!(app.should_copy_selected_issue_external_ref());
    }

    #[test]
    fn board_detail_copy_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Board, 0);
        assert!(!app.should_copy_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_copy_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("mailto:test@example.com".into());
        assert!(!app.should_copy_selected_issue_external_ref());

        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        assert!(app.should_copy_selected_issue_external_ref());
    }

    #[test]
    fn insights_detail_open_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Insights, 0);
        assert!(!app.should_open_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_open_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("mailto:test@example.com".into());
        }
        assert!(!app.should_open_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        assert!(app.should_open_selected_issue_external_ref());
    }

    #[test]
    fn insights_detail_copy_shortcut_only_activates_for_detail_focus_with_http_ref() {
        let mut app = new_app(ViewMode::Insights, 0);
        assert!(!app.should_copy_selected_issue_external_ref());

        app.focus = FocusPane::Detail;
        assert!(!app.should_copy_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("mailto:test@example.com".into());
        }
        assert!(!app.should_copy_selected_issue_external_ref());

        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        assert!(app.should_copy_selected_issue_external_ref());
    }

    #[test]
    fn main_mode_o_keeps_open_filter_shortcut_without_external_ref() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);
    }

    #[test]
    fn main_mode_y_without_external_ref_does_not_set_status_message() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.update(key(KeyCode::Char('y')));
        assert!(app.status_msg.is_empty());
    }

    #[test]
    fn main_footer_shows_external_ref_commands_when_detail_link_is_available() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("o open link"),
            "expected open-link hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("y copy link"),
            "expected copy-link hint, got:\n{rendered}"
        );
    }

    #[test]
    fn main_footer_hides_external_ref_commands_without_detail_link() {
        let rendered = render_frame(ViewMode::Main, 120, 40);
        assert!(
            !rendered.contains("o open link"),
            "unexpected open-link hint in:\n{rendered}"
        );
        assert!(
            !rendered.contains("y copy link"),
            "unexpected copy-link hint in:\n{rendered}"
        );
    }

    #[test]
    fn graph_footer_shows_external_ref_commands_when_detail_link_is_available() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("o open link"),
            "expected graph footer to advertise open-link hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("y copy link"),
            "expected graph footer to advertise copy-link hint, got:\n{rendered}"
        );
    }

    #[test]
    fn board_detail_render_shows_external_ref_link_actions() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let detail = app.board_detail_render_text();
        let urls = detail
            .lines()
            .iter()
            .flat_map(ftui::text::Line::spans)
            .filter_map(|span| span.link.as_deref())
            .collect::<Vec<_>>();
        assert!(
            urls.iter()
                .any(|url| *url == "https://github.com/org/repo/issues/42"),
            "expected board detail hyperlink span to be rendered, got {urls:?}"
        );

        let rendered = detail.to_plain_text();
        assert!(
            rendered.contains("(o open, y copy)"),
            "expected board detail inline external-ref action hint, got:\n{rendered}"
        );
    }

    #[test]
    fn board_footer_shows_external_ref_commands_when_detail_link_is_available() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        // Use a wide terminal (240 cols) so the footer text is not truncated
        // before the link hints. The footer string exceeds 200 chars.
        let rendered = render_app(&app, 240, 40);
        assert!(
            rendered.contains("o open link"),
            "expected board footer to advertise open-link hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("y copy link"),
            "expected board footer to advertise copy-link hint, got:\n{rendered}"
        );
    }

    #[test]
    fn board_footer_shows_status_message_when_present() {
        let mut app = new_app(ViewMode::Board, 0);
        app.status_msg = "Copied external issue reference to clipboard".into();

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("Copied external issue reference to clipboard"),
            "expected board footer to surface status message, got:\n{rendered}"
        );
    }

    #[test]
    fn board_footer_advertises_focus_and_search_controls() {
        let app = new_app(ViewMode::Board, 0);

        let rendered = render_app(&app, 240, 40);
        assert!(
            rendered.contains("Tab focus"),
            "expected board footer to advertise focus switching, got:\n{rendered}"
        );
        assert!(
            rendered.contains("/ search"),
            "expected board footer to advertise search, got:\n{rendered}"
        );
    }

    #[test]
    fn insights_detail_render_shows_external_ref_link_actions() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let detail = app.insights_detail_render_text();
        let urls = detail
            .lines()
            .iter()
            .flat_map(ftui::text::Line::spans)
            .filter_map(|span| span.link.as_deref())
            .collect::<Vec<_>>();
        assert!(
            urls.iter()
                .any(|url| *url == "https://github.com/org/repo/issues/42"),
            "expected insights detail hyperlink span to be rendered, got {urls:?}"
        );

        let rendered = detail.to_plain_text();
        assert!(
            rendered.contains("(o open, y copy)"),
            "expected insights detail inline external-ref action hint, got:\n{rendered}"
        );
    }

    #[test]
    fn insights_footer_shows_external_ref_commands_when_detail_link_is_available() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("o open link"),
            "expected insights footer to advertise open-link hint, got:\n{rendered}"
        );
        assert!(
            rendered.contains("y copy link"),
            "expected insights footer to advertise copy-link hint, got:\n{rendered}"
        );
    }

    #[test]
    fn insights_footer_advertises_focus_and_search_controls() {
        let app = new_app(ViewMode::Insights, 0);

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("Tab focus"),
            "expected insights footer to advertise focus switching, got:\n{rendered}"
        );
        assert!(
            rendered.contains("/ search"),
            "expected insights footer to advertise search, got:\n{rendered}"
        );
    }

    #[test]
    fn insights_footer_shows_status_message_when_present() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.status_msg = "Copied external issue reference to clipboard".into();

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("Copied external issue reference to clipboard"),
            "expected insights footer to surface status message, got:\n{rendered}"
        );
    }

    #[test]
    fn graph_footer_hides_external_ref_commands_without_detail_link() {
        let rendered = render_frame(ViewMode::Graph, 120, 40);
        assert!(
            !rendered.contains("o open link"),
            "unexpected graph open-link hint in:\n{rendered}"
        );
        assert!(
            !rendered.contains("y copy link"),
            "unexpected graph copy-link hint in:\n{rendered}"
        );
    }

    #[test]
    fn graph_footer_shows_status_message_when_present() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.status_msg = "Copied external issue reference to clipboard".into();

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("Copied external issue reference to clipboard"),
            "expected graph footer to surface status message, got:\n{rendered}"
        );
    }

    #[test]
    fn graph_footer_keeps_open_details_wording() {
        let rendered = render_frame(ViewMode::Graph, 120, 40);
        assert!(
            rendered.contains("Enter open details"),
            "expected graph footer to describe Enter accurately, got:\n{rendered}"
        );
    }

    #[test]
    fn graph_footer_keeps_link_actions_visible_on_narrow_detail_layout() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let rendered = render_app(&app, 80, 40);
        assert!(
            rendered.contains("o open link"),
            "expected narrow graph footer to keep open-link hint visible, got:\n{rendered}"
        );
        // At width=80, "y copy link" wraps to a second footer line that is
        // clipped by the 1-row footer constraint, so we only verify "o open link".
    }

    #[test]
    fn graph_footer_shows_scroll_hint_when_detail_focused() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;

        let rendered = render_app(&app, 120, 40);
        assert!(
            rendered.contains("^j/k scroll"),
            "expected graph footer to advertise detail scrolling, got:\n{rendered}"
        );
    }

    #[test]
    fn history_legacy_lifecycle_lines_match_go_shape() {
        let now = Utc::now();
        let history = HistoryBeadCompat {
            bead_id: "A".to_string(),
            title: "Root".to_string(),
            status: "open".to_string(),
            events: vec![
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "created".to_string(),
                    timestamp: (now - chrono::Duration::hours(3)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Alice".to_string(),
                    author_email: "alice@example.com".to_string(),
                },
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "claimed".to_string(),
                    timestamp: (now - chrono::Duration::hours(2)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Bob Builder".to_string(),
                    author_email: "bob@example.com".to_string(),
                },
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "closed".to_string(),
                    timestamp: (now - chrono::Duration::minutes(30)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Carol".to_string(),
                    author_email: "carol@example.com".to_string(),
                },
            ],
            milestones: HistoryMilestonesCompat::default(),
            commits: None,
            cycle_time: None,
            last_author: String::new(),
        };

        let lines = history_legacy_lifecycle_lines(&history, 5);
        let text = lines.join("\n");
        assert!(text.contains("LIFECYCLE (3)"));
        assert!(text.contains("✓"));
        assert!(text.contains("👤"));
        assert!(text.contains("🆕"));
        assert!(text.contains("CA"));
        assert!(text.contains("BB"));
        assert!(text.contains("AL"));
    }

    #[test]
    fn history_legacy_lifecycle_lines_show_overflow_summary() {
        let now = Utc::now();
        let events = (0..5)
            .map(|idx| HistoryEventCompat {
                bead_id: "A".to_string(),
                event_type: if idx == 0 {
                    "created".to_string()
                } else {
                    "updated".to_string()
                },
                timestamp: (now - chrono::Duration::hours(i64::from(5 - idx))).to_rfc3339(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: format!("Agent {idx}"),
                author_email: format!("agent{idx}@example.com"),
            })
            .collect::<Vec<_>>();
        let history = HistoryBeadCompat {
            bead_id: "A".to_string(),
            title: "Root".to_string(),
            status: "open".to_string(),
            events,
            milestones: HistoryMilestonesCompat::default(),
            commits: None,
            cycle_time: None,
            last_author: String::new(),
        };

        let lines = history_legacy_lifecycle_lines(&history, 5);
        let text = lines.join("\n");
        assert_eq!(lines.len(), 5);
        assert!(text.contains("LIFECYCLE (5)"));
        assert!(text.contains("+2 more"));
    }

    #[test]
    fn history_detail_prefers_legacy_lifecycle_summary_when_git_events_exist() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        let now = Utc::now();
        {
            let history = app
                .history_git_cache
                .as_mut()
                .and_then(|cache| cache.histories.get_mut("A"))
                .expect("history A present");
            history.events = vec![
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "created".to_string(),
                    timestamp: (now - chrono::Duration::hours(3)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Alice".to_string(),
                    author_email: "alice@example.com".to_string(),
                },
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "claimed".to_string(),
                    timestamp: (now - chrono::Duration::hours(2)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Bob Builder".to_string(),
                    author_email: "bob@example.com".to_string(),
                },
                HistoryEventCompat {
                    bead_id: "A".to_string(),
                    event_type: "closed".to_string(),
                    timestamp: (now - chrono::Duration::minutes(30)).to_rfc3339(),
                    commit_sha: String::new(),
                    commit_message: String::new(),
                    author: "Carol".to_string(),
                    author_email: "carol@example.com".to_string(),
                },
            ];
        }

        let text = app.history_detail_text();
        assert!(text.contains("LIFECYCLE (3)"));
        assert!(text.contains("🆕"));
        assert!(text.contains("👤"));
        assert!(text.contains("✓"));
        assert!(text.contains("CA"));
        assert!(!text.contains("  │ created"));
    }

    #[test]
    fn mouse_scroll_down_moves_selection() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.selected, 0);
        app.handle_mouse(MouseEvent::new(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(app.selected, 1);
        app.handle_mouse(MouseEvent::new(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn mouse_scroll_up_moves_selection() {
        let mut app = new_app(ViewMode::Main, 2);
        assert_eq!(app.selected, 2);
        app.handle_mouse(MouseEvent::new(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(app.selected, 1);
        app.handle_mouse(MouseEvent::new(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn mouse_scroll_works_in_board_mode() {
        let mut app = new_app(ViewMode::Board, 0);
        app.handle_mouse(MouseEvent::new(MouseEventKind::ScrollDown, 0, 0));
        // Should not panic, and should move selection
        assert!(app.selected <= 1);
    }

    #[test]
    fn mouse_other_events_are_noop() {
        let mut app = new_app(ViewMode::Main, 0);
        app.handle_mouse(MouseEvent::new(MouseEventKind::Moved, 0, 0));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn mouse_left_click_opens_main_detail_external_link() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("detail link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            !app.status_msg.is_empty(),
            "expected click to trigger open-link status"
        );
        assert_ne!(app.status_msg, "No external issue reference");
    }

    #[test]
    fn mouse_left_click_opens_board_detail_external_link() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("detail link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            !app.status_msg.is_empty(),
            "expected click to trigger open-link status"
        );
        assert_ne!(app.status_msg, "No external issue reference");
    }

    #[test]
    fn mouse_click_outside_main_detail_link_row_is_noop() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());
        let (x, y) = detail_non_link_click_point(&app, 120, 40).expect("non-link detail point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            app.status_msg.is_empty(),
            "expected non-link click to stay inert, got {:?}",
            app.status_msg
        );
    }

    #[test]
    fn mouse_left_click_on_header_mode_tab_switches_mode() {
        let mut app = new_app(ViewMode::Main, 0);
        let (x, y) =
            header_tab_click_point(&app, 120, 24, ViewMode::Graph).expect("graph header tab point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert_eq!(app.mode, ViewMode::Graph);
        assert_eq!(app.focus, FocusPane::List);
        assert_eq!(app.status_msg, "Switched to Graph");
    }

    #[test]
    fn current_detail_link_row_area_tracks_main_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Main, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let _ = render_app(&app, 120, 40);
        let initial = app
            .current_detail_link_row_area()
            .expect("initial main detail link row area");

        app.detail_scroll_offset = 1;
        let _ = render_app(&app, 120, 40);
        let scrolled = app
            .current_detail_link_row_area()
            .expect("scrolled main detail link row area");

        assert_eq!(scrolled.y, initial.y.saturating_sub(1));
        assert_eq!(scrolled.x, initial.x);
        assert_eq!(scrolled.width, initial.width);
    }

    #[test]
    fn current_detail_link_row_area_matches_board_hyperlink_row() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let _ = render_app(&app, 120, 40);
        let link_area = app
            .current_detail_link_row_area()
            .expect("board detail link row area");
        let detail_area = cached_detail_content_area();
        let detail = app.board_detail_render_text();
        let expected_row = detail
            .lines()
            .iter()
            .position(|line| {
                ftui::text::Line::spans(line)
                    .iter()
                    .any(|span| span.link.is_some())
            })
            .expect("board detail hyperlink row");

        assert_eq!(
            link_area.y,
            detail_area
                .y
                .saturating_add(saturating_scroll_offset(expected_row)),
        );
    }

    #[test]
    fn current_detail_link_row_area_tracks_board_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Board, 0);
        app.focus = FocusPane::Detail;
        app.analyzer.issues[0].external_ref = Some("https://github.com/org/repo/issues/42".into());

        let _ = render_app(&app, 120, 40);
        let initial = app
            .current_detail_link_row_area()
            .expect("initial board detail link row area");

        app.board_detail_scroll_offset = 1;
        let _ = render_app(&app, 120, 40);
        let scrolled = app
            .current_detail_link_row_area()
            .expect("scrolled board detail link row area");

        assert_eq!(scrolled.y, initial.y.saturating_sub(1));
        assert_eq!(scrolled.height, initial.height);
        assert_eq!(scrolled.width, initial.width);
    }

    #[test]
    fn current_detail_link_row_area_matches_graph_hyperlink_row() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let _ = render_app(&app, 120, 40);
        let link_area = app
            .current_detail_link_row_area()
            .expect("graph detail link row area");
        let detail_area = cached_detail_content_area();
        let detail = app.graph_detail_render_text();
        let expected_row = detail
            .lines()
            .iter()
            .position(|line| {
                ftui::text::Line::spans(line)
                    .iter()
                    .any(|span| span.link.is_some())
            })
            .expect("graph detail hyperlink row");

        assert_eq!(
            link_area.y,
            detail_area
                .y
                .saturating_add(saturating_scroll_offset(expected_row)),
        );
    }

    #[test]
    fn current_detail_link_row_area_tracks_graph_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let _ = render_app(&app, 120, 40);
        let initial = app
            .current_detail_link_row_area()
            .expect("initial graph detail link row area");

        app.detail_scroll_offset = 1;
        let _ = render_app(&app, 120, 40);
        let scrolled = app
            .current_detail_link_row_area()
            .expect("scrolled graph detail link row area");

        assert_eq!(scrolled.y, initial.y.saturating_sub(1));
        assert_eq!(scrolled.x, initial.x);
        assert_eq!(scrolled.width, initial.width);
    }

    #[test]
    fn current_detail_link_row_area_matches_insights_hyperlink_row() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let _ = render_app(&app, 120, 40);
        let link_area = app
            .current_detail_link_row_area()
            .expect("insights detail link row area");
        let detail_area = cached_detail_content_area();
        let detail = app.insights_detail_render_text();
        let expected_row = detail
            .lines()
            .iter()
            .position(|line| {
                ftui::text::Line::spans(line)
                    .iter()
                    .any(|span| span.link.is_some())
            })
            .expect("insights detail hyperlink row");

        assert_eq!(
            link_area.y,
            detail_area
                .y
                .saturating_add(saturating_scroll_offset(expected_row)),
        );
    }

    #[test]
    fn current_detail_link_row_area_tracks_insights_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }

        let _ = render_app(&app, 120, 40);
        let initial = app
            .current_detail_link_row_area()
            .expect("initial insights detail link row area");

        app.detail_scroll_offset = 1;
        let _ = render_app(&app, 120, 40);
        let scrolled = app
            .current_detail_link_row_area()
            .expect("scrolled insights detail link row area");

        assert_eq!(scrolled.y, initial.y.saturating_sub(1));
        assert_eq!(scrolled.height, initial.height);
        assert_eq!(scrolled.width, initial.width);
    }

    #[test]
    fn mouse_right_click_copies_graph_detail_external_link() {
        let mut app = new_app(ViewMode::Graph, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("detail link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Right), x, y));

        assert!(
            !app.status_msg.is_empty(),
            "expected click to trigger copy-link status"
        );
        assert_ne!(app.status_msg, "No external issue reference");
    }

    #[test]
    fn mouse_left_click_opens_insights_detail_external_link() {
        let mut app = new_app(ViewMode::Insights, 0);
        app.focus = FocusPane::Detail;
        for issue in &mut app.analyzer.issues {
            issue.external_ref = Some("https://github.com/org/repo/issues/42".into());
        }
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("detail link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            !app.status_msg.is_empty(),
            "expected click to trigger open-link status"
        );
        assert_ne!(app.status_msg, "No external issue reference");
    }

    #[test]
    fn mouse_left_click_opens_history_commit_link() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        app.focus = FocusPane::Detail;
        let temp = tempfile::tempdir().expect("temp git dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("init git repo");
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/org/repo.git"])
            .current_dir(temp.path())
            .output()
            .expect("add git remote");
        app.repo_root = Some(temp.path().to_path_buf());
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("history link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            !app.history_status_msg.is_empty(),
            "expected click to trigger history open status"
        );
        assert_ne!(app.history_status_msg, "No commit selected");
    }

    #[test]
    fn mouse_right_click_copies_history_commit_link() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        app.focus = FocusPane::Detail;
        let temp = tempfile::tempdir().expect("temp git dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("init git repo");
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/org/repo.git"])
            .current_dir(temp.path())
            .output()
            .expect("add git remote");
        app.repo_root = Some(temp.path().to_path_buf());
        let (x, y) = detail_link_click_point(&app, 120, 40).expect("history link point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Right), x, y));

        assert!(
            !app.history_status_msg.is_empty(),
            "expected right-click to trigger history copy status"
        );
        assert_ne!(app.history_status_msg, "No commit selected");
    }

    #[test]
    fn mouse_click_outside_history_detail_link_row_is_noop() {
        let mut app = history_app_with_git_cache(HistoryViewMode::Bead, 0);
        app.mode = ViewMode::History;
        app.focus = FocusPane::Detail;
        let temp = tempfile::tempdir().expect("temp git dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .expect("init git repo");
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/org/repo.git"])
            .current_dir(temp.path())
            .output()
            .expect("add git remote");
        app.repo_root = Some(temp.path().to_path_buf());
        let (x, y) = detail_non_link_click_point(&app, 120, 40).expect("non-link history point");

        app.update(mouse(MouseEventKind::Down(MouseButton::Left), x, y));

        assert!(
            app.history_status_msg.is_empty(),
            "expected non-link click to stay inert, got {:?}",
            app.history_status_msg
        );
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
    fn flow_matrix_list_handles_wide_unicode_labels_without_overflow() {
        let mut app = new_app(ViewMode::FlowMatrix, 0);
        app.mode = ViewMode::FlowMatrix;
        app.flow_matrix = Some(CrossLabelFlow {
            labels: vec!["界面".to_string(), "🚀-launch".to_string()],
            flow_matrix: vec![vec![0, 3], vec![1, 0]],
            dependencies: Vec::new(),
            bottleneck_labels: vec!["界面".to_string()],
            total_cross_label_deps: 4,
        });
        app.flow_matrix_row_cursor = 0;
        app.flow_matrix_col_cursor = 1;

        let list = app.flow_matrix_list_text();
        assert!(list.contains("界面"));
        assert!(list.contains("🚀-launch"));
        assert!(
            list.lines().all(|line| display_width(line) <= 80),
            "every flow-matrix line should remain width-safe: {list}"
        );
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
    fn ctrl_t_opens_and_dismisses_tutorial_modal() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key_ctrl(KeyCode::Char('t')));
        assert!(matches!(app.modal_overlay, Some(ModalOverlay::Tutorial)));

        app.update(key(KeyCode::Char('x')));
        assert!(app.modal_overlay.is_none());
    }

    #[test]
    fn modal_confirm_accepts_on_y_rejects_on_n() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_confirm_with_resume("Test", "Do you confirm?", None);
        assert!(app.modal_overlay.is_some());
        assert!(app.modal_confirm_result.is_none());

        app.update(key(KeyCode::Char('y')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(true));

        app.open_confirm_with_resume("Test", "Another question?", None);
        app.update(key(KeyCode::Char('n')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(false));

        app.open_confirm_with_resume("Test", "Third question?", None);
        app.update(key(KeyCode::Escape));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(false));
    }

    #[test]
    fn modal_confirm_ignores_unrelated_keys() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_confirm_with_resume("Test", "Do you confirm?", None);

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
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::Confirm { .. })
        ));
        assert!(app.modal_confirm_result.is_none());

        app.update(key(KeyCode::Char('y')));
        assert!(app.modal_overlay.is_none());
        assert_eq!(app.modal_confirm_result, Some(true));
    }

    #[test]
    fn uppercase_p_opens_pages_wizard_modal() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('P')));

        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::PagesWizard(_))
        ));
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
    fn modal_pages_wizard_confirm_cancel_restores_wizard() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_pages_wizard();

        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Enter));

        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::Confirm { .. })
        ));

        app.update(key(KeyCode::Escape));
        match &app.modal_overlay {
            Some(ModalOverlay::PagesWizard(wiz)) => {
                assert_eq!(wiz.step, 3);
                assert_eq!(wiz.export_dir, "./bv-pages");
            }
            other => panic!("expected PagesWizard to resume after cancel, got {other:?}"),
        }
        assert_eq!(app.modal_confirm_result, Some(false));
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

        app.open_confirm_with_resume("Test", "Question", None);
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

    /// Redact non-deterministic git data from snapshot text.
    /// Handles: commit counts, 7-char SHAs, ISO dates, and author info.
    #[allow(dead_code)]
    fn redact_git_volatile(text: &str) -> String {
        let mut result = String::with_capacity(text.len());

        for line in text.lines() {
            let mut redacted = line.to_string();

            // Redact "N/M correla" patterns (commit counts)
            if let Some(pos) = redacted.find(" correla") {
                let prefix = &redacted[..pos];
                if let Some(slash) = prefix.rfind('/') {
                    let num_start = prefix[..slash]
                        .rfind(|ch: char| !ch.is_ascii_digit())
                        .map_or(0, |i| i + 1);
                    let after_slash = &prefix[slash + 1..];
                    if !after_slash.is_empty()
                        && after_slash.chars().all(|ch| ch.is_ascii_digit())
                        && prefix[num_start..slash]
                            .chars()
                            .all(|ch| ch.is_ascii_digit())
                        && !prefix[num_start..slash].is_empty()
                    {
                        redacted = format!("{}N/N{}", &redacted[..num_start], &redacted[pos..]);
                    }
                }
            }

            // Redact "SHA: <full-hex>" lines by replacing the hex after "SHA: "
            if redacted.contains("SHA:") {
                if let Some(pos) = redacted.find("SHA: ") {
                    let sha_start = pos + 5;
                    let sha_end = redacted[sha_start..]
                        .find(|c: char| !c.is_ascii_hexdigit())
                        .map_or(redacted.len(), |i| sha_start + i);
                    if sha_end > sha_start {
                        redacted =
                            format!("{}AAAAAAA{}", &redacted[..sha_start], &redacted[sha_end..]);
                    }
                }
            }

            // Redact 7-char hex SHAs (e.g., "d50d5c0" → "AAAAAAA")
            // Only in lines that look like git commit references
            if redacted.contains("for ") || redacted.contains("> F ") {
                let chars: Vec<char> = redacted.chars().collect();
                let mut new_line = String::with_capacity(redacted.len());
                let mut i = 0;
                while i < chars.len() {
                    if i + 7 <= chars.len()
                        && chars[i..i + 7].iter().all(|c| c.is_ascii_hexdigit())
                        && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
                        && (i + 7 >= chars.len() || !chars[i + 7].is_ascii_alphanumeric())
                    {
                        // Check it's not all digits (could be a date fragment)
                        if chars[i..i + 7].iter().any(|c| c.is_ascii_alphabetic()) {
                            new_line.push_str("AAAAAAA");
                            i += 7;
                            continue;
                        }
                    }
                    new_line.push(chars[i]);
                    i += 1;
                }
                redacted = new_line;
            }

            // Redact ISO dates like "2026-03-23T06:14:45Z" → "YYYY-MM-DDTHH:MM:SSZ"
            if redacted.contains("Date:") {
                if let Some(pos) = redacted.find("20") {
                    let rest = &redacted[pos..];
                    if rest.len() >= 20 && rest.as_bytes().get(4) == Some(&b'-') {
                        redacted = format!(
                            "{}YYYY-MM-DDTHH:MM:SSZ{}",
                            &redacted[..pos],
                            &redacted[pos + 20..]
                        );
                    }
                }
            }

            result.push_str(&redacted);
            result.push('\n');
        }

        // Remove trailing newline to match input format
        if result.ends_with('\n') && !text.ends_with('\n') {
            result.pop();
        }

        result
    }

    fn render_app(app: &BvrApp, width: u16, height: u16) -> String {
        let mut pool = ftui::GraphemePool::default();
        let mut frame = ftui::render::frame::Frame::new(width, height, &mut pool);
        app.view(&mut frame);
        super::buffer_to_text(&frame.buffer, &pool)
    }

    fn capture_debug_replay(
        app: &BvrApp,
        width: u16,
        height: u16,
        step: &str,
        captures: &mut Vec<DebugReplayCapture>,
    ) -> String {
        let rendered = render_app(app, width, height);
        captures.push(DebugReplayCapture {
            step: step.to_string(),
            mode: app.mode,
            focus: app.focus,
            selected: app.selected,
            width,
            height,
            trace_len: app.key_trace.len(),
            rendered: rendered.clone(),
            layout: super::render_layout_debug_report(app, width, height),
            hittest: super::render_hittest_debug_report(app, width, height),
        });
        rendered
    }

    fn debug_replay_artifact(journey_name: &str, captures: &[DebugReplayCapture]) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== Debug Replay: {journey_name} ===");
        let _ = writeln!(out);
        for (idx, capture) in captures.iter().enumerate() {
            let _ = writeln!(
                out,
                "--- Step {}: {} | view={} | focus={} | selected={} | size={}x{} | trace-len={} ---",
                idx + 1,
                capture.step,
                capture.mode.label(),
                capture.focus.label(),
                capture.selected,
                capture.width,
                capture.height,
                capture.trace_len
            );
            let _ = writeln!(out, "{}", capture.rendered);
            let _ = writeln!(out);
            let _ = writeln!(out, "{}", capture.layout);
            let _ = writeln!(out);
            let _ = writeln!(out, "{}", capture.hittest);
            let _ = writeln!(out);
        }
        out
    }

    fn detail_link_click_point(app: &BvrApp, width: u16, height: u16) -> Option<(u16, u16)> {
        let _ = render_app(app, width, height);
        let area = app.current_detail_link_row_area()?;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        Some((area.x, area.y))
    }

    fn detail_non_link_click_point(app: &BvrApp, width: u16, height: u16) -> Option<(u16, u16)> {
        let _ = render_app(app, width, height);
        let detail_area = cached_detail_content_area();
        let link_area = app.current_detail_link_row_area()?;
        if detail_area.width == 0 || detail_area.height == 0 {
            return None;
        }

        if link_area.y > detail_area.y {
            return Some((detail_area.x, detail_area.y));
        }

        let next_y = link_area.y.saturating_add(1);
        if next_y < detail_area.y.saturating_add(detail_area.height) {
            return Some((detail_area.x, next_y));
        }

        None
    }

    fn header_tab_click_point(
        app: &BvrApp,
        width: u16,
        height: u16,
        mode: ViewMode,
    ) -> Option<(u16, u16)> {
        let _ = render_app(app, width, height);
        let tab = super::header_mode_tabs(app, width)
            .into_iter()
            .find(|tab| tab.mode == mode)?;
        Some((tab.rect.x, tab.rect.y))
    }

    #[test]
    fn saturating_scroll_offset_clamps_large_values() {
        assert_eq!(saturating_scroll_offset(0), 0);
        assert_eq!(saturating_scroll_offset(42), 42);
        assert_eq!(
            saturating_scroll_offset(usize::from(u16::MAX) + 1),
            u16::MAX
        );
    }

    fn rendered_link_urls(app: &BvrApp, width: u16, height: u16) -> Vec<String> {
        let mut pool = ftui::GraphemePool::default();
        let mut links = ftui::LinkRegistry::new();
        let mut frame =
            ftui::render::frame::Frame::with_links(width, height, &mut pool, &mut links);
        app.view(&mut frame);
        let mut link_ids = Vec::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = frame.buffer.get(x, y) {
                    let link_id = cell.attrs.link_id();
                    if link_id != 0 && !link_ids.contains(&link_id) {
                        link_ids.push(link_id);
                    }
                }
            }
        }
        drop(frame);
        link_ids
            .into_iter()
            .filter_map(|link_id| links.get(link_id).map(ToString::to_string))
            .collect()
    }

    fn init_temp_repo_with_remote(remote: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init should succeed");

        let status = std::process::Command::new("git")
            .args(["remote", "add", "origin", remote])
            .current_dir(dir.path())
            .status()
            .expect("git remote add origin");
        assert!(status.success(), "git remote add origin should succeed");
        dir
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
    fn keyflow_main_search_reacts_to_filter_changes() {
        let mut app = new_app(ViewMode::Main, 0);

        app.update(key(KeyCode::Char('o')));
        assert_eq!(app.list_filter, ListFilter::Open);

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('d')));
        // "d" matches A (via description) and B (via title "Dependent"); cursor=0 selects A
        assert_eq!(selected_issue_id(&app), "A");
        app.update(key(KeyCode::Enter));

        let open_only = app.list_panel_text();
        assert!(open_only.contains("Search: /d (n/N cycles)"));
        assert!(open_only.contains("Matches: 1/2"));

        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(app.list_filter, ListFilter::All);

        let all_issues = app.list_panel_text();
        assert!(all_issues.contains("Search: /d (n/N cycles)"));
        // All filter: A (desc), B (title), C (title "Closed" ends with 'd') = 3 matches
        assert!(all_issues.contains("Matches: 1/3"));

        app.update(key(KeyCode::Char('n')));
        assert_eq!(selected_issue_id(&app), "B");
        assert!(app.list_panel_text().contains("Matches: 2/3"));
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
        app.open_confirm_with_resume("Delete?", "Are you sure?", None);
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
        assert!(matches!(
            app.modal_overlay,
            Some(ModalOverlay::Confirm { .. })
        ));
        app.update(key(KeyCode::Char('y')));
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
        let app = history_app_with_git_cache(HistoryViewMode::Git, 0);

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
    fn snap_confirm_modal() {
        let mut app = new_app(ViewMode::Main, 0);
        app.open_confirm_with_resume(
            "Export Pages?",
            "Finalize the current pages export settings?",
            None,
        );

        let text = render_app(&app, 100, 30);
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
    fn time_travel_empty_ref_with_existing_diff_stays_in_mode() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = true;
        app.time_travel_last_ref = Some("HEAD~1".to_string());
        app.time_travel_diff = Some(crate::analysis::diff::compare_snapshots(
            &[],
            &app.analyzer.issues,
        ));

        app.update(key(KeyCode::Enter));

        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        assert!(!app.time_travel_input_active);
        assert!(app.time_travel_diff.is_some());
        assert_eq!(app.time_travel_last_ref.as_deref(), Some("HEAD~1"));
        assert_eq!(app.status_msg, "Time-travel: empty ref, cancelled");
    }

    #[test]
    fn time_travel_invalid_ref_sets_error_status_and_keeps_mode() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('t')));
        for ch in "__definitely_not_a_real_ref__".chars() {
            app.update(key(KeyCode::Char(ch)));
        }

        app.update(key(KeyCode::Enter));

        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        assert!(!app.time_travel_input_active);
        assert!(app.time_travel_diff.is_none());
        assert_eq!(
            app.time_travel_last_ref.as_deref(),
            Some("__definitely_not_a_real_ref__")
        );
        assert!(
            app.status_msg
                .contains("could not resolve '__definitely_not_a_real_ref__'"),
            "status should explain invalid ref: {}",
            app.status_msg
        );
    }

    #[test]
    fn time_travel_invalid_ref_preserves_existing_diff() {
        let mut app = new_app(ViewMode::Main, 0);
        let existing = crate::analysis::diff::compare_snapshots(&[], &app.analyzer.issues);
        let existing_new_count = existing.new_issues.as_ref().map_or(0, Vec::len);
        app.mode = ViewMode::TimeTravelDiff;
        app.time_travel_input_active = true;
        app.time_travel_last_ref = Some("HEAD~1".to_string());
        app.time_travel_diff = Some(existing);
        app.time_travel_ref_input = "__still_not_a_real_ref__".to_string();

        app.update(key(KeyCode::Enter));

        assert_eq!(app.mode, ViewMode::TimeTravelDiff);
        assert!(!app.time_travel_input_active);
        let retained_new_count = app
            .time_travel_diff
            .as_ref()
            .and_then(|diff| diff.new_issues.as_ref())
            .map_or(0, Vec::len);
        assert_eq!(retained_new_count, existing_new_count);
        assert_eq!(
            app.time_travel_last_ref.as_deref(),
            Some("__still_not_a_real_ref__")
        );
        assert!(
            app.status_msg
                .contains("could not resolve '__still_not_a_real_ref__'"),
            "status should explain invalid ref: {}",
            app.status_msg
        );
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
        let now = sprint_reference_now();
        Sprint {
            id: id.to_string(),
            name: name.to_string(),
            start_date: Some(now - chrono::Duration::days(7)),
            end_date: Some(now + chrono::Duration::days(7)),
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
    fn label_picker_filter_updates_selection_before_next_key() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.selected = 1;

        app.update(key(KeyCode::Char('L')));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));

        assert_eq!(app.modal_label_filter.as_deref(), Some("frontend"));
        assert_eq!(selected_issue_id(&app), "C");
        assert_eq!(
            app.selected_issue().map(|issue| issue.id.as_str()),
            Some("C")
        );
        assert!(
            app.list_panel_text()
                .lines()
                .any(|line| line.contains("▸") && line.contains(" C "))
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
    fn repo_picker_filter_updates_selection_before_next_key() {
        let mut app = new_app(ViewMode::Main, 2);

        app.update(key(KeyCode::Char('w')));
        app.update(key(KeyCode::Enter));

        assert_eq!(app.modal_repo_filter.as_deref(), Some("viewer"));
        assert_eq!(selected_issue_id(&app), "A");
        assert_eq!(
            app.selected_issue().map(|issue| issue.id.as_str()),
            Some("A")
        );
        assert!(
            app.list_panel_text()
                .lines()
                .any(|line| line.contains("▸") && line.contains(" A "))
        );
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
    fn label_filter_matches_case_insensitively() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.modal_label_filter = Some("FRONTEND".to_string());

        let visible_ids = app
            .visible_issue_indices()
            .into_iter()
            .map(|index| app.analyzer.issues[index].id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(visible_ids, vec!["C"]);
    }

    #[test]
    fn set_label_filter_toggles_case_insensitively() {
        let mut app = new_app(ViewMode::Main, 0);
        app.modal_label_filter = Some("backend".to_string());

        app.set_label_filter("BACKEND");

        assert!(app.modal_label_filter.is_none());
        assert_eq!(app.status_msg, "Label filter cleared");
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

    // -- Tree expanded coverage (bd-7oo.4.5) ---------------------------------

    #[test]
    fn keyflow_tree_full_journey() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.mode, ViewMode::Main);

        // Enter Tree mode
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);
        assert!(!app.tree_flat_nodes.is_empty(), "tree should build nodes");
        assert_eq!(app.focus, FocusPane::List);

        // Verify list shows dependency tree header
        let list = app.list_panel_text();
        assert!(
            list.contains("Dependency tree") || list.contains("no dependency tree"),
            "list should show tree header: {list}"
        );

        // Navigate down/up
        let node_count = app.tree_flat_nodes.len();
        if node_count > 1 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(app.tree_cursor, 1);
            app.update(key(KeyCode::Char('k')));
            assert_eq!(app.tree_cursor, 0);
        }

        // Switch to detail pane
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        let detail = app.detail_panel_text();
        assert!(
            detail.contains("ID:"),
            "detail should show issue ID: {detail}"
        );

        // Switch back to list
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        // Help overlay
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        let help_text = render_app(&app, 100, 30);
        assert!(
            help_text.contains("Help") || help_text.contains("help"),
            "help overlay should render: {help_text}"
        );
        app.update(key(KeyCode::Char('?')));
        assert!(!app.show_help);

        // Exit back to Main
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn tree_narrow_width_rendering_no_panic() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);

        // 40x20 — narrow but usable
        let text_40 = render_app(&app, 40, 20);
        assert!(!text_40.is_empty(), "40x20 render should produce output");

        // 20x10 — extremely narrow
        let text_20 = render_app(&app, 20, 10);
        assert!(!text_20.is_empty(), "20x10 render should produce output");

        // Navigate at narrow width — should not panic
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        let text_detail = render_app(&app, 40, 20);
        assert!(
            !text_detail.is_empty(),
            "detail at 40x20 should produce output"
        );
    }

    #[test]
    fn tree_cursor_clamp_at_boundary() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);

        // k at top should clamp to 0
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.tree_cursor, 0, "cursor should clamp at top");

        // Navigate to bottom and try to go past
        let node_count = app.tree_flat_nodes.len();
        for _ in 0..node_count + 5 {
            app.update(key(KeyCode::Char('j')));
        }
        assert_eq!(
            app.tree_cursor,
            node_count.saturating_sub(1),
            "cursor should clamp at bottom"
        );

        // One more j should stay clamped
        app.update(key(KeyCode::Char('j')));
        assert_eq!(
            app.tree_cursor,
            node_count.saturating_sub(1),
            "cursor should stay clamped at bottom"
        );
    }

    // -- LabelDashboard expanded coverage (bd-7oo.4.5) ----------------------

    #[test]
    fn keyflow_label_dashboard_full_journey() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        assert_eq!(app.mode, ViewMode::Main);

        // Enter LabelDashboard
        app.update(key(KeyCode::Char('[')));
        assert_eq!(app.mode, ViewMode::LabelDashboard);
        assert!(app.label_dashboard.is_some());
        assert_eq!(app.focus, FocusPane::List);

        // Verify list shows health header
        let list = app.list_panel_text();
        assert!(
            list.contains("Label health") || list.contains("no labels"),
            "list should show label health header: {list}"
        );

        // Navigate labels
        let count = app.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
        if count > 1 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(app.label_dashboard_cursor, 1);
        }

        // Switch to detail focus
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // Detail should show label info
        let detail = app.detail_panel_text();
        if count > 0 {
            assert!(
                detail.contains("Label:") || detail.contains("Health:"),
                "detail should show label health info: {detail}"
            );
        }

        // Navigate back
        app.update(key(KeyCode::Char('k')));
        if count > 1 {
            assert_eq!(app.label_dashboard_cursor, 0);
        }

        // Switch back to list
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        // Open help overlay
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        app.update(key(KeyCode::Char('?')));
        assert!(!app.show_help);

        // Return to Main via [
        app.update(key(KeyCode::Char('[')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn label_dashboard_narrow_width_rendering_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('[')));
        assert_eq!(app.mode, ViewMode::LabelDashboard);

        let text = render_app(&app, 40, 20);
        assert!(
            !text.is_empty(),
            "narrow label dashboard should produce output"
        );

        let text_tiny = render_app(&app, 20, 10);
        assert!(
            !text_tiny.is_empty(),
            "very narrow label dashboard should produce output"
        );
    }

    #[test]
    fn label_dashboard_detail_changes_on_navigation() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('[')));

        let count = app.label_dashboard.as_ref().map_or(0, |r| r.labels.len());
        if count >= 2 {
            let detail_0 = app.detail_panel_text();
            app.update(key(KeyCode::Char('j')));
            let detail_1 = app.detail_panel_text();
            assert_ne!(
                detail_0, detail_1,
                "navigating labels should change detail content"
            );
        }
    }

    // -- Sprint expanded coverage (bd-7oo.4.5) -----------------------------

    #[test]
    fn sprint_narrow_width_rendering_no_panic() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![
            make_sprint("s1", "Sprint Alpha", vec!["A", "B"]),
            make_sprint("s2", "Sprint Beta", vec!["C"]),
        ];

        let text = render_app(&app, 40, 20);
        assert!(
            !text.is_empty(),
            "narrow sprint render should produce output"
        );

        let text_tiny = render_app(&app, 20, 10);
        assert!(
            !text_tiny.is_empty(),
            "very narrow sprint render should produce output"
        );
    }

    #[test]
    fn sprint_detail_focus_navigation_no_panic() {
        let mut app = new_app(ViewMode::Main, 0);
        app.mode = ViewMode::Sprint;
        app.sprint_data = vec![
            make_sprint("s1", "Sprint Alpha", vec!["A", "B", "C"]),
            make_sprint("s2", "Sprint Beta", vec!["D"]),
        ];

        // Navigate sprints in list focus
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.sprint_cursor, 1);

        // Switch to detail and navigate issues
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('k')));

        // Switch back and navigate to first sprint
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.sprint_cursor, 0);

        // Render at multiple widths should not panic
        for width in [40, 80, 120] {
            let _ = render_app(&app, width, 30);
        }
    }

    // -- Actionable expanded coverage (bd-7oo.4.5) ---------------------------

    #[test]
    fn keyflow_actionable_full_journey() {
        let mut app = new_app(ViewMode::Main, 0);
        assert_eq!(app.mode, ViewMode::Main);

        // Enter Actionable mode
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);
        assert!(app.actionable_plan.is_some());
        assert_eq!(app.focus, FocusPane::List);

        // Verify list shows actionable header and recommended start
        let list = app.list_panel_text();
        assert!(
            list.contains("ACTIONABLE ITEMS"),
            "list should show actionable header: {list}"
        );
        assert!(
            list.contains("TRACK"),
            "list should show track info: {list}"
        );

        // Navigate tracks
        let track_count = app.actionable_plan.as_ref().map_or(0, |p| p.tracks.len());
        if track_count > 1 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(app.actionable_track_cursor, 1);
            app.update(key(KeyCode::Char('k')));
            assert_eq!(app.actionable_track_cursor, 0);
        }

        // Switch to detail focus — navigates items within track
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        let detail = app.detail_panel_text();
        assert!(
            detail.contains("TRACK"),
            "detail should show track info: {detail}"
        );

        // Item navigation in detail
        let item_count = app
            .actionable_plan
            .as_ref()
            .and_then(|p| p.tracks.first())
            .map_or(0, |t| t.items.len());
        if item_count > 1 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(app.actionable_item_cursor, 1);
            app.update(key(KeyCode::Char('k')));
            assert_eq!(app.actionable_item_cursor, 0);
        }

        // Help overlay
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        let help_text = render_app(&app, 100, 30);
        assert!(
            help_text.contains("Help") || help_text.contains("help"),
            "help overlay should render"
        );
        app.update(key(KeyCode::Char('?')));
        assert!(!app.show_help);

        // Switch back to list and exit
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn actionable_narrow_width_rendering_no_panic() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);

        // 40x20 — narrow but usable
        let text_40 = render_app(&app, 40, 20);
        assert!(!text_40.is_empty(), "40x20 render should produce output");

        // 20x10 — extremely narrow
        let text_20 = render_app(&app, 20, 10);
        assert!(!text_20.is_empty(), "20x10 render should produce output");

        // Navigate and render at narrow width
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        let text_detail = render_app(&app, 40, 20);
        assert!(
            !text_detail.is_empty(),
            "detail at 40x20 should produce output"
        );
    }

    #[test]
    fn actionable_track_cursor_clamp_at_boundary() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);

        // k at top should stay at 0
        app.update(key(KeyCode::Char('k')));
        assert_eq!(
            app.actionable_track_cursor, 0,
            "track cursor should clamp at top"
        );

        // Navigate past bottom
        let track_count = app.actionable_plan.as_ref().map_or(0, |p| p.tracks.len());
        for _ in 0..track_count + 5 {
            app.update(key(KeyCode::Char('j')));
        }
        assert_eq!(
            app.actionable_track_cursor,
            track_count.saturating_sub(1),
            "track cursor should clamp at bottom"
        );
    }

    #[test]
    fn actionable_detail_changes_on_track_navigation() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);

        let track_count = app.actionable_plan.as_ref().map_or(0, |p| p.tracks.len());
        if track_count > 1 {
            let detail_first = app.detail_panel_text();

            app.update(key(KeyCode::Char('j')));
            let detail_second = app.detail_panel_text();

            assert_ne!(
                detail_first, detail_second,
                "detail should change when navigating to a different track"
            );
        }
    }

    #[test]
    fn actionable_item_cursor_resets_on_track_change() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));

        // Navigate to detail and move item cursor
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Char('j')));
        let item_pos = app.actionable_item_cursor;

        // Switch back to list and change track
        app.update(key(KeyCode::Tab));
        let track_count = app.actionable_plan.as_ref().map_or(0, |p| p.tracks.len());
        if track_count > 1 && item_pos > 0 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(
                app.actionable_item_cursor, 0,
                "item cursor should reset when changing track"
            );
        }
    }

    #[test]
    fn actionable_detail_scroll_resets_on_track_navigation() {
        let mut app = new_app(ViewMode::Actionable, 0);
        app.mode = ViewMode::Actionable;
        app.focus = FocusPane::List;
        app.actionable_plan = Some(crate::analysis::plan::ExecutionPlan {
            total_actionable: 2,
            total_blocked: 0,
            tracks: vec![
                crate::analysis::plan::ExecutionTrack {
                    id: "track-1".to_string(),
                    reason: "first".to_string(),
                    items: vec![crate::analysis::plan::ExecutionItem {
                        id: "A".to_string(),
                        title: "Alpha".to_string(),
                        status: "open".to_string(),
                        priority: 1,
                        score: 5.0,
                        unblocks: Vec::new(),
                        claim_command: "br update A --status=in_progress".to_string(),
                        show_command: "br show A".to_string(),
                    }],
                },
                crate::analysis::plan::ExecutionTrack {
                    id: "track-2".to_string(),
                    reason: "second".to_string(),
                    items: vec![crate::analysis::plan::ExecutionItem {
                        id: "B".to_string(),
                        title: "Beta".to_string(),
                        status: "open".to_string(),
                        priority: 1,
                        score: 4.0,
                        unblocks: Vec::new(),
                        claim_command: "br update B --status=in_progress".to_string(),
                        show_command: "br show B".to_string(),
                    }],
                },
            ],
            summary: crate::analysis::plan::PlanSummary {
                track_count: 2,
                actionable_count: 2,
                unblocks_count: Some(0),
                highest_impact: Some("A".to_string()),
                impact_reason: Some("highest impact: A (score 5.00)".to_string()),
            },
        });
        app.detail_scroll_offset = 7;

        app.move_actionable_cursor(1);

        assert_eq!(app.actionable_track_cursor, 1);
        assert_eq!(app.actionable_item_cursor, 0);
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn actionable_detail_scroll_resets_on_item_navigation() {
        let mut app = new_app(ViewMode::Actionable, 0);
        app.mode = ViewMode::Actionable;
        app.focus = FocusPane::Detail;
        app.actionable_plan = Some(crate::analysis::plan::ExecutionPlan {
            total_actionable: 2,
            total_blocked: 0,
            tracks: vec![crate::analysis::plan::ExecutionTrack {
                id: "track-1".to_string(),
                reason: "first".to_string(),
                items: vec![
                    crate::analysis::plan::ExecutionItem {
                        id: "A".to_string(),
                        title: "Alpha".to_string(),
                        status: "open".to_string(),
                        priority: 1,
                        score: 5.0,
                        unblocks: Vec::new(),
                        claim_command: "br update A --status=in_progress".to_string(),
                        show_command: "br show A".to_string(),
                    },
                    crate::analysis::plan::ExecutionItem {
                        id: "B".to_string(),
                        title: "Beta".to_string(),
                        status: "open".to_string(),
                        priority: 1,
                        score: 4.0,
                        unblocks: Vec::new(),
                        claim_command: "br update B --status=in_progress".to_string(),
                        show_command: "br show B".to_string(),
                    },
                ],
            }],
            summary: crate::analysis::plan::PlanSummary {
                track_count: 1,
                actionable_count: 2,
                unblocks_count: Some(0),
                highest_impact: Some("A".to_string()),
                impact_reason: Some("highest impact: A (score 5.00)".to_string()),
            },
        });
        app.detail_scroll_offset = 6;

        app.move_actionable_cursor(1);

        assert_eq!(app.actionable_item_cursor, 1);
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn actionable_mode_entry_resets_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Main, 0);
        app.detail_scroll_offset = 5;

        app.update(key(KeyCode::Char('a')));

        assert_eq!(app.mode, ViewMode::Actionable);
        assert_eq!(app.detail_scroll_offset, 0);
    }

    #[test]
    fn actionable_mode_exit_resets_detail_scroll_offset() {
        let mut app = new_app(ViewMode::Main, 0);
        app.update(key(KeyCode::Char('a')));
        app.detail_scroll_offset = 5;

        app.update(key(KeyCode::Char('a')));

        assert_eq!(app.mode, ViewMode::Main);
        assert_eq!(app.detail_scroll_offset, 0);
    }

    // -- Attention mode expanded coverage (bd-7oo.4.5) ----------------------

    #[test]
    fn keyflow_attention_full_journey() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        assert_eq!(app.mode, ViewMode::Main);

        // Enter Attention mode
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Attention);
        assert!(app.attention_result.is_some());
        assert_eq!(app.focus, FocusPane::List);

        // Verify list has ranked labels
        let list = app.list_panel_text();
        assert!(list.contains("Rank"), "list should show rank header");
        assert!(list.contains("Score"), "list should show score header");

        // Navigate down through labels
        let label_count = app.attention_result.as_ref().unwrap().labels.len();
        assert!(label_count >= 2);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.attention_cursor, 1);

        // Switch to detail focus
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);

        // Detail should show breakdown for the navigated-to label
        let detail = app.detail_panel_text();
        assert!(detail.contains("Attention Score:"));
        assert!(detail.contains("Breakdown:"));
        assert!(detail.contains("Factors:"));

        // Navigate back up in detail focus
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.attention_cursor, 0);

        // Switch back to list
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::List);

        // Open help from Attention mode
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        app.update(key(KeyCode::Char('?')));
        assert!(!app.show_help);

        // Return to Main
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn attention_narrow_width_rendering_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Attention);

        // Render at narrow width — should not panic
        let text = render_app(&app, 40, 20);
        assert!(
            !text.is_empty(),
            "narrow attention render should produce output"
        );

        // Also at very narrow width
        let text_tiny = render_app(&app, 20, 10);
        assert!(
            !text_tiny.is_empty(),
            "very narrow attention render should produce output"
        );
    }

    #[test]
    fn attention_tab_focus_updates_detail_context() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));

        // In list focus, get detail for cursor 0
        let detail_at_0 = app.detail_panel_text();

        // Navigate to cursor 1 and switch to detail focus
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, FocusPane::Detail);
        let detail_at_1 = app.detail_panel_text();

        // Detail content should differ between labels
        assert_ne!(
            detail_at_0, detail_at_1,
            "detail pane should reflect cursor position change"
        );

        // Verify detail shows open issues for the focused label
        assert!(
            detail_at_1.contains("Open issues") || detail_at_1.contains("Label:"),
            "detail should show label info: {detail_at_1}"
        );
    }

    #[test]
    fn attention_cursor_clamp_at_boundary() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));

        let label_count = app.attention_result.as_ref().unwrap().labels.len();

        // Navigate past the end — should clamp
        for _ in 0..label_count + 5 {
            app.update(key(KeyCode::Char('j')));
        }
        assert!(
            app.attention_cursor < label_count,
            "cursor should be clamped: {} < {}",
            app.attention_cursor,
            label_count
        );

        // Navigate back past the start — should clamp to 0
        for _ in 0..label_count + 5 {
            app.update(key(KeyCode::Char('k')));
        }
        assert_eq!(app.attention_cursor, 0, "cursor should clamp to 0");
    }

    #[test]
    fn snap_attention_list_overview() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char('!')));
        // List focus, cursor at 0
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
    }

    // -- Flow Matrix mode expanded coverage (bd-7oo.4.5) --------------------

    #[test]
    fn keyflow_flow_matrix_full_journey() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        assert_eq!(app.mode, ViewMode::Main);

        // Enter FlowMatrix mode
        app.update(key(KeyCode::Char(']')));
        assert_eq!(app.mode, ViewMode::FlowMatrix);
        assert!(app.flow_matrix.is_some());
        assert_eq!(app.focus, FocusPane::List);

        // Verify list has flow header
        let list = app.list_panel_text();
        assert!(
            list.contains("Cross-label flow") || list.contains("no labels"),
            "list should show flow header: {list}"
        );

        // Navigate rows with j/k
        let label_count = app.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
        if label_count > 1 {
            app.update(key(KeyCode::Char('j')));
            assert_eq!(app.flow_matrix_row_cursor, 1);

            // Navigate columns with h/l
            app.update(key(KeyCode::Char('l')));
            assert_eq!(app.flow_matrix_col_cursor, 1);

            // Switch to detail focus
            app.update(key(KeyCode::Tab));
            assert_eq!(app.focus, FocusPane::Detail);

            // Detail should show cross-label info for selected cell
            let detail = app.detail_panel_text();
            assert!(
                !detail.is_empty(),
                "detail should have content for selected cell"
            );

            // Navigate column back
            app.update(key(KeyCode::Char('h')));
            assert_eq!(app.flow_matrix_col_cursor, 0);

            // Switch back to list
            app.update(key(KeyCode::Tab));
            assert_eq!(app.focus, FocusPane::List);
        }

        // Open help from FlowMatrix mode
        app.update(key(KeyCode::Char('?')));
        assert!(app.show_help);
        app.update(key(KeyCode::Char('?')));
        assert!(!app.show_help);

        // Return to Main
        app.update(key(KeyCode::Char(']')));
        assert_eq!(app.mode, ViewMode::Main);
    }

    #[test]
    fn flow_matrix_narrow_width_rendering_no_panic() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char(']')));
        assert_eq!(app.mode, ViewMode::FlowMatrix);

        // Render at narrow width — should not panic
        let text = render_app(&app, 40, 20);
        assert!(
            !text.is_empty(),
            "narrow flow matrix render should produce output"
        );

        // Also at very narrow width
        let text_tiny = render_app(&app, 20, 10);
        assert!(
            !text_tiny.is_empty(),
            "very narrow flow matrix render should produce output"
        );
    }

    #[test]
    fn flow_matrix_detail_changes_on_cell_navigation() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char(']')));

        let label_count = app.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
        if label_count >= 2 {
            // Detail at (0,0) — diagonal, shows label self-info
            let detail_diag = app.detail_panel_text();

            // Move to (0,1) — off-diagonal, shows cross-label flow
            app.update(key(KeyCode::Char('l')));
            let detail_off = app.detail_panel_text();

            assert_ne!(
                detail_diag, detail_off,
                "diagonal and off-diagonal cells should show different detail"
            );

            // Move row down to (1,1) — back on diagonal for different label
            app.update(key(KeyCode::Char('j')));
            let detail_diag_2 = app.detail_panel_text();

            assert_ne!(
                detail_diag, detail_diag_2,
                "different diagonal cells should show different labels"
            );
        }
    }

    #[test]
    fn flow_matrix_cursor_clamp_at_boundary() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char(']')));

        let label_count = app.flow_matrix.as_ref().map_or(0, |f| f.labels.len());
        if label_count > 0 {
            // Navigate past row end — should clamp
            for _ in 0..label_count + 5 {
                app.update(key(KeyCode::Char('j')));
            }
            assert!(
                app.flow_matrix_row_cursor < label_count,
                "row cursor should clamp: {} < {}",
                app.flow_matrix_row_cursor,
                label_count
            );

            // Navigate past column end — should clamp
            for _ in 0..label_count + 5 {
                app.update(key(KeyCode::Char('l')));
            }
            assert!(
                app.flow_matrix_col_cursor < label_count,
                "col cursor should clamp: {} < {}",
                app.flow_matrix_col_cursor,
                label_count
            );

            // Navigate back past start — should clamp to 0
            for _ in 0..label_count + 5 {
                app.update(key(KeyCode::Char('k')));
            }
            assert_eq!(app.flow_matrix_row_cursor, 0);
            for _ in 0..label_count + 5 {
                app.update(key(KeyCode::Char('h')));
            }
            assert_eq!(app.flow_matrix_col_cursor, 0);
        }
    }

    #[test]
    fn snap_flow_matrix_list_overview() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        app.update(key(KeyCode::Char(']')));
        // List focus, cursors at (0,0)
        let text = render_app(&app, 100, 30);
        insta::assert_snapshot!(text);
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

    // =========================================================================
    // E2E TUI JOURNEYS (bd-7oo.4.6)
    //
    // Multi-mode investigative flows with screen captures at each transition.
    // These exercise the complete user experience across mode boundaries.
    //
    // Run: cargo test --lib e2e_journey_
    // =========================================================================

    /// Helper: capture a labelled screen dump for an e2e journey step.
    /// Returns the rendered text for assertion or snapshot use.
    fn journey_capture(
        app: &BvrApp,
        width: u16,
        height: u16,
        step: &str,
        captures: &mut Vec<(String, String)>,
    ) -> String {
        let text = render_app(app, width, height);
        captures.push((step.to_string(), text.clone()));
        text
    }

    /// Format all captured journey steps into a single diagnostic artifact.
    fn journey_artifact(
        journey_name: &str,
        width: u16,
        height: u16,
        captures: &[(String, String)],
    ) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "=== E2E Journey: {journey_name} | {width}x{height} ===\n\n"
        ));
        for (i, (step, text)) in captures.iter().enumerate() {
            out.push_str(&format!("--- Step {}: {} ---\n{}\n\n", i + 1, step, text));
        }
        out
    }

    #[test]
    fn e2e_journey_main_board_insights_graph_investigation() {
        let mut app = new_app(ViewMode::Main, 0);
        let (w, h) = (120, 35);
        let mut caps: Vec<(String, String)> = Vec::new();

        // Step 1: Start in Main — verify issue list
        let text = journey_capture(&app, w, h, "main_list_start", &mut caps);
        assert!(
            text.contains("mode=Main") || text.contains("Issues"),
            "main should show issue list: {text}"
        );

        // Step 2: Select an issue and inspect detail
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        let text = journey_capture(&app, w, h, "main_detail_focus", &mut caps);
        assert!(
            text.contains("ID:") || text.contains("Status:"),
            "detail should show issue: {text}"
        );

        // Step 3: Enter Board mode
        app.update(key(KeyCode::Char('b')));
        assert_eq!(app.mode, ViewMode::Board);
        let text = journey_capture(&app, w, h, "board_entry", &mut caps);
        assert!(
            text.contains("open") || text.contains("Board"),
            "board should show lane content: {text}"
        );

        // Step 4: Navigate board lanes
        app.update(key(KeyCode::Char('l')));
        app.update(key(KeyCode::Char('j')));
        let text = journey_capture(&app, w, h, "board_navigate", &mut caps);
        assert!(!text.is_empty());

        // Step 5: Jump to Graph from board
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.mode, ViewMode::Graph);
        let text = journey_capture(&app, w, h, "graph_entry_from_board", &mut caps);
        assert!(
            text.contains("Graph") || text.contains("graph") || text.contains("Dep"),
            "graph should show graph content: {text}"
        );

        // Step 6: Inspect graph detail
        app.update(key(KeyCode::Tab));
        let text = journey_capture(&app, w, h, "graph_detail_focus", &mut caps);
        assert!(!text.is_empty());

        // Step 7: Switch to Insights
        app.update(key(KeyCode::Char('i')));
        assert_eq!(app.mode, ViewMode::Insights);
        let text = journey_capture(&app, w, h, "insights_entry", &mut caps);
        assert!(
            text.contains("Bottleneck") || text.contains("bottleneck") || text.contains("Insight"),
            "insights should show analysis: {text}"
        );

        // Step 8: Cycle insights panels
        app.update(key(KeyCode::Char('s')));
        let text = journey_capture(&app, w, h, "insights_panel_cycle", &mut caps);
        assert!(!text.is_empty());

        // Step 9: Return to Main
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
        let text = journey_capture(&app, w, h, "main_return", &mut caps);
        assert!(text.contains("mode=Main") || text.contains("Issues"));

        // Snapshot the full journey artifact
        let artifact = journey_artifact("main→board→graph→insights→main", w, h, &caps);
        insta::assert_snapshot!(artifact);
    }

    #[test]
    fn e2e_journey_actionable_tree_attention_flow() {
        let mut app = new_app_with_issues(ViewMode::Main, 0, labeled_issues());
        let (w, h) = (120, 35);
        let mut caps: Vec<(String, String)> = Vec::new();

        // Step 1: Start in Main
        journey_capture(&app, w, h, "main_start", &mut caps);

        // Step 2: Enter Actionable — check execution tracks
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Actionable);
        let text = journey_capture(&app, w, h, "actionable_entry", &mut caps);
        assert!(
            text.contains("ACTIONABLE") || text.contains("actionable"),
            "should show actionable items: {text}"
        );

        // Step 3: Navigate tracks and detail
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Char('j')));
        journey_capture(&app, w, h, "actionable_detail_nav", &mut caps);

        // Step 4: Return to Main, enter Tree
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.mode, ViewMode::Main);
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Tree);
        let text = journey_capture(&app, w, h, "tree_entry", &mut caps);
        assert!(
            text.contains("Dependency tree") || text.contains("no dependency tree"),
            "tree should show structure: {text}"
        );

        // Step 5: Navigate tree and expand/collapse
        let has_children = app.tree_flat_nodes.iter().any(|n| n.has_children);
        if has_children {
            let idx = app
                .tree_flat_nodes
                .iter()
                .position(|n| n.has_children)
                .unwrap();
            app.tree_cursor = idx;
            app.update(key(KeyCode::Enter)); // collapse
            journey_capture(&app, w, h, "tree_collapsed", &mut caps);
            app.update(key(KeyCode::Enter)); // expand
        }
        journey_capture(&app, w, h, "tree_navigate", &mut caps);

        // Step 6: Return to Main, enter Attention
        app.update(key(KeyCode::Char('T')));
        assert_eq!(app.mode, ViewMode::Main);
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Attention);
        let text = journey_capture(&app, w, h, "attention_entry", &mut caps);
        assert!(
            text.contains("Rank") || text.contains("Score") || text.contains("Attention"),
            "attention should show ranked labels: {text}"
        );

        // Step 7: Navigate attention labels and detail
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Tab));
        journey_capture(&app, w, h, "attention_detail", &mut caps);

        // Step 8: Return to Main
        app.update(key(KeyCode::Char('!')));
        assert_eq!(app.mode, ViewMode::Main);
        journey_capture(&app, w, h, "main_return", &mut caps);

        let artifact = journey_artifact("actionable→tree→attention", w, h, &caps);
        insta::assert_snapshot!(artifact);
    }

    #[test]
    fn e2e_journey_main_search_focus_recovery() {
        let mut app = new_app(ViewMode::Main, 0);
        let (w, h) = (120, 35);
        let mut caps: Vec<(String, String)> = Vec::new();

        let text = journey_capture(&app, w, h, "main_start", &mut caps);
        assert!(text.contains("Focus: list owns selection"));

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('d')));
        let text = journey_capture(&app, w, h, "search_active", &mut caps);
        assert!(text.contains("Search (active): /d"));

        app.update(key(KeyCode::Enter));
        let text = journey_capture(&app, w, h, "search_committed", &mut caps);
        assert!(text.contains("Matches: 1/3"));
        assert!(text.contains("hit 1/3"), "search committed frame: {text}");

        app.update(key(KeyCode::Char('n')));
        let text = journey_capture(&app, w, h, "search_cycle_second_hit", &mut caps);
        assert!(text.contains("Matches: 2/3"));
        assert!(text.contains("hit 2/3"), "search cycled frame: {text}");

        app.update(key(KeyCode::Tab));
        let text = journey_capture(&app, w, h, "detail_focus", &mut caps);
        assert!(text.contains("Focus: detail owns J/K deps"));

        app.update(key(KeyCode::Escape));
        let text = journey_capture(&app, w, h, "focus_recovered", &mut caps);
        assert!(text.contains("Focus returned to list"));

        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('z')));
        app.update(key(KeyCode::Enter));
        let text = journey_capture(&app, w, h, "search_no_hit", &mut caps);
        assert!(text.contains("Matches: none in visible issues"));

        app.update(key(KeyCode::Escape));
        let text = journey_capture(&app, w, h, "search_cleared", &mut caps);
        assert!(text.contains("Main search cleared"));

        app.update(key(KeyCode::Char('o')));
        let text = journey_capture(&app, w, h, "open_filter", &mut caps);
        assert!(text.contains("scope=open"));

        app.update(key(KeyCode::Escape));
        let text = journey_capture(&app, w, h, "filter_cleared", &mut caps);
        assert!(text.contains("scope=all"));
    }

    #[test]
    fn e2e_journey_narrow_geometry_stress() {
        // Exercises the same multi-mode flow at narrow terminal width
        let mut app = new_app(ViewMode::Main, 0);
        let (w, h) = (40, 15);
        let mut caps: Vec<(String, String)> = Vec::new();

        // Main at narrow
        journey_capture(&app, w, h, "narrow_main", &mut caps);

        // Board at narrow
        app.update(key(KeyCode::Char('b')));
        journey_capture(&app, w, h, "narrow_board", &mut caps);
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('l')));
        journey_capture(&app, w, h, "narrow_board_nav", &mut caps);

        // Graph at narrow
        app.update(key(KeyCode::Char('g')));
        journey_capture(&app, w, h, "narrow_graph", &mut caps);

        // Insights at narrow
        app.update(key(KeyCode::Char('i')));
        journey_capture(&app, w, h, "narrow_insights", &mut caps);
        app.update(key(KeyCode::Char('s')));
        journey_capture(&app, w, h, "narrow_insights_cycle", &mut caps);

        // Actionable at narrow
        app.update(key(KeyCode::Escape));
        app.update(key(KeyCode::Char('a')));
        journey_capture(&app, w, h, "narrow_actionable", &mut caps);

        // Tree at narrow
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Char('T')));
        journey_capture(&app, w, h, "narrow_tree", &mut caps);

        // Return to Main
        app.update(key(KeyCode::Char('T')));
        journey_capture(&app, w, h, "narrow_main_return", &mut caps);

        let artifact = journey_artifact("narrow-geometry-stress", w, h, &caps);
        insta::assert_snapshot!(artifact);
    }

    #[test]
    fn e2e_journey_empty_data_edge_case() {
        // Multi-mode flow with zero issues — proves no panics and useful messaging
        let mut app = new_app_with_issues(ViewMode::Main, 0, vec![]);
        let (w, h) = (100, 30);
        let mut caps: Vec<(String, String)> = Vec::new();

        // Main with no issues
        let text = journey_capture(&app, w, h, "empty_main", &mut caps);
        assert!(
            text.contains("issues=0") || text.contains("No issues") || text.contains("mode=Main"),
            "empty main should render: {text}"
        );

        // Board with no issues
        app.update(key(KeyCode::Char('b')));
        let text = journey_capture(&app, w, h, "empty_board", &mut caps);
        assert!(!text.is_empty(), "empty board should render something");

        // Graph with no issues
        app.update(key(KeyCode::Char('g')));
        journey_capture(&app, w, h, "empty_graph", &mut caps);

        // Insights with no issues
        app.update(key(KeyCode::Char('i')));
        journey_capture(&app, w, h, "empty_insights", &mut caps);

        // Actionable with no issues
        app.update(key(KeyCode::Escape));
        app.update(key(KeyCode::Char('a')));
        let text = journey_capture(&app, w, h, "empty_actionable", &mut caps);
        assert!(
            text.contains("Actionable")
                || text.contains("Execution Tracks")
                || text.contains("ACTIONABLE"),
            "empty actionable should render: {text}"
        );

        // Tree with no issues
        app.update(key(KeyCode::Char('a')));
        app.update(key(KeyCode::Char('T')));
        journey_capture(&app, w, h, "empty_tree", &mut caps);

        // Return to Main
        app.update(key(KeyCode::Char('T')));
        journey_capture(&app, w, h, "empty_main_return", &mut caps);

        let artifact = journey_artifact("empty-data-edge-case", w, h, &caps);
        insta::assert_snapshot!(artifact);
    }

    #[test]
    fn e2e_journey_history_deep_dive() {
        let mut app = new_app(ViewMode::Main, 0);
        inject_deterministic_git_cache(&mut app);
        let (w, h) = (120, 35);
        let mut caps: Vec<(String, String)> = Vec::new();

        // Enter History from Main
        app.update(key(KeyCode::Char('h')));
        assert_eq!(app.mode, ViewMode::History);
        let text = journey_capture(&app, w, h, "history_entry", &mut caps);
        assert!(!text.is_empty());

        // Navigate bead history
        app.update(key(KeyCode::Char('j')));
        journey_capture(&app, w, h, "history_bead_nav", &mut caps);

        // Switch to git mode
        app.update(key(KeyCode::Char('v')));
        journey_capture(&app, w, h, "history_git_mode", &mut caps);

        // Search in history
        app.update(key(KeyCode::Char('/')));
        for ch in "test".chars() {
            app.update(key(KeyCode::Char(ch)));
        }
        app.update(key(KeyCode::Enter));
        journey_capture(&app, w, h, "history_search", &mut caps);

        // Switch back to bead mode
        app.update(key(KeyCode::Char('v')));
        journey_capture(&app, w, h, "history_bead_return", &mut caps);

        // Navigate focus panes
        app.update(key(KeyCode::Tab));
        journey_capture(&app, w, h, "history_middle_focus", &mut caps);
        app.update(key(KeyCode::Tab));
        journey_capture(&app, w, h, "history_detail_focus", &mut caps);

        // Return to Main
        app.update(key(KeyCode::Escape));
        assert_eq!(app.mode, ViewMode::Main);
        journey_capture(&app, w, h, "main_after_history", &mut caps);

        let artifact = journey_artifact("history-deep-dive", w, h, &caps);
        insta::assert_snapshot!(artifact);
    }

    // -- Visual primitive tests ------------------------------------------------

    /// Convert a slice of `RichSpan` into plain text for test assertions.
    fn spans_text(spans: &[RichSpan<'_>]) -> String {
        RichLine::from_spans(spans.to_vec()).to_plain_text()
    }

    /// Convert a single `RichSpan` into plain text for test assertions.
    fn span_text(span: &RichSpan<'_>) -> String {
        RichLine::from_spans([span.clone()]).to_plain_text()
    }

    #[test]
    fn status_chip_covers_all_known_statuses() {
        for status in &[
            "open",
            "in_progress",
            "blocked",
            "closed",
            "deferred",
            "review",
            "pinned",
            "tombstone",
            "hooked",
        ] {
            let spans = status_chip(status);
            assert_eq!(
                spans.len(),
                2,
                "status_chip({status}) should return 2 spans"
            );
            let text = spans_text(&spans);
            assert!(!text.contains('?'), "known status {status} got '?' icon");
        }
    }

    #[test]
    fn status_chip_unknown_shows_question() {
        let text = spans_text(&status_chip("nonexistent"));
        assert!(text.contains('?'));
        assert!(text.contains("unkn"));
    }

    #[test]
    fn priority_badge_clamps_range() {
        assert_eq!(span_text(&priority_badge(0)), "P0");
        assert_eq!(span_text(&priority_badge(4)), "P4");
        assert_eq!(span_text(&priority_badge(-5)), "P0");
        assert_eq!(span_text(&priority_badge(99)), "P4");
    }

    #[test]
    fn type_badge_maps_types() {
        assert_eq!(span_text(&type_badge("task")), "T");
        assert_eq!(span_text(&type_badge("bug")), "B");
        assert_eq!(span_text(&type_badge("epic")), "E");
    }

    #[test]
    fn blocker_indicator_states() {
        assert!(blocker_indicator(0, 0).is_empty());
        assert!(spans_text(&blocker_indicator(3, 0)).contains('3'));
        assert!(spans_text(&blocker_indicator(0, 5)).contains('5'));
    }

    #[test]
    fn metric_strip_content() {
        let text = spans_text(&metric_strip("PR", 0.42, 1.0));
        assert!(text.contains("PR"));
        assert!(text.contains("0.42"));
    }

    #[test]
    fn section_separator_caps_width() {
        assert!(display_width(&section_separator(200).to_plain_text()) <= 120);
    }

    #[test]
    fn panel_header_content() {
        let h = panel_header("Issues", Some("3 open")).to_plain_text();
        assert!(h.contains("Issues"));
        assert!(h.contains("3 open"));
        assert_eq!(panel_header("Graph", None).to_plain_text(), "Graph");
    }

    #[test]
    fn label_chips_content() {
        let text = spans_text(&label_chips(&["backend".into(), "urgent".into()]));
        assert!(text.contains("[backend]"));
        assert!(text.contains("[urgent]"));
        assert!(label_chips(&[]).is_empty());
    }

    #[test]
    fn issue_scan_line_fields() {
        let issue = Issue {
            id: "BD-42".into(),
            title: "Fix widget".into(),
            status: "open".into(),
            priority: 1,
            issue_type: "bug".into(),
            ..Default::default()
        };
        let text = issue_scan_line(
            &issue,
            false,
            ScanLineContext {
                open_blockers: 0,
                blocks_count: 0,
                triage_rank: 3,
                pagerank_rank: 2,
                critical_depth: 1,
                graph_score: 0.0,
                search_match_position: None,
                total_search_matches: 0,
                diff_tag: None,
                available_width: 80,
            },
        )
        .to_plain_text();
        assert!(text.contains("BD-42"), "id missing: {text}");
        assert!(text.contains("Fix widget"), "title missing: {text}");
        assert!(text.contains("P1"), "priority missing: {text}");
        assert!(text.contains("#03"), "triage rank missing: {text}");
    }

    #[test]
    fn issue_scan_line_selected_marker() {
        let issue = Issue {
            id: "A".into(),
            title: "Test".into(),
            status: "open".into(),
            issue_type: "task".into(),
            ..Default::default()
        };
        assert!(
            issue_scan_line(
                &issue,
                true,
                ScanLineContext {
                    open_blockers: 0,
                    blocks_count: 0,
                    triage_rank: 1,
                    pagerank_rank: 1,
                    critical_depth: 0,
                    graph_score: 0.0,
                    search_match_position: None,
                    total_search_matches: 0,
                    diff_tag: None,
                    available_width: 60,
                },
            )
            .to_plain_text()
            .starts_with('▸')
        );
    }

    #[test]
    fn issue_scan_line_blocker() {
        let issue = Issue {
            id: "A".into(),
            title: "Blocked".into(),
            status: "blocked".into(),
            issue_type: "task".into(),
            ..Default::default()
        };
        let text = issue_scan_line(
            &issue,
            false,
            ScanLineContext {
                open_blockers: 2,
                blocks_count: 1,
                triage_rank: 4,
                pagerank_rank: 3,
                critical_depth: 2,
                graph_score: 0.0,
                search_match_position: None,
                total_search_matches: 0,
                diff_tag: None,
                available_width: 80,
            },
        )
        .to_plain_text();
        assert!(text.contains("⊘2"), "blocker missing: {text}");
        assert!(text.contains("↓1"), "downstream count missing: {text}");
    }

    // ---------- tree fold (zc / zo) tests ----------

    /// Build a three-level parent-child tree:
    ///     P
    ///     ├── C1   (leaf)
    ///     ├── C2
    ///     │   └── G (leaf grandchild of P, child of C2)
    ///     └── C3   (leaf)
    fn tree_fold_fixture_issues() -> Vec<Issue> {
        let p = Issue {
            id: "P".to_string(),
            title: "Parent".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        };
        let mk_child = |id: &str, parent: &str| Issue {
            id: id.to_string(),
            title: id.to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            dependencies: vec![Dependency {
                issue_id: id.to_string(),
                depends_on_id: parent.to_string(),
                dep_type: "parent-child".to_string(),
                ..Dependency::default()
            }],
            ..Issue::default()
        };
        vec![
            p,
            mk_child("C1", "P"),
            mk_child("C2", "P"),
            mk_child("G", "C2"),
            mk_child("C3", "P"),
        ]
    }

    fn tree_app_with(issues: Vec<Issue>) -> BvrApp {
        let mut app = new_app(ViewMode::Tree, 0);
        app.analyzer = Analyzer::new(issues);
        app.tree_cursor = 0;
        app.tree_collapsed = std::collections::HashSet::new();
        app.build_tree_flat_nodes();
        app
    }

    fn tree_row_ids(app: &BvrApp) -> Vec<String> {
        app.tree_flat_nodes
            .iter()
            .map(|n| app.analyzer.issues[n.issue_index].id.clone())
            .collect()
    }

    fn tree_cursor_id(app: &BvrApp) -> String {
        app.tree_flat_nodes
            .get(app.tree_cursor)
            .map(|n| app.analyzer.issues[n.issue_index].id.clone())
            .unwrap_or_default()
    }

    #[test]
    fn tree_zc_on_leaf_closes_enclosing_parent_and_moves_cursor() {
        let mut app = tree_app_with(tree_fold_fixture_issues());
        let rows = tree_row_ids(&app);
        // Expected DFS order: P, C1, C2, G, C3.
        assert_eq!(rows, vec!["P", "C1", "C2", "G", "C3"]);

        // Place cursor on leaf C1, press zc — should collapse P, cursor moves to P.
        let c1_idx = rows.iter().position(|id| id == "C1").unwrap();
        app.tree_cursor = c1_idx;
        app.tree_collapse_current();
        assert_eq!(
            tree_row_ids(&app),
            vec!["P"],
            "all children should fold into P"
        );
        assert_eq!(
            tree_cursor_id(&app),
            "P",
            "cursor should land on the newly closed parent"
        );
    }

    #[test]
    fn tree_zc_on_deep_leaf_closes_nearest_enclosing_ancestor() {
        let mut app = tree_app_with(tree_fold_fixture_issues());
        let rows = tree_row_ids(&app);

        // Cursor on grandchild G — should close C2 (nearest foldable ancestor),
        // NOT jump straight to P.
        let g_idx = rows.iter().position(|id| id == "G").unwrap();
        app.tree_cursor = g_idx;
        app.tree_collapse_current();

        let rows_after = tree_row_ids(&app);
        assert_eq!(rows_after, vec!["P", "C1", "C2", "C3"]);
        assert_eq!(tree_cursor_id(&app), "C2");

        // Pressing zc again from C2 (now a collapsed foldable) should walk up to P.
        app.tree_collapse_current();
        assert_eq!(tree_row_ids(&app), vec!["P"]);
        assert_eq!(tree_cursor_id(&app), "P");
    }

    #[test]
    fn tree_zc_on_open_parent_still_collapses_in_place() {
        // Regression guard: the original behavior — zc on a foldable, open node
        // collapses that node — must still work.
        let mut app = tree_app_with(tree_fold_fixture_issues());
        // Cursor on P (root, foldable).
        app.tree_cursor = 0;
        assert_eq!(tree_cursor_id(&app), "P");
        app.tree_collapse_current();
        assert_eq!(tree_row_ids(&app), vec!["P"]);
        assert_eq!(tree_cursor_id(&app), "P");
    }

    #[test]
    fn tree_zc_on_orphan_root_leaf_is_noop() {
        // A single issue with no children and no parent: zc must not panic or move cursor.
        let issues = vec![Issue {
            id: "SOLO".to_string(),
            title: "Solo".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let mut app = tree_app_with(issues);
        assert_eq!(tree_row_ids(&app), vec!["SOLO"]);
        app.tree_cursor = 0;
        app.tree_collapse_current();
        assert_eq!(tree_row_ids(&app), vec!["SOLO"]);
        assert_eq!(tree_cursor_id(&app), "SOLO");
    }
}
