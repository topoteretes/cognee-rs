//! **Deprecated — renamed to the [`cognee`] crate.**
//!
//! `cognee-lib` has been renamed to [`cognee`](https://crates.io/crates/cognee).
//! This crate is a thin re-export kept for backwards compatibility so existing
//! `cognee-lib` dependents keep compiling. New code should depend on `cognee`
//! directly (`cargo add cognee`) and import from it:
//!
//! ```ignore
//! // old
//! use cognee_lib::api::remember;
//! // new
//! use cognee::api::remember;
//! ```
//!
//! Everything from `cognee` is re-exported unchanged below, and every Cargo
//! feature forwards 1:1 to the `cognee` crate.
//!
//! [`cognee`]: https://crates.io/crates/cognee
pub use cognee::*;
