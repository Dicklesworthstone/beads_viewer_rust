use assert_cmd::Command;
use predicates::prelude::*;

fn bvr_command() -> Command {
    let bvr_bin = std::env::var("CARGO_BIN_EXE_bvr").expect("CARGO_BIN_EXE_bvr env var");
    Command::new(bvr_bin)
}

#[test]
fn check_update_prints_remediation_and_succeeds() {
    // With no curl on PATH, this exercises the unavailable-network outcome
    // deterministically and cannot make a real request during the test.
    let empty_path = tempfile::tempdir().expect("empty executable search path");
    let mut cmd = bvr_command();
    cmd.arg("--check-update").env("PATH", empty_path.path());

    cmd.assert().success().stdout(
        predicate::str::contains("Could not check for updates (network unavailable).")
            .and(predicate::str::contains("Current version: bvr")),
    );
}

#[test]
fn agents_force_defaults_to_check_mode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let work_dir = temp.path().join("level1/level2/level3");
    std::fs::create_dir_all(&work_dir).expect("create nested work dir");

    let mut cmd = bvr_command();
    cmd.current_dir(&work_dir).arg("--agents-force");

    cmd.assert().success().stdout(
        predicate::str::contains("No agent file found")
            .and(predicate::str::contains("bvr --agents-add")),
    );
}

#[test]
fn conflicting_agents_actions_are_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");

    let mut cmd = bvr_command();
    cmd.current_dir(temp.path())
        .args(["--agents-add", "--agents-remove"]);

    cmd.assert().code(2).stderr(predicate::str::contains(
        "only one of --agents-check/--agents-add/--agents-update/--agents-remove may be used",
    ));
}

#[test]
fn agents_force_uses_workspace_root_discovered_from_repo_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let nested_repo = workspace_root.join("services/api");
    std::fs::create_dir_all(workspace_root.join(".bv")).expect("create workspace dir");
    std::fs::create_dir_all(&nested_repo).expect("create nested repo");
    std::fs::write(
        workspace_root.join(".bv/workspace.yaml"),
        "repos:\n  - path: services/api\n    prefix: api-\n",
    )
    .expect("write workspace config");
    std::fs::write(
        workspace_root.join("AGENTS.md"),
        "<!-- bv-agent-instructions-v1 -->\n\n<!-- end-bv-agent-instructions -->\n",
    )
    .expect("write agents file");

    let mut cmd = bvr_command();
    cmd.current_dir(&nested_repo).args([
        "--agents-force",
        "--repo-path",
        nested_repo.to_str().unwrap(),
    ]);

    cmd.assert().success().stdout(
        predicate::str::contains("Found AGENTS.md")
            .and(predicate::str::contains(
                workspace_root.join("AGENTS.md").to_string_lossy(),
            ))
            .and(predicate::str::contains("up to date")),
    );
}

#[cfg(unix)]
mod upgrade_boundary {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use predicates::prelude::*;
    use tempfile::TempDir;

    use super::bvr_command;

    const LATEST: &str = "9999.0.0";
    const INSTALL_ARGS: &str = "install\nbeads_viewer_rust\n--version\n9999.0.0\n--locked\n";

    struct UpgradeFixture {
        dir: TempDir,
    }

    impl UpgradeFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("upgrade fixture");
            for (name, source) in [
                (
                    "curl",
                    r#"#!/bin/sh
printf '%s\n' "$@" >> "$BVR_TEST_CURL_LOG"
if [ "$BVR_TEST_FETCH_FAIL" = 1 ]; then exit 7; fi
printf '{"crate":{"max_stable_version":"%s"}}\n' "$BVR_TEST_LATEST_VERSION"
"#,
                ),
                (
                    "cargo",
                    r#"#!/bin/sh
printf '%s\n' "$@" > "$BVR_TEST_CARGO_LOG"
if [ -n "$BVR_TEST_CARGO_EXIT" ]; then exit "$BVR_TEST_CARGO_EXIT"; fi
exit 0
"#,
                ),
            ] {
                let path = dir.path().join(name);
                fs::write(&path, source).expect("write isolated executable fixture");
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                    .expect("make fixture executable");
            }
            Self { dir }
        }

        fn command(&self) -> assert_cmd::Command {
            let mut command = bvr_command();
            command
                .current_dir(self.dir.path())
                // Never inherit the real curl or cargo, even when a regression
                // accidentally crosses the installer boundary.
                .env("PATH", self.dir.path())
                .env("BVR_TEST_CURL_LOG", self.dir.path().join("curl.args"))
                .env("BVR_TEST_CARGO_LOG", self.dir.path().join("cargo.args"))
                .env("BVR_TEST_LATEST_VERSION", LATEST)
                .env_remove("BVR_TEST_FETCH_FAIL")
                .env_remove("BVR_TEST_CARGO_EXIT")
                .env_remove("BV_OUTPUT_FORMAT")
                .env_remove("TOON_DEFAULT_FORMAT")
                .timeout(Duration::from_secs(5));
            command
        }

        fn assert_no_install(&self) {
            assert!(
                !self.dir.path().join("cargo.args").exists(),
                "unexpected installer invocation"
            );
        }

        fn assert_no_lookup(&self) {
            assert!(
                !self.dir.path().join("curl.args").exists(),
                "invalid or informational arguments must exit before version lookup"
            );
        }

        fn assert_installed_exact_version(&self) {
            assert_eq!(
                fs::read_to_string(self.dir.path().join("cargo.args"))
                    .expect("installer should have run"),
                INSTALL_ARGS,
            );
        }
    }

    #[test]
    fn upgrade_dry_run_never_launches_the_installer() {
        for args in [
            vec!["upgrade", "--dry-run"],
            vec!["--update-dry-run"],
            vec!["--update", "--dry-run", "--yes"],
            vec!["self-update", "--dry-run"],
            vec!["--yes", "upgrade", "--dry-run"],
        ] {
            let fixture = UpgradeFixture::new();
            fixture.command().args(&args).assert().success().stdout(
                predicate::str::contains("Newer version available: v9999.0.0").and(
                    predicate::str::contains(
                        "Would run: cargo install beads_viewer_rust --version 9999.0.0 --locked",
                    ),
                ),
            );
            assert!(fixture.dir.path().join("curl.args").is_file(), "{args:?}");
            fixture.assert_no_install();
        }
    }

    #[test]
    fn upgrade_requires_a_terminal_or_explicit_yes() {
        for command in ["upgrade", "--update", "self-update", "selfupdate"] {
            for input in ["", "yes\n", "y\n"] {
                let fixture = UpgradeFixture::new();
                fixture
                    .command()
                    .arg(command)
                    .write_stdin(input)
                    .assert()
                    .code(1)
                    .stderr(
                        predicate::str::contains("confirmation from an interactive terminal")
                            .and(predicate::str::contains("upgrade --yes")),
                    );
                fixture.assert_no_install();
            }
        }
    }

    #[test]
    fn upgrade_explicit_yes_installs_the_pinned_version() {
        for args in [
            vec!["upgrade", "--yes"],
            vec!["upgrade", "-y"],
            vec!["--yes", "upgrade"],
            vec!["-y", "upgrade"],
            vec!["--update", "--yes"],
            vec!["--yes", "--update"],
            vec!["--update", "-y"],
            vec!["selfupdate", "-y"],
        ] {
            let fixture = UpgradeFixture::new();
            fixture
                .command()
                .args(&args)
                .write_stdin("no\n")
                .assert()
                .success()
                .stdout(predicate::str::contains("Upgraded bvr to v9999.0.0."));
            fixture.assert_installed_exact_version();
        }
    }

    #[test]
    fn upgrade_help_and_version_do_not_lookup_or_install() {
        for command in [
            "upgrade",
            "self-update",
            "selfupdate",
            "--update",
            "--update-dry-run",
        ] {
            for information in ["--help", "-h", "--version", "-V"] {
                for args in [
                    [command, "--yes", information],
                    [information, command, "--yes"],
                ] {
                    let fixture = UpgradeFixture::new();
                    let output = fixture.command().args(args).assert().success();
                    if matches!(information, "--help" | "-h") {
                        output.stdout(predicate::str::contains("Usage:"));
                    } else {
                        output
                            .stdout(predicate::str::is_empty())
                            .stderr(predicate::str::contains("bvr "));
                    }
                    fixture.assert_no_lookup();
                    fixture.assert_no_install();
                }
            }
        }
    }

    #[test]
    fn upgrade_ambiguous_arguments_do_not_lookup_or_install() {
        for args in [
            vec!["--robot-next", "--update", "--yes"],
            vec!["--update", "--robot-next", "--yes"],
            vec!["triage", "--update", "--yes"],
            vec!["--check-update", "--update", "--yes"],
            vec!["--update", "--unknown", "--yes"],
            vec!["--update", "--dry-rnu", "--yes"],
            vec!["--label", "--update", "--yes"],
            vec!["--beads-file", "--update", "--yes"],
            vec!["--robot-next", "upgrade", "--yes"],
            vec!["--yes", "--robot-next", "upgrade"],
            vec!["--check-update", "upgrade", "--yes"],
            vec!["--beads-file", "issues.jsonl", "upgrade", "--yes"],
            vec!["--label", "urgent", "self-update", "--yes"],
            vec!["upgrade", "--robot-next", "--yes"],
            vec!["upgrade", "--unknown", "--yes"],
            vec!["--", "--update", "--yes"],
            vec!["--", "upgrade", "--yes"],
            vec!["--", "self-update", "--yes"],
            vec!["upgrade", "--", "--yes"],
            vec!["--update", "--", "--yes"],
            vec!["robot-self-update", "--yes"],
        ] {
            let fixture = UpgradeFixture::new();
            fixture.command().args(&args).assert().code(2);
            fixture.assert_no_lookup();
            fixture.assert_no_install();
        }
    }

    #[test]
    fn upgrade_flag_shaped_values_reach_search_validation_without_installing() {
        let fixture = UpgradeFixture::new();
        fs::write(
            fixture.dir.path().join("issues.jsonl"),
            r#"{"id":"task-1","title":"A ready task","status":"open","issue_type":"task","priority":2}
"#,
        )
        .expect("write valid search fixture");
        fixture
            .command()
            .args([
                "--beads-file",
                "issues.jsonl",
                "search",
                "--search-min-score",
                "--update",
                "query",
                "--yes",
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "invalid --search-min-score \"--update\"",
            ));
        fixture.assert_no_lookup();
        fixture.assert_no_install();
    }

    #[test]
    fn upgrade_words_in_filenames_preserve_robot_aliases() {
        for name in ["upgrade", "self-update", "selfupdate", "--update"] {
            let fixture = UpgradeFixture::new();
            fs::write(
                fixture.dir.path().join(name),
                r#"{"id":"task-1","title":"A ready task","status":"open","issue_type":"task","priority":2}
"#,
            )
            .expect("write command-shaped filename");
            let args = if name.starts_with('-') {
                vec![
                    format!("--beads-file={name}"),
                    "next".to_string(),
                    "--json".to_string(),
                ]
            } else {
                vec![
                    "--beads-file".to_string(),
                    name.to_string(),
                    "next".to_string(),
                    "--json".to_string(),
                ]
            };
            let output = fixture.command().args(args).assert().success();
            let result: serde_json::Value =
                serde_json::from_slice(&output.get_output().stdout).expect("robot JSON");
            assert_eq!(result["id"], "task-1");
            fixture.assert_no_lookup();
            fixture.assert_no_install();
        }
    }

    #[test]
    fn upgrade_does_not_install_when_current_or_lookup_fails() {
        for version in [env!("CARGO_PKG_VERSION"), "0.0.0"] {
            let fixture = UpgradeFixture::new();
            fixture
                .command()
                .args(["upgrade", "--yes"])
                .env("BVR_TEST_LATEST_VERSION", version)
                .assert()
                .success()
                .stdout(predicate::str::contains("Up to date (v"));
            fixture.assert_no_install();
        }

        let fixture = UpgradeFixture::new();
        fixture
            .command()
            .args(["--update", "--yes"])
            .env("BVR_TEST_FETCH_FAIL", "1")
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "Could not determine the latest bvr version",
            ));
        fixture.assert_no_install();
    }

    #[test]
    fn upgrade_reports_installer_failure() {
        let fixture = UpgradeFixture::new();
        fixture
            .command()
            .args(["upgrade", "--yes"])
            .env("BVR_TEST_CARGO_EXIT", "7")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("cargo install failed"))
            .stdout(predicate::str::contains("Upgraded bvr to").not());
        fixture.assert_installed_exact_version();
    }

    #[test]
    fn check_update_reports_available_version_without_installing() {
        let fixture = UpgradeFixture::new();
        fixture
            .command()
            .arg("--check-update")
            .assert()
            .success()
            .stdout(
                predicate::str::contains("Newer version available: v9999.0.0")
                    .and(predicate::str::contains("Update: bvr upgrade")),
            );
        fixture.assert_no_install();
    }
}
