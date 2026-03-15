# TUI Gap Analysis: `bvr` vs Legacy `bv`

## Executive Summary

The current `bvr` TUI is not close to the repo's stated goal of "full-fidelity parity" with the original Go `bv` TUI. The dominant problem is not one missing feature. It is that the Rust TUI is built as a large plain-text report renderer inside bordered boxes, while the legacy product goal is a workflow tool with high information density, stronger visual hierarchy, richer interaction affordances, and sharper mode-specific behavior.

This is now a hard requirement:

- the `bvr` redesign must leverage every serious relevant capability available in FrankenTUI and `ftui`, first to replicate the original `bv` look-and-feel and workflow confidence, and then to go beyond it
- "uses `ftui`" is not enough; `bvr` has to use the advanced shell, responsive, hit-testing, pane, theming, diagnostics, search, and accessibility capabilities that make FrankenTUI showcase-grade
- the target is not "slightly nicer Rust output"; the target is a first-class terminal product

In its current state, the TUI feels stripped down because it is stripped down:

- most panels are rendered as `Paragraph::new(text)` blocks rather than purpose-built widgets or structured compositions
- most view modes are text dumps with different titles rather than genuinely distinct interaction surfaces
- the visual system is a thin dark palette plus borders, not a designed terminal product
- the repo's parity ledger currently overclaims completeness, which hides the scale of the remaining work

This document treats the problem as a product-gap audit, not just a code review.

## Scope and Method

This audit is based on:

- repo claims in `README.md` and `FEATURE_PARITY.md`
- current implementation in `src/tui.rs`
- current snapshot outputs in `src/snapshots/`
- legacy behavior summaries in `EXISTING_BEADS_VIEWER_STRUCTURE.md`

Important limitation:

- the checked-in legacy source tree is effectively absent here: `find legacy_beads_viewer_code/beads_viewer -maxdepth 2 -type f | wc -l` returns `0`

That means the current repo can prove robot-mode parity via fixtures, but it cannot currently prove TUI parity against live legacy code. The TUI parity claims therefore rest mostly on self-generated Rust snapshots plus prose assertions.

This document therefore adds a second comparison target in addition to legacy `bv`:

- the FrankenTUI showcase contract, as documented in the `frankentui` skill references and the `ftui-demo-showcase` reference app

That matters because the path to a believable `bv` parity TUI in Rust is not to invent ad hoc terminal tricks. It is to adopt the specific proven interaction and rendering patterns already available in FrankenTUI.

## Key Evidence

### Claimed parity is stronger than the implementation supports

- `README.md:5` says the current objective is "full-fidelity parity for robot mode + interactive TUI".
- `FEATURE_PARITY.md:28-34` marks the main TUI, board, insights, graph, history, and full keybinding parity as `complete`.

### The actual render path is generic and text-first

- `src/tui.rs:1115-1700` renders the app almost entirely through `Paragraph::new(...)` and `Block::bordered()`.
- `src/tui.rs:5232` `main_list_text()` builds the main list as formatted strings.
- `src/tui.rs:7813` `issue_detail_text()` builds the detail view as a long sequence of text lines.
- `src/tui.rs:8079` `board_detail_text()` builds the board card and dependency context as text.
- `src/tui.rs:8593` `graph_detail_text()` is an ASCII relationship summary, not a graph interaction surface.
- `src/tui.rs:8898` `history_detail_text()` is a long text report with inline sections.

### Current snapshots validate a sparse, report-like UI

- `src/snapshots/bvr__tui__tests__snap_main_wide.snap`
- `src/snapshots/bvr__tui__tests__snap_graph_wide.snap`
- `src/snapshots/bvr__tui__tests__e2e_journey_main_board_insights_graph_investigation.snap`

These snapshots show a product that is readable, but visually flat, sparse, and much closer to a diagnostics terminal than a polished operations TUI.

### `bvr` is barely using advanced FrankenTUI capabilities today

Cross-checking `bvr` against the FrankenTUI showcase references shows that the current TUI mostly uses:

- `Flex` for basic splitting
- `Paragraph`
- `Block::bordered()`
- a small custom color token set

The current `bvr` TUI does **not** appear to use the advanced capabilities that matter for a flagship operator UI:

- no global shell with tab strip, status bar, help integration, and theme cycling
- no `frame.register_hit(...)` hit-region routing for mouse-aware interaction surfaces
- no draggable splitter handles or pane geometry model
- no `ResponsiveLayout`, `Visibility`, or `Responsive<T>`-style structured breakpoint system
- no `LayoutDebugger` / `ConstraintOverlay`-style layout diagnostics workflow
- no `HoverStabilizer`
- no OSC-8 hyperlink registration with keyboard/mouse focus states
- no `display_width()` / grapheme-aware layout discipline visible in the TUI code path
- no `TextArea`-style richer editor/search surfaces
- no showcase-grade global chrome or command-palette level discoverability

Evidence:

- `src/tui.rs` has custom `Breakpoint` percentages, but no `ResponsiveLayout`-style structured responsive system.
- `src/tui.rs` shows no `register_hit`, `register_link`, `HitRegion`, `TextArea`, `HoverStabilizer`, `LayoutDebugger`, `ConstraintOverlay`, `display_width`, `grapheme_count`, or `flow_direction` usage.

This is the central implementation gap. `bvr` is not underpowered because FrankenTUI is underpowered. It is underpowered because the app is barely using the high-end parts of FrankenTUI.

## FrankenTUI Requirement

The redesign must explicitly treat FrankenTUI as the implementation playbook, not just the runtime crate.

### Non-negotiable rule

Before we claim parity or polish again, `bvr` must adopt every relevant advanced FrankenTUI capability needed to:

1. reproduce the operator confidence, visual richness, and workflow completeness of legacy `bv`
2. exceed it where FrankenTUI gives us better primitives than the original Go implementation had

### What this means in practice

- use showcase-grade shell patterns, not a custom minimalist header/footer
- use structured responsive layout APIs, not only hand-rolled width percentage switches
- use hit-tested interactive regions and pane models, not only keyboard selection state
- use richer visual delineation, panel semantics, and multi-panel IA
- use the diagnostics and testing affordances FrankenTUI already provides to debug layouts and responsiveness

## Specific FrankenTUI Features `bvr` Needs To Use

The list below is not speculative. These are concrete capability families from the FrankenTUI references that map directly onto `bvr`'s missing product qualities.

### 1. Global shell and discoverability primitives

`bvr` should stop presenting itself as a naked content screen and instead adopt a showcase-grade shell with:

- a real top navigation/tab strip for view modes
- a real bottom status bar for context, actions, toggles, and mode hints
- a help overlay that merges global and per-mode bindings
- global theme cycling, ideally with the same `Ctrl+T` expectation used by FrankenTUI showcase apps
- command-palette grade discoverability for actions and mode jumps

Why this is required:

- the current one-line header/footer is low-density and low-value
- mode switching feels bolted on
- discoverability lives in footer prose instead of product chrome

FrankenTUI references:

- app shell layering and chrome: `ftui-demo-showcase` deep dive sections 3, 6, 7, 13
- tab bar, status bar, help overlay, accent mapping, theme cycling

### 2. Structured responsive layout APIs

`bvr` needs to replace the current custom `Breakpoint::{Narrow,Medium,Wide}` percentage tweaks with a real responsive system:

- `ResponsiveLayout`
- `Breakpoint` tiers
- `Visibility::visible_above(...)`
- `Responsive<T>::resolve(...)`
- procedurally computed intrinsic sizing with minimum floors

Why this is required:

- current breakpoints mostly change percentages, not information architecture
- many views still waste space or collapse awkwardly
- mode-specific layouts need true tiered compositions

Immediate `bvr` targets:

- main triage
- board
- graph
- history
- help overlay
- modal flows

### 3. Adjustable panes and splitter interaction

For multi-panel modes, `bvr` should use real pane geometry and splitters:

- explicit pane rect caching
- splitter hit regions
- drag-to-resize with bounded ratios
- keyboard-safe drag reset behavior

Why this is required:

- the legacy app felt denser and more tool-like
- fixed ratios are leaving value on the table
- graph/history/board especially want user-controlled width allocation

Immediate `bvr` targets:

- main list/detail
- history's multi-panel investigation layout
- board lane/detail split
- graph node/detail relationship split

### 4. Hit-testing and mouse-aware interaction surfaces

`bvr` should adopt `frame.register_hit(...)`-style interaction regions and a proper hit map:

- clickable tabs and mode selectors
- clickable panel focus changes
- clickable board cards and lanes
- clickable history file tree entries
- clickable graph relationship targets
- clickable footer/status toggles

Why this is required:

- a serious TUI should not be keyboard-only by implementation accident
- hit regions also force better IA and layout discipline
- they enable richer affordances even for keyboard-first users

### 5. Rounded bordered multi-section panel grammar

`bvr` should adopt the stronger FrankenTUI panel delineation strategy:

- rounded borders for major sections
- semantic border accents by section type/focus/state
- distinct neutral surfaces with highlighted active regions
- stronger local hierarchy inside detail panes

Why this is required:

- current bordered paragraphs all look visually equivalent
- there is no strong section rhythm
- modes do not feel materially different from one another

Immediate `bvr` targets:

- main detail pane
- graph metrics and relationship zones
- insights metric modules
- history commit/event/file panes
- board lane summaries

### 6. Search excellence contract

`bvr` already has search in several modes, but it needs the full FrankenTUI search contract:

- search-as-you-type in every searchable surface
- explicit focus entry/exit
- current/total match visibility
- active-match emphasis stronger than passive matches
- list-level marker plus line-level highlight
- contextual search aids like local context snippets, radar, density indicators, or summary strips

Why this is required:

- current search is functionally present but visually weak
- search does not feel premium or confidence-building

Immediate `bvr` targets:

- main list
- board lane search
- graph node search
- history bead/git search
- filter pickers

### 7. Visual signal density primitives

`bvr` should use more of FrankenTUI’s visual-signal toolbox:

- metric bars and richer mini charts
- sparkline-style density indicators
- better badges and micro-status chips
- stronger high-signal emphasis for critical issues, blockers, stale work, cycles, and top-ranked nodes

Why this is required:

- current metrics are mostly text plus tiny bars
- high-value insights do not visually pop
- the TUI does not feel like an advanced triage engine on first glance

### 8. Layout debugging and constraint inspection

During redesign work, `bvr` should use or emulate the FrankenTUI layout-debug workflow:

- `LayoutDebugger`
- `LayoutRecord`
- `ConstraintOverlay`

Why this is required:

- the redesign will be heavily layout-driven
- right now there is no first-class way to inspect why a screen wastes space or collapses poorly
- layout debugging should be part of the development loop, not a guess-and-snapshot cycle

### 9. Theme system and palette discipline

`bvr` should move beyond its current small custom palette and adopt a more mature token discipline:

- neutral background surfaces
- accent colors reserved for focus/highlights/status
- multiple supported themes, ideally aligned with FrankenTUI’s standard theme rotation
- accessibility-aware contrast validation

Why this is required:

- current visual design feels like a basic dark theme, not a flagship terminal UI
- a stronger theme system will force better visual semantics and contrast decisions

### 10. Hyperlinks and terminal-native affordances

`bvr` should use terminal-native link affordances where appropriate:

- OSC-8 link registration for issue refs, repo links, commits, docs, and external tickets
- keyboard focus for links
- hover/focus/default link styling states

Why this is required:

- history, graph, and issue detail views all have obvious link-worthy entities
- opening external context should feel first-class

### 11. Grapheme-aware and internationalized text handling

Even if `bvr` is not deeply localized today, the redesign should follow FrankenTUI’s text-metrics discipline:

- `display_width()` for terminal layout
- grapheme-aware cursor/editing math
- safe truncation and wrapping primitives
- RTL-friendly layout support where appropriate

Why this is required:

- many of the current renderers are string-format heavy
- that is exactly where Unicode/layout bugs hide
- a flagship TUI should not regress on width correctness while being redesigned

### 12. Deterministic visual diagnostics and richer verification

`bvr` should go beyond static snapshots and adopt stronger visual verification patterns inspired by FrankenTUI:

- deterministic rendering hooks for complex visual states
- richer multi-step journey captures
- layout-threshold tests
- explicit tiny/medium/large acceptance criteria
- optional JSONL diagnostics for interactive state transitions

Why this is required:

- snapshot stability is currently preserving weak UX
- redesign work needs tools that help validate quality, not merely freeze output

## Findings

### Critical 1: The parity ledger is inaccurate and currently harmful

Location:

- `FEATURE_PARITY.md:28-34`
- `README.md:5`

Issue:

- The documentation claims TUI completeness while the current UI still lacks the depth, density, and confidence of a mature operator surface.
- Because the parity ledger says "complete", the codebase currently treats obvious product gaps as already solved.

Root cause:

- The project has a strong robot-mode conformance harness, but no equivalent evidence source for live TUI parity.
- Self-generated snapshots verify stability of current output, not equivalence to the legacy experience.

Impact:

- Misprioritization
- false confidence
- review blind spots
- pressure toward local patching instead of redesign

Recommended fix:

- Downgrade TUI parity statuses from `complete` to `partial` across the major modes until the redesign is real.
- Add an explicit "known TUI product gaps" section to `FEATURE_PARITY.md`.
- Treat this audit as the new source of truth until a real parity proof method exists.

### Critical 2: The TUI architecture is optimized for string assembly, not product quality

Location:

- `src/tui.rs:1115-1700`
- `src/tui.rs:5232`
- `src/tui.rs:7813`
- `src/tui.rs:8079`
- `src/tui.rs:8593`
- `src/tui.rs:8898`

Issue:

- The dominant implementation pattern is: generate a large multi-line string, wrap it in a bordered paragraph, and render it.
- This forces every view into the same visual grammar and blocks richer composition, emphasis, alignment, and affordance design.

Root cause:

- `src/tui.rs` became a monolithic renderer where the easiest way to add capability was to append more text lines.
- The code optimizes for getting content on screen, not for building reusable mode-specific UI primitives.

Impact:

- all views feel samey
- poor visual hierarchy
- weak density control
- awkward scrolling
- hard-to-evolve styling
- expensive future redesign

Recommended fix:

- Break the TUI into composable mode-specific renderers.
- Move away from whole-panel string generation for core views.
- Introduce reusable widgets for list rows, badges, metric strips, dependency chips, timeline rows, lane cards, and status bars.
- Rebuild the screen architecture around the actual FrankenTUI showcase patterns: shell chrome, responsive tiers, hit-region routing, pane geometry, and semantic panels.

### High 1: Main view is informative but still feels like a text dump, not an operator cockpit

Location:

- `src/tui.rs:5232-5265`
- `src/tui.rs:7813-8016`
- `src/snapshots/bvr__tui__tests__snap_main_wide.snap`

Issue:

- Main list rows only show `id`, `status`, `priority`, and `title`.
- Critical decision signals such as assignee, repo, open blockers, age/staleness, score, due pressure, and change activity are absent from the scan line.
- The detail pane is long and useful, but it reads like a generated report rather than a dashboard with strong grouping and emphasis.

Root cause:

- the main list was designed as a formatted string row rather than a compact triage row component
- the detail pane overuses line-oriented prose sections instead of structured blocks with stronger prioritization

Impact:

- slow scanning
- weak triage confidence
- unnecessary context switching
- poor "what should I do next?" ergonomics

Recommended fix:

- Redesign main rows to include a compact status chip, priority chip, blocker count, assignee, repo, and one risk/age indicator.
- Convert detail content into a small set of strong modules: Summary, Action State, Risk, Dependencies, Metrics, Narrative.
- Add clear top-of-pane action affordances: claim, open blocker, jump to dependent, filter by repo/label.

### High 2: Board mode is not a compelling kanban experience

Location:

- `src/tui.rs:5267-5350`
- `src/tui.rs:8079-8178`
- `src/snapshots/bvr__tui__tests__e2e_journey_main_board_insights_graph_investigation.snap`

Issue:

- The board is a textual lane summary with issue previews, not a convincing multi-card lane workspace.
- Empty vertical space dominates the screen.
- Lane state, WIP pressure, bottlenecks, and "why this lane matters" are weakly expressed.

Root cause:

- lane rendering is just text rows plus a single-card detail pane
- there is no visual card system, lane summary module, or richer board interaction model

Impact:

- board mode feels like a filtered report, not an operational board
- poor spatial memory
- low payoff for switching out of main mode

Recommended fix:

- Render true lane cards in-grid rather than only lane headers plus list lines.
- Add lane-level summary strips: count, WIP, blocked %, stale %, top risk.
- Make card density adaptive by width instead of preserving large empty gutters.

### High 3: Graph mode is analytically interesting but not experientially graph-like

Location:

- `src/tui.rs:8593-8788`
- `src/snapshots/bvr__tui__tests__snap_graph_wide.snap`

Issue:

- Graph mode is effectively a ranked node list plus an ASCII ego-node diagram and metrics report.
- It does not feel like graph exploration. It feels like reading a node profile.

Root cause:

- the implementation focuses on textual summaries of graph metrics instead of a stronger graph navigation metaphor
- edge traversal and neighborhood comprehension are secondary to prose output

Impact:

- weak sense of topology
- low discoverability of clusters and bottlenecks
- poor spatial intuition compared with what operators expect from a graph view

Recommended fix:

- Reframe graph mode around neighborhood exploration: focal node, immediate ring, blockers ring, dependents ring, edge legend, cycle badge cluster.
- Use clearer metric badges and side panels rather than a long vertical metric report.
- Consider dedicated narrow/medium/wide graph layouts rather than reusing one textual structure.

### High 4: Insights mode and history mode still read like reports rather than products

Location:

- `src/tui.rs:8198+`
- `src/tui.rs:8898+`
- snapshots under `src/snapshots/*insights*` and `src/snapshots/*history*`

Issue:

- Insights mode appears to be a rotating panel of textual analytics rather than a visually distinctive analysis workspace.
- History mode has more structure than the others, but it is still mainly sectioned text with inline legends and footers.

Root cause:

- the implementation strategy keeps extending string-based sections instead of adding specialized visual primitives

Impact:

- limited "mode identity"
- low memorability
- low sense of craftsmanship
- harder onboarding because every mode speaks the same visual language

Recommended fix:

- Give each mode its own design grammar.
- Insights should feel like an analytics console: tiles, meters, ranked outliers, heatmaps with stronger framing.
- History should feel like an investigation tool: event rail, commit cards, file-focus lens, issue-crosslink strip.

### High 5: The current TUI wastes too much screen real estate

Location:

- `src/tui.rs:1073-1109`
- `src/tui.rs:1497-1576`
- snapshots across `main`, `board`, `graph`, `insights`, `history`

Issue:

- Many screens show large blank zones while useful information is hidden in a second pane or lower in the text flow.
- The fixed one-line header and one-line footer consume permanent vertical space while repeating low-value guidance.
- The 42/58 split is applied broadly even when a mode would benefit from a very different layout.

Root cause:

- breakpoint logic mainly changes percentages, not information architecture
- layout policy is global and conservative rather than mode-specific and density-aware

Impact:

- stripped-down feel
- poor information density
- weak "premium" perception

Recommended fix:

- Replace the one-size-fits-all split policy with per-mode layout strategies.
- Collapse or compress low-value header/footer text.
- Use stacked micro-panels, metric bands, and inline chips to exploit width better.

### Medium 1: Visual design is too thin to support the claimed ambition

Location:

- `src/tui.rs:266-322`

Issue:

- The palette is a small set of muted dark-theme tokens with one accent, a few semantic colors, and border emphasis.
- The resulting UI is readable, but not distinctive, rich, or confident.

Root cause:

- styling exists as a token layer, but the product does not yet have a strong visual language
- too much of the UI depends on border/no-border and bright/dim text only

Impact:

- weak hierarchy
- low delight
- reduced readability under dense content
- "prototype" feel

Recommended fix:

- Expand the token system beyond basic fg/bg colors.
- Introduce role-specific surfaces, badges, separators, and emphasis levels.
- Use typography-like variation in terminal terms: weight, density, icon rhythm, spacing, boxed clusters, and contrast zones.

### Medium 2: Overlays and modals are functional but primitive

Location:

- `src/tui.rs:1134-1305`

Issue:

- Help, tutorial, quit confirm, recipe picker, label picker, repo picker, and wizard overlays all use the same bordered paragraph treatment.
- They feel like temporary text blobs, not designed modal flows.

Root cause:

- no reusable modal-shell system with richer structure
- no differentiated overlay patterns for confirm vs picker vs wizard vs tutorial

Impact:

- low polish
- weak interaction confidence
- poor discoverability

Recommended fix:

- Design a proper modal framework with title, subtitle, primary actions, secondary actions, key hints, and content regions.
- Give pickers real list affordances and selection emphasis.

### Medium 3: Discoverability currently relies too much on footer prose

Location:

- `src/tui.rs:1582-1699`

Issue:

- The footer is overloaded with long prose strings describing navigation.
- Important affordances are remembered only if the user reads and re-reads the footer.

Root cause:

- interaction discoverability was pushed into text hints instead of being embodied in the UI

Impact:

- learning friction
- clutter
- weak expert feel

Recommended fix:

- Use compact command chips and mode badges instead of long sentences.
- Surface context-sensitive actions near the focused object, not only in the footer.

### Medium 4: The snapshot strategy protects regressions in the current weak UI

Location:

- `src/snapshots/`
- `FEATURE_PARITY.md:168-170`

Issue:

- The snapshot suite is strong for stability, but it mostly captures the current Rust output.
- That can lock in mediocrity if the snapshots are treated as evidence of parity rather than evidence of consistency.

Root cause:

- TUI verification is Rust-self-referential while robot verification is legacy-referential

Impact:

- easier to preserve current UX flaws
- harder to justify large redesigns

Recommended fix:

- Reclassify snapshots as regression safety only.
- Build a redesign checklist from operator workflows instead of from current snapshot shapes.

## Gap Matrix

| Area | Current State | Why It Feels Bad | Priority |
|---|---|---|---|
| Main triage view | text rows + report detail | low scan density, weak action framing | Critical |
| Board | textual lanes + one detail card | not a true kanban workspace | High |
| Graph | ranked list + ASCII ego view | not graph-native exploration | High |
| Insights | rotating text analytics | insufficient visual identity | High |
| History | structured report with sections | investigation workflow still too text-heavy | High |
| Visual design | competent but generic dark theme | feels unfinished and bare | High |
| Modals | paragraph-based overlays | low polish and low confidence | Medium |
| Breakpoints | width percentage tweaks | not true responsive redesign | Medium |
| Parity process | overclaims + missing legacy TUI source | hides the real problem | Critical |

## What "1000 Times Better" Actually Means

The right target is not pixel-perfect parity. It is product parity in operator confidence. Concretely, the redesign should produce:

- faster scanability in the first 3 seconds
- stronger mode identity
- higher information density without clutter
- visible action affordances
- layouts that feel intentional at narrow, medium, and wide widths
- less footer reading, more obvious interaction
- a UI that feels like a serious triage console rather than a text report viewer

## Recommended Remediation Sequence

### Phase 1: Tell the truth in the docs

- downgrade TUI parity claims from `complete`
- add a "known TUI product gaps" section
- separate robot parity from TUI parity in project status

### Phase 2: Refactor the rendering architecture

- split `src/tui.rs` into mode-specific rendering modules
- replace whole-panel string assembly with structured widgets/composites
- define reusable primitives for rows, chips, cards, metrics, timelines, and command hints
- introduce a `bvr` shell layer modeled on FrankenTUI showcase chrome: top mode tabs, bottom status/actions, integrated help, integrated theme control
- introduce hit-region registration and pane-rect caching as first-class infrastructure
- introduce a structured responsive layout system rather than custom percentage-only breakpoints

### Phase 3: Rebuild the main triage experience first

- redesign the main view as the flagship screen
- make list rows far denser and more decision-oriented
- make the detail pane feel like a cockpit, not a report
- use semantic panels, compact badges, split metrics bands, and actionable controls
- use adjustable pane geometry, richer search affordances, and better section delineation

### Phase 4: Rebuild the high-value secondary modes

- board as a real kanban surface
- graph as a neighborhood exploration surface
- history as an investigation surface
- insights as an analytics surface
- use advanced FrankenTUI patterns directly:
  - board: hit-tested cards, drag-friendly lane geometry, lane summaries, keyboard/mouse parity
  - graph: focused neighborhoods, semantic panels, link targets, denser metric composition
  - history: event rail, commit cards, file tree focus system, interactive links
  - insights: tile/grid analytics, heatmaps, ranked outliers, responsive section visibility

### Phase 5: Rebuild the visual system

- stronger terminal design language
- better spacing, emphasis, and section rhythm
- compact command chips, badges, and contextual hints
- theme-aware accent system aligned with FrankenTUI palette discipline
- rounded semantic sections instead of generic bordered text boxes
- stronger contrast and mode identity across the shell and content panes

### Phase 6: Replace self-congratulation with acceptance criteria

- define workflow-based TUI acceptance tests
- measure scanability, discoverability, density, and mode identity
- stop calling a mode "complete" because it has keybindings and no panics
- add a FrankenTUI-capability checklist to each major TUI redesign bead so screens cannot regress into "just another paragraph in a box"

## FrankenTUI-Driven Acceptance Criteria

The redesign should not be considered done until `bvr` satisfies all of the following:

- it uses a true global shell, not just a header/footer line
- it uses structured responsive layout tiers with mode-specific compositions
- it uses hit-tested interactive regions for major controls and panel focus
- it uses adjustable panes where multi-panel workflows benefit from them
- it uses stronger semantic visual delineation than generic bordered paragraphs
- it uses premium search affordances in every searchable mode
- it uses richer signal density for triage-critical metrics and states
- it uses a mature theme/token system with stronger contrast and mode identity
- it has layout-debug and visual-verification workflows suited to redesign work
- it reaches original `bv` workflow confidence before attempting Rust-only enhancements beyond parity

## Bottom Line

The current `bvr` TUI is not failing because it is missing one or two features. It is failing because the implementation strategy produced a stable text renderer and then the project declared parity too early.

The redesign should start by admitting that the TUI is still in the "competent prototype" stage. Once that is explicit, the path forward is clear:

- fix the parity narrative
- refactor the render architecture
- rebuild the main operator workflows with much stronger visual and interaction design

That is the shortest path to a TUI that no longer feels like a stripped-down imitation of the original.
