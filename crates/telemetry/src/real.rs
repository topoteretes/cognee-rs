//! Real (`feature = "telemetry"`) dispatcher for `send_telemetry`.
//!
//! Assembles the [`TelemetryPayload`], applies URL sanitization, and
//! fires the POST on a detached `tokio::spawn`. When called outside
//! a tokio runtime, falls back to a one-shot single-thread runtime
//! per locked decision 5.

use serde_json::Value;

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
            handle.spawn(post(body));
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
