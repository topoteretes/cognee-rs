#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Every HTML this crate produces must work with no network.
//!
//! The vendored `template.html` loads d3 from a CDN and the Inter webfont from
//! Google Fonts; `src/html.rs` applies an "offline transform" that inlines the
//! bundled d3 and drops the webfont links. These tests pin the result on every
//! HTML path (`render_graph_data` on an empty and a non-trivial graph, and
//! `render_multi_user`):
//!
//! 1. The inline d3 blob is located by its exact bundled content and hashes to
//!    the pinned SHA-256.
//! 2. With that blob cut out, no resource-loading construct remains (d3 itself
//!    contains `fetch(` from d3-fetch, which is why the cut is needed).
//! 3. On the **whole** document, every `http://` / `https://` substring is one
//!    of a fixed set of non-fetched identifiers.
//! 4. On the whole document: no `<script src`, no `<link` tag, no `@import`, no
//!    `url(http` / `url(//`, and the only `data:` URI is the logo.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, MockGraphDB};
use cognee_visualization::{render_graph_data, render_multi_user};
use serde_json::{Value, json};

/// The bundled d3, byte-for-byte what `src/html.rs` inlines.
const D3_JS: &str = include_str!("../assets/vendor/d3.v7.9.0.min.js");

/// SHA-256 of `assets/vendor/d3.v7.9.0.min.js` (d3 7.9.0 `dist/d3.min.js`, as
/// published on jsdelivr and cdnjs). Recorded in `assets/README.md`.
const D3_SHA256: &str = "f2094bbf6141b359722c4fe454eb6c4b0f0e42cc10cc7af921fc158fceb86539";

/// The only absolute URLs allowed anywhere in the output. None is fetched:
/// the first is d3's banner comment, the rest are XML namespace identifiers d3
/// uses for `createElementNS` and friends.
const URL_ALLOWLIST: [&str; 6] = [
    "https://d3js.org",
    "http://www.w3.org/1999/xhtml",
    "http://www.w3.org/1999/xlink",
    "http://www.w3.org/2000/svg",
    "http://www.w3.org/2000/xmlns/",
    "http://www.w3.org/XML/1998/namespace",
];

// ── Minimal SHA-256 (FIPS 180-4) ─────────────────────────────────────────────
//
// The crate carries no hashing dependency; this is a straightforward test-only
// implementation, checked against the standard empty-string and "abc" vectors.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[test]
fn sha256_matches_standard_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn bundled_d3_matches_the_pinned_release() {
    assert_eq!(
        D3_JS.len(),
        279_706,
        "d3 7.9.0 dist/d3.min.js is 279,706 bytes"
    );
    assert_eq!(sha256_hex(D3_JS.as_bytes()), D3_SHA256);
}

// ── Scanners ─────────────────────────────────────────────────────────────────

/// Cut the inline d3 blob out of `html`, asserting it is present exactly once,
/// wrapped in its own `<script>` element, and unmodified (pinned SHA).
fn cut_inline_d3(html: &str) -> String {
    assert_eq!(
        html.matches(D3_JS).count(),
        1,
        "the bundled d3 must be inlined exactly once"
    );
    let start = html.find(D3_JS).expect("d3 present");
    let end = start + D3_JS.len();
    assert!(
        html[..start].ends_with("<script>"),
        "inline d3 must open its own <script> element"
    );
    assert!(
        html[end..].starts_with("</script>"),
        "inline d3 must close its own <script> element"
    );
    assert_eq!(sha256_hex(&html.as_bytes()[start..end]), D3_SHA256);
    format!("{}{}", &html[..start], &html[end..])
}

/// Byte offsets of every case-insensitive occurrence of `needle` in `haystack`.
fn find_all_ci(haystack: &str, needle: &str) -> Vec<usize> {
    let hay = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    hay.match_indices(&needle).map(|(at, _)| at).collect()
}

/// Every opening tag `<name …>` (case-insensitive), as the text between `<`
/// and the next `>`. `<links.length` (JS) is not a `<link` tag: the name must
/// be followed by whitespace, `/` or `>`.
fn tags<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let opener = format!("<{name}");
    find_all_ci(html, &opener)
        .into_iter()
        .filter(|&at| {
            html.as_bytes()
                .get(at + opener.len())
                .is_some_and(|&next| next.is_ascii_whitespace() || next == b'>' || next == b'/')
        })
        .map(|at| {
            let rest = &html[at..];
            &rest[..rest.find('>').map_or(rest.len(), |end| end + 1)]
        })
        .collect()
}

/// Rule 1 (on the document with d3 cut out): no resource-loading construct
/// points off-page. `src="data:` is the one allowed `src`.
fn assert_no_resource_loading_constructs(doc: &str, label: &str) {
    for tag in tags(doc, "script") {
        assert!(
            !tag.to_ascii_lowercase().contains("src="),
            "{label}: external script tag {tag}"
        );
    }
    for tag in tags(doc, "link") {
        assert!(
            !tag.to_ascii_lowercase().contains("href="),
            "{label}: link tag loads a resource: {tag}"
        );
    }
    for tag in tags(doc, "img") {
        let lower = tag.to_ascii_lowercase();
        let srcs: Vec<usize> = lower.match_indices("src=").map(|(at, _)| at).collect();
        for at in srcs {
            assert!(
                lower[at..].starts_with("src=\"data:"),
                "{label}: img loads a non-data resource: {}",
                &tag[..tag.len().min(120)]
            );
        }
    }
    for name in ["iframe", "object", "embed"] {
        assert!(tags(doc, name).is_empty(), "{label}: <{name}> present");
    }
    for construct in [
        "url(http",
        "url(//",
        "@import",
        "fetch(",
        "XMLHttpRequest",
        "importScripts(",
        "new WebSocket(",
        "new EventSource(",
        "navigator.sendBeacon",
    ] {
        assert!(
            find_all_ci(doc, construct).is_empty(),
            "{label}: `{construct}` present outside the inline d3"
        );
    }
}

/// Rule 2 (on the whole document): every absolute URL is allowlisted.
fn assert_only_allowlisted_urls(doc: &str, label: &str) {
    let mut seen = Vec::new();
    for scheme in ["http://", "https://"] {
        for at in find_all_ci(doc, scheme) {
            let rest = &doc[at..];
            let end = rest
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | '<' | '>' | '\\')
                })
                .unwrap_or(rest.len());
            let url = &rest[..end];
            assert!(
                URL_ALLOWLIST.contains(&url),
                "{label}: non-allowlisted URL {url:?}"
            );
            seen.push(url.to_string());
        }
    }
    // The d3 banner is always there; this proves the scan actually ran.
    assert!(seen.iter().any(|url| url == "https://d3js.org"));
}

/// The B1 belt-and-braces checks on the whole document.
fn assert_whole_document_is_self_contained(doc: &str, label: &str) {
    assert!(
        find_all_ci(doc, "<script src").is_empty(),
        "{label}: <script src present"
    );
    assert!(tags(doc, "link").is_empty(), "{label}: <link> tag present");
    for construct in ["@import", "url(http", "url(//"] {
        assert!(
            find_all_ci(doc, construct).is_empty(),
            "{label}: `{construct}` present"
        );
    }
    // The only data: URI is the embedded logo.
    let data_uris: Vec<usize> = find_all_ci(doc, "\"data:")
        .into_iter()
        .chain(find_all_ci(doc, "'data:"))
        .chain(find_all_ci(doc, "(data:"))
        .collect();
    assert_eq!(
        data_uris.len(),
        1,
        "{label}: expected only the logo data: URI"
    );
    assert!(doc[data_uris[0] + 1..].starts_with("data:image/png;base64,"));
}

fn assert_offline(html: &str, label: &str) {
    assert!(
        html.starts_with("<!DOCTYPE html>"),
        "{label}: not an HTML page"
    );
    // Rule 3: a d3 marker is present inline.
    assert!(
        html.contains("// https://d3js.org v7.9.0"),
        "{label}: no d3 banner"
    );
    assert!(html.contains("forceSimulation"), "{label}: no d3 code");

    let without_d3 = cut_inline_d3(html);
    assert_no_resource_loading_constructs(&without_d3, label);
    assert_only_allowlisted_urls(html, label);
    assert_whole_document_is_self_contained(html, label);

    // Nothing of the upstream CDN / webfont tags survives.
    for gone in ["d3.v7.min.js", "fonts.googleapis.com", "fonts.gstatic.com"] {
        assert!(!html.contains(gone), "{label}: `{gone}` survived");
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn graph_node(id: &str, props: Value) -> GraphNode {
    let map: HashMap<Cow<'static, str>, Value> = props
        .as_object()
        .expect("fixture props are an object")
        .iter()
        .map(|(key, value)| (Cow::Owned(key.clone()), value.clone()))
        .collect();
    (id.to_string(), map)
}

fn edge(source: &str, target: &str, relation: &str) -> EdgeData {
    (
        source.to_string(),
        target.to_string(),
        relation.to_string(),
        HashMap::from([(
            Cow::Borrowed("relationship_name"),
            Value::String(relation.to_string()),
        )]),
    )
}

/// A graph touching every tab: documents, chunks, entities with a semantic
/// type, a summary, a global context node, a DLT table and memory node sets.
fn non_trivial_graph() -> (Vec<GraphNode>, Vec<EdgeData>) {
    let nodes = vec![
        graph_node(
            "doc1",
            json!({"type": "TextDocument", "name": "alice.txt", "created_at": 1_700_000_000_000_i64,
                   "source_pipeline": "cognify_pipeline", "source_task": "classify_documents"}),
        ),
        graph_node(
            "chunk1",
            json!({"type": "DocumentChunk", "text": "Alice works at Acme as an engineer.",
                   "document_id": "doc1", "chunk_index": 0, "created_at": 1_700_000_000_100_i64}),
        ),
        graph_node(
            "alice",
            json!({"type": "Entity", "name": "Alice", "description": "An engineer",
                   "source_node_set": "session_learnings"}),
        ),
        graph_node(
            "acme",
            json!({"type": "Entity", "name": "Acme", "source_node_set": "skills"}),
        ),
        graph_node("person", json!({"type": "EntityType", "name": "Person"})),
        graph_node("company", json!({"type": "EntityType", "name": "Company"})),
        graph_node(
            "sum1",
            json!({"type": "TextSummary", "text": "Alice is an engineer at Acme."}),
        ),
        graph_node(
            "ctx1",
            json!({"type": "GlobalContextSummary", "text": "Root context", "is_root": true}),
        ),
        graph_node(
            "dana",
            json!({"type": "Person", "name": "Dana Kim",
                   "metadata": {"index_fields": ["name"], "identity_fields": ["name"]}}),
        ),
    ];
    let edges = vec![
        edge("doc1", "chunk1", "contains"),
        edge("chunk1", "alice", "contains"),
        edge("chunk1", "acme", "contains"),
        edge("alice", "person", "is_a"),
        edge("acme", "company", "is_a"),
        edge("alice", "acme", "works_at"),
        edge("chunk1", "sum1", "made_from"),
        edge("sum1", "ctx1", "summarized_in"),
    ];
    (nodes, edges)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn empty_graph_html_is_fully_offline() {
    let html = render_graph_data(Vec::new(), Vec::new()).expect("render succeeds");
    assert_offline(&html, "empty graph");
}

#[test]
fn non_trivial_graph_html_is_fully_offline() {
    let (nodes, edges) = non_trivial_graph();
    let html = render_graph_data(nodes, edges).expect("render succeeds");
    assert_offline(&html, "non-trivial graph");
    // The fixture really rendered (not an empty page that trivially passes).
    for id in ["doc1", "chunk1", "alice", "ctx1", "dana"] {
        assert!(
            html.contains(&format!("\"id\":\"{id}\"")),
            "node {id} missing"
        );
    }
}

#[test]
fn a_table_dataset_html_is_fully_offline() {
    let nodes = vec![
        graph_node(
            "users",
            json!({"type": "SchemaTable", "name": "users",
                   "columns": [{"name": "id", "type": "int", "primary_key": true}]}),
        ),
        graph_node(
            "posts",
            json!({"type": "SchemaTable", "name": "posts", "columns": "[]"}),
        ),
        graph_node(
            "fk",
            json!({"type": "SchemaRelationship", "name": "posts_user_id",
                   "source_table": "posts", "target_table": "users",
                   "relationship_type": "foreign_key"}),
        ),
    ];
    let html = render_graph_data(nodes, vec![edge("posts", "users", "is_table_of")])
        .expect("render succeeds");
    assert_offline(&html, "table dataset");
}

#[tokio::test]
async fn multi_user_html_is_fully_offline() {
    let first = MockGraphDB::new();
    first
        .add_node_raw(json!({"id": "a", "type": "Entity", "name": "Alice"}))
        .await
        .expect("add node a");
    first
        .add_node_raw(json!({"id": "t", "type": "EntityType", "name": "Person"}))
        .await
        .expect("add node t");
    first
        .add_edge("a", "t", "is_a", None)
        .await
        .expect("add edge a -> t");
    let second = MockGraphDB::new();
    second
        .add_node_raw(json!({"id": "b", "type": "DocumentChunk", "text": "hello"}))
        .await
        .expect("add node b");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![
        ("alice@example.com".to_string(), Arc::new(first)),
        ("bob@example.com".to_string(), Arc::new(second)),
    ];
    let html = render_multi_user(&pairs).await.expect("render succeeds");
    assert_offline(&html, "multi-user");
    assert!(html.contains("alice@example.com"));

    let empty = render_multi_user(&[]).await.expect("render succeeds");
    assert_offline(&empty, "multi-user (empty)");
}

/// The scanners must actually catch what they exist to catch — otherwise a
/// green run proves nothing.
#[test]
fn scanners_reject_external_resources() {
    let html = render_graph_data(Vec::new(), Vec::new()).expect("render succeeds");
    let cut = cut_inline_d3(&html);

    for injected in [
        "<script src=\"https://cdn.example/x.js\"></script>",
        "<LINK rel=\"stylesheet\" href=\"https://cdn.example/x.css\">",
        "<img src=\"https://cdn.example/x.png\">",
        "<iframe></iframe>",
        "<style>@import 'x.css';</style>",
        "<style>a{background:url(https://cdn.example/x.png)}</style>",
        "<script>fetch('/x')</script>",
        "<script>new WebSocket('ws://x')</script>",
    ] {
        let tampered = cut.replacen("</body>", &format!("{injected}</body>"), 1);
        let result = std::panic::catch_unwind(|| {
            assert_no_resource_loading_constructs(&tampered, "tampered");
        });
        assert!(result.is_err(), "construct scan missed {injected}");
    }

    let tampered = html.replacen("</body>", "<a>https://evil.example/</a></body>", 1);
    let result = std::panic::catch_unwind(|| assert_only_allowlisted_urls(&tampered, "tampered"));
    assert!(result.is_err(), "URL allowlist missed an external URL");

    // `<links.length` in JS is not a link tag.
    assert!(tags("for(i=0;i<links.length;i++)", "link").is_empty());
}

// ── Hostile data ─────────────────────────────────────────────────────────────
//
// User data (node names, chunk text, user labels) is embedded as JSON inside
// the template's `<script>` blocks. It must never change how the HTML
// tokenizer sees the page: no `</script` closing a block early, and no `<!--`
// + `<script` pair pushing the tokenizer into the script-data-double-escaped
// state (after which the template's own `</script>` no longer closes the
// block and the page renders blank).
//
// The URL allowlist question: escaping `<` / `>` / `&` neutralises markup but
// cannot hide a `https://` inside a string, and it should not — the data is
// the user's. So the offline check is split in two, keeping the guarantee:
//
// * every occurrence of a hostile URL is proven to sit inside a JSON string
//   literal, inside a `<script>` element, with no tag or attribute context
//   around it (the markup counts match the empty page, and the literal
//   decodes to the original string) — inert data the page never fetches;
// * those proven-inert literals are then blanked to `""` and the full,
//   unrelaxed `assert_offline` (allowlist included) runs on the rest.
//
// Any URL outside such a literal still trips the strict allowlist.

/// Each hostile string carries a unique `HOSTILE_` marker so the test can find
/// its JSON literal without depending on how the crate escapes it.
const HOSTILE: [&str; 6] = [
    "HOSTILE_A:<!--<script>",
    "HOSTILE_B:</script><img src=https://x>",
    "HOSTILE_C:&lt;&amp;",
    "HOSTILE_D:line\u{2028}para\u{2029}end",
    "HOSTILE_E:<!-- <script type=x> --> </SCRIPT >",
    "HOSTILE_F:<script/>https://evil.example/a.js",
];

/// The non-allowlisted URLs the hostile strings carry.
const HOSTILE_URLS: [&str; 2] = ["https://x", "https://evil.example/a.js"];

/// Byte span `[start, end)` of the JSON string literal opening at `start`.
fn json_string_span(html: &str, start: usize) -> (usize, usize) {
    let bytes = html.as_bytes();
    assert_eq!(bytes[start], b'"');
    let mut i = start + 1;
    while bytes[i] != b'"' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    (start, i + 1)
}

/// Whether byte offset `at` lies inside a `<script>` element: the last
/// `<script` opener before it is not closed before it.
fn inside_script_element(html: &str, at: usize) -> bool {
    let before = html[..at].to_ascii_lowercase();
    match (before.rfind("<script"), before.rfind("</script")) {
        (Some(open), Some(close)) => open > close,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Count of case-insensitive occurrences of each tokenizer-relevant sequence.
fn markup_counts(html: &str) -> [usize; 3] {
    [
        find_all_ci(html, "<!--").len(),
        find_all_ci(html, "<script").len(),
        find_all_ci(html, "</script").len(),
    ]
}

/// Assert the hostile strings are inert data in `html`, then run the full
/// offline check on `html` with their (proven-inert) literals blanked.
fn assert_hostile_data_is_inert(html: &str, label: &str, expected: &[&str]) {
    let baseline = render_graph_data(Vec::new(), Vec::new()).expect("render succeeds");
    assert_eq!(
        markup_counts(html),
        markup_counts(&baseline),
        "{label}: data changed the `<!--` / `<script` / `</script` counts"
    );
    for raw in [
        "<img",
        "<!--<script",
        "<!-- <script",
        "<script/>",
        "</script><",
    ] {
        assert!(
            !html.contains(raw) || baseline.contains(raw),
            "{label}: raw hostile markup {raw:?} reached the page"
        );
    }

    // Locate and decode every hostile literal.
    let mut spans = Vec::new();
    for at in find_all_ci(html, "\"HOSTILE_") {
        if html[..at].ends_with('\\') {
            continue; // a quote inside an enclosing literal, covered by it
        }
        let (start, end) = json_string_span(html, at);
        assert!(
            inside_script_element(html, start),
            "{label}: hostile literal outside a <script> element"
        );
        let decoded: String = serde_json::from_str(&html[start..end])
            .unwrap_or_else(|err| panic!("{label}: literal does not parse: {err}"));
        assert!(decoded.starts_with("HOSTILE_"));
        spans.push((start, end, decoded));
    }
    for original in expected {
        assert!(
            spans.iter().any(|(_, _, decoded)| decoded == original),
            "{label}: {original:?} does not round-trip through the page"
        );
    }

    // Every hostile URL lies inside one of those literals.
    for url in HOSTILE_URLS {
        for at in find_all_ci(html, url) {
            assert!(
                spans.iter().any(|&(start, end, _)| start < at && at < end),
                "{label}: {url} appears outside a JSON string literal"
            );
        }
    }

    // Blank the inert literals and run the unrelaxed offline check.
    let mut blanked = String::with_capacity(html.len());
    let mut cursor = 0;
    spans.sort_by_key(|&(start, _, _)| start);
    for (start, end, _) in &spans {
        blanked.push_str(&html[cursor..*start]);
        blanked.push_str("\"\"");
        cursor = *end;
    }
    blanked.push_str(&html[cursor..]);
    assert_offline(&blanked, label);
}

fn hostile_graph() -> (Vec<GraphNode>, Vec<EdgeData>) {
    let mut nodes = vec![graph_node(
        "doc1",
        json!({"type": "TextDocument", "name": HOSTILE[0]}),
    )];
    let mut edges = Vec::new();
    for (i, text) in HOSTILE.iter().enumerate() {
        let chunk = format!("chunk{i}");
        let entity = format!("ent{i}");
        nodes.push(graph_node(
            &chunk,
            json!({"type": "DocumentChunk", "text": text, "document_id": "doc1",
                   "chunk_index": i}),
        ));
        nodes.push(graph_node(
            &entity,
            json!({"type": "Entity", "name": text, "description": text,
                   "source_node_set": "session_learnings"}),
        ));
        edges.push(edge("doc1", &chunk, "contains"));
        edges.push(edge(&chunk, &entity, "contains"));
    }
    (nodes, edges)
}

#[test]
fn hostile_graph_data_is_inert_and_offline() {
    let (nodes, edges) = hostile_graph();
    let html = render_graph_data(nodes, edges).expect("render succeeds");
    assert_hostile_data_is_inert(&html, "hostile graph", &HOSTILE);
}

#[tokio::test]
async fn hostile_multi_user_data_is_inert_and_offline() {
    let db = MockGraphDB::new();
    for (i, text) in HOSTILE.iter().enumerate() {
        db.add_node_raw(json!({"id": format!("n{i}"), "type": "Entity", "name": text}))
            .await
            .expect("add node");
    }
    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![(HOSTILE[1].to_string(), Arc::new(db))];
    let html = render_multi_user(&pairs).await.expect("render succeeds");
    assert_hostile_data_is_inert(&html, "hostile multi-user", &HOSTILE);
}

/// The inertness check must catch hostile markup that escaped the JSON.
#[test]
fn hostile_scan_rejects_unescaped_markup() {
    let (nodes, edges) = hostile_graph();
    let html = render_graph_data(nodes, edges).expect("render succeeds");
    for injected in ["<!--<script>", "<img src=https://x>"] {
        let tampered = html.replacen("</body>", &format!("{injected}</body>"), 1);
        let result = std::panic::catch_unwind(|| {
            assert_hostile_data_is_inert(&tampered, "tampered", &HOSTILE);
        });
        assert!(result.is_err(), "hostile scan missed {injected}");
    }
}
