pub mod alerts;
pub mod correlation;
pub mod diff;
pub mod file_intel;
pub mod forecast;
pub mod git_history;
pub mod graph;
pub mod history;
pub mod label_intel;
pub mod plan;
pub mod suggest;
pub mod triage;

use std::collections::HashMap;

use serde::Serialize;

use crate::model::Issue;

use self::alerts::{AlertOptions, RobotAlertsOutput};
use self::diff::SnapshotDiff;
use self::forecast::ForecastOutput;
use self::graph::{GraphMetrics, IssueGraph};
use self::history::IssueHistory;
use self::plan::ExecutionPlan;
use self::suggest::{RobotSuggestOutput, SuggestOptions};
use self::triage::{Recommendation, TriageComputation, TriageOptions, compute_triage};

#[derive(Debug, Clone, Serialize)]
pub struct MetricStatusEntry {
    pub state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricStatus {
    #[serde(rename = "PageRank")]
    pub page_rank: MetricStatusEntry,
    #[serde(rename = "Betweenness")]
    pub betweenness: MetricStatusEntry,
    #[serde(rename = "Eigenvector")]
    pub eigenvector: MetricStatusEntry,
    #[serde(rename = "HITS")]
    pub hits: MetricStatusEntry,
    #[serde(rename = "Critical")]
    pub critical: MetricStatusEntry,
    #[serde(rename = "Cycles")]
    pub cycles: MetricStatusEntry,
    #[serde(rename = "KCore")]
    pub k_core: MetricStatusEntry,
    #[serde(rename = "Articulation")]
    pub articulation: MetricStatusEntry,
    #[serde(rename = "Slack")]
    pub slack: MetricStatusEntry,
}

impl MetricStatus {
    pub const fn computed() -> Self {
        Self {
            page_rank: MetricStatusEntry { state: "computed" },
            betweenness: MetricStatusEntry { state: "computed" },
            eigenvector: MetricStatusEntry { state: "computed" },
            hits: MetricStatusEntry { state: "computed" },
            critical: MetricStatusEntry { state: "computed" },
            cycles: MetricStatusEntry { state: "computed" },
            k_core: MetricStatusEntry { state: "computed" },
            articulation: MetricStatusEntry { state: "computed" },
            slack: MetricStatusEntry { state: "computed" },
        }
    }
}

impl Default for MetricStatus {
    fn default() -> Self {
        Self::computed()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InsightItem {
    pub id: String,
    pub title: String,
    pub score: f64,
    pub blocks_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricItem {
    pub id: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreItem {
    pub id: String,
    pub value: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Insights {
    pub status: MetricStatus,
    pub bottlenecks: Vec<InsightItem>,
    pub critical_path: Vec<String>,
    pub cycles: Vec<Vec<String>>,
    pub slack: Vec<String>,
    pub influencers: Vec<MetricItem>,
    pub betweenness: Vec<MetricItem>,
    pub hubs: Vec<MetricItem>,
    pub authorities: Vec<MetricItem>,
    pub eigenvector: Vec<MetricItem>,
    pub cores: Vec<CoreItem>,
    pub articulation_points: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Analyzer {
    pub issues: Vec<Issue>,
    pub graph: IssueGraph,
    pub metrics: GraphMetrics,
}

impl Analyzer {
    #[must_use]
    pub fn new(mut issues: Vec<Issue>) -> Self {
        issues.sort_by(|left, right| left.id.cmp(&right.id));
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        Self {
            issues,
            graph,
            metrics,
        }
    }

    #[must_use]
    pub fn triage(&self, options: TriageOptions) -> TriageComputation {
        compute_triage(&self.issues, &self.graph, &self.metrics, &options)
    }

    #[must_use]
    pub fn plan(&self, score_by_id: &HashMap<String, f64>) -> ExecutionPlan {
        plan::compute_execution_plan(&self.graph, score_by_id)
    }

    #[must_use]
    pub fn insights(&self) -> Insights {
        let mut bottlenecks = self
            .issues
            .iter()
            .filter(|issue| issue.is_open_like())
            .map(|issue| {
                let pagerank = self
                    .metrics
                    .pagerank
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default();
                let betweenness = self
                    .metrics
                    .betweenness
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default();
                let blocks_count = self
                    .metrics
                    .blocks_count
                    .get(&issue.id)
                    .copied()
                    .unwrap_or_default();

                // PageRank + betweenness favors central blockers and bridges.
                let score = pagerank + (0.1 * betweenness);

                InsightItem {
                    id: issue.id.clone(),
                    title: issue.title.clone(),
                    score,
                    blocks_count,
                }
            })
            .collect::<Vec<_>>();

        bottlenecks.sort_by(|left, right| {
            right
                .blocks_count
                .cmp(&left.blocks_count)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.id.cmp(&right.id))
        });
        bottlenecks.truncate(15);

        let mut critical_path = self
            .metrics
            .critical_depth
            .iter()
            .filter_map(|(id, depth)| {
                if *depth == 0 {
                    return None;
                }
                Some((id.clone(), *depth))
            })
            .collect::<Vec<_>>();
        critical_path
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        critical_path.truncate(20);

        let mut zero_slack = self
            .metrics
            .slack
            .iter()
            .filter_map(|(id, slack)| {
                if *slack <= 0.001 {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        zero_slack.sort();

        let mut articulation_points = self
            .metrics
            .articulation_points
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        articulation_points.sort();

        Insights {
            status: MetricStatus::computed(),
            bottlenecks,
            critical_path: critical_path.into_iter().map(|(id, _)| id).collect(),
            cycles: self.metrics.cycles.clone(),
            slack: zero_slack,
            influencers: top_metric_items(&self.metrics.pagerank, 20),
            betweenness: top_metric_items(&self.metrics.betweenness, 20),
            hubs: top_metric_items(&self.metrics.hubs, 20),
            authorities: top_metric_items(&self.metrics.authorities, 20),
            eigenvector: top_metric_items(&self.metrics.eigenvector, 20),
            cores: top_core_items(&self.metrics.k_core, 20),
            articulation_points,
        }
    }

    #[must_use]
    pub fn priority(
        &self,
        min_confidence: f64,
        max_results: usize,
        by_label: Option<&str>,
        by_assignee: Option<&str>,
    ) -> Vec<Recommendation> {
        let triage = self.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: max_results.max(50),
        });

        let mut results = triage
            .result
            .recommendations
            .into_iter()
            .filter(|rec| rec.confidence >= min_confidence)
            .filter(|rec| {
                by_label.is_none_or(|label| rec.labels.iter().any(|entry| entry == label))
            })
            .filter(|rec| by_assignee.is_none_or(|assignee| rec.assignee == assignee))
            .collect::<Vec<_>>();

        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

        if max_results > 0 {
            results.truncate(max_results);
        }

        results
    }

    #[must_use]
    pub fn diff(&self, before_issues: &[Issue]) -> SnapshotDiff {
        diff::compare_snapshots(before_issues, &self.issues)
    }

    #[must_use]
    pub fn history(&self, only_issue_id: Option<&str>, limit: usize) -> Vec<IssueHistory> {
        history::build_histories(&self.issues, only_issue_id, limit)
    }

    #[must_use]
    pub fn forecast(
        &self,
        issue_id_or_all: &str,
        label_filter: Option<&str>,
        agents: usize,
    ) -> ForecastOutput {
        forecast::estimate_forecast(
            &self.issues,
            &self.graph,
            &self.metrics,
            issue_id_or_all,
            label_filter,
            agents,
        )
    }

    #[must_use]
    pub fn suggest(&self, options: &SuggestOptions) -> RobotSuggestOutput {
        suggest::generate_robot_suggest_output(&self.issues, &self.metrics, options)
    }

    #[must_use]
    pub fn alerts(&self, options: &AlertOptions) -> RobotAlertsOutput {
        alerts::generate_robot_alerts_output(&self.issues, &self.graph, &self.metrics, options)
    }
}

fn top_metric_items(values: &HashMap<String, f64>, limit: usize) -> Vec<MetricItem> {
    let mut items = values
        .iter()
        .map(|(id, value)| MetricItem {
            id: id.clone(),
            value: *value,
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        right
            .value
            .total_cmp(&left.value)
            .then_with(|| left.id.cmp(&right.id))
    });

    if limit > 0 {
        items.truncate(limit);
    }

    items
}

fn top_core_items(values: &HashMap<String, u32>, limit: usize) -> Vec<CoreItem> {
    let mut items = values
        .iter()
        .map(|(id, value)| CoreItem {
            id: id.clone(),
            value: *value,
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then_with(|| left.id.cmp(&right.id))
    });

    if limit > 0 {
        items.truncate(limit);
    }

    items
}

#[cfg(test)]
mod tests {
    use crate::model::{Dependency, Issue};

    use super::Analyzer;

    #[test]
    fn insights_promote_primary_blocker_for_bd_3q0_slice() {
        let issues = vec![
            Issue {
                id: "bd-3q0".to_string(),
                title: "Primary blocker".to_string(),
                status: "in_progress".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "bd-3q1".to_string(),
                title: "Blocked follow-on".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "bd-3q1".to_string(),
                    depends_on_id: "bd-3q0".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "bd-3q2".to_string(),
                title: "Independent slice".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                ..Issue::default()
            },
        ];

        let analyzer = Analyzer::new(issues);
        let insights = analyzer.insights();

        assert_eq!(
            insights.bottlenecks.first().map(|item| item.id.as_str()),
            Some("bd-3q0")
        );
        assert_eq!(
            insights.bottlenecks.first().map(|item| item.blocks_count),
            Some(1)
        );
        assert_eq!(
            insights.critical_path.first().map(String::as_str),
            Some("bd-3q0")
        );
    }
}
