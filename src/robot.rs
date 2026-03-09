use std::collections::BTreeMap;

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
    ])
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
            "bvr --robot-plan                  # Dependency-respecting execution plan",
            "bvr --robot-insights              # Deep graph analysis (PageRank, betweenness, etc.)",
            "bvr --robot-triage-by-track       # Parallel work streams for multi-agent coordination",
            "bvr --robot-schema                # JSON Schema definitions for all commands",
        ],
        "data_source": ".beads/issues.jsonl and git history (correlations)",
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

    let env_vars = serde_json::json!({
        "BV_OUTPUT_FORMAT": "Default output format: json or toon (overridden by --format)",
        "TOON_DEFAULT_FORMAT": "Fallback format if BV_OUTPUT_FORMAT not set",
        "TOON_STATS": "Set to 1 to show JSON vs TOON token estimates on stderr",
        "TOON_KEY_FOLDING": "TOON key folding mode",
        "TOON_INDENT": "TOON indentation level (0-16)",
        "BV_PRETTY_JSON": "Set to 1 for indented JSON output",
        "BV_ROBOT": "Set to 1 to force robot mode (clean stdout)",
        "BV_SEARCH_MODE": "Search mode: text or hybrid",
        "BV_SEARCH_PRESET": "Hybrid search preset name",
    });

    let exit_codes = serde_json::json!({
        "0": "Success",
        "1": "Error (general failure, drift critical)",
        "2": "Invalid arguments or drift warning",
    });

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
                            "open_count": schema_prop("integer"),
                            "actionable_count": schema_prop("integer"),
                            "blocked_count": schema_prop("integer"),
                            "in_progress_count": schema_prop("integer"),
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
                    "reasons": {"type": "array", "items": schema_prop("string")},
                    "unblocks": schema_prop("integer"),
                },
                "required": ["id", "title", "score"],
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
                        "phases": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "phase": schema_prop("integer"),
                                    "issues": {"type": "array"},
                                }
                            }
                        },
                        "summary": {"type": "object"},
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
                "capacity": {"type": "object"},
                "projections": {"type": "array"},
            },
        }),
    );

    RobotSchemas {
        schema_version: "1.0.0".to_string(),
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
        assert!(
            commands.len() >= 15,
            "expected 15+ commands, got {}",
            commands.len()
        );
        assert!(commands.contains_key("robot-triage"));
        assert!(commands.contains_key("robot-next"));
        assert!(commands.contains_key("robot-schema"));
        assert!(commands.contains_key("robot-docs"));
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

    // --robot-schema tests

    #[test]
    fn robot_schema_has_required_top_level_fields() {
        let schemas = generate_robot_schemas();
        assert_eq!(schemas.schema_version, "1.0.0");
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
        let expected = [
            "robot-triage",
            "robot-next",
            "robot-plan",
            "robot-insights",
            "robot-priority",
            "robot-graph",
            "robot-diff",
            "robot-alerts",
            "robot-suggest",
            "robot-burndown",
            "robot-forecast",
            "robot-history",
            "robot-capacity",
        ];
        for cmd in &expected {
            assert!(
                schemas.commands.contains_key(*cmd),
                "missing schema for {cmd}"
            );
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
            "version": "v0.1.0"
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
