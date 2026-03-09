use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::Utc;
use serde::Serialize;

use super::graph::IssueGraph;
use crate::model::{Comment, Dependency, Issue};

const ZERO_TIME_RFC3339: &str = "0001-01-01T00:00:00Z";

#[derive(Debug, Clone)]
pub struct DiffMetadata {
    pub from_timestamp: String,
    pub to_timestamp: String,
    pub from_revision: Option<String>,
    pub to_revision: Option<String>,
}

impl Default for DiffMetadata {
    fn default() -> Self {
        Self {
            from_timestamp: "0001-01-01T00:00:00Z".to_string(),
            to_timestamp: Utc::now().to_rfc3339(),
            from_revision: None,
            to_revision: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldChange {
    pub field: String,
    pub old_value: String,
    pub new_value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModifiedIssue {
    pub issue_id: String,
    pub title: String,
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffIssue {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DiffDependency>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<DiffComment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffDependency {
    pub issue_id: String,
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffComment {
    pub id: i64,
    pub issue_id: String,
    pub author: String,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MetricDeltas {
    pub total_issues: i64,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub blocked_issues: i64,
    pub total_edges: i64,
    pub cycle_count: i64,
    pub component_count: i64,
    pub avg_pagerank: f64,
    pub avg_betweenness: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DiffSummary {
    pub total_changes: usize,
    pub issues_added: usize,
    pub issues_closed: usize,
    pub issues_removed: usize,
    pub issues_reopened: usize,
    pub issues_modified: usize,
    pub cycles_introduced: usize,
    pub cycles_resolved: usize,
    pub net_issue_change: i64,
    pub health_trend: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDiff {
    pub from_timestamp: String,
    pub to_timestamp: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub from_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub to_revision: String,
    pub new_issues: Option<Vec<DiffIssue>>,
    pub closed_issues: Option<Vec<DiffIssue>>,
    pub removed_issues: Option<Vec<DiffIssue>>,
    pub reopened_issues: Option<Vec<DiffIssue>>,
    pub modified_issues: Option<Vec<ModifiedIssue>>,
    pub new_cycles: Option<Vec<Vec<String>>>,
    pub resolved_cycles: Option<Vec<Vec<String>>>,
    pub metric_deltas: MetricDeltas,
    pub summary: DiffSummary,
}

impl SnapshotDiff {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.summary.total_changes == 0
            && self.summary.cycles_introduced == 0
            && self.summary.cycles_resolved == 0
    }

    #[must_use]
    pub fn has_significant_changes(&self) -> bool {
        option_len(self.new_issues.as_ref()) > 0
            || option_len(self.closed_issues.as_ref()) > 0
            || option_len(self.reopened_issues.as_ref()) > 0
            || option_len(self.new_cycles.as_ref()) > 0
            || option_len(self.resolved_cycles.as_ref()) > 0
            || self.summary.health_trend == "degrading"
    }
}

#[must_use]
pub fn compare_snapshots(before: &[Issue], after: &[Issue]) -> SnapshotDiff {
    compare_snapshots_with_metadata(before, after, &DiffMetadata::default())
}

#[must_use]
pub fn compare_snapshots_with_metadata(
    before: &[Issue],
    after: &[Issue],
    metadata: &DiffMetadata,
) -> SnapshotDiff {
    let before_map: HashMap<&str, &Issue> = before
        .iter()
        .map(|issue| (issue.id.as_str(), issue))
        .collect();
    let after_map: HashMap<&str, &Issue> = after
        .iter()
        .map(|issue| (issue.id.as_str(), issue))
        .collect();

    let before_ids: HashSet<&str> = before_map.keys().copied().collect();
    let after_ids: HashSet<&str> = after_map.keys().copied().collect();

    let mut new_issues = Vec::<DiffIssue>::new();
    let mut closed_issues = Vec::<DiffIssue>::new();
    let mut removed_issues = Vec::<DiffIssue>::new();
    let mut reopened_issues = Vec::<DiffIssue>::new();
    let mut modified_issues = Vec::<ModifiedIssue>::new();

    for id in after_ids.difference(&before_ids) {
        if let Some(issue) = after_map.get(id) {
            new_issues.push(to_diff_issue(issue));
        }
    }

    for id in before_ids.intersection(&after_ids) {
        let Some(before_issue) = before_map.get(id) else {
            continue;
        };
        let Some(after_issue) = after_map.get(id) else {
            continue;
        };

        let mut changes = detect_changes(before_issue, after_issue);
        let before_closed = before_issue.is_closed_like();
        let after_closed = after_issue.is_closed_like();
        let mut status_transition = false;

        if !before_closed && after_closed {
            status_transition = true;
            closed_issues.push(to_diff_issue(after_issue));
        } else if before_closed && !after_closed {
            status_transition = true;
            reopened_issues.push(to_diff_issue(after_issue));
        }

        if status_transition {
            changes.retain(|change| change.field != "status");
        }

        if !changes.is_empty() {
            modified_issues.push(ModifiedIssue {
                issue_id: after_issue.id.clone(),
                title: after_issue.title.clone(),
                changes,
            });
        }
    }

    for id in before_ids.difference(&after_ids) {
        if let Some(issue) = before_map.get(id) {
            removed_issues.push(to_diff_issue(issue));
        }
    }

    new_issues.sort_by(|left, right| left.id.cmp(&right.id));
    closed_issues.sort_by(|left, right| left.id.cmp(&right.id));
    removed_issues.sort_by(|left, right| left.id.cmp(&right.id));
    reopened_issues.sort_by(|left, right| left.id.cmp(&right.id));
    modified_issues.sort_by(|left, right| left.issue_id.cmp(&right.issue_id));

    let from_graph = IssueGraph::build(before);
    let to_graph = IssueGraph::build(after);
    let from_metrics = from_graph.compute_metrics();
    let to_metrics = to_graph.compute_metrics();
    let (new_cycles, resolved_cycles) = compare_cycles(&from_metrics.cycles, &to_metrics.cycles);

    let from_component_count = from_graph.connected_open_components().len();
    let to_component_count = to_graph.connected_open_components().len();

    let metric_deltas = calculate_metric_deltas(MetricDeltaInputs {
        before,
        after,
        new_cycles_count: option_len(new_cycles.as_ref()),
        resolved_cycles_count: option_len(resolved_cycles.as_ref()),
        from_pagerank: &from_metrics.pagerank,
        to_pagerank: &to_metrics.pagerank,
        from_betweenness: &from_metrics.betweenness,
        to_betweenness: &to_metrics.betweenness,
        from_edge_count: from_graph.edge_count(),
        to_edge_count: to_graph.edge_count(),
        from_component_count,
        to_component_count,
    });

    let summary = calculate_summary(SummaryInputs {
        issues_added: new_issues.len(),
        issues_closed: closed_issues.len(),
        issues_removed: removed_issues.len(),
        issues_reopened: reopened_issues.len(),
        issues_modified: modified_issues.len(),
        cycles_introduced: option_len(new_cycles.as_ref()),
        cycles_resolved: option_len(resolved_cycles.as_ref()),
        blocked_issue_delta: metric_deltas.blocked_issues,
    });

    SnapshotDiff {
        from_timestamp: metadata.from_timestamp.clone(),
        to_timestamp: metadata.to_timestamp.clone(),
        from_revision: metadata.from_revision.clone().unwrap_or_default(),
        to_revision: metadata.to_revision.clone().unwrap_or_default(),
        new_issues: into_option(new_issues),
        closed_issues: into_option(closed_issues),
        removed_issues: into_option(removed_issues),
        reopened_issues: into_option(reopened_issues),
        modified_issues: into_option(modified_issues),
        new_cycles,
        resolved_cycles,
        metric_deltas,
        summary,
    }
}

fn detect_changes(from: &Issue, to: &Issue) -> Vec<FieldChange> {
    let mut changes = Vec::<FieldChange>::new();

    if from.title != to.title {
        changes.push(FieldChange {
            field: "title".to_string(),
            old_value: from.title.clone(),
            new_value: to.title.clone(),
        });
    }

    if from.status != to.status {
        changes.push(FieldChange {
            field: "status".to_string(),
            old_value: from.status.clone(),
            new_value: to.status.clone(),
        });
    }

    if from.priority != to.priority {
        changes.push(FieldChange {
            field: "priority".to_string(),
            old_value: priority_string(from.priority),
            new_value: priority_string(to.priority),
        });
    }

    if from.assignee != to.assignee {
        changes.push(FieldChange {
            field: "assignee".to_string(),
            old_value: from.assignee.clone(),
            new_value: to.assignee.clone(),
        });
    }

    if from.issue_type != to.issue_type {
        changes.push(FieldChange {
            field: "type".to_string(),
            old_value: from.issue_type.clone(),
            new_value: to.issue_type.clone(),
        });
    }

    if from.description != to.description {
        changes.push(FieldChange {
            field: "description".to_string(),
            old_value: "(modified)".to_string(),
            new_value: "(modified)".to_string(),
        });
    }

    if from.design != to.design {
        changes.push(FieldChange {
            field: "design".to_string(),
            old_value: "(modified)".to_string(),
            new_value: "(modified)".to_string(),
        });
    }

    if from.acceptance_criteria != to.acceptance_criteria {
        changes.push(FieldChange {
            field: "acceptance_criteria".to_string(),
            old_value: "(modified)".to_string(),
            new_value: "(modified)".to_string(),
        });
    }

    if from.notes != to.notes {
        changes.push(FieldChange {
            field: "notes".to_string(),
            old_value: "(modified)".to_string(),
            new_value: "(modified)".to_string(),
        });
    }

    let from_deps = dependency_set(&from.dependencies);
    let to_deps = dependency_set(&to.dependencies);
    if from_deps != to_deps {
        changes.push(FieldChange {
            field: "dependencies".to_string(),
            old_value: format_dep_set(&from_deps),
            new_value: format_dep_set(&to_deps),
        });
    }

    let from_labels = string_set(&from.labels);
    let to_labels = string_set(&to.labels);
    if from_labels != to_labels {
        changes.push(FieldChange {
            field: "labels".to_string(),
            old_value: format_string_set(&from_labels),
            new_value: format_string_set(&to_labels),
        });
    }

    changes
}

fn dependency_set(deps: &[Dependency]) -> BTreeSet<String> {
    let mut values = BTreeSet::<String>::new();
    for dep in deps {
        if dep.depends_on_id.trim().is_empty() {
            continue;
        }
        values.insert(format!("{}:{}", dep.depends_on_id, dep.dep_type));
    }
    values
}

fn string_set(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
}

fn format_dep_set(values: &BTreeSet<String>) -> String {
    format_string_set(values)
}

fn format_string_set(values: &BTreeSet<String>) -> String {
    if values.is_empty() {
        "(none)".to_string()
    } else {
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn priority_string(priority: i32) -> String {
    format!("P{priority}")
}

fn to_diff_issue(issue: &Issue) -> DiffIssue {
    DiffIssue {
        id: issue.id.clone(),
        title: issue.title.clone(),
        description: issue.description.clone(),
        status: issue.status.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: non_empty(&issue.assignee),
        created_at: timestamp_or_zero(issue.created_at.as_deref()),
        updated_at: timestamp_or_zero(issue.updated_at.as_deref()),
        closed_at: issue.closed_at.as_deref().and_then(non_empty),
        labels: issue.labels.clone(),
        dependencies: issue.dependencies.iter().map(to_diff_dependency).collect(),
        comments: issue.comments.iter().map(to_diff_comment).collect(),
    }
}

fn to_diff_dependency(dep: &Dependency) -> DiffDependency {
    DiffDependency {
        issue_id: dep.issue_id.clone(),
        depends_on_id: dep.depends_on_id.clone(),
        dep_type: dep.dep_type.clone(),
        created_by: dep.created_by.clone(),
        created_at: timestamp_or_zero(dep.created_at.as_deref()),
    }
}

fn to_diff_comment(comment: &Comment) -> DiffComment {
    DiffComment {
        id: comment.id,
        issue_id: comment.issue_id.clone(),
        author: comment.author.clone(),
        text: comment.text.clone(),
        created_at: timestamp_or_zero(comment.created_at.as_deref()),
    }
}

fn timestamp_or_zero(raw: Option<&str>) -> String {
    raw.filter(|value| !value.trim().is_empty())
        .unwrap_or(ZERO_TIME_RFC3339)
        .to_string()
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn option_len<T>(values: Option<&Vec<T>>) -> usize {
    values.map_or(0, Vec::len)
}

fn into_option<T>(values: Vec<T>) -> Option<Vec<T>> {
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

type OptionalCycleSets = (Option<Vec<Vec<String>>>, Option<Vec<Vec<String>>>);

fn compare_cycles(from_cycles: &[Vec<String>], to_cycles: &[Vec<String>]) -> OptionalCycleSets {
    let from_cycle_set = from_cycles
        .iter()
        .map(|cycle| (normalize_cycle(cycle), cycle.clone()))
        .collect::<HashMap<_, _>>();
    let to_cycle_set = to_cycles
        .iter()
        .map(|cycle| (normalize_cycle(cycle), cycle.clone()))
        .collect::<HashMap<_, _>>();

    let mut new_cycles = to_cycle_set
        .iter()
        .filter_map(|(key, cycle)| {
            if from_cycle_set.contains_key(key) {
                None
            } else {
                Some(cycle.clone())
            }
        })
        .collect::<Vec<_>>();

    let mut resolved_cycles = from_cycle_set
        .iter()
        .filter_map(|(key, cycle)| {
            if to_cycle_set.contains_key(key) {
                None
            } else {
                Some(cycle.clone())
            }
        })
        .collect::<Vec<_>>();

    new_cycles.sort_by_key(|cycle| normalize_cycle(cycle));
    resolved_cycles.sort_by_key(|cycle| normalize_cycle(cycle));

    (into_option(new_cycles), into_option(resolved_cycles))
}

fn normalize_cycle(cycle: &[String]) -> String {
    if cycle.is_empty() {
        return String::new();
    }

    let mut min_idx = 0usize;
    for (index, id) in cycle.iter().enumerate().skip(1) {
        if id < &cycle[min_idx] {
            min_idx = index;
        }
    }

    (0..cycle.len())
        .map(|offset| cycle[(min_idx + offset) % cycle.len()].clone())
        .collect::<Vec<_>>()
        .join("->")
}

struct MetricDeltaInputs<'a> {
    before: &'a [Issue],
    after: &'a [Issue],
    new_cycles_count: usize,
    resolved_cycles_count: usize,
    from_pagerank: &'a HashMap<String, f64>,
    to_pagerank: &'a HashMap<String, f64>,
    from_betweenness: &'a HashMap<String, f64>,
    to_betweenness: &'a HashMap<String, f64>,
    from_edge_count: usize,
    to_edge_count: usize,
    from_component_count: usize,
    to_component_count: usize,
}

fn calculate_metric_deltas(inputs: MetricDeltaInputs<'_>) -> MetricDeltas {
    let before_counts = snapshot_counts(inputs.before);
    let after_counts = snapshot_counts(inputs.after);

    MetricDeltas {
        total_issues: i64::try_from(after_counts.total).unwrap_or(i64::MAX)
            - i64::try_from(before_counts.total).unwrap_or(i64::MAX),
        open_issues: i64::try_from(after_counts.open).unwrap_or(i64::MAX)
            - i64::try_from(before_counts.open).unwrap_or(i64::MAX),
        closed_issues: i64::try_from(after_counts.terminal()).unwrap_or(i64::MAX)
            - i64::try_from(before_counts.terminal()).unwrap_or(i64::MAX),
        blocked_issues: i64::try_from(after_counts.blocked).unwrap_or(i64::MAX)
            - i64::try_from(before_counts.blocked).unwrap_or(i64::MAX),
        total_edges: i64::try_from(inputs.to_edge_count).unwrap_or(i64::MAX)
            - i64::try_from(inputs.from_edge_count).unwrap_or(i64::MAX),
        cycle_count: i64::try_from(inputs.new_cycles_count).unwrap_or(i64::MAX)
            - i64::try_from(inputs.resolved_cycles_count).unwrap_or(i64::MAX),
        component_count: i64::try_from(inputs.to_component_count).unwrap_or(i64::MAX)
            - i64::try_from(inputs.from_component_count).unwrap_or(i64::MAX),
        avg_pagerank: average_map_value(inputs.to_pagerank)
            - average_map_value(inputs.from_pagerank),
        avg_betweenness: average_map_value(inputs.to_betweenness)
            - average_map_value(inputs.from_betweenness),
    }
}

#[derive(Debug, Copy, Clone, Default)]
struct SnapshotCounts {
    total: usize,
    open: usize,
    closed: usize,
    tombstone: usize,
    blocked: usize,
}

impl SnapshotCounts {
    fn terminal(&self) -> usize {
        self.closed + self.tombstone
    }
}

fn snapshot_counts(issues: &[Issue]) -> SnapshotCounts {
    let mut counts = SnapshotCounts {
        total: issues.len(),
        ..SnapshotCounts::default()
    };

    for issue in issues {
        if issue.is_tombstone() {
            counts.tombstone = counts.tombstone.saturating_add(1);
        } else if issue.is_closed() {
            counts.closed = counts.closed.saturating_add(1);
        } else {
            counts.open = counts.open.saturating_add(1);
            if issue.normalized_status() == "blocked" {
                counts.blocked = counts.blocked.saturating_add(1);
            }
        }
    }

    counts
}

fn average_map_value(values: &HashMap<String, f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mut keys = values.keys().cloned().collect::<Vec<_>>();
    keys.sort();

    let sum = keys
        .iter()
        .filter_map(|key| values.get(key))
        .fold(0.0_f64, |acc, value| acc + value);

    sum / (values.len() as f64)
}

struct SummaryInputs {
    issues_added: usize,
    issues_closed: usize,
    issues_removed: usize,
    issues_reopened: usize,
    issues_modified: usize,
    cycles_introduced: usize,
    cycles_resolved: usize,
    blocked_issue_delta: i64,
}

fn calculate_summary(inputs: SummaryInputs) -> DiffSummary {
    let total_changes = inputs.issues_added
        + inputs.issues_closed
        + inputs.issues_removed
        + inputs.issues_reopened
        + inputs.issues_modified;

    let mut score = 0_i64;
    score += i64::try_from(inputs.cycles_resolved.saturating_mul(2)).unwrap_or(i64::MAX);
    score -= i64::try_from(inputs.cycles_introduced.saturating_mul(3)).unwrap_or(i64::MAX);
    score += i64::try_from(inputs.issues_closed).unwrap_or(i64::MAX);
    score -= i64::try_from(inputs.issues_reopened).unwrap_or(i64::MAX);

    if inputs.blocked_issue_delta < 0 {
        score += 2;
    } else if inputs.blocked_issue_delta > 0 {
        score -= 1;
    }

    let health_trend = if score > 1 {
        "improving"
    } else if score < -1 {
        "degrading"
    } else {
        "stable"
    };

    DiffSummary {
        total_changes,
        issues_added: inputs.issues_added,
        issues_closed: inputs.issues_closed,
        issues_removed: inputs.issues_removed,
        issues_reopened: inputs.issues_reopened,
        issues_modified: inputs.issues_modified,
        cycles_introduced: inputs.cycles_introduced,
        cycles_resolved: inputs.cycles_resolved,
        net_issue_change: i64::try_from(inputs.issues_added).unwrap_or(i64::MAX)
            - i64::try_from(inputs.issues_removed).unwrap_or(i64::MAX),
        health_trend: health_trend.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{Dependency, Issue};

    use super::compare_snapshots;

    #[test]
    fn detects_new_closed_reopened_and_modified() {
        let before = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                ..Issue::default()
            },
        ];

        let after = vec![
            Issue {
                id: "A".to_string(),
                title: "A2".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                dependencies: vec![Dependency {
                    depends_on_id: "C".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                ..Issue::default()
            },
        ];

        let diff = compare_snapshots(&before, &after);
        assert_eq!(diff.new_issues.as_ref().map_or(0, Vec::len), 1);
        assert_eq!(diff.closed_issues.as_ref().map_or(0, Vec::len), 1);
        assert_eq!(diff.reopened_issues.as_ref().map_or(0, Vec::len), 1);
        assert_eq!(diff.modified_issues.as_ref().map_or(0, Vec::len), 1);
        assert_eq!(
            diff.modified_issues
                .as_ref()
                .and_then(|issues| issues.first())
                .map(|issue| issue.issue_id.as_str()),
            Some("A")
        );
        assert_eq!(diff.summary.issues_added, 1);
        assert_eq!(diff.summary.issues_removed, 0);
    }

    #[test]
    fn empty_before_and_after_produces_empty_diff() {
        let diff = compare_snapshots(&[], &[]);
        assert_eq!(diff.summary.issues_added, 0);
        assert_eq!(diff.summary.issues_removed, 0);
        assert_eq!(diff.summary.issues_modified, 0);
        assert!(diff.new_issues.as_ref().is_none_or(Vec::is_empty));
        assert!(diff.closed_issues.as_ref().is_none_or(Vec::is_empty));
    }

    #[test]
    fn all_new_issues_detected() {
        let after = vec![
            Issue {
                id: "N-1".to_string(),
                title: "New one".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "N-2".to_string(),
                title: "New two".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let diff = compare_snapshots(&[], &after);
        assert_eq!(diff.new_issues.as_ref().map_or(0, Vec::len), 2);
        assert_eq!(diff.summary.issues_added, 2);
    }

    #[test]
    fn removed_issues_tracked() {
        let before = vec![Issue {
            id: "G-1".to_string(),
            title: "Gone".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let diff = compare_snapshots(&before, &[]);
        assert_eq!(diff.summary.issues_removed, 1);
    }

    #[test]
    fn identical_snapshots_produce_no_changes() {
        let issues = vec![Issue {
            id: "S-1".to_string(),
            title: "Stable".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            priority: 1,
            ..Issue::default()
        }];
        let diff = compare_snapshots(&issues, &issues);
        assert!(diff.new_issues.as_ref().is_none_or(Vec::is_empty));
        assert!(diff.closed_issues.as_ref().is_none_or(Vec::is_empty));
        assert!(diff.reopened_issues.as_ref().is_none_or(Vec::is_empty));
        assert!(diff.modified_issues.as_ref().is_none_or(Vec::is_empty));
        assert_eq!(diff.summary.issues_added, 0);
        assert_eq!(diff.summary.issues_removed, 0);
    }

    #[test]
    fn priority_change_detected_as_modification() {
        let before = vec![Issue {
            id: "P-1".to_string(),
            title: "Same".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            priority: 1,
            ..Issue::default()
        }];
        let after = vec![Issue {
            id: "P-1".to_string(),
            title: "Same".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            priority: 3,
            ..Issue::default()
        }];
        let diff = compare_snapshots(&before, &after);
        assert_eq!(diff.modified_issues.as_ref().map_or(0, Vec::len), 1);
        let mods = diff.modified_issues.unwrap();
        assert!(mods[0].changes.iter().any(|c| c.field == "priority"));
    }

    #[test]
    fn dependency_change_detected() {
        let before = vec![Issue {
            id: "D-1".to_string(),
            title: "Dep change".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let after = vec![Issue {
            id: "D-1".to_string(),
            title: "Dep change".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            dependencies: vec![Dependency {
                depends_on_id: "D-2".to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            }],
            ..Issue::default()
        }];
        let diff = compare_snapshots(&before, &after);
        assert_eq!(diff.modified_issues.as_ref().map_or(0, Vec::len), 1);
    }

    #[test]
    fn metric_deltas_computed() {
        let before = vec![
            Issue {
                id: "M-1".to_string(),
                title: "Open".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "M-2".to_string(),
                title: "Blocked".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    depends_on_id: "M-1".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        let after = vec![
            Issue {
                id: "M-1".to_string(),
                title: "Open".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "M-2".to_string(),
                title: "Blocked".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let diff = compare_snapshots(&before, &after);
        // Closing M-1 should change open_issues delta
        assert_ne!(diff.metric_deltas.open_issues, 0);
    }

    #[test]
    fn metric_deltas_treat_review_like_status_as_open() {
        let before = vec![Issue {
            id: "R-1".to_string(),
            title: "In review".to_string(),
            status: "review".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let after = Vec::<Issue>::new();

        let diff = compare_snapshots(&before, &after);
        assert_eq!(
            diff.metric_deltas.open_issues, -1,
            "review status should be counted as open-like in deltas"
        );
    }
}
