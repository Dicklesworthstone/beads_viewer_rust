use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::analysis::graph::{GraphMetrics, IssueGraph};
use crate::model::Issue;

const DEFAULT_ESTIMATED_MINUTES: i64 = 60;
const I64_MAX_F64: f64 = 9_223_372_036_854_775_807.0;
const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;

#[derive(Debug, Clone, Serialize)]
pub struct ForecastItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub confidence: f64,
    pub eta_minutes: i64,
    pub estimated_days: f64,
    pub eta_date: String,
    pub eta_date_low: String,
    pub eta_date_high: String,
    pub velocity_minutes_per_day: f64,
    pub agents: usize,
    pub factors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForecastSummary {
    pub generated_at: String,
    pub count: usize,
    pub avg_eta_minutes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForecastOutput {
    pub summary: ForecastSummary,
    pub forecasts: Vec<ForecastItem>,
}

#[derive(Debug, Clone)]
pub struct EtaEstimate {
    pub estimated_minutes: i64,
    pub estimated_days: f64,
    pub eta_date: String,
    pub eta_date_low: String,
    pub eta_date_high: String,
    pub confidence: f64,
    pub velocity_minutes_per_day: f64,
    pub agents: usize,
    pub factors: Vec<String>,
}

#[must_use]
pub fn estimate_forecast(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    issue_id_or_all: &str,
    label_filter: Option<&str>,
    agents: usize,
) -> ForecastOutput {
    let now = Utc::now();
    let mut forecasts = Vec::<ForecastItem>::new();

    let target_all = issue_id_or_all.eq_ignore_ascii_case("all");

    for issue in issues {
        if !issue.is_open_like() {
            continue;
        }
        if !target_all && issue.id != issue_id_or_all {
            continue;
        }
        if label_filter.is_some_and(|label| !has_label(&issue.labels, label)) {
            continue;
        }

        let Some(eta) = estimate_eta_for_issue(issues, graph, metrics, &issue.id, agents, now)
        else {
            continue;
        };

        forecasts.push(ForecastItem {
            id: issue.id.clone(),
            title: issue.title.clone(),
            status: issue.status.clone(),
            confidence: eta.confidence,
            eta_minutes: eta.estimated_minutes,
            estimated_days: eta.estimated_days,
            eta_date: eta.eta_date,
            eta_date_low: eta.eta_date_low,
            eta_date_high: eta.eta_date_high,
            velocity_minutes_per_day: eta.velocity_minutes_per_day,
            agents: eta.agents,
            factors: eta.factors,
        });
    }

    let avg_eta_minutes = if forecasts.is_empty() {
        0
    } else {
        forecasts.iter().map(|item| item.eta_minutes).sum::<i64>()
            / i64::try_from(forecasts.len()).unwrap_or(1)
    };

    ForecastOutput {
        summary: ForecastSummary {
            generated_at: now.to_rfc3339(),
            count: forecasts.len(),
            avg_eta_minutes,
        },
        forecasts,
    }
}

#[must_use]
pub fn estimate_eta_for_issue(
    issues: &[Issue],
    _graph: &IssueGraph,
    metrics: &GraphMetrics,
    issue_id: &str,
    agents: usize,
    now: DateTime<Utc>,
) -> Option<EtaEstimate> {
    let issue = issues.iter().find(|issue| issue.id == issue_id)?;
    let agents = agents.max(1);

    let median_minutes = compute_median_estimated_minutes(issues);
    let (complexity_minutes, mut factors) =
        estimate_complexity_minutes(issue, metrics, median_minutes);
    let (mut velocity_per_day, velocity_samples, velocity_factors) =
        estimate_velocity_minutes_per_day(issues, issue, now, median_minutes);

    if velocity_per_day <= 0.0 {
        velocity_per_day = (median_minutes as f64) / 5.0;
        if velocity_per_day <= 0.0 {
            velocity_per_day = 60.0;
        }
        factors.extend(velocity_factors);
        factors.push("velocity: no recent closures; using default".to_string());
    } else {
        factors.extend(velocity_factors);
    }

    let capacity_per_day = velocity_per_day * (agents as f64);
    let mut estimated_days = if capacity_per_day > 0.0 {
        (complexity_minutes as f64) / capacity_per_day
    } else {
        0.0
    };
    if estimated_days.is_sign_negative() {
        estimated_days = 0.0;
    }

    let confidence = estimate_eta_confidence(issue, velocity_samples);
    let delta_days = 0.5_f64.max(estimated_days * (1.0 - confidence) * 0.8);

    let eta = now + duration_days(estimated_days);
    let eta_low = now + duration_days((estimated_days - delta_days).max(0.0));
    let eta_high = now + duration_days(estimated_days + delta_days);

    factors.push(format!("agents: {agents}"));
    if factors.len() > 8 {
        factors.truncate(8);
    }

    Some(EtaEstimate {
        estimated_minutes: complexity_minutes,
        estimated_days,
        eta_date: eta.to_rfc3339(),
        eta_date_low: eta_low.to_rfc3339(),
        eta_date_high: eta_high.to_rfc3339(),
        confidence,
        velocity_minutes_per_day: velocity_per_day,
        agents,
        factors,
    })
}

fn estimate_complexity_minutes(
    issue: &Issue,
    metrics: &GraphMetrics,
    median_minutes: i64,
) -> (i64, Vec<String>) {
    let mut factors = Vec::<String>::new();

    let explicit = issue.estimated_minutes.unwrap_or(0) > 0;
    let mut base_minutes = if explicit {
        i64::from(issue.estimated_minutes.unwrap_or(0))
    } else {
        median_minutes
    };

    let estimate_source = if explicit {
        "explicit"
    } else if base_minutes > 0 {
        "median"
    } else {
        "default"
    };
    if base_minutes <= 0 {
        base_minutes = DEFAULT_ESTIMATED_MINUTES;
    }
    factors.push(format!("estimate: {estimate_source} ({base_minutes}m)"));

    let issue_type = issue.issue_type.trim().to_ascii_lowercase();
    let type_weight = match issue_type.as_str() {
        "chore" => 0.8,
        "feature" => 1.3,
        "epic" => 2.0,
        _ => 1.0,
    };
    factors.push(format!("type: {issue_type}×{type_weight:.1}"));

    let depth = metrics.critical_depth.get(&issue.id).copied().unwrap_or(0) as f64;
    let depth_factor = 1.0 + (depth / 10.0).min(1.0);
    factors.push(format!("depth: {depth:.0}×{depth_factor:.2}"));

    let desc_runes = issue.description.chars().count();
    let desc_factor = 1.0 + ((desc_runes as f64) / 2000.0).min(1.0);
    if desc_runes > 0 {
        factors.push(format!("desc: {desc_runes}r×{desc_factor:.2}"));
    } else {
        factors.push("desc: empty×1.00".to_string());
    }

    let derived =
        truncate_f64_to_i64((base_minutes as f64) * type_weight * depth_factor * desc_factor)
            .unwrap_or(base_minutes);
    (derived.max(1), factors)
}

fn estimate_velocity_minutes_per_day(
    issues: &[Issue],
    issue: &Issue,
    now: DateTime<Utc>,
    median_minutes: i64,
) -> (f64, usize, Vec<String>) {
    let since = now - Duration::days(30);
    if issue.labels.is_empty() {
        let (velocity, samples) =
            velocity_minutes_per_day_for_label(issues, None, since, median_minutes);
        return (
            velocity,
            samples,
            vec![format!("velocity: global ({samples} samples/30d)")],
        );
    }

    let mut best_label = String::new();
    let mut best_velocity = 0.0;
    let mut best_samples = 0usize;

    for label in &issue.labels {
        let (velocity, samples) =
            velocity_minutes_per_day_for_label(issues, Some(label), since, median_minutes);
        if samples == 0 || velocity <= 0.0 {
            continue;
        }

        if best_velocity == 0.0
            || velocity < best_velocity
            || ((velocity - best_velocity).abs() < f64::EPSILON
                && label.to_ascii_lowercase() < best_label.to_ascii_lowercase())
        {
            best_label.clone_from(label);
            best_velocity = velocity;
            best_samples = samples;
        }
    }

    if best_velocity > 0.0 {
        return (
            best_velocity,
            best_samples,
            vec![format!(
                "velocity: label={best_label} ({best_velocity:.0} min/day, {best_samples} samples/30d)"
            )],
        );
    }

    let (velocity, samples) =
        velocity_minutes_per_day_for_label(issues, None, since, median_minutes);
    (
        velocity,
        samples,
        vec![format!("velocity: global ({samples} samples/30d)")],
    )
}

fn velocity_minutes_per_day_for_label(
    issues: &[Issue],
    label: Option<&str>,
    since: DateTime<Utc>,
    median_minutes: i64,
) -> (f64, usize) {
    let mut total_minutes = 0_i64;
    let mut samples = 0usize;

    for issue in issues {
        if !issue.status.eq_ignore_ascii_case("closed") {
            continue;
        }

        let closed_at = issue
            .closed_at
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .or_else(|| issue.updated_at.as_deref().and_then(parse_rfc3339_utc));
        let Some(closed_at) = closed_at else {
            continue;
        };
        if closed_at < since {
            continue;
        }

        if label.is_some_and(|needle| !has_label(&issue.labels, needle)) {
            continue;
        }

        let minutes = i64::from(issue.estimated_minutes.unwrap_or(0)).max(0);
        total_minutes += if minutes > 0 {
            minutes
        } else if median_minutes > 0 {
            median_minutes
        } else {
            DEFAULT_ESTIMATED_MINUTES
        };
        samples = samples.saturating_add(1);
    }

    if samples == 0 {
        (0.0, 0)
    } else {
        (total_minutes as f64 / 30.0, samples)
    }
}

fn has_label(labels: &[String], target: &str) -> bool {
    let target = target.to_ascii_lowercase();
    labels
        .iter()
        .any(|label| label.to_ascii_lowercase() == target)
}

fn estimate_eta_confidence(issue: &Issue, velocity_samples: usize) -> f64 {
    let mut confidence = 0.25_f64;

    if issue.estimated_minutes.unwrap_or(0) > 0 {
        confidence += 0.25;
    }

    confidence += if velocity_samples >= 15 {
        0.30
    } else if velocity_samples >= 5 {
        0.20
    } else if velocity_samples >= 1 {
        0.10
    } else {
        -0.05
    };

    if issue.labels.is_empty() {
        confidence -= 0.05;
    }

    clamp(confidence, 0.10, 0.90)
}

fn compute_median_estimated_minutes(issues: &[Issue]) -> i64 {
    let mut estimates = issues
        .iter()
        .filter_map(|issue| issue.estimated_minutes)
        .map(i64::from)
        .filter(|minutes| *minutes > 0)
        .collect::<Vec<_>>();

    if estimates.is_empty() {
        return DEFAULT_ESTIMATED_MINUTES;
    }

    estimates.sort_unstable();
    let mid = estimates.len() / 2;
    if estimates.len() % 2 == 0 {
        (estimates[mid - 1] + estimates[mid]) / 2
    } else {
        estimates[mid]
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn duration_days(days: f64) -> Duration {
    if days <= 0.0 || !days.is_finite() {
        return Duration::zero();
    }

    let nanos = truncate_f64_to_i64(days * 86_400.0 * 1_000_000_000.0)
        .unwrap_or(i64::MAX)
        .max(0);
    Duration::nanoseconds(nanos)
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn truncate_f64_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }

    if value >= I64_MAX_F64 {
        return Some(i64::MAX);
    }
    if value <= I64_MIN_F64 {
        return Some(i64::MIN);
    }

    value.trunc().to_string().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::analysis::graph::IssueGraph;
    use crate::model::Issue;

    use super::{estimate_eta_for_issue, estimate_forecast};

    #[test]
    fn forecast_for_all_open_issues() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(90),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(30),
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let output = estimate_forecast(&issues, &graph, &metrics, "all", None, 1);
        assert_eq!(output.summary.count, 2);
        assert_eq!(output.forecasts[0].id, "A");
        assert_eq!(output.forecasts[1].id, "B");
        assert!(output.forecasts[0].estimated_days >= 0.0);
    }

    #[test]
    fn eta_includes_bounds_and_normalizes_agents() {
        let issues = vec![Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let eta = estimate_eta_for_issue(&issues, &graph, &metrics, "A", 0, Utc::now())
            .expect("eta should be computed");

        assert_eq!(eta.agents, 1);
        assert!(!eta.eta_date.is_empty());
        assert!(!eta.eta_date_low.is_empty());
        assert!(!eta.eta_date_high.is_empty());
    }
}
