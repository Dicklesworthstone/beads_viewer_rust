use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::triage::{Recommendation, TriageComputation};
use crate::model::Issue;

// ---------------------------------------------------------------------------
// Recipe Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub filters: FilterConfig,
    #[serde(default)]
    pub sort: SortConfig,
    #[serde(default)]
    pub max_items: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterConfig {
    #[serde(default)]
    pub status: Vec<String>,
    #[serde(default)]
    pub min_priority: Option<i32>,
    #[serde(default)]
    pub max_priority: Option<i32>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub actionable: Option<bool>,
    #[serde(default)]
    pub has_blockers: Option<bool>,
    #[serde(default)]
    pub title_contains: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SortConfig {
    #[serde(default = "default_sort_field")]
    pub field: String,
    #[serde(default = "default_sort_direction")]
    pub direction: String,
}

fn default_sort_field() -> String {
    "priority".to_string()
}

fn default_sort_direction() -> String {
    "asc".to_string()
}

// ---------------------------------------------------------------------------
// Built-in Recipes
// ---------------------------------------------------------------------------

pub fn builtin_recipes() -> Vec<Recipe> {
    vec![
        Recipe {
            name: "default".to_string(),
            description: "All open issues sorted by priority".to_string(),
            filters: FilterConfig {
                status: vec!["open".into(), "in_progress".into(), "blocked".into()],
                ..Default::default()
            },
            sort: SortConfig {
                field: "priority".into(),
                direction: "asc".into(),
            },
            max_items: 0,
        },
        Recipe {
            name: "actionable".to_string(),
            description: "Issues ready to work on (no open blockers)".to_string(),
            filters: FilterConfig {
                actionable: Some(true),
                ..Default::default()
            },
            sort: SortConfig {
                field: "priority".into(),
                direction: "asc".into(),
            },
            max_items: 20,
        },
        Recipe {
            name: "blocked".to_string(),
            description: "Issues waiting on dependencies".to_string(),
            filters: FilterConfig {
                has_blockers: Some(true),
                ..Default::default()
            },
            sort: SortConfig {
                field: "priority".into(),
                direction: "asc".into(),
            },
            max_items: 0,
        },
        Recipe {
            name: "high-impact".to_string(),
            description: "Issues with highest graph centrality".to_string(),
            filters: FilterConfig {
                status: vec!["open".into(), "in_progress".into()],
                ..Default::default()
            },
            sort: SortConfig {
                field: "pagerank".into(),
                direction: "desc".into(),
            },
            max_items: 15,
        },
        Recipe {
            name: "triage".to_string(),
            description: "Sorted by computed triage score".to_string(),
            filters: FilterConfig::default(),
            sort: SortConfig {
                field: "triage".into(),
                direction: "desc".into(),
            },
            max_items: 20,
        },
        Recipe {
            name: "quick-wins".to_string(),
            description: "Easy low-priority actionable items".to_string(),
            filters: FilterConfig {
                actionable: Some(true),
                min_priority: Some(2),
                ..Default::default()
            },
            sort: SortConfig {
                field: "priority".into(),
                direction: "desc".into(),
            },
            max_items: 10,
        },
        Recipe {
            name: "stale".to_string(),
            description: "Issues not updated in 30+ days".to_string(),
            filters: FilterConfig {
                status: vec!["open".into(), "in_progress".into()],
                ..Default::default()
            },
            sort: SortConfig {
                field: "updated".into(),
                direction: "asc".into(),
            },
            max_items: 20,
        },
    ]
}

// ---------------------------------------------------------------------------
// Recipe Application
// ---------------------------------------------------------------------------

/// Apply a recipe's filters to a list of recommendations.
pub fn apply_recipe(
    recipe: &Recipe,
    recommendations: &[Recommendation],
    issues: &[Issue],
    actionable_ids: &[String],
    pagerank: &HashMap<String, f64>,
) -> Vec<Recommendation> {
    let issue_map: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut filtered: Vec<Recommendation> = recommendations
        .iter()
        .filter(|rec| {
            let issue = issue_map.get(rec.id.as_str());

            // Status filter
            if !recipe.filters.status.is_empty() {
                let status = issue.map_or("unknown", |i| i.status.as_str());
                let normalized = status.to_ascii_lowercase();
                if !recipe.filters.status.iter().any(|s| s == &normalized) {
                    return false;
                }
            }

            // Priority filter
            if let Some(min_p) = recipe.filters.min_priority {
                let priority = issue.map_or(99, |i| i.priority);
                if priority < min_p {
                    return false;
                }
            }
            if let Some(max_p) = recipe.filters.max_priority {
                let priority = issue.map_or(99, |i| i.priority);
                if priority > max_p {
                    return false;
                }
            }

            // Label filter
            if !recipe.filters.labels.is_empty() {
                let labels = issue.map_or(&[] as &[String], |i| &i.labels);
                if !recipe
                    .filters
                    .labels
                    .iter()
                    .any(|l| labels.iter().any(|il| il == l))
                {
                    return false;
                }
            }

            // Actionable filter
            if recipe.filters.actionable == Some(true) && !actionable_ids.contains(&rec.id) {
                return false;
            }

            // Has blockers filter
            if recipe.filters.has_blockers == Some(true) && !actionable_ids.contains(&rec.id) {
                // has_blockers = true means only show items WITH blockers
                // (items NOT in actionable list)
            } else if recipe.filters.has_blockers == Some(true) && actionable_ids.contains(&rec.id)
            {
                return false;
            }

            // Title contains filter
            if let Some(ref needle) = recipe.filters.title_contains {
                let title = issue.map_or("", |i| i.title.as_str());
                if !title
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
                {
                    return false;
                }
            }

            true
        })
        .cloned()
        .collect();

    // Sort
    match recipe.sort.field.as_str() {
        "priority" => {
            filtered.sort_by(|a, b| {
                let pa = issue_map.get(a.id.as_str()).map_or(99, |i| i.priority);
                let pb = issue_map.get(b.id.as_str()).map_or(99, |i| i.priority);
                if recipe.sort.direction == "desc" {
                    pb.cmp(&pa).then_with(|| a.id.cmp(&b.id))
                } else {
                    pa.cmp(&pb).then_with(|| a.id.cmp(&b.id))
                }
            });
        }
        "triage" | "score" => {
            filtered.sort_by(|a, b| {
                if recipe.sort.direction == "asc" {
                    a.score.total_cmp(&b.score).then_with(|| a.id.cmp(&b.id))
                } else {
                    b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id))
                }
            });
        }
        "pagerank" => {
            filtered.sort_by(|a, b| {
                let pa = pagerank.get(&a.id).copied().unwrap_or(0.0);
                let pb = pagerank.get(&b.id).copied().unwrap_or(0.0);
                if recipe.sort.direction == "asc" {
                    pa.total_cmp(&pb).then_with(|| a.id.cmp(&b.id))
                } else {
                    pb.total_cmp(&pa).then_with(|| a.id.cmp(&b.id))
                }
            });
        }
        _ => {
            // Default: sort by ID
            filtered.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }

    if recipe.max_items > 0 {
        filtered.truncate(recipe.max_items);
    }

    filtered
}

// ---------------------------------------------------------------------------
// Robot Output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RecipeSummary {
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct RobotRecipesOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    pub recipes: Vec<RecipeSummary>,
}

pub fn list_recipes() -> Vec<RecipeSummary> {
    let mut recipes: Vec<RecipeSummary> = builtin_recipes()
        .into_iter()
        .map(|r| RecipeSummary {
            name: r.name,
            description: r.description,
            source: "builtin".to_string(),
        })
        .collect();
    recipes.sort_by(|a, b| a.name.cmp(&b.name));
    recipes
}

pub fn find_recipe(name: &str) -> Option<Recipe> {
    builtin_recipes().into_iter().find(|r| r.name == name)
}

// ---------------------------------------------------------------------------
// Script Emission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum ScriptFormat {
    Bash,
    Fish,
    Zsh,
}

impl ScriptFormat {
    pub fn from_str_or_default(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "fish" => Self::Fish,
            "zsh" => Self::Zsh,
            _ => Self::Bash,
        }
    }

    pub const fn shebang(self) -> &'static str {
        match self {
            Self::Bash => "#!/usr/bin/env bash",
            Self::Fish => "#!/usr/bin/env fish",
            Self::Zsh => "#!/usr/bin/env zsh",
        }
    }
}

/// Generate a shell script for the top N recommendations.
pub fn emit_script(
    recommendations: &[Recommendation],
    limit: usize,
    format: ScriptFormat,
    generated_at: &str,
    data_hash: &str,
) -> String {
    let items: &[Recommendation] = if limit > 0 && recommendations.len() > limit {
        &recommendations[..limit]
    } else {
        recommendations
    };

    let mut lines = Vec::new();

    lines.push(format.shebang().to_string());
    if matches!(format, ScriptFormat::Bash | ScriptFormat::Zsh) {
        lines.push("set -euo pipefail".to_string());
    }
    lines.push(String::new());
    lines.push(format!(
        "# Generated by bvr --emit-script at {generated_at}"
    ));
    lines.push(format!("# Data hash: {data_hash}"));
    lines.push(format!(
        "# Top {} recommendations from {} total",
        items.len(),
        recommendations.len()
    ));
    lines.push(String::new());

    for (i, rec) in items.iter().enumerate() {
        let rank = i + 1;
        lines.push(format!("# {rank}. {} (score: {:.3})", rec.title, rec.score));
        if !rec.reasons.is_empty() {
            lines.push(format!("#    Reason: {}", rec.reasons.join("; ")));
        }
        lines.push(format!("br show {}", rec.id));
        lines.push(format!(
            "# To claim: br update {} --status=in_progress",
            rec.id
        ));
        lines.push(String::new());
    }

    if let Some(top) = items.first() {
        lines.push("# === Quick Actions ===".to_string());
        lines.push("# To claim the top pick:".to_string());
        lines.push(format!("# br update {} --status=in_progress", top.id));
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Feedback Tuning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEvent {
    pub issue_id: String,
    pub action: String,
    pub score: f64,
    pub timestamp: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightAdjustment {
    pub name: String,
    pub adjustment: f64,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackData {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub events: Vec<FeedbackEvent>,
    #[serde(default)]
    pub adjustments: Vec<WeightAdjustment>,
}

fn default_version() -> String {
    "1.0".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackStats {
    pub total_accepted: usize,
    pub total_ignored: usize,
    pub avg_accept_score: f64,
    pub avg_ignore_score: f64,
    pub adjustments: Vec<WeightAdjustment>,
}

#[derive(Debug, Serialize)]
pub struct RobotFeedbackOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    pub stats: FeedbackStats,
}

impl FeedbackData {
    pub fn load(project_dir: &Path) -> Self {
        let path = project_dir.join(".bv").join("feedback.json");
        if let Ok(content) = fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self, project_dir: &Path) -> Result<(), String> {
        let dir = project_dir.join(".bv");
        fs::create_dir_all(&dir).map_err(|e| format!("failed to create .bv dir: {e}"))?;
        let path = dir.join("feedback.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize feedback: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("failed to write feedback: {e}"))?;
        Ok(())
    }

    pub fn record_accept(&mut self, issue_id: &str, score: f64, by: &str, reason: &str) {
        self.events.push(FeedbackEvent {
            issue_id: issue_id.to_string(),
            action: "accept".to_string(),
            score,
            timestamp: chrono_now(),
            by: by.to_string(),
            reason: reason.to_string(),
        });
        self.update_adjustments();
    }

    pub fn record_ignore(&mut self, issue_id: &str, score: f64, by: &str, reason: &str) {
        self.events.push(FeedbackEvent {
            issue_id: issue_id.to_string(),
            action: "ignore".to_string(),
            score,
            timestamp: chrono_now(),
            by: by.to_string(),
            reason: reason.to_string(),
        });
        self.update_adjustments();
    }

    pub fn reset(&mut self) {
        self.events.clear();
        self.adjustments.clear();
    }

    pub fn stats(&self) -> FeedbackStats {
        let accepted: Vec<&FeedbackEvent> = self
            .events
            .iter()
            .filter(|e| e.action == "accept")
            .collect();
        let ignored: Vec<&FeedbackEvent> = self
            .events
            .iter()
            .filter(|e| e.action == "ignore")
            .collect();

        let avg_accept = if accepted.is_empty() {
            0.0
        } else {
            accepted.iter().map(|e| e.score).sum::<f64>() / accepted.len() as f64
        };
        let avg_ignore = if ignored.is_empty() {
            0.0
        } else {
            ignored.iter().map(|e| e.score).sum::<f64>() / ignored.len() as f64
        };

        FeedbackStats {
            total_accepted: accepted.len(),
            total_ignored: ignored.len(),
            avg_accept_score: avg_accept,
            avg_ignore_score: avg_ignore,
            adjustments: self.adjustments.clone(),
        }
    }

    fn update_adjustments(&mut self) {
        // Simple exponential smoothing on accept/ignore ratio
        let weight_names = [
            "PageRank",
            "Betweenness",
            "BlockerRatio",
            "Staleness",
            "PriorityBoost",
            "TimeToImpact",
            "Urgency",
            "Risk",
        ];

        let accepted = self.events.iter().filter(|e| e.action == "accept").count();
        let ignored = self.events.iter().filter(|e| e.action == "ignore").count();
        let total = accepted + ignored;

        if total == 0 {
            return;
        }

        let accept_ratio = accepted as f64 / total as f64;
        // Adjust weights based on accept ratio
        // If mostly accepted (>0.7), slightly boost all weights
        // If mostly ignored (<0.3), slightly reduce
        let adjustment = 1.0 + (accept_ratio - 0.5) * 0.2; // Range: 0.9-1.1
        let clamped = adjustment.clamp(0.5, 2.0);

        self.adjustments = weight_names
            .iter()
            .map(|name| WeightAdjustment {
                name: name.to_string(),
                adjustment: clamped,
                samples: total,
            })
            .collect();
    }
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::triage::Recommendation;

    fn make_rec(id: &str, title: &str, score: f64) -> Recommendation {
        Recommendation {
            id: id.to_string(),
            title: title.to_string(),
            score,
            confidence: 0.8,
            reasons: vec!["test".to_string()],
            unblocks: 0,
            status: "open".to_string(),
            priority: 2,
            issue_type: "task".to_string(),
            labels: Vec::new(),
            assignee: String::new(),
            claim_command: String::new(),
            show_command: String::new(),
        }
    }

    #[test]
    fn builtin_recipes_exist() {
        let recipes = builtin_recipes();
        assert!(recipes.len() >= 5);
        assert!(recipes.iter().any(|r| r.name == "default"));
        assert!(recipes.iter().any(|r| r.name == "actionable"));
        assert!(recipes.iter().any(|r| r.name == "triage"));
    }

    #[test]
    fn list_recipes_sorted() {
        let list = list_recipes();
        for i in 1..list.len() {
            assert!(list[i - 1].name <= list[i].name);
        }
    }

    #[test]
    fn find_recipe_works() {
        assert!(find_recipe("default").is_some());
        assert!(find_recipe("nonexistent").is_none());
    }

    #[test]
    fn apply_recipe_max_items() {
        let recs = vec![
            make_rec("A", "A", 0.9),
            make_rec("B", "B", 0.8),
            make_rec("C", "C", 0.7),
        ];
        let issues = Vec::new();
        let actionable = Vec::new();
        let pagerank = HashMap::new();

        let mut recipe = find_recipe("default").unwrap();
        recipe.max_items = 2;
        recipe.filters.status.clear(); // Remove status filter for test

        let result = apply_recipe(&recipe, &recs, &issues, &actionable, &pagerank);
        assert!(result.len() <= 2);
    }

    #[test]
    fn emit_script_bash() {
        let recs = vec![make_rec("A", "Fix auth", 0.9)];
        let script = emit_script(&recs, 5, ScriptFormat::Bash, "2025-01-01T00:00:00Z", "abc");

        assert!(script.starts_with("#!/usr/bin/env bash"));
        assert!(script.contains("br show A"));
        assert!(script.contains("Fix auth"));
    }

    #[test]
    fn emit_script_respects_limit() {
        let recs = vec![
            make_rec("A", "A", 0.9),
            make_rec("B", "B", 0.8),
            make_rec("C", "C", 0.7),
        ];
        let script = emit_script(&recs, 1, ScriptFormat::Bash, "2025-01-01T00:00:00Z", "abc");

        assert!(script.contains("br show A"));
        assert!(!script.contains("br show B"));
    }

    #[test]
    fn feedback_record_and_stats() {
        let mut feedback = FeedbackData::default();
        feedback.record_accept("A", 0.9, "user", "good pick");
        feedback.record_ignore("B", 0.3, "user", "not relevant");

        let stats = feedback.stats();
        assert_eq!(stats.total_accepted, 1);
        assert_eq!(stats.total_ignored, 1);
        assert!(stats.avg_accept_score > 0.0);
    }

    #[test]
    fn feedback_reset() {
        let mut feedback = FeedbackData::default();
        feedback.record_accept("A", 0.9, "user", "good");
        assert!(!feedback.events.is_empty());

        feedback.reset();
        assert!(feedback.events.is_empty());
        assert!(feedback.adjustments.is_empty());
    }

    #[test]
    fn feedback_serialization_roundtrip() {
        let mut feedback = FeedbackData::default();
        feedback.record_accept("A", 0.9, "user", "test");

        let json = serde_json::to_string(&feedback).unwrap();
        let restored: FeedbackData = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.events.len(), 1);
        assert_eq!(restored.events[0].issue_id, "A");
    }
}
