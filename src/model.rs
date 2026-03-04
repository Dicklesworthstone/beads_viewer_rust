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
