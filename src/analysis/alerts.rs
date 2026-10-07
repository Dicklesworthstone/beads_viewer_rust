use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::analysis::graph::{GraphMetrics, IssueGraph};
use crate::model::Issue;

const SECS_PER_DAY: u32 = 86_400;
const STALE_WARNING_DAYS: f64 = 14.0;
const STALE_CRITICAL_DAYS: f64 = 30.0;
const IN_PROGRESS_STALE_MULTIPLIER: f64 = 0.5;
const BLOCKING_CASCADE_INFO_THRESHOLD: usize = 3;
const BLOCKING_CASCADE_WARNING_THRESHOLD: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Critical,
    Warning,
    Info,
}

impl AlertSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    NewCycle,
    StaleIssue,
    BlockingCascade,
}

impl AlertType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewCycle => "new_cycle",
            Self::StaleIssue => "stale_issue",
            Self::BlockingCascade => "blocking_cascade",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    #[serde(rename = "type")]
    pub alert_type: AlertType,
    pub severity: AlertSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub detected_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unblocks_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downstream_priority_sum: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertSummary {
    pub total: usize,
    pub critical: usize,
    pub warning: usize,
    pub info: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RobotAlertsOutput {
    #[serde(flatten)]
    pub envelope: crate::robot::RobotEnvelope,
    pub alerts: Vec<Alert>,
    pub summary: AlertSummary,
    pub usage_hints: Vec<String>,
}

/// Tunable thresholds for alert generation.
#[derive(Debug, Clone)]
pub struct AlertThresholds {
    pub stale_warning_days: f64,
    pub stale_critical_days: f64,
    pub in_progress_stale_multiplier: f64,
    pub blocking_cascade_info: usize,
    pub blocking_cascade_warning: usize,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            stale_warning_days: STALE_WARNING_DAYS,
            stale_critical_days: STALE_CRITICAL_DAYS,
            in_progress_stale_multiplier: IN_PROGRESS_STALE_MULTIPLIER,
            blocking_cascade_info: BLOCKING_CASCADE_INFO_THRESHOLD,
            blocking_cascade_warning: BLOCKING_CASCADE_WARNING_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AlertOptions {
    pub severity: Option<String>,
    pub alert_type: Option<String>,
    pub alert_label: Option<String>,
    pub thresholds: AlertThresholds,
}

#[must_use]
pub fn generate_robot_alerts_output(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    options: &AlertOptions,
) -> RobotAlertsOutput {
    let now = Utc::now();

    let mut alerts = Vec::<Alert>::new();
    detect_new_cycles(metrics, now, &mut alerts);
    detect_stale_issues(issues, now, &options.thresholds, &mut alerts);
    detect_blocking_cascades(issues, graph, now, &options.thresholds, &mut alerts);

    alerts.retain(|alert| matches_alert_filters(alert, options));
    let summary = summarize_alerts(&alerts);

    RobotAlertsOutput {
        envelope: crate::robot::envelope(issues),
        alerts,
        summary,
        usage_hints: vec![
            "--severity=warning --alert-type=stale_issue   # stale warnings only".to_string(),
            "--alert-type=blocking_cascade                 # high-unblock opportunities"
                .to_string(),
            "jq '.alerts | map(.issue_id)'                # list impacted issues".to_string(),
        ],
    }
}

fn detect_new_cycles(metrics: &GraphMetrics, now: DateTime<Utc>, alerts: &mut Vec<Alert>) {
    if metrics.cycles.is_empty() {
        return;
    }

    let details = metrics
        .cycles
        .iter()
        .map(|cycle| cycle.join(" → "))
        .collect::<Vec<_>>();

    alerts.push(Alert {
        alert_type: AlertType::NewCycle,
        severity: AlertSeverity::Critical,
        message: format!("{} new cycle(s) detected", metrics.cycles.len()),
        baseline_value: Some(0.0),
        current_value: Some(metrics.cycles.len() as f64),
        delta: Some(metrics.cycles.len() as f64),
        details,
        issue_id: None,
        label: None,
        detected_at: now.to_rfc3339(),
        unblocks_count: None,
        downstream_priority_sum: None,
    });
}

fn detect_stale_issues(
    issues: &[Issue],
    now: DateTime<Utc>,
    thresholds: &AlertThresholds,
    alerts: &mut Vec<Alert>,
) {
    for issue in issues {
        let status = issue.normalized_status();
        if status == "closed" || status == "tombstone" {
            continue;
        }

        let Some(last_active) = issue.updated_at.or(issue.created_at) else {
            continue;
        };

        let mut warning_days = thresholds.stale_warning_days;
        let mut critical_days = thresholds.stale_critical_days;
        if status == "in_progress" {
            warning_days *= thresholds.in_progress_stale_multiplier;
            critical_days *= thresholds.in_progress_stale_multiplier;
        }

        let inactivity = now.signed_duration_since(last_active);
        if inactivity.num_seconds() < 0 {
            continue;
        }
        let days = inactivity.num_seconds() as f64 / f64::from(SECS_PER_DAY);

        let severity = if days >= critical_days {
            Some(AlertSeverity::Critical)
        } else if days >= warning_days {
            Some(AlertSeverity::Warning)
        } else {
            None
        };

        let Some(severity) = severity else {
            continue;
        };

        alerts.push(Alert {
            alert_type: AlertType::StaleIssue,
            severity,
            message: format!("Issue {} inactive for {:.0} days", issue.id, days),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: vec![
                format!("status={}", issue.status),
                format!("last_update={}", last_active.to_rfc3339()),
            ],
            issue_id: Some(issue.id.clone()),
            label: None,
            detected_at: now.to_rfc3339(),
            unblocks_count: None,
            downstream_priority_sum: None,
        });
    }
}

fn detect_blocking_cascades(
    issues: &[Issue],
    graph: &IssueGraph,
    now: DateTime<Utc>,
    thresholds: &AlertThresholds,
    alerts: &mut Vec<Alert>,
) {
    // Cascade alerts are about graph structure ("completing X unblocks N"), so
    // they consider every dependency-unblocked bead, including one parked in a
    // non-actionable status (a deferred blocker with fan-out is exactly what
    // this alert should surface).
    for issue_id in graph.dependency_unblocked_ids() {
        let unblocks = compute_unblocks(graph, &issue_id);
        let unblocks_count = unblocks.len();
        if unblocks_count < thresholds.blocking_cascade_info {
            continue;
        }

        let severity = if unblocks_count >= thresholds.blocking_cascade_warning {
            AlertSeverity::Warning
        } else {
            AlertSeverity::Info
        };

        let downstream_priority_sum = unblocks
            .iter()
            .filter_map(|id| issues.iter().find(|issue| issue.id == *id))
            .map(|issue| issue.priority)
            .sum::<i32>();

        alerts.push(Alert {
            alert_type: AlertType::BlockingCascade,
            severity,
            message: format!("Completing {issue_id} unblocks {unblocks_count} downstream item(s)"),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: unblocks,
            issue_id: Some(issue_id),
            label: None,
            detected_at: now.to_rfc3339(),
            unblocks_count: Some(unblocks_count),
            downstream_priority_sum: Some(downstream_priority_sum),
        });
    }
}

fn compute_unblocks(graph: &IssueGraph, issue_id: &str) -> Vec<String> {
    let mut unblocks = Vec::<String>::new();
    for dependent_id in graph.dependents(issue_id) {
        let Some(dependent_issue) = graph.issue(&dependent_id) else {
            continue;
        };
        if dependent_issue.is_closed_like() {
            continue;
        }

        let still_blocked = graph.blockers(&dependent_id).into_iter().any(|blocker| {
            blocker != issue_id && graph.issue(&blocker).is_some_and(Issue::is_open_like)
        });

        if !still_blocked {
            unblocks.push(dependent_id);
        }
    }

    unblocks.sort();
    unblocks
}

fn matches_alert_filters(alert: &Alert, options: &AlertOptions) -> bool {
    if options
        .severity
        .as_deref()
        .is_some_and(|severity| !alert.severity.as_str().eq_ignore_ascii_case(severity))
    {
        return false;
    }

    if options
        .alert_type
        .as_deref()
        .is_some_and(|alert_type| !alert.alert_type.as_str().eq_ignore_ascii_case(alert_type))
    {
        return false;
    }

    if let Some(raw_label) = options.alert_label.as_deref() {
        let needle = raw_label.to_ascii_lowercase();
        let found_in_details = alert
            .details
            .iter()
            .any(|detail| detail.to_ascii_lowercase().contains(&needle));

        if found_in_details {
            return true;
        }

        let found_in_label = alert
            .label
            .as_ref()
            .is_some_and(|label| label.to_ascii_lowercase().contains(&needle));
        if !found_in_label {
            return false;
        }
    }

    true
}

fn summarize_alerts(alerts: &[Alert]) -> AlertSummary {
    let mut summary = AlertSummary {
        total: alerts.len(),
        critical: 0,
        warning: 0,
        info: 0,
    };

    for alert in alerts {
        match alert.severity {
            AlertSeverity::Critical => summary.critical = summary.critical.saturating_add(1),
            AlertSeverity::Warning => summary.warning = summary.warning.saturating_add(1),
            AlertSeverity::Info => summary.info = summary.info.saturating_add(1),
        }
    }

    summary
}

// ---------------------------------------------------------------------------
// Sprint at-risk detection (legacy bv `analysis.DetectAtRisk`)
// ---------------------------------------------------------------------------

/// Day thresholds behind the time-based sprint at-risk signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtRiskThresholds {
    /// Minimum blocked duration for `blocked_too_long`.
    pub blocked_days: i64,
    /// Minimum inactivity for `no_activity`, and for a blocker to count as
    /// stalled in `blockers_not_closing`.
    pub inactive_days: i64,
}

impl Default for AtRiskThresholds {
    /// Legacy defaults: blocked for 2+ days, inactive for 4+ days.
    fn default() -> Self {
        Self {
            blocked_days: 2,
            inactive_days: 4,
        }
    }
}

/// One sprint bead flagged by [`detect_at_risk`].
#[derive(Debug, Clone, Serialize)]
pub struct AtRiskItem {
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub status: String,
    pub priority: i32,
    /// `blocked_too_long`, `no_activity`, `critical_blocked`,
    /// `blockers_not_closing`, in evaluation order.
    pub signals: Vec<&'static str>,
    /// Earliest instant behind any signal: the start of the blocked period,
    /// the last activity, or a stalled blocker's last activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Human-readable explanation of each signal.
    pub detail: String,
}

/// Flag the non-closed beads of a sprint that are at risk, as legacy bv's
/// `--robot-burndown` and sprint dashboard do. Four signals:
///
/// - `blocked_too_long`: blocked (status `blocked`, or waiting on an open
///   blocking dependency) for at least `blocked_days`;
/// - `no_activity`: no update for at least `inactive_days`;
/// - `critical_blocked`: a P0/P1 bead blocked at all;
/// - `blockers_not_closing`: an open blocker itself idle `inactive_days`.
///
/// There is no status-transition history, so "blocked since" is the later of
/// the bead's last activity and the creation of an open blocking dependency
/// — a lower bound that never overstates the blocked duration. Blockers
/// resolve against all issues, so one outside the sprint still counts.
/// Results are sorted by ID.
#[must_use]
pub fn detect_at_risk(
    issues: &[Issue],
    sprint_bead_ids: &[String],
    now: DateTime<Utc>,
    thresholds: AtRiskThresholds,
) -> Vec<AtRiskItem> {
    let by_id: std::collections::HashMap<&str, &Issue> = issues
        .iter()
        .map(|issue| (issue.id.as_str(), issue))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<AtRiskItem> = sprint_bead_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .filter(|issue| !issue.is_closed_like())
        .filter_map(|issue| evaluate_at_risk(issue, &by_id, now, thresholds))
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

fn last_activity(issue: &Issue) -> Option<DateTime<Utc>> {
    issue.updated_at.or(issue.created_at)
}

fn days_between(from: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    (now - from).num_seconds() as f64 / f64::from(SECS_PER_DAY)
}

fn evaluate_at_risk(
    issue: &Issue,
    by_id: &std::collections::HashMap<&str, &Issue>,
    now: DateTime<Utc>,
    thresholds: AtRiskThresholds,
) -> Option<AtRiskItem> {
    let last = last_activity(issue);
    let mut open_blockers: Vec<(&str, Option<DateTime<Utc>>)> = Vec::new();
    let mut blocked_since = last;
    for dep in issue.dependencies.iter().filter(|dep| dep.is_blocking()) {
        let Some(target) = by_id.get(dep.depends_on_id.as_str()) else {
            continue;
        };
        if target.is_closed_like() {
            continue;
        }
        open_blockers.push((target.id.as_str(), last_activity(target)));
        if let Some(created) = dep.created_at {
            if blocked_since.is_none_or(|since| created > since) {
                blocked_since = Some(created);
            }
        }
    }
    open_blockers.sort_by(|a, b| a.0.cmp(b.0));
    let blocked = issue.normalized_status() == "blocked" || !open_blockers.is_empty();

    let mut signals = Vec::new();
    let mut details = Vec::new();
    let mut since: Option<DateTime<Utc>> = None;
    let mut note_since = |at: Option<DateTime<Utc>>| {
        if let Some(at) = at {
            if since.is_none_or(|current| at < current) {
                since = Some(at);
            }
        }
    };

    if blocked {
        if let Some(start) = blocked_since {
            let days = days_between(start, now);
            if days >= thresholds.blocked_days as f64 {
                signals.push("blocked_too_long");
                details.push(format!(
                    "blocked for {days:.0}d (threshold {}d)",
                    thresholds.blocked_days
                ));
                note_since(Some(start));
            }
        }
    }
    if let Some(at) = last {
        let idle = days_between(at, now);
        if idle >= thresholds.inactive_days as f64 {
            signals.push("no_activity");
            details.push(format!(
                "no activity for {idle:.0}d (threshold {}d)",
                thresholds.inactive_days
            ));
            note_since(Some(at));
        }
    }
    if blocked && issue.priority <= 1 {
        signals.push("critical_blocked");
        let by = if open_blockers.is_empty() {
            "status=blocked".to_string()
        } else {
            format!(
                "blocked by {}",
                open_blockers
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        details.push(format!("P{} {by}", issue.priority));
        note_since(blocked_since.or(last));
    }
    let stalled: Vec<String> = open_blockers
        .iter()
        .filter_map(|(id, activity)| {
            let at = (*activity)?;
            let idle = days_between(at, now);
            (idle >= thresholds.inactive_days as f64).then(|| {
                note_since(Some(at));
                format!("{id} ({idle:.0}d idle)")
            })
        })
        .collect();
    if !stalled.is_empty() {
        signals.push("blockers_not_closing");
        details.push(format!("blockers stalled: {}", stalled.join(", ")));
    }

    if signals.is_empty() {
        return None;
    }
    Some(AtRiskItem {
        id: issue.id.clone(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        priority: issue.priority,
        signals,
        since: since.or(last),
        detail: details.join("; "),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::{
        AlertOptions, AlertSeverity, AlertType, AtRiskThresholds, detect_at_risk,
        generate_robot_alerts_output,
    };
    use crate::analysis::graph::IssueGraph;
    use crate::model::{Dependency, Issue};

    fn issue(id: &str, status: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: status.to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            labels: Vec::new(),
            comments: Vec::new(),
            dependencies: Vec::new(),
            source_repo: String::new(),
            workspace_prefix: None,
            workspace_repo_path: None,
            content_hash: None,
            external_ref: None,
        }
    }

    #[test]
    fn robot_alerts_include_cycle_stale_and_cascade() {
        let now = chrono::Utc::now();
        let stale_at = now - Duration::days(20);
        let fresh_at = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh_at);
        root.created_at = Some(fresh_at);

        let mut d1 = issue("D1", "open");
        d1.updated_at = Some(fresh_at);
        d1.created_at = Some(fresh_at);
        d1.dependencies.push(Dependency {
            issue_id: "D1".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d2 = issue("D2", "open");
        d2.updated_at = Some(fresh_at);
        d2.created_at = Some(fresh_at);
        d2.dependencies.push(Dependency {
            issue_id: "D2".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d3 = issue("D3", "open");
        d3.updated_at = Some(fresh_at);
        d3.created_at = Some(fresh_at);
        d3.dependencies.push(Dependency {
            issue_id: "D3".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut stale = issue("STALE", "open");
        stale.updated_at = Some(stale_at);
        stale.created_at = Some(stale_at);

        let mut tombstone = issue("TOMBSTONE", "tombstone");
        tombstone.updated_at = Some(stale_at);
        tombstone.created_at = Some(stale_at);

        let mut cycle_a = issue("cycle-a", "open");
        cycle_a.dependencies.push(Dependency {
            issue_id: "cycle-a".to_string(),
            depends_on_id: "cycle-b".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut cycle_b = issue("cycle-b", "open");
        cycle_b.dependencies.push(Dependency {
            issue_id: "cycle-b".to_string(),
            depends_on_id: "cycle-a".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let issues = vec![root, d1, d2, d3, stale, tombstone, cycle_a, cycle_b];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let output =
            generate_robot_alerts_output(&issues, &graph, &metrics, &AlertOptions::default());
        assert_eq!(output.summary.total, output.alerts.len());
        assert!(output.alerts.iter().any(|alert| {
            alert.alert_type == AlertType::StaleIssue
                && alert.severity == AlertSeverity::Warning
                && alert.issue_id.as_deref() == Some("STALE")
        }));
        assert!(!output.alerts.iter().any(|alert| {
            alert.alert_type == AlertType::StaleIssue
                && alert.issue_id.as_deref() == Some("TOMBSTONE")
        }));
        assert!(output.alerts.iter().any(|alert| {
            alert.alert_type == AlertType::BlockingCascade
                && alert.issue_id.as_deref() == Some("ROOT")
        }));
        assert!(
            output
                .alerts
                .iter()
                .any(|alert| alert.alert_type == AlertType::NewCycle)
        );
    }

    #[test]
    fn robot_alert_filters_are_applied() {
        let now = chrono::Utc::now();
        let stale_at = now - Duration::days(20);
        let fresh_at = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh_at);
        root.created_at = Some(fresh_at);

        let mut d1 = issue("D1", "open");
        d1.updated_at = Some(fresh_at);
        d1.created_at = Some(fresh_at);
        d1.dependencies.push(Dependency {
            issue_id: "D1".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d2 = issue("D2", "open");
        d2.updated_at = Some(fresh_at);
        d2.created_at = Some(fresh_at);
        d2.dependencies.push(Dependency {
            issue_id: "D2".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d3 = issue("D3", "open");
        d3.updated_at = Some(fresh_at);
        d3.created_at = Some(fresh_at);
        d3.dependencies.push(Dependency {
            issue_id: "D3".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut stale = issue("STALE", "open");
        stale.updated_at = Some(stale_at);
        stale.created_at = Some(stale_at);

        let issues = vec![root, d1, d2, d3, stale];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let warning_only = generate_robot_alerts_output(
            &issues,
            &graph,
            &metrics,
            &AlertOptions {
                severity: Some("warning".to_string()),
                alert_type: None,
                alert_label: None,
                ..AlertOptions::default()
            },
        );
        assert!(
            warning_only
                .alerts
                .iter()
                .all(|alert| alert.severity == AlertSeverity::Warning)
        );

        let stale_only = generate_robot_alerts_output(
            &issues,
            &graph,
            &metrics,
            &AlertOptions {
                severity: None,
                alert_type: Some("stale_issue".to_string()),
                alert_label: None,
                ..AlertOptions::default()
            },
        );
        assert!(!stale_only.alerts.is_empty());
        assert!(
            stale_only
                .alerts
                .iter()
                .all(|alert| alert.alert_type == AlertType::StaleIssue)
        );

        let detail_filter = generate_robot_alerts_output(
            &issues,
            &graph,
            &metrics,
            &AlertOptions {
                severity: None,
                alert_type: Some("blocking_cascade".to_string()),
                alert_label: Some("d1".to_string()),
                ..AlertOptions::default()
            },
        );
        assert_eq!(detail_filter.alerts.len(), 1);
        assert_eq!(detail_filter.alerts[0].issue_id.as_deref(), Some("ROOT"));

        let case_insensitive = generate_robot_alerts_output(
            &issues,
            &graph,
            &metrics,
            &AlertOptions {
                severity: Some("WaRnInG".to_string()),
                alert_type: Some("StAlE_IsSuE".to_string()),
                alert_label: None,
                ..AlertOptions::default()
            },
        );
        assert!(!case_insensitive.alerts.is_empty());
        assert!(
            case_insensitive
                .alerts
                .iter()
                .all(|alert| alert.severity == AlertSeverity::Warning
                    && alert.alert_type == AlertType::StaleIssue)
        );
    }

    // ── detect_stale_issues ─────────────────────────────────────────

    #[test]
    fn stale_warning_at_14_days() {
        let now = chrono::Utc::now();
        let at_15_days = now - Duration::days(15);
        let mut alerts = Vec::new();
        let mut i = issue("A", "open");
        i.updated_at = Some(at_15_days);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Warning);
    }

    #[test]
    fn stale_critical_at_30_days() {
        let now = chrono::Utc::now();
        let at_31_days = now - Duration::days(31);
        let mut alerts = Vec::new();
        let mut i = issue("A", "open");
        i.updated_at = Some(at_31_days);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn stale_not_triggered_for_fresh_issue() {
        let now = chrono::Utc::now();
        let fresh = now - Duration::days(5);
        let mut alerts = Vec::new();
        let mut i = issue("A", "open");
        i.updated_at = Some(fresh);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert!(alerts.is_empty());
    }

    #[test]
    fn stale_skips_closed_and_tombstone() {
        let now = chrono::Utc::now();
        let old = now - Duration::days(60);
        let mut alerts = Vec::new();
        let mut closed = issue("A", "closed");
        closed.updated_at = Some(old);
        let mut tomb = issue("B", "tombstone");
        tomb.updated_at = Some(old);
        super::detect_stale_issues(
            &[closed, tomb],
            now,
            &super::AlertThresholds::default(),
            &mut alerts,
        );
        assert!(alerts.is_empty());
    }

    #[test]
    fn in_progress_stale_multiplier_halves_thresholds() {
        let now = chrono::Utc::now();
        // in_progress: 14 * 0.5 = 7 day warning threshold, so 8 days triggers warning
        let at_8_days = now - Duration::days(8);
        let mut alerts = Vec::new();
        let mut i = issue("A", "in_progress");
        i.updated_at = Some(at_8_days);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Warning);
        assert!(alerts[0].message.contains("A"));
    }

    #[test]
    fn in_progress_critical_at_half_threshold() {
        let now = chrono::Utc::now();
        // 30 * 0.5 = 15 days critical threshold for in_progress
        let at_16_days = now - Duration::days(16);
        let mut alerts = Vec::new();
        let mut i = issue("A", "in_progress");
        i.updated_at = Some(at_16_days);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn stale_falls_back_to_created_at() {
        let now = chrono::Utc::now();
        let old = now - Duration::days(20);
        let mut alerts = Vec::new();
        let mut i = issue("A", "open");
        i.updated_at = None;
        i.created_at = Some(old);
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].issue_id.as_deref(), Some("A"));
    }

    #[test]
    fn stale_skips_no_timestamps() {
        let now = chrono::Utc::now();
        let mut alerts = Vec::new();
        let i = issue("A", "open");
        super::detect_stale_issues(&[i], now, &super::AlertThresholds::default(), &mut alerts);
        assert!(alerts.is_empty());
    }

    // ── detect_new_cycles ───────────────────────────────────────────

    #[test]
    fn cycle_alert_severity_is_critical() {
        let graph = IssueGraph::build(&[]);
        let mut metrics = graph.compute_metrics();
        metrics.cycles.push(vec!["A".to_string(), "B".to_string()]);

        let now = chrono::Utc::now();
        let mut alerts = Vec::new();
        super::detect_new_cycles(&metrics, now, &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
        assert_eq!(alerts[0].alert_type, AlertType::NewCycle);
        assert_eq!(alerts[0].current_value, Some(1.0));
    }

    #[test]
    fn cycle_alert_empty_cycles() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let mut alerts = Vec::new();
        super::detect_new_cycles(&metrics, chrono::Utc::now(), &mut alerts);
        assert!(alerts.is_empty());
    }

    // ── detect_blocking_cascades ────────────────────────────────────

    #[test]
    fn cascade_info_at_3_dependents() {
        let now = chrono::Utc::now();
        let fresh = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh);

        let mut deps = Vec::new();
        for i in 1..=3 {
            let mut d = issue(&format!("D{i}"), "open");
            d.updated_at = Some(fresh);
            d.dependencies.push(Dependency {
                issue_id: d.id.clone(),
                depends_on_id: "ROOT".to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            });
            deps.push(d);
        }

        let mut issues = vec![root];
        issues.extend(deps);
        let graph = IssueGraph::build(&issues);

        let mut alerts = Vec::new();
        super::detect_blocking_cascades(
            &issues,
            &graph,
            now,
            &super::AlertThresholds::default(),
            &mut alerts,
        );
        assert!(!alerts.is_empty());
        let cascade = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::BlockingCascade)
            .unwrap();
        assert_eq!(cascade.severity, AlertSeverity::Info);
        assert_eq!(cascade.unblocks_count, Some(3));
    }

    #[test]
    fn cascade_warning_at_5_dependents() {
        let now = chrono::Utc::now();
        let fresh = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh);

        let mut deps = Vec::new();
        for i in 1..=5 {
            let mut d = issue(&format!("D{i}"), "open");
            d.updated_at = Some(fresh);
            d.dependencies.push(Dependency {
                issue_id: d.id.clone(),
                depends_on_id: "ROOT".to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            });
            deps.push(d);
        }

        let mut issues = vec![root];
        issues.extend(deps);
        let graph = IssueGraph::build(&issues);

        let mut alerts = Vec::new();
        super::detect_blocking_cascades(
            &issues,
            &graph,
            now,
            &super::AlertThresholds::default(),
            &mut alerts,
        );
        let cascade = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::BlockingCascade)
            .unwrap();
        assert_eq!(cascade.severity, AlertSeverity::Warning);
        assert_eq!(cascade.unblocks_count, Some(5));
    }

    #[test]
    fn cascade_not_triggered_below_threshold() {
        let now = chrono::Utc::now();
        let fresh = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh);

        let mut d1 = issue("D1", "open");
        d1.updated_at = Some(fresh);
        d1.dependencies.push(Dependency {
            issue_id: "D1".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let issues = vec![root, d1];
        let graph = IssueGraph::build(&issues);

        let mut alerts = Vec::new();
        super::detect_blocking_cascades(
            &issues,
            &graph,
            now,
            &super::AlertThresholds::default(),
            &mut alerts,
        );
        assert!(alerts.is_empty(), "1 dependent < threshold of 3");
    }

    #[test]
    fn cascade_downstream_priority_sum() {
        let now = chrono::Utc::now();
        let fresh = now - Duration::days(1);

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh);

        let mut deps = Vec::new();
        for i in 1..=3 {
            let mut d = issue(&format!("D{i}"), "open");
            d.updated_at = Some(fresh);
            d.priority = i as i32; // priorities 1, 2, 3
            d.dependencies.push(Dependency {
                issue_id: d.id.clone(),
                depends_on_id: "ROOT".to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            });
            deps.push(d);
        }

        let mut issues = vec![root];
        issues.extend(deps);
        let graph = IssueGraph::build(&issues);

        let mut alerts = Vec::new();
        super::detect_blocking_cascades(
            &issues,
            &graph,
            now,
            &super::AlertThresholds::default(),
            &mut alerts,
        );
        let cascade = alerts
            .iter()
            .find(|a| a.alert_type == AlertType::BlockingCascade)
            .unwrap();
        assert_eq!(cascade.downstream_priority_sum, Some(6)); // 1+2+3
    }

    // ── summarize_alerts ────────────────────────────────────────────

    #[test]
    fn summarize_counts_by_severity() {
        let now_str = chrono::Utc::now().to_rfc3339();
        let mk = |severity, alert_type| super::Alert {
            alert_type,
            severity,
            message: String::new(),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: Vec::new(),
            issue_id: None,
            label: None,
            detected_at: now_str.clone(),
            unblocks_count: None,
            downstream_priority_sum: None,
        };

        let alerts = vec![
            mk(AlertSeverity::Critical, AlertType::NewCycle),
            mk(AlertSeverity::Warning, AlertType::StaleIssue),
            mk(AlertSeverity::Warning, AlertType::StaleIssue),
            mk(AlertSeverity::Info, AlertType::BlockingCascade),
        ];
        let summary = super::summarize_alerts(&alerts);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.warning, 2);
        assert_eq!(summary.info, 1);
    }

    #[test]
    fn summarize_empty() {
        let summary = super::summarize_alerts(&[]);
        assert_eq!(summary.total, 0);
        assert_eq!(summary.critical, 0);
    }

    // ── matches_alert_filters ───────────────────────────────────────

    #[test]
    fn filter_no_options_matches_all() {
        let alert = super::Alert {
            alert_type: AlertType::StaleIssue,
            severity: AlertSeverity::Warning,
            message: String::new(),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: Vec::new(),
            issue_id: None,
            label: None,
            detected_at: String::new(),
            unblocks_count: None,
            downstream_priority_sum: None,
        };
        assert!(super::matches_alert_filters(
            &alert,
            &AlertOptions::default()
        ));
    }

    #[test]
    fn filter_label_in_details() {
        let alert = super::Alert {
            alert_type: AlertType::BlockingCascade,
            severity: AlertSeverity::Info,
            message: String::new(),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: vec!["D1".to_string(), "D2".to_string()],
            issue_id: None,
            label: None,
            detected_at: String::new(),
            unblocks_count: None,
            downstream_priority_sum: None,
        };
        let opts = AlertOptions {
            alert_label: Some("d1".to_string()),
            ..AlertOptions::default()
        };
        assert!(super::matches_alert_filters(&alert, &opts));

        let no_match = AlertOptions {
            alert_label: Some("xyz".to_string()),
            ..AlertOptions::default()
        };
        assert!(!super::matches_alert_filters(&alert, &no_match));
    }

    #[test]
    fn filter_label_field_match() {
        let alert = super::Alert {
            alert_type: AlertType::StaleIssue,
            severity: AlertSeverity::Warning,
            message: String::new(),
            baseline_value: None,
            current_value: None,
            delta: None,
            details: Vec::new(),
            issue_id: None,
            label: Some("Backend".to_string()),
            detected_at: String::new(),
            unblocks_count: None,
            downstream_priority_sum: None,
        };
        let opts = AlertOptions {
            alert_label: Some("backend".to_string()),
            ..AlertOptions::default()
        };
        assert!(super::matches_alert_filters(&alert, &opts));
    }

    fn ago(days: i64) -> chrono::DateTime<chrono::Utc> {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-10T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        now - Duration::days(days)
    }

    fn sprint_issue(id: &str, status: &str, priority: i32, updated_days_ago: i64) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("{id} title"),
            status: status.to_string(),
            priority,
            created_at: Some(ago(30)),
            updated_at: Some(ago(updated_days_ago)),
            ..Issue::default()
        }
    }

    fn blocks(on: &str, created_days_ago: i64) -> Dependency {
        Dependency {
            depends_on_id: on.to_string(),
            dep_type: "blocks".to_string(),
            created_at: Some(ago(created_days_ago)),
            ..Dependency::default()
        }
    }

    #[test]
    fn at_risk_flags_each_legacy_signal() {
        let now = ago(0);
        let mut critical = sprint_issue("A", "open", 1, 3);
        critical.dependencies = vec![blocks("STALL", 3)];
        let idle = sprint_issue("B", "open", 3, 9);
        let fresh = sprint_issue("C", "in_progress", 2, 0);
        let done = sprint_issue("D", "closed", 0, 40);
        // Outside the sprint, but blocking A and idle for 6 days.
        let stall = sprint_issue("STALL", "open", 2, 6);
        let issues = vec![critical, idle, fresh, done, stall];
        let ids: Vec<String> = ["A", "B", "C", "D", "A"].map(String::from).to_vec();

        let items = detect_at_risk(&issues, &ids, now, AtRiskThresholds::default());
        let summary: Vec<(&str, Vec<&str>)> = items
            .iter()
            .map(|item| (item.id.as_str(), item.signals.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "A",
                    vec![
                        "blocked_too_long",
                        "critical_blocked",
                        "blockers_not_closing"
                    ]
                ),
                ("B", vec!["no_activity"]),
            ]
        );
        let a = &items[0];
        assert!(
            a.detail.contains("blocked for 3d (threshold 2d)"),
            "{}",
            a.detail
        );
        assert!(a.detail.contains("P1 blocked by STALL"), "{}", a.detail);
        assert!(a.detail.contains("STALL (6d idle)"), "{}", a.detail);
        // The stalled blocker's last activity is the earliest trigger.
        assert_eq!(a.since, Some(ago(6)));
    }

    #[test]
    fn at_risk_respects_thresholds_and_empty_inputs() {
        let now = ago(0);
        let issues = vec![sprint_issue("A", "blocked", 2, 1)];
        let ids = vec!["A".to_string()];
        assert!(detect_at_risk(&issues, &ids, now, AtRiskThresholds::default()).is_empty());
        let strict = AtRiskThresholds {
            blocked_days: 1,
            inactive_days: 1,
        };
        let items = detect_at_risk(&issues, &ids, now, strict);
        assert_eq!(items[0].signals, vec!["blocked_too_long", "no_activity"]);
        assert!(detect_at_risk(&[], &ids, now, strict).is_empty());
        assert!(detect_at_risk(&issues, &[], now, strict).is_empty());
    }
}
