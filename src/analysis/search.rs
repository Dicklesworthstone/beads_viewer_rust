use std::collections::HashMap;

use serde::Serialize;

use super::graph::GraphMetrics;
use crate::model::Issue;

// ---------------------------------------------------------------------------
// Search Modes and Presets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Text,
    Hybrid,
}

impl SearchMode {
    pub fn from_str_or_default(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "hybrid" => Self::Hybrid,
            _ => Self::Text,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchWeights {
    pub text: f64,
    pub pagerank: f64,
    pub status: f64,
    pub impact: f64,
    pub priority: f64,
    pub recency: f64,
}

impl SearchWeights {
    pub fn normalize(&self) -> Self {
        let sum =
            self.text + self.pagerank + self.status + self.impact + self.priority + self.recency;
        if sum <= 0.0 {
            return Self::default_preset();
        }
        Self {
            text: self.text / sum,
            pagerank: self.pagerank / sum,
            status: self.status / sum,
            impact: self.impact / sum,
            priority: self.priority / sum,
            recency: self.recency / sum,
        }
    }
}

pub fn get_preset(name: &str) -> SearchWeights {
    match name.to_ascii_lowercase().as_str() {
        "bug-hunting" => SearchWeights {
            text: 0.30,
            pagerank: 0.15,
            status: 0.15,
            impact: 0.15,
            priority: 0.20,
            recency: 0.05,
        },
        "sprint-planning" => SearchWeights {
            text: 0.30,
            pagerank: 0.20,
            status: 0.25,
            impact: 0.15,
            priority: 0.05,
            recency: 0.05,
        },
        "impact-first" => SearchWeights {
            text: 0.25,
            pagerank: 0.30,
            status: 0.10,
            impact: 0.20,
            priority: 0.10,
            recency: 0.05,
        },
        "text-only" => SearchWeights {
            text: 1.0,
            pagerank: 0.0,
            status: 0.0,
            impact: 0.0,
            priority: 0.0,
            recency: 0.0,
        },
        _ => SearchWeights::default_preset(),
    }
}

impl SearchWeights {
    pub fn default_preset() -> Self {
        Self {
            text: 0.40,
            pagerank: 0.20,
            status: 0.15,
            impact: 0.10,
            priority: 0.10,
            recency: 0.05,
        }
    }

    /// Parse custom weights from JSON string.
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        let map: HashMap<String, f64> =
            serde_json::from_str(json_str).map_err(|e| format!("invalid weights JSON: {e}"))?;

        let weights = Self {
            text: map.get("text").copied().unwrap_or(0.0),
            pagerank: map.get("pagerank").copied().unwrap_or(0.0),
            status: map.get("status").copied().unwrap_or(0.0),
            impact: map.get("impact").copied().unwrap_or(0.0),
            priority: map.get("priority").copied().unwrap_or(0.0),
            recency: map.get("recency").copied().unwrap_or(0.0),
        };

        // Validate: all non-negative
        if weights.text < 0.0
            || weights.pagerank < 0.0
            || weights.status < 0.0
            || weights.impact < 0.0
            || weights.priority < 0.0
            || weights.recency < 0.0
        {
            return Err("all weights must be non-negative".to_string());
        }

        Ok(weights.normalize())
    }
}

// ---------------------------------------------------------------------------
// Text Scoring
// ---------------------------------------------------------------------------

/// Compute text relevance score for an issue against a query.
fn compute_text_score(query: &str, issue: &Issue) -> f64 {
    let query_lower = query.to_ascii_lowercase();
    let tokens: Vec<&str> = query_lower.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.0;
    }

    // Build document text with field weighting (ID×3, title×2, labels, description)
    let doc = format!(
        "{id} {id} {id} {title} {title} {labels} {desc}",
        id = issue.id.to_ascii_lowercase(),
        title = issue.title.to_ascii_lowercase(),
        labels = issue.labels.join(" ").to_ascii_lowercase(),
        desc = issue.description.to_ascii_lowercase(),
    );

    // Exact issue ID match gets maximum boost
    if issue.id.to_ascii_lowercase() == query_lower {
        return 1.0;
    }

    // Count token hits
    let mut hit_count = 0usize;
    for token in &tokens {
        if doc.contains(token) {
            hit_count += 1;
        }
    }

    if hit_count == 0 {
        return 0.0;
    }

    let token_coverage = hit_count as f64 / tokens.len() as f64;

    // Bonus for substring match in title
    let title_lower = issue.title.to_ascii_lowercase();
    let title_bonus = if title_lower.contains(&query_lower) {
        0.3
    } else {
        0.0
    };

    // Bonus for substring match in ID
    let id_lower = issue.id.to_ascii_lowercase();
    let id_bonus = if id_lower.contains(&query_lower) {
        0.2
    } else {
        0.0
    };

    (token_coverage * 0.5 + title_bonus + id_bonus).min(1.0)
}

/// Short query detection (≤2 tokens or ≤12 chars).
fn is_short_query(query: &str) -> bool {
    let tokens = query.split_whitespace().count();
    tokens <= 2 || query.len() <= 12
}

/// Adjust weights for short queries (boost text to minimum 0.55).
fn adjust_weights_for_short_query(weights: &SearchWeights) -> SearchWeights {
    if weights.text >= 0.55 {
        return weights.clone();
    }
    let target = 0.55;
    let remaining = 1.0 - weights.text;
    if remaining <= 0.0 {
        return weights.clone();
    }
    let scale = (1.0 - target) / remaining;
    SearchWeights {
        text: target,
        pagerank: weights.pagerank * scale,
        status: weights.status * scale,
        impact: weights.impact * scale,
        priority: weights.priority * scale,
        recency: weights.recency * scale,
    }
    .normalize()
}

// ---------------------------------------------------------------------------
// Normalization functions
// ---------------------------------------------------------------------------

fn normalize_status(status: &str) -> f64 {
    match status.to_ascii_lowercase().as_str() {
        "open" => 1.0,
        "in_progress" => 0.8,
        "blocked" => 0.5,
        "closed" => 0.1,
        "tombstone" => 0.0,
        _ => 0.5,
    }
}

fn normalize_priority(priority: i32) -> f64 {
    match priority.clamp(0, 4) {
        0 => 1.0,
        1 => 0.8,
        2 => 0.6,
        3 => 0.4,
        _ => 0.2,
    }
}

fn normalize_impact(blocks_count: usize, max_blocks: usize) -> f64 {
    if max_blocks == 0 {
        return 0.5;
    }
    blocks_count as f64 / max_blocks as f64
}

fn normalize_recency(updated_at: Option<&str>) -> f64 {
    let Some(ts) = updated_at else {
        return 0.0;
    };
    // Parse date and compute days since update
    let now_ms = super::causal::parse_timestamp_ms_pub("2026-03-04T00:00:00Z").unwrap_or(0);
    let ts_ms = super::causal::parse_timestamp_ms_pub(ts).unwrap_or(0);
    if ts_ms == 0 || now_ms == 0 || ts_ms > now_ms {
        return 0.5;
    }
    let days = (now_ms - ts_ms) / 86_400_000;
    // Exponential decay with half-life ~30 days
    (-1.0 * days as f64 / 30.0_f64).exp()
}

// ---------------------------------------------------------------------------
// Search Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub issue_id: String,
    pub score: f64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_scores: Option<ComponentScores>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentScores {
    pub pagerank: f64,
    pub status: f64,
    pub impact: f64,
    pub priority: f64,
    pub recency: f64,
}

#[derive(Debug, Serialize)]
pub struct RobotSearchOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
    pub query: String,
    pub limit: usize,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<SearchWeights>,
    pub results: Vec<SearchResult>,
}

/// Execute a search query against all issues.
pub fn execute_search(
    query: &str,
    issues: &[Issue],
    metrics: &GraphMetrics,
    mode: SearchMode,
    weights: &SearchWeights,
    limit: usize,
) -> Vec<SearchResult> {
    let max_blocks = metrics.blocks_count.values().copied().max().unwrap_or(0);

    let effective_weights = if mode == SearchMode::Hybrid && is_short_query(query) {
        adjust_weights_for_short_query(weights)
    } else {
        weights.clone()
    };

    let mut results: Vec<SearchResult> = issues
        .iter()
        .filter_map(|issue| {
            let text_score = compute_text_score(query, issue);

            // In text mode, only return issues with non-zero text score
            if mode == SearchMode::Text && text_score <= 0.0 {
                return None;
            }

            // Short query lexical boost
            let lexical_boost = if is_short_query(query) {
                let doc = format!(
                    "{} {} {} {}",
                    issue.id,
                    issue.title,
                    issue.labels.join(" "),
                    issue.description,
                );
                if doc
                    .to_ascii_lowercase()
                    .contains(&query.to_ascii_lowercase())
                {
                    0.35
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let boosted_text = (text_score + lexical_boost).min(1.0);

            let score = match mode {
                SearchMode::Text => boosted_text,
                SearchMode::Hybrid => {
                    let pagerank = metrics.pagerank.get(&issue.id).copied().unwrap_or(0.0);
                    let status = normalize_status(&issue.status);
                    let blocks = metrics.blocks_count.get(&issue.id).copied().unwrap_or(0);
                    let impact = normalize_impact(blocks, max_blocks);
                    let priority = normalize_priority(issue.priority);
                    let recency = normalize_recency(issue.updated_at.as_deref());

                    effective_weights.text * boosted_text
                        + effective_weights.pagerank * pagerank
                        + effective_weights.status * status
                        + effective_weights.impact * impact
                        + effective_weights.priority * priority
                        + effective_weights.recency * recency
                }
            };

            // Skip zero scores in hybrid mode too
            if score <= 0.0 {
                return None;
            }

            let (text_score_field, components) = match mode {
                SearchMode::Text => (None, None),
                SearchMode::Hybrid => {
                    let pagerank = metrics.pagerank.get(&issue.id).copied().unwrap_or(0.0);
                    let status = normalize_status(&issue.status);
                    let blocks = metrics.blocks_count.get(&issue.id).copied().unwrap_or(0);
                    let impact_val = normalize_impact(blocks, max_blocks);
                    let priority_val = normalize_priority(issue.priority);
                    let recency_val = normalize_recency(issue.updated_at.as_deref());

                    (
                        Some(boosted_text),
                        Some(ComponentScores {
                            pagerank,
                            status,
                            impact: impact_val,
                            priority: priority_val,
                            recency: recency_val,
                        }),
                    )
                }
            };

            Some(SearchResult {
                issue_id: issue.id.clone(),
                score,
                title: issue.title.clone(),
                text_score: text_score_field,
                component_scores: components,
            })
        })
        .collect();

    // Sort by score descending, then by ID for determinism
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.issue_id.cmp(&b.issue_id))
    });

    // Promote exact ID match to top
    if let Some(pos) = results
        .iter()
        .position(|r| r.issue_id.to_ascii_lowercase() == query.to_ascii_lowercase())
    {
        if pos > 0 {
            let exact = results.remove(pos);
            results.insert(0, exact);
        }
    }

    if limit > 0 {
        results.truncate(limit);
    }

    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::graph::IssueGraph;
    use crate::model::Issue;

    fn make_issue(id: &str, title: &str, status: &str, priority: i32) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            priority,
            ..Issue::default()
        }
    }

    fn make_issues_and_metrics() -> (Vec<Issue>, GraphMetrics) {
        let issues = vec![
            make_issue("AUTH-1", "Fix authentication bug", "open", 0),
            make_issue("NET-2", "Network timeout handling", "in_progress", 1),
            make_issue("DB-3", "Database migration script", "open", 2),
            make_issue("AUTH-4", "OAuth token refresh", "blocked", 1),
            make_issue("UI-5", "Dashboard layout fix", "closed", 3),
        ];
        let graph = IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();
        (issues, metrics)
    }

    #[test]
    fn text_search_basic() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let results = execute_search(
            "authentication",
            &issues,
            &metrics,
            SearchMode::Text,
            &weights,
            10,
        );

        assert!(!results.is_empty());
        assert_eq!(results[0].issue_id, "AUTH-1");
    }

    #[test]
    fn text_search_no_results() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let results = execute_search(
            "zzzznotfound",
            &issues,
            &metrics,
            SearchMode::Text,
            &weights,
            10,
        );

        assert!(results.is_empty());
    }

    #[test]
    fn text_search_limit() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let results = execute_search("fix", &issues, &metrics, SearchMode::Text, &weights, 1);

        assert!(results.len() <= 1);
    }

    #[test]
    fn exact_id_match_promoted() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let results = execute_search("DB-3", &issues, &metrics, SearchMode::Text, &weights, 10);

        assert!(!results.is_empty());
        assert_eq!(results[0].issue_id, "DB-3");
    }

    #[test]
    fn hybrid_mode_includes_components() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let results = execute_search("auth", &issues, &metrics, SearchMode::Hybrid, &weights, 10);

        assert!(!results.is_empty());
        assert!(results[0].text_score.is_some());
        assert!(results[0].component_scores.is_some());
    }

    #[test]
    fn preset_weights_valid() {
        let presets = [
            "default",
            "bug-hunting",
            "sprint-planning",
            "impact-first",
            "text-only",
        ];
        for name in &presets {
            let w = get_preset(name);
            let sum = w.text + w.pagerank + w.status + w.impact + w.priority + w.recency;
            assert!((sum - 1.0).abs() < 0.001, "preset {name} sum = {sum}");
        }
    }

    #[test]
    fn custom_weights_parsing() {
        let json = r#"{"text":0.5,"pagerank":0.2,"status":0.1,"impact":0.1,"priority":0.05,"recency":0.05}"#;
        let weights = SearchWeights::from_json(json).unwrap();
        let sum = weights.text
            + weights.pagerank
            + weights.status
            + weights.impact
            + weights.priority
            + weights.recency;
        assert!((sum - 1.0).abs() < 0.001);
    }

    #[test]
    fn short_query_detection() {
        assert!(is_short_query("auth"));
        assert!(is_short_query("fix bug"));
        assert!(!is_short_query("authentication handling in the login flow"));
    }

    #[test]
    fn short_query_weight_adjustment() {
        let weights = SearchWeights::default_preset();
        let adjusted = adjust_weights_for_short_query(&weights);
        assert!(adjusted.text >= 0.55);
        let sum = adjusted.text
            + adjusted.pagerank
            + adjusted.status
            + adjusted.impact
            + adjusted.priority
            + adjusted.recency;
        assert!((sum - 1.0).abs() < 0.001);
    }

    #[test]
    fn deterministic_output() {
        let (issues, metrics) = make_issues_and_metrics();
        let weights = SearchWeights::default_preset();
        let r1 = execute_search("fix", &issues, &metrics, SearchMode::Text, &weights, 10);
        let r2 = execute_search("fix", &issues, &metrics, SearchMode::Text, &weights, 10);

        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.issue_id, b.issue_id);
            assert_eq!(a.score, b.score);
        }
    }
}
