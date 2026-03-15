# Proposed Rust Architecture (`bvr`)

## 1. Layered Modules
- `src/model.rs`: Beads domain types + validation.
- `src/loader.rs`: `.beads` discovery and JSONL parsing.
- `src/error.rs`: `BvrError` enum for all error types.
- `src/analysis/`
  - `mod.rs`: `Analyzer` struct — owns issues, graph, and metrics.
  - `graph.rs`: graph build + metrics (PageRank, betweenness, HITS, eigenvector, k-core, cycles, articulation points).
  - `triage.rs`: recommendations, quick refs, blocker analysis.
  - `plan.rs`: execution tracks and summaries.
  - `suggest.rs`: improvement suggestions with confidence scoring.
  - `alerts.rs`: issue alerts with severity levels.
  - `diff.rs`: snapshot comparison between issue states.
  - `forecast.rs`: workload estimation and scheduling.
  - `history.rs`: issue change history construction.
  - `git_history.rs`: git-backed history with commit correlation.
  - `brief.rs`: priority brief and agent brief generation.
  - `recipe.rs`: named triage recipes for filtering/sorting.
  - `search.rs`: hybrid search with fuzzy/exact/regex modes and ranking presets.
  - `label_intel.rs`: label health, flow analysis, and attention scoring.
  - `correlation.rs`: inter-issue correlation analysis.
  - `causal.rs`: blocker chain and causality networks.
  - `drift.rs`: baseline save/load and drift detection.
  - `file_intel.rs`: file-to-bead mapping and hotspot analysis.
- `src/robot.rs`: output envelopes, data hashing, and robot payload types.
- `src/tui.rs`: interactive app model powered by `frankentui` (current Rust implementation has 11 view modes, but the major operator-facing modes remain under active redesign to reach credible product-quality parity).
- `src/cli.rs`: clap flags and dispatch.
- `src/main.rs`: runtime entry and command routing.

## 2. Dependency Strategy
- `ftui` (from `/dp/frankentui/crates/ftui`) for TUI runtime.
- `asupersync` feature-gated integration (`asupersync-runtime`) for background workers.
- `serde`/`serde_json` for robot output and fixtures.
- `chrono` for timestamps.
- `clap` for CLI argument parsing.
- `criterion` for benchmarks.
- `insta` for snapshot testing.

## 3. Runtime Modes
- Robot mode: deterministic JSON-first output for `--robot-*` commands.
- Interactive mode: `bvr` without robot flags launches TUI.
- Profile mode: `--profile-startup` measures load/build/triage/insights timing.

## 4. Conformance Design
- `tests/conformance/go_reference/cmd/bvr/main.go`: captures legacy Go outputs.
- `tests/conformance/fixtures/go_outputs/bvr.json`: canonical fixtures (minimal, extended, adversarial).
- `tests/conformance.rs`: 73 Rust-vs-Go fixture comparisons.
- `tests/schema_validation.rs`: 31 JSON schema contract validations.
- `tests/test_utils/mod.rs`: ordering-invariant JSON comparator with typed schema validation.

## 5. Bench Design
- `benches/triage.rs`: 11 benchmark groups covering all analysis paths.
- Synthetic generators: `gen_sparse`, `gen_dense`, `gen_cyclic` at 100/500/1000 issues.
- Groups: analyzer_new, triage, insights, plan, diff, forecast, suggest, alerts, history, cycle_detection, real_fixture.
- All benchmarks complete sub-15ms at 1000 issues on standard hardware.

## 6. Async Strategy

Current async needs are met with standard library primitives:
- Two-phase metric computation: `std::thread::spawn` + `mpsc::channel` for graphs >200 nodes.
- Background file reload: same pattern via `ftui::TaskSpec` tick polling.
- No tokio or async runtime required for current feature set.

**Post-parity path:** `asupersync` is declared as an optional Cargo dependency (`asupersync-runtime` feature gate) for future structured async orchestration (watcher pipelines, background index builds, bounded cleanup/cancellation). This is not a Go parity requirement — Go's `bv` has no equivalent async framework.

## 7. TUI Fidelity Roadmap (`frankentui`)
- Current reality: the Rust TUI is functional and heavily regression-tested, but the repo should not currently describe it as a finished legacy-quality replacement for the Go `bv` TUI.
- Active redesign contract: `bd-2bpl.3.6.*` is the canonical FrankenTUI-first redesign program for the interactive product surface.
- Current redesign order:
  - contract and truthfulness reset
  - shared shell/layout/visual/text infrastructure
  - flagship main-screen rebuild
  - board/graph/insights/history rebuilds
  - redesign proof package with screen-family tests, hit-region tests, realistic scenario datasets, and replayable shell-level journeys
- Regression harness status: current snapshots/keyflows/journeys are valuable stability checks for the present implementation, but they are not by themselves proof that the redesign is complete.
