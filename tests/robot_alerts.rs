use std::fs;

use assert_cmd::Command;
use serde_json::Value;

fn run_bvr_json_in_dir(flags: &[&str], dir: &std::path::Path) -> Value {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    let mut command = Command::new(bvr_bin);
    command.current_dir(dir);
    command.args(flags);

    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("valid JSON output")
}

#[test]
fn robot_alerts_basic_contract_and_filters() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    let now = chrono::Utc::now();
    let stale_updated = (now - chrono::Duration::days(20)).to_rfc3339();
    let stale_created = (now - chrono::Duration::days(25)).to_rfc3339();
    let tombstone_updated = (now - chrono::Duration::days(20)).to_rfc3339();
    let tombstone_created = (now - chrono::Duration::days(25)).to_rfc3339();
    let fresh_time = (now - chrono::Duration::days(1)).to_rfc3339();

    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        format!(
            concat!(
                "{{\"id\":\"ROOT\",\"title\":\"Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\"}}\n",
                "{{\"id\":\"D1\",\"title\":\"Dep1\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D1\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"D2\",\"title\":\"Dep2\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D2\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"D3\",\"title\":\"Dep3\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D3\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"STALE\",\"title\":\"Stale issue\",\"status\":\"open\",\"priority\":3,\"issue_type\":\"task\",\"created_at\":\"{stale_created}\",\"updated_at\":\"{stale_updated}\"}}\n",
                "{{\"id\":\"TOMBSTONE\",\"title\":\"Removed\",\"status\":\"tombstone\",\"priority\":3,\"issue_type\":\"task\",\"created_at\":\"{tombstone_created}\",\"updated_at\":\"{tombstone_updated}\"}}\n"
            ),
            fresh = fresh_time,
            stale_created = stale_created,
            stale_updated = stale_updated,
            tombstone_created = tombstone_created,
            tombstone_updated = tombstone_updated
        ),
    )
    .expect("write beads");

    let base = run_bvr_json_in_dir(&["--robot-alerts"], repo_dir);
    assert!(base["generated_at"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(base["data_hash"].as_str().is_some_and(|v| !v.is_empty()));

    let alerts = base["alerts"].as_array().expect("alerts array");
    assert_eq!(
        base["summary"]["total"].as_u64().expect("summary.total"),
        u64::try_from(alerts.len()).unwrap_or(u64::MAX)
    );

    assert!(alerts.iter().any(|alert| {
        alert["type"] == "stale_issue"
            && alert["severity"] == "warning"
            && alert["issue_id"] == "STALE"
    }));
    assert!(
        !alerts
            .iter()
            .any(|alert| { alert["type"] == "stale_issue" && alert["issue_id"] == "TOMBSTONE" })
    );
    assert!(
        alerts
            .iter()
            .any(|alert| { alert["type"] == "blocking_cascade" && alert["issue_id"] == "ROOT" }),
        "expected blocking_cascade for ROOT, got {alerts:?}"
    );

    let stale_only = run_bvr_json_in_dir(&["--robot-alerts", "--alert-type=stale_issue"], repo_dir);
    let stale_alerts = stale_only["alerts"].as_array().expect("stale alerts");
    assert!(!stale_alerts.is_empty());
    assert!(
        stale_alerts
            .iter()
            .all(|alert| alert["type"] == "stale_issue")
    );

    let warning_only = run_bvr_json_in_dir(&["--robot-alerts", "--severity=warning"], repo_dir);
    let warning_alerts = warning_only["alerts"].as_array().expect("warning alerts");
    assert!(!warning_alerts.is_empty());
    assert!(
        warning_alerts
            .iter()
            .all(|alert| alert["severity"] == "warning")
    );
}

#[test]
fn robot_alerts_emit_cycle_alert_and_support_label_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = temp.path();
    fs::create_dir_all(repo_dir.join(".beads")).expect("mkdir beads");

    let fresh_time = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    fs::write(
        repo_dir.join(".beads/beads.jsonl"),
        format!(
            concat!(
                "{{\"id\":\"ROOT\",\"title\":\"Root\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\"}}\n",
                "{{\"id\":\"D1\",\"title\":\"Dep1\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D1\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"D2\",\"title\":\"Dep2\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D2\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"D3\",\"title\":\"Dep3\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"created_at\":\"{fresh}\",\"updated_at\":\"{fresh}\",\"dependencies\":[{{\"issue_id\":\"D3\",\"depends_on_id\":\"ROOT\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"cycle-a\",\"title\":\"Cycle A\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{{\"issue_id\":\"cycle-a\",\"depends_on_id\":\"cycle-b\",\"type\":\"blocks\"}}]}}\n",
                "{{\"id\":\"cycle-b\",\"title\":\"Cycle B\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"task\",\"dependencies\":[{{\"issue_id\":\"cycle-b\",\"depends_on_id\":\"cycle-a\",\"type\":\"blocks\"}}]}}\n"
            ),
            fresh = fresh_time
        ),
    )
    .expect("write beads");

    let base = run_bvr_json_in_dir(&["--robot-alerts"], repo_dir);
    let alerts = base["alerts"].as_array().expect("alerts");
    assert!(
        alerts
            .iter()
            .any(|alert| { alert["type"] == "new_cycle" && alert["severity"] == "critical" })
    );

    let label_filtered = run_bvr_json_in_dir(
        &[
            "--robot-alerts",
            "--alert-type=blocking_cascade",
            "--alert-label=d1",
        ],
        repo_dir,
    );
    let filtered_alerts = label_filtered["alerts"]
        .as_array()
        .expect("filtered alerts");
    assert_eq!(filtered_alerts.len(), 1);
    assert_eq!(filtered_alerts[0]["issue_id"], "ROOT");
}
