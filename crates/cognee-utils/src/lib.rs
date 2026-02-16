//! Shared utilities for the Cognee codebase.
//!
//! This crate provides common functionality used across multiple Cognee crates,
//! including retry logic, helpers, and other utilities.

pub mod retry;

pub use retry::{RetryConfig, RetryDecision, retry_with_backoff};
