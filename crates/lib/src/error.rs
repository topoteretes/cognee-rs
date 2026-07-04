//! Error types for the cognee-lib crate.
//!
//! [`ComponentError`] is defined in `cognee-components` and re-exported here so
//! that `cognee_lib::ComponentError` stays the identical type across the OSS
//! crates and the closed cloud repo (preserving `#[from]` / `From` impls in the
//! bindings and CLI error enums).

pub use cognee_components::ComponentError;
