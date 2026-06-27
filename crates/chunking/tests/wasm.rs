//! WebAssembly smoke test for the Config-1 spike.
//!
//! Proves the pure chunking primitives (`chunk_text` + a `TokenCounter`)
//! actually **run** inside a wasm host — not merely that they compile. This
//! closes the last Config-1 acceptance item (see `docs/spike-wasm-config1.md`).
//!
//! The whole file is gated to `wasm32`; on native targets it compiles to an
//! empty crate, so it never interferes with the normal test suite.
//!
//! Run it (on a host without Windows Smart App Control — e.g. WSL/Linux):
//!
//! ```bash
//! # one-time: provides the `wasm-bindgen-test-runner` used by .cargo/config.toml
//! cargo install wasm-bindgen-cli
//! # needs Node.js on PATH (the runner executes the wasm under node by default)
//!
//! cargo test -p cognee-chunking --target wasm32-unknown-unknown --test wasm
//! cargo test -p cognee-chunking --features tiktoken \
//!     --target wasm32-unknown-unknown --test wasm
//! ```
//!
//! `--test wasm` builds only this integration test (not the crate's inline
//! `#[test]` unit tests, which use the native libtest harness and don't run
//! under the wasm-bindgen runner).

#![cfg(target_arch = "wasm32")]

use cognee_chunking::{NAMESPACE_OID, TokenCounter, WordCounter, chunk_text};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn word_counter_runs_in_wasm() {
    assert_eq!(WordCounter.count_tokens("hello wasm world"), 3);
    assert_eq!(WordCounter.count_tokens(""), 0);
}

#[wasm_bindgen_test]
fn chunk_text_runs_in_wasm() {
    let counter = WordCounter;
    // NAMESPACE_OID is a valid Uuid; reuse it as the document id so the test
    // needs no direct uuid dependency.
    let doc = NAMESPACE_OID;
    let text = "First paragraph of the spike.\n\n\
                Second paragraph has a few more words than the first one does.";

    let chunks = chunk_text(doc, text, 8, &counter);

    assert!(!chunks.is_empty(), "expected at least one chunk");
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.chunk_index, i, "chunks must be indexed sequentially from 0");
        assert!(!c.text.is_empty(), "chunk text should be non-empty");
        assert!(c.chunk_size > 0, "chunk size should be counted");
        assert_eq!(c.document_id, doc, "chunk should carry its document id");
    }
}

#[cfg(feature = "tiktoken")]
#[wasm_bindgen_test]
fn tiktoken_counter_runs_in_wasm() {
    use cognee_chunking::TikTokenCounter;

    // The cl100k_base BPE tables are bundled in the binary (pure Rust) — this
    // exercises that they load and encode under wasm with no filesystem/network.
    let counter = TikTokenCounter::cl100k_base().expect("cl100k_base BPE loads in wasm");
    let n = counter.count_tokens("Hello, world!");
    assert!((3..=6).contains(&n), "expected 3-6 cl100k tokens, got {n}");
    assert_eq!(counter.count_tokens(""), 0);
}
