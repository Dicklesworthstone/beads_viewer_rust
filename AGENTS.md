# AGENTS.md — bvr (beads_viewer_rust)

> Guidelines for AI coding agents working in this Rust codebase.

---

## RULE 0 - THE FUNDAMENTAL OVERRIDE PREROGATIVE

If I tell you to do something, even if it goes against what follows below, YOU MUST LISTEN TO ME. I AM IN CHARGE, NOT YOU.

---

## RULE NUMBER 1: NO FILE DELETION

**YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.** Even a new file that you yourself created, such as a test code file. You have a horrible track record of deleting critically important files or otherwise throwing away tons of expensive work. As a result, you have permanently lost any and all rights to determine that a file or folder should be deleted.

**YOU MUST ALWAYS ASK AND RECEIVE CLEAR, WRITTEN PERMISSION BEFORE EVER DELETING A FILE OR FOLDER OF ANY KIND.**

---

## Irreversible Git & Filesystem Actions — DO NOT EVER BREAK GLASS

1. **Absolutely forbidden commands:** `git reset --hard`, `git clean -fd`, `rm -rf`, or any command that can delete or overwrite code/data must never be run unless the user explicitly provides the exact command and states, in the same message, that they understand and want the irreversible consequences.
2. **No guessing:** If there is any uncertainty about what a command might delete or overwrite, stop immediately and ask the user for specific approval. "I think it's safe" is never acceptable.
3. **Safer alternatives first:** When cleanup or rollbacks are needed, request permission to use non-destructive options (`git status`, `git diff`, `git stash`, copying to backups) before ever considering a destructive command.
4. **Mandatory explicit plan:** Even after explicit user authorization, restate the command verbatim, list exactly what will be affected, and wait for a confirmation that your understanding is correct. Only then may you execute it—if anything remains ambiguous, refuse and escalate.
5. **Document the confirmation:** When running any approved destructive command, record (in the session notes / final response) the exact user text that authorized it, the command actually run, and the execution time. If that record is absent, the operation did not happen.

---

## Git Branch: ONLY Use `main`

**The default branch is `main`.** All work happens on `main` — commits, PRs, feature branches all merge to `main`.

---

## Toolchain: Rust & Cargo

We only use **Cargo** in this project, NEVER any other package manager.

- **Edition:** Rust 2024 (`rust-version = "1.85"`)
- **Dependency versions:** Explicit versions for stability
- **Configuration:** Cargo.toml only (single crate, not a workspace)
- **Unsafe code:** Forbidden (`unsafe_code = "forbid"` in `[lints.rust]`)
- **Clippy:** Pedantic + nursery enabled as warnings (`[lints.clippy]`)

### Key Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing with derive macros |
| `serde` + `serde_json` | JSON serialization for robot output and data loading |
| `serde_yaml` | YAML config parsing |
| `chrono` | Timestamps with serde support |
| `ftui` | TUI runtime (frankentui) with crossterm backend |
| `petgraph` | Directed graph for dependency analysis |
| `rusqlite` | SQLite export (bundled-full) |
| `itertools` | Iterator combinators |
| `sha2` | Content hashing for data fingerprints |
| `signal-hook` | Unix signal handling for preview server |
| `tempfile` | Temp file management for exports |
| `thiserror` | Typed error definitions |
| `toon` (package: `tru`) | TOON output format encoding |
| `tracing` + `tracing-subscriber` | Structured logging with env-filter |
| `anyhow` | Error propagation |
| `png` | PNG generation for graph export |
| `vergen-gix` | Build metadata embedding (build.rs) |

### Dev Dependencies

| Crate | Purpose |
|-------|---------|
| `assert_cmd` | CLI integration testing |
| `criterion` | Benchmarks |
| `insta` | Snapshot testing |
| `predicates` | Test assertions |
| `pretty_assertions` | Readable test diffs |

### Release Profile

```toml
[profile.release]
opt-level = "z"     # Optimize for size
lto = true          # Link-time optimization
codegen-units = 1   # Single codegen unit
panic = "abort"     # No unwinding overhead
strip = true        # Remove debug symbols
```

### Feature Flags

```toml
[features]
default = []
asupersync-runtime = ["dep:asupersync"]  # Optional async orchestration (post-parity)
```

---

## Code Editing Discipline

### No Script-Based Changes

**NEVER** run a script that processes/changes code files in this repo. Brittle regex-based transformations create far more problems than they solve.

- **Always make code changes manually**, even when there are many instances
- For many simple changes: use parallel subagents
- For subtle/complex changes: do them methodically yourself

### No File Proliferation

If you want to change something or add a feature, **revise existing code files in place**.

**NEVER** create variations like:
- `mainV2.rs`
- `main_improved.rs`
- `main_enhanced.rs`

New files are reserved for **genuinely new functionality** that makes zero sense to include in any existing file. The bar for creating new files is **incredibly high**.

---

## Backwards Compatibility

We do not care about backwards compatibility—we're in early development with no users. We want to do things the **RIGHT** way with **NO TECH DEBT**.

- Never create "compatibility shims"
- Never create wrapper functions for deprecated APIs
- Just fix the code directly

---

## Compiler Checks (CRITICAL)

**After any substantive code changes, you MUST verify no errors were introduced:**

```bash
# All builds run via rch (remote compilation helper)
export TMPDIR=/data/tmp && rch exec -- cargo check --all-targets
export TMPDIR=/data/tmp && rch exec -- cargo clippy --all-targets
export TMPDIR=/data/tmp && rch exec -- cargo fmt --check
```

If you see errors, **carefully understand and resolve each issue**. Read sufficient context to fix them the RIGHT way.

---

## Testing

### Testing Policy

Every module includes inline `#[cfg(test)]` unit tests alongside the implementation. Tests must cover:
- Happy path
- Edge cases (empty input, max values, boundary conditions)
- Error conditions

### Test Suite (1,318 tests)

```bash
# Full suite via rch
export TMPDIR=/data/tmp && rch exec -- cargo test --tests

# Individual suites
export TMPDIR=/data/tmp && rch exec -- cargo test --lib                        # 936 unit tests
export TMPDIR=/data/tmp && rch exec -- cargo test --test conformance           # 75 conformance
export TMPDIR=/data/tmp && rch exec -- cargo test --test schema_validation     # 36 schema
export TMPDIR=/data/tmp && rch exec -- cargo test --test e2e_robot_matrix      # 35 e2e robot
export TMPDIR=/data/tmp && rch exec -- cargo test --test e2e_workspace_history # 27 e2e workspace
export TMPDIR=/data/tmp && rch exec -- cargo test --test e2e_export_pages      # 20 e2e pages
export TMPDIR=/data/tmp && rch exec -- cargo test --test stress_fixtures       # 49 stress

# Benchmarks
export TMPDIR=/data/tmp && rch exec -- cargo bench --bench triage              # 12 groups
```

### Test Categories

| Suite | Count | Purpose |
|-------|-------|---------|
| Unit tests (`--lib`) | 936 | Module-level with inline `#[cfg(test)]` |
| Conformance (`--test conformance`) | 75 | Go reference fixture parity |
| Schema validation | 36 | JSON schema compliance |
| E2E robot matrix | 35 | Full robot command integration |
| E2E workspace/history | 27 | Workspace and history flows |
| E2E export/pages | 20 | Pages export/preview/watch |
| Stress fixtures | 49 | Large/pathological dataset handling |
| Integration tests | 148 | CLI model, export, admin, background |
| Snapshots | 21 | insta TUI visual regression |
| Keyflow journeys | 11 | TUI interaction sequences |

### Shell E2E

```bash
scripts/e2e_preview_pages.sh  # 5 scenarios: export+preview, watch, wizard, error paths
```

---

## Third-Party Library Usage

If you aren't 100% sure how to use a third-party library, **SEARCH ONLINE** to find the latest documentation and current best practices.

---

## bvr (beads_viewer_rust) — This Project

**This is the project you're working on.** bvr is a spec-first Rust port of the Go `bv` graph-aware triage engine for issue tracking. It provides robot-mode JSON output for automated agents, an interactive TUI for humans, and static pages export for web dashboards.

### What It Does

Loads issues from `.beads/beads.jsonl`, builds a dependency graph, and computes PageRank, betweenness centrality, HITS, eigenvector, k-core decomposition, critical path, and cycle detection. Outputs triage recommendations, forecasts, alerts, suggestions, history correlations, and more via `--robot-*` flags. Also provides an 11-mode interactive TUI and a static pages export pipeline.

### Architecture

```
JSONL → Loader → Issue Vec → Analyzer (IssueGraph + metrics) → Robot JSON / TUI / Pages Export
```

**Two-phase analysis:**
- **Phase 1 (instant):** degree, topo sort, density
- **Phase 2 (async, 500ms timeout):** PageRank, betweenness, HITS, eigenvector, cycles

### Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` (198 KB) | CLI dispatch, robot mode routing, all `--robot-*` flag handling |
| `src/tui.rs` (548 KB) | Interactive TUI with 11 view modes, keybindings, modals |
| `src/lib.rs` | Library re-exports (analysis, model, loader, tui, robot, etc.) |
| `src/model.rs` | `Issue` struct, status/priority/type enums, serialization |
| `src/loader.rs` | JSONL/workspace loading, git history extraction |
| `src/cli.rs` | `CliArgs` struct (clap derive) with all CLI flags |
| `src/robot.rs` | `RobotEnvelope`, all robot output structs, TOON encoding |
| `src/error.rs` | Typed error definitions |
| `src/agents.rs` | Agent management CLI (`--agents-check/add/update/remove`) |
| `src/export_md.rs` | Markdown report export |
| `src/export_pages.rs` | Static HTML/SQLite pages bundle, preview server, watch mode |
| `src/export_sqlite.rs` | SQLite database export |
| `src/pages_wizard.rs` | 9-step interactive pages deployment wizard |
| `src/viewer_assets.rs` | Embedded viewer HTML/JS/CSS assets |

### Analysis Modules (`src/analysis/`)

| File | Purpose |
|------|---------|
| `mod.rs` | `Analyzer` struct, orchestrates all analysis |
| `graph.rs` | `IssueGraph` — PageRank, betweenness, HITS, eigenvector, k-core, cycles |
| `triage.rs` | Scoring, recommendations, quick wins, blockers |
| `plan.rs` | Parallel execution track grouping |
| `suggest.rs` | Hygiene suggestions (duplicates, missing deps, labels) |
| `forecast.rs` | ETA predictions with dependency-aware scheduling |
| `alerts.rs` | Stale issues, blocking cascades, priority mismatches |
| `diff.rs` | Snapshot diff (new/closed/modified, cycle deltas) |
| `history.rs` | Git-aware commit correlation and milestones |
| `git_history.rs` | Low-level git log parsing |
| `label_intel.rs` | Label health, flow, attention scoring |
| `file_intel.rs` | File-bead correlation, hotspot detection |
| `search.rs` | Semantic search and ranking |
| `recipe.rs` | Pre-filter recipes (actionable, high-impact, etc.) |
| `correlation.rs` | Issue correlation and impact analysis |
| `causal.rs` | Causal network analytics, blocker chains |
| `drift.rs` | Baseline drift detection |
| `whatif.rs` | What-if scenario analysis |
| `advanced.rs` | Advanced insights (TopKSet, CoverageSet, KPaths, etc.) |
| `brief.rs` | Priority and agent brief generation |
| `cache.rs` | Analysis result caching |

### TUI View Modes (11 total)

| Mode | Key | Description |
|------|-----|-------------|
| Main | default | Issue list with detail pane |
| Board | `b` | Kanban-style lane view |
| Insights | `i` | 10 cycling metric panels |
| Graph | `g` | Dependency graph with centrality metrics |
| History | `h` | Bead/git timeline with file tree |
| Actionable | `a` | Track-based execution plan (Rust-only) |
| Attention | `!` | Label attention scores (Rust-only) |
| Tree | `T` | Dependency tree with collapse/expand (Rust-only) |
| LabelDashboard | `[` | Label health dashboard (Rust-only) |
| FlowMatrix | `]` | Cross-label flow matrix (Rust-only) |
| TimeTravelDiff | `t` | Diff-against-ref view (Rust-only) |
| Sprint | `S` | Sprint-based planning (Rust-only) |

### Robot Output

All robot commands output JSON to stdout with a shared envelope:
- `generated_at` — ISO 8601 timestamp
- `data_hash` — SHA256 fingerprint of source data
- `output_format` — `"json"` or `"toon"`
- `version` — `"v{CARGO_PKG_VERSION}"`

**CRITICAL: Never run bare `bvr` — it launches the interactive TUI and blocks your session. Always use `--robot-*` flags.**

### Key Patterns for Contributors

- **TUI state fields** must be added to `BvrApp` struct AND all construction sites (15+ places in tests)
- **New ViewMode variants** need: enum+label, BvrApp fields, key handler, guard fn, j/k nav, list/detail text, 5 match dispatches, all construction sites
- **Robot output structs** use `#[serde(flatten)]` with `RobotEnvelope` for shared fields
- **IssueGraph** uses `Vec<Issue>` + `HashMap<String, usize>` (not `HashMap<String, Issue>`) for performance
- **Rust 2024 edition**: `std::env::set_var`/`remove_var` are unsafe — cannot use directly in tests
- **Cycle detection**: reports ALL SCC members (sorted), not minimal DFS path within SCC
- **Auto-diff**: `--diff-since` in non-TTY context auto-enables `--robot-diff`
- **TOON output**: `render_payload()` handles both JSON and TOON, patches `output_format` dynamically

---

## RCH — Remote Compilation Helper

RCH offloads `cargo build`, `cargo test`, `cargo clippy`, and other compilation commands to remote workers instead of building locally.

**All cargo commands in this project MUST use rch:**
```bash
export TMPDIR=/data/tmp && rch exec -- cargo test
export TMPDIR=/data/tmp && rch exec -- cargo build --release
export TMPDIR=/data/tmp && rch exec -- cargo clippy --all-targets
```

**IMPORTANT:** The `export TMPDIR=/data/tmp` is required because rch workers have a btrfs space constraint on `/tmp`. Without it, tests fail with ENOSPC. The `export` must be a separate statement — `TMPDIR=/data/tmp rch exec` does NOT propagate the env var.

Quick commands:
```bash
rch doctor                    # Health check
rch workers probe --all       # Test connectivity
rch status                    # Overview
rch queue                     # Active/waiting builds
```

If rch is unavailable, it fails open — builds run locally.

---

## ast-grep vs ripgrep

**Use `ast-grep` when structure matters.** It parses code and matches AST nodes, ignoring comments/strings, and can **safely rewrite** code.

**Use `ripgrep` when text is enough.** Fastest way to grep literals/regex.

### Rule of Thumb

- Need correctness or **applying changes** -> `ast-grep`
- Need raw speed or **hunting text** -> `rg`
- Often combine: `rg` to shortlist files, then `ast-grep` to match/modify

---

## Morph Warp Grep — AI-Powered Code Search

**Use `mcp__morph-mcp__warp_grep` for exploratory "how does X work?" questions.** An AI agent expands your query, greps the codebase, reads relevant files, and returns precise line ranges with full context.

**Use `ripgrep` for targeted searches.** When you know exactly what you're looking for.

---

## MCP Agent Mail — Multi-Agent Coordination

A mail-like layer that lets coding agents coordinate asynchronously via MCP tools. Provides identities, inbox/outbox, searchable threads, and advisory file reservations.

### Same Repository Workflow

1. **Register identity:**
   ```
   ensure_project(project_key=<abs-path>)
   register_agent(project_key, program, model)
   ```

2. **Reserve files before editing:**
   ```
   file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true)
   ```

3. **Communicate with threads:**
   ```
   send_message(..., thread_id="bd-123", subject="[bd-123] Start: <title>", ack_required=true)
   ```

4. **Quick reads:**
   ```
   resource://inbox/{Agent}?project=<abs-path>&limit=20
   resource://thread/{id}?project=<abs-path>&include_bodies=true
   ```

### Macros vs Granular Tools

- **Prefer macros for speed:** `macro_start_session`, `macro_prepare_thread`, `macro_file_reservation_cycle`, `macro_contact_handshake`
- **Use granular tools for control:** `register_agent`, `file_reservation_paths`, `send_message`, `fetch_inbox`, `acknowledge_message`

### Common Pitfalls

- `"from_agent not registered"`: Always `register_agent` in the correct `project_key` first
- `"FILE_RESERVATION_CONFLICT"`: Adjust patterns, wait for expiry, or use non-exclusive reservation

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

**Important:** `br` is non-invasive—it NEVER executes git commands. After `br sync --flush-only`, you must manually run `git add .beads/ && git commit`.

### Essential Commands

```bash
# CLI commands for agents
br ready              # Show issues ready to work (no blockers)
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br create --title="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason "Completed"
br close <id1> <id2>  # Close multiple issues at once
br sync --flush-only  # Export to JSONL (NO git operations)
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id> --reason "..."`
5. **Sync**: Run `br sync --flush-only` then manually commit

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, question, docs
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

---

## bv — Graph-Aware Triage Engine

bv is bvr running in robot mode. It computes PageRank, betweenness, critical path, cycles, HITS, eigenvector, and k-core metrics deterministically.

**CRITICAL: Use ONLY `--robot-*` flags. Bare `bv` launches an interactive TUI that blocks your session.**

### The Workflow: Start With Triage

```bash
bv --robot-triage        # THE MEGA-COMMAND: start here
bv --robot-next          # Minimal: just the single top pick + claim command
```

### Command Reference

**Planning:**
| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with `unblocks` lists |
| `--robot-priority` | Priority misalignment detection with confidence |

**Graph Analysis:**
| Command | Returns |
|---------|---------|
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core |
| `--robot-label-health` | Per-label health: `health_level`, `velocity_score`, `staleness`, `blocked_count` |
| `--robot-label-flow` | Cross-label dependency: `flow_matrix`, `dependencies`, `bottleneck_labels` |
| `--robot-label-attention` | Attention-ranked labels |

**History & Change Tracking:**
| Command | Returns |
|---------|---------|
| `--robot-history` | Bead-to-commit correlations |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues, cycles |

**Other:**
| Command | Returns |
|---------|---------|
| `--robot-burndown <sprint>` | Sprint burndown, scope changes, at-risk items |
| `--robot-forecast <id\|all>` | ETA predictions with dependency-aware scheduling |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |

### Scoping & Filtering

```bash
bv --robot-plan --label backend              # Scope to label's subgraph
bv --robot-insights --as-of HEAD~30          # Historical point-in-time
bv --recipe actionable --robot-plan          # Pre-filter: ready to work
bv --robot-triage --robot-triage-by-track    # Group by parallel work streams
bv --robot-triage --robot-triage-by-label    # Group by domain
```

### jq Quick Reference

```bash
bv --robot-triage | jq '.triage.quick_ref'                  # At-a-glance summary
bv --robot-triage | jq '.triage.recommendations[0]'         # Top recommendation
bv --robot-plan | jq '.plan.summary.highest_impact'          # Best unblock target
bv --robot-insights | jq '.Cycles'                           # Circular deps (must fix!)
```

---

## CI/CD Pipeline

GitHub Actions workflow at `.github/workflows/ci.yml`:

| Job | Purpose |
|-----|---------|
| `check` | `cargo fmt --check` + `cargo clippy --all-targets` |
| `unit` | Lib tests + snapshot verification |
| `conformance` | Conformance + schema validation |
| `e2e` | Robot command matrix + all integration tests |
| `bench` | Criterion smoke run |
| `build` | Release binary with artifact upload |

---

## Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads to JSONL
git add .beads/         # Stage beads changes
git commit -m "..."     # Commit everything together
git push                # Push to remote
```

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Sync beads** - `br sync --flush-only` to export to JSONL
5. **Hand off** - Provide context for next session
