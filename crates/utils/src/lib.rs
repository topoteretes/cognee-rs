//! Shared utilities for the Cognee codebase.
//!
//! This crate provides common functionality used across multiple Cognee crates,
//! including retry logic, ID generation, and other utilities.

pub mod id_generation;
pub mod retry;

pub use id_generation::{NAMESPACE_OID, generate_edge_name, generate_node_id, generate_node_name};
pub use retry::{RetryConfig, RetryDecision, retry_with_backoff};
