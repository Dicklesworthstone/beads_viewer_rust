# beads_viewer_rust (`bvr`)

Rust port of `legacy_beads_viewer_code/beads_viewer` (`bv`).

Current objective: full-fidelity parity for robot mode + interactive TUI while leveraging:
- `/dp/frankentui` for TUI runtime and widgets
- `/dp/asupersync` for structured async orchestration (feature-gated integration path)

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
cargo run -- --robot-label-health
cargo run -- --robot-label-flow
cargo run -- --robot-label-attention
cargo run -- --robot-label-attention --attention-limit 5
cargo run -- --export-graph /tmp/deps.json
cargo run -- --export-graph /tmp/deps.dot --graph-preset roomy --graph-title "Dependency Snapshot"
cargo run -- --export-graph /tmp/deps.mmd --graph-title "Docs Diagram"
cargo run -- --export-graph /tmp/deps.svg --graph-style grid --graph-preset roomy --graph-title "Team Dependency Map"
cargo run -- --export-graph /tmp/deps.png --graph-style force --graph-title "Snapshot"
cargo run -- --export-pages ./bv-pages --pages-title "Sprint Dashboard"
cargo run -- --preview-pages ./bv-pages
cargo run -- --preview-pages ./bv-pages --no-live-reload
cargo run -- --export-pages ./bv-pages --watch-export
cargo run -- --pages
cargo run -- --background-mode --robot-triage
cargo run -- --no-background-mode --robot-triage

# run against an explicit fixture file
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

Bare command launches the interactive TUI:

```bash
cargo run
```

Current parity key slice in TUI includes `?` help overlay toggle/dismiss, `Tab` list/detail focus toggle, non-main `Enter` return-to-main-detail behavior, `Esc` back-or-clear-filter-or-quit-confirm behavior, view-key toggles for `b/i/g` (press again to return to main), `h` history view toggle, history-mode `c` confidence cycling, history-mode `v` bead/git timeline toggle (with enter jump from git timeline to issue detail), `o/c/r/a` filter hotkeys with filter-aware list navigation, board-mode `1/2/3/4` lane-jump selection keys, board-mode `H/L` first/last lane jumps, board-mode `0/$` first/last-in-lane selection, board-mode `e` empty-lane visibility toggle, board-mode `s` grouping cycling (`status`, `priority`, `type`), insights-mode `e` explanation toggle, insights-mode `x` calculation-proof toggle, and main-mode `s` sort cycling (`created asc/desc`, `priority`, `updated`, `default`).

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

## Verification Commands

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
```

## Benchmark

```bash
cargo bench --bench triage
```

## License

MIT License (with OpenAI/Anthropic Rider). See `LICENSE`.
