use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, PartialEq)]
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
    let mut context = ForecastContext::new(issues, graph, metrics, agents, now);
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

        let Some(eta) = context.estimate(&issue.id) else {
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
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    issue_id: &str,
    agents: usize,
    now: DateTime<Utc>,
) -> Option<EtaEstimate> {
    ForecastContext::new(issues, graph, metrics, agents, now).estimate(issue_id)
}

/// Estimate each issue's own effort without evaluating dependency wait times or dates.
///
/// The median comes from `issues`, while complexity uses the supplied metrics. This
/// lets label-scoped capacity reports retain their existing full-graph depth model.
/// Exact duplicate IDs use the first record, matching [`estimate_eta_for_issue`].
#[must_use]
pub fn estimate_effort_minutes(issues: &[Issue], metrics: &GraphMetrics) -> HashMap<String, i64> {
    let median_minutes = compute_median_estimated_minutes(issues);
    let mut estimates = HashMap::with_capacity(issues.len());
    for issue in issues {
        estimates
            .entry(issue.id.clone())
            .or_insert_with(|| estimate_complexity_minutes(issue, metrics, median_minutes).0);
    }
    estimates
}

/// One immutable source snapshot, clock, and agent count for an entire forecast.
/// Cache only estimates independent of the current traversal's cycle cuts.
struct ForecastContext<'a> {
    nodes: Vec<ForecastNode<'a>>,
    indices: HashMap<&'a str, usize>,
    metrics: &'a GraphMetrics,
    median_minutes: i64,
    velocity: VelocityHistory,
    agents: usize,
    now: DateTime<Utc>,
    active: Vec<bool>,
}

struct ForecastNode<'a> {
    issue: &'a Issue,
    blockers: Vec<usize>,
    base: Option<BaseEstimate>,
    estimate: Option<EtaEstimate>,
}

struct BaseEstimate {
    minutes: i64,
    days: f64,
    confidence: f64,
    velocity_per_day: f64,
    factors: Vec<String>,
}

struct EstimateFrame {
    node: usize,
    next_blocker: usize,
    blocker_wait_days: f64,
    depends_on_cycle_cut: bool,
}

impl EstimateFrame {
    const fn new(node: usize) -> Self {
        Self {
            node,
            next_blocker: 0,
            blocker_wait_days: 0.0,
            depends_on_cycle_cut: false,
        }
    }
}

impl<'a> ForecastContext<'a> {
    fn new(
        issues: &'a [Issue],
        graph: &IssueGraph,
        metrics: &'a GraphMetrics,
        agents: usize,
        now: DateTime<Utc>,
    ) -> Self {
        let median_minutes = compute_median_estimated_minutes(issues);
        let velocity = VelocityHistory::new(issues, now, median_minutes);
        let mut nodes = Vec::with_capacity(issues.len());
        let mut indices = HashMap::with_capacity(issues.len());
        for issue in issues {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                indices.entry(issue.id.as_str())
            {
                entry.insert(nodes.len());
                nodes.push(ForecastNode {
                    issue,
                    blockers: Vec::new(),
                    base: None,
                    estimate: None,
                });
            }
        }
        for node in &mut nodes {
            node.blockers = graph
                .open_blockers(&node.issue.id)
                .into_iter()
                .filter_map(|id| indices.get(id.as_str()).copied())
                .collect();
        }
        let active = vec![false; nodes.len()];
        Self {
            nodes,
            indices,
            metrics,
            median_minutes,
            velocity,
            agents: agents.max(1),
            now,
            active,
        }
    }

    fn estimate(&mut self, issue_id: &str) -> Option<EtaEstimate> {
        let &root = self.indices.get(issue_id)?;
        if let Some(estimate) = &self.nodes[root].estimate {
            return Some(estimate.clone());
        }

        self.active[root] = true;
        let mut stack = vec![EstimateFrame::new(root)];
        while let Some(frame) = stack.last_mut() {
            if let Some(&blocker) = self.nodes[frame.node].blockers.get(frame.next_blocker) {
                frame.next_blocker += 1;
                if self.active[blocker] {
                    // Match the previous per-root recursion guard exactly. A
                    // cut affects this frame and every ancestor, even when its
                    // branch does not supply the maximum wait time.
                    frame.depends_on_cycle_cut = true;
                } else if let Some(estimate) = &self.nodes[blocker].estimate {
                    frame.blocker_wait_days = frame.blocker_wait_days.max(estimate.estimated_days);
                } else {
                    self.active[blocker] = true;
                    stack.push(EstimateFrame::new(blocker));
                }
                continue;
            }

            let completed = stack.pop()?;
            self.active[completed.node] = false;
            let estimate = self.finish_estimate(completed.node, completed.blocker_wait_days);
            let days = estimate.estimated_days;
            if stack.is_empty() {
                if !completed.depends_on_cycle_cut {
                    self.nodes[completed.node].estimate = Some(estimate.clone());
                }
                return Some(estimate);
            }
            if !completed.depends_on_cycle_cut {
                self.nodes[completed.node].estimate = Some(estimate);
            }
            if let Some(parent) = stack.last_mut() {
                parent.blocker_wait_days = parent.blocker_wait_days.max(days);
                parent.depends_on_cycle_cut |= completed.depends_on_cycle_cut;
            }
        }
        None
    }

    fn finish_estimate(&mut self, node_index: usize, blocker_wait_days: f64) -> EtaEstimate {
        let node = &mut self.nodes[node_index];
        let base = node.base.get_or_insert_with(|| {
            let (minutes, mut factors) =
                estimate_complexity_minutes(node.issue, self.metrics, self.median_minutes);
            let (mut velocity_per_day, samples, velocity_factors) =
                self.velocity.estimate(node.issue);
            factors.extend(velocity_factors);
            if velocity_per_day <= 0.0 {
                velocity_per_day = (self.median_minutes as f64) / 5.0;
                if velocity_per_day <= 0.0 {
                    velocity_per_day = 60.0;
                }
                factors.push("velocity: no recent closures; using default".to_string());
            }
            let capacity_per_day = velocity_per_day * (self.agents as f64);
            let mut days = if capacity_per_day > 0.0 {
                (minutes as f64) / capacity_per_day
            } else {
                0.0
            };
            if days.is_sign_negative() {
                days = 0.0;
            }
            BaseEstimate {
                minutes,
                days,
                confidence: estimate_eta_confidence(node.issue, samples),
                velocity_per_day,
                factors,
            }
        });

        let mut days = base.days;
        let mut factors = base.factors.clone();
        if blocker_wait_days > 0.0 {
            days += blocker_wait_days;
            factors.push(format!(
                "blocked: waits {blocker_wait_days:.1}d on dependencies"
            ));
        }
        let delta_days = 0.5_f64.max(days * (1.0 - base.confidence) * 0.8);
        let eta = self.now + duration_days(days);
        let eta_low = self.now + duration_days((days - delta_days).max(0.0));
        let eta_high = self.now + duration_days(days + delta_days);
        factors.push(format!("agents: {}", self.agents));
        factors.truncate(8);
        EtaEstimate {
            estimated_minutes: base.minutes,
            estimated_days: days,
            eta_date: eta.to_rfc3339(),
            eta_date_low: eta_low.to_rfc3339(),
            eta_date_high: eta_high.to_rfc3339(),
            confidence: base.confidence,
            velocity_minutes_per_day: base.velocity_per_day,
            agents: self.agents,
            factors,
        }
    }
}

#[derive(Default)]
struct VelocityTotals {
    minutes: i64,
    samples: usize,
}

impl VelocityTotals {
    fn add(&mut self, minutes: i64) {
        self.minutes += minutes;
        self.samples = self.samples.saturating_add(1);
    }

    fn per_day(&self) -> f64 {
        self.minutes as f64 / 30.0
    }
}

struct VelocityHistory {
    global: VelocityTotals,
    labels: HashMap<String, VelocityTotals>,
}

impl VelocityHistory {
    fn new(issues: &[Issue], now: DateTime<Utc>, median_minutes: i64) -> Self {
        let since = now - Duration::days(30);
        let mut history = Self {
            global: VelocityTotals::default(),
            labels: HashMap::new(),
        };
        for issue in issues {
            if !issue.is_closed_like()
                || !issue
                    .closed_at
                    .or(issue.updated_at)
                    .is_some_and(|at| at >= since)
            {
                continue;
            }
            let explicit = i64::from(issue.estimated_minutes.unwrap_or(0)).max(0);
            let minutes = if explicit > 0 {
                explicit
            } else if median_minutes > 0 {
                median_minutes
            } else {
                DEFAULT_ESTIMATED_MINUTES
            };
            history.global.add(minutes);
            // A closure contributes once per label, including records that
            // repeat a label with another ASCII case.
            let labels: HashSet<String> = issue
                .labels
                .iter()
                .map(|label| label.to_ascii_lowercase())
                .collect();
            for label in labels {
                history.labels.entry(label).or_default().add(minutes);
            }
        }
        history
    }

    fn estimate(&self, issue: &Issue) -> (f64, usize, Vec<String>) {
        let mut slowest: Option<(&str, &VelocityTotals)> = None;
        for label in &issue.labels {
            let Some(totals) = self.labels.get(&label.to_ascii_lowercase()) else {
                continue;
            };
            let velocity = totals.per_day();
            if totals.samples == 0 || velocity <= 0.0 {
                continue;
            }
            if slowest.is_none_or(|(previous_label, previous)| {
                velocity < previous.per_day()
                    || ((velocity - previous.per_day()).abs() < f64::EPSILON
                        && label.to_ascii_lowercase() < previous_label.to_ascii_lowercase())
            }) {
                slowest = Some((label, totals));
            }
        }
        if let Some((label, totals)) = slowest {
            let velocity = totals.per_day();
            return (
                velocity,
                totals.samples,
                vec![format!(
                    "velocity: label={label} ({velocity:.0} min/day, {} samples/30d)",
                    totals.samples
                )],
            );
        }
        (
            self.global.per_day(),
            self.global.samples,
            vec![format!(
                "velocity: global ({} samples/30d)",
                self.global.samples
            )],
        )
    }
}

// Retain the previous evaluator only as an independent small-graph regression
// oracle. Production traversal above never recurses or rescans closure history.
#[cfg(test)]
fn estimate_eta_reference(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    issue_id: &str,
    agents: usize,
    now: DateTime<Utc>,
    active: &mut HashSet<String>,
) -> Option<EtaEstimate> {
    if !active.insert(issue_id.to_string()) {
        return None;
    }

    let Some(issue) = issues.iter().find(|issue| issue.id == issue_id) else {
        active.remove(issue_id);
        return None;
    };
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

    let blocker_wait_days = graph
        .open_blockers(issue_id)
        .into_iter()
        .filter_map(|blocker_id| {
            estimate_eta_reference(issues, graph, metrics, &blocker_id, agents, now, active)
        })
        .map(|eta| eta.estimated_days)
        .fold(0.0_f64, f64::max);
    if blocker_wait_days > 0.0 {
        estimated_days += blocker_wait_days;
        factors.push(format!(
            "blocked: waits {:.1}d on dependencies",
            blocker_wait_days
        ));
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

    let estimate = EtaEstimate {
        estimated_minutes: complexity_minutes,
        estimated_days,
        eta_date: eta.to_rfc3339(),
        eta_date_low: eta_low.to_rfc3339(),
        eta_date_high: eta_high.to_rfc3339(),
        confidence,
        velocity_minutes_per_day: velocity_per_day,
        agents,
        factors,
    };
    active.remove(issue_id);
    Some(estimate)
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

#[cfg(test)]
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

    // Pick the most conservative (lowest) label velocity for pessimistic ETA estimation.
    // Using the slowest label avoids overoptimistic forecasts.
    let mut slowest_label = String::new();
    let mut slowest_velocity = 0.0;
    let mut slowest_samples = 0usize;

    for label in &issue.labels {
        let (velocity, samples) =
            velocity_minutes_per_day_for_label(issues, Some(label), since, median_minutes);
        if samples == 0 || velocity <= 0.0 {
            continue;
        }

        if slowest_velocity == 0.0
            || velocity < slowest_velocity
            || ((velocity - slowest_velocity).abs() < f64::EPSILON
                && label.to_ascii_lowercase() < slowest_label.to_ascii_lowercase())
        {
            slowest_label.clone_from(label);
            slowest_velocity = velocity;
            slowest_samples = samples;
        }
    }

    if slowest_velocity > 0.0 {
        return (
            slowest_velocity,
            slowest_samples,
            vec![format!(
                "velocity: label={slowest_label} ({slowest_velocity:.0} min/day, {slowest_samples} samples/30d)"
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

#[cfg(test)]
fn velocity_minutes_per_day_for_label(
    issues: &[Issue],
    label: Option<&str>,
    since: DateTime<Utc>,
    median_minutes: i64,
) -> (f64, usize) {
    let mut total_minutes = 0_i64;
    let mut samples = 0usize;

    for issue in issues {
        if !issue.is_closed_like() {
            continue;
        }

        let closed_at = issue.closed_at.or(issue.updated_at);
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

fn duration_days(days: f64) -> Duration {
    if days <= 0.0 || !days.is_finite() {
        return Duration::zero();
    }

    const NANOS_PER_DAY: f64 = 86_400.0 * 1_000_000_000.0;
    let nanos = truncate_f64_to_i64(days * NANOS_PER_DAY)
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

    #[allow(clippy::cast_possible_truncation)]
    Some(value.trunc() as i64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::{DateTime, Utc};

    use crate::analysis::graph::{GraphMetrics, IssueGraph};
    use crate::model::{Dependency, Issue};

    use super::{
        ForecastContext, VelocityHistory, estimate_effort_minutes, estimate_eta_for_issue,
        estimate_eta_reference, estimate_forecast, velocity_minutes_per_day_for_label,
    };

    fn fixed_clock() -> DateTime<Utc> {
        "2026-01-20T12:34:56Z".parse().expect("fixed clock")
    }

    fn dependency_issue(id: &str, blockers: &[&str]) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            estimated_minutes: Some(60),
            dependencies: blockers
                .iter()
                .map(|blocker| Dependency {
                    issue_id: id.to_string(),
                    depends_on_id: (*blocker).to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                })
                .collect(),
            ..Issue::default()
        }
    }

    fn assert_matches_reference(
        issues: &[Issue],
        graph: &IssueGraph,
        metrics: &GraphMetrics,
        order: &[&str],
        agents: usize,
    ) {
        let now = fixed_clock();
        let mut context = ForecastContext::new(issues, graph, metrics, agents, now);
        // Reuse the same context in both directions. A cycle-truncated result
        // must never contaminate a later root or a repeated request.
        for &id in order.iter().chain(order.iter().rev()) {
            let expected = estimate_eta_reference(
                issues,
                graph,
                metrics,
                id,
                agents,
                now,
                &mut HashSet::new(),
            );
            assert_eq!(context.estimate(id), expected, "forecast for {id}");
        }
    }

    #[test]
    fn iterative_forecasts_match_recursive_oracle_on_generated_dags() {
        for case in 0_u64..128 {
            let mut issues = Vec::new();
            for node in 0..8 {
                let id = format!("N{node}");
                let blockers = (0..node)
                    .filter(|&earlier| {
                        case.wrapping_mul(0x9e37_79b9_7f4a_7c15)
                            .rotate_left((node * 8 + earlier) as u32)
                            & 1
                            != 0
                    })
                    .map(|earlier| format!("N{earlier}"))
                    .collect::<Vec<_>>();
                let blocker_refs = blockers.iter().map(String::as_str).collect::<Vec<_>>();
                let mut issue = dependency_issue(&id, &blocker_refs);
                issue.estimated_minutes =
                    [None, Some(0), Some(-10), Some(30), Some(240)][(node + case as usize) % 5];
                issue.issue_type = ["task", " Feature ", "epic", "chore"][node % 4].to_string();
                issue.description = "évidence ".repeat(node * 43);
                issue.labels = match node % 4 {
                    0 => vec!["API".to_string(), "api".to_string()],
                    1 => vec!["Web".to_string(), "API".to_string()],
                    2 => vec!["unseen".to_string()],
                    _ => Vec::new(),
                };
                if node % 4 == 0 {
                    issue.status = "closed".to_string();
                    issue.closed_at = Some(fixed_clock() - chrono::Duration::days(3));
                }
                issues.push(issue);
            }
            let graph = IssueGraph::build(&issues);
            let metrics = graph.compute_metrics();
            assert_matches_reference(
                &issues,
                &graph,
                &metrics,
                &["N7", "N2", "N5", "N0", "N3", "N6", "N1", "N4", "missing"],
                case as usize % 4,
            );
        }
    }

    #[test]
    fn shared_dependency_diamonds_are_evaluated_without_enumerating_paths() {
        // Sixty-five issues contain over four billion root-to-leaf paths.
        // Each level adds exactly one issue's five-day effort to every ETA.
        let levels = 32;
        let mut issues = vec![dependency_issue("ROOT", &[])];
        let mut previous = vec!["ROOT".to_string()];
        for level in 0..levels {
            let next = [format!("L{level:02}A"), format!("L{level:02}B")];
            let blockers = previous.iter().map(String::as_str).collect::<Vec<_>>();
            for id in &next {
                issues.push(dependency_issue(id, &blockers));
            }
            previous = next.to_vec();
        }
        let graph = IssueGraph::build(&issues);
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let eta = estimate_eta_for_issue(&issues, &graph, &metrics, "L31B", 1, fixed_clock())
            .expect("diamond target");
        assert_eq!(eta.estimated_days, 5.0 * (levels + 1) as f64);
        assert_eq!(eta.estimated_minutes, 60);

        let forecasts = estimate_forecast(&issues, &graph, &metrics, "all", None, 1);
        assert_eq!(forecasts.forecasts.len(), issues.len());
        for (index, item) in forecasts.forecasts.iter().enumerate() {
            assert_eq!(item.id, issues[index].id);
            assert_eq!(item.estimated_days, 5.0 * ((index + 1) / 2 + 1) as f64);
            assert_eq!(item.eta_minutes, 60);
        }
    }

    #[test]
    fn deep_forecast_chain_does_not_use_the_process_stack() {
        let count = 25_000;
        let mut issues = Vec::with_capacity(count);
        issues.push(dependency_issue("N00000", &[]));
        for index in 1..count {
            issues.push(dependency_issue(
                &format!("N{index:05}"),
                &[&format!("N{:05}", index - 1)],
            ));
        }
        let graph = IssueGraph::build(&issues);
        // Exercise forecasting rather than unrelated centrality algorithms.
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let eta = estimate_eta_for_issue(&issues, &graph, &metrics, "N24999", 1, fixed_clock())
            .expect("deep chain target");
        assert_eq!(eta.estimated_minutes, 60);
        assert_eq!(eta.estimated_days, count as f64 * 5.0);
        assert!(
            eta.factors
                .iter()
                .any(|factor| factor == "blocked: waits 124995.0d on dependencies")
        );
    }

    #[test]
    fn cycle_cuts_do_not_leak_between_roots_or_acyclic_ancestors() {
        let cases = [
            vec![
                dependency_issue("A", &["B"]),
                dependency_issue("B", &["A"]),
                dependency_issue("C", &["A", "FREE"]),
                dependency_issue("D", &["B", "C"]),
                dependency_issue("FREE", &[]),
            ],
            vec![
                dependency_issue("A", &["B", "C"]),
                dependency_issue("B", &["A", "C", "FREE"]),
                dependency_issue("C", &["A", "B"]),
                dependency_issue("D", &["A", "C", "FREE"]),
                dependency_issue("FREE", &[]),
            ],
            vec![
                dependency_issue("A", &["A", "FREE"]),
                dependency_issue("B", &["A"]),
                dependency_issue("C", &["B", "FREE"]),
                dependency_issue("D", &["C"]),
                dependency_issue("FREE", &[]),
            ],
        ];
        for mut issues in cases {
            for (index, issue) in issues.iter_mut().enumerate() {
                issue.estimated_minutes = Some((index as i32 + 1) * 30);
            }
            let graph = IssueGraph::build(&issues);
            let metrics = graph.compute_metrics();
            for order in [
                ["D", "C", "B", "A", "FREE"],
                ["A", "B", "D", "FREE", "C"],
                ["FREE", "C", "A", "B", "D"],
            ] {
                assert_matches_reference(&issues, &graph, &metrics, &order, 3);
            }
        }
    }

    #[test]
    fn cached_forecasts_match_oracle_for_every_three_node_directed_graph() {
        let metrics = IssueGraph::build(&[]).compute_metrics();
        let ids = ["A", "B", "C"];
        // All 512 graphs, including self-loops and every overlapping cycle.
        for mask in 0..(1 << 9) {
            let issues = ids
                .iter()
                .enumerate()
                .map(|(node, id)| {
                    let blockers = ids
                        .iter()
                        .enumerate()
                        .filter_map(|(blocker, id)| {
                            (mask & (1 << (node * 3 + blocker)) != 0).then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    let mut issue = dependency_issue(id, &blockers);
                    issue.estimated_minutes = Some((node as i32 + 1) * 30);
                    issue
                })
                .collect::<Vec<_>>();
            let graph = IssueGraph::build(&issues);
            assert_matches_reference(&issues, &graph, &metrics, &["A", "B", "C"], 1);
            assert_matches_reference(&issues, &graph, &metrics, &["C", "A", "B"], 2);
        }
    }

    #[test]
    fn indexed_lookup_preserves_missing_closed_and_duplicate_id_behavior() {
        let mut issues = vec![
            dependency_issue("A", &[]),
            dependency_issue("A", &[]),
            dependency_issue("CLOSED", &[]),
            dependency_issue("TARGET", &["A", "CLOSED", "OUTSIDE"]),
            dependency_issue("OUTSIDE", &[]),
        ];
        issues[1].estimated_minutes = Some(600);
        issues[2].status = "tombstone".to_string();
        issues[2].updated_at = Some(fixed_clock());
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        // OUTSIDE exists in the graph but not in this caller's source slice.
        let selected = &issues[..4];
        assert_matches_reference(
            selected,
            &graph,
            &metrics,
            &["TARGET", "A", "CLOSED", "OUTSIDE", "missing"],
            1,
        );
        let efforts = estimate_effort_minutes(selected, &metrics);
        let first = estimate_eta_for_issue(selected, &graph, &metrics, "A", 1, fixed_clock())
            .expect("first duplicate issue");
        assert_eq!(efforts["A"], first.estimated_minutes);
        assert!(efforts["A"] < 600);
        assert!(!efforts.contains_key("OUTSIDE"));
    }

    #[test]
    fn indexed_velocity_preserves_closure_boundaries_label_case_and_fallbacks() {
        let now = fixed_clock();
        let mut issues = Vec::new();
        for (id, status, labels, age, minutes) in [
            ("one", "closed", vec!["API", "api", "Web"], 2, Some(120)),
            ("two", "tombstone", vec!["WEB", "Api"], 30, None),
            ("three", "closed", vec!["legacy"], 31, Some(900)),
            ("four", "open", vec!["API"], 1, Some(1200)),
            ("five", "closed", vec![" Api "], -1, Some(240)),
        ] {
            let mut issue = dependency_issue(id, &[]);
            issue.status = status.to_string();
            issue.labels = labels.into_iter().map(str::to_string).collect();
            issue.closed_at = Some(now - chrono::Duration::days(age));
            issue.estimated_minutes = minutes;
            issues.push(issue);
        }
        // updated_at is used only when closed_at is absent.
        issues[1].updated_at = issues[1].closed_at.take();
        let median = super::compute_median_estimated_minutes(&issues);
        let indexed = VelocityHistory::new(&issues, now, median);
        for labels in [
            vec![],
            vec!["api"],
            vec!["Web", "API"],
            vec!["WEB", "api", "Api"],
            vec![" Api "],
            vec!["absent", "legacy"],
        ] {
            let mut target = dependency_issue("target", &[]);
            target.labels = labels.into_iter().map(str::to_string).collect();
            assert_eq!(
                indexed.estimate(&target),
                super::estimate_velocity_minutes_per_day(&issues, &target, now, median),
                "labels {:?}",
                target.labels
            );
        }
        assert_eq!(indexed.labels["api"].samples, 2);
        assert_eq!(indexed.labels["web"].samples, 2);
        assert_eq!(indexed.global.samples, 3);
    }

    #[test]
    fn effort_batch_uses_selected_median_and_supplied_graph_depth() {
        let mut issues = vec![
            dependency_issue("SMALL", &[]),
            dependency_issue("LARGE", &[]),
            dependency_issue("UNESTIMATED", &["SMALL"]),
        ];
        issues[0].estimated_minutes = Some(30);
        issues[1].estimated_minutes = Some(90);
        issues[2].estimated_minutes = None;
        let mut metrics = IssueGraph::build(&[]).compute_metrics();
        metrics.critical_depth.insert("UNESTIMATED".to_string(), 4);
        let estimates = estimate_effort_minutes(&issues, &metrics);
        assert_eq!(estimates["SMALL"], 30);
        assert_eq!(estimates["LARGE"], 90);
        assert_eq!(estimates["UNESTIMATED"], 84);
        assert_eq!(
            estimate_effort_minutes(&issues[1..], &metrics)["UNESTIMATED"],
            125
        );
        assert!(estimate_effort_minutes(&[], &metrics).is_empty());
    }

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

    #[test]
    fn velocity_counts_tombstone_as_closed_like() {
        let now = Utc::now();
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(120),
                closed_at: Some(now - chrono::Duration::days(1)),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "tombstone".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(60),
                closed_at: Some(now - chrono::Duration::days(2)),
                ..Issue::default()
            },
        ];

        let (velocity, samples) =
            velocity_minutes_per_day_for_label(&issues, None, now - chrono::Duration::days(30), 60);

        assert_eq!(samples, 2);
        assert!((velocity - 6.0).abs() < 0.001);
    }

    // ── compute_median_estimated_minutes ─────────────────────────────

    #[test]
    fn median_odd_count() {
        let issues = vec![
            Issue {
                estimated_minutes: Some(30),
                ..Issue::default()
            },
            Issue {
                estimated_minutes: Some(60),
                ..Issue::default()
            },
            Issue {
                estimated_minutes: Some(120),
                ..Issue::default()
            },
        ];
        assert_eq!(super::compute_median_estimated_minutes(&issues), 60);
    }

    #[test]
    fn median_even_count() {
        let issues = vec![
            Issue {
                estimated_minutes: Some(30),
                ..Issue::default()
            },
            Issue {
                estimated_minutes: Some(90),
                ..Issue::default()
            },
        ];
        // (30 + 90) / 2 = 60
        assert_eq!(super::compute_median_estimated_minutes(&issues), 60);
    }

    #[test]
    fn median_empty_returns_default() {
        assert_eq!(
            super::compute_median_estimated_minutes(&[]),
            super::DEFAULT_ESTIMATED_MINUTES
        );
    }

    #[test]
    fn median_filters_zero_and_none() {
        let issues = vec![
            Issue {
                estimated_minutes: Some(0),
                ..Issue::default()
            },
            Issue {
                estimated_minutes: None,
                ..Issue::default()
            },
            Issue {
                estimated_minutes: Some(120),
                ..Issue::default()
            },
        ];
        assert_eq!(super::compute_median_estimated_minutes(&issues), 120);
    }

    // ── estimate_complexity_minutes ──────────────────────────────────

    #[test]
    fn complexity_uses_explicit_estimate() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let issue = Issue {
            id: "A".to_string(),
            estimated_minutes: Some(120),
            issue_type: "task".to_string(),
            ..Issue::default()
        };
        let (minutes, factors) = super::estimate_complexity_minutes(&issue, &metrics, 60);
        // task type_weight=1.0, depth=0 → depth_factor=1.0, empty desc → desc_factor=1.0
        // 120 * 1.0 * 1.0 * 1.0 = 120
        assert_eq!(minutes, 120);
        assert!(factors.iter().any(|f| f.contains("explicit")));
    }

    #[test]
    fn complexity_uses_median_fallback() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let issue = Issue {
            id: "A".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        };
        let (minutes, factors) = super::estimate_complexity_minutes(&issue, &metrics, 90);
        // No explicit estimate → uses median=90, task×1.0, depth=0, empty desc
        assert_eq!(minutes, 90);
        assert!(factors.iter().any(|f| f.contains("median")));
    }

    #[test]
    fn complexity_type_weight_feature() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let issue = Issue {
            id: "A".to_string(),
            estimated_minutes: Some(100),
            issue_type: "feature".to_string(),
            ..Issue::default()
        };
        let (minutes, _) = super::estimate_complexity_minutes(&issue, &metrics, 60);
        // 100 * 1.3 (feature) * 1.0 * 1.0 = 130
        assert_eq!(minutes, 130);
    }

    #[test]
    fn complexity_type_weight_epic() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let issue = Issue {
            id: "A".to_string(),
            estimated_minutes: Some(100),
            issue_type: "epic".to_string(),
            ..Issue::default()
        };
        let (minutes, _) = super::estimate_complexity_minutes(&issue, &metrics, 60);
        // 100 * 2.0 (epic) * 1.0 * 1.0 = 200
        assert_eq!(minutes, 200);
    }

    #[test]
    fn complexity_description_scales_estimate() {
        let graph = IssueGraph::build(&[]);
        let metrics = graph.compute_metrics();
        let long_desc = "x".repeat(2000);
        let issue = Issue {
            id: "A".to_string(),
            estimated_minutes: Some(100),
            issue_type: "task".to_string(),
            description: long_desc,
            ..Issue::default()
        };
        let (minutes, _) = super::estimate_complexity_minutes(&issue, &metrics, 60);
        // 100 * 1.0 * 1.0 * (1.0 + 2000/2000) = 100 * 2.0 = 200
        assert_eq!(minutes, 200);
    }

    // ── estimate_eta_confidence ──────────────────────────────────────

    #[test]
    fn confidence_base_no_estimate_no_velocity() {
        let issue = Issue::default();
        let confidence = super::estimate_eta_confidence(&issue, 0);
        // 0.25 (base) + (-0.05) (no velocity) + (-0.05) (no labels) = 0.15
        assert!((confidence - 0.15).abs() < 0.01);
    }

    #[test]
    fn confidence_with_explicit_estimate() {
        let issue = Issue {
            estimated_minutes: Some(60),
            ..Issue::default()
        };
        let confidence = super::estimate_eta_confidence(&issue, 0);
        // 0.25 + 0.25 (estimate) + (-0.05) (no velocity) + (-0.05) (no labels) = 0.40
        assert!((confidence - 0.40).abs() < 0.01);
    }

    #[test]
    fn confidence_high_velocity_samples() {
        let issue = Issue {
            estimated_minutes: Some(60),
            labels: vec!["backend".to_string()],
            ..Issue::default()
        };
        let confidence = super::estimate_eta_confidence(&issue, 20);
        // 0.25 + 0.25 (estimate) + 0.30 (>=15 samples) + 0.0 (has labels) = 0.80
        assert!((confidence - 0.80).abs() < 0.01);
    }

    // ── velocity with label filter ──────────────────────────────────

    #[test]
    fn velocity_label_filter() {
        let now = Utc::now();
        let issues = vec![
            Issue {
                id: "A".to_string(),
                status: "closed".to_string(),
                labels: vec!["backend".to_string()],
                estimated_minutes: Some(120),
                closed_at: Some(now - chrono::Duration::days(5)),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                status: "closed".to_string(),
                labels: vec!["frontend".to_string()],
                estimated_minutes: Some(60),
                closed_at: Some(now - chrono::Duration::days(3)),
                ..Issue::default()
            },
        ];

        let (vel_backend, samples_backend) = velocity_minutes_per_day_for_label(
            &issues,
            Some("backend"),
            now - chrono::Duration::days(30),
            60,
        );
        assert_eq!(samples_backend, 1);
        assert!((vel_backend - 4.0).abs() < 0.01); // 120/30

        let (vel_frontend, samples_frontend) = velocity_minutes_per_day_for_label(
            &issues,
            Some("frontend"),
            now - chrono::Duration::days(30),
            60,
        );
        assert_eq!(samples_frontend, 1);
        assert!((vel_frontend - 2.0).abs() < 0.01); // 60/30
    }

    #[test]
    fn velocity_ignores_old_closures() {
        let now = Utc::now();
        let issues = vec![Issue {
            id: "A".to_string(),
            status: "closed".to_string(),
            estimated_minutes: Some(120),
            closed_at: Some(now - chrono::Duration::days(60)),
            ..Issue::default()
        }];
        let (velocity, samples) =
            velocity_minutes_per_day_for_label(&issues, None, now - chrono::Duration::days(30), 60);
        assert_eq!(samples, 0);
        assert_eq!(velocity, 0.0);
    }

    // ── agents scaling ──────────────────────────────────────────────

    #[test]
    fn more_agents_reduces_eta() {
        let issues = vec![Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            estimated_minutes: Some(240),
            ..Issue::default()
        }];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let now = Utc::now();

        let eta1 = estimate_eta_for_issue(&issues, &graph, &metrics, "A", 1, now).unwrap();
        let eta3 = estimate_eta_for_issue(&issues, &graph, &metrics, "A", 3, now).unwrap();
        assert!(
            eta3.estimated_days < eta1.estimated_days || eta1.estimated_days == 0.0,
            "3 agents should complete faster than 1"
        );
    }

    #[test]
    fn blocked_issue_eta_includes_blocker_wait_time() {
        let issues = vec![
            Issue {
                id: "BLOCKER".to_string(),
                title: "Blocker".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(240),
                ..Issue::default()
            },
            Issue {
                id: "BLOCKED".to_string(),
                title: "Blocked".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                estimated_minutes: Some(60),
                dependencies: vec![crate::model::Dependency {
                    issue_id: "BLOCKED".to_string(),
                    depends_on_id: "BLOCKER".to_string(),
                    dep_type: "blocks".to_string(),
                    ..crate::model::Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let now = Utc::now();

        let blocker_eta =
            estimate_eta_for_issue(&issues, &graph, &metrics, "BLOCKER", 1, now).unwrap();
        let dependent_eta =
            estimate_eta_for_issue(&issues, &graph, &metrics, "BLOCKED", 1, now).unwrap();

        assert!(
            dependent_eta.estimated_days > blocker_eta.estimated_days,
            "blocked work should include at least the blocker wait plus its own work"
        );
        assert!(
            dependent_eta
                .factors
                .iter()
                .any(|factor| factor.contains("blocked: waits")),
            "forecast factors should explain blocker wait time"
        );
    }

    // ── forecast label filter ───────────────────────────────────────

    #[test]
    fn forecast_respects_label_filter() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                labels: vec!["backend".to_string()],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                labels: vec!["frontend".to_string()],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let output = estimate_forecast(&issues, &graph, &metrics, "all", Some("backend"), 1);
        assert_eq!(output.summary.count, 1);
        assert_eq!(output.forecasts[0].id, "A");
    }

    // ── edge case helpers ───────────────────────────────────────────

    #[test]
    fn duration_days_handles_zero_and_negative() {
        assert_eq!(super::duration_days(0.0), chrono::Duration::zero());
        assert_eq!(super::duration_days(-5.0), chrono::Duration::zero());
    }

    #[test]
    fn truncate_f64_to_i64_edge_cases() {
        assert_eq!(super::truncate_f64_to_i64(f64::NAN), None);
        assert_eq!(super::truncate_f64_to_i64(f64::INFINITY), None);
        assert_eq!(super::truncate_f64_to_i64(42.9), Some(42));
        assert_eq!(super::truncate_f64_to_i64(-3.7), Some(-3));
        assert_eq!(super::truncate_f64_to_i64(1e19), Some(i64::MAX));
        assert_eq!(super::truncate_f64_to_i64(-1e19), Some(i64::MIN));
    }

    #[test]
    fn clamp_works_correctly() {
        assert_eq!(super::clamp(0.5, 0.1, 0.9), 0.5);
        assert_eq!(super::clamp(-0.5, 0.1, 0.9), 0.1);
        assert_eq!(super::clamp(1.5, 0.1, 0.9), 0.9);
    }

    #[test]
    fn forecast_single_issue_by_id() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let output = estimate_forecast(&issues, &graph, &metrics, "B", None, 1);
        assert_eq!(output.summary.count, 1);
        assert_eq!(output.forecasts[0].id, "B");
    }
}
