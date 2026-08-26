//! Shared utilities for the Cognee codebase.
//!
//! This crate provides common functionality used across multiple Cognee crates,
//! including retry logic, ID generation, and other utilities.

pub mod env;
pub mod id_generation;
// Reactive dispatch pacing. Gated off wasm32: it reads the monotonic clock via
// `std::time::Instant::now()`, which panics on `wasm32-unknown-unknown`. The
// providers it paces are reached over reqwest, which is native-only anyway.
#[cfg(not(target_arch = "wasm32"))]
pub mod pacing;
pub mod redact;
pub mod retry;
pub mod sanitize;
pub mod tracing_keys;

pub use env::parse_env_bool;
pub use id_generation::{
    NAMESPACE_OID, data_point_id_for, generate_edge_name, generate_node_id, generate_node_name,
    normalize_identifier,
};
pub use redact::redact;
pub use retry::{RetryConfig, RetryDecision, retry_with_backoff};
pub use sanitize::{sanitize_json, sanitize_json_in_place, sanitize_str, sanitize_string};
