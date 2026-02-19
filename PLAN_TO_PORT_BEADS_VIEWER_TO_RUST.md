# Plan: Port beads_viewer (`bv`) to Rust (`bvr`)

## Executive Summary
This repository will become a full-fidelity Rust port of `legacy_beads_viewer_code/beads_viewer`, with a new binary named `bvr` that preserves legacy `bv` behavior for both robot/agent automation and the interactive TUI.

The port is spec-first:
1. Extract behavior/spec from legacy Go.
2. Implement Rust from the spec.
3. Verify with conformance fixtures and performance benchmarks.

## Goals
- Reach 100% functionality/behavioral parity with legacy `bv`.
- Preserve robot-mode contracts (`--robot-*`) for AI agents.
- Rebuild the interactive TUI using `/dp/frankentui` primitives.
- Leverage `/dp/asupersync` for structured async background work (watchers, indexing, correlation jobs).
- Provide feature-parity visibility and regression safety through fixture-based conformance tests.

## Non-Goals (Current Bootstrap Pass)
These are explicitly deferred for later parity waves, not dropped:
- Full static site export + preview server parity.
- Full update/install/self-update flows.
- Complete history/correlation + CASS integration parity.
- All graph export formats and wizard flows.
- All advanced label dashboards and every legacy modal.

## Reference Projects
- `/dp/frankentui`: TUI runtime, layout, widget primitives.
- `/dp/asupersync`: structured async orchestration, cancellation-correct workers.
- `/dp/rich_rust`: conformance discipline and output polish.
- `/dp/beads_rust`: beads-domain behavior and data conventions.

## Implementation Phases

### Phase 1: Bootstrap + Spec (in progress)
- Create spec docs and parity matrix.
- Establish crate/toolchain/lints/release profile.
- Build initial command skeleton for robot and TUI modes.

### Phase 2: Core Data + Analysis Engine
- Port issue loader semantics (`.beads` discovery, JSONL parsing, warning behavior).
- Port graph construction and core metrics needed by triage/plan/insights.
- Port recommendation/priority scoring and deterministic ordering.

### Phase 3: Robot Surface Parity
- Implement all high-use robot endpoints (`triage`, `next`, `plan`, `insights`, `priority`, `diff`, `history`, `forecast`, `capacity`, `burndown`, `suggest`, `graph`).
- Preserve output contracts and metadata fields.

### Phase 4: TUI Fidelity on FrankenTUI
- Implement multi-view layout and keybinding parity.
- Recreate split-view, board, graph, insights, and history flows.
- Add snapshot/golden rendering tests.

### Phase 5: Conformance + Bench + Hardening
- Capture legacy fixture outputs with Go reference harness.
- Run fixture comparison in Rust test suite.
- Benchmark hot paths and enforce no-regression thresholds.

## Success Criteria
- `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` all pass.
- Conformance suite green against reference fixtures.
- Feature parity matrix marks all legacy capabilities complete.
- `bvr` robot output trusted as drop-in for current `bv` agent workflows.
