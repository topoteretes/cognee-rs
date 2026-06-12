//! Shared pipeline operation implementations.
//!
//! Each submodule contains the pure-Rust async logic for a set of SDK
//! operations. Binding-specific wrappers (C string parsing, Neon promise
//! settling, PyO3 `future_into_py`, etc.) live in the individual binding
//! crates and call through to these shared functions.

pub mod data;
pub mod datasets;
pub mod pipeline;
pub mod retrieval;
