use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

use crate::analysis::graph::GraphMetrics;
use crate::model::Issue;
use crate::robot::compute_data_hash;

const DEFAULT_MAX_SUGGESTIONS: usize = 50;
const DUPLICATE_JACCARD_THRESHOLD: f64 = 0.7;
const DUPLICATE_MIN_KEYWORDS: usize = 2;
const DUPLICATE_MAX_SUGGESTIONS: usize = 20;
const DEPENDENCY_MIN_KEYWORD_OVERLAP: usize = 2;
const DEPENDENCY_MIN_CONFIDENCE: f64 = 0.5;
const DEPENDENCY_MAX_SUGGESTIONS: usize = 20;
const LABEL_MIN_CONFIDENCE: f64 = 0.5;
const LABEL_MAX_PER_ISSUE: usize = 3;
const LABEL_MAX_TOTAL: usize = 30;
const CYCLE_MAX: usize = 10;
const HIGH_CONFIDENCE_THRESHOLD: f64 = 0.7;
const LOW_CONFIDENCE_THRESHOLD: f64 = 0.4;

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "are", "was", "were", "been", "have",
    "has", "had", "does", "did", "will", "would", "could", "should", "may", "might", "can", "not",
    "all", "any", "some", "each", "when", "where", "what", "which", "how", "why", "who", "its",
    "also", "just", "only", "more", "than", "then", "now", "here", "there", "these", "those",
    "such", "into", "over", "after", "before", "being", "other", "about", "like", "very", "most",
    "make", "use",
];

const BUILTIN_LABEL_MAPPINGS: &[(&str, &[&str])] = &[
    ("database", &["database", "db"]),
    ("migration", &["database", "migration"]),
    ("api", &["api"]),
    ("endpoint", &["api"]),
    ("rest", &["api"]),
    ("graphql", &["api", "graphql"]),
    ("auth", &["auth", "security"]),
    ("login", &["auth"]),
    ("password", &["auth", "security"]),
    ("security", &["security"]),
    ("test", &["testing"]),
    ("tests", &["testing"]),
    ("unittest", &["testing"]),
    ("integration", &["testing", "integration"]),
    ("ui", &["ui", "frontend"]),
    ("frontend", &["frontend"]),
    ("backend", &["backend"]),
    ("server", &["backend"]),
    ("cli", &["cli"]),
    ("command", &["cli"]),
    ("config", &["config"]),
    ("settings", &["config"]),
    ("performance", &["performance"]),
    ("slow", &["performance"]),
    ("fast", &["performance"]),
    ("memory", &["performance"]),
    ("cache", &["performance", "cache"]),
    ("docs", &["documentation"]),
    ("readme", &["documentation"]),
    ("refactor", &["refactoring"]),
    ("cleanup", &["refactoring", "maintenance"]),
    ("dependency", &["dependencies"]),
    ("deps", &["dependencies"]),
    ("bug", &["bug"]),
    ("fix", &["bug"]),
    ("broken", &["bug"]),
    ("crash", &["bug"]),
    ("error", &["bug"]),
    ("feature", &["feature"]),
    ("enhance", &["enhancement"]),
    ("improve", &["enhancement"]),
    ("urgent", &["urgent", "priority"]),
    ("hotfix", &["urgent", "bug"]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionType {
    MissingDependency,
    PotentialDuplicate,
    LabelSuggestion,
    CycleWarning,
}

impl SuggestionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingDependency => "missing_dependency",
            Self::PotentialDuplicate => "potential_duplicate",
            Self::LabelSuggestion => "label_suggestion",
            Self::CycleWarning => "cycle_warning",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionConfidenceLevel {
    Low,
    Medium,
    High,
}

impl SuggestionConfidenceLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SuggestOptions {
    pub min_confidence: f64,
    pub max_suggestions: usize,
    pub filter_type: Option<SuggestionType>,
    pub filter_bead: Option<String>,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self {
            min_confidence: 0.0,
            max_suggestions: DEFAULT_MAX_SUGGESTIONS,
            filter_type: None,
            filter_bead: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    #[serde(rename = "type")]
    pub suggestion_type: SuggestionType,
    pub target_bead: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_bead: Option<String>,
    pub summary: String,
    pub reason: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_command: Option<String>,
    pub generated_at: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionSet {
    pub suggestions: Vec<Suggestion>,
    pub generated_at: String,
    pub data_hash: String,
    pub stats: SuggestionStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionStats {
    pub total: usize,
    pub by_type: BTreeMap<String, usize>,
    pub by_confidence: BTreeMap<String, usize>,
    pub high_confidence_count: usize,
    pub actionable_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestFilter {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub filter_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bead_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RobotSuggestOutput {
    pub generated_at: String,
    pub data_hash: String,
    pub filters: SuggestFilter,
    pub suggestions: SuggestionSet,
    pub usage_hints: Vec<String>,
}

#[must_use]
pub fn generate_robot_suggest_output(
    issues: &[Issue],
    metrics: &GraphMetrics,
    options: &SuggestOptions,
) -> RobotSuggestOutput {
    let generated_at = Utc::now().to_rfc3339();
    let data_hash = compute_data_hash(issues);
    let mut suggestions = detect_potential_duplicates(issues, &generated_at);
    suggestions.extend(detect_missing_dependencies(issues, &generated_at));
    suggestions.extend(detect_label_suggestions(issues, &generated_at));
    suggestions.extend(detect_cycle_warnings(metrics, &generated_at));
    suggestions.retain(|suggestion| matches_filters(suggestion, options));
    sort_suggestions(&mut suggestions);
    if options.max_suggestions > 0 && suggestions.len() > options.max_suggestions {
        suggestions.truncate(options.max_suggestions);
    }

    let suggestion_set = SuggestionSet {
        stats: compute_stats(&suggestions),
        suggestions,
        generated_at: generated_at.clone(),
        data_hash: data_hash.clone(),
    };

    let active_bead_filter = options
        .filter_bead
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let filters = SuggestFilter {
        filter_type: options.filter_type.map(|value| value.as_str().to_string()),
        min_confidence: if options.min_confidence > 0.0 {
            Some(options.min_confidence)
        } else {
            None
        },
        bead_id: active_bead_filter,
    };

    RobotSuggestOutput {
        generated_at,
        data_hash,
        filters,
        suggestions: suggestion_set,
        usage_hints: vec![
            "jq '.suggestions.suggestions[:5]' - Top 5 suggestions by confidence".to_string(),
            "jq '.suggestions.suggestions[] | select(.type==\"potential_duplicate\")' - Filter duplicates".to_string(),
            "jq '.suggestions.suggestions[] | select(.confidence >= 0.8)' - High-confidence only".to_string(),
            "jq '.suggestions.stats.by_type' - Count by suggestion type".to_string(),
            "jq '.suggestions.suggestions[].action_command' - All action commands".to_string(),
            "--suggest-type=dependency - Filter to dependency suggestions".to_string(),
            "--suggest-confidence=0.7 - Minimum confidence threshold".to_string(),
            "--suggest-bead=<id> - Suggestions for specific bead".to_string(),
        ],
    }
}

fn detect_potential_duplicates(issues: &[Issue], generated_at: &str) -> Vec<Suggestion> {
    if issues.len() < 2 {
        return Vec::new();
    }

    let keyword_sets = issues
        .iter()
        .map(|issue| extract_keywords(&issue.title, &issue.description))
        .collect::<Vec<_>>();

    let mut suggestions = Vec::<Suggestion>::new();
    for left_index in 0..issues.len() {
        if keyword_sets[left_index].len() < DUPLICATE_MIN_KEYWORDS {
            continue;
        }

        for right_index in (left_index + 1)..issues.len() {
            if keyword_sets[right_index].len() < DUPLICATE_MIN_KEYWORDS {
                continue;
            }

            let left_issue = &issues[left_index];
            let right_issue = &issues[right_index];
            if left_issue.normalized_status() == "tombstone"
                || right_issue.normalized_status() == "tombstone"
            {
                continue;
            }

            if left_issue.is_closed_like() != right_issue.is_closed_like() {
                continue;
            }

            let common = intersect_keywords(&keyword_sets[left_index], &keyword_sets[right_index]);
            let union_count = keyword_sets[left_index]
                .len()
                .saturating_add(keyword_sets[right_index].len())
                .saturating_sub(common.len());
            if union_count == 0 {
                continue;
            }

            let similarity = ratio(common.len(), union_count);
            if similarity < DUPLICATE_JACCARD_THRESHOLD {
                continue;
            }

            let mut suggestion = base_suggestion(
                generated_at,
                SuggestionType::PotentialDuplicate,
                left_issue.id.clone(),
                format!("Potential duplicate of {}", right_issue.id),
                format!(
                    "{:.0}% keyword similarity; common: {}",
                    similarity * 100.0,
                    common
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                similarity,
            );
            suggestion.related_bead = Some(right_issue.id.clone());
            if left_issue.is_open_like() && right_issue.is_open_like() {
                suggestion.action_command = Some(format!(
                    "br dep add {} {} --type=related",
                    left_issue.id, right_issue.id
                ));
            }
            suggestion
                .metadata
                .insert("method".to_string(), json!("jaccard"));
            suggestion
                .metadata
                .insert("common_keywords".to_string(), json!(common));
            suggestions.push(suggestion);
        }
    }

    sort_suggestions(&mut suggestions);
    if suggestions.len() > DUPLICATE_MAX_SUGGESTIONS {
        suggestions.truncate(DUPLICATE_MAX_SUGGESTIONS);
    }
    suggestions
}

fn detect_missing_dependencies(issues: &[Issue], generated_at: &str) -> Vec<Suggestion> {
    if issues.len() < 2 {
        return Vec::new();
    }

    let keyword_sets = issues
        .iter()
        .map(|issue| extract_keywords(&issue.title, &issue.description))
        .collect::<Vec<_>>();

    let label_sets = issues
        .iter()
        .map(|issue| {
            issue
                .labels
                .iter()
                .map(|value| value.to_ascii_lowercase())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    let mut suggestions = Vec::<Suggestion>::new();
    for left_index in 0..issues.len() {
        for right_index in (left_index + 1)..issues.len() {
            let left_issue = &issues[left_index];
            let right_issue = &issues[right_index];
            if left_issue.is_closed_like() || right_issue.is_closed_like() {
                continue;
            }
            if has_dependency_between(left_issue, right_issue) {
                continue;
            }

            let shared_keywords =
                intersect_keywords(&keyword_sets[left_index], &keyword_sets[right_index]);
            if shared_keywords.len() < DEPENDENCY_MIN_KEYWORD_OVERLAP {
                continue;
            }

            let shared_labels = label_sets[left_index]
                .intersection(&label_sets[right_index])
                .cloned()
                .collect::<Vec<_>>();

            let mut confidence = (usize_to_f64(shared_keywords.len()) * 0.1).min(0.5);
            if mentions_issue_id(left_issue, right_issue)
                || mentions_issue_id(right_issue, left_issue)
            {
                confidence += 0.3;
            }
            if title_keyword_overlap(right_issue, &keyword_sets[left_index]) {
                confidence += 0.15;
            }
            confidence += usize_to_f64(shared_labels.len()) * 0.1;
            confidence = confidence.min(0.95);
            if confidence < DEPENDENCY_MIN_CONFIDENCE {
                continue;
            }

            let (from_issue, to_issue) = dependency_direction(left_issue, right_issue);
            let mut suggestion = base_suggestion(
                generated_at,
                SuggestionType::MissingDependency,
                from_issue.id.clone(),
                format!("May depend on {}", to_issue.id),
                format!(
                    "{} shared keywords{}",
                    shared_keywords.len(),
                    if shared_labels.is_empty() {
                        String::new()
                    } else {
                        format!(", {} shared labels", shared_labels.len())
                    }
                ),
                confidence,
            );
            suggestion.related_bead = Some(to_issue.id.clone());
            suggestion.action_command =
                Some(format!("br dep add {} {}", from_issue.id, to_issue.id));
            suggestion
                .metadata
                .insert("shared_keywords".to_string(), json!(shared_keywords));
            if !shared_labels.is_empty() {
                suggestion
                    .metadata
                    .insert("shared_labels".to_string(), json!(shared_labels));
            }
            suggestions.push(suggestion);
        }
    }

    sort_suggestions(&mut suggestions);
    if suggestions.len() > DEPENDENCY_MAX_SUGGESTIONS {
        suggestions.truncate(DEPENDENCY_MAX_SUGGESTIONS);
    }
    suggestions
}

fn detect_label_suggestions(issues: &[Issue], generated_at: &str) -> Vec<Suggestion> {
    if issues.is_empty() {
        return Vec::new();
    }

    let all_labels = issues
        .iter()
        .flat_map(|issue| issue.labels.iter().map(|label| label.to_ascii_lowercase()))
        .collect::<BTreeSet<_>>();
    if all_labels.is_empty() {
        return Vec::new();
    }

    let learned_mappings = learn_label_mappings(issues);
    let mut matches = Vec::<Suggestion>::new();

    for issue in issues {
        if issue.is_closed_like() {
            continue;
        }

        let existing_labels = issue
            .labels
            .iter()
            .map(|label| label.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let keywords = extract_keywords(&issue.title, &issue.description);

        let mut label_scores = BTreeMap::<String, f64>::new();
        let mut label_reasons = BTreeMap::<String, BTreeSet<String>>::new();

        for keyword in &keywords {
            for &(mapping_keyword, labels) in BUILTIN_LABEL_MAPPINGS {
                if mapping_keyword != keyword {
                    continue;
                }

                for &label in labels {
                    let candidate = label.to_string();
                    if existing_labels.contains(&candidate) || !all_labels.contains(&candidate) {
                        continue;
                    }
                    *label_scores.entry(candidate.clone()).or_insert(0.0) += 0.3;
                    label_reasons
                        .entry(candidate)
                        .or_default()
                        .insert(keyword.clone());
                }
            }

            if let Some(label_counts) = learned_mappings.get(keyword) {
                for (label, count) in label_counts {
                    if existing_labels.contains(label) || !all_labels.contains(label) {
                        continue;
                    }
                    let learned_bonus = (0.1 + (usize_to_f64(*count) * 0.05)).min(0.4);
                    *label_scores.entry(label.clone()).or_insert(0.0) += learned_bonus;
                    label_reasons
                        .entry(label.clone())
                        .or_default()
                        .insert(keyword.clone());
                }
            }
        }

        let mut candidates = label_scores.into_iter().collect::<Vec<(String, f64)>>();
        candidates.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });

        for (index, (label, score)) in candidates.into_iter().enumerate() {
            if index >= LABEL_MAX_PER_ISSUE || score < LABEL_MIN_CONFIDENCE {
                continue;
            }

            let matched_keywords = label_reasons
                .get(&label)
                .map(|values| values.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let reason = format!("keywords: {}", matched_keywords.join(", "));

            let mut suggestion = base_suggestion(
                generated_at,
                SuggestionType::LabelSuggestion,
                issue.id.clone(),
                format!("Consider adding label '{label}'"),
                reason,
                score.min(0.95),
            );
            suggestion.action_command = Some(format!("br update {} --add-label={label}", issue.id));
            suggestion
                .metadata
                .insert("suggested_label".to_string(), json!(label));
            suggestion
                .metadata
                .insert("matched_keywords".to_string(), json!(matched_keywords));
            matches.push(suggestion);
        }
    }

    sort_suggestions(&mut matches);
    if matches.len() > LABEL_MAX_TOTAL {
        matches.truncate(LABEL_MAX_TOTAL);
    }
    matches
}

fn detect_cycle_warnings(metrics: &GraphMetrics, generated_at: &str) -> Vec<Suggestion> {
    let mut suggestions = Vec::<Suggestion>::new();

    for cycle in metrics.cycles.iter().take(CYCLE_MAX) {
        if cycle.is_empty() {
            continue;
        }

        let cycle_length = cycle.len();
        let distance_from_shortest = cycle_length.saturating_sub(2);
        let confidence = (1.0 - (usize_to_f64(distance_from_shortest) * 0.1)).clamp(0.5, 1.0);

        let summary = if cycle_length == 1 {
            format!("Self-loop: {} depends on itself", cycle[0])
        } else if cycle_length == 2 {
            format!("Direct cycle between {} and {}", cycle[0], cycle[1])
        } else {
            format!("Dependency cycle of {cycle_length} issues")
        };

        let mut cycle_path = cycle.clone();
        if cycle_length > 1 {
            cycle_path.push(cycle[0].clone());
        }

        let mut suggestion = base_suggestion(
            generated_at,
            SuggestionType::CycleWarning,
            cycle[0].clone(),
            summary,
            format!("Cycle path: {}", cycle_path.join(" -> ")),
            confidence,
        );
        if cycle_length >= 2 {
            let last = cycle[cycle_length - 1].clone();
            let first = cycle[0].clone();
            suggestion.action_command = Some(format!("br dep remove {last} {first}"));
            suggestion.related_bead = Some(cycle[1].clone());
        }
        suggestion
            .metadata
            .insert("cycle_length".to_string(), json!(cycle_length));
        suggestion
            .metadata
            .insert("cycle_path".to_string(), json!(cycle));
        suggestions.push(suggestion);
    }

    suggestions
}

fn base_suggestion(
    generated_at: &str,
    suggestion_type: SuggestionType,
    target_bead: String,
    summary: String,
    reason: String,
    confidence: f64,
) -> Suggestion {
    Suggestion {
        suggestion_type,
        target_bead,
        related_bead: None,
        summary,
        reason,
        confidence,
        action_command: None,
        generated_at: generated_at.to_string(),
        metadata: BTreeMap::new(),
    }
}

fn dependency_direction<'a>(left: &'a Issue, right: &'a Issue) -> (&'a Issue, &'a Issue) {
    let left_created = parse_timestamp(left.created_at.as_deref());
    let right_created = parse_timestamp(right.created_at.as_deref());

    let priority_cmp = left.priority.cmp(&right.priority);
    let time_cmp = match (left_created, right_created) {
        (Some(l), Some(r)) => l.cmp(&r),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };

    let left_is_blocker = match priority_cmp {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => match time_cmp {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => left.id < right.id,
        },
    };

    if left_is_blocker {
        (right, left)
    } else {
        (left, right)
    }
}

fn learn_label_mappings(issues: &[Issue]) -> BTreeMap<String, BTreeMap<String, usize>> {
    let mut mappings = BTreeMap::<String, BTreeMap<String, usize>>::new();

    for issue in issues {
        if issue.labels.is_empty() {
            continue;
        }

        let keywords = extract_keywords(&issue.title, &issue.description);
        for keyword in keywords {
            let label_counts = mappings.entry(keyword).or_default();
            for label in &issue.labels {
                let label = label.to_ascii_lowercase();
                *label_counts.entry(label).or_insert(0) += 1;
            }
        }
    }

    mappings
}

fn has_dependency_between(left: &Issue, right: &Issue) -> bool {
    left.dependencies
        .iter()
        .any(|dep| dep.depends_on_id == right.id)
        || right
            .dependencies
            .iter()
            .any(|dep| dep.depends_on_id == left.id)
}

fn mentions_issue_id(primary: &Issue, other: &Issue) -> bool {
    let primary_text = primary.description.to_ascii_lowercase();
    let other_id = other.id.to_ascii_lowercase();
    !other_id.is_empty() && primary_text.contains(&other_id)
}

fn title_keyword_overlap(other: &Issue, primary_keywords: &[String]) -> bool {
    let other_title = other.title.to_ascii_lowercase();
    primary_keywords
        .iter()
        .any(|keyword| keyword.len() >= 5 && other_title.contains(keyword))
}

fn matches_filters(suggestion: &Suggestion, options: &SuggestOptions) -> bool {
    if options.min_confidence > 0.0 && suggestion.confidence < options.min_confidence {
        return false;
    }

    if options
        .filter_type
        .is_some_and(|filter_type| suggestion.suggestion_type != filter_type)
    {
        return false;
    }

    let bead_filter = options
        .filter_bead
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let Some(bead_id) = bead_filter
        && suggestion.target_bead != bead_id
        && suggestion.related_bead.as_deref() != Some(bead_id)
    {
        return false;
    }

    true
}

fn sort_suggestions(suggestions: &mut [Suggestion]) {
    suggestions.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| {
                left.suggestion_type
                    .as_str()
                    .cmp(right.suggestion_type.as_str())
            })
            .then_with(|| left.target_bead.cmp(&right.target_bead))
            .then_with(|| left.related_bead.cmp(&right.related_bead))
    });
}

fn compute_stats(suggestions: &[Suggestion]) -> SuggestionStats {
    let mut by_type = BTreeMap::<String, usize>::new();
    let mut by_confidence = BTreeMap::<String, usize>::new();
    let mut high_confidence_count = 0usize;
    let mut actionable_count = 0usize;

    for suggestion in suggestions {
        *by_type
            .entry(suggestion.suggestion_type.as_str().to_string())
            .or_insert(0) += 1;

        let level = confidence_level(suggestion.confidence);
        *by_confidence.entry(level.as_str().to_string()).or_insert(0) += 1;
        if suggestion.confidence >= HIGH_CONFIDENCE_THRESHOLD {
            high_confidence_count += 1;
        }
        if suggestion.action_command.is_some() {
            actionable_count += 1;
        }
    }

    SuggestionStats {
        total: suggestions.len(),
        by_type,
        by_confidence,
        high_confidence_count,
        actionable_count,
    }
}

fn confidence_level(confidence: f64) -> SuggestionConfidenceLevel {
    if confidence < LOW_CONFIDENCE_THRESHOLD {
        return SuggestionConfidenceLevel::Low;
    }
    if confidence >= HIGH_CONFIDENCE_THRESHOLD {
        return SuggestionConfidenceLevel::High;
    }
    SuggestionConfidenceLevel::Medium
}

fn extract_keywords(title: &str, description: &str) -> Vec<String> {
    let normalized = normalize_text(&format!("{title} {description}"));
    let mut keywords = BTreeSet::<String>::new();

    for word in normalized.split_whitespace() {
        if word.len() < 3 || STOP_WORDS.contains(&word) {
            continue;
        }
        keywords.insert(word.to_string());
    }

    keywords.into_iter().collect()
}

fn normalize_text(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn intersect_keywords(left: &[String], right: &[String]) -> Vec<String> {
    let right_set = right.iter().cloned().collect::<BTreeSet<_>>();
    left.iter()
        .filter(|word| right_set.contains(*word))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    usize_to_f64(numerator) / usize_to_f64(denominator)
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn parse_timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_suggestion(
        suggestion_type: SuggestionType,
        confidence: f64,
        target: &str,
    ) -> Suggestion {
        Suggestion {
            suggestion_type,
            target_bead: target.to_string(),
            related_bead: None,
            summary: String::new(),
            reason: String::new(),
            confidence,
            action_command: None,
            generated_at: String::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn sort_by_confidence_descending() {
        let mut suggestions = vec![
            make_suggestion(SuggestionType::MissingDependency, 0.5, "bd-a"),
            make_suggestion(SuggestionType::MissingDependency, 0.9, "bd-b"),
            make_suggestion(SuggestionType::MissingDependency, 0.7, "bd-c"),
        ];
        sort_suggestions(&mut suggestions);
        assert_eq!(suggestions[0].target_bead, "bd-b"); // 0.9
        assert_eq!(suggestions[1].target_bead, "bd-c"); // 0.7
        assert_eq!(suggestions[2].target_bead, "bd-a"); // 0.5
    }

    #[test]
    fn sort_tiebreak_by_type_alphabetical() {
        // All same confidence — should sort alphabetically by type string
        let mut suggestions = vec![
            make_suggestion(SuggestionType::PotentialDuplicate, 0.8, "bd-a"), // "potential_duplicate"
            make_suggestion(SuggestionType::CycleWarning, 0.8, "bd-b"),       // "cycle_warning"
            make_suggestion(SuggestionType::MissingDependency, 0.8, "bd-c"), // "missing_dependency"
            make_suggestion(SuggestionType::LabelSuggestion, 0.8, "bd-d"),   // "label_suggestion"
        ];
        sort_suggestions(&mut suggestions);
        // Alphabetical: cycle_warning < label_suggestion < missing_dependency < potential_duplicate
        assert_eq!(suggestions[0].suggestion_type.as_str(), "cycle_warning");
        assert_eq!(suggestions[1].suggestion_type.as_str(), "label_suggestion");
        assert_eq!(
            suggestions[2].suggestion_type.as_str(),
            "missing_dependency"
        );
        assert_eq!(
            suggestions[3].suggestion_type.as_str(),
            "potential_duplicate"
        );
    }

    #[test]
    fn sort_tiebreak_by_target_bead() {
        // Same confidence and type — should sort by target_bead
        let mut suggestions = vec![
            make_suggestion(SuggestionType::CycleWarning, 0.8, "bd-z"),
            make_suggestion(SuggestionType::CycleWarning, 0.8, "bd-a"),
            make_suggestion(SuggestionType::CycleWarning, 0.8, "bd-m"),
        ];
        sort_suggestions(&mut suggestions);
        assert_eq!(suggestions[0].target_bead, "bd-a");
        assert_eq!(suggestions[1].target_bead, "bd-m");
        assert_eq!(suggestions[2].target_bead, "bd-z");
    }
}
