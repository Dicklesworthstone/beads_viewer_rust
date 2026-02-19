use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::analysis::graph::{GraphMetrics, IssueGraph};
use crate::model::Issue;
use crate::robot::compute_data_hash;

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
    pub generated_at: String,
    pub data_hash: String,
    pub alerts: Vec<Alert>,
    pub summary: AlertSummary,
    pub usage_hints: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AlertOptions {
    pub severity: Option<String>,
    pub alert_type: Option<String>,
    pub alert_label: Option<String>,
}

#[must_use]
pub fn generate_robot_alerts_output(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    options: &AlertOptions,
) -> RobotAlertsOutput {
    let generated_at = Utc::now().to_rfc3339();
    let now = Utc::now();
    let data_hash = compute_data_hash(issues);

    let mut alerts = Vec::<Alert>::new();
    detect_new_cycles(metrics, now, &mut alerts);
    detect_stale_issues(issues, now, &mut alerts);
    detect_blocking_cascades(issues, graph, now, &mut alerts);

    alerts.retain(|alert| matches_alert_filters(alert, options));
    let summary = summarize_alerts(&alerts);

    RobotAlertsOutput {
        generated_at,
        data_hash,
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

fn detect_stale_issues(issues: &[Issue], now: DateTime<Utc>, alerts: &mut Vec<Alert>) {
    for issue in issues {
        let status = issue.normalized_status();
        if status == "closed" || status == "tombstone" {
            continue;
        }

        let Some(last_active) = parse_timestamp(issue.updated_at.as_deref())
            .or_else(|| parse_timestamp(issue.created_at.as_deref()))
        else {
            continue;
        };

        let mut warning_days = STALE_WARNING_DAYS;
        let mut critical_days = STALE_CRITICAL_DAYS;
        if status == "in_progress" {
            warning_days *= IN_PROGRESS_STALE_MULTIPLIER;
            critical_days *= IN_PROGRESS_STALE_MULTIPLIER;
        }

        let inactivity = now.signed_duration_since(last_active);
        if inactivity.num_seconds() < 0 {
            continue;
        }
        let days = inactivity.num_seconds() as f64 / 86_400.0;

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
    alerts: &mut Vec<Alert>,
) {
    for issue_id in graph.actionable_ids() {
        let unblocks = compute_unblocks(graph, &issue_id);
        let unblocks_count = unblocks.len();
        if unblocks_count < BLOCKING_CASCADE_INFO_THRESHOLD {
            continue;
        }

        let severity = if unblocks_count >= BLOCKING_CASCADE_WARNING_THRESHOLD {
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

fn parse_timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    raw.and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|ts| ts.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::{
        AlertOptions, AlertSeverity, AlertType, generate_robot_alerts_output, parse_timestamp,
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
            closed_at: None,
            labels: Vec::new(),
            comments: Vec::new(),
            dependencies: Vec::new(),
            source_repo: String::new(),
        }
    }

    #[test]
    fn robot_alerts_include_cycle_stale_and_cascade() {
        let now = chrono::Utc::now();
        let stale_at = (now - Duration::days(20)).to_rfc3339();
        let fresh_at = (now - Duration::days(1)).to_rfc3339();

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh_at.clone());
        root.created_at = Some(fresh_at.clone());

        let mut d1 = issue("D1", "open");
        d1.updated_at = Some(fresh_at.clone());
        d1.created_at = Some(fresh_at.clone());
        d1.dependencies.push(Dependency {
            issue_id: "D1".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d2 = issue("D2", "open");
        d2.updated_at = Some(fresh_at.clone());
        d2.created_at = Some(fresh_at.clone());
        d2.dependencies.push(Dependency {
            issue_id: "D2".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d3 = issue("D3", "open");
        d3.updated_at = Some(fresh_at.clone());
        d3.created_at = Some(fresh_at);
        d3.dependencies.push(Dependency {
            issue_id: "D3".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut stale = issue("STALE", "open");
        stale.updated_at = Some(stale_at.clone());
        stale.created_at = Some(stale_at.clone());

        let mut tombstone = issue("TOMBSTONE", "tombstone");
        tombstone.updated_at = Some(stale_at.clone());
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
        let stale_at = (now - Duration::days(20)).to_rfc3339();
        let fresh_at = (now - Duration::days(1)).to_rfc3339();

        let mut root = issue("ROOT", "open");
        root.updated_at = Some(fresh_at.clone());
        root.created_at = Some(fresh_at.clone());

        let mut d1 = issue("D1", "open");
        d1.updated_at = Some(fresh_at.clone());
        d1.created_at = Some(fresh_at.clone());
        d1.dependencies.push(Dependency {
            issue_id: "D1".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d2 = issue("D2", "open");
        d2.updated_at = Some(fresh_at.clone());
        d2.created_at = Some(fresh_at.clone());
        d2.dependencies.push(Dependency {
            issue_id: "D2".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut d3 = issue("D3", "open");
        d3.updated_at = Some(fresh_at.clone());
        d3.created_at = Some(fresh_at);
        d3.dependencies.push(Dependency {
            issue_id: "D3".to_string(),
            depends_on_id: "ROOT".to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });

        let mut stale = issue("STALE", "open");
        stale.updated_at = Some(stale_at.clone());
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

    #[test]
    fn parse_timestamp_handles_rfc3339() {
        let ts = parse_timestamp(Some("2026-02-18T01:00:00Z"));
        assert!(ts.is_some());
        assert!(parse_timestamp(Some("not-a-timestamp")).is_none());
    }
}
