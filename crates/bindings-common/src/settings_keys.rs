//! Resolving the key spellings a binding's settings object may use.

use std::borrow::Cow;

use cognee::config::{builtin_schema, key_spec};

/// The key `key` should be stored under when a binding merges a settings
/// object onto env-derived [`Settings`](cognee::config::Settings).
///
/// - An alias of a component section setting (`llm_api_key`) becomes its
///   canonical key (`llm.api_key`). The env-derived base already holds the
///   canonical key, and when both spellings are present the canonical one
///   wins, so an alias left as-is would lose to the env value it was meant
///   to override.
/// - A section key with its dots flattened to underscores
///   (`llm_debug_requests` for `llm.debug_requests`) is accepted too: it is
///   what the camelCase read-back key of an alias-less section setting
///   (`llmDebugRequests`, from a binding's `config.get()`) turns into on its
///   way back in, so a get-then-construct round trip keeps every setting. (A
///   section key with an alias reads back under the alias's camelCase, which
///   the first rule already maps.)
/// - Anything else (a `Settings` key or alias, an unknown key) is unchanged.
pub fn canonical_settings_key(key: &str) -> Cow<'_, str> {
    let schema = builtin_schema();
    if let Some(def) = schema.key_def(key) {
        return Cow::Borrowed(def.key);
    }
    if key_spec(key).is_some() {
        return Cow::Borrowed(key);
    }
    schema
        .key_defs()
        .find(|def| def.key.contains('.') && def.key.replace('.', "_") == key)
        .map_or(Cow::Borrowed(key), |def| Cow::Borrowed(def.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TS SDK's `snakeToCamel` (dots and underscores are word breaks).
    fn snake_to_camel(key: &str) -> String {
        let mut out = String::new();
        let mut upper = false;
        for c in key.chars() {
            if c == '_' || c == '.' {
                upper = true;
            } else if upper {
                out.extend(c.to_uppercase());
                upper = false;
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The TS SDK's `camelToSnake`.
    fn camel_to_snake(key: &str) -> String {
        let mut out = String::new();
        for c in key.chars() {
            if c.is_ascii_uppercase() {
                out.push('_');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn aliases_resolve_to_canonical_keys() {
        assert_eq!(canonical_settings_key("llm_api_key"), "llm.api_key");
        assert_eq!(
            canonical_settings_key("transcription_model"),
            "llm.openai_compatible.transcription_model"
        );
        assert_eq!(canonical_settings_key("llm.model"), "llm.model");
        assert_eq!(canonical_settings_key("chunk_size"), "chunk_size");
        assert_eq!(canonical_settings_key("no_such_key"), "no_such_key");
    }

    /// Every section key a binding's `config.get()` emits in camelCase
    /// comes back to the same key through the constructor.
    #[test]
    fn every_section_key_survives_a_camel_case_round_trip() {
        for def in builtin_schema().key_defs() {
            let back = camel_to_snake(&snake_to_camel(def.key));
            assert_eq!(
                canonical_settings_key(&back),
                def.key,
                "{} -> {} does not resolve back",
                def.key,
                back
            );
        }
    }
}
