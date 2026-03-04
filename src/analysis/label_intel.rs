use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::Issue;

use super::graph::{GraphMetrics, IssueGraph};

// ============================================================================
// Constants
// ============================================================================

const DEFAULT_STALE_THRESHOLD_DAYS: i64 = 14;
const HEALTHY_THRESHOLD: i32 = 70;
const WARNING_THRESHOLD: i32 = 40;
const VELOCITY_WEIGHT: f64 = 0.25;
const FRESHNESS_WEIGHT: f64 = 0.25;
const FLOW_WEIGHT: f64 = 0.25;
const CRITICALITY_WEIGHT: f64 = 0.25;

// ============================================================================
// Label Health Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct VelocityMetrics {
    pub closed_last_7_days: i32,
    pub closed_last_30_days: i32,
    pub avg_days_to_close: f64,
    pub trend_direction: String,
    pub trend_percent: f64,
    pub velocity_score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FreshnessMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_recent_update: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_open_issue: Option<DateTime<Utc>>,
    pub avg_days_since_update: f64,
    pub stale_count: i32,
    pub stale_threshold_days: i64,
    pub freshness_score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowMetrics {
    pub incoming_deps: i32,
    pub outgoing_deps: i32,
    pub incoming_labels: Vec<String>,
    pub outgoing_labels: Vec<String>,
    pub blocked_by_external: i32,
    pub blocking_external: i32,
    pub flow_score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CriticalityMetrics {
    pub avg_pagerank: f64,
    pub avg_betweenness: f64,
    pub max_betweenness: f64,
    pub critical_path_count: i32,
    pub bottleneck_count: i32,
    pub criticality_score: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelHealth {
    pub label: String,
    pub issue_count: usize,
    pub open_count: usize,
    pub closed_count: usize,
    pub blocked_count: usize,
    pub health: i32,
    pub health_level: String,
    pub velocity: VelocityMetrics,
    pub freshness: FreshnessMetrics,
    pub flow: FlowMetrics,
    pub criticality: CriticalityMetrics,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelSummary {
    pub label: String,
    pub issue_count: usize,
    pub open_count: usize,
    pub health: i32,
    pub health_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_issue: Option<String>,
    pub needs_attention: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelHealthResult {
    pub total_labels: usize,
    pub healthy_count: usize,
    pub warning_count: usize,
    pub critical_count: usize,
    pub labels: Vec<LabelHealth>,
    pub summaries: Vec<LabelSummary>,
    pub attention_needed: Vec<String>,
}

// ============================================================================
// Cross-Label Flow Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct LabelDependency {
    pub from_label: String,
    pub to_label: String,
    pub issue_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issue_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocking_pairs: Vec<BlockingPair>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockingPair {
    pub blocker_id: String,
    pub blocked_id: String,
    pub blocker_label: String,
    pub blocked_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossLabelFlow {
    pub labels: Vec<String>,
    pub flow_matrix: Vec<Vec<i32>>,
    pub dependencies: Vec<LabelDependency>,
    pub bottleneck_labels: Vec<String>,
    pub total_cross_label_deps: usize,
}

// ============================================================================
// Attention Score Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct LabelAttentionScore {
    pub label: String,
    pub attention_score: f64,
    pub normalized_score: f64,
    pub rank: usize,
    pub pagerank_sum: f64,
    pub staleness_factor: f64,
    pub block_impact: f64,
    pub velocity_factor: f64,
    pub open_count: usize,
    pub blocked_count: usize,
    pub stale_count: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelAttentionResult {
    pub labels: Vec<LabelAttentionScore>,
    pub total_labels: usize,
    pub max_score: f64,
    pub min_score: f64,
}

// ============================================================================
// Computation Functions
// ============================================================================

fn clamp_score(v: i32) -> i32 {
    v.clamp(0, 100)
}

fn health_level(score: i32) -> &'static str {
    if score >= HEALTHY_THRESHOLD {
        "healthy"
    } else if score >= WARNING_THRESHOLD {
        "warning"
    } else {
        "critical"
    }
}

fn parse_datetime(s: &str) -> Option<DateTime<Utc>> {
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn compute_velocity(labeled_issues: &[&Issue], now: DateTime<Utc>) -> VelocityMetrics {
    let week_ago = now - chrono::Duration::days(7);
    let month_ago = now - chrono::Duration::days(30);
    let prev_week_start = now - chrono::Duration::days(14);

    let mut closed_7 = 0i32;
    let mut closed_30 = 0i32;
    let mut current_week = 0i32;
    let mut prev_week = 0i32;
    let mut total_close_days = 0.0f64;
    let mut close_samples = 0i32;

    for issue in labeled_issues {
        if !issue.is_closed_like() {
            continue;
        }
        let closed_at = issue
            .closed_at
            .as_deref()
            .and_then(parse_datetime)
            .or_else(|| issue.updated_at.as_deref().and_then(parse_datetime));
        let Some(closed_at) = closed_at else {
            continue;
        };

        if closed_at > week_ago {
            closed_7 += 1;
            current_week += 1;
        }
        if closed_at > month_ago {
            closed_30 += 1;
        }
        if closed_at > prev_week_start && closed_at <= week_ago {
            prev_week += 1;
        }

        if let Some(created) = issue.created_at.as_deref().and_then(parse_datetime) {
            let days = (closed_at - created).num_hours() as f64 / 24.0;
            if days >= 0.0 {
                total_close_days += days;
                close_samples += 1;
            }
        }
    }

    let avg_days = if close_samples > 0 {
        total_close_days / f64::from(close_samples)
    } else {
        0.0
    };

    let (trend_direction, trend_percent) = if prev_week > 0 {
        let pct = (f64::from(current_week - prev_week) / f64::from(prev_week)) * 100.0;
        let dir = if pct > 10.0 {
            "improving"
        } else if pct < -10.0 {
            "declining"
        } else {
            "stable"
        };
        (dir, pct)
    } else if current_week > 0 {
        ("improving", 100.0)
    } else {
        ("stable", 0.0)
    };

    #[allow(clippy::cast_possible_truncation)]
    let mut velocity_score = if closed_30 > 0 {
        (f64::from(closed_30) * 10.0).min(100.0) as i32
    } else {
        0
    };

    if trend_direction == "improving" && velocity_score < 100 {
        velocity_score = clamp_score(velocity_score + 10);
    }

    VelocityMetrics {
        closed_last_7_days: closed_7,
        closed_last_30_days: closed_30,
        avg_days_to_close: avg_days,
        trend_direction: trend_direction.to_string(),
        trend_percent,
        velocity_score: clamp_score(velocity_score),
    }
}

fn compute_freshness(
    labeled_issues: &[&Issue],
    now: DateTime<Utc>,
    stale_days: i64,
) -> FreshnessMetrics {
    let threshold = stale_days as f64;
    let mut most_recent: Option<DateTime<Utc>> = None;
    let mut oldest_open: Option<DateTime<Utc>> = None;
    let mut total_staleness = 0.0f64;
    let mut count = 0i32;
    let mut stale_count = 0i32;

    for issue in labeled_issues {
        if let Some(updated) = issue.updated_at.as_deref().and_then(parse_datetime) {
            if most_recent.is_none_or(|mr| updated > mr) {
                most_recent = Some(updated);
            }
            let days = (now - updated).num_hours() as f64 / 24.0;
            total_staleness += days;
            count += 1;
            if days >= threshold {
                stale_count += 1;
            }
        }

        if !issue.is_closed_like() {
            if let Some(created) = issue.created_at.as_deref().and_then(parse_datetime) {
                if oldest_open.is_none_or(|oo| created < oo) {
                    oldest_open = Some(created);
                }
            }
        }
    }

    let avg_staleness = if count > 0 {
        total_staleness / f64::from(count)
    } else {
        0.0
    };

    #[allow(clippy::cast_possible_truncation)]
    let freshness_score = (100.0 - (avg_staleness / (threshold * 2.0)) * 100.0).max(0.0) as i32;

    FreshnessMetrics {
        most_recent_update: most_recent,
        oldest_open_issue: oldest_open,
        avg_days_since_update: avg_staleness,
        stale_count,
        stale_threshold_days: stale_days,
        freshness_score: clamp_score(freshness_score),
    }
}

fn compute_flow(label: &str, labeled_issues: &[&Issue], all_issues: &[Issue]) -> FlowMetrics {
    let issue_label_map: HashMap<&str, &[String]> = all_issues
        .iter()
        .map(|i| (i.id.as_str(), i.labels.as_slice()))
        .collect();

    let mut incoming_deps = 0i32;
    let mut outgoing_deps = 0i32;
    let mut incoming_labels = BTreeSet::new();
    let mut outgoing_labels = BTreeSet::new();

    for issue in labeled_issues {
        for dep in &issue.dependencies {
            if !dep.is_blocking() {
                continue;
            }
            // incoming: other label blocks this label
            if let Some(blocker_labels) = issue_label_map.get(dep.depends_on_id.as_str()) {
                for bl in *blocker_labels {
                    if bl != label {
                        incoming_deps += 1;
                        incoming_labels.insert(bl.clone());
                    }
                }
            }
            // outgoing: this label's issues block others
            for tl in &issue.labels {
                if tl != label {
                    outgoing_deps += 1;
                    outgoing_labels.insert(tl.clone());
                }
            }
        }
    }

    let flow_score = clamp_score(100 - (incoming_deps * 5));

    FlowMetrics {
        incoming_deps,
        outgoing_deps,
        incoming_labels: incoming_labels.into_iter().collect(),
        outgoing_labels: outgoing_labels.into_iter().collect(),
        blocked_by_external: incoming_deps,
        blocking_external: outgoing_deps,
        flow_score,
    }
}

fn compute_criticality(labeled_issues: &[&Issue], metrics: &GraphMetrics) -> CriticalityMetrics {
    let max_pr = metrics.pagerank.values().copied().fold(0.0f64, f64::max);
    let max_bw = metrics.betweenness.values().copied().fold(0.0f64, f64::max);

    let mut pr_sum = 0.0f64;
    let mut bw_sum = 0.0f64;
    let mut max_bw_label = 0.0f64;
    let mut crit_count = 0i32;
    let mut bottleneck_count = 0i32;

    for issue in labeled_issues {
        let pr = metrics.pagerank.get(&issue.id).copied().unwrap_or(0.0);
        let bw = metrics.betweenness.get(&issue.id).copied().unwrap_or(0.0);
        pr_sum += pr;
        bw_sum += bw;
        if bw > max_bw_label {
            max_bw_label = bw;
        }
        if metrics.critical_depth.get(&issue.id).copied().unwrap_or(0) > 0 {
            crit_count += 1;
        }
        if bw > 0.0 {
            bottleneck_count += 1;
        }
    }

    let n = labeled_issues.len() as f64;
    let avg_pr = if n > 0.0 { pr_sum / n } else { 0.0 };
    let avg_bw = if n > 0.0 { bw_sum / n } else { 0.0 };

    #[allow(clippy::cast_possible_truncation)]
    let mut crit_score = 0i32;
    if max_pr > 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        {
            crit_score += ((avg_pr / max_pr) * 50.0) as i32;
        }
    }
    if max_bw > 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        {
            crit_score += ((max_bw_label / max_bw) * 50.0) as i32;
        }
    }

    CriticalityMetrics {
        avg_pagerank: avg_pr,
        avg_betweenness: avg_bw,
        max_betweenness: max_bw_label,
        critical_path_count: crit_count,
        bottleneck_count,
        criticality_score: clamp_score(crit_score),
    }
}

fn composite_health(velocity: i32, freshness: i32, flow: i32, criticality: i32) -> i32 {
    let weighted = f64::from(velocity) * VELOCITY_WEIGHT
        + f64::from(freshness) * FRESHNESS_WEIGHT
        + f64::from(flow) * FLOW_WEIGHT
        + f64::from(criticality) * CRITICALITY_WEIGHT;
    #[allow(clippy::cast_possible_truncation)]
    let score = (weighted + 0.5) as i32;
    clamp_score(score)
}

fn compute_label_health(
    label: &str,
    all_issues: &[Issue],
    metrics: &GraphMetrics,
    now: DateTime<Utc>,
) -> LabelHealth {
    let labeled: Vec<&Issue> = all_issues
        .iter()
        .filter(|i| i.labels.iter().any(|l| l == label))
        .collect();

    let issue_count = labeled.len();
    if issue_count == 0 {
        return LabelHealth {
            label: label.to_string(),
            issue_count: 0,
            open_count: 0,
            closed_count: 0,
            blocked_count: 0,
            health: 0,
            health_level: "critical".to_string(),
            velocity: VelocityMetrics {
                closed_last_7_days: 0,
                closed_last_30_days: 0,
                avg_days_to_close: 0.0,
                trend_direction: "stable".to_string(),
                trend_percent: 0.0,
                velocity_score: 0,
            },
            freshness: FreshnessMetrics {
                most_recent_update: None,
                oldest_open_issue: None,
                avg_days_since_update: 0.0,
                stale_count: 0,
                stale_threshold_days: DEFAULT_STALE_THRESHOLD_DAYS,
                freshness_score: 0,
            },
            flow: FlowMetrics {
                incoming_deps: 0,
                outgoing_deps: 0,
                incoming_labels: vec![],
                outgoing_labels: vec![],
                blocked_by_external: 0,
                blocking_external: 0,
                flow_score: 100,
            },
            criticality: CriticalityMetrics {
                avg_pagerank: 0.0,
                avg_betweenness: 0.0,
                max_betweenness: 0.0,
                critical_path_count: 0,
                bottleneck_count: 0,
                criticality_score: 0,
            },
            issues: vec![],
        };
    }

    let mut open_count = 0usize;
    let mut closed_count = 0usize;
    let mut blocked_count = 0usize;
    let mut issue_ids = Vec::with_capacity(issue_count);

    for issue in &labeled {
        issue_ids.push(issue.id.clone());
        let status = issue.normalized_status();
        if issue.is_closed_like() {
            closed_count += 1;
        } else if status == "blocked" {
            blocked_count += 1;
        } else {
            open_count += 1;
        }
    }

    let velocity = compute_velocity(&labeled, now);
    let freshness = compute_freshness(&labeled, now, DEFAULT_STALE_THRESHOLD_DAYS);
    let flow = compute_flow(label, &labeled, all_issues);
    let criticality = compute_criticality(&labeled, metrics);

    let health = composite_health(
        velocity.velocity_score,
        freshness.freshness_score,
        flow.flow_score,
        criticality.criticality_score,
    );

    LabelHealth {
        label: label.to_string(),
        issue_count,
        open_count,
        closed_count,
        blocked_count,
        health,
        health_level: health_level(health).to_string(),
        velocity,
        freshness,
        flow,
        criticality,
        issues: issue_ids,
    }
}

/// Compute health for all labels in the issue set.
pub fn compute_all_label_health(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
) -> LabelHealthResult {
    let now = Utc::now();
    let _ = graph; // graph is available for future use

    // Extract unique labels sorted
    let mut label_set = BTreeSet::new();
    for issue in issues {
        for label in &issue.labels {
            if !label.is_empty() {
                label_set.insert(label.clone());
            }
        }
    }

    let mut result = LabelHealthResult {
        total_labels: label_set.len(),
        healthy_count: 0,
        warning_count: 0,
        critical_count: 0,
        labels: Vec::with_capacity(label_set.len()),
        summaries: Vec::with_capacity(label_set.len()),
        attention_needed: vec![],
    };

    for label in &label_set {
        let health = compute_label_health(label, issues, metrics, now);

        let summary = LabelSummary {
            label: label.clone(),
            issue_count: health.issue_count,
            open_count: health.open_count,
            health: health.health,
            health_level: health.health_level.clone(),
            top_issue: health.issues.first().cloned(),
            needs_attention: health.health < HEALTHY_THRESHOLD,
        };

        match health.health_level.as_str() {
            "healthy" => result.healthy_count += 1,
            "warning" => {
                result.warning_count += 1;
                result.attention_needed.push(label.clone());
            }
            "critical" => {
                result.critical_count += 1;
                result.attention_needed.push(label.clone());
            }
            _ => {}
        }

        result.labels.push(health);
        result.summaries.push(summary);
    }

    // Sort summaries by health descending, then label ascending
    result
        .summaries
        .sort_by(|a, b| b.health.cmp(&a.health).then_with(|| a.label.cmp(&b.label)));

    result
}

/// Compute cross-label dependency flow analysis.
pub fn compute_cross_label_flow(issues: &[Issue]) -> CrossLabelFlow {
    // Extract unique labels sorted
    let mut label_set = BTreeSet::new();
    for issue in issues {
        for label in &issue.labels {
            if !label.is_empty() {
                label_set.insert(label.clone());
            }
        }
    }
    let label_list: Vec<String> = label_set.into_iter().collect();
    let n = label_list.len();

    let mut label_index: HashMap<&str, usize> = HashMap::with_capacity(n);
    for (i, label) in label_list.iter().enumerate() {
        label_index.insert(label.as_str(), i);
    }

    let mut matrix = vec![vec![0i32; n]; n];
    let issue_map: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();

    // Track dependencies between label pairs
    let mut dep_map: BTreeMap<(String, String), LabelDependency> = BTreeMap::new();
    let mut total_deps = 0usize;

    for blocked in issues {
        if blocked.is_closed_like() {
            continue;
        }
        for dep in &blocked.dependencies {
            if !dep.is_blocking() {
                continue;
            }
            let Some(blocker) = issue_map.get(dep.depends_on_id.as_str()) else {
                continue;
            };
            if blocker.is_closed_like() {
                continue;
            }

            for from_label in &blocker.labels {
                for to_label in &blocked.labels {
                    if from_label.is_empty() || to_label.is_empty() || from_label == to_label {
                        continue;
                    }
                    let Some(&i_from) = label_index.get(from_label.as_str()) else {
                        continue;
                    };
                    let Some(&i_to) = label_index.get(to_label.as_str()) else {
                        continue;
                    };
                    matrix[i_from][i_to] += 1;
                    total_deps += 1;

                    let key = (from_label.clone(), to_label.clone());
                    let entry = dep_map.entry(key).or_insert_with_key(|k| LabelDependency {
                        from_label: k.0.clone(),
                        to_label: k.1.clone(),
                        issue_count: 0,
                        issue_ids: vec![],
                        blocking_pairs: vec![],
                    });
                    entry.issue_count += 1;
                    entry.issue_ids.push(blocked.id.clone());
                    entry.blocking_pairs.push(BlockingPair {
                        blocker_id: blocker.id.clone(),
                        blocked_id: blocked.id.clone(),
                        blocker_label: from_label.clone(),
                        blocked_label: to_label.clone(),
                    });
                }
            }
        }
    }

    let dependencies: Vec<LabelDependency> = dep_map.into_values().collect();

    // Bottleneck labels: highest outgoing dependencies
    let mut out_counts: Vec<(usize, &str)> = Vec::with_capacity(n);
    let mut max_out = 0i32;
    for (i, row) in matrix.iter().enumerate() {
        let sum: i32 = row.iter().sum();
        out_counts.push((i, &label_list[i]));
        if sum > max_out {
            max_out = sum;
        }
    }

    let mut bottleneck_labels: Vec<String> = Vec::new();
    if max_out > 0 {
        for (i, _) in &out_counts {
            let sum: i32 = matrix[*i].iter().sum();
            if sum == max_out {
                bottleneck_labels.push(label_list[*i].clone());
            }
        }
    }
    bottleneck_labels.sort();

    CrossLabelFlow {
        labels: label_list,
        flow_matrix: matrix,
        dependencies,
        bottleneck_labels,
        total_cross_label_deps: total_deps,
    }
}

/// Compute attention scores for all labels.
/// Formula: `attention = (pagerank_sum * staleness_factor * block_impact) / velocity`
/// Higher score = needs more attention.
pub fn compute_label_attention(
    issues: &[Issue],
    metrics: &GraphMetrics,
    limit: usize,
) -> LabelAttentionResult {
    let now = Utc::now();

    // Extract unique labels
    let mut label_set = BTreeSet::new();
    for issue in issues {
        for label in &issue.labels {
            if !label.is_empty() {
                label_set.insert(label.clone());
            }
        }
    }

    if label_set.is_empty() {
        return LabelAttentionResult {
            labels: vec![],
            total_labels: 0,
            max_score: 0.0,
            min_score: 0.0,
        };
    }

    let mut scores: Vec<LabelAttentionScore> = Vec::with_capacity(label_set.len());

    for label in &label_set {
        let labeled: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.labels.iter().any(|l| l == label))
            .collect();

        let mut open_count = 0usize;
        let mut blocked_count = 0usize;
        let mut stale_count = 0usize;
        let mut pr_sum = 0.0f64;

        for issue in &labeled {
            if issue.is_closed_like() {
                continue;
            }
            open_count += 1;

            let status = issue.normalized_status();
            if status == "blocked" {
                blocked_count += 1;
            }

            pr_sum += metrics.pagerank.get(&issue.id).copied().unwrap_or(0.0);

            // Check staleness
            if let Some(updated) = issue.updated_at.as_deref().and_then(parse_datetime) {
                let days = (now - updated).num_hours() as f64 / 24.0;
                if days >= DEFAULT_STALE_THRESHOLD_DAYS as f64 {
                    stale_count += 1;
                }
            }
        }

        // staleness_factor: 1 + (stale_count / open_count)
        let staleness_factor = if open_count > 0 {
            1.0 + (stale_count as f64 / open_count as f64)
        } else {
            1.0
        };

        // block_impact: count of issues blocked by this label's issues
        let mut block_impact = 0.0f64;
        for issue in &labeled {
            if issue.is_closed_like() {
                continue;
            }
            // Count how many other issues depend on this issue
            for other in issues {
                for dep in &other.dependencies {
                    if dep.is_blocking() && dep.depends_on_id == issue.id {
                        // Check if the blocked issue has different labels
                        if other.labels.iter().any(|l| l != label) || !other.labels.contains(label)
                        {
                            block_impact += 1.0;
                        }
                    }
                }
            }
        }
        // Ensure at least 1.0 to avoid zeroing out
        let block_factor = (1.0 + block_impact).max(1.0);

        // velocity_factor: based on recent closures
        let velocity = compute_velocity(&labeled, now);
        let velocity_factor = (1.0 + f64::from(velocity.closed_last_30_days)).max(1.0);

        // attention = (pagerank_sum * staleness_factor * block_factor) / velocity_factor
        let attention = (pr_sum * staleness_factor * block_factor) / velocity_factor;

        // Build reason string
        let reason = if stale_count > 0 && blocked_count > 0 {
            format!("{stale_count} stale + {blocked_count} blocked issues need attention")
        } else if stale_count > 0 {
            format!("{stale_count} stale issue(s) need attention")
        } else if blocked_count > 0 {
            format!("{blocked_count} blocked issue(s)")
        } else if open_count > 0 {
            format!("{open_count} open issue(s)")
        } else {
            "no open issues".to_string()
        };

        scores.push(LabelAttentionScore {
            label: label.clone(),
            attention_score: attention,
            normalized_score: 0.0, // set after normalization
            rank: 0,               // set after sorting
            pagerank_sum: pr_sum,
            staleness_factor,
            block_impact,
            velocity_factor,
            open_count,
            blocked_count,
            stale_count,
            reason,
        });
    }

    // Sort by attention score descending, then label ascending for ties
    scores.sort_by(|a, b| {
        b.attention_score
            .total_cmp(&a.attention_score)
            .then_with(|| a.label.cmp(&b.label))
    });

    // Normalize and rank
    let max_score = scores.first().map_or(0.0, |s| s.attention_score);
    let min_score = scores.last().map_or(0.0, |s| s.attention_score);
    let range = max_score - min_score;

    for (i, score) in scores.iter_mut().enumerate() {
        score.rank = i + 1;
        score.normalized_score = if range > 0.0 {
            (score.attention_score - min_score) / range
        } else if max_score > 0.0 {
            1.0
        } else {
            0.0
        };
    }

    let total_labels = scores.len();

    // Apply limit
    if limit > 0 && scores.len() > limit {
        scores.truncate(limit);
    }

    LabelAttentionResult {
        labels: scores,
        total_labels,
        max_score,
        min_score,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use crate::model::{Dependency, Issue};

    use super::*;

    fn make_issue(id: &str, labels: &[&str], status: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: status.to_string(),
            issue_type: "task".to_string(),
            priority: 2,
            labels: labels.iter().map(|s| (*s).to_string()).collect(),
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: Some("2026-02-15T00:00:00Z".to_string()),
            ..Issue::default()
        }
    }

    fn make_issue_with_dep(id: &str, labels: &[&str], status: &str, depends_on: &str) -> Issue {
        let mut issue = make_issue(id, labels, status);
        issue.dependencies.push(Dependency {
            issue_id: id.to_string(),
            depends_on_id: depends_on.to_string(),
            dep_type: "blocks".to_string(),
            ..Dependency::default()
        });
        issue
    }

    #[test]
    fn label_health_empty_issues() {
        let issues: Vec<Issue> = vec![];
        let graph = super::super::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_all_label_health(&issues, &graph, &metrics);
        assert_eq!(result.total_labels, 0);
        assert!(result.labels.is_empty());
    }

    #[test]
    fn label_health_single_label() {
        let issues = vec![
            make_issue("A", &["backend"], "open"),
            make_issue("B", &["backend"], "closed"),
        ];
        let graph = super::super::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_all_label_health(&issues, &graph, &metrics);

        assert_eq!(result.total_labels, 1);
        assert_eq!(result.labels.len(), 1);
        assert_eq!(result.labels[0].label, "backend");
        assert_eq!(result.labels[0].issue_count, 2);
        assert_eq!(result.labels[0].open_count, 1);
        assert_eq!(result.labels[0].closed_count, 1);
    }

    #[test]
    fn label_health_levels_correct() {
        // health_level should be set based on score thresholds
        assert_eq!(health_level(80), "healthy");
        assert_eq!(health_level(70), "healthy");
        assert_eq!(health_level(69), "warning");
        assert_eq!(health_level(40), "warning");
        assert_eq!(health_level(39), "critical");
        assert_eq!(health_level(0), "critical");
    }

    #[test]
    fn cross_label_flow_empty() {
        let issues: Vec<Issue> = vec![];
        let flow = compute_cross_label_flow(&issues);
        assert!(flow.labels.is_empty());
        assert_eq!(flow.total_cross_label_deps, 0);
    }

    #[test]
    fn cross_label_flow_with_deps() {
        let issues = vec![
            make_issue("A", &["backend"], "open"),
            make_issue_with_dep("B", &["frontend"], "open", "A"),
        ];
        let flow = compute_cross_label_flow(&issues);

        assert_eq!(flow.labels.len(), 2);
        assert!(flow.total_cross_label_deps > 0);
        assert!(!flow.dependencies.is_empty());
        // backend blocks frontend
        let dep = &flow.dependencies[0];
        assert_eq!(dep.from_label, "backend");
        assert_eq!(dep.to_label, "frontend");
    }

    #[test]
    fn cross_label_flow_no_self_deps() {
        let issues = vec![
            make_issue("A", &["backend"], "open"),
            make_issue_with_dep("B", &["backend"], "open", "A"),
        ];
        let flow = compute_cross_label_flow(&issues);
        // Both have same label, so no cross-label deps
        assert_eq!(flow.total_cross_label_deps, 0);
    }

    #[test]
    fn attention_empty_issues() {
        let issues: Vec<Issue> = vec![];
        let graph = super::super::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_label_attention(&issues, &metrics, 0);
        assert_eq!(result.total_labels, 0);
        assert!(result.labels.is_empty());
    }

    #[test]
    fn attention_ranking_order() {
        let issues = vec![
            make_issue("A", &["critical"], "open"),
            make_issue("B", &["critical"], "blocked"),
            make_issue("C", &["stable"], "closed"),
        ];
        let graph = super::super::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_label_attention(&issues, &metrics, 0);

        assert_eq!(result.total_labels, 2);
        // All scores should have rank >= 1
        for score in &result.labels {
            assert!(score.rank >= 1);
        }
    }

    #[test]
    fn attention_respects_limit() {
        let issues = vec![
            make_issue("A", &["alpha"], "open"),
            make_issue("B", &["beta"], "open"),
            make_issue("C", &["gamma"], "open"),
        ];
        let graph = super::super::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let result = compute_label_attention(&issues, &metrics, 2);
        assert_eq!(result.labels.len(), 2);
        assert_eq!(result.total_labels, 3);
    }
}
