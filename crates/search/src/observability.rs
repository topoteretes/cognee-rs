//! Semantic attribute constant names for cognee search telemetry.
//!
//! The canonical declarations live in
//! [`cognee_utils::tracing_keys`](../../utils/src/tracing_keys.rs) so
//! adapter and search call sites import the same constants. This
//! module re-exports them as a backwards-compat alias for existing
//! `cognee_search::observability::COGNEE_*` users.

pub use cognee_utils::tracing_keys::*;
