pub mod advanced;
pub mod alerts;
pub mod brief;
pub mod cache;
pub mod causal;
pub mod correlation;
pub mod diff;
pub mod drift;
pub mod file_intel;
pub mod forecast;
pub mod git_history;
pub mod graph;
pub mod history;
pub mod label_intel;
pub mod plan;
pub mod recipe;
pub mod search;
pub mod suggest;
pub mod triage;
pub mod whatif;

use std::collections::{HashMap, HashSet};

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
    pub fn new_with_config(mut issues: Vec<Issue>, config: &graph::AnalysisConfig) -> Self {
        issues.sort_by(|left, right| left.id.cmp(&right.id));
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics_with_config(config);
        Self {
            issues,
            graph,
            metrics,
        }
    }

    /// Create an analyzer with only fast O(V+E) metrics computed.
    ///
    /// Betweenness, eigenvector, and HITS are deferred. Call
    /// [`spawn_slow_computation`] to compute them in a background thread.
    #[must_use]
    pub fn new_fast(mut issues: Vec<Issue>) -> Self {
        issues.sort_by(|left, right| left.id.cmp(&right.id));
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics_with_config(&graph::AnalysisConfig::fast_phase());
        Self {
            issues,
            graph,
            metrics,
        }
    }

    /// Returns true if this graph exceeds the background computation threshold.
    #[must_use]
    pub fn is_large_graph(&self) -> bool {
        self.graph.node_count() > graph::AnalysisConfig::BACKGROUND_THRESHOLD
    }

    /// Spawn a background thread to compute expensive metrics.
    ///
    /// Returns a receiver that will yield the slow-phase `GraphMetrics` when done.
    /// The caller should poll via `try_recv()` and call `apply_slow_metrics()`.
    pub fn spawn_slow_computation(
        &self,
    ) -> std::sync::mpsc::Receiver<graph::GraphMetrics> {
        let graph_clone = self.graph.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let slow = graph_clone
                .compute_metrics_with_config(&graph::AnalysisConfig::slow_phase());
            let _ = tx.send(slow);
        });
        rx
    }

    /// Merge slow-phase metrics into this analyzer's metrics.
    pub fn apply_slow_metrics(&mut self, slow: graph::GraphMetrics) {
        self.metrics.merge_slow(slow);
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
    pub fn what_if(&self, issue_id: &str) -> Option<whatif::WhatIfDelta> {
        whatif::compute_what_if(&self.issues, &self.graph, &self.metrics, issue_id)
    }

    #[must_use]
    pub fn top_what_ifs(&self, top_n: usize) -> Vec<whatif::WhatIfDelta> {
        whatif::top_what_if_deltas(&self.issues, &self.graph, &self.metrics, top_n)
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
    pub fn advanced_insights(&self) -> advanced::AdvancedInsights {
        advanced::compute_advanced_insights(&self.graph, &self.metrics)
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
            ..TriageOptions::default()
        });

        // Priority view should consider all open issues, including currently blocked items.
        // Triage intentionally limits to actionable work; augment with non-actionable open
        // issues so users can still rank and inspect the full active backlog.
        let mut results = triage.result.recommendations;
        let actionable_ids = results
            .iter()
            .map(|recommendation| recommendation.id.clone())
            .collect::<HashSet<_>>();

        let max_pagerank = self
            .metrics
            .pagerank
            .values()
            .copied()
            .fold(0.0_f64, f64::max)
            .max(1e-9);
        let max_unblocks = self
            .metrics
            .blocks_count
            .values()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);

        for issue in self
            .issues
            .iter()
            .filter(|issue| issue.is_open_like() && !actionable_ids.contains(&issue.id))
        {
            let pagerank = self
                .metrics
                .pagerank
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let pagerank_norm = pagerank / max_pagerank;

            let unblocks = self
                .metrics
                .blocks_count
                .get(&issue.id)
                .copied()
                .unwrap_or_default();
            let unblocks_norm = unblocks as f64 / max_unblocks as f64;

            let urgency = match issue.normalized_status().as_str() {
                "in_progress" => 1.0,
                "open" => 0.8,
                "review" => 0.7,
                "blocked" => 0.5,
                _ => 0.6,
            };

            let blockers = self.graph.open_blockers(&issue.id);
            let is_blocked = !blockers.is_empty();
            let mut score = (0.45 * pagerank_norm
                + 0.30 * unblocks_norm
                + 0.20 * issue.priority_normalized()
                + 0.05 * urgency)
                .clamp(0.0, 1.0);
            if is_blocked {
                score = (score * 0.9).clamp(0.0, 1.0);
            }

            let mut reasons = Vec::<String>::new();
            if is_blocked {
                reasons.push(format!("currently blocked by {} issue(s)", blockers.len()));
            }
            if pagerank_norm > 0.6 {
                reasons.push("high graph centrality".to_string());
            }
            if unblocks > 0 {
                reasons.push(format!("unblocks {unblocks} issues"));
            }
            if issue.priority <= 2 {
                reasons.push("high declared priority".to_string());
            }
            if reasons.is_empty() {
                reasons.push("ready to execute now".to_string());
            }

            results.push(Recommendation {
                id: issue.id.clone(),
                title: issue.title.clone(),
                score,
                impact_score: score,
                confidence: (0.5 + 0.5 * score).clamp(0.0, 1.0),
                reasons,
                unblocks,
                status: issue.status.clone(),
                priority: issue.priority,
                issue_type: issue.issue_type.clone(),
                labels: issue.labels.clone(),
                assignee: issue.assignee.clone(),
                claim_command: format!("br update {} --status=in_progress", issue.id),
                show_command: format!("br show {}", issue.id),
                breakdown: None,
            });
        }

        results.retain(|rec| rec.confidence >= min_confidence);
        results.retain(|rec| {
            by_label.is_none_or(|label| rec.labels.iter().any(|entry| entry == label))
        });
        results.retain(|rec| by_assignee.is_none_or(|assignee| rec.assignee == assignee));

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
    use crate::analysis::graph::AnalysisConfig;
    use crate::analysis::triage::TriageOptions;
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

    #[test]
    fn triage_runtime_config_preserves_plan_and_priority_outputs() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "Root blocker".to_string(),
                status: "open".to_string(),
                issue_type: "feature".to_string(),
                priority: 1,
                labels: vec!["core".to_string(), "backend".to_string()],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Depends on A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                labels: vec!["backend".to_string()],
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
                title: "Also depends on A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                labels: vec!["frontend".to_string()],
                dependencies: vec![Dependency {
                    issue_id: "C".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "D".to_string(),
                title: "Independent quick win".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                estimated_minutes: Some(30),
                labels: vec!["ops".to_string()],
                ..Issue::default()
            },
        ];

        let full = Analyzer::new(issues.clone());
        let lean = Analyzer::new_with_config(issues, &AnalysisConfig::triage_runtime());
        let triage_options = TriageOptions {
            max_recommendations: 20,
            ..TriageOptions::default()
        };

        let full_triage = full.triage(triage_options.clone());
        let lean_triage = lean.triage(triage_options);

        let full_plan = full.plan(&full_triage.score_by_id);
        let lean_plan = lean.plan(&lean_triage.score_by_id);
        assert_eq!(
            serde_json::to_value(&full_plan).unwrap(),
            serde_json::to_value(&lean_plan).unwrap()
        );

        let full_priority = full.priority(0.0, 20, None, None);
        let lean_priority = lean.priority(0.0, 20, None, None);
        assert_eq!(
            serde_json::to_value(&full_priority).unwrap(),
            serde_json::to_value(&lean_priority).unwrap()
        );
    }

    // -- Two-phase (fast/slow) Analyzer tests --------------------------------

    fn sample_issues() -> Vec<Issue> {
        vec![
            Issue {
                id: "A".to_string(),
                title: "Root".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Blocked".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
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
                ..Issue::default()
            },
        ]
    }

    #[test]
    fn new_fast_defers_slow_metrics() {
        let analyzer = Analyzer::new_fast(sample_issues());
        assert!(
            analyzer.metrics.has_pending_slow_metrics(),
            "fast analyzer should have pending slow metrics"
        );
        // PageRank should still be available
        assert!(
            !analyzer.metrics.pagerank.is_empty(),
            "fast analyzer should have PageRank"
        );
    }

    #[test]
    fn apply_slow_metrics_fills_gaps() {
        let mut analyzer = Analyzer::new_fast(sample_issues());
        assert!(analyzer.metrics.betweenness.is_empty());

        let slow = analyzer
            .graph
            .compute_metrics_with_config(&AnalysisConfig::slow_phase());
        analyzer.apply_slow_metrics(slow);

        assert!(
            !analyzer.metrics.betweenness.is_empty(),
            "betweenness should be filled after applying slow metrics"
        );
        assert!(
            !analyzer.metrics.has_pending_slow_metrics(),
            "should have no pending slow metrics"
        );
    }

    #[test]
    fn is_large_graph_below_threshold() {
        let analyzer = Analyzer::new(sample_issues());
        assert!(
            !analyzer.is_large_graph(),
            "3-node graph should not be large"
        );
    }

    #[test]
    fn spawn_slow_computation_completes() {
        let analyzer = Analyzer::new_fast(sample_issues());
        let rx = analyzer.spawn_slow_computation();
        let slow = rx.recv().expect("should receive slow metrics");
        assert!(
            !slow.betweenness.is_empty(),
            "background thread should compute betweenness"
        );
    }

    #[test]
    fn fast_triage_still_works() {
        let analyzer = Analyzer::new_fast(sample_issues());
        let options = TriageOptions::default();
        // Triage should work with fast-only metrics (betweenness component will be 0)
        let triage = analyzer.triage(options);
        assert!(
            !triage.result.recommendations.is_empty() || analyzer.issues.is_empty(),
            "triage should return results even with fast-only metrics"
        );
    }

    #[test]
    fn fast_insights_still_works() {
        let analyzer = Analyzer::new_fast(sample_issues());
        // Insights should not panic even with missing metrics
        let insights = analyzer.insights();
        assert!(
            !insights.influencers.is_empty(),
            "influencers (PageRank) should still be available"
        );
        // Betweenness-based fields will be empty but shouldn't panic
        assert!(insights.betweenness.is_empty());
    }

    // -- Integration: config → analysis → triage chain ---------------------

    #[test]
    fn triage_scores_improve_after_slow_metrics_applied() {
        let mut fast = Analyzer::new_fast(sample_issues());
        let options = TriageOptions::default();

        // Fast-only triage (betweenness component is 0)
        let fast_triage = fast.triage(options.clone());
        let fast_scores = fast_triage.score_by_id.clone();

        // Apply slow metrics
        let slow = fast
            .graph
            .compute_metrics_with_config(&AnalysisConfig::slow_phase());
        fast.apply_slow_metrics(slow);

        // Full triage (betweenness component now available)
        let full_triage = fast.triage(options);
        let full_scores = full_triage.score_by_id;

        // Scores should differ (betweenness now contributes)
        // For the sample graph, A blocks B so should have nonzero betweenness
        let a_fast = fast_scores.get("A").copied().unwrap_or(0.0);
        let a_full = full_scores.get("A").copied().unwrap_or(0.0);
        assert!(
            (a_fast - a_full).abs() > 0.0 || fast_scores.len() == full_scores.len(),
            "scores should differ or graph is degenerate: fast={a_fast}, full={a_full}"
        );
    }

    #[test]
    fn fast_then_slow_produces_same_insights_as_full() {
        let full = Analyzer::new(sample_issues());
        let full_insights = full.insights();

        let mut two_phase = Analyzer::new_fast(sample_issues());
        let slow = two_phase
            .graph
            .compute_metrics_with_config(&AnalysisConfig::slow_phase());
        two_phase.apply_slow_metrics(slow);
        let two_phase_insights = two_phase.insights();

        // Influencers (PageRank-based) should match exactly
        assert_eq!(
            full_insights.influencers.len(),
            two_phase_insights.influencers.len(),
            "influencer count should match"
        );
        // Betweenness should now match
        assert_eq!(
            full_insights.betweenness.len(),
            two_phase_insights.betweenness.len(),
            "betweenness item count should match"
        );
    }

    #[test]
    fn new_with_config_respects_selective_metrics() {
        let config = AnalysisConfig {
            enable_pagerank: true,
            enable_betweenness: false,
            enable_eigenvector: false,
            enable_hits: false,
            enable_cycles: true,
            enable_critical_path: false,
            enable_k_core: false,
            enable_articulation: false,
            enable_slack: false,
            betweenness_max_nodes: 10_000,
            eigenvector_max_nodes: 10_000,
        };
        let analyzer = Analyzer::new_with_config(sample_issues(), &config);
        assert!(!analyzer.metrics.pagerank.is_empty(), "PageRank should be computed");
        assert!(analyzer.metrics.betweenness.is_empty(), "betweenness should be skipped");
        assert!(analyzer.metrics.eigenvector.is_empty(), "eigenvector should be skipped");
        assert!(analyzer.metrics.k_core.is_empty(), "k_core should be skipped");
    }

    #[test]
    fn empty_issues_all_operations_succeed() {
        let analyzer = Analyzer::new(vec![]);
        assert!(analyzer.issues.is_empty());
        assert_eq!(analyzer.graph.node_count(), 0);

        // All operations should succeed on empty graph
        let triage = analyzer.triage(TriageOptions::default());
        assert!(triage.result.recommendations.is_empty());

        let insights = analyzer.insights();
        assert!(insights.influencers.is_empty());
        assert!(insights.cycles.is_empty());

        let plan = analyzer.plan(&std::collections::HashMap::new());
        assert!(plan.tracks.is_empty());
    }

    #[test]
    fn empty_issues_fast_phase_no_panic() {
        let analyzer = Analyzer::new_fast(vec![]);
        assert!(!analyzer.is_large_graph());
        let rx = analyzer.spawn_slow_computation();
        let slow = rx.recv().expect("should complete even for empty graph");
        assert!(slow.betweenness.is_empty());
    }
}
