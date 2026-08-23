# Changelog

All notable changes to **beads_viewer_rust** (`bvr`) are documented here.

Releases are published to crates.io (`cargo install beads_viewer_rust`) and
tagged on GitHub. Earlier history below is organized by development phase and
grouped by capability area rather than raw diff order. Every commit link
points to the canonical GitHub repository at
<https://github.com/Dicklesworthstone/beads_viewer_rust>.

Binary name: **`bvr`**

---

## [v0.3.0] -- 2026-08-22

### Self-update: new `bvr upgrade` command (#23)

- New `bvr upgrade` subcommand: resolves the newest released version
  (crates.io first, then GitHub releases, then the highest semver tag) and
  installs it via `cargo install beads_viewer_rust --version <latest> --locked`.
  `bvr upgrade --dry-run` reports what would be installed without changing
  anything.
  ([79b7386](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/79b7386))
- Fixed `--check-update`, which permanently reported "network unavailable":
  it only consulted GitHub `releases/latest`, which 404s on this repository,
  and its hint referenced a nonexistent `bvr` cargo package. It now shares
  the multi-source version resolution above and names the correct
  `beads_viewer_rust` package.
- README documents the upgrade flow.

### Test hardening

- `.beads`-discovery tests are now robust to non-hermetic environments
  (macOS `/private/var` canonicalization; ancestor `.beads/` directories
  above the test TMPDIR), with loud environment skips only where the test
  premise cannot be constructed.
  ([24727a4](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/24727a4))

---

## Unreleased (0.1.0-dev)

### Phase 7 -- TUI Semantic Redesign and Recommendation Schema (2026-03-21)

A visual-primitive refactor laying the groundwork for a full TUI redesign,
alongside richer robot recommendation payloads and triage behavioral fixes.

#### TUI Visual Primitives

Introduces a semantic tone system and reusable panel/summary-line primitives
(`semantic_panel_block`, styled detail lines) so that future TUI redesign
work can compose screens from a consistent visual vocabulary instead of
ad-hoc rendering.

- Add semantic tone system and reusable visual primitives for TUI.
  ([8f470d0](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/8f470d029e37fe09db2aaa7f8bb45c65f88d7913))
- Refactor TUI panels to use `semantic_panel_block` and add styled detail
  summary lines.
  ([aec6a15](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/aec6a1548add1213403753a9143807f4238e260b))
- Add reusable visual primitives for TUI redesign.
  ([745a875](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/745a8752b1625c42079e25cb2937487b249c997b))

#### TUI Debug Rendering

New debug render modes let developers inspect layout geometry, hit-test
regions, and the combined capture view without running the full interactive
loop.

- Add layout and hit-test debug rendering modes for `render_debug_view`.
  ([96c8f41](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/96c8f41bf16928195a263b65a6d6387904935278))
- Add capture debug render mode combining view, layout, and hit-test output.
  ([d290f50](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d290f505a43ef6b004502b5b860799cb4e01dc19))
- Remove dead code, improve list panel rendering and cursor detection.
  ([e70212d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/e70212d5a0dba25b05d533be3c2c3c0260e16d4f))

#### Robot Recommendation Schema

Recommendation payloads now carry `action`, `blocked_by`, `unblocks_ids`, and
`type` fields so agents can act on a recommendation without making extra
lookups into the issue graph.

- Enrich recommendation schema with action/blocked_by/unblocks_ids/type.
  ([6c4e938](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/6c4e9388025704b19da5d8e0ff1062b38906c9d1))
- Expand debug render views and enrich robot recommendation schema.
  ([ac3c960](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/ac3c9607c67a4672d231b918751f603c695b4734))

#### Triage Behavioral Changes

Items with `in_progress` status are now excluded from `top_picks` and
`highest_impact` in both triage and plan outputs, because recommending
work that is already underway wastes agent attention.

- Exclude `in_progress` items from `top_picks` and `highest_impact`.
  ([5944716](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/59447162f98f6c86a51f1a49b600e76addd33049))

#### Export

- Upgrade `history.json` in pages bundles to match `--robot-history` output
  shape for consistent downstream consumption.
  ([d48952c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d48952cc837f6048556672837441708eb86201b2))

#### Bug Fixes

- Always serialize `commits` and `cycle_time` fields as `null` in history
  output for consistent downstream parsing.
  ([5ffbc3a](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/5ffbc3a0538ae1b5287a0b373f1c07bde8170b5a))
- Remove silent `usize` truncation in suggest confidence calculations that
  could produce wrong confidence scores on platforms where usize differs from
  the expected integer width.
  ([1ef2ce1](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/1ef2ce1a47fa7b8c088b37bfc94a763c797923e6))

---

### Phase 6 -- Export Hardening, E2E Coverage, and Parity Closure (2026-03-18 to 2026-03-20)

Closes the last parity gaps with the legacy Go `bv`, hardens path resolution
across workspace and export flows, and significantly expands end-to-end test
coverage.

#### Path Resolution and Workspace Loading

A family of bugs where export paths, hook contexts, and workspace repo paths
were resolved relative to the wrong directory (cwd instead of workspace root
or project dir).

- Resolve relative export paths from workspace root, not cwd.
  ([fea4f66](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/fea4f66e6f7a799c648fd6c424aa806ff12a2165))
- Resolve relative paths in `HookContext` and normalize parent segments in
  workspace repo loading.
  ([9c31570](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/9c315708c0aac9bd6063d78a83161f7e01fbd16c))
- Run export hooks from project dir; resolve feedback project dir via
  `--repo-path` fallback chain.
  ([d91f90d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d91f90d57ec02e6f91f4fc72d50a880c01645c5d),
   [1706200](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/17062002a5350f2f21b23caf34866fbd7071cfc2))
- Error when all workspace repos fail to load instead of silently returning
  an empty issue set.
  ([b830bca](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b830bcae881af87a46895fa1beac089921d0f709))

#### Export and Pages Hardening

- Add pre-flight validation, timeouts, and better error guidance for export
  operations.
  ([1fb3918](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/1fb39181ef25dfd749e6c6196078c67e37d46f09))
- Add preview server timeouts and improve export error context.
  ([0a9c273](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0a9c27345e48068eb685188be6b4c8dca8b97e3f))
- Enforce vendored `sql.js` only; remove CDN fallback so pages bundles work
  fully offline.
  ([b593275](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b593275a5a1d4e7b05dc802cd1bebecc7f0be654))

#### TUI Improvements

- Add history-mode footer discoverability with context-aware commit actions.
  ([21cb22f](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/21cb22ff00d01c6b10f805549f064eb59276f84e))
- Add Main mode detail pane scrolling.
  ([311710c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/311710c4924be9427b31f59228cef08f5c5f4c60))
- Replace `as u16` scroll truncation with saturating clamp to prevent
  potential panic on very long lists.
  ([3536163](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/35361631d1bcab6de4cddaff14fdf3caacac1a89))
- Extract link detection, improve graph detail rendering, cache layout
  computation.
  ([646573f](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/646573f891ba9f99c44467fe02a67c698e88945f),
   [a443d77](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a443d77ef2fd91f1fb9cb3af1ea1c38b61c3be69))

#### Model and Search

- Add `closed_at` validation; eliminate search double-computation.
  ([642164c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/642164cece3e2710187d0ba91b433ded0e966398))

#### Loader

- Warn when multiple preferred JSONL files exist in `.beads/`.
  ([697712a](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/697712aa505c65296068b47a064626ec23cc8743))

#### Bug Fixes

- Correct `robot-docs` `key_fields` for 8 flattened commands.
  ([36801f2](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/36801f26ead564ba78d1fa2e0971f1267af2e7ff))
- Namespace diff baseline issues in single-repo workspace mode.
  ([2b546f7](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/2b546f7590ea96bd5917a548047bd9dbb47c4cff))
- Correct orphans test to check `stats.total_commits` instead of top-level
  `total_commits`.
  ([86f342d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/86f342d1212daf5ffea3681efbec07dff7cbee7b))
- Fix history footer/detail tests that leaked cwd git state.
  ([bb75ad6](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/bb75ad6c1a4d0d92fb8bcebddbbef6c4a1d11b96))

#### Test Coverage

- Extend preview/pages shell tests from 5 to 10 scenarios.
  ([f4b1dc6](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/f4b1dc603258abd19542716c774720b01e3a34ae))
- Expand `robot-schema`, `robot-docs`, `robot-help` coverage and fix drift
  baseline isolation.
  ([d4fe8be](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d4fe8be06327fd931dda346627db52ddedc5b38f))
- Make artifact bundles self-contained with portable replay scripts.
  ([6edc83b](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/6edc83b783d2b3f0bf243b09d7b0f1f0b1701f36))
- Add E2E tests for 8 previously untested robot commands.
  ([efc8126](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/efc81264b08c8ff295b6de6e6135068df449cb54))
- Add Unicode width coverage for text display primitives.
  ([7b98294](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/7b98294ac54aebf5f464d648ce1280143651629a))

#### Documentation

- Major README expansion with feature documentation (+436 lines).
  ([ed27118](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/ed27118f936eaaf9af51741e33e34d89d316f20c),
   [b7721c2](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b7721c274b610ea75974bc86d92adc79c99ab826))

---

### Phase 5 -- Massive Test Expansion and TUI Polish (2026-03-12 to 2026-03-17)

201 new unit tests across the codebase, major CLI flag additions, TUI view
mode expansion, pages wizard fixes, and documentation refresh.

#### Unit Test Campaign (201 new tests)

Targeted test blitz covering the loader, analysis, and export layers that had
been developed faster than they were tested.

- 79 tests for `analysis/diff.rs` and `analysis/drift.rs`.
  ([8fb00fd](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/8fb00fd7a16926a2ba455e35283395e8d521f35c))
- 41 tests for `loader.rs`.
  ([4fb417d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/4fb417dddad3bb92dc29ddd5ff08fa1513453393))
- 37 tests for `export_sqlite.rs` and `export_md.rs`.
  ([295e721](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/295e7217d94c3dc7b21f155456b65a646a8a08ab))
- 31 tests for `analysis/causal.rs`.
  ([9462ad7](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/9462ad7251a9ab7e5e089ab72226431f1fb3cbe1))
- 13 tests for `analysis/advanced.rs`.
  ([4586c36](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/4586c36833e91ba63181fd57765a6ceac7d5abda))

#### TUI Snapshot and E2E Tests

- TUI snapshot coverage for Attention, FlowMatrix, LabelDashboard, Sprint,
  Tree, Actionable, and TimeTravelDiff views.
  ([8042884](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/804288461bc3ef0cf4b1616ae60500312254ba7c),
   [e026ac8](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/e026ac8369e2b7338edd1630d3ff59b97165c599))
- E2E TUI journey tests with artifact capture.
  ([bac40bb](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/bac40bb8261b659a1d05ab9330ca3875ec99b1ae))
- E2E robot matrix test coverage.
  ([442db34](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/442db3464023f02ac14cdf2834920aadd59d91ca))

#### CLI Flags

- Add `--no-cache`, `--db`, `--baseline-info`, `--check-drift`, and
  `--related-include-closed` flags.
  ([544b342](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/544b342bd3b052e9df052be93f43a92cbd951e4e))

#### Triage and Robot Fixes

- Restore default closed-bead exclusion for `--robot-related` and add
  regression test.
  ([a27e20a](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a27e20a1c474a0d8b33d85fd7c22310b542a9a97))
- Use full actionable set for recipe filters instead of `top_picks` subset.
  ([662ba81](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/662ba8175efe3dd260c17fb57560cedebcf00f7a))
- Run export hooks from project dir and resolve paths to absolute.
  ([d91f90d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d91f90d57ec02e6f91f4fc72d50a880c01645c5d))

#### Pages Wizard

- Fix saved deploy config validation to catch missing target fields.
  ([d69109f](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d69109f44f92daa8bde1e7aee80313057e240423))
- Fix symlink escape and recursive loop handling in preview server.
  ([247a6c3](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/247a6c317f48601c9fc296284e55a69344bef2ab))
- Repair step for saved config validation jumps to correct wizard step.
  ([1d5e930](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/1d5e930adf73680323ca36f9f372619d5b902250))

#### TUI View Expansion

- Expand TUI with interactive navigation and view switching.
  ([0010cd8](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0010cd8b65215f6f1e5219f513c89c9c2f35954a))
- Add keyboard shortcut help overlay.
  ([b388019](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b388019cf7f7d059891fde731b243a51eab1d4dc))
- Extend TUI with additional view modes across multiple iterations.
  ([c1c2834](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c1c283463257f37d5f13844eb18cc7196dca0ac8),
   [16a4114](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/16a411432d472b1669a6e04b1931323d29e2d05e))
- Expand TUI interactive views, loader coverage, and workspace history tests.
  ([fabeb35](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/fabeb35a7b0a7d424651e0af29b187cf1bf1b088),
   [c74394b](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c74394bf9303cea71076ddebc720e91a257af477))

#### Robot Output

- Expand robot output mode with comprehensive analysis views.
  ([1807475](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/180747586cfc62f613db51a4a0ac8b394d5be1bd))
- Expand CLI subcommands, file intel, and add export/validation tests.
  ([8c62318](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/8c62318c1f07b716941477e0095ec9ceae6833cb))

#### Documentation

- Refresh project documentation and expand CLI validation tests.
  ([d67e3ea](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/d67e3ea67c01ce4ee5cec7d85b126a23921a0da2))
- Update FEATURE_PARITY.md and README.md test counts.
  ([cd5bd67](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/cd5bd679182d1f0164e30a8f43e11d2ce296a666),
   [cff03a9](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/cff03a9ac5fbbd289a34e60b3b34ae71195c0c61))

---

### Phase 4 -- Typed Timestamps, SQLite Export, FlowMatrix, and TUI Expansion (2026-03-08 to 2026-03-11)

Foundational refactors (typed timestamps, two-phase graph analysis) and major
new surfaces (SQLite export, FlowMatrix view, pages wizard). This phase also
introduced mouse support and several new TUI view modes.

#### Typed Timestamp Migration

All timestamp fields migrated from `Option<String>` to
`Option<DateTime<Utc>>`, then every analysis, TUI, export, and CLI layer
adapted to the new types. This eliminated a class of bugs around inconsistent
date parsing and comparison.

- Migrate timestamp fields in the data model.
  ([10f8235](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/10f823549f408686d9f7db7ffe6e20a355c82bbd))
- Adapt all analysis modules to typed timestamps.
  ([a4c9702](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a4c9702de28876f509e02e36da1106d4c62b5d9e))
- Adapt UI, export, and CLI layers to typed timestamps.
  ([6368140](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/636814068cae2f1d65120658fdb78557efecb858))

#### Data Model Enhancements

- Add `content_hash` and `external_ref` fields; split closed vs tombstone
  status predicates.
  ([af51b70](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/af51b70729933677f72e2eb3c8d1b2f4f292477b))

#### Advanced Graph Analysis

- Add what-if simulation, metrics caching, and transparent 8-component impact
  scoring to the analysis engine.
  ([0050cb8](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0050cb82d8ff2526b9f92f4331b1c7caec48084b))
- Two-phase graph analysis: instant Phase 1 metrics, async Phase 2 with
  500ms timeout for expensive computations like betweenness.
  ([b97f37e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b97f37e1d6231952811fa7e57028f75ffd3df426))
- Add `AnalysisConfig::triage_runtime()` profile with tests and benchmarks.
  ([a402697](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a402697e2df65c2fe2432a300a751ba2a0aa81ff))
- Introduce `TriageLookupCache` to avoid repeated graph traversals.
  ([93a1659](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/93a16598d30e491c18730f0a6e12027224eb1340))
- Cap k-paths DFS enumeration and include missing config flags in cache key.
  ([56a65ab](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/56a65ab9cbbc22838f9efc3f981a90da148bedad))

#### SQLite Export

Full SQLite database population for pages bundles, enabling offline querying
of issues, dependencies, comments, metrics, and triage results.

- Implement full SQLite database population with issues, deps, comments,
  metrics, triage, and chunked bundling.
  ([eeb06c6](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/eeb06c6d1036c72685806556d2462a36b573215a))
- Integrate SQLite population into pages bundles with legacy contract parity.
  ([f9978b7](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/f9978b77a0b34529d8c7f9c4573ea18b1b618bcc))
- Normalize timestamp format to Z suffix for consistent data hashes; preserve
  sub-second precision in SQLite export.
  ([2bdfb50](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/2bdfb50f6c9f800fb6f9da43cff6fc11c228e08b),
   [c7eab3c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c7eab3c48b85b4c168ccf22d68d545f6cc61dd27))

#### Viewer Assets

- Add `viewer_assets` module and canonical offline asset inventory for
  self-contained pages bundles.
  ([28d1efe](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/28d1efe725b1a4579872f4be666fe0996577eb9e))

#### TUI -- New View Modes and Interactions

- Add 4 new view modes, mouse support, search modes, priority hints, and
  action shortcuts.
  ([613e1da](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/613e1da98843fa7fdf19808dc7eb4c97a744903c))
- Scaffold and implement interactive FlowMatrix view (`]` key) with
  `j`/`k`/`h`/`l` navigation for cross-label dependency flow.
  ([4ed2150](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/4ed2150907b8b4f682a338b3c07328dd07501151),
   [c65d41e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c65d41e23741cbf8dadc1819afd8aeaa9c194462),
   [b8313c1](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b8313c19c934b09f1df72c91c3e83c6d1e289403))
- Add insights heatmap, board detail scrolling, and rich detail panel with
  history/comments.
  ([78bd2d5](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/78bd2d54390d5400ded820b59f81f8029ee021bc))
- Add time-travel and sprint view modes.
  ([63e3126](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/63e31267c2ee9ae276149b165e309f68a5553a9f))
- History compact timeline with legacy lifecycle parity.
  ([470eef2](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/470eef27e62b9b80b2040a0be7bec1ab3213f48f))
- Implement search in Main mode with `/` key.
  ([9137ae6](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/9137ae6c1d14ebc0b6cda637f9c7ac2721cef02c))
- Add list pane scroll tracking so cursor stays visible.
  ([0898a08](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0898a08329cf944703d7db0f114fbd85d1a8fb63))
- Add insta snapshots for new TUI screens.
  ([aa26167](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/aa261671d236de4bd68e59ee51447a6391b8c3b0))
- Expand TUI rendering and sync beads issue tracker history.
  ([f687b86](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/f687b86d1aa497b0ad2a934dfd6c64e109c1da6c))
- Add missing `T` and `[` keybindings to help overlay views section.
  ([ff2ad20](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/ff2ad2076744a0d14745b930e38266079a875b06))

#### Pages Wizard

- Wizard transcript tracing, `IssueGraph` Vec storage, pages wizard
  expansion, history view enhancements.
  ([0cdeb1b](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0cdeb1be91712eeb4a9dab8f74d041f4141fb111))

#### Robot Output

- Consolidate robot envelope; expand output with what-if, advanced, and
  full-stats payloads.
  ([f78067e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/f78067eeed0c51c60bce69d2f88811a935c39c5b))
- Add related-work relevance threshold, rename limit param, and surface
  feedback stats in triage output.
  ([1fd404b](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/1fd404b826d60c74147554c89a5042aaa03fe6c2))
- Exclude `in_progress` beads from `robot-next` recommendations.
  ([fc9ed6f](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/fc9ed6f402ff46ec7ac1b349114a8102ecc2bbc7))

#### Security

- Prevent XSS via HTML entity escaping in pages index title.
  ([2a3495d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/2a3495d0b5badcc8b062782989c219a914eebd7c))

#### Bug Fixes

- Resolve 4 correctness issues in TUI, export, cache, and graph.
  ([b7ae88a](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/b7ae88a2fdb8f38b0f0552ab58d066870290364f))
- Reuse betweenness scratch buffers to reduce allocations.
  ([c65d41e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c65d41e23741cbf8dadc1819afd8aeaa9c194462))

---

### Phase 3 -- TOON Output, CI Pipeline, and Full CLI Surface (2026-03-04 to 2026-03-07)

All 51 previously-missing CLI surfaces implemented in a single phase. Real
TOON output mode via `tru` binary integration. GitHub Actions CI pipeline.
Comprehensive benchmarks. This phase brought the robot surface to full parity
with the legacy Go `bv`.

#### Robot Commands -- Full CLI Surface Parity

Every legacy robot command now has a Rust implementation. The following were
added in this phase:

- Label intelligence, correlation audit, file intel, and schema validation.
  ([8cf6aab](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/8cf6aab931183fe7cc7ef632c7b60f3f1b036dd0))
- Impact, file-relations, and related-work exploration surfaces.
  ([44ca8c7](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/44ca8c78390a4009551cd118b50ea97bbe197b28))
- Blocker-chain, impact-network, and causality commands.
  ([1736224](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/173622495da0911e20dd54c4d56f7124836d1f64))
- Drift baseline save/load and `--robot-drift` detection.
  ([956d3fb](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/956d3fb20ce211086fc87834c5ae5f945c481332))
- Search / `robot-search` with hybrid scoring and ranking presets.
  ([37b56f9](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/37b56f98a8d612f96332b57c820fcacf1187728d))
- Recipe-based triage filtering.
  ([a0bf279](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a0bf2797d69fb01668eb6c65f3d7c88f232cf4e3))
- All 51 previously-missing CLI surfaces now implemented (parity milestone).
  ([fadff2c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/fadff2c2249e8fd0a40d66b0eebdf130e9fd2fbf))

#### Export and CLI

- Add export-pages, brief generation, background mode, recipe/script/feedback,
  and admin CLI.
  ([27f4b34](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/27f4b34c37cea55379c22c83ba479da09f9d21bd))

#### TOON Output Mode

- Implement real TOON output via `tru` binary integration, giving agents a
  compact, token-efficient alternative to JSON.
  ([ab1525b](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/ab1525b86a24b246e6242e8624fb88281fb904e7))

#### TUI

- Add visual tokens, breakpoint layout, and interaction parity with legacy.
  ([e0cc902](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/e0cc90202b9055ba029d427d0de1259807f02bdd),
   [08beebd](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/08beebd3f80337c916b7db964bad2cc962bc0d24))
- Add AGENTS.md blurb management and TUI improvements.
  ([a3214a9](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/a3214a97e3e6e2d71e18202e2adc2c52701ced77))

#### CI Pipeline

- Add GitHub Actions CI workflow (`ci.yml`): check, clippy, unit tests,
  conformance tests, E2E tests, stress tests.
  ([f6e5fa3](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/f6e5fa36a5991e75bc21f40468ac3f1407f1ecc3))

#### Bug Fixes

- Propagate blocked status through parent-child deps in triage
  (PR [#1](https://github.com/Dicklesworthstone/beads_viewer_rust/pull/1)).
  ([c0d8f0d](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c0d8f0d0ada2822ca6d056fa879469f7af0c66ec))
- Report all SCC members in cycle detection, not just minimal path.
  ([4b15696](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/4b1659621d9c1ed04789ac97f2c3ff2aefa0f53a))
- Make dependency direction deterministic with 3-level tiebreaking in suggest.
  ([271d6bc](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/271d6bccdd7264c05fa6a0002cb6def56f61362a))
- Auto-enable `--robot-diff` for piped output with `--diff-since`.
  ([5aeaca3](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/5aeaca3e24973d8ee8f66ef8426e47c91308b728))
- Resolve 5 conformance test failures and add missing e2e/stress test targets.
  ([5a8ae8c](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/5a8ae8cdbf6116699a0f66360cebc6d49fd3fb9c))
- Expand open-like status recognition and fix hardcoded recency timestamp.
  ([8e8e0af](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/8e8e0af909976cfffc23a5a9b42922bd78e90e26))

---

### Phase 2 -- Core Parity, Workspace Support, and Robot Surface Expansion (2026-02-27)

A single intensive day of development covering workspace loading, sprint and
metrics commands, graph-panel redesign, and the robot-capacity/alerts
subcommands. This phase also introduced stress-test fixtures and the
conformance test expansion that became the foundation for later parity work.

#### Robot Commands

- Add `robot-capacity` and `robot-alerts` subcommands with full
  implementation.
  ([0332c96](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0332c96daa28059e35b6a1ab90782f8f727c906b))

#### Workspace Support

- Add workspace support (`.bv/workspace.yaml`), markdown export,
  sprint/metrics robot commands, and historical revision loading.
  ([c0bff04](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/c0bff049149a6dd73bfd474412196fc0c03c8171))

#### TUI

- Add graph/insights search, detail-pane dependency navigation, burndown
  conformance, and output envelope metadata.
  ([9375676](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/93756764c7c1d5a2c1906c250d9bf9b4620a46e5))
- Redesign graph metrics panel and overhaul history view.
  ([7107903](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/7107903808a3ad1bc8255c228de715540b878a78))

#### Legacy Parity

- Improvements for history, suggest, and TUI parity with Go `bv`.
  ([1b9f202](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/1b9f202eb2c7ddc3aad2f67ffbb81f33a52b4bc7))

#### Tests

- Edge-case conformance fixtures and boundary-condition tests.
  ([551b54e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/551b54e6a0ab4d54a37d312efb9ee6b3b6afb13a))
- Expand Go reference harness and add workspace/sprint/export conformance
  tests.
  ([0bf9bc5](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/0bf9bc5ea22c0b8c6ab0fe487cbad75826c88cf4))
- Add stress-test fixture.
  ([7107903](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/7107903808a3ad1bc8255c228de715540b878a78))

---

### Phase 1 -- Repository Initialization (2026-02-18 to 2026-02-25)

The project was bootstrapped as a Rust port of the legacy Go `bv` tool.
The initial commit already contained 26,278 lines across a substantial
codebase -- this was not a from-zero start but a structured port with an
already-functional core.

#### Data Model and Loader

- Full data model (`model.rs`) for beads issues with status, priority, type,
  labels, dependencies, comments, and assignees.
- Loader (`loader.rs`) for `.beads/` directories with compatibility filename
  detection (`beads.jsonl`, `issues.jsonl`, `beads.base.jsonl`).

#### Analysis Engine

- Graph construction and centrality: PageRank, betweenness, HITS, eigenvector
  centrality, k-core decomposition.
- Cycle detection via strongly connected components.
- Critical path / critical depth computation and slack analysis.
- Articulation point detection for structural failure points.
- Alert generation for stale/blocking cascades.
- Forecast module for ETA predictions.
- Diff module for baseline snapshot comparison.
- Suggest module for dependency and hygiene suggestions.
- Plan module for parallel execution track grouping.
- Git history correlation (`git_history.rs`) and issue lifecycle history.
- Label intelligence.

#### Robot Output

- Structured JSON robot output mode (`robot.rs`) for agent and script
  consumption.

#### Interactive TUI

- FrankenTUI (`tui.rs`, 3,654 lines at init) with main issue list/detail
  view built on the `ftui` crate.

#### Testing Infrastructure

- Conformance test suite (1,177 lines) with Go reference harness and
  fixtures.
- Integration tests for robot-alerts, robot-burndown, and robot-history.
- Triage benchmarks (`benches/triage.rs`).
- Test data: `minimal.jsonl`, `adversarial_parity.jsonl`,
  `synthetic_complex.jsonl`.

#### Infrastructure

- Switch `asupersync` and `ftui` dependencies from local paths to crates.io.
  ([9a8299e](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/9a8299e0db996deebdeb920e8f59879167c300b6))
- MIT license.

([213bfee](https://github.com/Dicklesworthstone/beads_viewer_rust/commit/213bfee690b91e6a8d738c4a9cf80a6ba9ccf42c))

---

## Architecture Overview

```
src/
  main.rs            CLI entry point and command dispatch (238 KB)
  tui.rs             FrankenTUI with 12 view modes (737 KB)
  model.rs           Issue data model with typed timestamps
  loader.rs          .beads/ and workspace.yaml loading (70 KB)
  robot.rs           Robot JSON/TOON output formatting
  cli.rs             Clap CLI argument definitions
  agents.rs          AGENTS.md blurb management
  export_pages.rs    Static HTML/JS pages bundle export
  export_sqlite.rs   SQLite database population for pages
  export_md.rs       Markdown export
  pages_wizard.rs    Interactive deploy config wizard
  viewer_assets.rs   Vendored offline asset inventory
  error.rs           Error types
  lib.rs             Library re-exports
  analysis/
    triage.rs        Core triage scoring and ranking
    graph.rs         Dependency graph and centrality metrics
    advanced.rs      What-if simulation, impact scoring
    suggest.rs       Dependency and hygiene suggestions
    diff.rs          Baseline diff detection
    drift.rs         Configuration drift detection
    forecast.rs      ETA predictions
    alerts.rs        Stale/blocking cascade alerts
    causal.rs        Causality and blocker-chain analysis
    search.rs        Hybrid text/metadata search
    plan.rs          Parallel execution plan generation
    brief.rs         Markdown brief generation
    recipe.rs        Pre-built triage filter recipes
    cache.rs         Analysis metrics caching
    correlation.rs   Label/metric correlation audit
    file_intel.rs    File-level intelligence
    label_intel.rs   Label-level intelligence
    git_history.rs   Git commit correlation
    history.rs       Issue lifecycle history
    whatif.rs        What-if scenario simulation
    mod.rs           Module re-exports and AnalysisConfig
```

Total source: ~64,000 lines of Rust across 32 source files.

Binary name: `bvr`
