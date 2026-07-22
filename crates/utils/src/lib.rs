//! Shared utilities for the Cognee codebase.
//!
//! This crate provides common functionality used across multiple Cognee crates,
//! including retry logic, ID generation, and other utilities.

pub mod env;
pub mod id_generation;
pub mod redact;
pub mod retry;
pub mod tracing_keys;

pub use env::parse_env_bool;
pub use id_generation::{
    NAMESPACE_OID, data_point_id_for, generate_edge_name, generate_node_id, generate_node_name,
    normalize_identifier,
};
pub use redact::redact;
pub use retry::{RetryConfig, RetryDecision, retry_with_backoff};
