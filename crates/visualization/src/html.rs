//! HTML shell assembly: token substitution over the vendored frontend.
//!
//! Ports `cognee_network_visualization()` from
//! [`cognee/modules/visualization/cognee_network_visualization.py:122-169`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py#L122-L169).
//!
//! The HTML shell (`assets/template.html`) and the six view chunks
//! (`assets/views/*.js`) are vendored byte-for-byte from the Python repo — see
//! `assets/README.md` for the resync rule. This module is the only Rust code
//! that knows their filenames.
//!
//! # Substitution order is load-bearing
//!
//! Python replaces the **eight JS-chunk tokens first**, then the twelve data
//! tokens. That order is not cosmetic: `__SCHEMA_GRAPH_DATA__` and
//! `__SCHEMA_DATA__` live *inside* `views/schema_view.js`, `__MEMORY_DATA__` /
//! `__SEARCH_EVENTS__` inside `views/memory_map.js`, the six graph payload
//! tokens inside `views/story_view.js`, and `__SEMANTIC_POSITIONS__` inside the
//! one line emitted for `__SEMANTIC_LAYOUT_JS__`. Substituting data first would
//! leave every one of those tokens unresolved in the output.
//!
//! # …but each phase is a single left-to-right pass
//!
//! Within a phase the substitution must **never re-scan what it just wrote**.
//! Python gets this for free because `str.replace` is applied to a fresh
//! template string per token and the payloads land in disjoint slots; a naive
//! Rust translation (twelve chained `String::replace` calls) does not: the
//! eleventh call happily rewrites a `__SEARCH_EVENTS__` that the first call
//! embedded *inside the JSON node data*. That is not hypothetical — a node whose
//! text quotes this crate's own `assets/README.md` carries the literal token
//! names, and rewriting them mid-JSON produced `expected ',' or '}'` and a blank
//! page in every tab.
//!
//! [`substitute_once`] therefore walks its input once, copying spans and
//! appending replacements into a pre-sized `String`. Injected content sits past
//! the cursor's write position and is never examined again, so a payload may
//! contain any token name verbatim. It also replaces 20 whole-document copies
//! with one (tens of MB per render on a large graph).
//!
//! Every data token is substituted **unconditionally with a JSON fallback**
//! (`null`, `{}`, `[]`). A leaked `__TOKEN__` is a hard failure — it renders as
//! literal garbage inside a `<script>` block and breaks the page — which is why
//! Python does it this way (`cognee_network_visualization.py:156-167`) and why
//! `tests/html_test.rs` scans the whole output for surviving tokens.
//!
//! # Deferred payloads (two tokens are structurally `null`)
//!
//! Two payloads are deliberately out of scope for this crate today. Both
//! are substituted with valid JSON so no token leaks, and the vendored JS
//! renders a friendly empty state for each — the same state Python reaches
//! whenever its own best-effort computation fails (Python wraps
//! `_semantic_payload` in a blanket `except Exception`,
//! `cognee_network_visualization.py:63-65`), so this is a legal,
//! upstream-test-passing configuration rather than a broken one.
//!
//! * `__SEMANTIC_POSITIONS__` → `null`. Filling it needs the stored embedding
//!   vector of every node plus a 2-D projection. Rust's
//!   `cognee_vector::SearchResult` has no `vector` field, so stored vectors
//!   cannot be read back out of the vector store at all, and the workspace has
//!   no SVD/PCA implementation.
//! * `__SEMANTIC_CLUSTERS__` → `null`. Needs the same embeddings plus k-means
//!   seeded by NumPy's PCG64 to match Python's cluster ids; neither the
//!   clustering nor a PCG64 generator exists in the workspace.
//!
//! `__SEARCH_EVENTS__` carries the caller's session events (Python's
//! `search_events`; `cognee_session::collect_session_events` builds them) and
//! is `[]` when none are given.
//!
//! With both `null`, `views/semantic_map.js` shows its empty state and
//! the Semantic tab is inert; the Graph, Schema and Memory tabs are fully
//! functional.
//!
//! # Offline transform (the one deliberate divergence from upstream)
//!
//! Every HTML this crate produces must work with no network. The vendored
//! `template.html` loads d3 from `https://d3js.org` and the Inter webfont from
//! Google Fonts, so before any token is substituted [`offline_template`]
//! rewrites the shell:
//!
//! * the exact tag [`D3_CDN_TAG`] becomes the `__COGNEE_INLINE_D3__` slot,
//!   filled in the JS-chunk phase with `<script>` + the bundled
//!   `assets/vendor/d3.v7.9.0.min.js` + `</script>`;
//! * the three exact Google Fonts `<link>` lines ([`GOOGLE_FONTS_LINES`]) are
//!   deleted. The template's CSS already falls back to a locally installed
//!   Inter and then `system-ui`.
//!
//! Each pattern must occur **exactly once** or rendering fails with
//! [`VisualizationError::OfflineTransform`]: a resync that changes those tags
//! then breaks loudly instead of silently re-enabling the CDN. The vendored
//! files themselves stay byte-identical to upstream.

use std::borrow::Cow;

use serde_json::json;

use crate::VisualizationError;
use crate::preprocessor::PreprocessedGraph;

/// The vendored HTML shell.
///
/// Byte-for-byte copy of `cognee/modules/visualization/template.html`. Carries
/// the eight JS-chunk token slots (`:488-501`) plus the four tab buttons.
pub const HTML_TEMPLATE: &str = include_str!("../assets/template.html");

/// Vendored `views/ui_chrome.js` — theme toggle + tab switching.
const UI_CHROME_JS: &str = include_str!("../assets/views/ui_chrome.js");
/// Vendored `views/schema_view.js` — the Schema tab's ontology diagram.
const SCHEMA_VIEW_JS: &str = include_str!("../assets/views/schema_view.js");
/// Vendored `views/story_view.js` — the main canvas graph renderer.
const STORY_VIEW_JS: &str = include_str!("../assets/views/story_view.js");
/// Vendored `views/inspector.js` — the schema type/instance side panel.
const INSPECTOR_JS: &str = include_str!("../assets/views/inspector.js");
/// Vendored `views/memory_map.js` — the Memory tab's deterministic column map.
const MEMORY_VIEW_JS: &str = include_str!("../assets/views/memory_map.js");
/// Vendored `views/semantic_map.js` — the Semantic tab's meaning-space scatter.
const SEMANTIC_VIEW_JS: &str = include_str!("../assets/views/semantic_map.js");

/// d3 v7.9.0 (`dist/d3.min.js`, ISC license — see `assets/vendor/LICENSE-d3`),
/// inlined so the page never fetches it. Source and SHA-256 are recorded in
/// `assets/README.md`.
pub const D3_JS: &str = include_str!("../assets/vendor/d3.v7.9.0.min.js");

/// The upstream template's d3 CDN tag, replaced by the inline d3.
pub const D3_CDN_TAG: &str = r#"<script src="https://d3js.org/d3.v7.min.js"></script>"#;

/// Slot the offline transform leaves where [`D3_CDN_TAG`] was; filled in the
/// JS-chunk phase.
const INLINE_D3_SLOT: &str = "__COGNEE_INLINE_D3__";

/// The upstream template's three Google Fonts lines (with their newline),
/// deleted by the offline transform.
pub const GOOGLE_FONTS_LINES: [&str; 3] = [
    "<link rel=\"preconnect\" href=\"https://fonts.googleapis.com\">\n",
    "<link rel=\"preconnect\" href=\"https://fonts.gstatic.com\" crossorigin>\n",
    "<link href=\"https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap\" rel=\"stylesheet\">\n",
];

/// Replace the single occurrence of `pattern` in `source` with `replacement`,
/// or fail when it does not occur exactly once.
fn replace_exactly_once(
    source: &str,
    pattern: &'static str,
    replacement: &str,
) -> Result<String, VisualizationError> {
    let found = source.matches(pattern).count();
    if found != 1 {
        return Err(VisualizationError::OfflineTransform { pattern, found });
    }
    Ok(source.replacen(pattern, replacement, 1))
}

/// Apply the offline transform to a template (see the module docs): the d3
/// CDN tag becomes [`INLINE_D3_SLOT`] and the webfont links are dropped.
fn offline_template(template: &str) -> Result<String, VisualizationError> {
    let mut html = replace_exactly_once(template, D3_CDN_TAG, INLINE_D3_SLOT)?;
    for line in GOOGLE_FONTS_LINES {
        html = replace_exactly_once(&html, line, "")?;
    }
    Ok(html)
}

/// Markup sequences that must never appear in a verbatim-inlined script:
/// `</script` closes the element early, and `<!--` followed later by
/// `<script` moves the HTML tokenizer into the script-data-double-escaped
/// state, where the template's own `</script>` no longer closes the element.
const UNSAFE_INLINE_SCRIPT_SEQUENCES: [&[u8]; 3] = [b"</script", b"<!--", b"<script"];

/// Whether `script` contains any [`UNSAFE_INLINE_SCRIPT_SEQUENCES`] entry
/// (ASCII case-insensitively).
fn has_unsafe_inline_script_sequence(script: &str) -> bool {
    UNSAFE_INLINE_SCRIPT_SEQUENCES.iter().any(|needle| {
        script
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    })
}

/// `<script>` + [`D3_JS`] + `</script>`, refusing a d3 build that would close
/// the script element early or disturb the tokenizer state (see
/// [`UNSAFE_INLINE_SCRIPT_SEQUENCES`]).
fn inline_d3_script() -> Result<String, VisualizationError> {
    if has_unsafe_inline_script_sequence(D3_JS) {
        return Err(VisualizationError::UnsafeInlineScript);
    }
    let mut script = String::with_capacity(D3_JS.len() + 17);
    script.push_str("<script>");
    script.push_str(D3_JS);
    script.push_str("</script>");
    Ok(script)
}

/// `layouts/pipeline_layout.emit_js()` returns `""` upstream
/// ([`pipeline_layout.py:16-17`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/layouts/pipeline_layout.py#L16-L17)).
/// The slot is kept so a future L→R Story layout drops in without touching the
/// vendored template; the legacy `computeRankedLayout` inside `story_view.js`
/// provides the Flow layout in the meantime.
const PIPELINE_LAYOUT_JS: &str = "";

/// `layouts/semantic_layout.emit_js()` returns exactly this one line upstream
/// ([`semantic_layout.py:200-206`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/layouts/semantic_layout.py#L200-L206)).
/// It embeds `__SEMANTIC_POSITIONS__`, which is why this chunk must be
/// substituted before the data tokens.
const SEMANTIC_LAYOUT_JS: &str = "window._semanticPositions = __SEMANTIC_POSITIONS__;";

/// Serialize a value to JSON and escape every character that has meaning to
/// the HTML tokenizer, so the result is safe to embed inside a `<script>`
/// element.
///
/// Ports `_safe_json_embed()` (`cognee_network_visualization.py:70-73`) and
/// hardens it. Upstream only rewrites `</`, which stops a node `name` holding
/// `</script>` from closing the block, but not `<!--` followed later by
/// `<script` + space / `/` / `>`: that pair puts the tokenizer into the
/// script-data-double-escaped state, the template's own `</script>` then no
/// longer closes the element, and the whole page renders blank. Realistic
/// input does that — a `DocumentChunk` from an ingested HTML file.
///
/// So `<`, `>` and `&` become `\u003c`, `\u003e` and `\u0026`, and U+2028 /
/// U+2029 (line terminators in pre-ES2019 JS string literals) become
/// `\u2028` / `\u2029`. This is sound because `serde_json` emits none of these
/// characters outside a string literal, and inside one each replacement is a
/// JSON (and JS) escape decoding to the same character: every data token is a
/// JS expression, so the parsed value is unchanged. The `</` rule is subsumed.
/// This is the only XSS guard on the embedded payload.
fn safe_json_embed<T: serde::Serialize>(value: &T) -> Result<String, VisualizationError> {
    let raw = serde_json::to_string(value)?;
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Replace every `(token, value)` pair in `source` in **one** left-to-right
/// pass, so no injected value is ever re-scanned for a later token.
///
/// Every token is `__NAME__`, which lets the scan skip ahead on the two-byte
/// `__` sentinel instead of running one `find` per token per position. Both
/// bytes around a candidate are ASCII `_`, so every offset used to re-slice
/// `source` is a UTF-8 boundary.
///
/// A token absent from `source` is silently skipped (Python's `str.replace` is
/// equally forgiving); a token appearing more than once is replaced every time.
fn substitute_once(source: &str, replacements: &[(&str, Cow<'_, str>)]) -> String {
    debug_assert!(
        replacements
            .iter()
            .all(|(token, _)| token.starts_with("__") && token.len() > 2),
        "every token must be a non-empty `__NAME__`, or the scan below cannot advance"
    );

    // The payloads dwarf the template, so a hint of "template + everything we
    // are about to inject" keeps this to a single allocation in practice.
    let injected: usize = replacements.iter().map(|(_, value)| value.len()).sum();
    let mut out = String::with_capacity(source.len() + injected);

    let mut copied = 0; // everything before this byte is already in `out`
    let mut scan = 0; // where to look for the next candidate token
    while let Some(offset) = source[scan..].find("__") {
        let at = scan + offset;
        match replacements
            .iter()
            .find(|(token, _)| source[at..].starts_with(*token))
        {
            Some((token, value)) => {
                out.push_str(&source[copied..at]);
                out.push_str(value);
                copied = at + token.len();
                scan = copied;
            }
            // Not a token (`__proto__`, a `___TOKEN__` typo, …). Step one byte
            // so a token starting at `at + 1` is still found.
            None => scan = at + 1,
        }
    }
    out.push_str(&source[copied..]);
    out
}

/// Assemble the final HTML from the vendored shell + the preprocessed graph.
///
/// Substitutes all 20 `__TOKEN__` placeholders — the eight JS chunks first, then
/// the twelve data payloads — mirroring
/// `cognee_network_visualization.py:136-168` step for step. See the module docs
/// for why the phase order matters, why each phase is a single pass, and which
/// two payloads are deferred. `search_events` fills `__SEARCH_EVENTS__`.
pub(crate) fn build_html(
    pre: &PreprocessedGraph,
    search_events: &[serde_json::Value],
) -> Result<String, VisualizationError> {
    // 0) Offline transform: inline d3, drop the webfont links. Fails loudly
    //    when a resync changed the tags it rewrites.
    let template = offline_template(HTML_TEMPLATE)?;

    // 1) JS chunks: ordered so the first script block (ui_chrome + schema) runs
    //    before the main story-view IIFE in the second block. Substituted first
    //    so the data tokens *inside* these chunks get resolved below.
    let html = substitute_once(
        &template,
        &[
            (INLINE_D3_SLOT, Cow::Owned(inline_d3_script()?)),
            ("__UI_CHROME_JS__", Cow::Borrowed(UI_CHROME_JS)),
            ("__SCHEMA_VIEW_JS__", Cow::Borrowed(SCHEMA_VIEW_JS)),
            ("__STORY_VIEW_JS__", Cow::Borrowed(STORY_VIEW_JS)),
            ("__PIPELINE_LAYOUT_JS__", Cow::Borrowed(PIPELINE_LAYOUT_JS)),
            ("__INSPECTOR_JS__", Cow::Borrowed(INSPECTOR_JS)),
            ("__MEMORY_VIEW_JS__", Cow::Borrowed(MEMORY_VIEW_JS)),
            ("__SEMANTIC_LAYOUT_JS__", Cow::Borrowed(SEMANTIC_LAYOUT_JS)),
            ("__SEMANTIC_VIEW_JS__", Cow::Borrowed(SEMANTIC_VIEW_JS)),
        ],
    );

    // Caller-supplied opaque passthrough; the literal `null` when absent —
    // `schema_view.js:22` reads it as `const schemaData = null`.
    let schema_data = match pre.schema_data.as_ref() {
        Some(value) => safe_json_embed(value)?,
        None => "null".to_string(),
    };

    // Preprocessor-derived type graph; `{"nodes": [], "links": []}` when the
    // preprocessor produced nothing usable (mirrors Python's
    // `pre.schema_graph or {"nodes": [], "links": []}` truthiness fallback).
    let schema_graph_is_empty = match &pre.schema_graph {
        serde_json::Value::Object(map) => map.is_empty(),
        _ => true,
    };
    let schema_graph = if schema_graph_is_empty {
        safe_json_embed(&json!({"nodes": [], "links": []}))?
    } else {
        safe_json_embed(&pre.schema_graph)?
    };

    // Unconditional JSON-fallback substitutions: a leaked `__MEMORY_DATA__` /
    // `__SEARCH_EVENTS__` token would break the page and fail the
    // no-placeholder assembly test.
    let memory_is_empty = match &pre.memory_map {
        serde_json::Value::Object(map) => map.is_empty(),
        _ => true,
    };
    let memory_data = if memory_is_empty {
        safe_json_embed(&json!({}))?
    } else {
        safe_json_embed(&pre.memory_map)?
    };

    // 2) Data tokens: substituted last so JSON-embedded `__SCHEMA_GRAPH_DATA__`
    //    and friends inside the JS chunks above are reached. Listed in Python's
    //    order for reviewability — with a single pass the *within-phase* order is
    //    no longer load-bearing, which is the point.
    Ok(substitute_once(
        &html,
        &[
            ("__NODES_DATA__", Cow::Owned(safe_json_embed(&pre.nodes)?)),
            ("__LINKS_DATA__", Cow::Owned(safe_json_embed(&pre.links)?)),
            (
                "__TASK_COLORS__",
                Cow::Owned(safe_json_embed(&pre.color_maps.task)?),
            ),
            (
                "__PIPELINE_COLORS__",
                Cow::Owned(safe_json_embed(&pre.color_maps.pipeline)?),
            ),
            (
                "__NODESET_COLORS__",
                Cow::Owned(safe_json_embed(&pre.color_maps.node_set)?),
            ),
            (
                "__USER_COLORS__",
                Cow::Owned(safe_json_embed(&pre.color_maps.user)?),
            ),
            ("__SCHEMA_DATA__", Cow::Owned(schema_data)),
            ("__SCHEMA_GRAPH_DATA__", Cow::Owned(schema_graph)),
            ("__MEMORY_DATA__", Cow::Owned(memory_data)),
            // Memory-tab timeline events (Python `_safe_json_embed(search_events or [])`).
            (
                "__SEARCH_EVENTS__",
                Cow::Owned(safe_json_embed(&search_events)?),
            ),
            // Deferred: no readable stored embeddings, hence no projection or
            // clustering. `null` is the same state Python reaches when its
            // best-effort `_semantic_payload` raises; `semantic_map.js` renders
            // an empty state.
            ("__SEMANTIC_POSITIONS__", Cow::Borrowed("null")),
            ("__SEMANTIC_CLUSTERS__", Cow::Borrowed("null")),
        ],
    ))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Every token slot declared by the vendored template must be present
    /// before substitution — a missing one means a bad or partial asset resync.
    #[test]
    fn template_declares_all_js_chunk_slots() {
        for p in [
            "__UI_CHROME_JS__",
            "__SCHEMA_VIEW_JS__",
            "__STORY_VIEW_JS__",
            "__PIPELINE_LAYOUT_JS__",
            "__INSPECTOR_JS__",
            "__MEMORY_VIEW_JS__",
            "__SEMANTIC_LAYOUT_JS__",
            "__SEMANTIC_VIEW_JS__",
        ] {
            assert!(
                HTML_TEMPLATE.contains(p),
                "vendored template.html is missing JS chunk slot {p}"
            );
        }
    }

    /// The twelve data tokens live inside the vendored JS chunks, not in the
    /// shell — assert each chunk still declares the ones it owns.
    #[test]
    fn vendored_chunks_declare_all_data_slots() {
        for (chunk, name, tokens) in [
            (
                STORY_VIEW_JS,
                "story_view.js",
                &[
                    "__NODES_DATA__",
                    "__LINKS_DATA__",
                    "__TASK_COLORS__",
                    "__PIPELINE_COLORS__",
                    "__NODESET_COLORS__",
                    "__USER_COLORS__",
                ][..],
            ),
            (
                SCHEMA_VIEW_JS,
                "schema_view.js",
                &["__SCHEMA_DATA__", "__SCHEMA_GRAPH_DATA__"][..],
            ),
            (
                MEMORY_VIEW_JS,
                "memory_map.js",
                &["__MEMORY_DATA__", "__SEARCH_EVENTS__"][..],
            ),
            (
                SEMANTIC_VIEW_JS,
                "semantic_map.js",
                &["__SEMANTIC_CLUSTERS__"][..],
            ),
            (
                SEMANTIC_LAYOUT_JS,
                "semantic_layout chunk",
                &["__SEMANTIC_POSITIONS__"][..],
            ),
        ] {
            for t in tokens {
                assert!(
                    chunk.contains(t),
                    "vendored {name} is missing data slot {t}"
                );
            }
        }
    }

    #[test]
    fn safe_json_embed_escapes_closing_script() {
        let v = serde_json::json!({"x": "</script>"});
        let out = safe_json_embed(&v).expect("json encode");
        assert!(out.contains("\\u003c/script\\u003e"));
        assert!(!out.contains("</script>"));
    }

    /// No markup character survives, and the escaped JSON parses back to the
    /// original value.
    #[test]
    fn safe_json_embed_escapes_markup_and_round_trips() {
        let hostile = [
            "<!--<script>",
            "</script><img src=https://x>",
            "&lt;&amp;",
            "a\u{2028}b\u{2029}c",
            "<SCRIPT/x>-->",
        ];
        let value = serde_json::json!({"names": hostile, "<key>": "&"});
        let out = safe_json_embed(&value).expect("json encode");
        for c in ['<', '>', '&', '\u{2028}', '\u{2029}'] {
            assert!(!out.contains(c), "{c:?} survived in {out}");
        }
        let back: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(back, value);
    }

    #[test]
    fn bundled_d3_has_no_unsafe_inline_script_sequence() {
        assert!(!has_unsafe_inline_script_sequence(D3_JS));
        assert!(inline_d3_script().is_ok());
    }

    #[test]
    fn unsafe_inline_script_sequences_are_detected_case_insensitively() {
        for bad in ["a</SCRIPT>b", "x<!--y", "if(a<ScRiPt)", "</script"] {
            assert!(has_unsafe_inline_script_sequence(bad), "{bad}");
        }
        for ok in ["a < b", "x<!-y", "<\\/script>", "i<scrip"] {
            assert!(!has_unsafe_inline_script_sequence(ok), "{ok}");
        }
    }

    /// The whole point of the single pass: an injected value that happens to
    /// contain a *later* token's literal name must survive untouched.
    #[test]
    fn substitute_once_never_rescans_injected_values() {
        let out = substitute_once(
            "a=__FIRST__ b=__SECOND__",
            &[
                ("__FIRST__", Cow::Borrowed("\"quotes __SECOND__ verbatim\"")),
                ("__SECOND__", Cow::Borrowed("[]")),
            ],
        );
        assert_eq!(out, "a=\"quotes __SECOND__ verbatim\" b=[]");
    }

    /// `__`-prefixed non-tokens must not shift the scan past a real token, and
    /// an absent token is a no-op rather than an error.
    #[test]
    fn substitute_once_handles_near_misses_and_repeats() {
        let out = substitute_once(
            "___X__ __proto__ __X__ __X__ __ABSENT__x",
            &[("__X__", Cow::Borrowed("!"))],
        );
        // `___X__` contains `__X__` starting one byte in, so it becomes `_!`.
        assert_eq!(out, "_! __proto__ ! ! __ABSENT__x");
    }

    /// Multi-byte characters around a token must not be split.
    #[test]
    fn substitute_once_is_utf8_safe() {
        let out = substitute_once("héllo __T__ wörld", &[("__T__", Cow::Borrowed("→"))]);
        assert_eq!(out, "héllo → wörld");
    }

    /// Every offline-transform pattern is in the vendored template exactly
    /// once — the precondition `build_html` enforces at render time.
    #[test]
    fn offline_patterns_occur_exactly_once_in_the_vendored_template() {
        assert_eq!(HTML_TEMPLATE.matches(D3_CDN_TAG).count(), 1);
        for line in GOOGLE_FONTS_LINES {
            assert_eq!(HTML_TEMPLATE.matches(line).count(), 1, "{line}");
        }
        let html = offline_template(HTML_TEMPLATE).expect("transform applies");
        assert!(!html.contains("d3js.org/d3.v7.min.js"));
        assert!(!html.contains("fonts.googleapis.com"));
        assert!(!html.contains("fonts.gstatic.com"));
        assert_eq!(html.matches(INLINE_D3_SLOT).count(), 1);
    }

    /// A resync that drops or duplicates a pattern fails instead of silently
    /// re-enabling the CDN.
    #[test]
    fn offline_transform_rejects_missing_or_duplicated_patterns() {
        let missing = HTML_TEMPLATE.replace(D3_CDN_TAG, "");
        assert!(matches!(
            offline_template(&missing),
            Err(VisualizationError::OfflineTransform { found: 0, .. })
        ));
        let doubled = HTML_TEMPLATE.replace(D3_CDN_TAG, &format!("{D3_CDN_TAG}{D3_CDN_TAG}"));
        assert!(matches!(
            offline_template(&doubled),
            Err(VisualizationError::OfflineTransform { found: 2, .. })
        ));
        let no_font = HTML_TEMPLATE.replace(GOOGLE_FONTS_LINES[2], "");
        assert!(matches!(
            offline_template(&no_font),
            Err(VisualizationError::OfflineTransform { found: 0, .. })
        ));
    }

    #[test]
    fn bundled_d3_is_safe_to_inline() {
        assert!(!D3_JS.to_ascii_lowercase().contains("</script"));
        assert!(D3_JS.starts_with("// https://d3js.org v7.9.0 "));
        let script = inline_d3_script().expect("d3 inlines");
        assert!(script.starts_with("<script>// https://d3js.org v7.9.0"));
        assert!(script.ends_with("</script>"));
    }

    #[test]
    fn pipeline_layout_chunk_is_the_upstream_empty_stub() {
        assert_eq!(PIPELINE_LAYOUT_JS, "");
    }

    #[test]
    fn semantic_layout_chunk_matches_upstream_one_liner() {
        assert_eq!(
            SEMANTIC_LAYOUT_JS,
            "window._semanticPositions = __SEMANTIC_POSITIONS__;"
        );
    }
}
