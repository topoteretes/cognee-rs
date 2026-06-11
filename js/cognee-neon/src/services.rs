//! Re-export of [`CogneeServices`] from `cognee-bindings-common`.
//!
//! The implementation has moved to `cognee_bindings_common::services` so it can
//! be shared with the C API binding. This module keeps the original module path
//! (`crate::services::CogneeServices`) working for existing call-sites within
//! `cognee-neon`.

pub use cognee_bindings_common::CogneeServices;
