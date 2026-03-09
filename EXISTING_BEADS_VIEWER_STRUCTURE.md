# Existing beads_viewer Structure (Legacy Go)

After reading this file, implementation should not need direct legacy-code rereads for core behavior.

## 1. Product Summary
`bv` is a Beads issue tracker viewer and triage engine with two major surfaces:
- Interactive TUI (keyboard-first).
- Robot/agent mode (`--robot-*`) that emits structured machine-consumable output.

## 2. Primary Data Model
From `pkg/model/types.go`:
- `Issue`: id/title/description/status/priority/type/assignee/timestamps/labels/dependencies/comments/etc.
- `Dependency`: `{ issue_id, depends_on_id, type, created_at, created_by }`.
- Blocking dependency semantics: `type == "blocks"` OR empty string (legacy default) are blocking.
- Status values include: `open`, `in_progress`, `blocked`, `deferred`, `pinned`, `hooked`, `review`, `closed`, `tombstone`.

Validation requirements:
- `id` and `title` required.
- `status` must be known.
- `issue_type` must be non-empty.
- `updated_at >= created_at` when both present.

## 3. Loader Behavior
From `pkg/loader/loader.go`:
- Beads directory selection:
  - `BEADS_DIR` env override first.
  - Otherwise `<repo>/.beads` (with git-worktree main-repo fallback).
- JSONL file selection priority:
  1. `beads.jsonl`
  2. `issues.jsonl`
  3. `beads.base.jsonl`
- Parsing behavior:
  - Skip malformed lines with warning.
  - Skip invalid issues with warning.
  - Strip UTF-8 BOM on first line.
  - Large-line guardrail via configurable buffer.
  - Warnings suppressed in robot mode (`BV_ROBOT=1`).

## 4. Analysis Graph Semantics
From `pkg/analysis/graph.go`:
- Directed graph includes only blocking dependencies.
- Edge direction: `issue -> blocker` (`issue.depends_on_id`).
- Key derived sets:
  - `actionable`: open-like issues with no open blockers.
  - blockers/open blockers per issue.
  - cycle and critical-path awareness.

## 5. Robot Command Families
Primary flags in `cmd/bv/main.go` include:
- `--robot-triage`, `--robot-next`
- `--robot-plan`
- `--robot-insights`
- `--robot-priority`
- `--robot-diff`
- `--robot-history`
- `--robot-forecast`, `--robot-capacity`, `--robot-burndown`
- `--robot-suggest`
- `--robot-graph`

Output conventions:
- Structured payload to stdout.
- Diagnostics to stderr.
- Deterministic metadata fields commonly include `generated_at`, `data_hash`, analysis status/config.

## 6. TUI Behavioral Surface
Key aspects from README + `pkg/ui/*`:
- Split list/detail main view.
- Kanban board (`b`), insights (`i`), graph (`g`), history (`h`).
- Vim-style navigation (`j/k`, `gg`, `G`) and keyboard-centric operation.
- Adaptive layouts with width thresholds around 100 / 140 / 180 columns.

## 7. External Integration Surfaces
- Hooks lifecycle (`.bv/hooks.yaml`) around export operations.
- Workspace aggregation across repositories (`.bv/workspace.yaml`).
- File watchers and live-reload pipelines.
- Correlation/history stack for commit-to-bead and CASS enrichment.

## 8. Porting Constraints
- Full parity target: all legacy robot and TUI workflows.
- Rust implementation must leverage:
  - `frankentui` for UI runtime/widgets/layout.
  - Standard library `std::thread::spawn` + `mpsc::channel` for background async (two-phase metrics, file reload). `asupersync` is an optional post-parity enhancement path.
