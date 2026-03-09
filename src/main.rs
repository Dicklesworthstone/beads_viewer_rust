#![forbid(unsafe_code)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use bvr::analysis::alerts::AlertOptions;
use bvr::analysis::git_history::{
    HistoryBeadCompat, HistoryEventCompat, HistoryMilestonesCompat, HistoryStatsCompat,
    compute_history_stats, correlate_histories_with_git, finalize_history_entries,
    load_git_commits,
};
use bvr::analysis::suggest::{SuggestOptions, SuggestionType};
use bvr::analysis::triage::{TriageOptions, TriageScoringOptions};
use bvr::analysis::{Analyzer, Insights, MetricStatus};
use bvr::cli::{Cli, GraphFormat, GraphPreset, GraphStyle};
use bvr::loader;
use bvr::robot::{
    compute_data_hash, default_field_descriptions, emit_with_stats, envelope, envelope_empty,
    generate_robot_docs, generate_robot_schemas,
};
use chrono::{DateTime, Duration, Local, Utc};
use clap::Parser;
use serde::{Deserialize, Serialize};

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

    if cli.version {
        print_version();
        return ExitCode::SUCCESS;
    }

    if cli.background_mode && cli.no_background_mode {
        eprintln!("error: --background-mode and --no-background-mode are mutually exclusive");
        return ExitCode::from(2);
    }

    // --robot-schema and --robot-docs don't need issues loaded
    if cli.robot_schema {
        let schemas = generate_robot_schemas();

        if let Some(cmd) = cli.schema_command.as_deref() {
            if let Some(schema) = schemas.commands.get(cmd) {
                let single = serde_json::json!({
                    "schema_version": schemas.schema_version,
                    "generated_at": schemas.generated_at,
                    "command": cmd,
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
        let work_dir = cli
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

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

        let work_dir = cli
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

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
        bvr::export_pages::print_pages_wizard();
        return ExitCode::SUCCESS;
    }

    if let Some(bundle_path) = cli.preview_pages.as_deref() {
        if let Err(error) = bvr::export_pages::run_preview_server(bundle_path, !cli.no_live_reload)
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
    let analyzer = Analyzer::new(issues.clone());
    let build_duration = build_start.elapsed();

    if cli.robot_help {
        print_robot_help();
        return ExitCode::SUCCESS;
    }

    let (as_of, as_of_commit) = resolve_as_of(&cli);
    let (label_scope, label_context) = if let Some(ref label) = cli.label {
        let health = bvr::analysis::label_intel::compute_single_label_health(
            label,
            &issues,
            &analyzer.metrics,
        );
        (Some(label.clone()), Some(health))
    } else {
        (None, None)
    };

    // Load feedback adjustments for triage scoring (persisted from --feedback-accept/ignore).
    let feedback_data = {
        let work_dir = cli
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        bvr::analysis::recipe::FeedbackData::load(&work_dir)
    };
    let feedback_weight_adjustments = feedback_data.weight_adjustment_map();

    if cli.robot_next || cli.robot_triage || cli.robot_triage_by_track || cli.robot_triage_by_label
    {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: cli.robot_triage_by_track,
            group_by_label: cli.robot_triage_by_label,
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
        let insights = analyzer.insights();
        let legacy_bottlenecks = insights.bottlenecks.clone();
        let legacy_cycles = insights.cycles.clone();
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
            bottlenecks: legacy_bottlenecks,
            cycles: legacy_cycles,
            insights,
            full_stats,
            top_what_ifs,
            advanced_insights,
            usage_hints: vec![
                "jq '.insights.bottlenecks[:5]'".to_string(),
                "jq '.insights.cycles'".to_string(),
                "jq '.insights.critical_path[:10]'".to_string(),
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
        let sprints = match bvr::loader::load_sprints(cli.repo_path.as_deref()) {
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
        let output = RobotLabelHealthOutput {
            envelope: envelope(&issues),
            result: bvr::analysis::label_intel::compute_all_label_health(
                &issues,
                &analyzer.graph,
                &analyzer.metrics,
            ),
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
            flow: bvr::analysis::label_intel::compute_cross_label_flow(&issues),
        };
        if let Err(error) = emit_with_stats(cli.format, &output, cli.stats) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if cli.robot_label_attention {
        let limit = cli.attention_limit;
        let output = RobotLabelAttentionOutput {
            envelope: envelope(&issues),
            limit,
            result: bvr::analysis::label_intel::compute_label_attention(
                &issues,
                &analyzer.metrics,
                limit,
            ),
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
        let repo_root = cli.repo_path.clone().unwrap_or_else(|| PathBuf::from("."));
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
            let repo_root = cli.repo_path.clone().unwrap_or_else(|| PathBuf::from("."));
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
            let result = bvr::analysis::file_intel::find_related_work(
                bead_id,
                &history_output.histories_map,
                cli.related_min_relevance,
                cli.related_max_results,
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
    if let Some(ref description) = cli.save_baseline {
        let project_dir = cli.repo_path.clone().unwrap_or_else(|| PathBuf::from("."));
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

    if cli.robot_drift {
        let project_dir = cli.repo_path.clone().unwrap_or_else(|| PathBuf::from("."));
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
            Some(q) if !q.is_empty() => q,
            _ => {
                eprintln!("error: --robot-search requires --search <query>");
                return ExitCode::from(1);
            }
        };

        let mode = cli.search_mode.as_deref().map_or(
            bvr::analysis::search::SearchMode::Text,
            bvr::analysis::search::SearchMode::from_str_or_default,
        );

        let weights = if let Some(ref json) = cli.search_weights {
            match bvr::analysis::search::SearchWeights::from_json(json) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        } else {
            let preset_name = cli.search_preset.as_deref().unwrap_or("default");
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
            Some(
                cli.search_preset
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
            )
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
                let actionable_ids: Vec<String> = triage
                    .result
                    .quick_ref
                    .top_picks
                    .iter()
                    .map(|p| p.id.clone())
                    .collect();
                recommendations = bvr::analysis::recipe::apply_recipe(
                    &recipe,
                    &recommendations,
                    &issues,
                    &actionable_ids,
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
        let work_dir = cli
            .workspace
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

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
            include_closed: cli.pages_include_closed,
            include_history: cli.pages_include_history,
        };
        let export_issue_count = count_pages_export_issues(&issues, &options);

        match bvr::export_md::run_export_with_hooks(
            export_path,
            "html",
            export_issue_count,
            cli.no_hooks,
            cli.repo_path.as_deref(),
            || bvr::export_pages::export_pages_bundle(&issues, export_path, &options),
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
            let watched_paths = match resolve_watch_export_paths(&cli) {
                Ok(paths) => paths,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::from(1);
                }
            };
            let watched_mtimes = watched_paths
                .iter()
                .map(|path| {
                    file_mtime_epoch_millis(path)
                        .map(|mtime| (path.clone(), mtime))
                        .map_err(|error| {
                            bvr::BvrError::InvalidArgument(format!(
                                "failed to read watch source {}: {error}",
                                path.display()
                            ))
                        })
                })
                .collect::<bvr::Result<Vec<_>>>();
            let mut watched_mtimes = match watched_mtimes {
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

            eprintln!(
                "Watching {} source file(s) for changes (Ctrl+C to stop)...",
                watched_mtimes.len()
            );
            for (path, _) in &watched_mtimes {
                eprintln!("  - {}", path.display());
            }

            loop {
                std::thread::sleep(std::time::Duration::from_millis(watch_interval_ms));

                let mut changed = false;
                for (path, last_mtime) in &mut watched_mtimes {
                    let current_mtime = match file_mtime_epoch_millis(path) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!(
                                "warning: failed to read watch source {}: {error}",
                                path.display()
                            );
                            continue;
                        }
                    };

                    if current_mtime > *last_mtime {
                        *last_mtime = current_mtime;
                        changed = true;
                    }
                }

                if !changed {
                    if let Some(remaining) = max_loops.as_mut() {
                        *remaining = remaining.saturating_sub(1);
                        if *remaining == 0 {
                            break;
                        }
                    }
                    continue;
                }

                let refreshed_issues = match load_issues(&cli) {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("warning: failed to reload issues: {error}");
                        continue;
                    }
                };
                let refreshed_issues = if let Some(repo_filter) = cli.repo.as_deref() {
                    filter_by_repo(refreshed_issues, repo_filter)
                } else {
                    refreshed_issues
                };
                let refreshed_issue_count = count_pages_export_issues(&refreshed_issues, &options);

                match bvr::export_md::run_export_with_hooks(
                    export_path,
                    "html",
                    refreshed_issue_count,
                    cli.no_hooks,
                    cli.repo_path.as_deref(),
                    || {
                        bvr::export_pages::export_pages_bundle(
                            &refreshed_issues,
                            export_path,
                            &options,
                        )
                    },
                ) {
                    Ok(summary) => {
                        eprintln!(
                            "Regenerated pages bundle at {} (path: {}, issues: {}, history: {})",
                            summary.generated_at,
                            summary.export_path,
                            summary.issue_count,
                            summary.include_history
                        );
                    }
                    Err(error) => {
                        eprintln!("warning: export regeneration failed: {error}");
                    }
                }

                if let Some(remaining) = max_loops.as_mut() {
                    *remaining = remaining.saturating_sub(1);
                    if *remaining == 0 {
                        break;
                    }
                }
            }
        }

        return ExitCode::SUCCESS;
    }

    if let Some(export_path) = cli.export_md.as_deref() {
        if let Err(error) = bvr::export_md::export_markdown_with_hooks(
            &issues,
            export_path,
            cli.no_hooks,
            cli.repo_path.as_deref(),
        ) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if let Some(ref view_name) = cli.debug_render {
        match bvr::tui::render_debug_view(issues, view_name, cli.debug_width, cli.debug_height) {
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
    match bvr::tui::run_tui_with_background(issues, background_runtime) {
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

    Some(bvr::tui::BackgroundModeConfig {
        beads_file: cli.beads_file.clone(),
        workspace_config: cli.workspace.as_deref().map(resolve_workspace_config_path),
        repo_path: cli.repo_path.clone(),
        repo_filter: cli.repo.clone(),
        poll_interval_ms,
    })
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
    let action_count =
        usize::from(cli.check_update) + usize::from(cli.update) + usize::from(cli.rollback);

    if action_count > 1 {
        return EarlyCommandOutcome {
            message:
                "error: only one of --check-update/--update/--rollback may be used at a time.\n\
                      Remediation: rerun with a single operational action flag."
                    .to_string(),
            exit_code: ExitCode::from(2),
            to_stderr: true,
        };
    }

    if cli.yes && !cli.update {
        return EarlyCommandOutcome {
            message: "error: --yes can only be used with --update.\n\
                      Remediation: use --check-update for status, or combine --yes with --update."
                .to_string(),
            exit_code: ExitCode::from(2),
            to_stderr: true,
        };
    }

    if cli.check_update {
        return EarlyCommandOutcome {
            message: format!(
                "Automatic self-update checks are not implemented in this Rust port.\n\
                 Current version: bvr {}\n\
                 Remediation:\n\
                   1. git pull origin main\n\
                   2. cargo install --path .\n\
                   3. bvr --version",
                env!("CARGO_PKG_VERSION")
            ),
            exit_code: ExitCode::SUCCESS,
            to_stderr: false,
        };
    }

    if cli.update {
        let yes_note = if cli.yes {
            " --yes was accepted for compatibility."
        } else {
            ""
        };

        return EarlyCommandOutcome {
            message: format!(
                "error: --update is not supported in this Rust port.{yes_note}\n\
                 Remediation:\n\
                   1. git pull origin main\n\
                   2. cargo install --path .\n\
                   3. bvr --version"
            ),
            exit_code: ExitCode::from(2),
            to_stderr: true,
        };
    }

    if cli.rollback {
        return EarlyCommandOutcome {
            message: "error: --rollback is not supported in this Rust port.\n\
                      Remediation:\n\
                        1. Identify a known-good commit or tag.\n\
                        2. git checkout <commit-or-tag>\n\
                        3. cargo install --path .\n\
                        4. git checkout main (when done)"
                .to_string(),
            exit_code: ExitCode::from(2),
            to_stderr: true,
        };
    }

    EarlyCommandOutcome {
        message: "error: unsupported operational flag combination.\n\
                  Remediation: use one of --check-update, --update, or --rollback."
            .to_string(),
        exit_code: ExitCode::from(2),
        to_stderr: true,
    }
}

fn load_issues(cli: &Cli) -> bvr::Result<Vec<bvr::model::Issue>> {
    if let Some(ref_name) = &cli.as_of {
        return load_issues_at_revision(cli, ref_name);
    }

    if let Some(path) = &cli.beads_file {
        return loader::load_issues_from_file(path);
    }

    if let Some(path) = &cli.workspace {
        return loader::load_workspace_issues(&resolve_workspace_config_path(path));
    }

    loader::load_issues(cli.repo_path.as_deref())
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
    if let Some(path) = &cli.beads_file {
        return Ok(vec![path.clone()]);
    }

    if let Some(path) = &cli.workspace {
        return loader::find_workspace_issue_paths(&resolve_workspace_config_path(path));
    }

    let beads_dir = loader::get_beads_dir(cli.repo_path.as_deref())?;
    let beads_path = loader::find_jsonl_path(&beads_dir)?;
    Ok(vec![beads_path])
}

fn file_mtime_epoch_millis(path: &Path) -> bvr::Result<u64> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            let millis = duration.as_millis().min(u128::from(u64::MAX));
            u64::try_from(millis).unwrap_or(u64::MAX)
        });
    Ok(modified)
}

fn load_issues_at_revision(cli: &Cli, revision: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    let repo_root = cli.repo_path.clone().unwrap_or_else(|| PathBuf::from("."));

    // Try common beads file paths at the given revision
    let candidates = [".beads/issues.jsonl", ".beads/beads.jsonl"];

    for path in &candidates {
        let git_ref = format!("{revision}:{path}");
        let output = Command::new("git")
            .args(["show", &git_ref])
            .current_dir(&repo_root)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let content = String::from_utf8_lossy(&out.stdout);
                let mut issues = Vec::new();
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<bvr::model::Issue>(trimmed) {
                        Ok(issue) => issues.push(issue),
                        Err(error) => {
                            tracing::warn!("skipping malformed issue line at {revision}: {error}");
                        }
                    }
                }
                eprintln!(
                    "Loaded {} issues from {} (as-of: {revision})",
                    issues.len(),
                    path
                );
                return Ok(issues);
            }
            _ => {}
        }
    }

    Err(bvr::BvrError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("Could not load issues at revision: {revision}"),
    )))
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
    let Ok(sprints) = loader::load_sprints(cli.repo_path.as_deref()) else {
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
    if let Some(path) = resolve_reference_file_path(diff_since, cli.repo_path.as_deref()) {
        return loader::load_issues_from_file(&path);
    }

    load_issues_from_git_ref(cli, diff_since)
}

fn load_issues_from_git_ref(cli: &Cli, reference: &str) -> bvr::Result<Vec<bvr::model::Issue>> {
    let Some(repo_root) = resolve_repo_root(cli) else {
        return Err(bvr::BvrError::InvalidArgument(
            "could not determine repository root".to_string(),
        ));
    };

    let beads_dir = loader::get_beads_dir(cli.repo_path.as_deref())?;
    let beads_path = loader::find_jsonl_path(&beads_dir)?;

    let relative = beads_path.strip_prefix(&repo_root).ok().map_or_else(
        || ".beads/beads.jsonl".to_string(),
        |path| path.to_string_lossy().replace('\\', "/"),
    );

    let candidates = [relative, ".beads/beads.jsonl".to_string()];

    for candidate in candidates {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .arg("show")
            .arg(format!("{reference}:{candidate}"))
            .output()?;

        if !output.status.success() {
            continue;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        return parse_issues_from_jsonl_text(&text);
    }

    Err(bvr::BvrError::InvalidArgument(format!(
        "could not resolve --diff-since={reference} to a historical beads JSONL snapshot"
    )))
}

fn resolve_diff_revision(cli: &Cli, reference: &str) -> String {
    if let Some(path) = resolve_reference_file_path(reference, cli.repo_path.as_deref()) {
        return path.to_string_lossy().to_string();
    }

    let repo_root = if let Some(path) = &cli.repo_path {
        path.clone()
    } else if let Ok(path) = std::env::current_dir() {
        path
    } else {
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
    let repo_root = if let Some(path) = &cli.repo_path {
        path.clone()
    } else if let Ok(path) = std::env::current_dir() {
        path
    } else {
        return None;
    };

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
                    timestamp: event.timestamp.clone().unwrap_or_default(),
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

    if let Some(repo_root) = resolve_repo_root(cli) {
        let commits = load_git_commits(&repo_root, cli.history_limit, history_since)?;
        if let Some(commit) = commits.first() {
            latest_sha = Some(commit.sha.clone());
        }
        correlate_histories_with_git(
            &repo_root,
            &commits,
            &mut histories_map,
            &mut commit_index,
            &mut method_distribution,
        );
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

fn resolve_repo_root(cli: &Cli) -> Option<PathBuf> {
    let base = if let Some(path) = &cli.repo_path {
        path.clone()
    } else {
        std::env::current_dir().ok()?
    };

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
    if direct.is_file() {
        return Some(direct);
    }

    if let Some(root) = repo_path {
        let rooted = root.join(reference);
        if rooted.is_file() {
            return Some(rooted);
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
    let sprints = loader::load_sprints(cli.repo_path.as_deref())?;

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
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .or_else(|| issue.updated_at.as_deref().and_then(parse_rfc3339_utc))
        .or_else(|| issue.created_at.as_deref().and_then(parse_rfc3339_utc))
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
            .filter(|issue| issue.labels.iter().any(|entry| entry == label))
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
    id: Option<String>,
    title: Option<String>,
    score: Option<f64>,
    reasons: Vec<String>,
    unblocks: Option<usize>,
    claim_command: Option<String>,
    show_command: Option<String>,
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
    analysis_config: bvr::analysis::graph::AnalysisConfig,
    #[serde(rename = "Bottlenecks")]
    bottlenecks: Vec<bvr::analysis::InsightItem>,
    #[serde(rename = "Cycles")]
    cycles: Vec<Vec<String>>,
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
    by_label: Option<String>,
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
    start_date: Option<DateTime<Utc>>,
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
    result: bvr::analysis::label_intel::LabelHealthResult,
}

#[derive(Debug, Serialize)]
struct RobotLabelFlowOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    flow: bvr::analysis::label_intel::CrossLabelFlow,
}

#[derive(Debug, Serialize)]
struct RobotLabelAttentionOutput {
    #[serde(flatten)]
    envelope: bvr::robot::RobotEnvelope,
    limit: usize,
    result: bvr::analysis::label_intel::LabelAttentionResult,
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::process::ExitCode;

    use bvr::analysis::git_history::extract_ids_from_message;
    use clap::Parser;
    use tempfile::tempdir;

    use super::{
        BackgroundModeSource, Cli, build_background_mode_config, filter_by_repo,
        generate_daily_burndown_points, handle_operational_commands, parse_background_mode_bool,
        parse_scope_git_header_line, resolve_background_mode, resolve_git_toplevel,
        resolve_reference_file_path, resolve_workspace_config_path,
    };

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
            created_at: Some(start.to_rfc3339()),
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
        assert!(outcome.message.contains("Current version: bvr"));
        assert!(outcome.message.contains("cargo install --path ."));
    }

    #[test]
    fn operational_yes_without_update_returns_usage_error() {
        let cli = Cli::parse_from(["bvr", "--yes"]);
        let outcome = handle_operational_commands(&cli);

        assert_eq!(outcome.exit_code, ExitCode::from(2));
        assert!(outcome.to_stderr);
        assert!(
            outcome
                .message
                .contains("--yes can only be used with --update")
        );
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
    fn build_background_mode_config_disables_as_of_snapshots() {
        let cli = Cli::parse_from(["bvr", "--as-of", "HEAD~1"]);
        assert!(build_background_mode_config(&cli, true).is_none());
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
            bottlenecks: Vec::new(),
            cycles: Vec::new(),
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
}
