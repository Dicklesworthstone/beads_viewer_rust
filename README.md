# beads_viewer_rust (`bvr`)

Rust port of `legacy_beads_viewer_code/beads_viewer` (`bv`).

Current objective: full-fidelity robot-mode parity plus an evidence-driven FrankenTUI redesign for the interactive TUI while leveraging:
- `/dp/frankentui` for TUI runtime and widgets
- Background async via `std::thread::spawn` + `mpsc::channel` (two-phase metric computation for large graphs)

Current project reality:
- Robot/CLI parity is far ahead and is backed by fixture-heavy regression coverage.
- The interactive TUI is functional, but it is not yet at credible product parity with legacy `bv`.
- The active TUI goal is parity-first-then-beyond: recover legacy operator confidence and look/feel with fuller FrankenTUI usage, then exceed it with Rust-native improvements once the evidence exists.

> **Note:** `asupersync` is declared as an optional Cargo dependency (`asupersync-runtime` feature gate) but is not used at runtime. It is reserved as a post-parity enhancement path for structured async orchestration (watcher pipelines, background index builds). The current async needs (background metric computation, file reload) are handled directly with standard library primitives. Go's `bv` has no equivalent async framework, so this is not a parity requirement.

## Binary

```bash
cargo run -- --robot-help
cargo run -- --robot-triage
cargo run -- --robot-next
cargo run -- --robot-plan
cargo run -- --robot-insights
cargo run -- --robot-priority
cargo run -- --robot-alerts
cargo run -- --robot-alerts --severity warning --alert-type stale_issue
cargo run -- --robot-suggest
cargo run -- --robot-suggest --suggest-type cycle
cargo run -- --robot-suggest --suggest-confidence 0.7 --suggest-bead bd-101
cargo run -- --robot-diff --diff-since tests/testdata/minimal.jsonl
cargo run -- --robot-history --history-limit 20
cargo run -- --robot-history --history-since "2024-01-01T00:00:00Z" --history-limit 50
cargo run -- --robot-capacity --agents 2
cargo run -- --robot-capacity --capacity-label backend --agents 3
cargo run -- --robot-burndown current
cargo run -- --robot-forecast all --forecast-agents 2
cargo run -- --robot-forecast all --forecast-sprint sprint-1 --forecast-label backend
cargo run -- --robot-graph
cargo run -- --robot-graph --graph-format dot
cargo run -- --robot-graph --graph-format mermaid --graph-root B --graph-depth 2
cargo run -- --robot-graph --label api
cargo run -- --robot-metrics
cargo run -- --robot-docs guide
cargo run -- --robot-schema
cargo run -- --robot-sprint-list
cargo run -- --robot-sprint-show sprint-1
cargo run -- --robot-label-health
cargo run -- --robot-label-flow
cargo run -- --robot-label-attention
cargo run -- --robot-label-attention --attention-limit 5
cargo run -- --robot-search --search "auth" --search-limit 20
cargo run -- --robot-recipes
cargo run -- --robot-triage --recipe critical-blockers

# Profiling
cargo run -- --profile-startup --beads-file tests/testdata/synthetic_complex.jsonl
cargo run -- --profile-startup --profile-json --beads-file tests/testdata/synthetic_complex.jsonl

# Export
cargo run -- --export-md /tmp/report.md
cargo run -- --priority-brief /tmp/brief.md
cargo run -- --agent-brief /tmp/agent-output
cargo run -- --export-graph /tmp/deps.json
cargo run -- --export-graph /tmp/deps.dot --graph-preset roomy --graph-title "Dependency Snapshot"
cargo run -- --export-graph /tmp/deps.svg --graph-style grid --graph-preset roomy
cargo run -- --export-pages ./bv-pages --pages-title "Sprint Dashboard"
cargo run -- --preview-pages ./bv-pages
cargo run -- --pages

# Background mode
cargo run -- --background-mode --robot-triage
cargo run -- --no-background-mode --robot-triage

# Run against an explicit fixture file
cargo run -- --robot-triage --beads-file tests/testdata/minimal.jsonl
```

Background mode compatibility controls (TUI only) are accepted with legacy precedence:
- CLI flags: `--background-mode` / `--no-background-mode`
- Env override: `BV_BACKGROUND_MODE=1|0`
- User config fallback: `~/.config/bv/config.yaml` with:
  ```yaml
  experimental:
    background_mode: true
  ```

When enabled, TUI mode runs a bounded background reload loop that periodically refreshes issues and applies updates without blocking key handling.

Bare command launches the interactive TUI:

```bash
cargo run
```

## TUI Key Map

| Key | Context | Action |
|---|---|---|
| `?` | All | Toggle help overlay |
| `Tab` | All | Toggle list/detail focus |
| `Esc` | All | Back / clear filter / quit confirm |
| `b` | All | Toggle board view |
| `i` | All | Toggle insights view |
| `g` | All | Toggle graph view |
| `h` | All | Toggle history view |
| `s` | Main | Cycle sort (created asc/desc, priority, updated, default) |
| `o/c/r/a` | All | Filter by open/closed/review/all |
| `a` | All | Toggle actionable view |
| `!` | All | Toggle attention view |
| `T` | All | Toggle tree view |
| `[` | All | Toggle label dashboard |
| `]` | All | Toggle flow matrix |
| `t` | All | Toggle time-travel diff |
| `/` | Board, Graph, Insights, History | Search with n/N cycling |
| `J/K` | Board, Graph, Insights | Navigate dependency detail pane |
| `1/2/3/4` | Board | Jump to lane |
| `H/L` | Board | First/last lane |
| `0/$` | Board | First/last in lane |
| `e` | Board | Toggle empty lanes |
| `s` | Board | Cycle grouping (status/priority/type) |
| `e` | Insights | Toggle explanation |
| `x` | Insights | Toggle calculation proof |
| `c` | History | Cycle confidence |
| `v` | History | Toggle bead/git timeline |
| `Enter` | History (git) | Jump to issue detail |

## Porting Docs

- `PLAN_TO_PORT_BEADS_VIEWER_TO_RUST.md`
- `EXISTING_BEADS_VIEWER_STRUCTURE.md`
- `PROPOSED_ARCHITECTURE.md`
- `FEATURE_PARITY.md`

## Conformance Harness

Generate reference fixture from legacy Go implementation:

```bash
go run ./tests/conformance/go_reference/cmd/bvr/main.go \
  --legacy-root /data/projects/beads_viewer_rust/legacy_beads_viewer_code/beads_viewer \
  --beads-file /data/projects/beads_viewer_rust/tests/testdata/minimal.jsonl \
  --output /data/projects/beads_viewer_rust/tests/conformance/fixtures/go_outputs/bvr.json

# extended fixture with git-backed diff/history coverage
go run ./tests/conformance/go_reference/cmd/bvr/main.go \
  --legacy-root /data/projects/beads_viewer_rust/legacy_beads_viewer_code/beads_viewer \
  --beads-file /data/projects/beads_viewer_rust/tests/testdata/synthetic_complex.jsonl \
  --diff-before-file /data/projects/beads_viewer_rust/tests/testdata/minimal.jsonl \
  --output /data/projects/beads_viewer_rust/tests/conformance/fixtures/go_outputs/bvr_extended.json

# adversarial fixture with cycles + reopen churn + label edge cases
go run ./tests/conformance/go_reference/cmd/bvr/main.go \
  --legacy-root /data/projects/beads_viewer_rust/legacy_beads_viewer_code/beads_viewer \
  --beads-file /data/projects/beads_viewer_rust/tests/testdata/adversarial_parity.jsonl \
  --diff-before-file /data/projects/beads_viewer_rust/tests/testdata/minimal.jsonl \
  --output /data/projects/beads_viewer_rust/tests/conformance/fixtures/go_outputs/bvr_adversarial.json
```

Run Rust conformance tests:

```bash
cargo test --test conformance
```

Validate stress/adversarial fixture provenance metadata:

```bash
cargo test --test conformance stress_fixture_manifest_has_provenance_and_validated_counts
```

## Test Suite

| Suite | Command | Count |
|---|---|---|
| Unit tests | `cargo test --lib` | 936 |
| Snapshots | `cargo test --lib snap_` | 55 |
| Keyflow journeys | `cargo test --lib keyflow_` | 25 |
| E2E TUI journeys | `cargo test --lib e2e_journey_` | 5 |
| Conformance | `cargo test --test conformance` | 75 |
| Schema validation | `cargo test --test schema_validation` | 50 |
| E2E tests | `cargo test --test e2e_robot_matrix --test e2e_workspace_history --test e2e_export_pages` | 96 |
| Stress fixtures | `cargo test --test stress_fixtures` | 49 |
| Integration tests | `cargo test --test robot_alerts --test robot_burndown_scope --test robot_history_since --test export_md --test export_pages --test admin_cli --test background_mode --test cli_model_validation` | 148 |

Full suite:

```bash
cargo test
```

Optional e2e artifact bundles (stdout/stderr, replay command, metadata) for robot/debug-render triage:

```bash
BVR_E2E_ARTIFACT_DIR=target/bvr-e2e-artifacts cargo test --test e2e_robot_matrix
```

## CI

GitHub Actions workflow at `.github/workflows/ci.yml` runs on push/PR to main:
- **Check**: `cargo fmt --check` + `cargo clippy`
- **Unit**: lib tests + snapshot verification
- **Conformance**: conformance + schema validation
- **E2E**: robot command matrix + all integration tests
- **Bench**: Criterion smoke run
- **Build**: release binary with artifact upload

## Benchmark

```bash
cargo bench --bench triage
```

12 benchmark groups covering analyzer construction, triage, insights, plan, diff, forecast, suggest, alerts, history, cycle detection, real fixture, and stress fixture. Synthetic generators create sparse/dense/cyclic graphs at 100/500/1000 issues.

## License

MIT License (with OpenAI/Anthropic Rider). See `LICENSE`.
