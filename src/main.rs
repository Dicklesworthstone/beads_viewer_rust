#![forbid(unsafe_code)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use bvr::analysis::alerts::AlertOptions;
use bvr::analysis::git_history::{
    HistoryBeadCompat, HistoryEventCompat, HistoryMilestonesCompat, HistoryStatsCompat,
    compute_history_stats, correlate_histories_with_git, finalize_history_entries,
    load_git_commits,
};
use bvr::analysis::suggest::{SuggestOptions, SuggestionType};
use bvr::analysis::triage::TriageOptions;
use bvr::analysis::{Analyzer, Insights, MetricStatus};
use bvr::cli::{Cli, GraphFormat};
use bvr::loader;
use bvr::robot::{compute_data_hash, default_field_descriptions, emit, envelope};
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

    let cli = Cli::parse();

    if cli.version {
        print_version();
        return ExitCode::SUCCESS;
    }

    let issues = match load_issues(&cli) {
        Ok(issues) => issues,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };

    let analyzer = Analyzer::new(issues.clone());

    if cli.robot_help {
        print_robot_help();
        return ExitCode::SUCCESS;
    }

    if cli.robot_next || cli.robot_triage || cli.robot_triage_by_track || cli.robot_triage_by_label
    {
        let triage = analyzer.triage(TriageOptions {
            group_by_track: cli.robot_triage_by_track,
            group_by_label: cli.robot_triage_by_label,
            max_recommendations: cli.robot_max_results.max(10),
        });

        if cli.robot_next {
            let result = if let Some(top) = triage.result.quick_ref.top_picks.first() {
                RobotNextOutput {
                    generated_at: envelope(&issues).generated_at,
                    data_hash: envelope(&issues).data_hash,
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
                    generated_at: envelope(&issues).generated_at,
                    data_hash: envelope(&issues).data_hash,
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

            if let Err(error) = emit(cli.format, &result) {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }

        let output = RobotTriageOutput {
            generated_at: envelope(&issues).generated_at,
            data_hash: envelope(&issues).data_hash,
            triage: triage.result,
            usage_hints: vec![
                "jq '.triage.quick_ref.top_picks[:3]'".to_string(),
                "jq '.triage.blockers_to_clear | map(.id)'".to_string(),
                "jq '.triage.quick_wins | map({id,score})'".to_string(),
                "bvr --robot-next".to_string(),
            ],
        };

        if let Err(error) = emit(cli.format, &output) {
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
        });
        let plan = analyzer.plan(&triage.score_by_id);

        let output = RobotPlanOutput {
            generated_at: envelope(&issues).generated_at,
            data_hash: envelope(&issues).data_hash,
            status: MetricStatus::computed(),
            plan,
            usage_hints: vec![
                "jq '.plan.summary'".to_string(),
                "jq '.plan.tracks[].items[] | select(.unblocks | length > 0)'".to_string(),
            ],
        };

        if let Err(error) = emit(cli.format, &output) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_insights {
        let insights = analyzer.insights();
        let output = RobotInsightsOutput {
            generated_at: envelope(&issues).generated_at,
            data_hash: envelope(&issues).data_hash,
            insights,
            usage_hints: vec![
                "jq '.insights.bottlenecks[:5]'".to_string(),
                "jq '.insights.cycles'".to_string(),
                "jq '.insights.critical_path[:10]'".to_string(),
            ],
        };

        if let Err(error) = emit(cli.format, &output) {
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
            generated_at: envelope(&issues).generated_at,
            data_hash: envelope(&issues).data_hash,
            status: MetricStatus::computed(),
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

        if let Err(error) = emit(cli.format, &output) {
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

        if let Err(error) = emit(cli.format, &output) {
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

        if let Err(error) = emit(cli.format, &output) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_graph {
        let output = build_robot_graph_output(&issues, &analyzer, &cli);
        if let Err(error) = emit(cli.format, &output) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
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
            generated_at: envelope(&issues).generated_at,
            resolved_revision,
            from_data_hash: compute_data_hash(&before_issues),
            to_data_hash: compute_data_hash(&issues),
            diff,
        };

        if let Err(error) = emit(cli.format, &output) {
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

        if let Err(error) = emit(cli.format, &output) {
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

        if let Err(error) = emit(cli.format, &output) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if cli.robot_capacity {
        let output = build_robot_capacity_output(&issues, &cli);

        if let Err(error) = emit(cli.format, &output) {
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
            generated_at: envelope(&issues).generated_at,
            data_hash: envelope(&issues).data_hash,
            agents,
            filters,
            forecast_count: forecasts.len(),
            forecasts,
            summary,
            output_format: "json".to_owned(),
            version: format!("v{}", env!("CARGO_PKG_VERSION")),
        };

        if let Err(error) = emit(cli.format, &output) {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    match bvr::tui::run_tui(issues) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn load_issues(cli: &Cli) -> bvr::Result<Vec<bvr::model::Issue>> {
    if let Some(path) = &cli.beads_file {
        return loader::load_issues_from_file(path);
    }

    loader::load_issues(cli.repo_path.as_deref())
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
        _ => {
            return Err(bvr::BvrError::InvalidArgument(format!(
                "Invalid suggest-type: {value} (use: duplicate, dependency, label, cycle)"
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
        generated_at: envelope(issues).generated_at,
        data_hash: envelope(issues).data_hash,
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
        generated_at: envelope(issues).generated_at,
        data_hash: envelope(issues).data_hash,
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
        output_format: "json".to_owned(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
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

    while day <= end_date && day <= now {
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
        generated_at: envelope(issues).generated_at,
        data_hash: envelope(issues).data_hash,
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
        output_format: "json".to_owned(),
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
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
) -> RobotGraphOutput {
    let mut filtered_issues = filter_graph_issues(
        issues,
        cli.label.as_deref(),
        cli.graph_root.as_deref(),
        cli.graph_depth,
    );
    filtered_issues.sort_by(|left, right| left.id.cmp(&right.id));

    let format = graph_format_name(cli.graph_format).to_string();
    let filters_applied = collect_graph_filters(cli);
    let data_hash = compute_data_hash(issues);

    if filtered_issues.is_empty() {
        return RobotGraphOutput {
            format,
            graph: None,
            nodes: 0,
            edges: 0,
            filters_applied,
            explanation: GraphExplanation {
                what: "Empty graph - no issues match the filter criteria".to_string(),
                how_to_render: None,
                when_to_use: "Adjust filter parameters to include more issues".to_string(),
            },
            data_hash,
            adjacency: None,
        };
    }

    let edges = build_graph_edges(&filtered_issues);

    let mut graph = None;
    let mut adjacency = None;
    let explanation = match cli.graph_format {
        GraphFormat::Json => {
            adjacency = Some(build_graph_adjacency(
                &filtered_issues,
                &edges,
                &analyzer.metrics.pagerank,
            ));
            GraphExplanation {
                what: "Dependency graph as JSON adjacency list".to_string(),
                how_to_render: None,
                when_to_use: "When you need programmatic access to the graph structure".to_string(),
            }
        }
        GraphFormat::Dot => {
            graph = Some(generate_dot(
                &filtered_issues,
                &edges,
                &analyzer.metrics.pagerank,
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
            graph = Some(generate_mermaid(&filtered_issues, &edges));
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
        format,
        graph,
        nodes: filtered_issues.len(),
        edges: edges.len(),
        filters_applied,
        explanation,
        data_hash,
        adjacency,
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

fn generate_dot(
    issues: &[bvr::model::Issue],
    edges: &[GraphAdjacencyEdge],
    pagerank: &std::collections::HashMap<String, f64>,
) -> String {
    let mut out = String::new();
    out.push_str("digraph G {\n");
    out.push_str("    rankdir=LR;\n");
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
        "  --robot-forecast <id|all> ETA forecast (--forecast-label, --forecast-sprint, --forecast-agents)"
    );
    println!(
        "  --format json|toon        Structured output format (toon compatibility mode for now)"
    );
}

#[derive(Debug, Serialize)]
struct RobotTriageOutput {
    generated_at: String,
    data_hash: String,
    triage: bvr::analysis::triage::TriageResult,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotNextOutput {
    generated_at: String,
    data_hash: String,
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
    generated_at: String,
    data_hash: String,
    status: MetricStatus,
    plan: bvr::analysis::plan::ExecutionPlan,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotInsightsOutput {
    generated_at: String,
    data_hash: String,
    insights: Insights,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotGraphOutput {
    format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<String>,
    nodes: usize,
    edges: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    filters_applied: BTreeMap<String, String>,
    explanation: GraphExplanation,
    data_hash: String,
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
    generated_at: String,
    data_hash: String,
    status: MetricStatus,
    recommendations: Vec<bvr::analysis::triage::Recommendation>,
    field_descriptions: BTreeMap<&'static str, &'static str>,
    filters: PriorityFilterOutput,
    summary: PrioritySummaryOutput,
    usage_hints: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RobotDiffOutput {
    generated_at: String,
    resolved_revision: String,
    from_data_hash: String,
    to_data_hash: String,
    diff: bvr::analysis::diff::SnapshotDiff,
}

#[derive(Debug, Serialize)]
struct RobotHistoryOutput {
    generated_at: String,
    data_hash: String,
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
    generated_at: String,
    data_hash: String,
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
    output_format: String,
    version: String,
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
    generated_at: String,
    data_hash: String,
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
    output_format: String,
    version: String,
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
    generated_at: String,
    data_hash: String,
    agents: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    filters: BTreeMap<String, String>,
    forecast_count: usize,
    forecasts: Vec<RobotForecastItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<RobotForecastSummary>,
    output_format: String,
    version: String,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use bvr::analysis::git_history::extract_ids_from_message;
    use tempfile::tempdir;

    use super::{
        generate_daily_burndown_points, parse_scope_git_header_line, resolve_git_toplevel,
        resolve_reference_file_path,
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
}
