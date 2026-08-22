#![forbid(unsafe_code)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::too_many_lines)]

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fmt::Write;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use bvr::analysis::alerts::AlertOptions;
use bvr::analysis::git_history::{
    HistoryBeadCompat, HistoryEventCompat, HistoryMilestonesCompat, HistoryStatsCompat,
    build_workspace_id_aliases, compute_history_stats, correlate_histories_with_git_aliases,
    finalize_history_entries, load_git_commits,
};
use bvr::analysis::graph::AnalysisConfig;
use bvr::analysis::suggest::{SuggestOptions, SuggestionType};
use bvr::analysis::triage::{TriageOptions, TriageScoringOptions};
use bvr::analysis::{Analyzer, Insights, MetricStatus};
use bvr::cli::{BvrCommand, Cli, GraphFormat, GraphPreset, GraphStyle};
use bvr::loader;
use bvr::robot::{
    compute_data_hash, default_field_descriptions, emit_with_stats, envelope, envelope_empty,
    generate_robot_docs, generate_robot_schemas,
};
use chrono::{DateTime, Duration, Local, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn load_economics_overlay(
    cli_path: Option<&std::path::Path>,
) -> std::result::Result<bvr::analysis::economics::EconomicsOverlay, String> {
    // CLI flag takes precedence over env var so operators can override a
    // machine-wide default on a per-invocation basis.
    let path = cli_path
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("BVR_ECONOMICS_OVERLAY").map(std::path::PathBuf::from))
        .ok_or_else(|| {
            "--robot-economics requires --economics-overlay <path.json> or \
             BVR_ECONOMICS_OVERLAY to be set"
                .to_string()
        })?;
    let contents = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "failed to read economics overlay {}: {error}",
            path.display()
        )
    })?;
    bvr::analysis::economics::EconomicsOverlay::from_json_str(&contents)
}

fn analysis_config_for_cli(cli: &Cli) -> AnalysisConfig {
    if cli.robot_next
        || cli.robot_overview
        || cli.robot_triage
        || cli.robot_triage_by_track
        || cli.robot_triage_by_label
        || cli.robot_plan
        || cli.robot_priority
        || cli.emit_script
        || cli.feedback_accept.is_some()
        || cli.feedback_ignore.is_some()
        || cli.priority_brief.is_some()
    {
        AnalysisConfig::triage_runtime()
    } else {
        AnalysisConfig::full()
    }
}

fn actionable_ids_for_recipe_filters(analyzer: &Analyzer) -> Vec<String> {
    analyzer.graph.actionable_ids()
}

fn resolve_schema_command_key(schemas: &bvr::robot::RobotSchemas, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_flag_prefix = trimmed.trim_start_matches('-');
    let candidates = [
        trimmed.to_string(),
        without_flag_prefix.to_string(),
        format!("robot-{without_flag_prefix}"),
    ];

    candidates
        .into_iter()
        .find(|candidate| schemas.commands.contains_key(candidate))
}

fn arg_flag_was_explicit_in_args<I, S>(args: I, flag: &str) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter().any(|arg| {
        let text = arg.as_ref().to_string_lossy();
        text == flag || text.starts_with(&format!("{flag}="))
    })
}

fn validate_orphaned_modifier_flags(cli: &Cli) -> Option<String> {
    let raw_args: Vec<_> = std::env::args_os().skip(1).collect();

    let insights_flags = ["--robot-full-stats", "--insight-limit"];
    if !cli.robot_insights
        && insights_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some(
            "error: --robot-full-stats and --insight-limit require --robot-insights".to_string(),
        );
    }

    let graph_flags = [
        "--graph-format",
        "--graph-root",
        "--graph-depth",
        "--graph-preset",
        "--graph-style",
        "--graph-title",
    ];
    if !cli.robot_graph
        && cli.export_graph.is_none()
        && graph_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some(
            "error: graph modifiers require --robot-graph or --export-graph <path>".to_string(),
        );
    }

    let search_flags = [
        "--search-limit",
        "--search-mode",
        "--search-preset",
        "--search-weights",
    ];
    if !cli.robot_search
        && search_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: search modifiers require --robot-search".to_string());
    }

    if !cli.robot_search && arg_flag_was_explicit_in_args(&raw_args, "--search") {
        return Some("error: --search <query> requires --robot-search".to_string());
    }

    let history_flags = ["--history-limit", "--history-since", "--min-confidence"];
    if !cli.robot_history
        && cli.bead_history.is_none()
        && history_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some(
            "error: history modifiers require --robot-history or --bead-history <id>".to_string(),
        );
    }

    let alert_flags = ["--severity", "--alert-type", "--alert-label"];
    if !cli.robot_alerts
        && alert_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: alert filters require --robot-alerts".to_string());
    }

    let suggest_flags = ["--suggest-type", "--suggest-confidence", "--suggest-bead"];
    if !cli.robot_suggest
        && suggest_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: suggest filters require --robot-suggest".to_string());
    }

    if !cli.robot_orphans && arg_flag_was_explicit_in_args(&raw_args, "--orphans-min-score") {
        return Some("error: --orphans-min-score requires --robot-orphans".to_string());
    }

    if cli.robot_file_beads.is_none()
        && arg_flag_was_explicit_in_args(&raw_args, "--file-beads-limit")
    {
        return Some("error: --file-beads-limit requires --robot-file-beads <path>".to_string());
    }

    if !cli.robot_file_hotspots && arg_flag_was_explicit_in_args(&raw_args, "--hotspots-limit") {
        return Some("error: --hotspots-limit requires --robot-file-hotspots".to_string());
    }

    let relation_flags = ["--relations-threshold", "--relations-limit"];
    if cli.robot_file_relations.is_none()
        && relation_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: relation modifiers require --robot-file-relations <path>".to_string());
    }

    let related_flags = ["--related-min-relevance", "--related-max-results"];
    if cli.robot_related.is_none()
        && related_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: related-work modifiers require --robot-related <id>".to_string());
    }

    if cli.robot_related.is_none()
        && arg_flag_was_explicit_in_args(&raw_args, "--related-include-closed")
    {
        return Some("error: --related-include-closed requires --robot-related <id>".to_string());
    }

    if cli.robot_impact_network.is_none()
        && arg_flag_was_explicit_in_args(&raw_args, "--network-depth")
    {
        return Some("error: --network-depth requires --robot-impact-network <id>".to_string());
    }

    let forecast_flags = ["--forecast-label", "--forecast-sprint", "--forecast-agents"];
    if cli.robot_forecast.is_none()
        && forecast_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: forecast modifiers require --robot-forecast <id|all>".to_string());
    }

    let capacity_flags = ["--agents", "--capacity-label"];
    if !cli.robot_capacity
        && capacity_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: capacity modifiers require --robot-capacity".to_string());
    }

    if !cli.robot_label_attention && arg_flag_was_explicit_in_args(&raw_args, "--attention-limit") {
        return Some("error: --attention-limit requires --robot-label-attention".to_string());
    }

    let priority_flags = [
        "--robot-min-confidence",
        "--robot-by-label",
        "--robot-by-assignee",
    ];
    if !cli.robot_priority
        && priority_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: priority filters require --robot-priority".to_string());
    }

    let recommendation_result_flags = ["--robot-max-results"];
    let supports_result_limit = cli.robot_next
        || cli.robot_triage
        || cli.robot_triage_by_track
        || cli.robot_triage_by_label
        || cli.robot_priority
        || cli.robot_suggest;
    if !supports_result_limit
        && recommendation_result_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some(
            "error: --robot-max-results requires a recommendation command (--robot-next, --robot-triage, --robot-triage-by-track, --robot-triage-by-label, --robot-priority, or --robot-suggest)"
                .to_string(),
        );
    }

    let supports_weight_preset = supports_result_limit
        || cli.robot_plan
        || cli.emit_script
        || cli.feedback_accept.is_some()
        || cli.feedback_ignore.is_some()
        || cli.priority_brief.is_some()
        || cli.agent_brief.is_some();
    if !supports_weight_preset && arg_flag_was_explicit_in_args(&raw_args, "--weight-preset") {
        return Some(
            "error: --weight-preset requires a recommendation-scoring command".to_string(),
        );
    }

    let correlation_action_flags = ["--correlation-by", "--correlation-reason"];
    if cli.robot_confirm_correlation.is_none()
        && cli.robot_reject_correlation.is_none()
        && correlation_action_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some(
            "error: correlation action modifiers require --robot-confirm-correlation or --robot-reject-correlation"
                .to_string(),
        );
    }

    let emit_script_flags = ["--recipe", "--script-limit", "--script-format"];
    if !cli.emit_script
        && emit_script_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: script modifiers require --emit-script".to_string());
    }

    if !cli.profile_startup && arg_flag_was_explicit_in_args(&raw_args, "--profile-json") {
        return Some("error: --profile-json requires --profile-startup".to_string());
    }

    let debug_render_flags = ["--debug-width", "--debug-height"];
    if cli.debug_render.is_none()
        && debug_render_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: debug render dimensions require --debug-render <view>".to_string());
    }

    let export_pages_flags = [
        "--pages-include-closed",
        "--pages-include-history",
        "--pages-title",
        "--pages-subtitle",
    ];
    if cli.export_pages.is_none()
        && export_pages_flags
            .iter()
            .any(|flag| arg_flag_was_explicit_in_args(&raw_args, flag))
    {
        return Some("error: pages export modifiers require --export-pages <dir>".to_string());
    }

    if !cli.pages
        && cli.preview_pages.is_none()
        && arg_flag_was_explicit_in_args(&raw_args, "--no-live-reload")
    {
        return Some(
            "error: --no-live-reload requires --preview-pages <dir> or --pages".to_string(),
        );
    }

    None
}

fn feedback_project_dir(cli: &Cli) -> PathBuf {
    project_dir_for_load_target(cli).unwrap_or_else(|_| {
        cli.repo_path
            .clone()
            .or_else(|| cli.workspace.clone())
            .or_else(|| {
                cli.beads_file.as_ref().map(|path| {
                    let parent = path.parent().unwrap_or(path);
                    if parent.file_name().is_some_and(|name| name == ".beads") {
                        parent
                            .parent()
                            .map_or_else(|| parent.to_path_buf(), Path::to_path_buf)
                    } else {
                        parent.to_path_buf()
                    }
                })
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    })
}

fn absolute_from_current_dir(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn project_dir_for_load_target(cli: &Cli) -> bvr::Result<PathBuf> {
    match resolve_issue_load_target(cli)? {
        IssueLoadTarget::BeadsFile(path) => {
            let parent = path.parent().unwrap_or(path.as_path());
            let project_dir = if parent.file_name().is_some_and(|name| name == ".beads") {
                parent
                    .parent()
                    .map_or_else(|| parent.to_path_buf(), Path::to_path_buf)
            } else {
                parent.to_path_buf()
            };
            Ok(absolute_from_current_dir(&project_dir))
        }
        IssueLoadTarget::WorkspaceConfig(path) => {
            let project_dir = path.parent().and_then(Path::parent).map_or_else(
                || {
                    path.parent()
                        .unwrap_or_else(|| Path::new("."))
                        .to_path_buf()
                },
                Path::to_path_buf,
            );
            Ok(absolute_from_current_dir(&project_dir))
        }
        IssueLoadTarget::RepoPath(Some(path)) => Ok(absolute_from_current_dir(&path)),
        IssueLoadTarget::RepoPath(None) => Ok(std::env::current_dir()?),
    }
}

fn project_dir_for_export_hooks(cli: &Cli) -> bvr::Result<PathBuf> {
    project_dir_for_load_target(cli)
}

fn resolve_cli_path_from_project_dir(cli: &Cli, path: &Path) -> bvr::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(project_dir_for_load_target(cli)?.join(path))
    }
}

fn load_sprints_for_cli(cli: &Cli) -> bvr::Result<Vec<bvr::model::Sprint>> {
    let project_dir = project_dir_for_load_target(cli)?;
    loader::load_sprints(Some(&project_dir))
}

fn main() -> ExitCode {
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .with_level(false)
        .without_time()
        .try_init()
    {
        eprintln!("warning: tracing init failed: {error}");
    }

    let mut cli = Cli::parse();

    cli.format = match cli.resolve_output_format() {
        Ok(format) => format,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    cli.stats = cli.resolve_stats_flag();

    // Auto-enable --robot-diff when --diff-since is used in non-interactive context
    // (piped output or BV_ROBOT=1), matching Go behavior
    if cli.diff_since.is_some()
        && !cli.robot_diff
        && (bvr::loader::is_robot_mode() || !std::io::stdout().is_terminal())
    {
        cli.robot_diff = true;
    }

    bvr::loader::set_robot_warning_suppression(cli.is_robot_command());

    // --no-cache: silently accepted for Go CLI compatibility (Rust port has no disk cache layer).

    // --db: legacy compatibility alias for --beads-file.
    if let Some(ref db_path) = cli.db {
        if cli.beads_file.is_none() {
            let resolved = if db_path.is_absolute() {
                db_path.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(db_path)
            };
            cli.beads_file = Some(resolved);
        }
    }

    if cli.version {
        print_version();
        return ExitCode::SUCCESS;
    }

    if cli.background_mode && cli.no_background_mode {
        eprintln!("error: --background-mode and --no-background-mode are mutually exclusive");
        return ExitCode::from(2);
    }

    if let Some(error) = validate_orphaned_modifier_flags(&cli) {
        eprintln!("{error}");
        return ExitCode::from(2);
    }

    // --robot-schema and --robot-docs don't need issues loaded
    if cli.robot_schema {
        let schemas = generate_robot_schemas();

        if let Some(cmd) = cli.schema_command.as_deref() {
            if let Some(command_key) = resolve_schema_command_key(&schemas, cmd) {
                let schema = schemas
                    .commands
                    .get(&command_key)
                    .expect("resolved schema command must exist");
                let single = serde_json::json!({
                    "schema_version": schemas.schema_version,
                    "generated_at": schemas.generated_at,
                    "command": command_key,
                    "schema": schema,
                });
                if let Err(error) = emit_with_stats(cli.format, &single, cli.stats) {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
                return ExitCode::SUCCESS;
            }
            eprintln!("Unknown command: {cmd}");
            eprintln!("Available commands:");
            for name in schemas.commands.keys() {
                eprintln!("  {name}");
            }
            return ExitCode::from(1);
        }

        if let Err(error) = emit_with_stats(cli.format, &schemas, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if let Some(topic) = cli.robot_docs.as_deref() {
        let docs = generate_robot_docs(topic);
        if let Err(error) = emit_with_stats(cli.format, &docs, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        if docs.get("error").is_some() {
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }

    // Operational/admin command surface compatibility.
    if cli.is_operational_command() {
        let outcome = handle_operational_commands(&cli);
        if outcome.to_stderr {
            eprintln!("{}", outcome.message);
        } else {
            println!("{}", outcome.message);
        }
        return outcome.exit_code;
    }

    // --robot-recipes doesn't need issues loaded
    if cli.robot_recipes {
        let recipes = bvr::analysis::recipe::list_recipes();
        let output = bvr::analysis::recipe::RobotRecipesOutput {
            envelope: envelope_empty(),
            recipes,
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    // --feedback-show and --feedback-reset don't need issues loaded
    if cli.feedback_show || cli.feedback_reset {
        let work_dir = feedback_project_dir(&cli);

        if cli.feedback_reset {
            let mut feedback = bvr::analysis::recipe::FeedbackData::load(&work_dir);
            feedback.reset();
            if let Err(error) = feedback.save(&work_dir) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            println!("Feedback data reset successfully.");
            return ExitCode::SUCCESS;
        }

        let feedback = bvr::analysis::recipe::FeedbackData::load(&work_dir);
        let output = bvr::analysis::recipe::RobotFeedbackOutput {
            envelope: envelope_empty(),
            stats: feedback.stats(),
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    // --agents-* commands don't need issues loaded
    if cli.is_agents_command() {
        let agents_action_count = usize::from(cli.agents_check)
            + usize::from(cli.agents_add)
            + usize::from(cli.agents_update)
            + usize::from(cli.agents_remove);
        if agents_action_count > 1 {
            eprintln!(
                "error: only one of --agents-check/--agents-add/--agents-update/--agents-remove may be used at a time.\n\
                 Remediation: rerun with exactly one action flag (or only --agents-dry-run/--agents-force for default check mode)."
            );
            return ExitCode::from(2);
        }

        let work_dir = project_dir_for_load_target(&cli)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let result = if cli.agents_add {
            bvr::agents::agents_add(&work_dir, cli.agents_dry_run)
        } else if cli.agents_update {
            bvr::agents::agents_update(&work_dir, cli.agents_dry_run)
        } else if cli.agents_remove {
            bvr::agents::agents_remove(&work_dir, cli.agents_dry_run)
        } else {
            // Legacy parity: with only --agents-dry-run/--agents-force set, default to status check.
            Ok(bvr::agents::agents_check(&work_dir))
        };

        match result {
            Ok(r) => {
                println!("{}", r.message);
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    }

    // --pages wizard and --preview-pages do not require loading issues.
    if cli.pages {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            // Non-interactive mode: just print help
            bvr::export_pages::print_pages_wizard();
            return ExitCode::SUCCESS;
        }
        let beads_path = cli.beads_file.clone();
        let no_hooks = cli.no_hooks;
        let no_live_reload = cli.no_live_reload;
        let stdin = std::io::stdin();
        let mut reader = std::io::BufReader::new(stdin.lock());
        let mut writer = std::io::stderr();
        let saved_config = bvr::pages_wizard::load_wizard_config().ok().flatten();
        match bvr::pages_wizard::run_wizard_interactive(
            &mut reader,
            &mut writer,
            beads_path,
            saved_config,
            |config| {
                let output = config
                    .output_path
                    .as_deref()
                    .unwrap_or(Path::new("./bv-pages"));
                let issues = load_issues(&cli)?;
                let options = bvr::export_pages::ExportPagesOptions {
                    title: config.title.clone(),
                    subtitle: config.subtitle.clone(),
                    include_closed: config.include_closed,
                    include_history: config.include_history,
                };
                let count = count_pages_export_issues(&issues, &options);
                let hook_project_dir = project_dir_for_export_hooks(&cli)?;
                bvr::export_md::run_export_with_hooks(
                    output,
                    "html",
                    count,
                    no_hooks,
                    Some(hook_project_dir.as_path()),
                    |resolved_output| {
                        bvr::export_pages::export_pages_bundle(&issues, resolved_output, &options)
                    },
                )?;
                Ok(())
            },
            |path| {
                let resolved_preview_path = resolve_cli_path_from_project_dir(&cli, path)?;
                bvr::export_pages::run_preview_server(&resolved_preview_path, !no_live_reload)
            },
        ) {
            Ok(Some(_config)) => return ExitCode::SUCCESS,
            Ok(None) => {
                eprintln!("Wizard cancelled.");
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if let Some(bundle_path) = cli.preview_pages.as_deref() {
        let resolved_bundle_path = match resolve_cli_path_from_project_dir(&cli, bundle_path) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        if let Err(error) =
            bvr::export_pages::run_preview_server(&resolved_bundle_path, !cli.no_live_reload)
        {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if cli.watch_export && cli.export_pages.is_none() {
        eprintln!("error: --watch-export requires --export-pages <dir>");
        return ExitCode::from(2);
    }

    // --baseline-info doesn't need issues loaded — just reads the saved baseline file.
    if cli.baseline_info {
        let project_dir = match project_dir_for_load_target(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        match bvr::analysis::drift::Baseline::load(&project_dir) {
            Ok(bl) => {
                println!("Baseline info:");
                println!("  Created: {}", bl.created_at);
                if !bl.description.is_empty() {
                    println!("  Description: {}", bl.description);
                }
                println!("  Nodes: {}", bl.stats.node_count);
                println!("  Edges: {}", bl.stats.edge_count);
                println!("  Open: {}", bl.stats.open_count);
                println!("  Closed: {}", bl.stats.closed_count);
                println!("  Blocked: {}", bl.stats.blocked_count);
                println!("  Actionable: {}", bl.stats.actionable_count);
                println!("  Cycles: {}", bl.stats.cycle_count);
                println!("  Density: {:.4}", bl.stats.density);
                return ExitCode::SUCCESS;
            }
            Err(_) => {
                println!("No baseline found. Run --save-baseline to create one.");
                return ExitCode::SUCCESS;
            }
        }
    }

    let load_start = std::time::Instant::now();
    let mut issues = match load_issues(&cli) {
        Ok(issues) => issues,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };
    let load_duration = load_start.elapsed();

    if let Some(repo_filter) = cli.repo.as_deref() {
        issues = filter_by_repo(issues, repo_filter);
    }

    let build_start = std::time::Instant::now();
    let analysis_config = analysis_config_for_cli(&cli);
    let mut analyzer = Analyzer::new_with_config(issues, &analysis_config);
    let build_duration = build_start.elapsed();

    if cli.robot_help {
        print_robot_help();
        return ExitCode::SUCCESS;
    }

    let (as_of, as_of_commit) = resolve_as_of(&cli);

    // When --label is specified, scope the analysis to the label's subgraph.
    // This matches the Go tool's ComputeLabelSubgraph behavior: include issues
    // with the label plus their direct dependencies, then rerun analysis.
    if let Some(ref label) = cli.label {
        let subgraph = bvr::analysis::label_intel::compute_label_subgraph(&analyzer.issues, label);
        if subgraph.is_empty() {
            eprintln!("warning: no issues found with label {label:?}");
        }
        analyzer = Analyzer::new_with_config(subgraph, &analysis_config);
    }

    let issues = &analyzer.issues;

    let (label_scope, label_context) = if let Some(ref label) = cli.label {
        let health = bvr::analysis::label_intel::compute_single_label_health(
            label,
            issues,
            &analyzer.metrics,
        );
        (Some(label.clone()), Some(health))
    } else {
        (None, None)
    };

    // Load feedback adjustments for triage scoring (persisted from --feedback-accept/ignore).
    let feedback_data = {
        let work_dir = feedback_project_dir(&cli);
        bvr::analysis::recipe::FeedbackData::load(&work_dir)
    };
    let mut feedback_weight_adjustments = feedback_data.weight_adjustment_map();

    // Merge --weight-preset adjustments with feedback adjustments (preset first, feedback on top).
    if let Some(preset_name) = &cli.weight_preset {
        if let Some(preset) = bvr::analysis::triage::WeightPreset::from_name(preset_name) {
            for (key, value) in preset.adjustments() {
                feedback_weight_adjustments
                    .entry(key)
                    .and_modify(|existing| *existing *= value)
                    .or_insert(value);
            }
        } else {
            eprintln!(
                "warning: unknown weight preset {preset_name:?}, using default. Available: {}",
                bvr::analysis::triage::WeightPreset::ALL.join(", ")
            );
        }
    }

    if cli.robot_next
        || cli.robot_overview
        || cli.robot_triage
        || cli.robot_triage_by_track
        || cli.robot_triage_by_label
    {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: cli.robot_triage_by_track,
            group_by_label: cli.robot_triage_by_label || cli.robot_overview,
            max_recommendations: cli.robot_max_results.max(10),
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });

        if cli.robot_next {
            let env = envelope(&issues);
            let result = if let Some(top) = triage.result.quick_ref.top_picks.first() {
                RobotNextOutput {
                    envelope: env,
                    as_of: as_of.clone(),
                    as_of_commit: as_of_commit.clone(),
                    id: Some(top.id.clone()),
                    title: Some(top.title.clone()),
                    score: Some(top.score),
                    reasons: top.reasons.clone(),
                    unblocks: Some(top.unblocks),
                    claim_command: Some(format!("br update {} --status=in_progress", top.id)),
                    show_command: Some(format!("br show {}", top.id)),
                    message: None,
                }
            } else {
                RobotNextOutput {
                    envelope: env,
                    as_of: as_of.clone(),
                    as_of_commit: as_of_commit.clone(),
                    id: None,
                    title: None,
                    score: None,
                    reasons: Vec::new(),
                    unblocks: None,
                    claim_command: None,
                    show_command: None,
                    message: Some("No actionable items available".to_string()),
                }
            };

            if let Err(error) = emit_with_stats(cli.format, &result, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if cli.robot_overview {
            let output = build_robot_overview_output(&issues, &analyzer, &triage.result);
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        let feedback_stats = {
            let stats = feedback_data.stats();
            if stats.total_accepted > 0 || stats.total_ignored > 0 {
                Some(stats)
            } else {
                None
            }
        };
        let output = RobotTriageOutput {
            envelope: envelope(&issues),
            as_of: as_of.clone(),
            as_of_commit: as_of_commit.clone(),
            triage: triage.result,
            feedback: feedback_stats,
            usage_hints: vec![
                "jq '.triage.quick_ref.top_picks[:3]'".to_string(),
                "jq '.triage.blockers_to_clear | map(.id)'".to_string(),
                "jq '.triage.quick_wins | map({id,score})'".to_string(),
                "bvr --robot-next".to_string(),
            ],
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_plan {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 200,
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });
        let plan = analyzer.plan(&triage.score_by_id);

        let output = RobotPlanOutput {
            envelope: envelope(&issues),
            as_of: as_of.clone(),
            as_of_commit: as_of_commit.clone(),
            label_scope: label_scope.clone(),
            label_context: label_context.clone(),
            status: MetricStatus::computed(),
            analysis_config: analyzer.metrics.config.clone(),
            plan,
            usage_hints: vec![
                "jq '.plan.summary'".to_string(),
                "jq '.plan.tracks[].items[] | select(.unblocks | length > 0)'".to_string(),
            ],
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_insights {
        let insights = analyzer.insights_with_limit(cli.insight_limit.max(1));
        let full_stats = if cli.robot_full_stats {
            Some(build_full_stats(&analyzer.metrics))
        } else {
            None
        };
        let top_what_ifs = Some(analyzer.top_what_ifs(5));
        let advanced_insights = Some(analyzer.advanced_insights());
        let output = RobotInsightsOutput {
            envelope: envelope(&issues),
            as_of: as_of.clone(),
            as_of_commit: as_of_commit.clone(),
            label_scope: label_scope.clone(),
            label_context: label_context.clone(),
            analysis_config: analyzer.metrics.config.clone(),
            analysis_config_compat: analyzer.metrics.config.clone(),
            insights,
            full_stats,
            top_what_ifs,
            advanced_insights,
            usage_hints: vec![
                "jq '.Bottlenecks[:5]'".to_string(),
                "jq '.Cycles'".to_string(),
                "jq '.CriticalPath[:10]'".to_string(),
                "jq '.Keystones'".to_string(),
                "jq '.Velocity'".to_string(),
            ],
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_priority {
        let recommendations = analyzer.priority(
            cli.robot_min_confidence,
            cli.robot_max_results,
            cli.robot_by_label.as_deref(),
            cli.robot_by_assignee.as_deref(),
        );

        let high_confidence = recommendations
            .iter()
            .filter(|rec| rec.confidence >= 0.7)
            .count();

        let output = RobotPriorityOutput {
            envelope: envelope(&issues),
            as_of: as_of.clone(),
            as_of_commit: as_of_commit.clone(),
            label_scope: label_scope.clone(),
            label_context: label_context.clone(),
            status: MetricStatus::computed(),
            analysis_config: analyzer.metrics.config.clone(),
            recommendations,
            field_descriptions: default_field_descriptions(),
            filters: PriorityFilterOutput {
                min_confidence: cli.robot_min_confidence,
                max_results: cli.robot_max_results,
                by_label: cli.robot_by_label,
                by_assignee: cli.robot_by_assignee,
            },
            summary: PrioritySummaryOutput {
                total_issues: issues.len(),
                recommendations: analyzer
                    .priority(0.0, cli.robot_max_results.max(50), None, None)
                    .len(),
                high_confidence,
            },
            usage_hints: vec![
                "jq '.recommendations[] | select(.confidence > 0.7)'".to_string(),
                "jq '.recommendations | map({id,score,unblocks})'".to_string(),
            ],
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_economics {
        let overlay = match load_economics_overlay(cli.economics_overlay.as_deref()) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let title_by_id: std::collections::BTreeMap<&str, &str> = issues
            .iter()
            .map(|issue| (issue.id.as_str(), issue.title.as_str()))
            .collect();
        let bottlenecks = bvr::analysis::economics::bottlenecks_from_blocks_count(
            &analyzer.metrics.blocks_count,
            &title_by_id,
            cli.insight_limit.max(1),
        );
        let output = bvr::analysis::economics::compute_economics(
            bvr::analysis::economics::EconomicsComputation {
                issues: &issues,
                overlay: &overlay,
                bottlenecks: &bottlenecks,
                now: Utc::now(),
                cost_of_delay_limit: cli.insight_limit.max(1),
            },
        );

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_delivery {
        // Build the set of issue IDs that have at least one open blocker, so
        // delivery's milestone_pressure.is_blocked flag is coherent with the
        // --robot-alerts / --robot-plan blocker sense.
        let blocked_ids: std::collections::HashSet<String> = issues
            .iter()
            .filter(|issue| issue.is_open_like())
            .filter_map(|issue| {
                let blockers = analyzer.graph.blockers(&issue.id);
                if blockers.is_empty() {
                    None
                } else {
                    Some(issue.id.clone())
                }
            })
            .collect();
        let output = bvr::analysis::delivery::compute_delivery(
            bvr::analysis::delivery::DeliveryComputation {
                issues: &issues,
                blocked_ids: &blocked_ids,
                now: Utc::now(),
                milestone_pressure_limit: cli.insight_limit.max(1),
            },
        );

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_alerts {
        let output = analyzer.alerts(&AlertOptions {
            severity: cli
                .severity
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            alert_type: cli
                .alert_type
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            alert_label: cli
                .alert_label
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            ..AlertOptions::default()
        });

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_suggest {
        let filter_type = match parse_suggest_type(cli.suggest_type.as_deref()) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        };

        let options = SuggestOptions {
            min_confidence: cli.suggest_confidence.max(0.0),
            max_suggestions: cli.robot_max_results.max(50),
            filter_type,
            filter_bead: cli.suggest_bead.clone(),
        };
        let output = analyzer.suggest(&options);

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_graph {
        let output = build_robot_graph_output(&issues, &analyzer, &cli, None);
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if let Some(export_path) = cli.export_graph.as_deref() {
        let export_target = resolve_graph_export_target(export_path, cli.graph_format);

        match export_target {
            GraphExportTarget::Text(format) => {
                let output = build_robot_graph_output(&issues, &analyzer, &cli, Some(format));
                if let Err(error) = write_graph_export_snapshot(
                    export_path,
                    &output,
                    cli.graph_title.as_deref(),
                    cli.graph_preset,
                    cli.graph_style,
                ) {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            }
            GraphExportTarget::Static(format) => {
                let graph_data = build_graph_export_data(&issues, &analyzer, &cli);
                if let Err(error) = write_static_graph_export_snapshot(
                    export_path,
                    format,
                    &graph_data,
                    cli.graph_title.as_deref(),
                    cli.graph_preset,
                    cli.graph_style,
                ) {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            }
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_diff {
        let Some(diff_since) = cli.diff_since.as_deref() else {
            eprintln!("error: --robot-diff requires --diff-since <path|git-ref>");
            return ExitCode::from(2);
        };

        let before_issues = match load_issues_for_diff(&cli, diff_since) {
            Ok(issues) => issues,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

        let resolved_revision = resolve_diff_revision(&cli, diff_since);
        let diff = bvr::analysis::diff::compare_snapshots_with_metadata(
            &before_issues,
            &issues,
            &bvr::analysis::diff::DiffMetadata {
                from_timestamp: "0001-01-01T00:00:00Z".to_string(),
                to_timestamp: Local::now().to_rfc3339(),
                from_revision: Some(resolved_revision.clone()),
                to_revision: None,
            },
        );
        let output = RobotDiffOutput {
            envelope: envelope(&issues),
            resolved_revision,
            from_data_hash: compute_data_hash(&before_issues),
            to_data_hash: compute_data_hash(&issues),
            diff,
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_history || cli.bead_history.is_some() {
        let output = match build_robot_history_output(&cli, &issues, &analyzer) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if let Some(target_sprint) = cli.robot_burndown.as_deref() {
        let output = match build_robot_burndown_output(&cli, &issues, target_sprint) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_capacity {
        let output = build_robot_capacity_output(&issues, &cli);

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if let Some(target) = cli.robot_forecast.as_deref() {
        let agents = cli.forecast_agents.max(1);
        let target_all = target.eq_ignore_ascii_case("all");
        let sprint_bead_ids = if let Some(sprint_id) = cli.forecast_sprint.as_deref() {
            match resolve_forecast_sprint_beads(&cli, sprint_id) {
                Ok(ids) => Some(ids),
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            }
        } else {
            None
        };
        let forecast = analyzer.forecast(
            target,
            if target_all {
                cli.forecast_label.as_deref()
            } else {
                None
            },
            agents,
        );

        let mut filters = BTreeMap::<String, String>::new();
        if let Some(label) = cli.forecast_label.as_ref() {
            filters.insert("label".to_string(), label.clone());
        }
        if let Some(sprint) = cli.forecast_sprint.as_ref() {
            filters.insert("sprint".to_string(), sprint.clone());
        }

        let mut forecasts = Vec::with_capacity(forecast.forecasts.len());
        for item in &forecast.forecasts {
            if target_all
                && sprint_bead_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&item.id))
            {
                continue;
            }

            forecasts.push(RobotForecastItem {
                issue_id: item.id.clone(),
                estimated_minutes: item.eta_minutes,
                estimated_days: item.estimated_days,
                eta_date: item.eta_date.clone(),
                eta_date_low: item.eta_date_low.clone(),
                eta_date_high: item.eta_date_high.clone(),
                confidence: item.confidence,
                velocity_minutes_per_day: item.velocity_minutes_per_day,
                agents,
                factors: item.factors.clone(),
            });
        }

        let summary = if forecasts.len() > 1 {
            let total_minutes = forecasts
                .iter()
                .map(|item| item.estimated_minutes)
                .sum::<i64>();
            let total_minutes_i32 = i32::try_from(total_minutes).unwrap_or(i32::MAX);
            let total_days = f64::from(total_minutes_i32) / (60.0 * 8.0);
            let len_u32 = u32::try_from(forecasts.len()).unwrap_or(u32::MAX);
            let avg_confidence =
                forecasts.iter().map(|item| item.confidence).sum::<f64>() / f64::from(len_u32);

            let earliest_eta = forecasts
                .iter()
                .map(|item| item.eta_date.clone())
                .min()
                .unwrap_or_default();
            let latest_eta = forecasts
                .iter()
                .map(|item| item.eta_date.clone())
                .max()
                .unwrap_or_default();

            Some(RobotForecastSummary {
                total_minutes,
                total_days,
                avg_confidence,
                earliest_eta,
                latest_eta,
            })
        } else {
            None
        };

        let output = RobotForecastOutput {
            envelope: envelope(&issues),
            agents,
            filters,
            forecast_count: forecasts.len(),
            forecasts,
            summary,
        };

        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_sprint_list || cli.robot_sprint_show.is_some() {
        let sprints = match load_sprints_for_cli(&cli) {
            Ok(sprints) => sprints,
            Err(bvr::BvrError::MissingBeadsDir(_)) if cli.robot_sprint_list => Vec::new(),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

        if let Some(sprint_id) = cli.robot_sprint_show.as_deref() {
            if let Some(sprint) = sprints.iter().find(|s| s.id == sprint_id) {
                let output = RobotSprintShowOutput {
                    envelope: envelope(&issues),
                    sprint: sprint.clone(),
                };
                if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            } else {
                eprintln!("Sprint not found: {sprint_id}");
                return ExitCode::from(1);
            }
        } else {
            let output = RobotSprintListOutput {
                envelope: envelope(&issues),
                sprint_count: sprints.len(),
                sprints,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_metrics {
        let output = RobotMetricsOutput {
            envelope: envelope(&issues),
            timing: Vec::new(),
            cache: Vec::new(),
            memory: MetricsMemory::current(),
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if cli.profile_startup {
        let triage_start = std::time::Instant::now();
        let triage = analyzer.triage(TriageOptions {
            group_by_track: true,
            group_by_label: true,
            max_recommendations: 50,
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });
        let triage_duration = triage_start.elapsed();

        let insights_start = std::time::Instant::now();
        let insights = analyzer.insights();
        let insights_duration = insights_start.elapsed();

        let total = load_duration + build_duration + triage_duration + insights_duration;
        let issue_count = issues.len();
        let edge_count: usize = issues.iter().map(|i| i.dependencies.len()).sum();
        let issue_count_f64 = f64::from(u32::try_from(issue_count).unwrap_or(u32::MAX));
        let edge_count_f64 = f64::from(u32::try_from(edge_count).unwrap_or(u32::MAX));
        let density = if issue_count > 1 {
            edge_count_f64 / (issue_count_f64 * (issue_count_f64 - 1.0))
        } else {
            0.0
        };

        let profile = StartupProfile {
            node_count: issue_count,
            edge_count,
            density,
            load_jsonl: format_duration_ms(load_duration),
            build_graph: format_duration_ms(build_duration),
            triage: format_duration_ms(triage_duration),
            insights: format_duration_ms(insights_duration),
            total: format_duration_ms(total),
            cycle_count: insights.cycles.len(),
            bottleneck_count: insights.bottlenecks.len(),
            recommendation_count: triage.result.recommendations.len(),
        };

        let recommendations = generate_profile_recommendations(&profile, total);

        if cli.profile_json {
            let output = ProfileJsonOutput {
                envelope: envelope(&issues),
                profile,
                total_with_load: format_duration_ms(total),
                recommendations,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        } else {
            print_profile_report(&profile, &recommendations);
        }
        return ExitCode::SUCCESS;
    }

    if cli.robot_label_health {
        let generated_at = chrono::Utc::now().to_rfc3339();
        let output = RobotLabelHealthOutput {
            envelope: envelope(&issues),
            analysis_config: analyzer.metrics.config.clone(),
            results: RobotLabelHealthResultsOutput {
                generated_at,
                result: bvr::analysis::label_intel::compute_all_label_health(
                    &issues,
                    &analyzer.graph,
                    &analyzer.metrics,
                ),
            },
            usage_hints: vec![
                "jq '.results.summaries | sort_by(.health) | .[:3]' - lowest-health labels"
                    .to_string(),
                "jq '.results.labels[] | select(.health_level == \"critical\")' - critical label details"
                    .to_string(),
                "jq '.results.attention_needed' - labels needing attention".to_string(),
            ],
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if cli.robot_label_flow {
        let output = RobotLabelFlowOutput {
            envelope: envelope(&issues),
            analysis_config: analyzer.metrics.config.clone(),
            flow: bvr::analysis::label_intel::compute_cross_label_flow(&issues),
            usage_hints: vec![
                "jq '.flow.bottleneck_labels' - labels acting as bottlenecks".to_string(),
                "jq '.flow.dependencies[:10]' - first dependency edges".to_string(),
            ],
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if cli.robot_label_attention {
        let limit = cli.attention_limit;
        let result =
            bvr::analysis::label_intel::compute_label_attention(&issues, &analyzer.metrics, limit);
        let output = RobotLabelAttentionOutput {
            envelope: envelope(&issues),
            limit,
            total_labels: result.total_labels,
            labels: result.labels.into_iter().map(Into::into).collect(),
            usage_hints: vec![
                "jq '.labels[0]' - top attention label details".to_string(),
                "jq '.labels[] | select(.blocked_count > 0)' - labels with blocked issues"
                    .to_string(),
                "jq '.labels[] | {label:.label,score:.attention_score,reason:.reason}'".to_string(),
            ],
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    // ---- Correlation audit commands ----
    if cli.robot_explain_correlation.is_some()
        || cli.robot_confirm_correlation.is_some()
        || cli.robot_reject_correlation.is_some()
        || cli.robot_correlation_stats
    {
        let repo_root = feedback_project_dir(&cli);
        let feedback_path = bvr::analysis::correlation::default_feedback_path(&repo_root);

        if cli.robot_correlation_stats {
            let store = match bvr::analysis::correlation::FeedbackStore::open(&feedback_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            let output = bvr::analysis::correlation::RobotCorrelationStatsOutput {
                envelope: envelope(&issues),
                stats: store.stats(),
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        // explain / confirm / reject all need SHA:beadID parsing + history context
        let raw_arg = cli
            .robot_explain_correlation
            .as_deref()
            .or(cli.robot_confirm_correlation.as_deref())
            .or(cli.robot_reject_correlation.as_deref())
            .unwrap_or_default();
        if raw_arg.is_empty() {
            eprintln!("error: missing correlation target argument (expected SHA:BEAD_ID)");
            return ExitCode::from(1);
        }

        let (commit_sha, bead_id) = match bvr::analysis::correlation::parse_correlation_arg(raw_arg)
        {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        };

        // Build history to find the correlated commit
        let history_output = match build_robot_history_output(&cli, &issues, &analyzer) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        };

        // Search for the commit in the history data
        let commit_entry = history_output
            .histories_map
            .values()
            .filter(|h| h.bead_id == bead_id)
            .flat_map(|h| h.commits.as_deref().unwrap_or_default())
            .find(|c| c.sha.starts_with(&commit_sha) || commit_sha.starts_with(&c.sha));

        let Some(commit_entry) = commit_entry else {
            eprintln!("error: no correlation found for commit {commit_sha} and bead {bead_id}");
            return ExitCode::from(1);
        };

        let mut store = match bvr::analysis::correlation::FeedbackStore::open(&feedback_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        };

        if cli.robot_explain_correlation.is_some() {
            let existing = store.get(&commit_entry.sha, &bead_id);
            let explanation =
                bvr::analysis::correlation::build_explanation(commit_entry, &bead_id, existing);
            let output = bvr::analysis::correlation::RobotExplainOutput {
                envelope: envelope(&issues),
                explanation,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        } else {
            let by = cli.correlation_by.as_deref().unwrap_or("cli");
            let reason = cli.correlation_reason.as_deref().unwrap_or("");

            let (status, feedback) = if cli.robot_confirm_correlation.is_some() {
                let fb = match store.confirm(
                    &commit_entry.sha,
                    &bead_id,
                    by,
                    commit_entry.confidence,
                    reason,
                ) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(1);
                    }
                };
                ("confirmed", fb)
            } else {
                let fb = match store.reject(
                    &commit_entry.sha,
                    &bead_id,
                    by,
                    commit_entry.confidence,
                    reason,
                ) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::from(1);
                    }
                };
                ("rejected", fb)
            };

            let output = bvr::analysis::correlation::RobotCorrelationActionOutput {
                status: status.to_string(),
                commit: feedback.commit_sha,
                bead: feedback.bead_id,
                by: feedback.feedback_by,
                reason: feedback.reason,
                orig_conf: feedback.original_conf,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }

        return ExitCode::SUCCESS;
    }

    // ---- File intelligence commands (orphans, file-beads, hotspots, impact, relations, related) ----
    if cli.robot_orphans
        || cli.robot_file_beads.is_some()
        || cli.robot_file_hotspots
        || cli.robot_impact.is_some()
        || cli.robot_file_relations.is_some()
        || cli.robot_related.is_some()
    {
        let history_output = match build_robot_history_output(&cli, &issues, &analyzer) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        };

        if cli.robot_orphans {
            let repo_root = resolve_repo_root(&cli).unwrap_or_else(|| PathBuf::from("."));
            let all_commits = match bvr::analysis::git_history::load_git_commits(
                &repo_root,
                cli.history_limit,
                cli.history_since.as_deref(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };

            let report = bvr::analysis::file_intel::detect_orphans(
                &all_commits,
                &history_output.histories_map,
                &history_output.commit_index,
                cli.orphans_min_score,
            );
            let output = bvr::analysis::file_intel::RobotOrphansOutput {
                envelope: envelope(&issues),
                report,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref path) = cli.robot_file_beads {
            let result = bvr::analysis::file_intel::lookup_file_beads(
                path,
                &history_output.histories_map,
                cli.file_beads_limit,
            );
            let output = bvr::analysis::file_intel::RobotFileBeadsOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if cli.robot_file_hotspots {
            let hotspots = bvr::analysis::file_intel::compute_hotspots(
                &history_output.histories_map,
                cli.hotspots_limit,
            );
            let stats =
                bvr::analysis::file_intel::compute_file_index_stats(&history_output.histories_map);
            let output = bvr::analysis::file_intel::RobotFileHotspotsOutput {
                envelope: envelope(&issues),
                hotspots,
                stats,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref paths_str) = cli.robot_impact {
            let file_paths: Vec<String> =
                paths_str.split(',').map(|s| s.trim().to_owned()).collect();
            let result = bvr::analysis::file_intel::analyze_impact(
                &file_paths,
                &history_output.histories_map,
            );
            let output = bvr::analysis::file_intel::RobotImpactOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref path) = cli.robot_file_relations {
            let result = bvr::analysis::file_intel::compute_file_relations(
                path,
                &history_output.histories_map,
                cli.relations_threshold,
                cli.relations_limit,
            );
            let output = bvr::analysis::file_intel::RobotFileRelationsOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref bead_id) = cli.robot_related {
            let result = compute_related_work_result(
                bead_id,
                &history_output.histories_map,
                cli.related_min_relevance,
                cli.related_max_results,
                cli.related_include_closed,
            );
            let output = bvr::analysis::file_intel::RobotRelatedWorkOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }
    }

    // ---- Causal network commands (blocker-chain, impact-network, causality) ----
    if cli.robot_blocker_chain.is_some()
        || cli.robot_impact_network.is_some()
        || cli.robot_causality.is_some()
    {
        if let Some(ref target_id) = cli.robot_blocker_chain {
            let result = bvr::analysis::causal::get_blocker_chain(&analyzer.graph, target_id);
            let output = bvr::analysis::causal::RobotBlockerChainOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref bead_id) = cli.robot_impact_network {
            let history_output = match build_robot_history_output(&cli, &issues, &analyzer) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            let result = bvr::analysis::causal::build_impact_network_result(
                &analyzer.graph,
                &history_output.histories_map,
                bead_id,
                cli.network_depth,
            );
            let output = bvr::analysis::causal::RobotImpactNetworkOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        if let Some(ref bead_id) = cli.robot_causality {
            let history_output = match build_robot_history_output(&cli, &issues, &analyzer) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            };
            let result = bvr::analysis::causal::build_causality_chain(
                bead_id,
                &history_output.histories_map,
                &analyzer.graph,
            );
            let output = bvr::analysis::causal::RobotCausalityOutput {
                envelope: envelope(&issues),
                result,
            };
            if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }
    }

    // ---- Drift commands (save-baseline, robot-drift) ----
    let is_multi_repo = matches!(resolve_issue_load_target(&cli), Ok(IssueLoadTarget::WorkspaceConfig(ref p)) if {
        loader::load_workspace_config(p)
            .map(|c| c.repos.iter().filter(|r| r.enabled.unwrap_or(true)).count() > 1)
            .unwrap_or(false)
    });

    if let Some(ref description) = cli.save_baseline {
        if is_multi_repo {
            eprintln!(
                "warning: baselines are not fully supported for multi-repo workspaces. \
                 Issue IDs may not be namespaced correctly. Consider saving baselines \
                 per-repo using --beads-file instead."
            );
        }
        let project_dir = match project_dir_for_load_target(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        let baseline = bvr::analysis::drift::Baseline::from_current(
            &issues,
            &analyzer.graph,
            &analyzer.metrics,
            description,
        );
        match baseline.save(&project_dir) {
            Ok(path) => {
                eprintln!("Baseline saved to {}", path.display());
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if cli.check_drift {
        if is_multi_repo {
            eprintln!("warning: drift detection is not fully supported for multi-repo workspaces.");
        }
        let project_dir = match project_dir_for_load_target(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                eprintln!("hint: run --save-baseline first to create a baseline");
                return ExitCode::from(1);
            }
        };
        let baseline = match bvr::analysis::drift::Baseline::load(&project_dir) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!("hint: run --save-baseline first to create a baseline");
                return ExitCode::from(1);
            }
        };
        let result = bvr::analysis::drift::compute_drift(
            &baseline,
            &issues,
            &analyzer.graph,
            &analyzer.metrics,
        );
        if !result.has_drift {
            println!("No drift detected.");
        } else {
            println!(
                "Drift detected: {} critical, {} warning, {} info",
                result.summary.critical, result.summary.warning, result.summary.info
            );
            for alert in &result.alerts {
                println!(
                    "  [{}] {}: {}",
                    alert.severity.to_uppercase(),
                    alert.alert_type,
                    alert.message
                );
            }
        }
        return ExitCode::from(result.exit_code);
    }

    if cli.robot_drift {
        if is_multi_repo {
            eprintln!("warning: drift detection is not fully supported for multi-repo workspaces.");
        }
        let project_dir = match project_dir_for_load_target(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                eprintln!("hint: run --save-baseline first to create a baseline");
                return ExitCode::from(1);
            }
        };
        let baseline = match bvr::analysis::drift::Baseline::load(&project_dir) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!("hint: run --save-baseline first to create a baseline");
                return ExitCode::from(1);
            }
        };
        let result = bvr::analysis::drift::compute_drift(
            &baseline,
            &issues,
            &analyzer.graph,
            &analyzer.metrics,
        );
        let exit_code = result.exit_code;
        let output = bvr::analysis::drift::RobotDriftOutput {
            envelope: envelope(&issues),
            result,
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::from(exit_code);
    }

    // ---- Search commands ----
    if cli.robot_search {
        let query = match cli.search.as_deref() {
            Some(q) if !q.trim().is_empty() => q.trim(),
            _ => {
                eprintln!("error: --robot-search requires --search <query>");
                return ExitCode::from(1);
            }
        };

        let mode = cli.search_mode.as_deref().map_or(
            bvr::analysis::search::SearchMode::Text,
            bvr::analysis::search::SearchMode::from_str_or_default,
        );
        let resolved_search_preset = cli.resolve_search_preset();

        let weights = if let Some(ref json) = cli.search_weights {
            match bvr::analysis::search::SearchWeights::from_json(json) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        } else {
            let preset_name = resolved_search_preset.as_deref().unwrap_or("default");
            bvr::analysis::search::get_preset(preset_name)
        };

        let results = bvr::analysis::search::execute_search(
            query,
            &issues,
            &analyzer.metrics,
            mode,
            &weights,
            cli.search_limit,
        );

        let preset_field = if mode == bvr::analysis::search::SearchMode::Hybrid {
            Some(resolved_search_preset.unwrap_or_else(|| "default".to_string()))
        } else {
            None
        };
        let weights_field = if mode == bvr::analysis::search::SearchMode::Hybrid {
            Some(weights)
        } else {
            None
        };

        let output = bvr::analysis::search::RobotSearchOutput {
            envelope: envelope(&issues),
            query: query.to_string(),
            limit: cli.search_limit,
            mode: mode.as_str().to_string(),
            preset: preset_field,
            weights: weights_field,
            results,
            usage_hints: vec![
                "jq '.results[] | {id: .issue_id, score: .score, title: .title}' - extract ranked results"
                    .to_string(),
                "jq '.results[0]' - inspect the top match".to_string(),
            ],
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    // --emit-script: generate shell script from triage recommendations
    if cli.emit_script {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: cli.script_limit.max(10),
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });

        let mut recommendations = triage.result.recommendations;

        // Apply recipe filter if specified
        if let Some(ref recipe_name) = cli.recipe {
            if let Some(recipe) = bvr::analysis::recipe::find_recipe(recipe_name) {
                recommendations = bvr::analysis::recipe::apply_recipe(
                    &recipe,
                    &recommendations,
                    &issues,
                    &actionable_ids_for_recipe_filters(&analyzer),
                    &analyzer.metrics.pagerank,
                );
            } else {
                eprintln!("error: unknown recipe '{recipe_name}'");
                eprintln!("Available recipes:");
                for r in bvr::analysis::recipe::list_recipes() {
                    eprintln!("  {} - {}", r.name, r.description);
                }
                return ExitCode::from(1);
            }
        }

        let format = bvr::analysis::recipe::ScriptFormat::from_str_or_default(&cli.script_format);
        let script = bvr::analysis::recipe::emit_script(
            &recommendations,
            cli.script_limit,
            format,
            &Utc::now().to_rfc3339(),
            &compute_data_hash(&issues),
        );
        println!("{script}");
        return ExitCode::SUCCESS;
    }

    // --feedback-accept / --feedback-ignore: record feedback on a recommendation
    if cli.feedback_accept.is_some() || cli.feedback_ignore.is_some() {
        let work_dir = feedback_project_dir(&cli);

        let (issue_id, action) = if let Some(ref id) = cli.feedback_accept {
            (id.as_str(), "accept")
        } else if let Some(id) = cli.feedback_ignore.as_deref() {
            (id, "ignore")
        } else {
            eprintln!("error: feedback action requires --feedback-accept or --feedback-ignore");
            return ExitCode::from(1);
        };

        // Look up the issue's triage score (with current feedback applied)
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 100,
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });
        let score = triage
            .result
            .recommendations
            .iter()
            .find(|r| r.id == issue_id)
            .map_or(0.0, |r| r.score);

        let mut feedback = bvr::analysis::recipe::FeedbackData::load(&work_dir);
        if action == "accept" {
            feedback.record_accept(issue_id, score, "cli", "");
        } else {
            feedback.record_ignore(issue_id, score, "cli", "");
        }
        if let Err(error) = feedback.save(&work_dir) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        println!("Recorded {action} feedback for {issue_id} (score: {score:.3})");
        let stats = feedback.stats();
        println!(
            "Feedback summary: {} accepted, {} ignored",
            stats.total_accepted, stats.total_ignored
        );
        return ExitCode::SUCCESS;
    }

    // --priority-brief: generate markdown priority brief
    if let Some(ref brief_path) = cli.priority_brief {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 20,
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });
        let env = envelope(&issues);
        let brief = bvr::analysis::brief::generate_priority_brief(
            &issues,
            &triage.result,
            &env.data_hash,
            &env.generated_at,
        );
        if let Err(error) = std::fs::write(brief_path, &brief) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        eprintln!("Wrote priority brief to {}", brief_path.display());
        return ExitCode::SUCCESS;
    }

    // --agent-brief: generate agent brief bundle directory
    if let Some(ref brief_dir) = cli.agent_brief {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 20,
            scoring: TriageScoringOptions {
                weight_adjustments: feedback_weight_adjustments.clone(),
                ..TriageScoringOptions::default()
            },
            ..TriageOptions::default()
        });
        let insights = analyzer.insights();
        let insights_json = serde_json::to_value(&insights).unwrap_or_default();
        let env = envelope(&issues);
        match bvr::analysis::brief::generate_agent_brief(
            &issues,
            &triage.result,
            &insights_json,
            &env.data_hash,
            &env.generated_at,
            brief_dir,
        ) {
            Ok(files) => {
                eprintln!(
                    "Wrote agent brief ({} files) to {}",
                    files.len(),
                    brief_dir.display()
                );
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    }

    if let Some(export_path) = cli.export_pages.as_deref() {
        let options = bvr::export_pages::ExportPagesOptions {
            title: cli.pages_title.clone(),
            subtitle: cli.pages_subtitle.clone(),
            include_closed: cli.pages_include_closed,
            include_history: cli.pages_include_history,
        };
        let mut issue_count = count_pages_export_issues(&issues, &options);
        let hook_project_dir = match project_dir_for_export_hooks(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };

        match bvr::export_md::run_export_with_hooks(
            export_path,
            "html",
            issue_count,
            cli.no_hooks,
            Some(hook_project_dir.as_path()),
            |resolved_export_path| {
                bvr::export_pages::export_pages_bundle(&issues, resolved_export_path, &options)
            },
        ) {
            Ok(summary) => {
                eprintln!(
                    "Exported pages bundle to {} (issues: {}, history: {})",
                    summary.export_path, summary.issue_count, summary.include_history
                );
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }

        if cli.watch_export {
            let initial_watched_paths = match resolve_watch_export_paths(&cli) {
                Ok(paths) => paths,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            };
            let initial_watched_tokens = initial_watched_paths
                .iter()
                .map(|path| watch_export_token_for_path(path).map(|token| (path.clone(), token)))
                .collect::<bvr::Result<Vec<_>>>();
            let mut watched_mtimes = match initial_watched_tokens {
                Ok(entries) => entries,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            };

            let mut max_loops = std::env::var("BVR_WATCH_MAX_LOOPS")
                .ok()
                .and_then(|raw| raw.trim().parse::<usize>().ok())
                .filter(|value| *value > 0);
            let watch_interval_ms = std::env::var("BVR_WATCH_INTERVAL_MS")
                .ok()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(2_000);
            let debounce_ms: u64 = std::env::var("BVR_WATCH_DEBOUNCE_MS")
                .ok()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(500);

            eprintln!(
                "Watching {} source file(s) for changes (poll {}ms, debounce {}ms, Ctrl+C to stop)...",
                watched_mtimes.len(),
                watch_interval_ms,
                debounce_ms,
            );
            for (path, _) in &watched_mtimes {
                eprintln!("  - {}", path.display());
            }

            let mut cycle_count: u64 = 0;
            let mut last_outcome = "initial export";
            let mut last_change_at: Option<std::time::Instant> = None;

            loop {
                std::thread::sleep(std::time::Duration::from_millis(watch_interval_ms));

                let path_set_changed = match reconcile_watch_export_paths(&cli, &mut watched_mtimes)
                {
                    Ok(changed) => changed,
                    Err(error) => {
                        eprintln!("warning: cannot refresh watch paths: {error}");
                        false
                    }
                };
                let mut changed_files: Vec<String> = Vec::new();
                for (path, last_token) in &mut watched_mtimes {
                    let current_token = match file_watch_token(path) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("warning: cannot stat {}: {error}", path.display());
                            continue;
                        }
                    };

                    if current_token != *last_token {
                        *last_token = current_token;
                        changed_files.push(path.display().to_string());
                    }
                }
                if path_set_changed {
                    eprintln!(
                        "watch: refreshed source set ({} file(s) now tracked)",
                        watched_mtimes.len()
                    );
                    if changed_files.is_empty() {
                        changed_files.push("watch-set changed".to_string());
                    }
                }

                if changed_files.is_empty() {
                    if let Some(remaining) = max_loops.as_mut() {
                        *remaining = remaining.saturating_sub(1);
                        if *remaining == 0 {
                            eprintln!("watch: max loops reached, exiting (last: {last_outcome})");
                            break;
                        }
                    }
                    continue;
                }

                // Debounce: if we detected a change, wait the debounce period
                // then re-check to coalesce rapid successive writes.
                let now = std::time::Instant::now();
                if let Some(prev) = last_change_at {
                    if now.duration_since(prev).as_millis() < u128::from(debounce_ms) {
                        // Still within debounce window, skip this cycle
                        continue;
                    }
                }
                // Wait the debounce period, then re-scan to capture any trailing writes.
                std::thread::sleep(std::time::Duration::from_millis(debounce_ms));
                for (path, last_token) in &mut watched_mtimes {
                    if let Ok(current_token) = file_watch_token(path) {
                        if current_token != *last_token {
                            *last_token = current_token;
                            let display = path.display().to_string();
                            if !changed_files.contains(&display) {
                                changed_files.push(display);
                            }
                        }
                    }
                }
                last_change_at = Some(std::time::Instant::now());

                cycle_count += 1;
                eprintln!(
                    "watch: change #{cycle_count} detected in {} file(s):",
                    changed_files.len()
                );
                for f in &changed_files {
                    eprintln!("  ~ {f}");
                }

                let reload_start = std::time::Instant::now();
                let refreshed_issues = match load_issues(&cli) {
                    Ok(value) => value,
                    Err(error) => {
                        last_outcome = "reload failed";
                        eprintln!(
                            "warning: reload failed: {error} (last good export still served)"
                        );
                        continue;
                    }
                };
                let refreshed_issues = if let Some(repo_filter) = cli.repo.as_deref() {
                    filter_by_repo(refreshed_issues, repo_filter)
                } else {
                    refreshed_issues
                };
                let refreshed_issue_count = count_pages_export_issues(&refreshed_issues, &options);

                // Check if issue count actually changed (skip no-op regeneration)
                if refreshed_issue_count == issue_count {
                    // Content may still have changed even if count is same, so still export.
                    // But we note this is a same-count regeneration.
                }

                match bvr::export_md::run_export_with_hooks(
                    export_path,
                    "html",
                    refreshed_issue_count,
                    cli.no_hooks,
                    Some(hook_project_dir.as_path()),
                    |resolved_export_path| {
                        bvr::export_pages::export_pages_bundle(
                            &refreshed_issues,
                            resolved_export_path,
                            &options,
                        )
                    },
                ) {
                    Ok(summary) => {
                        let elapsed = reload_start.elapsed();
                        last_outcome = "success";
                        issue_count = refreshed_issue_count;
                        eprintln!(
                            "watch: regenerated in {elapsed:.1?} (path: {}, issues: {}, history: {})",
                            summary.export_path, summary.issue_count, summary.include_history,
                        );
                    }
                    Err(error) => {
                        last_outcome = "export failed";
                        eprintln!(
                            "warning: export failed: {error} (last good export still served)"
                        );
                    }
                }

                if let Some(remaining) = max_loops.as_mut() {
                    *remaining = remaining.saturating_sub(1);
                    if *remaining == 0 {
                        eprintln!("watch: max loops reached after {cycle_count} cycle(s), exiting");
                        break;
                    }
                }
            }
        }

        return ExitCode::SUCCESS;
    }

    if let Some(export_path) = cli.export_md.as_deref() {
        let hook_project_dir = match project_dir_for_export_hooks(&cli) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        };
        if let Err(error) = bvr::export_md::export_markdown_with_hooks(
            &issues,
            export_path,
            cli.no_hooks,
            Some(hook_project_dir.as_path()),
        ) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if let Some(ref view_name) = cli.debug_render {
        match bvr::tui::render_debug_view(
            issues.to_vec(),
            view_name,
            cli.debug_width,
            cli.debug_height,
        ) {
            Ok(output) => {
                println!("{output}");
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    }

    let (background_mode_enabled, background_mode_source) = resolve_background_mode(&cli);
    let background_runtime = build_background_mode_config(&cli, background_mode_enabled);
    if let Some(config) = background_runtime.as_ref() {
        eprintln!(
            "info: background mode enabled via {background_mode_source}; reload poll={}ms.",
            config.poll_interval_ms
        );
    }
    let initial_view = cli.view.as_deref().map(|v| {
        bvr::tui::ViewMode::from_cli(v).unwrap_or_else(|| {
            eprintln!(
                "warning: unknown --view '{v}'; supported: main, board, insights, graph, \
                 history, actionable, attention, tree, labels, flow, timediff, sprint"
            );
            bvr::tui::ViewMode::Main
        })
    });
    let initial_filter = cli.list_filter.as_deref().map(|f| {
        bvr::tui::ListFilter::from_cli(f).unwrap_or_else(|| {
            eprintln!(
                "warning: unknown --list-filter '{f}'; supported: all, open, in-progress, \
                 blocked, closed, ready"
            );
            bvr::tui::ListFilter::All
        })
    });
    match bvr::tui::run_tui_with_background(
        issues.to_vec(),
        background_runtime,
        initial_view,
        initial_filter,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

struct EarlyCommandOutcome {
    message: String,
    exit_code: ExitCode,
    to_stderr: bool,
}

fn parse_version_components(version: &str) -> Option<Vec<u64>> {
    let normalized = version.trim().trim_start_matches('v');
    if normalized.is_empty() {
        return None;
    }

    normalized
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

fn compare_versions(current: &str, latest: &str) -> Ordering {
    let Some(mut current_parts) = parse_version_components(current) else {
        return Ordering::Equal;
    };
    let Some(mut latest_parts) = parse_version_components(latest) else {
        return Ordering::Equal;
    };

    let max_len = current_parts.len().max(latest_parts.len());
    current_parts.resize(max_len, 0);
    latest_parts.resize(max_len, 0);

    current_parts.cmp(&latest_parts)
}

/// Fetch a small payload over HTTPS via the system curl (kept dependency-free,
/// matching the original update check). `--fail` turns HTTP errors (404 on a
/// repo with no GitHub releases) into curl failures instead of JSON error
/// bodies that fail later with a confusing parse message.
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let output = Command::new("curl")
        .args([
            "-sS",
            "--fail",
            "--max-time",
            "10",
            "-H",
            "User-Agent: bvr-update-check",
            url,
        ])
        .output()
        .map_err(|error| format!("failed to run curl: {error}"))?;

    if !output.status.success() {
        return Err(format!("curl exited with status {}", output.status));
    }

    Ok(output.stdout)
}

/// Extract the newest published version from a crates.io crate payload.
fn parse_crates_io_latest_version(payload: &[u8]) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| format!("invalid JSON: {error}"))?;
    let krate = value
        .get("crate")
        .ok_or_else(|| "missing crate object in crates.io payload".to_string())?;
    for key in ["max_stable_version", "max_version", "newest_version"] {
        if let Some(version) = krate
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "null")
        {
            return Ok(version.trim_start_matches('v').to_string());
        }
    }
    Err("no usable version field in crates.io payload".to_string())
}

/// Extract the tag of a GitHub releases/latest payload.
fn parse_github_latest_release(payload: &[u8]) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| format!("invalid JSON: {error}"))?;
    let tag_name = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing tag_name in GitHub release payload".to_string())?;
    Ok(tag_name.trim_start_matches('v').to_string())
}

/// Extract the highest semver-looking tag from a GitHub tags payload. The
/// tags API's ordering is not documented, so every parseable tag competes.
fn parse_github_highest_tag(payload: &[u8]) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|error| format!("invalid JSON: {error}"))?;
    let tags = value
        .as_array()
        .ok_or_else(|| "expected a JSON array of tags".to_string())?;
    let mut best: Option<String> = None;
    for tag in tags {
        let Some(name) = tag
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|name| name.trim().trim_start_matches('v'))
            .filter(|name| parse_version_components(name).is_some())
        else {
            continue;
        };
        match &best {
            Some(current) if compare_versions(name, current) != Ordering::Greater => {}
            _ => best = Some(name.to_string()),
        }
    }
    best.ok_or_else(|| "no version-shaped tags in GitHub tags payload".to_string())
}

/// Resolve the newest released bvr version. crates.io is consulted first — it
/// is the supported install source (`cargo install beads_viewer_rust`) — then
/// GitHub releases, then git tags. The repo tags releases without always
/// publishing GitHub Releases, so the old releases/latest-only check 404'd and
/// permanently reported "network unavailable" (issue #23).
fn fetch_latest_version() -> Result<String, String> {
    let mut errors = Vec::new();

    match http_get("https://crates.io/api/v1/crates/beads_viewer_rust")
        .and_then(|payload| parse_crates_io_latest_version(&payload))
    {
        Ok(version) => return Ok(version),
        Err(error) => errors.push(format!("crates.io: {error}")),
    }

    match http_get(
        "https://api.github.com/repos/Dicklesworthstone/beads_viewer_rust/releases/latest",
    )
    .and_then(|payload| parse_github_latest_release(&payload))
    {
        Ok(version) => return Ok(version),
        Err(error) => errors.push(format!("github releases: {error}")),
    }

    match http_get("https://api.github.com/repos/Dicklesworthstone/beads_viewer_rust/tags")
        .and_then(|payload| parse_github_highest_tag(&payload))
    {
        Ok(version) => return Ok(version),
        Err(error) => errors.push(format!("github tags: {error}")),
    }

    Err(errors.join("; "))
}

/// The exact cargo argv `bvr upgrade` runs. `--locked` keeps the published
/// lockfile authoritative; the crate name is `beads_viewer_rust` (the short
/// `bvr` name was taken on crates.io), though it installs a `bvr` binary.
fn upgrade_cargo_args(latest: &str) -> Vec<String> {
    vec![
        "install".to_string(),
        "beads_viewer_rust".to_string(),
        "--version".to_string(),
        latest.to_string(),
        "--locked".to_string(),
    ]
}

/// `bvr upgrade` (issue #23): check the latest release and install it through
/// the supported distribution mechanism. `--check-update` remains the
/// non-mutating counterpart.
fn run_upgrade(dry_run: bool) -> EarlyCommandOutcome {
    let current = env!("CARGO_PKG_VERSION");
    let latest = match fetch_latest_version() {
        Ok(latest) => latest,
        Err(error) => {
            return EarlyCommandOutcome {
                message: format!(
                    "Could not determine the latest bvr version ({error}).\n\
                     Current version: bvr v{current}.\n\
                     Check network access and retry, or install manually:\n  cargo install beads_viewer_rust --locked"
                ),
                exit_code: ExitCode::from(1),
                to_stderr: true,
            };
        }
    };

    match compare_versions(current, &latest) {
        Ordering::Equal | Ordering::Greater => EarlyCommandOutcome {
            message: format!("Up to date (v{current})"),
            exit_code: ExitCode::SUCCESS,
            to_stderr: false,
        },
        Ordering::Less => {
            let args = upgrade_cargo_args(&latest);
            let rendered = format!("cargo {}", args.join(" "));
            if dry_run {
                return EarlyCommandOutcome {
                    message: format!(
                        "Newer version available: v{latest} (you have v{current})\nWould run: {rendered}"
                    ),
                    exit_code: ExitCode::SUCCESS,
                    to_stderr: false,
                };
            }

            println!("Upgrading bvr v{current} -> v{latest}");
            println!("Running: {rendered}");
            // Inherit stdio so cargo's own progress and errors stream through.
            match Command::new("cargo").args(&args).status() {
                Ok(status) if status.success() => EarlyCommandOutcome {
                    message: format!(
                        "Upgraded bvr to v{latest}. The binary lives in cargo's bin directory \
                         (usually ~/.cargo/bin); run `bvr --version` to confirm."
                    ),
                    exit_code: ExitCode::SUCCESS,
                    to_stderr: false,
                },
                Ok(status) => EarlyCommandOutcome {
                    message: format!(
                        "cargo install failed ({status}).\nRetry manually with:\n  {rendered}"
                    ),
                    exit_code: ExitCode::from(1),
                    to_stderr: true,
                },
                Err(error) => EarlyCommandOutcome {
                    message: format!(
                        "Could not run cargo ({error}).\n\
                         bvr is distributed via cargo; install the Rust toolchain (https://rustup.rs), then run:\n  {rendered}"
                    ),
                    exit_code: ExitCode::from(1),
                    to_stderr: true,
                },
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IssueLoadTarget {
    BeadsFile(PathBuf),
    WorkspaceConfig(PathBuf),
    RepoPath(Option<PathBuf>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundModeSource {
    CliFlag,
    EnvVar,
    UserConfig,
    DefaultDisabled,
}

impl std::fmt::Display for BackgroundModeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliFlag => f.write_str("CLI flag"),
            Self::EnvVar => f.write_str("BV_BACKGROUND_MODE"),
            Self::UserConfig => f.write_str("~/.config/bv/config.yaml"),
            Self::DefaultDisabled => f.write_str("default"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct UserBackgroundConfig {
    experimental: Option<UserBackgroundExperimentalConfig>,
}

#[derive(Debug, Deserialize)]
struct UserBackgroundExperimentalConfig {
    background_mode: Option<bool>,
}

fn resolve_background_mode(cli: &Cli) -> (bool, BackgroundModeSource) {
    if cli.background_mode {
        return (true, BackgroundModeSource::CliFlag);
    }
    if cli.no_background_mode {
        return (false, BackgroundModeSource::CliFlag);
    }
    if let Some(value) = std::env::var("BV_BACKGROUND_MODE")
        .ok()
        .and_then(|raw| parse_background_mode_bool(&raw))
    {
        return (value, BackgroundModeSource::EnvVar);
    }
    if let Some(value) = load_background_mode_from_user_config() {
        return (value, BackgroundModeSource::UserConfig);
    }
    (false, BackgroundModeSource::DefaultDisabled)
}

fn build_background_mode_config(
    cli: &Cli,
    background_mode_enabled: bool,
) -> Option<bvr::tui::BackgroundModeConfig> {
    if !background_mode_enabled {
        return None;
    }

    if cli.as_of.is_some() {
        eprintln!("warning: background mode is ignored when --as-of is set.");
        return None;
    }

    let poll_interval_ms = std::env::var("BVR_BACKGROUND_POLL_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(bvr::tui::BackgroundModeConfig::DEFAULT_POLL_INTERVAL_MS);

    let load_target = match resolve_issue_load_target(cli) {
        Ok(load_target) => load_target,
        Err(error) => {
            eprintln!("warning: background mode disabled: {error}");
            return None;
        }
    };

    let (beads_file, workspace_config, repo_path) = match load_target {
        IssueLoadTarget::BeadsFile(path) => (Some(path), None, None),
        IssueLoadTarget::WorkspaceConfig(path) => (None, Some(path), None),
        IssueLoadTarget::RepoPath(path) => (None, None, path),
    };

    Some(bvr::tui::BackgroundModeConfig {
        beads_file,
        workspace_config,
        repo_path,
        repo_filter: cli.repo.clone(),
        poll_interval_ms,
    })
}

fn workspace_discovery_start_points(cli: &Cli) -> Vec<PathBuf> {
    let mut starts = Vec::<PathBuf>::new();
    let current_dir = std::env::current_dir().ok();
    if let Some(path) = cli.repo_path.clone() {
        let path = if path.is_absolute() {
            path
        } else if let Some(current_dir) = &current_dir {
            current_dir.join(path)
        } else {
            path
        };
        starts.push(path);
    }
    if let Some(path) = current_dir
        && !starts.iter().any(|existing| existing == &path)
    {
        starts.push(path);
    }
    if starts.is_empty() {
        starts.push(PathBuf::from("."));
    }
    starts
}

fn format_path_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("  - {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn discover_workspace_config_from_starts(starts: &[PathBuf]) -> bvr::Result<Option<PathBuf>> {
    let mut candidates = Vec::<PathBuf>::new();
    for start in starts {
        if let Some(candidate) = loader::find_workspace_config_from(start)
            && !candidates.iter().any(|existing| existing == &candidate)
        {
            candidates.push(candidate);
        }
    }

    if candidates.len() > 1 {
        return Err(bvr::BvrError::InvalidArgument(format!(
            "workspace auto-discovery is ambiguous.\n\
             Searched for {} from:\n{}\n\
             Candidates:\n{}\n\
             Remediation:\n\
               1. Re-run with --workspace <path-to-.bv/workspace.yaml>.\n\
               2. Or re-run with --beads-file <path-to-issues.jsonl> to bypass workspace aggregation.",
            loader::WORKSPACE_CONFIG_PATH,
            format_path_list(&starts),
            format_path_list(&candidates),
        )));
    }

    Ok(candidates.into_iter().next())
}

fn discover_workspace_config_for_cli(cli: &Cli) -> bvr::Result<Option<PathBuf>> {
    if cli.workspace.is_some() || cli.beads_file.is_some() {
        return Ok(None);
    }

    let starts = workspace_discovery_start_points(cli);
    discover_workspace_config_from_starts(&starts)
}

fn resolve_issue_load_target(cli: &Cli) -> bvr::Result<IssueLoadTarget> {
    if let Some(path) = &cli.beads_file {
        return Ok(IssueLoadTarget::BeadsFile(path.clone()));
    }

    if let Some(path) = &cli.workspace {
        return Ok(IssueLoadTarget::WorkspaceConfig(
            resolve_workspace_config_path(path),
        ));
    }

    if let Some(path) = discover_workspace_config_for_cli(cli)? {
        return Ok(IssueLoadTarget::WorkspaceConfig(path));
    }

    Ok(IssueLoadTarget::RepoPath(cli.repo_path.clone()))
}

fn with_workspace_discovery_guidance(cli: &Cli, error: bvr::BvrError) -> bvr::BvrError {
    if cli.workspace.is_some() || cli.beads_file.is_some() {
        return error;
    }

    let error_message = error.to_string();
    match error {
        bvr::BvrError::MissingBeadsDir(_) | bvr::BvrError::MissingBeadsFile(_) => {
            let starts = workspace_discovery_start_points(cli);
            bvr::BvrError::InvalidArgument(format!(
                "no workspace config or single-repo beads data could be resolved.\n\
                 Searched for {} from:\n{}\n\
                 Workspace candidates: none\n\
                 Single-repo fallback error: {error_message}\n\
                 Remediation:\n\
                   1. Re-run with --workspace <path-to-.bv/workspace.yaml>.\n\
                   2. Or re-run with --beads-file <path-to-issues.jsonl>.\n\
                   3. Or run from a repository/workspace containing .beads or .bv/workspace.yaml.",
                loader::WORKSPACE_CONFIG_PATH,
                format_path_list(&starts),
            ))
        }
        _ => error,
    }
}

fn parse_background_mode_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn load_background_mode_from_user_config() -> Option<bool> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".config/bv/config.yaml");
    let content = fs::read_to_string(path).ok()?;
    let config = serde_yaml::from_str::<UserBackgroundConfig>(&content).ok()?;
    config
        .experimental
        .and_then(|section| section.background_mode)
}

fn handle_operational_commands(cli: &Cli) -> EarlyCommandOutcome {
    if let Some(BvrCommand::Upgrade { dry_run }) = &cli.command {
        return run_upgrade(*dry_run);
    }

    if cli.check_update {
        let current = env!("CARGO_PKG_VERSION");
        let message = match fetch_latest_version() {
            Ok(latest) => match compare_versions(current, &latest) {
                Ordering::Less => format!(
                    "Newer version available: v{latest} (you have v{current})\n  Update: bvr upgrade   (or: cargo install beads_viewer_rust --locked)"
                ),
                Ordering::Equal | Ordering::Greater => format!("Up to date (v{current})"),
            },
            Err(_) => format!(
                "Could not check for updates (network unavailable).\nCurrent version: bvr v{current}"
            ),
        };

        return EarlyCommandOutcome {
            message,
            exit_code: ExitCode::SUCCESS,
            to_stderr: false,
        };
    }

    EarlyCommandOutcome {
        message: "error: unsupported operational flag.\n\
                  Remediation: use --check-update or `bvr upgrade`."
            .to_string(),
        exit_code: ExitCode::from(2),
        to_stderr: true,
    }
}

fn load_issues(cli: &Cli) -> bvr::Result<Vec<bvr::model::Issue>> {
    if let Some(ref_name) = &cli.as_of {
        return load_issues_at_revision(cli, ref_name);
    }

    match resolve_issue_load_target(cli)? {
        IssueLoadTarget::BeadsFile(path) => loader::load_issues_from_file(&path),
        IssueLoadTarget::WorkspaceConfig(path) => loader::load_workspace_issues(&path),
        IssueLoadTarget::RepoPath(repo_path) => loader::load_issues(repo_path.as_deref())
            .map_err(|error| with_workspace_discovery_guidance(cli, error)),
    }
}

fn count_pages_export_issues(
    issues: &[bvr::model::Issue],
    options: &bvr::export_pages::ExportPagesOptions,
) -> usize {
    if options.include_closed {
        issues.len()
    } else {
        issues.iter().filter(|issue| issue.is_open_like()).count()
    }
}

fn resolve_watch_export_paths(cli: &Cli) -> bvr::Result<Vec<PathBuf>> {
    match resolve_issue_load_target(cli)? {
        IssueLoadTarget::BeadsFile(path) => Ok(vec![path]),
        IssueLoadTarget::WorkspaceConfig(path) => {
            let mut paths = vec![path.clone()];
            paths.extend(loader::find_workspace_issue_paths(&path)?);
            paths.sort();
            paths.dedup();
            Ok(paths)
        }
        IssueLoadTarget::RepoPath(repo_path) => {
            let beads_dir = loader::get_beads_dir(repo_path.as_deref())
                .map_err(|error| with_workspace_discovery_guidance(cli, error))?;
            let beads_path = loader::find_jsonl_path(&beads_dir)
                .map_err(|error| with_workspace_discovery_guidance(cli, error))?;
            Ok(vec![beads_path])
        }
    }
}

fn watch_export_token_for_path(path: &Path) -> bvr::Result<Option<FileWatchToken>> {
    file_watch_token(path).map_err(|error| {
        bvr::BvrError::InvalidArgument(format!(
            "failed to read watch source {}: {error}",
            path.display()
        ))
    })
}

fn reconcile_watch_export_paths(
    cli: &Cli,
    watched_tokens: &mut Vec<(PathBuf, Option<FileWatchToken>)>,
) -> bvr::Result<bool> {
    let previous_tokens = watched_tokens
        .drain(..)
        .collect::<BTreeMap<PathBuf, Option<FileWatchToken>>>();
    let watched_paths = resolve_watch_export_paths(cli)?;
    let mut path_set_changed = watched_paths.len() != previous_tokens.len();
    let mut next_tokens = Vec::with_capacity(watched_paths.len());
    let mut previous_tokens = previous_tokens;

    for path in watched_paths {
        let token = match previous_tokens.remove(&path) {
            Some(existing) => existing,
            None => {
                path_set_changed = true;
                watch_export_token_for_path(&path)?
            }
        };
        next_tokens.push((path, token));
    }

    *watched_tokens = next_tokens;
    Ok(path_set_changed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileWatchToken {
    modified_millis: u64,
    len_bytes: u64,
    content_fingerprint: [u8; 32],
}

fn file_watch_token(path: &Path) -> bvr::Result<Option<FileWatchToken>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(bvr::BvrError::Io(error)),
    };
    let modified_millis = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            let millis = duration.as_millis().min(u128::from(u64::MAX));
            u64::try_from(millis).unwrap_or(u64::MAX)
        });
    let content_fingerprint = Sha256::digest(fs::read(path)?).into();
    Ok(Some(FileWatchToken {
        modified_millis,
        len_bytes: metadata.len(),
        content_fingerprint,
    }))
}

fn load_issues_from_git_relative_path(
    repo_root: &Path,
    relative_path: &Path,
    revision: &str,
) -> bvr::Result<Vec<bvr::model::Issue>> {
    let git_ref = format!("{revision}:{}", relative_path.to_string_lossy());
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("show")
        .arg(&git_ref)
        .output()?;

    if !output.status.success() {
        return Err(bvr::BvrError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "could not load {} at revision {revision}",
                relative_path.display()
            ),
        )));
    }

    parse_issues_from_jsonl_text(&String::from_utf8_lossy(&output.stdout))
}

fn historical_jsonl_skip_file_name(file_name: &str) -> bool {
    file_name.contains(".backup")
        || file_name.contains(".orig")
        || file_name.contains(".merge")
        || file_name == "deletions.jsonl"
        || file_name.starts_with("beads.left")
        || file_name.starts_with("beads.right")
}

fn git_blob_size(repo_root: &Path, object_spec: &str) -> bvr::Result<Option<u64>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("cat-file")
        .arg("-s")
        .arg(object_spec)
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let size = raw.trim().parse::<u64>().map_err(|error| {
        bvr::BvrError::InvalidArgument(format!(
            "git returned an invalid blob size for {object_spec}: {error}"
        ))
    })?;
    Ok(Some(size))
}

fn resolve_historical_jsonl_relative_path(
    repo_root: &Path,
    beads_dir_relative: &Path,
    revision: &str,
) -> bvr::Result<PathBuf> {
    for preferred in ["beads.jsonl", "issues.jsonl", "beads.base.jsonl"] {
        let candidate = beads_dir_relative.join(preferred);
        let object_spec = format!("{revision}:{}", candidate.to_string_lossy());
        if git_blob_size(repo_root, &object_spec)?.is_some_and(|size| size > 0) {
            return Ok(candidate);
        }
    }

    let directory_spec = format!("{revision}:{}", beads_dir_relative.to_string_lossy());
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("ls-tree")
        .arg("--name-only")
        .arg(&directory_spec)
        .output()?;

    if !output.status.success() {
        return Err(bvr::BvrError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "could not resolve historical beads directory {} at revision {revision}",
                beads_dir_relative.display()
            ),
        )));
    }

    let mut fallback_candidates = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| line.ends_with(".jsonl"))
        .filter(|line| !historical_jsonl_skip_file_name(&line.to_ascii_lowercase()))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    fallback_candidates.sort();

    fallback_candidates
        .into_iter()
        .next()
        .map(|relative_file_name| beads_dir_relative.join(relative_file_name))
        .ok_or_else(|| {
            bvr::BvrError::InvalidArgument(format!(
                "could not resolve historical beads JSONL inside {} at revision {revision}",
                beads_dir_relative.display()
            ))
        })
}

fn load_issues_from_history_beads_dir(
    beads_dir: &Path,
    revision: &str,
) -> bvr::Result<Vec<bvr::model::Issue>> {
    let absolute_beads_dir = absolute_from_current_dir(beads_dir);
    let repo_root = resolve_git_toplevel(
        absolute_beads_dir
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )
    .ok_or_else(|| {
        bvr::BvrError::InvalidArgument(format!(
            "could not determine repository root for historical beads dir {}",
            absolute_beads_dir.display()
        ))
    })?;
    let beads_dir_relative = absolute_beads_dir.strip_prefix(&repo_root).map_err(|_| {
        bvr::BvrError::InvalidArgument(format!(
            "historical beads dir {} is outside repository root {}",
            absolute_beads_dir.display(),
            repo_root.display()
        ))
    })?;
    let jsonl_relative =
        resolve_historical_jsonl_relative_path(&repo_root, beads_dir_relative, revision)?;

    load_issues_from_git_relative_path(&repo_root, &jsonl_relative, revision)
}

fn load_issues_from_history_file_path(
    path: &Path,
    revision: &str,
) -> bvr::Result<Vec<bvr::model::Issue>> {
    let absolute_path = absolute_from_current_dir(path);
    let repo_root = resolve_git_toplevel(absolute_path.parent().unwrap_or_else(|| Path::new(".")))
        .ok_or_else(|| {
            bvr::BvrError::InvalidArgument(format!(
                "could not determine repository root for historical issues path {}",
                absolute_path.display()
            ))
        })?;
    let relative_path = absolute_path.strip_prefix(&repo_root).map_err(|_| {
        bvr::BvrError::InvalidArgument(format!(
            "historical issues path {} is outside repository root {}",
            absolute_path.display(),
            repo_root.display()
        ))
    })?;

    load_issues_from_git_relative_path(&repo_root, relative_path, revision)
}

fn load_workspace_issues_at_revision(
    config_path: &Path,
    revision: &str,
) -> bvr::Result<Vec<bvr::model::Issue>> {
    let config = loader::load_workspace_config(config_path)?;
    let workspace_root = loader::resolve_workspace_root(config_path);
    let enabled_repos = config
        .repos
        .iter()
        .filter(|repo| repo.enabled.unwrap_or(true))
        .cloned()
        .collect::<Vec<_>>();
    let known_prefixes = enabled_repos
        .iter()
        .map(bvr::loader::WorkspaceRepoConfig::effective_prefix)
        .collect::<Vec<_>>();

    let mut all_issues = Vec::new();
    let mut failed_repos = Vec::new();

    for repo in enabled_repos {
        let repo_name = repo.effective_name();
        let prefix = repo.effective_prefix();
        let repo_path = if Path::new(repo.path.trim()).is_absolute() {
            PathBuf::from(repo.path.trim())
        } else {
            workspace_root.join(repo.path.trim())
        };
        let beads_dir = repo_path.join(repo.effective_beads_path(Some(&config.defaults)));

        let repo_issues = (|| -> bvr::Result<Vec<bvr::model::Issue>> {
            let mut issues = load_issues_from_history_beads_dir(&beads_dir, revision)?;
            loader::namespace_workspace_issues(&mut issues, &prefix, &repo_name, &known_prefixes);
            Ok(issues)
        })();

        match repo_issues {
            Ok(mut issues) => all_issues.append(&mut issues),
            Err(error) => {
                tracing::warn!(
                    "workspace repo '{}' failed to load at {}: {}",
                    repo_name,
                    revision,
                    error
                );
                failed_repos.push(repo_name);
            }
        }
    }

    if all_issues.is_empty() && !failed_repos.is_empty() {
        return Err(bvr::BvrError::InvalidArgument(format!(
            "workspace historical load failed for all repositories at {revision}: {}",
            failed_repos.join(", ")
        )));
    }

    Ok(all_issues)
}

fn load_issues_at_revision(cli: &Cli, revision: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    let issues = load_historical_issues_for_load_target(cli, revision)?;

    eprintln!("Loaded {} issues (as-of: {revision})", issues.len());
    Ok(issues)
}

fn parse_suggest_type(raw: Option<&str>) -> bvr::Result<Option<SuggestionType>> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let normalized = value.to_ascii_lowercase();

    let parsed = match normalized.as_str() {
        "duplicate" | "duplicates" => SuggestionType::PotentialDuplicate,
        "dependency" | "dependencies" => SuggestionType::MissingDependency,
        "label" | "labels" => SuggestionType::LabelSuggestion,
        "cycle" | "cycles" => SuggestionType::CycleWarning,
        "stale" | "stale_cleanup" => SuggestionType::StaleCleanup,
        _ => {
            return Err(bvr::BvrError::InvalidArgument(format!(
                "Invalid suggest-type: {value} (use: duplicate, dependency, label, cycle, stale)"
            )));
        }
    };

    Ok(Some(parsed))
}

fn resolve_forecast_sprint_beads(cli: &Cli, sprint_id: &str) -> bvr::Result<BTreeSet<String>> {
    let Ok(sprints) = load_sprints_for_cli(cli) else {
        return Err(bvr::BvrError::InvalidArgument(format!(
            "sprint not found: {sprint_id}"
        )));
    };

    let Some(sprint) = sprints.into_iter().find(|sprint| sprint.id == sprint_id) else {
        return Err(bvr::BvrError::InvalidArgument(format!(
            "sprint not found: {sprint_id}"
        )));
    };

    Ok(sprint.bead_ids.into_iter().collect())
}

fn load_issues_for_diff(cli: &Cli, diff_since: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    if let Some(path) = resolve_cli_reference_file_path(diff_since, cli) {
        let mut issues = loader::load_issues_from_file(&path)?;
        // When the current load target is a workspace, namespace the baseline
        // issues the same way the live workspace pipeline does, so diff IDs match.
        if let Ok(IssueLoadTarget::WorkspaceConfig(config_path)) = resolve_issue_load_target(cli) {
            if let Ok(config) = loader::load_workspace_config(&config_path) {
                let enabled_repos: Vec<_> = config
                    .repos
                    .iter()
                    .filter(|r| r.enabled.unwrap_or(true))
                    .collect();
                // For single-repo workspaces, safely namespace the baseline.
                // Baseline save/load is currently limited to single-repo
                // workspaces.
                if enabled_repos.len() == 1 {
                    let repo = enabled_repos[0];
                    let prefix = repo.effective_prefix();
                    let repo_name = repo.effective_name();
                    let known_prefixes = vec![prefix.clone()];
                    loader::namespace_workspace_issues(
                        &mut issues,
                        &prefix,
                        &repo_name,
                        &known_prefixes,
                    );
                }
            }
        }
        return Ok(issues);
    }

    load_issues_from_git_ref(cli, diff_since)
}

fn load_historical_issues_for_load_target(
    cli: &Cli,
    reference: &str,
) -> bvr::Result<Vec<bvr::model::Issue>> {
    match resolve_issue_load_target(cli)? {
        IssueLoadTarget::BeadsFile(path) => load_issues_from_history_file_path(&path, reference),
        IssueLoadTarget::WorkspaceConfig(path) => {
            load_workspace_issues_at_revision(&path, reference)
        }
        IssueLoadTarget::RepoPath(repo_path) => {
            let beads_dir = loader::get_beads_dir(repo_path.as_deref())
                .map_err(|error| with_workspace_discovery_guidance(cli, error))?;
            load_issues_from_history_beads_dir(&beads_dir, reference)
        }
    }
}

fn load_issues_from_git_ref(cli: &Cli, reference: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    load_historical_issues_for_load_target(cli, reference).map_err(|_| {
        bvr::BvrError::InvalidArgument(format!(
            "could not resolve --diff-since={reference} to a historical beads JSONL snapshot"
        ))
    })
}

fn resolve_diff_revision(cli: &Cli, reference: &str) -> String {
    if let Some(path) = resolve_cli_reference_file_path(reference, cli) {
        return path.to_string_lossy().to_string();
    }

    let Some(repo_root) = resolve_repo_root(cli) else {
        return reference.to_string();
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .arg("rev-parse")
        .arg("--verify")
        .arg(reference)
        .output();

    let Ok(output) = output else {
        return reference.to_string();
    };

    if !output.status.success() {
        return reference.to_string();
    }

    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if resolved.is_empty() {
        reference.to_string()
    } else {
        resolved
    }
}

/// Resolve --as-of flag to (as_of_value, resolved_commit_sha).
fn resolve_as_of(cli: &Cli) -> (Option<String>, Option<String>) {
    let Some(ref_name) = &cli.as_of else {
        return (None, None);
    };
    let resolved = resolve_diff_revision(cli, ref_name);
    let commit = if resolved != *ref_name {
        Some(resolved)
    } else {
        None
    };
    (Some(ref_name.clone()), commit)
}

fn latest_commit_sha(cli: &Cli) -> Option<String> {
    let repo_root = resolve_repo_root(cli)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

fn parse_issues_from_jsonl_text(text: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    let mut issues = Vec::<bvr::model::Issue>::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let mut issue: bvr::model::Issue = serde_json::from_str(line)?;
        issue.status = issue.normalized_status();
        issue.validate()?;
        issues.push(issue);
    }

    Ok(issues)
}

fn build_robot_history_output(
    cli: &Cli,
    issues: &[bvr::model::Issue],
    analyzer: &Analyzer,
) -> bvr::Result<RobotHistoryOutput> {
    let history_since = cli
        .history_since
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let histories_timeline = analyzer.history(cli.bead_history.as_deref(), cli.history_limit);
    let include_timeline = cli.bead_history.is_some();
    let mut histories_map = histories_timeline
        .iter()
        .map(|history| {
            let events = history
                .events
                .iter()
                .map(|event| HistoryEventCompat {
                    bead_id: history.id.clone(),
                    event_type: event.kind.clone(),
                    timestamp: event
                        .timestamp
                        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                        .unwrap_or_default(),
                    commit_sha: String::new(),
                    commit_message: event.details.clone(),
                    author: String::new(),
                    author_email: String::new(),
                })
                .collect::<Vec<_>>();

            (
                history.id.clone(),
                HistoryBeadCompat {
                    bead_id: history.id.clone(),
                    title: history.title.clone(),
                    status: history.status.clone(),
                    events,
                    milestones: HistoryMilestonesCompat::default(),
                    commits: None,
                    cycle_time: None,
                    last_author: String::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut commit_index = BTreeMap::<String, Vec<String>>::new();
    let mut method_distribution = BTreeMap::<String, usize>::new();
    let mut latest_sha = latest_commit_sha(cli);

    if let Some(workspace_repos) = resolve_workspace_history_repos(cli, issues)? {
        let mut latest_commit = None::<(String, String)>;
        for workspace_repo in workspace_repos {
            let repo_root = resolve_git_toplevel(&workspace_repo.repo_root)
                .unwrap_or_else(|| workspace_repo.repo_root.clone());
            let commits = load_git_commits(&repo_root, cli.history_limit, history_since)?;
            if let Some(commit) = commits.first() {
                update_latest_history_commit_sha(
                    &mut latest_commit,
                    &commit.timestamp,
                    &commit.sha,
                );
            }
            correlate_histories_with_git_aliases(
                &repo_root,
                &commits,
                &mut histories_map,
                &mut commit_index,
                &mut method_distribution,
                &workspace_repo.aliases,
            );
        }
        if let Some((_, sha)) = latest_commit {
            latest_sha = Some(sha);
        }
    } else {
        let workspace_aliases = build_workspace_id_aliases(issues);
        if let Some(repo_root) = resolve_repo_root(cli) {
            let commits = load_git_commits(&repo_root, cli.history_limit, history_since)?;
            if let Some(commit) = commits.first() {
                latest_sha = Some(commit.sha.clone());
            }
            correlate_histories_with_git_aliases(
                &repo_root,
                &commits,
                &mut histories_map,
                &mut commit_index,
                &mut method_distribution,
                &workspace_aliases,
            );
        }
    }

    if latest_sha.is_none() {
        latest_sha = latest_commit_sha(cli);
    }

    if cli.history_min_confidence > 0.0 {
        for history in histories_map.values_mut() {
            if let Some(commits) = history.commits.as_mut() {
                commits.retain(|commit| commit.confidence >= cli.history_min_confidence);
            }
        }

        commit_index.clear();
        method_distribution.clear();
        for (bead_id, history) in &histories_map {
            for commit in history.commits.as_deref().unwrap_or_default() {
                let ids = commit_index.entry(commit.sha.clone()).or_default();
                if !ids.contains(bead_id) {
                    ids.push(bead_id.clone());
                }
                *method_distribution
                    .entry(commit.method.clone())
                    .or_insert(0) += 1;
            }
        }
        for ids in commit_index.values_mut() {
            ids.sort();
            ids.dedup();
        }
    }

    finalize_history_entries(&mut histories_map);
    let stats = compute_history_stats(&histories_map, &commit_index, method_distribution);
    let history_count = include_timeline.then_some(histories_timeline.len());
    let histories_timeline = include_timeline.then_some(histories_timeline);

    let git_range = if let Some(since) = history_since {
        if cli.history_limit == 0 {
            format!("since {since}")
        } else {
            format!("since {since}, last {} commits", cli.history_limit)
        }
    } else if cli.history_limit == 0 {
        "all history".to_string()
    } else {
        format!("last {} commits", cli.history_limit)
    };

    Ok(RobotHistoryOutput {
        envelope: envelope(issues),
        bead_history: cli.bead_history.clone(),
        history_count,
        histories_timeline,
        git_range,
        latest_commit_sha: latest_sha,
        stats,
        histories_map,
        commit_index,
    })
}

#[derive(Debug)]
struct WorkspaceHistoryRepo {
    repo_root: PathBuf,
    aliases: BTreeMap<String, String>,
}

fn update_latest_history_commit_sha(
    latest_commit: &mut Option<(String, String)>,
    timestamp: &str,
    sha: &str,
) {
    if latest_commit
        .as_ref()
        .is_none_or(|(latest_timestamp, _)| timestamp > latest_timestamp.as_str())
    {
        *latest_commit = Some((timestamp.to_string(), sha.to_string()));
    }
}

fn build_workspace_id_aliases_for_repo(
    issues: &[bvr::model::Issue],
    repo_name: &str,
) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::<String, String>::new();
    let repo_name = repo_name.trim();

    for issue in issues {
        if issue.source_repo.trim() != repo_name {
            continue;
        }

        let prefix = issue
            .workspace_prefix
            .as_deref()
            .map(str::trim)
            .filter(|prefix| !prefix.is_empty())
            .map(std::borrow::ToOwned::to_owned)
            .unwrap_or_else(|| format!("{repo_name}-"));

        let id_lower = issue.id.to_ascii_lowercase();
        let prefix_lower = prefix.to_ascii_lowercase();
        if let Some(raw) = id_lower.strip_prefix(&prefix_lower)
            && !raw.is_empty()
        {
            aliases
                .entry(raw.to_string())
                .or_insert_with(|| issue.id.clone());
        }
    }

    aliases
}

fn resolve_workspace_history_repos(
    cli: &Cli,
    issues: &[bvr::model::Issue],
) -> bvr::Result<Option<Vec<WorkspaceHistoryRepo>>> {
    let IssueLoadTarget::WorkspaceConfig(config_path) = resolve_issue_load_target(cli)? else {
        return Ok(None);
    };

    let config = loader::load_workspace_config(&config_path)?;
    let workspace_root = loader::resolve_workspace_root(&config_path);
    let mut repos = Vec::<WorkspaceHistoryRepo>::new();

    for repo in config.repos {
        if !repo.enabled.unwrap_or(true) {
            continue;
        }

        let repo_path = Path::new(repo.path.trim());
        let repo_root = if repo_path.is_absolute() {
            repo_path.to_path_buf()
        } else {
            workspace_root.join(repo_path)
        };

        repos.push(WorkspaceHistoryRepo {
            repo_root,
            aliases: build_workspace_id_aliases_for_repo(issues, &repo.name),
        });
    }

    Ok(Some(repos))
}

fn compute_related_work_result(
    bead_id: &str,
    histories_map: &BTreeMap<String, HistoryBeadCompat>,
    min_relevance: u32,
    max_results: usize,
    include_closed: bool,
) -> bvr::analysis::file_intel::RelatedWorkResult {
    bvr::analysis::file_intel::find_related_work_with_options(
        bead_id,
        histories_map,
        min_relevance,
        max_results,
        include_closed,
    )
}

fn resolve_repo_root(cli: &Cli) -> Option<PathBuf> {
    let base = project_dir_for_load_target(cli)
        .ok()
        .or_else(|| std::env::current_dir().ok())?;

    resolve_git_toplevel(&base).or(Some(base))
}

fn resolve_git_toplevel(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn resolve_reference_file_path(reference: &str, repo_path: Option<&Path>) -> Option<PathBuf> {
    let direct = PathBuf::from(reference);
    if direct.is_absolute() && direct.is_file() {
        return Some(direct);
    }

    if let Some(root) = repo_path {
        let rooted = root.join(reference);
        if rooted.is_file() {
            return Some(rooted);
        }
    }

    direct.is_file().then_some(direct)
}

fn resolve_cli_reference_file_path(reference: &str, cli: &Cli) -> Option<PathBuf> {
    let project_dir = project_dir_for_load_target(cli).ok();
    if let Some(path) = resolve_reference_file_path(reference, project_dir.as_deref()) {
        return Some(path);
    }

    if let Some(repo_path) = cli.repo_path.as_deref() {
        let absolute_repo_path = absolute_from_current_dir(repo_path);
        if project_dir.as_ref() != Some(&absolute_repo_path) {
            if let Some(path) = resolve_reference_file_path(reference, Some(&absolute_repo_path)) {
                return Some(path);
            }
        }
    }

    None
}

fn resolve_workspace_config_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.join(loader::WORKSPACE_CONFIG_PATH);
    }

    path.to_path_buf()
}

fn filter_by_repo(issues: Vec<bvr::model::Issue>, repo_filter: &str) -> Vec<bvr::model::Issue> {
    let filter = repo_filter.trim().to_ascii_lowercase();
    if filter.is_empty() {
        return issues;
    }

    let needs_flexible_match =
        !filter.ends_with('-') && !filter.ends_with(':') && !filter.ends_with('_');
    let with_dash = format!("{filter}-");
    let with_colon = format!("{filter}:");
    let with_underscore = format!("{filter}_");

    issues
        .into_iter()
        .filter(|issue| {
            let id = issue.id.to_ascii_lowercase();
            if id.starts_with(&filter) {
                return true;
            }
            if needs_flexible_match
                && (id.starts_with(&with_dash)
                    || id.starts_with(&with_colon)
                    || id.starts_with(&with_underscore))
            {
                return true;
            }

            let source_repo = issue.source_repo.trim();
            if source_repo.is_empty() || source_repo == "." {
                return false;
            }

            let source_repo = source_repo.to_ascii_lowercase();
            if source_repo.starts_with(&filter) {
                return true;
            }

            needs_flexible_match
                && (source_repo.starts_with(&with_dash)
                    || source_repo.starts_with(&with_colon)
                    || source_repo.starts_with(&with_underscore))
        })
        .collect()
}

fn build_robot_burndown_output(
    cli: &Cli,
    issues: &[bvr::model::Issue],
    sprint_id_or_current: &str,
) -> bvr::Result<RobotBurndownOutput> {
    let now = Utc::now();
    let sprints = load_sprints_for_cli(cli)?;

    let target = if sprint_id_or_current.eq_ignore_ascii_case("current") {
        sprints.iter().find(|sprint| sprint.is_active_at(now))
    } else {
        sprints
            .iter()
            .find(|sprint| sprint.id == sprint_id_or_current)
    };

    let Some(sprint) = target else {
        let message = if sprint_id_or_current.eq_ignore_ascii_case("current") {
            "no active sprint found".to_string()
        } else {
            format!("sprint not found: {sprint_id_or_current}")
        };
        return Err(bvr::BvrError::InvalidArgument(message));
    };

    let issue_map = issues
        .iter()
        .map(|issue| (issue.id.clone(), issue))
        .collect::<BTreeMap<_, _>>();
    let sprint_issues = sprint
        .bead_ids
        .iter()
        .filter_map(|id| issue_map.get(id).copied())
        .collect::<Vec<_>>();

    let total_issues = sprint_issues.len();
    let completed_issues = sprint_issues
        .iter()
        .filter(|issue| is_closed_status(&issue.status))
        .count();
    let remaining_issues = total_issues.saturating_sub(completed_issues);

    let (total_days, elapsed_days, remaining_days) =
        compute_sprint_day_stats(sprint.start_date, sprint.end_date, now);

    let ideal_burn_rate = if total_days > 0 {
        let total_u32 = u32::try_from(total_issues).unwrap_or(u32::MAX);
        let days_u32 = u32::try_from(total_days).unwrap_or(u32::MAX);
        f64::from(total_u32) / f64::from(days_u32)
    } else {
        0.0
    };

    let actual_burn_rate = if elapsed_days > 0 {
        let completed_u32 = u32::try_from(completed_issues).unwrap_or(u32::MAX);
        let elapsed_u32 = u32::try_from(elapsed_days).unwrap_or(u32::MAX);
        f64::from(completed_u32) / f64::from(elapsed_u32)
    } else {
        0.0
    };

    let mut on_track = true;
    let projected_complete = if actual_burn_rate > 0.0 && remaining_issues > 0 {
        let days_to_complete = remaining_issues.saturating_mul(elapsed_days) / completed_issues;
        let projected = now
            + Duration::days(i64::try_from(days_to_complete.saturating_add(1)).unwrap_or(i64::MAX));
        if let Some(end_date) = sprint.end_date {
            on_track = projected <= end_date;
        }
        Some(projected)
    } else if remaining_issues == 0 {
        on_track = true;
        None
    } else if elapsed_days > 0 && completed_issues == 0 {
        on_track = false;
        None
    } else {
        None
    };

    let daily_points = generate_daily_burndown_points(sprint, &sprint_issues, now);
    let ideal_line = generate_ideal_burndown_line(sprint, total_issues);
    let scope_changes = resolve_repo_root(cli)
        .and_then(|repo_root| {
            compute_sprint_scope_changes(&repo_root, sprint, &issue_map, now)
                .ok()
                .filter(|changes| !changes.is_empty())
        })
        .unwrap_or_default();

    Ok(RobotBurndownOutput {
        envelope: envelope(issues),
        sprint_id: sprint.id.clone(),
        sprint_name: sprint.name.clone(),
        start_date: sprint.start_date,
        end_date: sprint.end_date,
        total_days,
        elapsed_days,
        remaining_days,
        total_issues,
        completed_issues,
        remaining_issues,
        ideal_burn_rate,
        actual_burn_rate,
        projected_complete,
        on_track,
        daily_points,
        ideal_line,
        scope_changes,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct SprintSnapshot {
    id: String,
    #[serde(default)]
    bead_ids: Vec<String>,
}

#[derive(Debug)]
struct ScopeCommit {
    timestamp: DateTime<Utc>,
    order: usize,
    events: Vec<ScopeChangeCompat>,
}

fn compute_sprint_scope_changes(
    repo_root: &Path,
    sprint: &bvr::model::Sprint,
    issue_map: &BTreeMap<String, &bvr::model::Issue>,
    now: DateTime<Utc>,
) -> bvr::Result<Vec<ScopeChangeCompat>> {
    if sprint.id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let Some(start_date) = sprint.start_date else {
        return Ok(Vec::new());
    };
    let Some(end_date) = sprint.end_date else {
        return Ok(Vec::new());
    };
    if !repo_root.join(".git").exists() {
        return Ok(Vec::new());
    }

    let since = start_date - Duration::days(1);
    let until = if end_date > now { now } else { end_date };
    let sprint_file = format!(".beads/{}", loader::SPRINTS_FILE_NAME);

    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .arg("-c")
        .arg("color.ui=false")
        .arg("log")
        .arg("-p")
        .arg("-U0")
        .arg("--format=%H%x00%cI")
        .arg(format!("--since={}", since.to_rfc3339()))
        .arg(format!("--until={}", until.to_rfc3339()))
        .arg("--")
        .arg(sprint_file);

    let output = command.output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let mut commits = Vec::<ScopeCommit>::new();
    let text = String::from_utf8_lossy(&output.stdout);
    let mut current_timestamp = None::<DateTime<Utc>>;
    let mut current_order = 0usize;
    let mut old_snapshot = None::<SprintSnapshot>;
    let mut new_snapshot = None::<SprintSnapshot>;
    let mut next_order = 0usize;

    for line in text.lines() {
        if let Some(timestamp) = parse_scope_git_header_line(line) {
            push_scope_commit_if_changed(
                &mut commits,
                current_timestamp.take(),
                current_order,
                old_snapshot.take(),
                new_snapshot.take(),
                sprint,
                issue_map,
            );
            current_timestamp = Some(timestamp);
            current_order = next_order;
            next_order = next_order.saturating_add(1);
            continue;
        }

        if current_timestamp.is_none() {
            continue;
        }

        if let Some(stripped) = line.strip_prefix('-')
            && let Some(snapshot) = parse_sprint_snapshot_line(stripped)
            && snapshot.id == sprint.id
        {
            old_snapshot = Some(snapshot);
            continue;
        }

        if let Some(stripped) = line.strip_prefix('+')
            && let Some(snapshot) = parse_sprint_snapshot_line(stripped)
            && snapshot.id == sprint.id
        {
            new_snapshot = Some(snapshot);
        }
    }

    push_scope_commit_if_changed(
        &mut commits,
        current_timestamp,
        current_order,
        old_snapshot,
        new_snapshot,
        sprint,
        issue_map,
    );

    if commits.is_empty() {
        return Ok(Vec::new());
    }

    commits.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| right.order.cmp(&left.order))
    });

    let mut scope_changes = Vec::<ScopeChangeCompat>::new();
    for commit in commits {
        scope_changes.extend(commit.events);
    }

    Ok(scope_changes)
}

fn push_scope_commit_if_changed(
    commits: &mut Vec<ScopeCommit>,
    timestamp: Option<DateTime<Utc>>,
    order: usize,
    old_snapshot: Option<SprintSnapshot>,
    new_snapshot: Option<SprintSnapshot>,
    sprint: &bvr::model::Sprint,
    issue_map: &BTreeMap<String, &bvr::model::Issue>,
) {
    let Some(timestamp) = timestamp else {
        return;
    };
    let (Some(old_snapshot), Some(new_snapshot)) = (old_snapshot, new_snapshot) else {
        return;
    };
    if old_snapshot.id != sprint.id || new_snapshot.id != sprint.id {
        return;
    }

    let mut added = set_difference(&new_snapshot.bead_ids, &old_snapshot.bead_ids);
    let mut removed = set_difference(&old_snapshot.bead_ids, &new_snapshot.bead_ids);
    if added.is_empty() && removed.is_empty() {
        return;
    }
    added.sort();
    removed.sort();

    let mut events = Vec::<ScopeChangeCompat>::with_capacity(added.len() + removed.len());
    for issue_id in removed {
        let issue_title = issue_map
            .get(&issue_id)
            .map_or_else(String::new, |issue| issue.title.clone());
        events.push(ScopeChangeCompat {
            date: timestamp,
            issue_id,
            issue_title,
            action: "removed".to_string(),
        });
    }
    for issue_id in added {
        let issue_title = issue_map
            .get(&issue_id)
            .map_or_else(String::new, |issue| issue.title.clone());
        events.push(ScopeChangeCompat {
            date: timestamp,
            issue_id,
            issue_title,
            action: "added".to_string(),
        });
    }

    commits.push(ScopeCommit {
        timestamp,
        order,
        events,
    });
}

fn parse_scope_git_header_line(line: &str) -> Option<DateTime<Utc>> {
    let (sha, raw_timestamp) = line.split_once('\0')?;
    if sha.trim().is_empty() {
        return None;
    }
    parse_rfc3339_utc(raw_timestamp.trim())
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn parse_sprint_snapshot_line(line: &str) -> Option<SprintSnapshot> {
    let snapshot: SprintSnapshot = serde_json::from_str(line).ok()?;
    if snapshot.id.trim().is_empty() {
        return None;
    }
    Some(snapshot)
}

fn set_difference(left: &[String], right: &[String]) -> Vec<String> {
    let right_set = right
        .iter()
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    let mut output = Vec::<String>::new();
    for value in left {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !right_set.contains(trimmed) {
            output.push(trimmed.to_string());
        }
    }
    output
}

fn compute_sprint_day_stats(
    start_date: Option<DateTime<Utc>>,
    end_date: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (usize, usize, usize) {
    let Some(start_date) = start_date else {
        return (0, 0, 0);
    };
    let Some(end_date) = end_date else {
        return (0, 0, 0);
    };
    if end_date < start_date {
        return (0, 0, 0);
    }

    let total_days_i64 = ((end_date - start_date).num_hours() / 24).saturating_add(1);
    let total_days = usize::try_from(total_days_i64).unwrap_or(usize::MAX);

    if now < start_date {
        return (total_days, 0, total_days);
    }
    if now > end_date {
        return (total_days, total_days, 0);
    }

    let elapsed_days_i64 = ((now - start_date).num_hours() / 24).saturating_add(1);
    let elapsed_days = usize::try_from(elapsed_days_i64).unwrap_or(total_days);
    let remaining_days = total_days.saturating_sub(elapsed_days);
    (total_days, elapsed_days, remaining_days)
}

fn generate_daily_burndown_points(
    sprint: &bvr::model::Sprint,
    sprint_issues: &[&bvr::model::Issue],
    now: DateTime<Utc>,
) -> Vec<BurndownPointCompat> {
    let Some(start_date) = sprint.start_date else {
        return Vec::new();
    };
    let Some(end_date) = sprint.end_date else {
        return Vec::new();
    };

    let total_issues = i32::try_from(sprint_issues.len()).unwrap_or(i32::MAX);
    let mut points = Vec::<BurndownPointCompat>::new();
    let mut day = start_date;
    let upper_bound = if now > end_date && end_date > start_date {
        end_date - Duration::days(1)
    } else {
        now.min(end_date)
    };
    if upper_bound < start_date {
        return points;
    }

    while day <= upper_bound {
        let day_end = day + Duration::hours(24) - Duration::seconds(1);
        let completed_usize = sprint_issues
            .iter()
            .filter(|issue| {
                is_closed_status(&issue.status)
                    && issue_closed_at_or_sprint_start(issue, start_date) <= day_end
            })
            .count();
        let completed = i32::try_from(completed_usize).unwrap_or(i32::MAX);
        points.push(BurndownPointCompat {
            date: day,
            remaining: total_issues.saturating_sub(completed),
            completed,
        });
        day += Duration::days(1);
    }

    points
}

fn issue_closed_at_or_sprint_start(
    issue: &bvr::model::Issue,
    sprint_start: DateTime<Utc>,
) -> DateTime<Utc> {
    issue
        .closed_at
        .or(issue.updated_at)
        .or(issue.created_at)
        .unwrap_or(sprint_start)
}

fn generate_ideal_burndown_line(
    sprint: &bvr::model::Sprint,
    total_issues: usize,
) -> Vec<BurndownPointCompat> {
    let Some(start_date) = sprint.start_date else {
        return Vec::new();
    };
    let Some(end_date) = sprint.end_date else {
        return Vec::new();
    };
    if total_issues == 0 || end_date < start_date {
        return Vec::new();
    }

    let total_days_i64 = ((end_date - start_date).num_hours() / 24).saturating_add(1);
    if total_days_i64 <= 0 {
        return Vec::new();
    }

    let total_days = usize::try_from(total_days_i64).unwrap_or(usize::MAX);
    let total_issues_i32 = i32::try_from(total_issues).unwrap_or(i32::MAX);

    let mut line = Vec::<BurndownPointCompat>::new();
    for day_index in 0..=total_days {
        let burned = day_index.saturating_mul(total_issues) / total_days;
        let burned_i32 = i32::try_from(burned).unwrap_or(i32::MAX);
        let remaining = total_issues_i32.saturating_sub(burned_i32).max(0);
        line.push(BurndownPointCompat {
            date: start_date + Duration::days(i64::try_from(day_index).unwrap_or(i64::MAX)),
            remaining,
            completed: total_issues_i32.saturating_sub(remaining),
        });
    }

    line
}

fn build_robot_capacity_output(issues: &[bvr::model::Issue], cli: &Cli) -> RobotCapacityOutput {
    let target_issues = if let Some(label) = cli
        .capacity_label
        .as_deref()
        .filter(|label| !label.is_empty())
    {
        issues
            .iter()
            .filter(|issue| {
                issue
                    .labels
                    .iter()
                    .any(|entry| entry.eq_ignore_ascii_case(label))
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        issues.to_vec()
    };

    let issue_map = target_issues
        .iter()
        .map(|issue| (issue.id.clone(), issue))
        .collect::<BTreeMap<_, _>>();
    let open_issues = target_issues
        .iter()
        .filter(|issue| !is_closed_status(&issue.status))
        .collect::<Vec<_>>();
    let capacity_graph = bvr::analysis::graph::IssueGraph::build(issues);
    let capacity_metrics = capacity_graph.compute_metrics();
    let now = Utc::now();

    let eta_by_issue = open_issues
        .iter()
        .map(|issue| {
            let estimated = bvr::analysis::forecast::estimate_eta_for_issue(
                &target_issues,
                &capacity_graph,
                &capacity_metrics,
                &issue.id,
                1,
                now,
            )
            .map_or_else(
                || estimate_issue_minutes(issue),
                |eta| eta.estimated_minutes,
            );
            (issue.id.clone(), estimated)
        })
        .collect::<BTreeMap<_, _>>();

    let total_minutes = eta_by_issue.values().copied().sum::<i64>();

    let mut blocked_by = BTreeMap::<String, Vec<String>>::new();
    let mut blocks = BTreeMap::<String, Vec<String>>::new();
    for issue in &open_issues {
        for dep in &issue.dependencies {
            let dep_id = dep.depends_on_id.trim();
            if dep_id.is_empty() || !issue_map.contains_key(dep_id) {
                continue;
            }
            blocked_by
                .entry(issue.id.clone())
                .or_default()
                .push(dep_id.to_string());
            blocks
                .entry(dep_id.to_string())
                .or_default()
                .push(issue.id.clone());
        }
    }

    for ids in blocked_by.values_mut() {
        ids.sort();
        ids.dedup();
    }
    for ids in blocks.values_mut() {
        ids.sort();
        ids.dedup();
    }

    let mut actionable = open_issues
        .iter()
        .filter_map(|issue| {
            let has_open_blocker = blocked_by.get(&issue.id).is_some_and(|deps| {
                deps.iter().any(|dep_id| {
                    issue_map
                        .get(dep_id)
                        .is_some_and(|dep| !is_closed_status(&dep.status))
                })
            });
            if has_open_blocker {
                None
            } else {
                Some(issue.id.clone())
            }
        })
        .collect::<Vec<_>>();
    actionable.sort();

    let mut longest_chain = Vec::<String>::new();
    let mut path = Vec::<String>::new();
    let mut visiting = BTreeSet::<String>::new();
    for start in &actionable {
        dfs_capacity_chain(
            start,
            &issue_map,
            &blocks,
            &mut visiting,
            &mut path,
            &mut longest_chain,
        );
    }

    let serial_minutes = longest_chain
        .iter()
        .filter_map(|id| eta_by_issue.get(id).copied())
        .sum::<i64>();
    let parallel_minutes = total_minutes.saturating_sub(serial_minutes);
    let parallelizable_pct = if total_minutes == 0 {
        0.0
    } else {
        let parallel_i32 = i32::try_from(parallel_minutes).unwrap_or(i32::MAX);
        let total_i32 = i32::try_from(total_minutes).unwrap_or(i32::MAX);
        (f64::from(parallel_i32) / f64::from(total_i32)) * 100.0
    };

    let agents = cli.capacity_agents.max(1);
    let agents_i64 = i64::try_from(agents).unwrap_or(1);
    let effective_minutes = serial_minutes + parallel_minutes / agents_i64;
    let effective_i32 = i32::try_from(effective_minutes).unwrap_or(i32::MAX);
    let total_i32 = i32::try_from(total_minutes).unwrap_or(i32::MAX);
    let estimated_days = f64::from(effective_i32) / (60.0 * 8.0);

    let mut bottlenecks = open_issues
        .iter()
        .filter_map(|issue| {
            let blocked = blocks.get(&issue.id).cloned().unwrap_or_default();
            if blocked.len() <= 1 {
                return None;
            }
            Some(CapacityBottleneck {
                id: issue.id.clone(),
                title: issue.title.clone(),
                blocks_count: blocked.len(),
                blocks: blocked,
            })
        })
        .collect::<Vec<_>>();
    bottlenecks.sort_by(|left, right| {
        right
            .blocks_count
            .cmp(&left.blocks_count)
            .then_with(|| left.id.cmp(&right.id))
    });
    bottlenecks.truncate(5);

    RobotCapacityOutput {
        envelope: envelope(issues),
        agents,
        label: cli
            .capacity_label
            .as_ref()
            .filter(|label| !label.is_empty())
            .cloned(),
        open_issue_count: open_issues.len(),
        total_minutes,
        total_days: f64::from(total_i32) / (60.0 * 8.0),
        serial_minutes,
        parallel_minutes,
        parallelizable_pct,
        estimated_days,
        critical_path_length: longest_chain.len(),
        critical_path: longest_chain,
        actionable_count: actionable.len(),
        actionable,
        bottlenecks,
    }
}

fn dfs_capacity_chain(
    issue_id: &str,
    issue_map: &BTreeMap<String, &bvr::model::Issue>,
    blocks: &BTreeMap<String, Vec<String>>,
    visiting: &mut BTreeSet<String>,
    path: &mut Vec<String>,
    longest_chain: &mut Vec<String>,
) {
    if visiting.contains(issue_id) {
        return;
    }
    visiting.insert(issue_id.to_string());
    path.push(issue_id.to_string());

    if path.len() > longest_chain.len() {
        *longest_chain = path.clone();
    }

    if let Some(next_ids) = blocks.get(issue_id) {
        for next_id in next_ids {
            if issue_map
                .get(next_id)
                .is_some_and(|issue| !is_closed_status(&issue.status))
            {
                dfs_capacity_chain(next_id, issue_map, blocks, visiting, path, longest_chain);
            }
        }
    }

    path.pop();
    visiting.remove(issue_id);
}

fn estimate_issue_minutes(issue: &bvr::model::Issue) -> i64 {
    i64::from(issue.estimated_minutes.unwrap_or(60).max(1))
}

fn is_closed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    is_closed_like_status(&normalized)
}

fn build_robot_graph_output(
    issues: &[bvr::model::Issue],
    analyzer: &Analyzer,
    cli: &Cli,
    graph_format_override: Option<GraphFormat>,
) -> RobotGraphOutput {
    let graph_data = build_graph_export_data(issues, analyzer, cli);
    let graph_format = graph_format_override.unwrap_or(cli.graph_format);
    let format = graph_format_name(graph_format).to_string();

    let env = bvr::robot::RobotEnvelope {
        generated_at: chrono::Utc::now().to_rfc3339(),
        data_hash: graph_data.data_hash.clone(),
        output_format: "json".to_string(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
    };

    if graph_data.filtered_issues.is_empty() {
        return RobotGraphOutput {
            envelope: env,
            format,
            graph: None,
            nodes: 0,
            edges: 0,
            filters_applied: graph_data.filters_applied,
            explanation: GraphExplanation {
                what: "Empty graph - no issues match the filter criteria".to_string(),
                how_to_render: None,
                when_to_use: "Adjust filter parameters to include more issues".to_string(),
            },
            adjacency: None,
        };
    }

    let mut graph = None;
    let mut adjacency = None;
    let explanation = match graph_format {
        GraphFormat::Json => {
            adjacency = Some(build_graph_adjacency(
                &graph_data.filtered_issues,
                &graph_data.edges,
                &graph_data.pagerank,
            ));
            GraphExplanation {
                what: "Dependency graph as JSON adjacency list".to_string(),
                how_to_render: None,
                when_to_use: "When you need programmatic access to the graph structure".to_string(),
            }
        }
        GraphFormat::Dot => {
            graph = Some(generate_dot(
                &graph_data.filtered_issues,
                &graph_data.edges,
                &graph_data.pagerank,
                cli.graph_preset,
            ));
            GraphExplanation {
                what: "Dependency graph in Graphviz DOT format".to_string(),
                how_to_render: Some(
                    "Save to file.dot, run: dot -Tpng file.dot -o graph.png".to_string(),
                ),
                when_to_use:
                    "When you need a visual overview of dependencies for documentation or debugging"
                        .to_string(),
            }
        }
        GraphFormat::Mermaid => {
            graph = Some(generate_mermaid(
                &graph_data.filtered_issues,
                &graph_data.edges,
            ));
            GraphExplanation {
                what: "Dependency graph in Mermaid diagram format".to_string(),
                how_to_render: Some(
                    "Paste into any Markdown renderer that supports Mermaid, or use mermaid.live"
                        .to_string(),
                ),
                when_to_use:
                    "When you need an embeddable diagram for documentation or GitHub issues"
                        .to_string(),
            }
        }
    };

    RobotGraphOutput {
        envelope: env,
        format,
        graph,
        nodes: graph_data.filtered_issues.len(),
        edges: graph_data.edges.len(),
        filters_applied: graph_data.filters_applied,
        explanation,
        adjacency,
    }
}

struct GraphExportData {
    filtered_issues: Vec<bvr::model::Issue>,
    edges: Vec<GraphAdjacencyEdge>,
    filters_applied: BTreeMap<String, String>,
    data_hash: String,
    pagerank: std::collections::HashMap<String, f64>,
    critical_depth: std::collections::HashMap<String, usize>,
}

fn build_graph_export_data(
    issues: &[bvr::model::Issue],
    analyzer: &Analyzer,
    cli: &Cli,
) -> GraphExportData {
    let mut filtered_issues = filter_graph_issues(
        issues,
        cli.label.as_deref(),
        cli.graph_root.as_deref(),
        cli.graph_depth,
    );
    filtered_issues.sort_by(|left, right| left.id.cmp(&right.id));

    GraphExportData {
        edges: build_graph_edges(&filtered_issues),
        filters_applied: collect_graph_filters(cli),
        data_hash: compute_data_hash(issues),
        pagerank: analyzer.metrics.pagerank.clone(),
        critical_depth: analyzer.metrics.critical_depth.clone(),
        filtered_issues,
    }
}

fn collect_graph_filters(cli: &Cli) -> BTreeMap<String, String> {
    let mut filters = BTreeMap::<String, String>::new();
    if let Some(label) = cli
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        filters.insert("label".to_string(), label.to_string());
    }
    if let Some(root) = cli
        .graph_root
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        filters.insert("root".to_string(), root.to_string());
    }
    if cli.graph_depth > 0 {
        filters.insert("depth".to_string(), cli.graph_depth.to_string());
    }
    filters
}

fn filter_graph_issues(
    issues: &[bvr::model::Issue],
    label: Option<&str>,
    root: Option<&str>,
    depth: usize,
) -> Vec<bvr::model::Issue> {
    let mut filtered = if let Some(label) = label.map(str::trim).filter(|s| !s.is_empty()) {
        issues
            .iter()
            .filter(|issue| {
                issue
                    .labels
                    .iter()
                    .any(|entry| entry.eq_ignore_ascii_case(label))
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        issues.to_vec()
    };

    if let Some(root_id) = root.map(str::trim).filter(|s| !s.is_empty()) {
        filtered = extract_graph_subgraph(&filtered, root_id, depth);
    }

    filtered
}

fn extract_graph_subgraph(
    issues: &[bvr::model::Issue],
    root_id: &str,
    max_depth: usize,
) -> Vec<bvr::model::Issue> {
    let issue_map = issues
        .iter()
        .map(|issue| (issue.id.clone(), issue))
        .collect::<BTreeMap<_, _>>();

    let mut visited = BTreeSet::<String>::new();
    let mut queue = VecDeque::<(String, usize)>::new();
    queue.push_back((root_id.to_string(), 0));

    while let Some((id, depth)) = queue.pop_front() {
        if visited.contains(&id) {
            continue;
        }
        if max_depth > 0 && depth > max_depth {
            continue;
        }

        visited.insert(id.clone());
        let Some(issue) = issue_map.get(&id) else {
            continue;
        };

        for dep in &issue.dependencies {
            let depends_on = dep.depends_on_id.trim();
            if depends_on.is_empty() || visited.contains(depends_on) {
                continue;
            }
            queue.push_back((depends_on.to_string(), depth.saturating_add(1)));
        }
    }

    issues
        .iter()
        .filter(|issue| visited.contains(&issue.id))
        .cloned()
        .collect()
}

fn build_graph_edges(issues: &[bvr::model::Issue]) -> Vec<GraphAdjacencyEdge> {
    let issue_ids = issues
        .iter()
        .map(|issue| issue.id.as_str())
        .collect::<BTreeSet<_>>();

    let mut edge_set = BTreeSet::<(String, String, String)>::new();
    for issue in issues {
        for dep in &issue.dependencies {
            let depends_on = dep.depends_on_id.trim();
            if depends_on.is_empty() || !issue_ids.contains(depends_on) {
                continue;
            }

            let edge_type = if dep.is_blocking() {
                "blocks"
            } else {
                "related"
            };
            edge_set.insert((
                issue.id.clone(),
                depends_on.to_string(),
                edge_type.to_string(),
            ));
        }
    }

    edge_set
        .into_iter()
        .map(|(from, to, edge_type)| GraphAdjacencyEdge {
            from,
            to,
            edge_type,
        })
        .collect()
}

fn build_graph_adjacency(
    issues: &[bvr::model::Issue],
    edges: &[GraphAdjacencyEdge],
    pagerank: &std::collections::HashMap<String, f64>,
) -> GraphAdjacency {
    let nodes = issues
        .iter()
        .map(|issue| GraphAdjacencyNode {
            id: issue.id.clone(),
            title: issue.title.clone(),
            status: issue.status.clone(),
            priority: issue.priority,
            labels: issue.labels.clone(),
            pagerank: pagerank.get(&issue.id).copied(),
        })
        .collect::<Vec<_>>();

    GraphAdjacency {
        nodes,
        edges: edges.to_vec(),
    }
}

const fn graph_format_name(format: GraphFormat) -> &'static str {
    match format {
        GraphFormat::Json => "json",
        GraphFormat::Dot => "dot",
        GraphFormat::Mermaid => "mermaid",
    }
}

#[derive(Debug, Clone, Copy)]
enum StaticGraphFormat {
    Svg,
    Png,
}

#[derive(Debug, Clone, Copy)]
enum GraphExportTarget {
    Text(GraphFormat),
    Static(StaticGraphFormat),
}

#[derive(Debug, Clone)]
struct StaticGraphNode {
    id: String,
    title: String,
    status: String,
    priority: i32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct StaticGraphLayout {
    width: u32,
    height: u32,
    title: String,
    style: GraphStyle,
    preset: GraphPreset,
    filters: String,
    data_hash: String,
    nodes: Vec<StaticGraphNode>,
    edges: Vec<GraphAdjacencyEdge>,
}

#[derive(Debug, Clone, Copy)]
struct RgbaColor {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl RgbaColor {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

#[derive(Debug)]
struct PngCanvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl PngCanvas {
    fn new(width: usize, height: usize, background: RgbaColor) -> Self {
        let mut pixels = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = background.r;
            chunk[1] = background.g;
            chunk[2] = background.b;
            chunk[3] = background.a;
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: RgbaColor) {
        let Ok(x) = usize::try_from(x) else {
            return;
        };
        let Ok(y) = usize::try_from(y) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = (y * self.width + x) * 4;
        self.pixels[idx] = color.r;
        self.pixels[idx + 1] = color.g;
        self.pixels[idx + 2] = color.b;
        self.pixels[idx + 3] = color.a;
    }

    fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: RgbaColor) {
        let right = x.saturating_add(width);
        let bottom = y.saturating_add(height);
        for yy in y..bottom {
            for xx in x..right {
                self.set_pixel(xx, yy, color);
            }
        }
    }

    fn stroke_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: RgbaColor) {
        if width <= 0 || height <= 0 {
            return;
        }
        let right = x.saturating_add(width - 1);
        let bottom = y.saturating_add(height - 1);
        for xx in x..=right {
            self.set_pixel(xx, y, color);
            self.set_pixel(xx, bottom, color);
        }
        for yy in y..=bottom {
            self.set_pixel(x, yy, color);
            self.set_pixel(right, yy, color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: RgbaColor,
        dashed: bool,
        thick: bool,
    ) {
        let mut x = x0;
        let mut y = y0;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut step = 0_i32;

        loop {
            let draw = !dashed || ((step / 8) % 2 == 0);
            if draw {
                self.set_pixel(x, y, color);
                if thick {
                    self.set_pixel(x + 1, y, color);
                    self.set_pixel(x - 1, y, color);
                    self.set_pixel(x, y + 1, color);
                    self.set_pixel(x, y - 1, color);
                }
            }

            if x == x1 && y == y1 {
                break;
            }
            let e2 = err.saturating_mul(2);
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
            step = step.saturating_add(1);
        }
    }
}

const fn graph_preset_name(preset: GraphPreset) -> &'static str {
    match preset {
        GraphPreset::Compact => "compact",
        GraphPreset::Roomy => "roomy",
    }
}

const fn graph_style_name(style: GraphStyle) -> &'static str {
    match style {
        GraphStyle::Force => "force",
        GraphStyle::Grid => "grid",
    }
}

fn resolve_graph_export_target(path: &Path, fallback: GraphFormat) -> GraphExportTarget {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return GraphExportTarget::Text(fallback);
    };

    if extension.eq_ignore_ascii_case("json") {
        GraphExportTarget::Text(GraphFormat::Json)
    } else if extension.eq_ignore_ascii_case("dot") {
        GraphExportTarget::Text(GraphFormat::Dot)
    } else if extension.eq_ignore_ascii_case("mmd") || extension.eq_ignore_ascii_case("mermaid") {
        GraphExportTarget::Text(GraphFormat::Mermaid)
    } else if extension.eq_ignore_ascii_case("svg") {
        GraphExportTarget::Static(StaticGraphFormat::Svg)
    } else if extension.eq_ignore_ascii_case("png") {
        GraphExportTarget::Static(StaticGraphFormat::Png)
    } else {
        GraphExportTarget::Text(fallback)
    }
}

fn write_graph_export_snapshot(
    path: &Path,
    output: &RobotGraphOutput,
    title: Option<&str>,
    preset: GraphPreset,
    style: GraphStyle,
) -> bvr::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let payload = render_graph_export_snapshot(output, title, preset, style)?;
    fs::write(path, payload)?;
    Ok(())
}

fn render_graph_export_snapshot(
    output: &RobotGraphOutput,
    title: Option<&str>,
    preset: GraphPreset,
    style: GraphStyle,
) -> bvr::Result<String> {
    let title = title.map(str::trim).filter(|value| !value.is_empty());
    let preset_name = graph_preset_name(preset);
    let style_name = graph_style_name(style);

    match output.format.as_str() {
        "json" => {
            let mut line = serde_json::to_string_pretty(output)?;
            line.push('\n');
            Ok(line)
        }
        "dot" => {
            let graph = output
                .graph
                .clone()
                .unwrap_or_else(|| "digraph G {\n    // no matching issues\n}\n".to_string());
            let mut lines = Vec::<String>::new();
            if let Some(graph_title) = title {
                lines.push(format!("// {graph_title}"));
            }
            lines.push(format!("// preset: {preset_name}"));
            lines.push(format!("// style: {style_name}"));
            lines.push(graph);
            Ok(lines.join("\n"))
        }
        "mermaid" => {
            let graph = output
                .graph
                .clone()
                .unwrap_or_else(|| "graph TD\n    %% no matching issues\n".to_string());
            let mut lines = Vec::<String>::new();
            if let Some(graph_title) = title {
                lines.push(format!("%% {graph_title}"));
            }
            lines.push(format!("%% preset: {preset_name}"));
            lines.push(format!("%% style: {style_name}"));
            lines.push(graph);
            Ok(lines.join("\n"))
        }
        other => Err(bvr::error::BvrError::InvalidArgument(format!(
            "unsupported graph export format: {other}"
        ))),
    }
}

fn write_static_graph_export_snapshot(
    path: &Path,
    format: StaticGraphFormat,
    graph_data: &GraphExportData,
    title: Option<&str>,
    preset: GraphPreset,
    style: GraphStyle,
) -> bvr::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let layout = build_static_graph_layout(graph_data, title, preset, style);
    match format {
        StaticGraphFormat::Svg => {
            let payload = render_static_svg_snapshot(&layout);
            fs::write(path, payload)?;
            Ok(())
        }
        StaticGraphFormat::Png => render_static_png_snapshot(path, &layout),
    }
}

fn build_static_graph_layout(
    graph_data: &GraphExportData,
    title: Option<&str>,
    preset: GraphPreset,
    style: GraphStyle,
) -> StaticGraphLayout {
    let title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Graph Snapshot")
        .to_string();
    let filters = if graph_data.filters_applied.is_empty() {
        "none".to_string()
    } else {
        graph_data
            .filters_applied
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    match style {
        GraphStyle::Grid => build_grid_layout(graph_data, title, preset, style, filters),
        GraphStyle::Force => build_force_layout(graph_data, title, preset, style, filters),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn build_grid_layout(
    graph_data: &GraphExportData,
    title: String,
    preset: GraphPreset,
    style: GraphStyle,
    filters: String,
) -> StaticGraphLayout {
    let (node_w, node_h, col_gap, row_gap, padding, header_h) = match preset {
        GraphPreset::Compact => (168.0, 70.0, 62.0, 34.0, 28.0, 110.0),
        GraphPreset::Roomy => (192.0, 84.0, 92.0, 52.0, 36.0, 134.0),
    };

    let mut levels = BTreeMap::<usize, Vec<&bvr::model::Issue>>::new();
    for issue in &graph_data.filtered_issues {
        let level = graph_data
            .critical_depth
            .get(&issue.id)
            .copied()
            .unwrap_or(1)
            .max(1);
        levels.entry(level).or_default().push(issue);
    }

    let mut ordered_levels = levels.keys().copied().collect::<Vec<_>>();
    ordered_levels.sort_unstable();
    for issues in levels.values_mut() {
        issues.sort_by(|left, right| {
            let left_rank = graph_data
                .pagerank
                .get(&left.id)
                .copied()
                .unwrap_or_default();
            let right_rank = graph_data
                .pagerank
                .get(&right.id)
                .copied()
                .unwrap_or_default();
            let delta = (right_rank - left_rank).abs();
            if delta > 1e-9 {
                right_rank
                    .partial_cmp(&left_rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                left.id.cmp(&right.id)
            }
        });
    }

    let mut nodes = Vec::<StaticGraphNode>::new();
    let mut max_rows = 1_usize;
    for (column, level) in ordered_levels.iter().enumerate() {
        let Some(bucket) = levels.get(level) else {
            continue;
        };
        max_rows = max_rows.max(bucket.len());
        for (row, issue) in bucket.iter().enumerate() {
            let x = (column as f64).mul_add(node_w + col_gap, padding);
            let y = (row as f64).mul_add(node_h + row_gap, header_h + padding);
            nodes.push(StaticGraphNode {
                id: issue.id.clone(),
                title: truncate_runes(&issue.title, 34),
                status: issue.status.clone(),
                priority: issue.priority,
                x,
                y,
                width: node_w,
                height: node_h,
            });
        }
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));

    let width = if ordered_levels.is_empty() {
        760_u32
    } else {
        let columns = ordered_levels.len() as f64;
        let column_gaps = ordered_levels.len().saturating_sub(1) as f64;
        column_gaps
            .mul_add(col_gap, padding.mul_add(2.0, columns.mul_add(node_w, 0.0)))
            .ceil()
            .max(760.0) as u32
    };
    let rows = max_rows as f64;
    let row_gaps = max_rows.saturating_sub(1) as f64;
    let height = row_gaps
        .mul_add(
            row_gap,
            rows.mul_add(node_h, padding.mul_add(2.0, header_h)),
        )
        .ceil()
        .max(540.0) as u32;

    StaticGraphLayout {
        width,
        height,
        title,
        style,
        preset,
        filters,
        data_hash: graph_data.data_hash.clone(),
        nodes,
        edges: graph_data.edges.clone(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn build_force_layout(
    graph_data: &GraphExportData,
    title: String,
    preset: GraphPreset,
    style: GraphStyle,
    filters: String,
) -> StaticGraphLayout {
    let (width, height, header_h, node_w, node_h, margin) = match preset {
        GraphPreset::Compact => (980_u32, 720_u32, 112.0, 150.0, 66.0, 88.0),
        GraphPreset::Roomy => (1280_u32, 900_u32, 132.0, 176.0, 78.0, 112.0),
    };

    let mut ordered = graph_data.filtered_issues.clone();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));

    let mut nodes = Vec::<StaticGraphNode>::new();
    let count = ordered.len().max(1);
    let radius_base = (f64::from(width.min(height)) * 0.34).max(120.0);
    let cx = f64::from(width) / 2.0;
    let cy = header_h + (f64::from(height) - header_h) / 2.0;

    for (index, issue) in ordered.iter().enumerate() {
        let angle = std::f64::consts::TAU * (index as f64 / count as f64);
        let rank = graph_data
            .pagerank
            .get(&issue.id)
            .copied()
            .unwrap_or_default();
        let radial_jitter = rank.mul_add(45.0, 0.0).clamp(0.0, 62.0);
        let radius = radius_base + radial_jitter;
        let x = (cx + radius * angle.cos() - node_w / 2.0)
            .clamp(margin, f64::from(width) - margin - node_w);
        let y = (cy + radius * angle.sin() - node_h / 2.0)
            .clamp(header_h + 12.0, f64::from(height) - margin - node_h);
        nodes.push(StaticGraphNode {
            id: issue.id.clone(),
            title: truncate_runes(&issue.title, 34),
            status: issue.status.clone(),
            priority: issue.priority,
            x,
            y,
            width: node_w,
            height: node_h,
        });
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));

    StaticGraphLayout {
        width,
        height,
        title,
        style,
        preset,
        filters,
        data_hash: graph_data.data_hash.clone(),
        nodes,
        edges: graph_data.edges.clone(),
    }
}

fn render_static_svg_snapshot(layout: &StaticGraphLayout) -> String {
    let mut out = String::new();
    let style_name = graph_style_name(layout.style);
    let preset_name = graph_preset_name(layout.preset);
    let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
        layout.width, layout.height, layout.width, layout.height
    );
    let _ = writeln!(out, "<!-- format: svg -->");
    let _ = writeln!(out, "<!-- style: {style_name} -->");
    let _ = writeln!(out, "<!-- preset: {preset_name} -->");
    let _ = writeln!(
        out,
        "<!-- filters: {} -->",
        escape_xml_text(&layout.filters)
    );
    let _ = writeln!(out, "<!-- data_hash: {} -->", layout.data_hash);
    let _ = writeln!(
        out,
        "<!-- counts: nodes={} edges={} -->",
        layout.nodes.len(),
        layout.edges.len()
    );
    let _ = writeln!(
        out,
        "<rect x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"#F8FAFC\"/>",
        layout.width, layout.height
    );
    let _ = writeln!(
        out,
        "<rect x=\"0\" y=\"0\" width=\"{}\" height=\"82\" fill=\"#E2E8F0\"/>",
        layout.width
    );
    let _ = writeln!(
        out,
        "<text x=\"24\" y=\"34\" font-size=\"20\" font-weight=\"700\" fill=\"#0F172A\">{}</text>",
        escape_xml_text(&layout.title)
    );
    let _ = writeln!(
        out,
        "<text x=\"24\" y=\"60\" font-size=\"12\" fill=\"#334155\">style={} preset={} filters={}</text>",
        style_name,
        preset_name,
        escape_xml_text(&layout.filters)
    );

    let centers = layout
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                (
                    node.x + node.width / 2.0,
                    node.y + node.height / 2.0,
                    node.width,
                    node.height,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for edge in &layout.edges {
        let Some(&(from_x, from_y, _, _)) = centers.get(&edge.from) else {
            continue;
        };
        let Some(&(to_x, to_y, _, _)) = centers.get(&edge.to) else {
            continue;
        };
        let is_blocks = edge.edge_type == "blocks";
        let stroke = if is_blocks { "#E11D48" } else { "#64748B" };
        let width = if is_blocks { 2 } else { 1 };
        let dash = if is_blocks {
            String::new()
        } else {
            " stroke-dasharray=\"6 5\"".to_string()
        };
        let _ = writeln!(
            out,
            "<line x1=\"{from_x:.1}\" y1=\"{from_y:.1}\" x2=\"{to_x:.1}\" y2=\"{to_y:.1}\" stroke=\"{stroke}\" stroke-width=\"{width}\"{dash}/>"
        );
    }

    for node in &layout.nodes {
        let fill = dot_status_color(&node.status);
        let x = node.x;
        let y = node.y;
        let w = node.width;
        let h = node.height;
        let _ = writeln!(
            out,
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"8\" ry=\"8\" fill=\"{fill}\" stroke=\"#334155\" stroke-width=\"1\"/>"
        );
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" font-weight=\"700\" fill=\"#0F172A\">{}</text>",
            x + 10.0,
            y + 20.0,
            escape_xml_text(&truncate_runes(&node.id, 24))
        );
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"#0F172A\">{}</text>",
            x + 10.0,
            y + 38.0,
            escape_xml_text(&truncate_runes(&node.title, 30))
        );
        let _ = writeln!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10\" fill=\"#334155\">P{} {}</text>",
            x + 10.0,
            y + h - 12.0,
            node.priority,
            escape_xml_text(&node.status)
        );
    }

    let _ = writeln!(out, "</svg>");
    out
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn render_static_png_snapshot(path: &Path, layout: &StaticGraphLayout) -> bvr::Result<()> {
    let mut canvas = PngCanvas::new(
        layout.width as usize,
        layout.height as usize,
        RgbaColor::rgb(248, 250, 252),
    );
    canvas.fill_rect(
        0,
        0,
        i32::try_from(layout.width).unwrap_or(i32::MAX),
        82,
        RgbaColor::rgb(226, 232, 240),
    );

    let centers = layout
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                (
                    (node.x + node.width / 2.0).round() as i32,
                    (node.y + node.height / 2.0).round() as i32,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for edge in &layout.edges {
        let Some(&(from_x, from_y)) = centers.get(&edge.from) else {
            continue;
        };
        let Some(&(to_x, to_y)) = centers.get(&edge.to) else {
            continue;
        };
        let is_blocks = edge.edge_type == "blocks";
        let color = if is_blocks {
            RgbaColor::rgb(225, 29, 72)
        } else {
            RgbaColor::rgb(100, 116, 139)
        };
        canvas.draw_line(from_x, from_y, to_x, to_y, color, !is_blocks, is_blocks);
    }

    for node in &layout.nodes {
        let fill = status_fill_color(&node.status);
        let x = node.x.round() as i32;
        let y = node.y.round() as i32;
        let w = node.width.round() as i32;
        let h = node.height.round() as i32;
        canvas.fill_rect(x, y, w, h, fill);
        canvas.stroke_rect(x, y, w, h, RgbaColor::rgb(51, 65, 85));
    }

    let file = fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, layout.width, layout.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk("Title".to_string(), layout.title.clone())
        .map_err(|error| {
            bvr::error::BvrError::InvalidArgument(format!("png metadata write failed: {error}"))
        })?;
    encoder
        .add_text_chunk(
            "Style".to_string(),
            graph_style_name(layout.style).to_string(),
        )
        .map_err(|error| {
            bvr::error::BvrError::InvalidArgument(format!("png metadata write failed: {error}"))
        })?;
    encoder
        .add_text_chunk(
            "Preset".to_string(),
            graph_preset_name(layout.preset).to_string(),
        )
        .map_err(|error| {
            bvr::error::BvrError::InvalidArgument(format!("png metadata write failed: {error}"))
        })?;
    encoder
        .add_text_chunk(
            "Counts".to_string(),
            format!("nodes={},edges={}", layout.nodes.len(), layout.edges.len()),
        )
        .map_err(|error| {
            bvr::error::BvrError::InvalidArgument(format!("png metadata write failed: {error}"))
        })?;

    let mut writer = encoder.write_header().map_err(|error| {
        bvr::error::BvrError::InvalidArgument(format!("png header write failed: {error}"))
    })?;
    writer.write_image_data(&canvas.pixels).map_err(|error| {
        bvr::error::BvrError::InvalidArgument(format!("png data write failed: {error}"))
    })?;
    Ok(())
}

fn status_fill_color(status: &str) -> RgbaColor {
    let normalized = status.trim().to_ascii_lowercase();
    if is_closed_like_status(&normalized) {
        return RgbaColor::rgb(207, 216, 220);
    }
    match normalized.as_str() {
        "open" => RgbaColor::rgb(200, 230, 201),
        "in_progress" => RgbaColor::rgb(187, 222, 251),
        "blocked" => RgbaColor::rgb(255, 205, 210),
        _ => RgbaColor::rgb(255, 255, 255),
    }
}

fn escape_xml_text(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn generate_dot(
    issues: &[bvr::model::Issue],
    edges: &[GraphAdjacencyEdge],
    pagerank: &std::collections::HashMap<String, f64>,
    preset: GraphPreset,
) -> String {
    let mut out = String::new();
    out.push_str("digraph G {\n");
    out.push_str("    rankdir=LR;\n");
    match preset {
        GraphPreset::Compact => {
            out.push_str("    nodesep=0.35;\n");
            out.push_str("    ranksep=0.45;\n");
        }
        GraphPreset::Roomy => {
            out.push_str("    nodesep=0.75;\n");
            out.push_str("    ranksep=1.00;\n");
        }
    }
    out.push_str("    node [shape=box, fontname=\"Helvetica\", fontsize=10];\n");
    out.push_str("    edge [fontname=\"Helvetica\", fontsize=8];\n\n");

    for issue in issues {
        let raw_title = truncate_runes(&issue.title, 30);
        let title = escape_dot_string(&raw_title);
        let escaped_id = escape_dot_string(&issue.id);
        let color = dot_status_color(&issue.status);

        let label = format!(
            "{escaped_id}\\n{title}\\nP{} {}",
            issue.priority, issue.status
        );
        let penwidth = pagerank
            .get(&issue.id)
            .copied()
            .map_or(1.0, |value| value.mul_add(3.0, 1.0));

        let _ = writeln!(
            out,
            "    \"{}\" [label=\"{}\", fillcolor=\"{}\", style=filled, penwidth={penwidth:.1}];",
            escape_dot_string(&issue.id),
            label,
            color
        );
    }

    out.push('\n');

    for edge in edges {
        let (style, color) = if edge.edge_type == "blocks" {
            ("bold", "#E53935")
        } else {
            ("dashed", "#999999")
        };

        let _ = writeln!(
            out,
            "    \"{}\" -> \"{}\" [style={style}, color=\"{color}\"];",
            escape_dot_string(&edge.from),
            escape_dot_string(&edge.to)
        );
    }

    out.push_str("}\n");
    out
}

fn dot_status_color(status: &str) -> &'static str {
    let normalized = status.trim().to_ascii_lowercase();
    if is_closed_like_status(&normalized) {
        return "#CFD8DC";
    }
    match normalized.as_str() {
        "open" => "#C8E6C9",
        "in_progress" => "#BBDEFB",
        "blocked" => "#FFCDD2",
        _ => "#FFFFFF",
    }
}

fn is_closed_like_status(status: &str) -> bool {
    matches!(status, "closed" | "tombstone")
}

fn escape_dot_string(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' | '\r' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn truncate_runes(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }

    let runes = text.chars().collect::<Vec<_>>();
    if runes.len() <= max {
        return text.to_string();
    }
    if max <= 3 {
        return runes[..max].iter().collect();
    }

    let mut out = runes[..max - 3].iter().collect::<String>();
    out.push_str("...");
    out
}

fn generate_mermaid(issues: &[bvr::model::Issue], edges: &[GraphAdjacencyEdge]) -> String {
    let mut out = String::new();
    out.push_str("graph TD\n");
    out.push_str("    classDef open fill:#50FA7B,stroke:#333,color:#000\n");
    out.push_str("    classDef inprogress fill:#8BE9FD,stroke:#333,color:#000\n");
    out.push_str("    classDef blocked fill:#FF5555,stroke:#333,color:#000\n");
    out.push_str("    classDef closed fill:#6272A4,stroke:#333,color:#fff\n\n");

    let mut safe_ids = BTreeMap::<String, String>::new();
    let mut used = BTreeSet::<String>::new();
    for (index, issue) in issues.iter().enumerate() {
        let base = {
            let value = sanitize_mermaid_id(&issue.id);
            if value.is_empty() {
                "node".to_string()
            } else {
                value
            }
        };

        let mut candidate = base.clone();
        if used.contains(&candidate) {
            candidate = format!("{base}_{index}");
            let mut suffix = 1usize;
            while used.contains(&candidate) {
                candidate = format!("{base}_{index}_{suffix}");
                suffix = suffix.saturating_add(1);
            }
        }

        used.insert(candidate.clone());
        safe_ids.insert(issue.id.clone(), candidate);
    }

    for issue in issues {
        let Some(safe_id) = safe_ids.get(&issue.id) else {
            continue;
        };
        let safe_title = sanitize_mermaid_text(&issue.title);
        let safe_label_id = sanitize_mermaid_text(&issue.id);

        let _ = writeln!(out, "    {safe_id}[\"{safe_label_id}<br/>{safe_title}\"]");

        let normalized_status = issue.status.trim().to_ascii_lowercase();
        let class_name = if is_closed_like_status(&normalized_status) {
            Some("closed")
        } else {
            match normalized_status.as_str() {
                "open" => Some("open"),
                "in_progress" => Some("inprogress"),
                "blocked" => Some("blocked"),
                _ => None,
            }
        };

        if let Some(class_name) = class_name {
            let _ = writeln!(out, "    class {safe_id} {class_name}");
        }
    }

    out.push('\n');

    for edge in edges {
        let Some(from) = safe_ids.get(&edge.from) else {
            continue;
        };
        let Some(to) = safe_ids.get(&edge.to) else {
            continue;
        };
        let link_style = if edge.edge_type == "blocks" {
            "==>"
        } else {
            "-.->"
        };

        let _ = writeln!(out, "    {from} {link_style} {to}");
    }

    out
}

fn sanitize_mermaid_id(id: &str) -> String {
    id.chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '-' || *ch == '_')
        .collect::<String>()
}

fn sanitize_mermaid_text(text: &str) -> String {
    let replaced = text
        .replace('\"', "'")
        .replace('[', "(")
        .replace(']', ")")
        .replace('{', "(")
        .replace('}', ")")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "/")
        .replace('`', "'")
        .replace('\n', " ")
        .replace('\r', "");

    let mut cleaned = replaced
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    cleaned = cleaned.trim().to_string();

    let runes = cleaned.chars().collect::<Vec<_>>();
    if runes.len() > 40 {
        let mut short = runes[..37].iter().collect::<String>();
        short.push_str("...");
        short
    } else {
        cleaned
    }
}

fn print_version() {
    let pkg = env!("CARGO_PKG_VERSION");
    let rustc = option_env!("VERGEN_RUSTC_SEMVER").unwrap_or("unknown-rustc");
    let target = option_env!("VERGEN_CARGO_TARGET_TRIPLE").unwrap_or("unknown-target");
    let ts = option_env!("VERGEN_BUILD_TIMESTAMP").unwrap_or("unknown-build-ts");
    eprintln!("bvr {pkg} ({rustc}, {target}, {ts})");
}

fn print_robot_help() {
    println!("Using bvr as an AI sidecar\n");
    println!("Use only --robot-* flags in automation contexts.");
    println!("Commands:");
    println!("  --robot-triage            Unified triage payload");
    println!("  --robot-next              Single top recommendation");
    println!("  --robot-overview          Compact orientation snapshot");
    println!("  --robot-triage-by-track   Triage grouped by parallel execution track");
    println!("  --robot-triage-by-label   Triage grouped by label/domain");
    println!("  --robot-plan              Dependency-aware execution tracks");
    println!("  --robot-insights          Graph-centric insight payload");
    println!("  --robot-priority          Priority recommendation payload");
    println!("  --robot-alerts            Drift + proactive alerts payload");
    println!("  --robot-suggest           Smart duplicate/dependency/label/cycle suggestions");
    println!("  --robot-diff              Snapshot diff (requires --diff-since)");
    println!(
        "  --robot-history           Issue-level timeline view (--history-since, --history-limit, --min-confidence)"
    );
    println!("  --robot-burndown <id|current> Sprint burndown data");
    println!("  --robot-capacity          Capacity simulation output");
    println!("  --robot-graph             Graph export (json|dot|mermaid)");
    println!(
        "  --export-graph <file>     Write graph snapshot to file (.json|.dot|.mmd|.svg|.png)"
    );
    println!("  --graph-title <text>      Optional title comment for exported graph files");
    println!("  --graph-preset <preset>   Graph layout spacing preset: compact (default) or roomy");
    println!("  --graph-style <style>     Static snapshot layout style: force (default) or grid");
    println!(
        "  --robot-forecast <id|all> ETA forecast (--forecast-label, --forecast-sprint, --forecast-agents)"
    );
    println!(
        "  --robot-docs <topic>      Machine-readable docs (guide|commands|examples|env|exit-codes|all)"
    );
    println!(
        "  --robot-schema            JSON Schema definitions for all commands (--schema-command <cmd>)"
    );
    println!(
        "  --format json|toon        Structured output format (env: BV_OUTPUT_FORMAT, TOON_DEFAULT_FORMAT)"
    );
    println!("  --robot-sprint-list       List all sprints as JSON");
    println!("  --robot-sprint-show <id>  Show specific sprint details");
    println!("  --robot-metrics           Performance metrics (timing, cache, memory)");
    println!("  --robot-label-health      Per-label health, velocity, and staleness");
    println!("  --robot-label-flow        Cross-label dependency flow matrix");
    println!("  --robot-label-attention   Attention-ranked labels");
    println!("  --robot-explain-correlation <sha:bead> Explain a history correlation");
    println!("  --robot-confirm-correlation <sha:bead> Confirm a history correlation");
    println!("  --robot-reject-correlation <sha:bead> Reject a history correlation");
    println!("  --robot-correlation-stats Show stored correlation feedback stats");
    println!("  --robot-orphans           Detect repo files not covered by bead history");
    println!("  --robot-file-beads <path> Find beads related to a file");
    println!("  --robot-file-hotspots     Rank hotspot files from history evidence");
    println!("  --robot-impact <paths>    Analyze issue impact of changed files");
    println!("  --robot-file-relations <path> Find related files by history overlap");
    println!("  --robot-related <bead>    Find related work for a bead");
    println!("  --robot-blocker-chain <bead> Show upstream blocker chain");
    println!("  --robot-impact-network <bead> Build causal impact network");
    println!("  --robot-causality <bead>  Build causality chain for a bead");
    println!("  --robot-drift             Compare current state to saved baseline");
    println!("  --robot-search            Search beads (--search <query>)");
    println!("  --robot-recipes           List available recipe filters");
    println!("  --profile-startup         Output detailed startup timing profile");
    println!(
        "  --profile-json            Output profile in JSON format (use with --profile-startup)"
    );
    println!("  --export-md <file>        Export issues to a Markdown report");
    println!("  --export-pages <dir>      Export static pages bundle (index + data + assets)");
    println!(
        "  --preview-pages <dir>     Preview static pages bundle at localhost with optional live reload"
    );
    println!("  --watch-export            Regenerate pages export when beads data changes");
    println!("  --pages                   Show pages wizard guidance");
    println!("  --pages-title <title>     Custom title for exported pages");
    println!("  --pages-include-closed=<bool>  Include closed issues in pages export");
    println!("  --pages-include-history=<bool> Include history payload in pages export");
    println!("  --no-live-reload          Disable live reload while previewing pages");
    println!("  --background-mode         Enable experimental background snapshot mode (TUI only)");
    println!(
        "  --no-background-mode      Disable experimental background snapshot mode (TUI only)"
    );
    println!("  --no-hooks                Skip hook execution for export workflows");
    println!("  --as-of <ref>             View state at point in time (commit, tag, date)");
    println!("  --force-full-analysis     Compute all metrics regardless of graph size");
    println!("  --stats                   Show format token estimates on stderr");
}

#[derive(Debug, Serialize)]
struct RobotOverviewOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    summary: RobotOverviewSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_pick: Option<RobotOverviewPick>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_blocker: Option<RobotOverviewBlocker>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    top_labels: Vec<RobotOverviewLabelCount>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fronts: Vec<RobotOverviewFront>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unlock_maximizers: Vec<RobotOverviewUnlockMaximizer>,
    commands: RobotOverviewCommands,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotOverviewFront {
    label: String,
    open_count: usize,
    representative: RobotOverviewPick,
}

#[derive(Debug, Serialize)]
struct RobotOverviewUnlockMaximizer {
    id: String,
    title: String,
    marginal_unlocks: usize,
    cumulative_unlocks: usize,
    claim_command: String,
}

#[derive(Debug, Serialize)]
struct RobotOverviewSummary {
    open_issues: usize,
    actionable_issues: usize,
    blocked_issues: usize,
    in_progress_issues: usize,
    closed_issues: usize,
    cycle_count: usize,
}

#[derive(Debug, Serialize)]
struct RobotOverviewPick {
    id: String,
    title: String,
    score: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<String>,
    claim_command: String,
}

#[derive(Debug, Serialize)]
struct RobotOverviewBlocker {
    id: String,
    title: String,
    unblocks: usize,
    show_command: String,
}

#[derive(Debug, Serialize)]
struct RobotOverviewLabelCount {
    label: String,
    open_issues: usize,
}

#[derive(Debug, Serialize)]
struct RobotOverviewCommands {
    next: String,
    triage: String,
    plan: String,
    history: String,
}

fn build_robot_overview_output(
    issues: &[bvr::model::Issue],
    analyzer: &Analyzer,
    triage: &bvr::analysis::triage::TriageResult,
) -> RobotOverviewOutput {
    let mut label_counts = BTreeMap::<String, usize>::new();
    let mut blocked_issues = 0usize;
    let mut in_progress_issues = 0usize;
    let mut closed_issues = 0usize;

    for issue in issues {
        if issue.is_closed_like() {
            closed_issues = closed_issues.saturating_add(1);
            continue;
        }

        if issue.normalized_status() == "blocked" {
            blocked_issues = blocked_issues.saturating_add(1);
        }
        if issue.is_in_progress() {
            in_progress_issues = in_progress_issues.saturating_add(1);
        }

        for label in &issue.labels {
            let label = label.trim();
            if label.is_empty() {
                continue;
            }
            *label_counts.entry(label.to_string()).or_insert(0) += 1;
        }
    }

    let mut top_labels = label_counts
        .into_iter()
        .map(|(label, open_issues)| RobotOverviewLabelCount { label, open_issues })
        .collect::<Vec<_>>();
    top_labels.sort_by(|left, right| {
        right
            .open_issues
            .cmp(&left.open_issues)
            .then_with(|| left.label.cmp(&right.label))
    });
    top_labels.truncate(5);

    let top_pick = triage
        .quick_ref
        .top_picks
        .first()
        .map(|pick| RobotOverviewPick {
            id: pick.id.clone(),
            title: pick.title.clone(),
            score: pick.score,
            reasons: pick.reasons.clone(),
            claim_command: format!("br update {} --status=in_progress", pick.id),
        });

    let top_blocker = triage
        .blockers_to_clear
        .first()
        .map(|blocker| RobotOverviewBlocker {
            id: blocker.id.clone(),
            title: blocker.title.clone(),
            unblocks: blocker.unblocks,
            show_command: format!("br show {}", blocker.id),
        });

    // Build diverse work fronts from label-grouped recommendations.
    // Each front represents a distinct area of the graph with its own top pick,
    // capped at 8 fronts for compact output.
    let top_pick_id = triage
        .quick_ref
        .top_picks
        .first()
        .map(|p| p.id.clone())
        .unwrap_or_default();

    let mut fronts: Vec<RobotOverviewFront> = triage
        .recommendations_by_label
        .iter()
        .filter_map(|by_label| {
            let rec = by_label.top_pick.as_ref()?;
            // Skip fronts whose representative duplicates the global top pick.
            if rec.id == top_pick_id {
                return None;
            }
            Some(RobotOverviewFront {
                label: by_label.label.clone(),
                open_count: by_label.item_ids.len(),
                representative: RobotOverviewPick {
                    id: rec.id.clone(),
                    title: rec.title.clone(),
                    score: rec.score,
                    reasons: rec.reasons.clone(),
                    claim_command: format!("br update {} --status=in_progress", rec.id),
                },
            })
        })
        .collect();

    // Sort by open count descending so the largest active areas come first.
    fronts.sort_by(|left, right| {
        right
            .open_count
            .cmp(&left.open_count)
            .then_with(|| left.label.cmp(&right.label))
    });
    fronts.truncate(8);

    // Unlock-maximizers come from a different objective than triage: greedy
    // submodular selection over downstream-unlock coverage rather than the
    // composite triage score. Items high on unlock impact but low on triage
    // score (e.g. broad task items that unblock many descendants without
    // scoring highly on centrality) are invisible to triage-only surfaces
    // like `--robot-triage` and its `by-label`/`by-track` regroupings.
    // Including them here ensures orient output combines both objectives
    // in one compact payload. See issue #4 for the SL-000 case study.
    let already_surfaced: BTreeSet<String> = core::iter::empty::<String>()
        .chain(top_pick.as_ref().map(|p| p.id.clone()))
        .chain(fronts.iter().map(|f| f.representative.id.clone()))
        .collect();
    // Call the single-purpose `top_k_unlock_set` helper rather than
    // `advanced_insights()` — the latter also computes coverage, k-paths,
    // cycle-break, parallel-cut, and parallel-gain, none of which we use
    // here. On large graphs the unused computations add noticeable latency
    // to every `--robot-overview` / `--robot-orient` invocation.
    let top_k = analyzer.top_k_unlock_set(10);
    let issue_title_by_id: BTreeMap<&str, &str> = issues
        .iter()
        .map(|issue| (issue.id.as_str(), issue.title.as_str()))
        .collect();
    let unlock_maximizers: Vec<RobotOverviewUnlockMaximizer> = top_k
        .items
        .iter()
        .filter(|item| !already_surfaced.contains(&item.id))
        .take(5)
        .map(|item| RobotOverviewUnlockMaximizer {
            id: item.id.clone(),
            title: issue_title_by_id
                .get(item.id.as_str())
                .map(|t| (*t).to_string())
                .unwrap_or_default(),
            marginal_unlocks: item.marginal_unlocks,
            cumulative_unlocks: item.cumulative_unlocks,
            claim_command: format!("br update {} --status=in_progress", item.id),
        })
        .collect();

    RobotOverviewOutput {
        envelope: envelope(issues),
        summary: RobotOverviewSummary {
            open_issues: triage.quick_ref.total_open,
            actionable_issues: triage.quick_ref.total_actionable,
            blocked_issues,
            in_progress_issues,
            closed_issues,
            cycle_count: analyzer.metrics.cycles.len(),
        },
        top_pick,
        top_blocker,
        top_labels,
        fronts,
        unlock_maximizers,
        commands: RobotOverviewCommands {
            next: "bvr --robot-next".to_string(),
            triage: "bvr --robot-triage".to_string(),
            plan: "bvr --robot-plan".to_string(),
            history: "bvr --robot-history --history-limit 20".to_string(),
        },
        usage_hints: vec![
            "jq '.summary'".to_string(),
            "jq '.top_pick'".to_string(),
            "jq '.fronts[] | {label, id: .representative.id}'".to_string(),
            "bvr --robot-triage".to_string(),
        ],
    }
}

#[derive(Debug, Serialize)]
struct RobotTriageOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of_commit: Option<String>,
    triage: bvr::analysis::triage::TriageResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    feedback: Option<bvr::analysis::recipe::FeedbackStats>,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotNextOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unblocks: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    show_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct RobotPlanOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_context: Option<bvr::analysis::label_intel::LabelHealth>,
    status: MetricStatus,
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    plan: bvr::analysis::plan::ExecutionPlan,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FullStatsNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pagerank: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    betweenness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eigenvector: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hits_hub: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hits_authority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kcore: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    critical_depth: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slack: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocks_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_by_count: Option<usize>,
    is_articulation_point: bool,
}

fn build_full_stats(
    metrics: &bvr::analysis::graph::GraphMetrics,
) -> BTreeMap<String, FullStatsNode> {
    let mut all_ids = BTreeSet::new();
    for id in metrics.pagerank.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.betweenness.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.eigenvector.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.hubs.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.authorities.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.k_core.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.critical_depth.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.slack.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.blocks_count.keys() {
        all_ids.insert(id.clone());
    }
    for id in metrics.blocked_by_count.keys() {
        all_ids.insert(id.clone());
    }

    all_ids
        .into_iter()
        .map(|id| {
            let node = FullStatsNode {
                pagerank: metrics.pagerank.get(&id).copied(),
                betweenness: metrics.betweenness.get(&id).copied(),
                eigenvector: metrics.eigenvector.get(&id).copied(),
                hits_hub: metrics.hubs.get(&id).copied(),
                hits_authority: metrics.authorities.get(&id).copied(),
                kcore: metrics.k_core.get(&id).copied(),
                critical_depth: metrics.critical_depth.get(&id).copied(),
                slack: metrics.slack.get(&id).copied(),
                blocks_count: metrics.blocks_count.get(&id).copied(),
                blocked_by_count: metrics.blocked_by_count.get(&id).copied(),
                is_articulation_point: metrics.articulation_points.contains(&id),
            };
            (id, node)
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct RobotInsightsOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_context: Option<bvr::analysis::label_intel::LabelHealth>,
    #[serde(rename = "Stats")]
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    #[serde(rename = "analysis_config")]
    analysis_config_compat: bvr::analysis::graph::AnalysisConfig,
    #[serde(flatten)]
    insights: Insights,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_stats: Option<BTreeMap<String, FullStatsNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_what_ifs: Option<Vec<bvr::analysis::whatif::WhatIfDelta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    advanced_insights: Option<bvr::analysis::advanced::AdvancedInsights>,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotGraphOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<String>,
    nodes: usize,
    edges: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    filters_applied: BTreeMap<String, String>,
    explanation: GraphExplanation,
    #[serde(skip_serializing_if = "Option::is_none")]
    adjacency: Option<GraphAdjacency>,
}

#[derive(Debug, Serialize)]
struct GraphExplanation {
    what: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    how_to_render: Option<String>,
    when_to_use: String,
}

#[derive(Debug, Serialize)]
struct GraphAdjacency {
    nodes: Vec<GraphAdjacencyNode>,
    edges: Vec<GraphAdjacencyEdge>,
}

#[derive(Debug, Serialize)]
struct GraphAdjacencyNode {
    id: String,
    title: String,
    status: String,
    priority: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagerank: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct GraphAdjacencyEdge {
    from: String,
    to: String,
    #[serde(rename = "type")]
    edge_type: String,
}

#[derive(Debug, Serialize)]
struct PriorityFilterOutput {
    min_confidence: f64,
    max_results: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    by_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    by_assignee: Option<String>,
}

#[derive(Debug, Serialize)]
struct PrioritySummaryOutput {
    total_issues: usize,
    recommendations: usize,
    high_confidence: usize,
}

#[derive(Debug, Serialize)]
struct RobotPriorityOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label_context: Option<bvr::analysis::label_intel::LabelHealth>,
    status: MetricStatus,
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    recommendations: Vec<bvr::analysis::triage::Recommendation>,
    field_descriptions: BTreeMap<&'static str, &'static str>,
    filters: PriorityFilterOutput,
    summary: PrioritySummaryOutput,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotDiffOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    resolved_revision: String,
    from_data_hash: String,
    to_data_hash: String,
    diff: bvr::analysis::diff::SnapshotDiff,
}

#[derive(Debug, Serialize)]
struct RobotHistoryOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    bead_history: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "histories_timeline")]
    histories_timeline: Option<Vec<bvr::analysis::history::IssueHistory>>,
    git_range: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_commit_sha: Option<String>,
    stats: HistoryStatsCompat,
    #[serde(rename = "histories")]
    histories_map: BTreeMap<String, HistoryBeadCompat>,
    commit_index: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Serialize)]
struct RobotBurndownOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    sprint_id: String,
    sprint_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_date: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_date: Option<DateTime<Utc>>,
    total_days: usize,
    elapsed_days: usize,
    remaining_days: usize,
    total_issues: usize,
    completed_issues: usize,
    remaining_issues: usize,
    ideal_burn_rate: f64,
    actual_burn_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    projected_complete: Option<DateTime<Utc>>,
    on_track: bool,
    daily_points: Vec<BurndownPointCompat>,
    ideal_line: Vec<BurndownPointCompat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scope_changes: Vec<ScopeChangeCompat>,
}

#[derive(Debug, Serialize)]
struct BurndownPointCompat {
    date: DateTime<Utc>,
    remaining: i32,
    completed: i32,
}

#[derive(Debug, Serialize)]
struct ScopeChangeCompat {
    date: DateTime<Utc>,
    issue_id: String,
    issue_title: String,
    action: String,
}

#[derive(Debug, Serialize)]
struct RobotCapacityOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    agents: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    open_issue_count: usize,
    total_minutes: i64,
    total_days: f64,
    serial_minutes: i64,
    parallel_minutes: i64,
    parallelizable_pct: f64,
    estimated_days: f64,
    critical_path_length: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    critical_path: Vec<String>,
    actionable_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actionable: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bottlenecks: Vec<CapacityBottleneck>,
}

#[derive(Debug, Serialize)]
struct CapacityBottleneck {
    id: String,
    title: String,
    blocks_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotForecastOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    agents: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    filters: BTreeMap<String, String>,
    forecast_count: usize,
    forecasts: Vec<RobotForecastItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<RobotForecastSummary>,
}

#[derive(Debug, Serialize)]
struct RobotForecastItem {
    issue_id: String,
    estimated_minutes: i64,
    estimated_days: f64,
    eta_date: String,
    eta_date_low: String,
    eta_date_high: String,
    confidence: f64,
    velocity_minutes_per_day: f64,
    agents: usize,
    factors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotForecastSummary {
    total_minutes: i64,
    total_days: f64,
    avg_confidence: f64,
    earliest_eta: String,
    latest_eta: String,
}

#[derive(Debug, Serialize)]
struct RobotSprintListOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    sprint_count: usize,
    sprints: Vec<bvr::model::Sprint>,
}

#[derive(Debug, Serialize)]
struct RobotSprintShowOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    sprint: bvr::model::Sprint,
}

#[derive(Debug, Serialize)]
struct RobotMetricsOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    timing: Vec<MetricsTiming>,
    cache: Vec<MetricsCache>,
    memory: MetricsMemory,
}

#[derive(Debug, Serialize)]
struct MetricsTiming {
    name: String,
    count: u64,
    total_ms: f64,
    avg_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Serialize)]
struct MetricsCache {
    name: String,
    hits: u64,
    misses: u64,
    total: u64,
    hit_rate: f64,
}

#[derive(Debug, Serialize)]
struct MetricsMemory {
    rss_mb: f64,
}

impl MetricsMemory {
    fn current() -> Self {
        // Basic RSS estimation from /proc/self/statm (Linux)
        let rss_mb = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map_or(0.0, |pages| {
                f64::from(u32::try_from(pages).unwrap_or(u32::MAX)) * 4096.0 / (1024.0 * 1024.0)
            });
        Self { rss_mb }
    }
}

#[derive(Debug, Serialize)]
struct RobotLabelHealthOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    results: RobotLabelHealthResultsOutput,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotLabelFlowOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    flow: bvr::analysis::label_intel::CrossLabelFlow,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotLabelAttentionOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    limit: usize,
    labels: Vec<RobotLabelAttentionScoreOutput>,
    total_labels: usize,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotLabelHealthResultsOutput {
    generated_at: String,
    #[serde(flatten)]
    result: bvr::analysis::label_intel::LabelHealthResult,
}

#[derive(Debug, Serialize)]
struct RobotLabelAttentionScoreOutput {
    rank: usize,
    label: String,
    attention_score: f64,
    normalized_score: f64,
    reason: String,
    open_count: usize,
    blocked_count: usize,
    stale_count: usize,
    pagerank_sum: f64,
    velocity_factor: f64,
}

impl From<bvr::analysis::label_intel::LabelAttentionScore> for RobotLabelAttentionScoreOutput {
    fn from(value: bvr::analysis::label_intel::LabelAttentionScore) -> Self {
        Self {
            rank: value.rank,
            label: value.label,
            attention_score: value.attention_score,
            normalized_score: value.normalized_score,
            reason: value.reason,
            open_count: value.open_count,
            blocked_count: value.blocked_count,
            stale_count: value.stale_count,
            pagerank_sum: value.pagerank_sum,
            velocity_factor: value.velocity_factor,
        }
    }
}

// =========================================================================
// Startup Profile
// =========================================================================

#[derive(Debug, Clone, Serialize)]
struct StartupProfile {
    node_count: usize,
    edge_count: usize,
    density: f64,
    load_jsonl: String,
    build_graph: String,
    triage: String,
    insights: String,
    total: String,
    cycle_count: usize,
    bottleneck_count: usize,
    recommendation_count: usize,
}

#[derive(Debug, Serialize)]
struct ProfileJsonOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    profile: StartupProfile,
    total_with_load: String,
    recommendations: Vec<String>,
}

fn format_duration_ms(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1.0 {
        format!("{ms:.3}ms")
    } else if ms < 1000.0 {
        format!("{ms:.1}ms")
    } else {
        format!("{:.2}s", ms / 1000.0)
    }
}

fn generate_profile_recommendations(
    profile: &StartupProfile,
    total: std::time::Duration,
) -> Vec<String> {
    let mut recs = Vec::new();
    let total_ms = total.as_secs_f64() * 1000.0;

    if total_ms > 200.0 {
        recs.push("Total startup exceeds 200ms; consider async loading".to_string());
    }
    if profile.node_count > 500 {
        recs.push(format!(
            "Large dataset ({} issues); graph algorithms may benefit from parallelism",
            profile.node_count
        ));
    }
    if profile.cycle_count > 0 {
        recs.push(format!(
            "{} cycle(s) detected; resolving cycles improves graph analysis accuracy",
            profile.cycle_count
        ));
    }
    if profile.density > 0.1 {
        recs.push(format!(
            "High dependency density ({:.4}); consider pruning transitive edges",
            profile.density
        ));
    }
    if recs.is_empty() {
        recs.push("No performance concerns detected".to_string());
    }
    recs
}

fn print_profile_report(profile: &StartupProfile, recommendations: &[String]) {
    println!("Startup Profile");
    println!("===============");
    println!(
        "Data: {} issues, {} dependencies, density={:.4}\n",
        profile.node_count, profile.edge_count, profile.density
    );
    println!("Phase 1 (blocking):");
    println!("  Load JSONL:      {}", profile.load_jsonl);
    println!("  Build graph:     {}", profile.build_graph);
    println!();
    println!("Phase 2 (analysis):");
    println!("  Triage:          {}", profile.triage);
    println!("  Insights:        {}", profile.insights);
    println!();
    println!("Total startup:     {}", profile.total);
    println!();
    println!("Results:");
    println!("  Recommendations: {}", profile.recommendation_count);
    println!("  Bottlenecks:     {}", profile.bottleneck_count);
    println!("  Cycles:          {}", profile.cycle_count);
    println!();
    if !recommendations.is_empty() {
        println!("Recommendations:");
        for rec in recommendations {
            println!("  - {rec}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use bvr::analysis::git_history::{
        HistoryBeadCompat, HistoryCommitCompat, HistoryEventCompat, HistoryFileChangeCompat,
        HistoryMilestonesCompat, extract_ids_from_message,
    };
    use clap::Parser;
    use tempfile::tempdir;

    use super::{
        BackgroundModeSource, BvrCommand, Cli, IssueLoadTarget, actionable_ids_for_recipe_filters,
        build_background_mode_config, compare_versions, compute_related_work_result,
        discover_workspace_config_from_starts, feedback_project_dir, file_watch_token,
        filter_by_repo, generate_daily_burndown_points, handle_operational_commands, load_issues,
        parse_background_mode_bool, parse_crates_io_latest_version, parse_github_highest_tag,
        parse_github_latest_release, parse_scope_git_header_line, project_dir_for_export_hooks,
        reconcile_watch_export_paths, resolve_background_mode, resolve_cli_path_from_project_dir,
        resolve_cli_reference_file_path, resolve_git_toplevel, resolve_issue_load_target,
        resolve_reference_file_path, resolve_watch_export_paths, resolve_workspace_config_path,
        upgrade_cargo_args,
    };

    struct CurrentDirGuard {
        original: PathBuf,
        _lock: MutexGuard<'static, ()>,
    }

    impl CurrentDirGuard {
        fn lock() -> MutexGuard<'static, ()> {
            static CURRENT_DIR_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
            CURRENT_DIR_MUTEX
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn set(path: &Path) -> Self {
            let lock = Self::lock();
            let original = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
            std::env::set_current_dir(path).expect("set current dir");
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            if std::env::set_current_dir(&self.original).is_err() {
                std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("restore current dir");
            }
        }
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn make_history(bead_id: &str, status: &str, files: &[&str]) -> HistoryBeadCompat {
        let commits = vec![HistoryCommitCompat {
            sha: format!("sha-{bead_id}"),
            short_sha: "abc123".to_string(),
            message: format!("work on {bead_id}"),
            author: "TestUser".to_string(),
            author_email: "test@example.com".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            files: files
                .iter()
                .map(|path| HistoryFileChangeCompat {
                    path: (*path).to_string(),
                    action: "M".to_string(),
                    insertions: 1,
                    deletions: 0,
                })
                .collect(),
            method: "explicit_id".to_string(),
            confidence: 1.0,
            reason: "test".to_string(),
            field_changes: vec![],
            bead_diff_lines: vec![],
        }];

        HistoryBeadCompat {
            bead_id: bead_id.to_string(),
            title: bead_id.to_string(),
            status: status.to_string(),
            events: vec![HistoryEventCompat {
                bead_id: bead_id.to_string(),
                event_type: status.to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                commit_sha: String::new(),
                commit_message: String::new(),
                author: String::new(),
                author_email: String::new(),
            }],
            milestones: HistoryMilestonesCompat::default(),
            commits: Some(commits),
            cycle_time: None,
            last_author: "TestUser".to_string(),
        }
    }

    #[test]
    fn resolve_reference_file_path_checks_repo_relative_paths() {
        let dir = tempdir().expect("tempdir");
        let repo_root = dir.path();
        let snapshots = repo_root.join("snapshots");
        fs::create_dir_all(&snapshots).expect("create snapshots dir");
        let before = snapshots.join("before.jsonl");
        fs::write(&before, "{}\n").expect("write snapshot");

        let resolved = resolve_reference_file_path("snapshots/before.jsonl", Some(repo_root))
            .expect("resolve repo-relative path");

        assert_eq!(resolved, before);
    }

    #[test]
    fn resolve_cli_reference_file_path_checks_repo_relative_paths_after_workspace_discovery() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let repo_root = root.join("services/api");
        let snapshots = repo_root.join("snapshots");
        fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        fs::create_dir_all(&snapshots).expect("create snapshots dir");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n    prefix: api-\n",
        )
        .expect("write workspace config");
        let before = snapshots.join("before.jsonl");
        fs::write(&before, "{}\n").expect("write snapshot");

        let repo_arg = repo_root.to_string_lossy().to_string();
        let cli = Cli::parse_from([
            "bvr",
            "--robot-diff",
            "--diff-since",
            "snapshots/before.jsonl",
            "--repo-path",
            &repo_arg,
        ]);

        let resolved = resolve_cli_reference_file_path("snapshots/before.jsonl", &cli)
            .expect("resolve repo-relative path after workspace discovery");

        assert_eq!(resolved, before);
    }

    #[test]
    fn resolve_git_toplevel_finds_repo_root_from_nested_path() {
        let dir = tempdir().expect("tempdir");
        let repo_root = dir.path();
        let nested = repo_root.join("nested/work");
        fs::create_dir_all(&nested).expect("create nested path");

        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("init")
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");

        let resolved = resolve_git_toplevel(&nested).expect("resolve git top-level");
        assert_eq!(resolved, repo_root);
    }

    #[test]
    fn extract_ids_from_message_respects_token_boundaries() {
        let known = BTreeMap::from([
            ("a".to_string(), "A".to_string()),
            ("bd-10".to_string(), "BD-10".to_string()),
        ]);

        let no_false_positive = extract_ids_from_message("refactor parser internals", &known);
        assert!(
            no_false_positive.is_empty(),
            "single-char IDs should not match arbitrary substrings"
        );

        let token_match = extract_ids_from_message("close A and update docs", &known);
        assert!(token_match.contains("A"));

        let exact_hyphenated = extract_ids_from_message("ship fix for bd-10", &known);
        assert!(exact_hyphenated.contains("BD-10"));

        let no_prefix_match = extract_ids_from_message("ship fix for bd-100", &known);
        assert!(!no_prefix_match.contains("BD-10"));
    }

    #[test]
    fn parse_scope_header_accepts_sha256_ids() {
        let line = format!("{}\02026-01-01T00:00:00Z", "a".repeat(64));
        let parsed = parse_scope_git_header_line(&line);
        assert!(parsed.is_some());
    }

    #[test]
    fn burndown_counts_closed_issue_without_closed_at() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("parse start")
            .with_timezone(&chrono::Utc);
        let end = start + chrono::Duration::days(1);

        let sprint = bvr::model::Sprint {
            id: "sprint-1".to_string(),
            name: "Sprint 1".to_string(),
            start_date: Some(start),
            end_date: Some(end),
            bead_ids: vec!["A".to_string()],
        };

        let issue = bvr::model::Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "closed".to_string(),
            issue_type: "task".to_string(),
            created_at: Some(start),
            ..bvr::model::Issue::default()
        };
        let issues = [issue];
        let issue_refs = issues.iter().collect::<Vec<_>>();

        let points = generate_daily_burndown_points(&sprint, &issue_refs, end);
        let last = points.last().expect("last burndown point");
        assert_eq!(last.completed, 1);
        assert_eq!(last.remaining, 0);
    }

    #[test]
    fn resolve_workspace_config_path_appends_default_file_for_directories() {
        let dir = tempdir().expect("tempdir");
        let resolved = resolve_workspace_config_path(dir.path());
        assert!(resolved.ends_with(".bv/workspace.yaml"));
    }

    #[test]
    fn project_dir_for_export_hooks_uses_workspace_root_when_repo_path_discovers_workspace() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api/src");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write workspace");

        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from(["bvr", "--repo-path", &nested_arg, "--export-pages", "out"]);

        let _guard = CurrentDirGuard::set(root);
        let project_dir = project_dir_for_export_hooks(&cli).expect("project dir");
        assert_eq!(project_dir, root);
    }

    #[test]
    fn resolve_cli_path_from_project_dir_uses_workspace_root_when_repo_path_discovers_workspace() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api/src");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write workspace");

        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from([
            "bvr",
            "--repo-path",
            &nested_arg,
            "--preview-pages",
            "bundle",
        ]);

        // Guard CWD so the ancestor-walk discovery stays inside this tempdir
        // and does not pick up workspace configs from sibling tempdirs.
        let _guard = CurrentDirGuard::set(root);
        let resolved =
            resolve_cli_path_from_project_dir(&cli, Path::new("bundle")).expect("resolved path");
        assert_eq!(resolved, root.join("bundle"));
    }

    #[test]
    fn feedback_project_dir_uses_workspace_root_when_repo_path_discovers_workspace() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api/src");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write workspace");

        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from(["bvr", "--repo-path", &nested_arg, "--feedback-show"]);

        let _guard = CurrentDirGuard::set(root);
        let project_dir = feedback_project_dir(&cli);
        assert_eq!(project_dir, root);
    }

    #[test]
    fn resolve_issue_load_target_discovers_workspace_from_repo_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api/src");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        let config_path = workspace_dir.join("workspace.yaml");
        fs::write(&config_path, "repos:\n  - path: services/api\n").expect("write workspace");

        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from(["bvr", "--repo-path", &nested_arg]);

        // Guard CWD so the ancestor-walk discovery stays inside this tempdir
        // and does not pick up workspace configs from sibling tempdirs.
        let _guard = CurrentDirGuard::set(root);
        let target = resolve_issue_load_target(&cli).expect("resolve issue load target");
        assert_eq!(target, IssueLoadTarget::WorkspaceConfig(config_path));
    }

    #[test]
    fn resolve_issue_load_target_discovers_workspace_from_relative_repo_path_without_ambiguity() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api/src");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        let config_path = workspace_dir.join("workspace.yaml");
        fs::write(&config_path, "repos:\n  - path: services/api\n").expect("write workspace");

        let cli = Cli::parse_from(["bvr", "--repo-path", "services/api/src"]);

        let _guard = CurrentDirGuard::set(root);
        let target = resolve_issue_load_target(&cli).expect("resolve issue load target");

        assert_eq!(target, IssueLoadTarget::WorkspaceConfig(config_path));
    }

    #[test]
    fn resolve_issue_load_target_prefers_explicit_workspace_over_discovery() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        let explicit_root = root.join("explicit");
        let discovered_root = root.join("discovered");
        let explicit_workspace = explicit_root.join(".bv");
        let discovered_workspace = discovered_root.join(".bv");
        let nested = discovered_root.join("services/api");

        fs::create_dir_all(&explicit_workspace).expect("create explicit .bv");
        fs::create_dir_all(&discovered_workspace).expect("create discovered .bv");
        fs::create_dir_all(&nested).expect("create nested");

        fs::write(
            explicit_workspace.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write explicit workspace");
        fs::write(
            discovered_workspace.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write discovered workspace");

        let explicit_arg = explicit_root.to_string_lossy().to_string();
        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from([
            "bvr",
            "--workspace",
            &explicit_arg,
            "--repo-path",
            &nested_arg,
        ]);

        let target = resolve_issue_load_target(&cli).expect("resolve issue load target");
        assert_eq!(
            target,
            IssueLoadTarget::WorkspaceConfig(explicit_workspace.join("workspace.yaml"))
        );
    }

    #[test]
    fn load_issues_as_of_uses_workspace_config_instead_of_raw_repo_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let api_beads = root.join("services/api/.beads");
        let web_beads = root.join("apps/web/.beads");
        fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        fs::create_dir_all(&api_beads).expect("create api beads");
        fs::create_dir_all(&web_beads).expect("create web beads");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n    prefix: api-\n  - path: apps/web\n    prefix: web-\n",
        )
        .expect("write workspace config");
        fs::write(
            api_beads.join("beads.jsonl"),
            "{\"id\":\"AUTH-1\",\"title\":\"API issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api beads");
        fs::write(
            web_beads.join("beads.jsonl"),
            "{\"id\":\"WEB-1\",\"title\":\"Web issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write web beads");

        run_git(root, &["init"]);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "snapshot"]);

        let repo_arg = root.join("services/api").to_string_lossy().to_string();
        let cli = Cli::parse_from([
            "bvr",
            "--robot-triage",
            "--as-of",
            "HEAD",
            "--repo-path",
            &repo_arg,
        ]);
        let issues = load_issues(&cli).expect("load workspace issues at HEAD");

        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .any(|issue| issue.title == "API issue" && issue.source_repo == "api")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.title == "Web issue" && issue.source_repo == "web")
        );
    }

    #[test]
    fn resolve_watch_export_paths_includes_workspace_config_file() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let api_beads = root.join("services/api/.beads");
        fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        fs::create_dir_all(&api_beads).expect("create api beads");
        let config_path = workspace_dir.join("workspace.yaml");
        fs::write(
            &config_path,
            "repos:\n  - path: services/api\n    prefix: api-\n",
        )
        .expect("write workspace config");
        let issues_path = api_beads.join("issues.jsonl");
        fs::write(
            &issues_path,
            "{\"id\":\"AUTH-1\",\"title\":\"API issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        let cli = Cli::parse_from([
            "bvr",
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--workspace",
            ".",
        ]);
        let _guard = CurrentDirGuard::set(root);
        let watched_paths = resolve_watch_export_paths(&cli).expect("resolve watch paths");

        assert!(
            watched_paths
                .iter()
                .any(|path| path.ends_with(".bv/workspace.yaml")),
            "expected workspace config path in watch set, got {watched_paths:?}"
        );
        assert!(watched_paths.contains(&issues_path));
    }

    #[test]
    fn file_watch_token_changes_for_same_size_rewrite() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("issues.jsonl");
        fs::write(&path, "AAAA").expect("write first payload");
        let first = file_watch_token(&path)
            .expect("first token")
            .expect("first token present");

        fs::write(&path, "BBBB").expect("write second payload");
        let second = file_watch_token(&path)
            .expect("second token")
            .expect("second token present");

        assert_eq!(first.len_bytes, second.len_bytes);
        assert_ne!(first.content_fingerprint, second.content_fingerprint);
        assert_ne!(first, second);
    }

    #[test]
    fn reconcile_watch_export_paths_adds_new_workspace_repo_issue_file() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let workspace_dir = root.join(".bv");
        let api_beads = root.join("services/api/.beads");
        let web_beads = root.join("apps/web/.beads");
        fs::create_dir_all(&workspace_dir).expect("create workspace dir");
        fs::create_dir_all(&api_beads).expect("create api beads");
        let config_path = workspace_dir.join("workspace.yaml");
        fs::write(
            &config_path,
            "repos:\n  - path: services/api\n    prefix: api-\n",
        )
        .expect("write workspace config");
        let api_issues_path = api_beads.join("issues.jsonl");
        fs::write(
            &api_issues_path,
            "{\"id\":\"AUTH-1\",\"title\":\"API issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write api issues");

        let cli = Cli::parse_from([
            "bvr",
            "--export-pages",
            "pages-out",
            "--watch-export",
            "--workspace",
            ".",
        ]);
        let _guard = CurrentDirGuard::set(root);
        let mut watched_tokens = resolve_watch_export_paths(&cli)
            .expect("resolve watch paths")
            .into_iter()
            .map(|path| {
                let token = file_watch_token(&path)
                    .expect("read watch token")
                    .expect("watch token present");
                (path, Some(token))
            })
            .collect::<Vec<_>>();

        fs::create_dir_all(&web_beads).expect("create web beads");
        let web_issues_path = web_beads.join("issues.jsonl");
        fs::write(
            &web_issues_path,
            "{\"id\":\"WEB-1\",\"title\":\"Web issue\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\"}\n",
        )
        .expect("write web issues");
        fs::write(
            &config_path,
            "repos:\n  - path: services/api\n    prefix: api-\n  - path: apps/web\n    prefix: web-\n",
        )
        .expect("update workspace config");

        let path_set_changed =
            reconcile_watch_export_paths(&cli, &mut watched_tokens).expect("reconcile watch paths");

        assert!(path_set_changed, "expected watch set to change");
        assert!(
            watched_tokens
                .iter()
                .any(|(path, token)| path == &web_issues_path && token.is_some()),
            "expected new repo issues file in watch set, got {watched_tokens:?}"
        );
    }

    #[test]
    fn discover_workspace_config_from_starts_reports_ambiguous_candidates() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let alpha_root = root.join("alpha");
        let beta_root = root.join("beta");
        let alpha_workspace = alpha_root.join(".bv/workspace.yaml");
        let beta_workspace = beta_root.join(".bv/workspace.yaml");
        let starts = vec![alpha_root.join("services/api"), beta_root.join("apps/web")];

        fs::create_dir_all(alpha_workspace.parent().expect("alpha workspace parent"))
            .expect("create alpha .bv");
        fs::create_dir_all(beta_workspace.parent().expect("beta workspace parent"))
            .expect("create beta .bv");
        fs::create_dir_all(&starts[0]).expect("create alpha nested");
        fs::create_dir_all(&starts[1]).expect("create beta nested");
        fs::write(&alpha_workspace, "repos:\n  - path: services/api\n").expect("write alpha");
        fs::write(&beta_workspace, "repos:\n  - path: apps/web\n").expect("write beta");

        let error =
            discover_workspace_config_from_starts(&starts).expect_err("ambiguous discovery");
        let message = error.to_string();
        assert!(message.contains("workspace auto-discovery is ambiguous"));
        assert!(message.contains(&starts[0].display().to_string()));
        assert!(message.contains(&starts[1].display().to_string()));
        assert!(message.contains(&alpha_workspace.display().to_string()));
        assert!(message.contains(&beta_workspace.display().to_string()));
        assert!(message.contains("--workspace"));
        assert!(message.contains("--beads-file"));
    }

    #[test]
    fn filter_by_repo_matches_id_and_source_repo_case_insensitively() {
        let issues = vec![
            bvr::model::Issue {
                id: "api-AUTH-1".to_string(),
                source_repo: "api".to_string(),
                title: "API issue".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "WEB-UI-1".to_string(),
                source_repo: "frontend/web".to_string(),
                title: "Web issue".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..bvr::model::Issue::default()
            },
        ];

        let filtered_by_prefix = filter_by_repo(issues.clone(), "api");
        assert_eq!(filtered_by_prefix.len(), 1);
        assert_eq!(filtered_by_prefix[0].id, "api-AUTH-1");

        let filtered_by_source_repo = filter_by_repo(issues, "front");
        assert_eq!(filtered_by_source_repo.len(), 1);
        assert_eq!(filtered_by_source_repo[0].id, "WEB-UI-1");
    }

    #[test]
    fn operational_check_update_is_deterministic_and_successful() {
        let cli = Cli::parse_from(["bvr", "--check-update"]);
        let outcome = handle_operational_commands(&cli);

        assert_eq!(outcome.exit_code, ExitCode::SUCCESS);
        assert!(!outcome.to_stderr);
        assert!(
            outcome.message.contains("Up to date (v")
                || outcome.message.contains("Newer version available:")
                || outcome
                    .message
                    .contains("Could not check for updates (network unavailable).")
        );
    }

    #[test]
    fn upgrade_subcommand_parses() {
        let cli = Cli::parse_from(["bvr", "upgrade"]);
        assert!(matches!(
            cli.command,
            Some(BvrCommand::Upgrade { dry_run: false })
        ));
        assert!(cli.is_operational_command());

        let cli = Cli::parse_from(["bvr", "upgrade", "--dry-run"]);
        assert!(matches!(
            cli.command,
            Some(BvrCommand::Upgrade { dry_run: true })
        ));

        // No subcommand leaves the flag surface untouched.
        let cli = Cli::parse_from(["bvr", "--check-update"]);
        assert!(cli.command.is_none());
        assert!(cli.is_operational_command());
    }

    #[test]
    fn upgrade_cargo_args_pin_version_and_lockfile() {
        assert_eq!(
            upgrade_cargo_args("0.9.3"),
            vec![
                "install",
                "beads_viewer_rust",
                "--version",
                "0.9.3",
                "--locked"
            ]
        );
    }

    #[test]
    fn parse_crates_io_latest_version_prefers_stable() {
        let payload = br#"{"crate":{"max_stable_version":"0.2.1","max_version":"0.3.0","newest_version":"0.3.0"}}"#;
        assert_eq!(parse_crates_io_latest_version(payload).unwrap(), "0.2.1");

        // A null stable version falls through to max_version.
        let payload = br#"{"crate":{"max_stable_version":null,"max_version":"0.2.1"}}"#;
        assert_eq!(parse_crates_io_latest_version(payload).unwrap(), "0.2.1");

        assert!(parse_crates_io_latest_version(br#"{"crate":{}}"#).is_err());
        assert!(parse_crates_io_latest_version(b"not json").is_err());
    }

    #[test]
    fn parse_github_highest_tag_picks_semver_max() {
        // The tags API's ordering is unspecified: the semver max must win and
        // non-version tags must be ignored, not poison the comparison.
        let payload =
            br#"[{"name":"v0.2.1"},{"name":"v0.10.0"},{"name":"nightly"},{"name":"v0.9.9"}]"#;
        assert_eq!(parse_github_highest_tag(payload).unwrap(), "0.10.0");

        assert!(parse_github_highest_tag(br"[]").is_err());
        assert!(parse_github_highest_tag(br#"[{"name":"nightly"}]"#).is_err());
        assert!(parse_github_highest_tag(br#"{"message":"rate limited"}"#).is_err());
    }

    #[test]
    fn parse_github_latest_release_extracts_tag() {
        assert_eq!(
            parse_github_latest_release(br#"{"tag_name":"v0.2.1"}"#).unwrap(),
            "0.2.1"
        );
        assert!(parse_github_latest_release(br#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn compare_versions_detects_equal_versions() {
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_versions("v0.1.0", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn compare_versions_detects_newer_available() {
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Ordering::Less);
    }

    #[test]
    fn compare_versions_detects_current_is_newer() {
        assert_eq!(compare_versions("0.3.0", "0.2.9"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_treats_malformed_versions_as_equal() {
        assert_eq!(compare_versions("dev-build", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.0", "latest"), Ordering::Equal);
    }

    #[test]
    fn parse_background_mode_bool_accepts_common_truthy_and_falsy_values() {
        assert_eq!(parse_background_mode_bool("1"), Some(true));
        assert_eq!(parse_background_mode_bool("true"), Some(true));
        assert_eq!(parse_background_mode_bool("YES"), Some(true));
        assert_eq!(parse_background_mode_bool("on"), Some(true));

        assert_eq!(parse_background_mode_bool("0"), Some(false));
        assert_eq!(parse_background_mode_bool("false"), Some(false));
        assert_eq!(parse_background_mode_bool("No"), Some(false));
        assert_eq!(parse_background_mode_bool("off"), Some(false));

        assert_eq!(parse_background_mode_bool("maybe"), None);
    }

    #[test]
    fn resolve_background_mode_prefers_cli_flags() {
        let enabled_cli = Cli::parse_from(["bvr", "--background-mode"]);
        let disabled_cli = Cli::parse_from(["bvr", "--no-background-mode"]);

        let (enabled, enabled_source) = resolve_background_mode(&enabled_cli);
        let (disabled, disabled_source) = resolve_background_mode(&disabled_cli);

        assert!(enabled);
        assert_eq!(enabled_source, BackgroundModeSource::CliFlag);
        assert!(!disabled);
        assert_eq!(disabled_source, BackgroundModeSource::CliFlag);
    }

    #[test]
    fn build_background_mode_config_resolves_workspace_and_repo_filter() {
        let temp = tempdir().expect("tempdir");
        let workspace_root = temp.path().to_string_lossy().to_string();
        let cli = Cli::parse_from([
            "bvr",
            "--background-mode",
            "--workspace",
            &workspace_root,
            "--repo",
            "api",
        ]);

        let config = build_background_mode_config(&cli, true).expect("background config");
        assert_eq!(config.repo_filter.as_deref(), Some("api"));
        assert_eq!(config.beads_file, None);
        assert!(
            config
                .workspace_config
                .as_deref()
                .expect("workspace config path")
                .ends_with(".bv/workspace.yaml")
        );
        assert!(config.poll_interval_ms > 0);
    }

    #[test]
    fn build_background_mode_config_discovers_workspace_without_flag() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        let workspace_dir = root.join(".bv");
        let nested = root.join("services/api");
        fs::create_dir_all(&workspace_dir).expect("create .bv");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(
            workspace_dir.join("workspace.yaml"),
            "repos:\n  - path: services/api\n",
        )
        .expect("write workspace");

        let nested_arg = nested.to_string_lossy().to_string();
        let cli = Cli::parse_from(["bvr", "--background-mode", "--repo-path", &nested_arg]);
        let config = build_background_mode_config(&cli, true).expect("background config");

        assert_eq!(config.beads_file, None);
        assert_eq!(config.repo_path, None);
        assert_eq!(
            config.workspace_config,
            Some(workspace_dir.join("workspace.yaml"))
        );
    }

    #[test]
    fn load_issues_reports_workspace_search_guidance_when_no_sources_exist() {
        let temp = tempdir().expect("tempdir");
        let empty_root = temp.path().join("empty");
        fs::create_dir_all(&empty_root).expect("create empty root");
        // The upward .beads search checks every ancestor, so a tempdir that
        // lives inside a beads repo (remote build workers point TMPDIR at a
        // synced checkout) legitimately finds sources and the "no sources"
        // scenario under test never exists.
        if empty_root.ancestors().any(|a| a.join(".beads").is_dir()) {
            eprintln!("skipping: tempdir has a .beads ancestor that wins the upward search");
            return;
        }
        let _guard = CurrentDirGuard::set(&empty_root);
        let empty_arg = empty_root.to_string_lossy().to_string();
        let cli = Cli::parse_from(["bvr", "--repo-path", &empty_arg]);

        let error = super::load_issues(&cli).expect_err("missing sources");
        let message = error.to_string();
        assert!(message.contains("Searched for .bv/workspace.yaml"));
        assert!(message.contains("--workspace"));
        assert!(message.contains("--beads-file"));
    }

    #[test]
    fn build_background_mode_config_disables_as_of_snapshots() {
        let cli = Cli::parse_from(["bvr", "--as-of", "HEAD~1"]);
        assert!(build_background_mode_config(&cli, true).is_none());
    }

    #[test]
    fn compute_related_work_result_excludes_closed_by_default() {
        let histories = BTreeMap::from([
            (
                "bd-1".to_string(),
                make_history("bd-1", "open", &["shared.rs"]),
            ),
            (
                "bd-2".to_string(),
                make_history("bd-2", "closed", &["shared.rs"]),
            ),
            (
                "bd-3".to_string(),
                make_history("bd-3", "open", &["shared.rs"]),
            ),
        ]);

        // include_closed=false (default) should exclude closed beads
        let result = compute_related_work_result("bd-1", &histories, 0, 10, false);
        let ids: Vec<&str> = result
            .related
            .iter()
            .map(|related| related.bead_id.as_str())
            .collect();
        assert!(
            !ids.contains(&"bd-2"),
            "closed beads should be excluded when include_closed=false: {ids:?}"
        );
        assert!(ids.contains(&"bd-3"));

        // include_closed=true should include closed beads
        let result = compute_related_work_result("bd-1", &histories, 0, 10, true);
        let ids: Vec<&str> = result
            .related
            .iter()
            .map(|related| related.bead_id.as_str())
            .collect();
        assert!(ids.contains(&"bd-2"));
        assert!(ids.contains(&"bd-3"));
    }

    #[test]
    fn actionable_recipe_filters_use_full_actionable_set_not_top_picks_subset() {
        let issues = (1..=4)
            .map(|index| bvr::model::Issue {
                id: format!("A-{index}"),
                title: format!("Actionable {index}"),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: index,
                ..bvr::model::Issue::default()
            })
            .collect::<Vec<_>>();
        let analyzer = bvr::analysis::Analyzer::new_with_config(
            issues.clone(),
            &bvr::analysis::graph::AnalysisConfig::triage_runtime(),
        );
        let triage = analyzer.triage(bvr::analysis::triage::TriageOptions {
            max_recommendations: 10,
            ..bvr::analysis::triage::TriageOptions::default()
        });
        assert_eq!(triage.result.quick_ref.top_picks.len(), 3);

        let actionable_recipe =
            bvr::analysis::recipe::find_recipe("actionable").expect("actionable recipe");
        let filtered = bvr::analysis::recipe::apply_recipe(
            &actionable_recipe,
            &triage.result.recommendations,
            &issues,
            &actionable_ids_for_recipe_filters(&analyzer),
            &analyzer.metrics.pagerank,
        );

        assert_eq!(filtered.len(), 4);
        let ids = filtered
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"A-1"));
        assert!(ids.contains(&"A-2"));
        assert!(ids.contains(&"A-3"));
        assert!(ids.contains(&"A-4"));
    }

    fn assert_matches_triage_runtime(config: &bvr::analysis::graph::AnalysisConfig) {
        assert!(config.enable_pagerank);
        assert!(config.enable_betweenness);
        assert!(!config.enable_eigenvector);
        assert!(!config.enable_hits);
        assert!(config.enable_cycles);
        assert!(config.enable_critical_path);
        assert!(!config.enable_k_core);
        assert!(config.enable_articulation);
        assert!(!config.enable_slack);
        assert_eq!(config.betweenness_max_nodes, 10_000);
        assert_eq!(config.eigenvector_max_nodes, 10_000);
    }

    #[test]
    fn analysis_config_routes_triage_oriented_commands_to_runtime_profile() {
        // `Cli` is large; keep the cases on the heap to avoid a
        // large-stack-arrays lint without changing test coverage.
        let cases: Vec<Cli> = vec![
            Cli::parse_from(["bvr", "--robot-next"]),
            Cli::parse_from(["bvr", "--robot-triage"]),
            Cli::parse_from(["bvr", "--robot-triage-by-track"]),
            Cli::parse_from(["bvr", "--robot-triage-by-label"]),
            Cli::parse_from(["bvr", "--robot-plan"]),
            Cli::parse_from(["bvr", "--robot-priority"]),
            Cli::parse_from(["bvr", "--emit-script"]),
            Cli::parse_from(["bvr", "--feedback-accept", "A-1"]),
            Cli::parse_from(["bvr", "--feedback-ignore", "A-1"]),
            Cli::parse_from(["bvr", "--priority-brief", "priority.md"]),
        ];

        for cli in &cases {
            let config = super::analysis_config_for_cli(cli);
            assert_matches_triage_runtime(&config);
        }
    }

    #[test]
    fn analysis_config_keeps_full_profile_for_richer_analysis_surfaces() {
        for cli in [
            Cli::parse_from(["bvr"]),
            Cli::parse_from(["bvr", "--robot-insights"]),
            Cli::parse_from(["bvr", "--robot-graph"]),
            Cli::parse_from(["bvr", "--robot-diff"]),
        ] {
            let config = super::analysis_config_for_cli(&cli);
            assert!(config.enable_eigenvector);
            assert!(config.enable_hits);
            assert!(config.enable_k_core);
            assert!(config.enable_slack);
            assert!(config.enable_articulation);
        }
    }

    #[test]
    fn full_stats_serializes_correctly() {
        use std::collections::HashMap;

        let mut metrics = bvr::analysis::graph::GraphMetrics {
            pagerank: HashMap::new(),
            betweenness: HashMap::new(),
            eigenvector: HashMap::new(),
            hubs: HashMap::new(),
            authorities: HashMap::new(),
            blocks_count: HashMap::new(),
            blocked_by_count: HashMap::new(),
            critical_depth: HashMap::new(),
            k_core: HashMap::new(),
            articulation_points: std::collections::HashSet::new(),
            slack: HashMap::new(),
            cycles: Vec::new(),
            skipped_metrics: Vec::new(),
            config: bvr::analysis::graph::AnalysisConfig::full(),
        };
        metrics.pagerank.insert("A".to_string(), 0.5);
        metrics.betweenness.insert("A".to_string(), 0.3);
        metrics.k_core.insert("A".to_string(), 2);
        metrics.articulation_points.insert("A".to_string());
        metrics.pagerank.insert("B".to_string(), 0.1);

        let stats = super::build_full_stats(&metrics);
        let json = serde_json::to_value(&stats).unwrap();

        // Keys are sorted (BTreeMap).
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["A", "B"]);

        // Node A has all populated fields.
        let node_a = &json["A"];
        assert_eq!(node_a["pagerank"], 0.5);
        assert_eq!(node_a["betweenness"], 0.3);
        assert_eq!(node_a["kcore"], 2);
        assert_eq!(node_a["is_articulation_point"], true);

        // Node B has only pagerank; other optional fields are absent.
        let node_b = &json["B"];
        assert_eq!(node_b["pagerank"], 0.1);
        assert!(node_b.get("betweenness").is_none());
        assert!(node_b.get("kcore").is_none());
        assert_eq!(node_b["is_articulation_point"], false);
    }

    #[test]
    fn full_stats_empty_metrics_produces_empty_map() {
        use std::collections::HashMap;

        let metrics = bvr::analysis::graph::GraphMetrics {
            pagerank: HashMap::new(),
            betweenness: HashMap::new(),
            eigenvector: HashMap::new(),
            hubs: HashMap::new(),
            authorities: HashMap::new(),
            blocks_count: HashMap::new(),
            blocked_by_count: HashMap::new(),
            critical_depth: HashMap::new(),
            k_core: HashMap::new(),
            articulation_points: std::collections::HashSet::new(),
            slack: HashMap::new(),
            cycles: Vec::new(),
            skipped_metrics: Vec::new(),
            config: bvr::analysis::graph::AnalysisConfig::full(),
        };

        let stats = super::build_full_stats(&metrics);
        assert!(stats.is_empty());
    }

    #[test]
    fn full_stats_omitted_when_flag_not_set() {
        let output = super::RobotInsightsOutput {
            envelope: bvr::robot::envelope(&[]),
            as_of: None,
            as_of_commit: None,
            label_scope: None,
            label_context: None,
            analysis_config: bvr::analysis::graph::AnalysisConfig::full(),
            analysis_config_compat: bvr::analysis::graph::AnalysisConfig::full(),
            insights: bvr::analysis::Insights {
                status: bvr::analysis::MetricStatus::computed(),
                bottlenecks: Vec::new(),
                critical_path: Vec::new(),
                cycles: Vec::new(),
                slack: Vec::new(),
                influencers: Vec::new(),
                betweenness: Vec::new(),
                hubs: Vec::new(),
                authorities: Vec::new(),
                eigenvector: Vec::new(),
                cores: Vec::new(),
                articulation_points: Vec::new(),
                keystones: Vec::new(),
                orphans: Vec::new(),
                cluster_density: 0.0,
                velocity: bvr::analysis::InsightsVelocity {
                    closed_last_7_days: 0,
                    closed_last_30_days: 0,
                    avg_days_to_close: 0,
                    weekly: Vec::new(),
                },
            },
            full_stats: None,
            top_what_ifs: None,
            advanced_insights: None,
            usage_hints: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(
            json.get("full_stats").is_none(),
            "full_stats should be absent when None"
        );
        assert!(
            json.get("top_what_ifs").is_none(),
            "top_what_ifs should be absent when None"
        );
        assert!(
            json.get("advanced_insights").is_none(),
            "advanced_insights should be absent when None"
        );
    }

    #[test]
    fn top_what_ifs_present_in_insights_output() {
        let issues = vec![
            bvr::model::Issue {
                id: "A".to_string(),
                title: "Blocker".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "B".to_string(),
                title: "Blocked".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![bvr::model::Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..bvr::model::Dependency::default()
                }],
                ..bvr::model::Issue::default()
            },
        ];
        let analyzer = bvr::analysis::Analyzer::new(issues);
        let what_ifs = analyzer.top_what_ifs(5);
        assert!(!what_ifs.is_empty());
        // A should appear since completing it unblocks B.
        assert!(what_ifs.iter().any(|d| d.issue_id == "A"));
        let delta_a = what_ifs.iter().find(|d| d.issue_id == "A").unwrap();
        assert!(!delta_a.direct_unblocks.is_empty());
        let json = serde_json::to_value(&what_ifs).unwrap();
        assert!(json.is_array());
    }

    #[test]
    fn advanced_insights_present_in_insights_output() {
        let issues = vec![
            bvr::model::Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                dependencies: vec![bvr::model::Dependency {
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..bvr::model::Dependency::default()
                }],
                ..bvr::model::Issue::default()
            },
        ];
        let analyzer = bvr::analysis::Analyzer::new(issues);
        let advanced = analyzer.advanced_insights();
        let json = serde_json::to_value(&advanced).unwrap();
        assert!(json.is_object());
        assert!(json.get("top_k_set").is_some());
        assert!(json.get("coverage_set").is_some());
    }

    #[test]
    fn label_scope_omitted_when_no_label() {
        let issues = vec![bvr::model::Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            labels: vec!["backend".to_string()],
            ..bvr::model::Issue::default()
        }];
        let graph = bvr::analysis::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let output = super::RobotPlanOutput {
            envelope: bvr::robot::envelope(&issues),
            as_of: None,
            as_of_commit: None,
            label_scope: None,
            label_context: None,
            status: bvr::analysis::MetricStatus::computed(),
            analysis_config: metrics.config.clone(),
            plan: bvr::analysis::plan::compute_execution_plan(
                &graph,
                &std::collections::HashMap::new(),
            ),
            usage_hints: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(
            json.get("label_scope").is_none(),
            "label_scope absent when None"
        );
        assert!(
            json.get("label_context").is_none(),
            "label_context absent when None"
        );
    }

    #[test]
    fn label_scope_present_when_label_set() {
        let issues = vec![bvr::model::Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            labels: vec!["backend".to_string()],
            ..bvr::model::Issue::default()
        }];
        let graph = bvr::analysis::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let health =
            bvr::analysis::label_intel::compute_single_label_health("backend", &issues, &metrics);

        let output = super::RobotPlanOutput {
            envelope: bvr::robot::envelope(&issues),
            as_of: None,
            as_of_commit: None,
            label_scope: Some("backend".to_string()),
            label_context: Some(health),
            status: bvr::analysis::MetricStatus::computed(),
            analysis_config: metrics.config.clone(),
            plan: bvr::analysis::plan::compute_execution_plan(
                &graph,
                &std::collections::HashMap::new(),
            ),
            usage_hints: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["label_scope"], "backend");
        let ctx = &json["label_context"];
        assert_eq!(ctx["label"], "backend");
        assert_eq!(ctx["issue_count"], 1);
        assert!(ctx["health"].is_number(), "health should be a number");
    }

    #[test]
    fn label_context_nonexistent_label_produces_zero_health() {
        let issues = vec![bvr::model::Issue {
            id: "A".to_string(),
            title: "A".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            labels: vec!["backend".to_string()],
            ..bvr::model::Issue::default()
        }];
        let graph = bvr::analysis::graph::IssueGraph::build(&issues);
        let metrics = graph.compute_metrics();

        let health = bvr::analysis::label_intel::compute_single_label_health(
            "nonexistent",
            &issues,
            &metrics,
        );

        assert_eq!(health.issue_count, 0);
        assert_eq!(health.health, 0);
        assert_eq!(health.health_level, "critical");
    }

    #[test]
    fn robot_next_omits_absent_fields_when_no_actionable_item_exists() {
        let output = super::RobotNextOutput {
            envelope: bvr::robot::envelope(&[]),
            as_of: None,
            as_of_commit: None,
            id: None,
            title: None,
            score: None,
            reasons: Vec::new(),
            unblocks: None,
            claim_command: None,
            show_command: None,
            message: Some("No actionable items available".to_string()),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("id").is_none());
        assert!(json.get("title").is_none());
        assert!(json.get("score").is_none());
        assert!(json.get("reasons").is_none());
        assert!(json.get("unblocks").is_none());
        assert!(json.get("claim_command").is_none());
        assert!(json.get("show_command").is_none());
        assert_eq!(json["message"], "No actionable items available");
    }

    #[test]
    fn robot_overview_omits_optional_pick_and_blocker_when_absent() {
        let output = super::RobotOverviewOutput {
            envelope: bvr::robot::envelope(&[]),
            summary: super::RobotOverviewSummary {
                open_issues: 0,
                actionable_issues: 0,
                blocked_issues: 0,
                in_progress_issues: 0,
                closed_issues: 0,
                cycle_count: 0,
            },
            top_pick: None,
            top_blocker: None,
            top_labels: Vec::new(),
            fronts: Vec::new(),
            unlock_maximizers: Vec::new(),
            commands: super::RobotOverviewCommands {
                next: "bvr --robot-next".to_string(),
                triage: "bvr --robot-triage".to_string(),
                plan: "bvr --robot-plan".to_string(),
                history: "bvr --robot-history --history-limit 20".to_string(),
            },
            usage_hints: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("top_pick").is_none());
        assert!(json.get("top_blocker").is_none());
        assert!(json.get("top_labels").is_none());
        assert!(json.get("unlock_maximizers").is_none());
        assert_eq!(json["commands"]["next"], "bvr --robot-next");
    }

    #[test]
    fn build_robot_overview_output_counts_labels_and_cycles() {
        let issues = vec![
            bvr::model::Issue {
                id: "A".to_string(),
                title: "A".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                labels: vec!["backend".to_string(), "api".to_string()],
                dependencies: vec![bvr::model::Dependency {
                    issue_id: "A".to_string(),
                    depends_on_id: "B".to_string(),
                    dep_type: "blocks".to_string(),
                    ..bvr::model::Dependency::default()
                }],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "B".to_string(),
                title: "B".to_string(),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                labels: vec!["backend".to_string()],
                dependencies: vec![bvr::model::Dependency {
                    issue_id: "B".to_string(),
                    depends_on_id: "A".to_string(),
                    dep_type: "blocks".to_string(),
                    ..bvr::model::Dependency::default()
                }],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "C".to_string(),
                title: "C".to_string(),
                status: "closed".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                labels: vec!["docs".to_string()],
                ..bvr::model::Issue::default()
            },
        ];

        let analyzer = bvr::analysis::Analyzer::new(issues.clone());
        let triage = analyzer.triage(bvr::analysis::triage::TriageOptions::default());
        let output = super::build_robot_overview_output(&issues, &analyzer, &triage.result);

        assert_eq!(output.summary.open_issues, 2);
        assert_eq!(output.summary.blocked_issues, 1);
        assert_eq!(output.summary.closed_issues, 1);
        assert_eq!(output.summary.cycle_count, 1);
        assert_eq!(output.top_labels[0].label, "backend");
        assert_eq!(output.top_labels[0].open_issues, 2);
        assert!(output.top_pick.is_none());
        assert_eq!(output.commands.next, "bvr --robot-next");
    }

    #[test]
    fn build_robot_overview_output_populates_fronts_from_label_groups() {
        let issues = vec![
            bvr::model::Issue {
                id: "A".to_string(),
                title: "Implement auth".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                labels: vec!["backend".to_string()],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "B".to_string(),
                title: "Fix login page".to_string(),
                status: "open".to_string(),
                issue_type: "bug".to_string(),
                priority: 2,
                labels: vec!["frontend".to_string()],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "C".to_string(),
                title: "Update README".to_string(),
                status: "open".to_string(),
                issue_type: "docs".to_string(),
                priority: 3,
                labels: vec!["docs".to_string()],
                ..bvr::model::Issue::default()
            },
        ];

        let analyzer = bvr::analysis::Analyzer::new(issues.clone());
        let triage = analyzer.triage(bvr::analysis::triage::TriageOptions {
            group_by_label: true,
            ..bvr::analysis::triage::TriageOptions::default()
        });
        let output = super::build_robot_overview_output(&issues, &analyzer, &triage.result);

        // With 3 open issues across 3 labels, fronts should be populated.
        // The global top_pick's label is excluded from fronts to avoid redundancy,
        // so we expect at most 2 fronts.
        assert!(output.fronts.len() <= 3);
        for front in &output.fronts {
            assert!(!front.label.is_empty());
            assert!(!front.representative.id.is_empty());
            // Each front's representative should differ from the global top pick.
            if let Some(ref pick) = output.top_pick {
                assert_ne!(front.representative.id, pick.id);
            }
        }

        // Verify JSON serialization includes fronts when non-empty.
        let json = serde_json::to_value(&output).unwrap();
        if !output.fronts.is_empty() {
            assert!(json["fronts"].is_array());
        }
        assert!(json["usage_hints"].is_array());
    }

    #[test]
    fn robot_overview_surfaces_unlock_maximizer_triage_would_miss() {
        // Regression test for issue #4 (SL-000 scenario).
        //
        // Graph layout:
        //   UNLOCK_KING (actionable, open, priority=1) -> blocks X0..X6   (7 unlocks)
        //   HUB_A, HUB_B, HUB_C (actionable, open, priority=0) -> each blocks 3 distinct items
        //
        // Core invariant under test: when triage's composite score and the
        // greedy-unlock objective disagree on ranking, the orient payload must
        // surface high-unlock items somewhere — either via triage's own
        // surfaces (top_pick / fronts) OR via the separate `unlock_maximizers`
        // surface that's drawn from top_k_set. Which specific surface picks up
        // UNLOCK_KING depends on how triage's priority/centrality weighting
        // shakes out for this exact fixture and is intentionally not asserted.
        // The important guarantees are (a) it lands SOMEWHERE and (b) never in
        // two surfaces at once (dedup works).
        let mut issues = vec![
            bvr::model::Issue {
                id: "UNLOCK_KING".to_string(),
                title: "Broad task that unlocks many downstream items".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 1,
                labels: vec!["broad".to_string()],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "HUB_A".to_string(),
                title: "Hub A coordinator".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                labels: vec!["hub".to_string(), "core".to_string()],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "HUB_B".to_string(),
                title: "Hub B coordinator".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                labels: vec!["hub".to_string(), "core".to_string()],
                ..bvr::model::Issue::default()
            },
            bvr::model::Issue {
                id: "HUB_C".to_string(),
                title: "Hub C coordinator".to_string(),
                status: "open".to_string(),
                issue_type: "task".to_string(),
                priority: 0,
                labels: vec!["hub".to_string(), "core".to_string()],
                ..bvr::model::Issue::default()
            },
        ];

        // 7 downstream items all blocked by UNLOCK_KING.
        for i in 0..7 {
            issues.push(bvr::model::Issue {
                id: format!("DS_UK_{i}"),
                title: format!("Downstream of UNLOCK_KING ({i})"),
                status: "blocked".to_string(),
                issue_type: "task".to_string(),
                priority: 2,
                dependencies: vec![bvr::model::Dependency {
                    issue_id: format!("DS_UK_{i}"),
                    depends_on_id: "UNLOCK_KING".to_string(),
                    dep_type: "blocks".to_string(),
                    ..bvr::model::Dependency::default()
                }],
                ..bvr::model::Issue::default()
            });
        }

        // 3 downstream items per hub (9 total).
        for (hub, hub_id) in ["HUB_A", "HUB_B", "HUB_C"].iter().enumerate() {
            for i in 0..3 {
                issues.push(bvr::model::Issue {
                    id: format!("DS_H{hub}_{i}"),
                    title: format!("Downstream of {hub_id} ({i})"),
                    status: "blocked".to_string(),
                    issue_type: "task".to_string(),
                    priority: 2,
                    dependencies: vec![bvr::model::Dependency {
                        issue_id: format!("DS_H{hub}_{i}"),
                        depends_on_id: (*hub_id).to_string(),
                        dep_type: "blocks".to_string(),
                        ..bvr::model::Dependency::default()
                    }],
                    ..bvr::model::Issue::default()
                });
            }
        }

        let analyzer = bvr::analysis::Analyzer::new(issues.clone());
        let top_k = analyzer.top_k_unlock_set(10);
        // Sanity: UNLOCK_KING has the largest marginal_unlocks (7 downstream)
        // in the top_k_set objective, so it ranks first in that ordering even
        // though triage's composite score may prefer a different item.
        assert_eq!(top_k.items[0].id, "UNLOCK_KING");
        assert_eq!(top_k.items[0].marginal_unlocks, 7);

        let triage = analyzer.triage(bvr::analysis::triage::TriageOptions {
            group_by_label: true,
            max_recommendations: 10,
            ..bvr::analysis::triage::TriageOptions::default()
        });
        let output = super::build_robot_overview_output(&issues, &analyzer, &triage.result);

        // Acceptance criteria from issue #4: the orient surface must combine
        // triage and unlock-objectives so items high on unlock impact but low
        // on triage score still surface. UNLOCK_KING must appear somewhere in
        // the output — as top_pick, a front representative, or an unlock
        // maximizer — but never simultaneously (the dedup logic prevents that).
        let seen_in_top = output
            .top_pick
            .as_ref()
            .is_some_and(|p| p.id == "UNLOCK_KING");
        let seen_in_fronts = output
            .fronts
            .iter()
            .any(|f| f.representative.id == "UNLOCK_KING");
        let seen_in_maximizers = output
            .unlock_maximizers
            .iter()
            .any(|u| u.id == "UNLOCK_KING");
        assert!(
            seen_in_top || seen_in_fronts || seen_in_maximizers,
            "UNLOCK_KING must appear in orient output (top_pick, fronts, or unlock_maximizers)",
        );
        let surface_count = [seen_in_top, seen_in_fronts, seen_in_maximizers]
            .iter()
            .filter(|s| **s)
            .count();
        assert_eq!(
            surface_count, 1,
            "UNLOCK_KING must appear in exactly one surface — dedup prevents double counting",
        );

        // Each unlock maximizer must be a legitimate unlock-ordered item.
        for (idx, um) in output.unlock_maximizers.iter().enumerate() {
            assert!(
                !um.id.is_empty(),
                "unlock_maximizers[{idx}].id must not be empty",
            );
            assert!(
                !um.title.is_empty(),
                "unlock_maximizers[{idx}].title must be looked up from issues",
            );
            assert_eq!(
                um.claim_command,
                format!("br update {} --status=in_progress", um.id),
                "claim_command must be pre-baked",
            );
        }

        // JSON shape: the field is serialized iff the Vec is non-empty
        // (skip_serializing_if = "Vec::is_empty"). Assert symmetrically.
        let json = serde_json::to_value(&output).unwrap();
        if output.unlock_maximizers.is_empty() {
            assert!(json.get("unlock_maximizers").is_none());
        } else {
            assert!(json["unlock_maximizers"].is_array());
            assert_eq!(
                json["unlock_maximizers"].as_array().unwrap().len(),
                output.unlock_maximizers.len(),
            );
        }
    }

    #[test]
    fn robot_overview_fronts_empty_when_no_label_groups() {
        let issues = vec![bvr::model::Issue {
            id: "X".to_string(),
            title: "Solo task".to_string(),
            status: "open".to_string(),
            issue_type: "task".to_string(),
            priority: 1,
            labels: vec!["only-label".to_string()],
            ..bvr::model::Issue::default()
        }];

        let analyzer = bvr::analysis::Analyzer::new(issues.clone());
        // Without group_by_label, recommendations_by_label is empty.
        let triage = analyzer.triage(bvr::analysis::triage::TriageOptions::default());
        let output = super::build_robot_overview_output(&issues, &analyzer, &triage.result);

        assert!(output.fronts.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        // Empty fronts should be omitted from JSON via skip_serializing_if.
        assert!(json.get("fronts").is_none());
    }

    #[test]
    fn priority_filters_omit_unset_optional_fields() {
        let filters = super::PriorityFilterOutput {
            min_confidence: 0.5,
            max_results: 10,
            by_label: None,
            by_assignee: None,
        };

        let json = serde_json::to_value(&filters).unwrap();
        assert_eq!(json["min_confidence"], 0.5);
        assert_eq!(json["max_results"], 10);
        assert!(json.get("by_label").is_none());
        assert!(json.get("by_assignee").is_none());
    }

    #[test]
    fn robot_history_omits_absent_optional_fields() {
        let output = super::RobotHistoryOutput {
            envelope: bvr::robot::envelope(&[]),
            bead_history: None,
            history_count: None,
            histories_timeline: None,
            git_range: "HEAD".to_string(),
            latest_commit_sha: None,
            stats: super::HistoryStatsCompat {
                total_beads: 0,
                beads_with_commits: 0,
                total_commits: 0,
                unique_authors: 0,
                avg_commits_per_bead: 0.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
            },
            histories_map: BTreeMap::new(),
            commit_index: BTreeMap::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("bead_history").is_none());
        assert!(json.get("history_count").is_none());
        assert!(json.get("histories_timeline").is_none());
        assert!(json.get("latest_commit_sha").is_none());
        assert_eq!(json["git_range"], "HEAD");
    }

    #[test]
    fn robot_burndown_omits_absent_dates() {
        let output = super::RobotBurndownOutput {
            envelope: bvr::robot::envelope(&[]),
            sprint_id: "sprint-1".to_string(),
            sprint_name: "Sprint 1".to_string(),
            start_date: None,
            end_date: None,
            total_days: 14,
            elapsed_days: 3,
            remaining_days: 11,
            total_issues: 10,
            completed_issues: 2,
            remaining_issues: 8,
            ideal_burn_rate: 0.5,
            actual_burn_rate: 0.67,
            projected_complete: None,
            on_track: true,
            daily_points: Vec::new(),
            ideal_line: Vec::new(),
            scope_changes: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("start_date").is_none());
        assert!(json.get("end_date").is_none());
        assert!(json.get("projected_complete").is_none());
    }
}
