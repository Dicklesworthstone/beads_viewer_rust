use std::hint::black_box;
use std::path::PathBuf;

use bvr::analysis::Analyzer;
use bvr::analysis::triage::TriageOptions;
use bvr::loader;
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_robot_triage(c: &mut Criterion) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/testdata/synthetic_complex.jsonl");
    let issues = loader::load_issues_from_file(&path).expect("load synthetic fixture");

    c.bench_function("robot_triage_synthetic_complex", |b| {
        b.iter(|| {
            let analyzer = Analyzer::new(issues.clone());
            let triage = analyzer.triage(TriageOptions {
                group_by_track: true,
                group_by_label: true,
                max_recommendations: 50,
            });
            black_box(triage.result.quick_ref.total_actionable)
        });
    });
}

criterion_group!(benches, bench_robot_triage);
criterion_main!(benches);
