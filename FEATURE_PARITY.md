# Feature Parity Matrix (`bv` -> `bvr`)

Legend:
- `complete`: behavior implemented and tested.
- `partial`: subset implemented.
- `planned`: not implemented yet.

Important reading note:
- Robot/CLI parity claims below are generally backed by fixture and integration evidence.
- Interactive TUI entries are intentionally stricter: current Rust screens are functional and heavily tested for stability, but the major operator-facing modes remain under an active FrankenTUI-first redesign program (`bd-2bpl.3.6.*`).
- Snapshot/keyflow stability is not, by itself, enough to claim product-quality TUI parity.

## Robot / CLI
| Legacy Capability | Status | Notes |
|---|---|---|
| `--robot-help` | complete | Implemented in Rust CLI. |
| `--robot-next` | complete | Top recommendation output supported. |
| `--robot-triage` | complete | Quick ref + recommendations + blockers + quick wins. |
| `--robot-plan` | complete | Track grouping and summary implemented. |
| `--robot-insights` | complete | Core graph metrics + cycle + bottleneck output. |
| `--robot-priority` | complete | Ranked recommendation surface implemented. |
| `--robot-diff` | complete | Snapshot diff emits legacy-style nested metadata (`from/to` timestamps, revision, removed issues, cycle deltas, metric deltas, health summary) and legacy-shaped issue payloads (compact fields, dependency metadata, comments, zero-time defaults) with full fixture-backed conformance assertions; Go reference does not include `output_format`/`version` for diff output. |
| `--robot-history` | complete | Git-aware commit correlation, commit index, milestones, method stats, `--history-since`, and `--min-confidence` filtering; robot-history export shape omits bead-only fields to align with legacy output contracts; milestones and cycle_time use `skip_serializing_if` to omit null fields matching Go behavior; commits serialize as `null` when empty (matching Go); word-boundary-aware event type inference; fixture-backed conformance + e2e + specialized tests for all flags. |
| `--robot-forecast` | complete | ETA forecast with `--forecast-label`, `--forecast-sprint`, and `--forecast-agents` with legacy-compatible all-vs-single filtering semantics, legacy-style ETA payload fields (`eta_date_low/high`, `velocity_minutes_per_day`), `output_format` and `version` envelope metadata; fixture-backed conformance assertions for forecast count, issue IDs, and all filter combinations. |
| `--robot-capacity` | complete | `--agents` + `--capacity-label`, critical path/actionable/bottleneck metrics, ETA-minute projection via legacy-inspired `EstimateETAForIssue` complexity/velocity model, `output_format` and `version` envelope metadata; fixture-backed capacity parity checks (including label-scoped output), exact label-scope edge semantics, and forecast/capacity total-minute consistency checks. |
| `--robot-burndown` | complete | Sprint selection (`current` or ID), burndown totals, `daily_points`, `ideal_line`, git-derived `scope_changes`, `output_format` and `version` envelope metadata; Go-reference fixture generated and fixture-backed conformance assertions for core scalar fields, burn rates, and array lengths. |
| `--robot-suggest` | complete | Suggestion suite with `--suggest-type`, `--suggest-confidence`, and `--suggest-bead` filters; detector caps/sorting uses alphabetical type string ordering (matching Go behavior); dependency-direction heuristics aligned with legacy; fixture-backed conformance + filter + hash stability tests. |
| `--robot-graph` | complete | JSON/DOT/Mermaid export with `--graph-root`/`--graph-depth`/`--label` filters and deterministic output implemented. |

## Interactive TUI
| Legacy Capability | Status | Notes |
|---|---|---|
| Bare command launches TUI | complete | `bvr` launches frankentui app. |
| Main list/detail split | partial | Functional split-pane triage surface exists with navigation and focus switching, but the current main screen is still more report-like than cockpit-grade. Active redesign: `bd-2bpl.3.6.6`, `bd-2bpl.3.6.6.1`, `bd-2bpl.3.6.6.2`, `bd-2bpl.3.6.6.3`. |
| Board view (`b`) | partial | Board mode exists with grouping, navigation, search, and detail behavior, but the current presentation and interaction model are not yet at credible kanban-workspace parity. Active redesign: `bd-2bpl.3.6.7`, `bd-2bpl.3.6.7.1`, `bd-2bpl.3.6.7.2`. |
| Insights view (`i`) | partial | Insights mode exposes real analytics and explanation toggles, but current information density and visual hierarchy still read more like rotating text panels than a high-confidence analytics console. Active redesign: `bd-2bpl.3.6.9`, `bd-2bpl.3.6.9.1`, `bd-2bpl.3.6.9.2`. |
| Graph view (`g`) | partial | Graph mode has meaningful metrics and traversal, but it still behaves more like a metric report plus ego box than a true topology-exploration surface. Active redesign: `bd-2bpl.3.6.8`, `bd-2bpl.3.6.8.1`, `bd-2bpl.3.6.8.2`. |
| History view (`h`) | partial | History mode is one of the stronger current screens and includes timeline/file-tree/search/jump workflows, but it still falls short of the intended investigation-workspace quality bar. Active redesign: `bd-2bpl.3.6.10`, `bd-2bpl.3.6.10.1`, `bd-2bpl.3.6.10.2`. |
| Full keybinding parity | partial | The current Rust TUI implements a very large keybinding set with extensive tests, but key availability alone is not sufficient evidence of workflow-quality parity while the major mode redesign beads remain open. |

## TUI Fidelity Contract

### Layout Structure
- **Frame**: Header (1 line) | Body (remaining) | Footer (1 line).
- **Body split**: 42% list pane | 58% detail pane.
- **Focus**: `FocusPane::List` or `FocusPane::Detail`; `Tab` toggles. Active pane title shows `[focus]`.
- **Width breakpoints**: Narrow/Medium/Wide breakpoint enum with adaptive split ratios; titles/descriptions truncated via `truncate_str()` to available width.

### View Modes
| Mode | List Pane | Detail Pane |
|---|---|---|
| Main | Issue rows: `> {id} {status} p{priority} {title}` | ID, title, status, priority, type, assignee, labels, PageRank, critical depth, dependencies, description |
| Board | Lane headers: `> {lane} [{count}] {bar}` + card preview (6/lane) | ASCII box card with lane, assignee, blocker/dependent lists, J/K dep nav |
| Graph | Nodes sorted by critical depth+PR: `> {icon} {id} in:{n} out:{n} pr:{f}` | Ego-node ASCII art, BLOCKED BY / BLOCKS sections, GRAPH METRICS (3 subsections), Top PageRank |
| Insights | 10 cycling panels (Bottlenecks..Cycles): `s/S` cycles | Metrics summary, focus issue, expandable explanations (`e`), calc proof (`x`) |
| History | Bead mode: `> {id} events:{n} {status}` / Git mode: `> {sha} {beads} {msg} {ts}` | LIFECYCLE: timeline with connectors, COMMIT DETAILS, RELATED BEADS |
| Actionable (`a`) | Track-based execution plan items | Track details with claim/resolve recommendations (Rust-only) |
| Attention (`!`) | Ranked label attention scores | Label score breakdowns (Rust-only) |
| Tree (`T`) | Dependency tree with collapse/expand | Tree node details (Rust-only) |
| LabelDashboard (`[`) | Label health metrics | Label detail panel (Rust-only) |
| FlowMatrix (`]`) | Cross-label dependency flow | Flow detail panel (Rust-only) |
| TimeTravelDiff (`t`) | Diff-against-ref changes | Diff detail panel (Rust-only) |
| Sprint (`S`) | Sprint-based issue grouping | Sprint issue details (Rust-only) |

### Keybinding Matrix (pass/fail checkable)

#### Global (all modes)
| Key | Action | Implemented |
|---|---|---|
| `?` | Toggle help overlay | yes |
| `Tab` | Flip list/detail focus | yes |
| `Esc` | Back-out (mode→main→clear-filter→quit-confirm) | yes |
| `q` | Return to main (or quit from main) | yes |
| `Ctrl+C` | Quit immediately | yes |
| `j`/`Down` | Selection down | yes |
| `k`/`Up` | Selection up | yes |
| `Ctrl+d` | Page down (10) | yes |
| `Ctrl+u` | Page up (10) | yes |
| `PageDown`/`PageUp` | Page scroll | yes |
| `Home` | Top | yes |
| `End`/`G` | Bottom | yes |
| `o`/`c`/`r`/`a` | Filter: open/closed/ready/all | yes |

#### Main
| Key | Action | Implemented |
|---|---|---|
| `s` | Sort cycle (default→created-asc→created-desc→priority→updated) | yes |
| `b`/`i`/`g` | Toggle Board/Insights/Graph (second press returns) | yes |
| `h` | Enter History (saves previous mode) | yes |

#### Board
| Key | Action | Implemented |
|---|---|---|
| `h`/`l` | Lane left/right | yes |
| `H`/`L` | First/last lane | yes |
| `1`-`4` | Jump to lane N | yes |
| `0`/`$`/`Home`/`End` | First/last in lane | yes |
| `j`/`k` | Within-lane vertical nav | yes |
| `Ctrl+d`/`Ctrl+u` | Lane page scroll | yes |
| `/` | Search (n/N cycling, Esc cancel, Enter keep) | yes |
| `s` | Grouping cycle (status→priority→type) | yes |
| `e` | Toggle empty lanes | yes |
| `J`/`K` | Detail dep navigation | yes |

#### Graph
| Key | Action | Implemented |
|---|---|---|
| `h`/`l` | List nav (or detail→list) | yes |
| `H`/`L` | Jump left/right 10 | yes |
| `Ctrl+d`/`Ctrl+u` | Page scroll | yes |
| `/` | Search with n/N cycling | yes |
| `J`/`K` | Detail dep navigation | yes |

#### Insights
| Key | Action | Implemented |
|---|---|---|
| `h`/`l` | Pane focus switching | yes |
| `s`/`S` | Panel forward/backward cycle (10 panels) | yes |
| `e` | Toggle explanations | yes |
| `x` | Toggle calculation proof | yes |
| `/` | Search with n/N cycling | yes |
| `J`/`K` | Detail dep navigation | yes |

#### History
| Key | Action | Implemented |
|---|---|---|
| `v` | Toggle bead/git view | yes |
| `c` | Confidence filter cycle (bead mode) | yes |
| `/` | Search (bead list + git timeline) | yes |
| `g` | Jump to Graph for selected issue | yes |
| `J`/`K` | Bead commit nav / Git related-bead nav | yes |
| `Enter` | Git mode: jump to related issue | yes |

### Visual Tokens
| Token | Context | Spec |
|---|---|---|
| Status icons | `o`/`*`/`!`/`x`/`~`/`r`/`^`/`?` | All views |
| Type icons | `B`/`F`/`T`/`E`/`Q`/`D`/`-` | All views |
| Mini-bar | 6-char `█▒` bar | Graph metrics |
| Rank badge | `#N` suffix | Graph metrics |
| Box drawing | `┌┐└┘├┤─│` | Board detail card |
| Lane bar | `█` fill (20 max) | Board lanes |
| Lifecycle connectors | `│`/`└` tree | History detail |
| Commit type icons | `F`/`B`/`D`/`R`/`T`/`C`/`P`/`I`/`K`/`S`/`M`/`<`/`*` | History detail |
| Author initials | `[XX]` badge | History detail |
| Section headers | `GRAPH METRICS`, `LIFECYCLE:`, `COMMIT DETAILS`, `RELATED BEADS:` | Detail panes |

### Modal Flows
1. **Help**: `?` → full-pane scrollable overlay → `?`/`Esc` closes.
2. **Quit confirm**: main `Esc` (no filter) → "Quit bvr?" modal → `Esc`/`Y` quits, other cancels.
3. **Search input**: `/` → append chars, `Backspace` deletes, `n`/`N` cycles, `Esc` cancels, `Enter` keeps.
4. **Tutorial**: `ModalOverlay::Tutorial` → full-pane getting-started overlay → any key dismisses.
5. **Confirm dialog**: `ModalOverlay::Confirm` → title+message overlay → `Y` accepts, `N`/`Esc` rejects.
6. **Pages wizard**: `ModalOverlay::PagesWizard` → 4-step flow (export dir → title → options → review) → `Enter` advances, `Esc` cancels, `Backspace` goes back, `c`/`h` toggle options.

### Active TUI Product Gaps
- The current TUI is still architecturally closer to a string-heavy report renderer than to the intended FrankenTUI-driven operator console.
- Main, board, graph, insights, and history all need mode-specific redesign work before product-quality parity should be claimed.
- Current snapshot/keyflow coverage proves stability of the present implementation; it does not by itself prove the redesigned product bar.
- Mouse/hit-region behavior, realistic scenario datasets, and richer E2E artifact logging are now tracked explicitly under `bd-2bpl.3.6.12.*` because they were previously too easy to under-prove.
- Workspace auto-discovery defaults (Go-style implicit `.bv/workspace.yaml` discovery from workspace root, nested subdirs, and `--repo-path`) are implemented and covered by resolver + e2e tests.

## Integrations
| Capability | Status | Notes |
|---|---|---|
| FrankentUI runtime integration | complete | Active dependency and runtime app usage. |
| Asupersync integration points | deferred | Optional Cargo dependency declared but unused in source. Background async (two-phase metric computation, file reload) uses `std::thread::spawn` + `mpsc::channel` directly. Asupersync orchestration is a post-parity enhancement, not a Go parity requirement (Go has no equivalent). |
| Hooks/workspace/history parity | complete | Export hooks, explicit and implicit workspace loading/aggregation (including auto-discovered `.bv/workspace.yaml` from workspace root, nested subdirs, and `--repo-path`), repo filters, robot history, TUI history (all 5 search modes), and full pages export/preview/watch/wizard are implemented. |

## Verification
| Capability | Status | Notes |
|---|---|---|
| Conformance harness scaffold | complete | Go reference harness + fixture + Rust test skeleton in repo. |
| Fixture-driven parity tests | complete | Legacy fixture-backed conformance checks for diff/history/forecast/triage/plan/priority/burndown with adversarial coverage; edge-case fixtures; 89-issue `stress_complex_89.jsonl` stress fixture; **large-dataset stress fixtures**: `stress_large_500.jsonl` (500 issues, 7 topologies), `pathological_deps.jsonl` (233 issues, extreme dep patterns: deep chain, convergence/divergence, overlapping cycles, self-dep, bidirectional, long cycle, dangling), `malformed_metadata.jsonl` (24 issues, edge-case metadata); 49 stress tests in `tests/stress_fixtures.rs`; fixture catalog at `tests/testdata/FIXTURES.md`. |
| Bench harness | complete | Criterion benchmark for triage path added. |

TUI verification reality:
- Current Rust TUI snapshots, keyflow tests, and journeys are valuable stability checks for the current implementation.
- They should not be read as proof that major TUI modes have already reached legacy-quality operator parity.
- The active redesign proof contract lives under `bd-2bpl.3.6.12.*` and requires screen-family proof, hit-region proof, realistic scenario datasets, structured artifact bundles, and replayable shell-level journeys before final TUI parity claims should return to `complete`.

### How to Rerun the Proof Set

```bash
# Full test suite (1,318 tests; 2 flaky e2e_watch may fail under load)
cargo test --tests

# Individual suites
cargo test --lib                           # 936 unit tests
cargo test --test conformance              # 75 conformance tests
cargo test --test schema_validation        # 50 schema tests
cargo test --test e2e_robot_matrix         # 49 e2e robot matrix
cargo test --test e2e_workspace_history    # 27 e2e workspace/history
cargo test --test e2e_export_pages         # 20 e2e export/pages
cargo test --test stress_fixtures          # 49 stress tests
cargo test --test cli_model_validation     # 39 CLI model tests
cargo test --test export_pages             # 15 integration export tests

# Snapshot verification
cargo test --lib snap_                     # 55 insta snapshots
cargo test --lib keyflow_                  # 25 keyflow journeys

# E2E TUI journeys (multi-mode flows with artifact capture)
cargo test --lib e2e_journey_              # 5 journey tests

# Benchmarks (not included in test count)
cargo bench --bench triage                 # 12 benchmark groups

# Shell e2e (preview/watch/wizard scenarios)
scripts/e2e_preview_pages.sh               # 5 scenarios

# Quality gates
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# E2E artifacts (optional, for debugging failures)
BVR_E2E_ARTIFACT_DIR=target/bvr-e2e-artifacts cargo test --test e2e_robot_matrix
```

Artifact locations:
- Conformance fixtures: `tests/conformance/fixtures/go_outputs/`
- Stress fixtures: `tests/testdata/` (see `tests/testdata/FIXTURES.md`)
- Snapshot baselines: `src/snapshots/`
- E2E journey artifacts: `src/snapshots/bvr__tui__tests__e2e_journey_*.snap` (multi-step screen captures)
- E2E artifacts: `target/bvr-e2e-artifacts/` (when `BVR_E2E_ARTIFACT_DIR` is set)
- Shell e2e logs: temp dir printed on failure (preserved by default)

## CLI Flag Parity Ledger (129 legacy flags)

Legend: `complete` / `partial` / `missing` / `excluded` (intentionally out-of-scope) / `bvr-only` (Rust addition).

### Robot Commands (Core) — 6/6 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-help` | `--robot-help` | complete |
| `--robot-next` | `--robot-next` | complete |
| `--robot-triage` | `--robot-triage` | complete |
| `--robot-plan` | `--robot-plan` | complete |
| `--robot-insights` | `--robot-insights` | complete |
| `--robot-priority` | `--robot-priority` | complete |

### Robot Commands (Analysis/Suggest) — 6 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--robot-diff` | `--robot-diff` | complete | Full fixture-backed conformance. |
| `--diff-since` | `--diff-since` | complete | |
| `--robot-suggest` | `--robot-suggest` | complete | Fixture-backed conformance + filter + hash stability tests. |
| `--suggest-type` | `--suggest-type` | complete | |
| `--suggest-confidence` | `--suggest-confidence` | complete | |
| `--suggest-bead` | `--suggest-bead` | complete | |

### Robot Commands (Alerts) — 4 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-alerts` | `--robot-alerts` | complete |
| `--alert-type` | `--alert-type` | complete |
| `--alert-label` | `--alert-label` | complete |
| `--severity` | `--severity` | complete |

### Robot Commands (Forecast/Capacity/Burndown) — 8 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-forecast` | `--robot-forecast` | complete |
| `--forecast-label` | `--forecast-label` | complete |
| `--forecast-sprint` | `--forecast-sprint` | complete |
| `--forecast-agents` | `--forecast-agents` | complete |
| `--robot-capacity` | `--robot-capacity` | complete |
| `--agents` | `--agents` | complete |
| `--capacity-label` | `--capacity-label` | complete |
| `--robot-burndown` | `--robot-burndown` | complete |

### Robot Commands (History) — 5 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-history` | `--robot-history` | complete |
| `--bead-history` | `--bead-history` | complete |
| `--history-limit` | `--history-limit` | complete |
| `--history-since` | `--history-since` | complete |
| `--min-confidence` | `--min-confidence` | complete |

### Robot Commands (Graph) — 3 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-graph` | `--robot-graph` | complete |
| `--graph-format` | `--graph-format` | complete |
| `--graph-root` | `--graph-root` | complete |
| `--graph-depth` | `--graph-depth` | complete |

### Export/Graph Snapshot Flags — 6 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--export-md` | `--export-md` | complete | Markdown report export. |
| `--no-hooks` | `--no-hooks` | complete | Skip export hook execution. |
| `--export-graph` | `--export-graph` | complete | Deterministic graph snapshot output for `.json/.dot/.mmd/.svg/.png` with extension-based format inference. |
| `--graph-title` | `--graph-title` | complete | Optional title metadata for exported graph snapshots (text + static). |
| `--graph-preset` | `--graph-preset` | complete | Layout density preset (`compact`/`roomy`) for text and static snapshots. |
| `--graph-style` | `--graph-style` | complete | Static snapshot layout style (`force`/`grid`). |

### Workspace/Repo Scoping — 3 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--workspace` | `--workspace` | complete |
| `--repo` | `--repo` | complete |
| `-r` | `-r` | complete |

### Robot Commands (Sprint/Metrics) — 3 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--robot-sprint-list` | `--robot-sprint-list` | complete | Sprint listing with envelope metadata. |
| `--robot-sprint-show` | `--robot-sprint-show` | complete | Sprint detail by ID. |
| `--robot-metrics` | `--robot-metrics` | complete | Timing, cache, and memory metrics. |

### Robot Options (General) — 5 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-max-results` | `--robot-max-results` | complete |
| `--robot-min-confidence` | `--robot-min-confidence` | complete |
| `--robot-by-label` | `--robot-by-label` | complete |
| `--robot-by-assignee` | `--robot-by-assignee` | complete |
| `--label` | `--label` | complete |
| `--robot-triage-by-label` | `--robot-triage-by-label` | complete |
| `--robot-triage-by-track` | `--robot-triage-by-track` | complete |

### Robot Commands (Metadata/Docs) — 3 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--robot-docs` | `--robot-docs` | complete | Topic-based documentation output. |
| `--robot-schema` | `--robot-schema` | complete | JSON Schema for all robot commands. |
| `--schema-command` | `--schema-command` | complete | Schema for specific command. |

### Format/Meta — 4 complete
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--format` | `--format` | complete |
| `--stats` | `--stats` | complete |
| `--help` | `--help` | complete |
| `--version` | `--version` | complete |

### Advanced Analysis Flags — 2 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--as-of` | `--as-of` | complete | Loads issues from historical git revision. |
| `--force-full-analysis` | `--force-full-analysis` | complete | Bypasses incremental analysis caches. |

### Rust-Only Additions
| bvr Flag | Notes |
|---|---|
| `--beads-file` | Hidden; override `.beads/beads.jsonl` path. |
| `--repo-path` | Hidden; override repository root auto-detection. |

### Missing — Not Yet Ported
| Category | Flags |
|---|---|
| ~~Correlation/Impact~~ | ~~`--robot-causality`, `--robot-confirm-correlation`, `--robot-reject-correlation`, `--robot-explain-correlation`, `--robot-correlation-stats`, `--correlation-by`, `--correlation-reason`, `--robot-impact`, `--robot-impact-network`, `--robot-related`, `--related-include-closed`, `--related-max-results`, `--related-min-relevance`, `--relations-limit`, `--relations-threshold`, `--network-depth`~~ — **Implemented** |
| ~~File Analysis~~ | ~~`--robot-file-beads`, `--robot-file-hotspots`, `--robot-file-relations`, `--file-beads-limit`, `--hotspots-limit`~~ — **Implemented** |
| ~~Label/Attention~~ | ~~`--robot-label-attention`, `--robot-label-flow`, `--robot-label-health`, `--attention-limit`~~ — **Implemented** |
| Sprint | (moved to implemented) |
| ~~Search~~ | ~~`--search`, `--search-limit`, `--search-mode`, `--search-preset`, `--search-weights`, `--robot-search`~~ — **Implemented** |
| ~~Export/Pages~~ | ~~`--pages`, `--pages-include-closed`, `--pages-include-history`, `--pages-title`, `--preview-pages`, `--export-pages`, `--watch-export`, `--no-live-reload`~~ — **Implemented** | complete | Full pages export (HTML+SQLite+JSON bundle), preview server with live reload and status endpoint, watch-export with debounce, interactive 9-step pages wizard with config persistence/transcript/automation boundaries, TUI PagesWizard modal, browser-open support; 75 wizard tests + 20 e2e_export_pages tests + shell e2e script. |
| ~~Script~~ | ~~`--emit-script`, `--script-format`, `--script-limit`~~ — **Implemented** |
| ~~Baseline/Drift~~ | ~~`--save-baseline`, `--baseline-info`, `--check-drift`, `--robot-drift`~~ — **Implemented** |
| ~~Feedback~~ | ~~`--feedback-show`, `--feedback-accept`, `--feedback-ignore`, `--feedback-reset`~~ — **Implemented** |
| ~~Workflow~~ | ~~`--robot-blocker-chain`, `--robot-orphans`, `--orphans-min-score`, `--priority-brief`, `--agent-brief`~~ — **Implemented** |
| ~~Metadata/Docs~~ | ~~`--robot-recipes`~~ — **Implemented** |
| ~~Workspace/Config~~ | ~~`--recipe`~~ — **Implemented** |

### Harness Coverage Notes
| Command | Fixture Status | Notes |
|---|---|---|
| `--robot-help` | text-only | Legacy outputs plain text, not JSON. No fixture-based conformance possible. |
| `--robot-docs` | Rust-only | Implemented in Rust (bd-33w.2.1); legacy has its own implementation but output shape differs. |
| `--robot-schema` | Rust-only | Implemented in Rust (bd-33w.2.1); schema shapes are implementation-specific. |
| `--robot-metrics` | captured | Legacy outputs Go runtime stats (goroutines, GC); Rust outputs /proc/self RSS. Structure differs. |
| `--robot-sprint-list` | captured | Only in bvr_extended.json (requires sprints file). |
| `--robot-sprint-show` | captured | Only in bvr_extended.json (requires sprints file). |
| `--robot-graph --graph-format=dot` | captured (text) | DOT output stored as string, not JSON. |
| `--robot-graph --graph-format=mermaid` | captured (text) | Mermaid output stored as string, not JSON. |

### Excluded — Intentionally Out-of-Scope (1 flag)
| Flag | Reason |
|---|---|
| `--cpu-profile` | Go pprof equivalent; `--profile-startup` covers startup timing, external tools (perf, samply) available for CPU profiling. |

Previously listed as excluded but now implemented:
- `--update`, `--check-update`, `--rollback`, `--yes` — stub responses with remediation guidance.
- `--profile-json`, `--profile-startup` — phase timing with JSON output option.
- `--debug-render`, `--debug-height`, `--debug-width` — non-interactive TUI rendering for CI/diagnostics.
- `--background-mode`, `--no-background-mode` — CLI/env/config precedence plus TUI background reload loop.

### Parity Summary
| Category | Complete | Partial | Missing | Excluded | Total |
|---|---|---|---|---|---|
| Robot Commands | 40 | 0 | 0 | 0 | 40 |
| Robot Options | 7 | 0 | 0 | 0 | 7 |
| Format/Meta | 4 | 0 | 0 | 0 | 4 |
| Advanced Analysis | 2 | 0 | 0 | 0 | 2 |
| Export/Pages | 8 | 0 | 0 | 0 | 8 |
| Previously Missing Surfaces | 51 | 0 | 0 | 0 | 51 |
| Previously Excluded (now impl) | 11 | 0 | 0 | 0 | 11 |
| Excluded | 0 | 0 | 0 | 1 | 1 |
| Rust-Only | 2 | 0 | 0 | 0 | 2 |
| **Totals** | **125** | **0** | **0** | **1** | **126+2** |

## Phased Implementation Plan

Historical note:
- The wave table below records what landed in Rust, not the current product-quality verdict for the interactive TUI.
- For robot/CLI and export surfaces, these gates are still a useful completion history.
- For TUI-facing gates, implementation history must now be read alongside the active redesign subtree (`bd-2bpl.3.6.*`) and the stricter verification contract above.

### Wave 0: Spec Contract (COMPLETE)
| Gate | Bead | Verification | Status |
|---|---|---|---|
| CLI flag parity ledger | bd-33w.1.1 | `parity_ledger_documents_all_implemented_bvr_flags` test | done |
| TUI fidelity contract | bd-33w.1.2 | Keybinding matrix in this doc | done |
| Phased gates frozen | bd-33w.1.3 | This section exists with pass/fail criteria | done |

### Wave 1: Robot Contract Utilities + Go Reference Harness
Prerequisites: Wave 0 complete.
| Gate | Bead | Verification |
|---|---|---|
| `--robot-docs`, `--robot-schema`, `--schema-command`, format stats | bd-33w.2.1 | `cargo test robot_docs` + `cargo test robot_schema` pass | done |
| Go reference harness covers full robot command matrix | bd-33w.6.1 | Fixture files exist for every robot command in `tests/conformance/fixtures/go_outputs/` |
| Fixture matrix covers parity + adversarial scenarios | bd-33w.6.2 | Each robot mode has >= 1 positive + 1 adversarial fixture |
| Strict comparator and schema-validation test utilities | bd-33w.6.3 | Conformance tests use typed schema validation, not ad-hoc field checks |

### Wave 2: Core Feature Parity (Robot + TUI + Integration)
Prerequisites: Wave 1 complete.
| Gate | Bead | Verification |
|---|---|---|
| Workspace aggregation + `--repo` filtering | bd-33w.4.1 | `cargo test workspace` passes with multi-repo fixture |
| Export-md + hook lifecycle (`--no-hooks`) | bd-33w.4.2 | `cargo test export_md` + hook integration test |
| Graph export parity (json/dot/mermaid + static) | bd-33w.4.3 | `cargo test graph_export` with all 3 formats |
| TUI visual token baseline + breakpoint layout | bd-33w.3.1 | Width-aware layout test at 100/140/180 cols |
| Main/board/insights/graph interaction gap closure | bd-33w.3.2 | Keybinding matrix rows all `yes` |
| History view implementation milestone (responsive, file tree, search, o/y) | bd-33w.3.3 | Implemented features exist in Rust, but product-quality history parity is now tracked as `partial` pending `bd-2bpl.3.6.10.*` |
| ~~Recipe/script workflow (`--robot-recipes`, `--emit-script`, feedback)~~ | bd-33w.2.2 | **Implemented** — `cargo test recipe` passes (9 tests), CLI flags wired |
| Label intelligence + drift baseline | bd-33w.2.3 | `cargo test label_health` + `cargo test drift` pass |
| Semantic search + ranking | bd-33w.2.4 | `cargo test search` passes |
| Sprint/planning adjunct surfaces | bd-33w.2.5 | `cargo test sprint_list` + `cargo test sprint_show` pass | done |

### Wave 3: Intelligence + Scale + Polish
Prerequisites: Wave 2 complete.
| Gate | Bead | Verification |
|---|---|---|
| Correlation audit loop commands + persistence | bd-33w.5.1 | `cargo test correlation` passes |
| Orphan/file-index intelligence | bd-33w.5.2 | `cargo test orphans` + `cargo test file_beads` pass |
| Impact exploration (impact, file-relations, related) | bd-33w.5.3 | `cargo test impact` passes |
| Causal network analytics (blocker-chain, causality) | bd-33w.5.4 | `cargo test causality` passes |
| ~~Performance budgets + stress harness~~ | bd-33w.7.1 | **Implemented** — `benches/triage.rs` with 12 benchmark groups: analyzer_new (sparse/dense × 100/500/1000), triage, insights, plan, diff, forecast, suggest, alerts, history, cycle_detection, real_fixture, stress_fixture; synthetic generators for sparse/dense/cyclic graphs at scale; all benchmarks sub-15ms at 1000 issues |
| Background-mode async orchestration | bd-33w.7.2 | `cargo test background_mode` passes |
| ~~Profiling parity (cpu-profile, startup profile)~~ | bd-33w.7.3 | **Implemented** — `--profile-startup` (human-readable) and `--profile-startup --profile-json` (machine-readable) with phase timing (load/build/triage/insights), density/cycle/bottleneck stats, and auto-generated recommendations; 2 e2e tests |
| ~~Module-level unit tests + edge/error coverage~~ | bd-33w.6.4 | **Implemented** — added tests for model.rs (23), error.rs (9), robot.rs (+7), triage.rs (+6), diff.rs (+6), graph.rs (+5); total lib tests: 356; all modules have `#[cfg(test)]` blocks |
| ~~E2E scripts with diagnostics~~ | bd-33w.6.5 | **Implemented** — `tests/e2e_robot_matrix.rs` with 42 tests: full robot command matrix (triage/next/plan/insights/priority/graph/diff/suggest/alerts/history/forecast/capacity/burndown/metrics/help/docs/schema), debug-render (all views + width variations), export (md/priority-brief/agent-brief), error handling, cross-fixture consistency |
| ~~CI parity gates wired~~ | bd-33w.6.6 | **Implemented** — `.github/workflows/ci.yml` with 5 jobs: check (fmt+clippy), unit (356 lib tests + snapshots), conformance (74 conformance + 31 schema), e2e (45 robot matrix + alerts + burndown + history + exports + admin + background), bench (criterion smoke), release build with artifact upload |

### Wave 4: Release Readiness
Prerequisites: Wave 3 complete.
| Gate | Bead | Verification |
|---|---|---|
| ~~Static pages pipeline (export/preview/watch/wizard)~~ | bd-33w.4.4 | **Implemented** — Full pages export (HTML+SQLite+JSON), preview server with live reload, watch-export with debounce, 9-step interactive wizard with config persistence; 75 wizard + 20 e2e + 5 shell e2e tests |
| ~~Brief-generation surfaces~~ | bd-33w.4.5 | **Implemented** — `--priority-brief PATH` and `--agent-brief DIR` + `analysis::brief` module (5 tests) |
| Modal/wizard implementation milestone | bd-33w.3.4 | Implemented overlays/modals exist in Rust; TUI product parity still depends on the redesign contract where those flows appear inside a stronger shell |
| TUI regression harness (snapshots + keyflow) | bd-33w.3.5 | Implemented and useful for stability, but not sufficient evidence of full TUI parity without the redesign proof contract in `bd-2bpl.3.6.12.*` |
| Debug-render parity flags | bd-33w.3.6 | `cargo test debug_render` passes |
| ~~Operational/admin CLI (update/rollback + agents blurb)~~ | bd-33w.2.6 | **Implemented** — `--agents-check/add/update/remove/dry-run/force` + `--check-update/update/rollback/yes` stubs |
| ~~Docs hardened, roadmap self-contained~~ | bd-33w.8.1 | **Implemented** — README.md updated with full command surface (50+ commands), TUI key map table (11 view modes), test suite table (1,248 tests across 7 suites), CI workflow docs, benchmark docs; PROPOSED_ARCHITECTURE.md updated with all 19 analysis modules, conformance/bench design, TUI fidelity status |
| ~~Release-readiness checklist + evidence index~~ | bd-33w.8.2 | **Implemented** — Release readiness checklist below with evidence links for all parity surfaces |

### Completion Criteria
- All Wave 0-3 gates pass → bvr is functionally equivalent to bv for core workflows.
- Wave 4 gates pass → bvr is release-ready with full documentation and CI coverage.
- 12 excluded flags are documented with rationale and not counted as gaps.

---

## Release Readiness Checklist

### Core Parity Surfaces

| Surface | Evidence | Status | Risk |
|---|---|---|---|
| **Robot triage/next/plan** | `cargo test --test e2e_robot_matrix e2e_robot_triage` + `e2e_robot_next` + `e2e_robot_plan` | PASS | None |
| **Robot insights** | `cargo test --test e2e_robot_matrix e2e_robot_insights` + conformance tests | PASS | None |
| **Robot priority** | `cargo test --test e2e_robot_matrix e2e_robot_priority` | PASS | None |
| **Robot graph (json/dot/mermaid)** | `cargo test --test e2e_robot_matrix e2e_robot_graph_json` + `_dot` + `_mermaid` | PASS | None |
| **Robot diff** | `cargo test --test e2e_robot_matrix e2e_robot_diff` + conformance | PASS | None |
| **Robot suggest** | `cargo test --test e2e_robot_matrix e2e_robot_suggest` | PASS | None |
| **Robot alerts** | `cargo test --test e2e_robot_matrix e2e_robot_alerts` + `cargo test --test robot_alerts` | PASS | None |
| **Robot history** | `cargo test --test e2e_robot_matrix e2e_robot_history` + `cargo test --test robot_history_since` | PASS | None |
| **Robot forecast** | `cargo test --test e2e_robot_matrix e2e_robot_forecast` | PASS | None |
| **Robot capacity** | `cargo test --test e2e_robot_matrix e2e_robot_capacity` | PASS | None |
| **Robot burndown** | `cargo test --test robot_burndown_scope` + conformance | PASS | None |
| **Robot metrics** | `cargo test --test e2e_robot_matrix e2e_robot_metrics` | PASS | None |
| **Robot docs/schema/help** | `cargo test --test e2e_robot_matrix e2e_robot_docs` + `_schema` + `_help` | PASS | None |
| **Robot sprint-list/show** | conformance tests | PASS | None |
| **Robot label-health/flow/attention** | conformance tests | PASS | None |
| **Robot search** | `cargo test search` | PASS | None |
| **Robot recipes** | `cargo test recipe` | PASS | None |
| **Profiling** | `cargo test --test e2e_robot_matrix e2e_profile_startup` | PASS | None |

### Export Surfaces

| Surface | Evidence | Status | Risk |
|---|---|---|---|
| **Export-md** | `cargo test --test e2e_robot_matrix e2e_export_md` + `cargo test --test export_md` | PASS | None |
| **Priority-brief** | `cargo test --test e2e_robot_matrix e2e_priority_brief` | PASS | None |
| **Agent-brief** | `cargo test --test e2e_robot_matrix e2e_agent_brief` | PASS | None |
| **Export-pages** | `cargo test --test export_pages` + `cargo test --test e2e_export_pages` | PASS | None |
| **Pages wizard** | `cargo test wizard` (75 tests) + `scripts/e2e_preview_pages.sh` (5 scenarios) | PASS | None |
| **Preview server** | `cargo test preview` + `scripts/e2e_preview_pages.sh` (status endpoint check) | PASS | None |

### TUI Surfaces

| Surface | Evidence | Status | Risk |
|---|---|---|---|
| **11 current view modes render and snapshot stably** | `cargo test snap_` (55 snapshots) | PASS | Stability evidence only; not a product-parity verdict |
| **Keyboard interaction stays regression-tested** | `cargo test keyflow_` (25 journey tests) | PASS | Interaction stability only; major modes still under redesign |
| **E2E TUI journeys run on the current implementation** | `cargo test e2e_journey_` (5 multi-mode flows with artifacts) | PASS | Current-implementation evidence only; redesign proof contract is stricter |
| **Debug render** | `cargo test --test e2e_robot_matrix e2e_debug_render` | PASS | None |
| **Modal/wizard flows** | `cargo test modal` (8 tests) | PASS | Modal infrastructure exists; broader shell/TUI product parity still open |
| **History view current implementation stays tested** | `cargo test history` (56 tests) | PASS | Strongest current TUI slice, but still partial pending redesign |

### Quality Gates

| Gate | Evidence | Status |
|---|---|---|
| **Format clean** | `cargo fmt --check` (CI enforced) | PASS |
| **Clippy warnings** | `cargo clippy --all-targets` (CI enforced) | PASS (0 warnings) |
| **936 unit tests** | `cargo test --lib` | PASS |
| **75 conformance tests** | `cargo test --test conformance` | PASS |
| **50 schema validation** | `cargo test --test schema_validation` | PASS |
| **82 e2e tests** | 3 e2e test files (robot_matrix, workspace_history, export_pages) | PASS |
| **148 integration tests** | 11 integration test files (stress, cli_model, export, admin, etc.) | PASS |
| **55 snapshot baselines** | `cargo test snap_` with insta | PASS |
| **25 keyflow journeys** | `cargo test keyflow_` | PASS |
| **5 e2e TUI journeys** | `cargo test e2e_journey_` with multi-step artifact capture | PASS |
| **12 benchmark groups** | `cargo bench --bench triage` | PASS (all sub-15ms at 1000 issues) |

### Known Risks

| Risk | Impact | Mitigation |
|---|---|---|
| No `--cpu-profile` (pprof equivalent) | Low | `--profile-startup` covers startup timing; external tools (perf, samply) available for CPU profiling |

### Go/No-Go Decision

All core robot commands, export surfaces, pages workflows, and quality gates are passing with 1,318 tests (936 lib + 382 integration/conformance/e2e). The project is in strong shape for automated/robot workflows and related export surfaces.

The interactive TUI should not currently be described as a finished product-quality replacement for the legacy Go `bv` TUI. It is functional and test-stable, but the major operator-facing modes remain under the active FrankenTUI-first redesign and proof program (`bd-2bpl.3.6.*`).

## Open Gaps to 100%
1. Active TUI redesign across main, board, graph, insights, and history so the product reaches legacy-quality operator confidence before parity returns to `complete`.
2. Active redesign proof contract: screen-family proof, hit-region proof, realistic scenario datasets, structured artifact bundles, and replayable shell-level journeys under `bd-2bpl.3.6.12.*`.
3. Remaining repo-facing parity language should continue to be audited against the redesign contract so docs do not drift back into overclaiming.
4. Robot/CLI, pages, and workspace surfaces are substantially implemented and remain governed by their existing regression suites.
