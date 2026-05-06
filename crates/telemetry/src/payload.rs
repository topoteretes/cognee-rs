//! Strongly-typed serde model of the `send_telemetry` proxy payload.
//!
//! Field-for-field parity with Python's
//! `cognee.shared.utils.send_telemetry` (utils.py:176-228). Includes
//! the backward-compat `api_key_hash` alias (utils.py:226) which
//! carries the same value as `api_key_tracking_id`.

#[cfg(feature = "telemetry")]
use serde::Serialize;
#[cfg(feature = "telemetry")]
use serde_json::Value;

/// Top-level proxy payload, dispatched as the body of
/// `POST https://test.prometh.ai`.
#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct TelemetryPayload<'a> {
    /// Project-local uuid4 from `<project_root>/.anon_id`.
    pub anonymous_id: &'a str,
    /// Caller-supplied event name (e.g. `"cognee.forget"`).
    pub event_name: &'a str,
    /// Identity tuple repeated under the `user_properties` view.
    pub user_properties: UserProperties<'a>,
    /// Identity tuple plus `time` and the spread of caller-supplied
    /// `additional_properties` (after URL sanitization).
    pub properties: Properties<'a>,
}

/// Identity tuple under the `user_properties` view. Mirrors Python's
/// nested object so dashboards that flatten only `user_properties`
/// still see the full identity triplet.
#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct UserProperties<'a> {
    /// Cognee `User.id` or symbolic identifier (e.g. `"sdk"`).
    pub user_id: &'a str,
    /// Persistent device identifier (uuid5 from machine-id-derived seed).
    pub persistent_id: &'a str,
    /// HMAC-derived API-key tracking id (empty string when no key set).
    pub api_key_tracking_id: &'a str,
    /// Backward-compat alias of `api_key_tracking_id`. Same value.
    pub api_key_hash: &'a str,
}

/// Wide identity + version + sanitized caller properties under the
/// `properties` view. Caller-supplied `additional_properties` are
/// flattened into this object on the wire — Python spreads the dict.
///
/// Reserved field names — callers MUST NOT pass any of these in
/// `additional_properties` (collisions produce duplicate JSON keys
/// with implementation-defined deduplication on the consumer side):
/// `time`, `user_id`, `anonymous_id`, `persistent_id`,
/// `api_key_tracking_id`, `api_key_hash`, `sdk_runtime`,
/// `cognee_version`.
#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct Properties<'a> {
    /// `MM/DD/YYYY` of the current date — Python's
    /// `current_time.strftime("%m/%d/%Y")`.
    pub time: String,
    /// Identity tuple, repeated for analytics dashboards that flatten
    /// only the `properties` view.
    pub user_id: &'a str,
    /// Identity tuple, repeated for analytics dashboards that flatten
    /// only the `properties` view.
    pub anonymous_id: &'a str,
    /// Identity tuple, repeated for analytics dashboards that flatten
    /// only the `properties` view.
    pub persistent_id: &'a str,
    /// Identity tuple, repeated for analytics dashboards that flatten
    /// only the `properties` view.
    pub api_key_tracking_id: &'a str,
    /// Backward-compat alias of `api_key_tracking_id`. Same value.
    pub api_key_hash: &'a str,
    /// `sdk_runtime: "rust"` — added per locked decision 2 so the
    /// proxy can distinguish Rust vs Python events without losing
    /// cross-SDK identity grouping.
    pub sdk_runtime: &'static str,
    /// Cognee crate version — `env!("CARGO_PKG_VERSION")`.
    pub cognee_version: &'static str,
    /// Caller-supplied properties, already sanitized by
    /// `sanitize_nested_properties` (URL keys hashed). Flattened into
    /// the parent object on the wire — Python spreads the dict.
    #[serde(flatten)]
    pub additional: AdditionalProperties,
}

/// A `serde_json::Value::Object` flattened into [`Properties`]. Modelled
/// as a wrapper so the `#[serde(flatten)]` works correctly on a
/// `Value` and so we can hand mutable access out for sanitization.
#[cfg(feature = "telemetry")]
#[derive(Debug, Default, Serialize)]
#[serde(transparent)]
pub struct AdditionalProperties {
    inner: serde_json::Map<String, Value>,
}

#[cfg(feature = "telemetry")]
impl AdditionalProperties {
    /// Construct from a caller-provided `Value::Object`. Anything
    /// other than an object (e.g. `Value::Array`, `Value::String`)
    /// is dropped with a `tracing::debug` log and treated as empty —
    /// Python coerces silently, we diverge for safety since the
    /// payload contract requires a flat object.
    pub fn from_value(v: Option<Value>) -> Self {
        match v {
            Some(Value::Object(map)) => Self { inner: map },
            Some(other) => {
                tracing::debug!(
                    target: "cognee.telemetry",
                    actual_type = std::any::type_name_of_val(&other),
                    "additional_properties was not an object; dropping"
                );
                Self::default()
            }
            None => Self::default(),
        }
    }

    /// Take the inner map out as a [`Value::Object`], leaving `self`
    /// empty. Pair with [`Self::replace_with`] after sanitizing.
    pub fn as_value_mut(&mut self) -> Value {
        Value::Object(std::mem::take(&mut self.inner))
    }

    /// Restore from a sanitized [`Value`]. Non-object values are
    /// silently dropped (defensive — sanitization should never change
    /// the outer type).
    pub fn replace_with(&mut self, v: Value) {
        if let Value::Object(map) = v {
            self.inner = map;
        }
    }
}

/// Format the current date as `MM/DD/YYYY` to match Python's
/// `current_time.strftime("%m/%d/%Y")` (utils.py:206).
#[cfg(feature = "telemetry")]
pub fn format_time_field(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%m/%d/%Y").to_string()
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn time_field_format() {
        let when = chrono::DateTime::parse_from_rfc3339("2026-05-06T12:00:00Z")
            .expect("rfc3339 fixture")
            .with_timezone(&chrono::Utc);
        assert_eq!(format_time_field(when), "05/06/2026");
    }

    #[test]
    fn payload_roundtrips_to_python_compatible_json() {
        let additional = AdditionalProperties::from_value(Some(json!({
            "endpoint": "POST /api/v1/forget",
        })));
        let payload = TelemetryPayload {
            anonymous_id: "a-id",
            event_name: "cognee.forget",
            user_properties: UserProperties {
                user_id: "u-id",
                persistent_id: "p-id",
                api_key_tracking_id: "ak_deadbeefcafebabe0123456789abcdef",
                api_key_hash: "ak_deadbeefcafebabe0123456789abcdef",
            },
            properties: Properties {
                time: "05/06/2026".into(),
                user_id: "u-id",
                anonymous_id: "a-id",
                persistent_id: "p-id",
                api_key_tracking_id: "ak_deadbeefcafebabe0123456789abcdef",
                api_key_hash: "ak_deadbeefcafebabe0123456789abcdef",
                sdk_runtime: "rust",
                cognee_version: "0.1.0",
                additional,
            },
        };
        let v = serde_json::to_value(&payload).expect("serialize");
        // Spot-check the wire schema.
        assert_eq!(v["anonymous_id"], "a-id");
        assert_eq!(v["event_name"], "cognee.forget");
        assert_eq!(
            v["user_properties"]["api_key_hash"],
            v["user_properties"]["api_key_tracking_id"]
        );
        assert_eq!(v["properties"]["sdk_runtime"], "rust");
        assert_eq!(v["properties"]["time"], "05/06/2026");
        // additional_properties were flattened.
        assert_eq!(v["properties"]["endpoint"], "POST /api/v1/forget");
    }

    #[test]
    fn from_value_drops_non_object() {
        let arr = AdditionalProperties::from_value(Some(json!([1, 2, 3])));
        let out = serde_json::to_value(&arr).expect("serialize");
        assert_eq!(out, json!({}));

        let none = AdditionalProperties::from_value(None);
        let out = serde_json::to_value(&none).expect("serialize");
        assert_eq!(out, json!({}));
    }
}
