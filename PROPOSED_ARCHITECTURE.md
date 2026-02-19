# Proposed Rust Architecture (`bvr`)

## 1. Layered Modules
- `src/model.rs`: Beads domain types + validation.
- `src/loader.rs`: `.beads` discovery and JSONL parsing.
- `src/analysis/`
  - `graph.rs`: graph build + metrics.
  - `triage.rs`: recommendations and quick refs.
  - `plan.rs`: execution tracks and summaries.
- `src/robot.rs`: output envelopes and robot payload types.
- `src/tui.rs`: interactive app model powered by `frankentui`.
- `src/cli.rs`: clap flags and dispatch.
- `src/main.rs`: runtime entry and command routing.

## 2. Dependency Strategy
- `ftui` (from `/dp/frankentui/crates/ftui`) for TUI runtime.
- `asupersync` feature-gated integration (`asupersync-runtime`) for background workers.
- `petgraph` for graph primitives; custom domain scoring on top.
- `serde`/`serde_json` for robot output and fixtures.

## 3. Runtime Modes
- Robot mode: deterministic JSON-first output for `--robot-*` commands.
- Interactive mode: `bvr` without robot flags launches TUI.

## 4. Conformance Design
- `tests/conformance/go_reference/cmd/bvr/main.go`: captures legacy Go outputs.
- `tests/conformance/fixtures/go_outputs/bvr.json`: canonical fixtures.
- `tests/conformance.rs`: Rust-vs-Go fixture comparisons.

## 5. Bench Design
- `benches/triage.rs` measures loader+analysis+triage hot path.
- Bench inputs include minimal and synthetic fixtures.
- Future wave: compare against captured Go baseline numbers.

## 6. Async Integration Plan (`asupersync`)
Feature-gated adapters will support:
- watcher pipelines,
- background index builds,
- bounded cleanup/cancellation for long-running analysis.

The default build remains lightweight; advanced async orchestration is opt-in.

## 7. TUI Fidelity Roadmap (`frankentui`)
- Wave 1: split list/detail, keyboard navigation, status line.
- Wave 2: board/graph/insights/history dedicated views.
- Wave 3: rendering parity polish and snapshot-level visual checks.
