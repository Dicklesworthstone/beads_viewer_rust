use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::analysis::graph::IssueGraph;

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub score: f64,
    pub unblocks: Vec<String>,
    pub claim_command: String,
    pub show_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionTrack {
    #[serde(rename = "track_id")]
    pub id: String,
    pub items: Vec<ExecutionItem>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PlanSummary {
    pub track_count: usize,
    pub actionable_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unblocks_count: Option<usize>,
    pub highest_impact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionPlan {
    pub total_actionable: usize,
    pub total_blocked: usize,
    pub tracks: Vec<ExecutionTrack>,
    pub summary: PlanSummary,
}

fn unique_unblocks_count<'a>(items: impl IntoIterator<Item = &'a ExecutionItem>) -> usize {
    items
        .into_iter()
        .flat_map(|item| item.unblocks.iter().cloned())
        .collect::<HashSet<_>>()
        .len()
}

pub fn compute_execution_plan(
    graph: &IssueGraph,
    score_by_id: &HashMap<String, f64>,
) -> ExecutionPlan {
    let components = graph.connected_open_components();
    let actionable: HashSet<String> = graph.actionable_ids().into_iter().collect();

    let mut tracks = Vec::<ExecutionTrack>::new();
    let mut track_number: usize = 0;

    for component in &components {
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
                status: issue.status.clone(),
                priority: issue.priority,
                score: score_by_id.get(issue_id).copied().unwrap_or_default(),
                unblocks,
                claim_command: issue.claim_command(),
                show_command: issue.show_command(),
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

        // Build descriptive reason for this track.
        let total_unblocks = unique_unblocks_count(items.iter());
        let reason = if items.len() == 1 {
            let item = &items[0];
            if item.unblocks.is_empty() {
                "independent issue — can execute in parallel".to_string()
            } else {
                format!(
                    "completing {} unblocks {} issue(s)",
                    item.id,
                    item.unblocks.len()
                )
            }
        } else if total_unblocks > 0 {
            format!(
                "connected component of {} actionable items unblocking {} downstream issue(s)",
                items.len(),
                total_unblocks
            )
        } else {
            format!("connected component of {} independent items", items.len())
        };

        track_number += 1;
        tracks.push(ExecutionTrack {
            id: format!("track-{track_number}"),
            items,
            reason,
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

    // Exclude in_progress issues from highest_impact to match legacy behavior:
    // the "highest impact" pick should surface new work, not work already claimed.
    // Track order includes running work, so after filtering it cannot rank the
    // remaining candidates. Compare every candidate using the item score/ID order.
    let highest_impact_item = tracks
        .iter()
        .flat_map(|track| track.items.iter())
        .filter(|item| {
            graph
                .issue(&item.id)
                .is_none_or(|issue| issue.normalized_status() != "in_progress")
        })
        .min_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });

    let highest_impact = highest_impact_item.map(|item| item.id.clone());

    let impact_reason = highest_impact_item.map(|item| {
        let mut parts = Vec::new();
        parts.push(format!("score {:.2}", item.score));
        if !item.unblocks.is_empty() {
            parts.push(format!("unblocks {} issue(s)", item.unblocks.len()));
        }
        format!("highest impact: {} ({})", item.id, parts.join(", "))
    });

    let actionable_count: usize = tracks.iter().map(|track| track.items.len()).sum();
    let total_blocked = graph
        .issues
        .iter()
        .filter(|issue| issue.is_open_like() && !graph.open_blockers(&issue.id).is_empty())
        .count();
    let total_unblocks = unique_unblocks_count(tracks.iter().flat_map(|track| track.items.iter()));
    let track_count = tracks.len();

    ExecutionPlan {
        total_actionable: actionable_count,
        total_blocked,
        tracks,
        summary: PlanSummary {
            track_count,
            actionable_count,
            unblocks_count: Some(total_unblocks),
            highest_impact,
            impact_reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::analysis::graph::IssueGraph;
    use crate::model::{Dependency, Issue};

    use super::{compute_execution_plan, unique_unblocks_count};

    fn impact_issue(id: &str, status: &str, prerequisites: &[&str]) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: status.to_string(),
            issue_type: "task".to_string(),
            priority: 2,
            dependencies: prerequisites
                .iter()
                .map(|target| Dependency {
                    depends_on_id: (*target).to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                })
                .collect(),
            ..Issue::default()
        }
    }

    fn impact_tracks() -> Vec<Issue> {
        // RUN and Z are actionable in one track because both block JOIN.
        // A is an independent track. RUN sorts first but cannot be the pick.
        vec![
            impact_issue("RUN", "in_progress", &[]),
            impact_issue("Z", "open", &[]),
            impact_issue("A", "open", &[]),
            impact_issue("JOIN", "open", &["RUN", "Z"]),
        ]
    }

    #[test]
    fn plan_highest_impact_compares_candidates_across_tracks() {
        for (local_score, other_score, expected) in [(0.1, 0.8, "A"), (0.8, 0.1, "Z")] {
            let scores = HashMap::from([
                ("RUN".to_string(), 0.99),
                ("Z".to_string(), local_score),
                ("A".to_string(), other_score),
                // A high score must not make the blocked join a candidate.
                ("JOIN".to_string(), 1.0),
            ]);
            let mut issues = impact_tracks();
            for _ in 0..issues.len() {
                let plan = compute_execution_plan(&IssueGraph::build(&issues), &scores);
                assert_eq!(plan.tracks.len(), 2);
                assert_eq!(plan.tracks[0].items[0].id, "RUN");
                assert_eq!(plan.tracks[0].items[1].id, "Z");
                assert_eq!(plan.total_actionable, 3);
                assert_eq!(plan.total_blocked, 1);
                assert_eq!(plan.summary.highest_impact.as_deref(), Some(expected));
                assert!(
                    plan.summary
                        .impact_reason
                        .as_deref()
                        .expect("selected candidate has an explanation")
                        .starts_with(&format!("highest impact: {expected} (score 0.80"))
                );
                issues.rotate_left(1);
            }
        }
    }

    #[test]
    fn plan_highest_impact_ties_use_issue_ids_not_track_rank() {
        let scores = HashMap::from([
            ("RUN".to_string(), 0.99),
            ("Z".to_string(), 0.5),
            ("A".to_string(), 0.5),
        ]);
        let mut issues = impact_tracks();
        for _ in 0..issues.len() {
            let plan = compute_execution_plan(&IssueGraph::build(&issues), &scores);
            assert_eq!(plan.tracks[0].items[0].id, "RUN");
            assert_eq!(plan.summary.highest_impact.as_deref(), Some("A"));
            assert_eq!(
                plan.summary.impact_reason.as_deref(),
                Some("highest impact: A (score 0.50)")
            );
            issues.rotate_left(1);
        }
    }

    #[test]
    fn plan_highest_impact_skips_a_track_containing_only_running_work() {
        let issues = vec![
            impact_issue("RUN", "in_progress", &[]),
            impact_issue("OPEN", "open", &[]),
        ];
        let scores = HashMap::from([("RUN".to_string(), 0.99), ("OPEN".to_string(), 0.2)]);
        let plan = compute_execution_plan(&IssueGraph::build(&issues), &scores);
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(plan.tracks[0].items[0].id, "RUN");
        assert_eq!(plan.total_actionable, 2, "running work stays in the plan");
        assert_eq!(plan.summary.highest_impact.as_deref(), Some("OPEN"));
        assert_eq!(
            plan.summary.impact_reason.as_deref(),
            Some("highest impact: OPEN (score 0.20)")
        );
    }

    #[test]
    fn plan_highest_impact_is_empty_when_all_actionable_work_is_running() {
        let issues = vec![
            impact_issue("A", "in_progress", &[]),
            impact_issue("B", "in_progress", &[]),
        ];
        let scores = HashMap::from([("A".to_string(), 0.99), ("B".to_string(), 0.2)]);
        let plan = compute_execution_plan(&IssueGraph::build(&issues), &scores);
        assert_eq!(plan.total_actionable, 2);
        assert_eq!(plan.tracks.len(), 2);
        assert!(plan.summary.highest_impact.is_none());
        assert!(plan.summary.impact_reason.is_none());
    }

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
    fn plan_track_reason_describes_component() {
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
                title: "Depends".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "Independent".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.8);
        scores.insert("C".to_string(), 0.5);

        let plan = compute_execution_plan(&graph, &scores);

        // Track with A should mention unblocking.
        let track_a = plan
            .tracks
            .iter()
            .find(|t| t.items.iter().any(|i| i.id == "A"));
        assert!(track_a.is_some());
        assert!(
            !track_a.unwrap().reason.is_empty(),
            "track reason should not be empty"
        );

        // Independent track should mention independence.
        let track_c = plan
            .tracks
            .iter()
            .find(|t| t.items.iter().any(|i| i.id == "C"));
        assert!(track_c.is_some());
        assert!(track_c.unwrap().reason.contains("independent"));
    }

    #[test]
    fn plan_impact_reason_present_when_tracks_exist() {
        let issues = vec![Issue {
            id: "A".to_string(),
            title: "Only".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            priority: 1,
            ..Issue::default()
        }];
        let graph = IssueGraph::build(&issues);
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.9);

        let plan = compute_execution_plan(&graph, &scores);
        assert!(plan.summary.impact_reason.is_some());
        let reason = plan.summary.impact_reason.unwrap();
        assert!(reason.contains("A"), "should mention the issue ID");
        assert!(reason.contains("0.90"), "should mention the score");
    }

    #[test]
    fn plan_impact_reason_none_when_no_tracks() {
        let issues: Vec<Issue> = vec![];
        let graph = IssueGraph::build(&issues);
        let plan = compute_execution_plan(&graph, &HashMap::new());
        assert!(plan.summary.impact_reason.is_none());
    }

    #[test]
    fn plan_reason_serializes_to_json() {
        let issues = vec![Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            ..Issue::default()
        }];
        let graph = IssueGraph::build(&issues);
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.5);
        let plan = compute_execution_plan(&graph, &scores);

        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("\"reason\""));
        assert!(json.contains("\"impact_reason\""));
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

    #[test]
    fn plan_track_ids_are_contiguous() {
        // Create 3 components where the middle one has no actionable items,
        // so it gets skipped. Track IDs should still be contiguous (1, 2).
        let issues = vec![
            Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
            // B and C form a component where both are blocked (no actionable items).
            Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![Dependency {
                    depends_on_id: "C".to_string(),
                    dep_type: "blocks".to_string(),
                    ..Dependency::default()
                }],
                ..Issue::default()
            },
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
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
                id: "D".to_string(),
                title: "D".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.8);
        scores.insert("D".to_string(), 0.7);

        let plan = compute_execution_plan(&graph, &scores);
        assert_eq!(plan.tracks.len(), 2);
        // Track IDs should be contiguous: track-1, track-2 (no gaps).
        let mut ids: Vec<&str> = plan.tracks.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["track-1", "track-2"]);
    }

    #[test]
    fn unique_unblocks_are_counted_once_in_summary_and_reason() {
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
            Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![
                    Dependency {
                        depends_on_id: "A".to_string(),
                        dep_type: "blocks".to_string(),
                        ..Dependency::default()
                    },
                    Dependency {
                        depends_on_id: "B".to_string(),
                        dep_type: "blocks".to_string(),
                        ..Dependency::default()
                    },
                ],
                ..Issue::default()
            },
        ];

        let graph = IssueGraph::build(&issues);
        let mut scores = HashMap::new();
        scores.insert("A".to_string(), 0.8);
        scores.insert("B".to_string(), 0.7);

        let plan = compute_execution_plan(&graph, &scores);
        assert_eq!(plan.summary.unblocks_count, Some(1));
        assert_eq!(plan.tracks.len(), 1);
        assert!(
            plan.tracks[0]
                .reason
                .contains("unblocking 1 downstream issue")
        );
        assert_eq!(unique_unblocks_count(plan.tracks[0].items.iter()), 1);
    }
}
