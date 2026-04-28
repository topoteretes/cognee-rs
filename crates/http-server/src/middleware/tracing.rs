//! Request tracing middleware.
//!
//! `trace_layer` returns a `tower_http::trace::TraceLayer` that emits a tracing
//! span for every HTTP request with `method`, `uri`, `status`, and `latency_ms`
//! fields.
//!
//! **Important**: this module does NOT install a global tracing subscriber.
//! The standalone binary's `init_tracing()` does that.  Library embedders
//! install their own subscriber.  Keeping the layer separate makes the access-log
//! shape consistent across all entry points.
//!
//! ## Header redaction
//!
//! [`REDACTED_HEADERS`] lists the request headers excluded from access-log
//! span attributes. The list is case-insensitive — see [`is_header_redacted`].
//! Authorization, Cookie, X-Api-Key are redacted by default per
//! [`docs/http-server/observability.md §7`](../../../docs/http-server/observability.md#7-access-logging).

use std::time::Duration;

use tower_http::trace::{DefaultOnResponse, MakeSpan, TraceLayer};
use tracing::Level;

/// Build the request tracing tower layer.
///
/// - On failure: logs at `error`.
/// - On response: logs at `debug` (access-log filter drops sub-`warn` for
///   `/health` in the subscriber `EnvFilter` — see the binary's `init_tracing`).
pub fn trace_layer() -> TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
    HttpMakeSpan,
> {
    TraceLayer::new_for_http()
        .make_span_with(HttpMakeSpan)
        .on_response(
            DefaultOnResponse::new()
                .level(Level::DEBUG)
                .latency_unit(tower_http::LatencyUnit::Millis),
        )
}

/// Custom span maker that records `method` and `uri` at span creation time.
#[derive(Clone, Debug)]
pub struct HttpMakeSpan;

impl<B> MakeSpan<B> for HttpMakeSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> tracing::Span {
        tracing::span!(
            Level::DEBUG,
            "http_request",
            method = %request.method(),
            uri = %request.uri(),
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        )
    }
}

/// Headers excluded from access-log span attributes.
///
/// Case-insensitive. Match against an incoming header name with
/// [`is_header_redacted`].
pub const REDACTED_HEADERS: &[&str] = &["authorization", "cookie", "x-api-key"];

/// Returns `true` if the supplied header name should be redacted from access
/// logs.
pub fn is_header_redacted(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    REDACTED_HEADERS.iter().any(|h| **h == lower)
}

/// Convenience re-export so callers can write `middleware::tracing::trace_layer()`.
pub use self::trace_layer as make_trace_layer;

/// Format a [`Duration`] as integer milliseconds for the `latency_ms` span field.
#[allow(dead_code)]
pub fn duration_ms(d: Duration) -> u64 {
    d.as_millis() as u64
}
