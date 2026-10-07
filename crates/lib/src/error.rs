//! Error types for the cognee crate.
//!
//! [`ComponentError`] is defined in `cognee-components` and re-exported here so
//! that `cognee::ComponentError` stays the identical type across the OSS
//! crates and downstream consumers (preserving `#[from]` / `From` impls in the
//! bindings and CLI error enums).

pub use cognee_components::ComponentError;
