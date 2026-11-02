use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::model::Issue;

use super::graph::{GraphMetrics, IssueGraph};

/// The delta produced by simulating completion of one issue.
#[derive(Debug, Clone, Serialize)]
pub struct WhatIfDelta {
    pub issue_id: String,
    pub title: String,
    /// Immediate blocking dependents that become dependency-unblocked.
    pub direct_unblocks: Vec<String>,
    /// All issues newly dependency-unblocked, including inherited parent blocking.
    /// Only the target is completed; newly unblocked issues remain open.
    pub transitive_unblocks: Vec<String>,
    /// Heuristic of two days saved per newly dependency-unblocked issue.
    pub estimated_days_saved: f64,
    /// Before-minus-after PageRank sum over remaining open issues, with the
    /// completed node removed from the counterfactual dependency graph.
    /// Zero when baseline PageRank is disabled or unavailable.
    pub pagerank_delta: f64,
    /// Original cyclic components made entirely acyclic by removing this node.
    /// A component with a surviving subcycle is not counted as broken.
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
    let original_unblocked = graph.dependency_unblocked_ids().into_iter().collect();
    let mut delta = completion_unblocks(graph, &original_unblocked, issue);
    if issue.is_open_like() {
        refine_metrics(issues, graph, metrics, &mut delta);
    }
    Some(delta)
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

    let original_unblocked = graph.dependency_unblocked_ids().into_iter().collect();
    let mut best = BTreeMap::new();
    for issue in issues
        .iter()
        .filter(|i| i.is_open_like())
        .filter_map(|issue| graph.issue(&issue.id))
    {
        let delta = completion_unblocks(graph, &original_unblocked, issue);
        let rank = (
            Reverse(delta.transitive_unblocks.len()),
            Reverse(delta.direct_unblocks.len()),
            delta.issue_id.clone(),
        );
        best.insert(rank, delta);
        // A deep hierarchy can give every candidate a large released-ID list.
        // Retain only the requested results instead of all those lists at once.
        if best.len() > top_n {
            best.pop_last();
        }
    }
    let mut deltas: Vec<WhatIfDelta> = best.into_values().collect();

    // Ranking depends only on the readiness changes above. Compute the actual
    // counterfactual metrics only for results the caller will receive.
    for delta in &mut deltas {
        refine_metrics(issues, graph, metrics, delta);
    }
    deltas
}

fn completion_unblocks(
    graph: &IssueGraph,
    original_unblocked: &HashSet<String>,
    issue: &Issue,
) -> WhatIfDelta {
    let mut delta = WhatIfDelta {
        issue_id: issue.id.clone(),
        title: issue.title.clone(),
        direct_unblocks: Vec::new(),
        transitive_unblocks: Vec::new(),
        estimated_days_saved: 0.0,
        pagerank_delta: 0.0,
        cycles_broken: 0,
    };
    if issue.is_closed_like() {
        return delta;
    }

    // This is dependency readiness, not the status-gated actionable set: a
    // blocked/deferred issue can lose its prerequisites without changing status.
    // The shared graph algorithm also accounts for inherited parent blocking.
    delta.transitive_unblocks = graph
        .dependency_unblocked_ids_after_closing(&issue.id)
        .into_iter()
        .filter(|id| id != &issue.id && !original_unblocked.contains(id))
        .collect();
    delta.transitive_unblocks.sort();
    delta.direct_unblocks = graph
        .dependents(&issue.id)
        .into_iter()
        .filter(|id| delta.transitive_unblocks.binary_search(id).is_ok())
        .collect();
    delta.direct_unblocks.sort();
    delta.estimated_days_saved = delta.transitive_unblocks.len() as f64 * 2.0;
    delta
}

fn refine_metrics(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    delta: &mut WhatIfDelta,
) {
    delta.cycles_broken = metrics
        .cycles
        .iter()
        .filter(|component| {
            component.contains(&delta.issue_id)
                && residual_component_is_acyclic(graph, component, &delta.issue_id)
        })
        .count();

    if !metrics.config.enable_pagerank || metrics.pagerank.is_empty() {
        return;
    }
    let before_sum: Option<f64> = issues
        .iter()
        .filter(|issue| issue.is_open_like() && issue.id != delta.issue_id)
        .map(|issue| metrics.pagerank.get(&issue.id).copied())
        .sum();
    let Some(before_sum) = before_sum else {
        // An incomplete baseline is unavailable, not a set of zero scores.
        return;
    };

    // Merely changing status leaves the PageRank topology unchanged. Removing
    // the completed node measures the centrality redistribution requested by
    // this counterfactual, without computing unrelated expensive metrics.
    let remaining: Vec<Issue> = issues
        .iter()
        .filter(|issue| issue.id != delta.issue_id)
        .cloned()
        .collect();
    let after = IssueGraph::build(&remaining).compute_pagerank();
    let after_sum: f64 = remaining
        .iter()
        .filter(|issue| issue.is_open_like())
        .map(|issue| after.get(&issue.id).copied().unwrap_or(0.0))
        .sum();
    delta.pagerank_delta = before_sum - after_sum;
}

fn residual_component_is_acyclic(
    graph: &IssueGraph,
    component: &[String],
    completed: &str,
) -> bool {
    let remaining: HashSet<&str> = component
        .iter()
        .map(String::as_str)
        .filter(|id| *id != completed)
        .collect();
    let mut pending_blockers: HashMap<&str, usize> = remaining
        .iter()
        .map(|&id| {
            let blockers = graph
                .blockers(id)
                .iter()
                .filter(|blocker| remaining.contains(blocker.as_str()))
                .count();
            (id, blockers)
        })
        .collect();
    let mut ready = pending_blockers
        .iter()
        .filter_map(|(&id, &count)| (count == 0).then_some(id))
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    while let Some(id) = ready.pop() {
        visited += 1;
        for dependent in graph.dependents(id) {
            if let Some(&dependent) = remaining.get(dependent.as_str()) {
                let count = pending_blockers
                    .get_mut(dependent)
                    .expect("component member");
                *count -= 1;
                if *count == 0 {
                    ready.push(dependent);
                }
            }
        }
    }
    visited == remaining.len()
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

    fn add_dependency(issue: &mut Issue, target: &str, kind: &str) {
        issue.dependencies.push(Dependency {
            issue_id: issue.id.clone(),
            depends_on_id: target.to_string(),
            dep_type: kind.to_string(),
            ..Dependency::default()
        });
    }

    fn assert_same_delta(actual: &WhatIfDelta, expected: &WhatIfDelta) {
        assert_eq!(actual.issue_id, expected.issue_id);
        assert_eq!(actual.title, expected.title);
        assert_eq!(actual.direct_unblocks, expected.direct_unblocks);
        assert_eq!(actual.transitive_unblocks, expected.transitive_unblocks);
        assert_eq!(actual.cycles_broken, expected.cycles_broken);
        assert!((actual.estimated_days_saved - expected.estimated_days_saved).abs() < 1e-12);
        assert!((actual.pagerank_delta - expected.pagerank_delta).abs() < 1e-12);
    }

    #[test]
    fn what_if_single_blocker_unblocks_dependent() {
        let issues = vec![make_issue("A", "open"), make_blocked("B", "A")];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").unwrap();
        assert_eq!(delta.issue_id, "A");
        assert_eq!(delta.direct_unblocks, vec!["B"]);
        assert_eq!(delta.transitive_unblocks, vec!["B"]);
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
        assert_eq!(delta.transitive_unblocks, vec!["B"]);
    }

    #[test]
    fn what_if_direct_unblocks_require_resolved_inherited_and_own_blockers() {
        let mut inherited = make_blocked("INHERITED", "A");
        add_dependency(&mut inherited, "PARENT", "parent-child");
        let mut multiple = make_blocked("MULTIPLE", "A");
        add_dependency(&mut multiple, "OTHER", "waits-for");
        let mut closed = make_blocked("CLOSED", "A");
        closed.status = "closed".to_string();
        let mut tombstone = make_blocked("TOMBSTONE", "A");
        tombstone.status = "tombstone".to_string();
        let mut deferred = make_blocked("DEFERRED", "A");
        deferred.status = "deferred".to_string();
        let issues = vec![
            make_issue("A", "open"),
            make_issue("OTHER", "open"),
            make_blocked("PARENT", "OTHER"),
            inherited,
            multiple,
            closed,
            tombstone,
            deferred,
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").expect("known target");
        assert_eq!(delta.direct_unblocks, vec!["DEFERRED"]);
        assert_eq!(delta.transitive_unblocks, vec!["DEFERRED"]);
        assert_eq!(delta.estimated_days_saved, 2.0);
        assert_eq!(graph.issue("A").expect("original target").status, "open");
    }

    #[test]
    fn what_if_inherited_descendants_are_released_without_completing_their_parent() {
        let mut child = make_issue("CHILD", "deferred");
        add_dependency(&mut child, "PARENT", "parent-child");
        let mut grandchild = make_issue("GRANDCHILD", "blocked");
        add_dependency(&mut grandchild, "CHILD", "parent-child");
        let issues = vec![
            make_issue("A", "open"),
            make_blocked("PARENT", "A"),
            child,
            grandchild,
            make_blocked("STILL-WAITING", "PARENT"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let delta = compute_what_if(&issues, &graph, &metrics, "A").expect("known target");
        assert_eq!(delta.direct_unblocks, vec!["PARENT"]);
        assert_eq!(
            delta.transitive_unblocks,
            vec!["CHILD", "GRANDCHILD", "PARENT"]
        );
        assert_eq!(delta.estimated_days_saved, 6.0);
    }

    #[test]
    fn what_if_already_closed_targets_are_noops_even_in_structural_cycles() {
        for status in ["closed", "tombstone"] {
            let mut target = make_issue("A", status);
            add_dependency(&mut target, "B", "blocks");
            let issues = vec![target, make_blocked("B", "A")];
            let graph = IssueGraph::build(&issues);
            let metrics = graph.compute_metrics();
            assert_eq!(metrics.cycles, vec![vec!["A", "B"]]);

            let delta = compute_what_if(&issues, &graph, &metrics, "A").expect("known target");
            assert!(delta.direct_unblocks.is_empty());
            assert!(delta.transitive_unblocks.is_empty());
            assert_eq!(delta.estimated_days_saved, 0.0);
            assert_eq!(delta.pagerank_delta, 0.0);
            assert_eq!(delta.cycles_broken, 0);
        }
    }

    #[test]
    fn what_if_pagerank_matches_an_independent_removed_node_graph() {
        let issues = vec![
            make_issue("A", "open"),
            make_blocked("B", "A"),
            make_issue("C", "closed"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        // The independently constructed residual graph has two isolated nodes:
        // B's old dependency is dangling after A is removed, and each rank is 1/2.
        let residual = vec![make_issue("B", "blocked"), make_issue("C", "closed")];
        let residual_metrics = IssueGraph::build(&residual).compute_metrics();
        let expected = metrics.pagerank["B"] - residual_metrics.pagerank["B"];

        let delta = compute_what_if(&issues, &graph, &metrics, "A").expect("known target");
        assert!((delta.pagerank_delta - expected).abs() < 1e-9);
        // Analytic PageRank with damping 0.85 gives B = 20/77 before removal.
        assert!((delta.pagerank_delta - (20.0 / 77.0 - 0.5)).abs() < 1e-6);
        assert!(delta.pagerank_delta.abs() > 0.2);
    }

    #[test]
    fn what_if_does_not_compare_missing_pagerank_to_computed_scores() {
        let issues = vec![make_issue("A", "open"), make_blocked("B", "A")];
        let graph = IssueGraph::build(&issues);
        let mut disabled_config = super::super::graph::AnalysisConfig::fast_phase();
        disabled_config.enable_pagerank = false;
        let disabled = graph.compute_metrics_with_config(&disabled_config);
        assert!(disabled.pagerank.is_empty());
        let delta = compute_what_if(&issues, &graph, &disabled, "A").expect("known target");
        assert_eq!(delta.pagerank_delta, 0.0);
        assert_eq!(delta.direct_unblocks, vec!["B"]);

        let mut unavailable = graph.compute_metrics();
        unavailable.pagerank.remove("B");
        let delta = compute_what_if(&issues, &graph, &unavailable, "A").expect("known target");
        assert_eq!(delta.pagerank_delta, 0.0);
        unavailable.pagerank.clear();
        let delta = compute_what_if(&issues, &graph, &unavailable, "A").expect("known target");
        assert_eq!(delta.pagerank_delta, 0.0);
    }

    #[test]
    fn what_if_does_not_claim_a_component_with_a_surviving_cycle_is_broken() {
        let mut middle = make_blocked("B", "A");
        add_dependency(&mut middle, "C", "blocks");
        let issues = vec![
            make_blocked("A", "B"),
            middle,
            make_blocked("C", "B"),
            make_blocked("SELF", "SELF"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        assert_eq!(metrics.cycles, vec![vec!["A", "B", "C"], vec!["SELF"]]);

        // Removing an end of the figure-eight leaves the other two-node cycle.
        for id in ["A", "C"] {
            let delta = compute_what_if(&issues, &graph, &metrics, id).expect("known target");
            assert_eq!(
                delta.cycles_broken, 0,
                "surviving cycle after removing {id}"
            );
        }
        let middle = compute_what_if(&issues, &graph, &metrics, "B").expect("known target");
        assert_eq!(middle.cycles_broken, 1);
        let self_loop = compute_what_if(&issues, &graph, &metrics, "SELF").expect("known target");
        assert_eq!(self_loop.cycles_broken, 1);
    }

    #[test]
    fn what_if_splitting_one_cyclic_component_into_two_does_not_break_it() {
        let mut target = make_blocked("A", "B");
        add_dependency(&mut target, "D", "blocks");
        let mut left = make_blocked("B", "C");
        add_dependency(&mut left, "A", "blocks");
        let mut right = make_blocked("D", "E");
        add_dependency(&mut right, "A", "blocks");
        let issues = vec![
            target,
            left,
            make_blocked("C", "B"),
            right,
            make_blocked("E", "D"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        assert_eq!(metrics.cycles, vec![vec!["A", "B", "C", "D", "E"]]);

        let delta = compute_what_if(&issues, &graph, &metrics, "A").expect("known target");
        assert_eq!(delta.cycles_broken, 0);
    }

    #[test]
    fn top_what_ifs_match_single_simulations_and_ignore_input_order() {
        let mut inherited = make_issue("CHILD", "open");
        add_dependency(&mut inherited, "PARENT", "parent-child");
        let mut issues = vec![
            make_issue("A", "open"),
            make_issue("B", "open"),
            make_issue("C", "open"),
            make_issue("Z", "open"),
            make_blocked("PARENT", "A"),
            inherited,
            make_blocked("SECOND", "B"),
            make_blocked("THIRD", "C"),
            make_blocked("FOURTH", "Z"),
            make_blocked("FIFTH", "Z"),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let expected = top_what_if_deltas(&issues, &graph, &metrics, 3);
        // Z and A release the same total, but Z has more direct unblocks.
        // B and C tie on both counts, so their IDs decide the final slot.
        assert_eq!(
            expected
                .iter()
                .map(|delta| delta.issue_id.as_str())
                .collect::<Vec<_>>(),
            vec!["Z", "A", "B"]
        );
        for delta in &expected {
            let single = compute_what_if(&issues, &graph, &metrics, &delta.issue_id)
                .expect("selected candidate");
            assert_same_delta(delta, &single);
        }

        issues.reverse();
        let reversed_graph = IssueGraph::build(&issues);
        let reversed_metrics = reversed_graph.compute_metrics();
        let reversed = top_what_if_deltas(&issues, &reversed_graph, &reversed_metrics, 3);
        assert_eq!(reversed.len(), expected.len());
        for (actual, expected) in reversed.iter().zip(&expected) {
            assert_same_delta(actual, expected);
        }
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
