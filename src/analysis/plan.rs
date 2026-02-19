use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::analysis::graph::IssueGraph;

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionItem {
    pub id: String,
    pub title: String,
    pub score: f64,
    pub unblocks: Vec<String>,
    pub claim_command: String,
    pub show_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionTrack {
    pub id: String,
    pub items: Vec<ExecutionItem>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PlanSummary {
    pub track_count: usize,
    pub actionable_count: usize,
    pub highest_impact: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionPlan {
    pub tracks: Vec<ExecutionTrack>,
    pub summary: PlanSummary,
}

pub fn compute_execution_plan(
    graph: &IssueGraph,
    score_by_id: &HashMap<String, f64>,
) -> ExecutionPlan {
    let components = graph.connected_open_components();
    let actionable: HashSet<String> = graph.actionable_ids().into_iter().collect();

    let mut tracks = Vec::<ExecutionTrack>::new();

    for (index, component) in components.iter().enumerate() {
        let mut items = Vec::<ExecutionItem>::new();

        for issue_id in component {
            if !actionable.contains(issue_id) {
                continue;
            }
            let Some(issue) = graph.issue(issue_id) else {
                continue;
            };

            let mut unblocks = graph
                .dependents(issue_id)
                .into_iter()
                .filter(|dependent_id| {
                    graph
                        .issue(dependent_id)
                        .is_some_and(crate::model::Issue::is_open_like)
                })
                .collect::<Vec<_>>();
            unblocks.sort();

            items.push(ExecutionItem {
                id: issue.id.clone(),
                title: issue.title.clone(),
                score: score_by_id.get(issue_id).copied().unwrap_or_default(),
                unblocks,
                claim_command: format!("br update {} --status=in_progress", issue.id),
                show_command: format!("br show {}", issue.id),
            });
        }

        items.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

        if items.is_empty() {
            continue;
        }

        tracks.push(ExecutionTrack {
            id: format!("track-{}", index + 1),
            items,
        });
    }

    tracks.sort_by(|left, right| {
        let left_score = left
            .items
            .first()
            .map(|item| item.score)
            .unwrap_or_default();
        let right_score = right
            .items
            .first()
            .map(|item| item.score)
            .unwrap_or_default();
        right_score
            .total_cmp(&left_score)
            .then_with(|| left.id.cmp(&right.id))
    });

    let highest_impact = tracks
        .first()
        .and_then(|track| track.items.first())
        .map(|item| item.id.clone());

    let actionable_count = tracks.iter().map(|track| track.items.len()).sum();
    let track_count = tracks.len();

    ExecutionPlan {
        tracks,
        summary: PlanSummary {
            track_count,
            actionable_count,
            highest_impact,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::analysis::graph::IssueGraph;
    use crate::model::{Dependency, Issue};

    use super::compute_execution_plan;

    #[test]
    fn plan_groups_by_components() {
        let issues = vec![
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
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "blocked".to_string(),
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
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.8);
        scores.insert("B".to_string(), 0.7);

        let plan = compute_execution_plan(&graph, &scores);
        assert_eq!(plan.summary.actionable_count, 2);
        assert!(plan.summary.track_count >= 1);
        assert_eq!(plan.summary.track_count, plan.tracks.len());
    }

    #[test]
    fn plan_summary_track_count_reflects_non_empty_tracks_only() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "blocked".to_string(),
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
                title: "B".to_string(),
                status: "blocked".to_string(),
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
        let scores = HashMap::new();
        let plan = compute_execution_plan(&graph, &scores);

        assert_eq!(plan.tracks.len(), 0);
        assert_eq!(plan.summary.track_count, 0);
        assert_eq!(plan.summary.actionable_count, 0);
        assert!(plan.summary.highest_impact.is_none());
    }
}
