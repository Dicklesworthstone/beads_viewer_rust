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

**Pending for this upgrade:** rebuilding and checking the new actual exported bundle; running the new mutation scenario and existing viewer/export regressions; a library-only comparison; and same-host latency/memory comparisons against measured A/A noise. The execution environment became unavailable during preparation. Earlier #32/#30 browser passes used the previous DOMPurify asset and do not validate this upgrade. Do not treat this focused source change as satisfying all of #33's acceptance criteria.
