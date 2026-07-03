//! WebAssembly smoke test for the Config-1 spike — **headless browser** runner.
//!
//! Identical assertions to `wasm.rs` (shared via `wasm_smoke/mod.rs`), but
//! `wasm_bindgen_test_configure!(run_in_browser)` makes the test runner drive a
//! real browser through a WebDriver instead of executing under Node. This proves
//! the wasm artifact runs on the *actual* target the repo owner asked for —
//! `wasm32-unknown-unknown` + JS glue in the browser — not just under Node.
//!
//! The whole file is gated to `wasm32`; on native targets it compiles to an
//! empty crate.
//!
//! Run it (WSL/Linux) with a browser + matching WebDriver available. With
//! Chrome for Testing unpacked under `~/.cft`:
//!
//! ```bash
//! export CHROMEDRIVER="$HOME/.cft/chromedriver-linux64/chromedriver"
//! export CHROME="$HOME/.cft/chrome-linux64/chrome"   # binary the driver launches
//! # the runner launches the browser headless by default (set NO_HEADLESS=1 to see it)
//!
//! cargo test -p cognee-chunking --target wasm32-unknown-unknown --test wasm_browser
//! cargo test -p cognee-chunking --features tiktoken \
//!     --target wasm32-unknown-unknown --test wasm_browser
//! ```
//!
//! Geckodriver/Firefox works too (`GECKODRIVER` env var) — any WebDriver the
//! `wasm-bindgen-test-runner` recognizes.

#![cfg(target_arch = "wasm32")]

mod wasm_smoke;

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn word_counter_runs_in_browser() {
    wasm_smoke::word_counter();
}

#[wasm_bindgen_test]
fn chunk_text_runs_in_browser() {
    wasm_smoke::chunk_text_smoke();
}

#[cfg(feature = "tiktoken")]
#[wasm_bindgen_test]
fn tiktoken_counter_runs_in_browser() {
    wasm_smoke::tiktoken_counter();
}
