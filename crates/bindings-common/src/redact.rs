//! Config-field redaction shared by all language bindings.
//!
//! Every binding exposes a `config.get()` / `getConfig` / `cg_sdk_config_get`
//! surface that reads back the current `Settings` as a JSON object before
//! marshalling it to the host language. Before that crossing, secret fields
//! must be blanked so that API keys are never echoed out.
//!
//! `cognee_utils::redact` only catches *secret-shaped* substrings (e.g.
//! `sk-…`, `Bearer …`) — a bare value like `"llm_api_key": "abc123"` is NOT
//! caught by it. The explicit allow-list here is the durable, binding-neutral
//! guard against credential leakage.

/// Config keys whose values must be redacted before crossing any binding boundary.
///
/// All three bindings (C API, JS/Neon, Python/PyO3) call
/// [`redact_config_json`] and therefore share this single list. Adding a
/// new secret field here protects every binding at once.
pub const SECRET_FIELDS: &[&str] = &[
    "llm_api_key",
    "llm_fallback_api_key",
    "embedding_api_key",
    "vector_db_key",
    "vector_db_password",
    "graph_database_key",
    "graph_database_password",
    "db_password",
    "cache_password",
    "default_user_password",
    "otel_exporter_otlp_headers",
];

const REDACTED: &str = "***REDACTED***";

/// The current settings of `config` as a JSON object, with every secret
/// masked: the keys in [`SECRET_FIELDS`] plus every key the configuration
/// schema marks as a secret ([`ConfigManager::secret_keys`]), which covers the
/// component section settings flattened into the serialized `Settings` (e.g.
/// `"llm.<adapter>.api_key"`). This is what every binding's `config.get()`
/// returns.
///
/// [`ConfigManager::secret_keys`]: cognee::config::ConfigManager::secret_keys
pub fn redacted_settings_json(
    config: &cognee::config::ConfigManager,
) -> serde_json::Result<serde_json::Value> {
    let secret = config.secret_keys();
    let mut value = serde_json::to_value(&*config.read())?;
    redact_config_json(&mut value);
    redact_keys(&mut value, &secret);
    Ok(value)
}

/// Mask the non-null values of `keys` in a JSON object, in place.
fn redact_keys(value: &mut serde_json::Value, keys: &[&str]) {
    if let Some(obj) = value.as_object_mut() {
        for key in keys {
            if let Some(v) = obj.get_mut(*key)
                && !v.is_null()
            {
                *v = serde_json::Value::String(REDACTED.to_string());
            }
        }
    }
}

/// Redact secret values in a config JSON object in place.
///
/// Every key in [`SECRET_FIELDS`] whose value is non-null is replaced with
/// `"***REDACTED***"`. Null values are left null so the caller can distinguish
/// "key exists but is unset" from "key was set to something".
///
/// The function recurses into nested JSON objects so that both flat configs
/// (`{"llm_api_key": "sk-…"}`) and structured sub-configs
/// (`{"llm": {"api_key": "sk-…"}}`) are protected. The current `Settings`
/// serialization is flat, but the recursion is cheap and future-proofs the
/// helper against schema changes.
pub fn redact_config_json(value: &mut serde_json::Value) {
    redact_keys(value, SECRET_FIELDS);
    if let Some(obj) = value.as_object_mut() {
        // Recurse into nested config sub-objects (e.g. llm/embedding/vector/graph).
        for (_k, v) in obj.iter_mut() {
            if v.is_object() {
                redact_config_json(v);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_all_secret_fields_flat() {
        let mut value = json!({
            "llm_api_key": "sk-abc",
            "llm_fallback_api_key": "sk-fallback",
            "embedding_api_key": "emb-key",
            "vector_db_key": "vec-key",
            "vector_db_password": "vec-pass",
            "graph_database_key": "graph-key",
            "graph_database_password": "graph-pass",
            "db_password": "db-pass",
            "cache_password": "cache-pass",
            "default_user_password": "user-pass",
            "otel_exporter_otlp_headers": "Authorization=Bearer token",
            "llm_model": "gpt-4o",
        });
        redact_config_json(&mut value);
        let obj = value.as_object().expect("still an object");

        // All SECRET_FIELDS must be replaced.
        for field in SECRET_FIELDS {
            assert_eq!(
                obj[*field],
                serde_json::Value::String(REDACTED.to_string()),
                "field {field} was not redacted"
            );
        }
        // Non-secret fields must pass through unchanged.
        assert_eq!(obj["llm_model"], json!("gpt-4o"));
    }

    #[test]
    fn null_values_are_left_null() {
        let mut value = json!({ "llm_api_key": null, "llm_model": "gpt-4o-mini" });
        redact_config_json(&mut value);
        let obj = value.as_object().expect("still an object");
        assert!(obj["llm_api_key"].is_null(), "null must stay null");
        assert_eq!(obj["llm_model"], json!("gpt-4o-mini"));
    }

    #[test]
    fn non_secret_fields_pass_through() {
        let mut value = json!({
            "llm_model": "gpt-4o",
            "llm_provider": "openai",
            "chunk_size": 1024,
            "llm_streaming": true,
        });
        let original = value.clone();
        redact_config_json(&mut value);
        assert_eq!(value, original);
    }

    #[test]
    fn recurses_into_nested_objects() {
        let mut value = json!({
            "llm": {
                "llm_api_key": "sk-nested",
                "llm_model": "gpt-4o",
            },
            "llm_api_key": "sk-top",
        });
        redact_config_json(&mut value);
        let obj = value.as_object().expect("still an object");
        assert_eq!(
            obj["llm_api_key"],
            serde_json::Value::String(REDACTED.to_string()),
            "top-level secret must be redacted"
        );
        let nested = obj["llm"].as_object().expect("llm is still an object");
        assert_eq!(
            nested["llm_api_key"],
            serde_json::Value::String(REDACTED.to_string()),
            "nested secret must be redacted"
        );
        assert_eq!(nested["llm_model"], json!("gpt-4o"));
    }

    #[test]
    fn no_op_on_non_object() {
        let mut value = json!("just a string");
        redact_config_json(&mut value);
        assert_eq!(value, json!("just a string"));
    }

    /// Every `Settings` row marked `secret` in the key registry is masked by
    /// the binding path, including ones missing from `SECRET_FIELDS`.
    #[test]
    fn every_registry_secret_is_redacted() {
        let cm = cognee::config::ConfigManager::new(cognee::config::Settings::default());
        let secrets: Vec<&str> = cognee::config::keys()
            .iter()
            .filter(|spec| spec.secret)
            .map(|spec| spec.key)
            .collect();
        assert!(secrets.contains(&"llm_fallback_api_key"));
        for key in &secrets {
            cm.set(key, json!("hunter2"))
                .expect("secret rows take a string");
        }
        let value = redacted_settings_json(&cm).expect("serializes");
        for key in &secrets {
            assert_eq!(value[*key], json!(REDACTED), "{key} leaked");
        }
        assert!(!value.to_string().contains("hunter2"));
    }

    cognee_config_core::config_section! {
        /// A section with a credential.
        pub struct SecretDemo in "test.redact" {
            /// A token.
            pub api_key: String = String::new() => { env: [], alias: [], secret: true },
            /// Not a secret.
            pub model: String = String::new() => { env: [], alias: [], secret: false },
        }
    }

    /// A section setting declared `secret: true` is masked even though it is
    /// flattened into the serialized `Settings` under a dotted key.
    #[test]
    fn secret_section_keys_are_redacted() {
        use cognee_config_core::{ConfigSchema, ConfigSection};
        let cm = cognee::config::ConfigManager::new(cognee::config::Settings::default());
        let mut schema = ConfigSchema::new();
        schema.add(SecretDemo::schema()).expect("no collision");
        cm.extend_schema(&schema).expect("no collision");
        cm.set("test.redact.api_key", json!("sk-section"))
            .expect("set");
        cm.set("test.redact.model", json!("m")).expect("set");
        let value = redacted_settings_json(&cm).expect("serializes");
        assert_eq!(value["test.redact.api_key"], json!(REDACTED));
        assert_eq!(value["test.redact.model"], json!("m"));
    }
}
