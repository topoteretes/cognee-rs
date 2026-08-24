//! Real (`feature = "telemetry"`) dispatcher for `send_telemetry`.
//!
//! Assembles the [`TelemetryPayload`], applies URL sanitization, and
//! fires the POST on a detached `tokio::spawn`. When called outside
//! a tokio runtime, falls back to a one-shot single-thread runtime
//! per locked decision 5.
//!
//! The spawned POSTs are counted so [`flush_impl`] can wait for them:
//! `spawn` + an immediate runtime shutdown delivers **nothing**.
//! Measured against a local stub collector: a `send_telemetry`
//! followed straight away by dropping the runtime delivered **0 of 1**
//! POSTs; with a short grace period, 1 of 1. Every explicit teardown
//! path (the CLI, the HTTP server's `on_shutdown`, the bindings'
//! handle teardown) therefore ends with a bounded flush.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Notify;

use crate::UserIdRef;
use crate::client::client;
use crate::env::{is_disabled, proxy_url, request_timeout_secs};
use crate::ids::{get_anonymous_id, get_api_key_tracking_id, get_persistent_id};
use crate::payload::{
    AdditionalProperties, Properties, TelemetryPayload, UserProperties, format_time_field,
};
use crate::sanitize::sanitize_nested_properties;

/// Real implementation of `send_telemetry`. Returns immediately;
/// the HTTP POST is dispatched on a detached tokio task. When called
/// outside a tokio runtime, falls back to a one-shot single-thread
/// runtime (decision 5) and blocks the calling thread up to
/// `TELEMETRY_REQUEST_TIMEOUT` (default 5s, clamped `[1, 60]`).
/// Outstanding detached POSTs, so a teardown can wait for them.
///
/// A counter plus a `Notify` rather than a `JoinSet`/`TaskTracker`: the
/// dispatcher is a free function reachable from anywhere (including
/// outside a runtime), so there is no owner to hold a join set, and the
/// only question a flush needs answered is "is anything still in
/// flight". Nothing is ever *awaited* by the dispatcher itself, which
/// keeps `send_telemetry` fire-and-forget.
struct Inflight {
    count: AtomicUsize,
    idle: Notify,
}

static INFLIGHT: OnceLock<Inflight> = OnceLock::new();

/// Decrements the in-flight count when the POST task ends — **including when the
/// task is cancelled**, which is what a runtime shutdown does to every pending
/// task.
///
/// A plain `fetch_sub` at the end of the task body would be skipped in exactly
/// that case, permanently inflating the counter, and every later `flush` would
/// then wait out its whole timeout for a POST that no longer exists. Dropping the
/// future runs this instead.
struct InflightGuard(&'static Inflight);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        // `notify_waiters` (not `notify_one`) because a flush may not be waiting
        // yet, and a stored permit would make the *next* flush return early. The
        // counter is the source of truth; this only wakes an existing waiter to
        // re-read it.
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

fn inflight() -> &'static Inflight {
    INFLIGHT.get_or_init(|| Inflight {
        count: AtomicUsize::new(0),
        idle: Notify::new(),
    })
}

/// Wait until every in-flight telemetry POST has finished, or `timeout`
/// elapses. Returns `true` if the queue drained.
///
/// **Hard-bounded on purpose.** A blackholed or very slow collector must
/// never hang a process exit, and telemetry must never fail one, so the
/// timeout is honoured and the result is advisory — callers log it at
/// most.
pub(crate) async fn flush_impl(timeout: Duration) -> bool {
    let inflight = inflight();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Register interest *before* reading the counter: the reverse
        // order can miss the notification of the last completing POST
        // and then wait out the whole timeout for nothing.
        let idle = inflight.idle.notified();
        if inflight.count.load(Ordering::Acquire) == 0 {
            return true;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if tokio::time::timeout(remaining, idle).await.is_err() {
            return inflight.count.load(Ordering::Acquire) == 0;
        }
    }
}

pub(crate) fn send_telemetry_impl(
    event_name: &str,
    user_id: UserIdRef<'_>,
    additional_properties: Option<Value>,
) {
    if is_disabled() {
        return;
    }

    let body = build_body(event_name, user_id, additional_properties);

    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // Count before spawning, so a flush that starts between these
            // two lines still observes this POST.
            let tracked = inflight();
            tracked.count.fetch_add(1, Ordering::AcqRel);
            let guard = InflightGuard(tracked);
            handle.spawn(async move {
                let _guard = guard;
                post(body).await;
            });
        }
        Err(_) => {
            tracing::warn!(
                target: "cognee.telemetry",
                "send_telemetry called from a non-tokio context; \
                 spinning up a one-shot runtime (decision 5). \
                 Consider calling from an async context for better \
                 performance."
            );
            spin_up_one_shot(body);
        }
    }
}

fn build_body(
    event_name: &str,
    user_id: UserIdRef<'_>,
    additional_properties: Option<Value>,
) -> Value {
    let anon = get_anonymous_id();
    let persistent = get_persistent_id();
    let tracking = get_api_key_tracking_id();
    let user = match user_id {
        UserIdRef::Uuid(u) => u.to_string(),
        UserIdRef::Symbolic(s) => s.to_string(),
        UserIdRef::None => String::new(),
    };

    // Sanitize URL keys before assembling the payload.
    let mut additional = AdditionalProperties::from_value(additional_properties);
    let mut as_value = additional.as_value_mut();
    sanitize_nested_properties(&mut as_value, &["url"]);
    additional.replace_with(as_value);

    let payload = TelemetryPayload {
        anonymous_id: &anon,
        event_name,
        user_properties: UserProperties {
            user_id: &user,
            persistent_id: &persistent,
            api_key_tracking_id: &tracking,
            api_key_hash: &tracking,
        },
        properties: Properties {
            time: format_time_field(chrono::Utc::now()),
            user_id: &user,
            anonymous_id: &anon,
            persistent_id: &persistent,
            api_key_tracking_id: &tracking,
            api_key_hash: &tracking,
            sdk_runtime: "rust",
            cognee_version: env!("CARGO_PKG_VERSION"),
            additional,
        },
    };

    // Serialize once. The schema is fully owned by us; failure is
    // impossible in practice, but we degrade gracefully rather than
    // panic if a future schema change introduces a non-serialisable
    // variant.
    serde_json::to_value(&payload).unwrap_or_else(|e| {
        tracing::debug!(
            target: "cognee.telemetry",
            error = %e,
            "telemetry payload serialization failed"
        );
        Value::Null
    })
}

async fn post(body: Value) {
    if body.is_null() {
        return;
    }
    let url = proxy_url();
    match client().post(&url).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            tracing::debug!(
                target: "cognee.telemetry",
                status = %resp.status(),
                "telemetry proxy returned non-2xx"
            );
        }
        Err(e) => {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                "telemetry request failed"
            );
        }
        _ => {}
    }
}

/// The non-runtime fallback needs no tracking: it blocks the calling
/// thread until the POST completes (or times out), so by the time it
/// returns there is nothing left in flight.
fn spin_up_one_shot(body: Value) {
    let timeout = std::time::Duration::from_secs(request_timeout_secs());
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                "could not bootstrap one-shot tokio runtime; dropping event"
            );
            return;
        }
    };
    rt.block_on(async move {
        let _ = tokio::time::timeout(timeout, post(body)).await;
    });
}
