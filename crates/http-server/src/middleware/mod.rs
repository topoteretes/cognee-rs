//! Tower middleware layers for the cognee HTTP server.
//!
//! - `cors` — CORS layer matching Python's `add_cors_middleware`.
//! - `tracing` — per-request access-log span layer.
//! - `validation` — custom JSON extractor that emits `ApiError::Validation`.

pub mod cors;
pub mod tracing;
pub mod validation;
