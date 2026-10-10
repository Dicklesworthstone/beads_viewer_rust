use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use toon::EncodeOptions;
use toon::options::KeyFoldingMode;

use crate::Result;
use crate::cli::OutputFormat;
use crate::model::Issue;

#[derive(Debug, Clone, Serialize)]
pub struct RobotEnvelope {
    pub generated_at: String,
    pub data_hash: String,
    pub output_format: String,
    pub version: String,
}

#[must_use]
pub fn envelope(issues: &[Issue]) -> RobotEnvelope {
    RobotEnvelope {
        generated_at: Utc::now().to_rfc3339(),
        data_hash: compute_data_hash(issues),
        output_format: "json".to_string(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
    }
}

/// Create an envelope without issues (for commands that don't load issues).
#[must_use]
pub fn envelope_empty() -> RobotEnvelope {
    RobotEnvelope {
        generated_at: Utc::now().to_rfc3339(),
        data_hash: String::new(),
        output_format: "json".to_string(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
    }
}

#[must_use]
pub fn compute_data_hash(issues: &[Issue]) -> String {
    let mut stable = issues
        .iter()
        .map(|issue| {
            (
                issue.id.clone(),
                issue.status.clone(),
                issue.priority,
                issue
                    .updated_at
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();

    stable.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    for row in stable {
        hasher.update(row.0);
        hasher.update(b"\x1f");
        hasher.update(row.1);
        hasher.update(b"\x1f");
        hasher.update(row.2.to_string());
        hasher.update(b"\x1f");
        hasher.update(row.3);
        hasher.update("\n");
    }

    let digest = hasher.finalize();
    format!("{digest:x}")[..16].to_string()
}

/// Fingerprint every issue field for in-process change detection, including
/// workspace routing fields omitted from serialized output. Issue order is
/// ignored; ordering within labels, comments, and dependencies remains meaningful.
///
/// This is separate from the robot data hash and is not a stable wire format.
#[must_use]
pub fn compute_snapshot_hash(issues: &[Issue]) -> [u8; 32] {
    let mut stable = issues.iter().collect::<Vec<_>>();
    stable.sort_by(|left, right| left.id.cmp(&right.id));

    let mut hasher = SnapshotHasher(Sha256::new());
    stable.hash(&mut hasher);
    hasher.0.finalize().into()
}

struct SnapshotHasher(Sha256);

impl Hasher for SnapshotHasher {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        let mut prefix = [0; 8];
        prefix.copy_from_slice(&digest[..8]);
        u64::from_le_bytes(prefix)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

pub fn emit<T: Serialize>(format: OutputFormat, payload: &T) -> Result<()> {
    let rendered = render_payload(format, payload)?;
    print_output(&rendered.output);
    Ok(())
}

pub fn emit_with_stats<T: Serialize>(
    format: OutputFormat,
    payload: &T,
    show_stats: bool,
) -> Result<()> {
    let rendered = render_payload(format, payload)?;
    print_output(&rendered.output);
    if show_stats {
        print_format_stats(&rendered.json_for_stats, rendered.toon_for_stats.as_deref());
    }
    Ok(())
}

struct RenderedPayload {
    output: String,
    json_for_stats: String,
    toon_for_stats: Option<String>,
}

// ---------------------------------------------------------------------------
// TOON encoding (direct in-process library integration)
// ---------------------------------------------------------------------------

/// Options for TOON encoding, resolved from environment variables.
struct ToonEncodeOptions {
    key_folding: Option<KeyFoldingMode>,
    indent: Option<usize>,
}

fn resolve_toon_encode_options() -> ToonEncodeOptions {
    let key_folding = std::env::var("TOON_KEY_FOLDING")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(parse_toon_key_folding_mode);

    let indent = std::env::var("TOON_INDENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .and_then(|raw| {
            raw.parse::<usize>().map_or_else(
                |_| {
                    eprintln!(
                        "warning: ignoring invalid TOON_INDENT value {raw:?}; expected integer 0-16"
                    );
                    None
                },
                |indent| Some(indent.min(16)),
            )
        });

    ToonEncodeOptions {
        key_folding,
        indent,
    }
}

fn parse_toon_key_folding_mode(raw: &str) -> Option<KeyFoldingMode> {
    match raw {
        "off" => None,
        "safe" => Some(KeyFoldingMode::Safe),
        invalid => {
            eprintln!(
                "warning: ignoring invalid TOON_KEY_FOLDING value {invalid:?}; expected off|safe"
            );
            None
        }
    }
}

fn render_payload<T: Serialize>(format: OutputFormat, payload: &T) -> Result<RenderedPayload> {
    let mut value = serde_json::to_value(payload)?;

    match format {
        OutputFormat::Json => {
            set_top_level_output_format(&mut value, OutputFormat::Json);
            let json_for_stats = serde_json::to_string(&value)?;
            let output = encode_json(&value)?;
            Ok(RenderedPayload {
                output,
                json_for_stats,
                toon_for_stats: None,
            })
        }
        OutputFormat::Toon => {
            set_top_level_output_format(&mut value, OutputFormat::Toon);
            let json_for_stats = serde_json::to_string(&value)?;
            let output = encode_toon(&value);
            Ok(RenderedPayload {
                toon_for_stats: Some(output.clone()),
                output,
                json_for_stats,
            })
        }
    }
}

fn encode_json(value: &Value) -> Result<String> {
    if std::env::var("BV_PRETTY_JSON").is_ok_and(|value| value.trim() == "1") {
        Ok(serde_json::to_string_pretty(value)?)
    } else {
        Ok(serde_json::to_string(value)?)
    }
}

fn set_top_level_output_format(value: &mut Value, format: OutputFormat) {
    if let Some(object) = value.as_object_mut()
        && object.contains_key("output_format")
    {
        let label = match format {
            OutputFormat::Json => "json",
            OutputFormat::Toon => "toon",
        };
        object.insert(
            "output_format".to_string(),
            Value::String(label.to_string()),
        );
    }
}

fn print_output(output: &str) {
    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
}

fn encode_toon(value: &Value) -> String {
    let opts = resolve_toon_encode_options();
    let options = Some(EncodeOptions {
        indent: opts.indent,
        delimiter: None,
        key_folding: opts.key_folding,
        flatten_depth: None,
        replacer: None,
    });
    let mut toon_str = toon::encode(value.clone(), options);
    // Normalize: trim trailing whitespace, add single newline
    let trimmed_len = toon_str.trim_end().len();
    toon_str.truncate(trimmed_len);
    toon_str.push('\n');
    toon_str
}

#[must_use]
pub fn default_field_descriptions() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("score", "Composite impact score (0..1)"),
        ("impact_score", "Legacy alias of score for compatibility"),
        (
            "confidence",
            "Heuristic confidence for recommendation quality (0..1)",
        ),
        (
            "unblocks",
            "Count of downstream issues immediately unblocked",
        ),
        (
            "claim_command",
            "Suggested br command to claim/start the issue",
        ),
    ])
}

// ---------------------------------------------------------------------------
// --robot-docs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CmdDoc {
    flag: &'static str,
    description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    key_fields: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    params: Vec<&'static str>,
    needs_issues: bool,
}

#[cfg(test)]
fn implemented_robot_command_names() -> &'static [&'static str] {
    &[
        "robot-triage",
        "robot-next",
        "robot-overview",
        "robot-plan",
        "robot-insights",
        "robot-priority",
        "robot-triage-by-track",
        "robot-triage-by-label",
        "robot-alerts",
        "robot-suggest",
        "robot-schema",
        "robot-docs",
        "robot-history",
        "robot-diff",
        "robot-graph",
        "robot-forecast",
        "robot-capacity",
        "robot-burndown",
        "robot-sprint-list",
        "robot-sprint-show",
        "robot-metrics",
        "robot-label-health",
        "robot-label-flow",
        "robot-label-attention",
        "robot-explain-correlation",
        "robot-confirm-correlation",
        "robot-reject-correlation",
        "robot-correlation-stats",
        "robot-orphans",
        "robot-file-beads",
        "robot-file-hotspots",
        "robot-impact",
        "robot-file-relations",
        "robot-related",
        "robot-blocker-chain",
        "robot-impact-network",
        "robot-causality",
        "robot-drift",
        "robot-search",
        "robot-recipes",
        "robot-economics",
        "robot-delivery",
    ]
}

fn robot_command_docs() -> BTreeMap<&'static str, CmdDoc> {
    BTreeMap::from([
        (
            "robot-triage",
            CmdDoc {
                flag: "--robot-triage",
                description: "Unified triage: top picks, recommendations, quick wins, blockers, project health, velocity.",
                key_fields: vec![
                    "triage.quick_ref.top_picks",
                    "triage.recommendations",
                    "triage.quick_wins",
                    "triage.blockers_to_clear",
                    "triage.project_health",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-next",
            CmdDoc {
                flag: "--robot-next",
                description: "Single top recommendation with claim/show commands.",
                key_fields: vec![
                    "id",
                    "title",
                    "score",
                    "reasons",
                    "unblocks",
                    "claim_command",
                    "show_command",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-overview",
            CmdDoc {
                flag: "--robot-overview",
                description: "Compact project orientation surface: counts, next action, blocker signal, diverse work fronts, and quick commands.",
                key_fields: vec![
                    "summary",
                    "top_pick",
                    "top_blocker",
                    "top_labels",
                    "fronts",
                    "commands",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-plan",
            CmdDoc {
                flag: "--robot-plan",
                description: "Dependency-respecting execution plan with parallel tracks.",
                key_fields: vec!["tracks", "items", "unblocks", "summary"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-insights",
            CmdDoc {
                flag: "--robot-insights",
                description: "Deep graph analysis: PageRank, betweenness, HITS, eigenvector, k-core, cycle detection.",
                key_fields: vec![
                    "pagerank",
                    "betweenness",
                    "hits",
                    "eigenvector",
                    "k_core",
                    "cycles",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-priority",
            CmdDoc {
                flag: "--robot-priority",
                description: "Priority misalignment detection: items whose graph importance differs from assigned priority.",
                key_fields: vec!["misalignments", "suggestions"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-triage-by-track",
            CmdDoc {
                flag: "--robot-triage-by-track",
                description: "Triage grouped by independent parallel execution tracks.",
                key_fields: vec!["tracks[].track_id", "tracks[].top_pick", "tracks[].items"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-triage-by-label",
            CmdDoc {
                flag: "--robot-triage-by-label",
                description: "Triage grouped by label for area-focused agents.",
                key_fields: vec!["labels[].label", "labels[].top_pick", "labels[].items"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-alerts",
            CmdDoc {
                flag: "--robot-alerts",
                description: "Stale issues, blocking cascades, priority mismatches.",
                key_fields: vec!["alerts", "severity", "affected_issues"],
                params: vec![
                    "--severity info|warning|critical",
                    "--alert-type <type>",
                    "--alert-label <label>",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-suggest",
            CmdDoc {
                flag: "--robot-suggest",
                description: "Smart suggestions: potential duplicates, missing dependencies, label assignments, cycle warnings.",
                key_fields: vec!["suggestions", "type", "confidence"],
                params: vec![
                    "--suggest-type duplicate|dependency|label|cycle",
                    "--suggest-confidence 0.0-1.0",
                    "--suggest-bead <id>",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-schema",
            CmdDoc {
                flag: "--robot-schema",
                description: "JSON Schema definitions for all robot command outputs.",
                key_fields: vec!["schema_version", "envelope", "commands"],
                params: vec!["--schema-command <cmd>"],
                needs_issues: false,
            },
        ),
        (
            "robot-docs",
            CmdDoc {
                flag: "--robot-docs <topic>",
                description: "Machine-readable JSON documentation. Topics: guide, commands, examples, env, exit-codes, all.",
                key_fields: vec![],
                params: vec![],
                needs_issues: false,
            },
        ),
        (
            "robot-history",
            CmdDoc {
                flag: "--robot-history",
                description: "Bead-to-commit correlations from git history.",
                key_fields: vec!["correlations", "confidence", "commit_sha", "bead_id"],
                params: vec![
                    "--bead-history <id>",
                    "--history-since <date>",
                    "--history-limit <n>",
                    "--min-confidence 0.0-1.0",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-diff",
            CmdDoc {
                flag: "--robot-diff",
                description: "Changes since a historical point (commit, branch, tag, or date).",
                key_fields: vec![],
                params: vec!["--diff-since <ref>"],
                needs_issues: true,
            },
        ),
        (
            "robot-graph",
            CmdDoc {
                flag: "--robot-graph",
                description: "Dependency graph export in JSON, DOT, or Mermaid format.",
                key_fields: vec![],
                params: vec![
                    "--graph-format json|dot|mermaid",
                    "--graph-root <id>",
                    "--graph-depth <n>",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-forecast",
            CmdDoc {
                flag: "--robot-forecast <id|all>",
                description: "ETA predictions for bead completion.",
                key_fields: vec![],
                params: vec![
                    "--forecast-label <label>",
                    "--forecast-sprint <id>",
                    "--forecast-agents <n>",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-capacity",
            CmdDoc {
                flag: "--robot-capacity",
                description: "Capacity simulation and completion projections.",
                key_fields: vec![],
                params: vec!["--agents <n>", "--capacity-label <label>"],
                needs_issues: true,
            },
        ),
        (
            "robot-burndown",
            CmdDoc {
                flag: "--robot-burndown <sprint|current>",
                description: "Sprint burndown data.",
                key_fields: vec![],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-sprint-list",
            CmdDoc {
                flag: "--robot-sprint-list",
                description: "List all discovered sprints.",
                key_fields: vec!["sprint_count", "sprints"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-sprint-show",
            CmdDoc {
                flag: "--robot-sprint-show <id>",
                description: "Show a single sprint payload.",
                key_fields: vec!["sprint"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-metrics",
            CmdDoc {
                flag: "--robot-metrics",
                description: "Emit timing, cache, and memory telemetry.",
                key_fields: vec!["timing", "cache", "memory"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-label-health",
            CmdDoc {
                flag: "--robot-label-health",
                description: "Per-label health, staleness, and blocked-work summary.",
                key_fields: vec!["analysis_config", "results"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-label-flow",
            CmdDoc {
                flag: "--robot-label-flow",
                description: "Cross-label dependency flow and bottlenecks.",
                key_fields: vec!["analysis_config", "flow"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-label-attention",
            CmdDoc {
                flag: "--robot-label-attention",
                description: "Attention-ranked labels using graph and freshness signals.",
                key_fields: vec!["limit", "labels", "total_labels"],
                params: vec!["--attention-limit <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-explain-correlation",
            CmdDoc {
                flag: "--robot-explain-correlation <sha:bead>",
                description: "Explain a git-history correlation candidate.",
                key_fields: vec!["explanation"],
                params: vec!["--correlation-by <actor>", "--correlation-reason <text>"],
                needs_issues: true,
            },
        ),
        (
            "robot-confirm-correlation",
            CmdDoc {
                flag: "--robot-confirm-correlation <sha:bead>",
                description: "Persist positive feedback for a history correlation candidate.",
                key_fields: vec!["status", "commit", "bead", "by", "reason", "orig_conf"],
                params: vec!["--correlation-by <actor>", "--correlation-reason <text>"],
                needs_issues: true,
            },
        ),
        (
            "robot-reject-correlation",
            CmdDoc {
                flag: "--robot-reject-correlation <sha:bead>",
                description: "Persist rejection feedback for a history correlation candidate.",
                key_fields: vec!["status", "commit", "bead", "by", "reason", "orig_conf"],
                params: vec!["--correlation-by <actor>", "--correlation-reason <text>"],
                needs_issues: true,
            },
        ),
        (
            "robot-correlation-stats",
            CmdDoc {
                flag: "--robot-correlation-stats",
                description: "Summarize stored correlation feedback.",
                key_fields: vec!["total_feedback", "confirmed", "rejected", "accuracy_rate"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-orphans",
            CmdDoc {
                flag: "--robot-orphans",
                description: "Detect high-signal repository files that are not covered by bead history.",
                key_fields: vec!["stats", "candidates"],
                params: vec!["--orphans-min-score <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-file-beads",
            CmdDoc {
                flag: "--robot-file-beads <path>",
                description: "Look up beads correlated with a file path.",
                key_fields: vec!["file_path", "total_beads", "open_beads", "closed_beads"],
                params: vec!["--file-beads-limit <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-file-hotspots",
            CmdDoc {
                flag: "--robot-file-hotspots",
                description: "Rank file hotspots from bead-history evidence.",
                key_fields: vec!["hotspots", "stats"],
                params: vec!["--hotspots-limit <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-impact",
            CmdDoc {
                flag: "--robot-impact <path[,path...]>",
                description: "Estimate issue impact for one or more file paths.",
                key_fields: vec![
                    "risk_level",
                    "risk_score",
                    "summary",
                    "files",
                    "affected_beads",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-file-relations",
            CmdDoc {
                flag: "--robot-file-relations <path>",
                description: "Find related files using shared bead-history evidence.",
                key_fields: vec!["source_file", "related_files", "total_commits_for_source"],
                params: vec!["--relations-threshold <n>", "--relations-limit <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-related",
            CmdDoc {
                flag: "--robot-related <bead-id>",
                description: "Find related work from file and history overlap.",
                key_fields: vec!["source_bead", "related"],
                params: vec![
                    "--related-min-relevance <n>",
                    "--related-max-results <n>",
                    "--related-include-closed",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-blocker-chain",
            CmdDoc {
                flag: "--robot-blocker-chain <bead-id>",
                description: "Show upstream blockers for a target bead.",
                key_fields: vec![
                    "target_id",
                    "chain_length",
                    "is_blocked",
                    "has_cycle",
                    "root_blockers",
                ],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-impact-network",
            CmdDoc {
                flag: "--robot-impact-network <bead-id>",
                description: "Build a causal impact network around a target bead.",
                key_fields: vec!["bead_id", "depth", "network", "top_connected"],
                params: vec!["--network-depth <n>"],
                needs_issues: true,
            },
        ),
        (
            "robot-causality",
            CmdDoc {
                flag: "--robot-causality <bead-id>",
                description: "Build a causality chain using graph and history evidence.",
                key_fields: vec!["chain", "insights"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-drift",
            CmdDoc {
                flag: "--robot-drift",
                description: "Compare current state to a saved baseline and emit structured drift alerts.",
                key_fields: vec!["summary", "alerts", "baseline"],
                params: vec![],
                needs_issues: true,
            },
        ),
        (
            "robot-search",
            CmdDoc {
                flag: "--robot-search",
                description: "Run text or hybrid ranking over beads.",
                key_fields: vec!["query", "limit", "mode", "results"],
                params: vec![
                    "--search <query>",
                    "--search-mode text|hybrid",
                    "--search-preset <name>",
                    "--search-weights <json>",
                    "--search-limit <n>",
                    "--search-min-score <-1..1>",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-recipes",
            CmdDoc {
                flag: "--robot-recipes",
                description: "List named pre-filter recipes used by triage and scripting flows.",
                key_fields: vec!["recipes"],
                params: vec![],
                needs_issues: false,
            },
        ),
        (
            "robot-economics",
            CmdDoc {
                flag: "--robot-economics",
                description: "Operating-cost projection: burn rate, throughput, cost-to-complete, cost-of-delay. Pure arithmetic over existing analyzer state plus a small opt-in overlay.",
                key_fields: vec![
                    "schema_version",
                    "overlay_hash",
                    "inputs.hourly_rate",
                    "inputs.estimate_coverage_pct",
                    "projections.burn_rate_per_day",
                    "projections.cost_to_complete",
                    "projections.cost_of_delay",
                    "guards",
                ],
                params: vec![
                    "--economics-overlay <path.json>",
                    "--insight-limit <n>",
                    "(env) BVR_ECONOMICS_OVERLAY",
                ],
                needs_issues: true,
            },
        ),
        (
            "robot-delivery",
            CmdDoc {
                flag: "--robot-delivery",
                description: "Delivery posture: Reinertsen flow_distribution (Risk>Debt>Defects>Features), urgency_profile (Expedite>Fixed-Date>Intangible>Standard), and milestone_pressure. Classification only; no overlay required.",
                key_fields: vec![
                    "schema_version",
                    "flow_distribution",
                    "urgency_profile",
                    "milestone_pressure",
                ],
                params: vec!["--insight-limit <n>"],
                needs_issues: true,
            },
        ),
    ])
}

fn robot_env_vars() -> Value {
    serde_json::json!({
        "BV_OUTPUT_FORMAT": "Default output format: json or toon (overridden by --format)",
        "TOON_DEFAULT_FORMAT": "Fallback format if BV_OUTPUT_FORMAT not set",
        "TOON_STATS": "Set to 1 to show JSON vs TOON token estimates on stderr",
        "TOON_KEY_FOLDING": "TOON key folding mode",
        "TOON_INDENT": "TOON indentation level (0-16)",
        "BV_PRETTY_JSON": "Set to 1 for indented JSON output",
        "BV_ROBOT": "Set to 1 to force robot mode (clean stdout)",
        "BV_SEARCH_MODE": "Search mode: text or hybrid",
        "BV_SEARCH_PRESET": "Hybrid search preset name",
        "BV_ROBOT_NOT_READY_LABELS": "Comma-separated labels excluded from claimable top picks (overridden by --robot-not-ready-labels)",
        "BEADS_DB": "Beads database/JSONL file or .beads directory to read (takes priority over BEADS_DIR)",
        "BEADS_DIR": "Beads directory to read instead of discovering .beads",
        "BV_DATA_SOURCE": "Store read from a br .beads directory: auto (freshest, default), sqlite, or jsonl",
    })
}

fn robot_exit_codes() -> Value {
    serde_json::json!({
        "0": "Success",
        "1": "Error (general failure, drift critical)",
        "2": "Invalid arguments or drift warning",
    })
}

/// `--robot-capabilities`: one machine-readable manifest of the robot surface.
///
/// Covers every robot
/// command (flag, parameters, key fields, data needs), output formats, docs
/// topics, environment variables, exit codes, and the stdout/stderr contract,
/// so an agent can discover the whole surface in a single call.
#[must_use]
pub fn generate_robot_capabilities() -> Value {
    let commands: Vec<Value> = robot_command_docs()
        .into_iter()
        .map(|(name, doc)| {
            let mut entry = serde_json::json!({
                "name": name,
                "flag": doc.flag,
                "description": doc.description,
                "preferred_invocation": format!("bvr {} --format json", doc.flag),
                "needs_issues": doc.needs_issues,
                "mutates_state": matches!(
                    name,
                    "robot-confirm-correlation" | "robot-reject-correlation"
                ),
            });
            if !doc.key_fields.is_empty() {
                entry["key_fields"] = serde_json::json!(doc.key_fields);
            }
            if !doc.params.is_empty() {
                entry["params"] = serde_json::json!(doc.params);
            }
            entry
        })
        .collect();

    serde_json::json!({
        "generated_at": Utc::now().to_rfc3339(),
        "tool": "bvr",
        "version": env!("CARGO_PKG_VERSION"),
        "output_format": "json",
        "default_robot_command": "bvr --robot-triage",
        "output_formats": ["json", "toon"],
        "commands": commands,
        "docs_topics": ["guide", "commands", "examples", "env", "exit-codes", "all"],
        "docs_command": "bvr --robot-docs <topic>",
        "schema_command": "bvr --robot-schema",
        "agent_intent_aliases": {
            "bvr <command> | bvr robot-<command>": "Same as --robot-<command> (triage, next, plan, insights, priority, alerts, suggest, capabilities, label-health, ...)",
            "bvr <command> <value>": "Value commands take their first positional: forecast [id|all], burndown [sprint|current], related <id>, blockers <id>, impact <paths>, causality <id>, file-beads <path>",
            "bvr search <words...>": "--search \"<words>\" --robot-search",
            "bvr graph [json|dot|mermaid]": "--robot-graph --graph-format <fmt>",
            "bvr diff <ref> / bvr history [id]": "--robot-diff --diff-since <ref> / --robot-history [--bead-history <id>]",
            "bvr docs [topic] / bvr schema [command]": "--robot-docs <topic> / --robot-schema --schema-command robot-<command>",
            "--json / --toon / -o <fmt>": "--format <fmt>; --json alone runs --robot-triage",
            "--limit <n>": "The command's own limit flag (--robot-max-results, --search-limit, --history-limit, ...)",
        },
        "environment_variables": robot_env_vars(),
        "exit_codes": robot_exit_codes(),
        "stream_contract": {
            "stdout": "Structured robot data only for robot commands.",
            "stderr": "Diagnostics, warnings, and actionable errors.",
        },
    })
}

#[must_use]
pub fn generate_robot_docs(topic: &str) -> Value {
    let now = Utc::now().to_rfc3339();
    let version = env!("CARGO_PKG_VERSION");

    let mut result = serde_json::json!({
        "generated_at": now,
        "output_format": "json",
        "version": version,
        "topic": topic,
    });

    let guide = serde_json::json!({
        "description": "bvr (Beads Viewer Rust) provides structural analysis of the beads issue tracker DAG. It is the primary interface for AI agents to understand project state, plan work, and discover high-impact tasks.",
        "quickstart": [
            "bvr --robot-triage               # Full triage with recommendations",
            "bvr --robot-next                  # Single top pick for immediate work",
            "bvr --robot-overview              # Compact orientation snapshot for fast agent loops",
            "bvr --robot-plan                  # Dependency-respecting execution plan",
            "bvr --robot-insights              # Deep graph analysis (PageRank, betweenness, etc.)",
            "bvr --robot-triage-by-track       # Parallel work streams for multi-agent coordination",
            "bvr --robot-schema                # JSON Schema definitions for all commands",
        ],
        "data_source": "The .beads store: the freshest of br's SQLite database and JSONL export declared in .beads/metadata.json (BV_DATA_SOURCE forces one), else beads.jsonl/issues.jsonl/beads.base.jsonl; bd (Dolt) workspaces via .beads/issues.jsonl. .beads/redirect chains are followed. Plus git history correlations.",
        "output_modes": {
            "json": "Default structured output",
            "toon": "Token-optimized notation (saves ~30-50% tokens)",
        },
    });

    let commands =
        serde_json::to_value(robot_command_docs()).unwrap_or_else(|_| serde_json::json!({}));

    let examples = serde_json::json!([
        {"description": "Get top 3 picks for immediate work", "command": "bvr --robot-triage | jq '.triage.quick_ref.top_picks[:3]'"},
        {"description": "Claim the top recommendation", "command": "bvr --robot-next | jq -r '.claim_command' | sh"},
        {"description": "Find high-impact blockers to clear", "command": "bvr --robot-triage | jq '.triage.blockers_to_clear | map(.id)'"},
        {"description": "Get bug-only recommendations", "command": "bvr --robot-triage | jq '.triage.recommendations[] | select(.type == \"bug\")'"},
        {"description": "Multi-agent: top pick per parallel track", "command": "bvr --robot-triage-by-track | jq '.triage.recommendations_by_track[].top_pick'"},
        {"description": "Get TOON output (saves tokens)", "command": "bvr --robot-triage --format toon"},
        {"description": "Use env for default format", "command": "BV_OUTPUT_FORMAT=toon bvr --robot-triage"},
        {"description": "Show token savings estimate", "command": "bvr --robot-triage --format toon --stats"},
    ]);

    let env_vars = robot_env_vars();

    let exit_codes = robot_exit_codes();

    match topic {
        "guide" => {
            result["guide"] = guide;
        }
        "commands" => {
            result["commands"] = commands;
        }
        "examples" => {
            result["examples"] = examples;
        }
        "env" => {
            result["environment_variables"] = env_vars;
        }
        "exit-codes" => {
            result["exit_codes"] = exit_codes;
        }
        "all" => {
            result["guide"] = guide;
            result["commands"] = commands;
            result["examples"] = examples;
            result["environment_variables"] = env_vars;
            result["exit_codes"] = exit_codes;
        }
        _ => {
            result["error"] = Value::String(format!("Unknown topic: {topic}"));
            result["available_topics"] =
                serde_json::json!(["guide", "commands", "examples", "env", "exit-codes", "all"]);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// --robot-schema
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RobotSchemas {
    pub schema_version: String,
    pub generated_at: String,
    pub envelope: Value,
    pub commands: BTreeMap<String, Value>,
}

fn schema_prop(type_str: &str) -> Value {
    serde_json::json!({"type": type_str})
}

fn schema_prop_dt() -> Value {
    serde_json::json!({"type": "string", "format": "date-time"})
}

fn simple_command_schema(title: &str, description: &str, properties: Value) -> Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "description": description,
        "type": "object",
        "properties": properties,
    })
}

fn versioned_properties(mut properties: Value) -> Value {
    let Value::Object(ref mut map) = properties else {
        return properties;
    };
    map.insert("output_format".to_string(), schema_prop("string"));
    map.insert("version".to_string(), schema_prop("string"));
    properties
}

fn versioned_simple_command_schema(title: &str, description: &str, properties: Value) -> Value {
    simple_command_schema(title, description, versioned_properties(properties))
}

#[must_use]
pub fn generate_robot_schemas() -> RobotSchemas {
    let now = Utc::now().to_rfc3339();

    let envelope = serde_json::json!({
        "type": "object",
        "properties": {
            "generated_at": {
                "type": "string",
                "format": "date-time",
                "description": "ISO 8601 timestamp when output was generated",
            },
            "data_hash": {
                "type": "string",
                "description": "Fingerprint of source beads.jsonl for cache validation",
            },
            "output_format": {
                "type": "string",
                "enum": ["json", "toon"],
                "description": "Output format used (json or toon)",
            },
            "version": {
                "type": "string",
                "description": "bvr version that generated this output",
            },
        },
        "required": ["generated_at", "data_hash"],
    });

    let mut commands = BTreeMap::new();

    commands.insert("robot-triage".to_string(), serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Robot Triage Output",
        "description": "Unified triage recommendations with quick picks, blockers, and project health",
        "type": "object",
        "properties": {
            "generated_at": schema_prop_dt(),
            "data_hash": schema_prop("string"),
            "triage": {
                "type": "object",
                "properties": {
                    "meta": {
                        "type": "object",
                        "properties": {
                            "version": schema_prop("string"),
                            "generated_at": schema_prop("string"),
                            "phase2_ready": schema_prop("boolean"),
                            "issue_count": schema_prop("integer"),
                        }
                    },
                    "quick_ref": {
                        "type": "object",
                        "properties": {
                            "open_count": {"type": "integer", "description": "Strict count of issues with status == open"},
                            "actionable_count": {"type": "integer", "description": "Non-closed issues ready to work on now"},
                            "blocked_count": {"type": "integer", "description": "Strict count of issues with status == blocked"},
                            "in_progress_count": {"type": "integer", "description": "Strict count of issues with status == in_progress"},
                            "not_closed_count": {"type": "integer", "description": "All non-closed issues; equals actionable_count + not_actionable_count"},
                            "not_actionable_count": {"type": "integer", "description": "Non-closed issues that are not actionable (open blockers, parked status, deferral)"},
                            "top_picks": {
                                "type": "array",
                                "items": {"$ref": "#/$defs/recommendation"}
                            }
                        }
                    },
                    "recommendations": {"type": "array", "items": {"$ref": "#/$defs/recommendation"}},
                    "quick_wins": {"type": "array"},
                    "blockers_to_clear": {"type": "array"},
                    "project_health": {"type": "object"},
                    "commands": {"type": "object"},
                }
            },
            "usage_hints": {"type": "array", "items": schema_prop("string")},
        },
        "$defs": {
            "recommendation": {
                "type": "object",
                "properties": {
                    "id": schema_prop("string"),
                    "title": schema_prop("string"),
                    "type": schema_prop("string"),
                    "status": schema_prop("string"),
                    "priority": schema_prop("integer"),
                    "labels": {"type": "array", "items": schema_prop("string")},
                    "score": schema_prop("number"),
                    "impact_score": schema_prop("number"),
                    "confidence": schema_prop("number"),
                    "action": schema_prop("string"),
                    "reasons": {"type": "array", "items": schema_prop("string")},
                    "unblocks": schema_prop("integer"),
                    "unblocks_ids": {"type": "array", "items": schema_prop("string")},
                    "blocked_by": {"type": "array", "items": schema_prop("string")},
                    "assignee": schema_prop("string"),
                    "claim_command": schema_prop("string"),
                    "show_command": schema_prop("string"),
                },
                "required": ["id", "title", "score", "action"],
            }
        }
    }));

    commands.insert(
        "robot-next".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Next Output",
            "description": "Single top pick recommendation with claim command",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "id": schema_prop("string"),
                "title": schema_prop("string"),
                "score": schema_prop("number"),
                "reasons": {"type": "array", "items": schema_prop("string")},
                "unblocks": schema_prop("integer"),
                "claim_command": schema_prop("string"),
                "show_command": schema_prop("string"),
            },
            "required": ["generated_at", "data_hash", "id", "title", "score"],
        }),
    );

    commands.insert(
        "robot-overview".to_string(),
        versioned_simple_command_schema(
            "Robot Overview Output",
            "Compact orientation payload with headline counts, top action, blocker signal, labels, and suggested next commands.",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "summary": {
                    "type": "object",
                    "properties": {
                        "open_issues": schema_prop("integer"),
                        "actionable_issues": schema_prop("integer"),
                        "blocked_issues": schema_prop("integer"),
                        "in_progress_issues": schema_prop("integer"),
                        "closed_issues": schema_prop("integer"),
                        "cycle_count": schema_prop("integer"),
                    },
                },
                "top_pick": {
                    "type": "object",
                    "properties": {
                        "id": schema_prop("string"),
                        "title": schema_prop("string"),
                        "score": schema_prop("number"),
                        "reasons": {"type": "array", "items": schema_prop("string")},
                        "claim_command": schema_prop("string"),
                    },
                },
                "top_blocker": {
                    "type": "object",
                    "properties": {
                        "id": schema_prop("string"),
                        "title": schema_prop("string"),
                        "unblocks": schema_prop("integer"),
                        "show_command": schema_prop("string"),
                    },
                },
                "top_labels": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": schema_prop("string"),
                            "open_issues": schema_prop("integer"),
                        },
                    },
                },
                "fronts": {
                    "type": "array",
                    "description": "Diverse work fronts grouped by label, each with a representative pick (max 8)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": schema_prop("string"),
                            "open_count": schema_prop("integer"),
                            "representative": {
                                "type": "object",
                                "properties": {
                                    "id": schema_prop("string"),
                                    "title": schema_prop("string"),
                                    "score": schema_prop("number"),
                                    "reasons": {"type": "array", "items": schema_prop("string")},
                                    "claim_command": schema_prop("string"),
                                },
                            },
                        },
                    },
                },
                "commands": {
                    "type": "object",
                    "properties": {
                        "next": schema_prop("string"),
                        "triage": schema_prop("string"),
                        "plan": schema_prop("string"),
                        "history": schema_prop("string"),
                    },
                },
                "usage_hints": {"type": "array", "items": schema_prop("string")},
            }),
        ),
    );

    commands.insert(
        "robot-plan".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Plan Output",
            "description": "Dependency-respecting execution plan with parallel tracks",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "plan": {
                    "type": "object",
                    "properties": {
                        "total_actionable": schema_prop("integer"),
                        "total_blocked": schema_prop("integer"),
                        "tracks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "track_id": schema_prop("string"),
                                    "items": {"type": "array"},
                                    "reason": schema_prop("string"),
                                }
                            }
                        },
                        "summary": {
                            "type": "object",
                            "properties": {
                                "track_count": schema_prop("integer"),
                                "actionable_count": schema_prop("integer"),
                                "unblocks_count": schema_prop("integer"),
                                "highest_impact": schema_prop("string"),
                                "impact_reason": schema_prop("string"),
                            }
                        },
                    }
                },
                "status": {"type": "object"},
                "usage_hints": {"type": "array"},
            },
        }),
    );

    commands.insert("robot-insights".to_string(), serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Robot Insights Output",
        "description": "Full graph analysis metrics including PageRank, betweenness, HITS, cycles",
        "type": "object",
        "properties": {
            "generated_at": schema_prop_dt(),
            "data_hash": schema_prop("string"),
            "Stats": {"type": "object"},
            "Cycles": {"type": "array"},
            "Keystones": {"type": "array"},
            "Bottlenecks": {"type": "array"},
            "Influencers": {"type": "array"},
            "Hubs": {"type": "array"},
            "Authorities": {"type": "array"},
            "Orphans": {"type": "array"},
            "Cores": {"type": "object"},
            "Articulation": {"type": "array"},
            "Slack": {"type": "object"},
            "Velocity": {"type": "object"},
            "status": {"type": "object"},
            "advanced_insights": {"type": "object"},
            "usage_hints": {"type": "array"},
        },
    }));

    commands.insert(
        "robot-priority".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Priority Output",
            "description": "Priority misalignment detection with recommendations",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "recommendations": {"type": "array"},
                "status": {"type": "object"},
                "usage_hints": {"type": "array"},
            },
        }),
    );

    commands.insert(
        "robot-graph".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Graph Output",
            "description": "Dependency graph in JSON/DOT/Mermaid format",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "format": {"type": "string", "enum": ["json", "dot", "mermaid"]},
                "nodes": {"type": "array"},
                "edges": {"type": "array"},
                "stats": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-diff".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Diff Output",
            "description": "Changes since a historical point (commit, branch, date)",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "since": schema_prop("string"),
                "since_commit": schema_prop("string"),
                "new": {"type": "array"},
                "closed": {"type": "array"},
                "modified": {"type": "array"},
                "cycles": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-alerts".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Alerts Output",
            "description": "Stale issues, blocking cascades, priority mismatches",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "alerts": {"type": "array"},
                "summary": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-suggest".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Suggest Output",
            "description": "Smart suggestions for duplicates, dependencies, labels, cycle breaks",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "suggestions": {"type": "array"},
                "counts": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-burndown".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Burndown Output",
            "description": "Sprint burndown data with scope changes and at-risk items",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "output_format": schema_prop("string"),
                "version": schema_prop("string"),
                "sprint_id": schema_prop("string"),
                "burndown": {"type": "array"},
                "scope_changes": {"type": "array"},
                "at_risk": {"type": "array"},
            },
        }),
    );

    commands.insert(
        "robot-forecast".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Forecast Output",
            "description": "ETA predictions with dependency-aware scheduling",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "output_format": schema_prop("string"),
                "version": schema_prop("string"),
                "forecasts": {"type": "array"},
                "methodology": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-history".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot History Output",
            "description": "Bead-to-commit correlations from git history",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "beads": {"type": "array"},
                "stats": {"type": "object"},
            },
        }),
    );

    commands.insert(
        "robot-capacity".to_string(),
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Robot Capacity Output",
            "description": "Capacity simulation and completion projections",
            "type": "object",
            "properties": {
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "output_format": schema_prop("string"),
                "version": schema_prop("string"),
                "capacity": {"type": "object"},
                "projections": {"type": "array"},
            },
        }),
    );

    commands.insert(
        "robot-triage-by-track".to_string(),
        simple_command_schema(
            "Robot Triage By Track Output",
            "Triage grouped by parallel execution track",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "triage": {"type": "object"},
                "feedback": {"type": "object"},
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-triage-by-label".to_string(),
        simple_command_schema(
            "Robot Triage By Label Output",
            "Triage grouped by label/domain",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "triage": {"type": "object"},
                "feedback": {"type": "object"},
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-schema".to_string(),
        simple_command_schema(
            "Robot Schema Output",
            "JSON Schema definitions for robot commands",
            serde_json::json!({
                "schema_version": schema_prop("string"),
                "generated_at": schema_prop_dt(),
                "command": schema_prop("string"),
                "schema": {"type": "object"},
                "envelope": {"type": "object"},
                "commands": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-docs".to_string(),
        simple_command_schema(
            "Robot Docs Output",
            "Machine-readable documentation for robot command usage",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "output_format": schema_prop("string"),
                "version": schema_prop("string"),
                "topic": schema_prop("string"),
                "guide": {"type": "object"},
                "commands": {"type": "object"},
                "examples": {"type": "array"},
                "environment_variables": {"type": "object"},
                "exit_codes": {"type": "object"},
                "error": schema_prop("string"),
                "available_topics": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-sprint-list".to_string(),
        versioned_simple_command_schema(
            "Robot Sprint List Output",
            "List of available sprints",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "sprint_count": schema_prop("integer"),
                "sprints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-sprint-show".to_string(),
        versioned_simple_command_schema(
            "Robot Sprint Show Output",
            "Single sprint detail payload",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "sprint": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-metrics".to_string(),
        versioned_simple_command_schema(
            "Robot Metrics Output",
            "Timing, cache, and memory telemetry",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "timing": {"type": "array"},
                "cache": {"type": "array"},
                "memory": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-label-health".to_string(),
        versioned_simple_command_schema(
            "Robot Label Health Output",
            "Per-label health summary",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "analysis_config": {"type": "object"},
                "results": {"type": "object"},
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-label-flow".to_string(),
        versioned_simple_command_schema(
            "Robot Label Flow Output",
            "Cross-label flow summary",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "analysis_config": {"type": "object"},
                "flow": {"type": "object"},
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-label-attention".to_string(),
        versioned_simple_command_schema(
            "Robot Label Attention Output",
            "Attention-ranked labels",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "limit": schema_prop("integer"),
                "labels": {"type": "array"},
                "total_labels": schema_prop("integer"),
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-explain-correlation".to_string(),
        simple_command_schema(
            "Robot Explain Correlation Output",
            "Explanation for a commit-to-bead correlation",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "explanation": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-confirm-correlation".to_string(),
        simple_command_schema(
            "Robot Confirm Correlation Output",
            "Confirmation feedback result for a correlation candidate",
            serde_json::json!({
                "status": schema_prop("string"),
                "commit": schema_prop("string"),
                "bead": schema_prop("string"),
                "by": schema_prop("string"),
                "reason": schema_prop("string"),
                "orig_conf": schema_prop("number"),
            }),
        ),
    );
    commands.insert(
        "robot-reject-correlation".to_string(),
        simple_command_schema(
            "Robot Reject Correlation Output",
            "Rejection feedback result for a correlation candidate",
            serde_json::json!({
                "status": schema_prop("string"),
                "commit": schema_prop("string"),
                "bead": schema_prop("string"),
                "by": schema_prop("string"),
                "reason": schema_prop("string"),
                "orig_conf": schema_prop("number"),
            }),
        ),
    );
    commands.insert(
        "robot-correlation-stats".to_string(),
        versioned_simple_command_schema(
            "Robot Correlation Stats Output",
            "Stored correlation feedback statistics",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "total_feedback": schema_prop("integer"),
                "confirmed": schema_prop("integer"),
                "rejected": schema_prop("integer"),
                "ignored": schema_prop("integer"),
                "accuracy_rate": schema_prop("number"),
                "avg_confirm_conf": schema_prop("number"),
                "avg_reject_conf": schema_prop("number"),
            }),
        ),
    );
    commands.insert(
        "robot-orphans".to_string(),
        versioned_simple_command_schema(
            "Robot Orphans Output",
            "Repository orphan-file report",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "stats": {"type": "object"},
                "candidates": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-file-beads".to_string(),
        versioned_simple_command_schema(
            "Robot File Beads Output",
            "Beads related to a file path",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "file_path": schema_prop("string"),
                "open_beads": {"type": "array"},
                "closed_beads": {"type": "array"},
                "total_beads": schema_prop("integer"),
            }),
        ),
    );
    commands.insert(
        "robot-file-hotspots".to_string(),
        versioned_simple_command_schema(
            "Robot File Hotspots Output",
            "Hotspot ranking derived from file history",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "hotspots": {"type": "array"},
                "stats": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-impact".to_string(),
        versioned_simple_command_schema(
            "Robot Impact Output",
            "Impact analysis for one or more file paths",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "files": {"type": "array"},
                "affected_beads": {"type": "array"},
                "risk_level": schema_prop("string"),
                "risk_score": schema_prop("number"),
                "summary": schema_prop("string"),
            }),
        ),
    );
    commands.insert(
        "robot-file-relations".to_string(),
        versioned_simple_command_schema(
            "Robot File Relations Output",
            "Related files derived from bead/file co-occurrence",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "source_file": schema_prop("string"),
                "related_files": {"type": "array"},
                "total_commits_for_source": schema_prop("integer"),
            }),
        ),
    );
    commands.insert(
        "robot-related".to_string(),
        versioned_simple_command_schema(
            "Robot Related Output",
            "Related work recommendations for a bead",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "source_bead": schema_prop("string"),
                "related": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-blocker-chain".to_string(),
        versioned_simple_command_schema(
            "Robot Blocker Chain Output",
            "Upstream blocker chain for a bead",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "target_id": schema_prop("string"),
                "target_title": schema_prop("string"),
                "is_blocked": schema_prop("boolean"),
                "chain_length": schema_prop("integer"),
                "root_blockers": {"type": "array"},
                "chain": {"type": "array"},
                "has_cycle": schema_prop("boolean"),
                "cycle_ids": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-impact-network".to_string(),
        versioned_simple_command_schema(
            "Robot Impact Network Output",
            "Impact network around a bead",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "bead_id": schema_prop("string"),
                "depth": schema_prop("integer"),
                "network": {"type": "object"},
                "top_connected": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-causality".to_string(),
        versioned_simple_command_schema(
            "Robot Causality Output",
            "Causality chain around a bead",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "chain": {"type": "object"},
                "insights": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-drift".to_string(),
        versioned_simple_command_schema(
            "Robot Drift Output",
            "Structured baseline drift result",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "has_drift": schema_prop("boolean"),
                "exit_code": schema_prop("integer"),
                "summary": {"type": "object"},
                "alerts": {"type": "array"},
                "baseline": {"type": "object"},
            }),
        ),
    );
    commands.insert(
        "robot-search".to_string(),
        versioned_simple_command_schema(
            "Robot Search Output",
            "Search results over beads",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "query": schema_prop("string"),
                "limit": schema_prop("integer"),
                "mode": schema_prop("string"),
                "preset": schema_prop("string"),
                "weights": {"type": "object"},
                "min_score": schema_prop("number"),
                "results": {"type": "array"},
                "usage_hints": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-recipes".to_string(),
        versioned_simple_command_schema(
            "Robot Recipes Output",
            "Available named triage recipes",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "recipes": {"type": "array"},
            }),
        ),
    );
    commands.insert(
        "robot-economics".to_string(),
        versioned_simple_command_schema(
            "Robot Economics Output",
            "Operating-cost projection: burn rate, cost-to-complete, cost-of-delay. Pure arithmetic over analyzer state + opt-in overlay.",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "schema_version": schema_prop("string"),
                "overlay_hash": schema_prop("string"),
                "inputs": {
                    "type": "object",
                    "properties": {
                        "hourly_rate": schema_prop("number"),
                        "hours_per_day": schema_prop("number"),
                        "budget_envelope": schema_prop("number"),
                        "currency": schema_prop("string"),
                        "throughput_window_days": schema_prop("integer"),
                        "project_age_days": schema_prop("integer"),
                        "estimate_coverage_pct": schema_prop("number"),
                        "open_issues": schema_prop("integer"),
                        "closed_in_window": schema_prop("integer"),
                    },
                    "required": [
                        "hourly_rate", "hours_per_day", "throughput_window_days",
                        "project_age_days", "estimate_coverage_pct",
                        "open_issues", "closed_in_window",
                    ],
                },
                "projections": {
                    "type": "object",
                    "properties": {
                        "burn_rate_per_day": schema_prop("number"),
                        "throughput_issues_per_day": schema_prop("number"),
                        "cost_to_complete": schema_prop("number"),
                        "budget_utilization_pct": schema_prop("number"),
                        "cost_of_delay": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": schema_prop("string"),
                                    "title": schema_prop("string"),
                                    "dependents_count": schema_prop("integer"),
                                    "rate_per_day": schema_prop("number"),
                                },
                                "required": ["id", "dependents_count", "rate_per_day"],
                            },
                        },
                    },
                    "required": [
                        "burn_rate_per_day", "throughput_issues_per_day", "cost_of_delay",
                    ],
                },
                "guards": {
                    "type": "object",
                    "properties": {
                        "estimate_coverage_below_threshold": schema_prop("boolean"),
                        "project_too_young_for_throughput": schema_prop("boolean"),
                        "zero_throughput": schema_prop("boolean"),
                        "no_budget_envelope": schema_prop("boolean"),
                    },
                    "required": [
                        "estimate_coverage_below_threshold",
                        "project_too_young_for_throughput",
                        "zero_throughput",
                        "no_budget_envelope",
                    ],
                },
            }),
        ),
    );
    commands.insert(
        "robot-delivery".to_string(),
        versioned_simple_command_schema(
            "Robot Delivery Output",
            "Delivery posture: Reinertsen flow_distribution, urgency_profile, milestone_pressure. Classification only.",
            serde_json::json!({
                "generated_at": schema_prop_dt(),
                "data_hash": schema_prop("string"),
                "schema_version": schema_prop("string"),
                "open_issues": schema_prop("integer"),
                "window_days": schema_prop("integer"),
                "flow_distribution": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "category": {
                                "type": "string",
                                "enum": ["risk", "debt", "defects", "features"],
                            },
                            "count": schema_prop("integer"),
                            "pct": schema_prop("number"),
                        },
                        "required": ["category", "count", "pct"],
                    },
                },
                "urgency_profile": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "category": {
                                "type": "string",
                                "enum": ["expedite", "fixed_date", "intangible", "standard"],
                            },
                            "count": schema_prop("integer"),
                            "pct": schema_prop("number"),
                        },
                        "required": ["category", "count", "pct"],
                    },
                },
                "milestone_pressure": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": schema_prop("string"),
                            "title": schema_prop("string"),
                            "due_date": schema_prop_dt(),
                            "days_until_due": schema_prop("integer"),
                            "is_overdue": schema_prop("boolean"),
                            "is_blocked": schema_prop("boolean"),
                        },
                        "required": ["id", "due_date", "days_until_due", "is_overdue", "is_blocked"],
                    },
                },
            }),
        ),
    );

    RobotSchemas {
        schema_version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now,
        envelope,
        commands,
    }
}

// ---------------------------------------------------------------------------
// --stats (format token estimation)
// ---------------------------------------------------------------------------

#[must_use]
pub fn estimate_tokens(s: &str) -> usize {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return 0;
    }
    trimmed.len().div_ceil(4)
}

pub fn print_format_stats(json_output: &str, toon_output: Option<&str>) {
    let json_tokens = estimate_tokens(json_output);
    if let Some(toon) = toon_output {
        let toon_tokens = estimate_tokens(toon);
        let savings = if json_tokens > 0 && toon_tokens <= json_tokens {
            ((json_tokens - toon_tokens) * 100) / json_tokens
        } else {
            0
        };
        eprintln!("[stats] JSON~{json_tokens} tok, TOON~{toon_tokens} tok ({savings}% savings)");
    } else {
        eprintln!("Format stats:");
        eprintln!(
            "  JSON: ~{json_tokens} tokens ({} bytes)",
            json_output.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --robot-docs tests

    #[test]
    fn robot_capabilities_lists_every_documented_command() {
        let caps = generate_robot_capabilities();
        assert_eq!(caps["tool"], "bvr");
        let commands = caps["commands"].as_array().expect("commands array");
        let docs = generate_robot_docs("commands");
        assert_eq!(commands.len(), docs["commands"].as_object().unwrap().len());
        for entry in commands {
            assert!(entry["name"].is_string());
            assert!(entry["flag"].as_str().unwrap().starts_with("--"));
            assert!(entry["needs_issues"].is_boolean());
        }
        assert!(caps["environment_variables"]["BEADS_DB"].is_string());
        assert!(caps["exit_codes"]["0"].is_string());
    }

    #[test]
    fn robot_docs_guide_has_required_fields() {
        let docs = generate_robot_docs("guide");
        assert!(docs["generated_at"].is_string());
        assert_eq!(docs["output_format"], "json");
        assert_eq!(docs["topic"], "guide");
        assert!(docs["guide"]["description"].is_string());
        assert!(docs["guide"]["quickstart"].is_array());
        assert!(docs["guide"]["data_source"].is_string());
        assert!(docs["guide"]["output_modes"].is_object());
    }

    #[test]
    fn robot_docs_commands_lists_all_robot_commands() {
        let docs = generate_robot_docs("commands");
        let commands = docs["commands"].as_object().unwrap();
        let expected = implemented_robot_command_names();
        assert_eq!(
            commands.len(),
            expected.len(),
            "expected {} commands, got {}",
            expected.len(),
            commands.len()
        );
        for cmd in expected {
            assert!(commands.contains_key(*cmd), "missing docs entry for {cmd}");
        }
    }

    #[test]
    fn robot_docs_examples_is_array() {
        let docs = generate_robot_docs("examples");
        assert!(docs["examples"].is_array());
        let examples = docs["examples"].as_array().unwrap();
        assert!(!examples.is_empty());
        assert!(examples[0]["description"].is_string());
        assert!(examples[0]["command"].is_string());
    }

    #[test]
    fn robot_docs_env_vars_present() {
        let docs = generate_robot_docs("env");
        let env = docs["environment_variables"].as_object().unwrap();
        assert!(env.contains_key("BV_OUTPUT_FORMAT"));
        assert!(env.contains_key("TOON_STATS"));
    }

    #[test]
    fn robot_docs_exit_codes_present() {
        let docs = generate_robot_docs("exit-codes");
        let codes = docs["exit_codes"].as_object().unwrap();
        assert!(codes.contains_key("0"));
        assert!(codes.contains_key("1"));
        assert!(codes.contains_key("2"));
    }

    #[test]
    fn robot_docs_all_includes_every_section() {
        let docs = generate_robot_docs("all");
        assert!(docs["guide"].is_object());
        assert!(docs["commands"].is_object());
        assert!(docs["examples"].is_array());
        assert!(docs["environment_variables"].is_object());
        assert!(docs["exit_codes"].is_object());
    }

    #[test]
    fn robot_docs_invalid_topic_returns_error() {
        let docs = generate_robot_docs("nonsense");
        assert!(docs["error"].is_string());
        assert!(docs["available_topics"].is_array());
        let topics = docs["available_topics"].as_array().unwrap();
        assert!(topics.contains(&serde_json::json!("all")));
    }

    #[test]
    fn robot_docs_version_matches_cargo() {
        let docs = generate_robot_docs("guide");
        assert_eq!(docs["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn robot_docs_command_entries_match_current_flattened_payloads() {
        let docs = generate_robot_docs("commands");
        let commands = docs["commands"].as_object().unwrap();

        assert_eq!(
            commands["robot-correlation-stats"]["key_fields"],
            json!(["total_feedback", "confirmed", "rejected", "accuracy_rate"])
        );
        assert_eq!(
            commands["robot-orphans"]["key_fields"],
            json!(["stats", "candidates"])
        );
        assert_eq!(
            commands["robot-search"]["key_fields"],
            json!(["query", "limit", "mode", "results"])
        );
        assert_eq!(
            commands["robot-label-health"]["key_fields"],
            json!(["analysis_config", "results"])
        );
        assert_eq!(
            commands["robot-label-flow"]["key_fields"],
            json!(["analysis_config", "flow"])
        );
        assert_eq!(
            commands["robot-label-attention"]["key_fields"],
            json!(["limit", "labels", "total_labels"])
        );
        assert_eq!(
            commands["robot-overview"]["key_fields"],
            json!([
                "summary",
                "top_pick",
                "top_blocker",
                "top_labels",
                "fronts",
                "commands"
            ])
        );
    }

    // --robot-schema tests

    #[test]
    fn robot_schema_has_required_top_level_fields() {
        let schemas = generate_robot_schemas();
        assert_eq!(schemas.schema_version, env!("CARGO_PKG_VERSION"));
        assert!(!schemas.generated_at.is_empty());
        assert!(schemas.envelope.is_object());
        assert!(!schemas.commands.is_empty());
    }

    #[test]
    fn robot_schema_envelope_has_core_properties() {
        let schemas = generate_robot_schemas();
        let props = schemas.envelope["properties"].as_object().unwrap();
        assert!(props.contains_key("generated_at"));
        assert!(props.contains_key("data_hash"));
        assert!(props.contains_key("output_format"));
        assert!(props.contains_key("version"));
    }

    #[test]
    fn robot_schema_covers_all_implemented_commands() {
        let schemas = generate_robot_schemas();
        for cmd in implemented_robot_command_names() {
            assert!(
                schemas.commands.contains_key(*cmd),
                "missing schema for {cmd}"
            );
        }
    }

    #[test]
    fn robot_docs_and_schema_command_sets_match() {
        let docs = generate_robot_docs("commands");
        let docs_commands = docs["commands"].as_object().unwrap();
        let schemas = generate_robot_schemas();

        for cmd in implemented_robot_command_names() {
            assert!(docs_commands.contains_key(*cmd), "docs missing {cmd}");
            assert!(schemas.commands.contains_key(*cmd), "schema missing {cmd}");
        }
    }

    #[test]
    fn robot_schema_triage_has_defs() {
        let schemas = generate_robot_schemas();
        let triage = &schemas.commands["robot-triage"];
        assert!(triage["$defs"].is_object());
        assert!(triage["$defs"]["recommendation"].is_object());
    }

    #[test]
    fn robot_schema_each_command_has_type_object() {
        let schemas = generate_robot_schemas();
        for (name, schema) in &schemas.commands {
            assert_eq!(
                schema["type"], "object",
                "schema for {name} should be type: object"
            );
        }
    }

    #[test]
    fn robot_schema_overview_has_summary_and_commands() {
        let schemas = generate_robot_schemas();
        let overview = &schemas.commands["robot-overview"];
        assert!(overview["properties"]["summary"].is_object());
        assert!(overview["properties"]["commands"].is_object());
    }

    // estimate_tokens tests

    #[test]
    fn estimate_tokens_empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("   "), 0);
    }

    #[test]
    fn estimate_tokens_short_string() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn estimate_tokens_matches_go_heuristic() {
        // Go: (len(trimmed) + 3) / 4
        let s = "hello world test string";
        let expected = s.len().div_ceil(4);
        assert_eq!(estimate_tokens(s), expected);
    }

    #[test]
    fn render_payload_sets_output_format_for_json() {
        let payload = json!({
            "generated_at": "2026-03-07T00:00:00Z",
            "data_hash": "abc123",
            "output_format": "json",
            "version": format!("v{}", env!("CARGO_PKG_VERSION"))
        });

        let rendered = render_payload(OutputFormat::Json, &payload).expect("rendered payload");
        let json: Value = serde_json::from_str(&rendered.output).expect("json output");
        assert_eq!(json["output_format"].as_str(), Some("json"));
        assert!(rendered.toon_for_stats.is_none());
    }

    #[test]
    fn print_format_stats_supports_toon_comparison() {
        print_format_stats("{\"id\":\"A\"}", Some("id: A\n"));
    }

    #[test]
    fn parse_toon_key_folding_mode_supports_safe_and_off() {
        assert_eq!(
            parse_toon_key_folding_mode("safe"),
            Some(KeyFoldingMode::Safe)
        );
        assert_eq!(parse_toon_key_folding_mode("off"), None);
    }

    // -- compute_data_hash tests --

    #[test]
    fn compute_data_hash_is_deterministic() {
        let issues = vec![
            Issue {
                id: "A".to_string(),
                status: "open".to_string(),
                priority: 1,
                ..Default::default()
            },
            Issue {
                id: "B".to_string(),
                status: "closed".to_string(),
                priority: 2,
                ..Default::default()
            },
        ];
        let h1 = compute_data_hash(&issues);
        let h2 = compute_data_hash(&issues);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16, "hash should be 16 hex chars");
    }

    #[test]
    fn compute_data_hash_is_order_independent() {
        let issues_ab = vec![
            Issue {
                id: "A".to_string(),
                status: "open".to_string(),
                ..Default::default()
            },
            Issue {
                id: "B".to_string(),
                status: "closed".to_string(),
                ..Default::default()
            },
        ];
        let issues_ba = vec![issues_ab[1].clone(), issues_ab[0].clone()];
        assert_eq!(
            compute_data_hash(&issues_ab),
            compute_data_hash(&issues_ba),
            "hash should be independent of issue order"
        );
    }

    #[test]
    fn compute_data_hash_empty_issues() {
        let h = compute_data_hash(&[]);
        assert_eq!(h.len(), 16);
    }

    #[test]
    fn compute_data_hash_changes_with_data() {
        let v1 = vec![Issue {
            id: "A".to_string(),
            status: "open".to_string(),
            ..Default::default()
        }];
        let v2 = vec![Issue {
            id: "A".to_string(),
            status: "closed".to_string(),
            ..Default::default()
        }];
        assert_ne!(
            compute_data_hash(&v1),
            compute_data_hash(&v2),
            "different status should produce different hash"
        );
    }

    #[test]
    fn snapshot_hash_detects_content_changes_without_timestamp_updates() {
        let issue = Issue {
            id: "A".to_string(),
            title: "Original title".to_string(),
            description: "Original description".to_string(),
            status: "open".to_string(),
            updated_at: crate::model::ts("2026-10-10T00:00:00Z"),
            labels: vec!["backend".to_string()],
            comments: vec![crate::model::Comment {
                id: 1,
                issue_id: "A".to_string(),
                text: "Original comment".to_string(),
                ..Default::default()
            }],
            dependencies: vec![crate::model::Dependency {
                issue_id: "A".to_string(),
                depends_on_id: "B".to_string(),
                dep_type: "blocks".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let original = compute_snapshot_hash(std::slice::from_ref(&issue));
        let robot_hash = compute_data_hash(std::slice::from_ref(&issue));

        let mut description = issue.clone();
        description.description = "Updated description".to_string();
        let mut comment = issue.clone();
        comment.comments[0].text = "Updated comment".to_string();
        let mut dependency = issue.clone();
        dependency.dependencies[0].depends_on_id = "C".to_string();
        let mut title = issue.clone();
        title.title = "Updated title".to_string();
        let mut label = issue.clone();
        label.labels[0] = "frontend".to_string();

        for (field, changed) in [
            ("description", description),
            ("comment", comment),
            ("dependency", dependency),
            ("title", title),
            ("label", label),
        ] {
            assert_eq!(
                compute_data_hash(std::slice::from_ref(&changed)),
                robot_hash,
                "{field} edit must leave the public robot hash unchanged"
            );
            assert_ne!(
                compute_snapshot_hash(std::slice::from_ref(&changed)),
                original,
                "{field} edit must invalidate the full snapshot"
            );
        }
    }

    #[test]
    fn snapshot_hash_detects_workspace_routing_changes() {
        let issue = Issue {
            id: "repo-A".to_string(),
            workspace_prefix: Some("repo-".to_string()),
            workspace_repo_path: Some(std::path::PathBuf::from("/workspace/first")),
            workspace_local_id: Some("A".to_string()),
            ..Default::default()
        };
        let original = compute_snapshot_hash(std::slice::from_ref(&issue));
        let serialized = serde_json::to_value(&issue).expect("serialize issue");

        let mut repo_path = issue.clone();
        repo_path.workspace_repo_path = Some(std::path::PathBuf::from("/workspace/second"));
        let mut prefix = issue.clone();
        prefix.workspace_prefix = Some("other-".to_string());
        let mut local_id = issue.clone();
        local_id.workspace_local_id = Some("local-A".to_string());

        for changed in [repo_path, prefix, local_id] {
            assert_eq!(
                serde_json::to_value(&changed).expect("serialize changed issue"),
                serialized,
                "workspace routing is omitted from serialized issue data"
            );
            assert_ne!(
                compute_snapshot_hash(std::slice::from_ref(&changed)),
                original,
                "workspace routing changes must invalidate the full snapshot"
            );
        }
    }

    #[test]
    fn snapshot_hash_ignores_issue_order_but_preserves_nested_order() {
        let issues = [
            Issue {
                id: "A".to_string(),
                labels: vec!["first".to_string(), "second".to_string()],
                ..Default::default()
            },
            Issue {
                id: "B".to_string(),
                ..Default::default()
            },
        ];
        let original = compute_snapshot_hash(&issues);
        assert_eq!(
            original,
            compute_snapshot_hash(&[issues[1].clone(), issues[0].clone()]),
            "the issue array may arrive in a different order"
        );

        let mut reordered_labels = issues.clone();
        reordered_labels[0].labels.reverse();
        assert_ne!(
            original,
            compute_snapshot_hash(&reordered_labels),
            "nested order is part of the displayed content"
        );
    }

    // -- envelope tests --

    #[test]
    fn envelope_produces_valid_fields() {
        let issues = vec![Issue {
            id: "X".to_string(),
            status: "open".to_string(),
            ..Default::default()
        }];
        let env = envelope(&issues);
        assert!(!env.generated_at.is_empty());
        assert_eq!(env.data_hash.len(), 16);
        // generated_at should be a parseable RFC3339 timestamp
        assert!(env.generated_at.contains('T'));
        assert_eq!(env.output_format, "json");
        assert!(env.version.starts_with('v'));
    }

    #[test]
    fn envelope_empty_has_empty_hash() {
        let env = envelope_empty();
        assert!(!env.generated_at.is_empty());
        assert!(env.data_hash.is_empty());
        assert_eq!(env.output_format, "json");
        assert!(env.version.starts_with('v'));
    }

    // -- default_field_descriptions tests --

    #[test]
    fn default_field_descriptions_has_core_fields() {
        let desc = default_field_descriptions();
        assert!(desc.contains_key("score"));
        assert!(desc.contains_key("confidence"));
        assert!(desc.contains_key("unblocks"));
        // All values should be non-empty
        for (key, value) in &desc {
            assert!(
                !value.is_empty(),
                "description for {key} should not be empty"
            );
        }
    }

    // -- TOON encoding tests --

    #[test]
    fn encode_toon_produces_output() {
        let value = json!({
            "id": "A",
            "score": 0.5
        });
        let result = encode_toon(&value);
        assert!(!result.is_empty());
        assert!(result.ends_with('\n'));
        assert!(
            result.contains("id:") || result.contains("id: A"),
            "TOON output should use key: value format"
        );
    }

    #[test]
    fn resolve_toon_encode_options_parses_env() {
        // Test with default env (no TOON_* vars set in test)
        let opts = resolve_toon_encode_options();
        // key_folding should be None unless env is set to non-"off" value
        // indent should be None unless env is set
        // Just verify it doesn't panic
        let _ = opts.key_folding;
        let _ = opts.indent;
    }

    #[test]
    fn set_top_level_output_format_patches_toon() {
        let mut value = json!({
            "output_format": "json",
            "data_hash": "abc"
        });
        set_top_level_output_format(&mut value, OutputFormat::Toon);
        assert_eq!(value["output_format"], "toon");
    }

    #[test]
    fn set_top_level_output_format_skips_when_absent() {
        let mut value = json!({"data_hash": "abc"});
        set_top_level_output_format(&mut value, OutputFormat::Toon);
        assert!(value.get("output_format").is_none());
    }

    #[test]
    fn render_payload_toon_sets_output_format_field() {
        let payload = json!({
            "output_format": "json",
            "data_hash": "abc123"
        });
        let rendered = render_payload(OutputFormat::Toon, &payload).expect("rendered");
        let stats_json: Value =
            serde_json::from_str(&rendered.json_for_stats).expect("parse stats json");
        assert_eq!(stats_json["output_format"], "toon");
        assert!(rendered.toon_for_stats.is_some());
    }

    #[test]
    fn render_payload_toon_output_is_not_json() {
        let payload = json!({
            "output_format": "toon",
            "data_hash": "abc123"
        });
        let rendered = render_payload(OutputFormat::Toon, &payload).expect("rendered");
        assert!(rendered.toon_for_stats.is_some());
        assert!(!rendered.output.trim_start().starts_with('{'));
    }

    #[test]
    fn print_format_stats_json_only_no_toon() {
        print_format_stats(r#"{"id":"A","count":10}"#, None);
    }

    #[test]
    fn print_format_stats_with_savings() {
        let json = r#"{"status":"open","priority":1,"labels":["backend","api"]}"#;
        let toon = "status: open\npriority: 1\nlabels: backend,api\n";
        print_format_stats(json, Some(toon));
    }

    #[test]
    fn encode_json_respects_pretty_flag() {
        let value = json!({"a": 1, "b": 2});
        let compact = encode_json(&value).expect("compact json");
        assert!(
            !compact.contains('\n'),
            "compact JSON should be single line"
        );
    }
}
