//! WebAssembly smoke test for the Config-1 spike — **Node** runner.
//!
//! Proves the pure chunking primitives (`chunk_text` + a `TokenCounter`)
//! actually **run** inside a wasm host — not merely that they compile. This
//! closes the last Config-1 acceptance item (see `docs/spike-wasm-config1.md`).
//! The same assertions also run in a real headless browser via
//! `wasm_browser.rs`; both share the bodies in `wasm_smoke/mod.rs`.
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

mod wasm_smoke;

use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn word_counter_runs_in_wasm() {
    wasm_smoke::word_counter();
}

#[wasm_bindgen_test]
fn chunk_text_runs_in_wasm() {
    wasm_smoke::chunk_text_smoke();
}

#[cfg(feature = "tiktoken")]
#[wasm_bindgen_test]
fn tiktoken_counter_runs_in_wasm() {
    wasm_smoke::tiktoken_counter();
}
