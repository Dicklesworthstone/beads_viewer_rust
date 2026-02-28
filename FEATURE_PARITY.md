# Feature Parity Matrix (`bv` -> `bvr`)

Legend:
- `complete`: behavior implemented and tested.
- `partial`: subset implemented.
- `planned`: not implemented yet.

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
| `--robot-history` | partial | Git-aware commit correlation, commit index, milestones, method stats, `--history-since`, and `--min-confidence` filtering are implemented; robot-history export shape now omits bead-only fields to align with legacy output contracts; milestones and cycle_time now use `skip_serializing_if` to omit null fields matching Go behavior; commits serialize as `null` when empty (matching Go); word-boundary-aware event type inference prevents false positives. |
| `--robot-forecast` | partial | ETA forecast now supports `--forecast-label`, `--forecast-sprint`, and `--forecast-agents` with legacy-compatible all-vs-single filtering semantics, plus legacy-style ETA payload fields (`eta_date_low/high`, `velocity_minutes_per_day`), `output_format` and `version` envelope metadata, and order/factor/value conformance assertions against fixture data. |
| `--robot-capacity` | partial | Implemented `--agents` + `--capacity-label`, critical path/actionable/bottleneck metrics, ETA-minute projection via legacy-inspired `EstimateETAForIssue` complexity/velocity model, `output_format` and `version` envelope metadata; added fixture-backed capacity parity checks (including label-scoped output), exact label-scope edge semantics, and forecast/capacity total-minute consistency checks. |
| `--robot-burndown` | partial | Implemented sprint selection (`current` or ID), burndown totals, `daily_points`, `ideal_line`, git-derived `scope_changes`, `output_format` and `version` envelope metadata; Go-reference fixture generated and fixture-backed conformance assertions for core scalar fields, burn rates, and array lengths added. |
| `--robot-suggest` | partial | Suggestion suite implemented with `--suggest-type`, `--suggest-confidence`, and `--suggest-bead` filters; detector caps/sorting now uses alphabetical type string ordering (matching Go behavior) and dependency-direction heuristics are aligned with legacy. |
| `--robot-graph` | complete | JSON/DOT/Mermaid export with `--graph-root`/`--graph-depth`/`--label` filters and deterministic output implemented. |

## Interactive TUI
| Legacy Capability | Status | Notes |
|---|---|---|
| Bare command launches TUI | complete | `bvr` launches frankentui app. |
| Main list/detail split | partial | Base split and navigation in place. |
| Board view (`b`) | partial | Replaced placeholder with lane-aware board pane (lane counts, queue sample, selected issue blockers/dependents); full visual/keybinding parity with legacy board workflow still pending. |
| Insights view (`i`) | partial | Replaced placeholder with bottleneck/critical-path/cycle hotspot pane; full visual/keybinding parity with legacy insights workflow still pending. |
| Graph view (`g`) | partial | Data-rich graph pane with centrality, blockers/dependents, cycle membership, top PageRank list; Go-parity 3-section metrics panel (Importance/Flow & Connectivity/Connections) with 8 metrics, mini-bars, rank badges, in-degree/out-degree; graph-mode `h` from detail returns to list focus; keybinding hints in list header. |
| History view (`h`) | partial | Lifecycle timeline pane with box-drawing tree connectors and lifecycle icons; milestones section shows created/claimed/closed/reopened with author; commit detail with type icons, author initials badges, file change breakdown with action icons and +/- stats; git-mode detail with COMMIT DETAILS/RELATED BEADS sections; keybinding hints in detail footer. |
| Full keybinding parity | partial | Core nav + mode switching plus legacy-aligned `?` help toggle/dismiss, `Tab` list/detail focus flip, `Esc`/`q` back-out behavior from board/insights/graph, non-main `Enter` return-to-main-detail behavior, main-view `Esc` clear-filter-then-quit-confirm flow, `b/i/g` toggle semantics (second press returns to main), `h` history toggle, history `c` confidence cycling, history `v` bead/git timeline toggle (with git-mode enter jump to related issue) plus git-mode `J/K` secondary navigation, history `/` search with query input + filtering (bead list + git timeline) where `Enter` exits input but keeps filter and `Esc` clears, history `g` jump to graph view (git mode selects the event’s issue), `o/c/r/a` filter hotkeys with filter-aware navigation, board-mode `h/l` lane traversal, board-mode `j/k` and `Ctrl+d/Ctrl+u` within-lane vertical paging, board-mode `/` search with query mode plus `n/N` match cycling, board-mode `1/2/3/4` lane selection jumps, board-mode `H/L` first/last lane jumps, board-mode `0/$` plus `Home/End` first/last-in-lane selection, board-mode `e` empty-lane visibility toggle, board-mode `s` grouping cycle (`status/priority/type`), graph-mode `h/l`, `H/L`, and `Ctrl+d/Ctrl+u` list navigation, graph-mode `/` search with `n/N` match cycling, insights-mode `h/l` pane focus switching, insights-mode `/` search with `n/N` match cycling, insights-mode `e` explanation toggle, insights-mode `x` calculation-proof toggle, and main-mode `s` sort-cycle behavior (`created asc/desc`, `priority`, `updated`) are implemented with unit coverage; board/graph/insights detail-pane `J/K` dependency navigation with cursor indicator (falls through to normal nav when no deps exist) are implemented with unit coverage; richer graph/history interaction parity still pending. |

## TUI Fidelity Contract

### Layout Structure
- **Frame**: Header (1 line) | Body (remaining) | Footer (1 line).
- **Body split**: 42% list pane | 58% detail pane.
- **Focus**: `FocusPane::List` or `FocusPane::Detail`; `Tab` toggles. Active pane title shows `[focus]`.
- **No width breakpoints**: fixed split ratios; titles/descriptions truncated via `truncate_str()` to available width.

### View Modes
| Mode | List Pane | Detail Pane |
|---|---|---|
| Main | Issue rows: `> {id} {status} p{priority} {title}` | ID, title, status, priority, type, assignee, labels, PageRank, critical depth, dependencies, description |
| Board | Lane headers: `> {lane} [{count}] {bar}` + card preview (6/lane) | ASCII box card with lane, assignee, blocker/dependent lists, J/K dep nav |
| Graph | Nodes sorted by critical depth+PR: `> {icon} {id} in:{n} out:{n} pr:{f}` | Ego-node ASCII art, BLOCKED BY / BLOCKS sections, GRAPH METRICS (3 subsections), Top PageRank |
| Insights | 10 cycling panels (Bottlenecks..Cycles): `s/S` cycles | Metrics summary, focus issue, expandable explanations (`e`), calc proof (`x`) |
| History | Bead mode: `> {id} events:{n} {status}` / Git mode: `> {sha} {beads} {msg} {ts}` | LIFECYCLE: timeline with connectors, COMMIT DETAILS, RELATED BEADS |

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

### Remaining Fidelity Gaps
- Responsive width breakpoints (~100/140/180 cols) not implemented.
- File tree panel, `o`/`y` hotkeys, legacy search modes not ported.
- No snapshot-based automated visual regression framework yet.

## Integrations
| Capability | Status | Notes |
|---|---|---|
| FrankentUI runtime integration | complete | Active dependency and runtime app usage. |
| Asupersync integration points | partial | Feature-gated wiring scaffolded; deeper worker orchestration pending. |
| Hooks/workspace/history full parity | planned | To be ported in subsequent waves. |

## Verification
| Capability | Status | Notes |
|---|---|---|
| Conformance harness scaffold | complete | Go reference harness + fixture + Rust test skeleton in repo. |
| Fixture-driven parity tests | partial | Legacy fixture-backed conformance checks for diff/history/forecast/triage/plan/priority/burndown with adversarial coverage; added edge-case fixtures (`single_issue.jsonl`, `all_closed.jsonl`) with boundary-condition tests for triage, suggest, plan, insights, forecast, graph, history, and burndown modes; Go-reference burndown fixture generated via `--sprints-file` harness flag; history milestones validated to omit null fields; 89-issue `stress_complex_89.jsonl` stress fixture with diamond deps, fan-out hub, overlapping cycles, deep chain, mixed closed/open, independent islands and 7 conformance tests covering triage counts, cycle detection, graph topology, plan tracks, suggest warnings, graph-root filtering, and deep-chain traversal. |
| Bench harness | complete | Criterion benchmark for triage path added. |

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

### Robot Commands (Analysis/Suggest) — 5 complete+partial, 1 missing
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--robot-diff` | `--robot-diff` | complete | Full fixture-backed conformance. |
| `--diff-since` | `--diff-since` | complete | |
| `--robot-suggest` | `--robot-suggest` | partial | `--suggest-type`, `--suggest-confidence`, `--suggest-bead` implemented. |
| `--suggest-type` | `--suggest-type` | partial | |
| `--suggest-confidence` | `--suggest-confidence` | partial | |
| `--suggest-bead` | `--suggest-bead` | partial | |

### Robot Commands (Alerts) — 1 complete, 3 partial
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-alerts` | `--robot-alerts` | complete |
| `--alert-type` | `--alert-type` | partial |
| `--alert-label` | `--alert-label` | partial |
| `--severity` | `--severity` | partial |

### Robot Commands (Forecast/Capacity/Burndown) — 7 partial
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-forecast` | `--robot-forecast` | partial |
| `--forecast-label` | `--forecast-label` | partial |
| `--forecast-sprint` | `--forecast-sprint` | partial |
| `--forecast-agents` | `--forecast-agents` | partial |
| `--robot-capacity` | `--robot-capacity` | partial |
| `--agents` | `--agents` | partial |
| `--capacity-label` | `--capacity-label` | partial |
| `--robot-burndown` | `--robot-burndown` | partial |

### Robot Commands (History) — 1 partial block
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-history` | `--robot-history` | partial |
| `--bead-history` | `--bead-history` | partial |
| `--history-limit` | `--history-limit` | partial |
| `--history-since` | `--history-since` | partial |
| `--min-confidence` | `--min-confidence` | partial |

### Robot Commands (Graph) — 1 complete, 2 partial
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-graph` | `--robot-graph` | complete |
| `--graph-format` | `--graph-format` | complete |
| `--graph-root` | `--graph-root` | partial |
| `--graph-depth` | `--graph-depth` | partial |

### Robot Commands (Sprint/Metrics) — 3 complete
| Legacy Flag | bvr Flag | Status | Notes |
|---|---|---|---|
| `--robot-sprint-list` | `--robot-sprint-list` | complete | Sprint listing with envelope metadata. |
| `--robot-sprint-show` | `--robot-sprint-show` | complete | Sprint detail by ID. |
| `--robot-metrics` | `--robot-metrics` | complete | Timing, cache, and memory metrics. |

### Robot Options (General) — 5 partial
| Legacy Flag | bvr Flag | Status |
|---|---|---|
| `--robot-max-results` | `--robot-max-results` | partial |
| `--robot-min-confidence` | `--robot-min-confidence` | partial |
| `--robot-by-label` | `--robot-by-label` | partial |
| `--robot-by-assignee` | `--robot-by-assignee` | partial |
| `--label` | `--label` | partial |
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

### Missing — Not Yet Ported (54 flags)
| Category | Flags |
|---|---|
| Correlation/Impact | `--robot-causality`, `--robot-confirm-correlation`, `--robot-reject-correlation`, `--robot-explain-correlation`, `--robot-correlation-stats`, `--correlation-by`, `--correlation-reason`, `--robot-impact`, `--robot-impact-network`, `--robot-related`, `--related-include-closed`, `--related-max-results`, `--related-min-relevance`, `--relations-limit`, `--relations-threshold`, `--network-depth` |
| File Analysis | `--robot-file-beads`, `--robot-file-hotspots`, `--robot-file-relations`, `--file-beads-limit`, `--hotspots-limit` |
| Label/Attention | `--robot-label-attention`, `--robot-label-flow`, `--robot-label-health`, `--attention-limit` |
| Sprint | (moved to implemented) |
| Search | `--search`, `--search-limit`, `--search-mode`, `--search-preset`, `--search-weights`, `--robot-search` |
| Export/Pages | `--pages`, `--pages-include-closed`, `--pages-include-history`, `--pages-title`, `--preview-pages`, `--export-pages`, `--export-graph`, `--export-md`, `--watch-export`, `--no-live-reload` |
| Script | `--emit-script`, `--script-format`, `--script-limit` |
| Baseline/Drift | `--save-baseline`, `--baseline-info`, `--check-drift`, `--robot-drift` |
| Feedback | `--feedback-show`, `--feedback-accept`, `--feedback-ignore`, `--feedback-reset` |
| Workflow | `--robot-blocker-chain`, `--robot-orphans`, `--orphans-min-score`, `--priority-brief`, `--agent-brief` |
| Metadata/Docs | `--robot-recipes` |
| Graph (Advanced) | `--graph-title`, `--graph-preset` |
| Workspace/Config | `--workspace`, `--repo`, `-r`, `--recipe`, `--no-hooks` |

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

### Excluded — Intentionally Out-of-Scope (12 flags)
| Flag | Reason |
|---|---|
| `--update`, `--check-update`, `--rollback`, `--yes` | Self-update (Rust distribution model differs). |
| `--cpu-profile`, `--profile-json`, `--profile-startup` | Dev profiling (not user-facing). |
| `--debug-render`, `--debug-height`, `--debug-width` | TUI debug rendering (internal tooling). |
| `--background-mode`, `--no-background-mode` | Background daemon (architecture differs). |

### Parity Summary
| Category | Complete | Partial | Missing | Excluded | Total |
|---|---|---|---|---|---|
| Robot Commands | 18 | 22 | 0 | 0 | 40 |
| Robot Options | 2 | 5 | 0 | 0 | 7 |
| Format/Meta | 4 | 0 | 0 | 0 | 4 |
| Advanced Analysis | 2 | 0 | 0 | 0 | 2 |
| Missing Surfaces | 0 | 0 | 51 | 0 | 51 |
| Excluded | 0 | 0 | 0 | 11 | 11 |
| Rust-Only | 2 | 0 | 0 | 0 | 2 |
| **Totals** | **32** | **27** | **51** | **11** | **121+2** |

## Phased Implementation Plan

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
| History view full parity (responsive, file tree, search, o/y) | bd-33w.3.3 | History keyflow tests pass |
| Recipe/script workflow (`--robot-recipes`, `--emit-script`, feedback) | bd-33w.2.2 | `cargo test recipe` + `cargo test emit_script` pass |
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
| Performance budgets + stress harness | bd-33w.7.1 | Criterion benchmarks under budget |
| Background-mode async orchestration | bd-33w.7.2 | `cargo test background_mode` passes |
| Profiling parity (cpu-profile, startup profile) | bd-33w.7.3 | `cargo test profiling` passes |
| Module-level unit tests + edge/error coverage | bd-33w.6.4 | Code coverage >= 80% for each module |
| E2E scripts with diagnostics | bd-33w.6.5 | `tests/e2e/*.sh` all exit 0 |
| CI parity gates wired | bd-33w.6.6 | CI runs conformance + e2e + perf checks |

### Wave 4: Release Readiness
Prerequisites: Wave 3 complete.
| Gate | Bead | Verification |
|---|---|---|
| Static pages pipeline (export/preview/watch) | bd-33w.4.4 | `cargo test pages` passes |
| Brief-generation surfaces | bd-33w.4.5 | `cargo test priority_brief` passes |
| Modal/wizard parity | bd-33w.3.4 | `cargo test modal` passes |
| TUI regression harness (snapshots + keyflow) | bd-33w.3.5 | Snapshot tests pass, keyflow scripts complete |
| Debug-render parity flags | bd-33w.3.6 | `cargo test debug_render` passes |
| Operational/admin CLI (update/rollback) | bd-33w.2.6 | excluded (different distribution model) |
| Docs hardened, roadmap self-contained | bd-33w.8.1 | All links in FEATURE_PARITY.md resolve |
| Release-readiness checklist + evidence index | bd-33w.8.2 | Checklist in repo with all items checked |

### Completion Criteria
- All Wave 0-3 gates pass → bvr is functionally equivalent to bv for core workflows.
- Wave 4 gates pass → bvr is release-ready with full documentation and CI coverage.
- 12 excluded flags are documented with rationale and not counted as gaps.

## Open Gaps to 100%
1. Remaining TUI interaction parity (responsive history layout, file tree panel, `o`/`y` keys, search modes).
2. 54 missing CLI flags across correlation/impact, file analysis, label analytics, search, export, script, baseline, feedback, workflow, and metadata categories.
