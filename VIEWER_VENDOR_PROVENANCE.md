# Exported viewer dependency provenance

This record covers the focused DOMPurify update for [issue #33](https://github.com/Dicklesworthstone/beads_viewer_rust/issues/33). It does not establish the provenance or advisory status of the other vendored viewer assets. Keep the broader issue open until its remaining inventory, browser, and performance acceptance work is complete.

## DOMPurify 3.4.16

The official stable release was checked on **2026-10-10**. The vendored file is the exact upstream distribution file; it has not been rebuilt, reformatted, or modified.

| Item | Recorded value |
| --- | --- |
| Repository path | `viewer_assets/vendor/dompurify.min.js` |
| Upstream project | [cure53/DOMPurify](https://github.com/cure53/DOMPurify) |
| Release | [3.4.16](https://github.com/cure53/DOMPurify/releases/tag/3.4.16), published 2026-09-23T15:23:58Z; neither draft nor prerelease |
| Annotated tag object | `7858197e2a666b21fd482773fd0157fff059a537` |
| Tag target commit | `b9b9d80f7e401771c2ccaef5f45def7eec8f27d7` |
| Exact file origin | [dist/purify.min.js at the pinned commit](https://raw.githubusercontent.com/cure53/DOMPurify/b9b9d80f7e401771c2ccaef5f45def7eec8f27d7/dist/purify.min.js) |
| File bytes | 28,885 |
| File SHA-256 | `2c90a9b46d6463f26038a29b686e82bc91de01fdac9d5229e7cfe3b360134ea2` |
| Git blob object | `00742c47118a703f414f6a116ddff07965d8e59e` |
| License | MPL-2.0 OR Apache-2.0; preserve the upstream license header and [license terms](https://github.com/cure53/DOMPurify/blob/b9b9d80f7e401771c2ccaef5f45def7eec8f27d7/LICENSE) |

GitHub reports the annotated tag's signature as verified with reason `valid` in the [tag object API](https://api.github.com/repos/cure53/DOMPurify/git/tags/7858197e2a666b21fd482773fd0157fff059a537). This is GitHub's verification result, not a claim of separate local GPG verification.

### Published package corroboration

The [npm version metadata](https://registry.npmjs.org/dompurify/3.4.16) identifies the same Git commit. The published tarball was read and hashed without installing a package or introducing a package-manager project.

| Item | Recorded value |
| --- | --- |
| Tarball | [dompurify-3.4.16.tgz](https://registry.npmjs.org/dompurify/-/dompurify-3.4.16.tgz) |
| Tarball bytes | 331,885 |
| Tarball SHA-1 | `c51b3709ea15ee9abb07e804f2ef30ea9b953128`, matching the registry shasum |
| Tarball SHA-256 | `d288ced25e6bda3e5fc76d1fc9bca899260cf886e005e84f232e686ada57f1b4` |
| Registry integrity | `sha512-sqo+pNp3qRhCIpbgRi1y8Tgk27Bo2Ry7w0dC1NBeNTdZChWjz9Xb/KOoZbRP/R6pQZ80Qw8YhXw13hWWBbMRnQ==`, matching the downloaded tarball |
| Package member | `package/dist/purify.min.js` |
| Member identity | 28,885 bytes; SHA-256 `2c90a9b46d6463f26038a29b686e82bc91de01fdac9d5229e7cfe3b360134ea2`; identical to the pinned upstream file |

The previous vendored file advertised 3.0.6, measured 20,931 bytes, and had SHA-256 `ea4b09082ca4ba0ae71be6431a097678751d0453b9c52a4d2c7c39a2166ed9fc`. That is the identity of the repository's old file, not a completed comparison against the upstream 3.0.6 package. The new file adds 7,954 bytes. This size difference is not a latency or memory measurement.

## Rendering context and advisory scope

The shared helpers call `DOMPurify.sanitize(marked.parse(source))` or `DOMPurify.sanitize(marked.parseInline(source))` with default sanitizer options. Missing, unsupported, incomplete, or throwing libraries cause the original source to be escaped as literal text. The issue and graph detail panels assign the sanitized string to ordinary `div` elements through Alpine `x-html`; the issue-list excerpt uses a `p` element.

The inspected helper does not enable `SAFE_FOR_TEMPLATES`, `IN_PLACE`, `RETURN_DOM`, custom allowlists, hooks, or cross-realm DOM inputs. It does not intentionally wrap sanitized output in raw-text elements. Version-range correlation alone does not establish that a configuration-dependent advisory is exploitable in these paths.

| Upstream advisory | Relevance to this focused change |
| --- | --- |
| [GHSA-gx9m-whjm-85jf / CVE-2024-47875](https://github.com/cure53/DOMPurify/security/advisories/GHSA-gx9m-whjm-85jf) | Nesting-based mutation XSS affects the old release range. Default string sanitization followed by HTML insertion warrants regression coverage; actual execution must be established in the browser rather than inferred from the version. |
| [GHSA-mmhx-hmjr-r674 / CVE-2024-45801](https://github.com/cure53/DOMPurify/security/advisories/GHSA-mmhx-hmjr-r674) | Special nesting can bypass depth checks, and prototype pollution can also undermine them. It would be inaccurate to classify the entire advisory as requiring prior prototype pollution. |
| [GHSA-vhxf-7vqr-mrjg / CVE-2025-26791](https://github.com/cure53/DOMPurify/security/advisories/GHSA-vhxf-7vqr-mrjg) | Its `SAFE_FOR_TEMPLATES: true` prerequisite is absent from the inspected helper. The upgrade does not by itself demonstrate this advisory in the viewer. |
| [GHSA-h8r8-wccr-v5f2 / CVE-2026-3126](https://github.com/cure53/DOMPurify/security/advisories/GHSA-h8r8-wccr-v5f2) | Its raw-text recontextualization prerequisite is not established by the ordinary detail-panel insertion. Payloads containing raw-text tags are still useful default-sanitizer regressions, without asserting this separate wrapper-based attack path. |
| [GHSA-87xg-pxx2-7hvx / CVE-2026-47423](https://github.com/cure53/DOMPurify/security/advisories/GHSA-87xg-pxx2-7hvx) | The bounded `selectedcontent` fixture pins the current default content-removal contract. This advisory's reported affected release is 3.4.4; including its structural regression does not label the old 3.0.6 asset affected by that advisory. |

Other advisories involving nondefault options, DOM-object input, hooks, or prior prototype pollution need their own prerequisite analysis. This document is not a complete advisory inventory and does not resolve the other assets listed in #33.

## Browser regression corpus

The corpus in `scripts/test_viewer_search.cjs` is pinned to [upstream commit b9b9d80](https://github.com/cure53/DOMPurify/tree/b9b9d80f7e401771c2ccaef5f45def7eec8f27d7):

- From [test/fixtures/expect.mjs](https://github.com/cure53/DOMPurify/blob/b9b9d80f7e401771c2ccaef5f45def7eec8f27d7/test/fixtures/expect.mjs): all five nesting-based cases, attribute-based cases 2/3 and 3/3, both removal-based cases, and both fake-element namespace-confusion cases. Their payload strings and accepted direct outputs are preserved.
- From [test/test-suite.js](https://github.com/cure53/DOMPurify/blob/b9b9d80f7e401771c2ccaef5f45def7eec8f27d7/test/test-suite.js): declarative template image insertion; a marker inside MathML; chained templates with 96 nested `b` elements per link and 4, 6, or 8 links; the default declarative-shadow-template case; and the bounded, outside-select `selectedcontent` content-drop case. These use default `sanitize`, without enabling the upstream suite's separate configuration-specific variants.

The historical nesting fix used exact depth-truncation expectations for an older implementation. Those old numeric output expectations are not imposed on the current sanitizer. The current upstream chain construction supplies deep-tree security coverage instead.

The new scenario is named:

```text
security: exported DOMPurify neutralizes upstream mutation cases offline
```

It requires a freshly built exporter and creates a real `--export-pages` dashboard from 18 upstream cases plus ordinary Markdown and empty-description controls. The harness verifies emitted `index.html`, `viewer.js`, `graph.js`, and `vendor/dompurify.min.js` byte-for-byte against the checkout before serving the export. A stale binary cannot silently validate an older vendored sanitizer.

Each mutation case first passes through the real browser's default `DOMPurify.sanitize` and ordinary live-div insertion. This direct check prevents a false pass caused solely by Marked escaping a particular raw-HTML shape. The exported excerpt, issue modal, and graph detail panel are then exercised through their actual application code. The DOM inspection walks template contents and open shadow roots as well as normal descendants. Connected images are allowed to load or fail; surviving unsafe links and interaction payloads are exercised; executable attributes, script URLs, and active embedded elements must be absent.

Upstream `alert()` payloads remain unchanged and actual browser dialogs are observed and dismissed. Existing #32 payload counters remain active. Requests to nonlocal HTTP(S) origins are blocked and recorded, and the scenario requires no such requests. This checks a locally served offline export without replacing the DOM, parser, sanitizer, or application renderer.

The export bundle, input JSONL, exporter output, and `mutation-regressions.json` are retained under `BVR_VIEWER_ARTIFACT_DIR` (or a temporary directory by default). The JSON records the library version actually loaded, any asset revision overrides, completed case/surface checks, execution counters, dialogs, and external requests.

## Reproduction and validation status

Use an existing supported Playwright/Chromium runtime. No repository npm manifest or additional dependency is required.

```sh
cargo build --locked
NODE_PATH=/path/to/node_modules \
BVR_CHROMIUM_EXECUTABLE=/path/to/chrome-headless-shell \
BVR_BIN="$PWD/target/debug/bvr" \
BVR_VIEWER_ARTIFACT_DIR="$PWD/target/bvr-viewer-artifacts" \
node --test --test-name-pattern='security:' scripts/test_viewer_search.cjs
```

For a library-only comparison, set `BVR_DOMPURIFY_REVISION=1dffe3ea56ffc67d9c6e3a59bd89959f1dc08dc9` and run only the new mutation scenario. That Git revision must exist locally. This serves the previous library bytes without modifying the exported files; application code and database remain current. The fresh export is still checked against the checkout before the serving override. The distinct `BVR_VIEWER_REVISION` option remains available for the original #32 application-code negative control. Do not combine the two when attributing a result to a single change.

A comparison failure is meaningful only after the application initializes and a concrete output or inertness assertion fails. A changed structural output does not prove script execution; dialogs or execution counters must be reported separately. Infrastructure failures are not security evidence.

**Verified during preparation:** official release/tag identity, the pinned file's byte count and SHA-256, and equality with the published npm package member, including tarball digest verification.

**Browser verification on 2026-10-10:** after rebuilding commit `522ccbe51ac64e01d22a8bd362bcde05ed62cf94`, all 11 security scenarios passed in Chromium 151.0.7922.34 with Playwright 1.62.1 and Node 24.19.0 (180.8 seconds). The exporter preserved the fixture descriptions, and emitted assets matched the checkout byte-for-byte. DOMPurify reported version 3.4.16 in the running exported dashboard. All 18 direct default-sanitize/live-div checks and all 60 exported excerpt, issue-detail, and graph-detail checks completed; execution counters, dialogs, and external-request records were empty. The other scenarios covered ordinary Markdown, safe raw HTML, event handlers, Alpine directives, graph tooltips, and all nine available/missing/incomplete/unsupported/throwing-library conditions.

The same-host library-only comparison loaded 3.0.6 from `1dffe3ea56ffc67d9c6e3a59bd89959f1dc08dc9` while keeping application code and exported data current. It completed the first 14 direct cases, then failed the bounded `selectedcontent` output contract: 3.0.6 returned `<div><img src="x"><b>copy</b></div>`, whereas the pinned 3.4.16 fixture requires `<div></div>`. The old output had no executable attributes, dialogs, or payload-counter executions. This is a structural behavior difference, **not demonstrated JavaScript execution or proof that 3.0.6 is affected by the later `selectedcontent` advisory**. The fail-fast comparison did not reach the three template-chain cases or the exported-surface loop. The run logs and per-scenario JSON were retained under `target/browser-validation-20261010-HybjR0`.

**Full viewer qualification on the final viewer assets:** the complete 48-test invocation finished with **47 passes and one failure** in 506.5 seconds. All 28 general desktop/mobile viewer, graph, and history scenarios passed, as did all 16 security scenarios. The latter include five new real-export dependency-diagram regressions for distinct punctuation-bearing IDs, comma-containing dependency IDs, literal quoted/HTML-like IDs, pointer/keyboard navigation under Mermaid strict mode, and repeated graph opening after navigation. The mutation scenario again completed all 78 checks with DOMPurify 3.4.16, no asset revision overrides, and empty execution/dialog/external-request records.

The sole full-run failure was a 10-second search-result wait in the 10,002-issue missing-approximation scenario, after its initialization, graph-size, bounded-algorithm, and empty-ranking assertions had passed. That exact test then passed unchanged in isolation (10.7 seconds). The harness subsequently gained read-only route, app/input query, pending-search, and visible-row diagnostics; no result assertion, timeout, navigation action, or retry behavior changed. One bounded rerun of the four bottleneck scenarios passed **4/4** (22.3 seconds). Each real-export variant ended at `#/issues?q=09999` with one visible result, `fixture-09999`; the 10,002-node/6,668-edge graph made zero exact betweenness calls. The original timeout's cause remains unproven; these reruns do not turn the first invocation into a 48/48 pass. Retained logs are `viewer-full.log`, `bottleneck-missing-rerun.log`, and `bottleneck-group-rerun.log` under `target/browser-validation-20261010-HybjR0`.

Relevant Rust/export checks also passed: all 39 viewer-asset unit tests, both export integration targets (26 `export_pages` tests and 20 `e2e_export_pages` tests), and all 10 shell preview/export checks. The shell run used the final binary and retained its log at `target/validation-20261010/shell-preview-export.log`.

**Still pending for the broader upgrade review:** the unresolved vendor inventory and same-host latency/memory comparisons against measured A/A noise. The isolated full-run search timeout also remains recorded for follow-up if it recurs. Do not treat the focused DOMPurify update and its passing security scenarios as satisfying all of #33's acceptance criteria.
