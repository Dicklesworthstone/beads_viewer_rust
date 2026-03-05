//! Priority and agent brief generation.
//!
//! Produces markdown briefs from triage results for human or agent consumption.

use std::fmt::Write;
use std::path::Path;

use serde::Serialize;

use super::triage::TriageResult;
use crate::Result;
use crate::model::Issue;

// ---------------------------------------------------------------------------
// Priority brief
// ---------------------------------------------------------------------------

/// Generate a priority brief as markdown.
pub fn generate_priority_brief(
    issues: &[Issue],
    triage: &TriageResult,
    data_hash: &str,
    generated_at: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Priority Brief\n");
    let _ = writeln!(out, "Generated: {generated_at}  ");
    let _ = writeln!(out, "Data hash: `{data_hash}`  ");

    let open = issues.iter().filter(|i| i.is_open_like()).count();
    let closed = issues.iter().filter(|i| !i.is_open_like()).count();
    let total = issues.len();
    let _ = writeln!(out, "Issues: {total} total, {open} open, {closed} closed\n");

    // Top recommendations
    let _ = writeln!(out, "## Top Recommendations\n");
    let recs = &triage.recommendations;
    if recs.is_empty() {
        let _ = writeln!(out, "_No recommendations available._\n");
    } else {
        let _ = writeln!(out, "| # | ID | Title | Score | Priority | Status |");
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for (i, rec) in recs.iter().take(10).enumerate() {
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {:.3} | P{} | {} |",
                i + 1,
                rec.id,
                truncate_str(&rec.title, 50),
                rec.score,
                rec.priority,
                rec.status,
            );
        }
        let _ = writeln!(out);
    }

    // Quick wins
    let _ = writeln!(out, "## Quick Wins\n");
    let wins = &triage.quick_ref.top_picks;
    if wins.is_empty() {
        let _ = writeln!(out, "_No quick wins identified._\n");
    } else {
        for pick in wins.iter().take(5) {
            let _ = writeln!(
                out,
                "- **{}**: {} (unblocks {})",
                pick.id, pick.title, pick.unblocks
            );
        }
        let _ = writeln!(out);
    }

    // Blockers
    let _ = writeln!(out, "## Blockers to Clear\n");
    let blockers = &triage.blockers_to_clear;
    if blockers.is_empty() {
        let _ = writeln!(out, "_No blockers identified._\n");
    } else {
        for b in blockers.iter().take(5) {
            let _ = writeln!(out, "- **{}**: {} (unblocks {})", b.id, b.title, b.unblocks);
        }
        let _ = writeln!(out);
    }

    // Commands
    let _ = writeln!(out, "## Commands\n");
    let _ = writeln!(out, "```bash");
    let _ = writeln!(out, "# Claim the top pick:");
    if let Some(top) = recs.first() {
        let _ = writeln!(out, "br update {} --status=in_progress", top.id);
    }
    let _ = writeln!(out, "\n# Refresh triage:");
    let _ = writeln!(out, "bvr --robot-triage");
    let _ = writeln!(out, "```\n");

    out
}

// ---------------------------------------------------------------------------
// Agent brief bundle
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct BriefMeta {
    generated_at: String,
    data_hash: String,
    issue_count: usize,
    version: String,
    files: Vec<String>,
}

/// Generate an agent brief bundle in the specified directory.
pub fn generate_agent_brief(
    issues: &[Issue],
    triage: &TriageResult,
    insights_json: &serde_json::Value,
    data_hash: &str,
    generated_at: &str,
    output_dir: &Path,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(output_dir)?;

    let files = vec![
        "triage.json".to_string(),
        "insights.json".to_string(),
        "brief.md".to_string(),
        "helpers.md".to_string(),
        "meta.json".to_string(),
    ];

    // triage.json
    let triage_json = serde_json::to_string_pretty(triage)?;
    std::fs::write(output_dir.join("triage.json"), &triage_json)?;

    // insights.json
    let insights_str = serde_json::to_string_pretty(insights_json)?;
    std::fs::write(output_dir.join("insights.json"), &insights_str)?;

    // brief.md
    let brief = generate_priority_brief(issues, triage, data_hash, generated_at);
    std::fs::write(output_dir.join("brief.md"), &brief)?;

    // helpers.md
    let helpers = generate_helpers_md();
    std::fs::write(output_dir.join("helpers.md"), &helpers)?;

    // meta.json
    let meta = BriefMeta {
        generated_at: generated_at.to_string(),
        data_hash: data_hash.to_string(),
        issue_count: issues.len(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
        files: files.clone(),
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    std::fs::write(output_dir.join("meta.json"), &meta_json)?;

    Ok(files)
}

fn generate_helpers_md() -> String {
    r#"# Agent Brief: jq Quick Reference

## Triage Data (triage.json)

```bash
# Top 3 recommendations
jq '.recommendations[:3] | map({id, title, score})' triage.json

# All quick wins
jq '.quick_ref.top_picks' triage.json

# Blockers to clear
jq '.blockers_to_clear | map({id, title, unblocks})' triage.json

# Claim top pick
jq -r '.recommendations[0] | "br update \(.id) --status=in_progress"' triage.json
```

## Insights Data (insights.json)

```bash
# Top bottlenecks
jq '.bottlenecks[:5] | map({id, title, score})' insights.json

# Critical path
jq '.critical_path' insights.json

# Cycle detection
jq '.cycles' insights.json

# Top PageRank influencers
jq '.influencers[:5]' insights.json
```

## Combined Queries

```bash
# High-impact actionable items
jq '[.recommendations[] | select(.score > 0.2)] | map({id, title, score})' triage.json

# Blocked items with blockers
jq '.blockers_to_clear[] | select(.unblocks > 1)' triage.json
```
"#
    .to_string()
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::triage::{BlockerToClear, QuickPick, QuickRef, Recommendation};

    fn make_triage_result() -> TriageResult {
        TriageResult {
            quick_ref: QuickRef {
                total_open: 5,
                total_actionable: 3,
                top_picks: vec![QuickPick {
                    id: "A-1".to_string(),
                    title: "Fix auth".to_string(),
                    score: 0.9,
                    reasons: vec!["high priority".to_string()],
                    unblocks: 2,
                }],
            },
            recommendations: vec![Recommendation {
                id: "A-1".to_string(),
                title: "Fix auth".to_string(),
                score: 0.9,
                confidence: 0.8,
                reasons: vec!["high priority".to_string()],
                unblocks: 2,
                status: "open".to_string(),
                priority: 1,
                issue_type: "task".to_string(),
                labels: Vec::new(),
                assignee: String::new(),
                claim_command: "br update A-1 --status=in_progress".to_string(),
                show_command: "br show A-1".to_string(),
            }],
            quick_wins: Vec::new(),
            blockers_to_clear: vec![BlockerToClear {
                id: "B-1".to_string(),
                title: "DB migration".to_string(),
                status: "open".to_string(),
                unblocks: 3,
            }],
            recommendations_by_track: Vec::new(),
            recommendations_by_label: Vec::new(),
        }
    }

    #[test]
    fn priority_brief_contains_header_and_hash() {
        let triage = make_triage_result();
        let brief = generate_priority_brief(&[], &triage, "abc123", "2025-01-01T00:00:00Z");

        assert!(brief.contains("# Priority Brief"));
        assert!(brief.contains("abc123"));
        assert!(brief.contains("Fix auth"));
        assert!(brief.contains("## Top Recommendations"));
        assert!(brief.contains("## Blockers to Clear"));
    }

    #[test]
    fn priority_brief_includes_recommendations_table() {
        let triage = make_triage_result();
        let brief = generate_priority_brief(&[], &triage, "hash", "now");

        assert!(brief.contains("| 1 |"));
        assert!(brief.contains("`A-1`"));
        assert!(brief.contains("0.900"));
    }

    #[test]
    fn agent_brief_creates_all_files() {
        let triage = make_triage_result();
        let insights = serde_json::json!({"bottlenecks": [], "critical_path": []});
        let tmp = tempfile::tempdir().unwrap();

        let files =
            generate_agent_brief(&[], &triage, &insights, "hash", "now", tmp.path()).unwrap();

        assert_eq!(files.len(), 5);
        for f in &files {
            assert!(tmp.path().join(f).exists(), "missing file: {f}");
        }

        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("meta.json")).unwrap())
                .unwrap();
        assert_eq!(meta["data_hash"], "hash");
        assert_eq!(meta["files"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn helpers_md_contains_jq_snippets() {
        let helpers = generate_helpers_md();
        assert!(helpers.contains("jq"));
        assert!(helpers.contains("triage.json"));
        assert!(helpers.contains("insights.json"));
    }

    #[test]
    fn truncate_str_works() {
        assert_eq!(truncate_str("short", 10), "short");
        assert_eq!(truncate_str("this is a long string", 10), "this is...");
    }
}
