use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::analysis::graph::{GraphMetrics, IssueGraph};
use crate::model::Issue;

#[derive(Debug, Clone, Serialize)]
pub struct QuickPick {
    pub id: String,
    pub title: String,
    pub score: f64,
    pub reasons: Vec<String>,
    pub unblocks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub id: String,
    pub title: String,
    pub score: f64,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub unblocks: usize,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub labels: Vec<String>,
    pub assignee: String,
    pub claim_command: String,
    pub show_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerToClear {
    pub id: String,
    pub title: String,
    pub status: String,
    pub unblocks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecommendationsByTrack {
    pub track_id: String,
    pub top_pick: Option<Recommendation>,
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecommendationsByLabel {
    pub label: String,
    pub top_pick: Option<Recommendation>,
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuickRef {
    pub total_open: usize,
    pub total_actionable: usize,
    pub top_picks: Vec<QuickPick>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageResult {
    pub quick_ref: QuickRef,
    pub recommendations: Vec<Recommendation>,
    pub quick_wins: Vec<Recommendation>,
    pub blockers_to_clear: Vec<BlockerToClear>,
    pub recommendations_by_track: Vec<RecommendationsByTrack>,
    pub recommendations_by_label: Vec<RecommendationsByLabel>,
}

#[derive(Debug, Clone, Default)]
pub struct TriageOptions {
    pub group_by_track: bool,
    pub group_by_label: bool,
    pub max_recommendations: usize,
}

#[derive(Debug, Clone)]
pub struct TriageComputation {
    pub result: TriageResult,
    pub score_by_id: HashMap<String, f64>,
}

pub fn compute_triage(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    options: &TriageOptions,
) -> TriageComputation {
    let max_recommendations = if options.max_recommendations == 0 {
        50
    } else {
        options.max_recommendations
    };

    let actionable: HashSet<String> = graph.actionable_ids().into_iter().collect();
    let total_open = issues.iter().filter(|issue| issue.is_open_like()).count();

    let max_pagerank = metrics
        .pagerank
        .values()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let max_unblocks = metrics
        .blocks_count
        .values()
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);

    let mut recommendations = Vec::<Recommendation>::new();
    let mut score_by_id = HashMap::<String, f64>::new();

    for issue in issues.iter().filter(|issue| actionable.contains(&issue.id)) {
        let pagerank = metrics.pagerank.get(&issue.id).copied().unwrap_or_default();
        let pagerank_norm = pagerank / max_pagerank;

        let unblocks = metrics
            .blocks_count
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let unblocks_norm = unblocks as f64 / max_unblocks as f64;

        let urgency = match issue.normalized_status().as_str() {
            "in_progress" => 1.0,
            "open" => 0.8,
            "review" => 0.7,
            _ => 0.6,
        };

        let score = (0.45 * pagerank_norm
            + 0.30 * unblocks_norm
            + 0.20 * issue.priority_normalized()
            + 0.05 * urgency)
            .clamp(0.0, 1.0);

        let mut reasons = Vec::<String>::new();
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

        score_by_id.insert(issue.id.clone(), score);

        recommendations.push(Recommendation {
            id: issue.id.clone(),
            title: issue.title.clone(),
            score,
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
        });
    }

    recommendations.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    recommendations.truncate(max_recommendations);

    let top_picks = recommendations
        .iter()
        .take(3)
        .map(|rec| QuickPick {
            id: rec.id.clone(),
            title: rec.title.clone(),
            score: rec.score,
            reasons: rec.reasons.clone(),
            unblocks: rec.unblocks,
        })
        .collect();

    let quick_wins = recommendations
        .iter()
        .filter(|rec| {
            issues
                .iter()
                .find(|issue| issue.id == rec.id)
                .is_some_and(|issue| {
                    issue.estimated_minutes.is_some_and(|mins| mins <= 120)
                        || (issue.priority <= 2 && rec.unblocks > 0)
                })
        })
        .take(10)
        .cloned()
        .collect();

    let blockers_to_clear = compute_blockers_to_clear(issues, graph, metrics, &actionable);

    let recommendations_by_track = if options.group_by_track {
        compute_recommendations_by_track(graph, &recommendations)
    } else {
        Vec::new()
    };

    let recommendations_by_label = if options.group_by_label {
        compute_recommendations_by_label(&recommendations)
    } else {
        Vec::new()
    };

    let result = TriageResult {
        quick_ref: QuickRef {
            total_open,
            total_actionable: actionable.len(),
            top_picks,
        },
        recommendations,
        quick_wins,
        blockers_to_clear,
        recommendations_by_track,
        recommendations_by_label,
    };

    TriageComputation {
        result,
        score_by_id,
    }
}

fn compute_blockers_to_clear(
    issues: &[Issue],
    graph: &IssueGraph,
    metrics: &GraphMetrics,
    actionable: &HashSet<String>,
) -> Vec<BlockerToClear> {
    let mut blockers = Vec::<BlockerToClear>::new();

    for issue in issues
        .iter()
        .filter(|issue| issue.is_open_like() && !actionable.contains(&issue.id))
    {
        let unblocks = metrics
            .blocks_count
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        if unblocks == 0 {
            continue;
        }
        if graph.open_blockers(&issue.id).is_empty() {
            continue;
        }

        blockers.push(BlockerToClear {
            id: issue.id.clone(),
            title: issue.title.clone(),
            status: issue.status.clone(),
            unblocks,
        });
    }

    blockers.sort_by(|left, right| {
        right
            .unblocks
            .cmp(&left.unblocks)
            .then_with(|| left.id.cmp(&right.id))
    });
    blockers.truncate(15);
    blockers
}

fn compute_recommendations_by_track(
    graph: &IssueGraph,
    recommendations: &[Recommendation],
) -> Vec<RecommendationsByTrack> {
    let component_lookup = graph.connected_open_components();
    let rec_by_id: HashMap<&str, &Recommendation> = recommendations
        .iter()
        .map(|rec| (rec.id.as_str(), rec))
        .collect();

    let mut by_track = Vec::<RecommendationsByTrack>::new();

    for (index, component) in component_lookup.iter().enumerate() {
        let mut items: Vec<&Recommendation> = component
            .iter()
            .filter_map(|id| rec_by_id.get(id.as_str()).copied())
            .collect();

        items.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

        if items.is_empty() {
            continue;
        }

        by_track.push(RecommendationsByTrack {
            track_id: format!("track-{}", index + 1),
            top_pick: items.first().map(|item| (*item).clone()),
            item_ids: items.into_iter().map(|item| item.id.clone()).collect(),
        });
    }

    by_track
}

fn compute_recommendations_by_label(
    recommendations: &[Recommendation],
) -> Vec<RecommendationsByLabel> {
    let mut groups: BTreeMap<String, Vec<Recommendation>> = BTreeMap::new();

    for rec in recommendations {
        for label in &rec.labels {
            groups.entry(label.clone()).or_default().push(rec.clone());
        }
    }

    let mut out = Vec::<RecommendationsByLabel>::new();
    for (label, mut recs) in groups {
        recs.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

        out.push(RecommendationsByLabel {
            label,
            top_pick: recs.first().cloned(),
            item_ids: recs.into_iter().map(|rec| rec.id).collect(),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use crate::analysis::graph::IssueGraph;
    use crate::model::Issue;

    use super::{TriageOptions, compute_triage};

    #[test]
    fn triage_produces_recommendations() {
        let issues = vec![
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
                title: "Depends on A".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![crate::model::Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..crate::model::Dependency::default()
                }],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: true,
                group_by_label: true,
                max_recommendations: 10,
            },
        );

        assert_eq!(triage.result.quick_ref.total_open, 2);
        assert_eq!(triage.result.quick_ref.total_actionable, 1);
        assert_eq!(triage.result.recommendations.len(), 1);
        assert_eq!(triage.result.recommendations[0].id, "A");
    }

    #[test]
    fn triage_empty_issues_produces_zero_recommendations() {
        let issues: Vec<Issue> = vec![];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: false,
                max_recommendations: 10,
            },
        );
        assert_eq!(triage.result.quick_ref.total_open, 0);
        assert_eq!(triage.result.quick_ref.total_actionable, 0);
        assert!(triage.result.recommendations.is_empty());
        assert!(triage.result.blockers_to_clear.is_empty());
    }

    #[test]
    fn triage_all_closed_produces_no_actionable() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "Done".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "Also done".to_string(),
                status: "tombstone".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: false,
                max_recommendations: 10,
            },
        );
        assert_eq!(triage.result.quick_ref.total_open, 0);
        assert!(triage.result.recommendations.is_empty());
    }

    #[test]
    fn triage_max_recommendations_limits_output() {
        let issues: Vec<Issue> = (0..20)
            .map(|i| Issue {
                id: format!("X-{i}"),
                title: format!("Issue {i}"),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            })
            .collect();
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: false,
                max_recommendations: 5,
            },
        );
        assert!(triage.result.recommendations.len() <= 5);
    }

    #[test]
    fn triage_scores_are_sorted_descending() {
        let issues: Vec<Issue> = (0..5)
            .map(|i| Issue {
                id: format!("P-{i}"),
                title: format!("Task {i}"),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: i + 1,
                ..Issue::default()
            })
            .collect();
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: false,
                max_recommendations: 10,
            },
        );
        let scores: Vec<f64> = triage
            .result
            .recommendations
            .iter()
            .map(|r| r.score)
            .collect();
        for w in scores.windows(2) {
            assert!(
                w[0] >= w[1],
                "scores should be descending: {} >= {}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn triage_blockers_to_clear_identifies_chained_blockers() {
        // Chain: A (open, actionable) blocks B (open, blocked by A, blocks C+D)
        // B is not actionable (blocked by A) but blocks C+D => should be in blockers_to_clear
        let issues = vec![
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
                title: "Middle".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![crate::model::Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..crate::model::Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "Leaf 1".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                dependencies: vec![crate::model::Dependency {
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..crate::model::Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "D".to_string(),
                title: "Leaf 2".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 3,
                dependencies: vec![crate::model::Dependency {
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..crate::model::Dependency::default()
                }],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: false,
                max_recommendations: 10,
            },
        );
        // B should be in blockers_to_clear (blocked by A, blocks C+D)
        let b_blocker = triage.result.blockers_to_clear.iter().find(|b| b.id == "B");
        assert!(
            b_blocker.is_some(),
            "B should be in blockers_to_clear (got {:?})",
            triage.result.blockers_to_clear
        );
        assert!(
            b_blocker.unwrap().unblocks >= 2,
            "B should unblock 2+ issues"
        );
    }

    #[test]
    fn triage_group_by_label_produces_label_groups() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "UI fix".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                labels: vec!["ui".to_string()],
                ..Issue::default()
            },
            Issue {
                id: "B".to_string(),
                title: "API fix".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                labels: vec!["api".to_string()],
                ..Issue::default()
            },
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        let triage = compute_triage(
            &issues,
            &graph,
            &metrics,
            &TriageOptions {
                group_by_track: false,
                group_by_label: true,
                max_recommendations: 10,
            },
        );
        assert!(
            !triage.result.recommendations_by_label.is_empty(),
            "should group by label"
        );
    }
}
