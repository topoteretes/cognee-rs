# Vendored visualization frontend

Everything in this directory except `vendor/` is **vendored verbatim** from the
upstream Python [`cognee`](https://github.com/topoteretes/cognee) repository
(branch `dev`). Not a port, not an adaptation — a byte-for-byte copy.
`vendor/` holds the d3 release the offline transform inlines (see below).

- **Upstream commit:** `685de12d2` (`685de12d2c8cd9bd2e56be92095dbf4347c8f191`, `origin/dev`)

| Vendored file            | Upstream source path                                       |
| ------------------------ | ---------------------------------------------------------- |
| `template.html`          | `cognee/modules/visualization/template.html`               |
| `views/ui_chrome.js`     | `cognee/modules/visualization/views/ui_chrome.js`          |
| `views/schema_view.js`   | `cognee/modules/visualization/views/schema_view.js`        |
| `views/story_view.js`    | `cognee/modules/visualization/views/story_view.js`         |
| `views/inspector.js`     | `cognee/modules/visualization/views/inspector.js`          |
| `views/memory_map.js`    | `cognee/modules/visualization/views/memory_map.js`         |
| `views/semantic_map.js`  | `cognee/modules/visualization/views/semantic_map.js`       |

## Rule: re-sync wholesale, never hand-edit

**Do not hand-edit any file in this directory.** If the frontend needs to
change, change it upstream in the Python repo first, then re-copy the whole set
here and bump the commit recorded above.

This rule exists because it was already broken once. Rust used to carry
`assets/graph_template.html`, a 1680-line hand-forked copy of Python's
pre-refactor monolith. It was edited in place, drifted silently, and ended up
several features behind: two tabs instead of four, no Memory or Semantic view,
no Story/Flow/Force layout switch, no schema inspector, and a Schema tab that
could never render because `schema_data` was hardcoded to `None` on the Rust
side. Hand-editing is exactly how that happened, so the resync rule is the fix.

Re-sync procedure:

```bash
git clone --depth 1 --branch dev https://github.com/topoteretes/cognee.git /tmp/cognee-python
SRC=/tmp/cognee-python/cognee/modules/visualization
DST=crates/visualization/assets
cp "$SRC/template.html" "$DST/template.html"
for f in ui_chrome schema_view story_view inspector memory_map semantic_map; do
  cp "$SRC/views/$f.js" "$DST/views/$f.js"
done
git -C /tmp/cognee-python rev-parse HEAD   # record this as the upstream commit above
cargo test -p cognee-visualization
```

Then check, before committing:

1. **The offline transform still applies.** `src/html.rs` rewrites three exact
   tag patterns in `template.html` (the d3 CDN `<script>` and the three Google
   Fonts `<link>` lines). If upstream changed any of them, rendering fails with
   `VisualizationError::OfflineTransform` and the unit test
   `offline_patterns_occur_exactly_once_in_the_vendored_template` fails. Update
   the patterns (`D3_CDN_TAG`, `GOOGLE_FONTS_LINES`) — never the vendored file.
2. **Nothing new loads from the network.** `tests/offline_test.rs` fails if the
   new files introduce an external URL, `fetch`, `<link href>`, etc. If upstream
   bumped the d3 major/minor version it loads, bump `vendor/` too (below).
3. **The preprocessor still matches.** Diff the Python side
   (`git diff <old> <new> -- cognee/modules/visualization`) and port changes to
   `preprocessor.py` / `operations_catalog.py` into `../src/preprocessor/`.

`cargo test -p cognee-visualization` is enough on its own: the crate declares
every feature its output ordering depends on (notably `serde_json/preserve_order`)
in its own `Cargo.toml`, so the `-p` build renders the same bytes as the
workspace binary. It used to inherit `preserve_order` from `cognee-database` by
feature unification, which made the `-p` build emit a different Schema tab than
the shipped one.

The orchestrator that stitches these together — the `__TOKEN__` substitutions —
lives in [`../src/html.rs`](../src/html.rs) and is the *only* Rust code that
should know about these filenames. `crates/visualization/tests/html_test.rs`
asserts that every token is resolved and that each chunk's distinctive
identifiers survived the copy, so a bad or partial resync fails the test suite
rather than silently shipping a broken page.

## Token slots

`template.html` and the `views/*.js` chunks between them declare 20
`__TOKEN__` placeholders. The full list, the substitution order (JS chunks
first, data payloads second) and the rationale are documented in
[`../src/html.rs`](../src/html.rs) and
[`../README.md`](../README.md).

Two of the tokens are *not* filled from a vendored file:

- `__PIPELINE_LAYOUT_JS__` → the empty string. Upstream
  `layouts/pipeline_layout.py:16-17` is a deliberate stub whose `emit_js()`
  returns `""`. The slot is kept so a future Phase 1d layout drops in without a
  template change.
- `__SEMANTIC_LAYOUT_JS__` → the single line
  `window._semanticPositions = __SEMANTIC_POSITIONS__;`, matching upstream
  `layouts/semantic_layout.py:200-206`. This is why the layout chunk must be
  substituted *before* the data tokens.

## Offline transform: the one deliberate divergence

The vendored `template.html` loads d3 v7 from `https://d3js.org` and the Inter
webfont from `https://fonts.googleapis.com` / `https://fonts.gstatic.com`, as
Python does. Every HTML this crate produces must work with no network, so
`../src/html.rs` rewrites the shell at render time (the file on disk stays
byte-identical to upstream):

- `<script src="https://d3js.org/d3.v7.min.js"></script>` → `<script>` +
  `vendor/d3.v7.9.0.min.js` + `</script>`;
- the three Google Fonts `<link>` lines are deleted; the CSS falls back to a
  local Inter, then `system-ui`.

Each pattern must match exactly once, or rendering returns
`VisualizationError::OfflineTransform`. `../tests/offline_test.rs` pins the
result. See [`../README.md`](../README.md#fully-offline-output).

## Bundled d3 (`vendor/`)

| File | What |
| --- | --- |
| `vendor/d3.v7.9.0.min.js` | d3 **7.9.0**, the official `dist/d3.min.js`, unmodified |
| `vendor/LICENSE-d3` | d3's ISC license (Copyright 2010-2023 Mike Bostock) |

- **Source:** `https://cdn.jsdelivr.net/npm/d3@7.9.0/dist/d3.min.js`
  (byte-identical to `https://cdnjs.cloudflare.com/ajax/libs/d3/7.9.0/d3.min.js`).
- **Size:** 279,706 bytes.
- **SHA-256:** `f2094bbf6141b359722c4fe454eb6c4b0f0e42cc10cc7af921fc158fceb86539`
  (asserted by `../tests/offline_test.rs`).
- Contains no `</script` (so it can be inlined verbatim; `build_html` refuses
  one that does). Its only URLs are the `https://d3js.org` banner comment and
  five w3.org XML namespace identifiers, none of which is fetched.

The template's `d3.v7.min.js` is "latest v7"; 7.9.0 is the current v7 release.
To bump: download the new `dist/d3.min.js`, verify it on two CDNs, replace the
file (renaming it and the `include_str!` in `../src/html.rs`), and update the
version, size and SHA here and in `../tests/offline_test.rs`.
