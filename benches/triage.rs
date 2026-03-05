use std::collections::HashMap;
use std::hint::black_box;
use std::path::PathBuf;

use bvr::analysis::Analyzer;
use bvr::analysis::alerts::AlertOptions;
use bvr::analysis::suggest::SuggestOptions;
use bvr::analysis::triage::TriageOptions;
use bvr::loader;
use bvr::model::{Dependency, Issue};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

// =========================================================================
// Synthetic fixture generation
// =========================================================================

fn make_issue(id: usize, deps: &[usize], status: &str) -> Issue {
    let dep_list = deps
        .iter()
        .map(|&d| Dependency {
            issue_id: format!("ISS-{id}"),
            depends_on_id: format!("ISS-{d}"),
            dep_type: "blocks".to_string(),
            ..Default::default()
        })
        .collect();

    Issue {
        id: format!("ISS-{id}"),
        title: format!("Issue {id}"),
        status: status.to_string(),
        issue_type: "task".to_string(),
        priority: ((id % 5) as i32) + 1,
        estimated_minutes: Some(30 + (id % 120) as i32),
        labels: vec![format!("area-{}", id % 8), format!("team-{}", id % 3)],
        dependencies: dep_list,
        created_at: Some("2026-01-15T10:00:00Z".to_string()),
        ..Default::default()
    }
}

/// Generate a sparse dependency graph: each issue depends on ~1 predecessor.
fn gen_sparse(n: usize) -> Vec<Issue> {
    (0..n)
        .map(|i| {
            let deps = if i > 0 { vec![i - 1] } else { vec![] };
            let status = if i % 5 == 0 { "closed" } else { "open" };
            make_issue(i, &deps, status)
        })
        .collect()
}

/// Generate a dense dependency graph: each issue depends on ~3 predecessors.
fn gen_dense(n: usize) -> Vec<Issue> {
    (0..n)
        .map(|i| {
            let deps: Vec<usize> = (1..=3).filter_map(|d| i.checked_sub(d)).collect();
            let status = if i % 4 == 0 { "closed" } else { "open" };
            make_issue(i, &deps, status)
        })
        .collect()
}

/// Generate a graph with cycles (mutual dependencies every 10 issues).
fn gen_cyclic(n: usize) -> Vec<Issue> {
    (0..n)
        .map(|i| {
            let mut deps = if i > 0 { vec![i - 1] } else { vec![] };
            // Add cycle: every 10th issue depends on i+5
            if i % 10 == 0 && i + 5 < n {
                deps.push(i + 5);
            }
            make_issue(i, &deps, "open")
        })
        .collect()
}

// =========================================================================
// Benchmark groups
// =========================================================================

fn bench_analyzer_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyzer_new");
    for &size in &[100, 500, 1000] {
        let issues = gen_sparse(size);
        group.bench_with_input(BenchmarkId::new("sparse", size), &issues, |b, issues| {
            b.iter(|| black_box(Analyzer::new(issues.clone())))
        });
    }
    for &size in &[100, 500, 1000] {
        let issues = gen_dense(size);
        group.bench_with_input(BenchmarkId::new("dense", size), &issues, |b, issues| {
            b.iter(|| black_box(Analyzer::new(issues.clone())))
        });
    }
    group.finish();
}

fn bench_triage(c: &mut Criterion) {
    let mut group = c.benchmark_group("triage");
    let opts = TriageOptions {
        group_by_track: true,
        group_by_label: true,
        max_recommendations: 50,
    };

    for &size in &[100, 500, 1000] {
        let issues = gen_sparse(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("sparse", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.triage(opts.clone())))
        });
    }
    for &size in &[100, 500, 1000] {
        let issues = gen_dense(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("dense", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.triage(opts.clone())))
        });
    }
    group.finish();
}

fn bench_insights(c: &mut Criterion) {
    let mut group = c.benchmark_group("insights");
    for &size in &[100, 500, 1000] {
        let issues = gen_dense(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("dense", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.insights()))
        });
    }
    group.finish();
}

fn bench_plan(c: &mut Criterion) {
    let mut group = c.benchmark_group("plan");
    for &size in &[100, 500, 1000] {
        let issues = gen_sparse(size);
        let analyzer = Analyzer::new(issues);
        let opts = TriageOptions {
            group_by_track: false,
            group_by_label: false,
            max_recommendations: 50,
        };
        let triage = analyzer.triage(opts);
        let scores: HashMap<String, f64> = triage
            .result
            .recommendations
            .iter()
            .map(|r| (r.id.clone(), r.score))
            .collect();
        group.bench_with_input(
            BenchmarkId::new("sparse", size),
            &(&analyzer, &scores),
            |b, (a, s)| b.iter(|| black_box(a.plan(s))),
        );
    }
    group.finish();
}

fn bench_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff");
    for &size in &[100, 500, 1000] {
        let before = gen_sparse(size);
        let mut after = before.clone();
        // Modify half the issues
        for issue in after.iter_mut().step_by(2) {
            issue.status = "closed".to_string();
        }
        let analyzer = Analyzer::new(after);
        group.bench_with_input(
            BenchmarkId::new("sparse", size),
            &(&analyzer, &before),
            |b, (a, bef)| b.iter(|| black_box(a.diff(bef))),
        );
    }
    group.finish();
}

fn bench_forecast(c: &mut Criterion) {
    let mut group = c.benchmark_group("forecast");
    for &size in &[100, 500] {
        let issues = gen_sparse(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("sparse", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.forecast("all", None, 2)))
        });
    }
    group.finish();
}

fn bench_suggest(c: &mut Criterion) {
    let mut group = c.benchmark_group("suggest");
    let opts = SuggestOptions {
        min_confidence: 0.3,
        max_suggestions: 20,
        filter_type: None,
        filter_bead: None,
    };
    for &size in &[100, 500, 1000] {
        let issues = gen_dense(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("dense", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.suggest(&opts)))
        });
    }
    group.finish();
}

fn bench_alerts(c: &mut Criterion) {
    let mut group = c.benchmark_group("alerts");
    let opts = AlertOptions {
        severity: None,
        alert_type: None,
        alert_label: None,
    };
    for &size in &[100, 500, 1000] {
        let issues = gen_dense(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("dense", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.alerts(&opts)))
        });
    }
    group.finish();
}

fn bench_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("history");
    for &size in &[100, 500, 1000] {
        let issues = gen_sparse(size);
        let analyzer = Analyzer::new(issues);
        group.bench_with_input(BenchmarkId::new("sparse", size), &analyzer, |b, a| {
            b.iter(|| black_box(a.history(None, 50)))
        });
    }
    group.finish();
}

fn bench_cycle_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("cycle_detection");
    for &size in &[100, 500, 1000] {
        let issues = gen_cyclic(size);
        group.bench_with_input(BenchmarkId::new("cyclic", size), &issues, |b, issues| {
            b.iter(|| {
                let a = Analyzer::new(issues.clone());
                black_box(a.insights().cycles.len())
            })
        });
    }
    group.finish();
}

fn bench_real_fixture(c: &mut Criterion) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/testdata/synthetic_complex.jsonl");
    let issues = loader::load_issues_from_file(&path).expect("load synthetic fixture");

    let mut group = c.benchmark_group("real_fixture");
    let analyzer = Analyzer::new(issues.clone());

    group.bench_function("triage", |b| {
        b.iter(|| {
            black_box(analyzer.triage(TriageOptions {
                group_by_track: true,
                group_by_label: true,
                max_recommendations: 50,
            }))
        });
    });

    group.bench_function("insights", |b| {
        b.iter(|| black_box(analyzer.insights()));
    });

    group.bench_function("analyzer_new", |b| {
        b.iter(|| black_box(Analyzer::new(issues.clone())));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_analyzer_construction,
    bench_triage,
    bench_insights,
    bench_plan,
    bench_diff,
    bench_forecast,
    bench_suggest,
    bench_alerts,
    bench_history,
    bench_cycle_detection,
    bench_real_fixture,
);
criterion_main!(benches);
