use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{BvrError, Result};

const KNOWN_STATUSES: &[&str] = &[
    "open",
    "in_progress",
    "blocked",
    "deferred",
    "pinned",
    "hooked",
    "review",
    "closed",
    "tombstone",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Issue {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub design: String,
    #[serde(default)]
    pub acceptance_criteria: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub status: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub issue_type: String,
    #[serde(default)]
    pub assignee: String,
    #[serde(default)]
    pub estimated_minutes: Option<i32>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub source_repo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Dependency {
    #[serde(default)]
    pub issue_id: String,
    #[serde(default)]
    pub depends_on_id: String,
    #[serde(default, rename = "type")]
    pub dep_type: String,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Comment {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub issue_id: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl Dependency {
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        let t = self.dep_type.trim().to_ascii_lowercase();
        t.is_empty() || t == "blocks"
    }

    #[must_use]
    pub fn is_parent_child(&self) -> bool {
        let t = self.dep_type.trim().to_ascii_lowercase();
        t == "parent-child"
    }
}

impl Issue {
    #[must_use]
    pub fn normalized_status(&self) -> String {
        self.status.trim().to_ascii_lowercase()
    }

    #[must_use]
    pub fn is_closed_like(&self) -> bool {
        matches!(self.normalized_status().as_str(), "closed" | "tombstone")
    }

    #[must_use]
    pub fn is_open_like(&self) -> bool {
        !self.is_closed_like()
    }

    #[must_use]
    pub fn priority_normalized(&self) -> f64 {
        let p = self.priority.clamp(1, 5);
        // Priority 1 => 1.0, Priority 5 => 0.2
        (6_i32.saturating_sub(p)) as f64 / 5.0
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(BvrError::InvalidIssue(
                "issue id cannot be empty".to_string(),
            ));
        }
        if self.title.trim().is_empty() {
            return Err(BvrError::InvalidIssue(format!(
                "issue {} title cannot be empty",
                self.id
            )));
        }
        if self.issue_type.trim().is_empty() {
            return Err(BvrError::InvalidIssue(format!(
                "issue {} issue_type cannot be empty",
                self.id
            )));
        }

        let status = self.normalized_status();
        if status.is_empty() {
            return Err(BvrError::InvalidIssue(format!(
                "issue {} status cannot be empty",
                self.id
            )));
        }
        if !KNOWN_STATUSES.contains(&status.as_str()) {
            return Err(BvrError::InvalidIssue(format!(
                "issue {} has unknown status: {}",
                self.id, self.status
            )));
        }

        Ok(())
    }
}

const fn default_priority() -> i32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Sprint {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub bead_ids: Vec<String>,
}

impl Sprint {
    #[must_use]
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        let Some(start_date) = self.start_date else {
            return false;
        };
        let Some(end_date) = self.end_date else {
            return false;
        };

        now >= start_date && now <= end_date
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurndownPoint {
    pub date: DateTime<Utc>,
    pub remaining: i32,
    pub completed: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Dependency tests --

    #[test]
    fn dependency_is_blocking_for_empty_type() {
        let dep = Dependency {
            dep_type: "".to_string(),
            ..Default::default()
        };
        assert!(dep.is_blocking());
    }

    #[test]
    fn dependency_is_blocking_for_blocks_type() {
        let dep = Dependency {
            dep_type: "blocks".to_string(),
            ..Default::default()
        };
        assert!(dep.is_blocking());
        // case insensitive + trim
        let dep2 = Dependency {
            dep_type: "  Blocks  ".to_string(),
            ..Default::default()
        };
        assert!(dep2.is_blocking());
    }

    #[test]
    fn dependency_not_blocking_for_other_types() {
        for t in ["parent-child", "related", "mentions", "unknown"] {
            let dep = Dependency {
                dep_type: t.to_string(),
                ..Default::default()
            };
            assert!(!dep.is_blocking(), "{t} should not be blocking");
        }
    }

    #[test]
    fn dependency_is_parent_child() {
        let dep = Dependency {
            dep_type: "parent-child".to_string(),
            ..Default::default()
        };
        assert!(dep.is_parent_child());
        // case insensitive
        let dep2 = Dependency {
            dep_type: " Parent-Child ".to_string(),
            ..Default::default()
        };
        assert!(dep2.is_parent_child());
        // Not parent-child
        let dep3 = Dependency {
            dep_type: "blocks".to_string(),
            ..Default::default()
        };
        assert!(!dep3.is_parent_child());
    }

    // -- Issue status tests --

    #[test]
    fn normalized_status_lowercases_and_trims() {
        let issue = Issue {
            status: "  OPEN  ".to_string(),
            ..Default::default()
        };
        assert_eq!(issue.normalized_status(), "open");
    }

    #[test]
    fn is_closed_like_detects_closed_and_tombstone() {
        for status in ["closed", "Closed", "CLOSED", "tombstone", "Tombstone"] {
            let issue = Issue {
                status: status.to_string(),
                ..Default::default()
            };
            assert!(issue.is_closed_like(), "{status} should be closed-like");
            assert!(!issue.is_open_like(), "{status} should not be open-like");
        }
    }

    #[test]
    fn is_open_like_for_all_open_statuses() {
        for status in [
            "open",
            "in_progress",
            "blocked",
            "deferred",
            "pinned",
            "hooked",
            "review",
        ] {
            let issue = Issue {
                status: status.to_string(),
                ..Default::default()
            };
            assert!(issue.is_open_like(), "{status} should be open-like");
            assert!(
                !issue.is_closed_like(),
                "{status} should not be closed-like"
            );
        }
    }

    // -- Priority normalization --

    #[test]
    fn priority_normalized_maps_1_to_1_and_5_to_low() {
        let p1 = Issue {
            priority: 1,
            ..Default::default()
        };
        assert!((p1.priority_normalized() - 1.0).abs() < f64::EPSILON);

        let p5 = Issue {
            priority: 5,
            ..Default::default()
        };
        assert!((p5.priority_normalized() - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn priority_normalized_clamps_out_of_range() {
        let too_low = Issue {
            priority: -10,
            ..Default::default()
        };
        // clamp(1, 5) => 1 => (6-1)/5 = 1.0
        assert!((too_low.priority_normalized() - 1.0).abs() < f64::EPSILON);

        let too_high = Issue {
            priority: 100,
            ..Default::default()
        };
        // clamp(1, 5) => 5 => (6-5)/5 = 0.2
        assert!((too_high.priority_normalized() - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn priority_normalized_default_clamps_zero_to_one() {
        // Issue::default() has priority=0 (Rust default), but serde default is 3.
        // priority_normalized clamps to [1,5], so 0 => clamped to 1 => (6-1)/5 = 1.0
        let issue = Issue::default();
        assert_eq!(issue.priority, 0);
        assert!((issue.priority_normalized() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn priority_normalized_serde_default_is_3() {
        let json = r#"{"id":"X","title":"T"}"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.priority, 3);
        // (6-3)/5 = 0.6
        assert!((issue.priority_normalized() - 0.6).abs() < f64::EPSILON);
    }

    // -- Validation --

    #[test]
    fn validate_rejects_empty_id() {
        let issue = Issue {
            id: "  ".to_string(),
            title: "T".to_string(),
            issue_type: "task".to_string(),
            status: "open".to_string(),
            ..Default::default()
        };
        let err = issue.validate().unwrap_err();
        assert!(err.to_string().contains("id cannot be empty"));
    }

    #[test]
    fn validate_rejects_empty_title() {
        let issue = Issue {
            id: "X-1".to_string(),
            title: "".to_string(),
            issue_type: "task".to_string(),
            status: "open".to_string(),
            ..Default::default()
        };
        let err = issue.validate().unwrap_err();
        assert!(err.to_string().contains("title cannot be empty"));
    }

    #[test]
    fn validate_rejects_empty_type() {
        let issue = Issue {
            id: "X-1".to_string(),
            title: "Test".to_string(),
            issue_type: "".to_string(),
            status: "open".to_string(),
            ..Default::default()
        };
        let err = issue.validate().unwrap_err();
        assert!(err.to_string().contains("issue_type cannot be empty"));
    }

    #[test]
    fn validate_rejects_empty_status() {
        let issue = Issue {
            id: "X-1".to_string(),
            title: "Test".to_string(),
            issue_type: "task".to_string(),
            status: "".to_string(),
            ..Default::default()
        };
        let err = issue.validate().unwrap_err();
        assert!(err.to_string().contains("status cannot be empty"));
    }

    #[test]
    fn validate_rejects_unknown_status() {
        let issue = Issue {
            id: "X-1".to_string(),
            title: "Test".to_string(),
            issue_type: "task".to_string(),
            status: "banana".to_string(),
            ..Default::default()
        };
        let err = issue.validate().unwrap_err();
        assert!(err.to_string().contains("unknown status"));
    }

    #[test]
    fn validate_accepts_all_known_statuses() {
        for status in KNOWN_STATUSES {
            let issue = Issue {
                id: "X-1".to_string(),
                title: "Test".to_string(),
                issue_type: "task".to_string(),
                status: status.to_string(),
                ..Default::default()
            };
            assert!(issue.validate().is_ok(), "status {status} should be valid");
        }
    }

    // -- Sprint tests --

    #[test]
    fn sprint_is_active_at_within_range() {
        let sprint = Sprint {
            id: "s1".to_string(),
            name: "Sprint 1".to_string(),
            start_date: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            end_date: Some("2026-01-14T00:00:00Z".parse().unwrap()),
            bead_ids: Vec::new(),
        };
        let mid: DateTime<Utc> = "2026-01-07T12:00:00Z".parse().unwrap();
        assert!(sprint.is_active_at(mid));
    }

    #[test]
    fn sprint_not_active_outside_range() {
        let sprint = Sprint {
            id: "s1".to_string(),
            name: "Sprint 1".to_string(),
            start_date: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            end_date: Some("2026-01-14T00:00:00Z".parse().unwrap()),
            bead_ids: Vec::new(),
        };
        let before: DateTime<Utc> = "2025-12-31T00:00:00Z".parse().unwrap();
        let after: DateTime<Utc> = "2026-01-15T00:00:00Z".parse().unwrap();
        assert!(!sprint.is_active_at(before));
        assert!(!sprint.is_active_at(after));
    }

    #[test]
    fn sprint_not_active_without_dates() {
        let sprint = Sprint {
            start_date: None,
            end_date: None,
            ..Default::default()
        };
        let now: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        assert!(!sprint.is_active_at(now));
    }

    #[test]
    fn sprint_active_at_boundary() {
        let sprint = Sprint {
            start_date: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            end_date: Some("2026-01-14T00:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        let at_start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let at_end: DateTime<Utc> = "2026-01-14T00:00:00Z".parse().unwrap();
        assert!(sprint.is_active_at(at_start), "active at start boundary");
        assert!(sprint.is_active_at(at_end), "active at end boundary");
    }

    // -- Serde round-trip --

    #[test]
    fn issue_deserializes_with_defaults() {
        let json = r#"{"id":"X-1","title":"Test"}"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.id, "X-1");
        assert_eq!(issue.priority, 3); // default
        assert_eq!(issue.status, "");
        assert!(issue.labels.is_empty());
        assert!(issue.dependencies.is_empty());
    }

    #[test]
    fn dependency_deserializes_type_field() {
        let json = r#"{"issue_id":"A","depends_on_id":"B","type":"blocks"}"#;
        let dep: Dependency = serde_json::from_str(json).unwrap();
        assert_eq!(dep.dep_type, "blocks");
        assert!(dep.is_blocking());
    }
}
