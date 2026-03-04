use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::git_history::HistoryCommitCompat;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackType {
    Confirm,
    Reject,
    Ignore,
}

impl std::fmt::Display for FeedbackType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Confirm => write!(f, "confirm"),
            Self::Reject => write!(f, "reject"),
            Self::Ignore => write!(f, "ignore"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationFeedback {
    pub commit_sha: String,
    pub bead_id: String,
    pub feedback_at: String,
    pub feedback_by: String,
    #[serde(rename = "type")]
    pub feedback_type: FeedbackType,
    pub reason: String,
    pub original_conf: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackStats {
    pub total_feedback: usize,
    pub confirmed: usize,
    pub rejected: usize,
    pub ignored: usize,
    pub accuracy_rate: f64,
    pub avg_confirm_conf: f64,
    pub avg_reject_conf: f64,
}

// ---------------------------------------------------------------------------
// Signal and Explanation types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CorrelationSignal {
    #[serde(rename = "type")]
    pub signal_type: String,
    pub weight: u32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorrelationExplanation {
    pub commit_sha: String,
    pub bead_id: String,
    pub confidence: f64,
    pub confidence_pct: u32,
    pub level: String,
    pub method: String,
    pub signals: Vec<CorrelationSignal>,
    pub total_weight: u32,
    pub summary: String,
    pub recommendation: String,
}

// ---------------------------------------------------------------------------
// FeedbackStore
// ---------------------------------------------------------------------------

pub struct FeedbackStore {
    path: PathBuf,
    cache: BTreeMap<String, CorrelationFeedback>,
}

fn cache_key(commit_sha: &str, bead_id: &str) -> String {
    format!("{commit_sha}:{bead_id}")
}

impl FeedbackStore {
    /// Open (or create) a feedback store backed by the given JSONL file.
    pub fn open(path: &Path) -> crate::Result<Self> {
        let mut cache = BTreeMap::new();

        if path.exists() {
            let file = File::open(path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<CorrelationFeedback>(line) {
                    let key = cache_key(&entry.commit_sha, &entry.bead_id);
                    cache.insert(key, entry);
                }
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            cache,
        })
    }

    /// Record feedback, appending to the JSONL file and updating the cache.
    pub fn save(&mut self, feedback: CorrelationFeedback) -> crate::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        let json = serde_json::to_string(&feedback)?;
        writeln!(file, "{json}")?;

        let key = cache_key(&feedback.commit_sha, &feedback.bead_id);
        self.cache.insert(key, feedback);
        Ok(())
    }

    /// Convenience: record a confirmation.
    pub fn confirm(
        &mut self,
        commit_sha: &str,
        bead_id: &str,
        by: &str,
        original_conf: f64,
        reason: &str,
    ) -> crate::Result<CorrelationFeedback> {
        let feedback = CorrelationFeedback {
            commit_sha: commit_sha.to_string(),
            bead_id: bead_id.to_string(),
            feedback_at: Utc::now().to_rfc3339(),
            feedback_by: by.to_string(),
            feedback_type: FeedbackType::Confirm,
            reason: reason.to_string(),
            original_conf,
        };
        self.save(feedback.clone())?;
        Ok(feedback)
    }

    /// Convenience: record a rejection.
    pub fn reject(
        &mut self,
        commit_sha: &str,
        bead_id: &str,
        by: &str,
        original_conf: f64,
        reason: &str,
    ) -> crate::Result<CorrelationFeedback> {
        let feedback = CorrelationFeedback {
            commit_sha: commit_sha.to_string(),
            bead_id: bead_id.to_string(),
            feedback_at: Utc::now().to_rfc3339(),
            feedback_by: by.to_string(),
            feedback_type: FeedbackType::Reject,
            reason: reason.to_string(),
            original_conf,
        };
        self.save(feedback.clone())?;
        Ok(feedback)
    }

    /// Look up existing feedback for a commit+bead pair.
    #[must_use]
    pub fn get(&self, commit_sha: &str, bead_id: &str) -> Option<&CorrelationFeedback> {
        self.cache.get(&cache_key(commit_sha, bead_id))
    }

    /// Check if feedback exists for a commit+bead pair.
    #[must_use]
    pub fn has_feedback(&self, commit_sha: &str, bead_id: &str) -> bool {
        self.cache.contains_key(&cache_key(commit_sha, bead_id))
    }

    /// All feedback entries for a specific bead.
    #[must_use]
    pub fn get_by_bead(&self, bead_id: &str) -> Vec<&CorrelationFeedback> {
        self.cache
            .values()
            .filter(|entry| entry.bead_id == bead_id)
            .collect()
    }

    /// Compute aggregate statistics.
    #[must_use]
    pub fn stats(&self) -> FeedbackStats {
        let mut confirmed = 0usize;
        let mut rejected = 0usize;
        let mut ignored = 0usize;
        let mut confirm_conf_sum = 0.0_f64;
        let mut reject_conf_sum = 0.0_f64;

        for entry in self.cache.values() {
            match entry.feedback_type {
                FeedbackType::Confirm => {
                    confirmed += 1;
                    confirm_conf_sum += entry.original_conf;
                }
                FeedbackType::Reject => {
                    rejected += 1;
                    reject_conf_sum += entry.original_conf;
                }
                FeedbackType::Ignore => {
                    ignored += 1;
                }
            }
        }

        let total = confirmed + rejected + ignored;
        let accuracy_rate = if confirmed + rejected > 0 {
            confirmed as f64 / (confirmed + rejected) as f64
        } else {
            0.0
        };
        let avg_confirm_conf = if confirmed > 0 {
            confirm_conf_sum / confirmed as f64
        } else {
            0.0
        };
        let avg_reject_conf = if rejected > 0 {
            reject_conf_sum / rejected as f64
        } else {
            0.0
        };

        FeedbackStats {
            total_feedback: total,
            confirmed,
            rejected,
            ignored,
            accuracy_rate,
            avg_confirm_conf,
            avg_reject_conf,
        }
    }
}

// ---------------------------------------------------------------------------
// Explanation builder
// ---------------------------------------------------------------------------

/// Map a confidence score to a human-readable level string.
#[must_use]
pub fn confidence_level(confidence: f64) -> &'static str {
    if confidence >= 0.9 {
        "very high"
    } else if confidence >= 0.75 {
        "high"
    } else if confidence >= 0.5 {
        "moderate"
    } else if confidence >= 0.3 {
        "low"
    } else {
        "very low"
    }
}

/// Build a detailed explanation for a commit-bead correlation.
#[must_use]
pub fn build_explanation(
    commit: &HistoryCommitCompat,
    bead_id: &str,
    existing_feedback: Option<&CorrelationFeedback>,
) -> CorrelationExplanation {
    let mut signals = Vec::new();

    // Primary signal based on correlation method
    match commit.method.as_str() {
        "co_committed" => {
            signals.push(CorrelationSignal {
                signal_type: "co_commit".to_string(),
                weight: 50,
                detail: "Commit modified both code and beads file together".to_string(),
            });
        }
        "explicit_id" => {
            signals.push(CorrelationSignal {
                signal_type: "message_match".to_string(),
                weight: 40,
                detail: format!("Commit message references bead ID '{bead_id}'"),
            });
        }
        "temporal_author" => {
            signals.push(CorrelationSignal {
                signal_type: "timing".to_string(),
                weight: 25,
                detail: "Commit within bead's active time window".to_string(),
            });
            signals.push(CorrelationSignal {
                signal_type: "author_match".to_string(),
                weight: 15,
                detail: format!("Same author: {}", commit.author),
            });
        }
        _ => {
            signals.push(CorrelationSignal {
                signal_type: "unknown".to_string(),
                weight: 10,
                detail: format!("Correlation method: {}", commit.method),
            });
        }
    }

    // File overlap signal
    let file_count = commit.files.len();
    if file_count > 0 {
        let weight = (file_count as u32 * 5).min(15);
        signals.push(CorrelationSignal {
            signal_type: "file_overlap".to_string(),
            weight,
            detail: format!("{file_count} file(s) modified in this commit"),
        });
    }

    let total_weight: u32 = signals.iter().map(|s| s.weight).sum();
    let level = confidence_level(commit.confidence);
    let confidence_pct = (commit.confidence * 100.0) as u32;

    let summary = format!(
        "{} with bead update ({confidence_pct}% confidence, {} signal{})",
        match commit.method.as_str() {
            "co_committed" => "Co-committed",
            "explicit_id" => "Explicit ID reference",
            "temporal_author" => "Temporal+author match",
            _ => "Unknown method",
        },
        signals.len(),
        if signals.len() == 1 { "" } else { "s" }
    );

    let recommendation = if let Some(fb) = existing_feedback {
        format!(
            "Already {} by {} at {}",
            fb.feedback_type, fb.feedback_by, fb.feedback_at
        )
    } else if commit.confidence >= 0.75 {
        "High confidence - likely correct, no action needed".to_string()
    } else if commit.confidence >= 0.5 {
        "Moderate confidence - review recommended".to_string()
    } else {
        "Low confidence - manual verification suggested".to_string()
    };

    CorrelationExplanation {
        commit_sha: commit.sha.clone(),
        bead_id: bead_id.to_string(),
        confidence: commit.confidence,
        confidence_pct,
        level: level.to_string(),
        method: commit.method.clone(),
        signals,
        total_weight,
        summary,
        recommendation,
    }
}

/// Parse a "SHA:beadID" argument into (commit_sha, bead_id).
pub fn parse_correlation_arg(arg: &str) -> crate::Result<(String, String)> {
    let parts: Vec<&str> = arg.splitn(2, ':').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(crate::BvrError::InvalidArgument(format!(
            "Expected format SHA:beadID, got '{arg}'"
        )));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Default feedback file path: `.beads/correlation_feedback.jsonl` relative to repo root.
#[must_use]
pub fn default_feedback_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".beads").join("correlation_feedback.jsonl")
}

// ---------------------------------------------------------------------------
// Robot output structs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RobotExplainOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    pub explanation: CorrelationExplanation,
}

#[derive(Debug, Serialize)]
pub struct RobotCorrelationActionOutput {
    pub status: String,
    pub commit: String,
    pub bead: String,
    pub by: String,
    pub reason: String,
    pub orig_conf: f64,
}

#[derive(Debug, Serialize)]
pub struct RobotCorrelationStatsOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    #[serde(flatten)]
    pub stats: FeedbackStats,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::git_history::{HistoryCommitCompat, HistoryFileChangeCompat};
    use tempfile::TempDir;

    fn make_commit(method: &str, confidence: f64) -> HistoryCommitCompat {
        HistoryCommitCompat {
            sha: "abc123def456".to_string(),
            short_sha: "abc123d".to_string(),
            message: "feat(bd-test): implement feature".to_string(),
            author: "TestUser".to_string(),
            author_email: "test@example.com".to_string(),
            timestamp: "2026-01-15T10:00:00Z".to_string(),
            files: vec![HistoryFileChangeCompat {
                path: "src/main.rs".to_string(),
                action: "M".to_string(),
                insertions: 10,
                deletions: 2,
            }],
            method: method.to_string(),
            confidence,
            reason: "test reason".to_string(),
        }
    }

    #[test]
    fn feedback_store_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");

        {
            let mut store = FeedbackStore::open(&path).unwrap();
            assert_eq!(store.stats().total_feedback, 0);

            store
                .confirm("sha1", "bd-1", "agent-a", 0.9, "looks good")
                .unwrap();
            store
                .reject("sha2", "bd-2", "agent-b", 0.3, "false positive")
                .unwrap();

            assert!(store.has_feedback("sha1", "bd-1"));
            assert!(!store.has_feedback("sha1", "bd-2"));

            let stats = store.stats();
            assert_eq!(stats.total_feedback, 2);
            assert_eq!(stats.confirmed, 1);
            assert_eq!(stats.rejected, 1);
        }

        // Reopen store and verify persistence
        {
            let store = FeedbackStore::open(&path).unwrap();
            assert_eq!(store.stats().total_feedback, 2);
            assert!(store.has_feedback("sha1", "bd-1"));

            let fb = store.get("sha1", "bd-1").unwrap();
            assert_eq!(fb.feedback_type, FeedbackType::Confirm);
            assert_eq!(fb.feedback_by, "agent-a");
        }
    }

    #[test]
    fn feedback_store_get_by_bead() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");

        let mut store = FeedbackStore::open(&path).unwrap();
        store.confirm("sha1", "bd-1", "agent", 0.8, "").unwrap();
        store.confirm("sha2", "bd-1", "agent", 0.7, "").unwrap();
        store.confirm("sha3", "bd-2", "agent", 0.9, "").unwrap();

        let entries = store.get_by_bead("bd-1");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn feedback_stats_accuracy() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("feedback.jsonl");

        let mut store = FeedbackStore::open(&path).unwrap();
        for i in 0..8 {
            store
                .confirm(&format!("sha-c{i}"), "bd-1", "agent", 0.8, "")
                .unwrap();
        }
        for i in 0..2 {
            store
                .reject(&format!("sha-r{i}"), "bd-1", "agent", 0.3, "")
                .unwrap();
        }

        let stats = store.stats();
        assert_eq!(stats.confirmed, 8);
        assert_eq!(stats.rejected, 2);
        assert!((stats.accuracy_rate - 0.8).abs() < 0.001);
        assert!((stats.avg_confirm_conf - 0.8).abs() < 0.001);
        assert!((stats.avg_reject_conf - 0.3).abs() < 0.001);
    }

    #[test]
    fn explanation_co_committed() {
        let commit = make_commit("co_committed", 0.95);
        let explanation = build_explanation(&commit, "bd-test", None);

        assert_eq!(explanation.level, "very high");
        assert_eq!(explanation.confidence_pct, 95);
        assert!(
            explanation
                .signals
                .iter()
                .any(|s| s.signal_type == "co_commit")
        );
        assert!(explanation.recommendation.contains("likely correct"));
    }

    #[test]
    fn explanation_explicit_id() {
        let commit = make_commit("explicit_id", 0.75);
        let explanation = build_explanation(&commit, "bd-test", None);

        assert_eq!(explanation.level, "high");
        assert!(
            explanation
                .signals
                .iter()
                .any(|s| s.signal_type == "message_match")
        );
    }

    #[test]
    fn explanation_low_confidence() {
        let commit = make_commit("temporal_author", 0.25);
        let explanation = build_explanation(&commit, "bd-test", None);

        assert_eq!(explanation.level, "very low");
        assert!(explanation.recommendation.contains("manual verification"));
    }

    #[test]
    fn explanation_with_existing_feedback() {
        let commit = make_commit("co_committed", 0.95);
        let fb = CorrelationFeedback {
            commit_sha: "abc123def456".to_string(),
            bead_id: "bd-test".to_string(),
            feedback_at: "2026-01-15T12:00:00Z".to_string(),
            feedback_by: "agent-x".to_string(),
            feedback_type: FeedbackType::Confirm,
            reason: "verified".to_string(),
            original_conf: 0.95,
        };
        let explanation = build_explanation(&commit, "bd-test", Some(&fb));

        assert!(explanation.recommendation.contains("Already confirm"));
        assert!(explanation.recommendation.contains("agent-x"));
    }

    #[test]
    fn parse_correlation_arg_valid() {
        let (sha, bead) = parse_correlation_arg("abc123:bd-test").unwrap();
        assert_eq!(sha, "abc123");
        assert_eq!(bead, "bd-test");
    }

    #[test]
    fn parse_correlation_arg_invalid() {
        assert!(parse_correlation_arg("no-colon").is_err());
        assert!(parse_correlation_arg(":bd-test").is_err());
        assert!(parse_correlation_arg("sha:").is_err());
    }

    #[test]
    fn confidence_level_boundaries() {
        assert_eq!(confidence_level(0.95), "very high");
        assert_eq!(confidence_level(0.90), "very high");
        assert_eq!(confidence_level(0.89), "high");
        assert_eq!(confidence_level(0.75), "high");
        assert_eq!(confidence_level(0.74), "moderate");
        assert_eq!(confidence_level(0.50), "moderate");
        assert_eq!(confidence_level(0.49), "low");
        assert_eq!(confidence_level(0.30), "low");
        assert_eq!(confidence_level(0.29), "very low");
        assert_eq!(confidence_level(0.0), "very low");
    }

    #[test]
    fn empty_feedback_store_stats() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let store = FeedbackStore::open(&path).unwrap();
        let stats = store.stats();

        assert_eq!(stats.total_feedback, 0);
        assert_eq!(stats.accuracy_rate, 0.0);
    }
}
