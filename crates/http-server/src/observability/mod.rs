//! In-process observability tier — span buffer, redaction, and the `tracing`
//! layer that feeds them.
//!
//! See [`docs/http-server/observability.md`](../../../../../docs/http-server/observability.md)
//! for the design.

pub mod redaction;
pub mod span_buffer;
pub mod span_buffer_layer;

pub use span_buffer::{
    BufferConfig, BufferStats, RecordedSpan, SpanBuffer, SpanStatus, TraceSummary,
};
pub use span_buffer_layer::SpanBufferLayer;
