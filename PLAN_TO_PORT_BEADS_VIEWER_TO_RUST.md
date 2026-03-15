# Plan: Port beads_viewer (`bv`) to Rust (`bvr`)

## Executive Summary
This repository is building a Rust port of `legacy_beads_viewer_code/beads_viewer`, with a new binary named `bvr` that aims to preserve legacy `bv` behavior for both robot/agent automation and the interactive TUI.

Current reality:
- Robot-mode and data/export surfaces are much closer to parity and have substantially stronger fixture/integration evidence.
- The interactive TUI is functional, but it is not yet a credible product-quality replacement for the legacy Go `bv` TUI.
- The active TUI program is parity-first-then-beyond: recover legacy operator confidence and look/feel first, then exceed it with fuller FrankenTUI-native capabilities once the proof exists.

The port is spec-first:
1. Extract behavior/spec from legacy Go.
2. Implement Rust from the spec.
3. Verify with conformance fixtures and performance benchmarks.

## Goals
- Reach 100% functionality/behavioral parity with legacy `bv`.
- Preserve robot-mode contracts (`--robot-*`) for AI agents.
- Rebuild the interactive TUI using the full relevant `/dp/frankentui` capability set rather than a minimal `Paragraph`/`Block` shell.
- Use standard library async primitives (`std::thread::spawn` + `mpsc::channel`) for background work; `asupersync` integration is a post-parity enhancement.
- Provide feature-parity visibility and regression safety through fixture-based conformance tests.

Important note:
- The TUI parity bar is not "the Rust app has all the same modes and many keybindings." It is "the Rust app recovers legacy workflow confidence, density, and look/feel with evidence strong enough to justify parity claims."

## Non-Goals (Current Bootstrap Pass)
These are explicitly deferred for later parity waves, not dropped:
- Full static site export + preview server parity.
- Full update/install/self-update flows.
- Complete history/correlation + CASS integration parity.
- All graph export formats and wizard flows.
- All advanced label dashboards and every legacy modal.

## Reference Projects
- `/dp/frankentui`: TUI runtime, layout, widget primitives.
- `/dp/asupersync`: structured async orchestration (post-parity enhancement, not required for Go parity).
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
- Reset the tracker and repo docs so they stop overstating current TUI parity.
- Build the shared FrankenTUI shell/layout contract first: mode tabs, status/help surfaces, theme/discoverability, responsive layout tiers, adjustable panes, hit-tested regions, semantic panels, hyperlinks, and grapheme-safe text handling.
- Rebuild flagship screens and workflows around that contract: main, board, graph, insights, and history.
- Prove the redesign with screen-family tests, keyflows, mouse/hit-region tests, realistic scenario datasets, and replayable shell-level journeys with artifact logging.

### Phase 5: Conformance + Bench + Hardening
- Capture legacy fixture outputs with Go reference harness.
- Run fixture comparison in Rust test suite.
- Benchmark hot paths and enforce no-regression thresholds.

## Success Criteria
- `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` all pass.
- Conformance suite green against reference fixtures.
- Feature parity matrix marks legacy capabilities complete only where the evidence is strong enough to justify it.
- `bvr` robot output trusted as drop-in for current `bv` agent workflows.
- `bvr` TUI is not described as a full legacy-quality replacement until the redesign contract and proof package are complete.
