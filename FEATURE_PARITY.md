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
| `--robot-diff` | partial | Snapshot diff now emits legacy-style nested metadata (`from/to` timestamps, revision, removed issues, cycle deltas, metric deltas, health summary) and legacy-shaped issue payloads (compact fields, dependency metadata, comments, zero-time defaults) with expanded fixture-backed conformance assertions. |
| `--robot-history` | partial | Git-aware commit correlation, commit index, milestones, method stats, `--history-since`, and `--min-confidence` filtering are implemented; robot-history export shape now omits bead-only fields to align with legacy output contracts, while deeper confidence/scoring heuristic parity tuning remains. |
| `--robot-forecast` | partial | ETA forecast now supports `--forecast-label`, `--forecast-sprint`, and `--forecast-agents` with legacy-compatible all-vs-single filtering semantics, plus legacy-style ETA payload fields (`eta_date_low/high`, `velocity_minutes_per_day`) and order/factor/value conformance assertions against fixture data. |
| `--robot-capacity` | partial | Implemented `--agents` + `--capacity-label`, critical path/actionable/bottleneck metrics, and ETA-minute projection via legacy-inspired `EstimateETAForIssue` complexity/velocity model; added fixture-backed capacity parity checks (including label-scoped output), exact label-scope edge semantics, and forecast/capacity total-minute consistency checks. |
| `--robot-burndown` | partial | Implemented sprint selection (`current` or ID), burndown totals, `daily_points`, `ideal_line`, and git-derived `scope_changes`; minor metadata parity details still pending. |
| `--robot-suggest` | partial | Suggestion suite implemented with `--suggest-type`, `--suggest-confidence`, and `--suggest-bead` filters; detector caps/sorting and dependency-direction heuristics are now aligned more closely with legacy behavior, with final edge-case tuning still pending. |
| `--robot-graph` | complete | JSON/DOT/Mermaid export with `--graph-root`/`--graph-depth`/`--label` filters and deterministic output implemented. |

## Interactive TUI
| Legacy Capability | Status | Notes |
|---|---|---|
| Bare command launches TUI | complete | `bvr` launches frankentui app. |
| Main list/detail split | partial | Base split and navigation in place. |
| Board view (`b`) | partial | Replaced placeholder with lane-aware board pane (lane counts, queue sample, selected issue blockers/dependents); full visual/keybinding parity with legacy board workflow still pending. |
| Insights view (`i`) | partial | Replaced placeholder with bottleneck/critical-path/cycle hotspot pane; full visual/keybinding parity with legacy insights workflow still pending. |
| Graph view (`g`) | partial | Replaced placeholder with data-rich graph pane (centrality, blockers/dependents, cycle membership, top PageRank list); full legacy keyboard/visual parity still pending. |
| History view (`h`) | partial | Replaced placeholder with lifecycle timeline pane (events, status timestamps, cycle-time summary); full git/correlation-rich history UI parity still pending. |
| Full keybinding parity | partial | Core nav + mode switching plus legacy-aligned `?` help toggle/dismiss, `Tab` list/detail focus flip, `Esc`/`q` back-out behavior from board/insights/graph, non-main `Enter` return-to-main-detail behavior, main-view `Esc` clear-filter-then-quit-confirm flow, `b/i/g` toggle semantics (second press returns to main), `h` history toggle, history `c` confidence cycling, history `v` bead/git timeline toggle (with git-mode enter jump to related issue) plus git-mode `J/K` secondary navigation, history `/` search with query input + filtering (bead list + git timeline) where `Enter` exits input but keeps filter and `Esc` clears, history `g` jump to graph view (git mode selects the event’s issue), `o/c/r/a` filter hotkeys with filter-aware navigation, board-mode `h/l` lane traversal, board-mode `j/k` and `Ctrl+d/Ctrl+u` within-lane vertical paging, board-mode `/` search with query mode plus `n/N` match cycling, board-mode `1/2/3/4` lane selection jumps, board-mode `H/L` first/last lane jumps, board-mode `0/$` plus `Home/End` first/last-in-lane selection, board-mode `e` empty-lane visibility toggle, board-mode `s` grouping cycle (`status/priority/type`), graph-mode `h/l`, `H/L`, and `Ctrl+d/Ctrl+u` list navigation, insights-mode `h/l` pane focus switching, insights-mode `e` explanation toggle, insights-mode `x` calculation-proof toggle, and main-mode `s` sort-cycle behavior (`created asc/desc`, `priority`, `updated`) are implemented with unit coverage; deeper board detail shortcuts and richer graph/history interaction parity are still pending. |

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
| Fixture-driven parity tests | partial | Added legacy fixture-backed conformance checks for diff/history/forecast core fields plus triage/plan/priority, expanded corpus with adversarial cycle/reopen/label-edge fixture coverage (`adversarial_parity.jsonl` + `bvr_adversarial.json`), and added a focused `bd-3q0` blocker scenario asserting graph/insights/history consistency. Deep payload parity still pending. |
| Bench harness | complete | Criterion benchmark for triage path added. |

## Open Gaps to 100%
1. Port remaining robot metadata/details: final suggest/history edge-case tuning against legacy fixtures.
2. Close remaining TUI keyboard/visual parity gaps across board/insights/graph/history modes.
3. Finish remaining history/correlation options and export-toolchain parity.
4. Tighten fixture corpus to cover edge cases and large datasets.
