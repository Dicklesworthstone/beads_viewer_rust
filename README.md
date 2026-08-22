# beads_viewer_rust (`bvr`)

<div align="center">

```text
JSONL / workspace config / git history
                │
                ▼
         loader + model validation
                │
                ▼
   IssueGraph + Analyzer + graph metrics
                │
     ┌──────────┼──────────┬───────────────┐
     ▼          ▼          ▼               ▼
  robot JSON   TOON       FrankenTUI      static pages
```

[![CI](https://github.com/Dicklesworthstone/beads_viewer_rust/actions/workflows/ci.yml/badge.svg)](https://github.com/Dicklesworthstone/beads_viewer_rust/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

</div>

`bvr` is a graph-aware issue triage engine for `.beads` data that gives you machine-friendly robot output, an interactive terminal UI, and a self-contained static dashboard export from the same binary.

## Quick Install

```bash
cargo install --git https://github.com/Dicklesworthstone/beads_viewer_rust.git beads_viewer_rust
```

## TL;DR

**The Problem:** issue backlogs are easy to store but hard to prioritize. Once dependencies, blockers, stale work, cross-label bottlenecks, workspace layouts, and git history enter the picture, manual sorting or ad-hoc `jq` scripts stop being trustworthy.

**The Solution:** `bvr` loads `.beads` issue data, builds a dependency graph, computes centrality and planning metrics, and then exposes the result through robot commands, a TUI, markdown briefs, graph export, and static pages bundles.

### Why Use `bvr`?

| Capability | What It Does |
|---|---|
| **Robot-first triage** | Emits structured JSON or TOON for agents, scripts, and automation via `--robot-*` commands. |
| **Graph-aware planning** | Computes PageRank, betweenness, HITS, eigenvector, k-core, cycles, critical path, articulation points, and slack. |
| **Multiple operator surfaces** | Supports automation, a FrankenTUI, markdown briefs, graph export, and pages export from one codebase. |
| **Workspace-aware loading** | Understands `.beads/` layouts, compatibility filenames, and `.bv/workspace.yaml` aggregation. |
| **History and feedback loops** | Correlates git history, supports drift baselines, recommendation feedback, and correlation review commands. |
| **Evidence-heavy testing** | Backed by conformance, schema, e2e, workspace/history, export, snapshot, and stress tests. |

## Quick Example

```bash
# 1. Ask for the single best next move
bvr --robot-next

# 2. Get a compact orientation snapshot
bvr --robot-overview

# 3. Get the full triage payload
bvr --robot-triage

# 4. Inspect graph metrics and cycles
bvr --robot-insights

# 5. See an execution plan grouped into parallel tracks
bvr --robot-plan

# 6. Search across issue text and metadata
bvr --robot-search --search "auth" --search-limit 5

# 7. Export a static bundle for sharing
bvr --export-pages ./bv-pages --pages-title "Sprint Dashboard"

# 8. Preview it locally
bvr --preview-pages ./bv-pages
```

## What `bvr` Solves

`bvr` is the Rust port of the legacy Go `bv` tool, but the current codebase is no longer just a straight porting spike. Today it includes:

- Robot-mode parity work for the legacy CLI surface.
- A much larger analysis surface than simple ranking.
- A functional FrankenTUI with the main issue view plus 11 specialized modes.
- Static pages export, preview, watch mode, and a pages deployment wizard.
- Workspace-aware loading and git-history-aware analysis.

The current direction is to lock down parity first, then extend from there:

- Keep the machine-facing behavior compatible and evidence-driven.
- Recover operator confidence in the interactive workflows.
- Extend the Rust version with capabilities that make sense natively here.

## Problem Cases `bvr` Handles Well

`bvr` is most useful when a backlog has enough structure that naive sorting starts lying to you. Common cases:

### 1. A high-priority issue is noisy, but not actually central

An issue can be marked urgent and still not unblock much of anything. `bvr` distinguishes declared priority from structural importance, which is why the triage surface includes both priority signals and graph-derived signals.

### 2. A mediocre-looking issue is the real bottleneck

Sometimes the most important work is a root blocker, a bridge between clusters, or an articulation point that keeps whole branches connected. Those rarely stand out in a plain list view.

### 3. A team is drowning in stale work and cannot tell what is safely ignorable

Staleness alone is not enough. A stale leaf item and a stale central blocker are very different problems. `bvr` combines freshness, dependency structure, and blocker counts, so stale-work review does not collapse into "oldest first."

### 4. Multiple repos act like one delivery system

In a workspace layout, the API repo, frontend repo, and shared library repo can form one planning graph. `bvr` can aggregate that via `.bv/workspace.yaml` instead of forcing you to triage each repo in isolation.

### 5. Operators need different surfaces for the same underlying truth

Agents want structured output. Humans want TUI workflows. Stakeholders may want a static dashboard. `bvr` exists so those do not drift into separate, contradictory tools.

### 6. "What changed?" matters as much as "What exists?"

Baselines, diffs, history correlation, and export/watch flows exist because triage is usually about movement over time, not a single snapshot of open issues.

## Design Philosophy

### 1. One source of truth, many surfaces

The loader and analyzer feed everything else. Robot output, TUI screens, markdown briefs, and pages export all work from the same issue graph instead of reimplementing their own logic.

> **Substrate caveat:** `bvr` does not introduce drift between the surfaces it renders, but it inherits whatever state the substrate (the `br` CLI writing `.beads/beads.db` and `.beads/issues.jsonl`) hands it. The two beads stores can themselves diverge under specific failure modes — close-path persistence races ([beads_rust#285](https://github.com/Dicklesworthstone/beads_rust/issues/285)) and id-set divergence visible only through `br doctor` ([beads_rust#286](https://github.com/Dicklesworthstone/beads_rust/issues/286)) are the two known classes. `bvr` reads `issues.jsonl` preferentially when both stores exist, so the screens you see are correct against the JSONL stream even when SQLite is behind; `br sync --rebuild` reconciles the SQLite side. Run `br doctor` periodically if you need substrate-level drift signals — `bvr` itself does not surface them.

### 2. Graph structure matters more than raw counts

Priority is only one field on an issue. `bvr` also looks at blockers, centrality, flow, and path structure so it can distinguish a noisy task from a true bottleneck.

### 3. Automation should not scrape terminal output

Robot commands are first-class. If you are integrating with agents or scripts, use `--robot-*` and optionally `--format toon` instead of screen-scraping the TUI.

### 4. Workspace and path semantics are product behavior

This project treats `.beads` discovery, workspace aggregation, repo-path handling, watch paths, preview serving, and history resolution as part of the real contract, not implementation trivia.

### 5. Parity claims need evidence

The repo carries conformance fixtures, schema validation, e2e coverage, snapshots, and stress fixtures because "probably compatible" is not a useful standard for a triage engine.

## Why Graph-Aware Triage Beats Naive Sorting

A normal issue list encourages bad habits:

- sort by declared priority
- maybe filter by status
- maybe scan titles by hand

That works until dependencies matter. Once issues block each other, connect teams, span repos, or form cycles, the backlog becomes a graph problem rather than a spreadsheet problem.

`bvr` treats the backlog that way. Instead of asking only "what is marked important?", it also asks:

- what work influences the most downstream work?
- what issues sit on the bridges between clusters?
- what items are central but not obviously loud?
- what gets value moving fastest if finished now?
- what work is risky because it is in cycles or behind blockers?

That is why `--robot-next` and `--robot-triage` are graph-aware ranking outputs, not dressed-up sort orders.

## How Scoring Works

The recommendation engine does not rely on a single metric. The current impact score combines multiple normalized components and then renormalizes them into a single score.

At a high level, the scoring model considers:

| Component | What It Captures | Why It Matters |
|---|---|---|
| **PageRank** | Global centrality in the dependency graph | Finds issues that influence a lot of important work |
| **Betweenness** | Bridge importance between regions of the graph | Finds chokepoints and routing issues |
| **BlockerRatio** | How much open work this issue blocks | Rewards high-unblock items |
| **Staleness** | How recently the issue moved | Prevents long-dead work from floating to the top by default |
| **PriorityBoost** | Declared issue priority | Respects operator intent without blindly obeying it |
| **TimeToImpact** | How quickly value propagates after completion | Rewards work near the roots of dependency chains |
| **Urgency** | Status-based urgency signal | Distinguishes active, open, review, blocked, and deferred states |
| **Risk** | Open blockers, cycle membership, articulation-point risk | Discounts work that is structurally risky to execute now |

Two important design choices:

- The model is **multi-signal**, so no single bad field dominates the ranking.
- The model is **explainable**, because recommendations carry reasons rather than just a score.

This is also why `bvr` can support recommendation feedback: the score is composed from named components, not a black-box classifier.

## Algorithms Under the Hood

The analysis engine uses standard graph algorithms, but it applies them to issue planning rather than academic toy graphs.

### Core metrics

| Algorithm | Used For |
|---|---|
| **PageRank** | Global influence / importance |
| **Betweenness centrality** | Bridge and bottleneck detection |
| **Eigenvector centrality** | Influence based on influential neighbors |
| **HITS** | Hub and authority style ranking |
| **K-core decomposition** | Dense structural cores in the graph |
| **Articulation point detection** | Single points of structural failure |
| **Critical depth / critical path signals** | Long dependency-chain importance |
| **Slack computation** | How much scheduling flexibility exists |
| **Strongly connected component cycle detection** | Detecting circular dependencies |

### Related analysis families

Beyond pure graph centrality, the repo also includes:

- forecast analysis
- execution plan grouping
- label health and cross-label flow
- git-history correlation
- drift against saved baselines
- search and recipe filtering
- file-to-bead and hotspot analysis
- blocker-chain, impact-network, and causality views

### Practical note on scale

The code intentionally treats some metrics as more expensive than others. Faster metrics can be computed immediately, while slower metrics can be deferred or sampled on larger graphs.

## Why These Algorithms Matter for Issue Triage

Listing algorithms is not enough. Their value in this repo is operational:

- **PageRank** helps surface issues that matter globally, not just locally.
- **Betweenness** highlights bridge work that sits between clusters or teams.
- **Articulation points** expose single issues whose removal would fragment the dependency graph.
- **K-core** helps distinguish deep structural clusters from shallow leaves.
- **Cycle detection** catches planning deadlocks that look normal in a flat list.
- **Critical depth and slack** make scheduling more defensible than gut feel.

If you have ever asked "why is this weird-looking issue ranked so high?", the answer is usually one of those structural effects.

## How `bvr` Compares

| Feature | `bvr` | legacy Go `bv` | ad-hoc `jq` / scripts | manual triage |
|---|---|---|---|---|
| Structured robot output | ✅ JSON + TOON | ✅ JSON-style robot output | ⚠️ custom per script | ❌ |
| Interactive TUI | ✅ functional, expanding | ✅ mature baseline | ❌ | ❌ |
| Static pages export | ✅ built in | ✅ | ❌ | ❌ |
| Workspace-aware loading | ✅ `.bv/workspace.yaml` + repo-path semantics | ✅ baseline behavior | ❌ usually custom | ❌ |
| Graph metrics | ✅ broad built-in set | ✅ core set | ⚠️ manual and brittle | ❌ |
| Drift/correlation/file intel surfaces | ✅ built in | ⚠️ mixed / legacy-dependent | ❌ | ❌ |
| Best fit today | automation + analysis + export | legacy operator muscle memory | one-off filtering | small backlogs |

**Use `bvr` when:**

- You want a single binary for triage, diagnostics, dashboards, and automation.
- You are working in a `.beads`-based repo and need dependency-aware ranking.
- You want machine output that is easier to consume than terminal text.

**Prefer something else when:**

- You only need one quick text filter on a tiny file.
- You require exact legacy TUI feel today and do not want parity-in-progress behavior.

## Installation

### 1. Install from GitHub with Cargo

```bash
cargo install --git https://github.com/Dicklesworthstone/beads_viewer_rust.git beads_viewer_rust
```

The crate is named `beads_viewer_rust` (the short `bvr` name was already taken on crates.io by an unrelated crate), but it installs a binary named `bvr`. The same crate is also published to crates.io, so `cargo install beads_viewer_rust` works too.

### 2. Build from a local checkout

```bash
git clone https://github.com/Dicklesworthstone/beads_viewer_rust.git
cd beads_viewer_rust
cargo build --release
./target/release/bvr --robot-help
```

When validating the current checkout, prefer the binary built from that checkout (`./target/debug/bvr` or `./target/release/bvr`). Do not assume a global `bv` wrapper or previously installed binary matches the source tree you are editing.

### 3. Install from a local checkout into your Cargo bin dir

```bash
git clone https://github.com/Dicklesworthstone/beads_viewer_rust.git
cd beads_viewer_rust
cargo install --path .
```

### Upgrading

```bash
bvr --check-update   # non-mutating: report whether a newer version exists
bvr upgrade          # install the latest release via `cargo install beads_viewer_rust --locked`
bvr upgrade --dry-run  # show what would be installed without changing anything
```

`bvr upgrade` resolves the latest version from crates.io (falling back to GitHub releases/tags) and reinstalls through cargo, so it works for any cargo-based install above. It requires the Rust toolchain to be present — the same prerequisite as installing in the first place.

### Toolchain

- Rust edition: `2024`
- Minimum Rust version: `1.85`
- Binary name: `bvr`

## Quick Start

### 1. Point `bvr` at issue data

By default, `bvr` looks for `.beads/` data in the current repository. It also supports compatibility filenames such as `issues.jsonl` and `beads.base.jsonl`.

```bash
bvr --robot-triage
```

If you want to bypass auto-discovery:

```bash
bvr --robot-triage --beads-file tests/testdata/minimal.jsonl
```

### 2. Use workspace mode when one repo is not enough

`bvr` can aggregate multiple repos through `.bv/workspace.yaml`.

```yaml
repos:
  - path: services/api
  - path: apps/web
```

```bash
bvr --workspace .bv/workspace.yaml --robot-plan
```

### 3. Start with the high-signal robot commands

```bash
bvr --robot-next
bvr --robot-overview
bvr --robot-triage
bvr --robot-plan
bvr --robot-insights
```

### 4. Use the TUI for operator workflows

```bash
bvr
```

### 5. Export a shareable dashboard bundle

```bash
bvr --export-pages ./bv-pages --pages-title "Sprint Dashboard"
bvr --preview-pages ./bv-pages
```

## Input Model

### Default data sources

`bvr` loads issues from `.beads` by default, with compatibility for:

- `beads.jsonl`
- `issues.jsonl`
- `beads.base.jsonl`

It can also aggregate repositories via `.bv/workspace.yaml`.

### Output formats

Robot commands can emit:

- `json` via `--format json`
- `toon` via `--format toon`

Examples:

```bash
bvr --robot-triage --format json
bvr --robot-triage --format toon
BV_OUTPUT_FORMAT=toon bvr --robot-next
```

## Robot Output Philosophy

The robot surface exists so external automation does not have to guess at terminal text.

### Design goals

- **Deterministic structure** for scripts and agents
- **Shared envelope fields** such as generation timestamp, data hash, output format, and version
- **Discoverability** through `--robot-help`, `--robot-docs`, and `--robot-schema`
- **Compact output** through TOON when JSON is too expensive for agent loops

### Why both JSON and TOON exist

JSON is the safest default for downstream tooling. TOON exists because agent workflows often care about token cost and readability more than strict JSON syntax.

### Why the schema/docs commands matter

The repo emits payloads and machine-readable descriptions of their contracts. That matters for:

- agent integration
- regression detection
- contract review
- new command discovery

## Workspace and Path Semantics

Path handling is one of the subtle but critical parts of `bvr`.

### What can be discovered automatically

`bvr` can look for:

- `.beads/` in the current repo or ancestors
- compatibility JSONL filenames
- `.bv/workspace.yaml` for multi-repo aggregation

### Why this gets its own section

In this project, path resolution affects:

- which issues are loaded
- whether workspace aggregation happens
- where watch mode listens for changes
- where historical loads and diffs resolve from
- where pages workflows and persisted state live

### Practical usage rules

- Use `--beads-file` when you want an exact source file and do not want discovery.
- Use `--workspace` when you want an exact workspace config and do not want discovery.
- Let auto-discovery work when the repo layout is conventional and you want convenience.

## Static Pages, Preview, Watch, and Wizard

The pages system is a first-class surface, not an afterthought.

### What export produces

The bundle includes:

- `index.html`
- local viewer assets
- JSON payloads for issues, metadata, triage, and optional history
- a SQLite database bundle
- static-host helper files such as `_headers`
- a deploy-facing bundle README

### Why pages export exists

It solves a different problem than robot mode:

- robot mode is for agents and automation
- the TUI is for interactive operators
- pages export is for sharing, review, and lightweight dashboard publishing

### Preview server

The preview flow provides a local HTTP server for the exported bundle, a status endpoint, and optional live reload. It is intentionally geared toward testing the real exported artifact rather than a separate dev-only web app.

### Watch mode

`--watch-export` exists for a fast edit-refresh cycle when the issue data changes and you want the static bundle to keep up.

### Pages wizard

`--pages` is an interactive deployment-oriented workflow that helps collect export options, target settings, preview choices, and deployment instructions without requiring the operator to remember every flag.

## Command Reference

For the machine-readable inventory, use:

```bash
bvr --robot-help
bvr --robot-docs commands
bvr --robot-schema
```

### Core triage and planning

```bash
bvr --robot-next
bvr --robot-overview
bvr --robot-triage
bvr --robot-triage-by-track
bvr --robot-triage-by-label
bvr --robot-plan
bvr --robot-priority
bvr --robot-alerts
bvr --robot-suggest
```

Use these when you want a fast orientation snapshot, ranked recommendations, quick wins, blockers to clear, grouped tracks, or priority mismatch detection.

### Graph analysis and forecasting

```bash
bvr --robot-insights
bvr --robot-graph --graph-format json
bvr --robot-graph --graph-format dot --graph-root B --graph-depth 2
bvr --robot-forecast all --forecast-agents 2
bvr --robot-capacity --agents 3
bvr --robot-burndown current
bvr --robot-sprint-list
bvr --robot-sprint-show sprint-1
```

Use these when you need graph metrics, projected execution, sprint state, or graph exports for downstream tools.

### History, diff, drift, and correlation

```bash
bvr --robot-history --history-limit 20
bvr --robot-diff --diff-since HEAD~10
bvr --save-baseline baseline.json
bvr --robot-drift
bvr --check-drift
bvr --robot-explain-correlation <sha:bead>
bvr --robot-confirm-correlation <sha:bead>
bvr --robot-reject-correlation <sha:bead>
bvr --robot-correlation-stats
```

Use these when you want git-aware change tracking, baseline comparisons, or a feedback loop for commit-to-bead correlations.

### Label, search, and file intelligence

```bash
bvr --robot-label-health
bvr --robot-label-flow
bvr --robot-label-attention --attention-limit 5
bvr --robot-search --search "auth" --search-limit 10
bvr --robot-recipes
bvr --robot-orphans
bvr --robot-file-beads src/main.rs
bvr --robot-file-hotspots
bvr --robot-impact bd-123
bvr --robot-file-relations src/main.rs
bvr --robot-related bd-123
bvr --robot-blocker-chain bd-123
bvr --robot-impact-network bd-123 --network-depth 3
bvr --robot-causality bd-123
```

Use these when you need label health, workspace search, orphan detection, file-to-bead mapping, or impact analysis.

### Export, reports, and automation helpers

```bash
bvr --export-md /tmp/report.md
bvr --priority-brief /tmp/priority-brief.md
bvr --agent-brief /tmp/agent-brief
bvr --export-graph /tmp/deps.json
bvr --export-pages ./bv-pages
bvr --preview-pages ./bv-pages
bvr --export-pages ./bv-pages --watch-export
bvr --pages
```

These commands generate static artifacts, local previews, and operator-facing deployment guidance.

### TUI and diagnostics

```bash
bvr
bvr --debug-render graph --debug-width 160 --debug-height 50
bvr --profile-startup
bvr --profile-startup --profile-json
bvr --background-mode --robot-triage
bvr --no-background-mode --robot-triage
```

Use these when you want the interactive UI, deterministic render output for debugging, or startup timing diagnostics.

### AGENTS.md workflow helpers

```bash
bvr --agents-check
bvr --agents-add
bvr --agents-update
bvr --agents-remove
```

These commands inspect or manage the beads workflow blurb inside `AGENTS.md`.

## TUI Overview

Bare `bvr` launches the interactive terminal UI. For automation, do not run the bare command; use `--robot-*`.

### Main view plus 11 specialized modes

| Key | Mode | Purpose |
|---|---|---|
| default | Main | Issue list with detail pane |
| `b` | Board | Kanban-style lane view |
| `i` | Insights | Metric and explanation panels |
| `g` | Graph | Dependency graph and edge inspection |
| `H` | History | Bead/git timeline and file tree |
| `a` | Actionable | Parallel execution tracks |
| `!` | Attention | Label attention ranking |
| `T` | Tree | Dependency tree |
| `[` | Labels | Label health dashboard |
| `]` | Flow | Cross-label flow matrix |
| `t` | Time Travel | Diff-against-ref view |
| `S` | Sprint | Sprint planning/detail view |

### Common keys

| Key | Action |
|---|---|
| `?` | Toggle help overlay |
| `Tab` | Toggle list/detail focus |
| `Esc` | Back, clear, or quit confirm |
| `j` / `k` | Move within the focused pane |
| `/` | Search in supported views |

## TUI Design Goals

The TUI is not trying to be a pretty wrapper around one list. Its job is to support different operator tasks without making the user mentally re-derive the graph from a table every time.

### Why there are many modes

Different planning questions need different visual structures:

- **Main** for backlog scanning and detail reading
- **Board** for lane-based operational overview
- **Insights** for metric interpretation
- **Graph** for structural debugging
- **History** for time and git context
- **Actionable** for "what can teams do in parallel right now?"
- **Attention / Labels / Flow** for label-centric health and bottleneck analysis
- **Tree / Time Travel / Sprint** for dependency, comparison, and planning workflows

### Why the TUI is still evolving

The project explicitly treats operator trust as something that has to be earned. The Rust TUI is already useful, but parity and workflow polish are still active goals rather than a finished story.

## Configuration

### Workspace config: `.bv/workspace.yaml`

```yaml
repos:
  - path: services/api
  - path: apps/web
discovery:
  patterns:
    - services/*
    - apps/*
```

### User config for TUI background reload: `~/.config/bv/config.yaml`

```yaml
experimental:
  background_mode: true
```

### Useful environment variables

| Variable | Purpose |
|---|---|
| `BV_OUTPUT_FORMAT` | Default robot output format: `json` or `toon` |
| `TOON_DEFAULT_FORMAT` | Fallback output format if `BV_OUTPUT_FORMAT` is unset |
| `TOON_STATS` | Print JSON vs TOON token estimates on stderr |
| `TOON_KEY_FOLDING` | Configure TOON key folding |
| `TOON_INDENT` | Configure TOON indentation |
| `BV_SEARCH_PRESET` | Default hybrid search preset |
| `BVR_PREVIEW_PORT` | Preferred preview server port |
| `BVR_PREVIEW_MAX_REQUESTS` | Auto-stop the preview server after N requests |
| `BV_BACKGROUND_MODE` | Enable or disable TUI background reload |
| `BVR_E2E_ARTIFACT_DIR` | Persist e2e regression artifacts for test runs |

## Architecture

```text
┌────────────────────────────────────────────────────────────────────┐
│ Input layer                                                       │
│  - .beads/*.jsonl                                                 │
│  - compatibility filenames                                        │
│  - .bv/workspace.yaml                                             │
│  - git history / baselines / feedback files                       │
└────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────┐
│ Loader + model validation                                         │
│  - path discovery                                                 │
│  - workspace aggregation                                          │
│  - issue parsing + validation                                     │
│  - warning suppression for robot mode                             │
└────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────┐
│ Analyzer                                                          │
│  - IssueGraph build                                               │
│  - fast metrics phase                                             │
│  - optional slow metrics phase in background thread               │
│  - triage / plan / alerts / search / history / drift / file intel │
└────────────────────────────────────────────────────────────────────┘
                                │
        ┌───────────────────────┼────────────────────────┐
        ▼                       ▼                        ▼
┌────────────────────┐  ┌────────────────────┐  ┌────────────────────┐
│ Robot surfaces     │  │ Operator surfaces  │  │ Export surfaces    │
│ JSON / TOON        │  │ FrankenTUI         │  │ MD / pages / SQLite│
└────────────────────┘  └────────────────────┘  └────────────────────┘
```

## Performance Model

Not all analysis work costs the same amount, and the code reflects that.

### Two-phase analysis

The engine distinguishes between:

- **fast structural work** that is cheap enough to do immediately
- **slower graph work** that can be deferred, backgrounded, or sampled on larger graphs

This lets the tool stay responsive without pretending expensive graph algorithms are free.

### Large-graph behavior

For larger graphs, some computations change strategy instead of doing the most expensive exact version every time. One example is sampled betweenness on larger graphs.

### Why background mode exists

Background mode exists to keep the TUI usable while deeper analysis catches up.

## Data and Artifact Layout

### Input-side layout

| Path / Artifact | Role |
|---|---|
| `.beads/` | Primary issue-data home |
| `beads.jsonl` / `issues.jsonl` / `beads.base.jsonl` | Supported JSONL issue sources |
| `.bv/workspace.yaml` | Multi-repo aggregation config |
| git history | History correlation and timeline input |

### Output-side layout

| Artifact | Produced By |
|---|---|
| robot JSON / TOON payloads | `--robot-*` |
| markdown reports and briefs | `--export-md`, `--priority-brief`, `--agent-brief` |
| graph exports | `--export-graph`, `--robot-graph` |
| static pages bundle | `--export-pages` |
| preview server responses | `--preview-pages` |
| baseline comparisons | `--save-baseline`, `--robot-drift`, `--check-drift` |

### Pages bundle layout

At a high level, exported bundles contain:

- a root HTML entry point
- local viewer assets
- `data/` payloads
- a SQLite database export
- deployment helper files

That layout is intentionally self-contained so preview and static hosting use the same artifact shape.

## Testing and Verification

This repo carries multiple verification layers rather than relying on a single happy-path suite:

- Conformance tests against a Go reference harness.
- JSON schema validation for robot output contracts.
- E2E robot, export, and workspace/history integration tests.
- TUI snapshots and keyflow journeys.
- Stress fixtures for large and pathological graphs.
- Benchmarks for the analysis pipeline.

Useful commands:

```bash
cargo test --test conformance
cargo test --test schema_validation
cargo test --test e2e_robot_matrix
cargo test --test e2e_workspace_history
cargo test --test export_pages
cargo bench --bench triage
```

Optional artifact capture for robot regressions:

```bash
BVR_E2E_ARTIFACT_DIR=target/bvr-e2e-artifacts cargo test --test e2e_robot_matrix
```

## Workflow Recipes

### I am an agent and need the next best move

```bash
bvr --robot-next
bvr --robot-overview
bvr --robot-triage
bvr --robot-plan
```

Start narrow, then expand. `--robot-next` gives the top pick, `--robot-overview` gives a compact project snapshot, `--robot-triage` gives deeper context, and `--robot-plan` tells you whether parallel work exists.

### I am a human operator triaging a sprint

```bash
bvr
# then use Main, Actionable, Sprint, and History modes
```

Use this when you need to move between ranking, dependency structure, and recent changes interactively.

### I need to understand dependency bottlenecks

```bash
bvr --robot-insights
bvr --robot-graph --graph-format dot
bvr --robot-blocker-chain bd-123
```

Use graph metrics for global context and blocker-chain output for a specific issue.

### I need to review what changed since the last checkpoint

```bash
bvr --save-baseline baseline.json
# later
bvr --robot-drift
bvr --check-drift
bvr --robot-diff --diff-since HEAD~10
```

Use baselines for structural drift and `--robot-diff` for issue-level change summaries.

### I need a shareable dashboard

```bash
bvr --export-pages ./bv-pages --pages-title "Review Dashboard"
bvr --preview-pages ./bv-pages
```

Use this when you want to share triage output with someone who is not going to run the CLI locally.

## Testing Philosophy

The test strategy exists because this tool has several different contracts at once.

### 1. Parity contract

Conformance tests protect behavior that is supposed to match legacy expectations.

### 2. Robot contract

Schema validation protects machine-facing output shape. This matters because agents and scripts are less forgiving than humans.

### 3. Operator contract

Snapshot and journey tests protect the TUI from accidental regressions in navigation and rendering.

### 4. Integration contract

Workspace/history/export tests exist because path semantics, workspace promotion, preview behavior, and pages flows are easy to break accidentally.

### 5. Stress contract

Stress fixtures protect the tool from becoming "correct only on toy graphs."

## Troubleshooting

### "My script hung"

You probably ran bare `bvr`, which launches the TUI.

```bash
# Use robot mode for automation
bvr --robot-triage
```

### "No issues were found"

Be explicit about the source instead of relying on discovery.

```bash
bvr --robot-triage --beads-file /path/to/issues.jsonl
bvr --workspace /path/to/.bv/workspace.yaml --robot-triage
```

### "Workspace auto-discovery is ambiguous"

Tell `bvr` exactly which workspace or beads file you want.

```bash
bvr --workspace .bv/workspace.yaml --robot-plan
# or
bvr --beads-file .beads/beads.jsonl --robot-plan
```

### "Preview pages failed to bind a port"

Pick an explicit port.

```bash
BVR_PREVIEW_PORT=9010 bvr --preview-pages ./bv-pages
```

### "Robot output is too verbose"

Use TOON.

```bash
bvr --robot-next --format toon
BV_OUTPUT_FORMAT=toon bvr --robot-triage
```

## Non-Goals

`bvr` is intentionally not trying to be all things:

- It is **not** a hosted project-management platform.
- It is **not** a replacement for issue creation or collaboration systems.
- It is **not** just a pretty terminal wrapper around `jq`.
- It is **not** optimized around community contribution workflows.

Those boundaries help keep the project focused on triage, analysis, automation, and export.

## Roadmap and Current Priorities

The current high-level priorities are:

### 1. Preserve verified legacy parity

Robot, CLI, export, workspace, and interactive TUI behavior now have a parity proof surface. The job is to keep that contract stable as the code evolves.

### 2. Harden regression evidence

Conformance, schema, snapshot, keyflow, journey, stress, and integration suites are part of the product contract, not optional maintenance.

### 3. Extend only where Rust-native additions stay coherent

Search, label intelligence, drift, correlation review, file-intel, and future async/runtime upgrades should build on top of the locked parity baseline rather than destabilize it.

### 4. Keep pages/export/workspace workflows polished

Export, preview, watch mode, wizard flows, and workspace path semantics remain high-value operator workflows and should stay first-class.

## Why This Exists

Issue data is easy to store. The harder problem is turning it into trustworthy action.

Flat issue lists fail in predictable ways:

- they hide structural blockers behind cosmetic metadata
- they over-trust stale priority fields
- they fragment multi-repo work into separate local truths
- they make humans and agents consume different, often contradictory, surfaces

`bvr` exists because the underlying problem is bigger than printing a list of issues. The real question is how to reason about dependency-shaped work across multiple consumers without duplicating logic everywhere.

For that reason, the project is built around a shared analyzer and projected into multiple surfaces, instead of building one-off ranking logic for each output mode.

## System Tour

One straightforward way to understand `bvr` is to follow one issue through the system.

### Step 1. The issue is discovered and loaded

The loader finds `.beads` data, compatibility JSONL sources, or workspace repo inputs, then parses and validates issues into the in-memory model.

### Step 2. The issue enters the graph

Its dependencies become edges in `IssueGraph`, which means the issue is no longer treated as an isolated row. It now has predecessors, successors, graph depth, blocker relationships, and possible cycle membership.

### Step 3. Metrics are computed around it

The analyzer computes centrality, blocker counts, critical depth, slack, cycle membership, and other structural signals. At this point, the issue stops being "just title + priority + status."

### Step 4. Triage synthesizes a recommendation

The triage layer combines graph signals with declared metadata such as priority, status, freshness, and risk signals. This is where the issue may become a top pick, a blocker to clear, a quick win, or a lower-confidence candidate.

### Step 5. The same truth is projected outward

That result can then appear as:

- a `--robot-next` top recommendation
- one row inside `--robot-triage`
- a node in the graph view
- a detail pane entry in the TUI
- a track item in Actionable mode
- a row in exported pages JSON / SQLite payloads

These are not separate systems with separate logic. They are projections of the same analyzed issue state.

## From Data to Recommendation

Here is the end-to-end pipeline in slightly more explicit form:

1. **Discover inputs**
   - locate `.beads` data, compatibility JSONL files, or `.bv/workspace.yaml`
2. **Parse and validate**
   - read JSONL
   - normalize issue fields
   - validate timestamps, statuses, and dependency references
3. **Build the graph**
   - construct `IssueGraph` with IDs, edges, successors, and predecessors
4. **Compute metrics**
   - run fast structural analysis first
   - run deeper graph metrics when appropriate
5. **Synthesize analysis products**
   - triage
   - planning
   - forecasting
   - alerts
   - search
   - label/file/history/drift/correlation outputs
6. **Project into surfaces**
   - robot JSON / TOON
   - TUI
   - markdown reports and briefs
   - graph export
   - pages bundle and preview flows

That pipeline is the real architecture of the project. The flags are just ways of selecting which projection you want.

## Metric Glossary

### PageRank

**Mathematically:** a recursive influence score over the dependency graph.

**Operationally:** "if I care about globally important work, how central is this issue?"

**Can mislead when:** a graph is tiny or nearly flat, where everything is structurally similar.

### Betweenness

**Mathematically:** how often a node lies on shortest paths between other nodes.

**Operationally:** "is this issue a bridge or chokepoint between clusters?"

**Can mislead when:** there are many equivalent alternate routes or the graph is too small to make bridging meaningful.

### Eigenvector Centrality

**Mathematically:** influence weighted by the influence of neighbors.

**Operationally:** "is this issue connected to other important issues?"

### HITS

**Mathematically:** separates hub-like and authority-like roles in a graph.

**Operationally:** useful for distinguishing issues that point to many important dependencies from issues that are important dependency targets themselves.

### K-Core

**Mathematically:** the deepest dense subgraph layer a node belongs to.

**Operationally:** "how embedded is this issue in the core of the dependency structure?"

### Articulation Point

**Mathematically:** a node whose removal disconnects part of the graph.

**Operationally:** "is this a single point of structural failure?"

### Critical Depth

**Mathematically:** a depth-like measure over dependency structure.

**Operationally:** "if this moves, how far does value propagate down the chain?"

### Slack

**Mathematically:** scheduling flexibility relative to critical structure.

**Operationally:** "how little room for delay does this issue have?"

### Cycle Membership

**Mathematically:** membership in a strongly connected component with a cycle.

**Operationally:** "is this issue trapped in a circular dependency?"

## Scoring Breakdown Example

Imagine an issue with this profile:

- blocks several open items
- sits near the root of a dependency chain
- has decent declared priority
- is not currently blocked itself
- is part of an important bridge between two graph regions

Its score might look conceptually like this:

| Component | Interpretation |
|---|---|
| **PageRank: high** | globally central work |
| **Betweenness: high** | connects otherwise separate work streams |
| **BlockerRatio: medium-high** | unblocks real downstream work |
| **PriorityBoost: medium** | operator intent supports it but is not the only reason |
| **TimeToImpact: high** | finishing it moves value quickly |
| **Urgency: medium** | status says it is live work, not deferred noise |
| **Risk discount: low penalty** | no blockers or cycles holding it back |

The final recommendation is strong not because one number said so, but because several independent signals agree.

## Design Tradeoffs

Several design choices shape the project:

### Explainable ranking over black-box ranking

It would be easier to make the ranking opaque than to make it transparent. This project chooses transparency and defensibility.

### Deterministic outputs over clever nondeterminism

For agent workflows, stable output shape and ordering are more valuable than vaguely smarter but unstable behavior.

### Shared analyzer over per-surface duplication

Robot mode, TUI, and export all use the same core analysis instead of each surface inventing its own rules.

### Single binary over service sprawl

`bvr` is intentionally a CLI/TUI/export tool, not a distributed backend with several moving processes.

### Cargo-first distribution over premature packaging sprawl

Right now, the project supports Cargo and source installation cleanly. It does not claim a broader packaging story than it actually has.

## Why TOON Exists

TOON exists because machine-facing output has two competing goals:

- strict machine readability
- compactness and lower token cost

JSON wins the first goal. TOON helps with the second.

### Prefer TOON when:

- an agent is repeatedly consuming large robot payloads
- you want a more compact representation for iterative loops
- strict JSON parsing is not required by the caller

### Prefer JSON when:

- another tool expects JSON directly
- you want maximal interoperability
- the payload will be fed into standard JSON tooling

TOON exists because agent ergonomics are part of the product surface.

## Failure Modes and Defensive Behavior

Many failure cases are treated as part of the real contract.

### Malformed JSONL lines

The loader does not assume pristine input forever. Robot mode also treats warning behavior differently because leaking noisy warnings into machine-facing stderr can break automation expectations.

### Empty datasets

An empty issue set is handled as a real scenario, not as a crash-worthy anomaly.

### Missing or ambiguous workspace configs

When discovery becomes ambiguous, the tool prefers explicit guidance over silently loading the wrong project shape.

### Changed path / filename conventions across history

Historical and workspace-aware loading are real concerns in this repo because path semantics affect correctness, not convenience alone.

### Large graphs

The engine does not pretend every metric is equally cheap. It uses staged computation and alternate strategies where appropriate.

### Preview server conflicts

The preview workflow explicitly handles port conflicts and exposes overrides, because export-preview loops are meant to be operational, not toy demos.

## Historical and Temporal Analysis

`bvr` is not only about ranking the current graph snapshot.

The temporal surfaces include:

- `--robot-history`
- `--robot-diff`
- saved baselines
- `--robot-drift`
- human-readable `--check-drift`
- correlation explanation / confirmation / rejection

Together, those features answer a broader class of questions:

- what changed?
- what drifted?
- what newly matters?
- what used to be true but is no longer true?
- which commit history seems to explain this issue movement?

Together, those features make `bvr` more of a backlog-analysis engine than a static ranking script.

## Why Static Pages Matter

Static pages solve a product problem that robot mode and the TUI do not.

Robot mode is ideal for automation. The TUI is ideal for operators. Static pages matter because sometimes you need:

- a shareable artifact
- a dashboard for someone who will not run the CLI
- a reproducible snapshot of analysis
- something previewable locally and publishable to a static host

That is also why export includes the real viewer assets and SQLite/data payloads instead of dumping one JSON file and calling it done.

## Operator Personas

### Agent / automation consumer

Wants deterministic machine-readable outputs, schema truthfulness, and low-friction command surfaces.

### Solo engineer

Wants a fast answer to "what should I do next?" and "what is actually blocked?"

### Tech lead / sprint planner

Wants prioritization, parallelization, bottleneck detection, label flow, and planning visibility across a broader surface area.

### Stakeholder consuming pages export

Wants a shareable view of the analyzed state without learning the CLI or gaining repo access.

## Real Query and Workflow Examples

### Find high-signal next work

```bash
bvr --robot-next
bvr --robot-triage
```

### Inspect structural bottlenecks

```bash
bvr --robot-insights
bvr --robot-graph --graph-format mermaid
```

### Audit one label area

```bash
bvr --robot-triage --label backend
bvr --robot-label-health
bvr --robot-label-flow
```

### Search with graph-aware ranking

```bash
bvr --robot-search --search "auth"
bvr --robot-search --search "auth" --search-mode hybrid
bvr --robot-search --search "auth" --search-preset impact-first
```

### Review temporal change

```bash
bvr --robot-diff --diff-since HEAD~5
bvr --robot-history --history-limit 25
bvr --robot-drift
```

### Produce a stakeholder-facing snapshot

```bash
bvr --export-pages ./bv-pages --pages-title "Weekly Review"
bvr --preview-pages ./bv-pages
```

## Module-by-Module Architecture Map

| Module / File | Responsibility |
|---|---|
| `src/loader.rs` | issue discovery, workspace loading, path semantics, JSONL parsing |
| `src/model.rs` | core issue data model and validation rules |
| `src/analysis/graph.rs` | graph construction and centrality / structural metrics |
| `src/analysis/triage.rs` | ranking, impact score, recommendations, project health |
| `src/analysis/plan.rs` | parallel execution-track grouping |
| `src/analysis/history.rs` / `git_history.rs` | history and git correlation support |
| `src/analysis/label_intel.rs` | label health, flow, and attention analysis |
| `src/analysis/file_intel.rs` | file-bead mapping, hotspots, related work, orphans |
| `src/analysis/search.rs` | text and hybrid search logic, presets, weights |
| `src/analysis/drift.rs` | baseline comparison and drift reporting |
| `src/robot.rs` | envelopes, docs, schemas, TOON rendering, payload contracts |
| `src/tui.rs` | interactive multi-mode terminal UI |
| `src/export_pages.rs` | static bundle export, preview, and watch flows |
| `src/pages_wizard.rs` | interactive pages deployment-oriented wizard |
| `src/export_md.rs` / `src/export_sqlite.rs` | report and export artifact generation |
| `src/main.rs` | CLI dispatch and surface orchestration |

## What Makes This Hard

Several parts of this project are harder than they first look:

### Behavioral parity is not a cosmetic problem

If robot output, warning behavior, path semantics, or workspace resolution drift, users and agents can make wrong decisions even when the binary "works."

### Path semantics are a product feature

This repo has repeatedly surfaced subtle bugs around workspace roots, repo paths, historical loads, preview behavior, and related state resolution. Those are correctness problems, not housekeeping.

### Graph metrics must become action, not ornament

It is easy to compute centrality and still fail to tell the user what to do. Turning metrics into usable recommendations is the harder job.

### Multiple surfaces must agree

Robot mode, TUI, markdown export, graph export, and pages export all need to present coherent projections of the same analysis.

### Responsiveness and depth are in tension

The richer the graph analysis gets, the easier it is to make the operator experience laggy. The staged computation model exists because this tension is real.

## Determinism and Trust

The project leans hard on determinism because trust is the whole game in a triage tool.

### Deterministic ordering matters

Agents, tests, and humans all benefit when repeated runs over the same input produce stable ordering and payload shape.

### Data hashing matters

The shared envelope includes a data hash because "what exact source state generated this?" is a legitimate operational question.

### Docs and schemas matter

`--robot-docs` and `--robot-schema` are not decorative. They help make the machine-facing surface inspectable and regression-resistant.

### Test layers matter

Conformance, schema tests, e2e coverage, and snapshots are all part of building trust that the tool means what it says.

## Future Research and Expansion Ideas

These are not promises; they are plausible directions that fit the design of the system:

- richer search ranking and explainability
- deeper causal / impact-path reasoning
- stronger recommendation feedback loops
- broader workspace-scale planning and aggregation views
- more operator workflows in the TUI
- more analysis surfaces that stay coherent with the shared analyzer model

## Limitations

### What `bvr` does not do perfectly yet

- **Legacy TUI parity is still in progress.** The TUI is functional and much broader than a toy interface, but the project is still actively refining operator workflows relative to legacy `bv`.
- **Distribution is still Cargo-centric.** The repo does not currently ship a dedicated curl installer, Homebrew formula, or packaged release manager workflow in this README.
- **Some surfaces are newer than the older README era.** Search, correlation feedback, drift, file intel, and some label flows are current capabilities, but they are evolving quickly.
- **This is a CLI/TUI/dashboard tool, not a hosted service.** You bring the `.beads` data, git history, and deployment target.

## FAQ

### Is `bvr` just a Rust rewrite of `bv`?

It started there, but the current repo now includes Rust-native surfaces such as richer pages export workflows, drift and baseline tooling, correlation review commands, file intelligence, and a broader TUI.

### Should agents use the TUI?

Usually not. Agents and scripts should prefer `--robot-*` with `json` or `toon`; the TUI is for humans.

### What should I run first?

Start with:

```bash
bvr --robot-next
bvr --robot-triage
bvr --robot-plan
```

### Can it work across multiple repos?

Yes. Use `.bv/workspace.yaml`, then either let `bvr` discover it or pass `--workspace` explicitly.

### Does it only read `.beads/beads.jsonl`?

No. It also supports compatibility filenames such as `issues.jsonl` and `beads.base.jsonl`, along with workspace aggregation.

### Can I share results without giving people the repo?

Yes. Export a static bundle:

```bash
bvr --export-pages ./bv-pages
bvr --preview-pages ./bv-pages
```

### Is TOON optional?

Yes. JSON remains the default. TOON is there for more compact agent-facing output.

## Development Notes

### Build metadata

`build.rs` embeds build timestamp, target triple, and rustc metadata via `vergen-gix`.

### CI

GitHub Actions currently runs:

- format + clippy
- unit + snapshot verification
- conformance + schema validation
- e2e and integration suites
- benchmark smoke
- release build artifact creation

## About Contributions

> *About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

## License

MIT License with the OpenAI/Anthropic rider. See [LICENSE](LICENSE).
