use serde::Serialize;

use crate::model::Issue;

use super::graph::{GraphMetrics, IssueGraph};

/// The delta produced by simulating completion of one issue.
#[derive(Debug, Clone, Serialize)]
pub struct WhatIfDelta {
    pub issue_id: String,
    pub title: String,
    /// Issues immediately unblocked (their only remaining blocker was this issue).
    pub direct_unblocks: Vec<String>,
    /// All downstream issues transitively unblocked.
    pub transitive_unblocks: Vec<String>,
    /// Estimated days saved based on transitive unblock count and average depth.
    pub estimated_days_saved: f64,
    /// Change in sum-of-pagerank for remaining open issues (positive = graph got healthier).
    pub pagerank_delta: f64,
    /// Number of dependency cycles broken by completing this issue.
    pub cycles_broken: usize,
}

/// Compute the what-if delta for completing a single issue.
pub fn compute_what_if(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    issue_id: &str,
) -> Option<WhatIfDelta> {
    let issue = graph.issue(issue_id)?;
    let title = issue.title.clone();

    // Direct unblocks: issues where this is the sole open blocker.
    let dependents = graph.dependents(issue_id);
    let mut direct_unblocks = Vec::new();
    for dep_id in &dependents {
        let blockers = graph.open_blockers(dep_id);
        // If the only open blocker is the target issue, it gets unblocked.
        if blockers.len() == 1 && blockers[0] == issue_id {
            direct_unblocks.push(dep_id.clone());
        }
    }
    direct_unblocks.sort();

    // Transitive unblocks: simulate removing the issue and see which issues
    // lose their last open blocker. This is the dependency-structural view
    // (`dependency_unblocked_ids`), not the status-gated `actionable_ids`: a
    // dependent parked as `blocked`/`deferred` is still "unblocked" by closing
    // its blocker even though it needs a status change before it is ready.
    // Build a modified issue list with the target marked as closed.
    let modified_issues: Vec<Issue> = issues
        .iter()
        .map(|i| {
            if i.id == issue_id {
                let mut closed = i.clone();
                closed.status = "closed".to_string();
                closed
            } else {
                i.clone()
            }
        })
        .collect();

    let modified_graph = IssueGraph::build(&modified_issues);
    let modified_actionable: std::collections::HashSet<String> = modified_graph
        .dependency_unblocked_ids()
        .into_iter()
        .collect();
    let original_actionable: std::collections::HashSet<String> =
        graph.dependency_unblocked_ids().into_iter().collect();

    let mut transitive_unblocks: Vec<String> = modified_actionable
        .difference(&original_actionable)
        .filter(|id| *id != issue_id)
        .cloned()
        .collect();
    transitive_unblocks.sort();

    // Estimated days saved: heuristic based on unblock depth.
    // Each transitively unblocked issue saves ~2 days of blocked time (conservative estimate).
    let estimated_days_saved = transitive_unblocks.len() as f64 * 2.0;

    // PageRank delta: compare sum of pagerank for open issues before/after.
    let modified_metrics = modified_graph.compute_metrics();
    let before_pr_sum: f64 = issues
        .iter()
        .filter(|i| i.is_open_like() && i.id != issue_id)
        .map(|i| metrics.pagerank.get(&i.id).copied().unwrap_or(0.0))
        .sum();
    let after_pr_sum: f64 = modified_issues
        .iter()
        .filter(|i| i.is_open_like() && i.id != issue_id)
        .map(|i| modified_metrics.pagerank.get(&i.id).copied().unwrap_or(0.0))
        .sum();
    let pagerank_delta = before_pr_sum - after_pr_sum;

    // Cycles broken: how many cycles contained this issue.
    let cycles_broken = metrics
        .cycles
        .iter()
        .filter(|cycle| cycle.contains(&issue_id.to_string()))
        .count();

    Some(WhatIfDelta {
        issue_id: issue_id.to_string(),
        title,
        direct_unblocks,
        transitive_unblocks,
        estimated_days_saved,
        pagerank_delta,
        cycles_broken,
    })
}

/// Compute what-if deltas for all open issues and return the top N by impact.
pub fn top_what_if_deltas(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    top_n: usize,
) -> Vec<WhatIfDelta> {
    if top_n == 0 {
        return Vec::new();
    }

    let mut deltas: Vec<WhatIfDelta> = issues
        .iter()
        .filter(|i| i.is_open_like())
        .filter_map(|i| compute_what_if(issues, graph, metrics, &i.id))
        .collect();

    // Sort by transitive unblocks (desc), then direct unblocks (desc), then id (asc).
    deltas.sort_by(|a, b| {
        b.transitive_unblocks
            .len()
            .cmp(&a.transitive_unblocks.len())
            .then_with(|| b.direct_unblocks.len().cmp(&a.direct_unblocks.len()))
            .then_with(|| a.issue_id.cmp(&b.issue_id))
    });
    deltas.truncate(top_n);
    deltas
}

#[cfg(test)]
mod tests {
    use crate::model::{Dependency, Issue};

    use super::*;

    fn make_issue(id: &str, status: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: status.to_string(),
            issue_type: "task".to_string(),
            priority: 2,
            ..Issue::default()
        }
    }

    fn make_blocked(id: &str, depends_on: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "blocked".to_string(),
            issue_type: "task".to_string(),
            priority: 2,
            dependencies: vec![Dependency {
                issue_id: id.to_string(),
                depends_on_id: depends_on.to_string(),
                dep_type: "blocks".to_string(),
                ..Dependency::default()
            }],
            ..Issue::default()
        }
    }

    #[test]
    fn what_if_single_blocker_unblocks_dependent() {
        let issues = vec![make_issue("A", "open"), make_blocked("B", "A")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert_eq!(delta.issue_id, "A");
        assert_eq!(delta.direct_unblocks, vec!["B"]);
        assert!(delta.transitive_unblocks.contains(&"B".to_string()));
        assert!(delta.estimated_days_saved >= 2.0);
    }

    #[test]
    fn what_if_chain_produces_transitive_unblocks() {
        // A blocks B, B blocks C
        let issues = vec![
            make_issue("A", "open"),
            make_blocked("B", "A"),
            Issue {
                id: "C".to_string(),
                title: "Issue C".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![Dependency {
                    issue_id: "C".to_string(),
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert_eq!(delta.direct_unblocks, vec!["B"]);
        // B becomes actionable (not C yet, since B still needs to be completed)
        assert!(delta.transitive_unblocks.contains(&"B".to_string()));
    }

    #[test]
    fn what_if_no_dependents_returns_empty_unblocks() {
        let issues = vec![make_issue("A", "open"), make_issue("B", "open")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert!(delta.direct_unblocks.is_empty());
        assert!(delta.transitive_unblocks.is_empty());
        assert!((delta.estimated_days_saved - 0.0).abs() < 1e-6);
    }

    #[test]
    fn what_if_nonexistent_issue_returns_none() {
        let issues = vec![make_issue("A", "open")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        assert!(compute_what_if(&issues, &graph, &metrics, "X").is_none());
    }

    #[test]
    fn what_if_empty_graph() {
        let issues: Vec<Issue> = vec![];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        assert!(compute_what_if(&issues, &graph, &metrics, "A").is_none());
        let deltas = top_what_if_deltas(&issues, &graph, &metrics, 5);
        assert!(deltas.is_empty());
    }

    #[test]
    fn what_if_single_node_graph() {
        let issues = vec![make_issue("A", "open")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert!(delta.direct_unblocks.is_empty());
        assert!(delta.transitive_unblocks.is_empty());
        assert!((delta.estimated_days_saved - 0.0).abs() < 1e-6);
    }

    #[test]
    fn top_what_if_deltas_sorts_by_impact() {
        // A blocks 3, B blocks 1, C blocks nothing
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "open"),
            make_issue("C", "open"),
            make_blocked("D1", "A"),
            make_blocked("D2", "A"),
            make_blocked("D3", "A"),
            make_blocked("E1", "B"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let deltas = top_what_if_deltas(&issues, &graph, &metrics, 3);
        assert!(!deltas.is_empty());
        // A should be first (most transitive unblocks)
        assert_eq!(deltas[0].issue_id, "A");
        // B should be second
        assert_eq!(deltas[1].issue_id, "B");
    }

    #[test]
    fn what_if_respects_top_n_limit() {
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "open"),
            make_issue("C", "open"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let deltas = top_what_if_deltas(&issues, &graph, &metrics, 2);
        assert!(deltas.len() <= 2);
    }

    #[test]
    fn what_if_zero_top_n_returns_no_results() {
        let issues = vec![
            make_issue("A", "open"),
            make_issue("B", "open"),
            make_blocked("C", "A"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let deltas = top_what_if_deltas(&issues, &graph, &metrics, 0);
        assert!(deltas.is_empty());
    }

    #[test]
    fn what_if_cycle_detection() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "In cycle".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "In cycle".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert!(
            delta.cycles_broken > 0,
            "A is in a cycle, should break at least 1"
        );
    }

    #[test]
    fn what_if_serializes_to_json() {
        let delta = WhatIfDelta {
            issue_id: "A".to_string(),
            title: "Test".to_string(),
            direct_unblocks: vec!["B".to_string()],
            transitive_unblocks: vec!["B".to_string(), "C".to_string()],
            estimated_days_saved: 4.0,
            pagerank_delta: 0.05,
            cycles_broken: 0,
        };

        let json = serde_json::to_value(&delta).unwrap();
        assert_eq!(json["issue_id"], "A");
        assert_eq!(json["direct_unblocks"], serde_json::json!(["B"]));
        assert_eq!(json["estimated_days_saved"], 4.0);
        assert_eq!(json["cycles_broken"], 0);
    }

    #[test]
    fn what_if_large_graph_correctness() {
        // Build a chain of 100 issues: I0 → I1 → I2 → ... → I99
        let mut issues = vec![make_issue("I0", "open")];
        for i in 1..100 {
            issues.push(make_blocked(&format!("I{i}"), &format!("I{}", i - 1)));
        }

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        // Completing I0 should directly unblock I1
        let delta = compute_what_if(&issues, &graph, &metrics, "I0").unwrap();
        assert_eq!(delta.direct_unblocks, vec!["I1"]);
        // Transitively only I1 becomes actionable (I2 still blocked by I1)
        assert!(delta.transitive_unblocks.contains(&"I1".to_string()));
        assert!(
            !delta.transitive_unblocks.contains(&"I2".to_string()),
            "I2 should still be blocked by I1"
        );

        // Top deltas: I0 should rank high (most downstream impact)
        let deltas = top_what_if_deltas(&issues, &graph, &metrics, 5);
        assert!(!deltas.is_empty());
        // All scores should be valid
        for d in &deltas {
            assert!(d.estimated_days_saved >= 0.0);
        }
    }
}
