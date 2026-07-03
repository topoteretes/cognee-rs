//! Shared smoke-test bodies for the Config-1 wasm spike.
//!
//! These plain functions hold the actual assertions; they are wrapped with
//! `#[wasm_bindgen_test]` by both runners:
//!
//! * `wasm.rs`         — executes them under **Node** (the default runner).
//! * `wasm_browser.rs` — executes them in a **headless browser** (via
//!   `wasm_bindgen_test_configure!(run_in_browser)` + a WebDriver).
//!
//! Keeping the bodies here means the two harnesses can never drift apart. This
//! file lives in a `tests/` subdirectory, so cargo does not treat it as its own
//! test target — it is only compiled when `mod`-included by a wasm test file.

use cognee_chunking::{NAMESPACE_OID, TokenCounter, WordCounter, chunk_text};

pub fn word_counter() {
    assert_eq!(WordCounter.count_tokens("hello wasm world"), 3);
    assert_eq!(WordCounter.count_tokens(""), 0);
}

pub fn chunk_text_smoke() {
    let counter = WordCounter;
    // NAMESPACE_OID is a valid Uuid; reuse it as the document id so the test
    // needs no direct uuid dependency.
    let doc = NAMESPACE_OID;
    let text = "First paragraph of the spike.\n\n\
                Second paragraph has a few more words than the first one does.";

    let chunks = chunk_text(doc, text, 8, &counter);

    assert!(!chunks.is_empty(), "expected at least one chunk");
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(
            c.chunk_index, i,
            "chunks must be indexed sequentially from 0"
        );
        assert!(!c.text.is_empty(), "chunk text should be non-empty");
        assert!(c.chunk_size > 0, "chunk size should be counted");
        assert_eq!(c.document_id, doc, "chunk should carry its document id");
    }
}

#[cfg(feature = "tiktoken")]
pub fn tiktoken_counter() {
    use cognee_chunking::TikTokenCounter;

    // The cl100k_base BPE tables are bundled in the binary (pure Rust) — this
    // exercises that they load and encode under wasm with no filesystem/network.
    let counter = TikTokenCounter::cl100k_base().expect("cl100k_base BPE loads in wasm");
    let n = counter.count_tokens("Hello, world!");
    assert!((3..=6).contains(&n), "expected 3-6 cl100k tokens, got {n}");
    assert_eq!(counter.count_tokens(""), 0);
}
