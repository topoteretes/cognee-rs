//! Component-owned configuration sections.
//!
//! A component (an LLM adapter, a vector or graph backend, a server module)
//! declares the settings it reads as a typed section with
//! [`config_section!`](crate::config_section). Registering the component
//! registers its section in a [`ConfigSchema`], and the schema is what the
//! config loaders consult. So a component that is not compiled in, or not
//! registered, contributes no keys: its env vars are not read and setting its
//! keys is an unknown-key condition (see [`unknown_key`]).
//!
//! Section values travel as JSON in a [`ConfigValues`] map keyed by the
//! canonical, namespaced key (`"graph.ladybug.max_db_size"`). Every value is
//! checked against its [`KeyDef`] when it enters the map, so turning the map
//! back into the typed section ([`ConfigSection::from_values`]) cannot fail.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::EnvSource;

/// Section values keyed by canonical key.
pub type ConfigValues = BTreeMap<String, Value>;

/// A Rust type a setting can hold: how it is parsed from an env var, and how
/// it maps to and from JSON.
pub trait ConfigValue: Sized {
    /// Parse a raw, non-empty env value. `None` means "malformed": the value is
    /// ignored and the setting keeps its previous value.
    fn parse_env(raw: &str) -> Option<Self>;
    /// Convert a JSON value, or explain why it has the wrong type.
    fn from_json(value: &Value) -> Result<Self, String>;
    /// The JSON form of the value.
    fn to_json(&self) -> Value;
}

impl ConfigValue for String {
    fn parse_env(raw: &str) -> Option<Self> {
        Some(raw.to_string())
    }
    fn from_json(value: &Value) -> Result<Self, String> {
        value
            .as_str()
            .map(ToString::to_string)
            .ok_or_else(|| "expected a string".to_string())
    }
    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }
}

impl ConfigValue for bool {
    /// `true | 1 | yes | on` (trimmed, case-insensitive) is true; any other
    /// value is false.
    fn parse_env(raw: &str) -> Option<Self> {
        Some(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ))
    }
    fn from_json(value: &Value) -> Result<Self, String> {
        value
            .as_bool()
            .ok_or_else(|| "expected a boolean".to_string())
    }
    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

macro_rules! unsigned_config_value {
    ($($t:ty),*) => {$(
        impl ConfigValue for $t {
            fn parse_env(raw: &str) -> Option<Self> {
                raw.parse().ok()
            }
            fn from_json(value: &Value) -> Result<Self, String> {
                value
                    .as_u64()
                    .and_then(|n| <$t>::try_from(n).ok())
                    .ok_or_else(|| {
                        format!("expected a positive integer ({})", stringify!($t))
                    })
            }
            fn to_json(&self) -> Value {
                Value::from(*self)
            }
        }
    )*};
}
unsigned_config_value!(u16, u32, u64, usize);

impl ConfigValue for f64 {
    fn parse_env(raw: &str) -> Option<Self> {
        raw.parse().ok()
    }
    fn from_json(value: &Value) -> Result<Self, String> {
        value
            .as_f64()
            .ok_or_else(|| "expected a number".to_string())
    }
    fn to_json(&self) -> Value {
        Value::from(*self)
    }
}

/// `Option<T>`: JSON `null` means unset.
impl<T: ConfigValue> ConfigValue for Option<T> {
    fn parse_env(raw: &str) -> Option<Self> {
        T::parse_env(raw).map(Some)
    }
    fn from_json(value: &Value) -> Result<Self, String> {
        if value.is_null() {
            Ok(None)
        } else {
            T::from_json(value).map(Some)
        }
    }
    fn to_json(&self) -> Value {
        self.as_ref().map_or(Value::Null, ConfigValue::to_json)
    }
}

/// One setting of a section.
#[derive(Debug)]
pub struct KeyDef {
    /// Canonical, namespaced key: `"<section>.<field>"`.
    pub key: &'static str,
    /// Plain aliases, typically the Python SDK's flat name.
    pub aliases: &'static [&'static str],
    /// Env vars that bind the setting, in priority order. A later variable is
    /// read only when every earlier one is unset or empty.
    pub env: &'static [&'static str],
    /// Whether the value is a credential that must not be shown unmasked.
    pub secret: bool,
    /// The field's doc comment.
    pub doc: &'static str,
    /// The default value, as JSON.
    pub default: fn() -> Value,
    /// Parse a raw env value into JSON; `None` ignores a malformed value.
    pub parse_env: fn(&str) -> Option<Value>,
    /// Check a JSON value and return its canonical form.
    pub validate: fn(&Value) -> Result<Value, String>,
}

/// A component's configuration section: its name and its settings.
#[derive(Debug)]
pub struct SectionSchema {
    /// Section name, the prefix of every key in it (`"graph.ladybug"`).
    pub name: &'static str,
    /// The section's doc comment.
    pub doc: &'static str,
    /// The settings.
    pub keys: &'static [KeyDef],
}

/// A typed configuration section, declared with
/// [`config_section!`](crate::config_section).
pub trait ConfigSection: Default {
    /// The section's schema.
    fn schema() -> &'static SectionSchema;
    /// Build the section from defaults plus the values present in `values`.
    fn from_values(values: &ConfigValues) -> Self;
}

/// Why a key could not be set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// No registered setting has this key or alias.
    #[error("Unknown config key: {0}")]
    Unknown(String),
    /// The value has the wrong type for the setting.
    #[error("Type mismatch for key '{key}': {reason}")]
    Invalid {
        /// The key, as given.
        key: String,
        /// What was expected.
        reason: String,
    },
}

/// Two registered settings claim the same key, alias or env var.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("config key collision: {what} {name:?} is claimed by both {first} and {second}")]
pub struct SchemaError {
    /// `"key"` or `"env var"`.
    pub what: &'static str,
    /// The contested name.
    pub name: String,
    /// The owner registered first.
    pub first: String,
    /// The owner that tried to register it again.
    pub second: String,
}

/// Apply the unknown-key policy: with `strict`, an unknown key is an error;
/// otherwise it is logged as a warning and ignored.
///
/// A typo and a key belonging to a component that is not compiled in look the
/// same here, because an absent component registers nothing; the warning says
/// so.
pub fn unknown_key(key: &str, strict: bool) -> Result<(), KeyError> {
    if strict {
        return Err(KeyError::Unknown(key.to_string()));
    }
    tracing::warn!(
        key,
        "ignoring unknown config key: it is misspelled, or belongs to a component \
         that is not compiled into or registered with this build \
         (set strict_config / COGNEE_STRICT_CONFIG=true to make this an error)"
    );
    Ok(())
}

/// The registered sections, plus the names reserved by the settings that are
/// not in a section (the core `Settings` fields), for collision checks.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema {
    sections: Vec<&'static SectionSchema>,
    /// key or alias -> owner
    keys: BTreeMap<&'static str, String>,
    /// primary env var -> owner
    env: BTreeMap<&'static str, String>,
}

impl ConfigSchema {
    /// An empty schema.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve keys and primary env vars owned outside any section, so a
    /// section cannot claim them.
    pub fn reserve(
        &mut self,
        owner: &str,
        keys: impl IntoIterator<Item = &'static str>,
        env: impl IntoIterator<Item = &'static str>,
    ) -> Result<(), SchemaError> {
        for key in keys {
            Self::claim(&mut self.keys, "key", key, owner)?;
        }
        for var in env {
            Self::claim(&mut self.env, "env var", var, owner)?;
        }
        Ok(())
    }

    /// Register a section. Registering the same section twice is a no-op, so
    /// two components that share a section (e.g. the AWS credentials of the
    /// Bedrock LLM and the Bedrock embedder) can both declare it.
    pub fn add(&mut self, section: &'static SectionSchema) -> Result<(), SchemaError> {
        if self.sections.iter().any(|s| s.name == section.name) {
            return Ok(());
        }
        let owner = format!("section {:?}", section.name);
        let mut keys = self.keys.clone();
        let mut env = self.env.clone();
        for def in section.keys {
            for name in std::iter::once(def.key).chain(def.aliases.iter().copied()) {
                Self::claim(&mut keys, "key", name, &owner)?;
            }
            if let Some(primary) = def.env.first() {
                Self::claim(&mut env, "env var", primary, &owner)?;
            }
        }
        self.keys = keys;
        self.env = env;
        self.sections.push(section);
        Ok(())
    }

    /// Register every section of `other`.
    pub fn merge(&mut self, other: &ConfigSchema) -> Result<(), SchemaError> {
        for section in &other.sections {
            self.add(section)?;
        }
        Ok(())
    }

    fn claim(
        map: &mut BTreeMap<&'static str, String>,
        what: &'static str,
        name: &'static str,
        owner: &str,
    ) -> Result<(), SchemaError> {
        match map.get(name) {
            Some(first) if first != owner => Err(SchemaError {
                what,
                name: name.to_string(),
                first: first.clone(),
                second: owner.to_string(),
            }),
            _ => {
                map.insert(name, owner.to_string());
                Ok(())
            }
        }
    }

    /// The registered sections, in registration order.
    pub fn sections(&self) -> &[&'static SectionSchema] {
        &self.sections
    }

    /// Every registered section setting.
    pub fn key_defs(&self) -> impl Iterator<Item = &'static KeyDef> + '_ {
        self.sections.iter().flat_map(|s| s.keys.iter())
    }

    /// The section setting for `key`, matching its canonical key or an alias.
    pub fn key_def(&self, key: &str) -> Option<&'static KeyDef> {
        self.key_defs()
            .find(|def| def.key == key || def.aliases.contains(&key))
    }

    /// Set `key` (canonical or alias) in `values` from JSON, after checking
    /// the value against its definition. Returns the canonical key.
    pub fn set(
        &self,
        values: &mut ConfigValues,
        key: &str,
        value: &Value,
    ) -> Result<&'static str, KeyError> {
        let def = self
            .key_def(key)
            .ok_or_else(|| KeyError::Unknown(key.to_string()))?;
        let canonical = (def.validate)(value).map_err(|reason| KeyError::Invalid {
            key: key.to_string(),
            reason,
        })?;
        values.insert(def.key.to_string(), canonical);
        Ok(def.key)
    }

    /// The value of `key` (canonical or alias): the one in `values`, else the
    /// default. `None` for an unknown key.
    pub fn get(&self, values: &ConfigValues, key: &str) -> Option<Value> {
        let def = self.key_def(key)?;
        Some(values.get(def.key).cloned().unwrap_or_else(def.default))
    }

    /// Overlay the env vars of every registered setting onto `values`. A set,
    /// non-empty variable that parses replaces the value; a malformed one is
    /// ignored and does not fall through to a later alias.
    pub fn overlay_env(&self, values: &mut ConfigValues, env: &dyn EnvSource) {
        for def in self.key_defs() {
            let raw = def
                .env
                .iter()
                .find_map(|name| env.get(name).filter(|v| !v.is_empty()));
            if let Some(value) = raw.and_then(|raw| (def.parse_env)(&raw)) {
                values.insert(def.key.to_string(), value);
            }
        }
    }

    /// Bring `values` into canonical form: rewrite aliases to canonical keys
    /// and check every value. A key that matches no registered setting is
    /// handled by the unknown-key policy and, when lenient, *kept*, so a
    /// config written by a build with more components is not destroyed by one
    /// with fewer. A value of the wrong type is always an error.
    pub fn normalize(&self, values: &mut ConfigValues, strict: bool) -> Result<(), KeyError> {
        let entries = std::mem::take(values);
        for (key, value) in entries {
            match self.key_def(&key) {
                Some(def) => {
                    let canonical = (def.validate)(&value)
                        .map_err(|reason| KeyError::Invalid { key, reason })?;
                    values.insert(def.key.to_string(), canonical);
                }
                None => {
                    unknown_key(&key, strict)?;
                    values.insert(key, value);
                }
            }
        }
        Ok(())
    }
}

/// What [`ConfigSchema::normalize_lenient`] found wrong with a bulk-loaded
/// set of values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LenientReport {
    /// Keys that match no registered setting. They are kept in the values.
    pub unknown: Vec<String>,
    /// Known keys whose value has the wrong type, as `(canonical key,
    /// reason)`. Their values are dropped, so the setting keeps its default.
    pub invalid: Vec<(String, String)>,
}

impl ConfigSchema {
    /// [`ConfigSchema::normalize`] for values that arrive in bulk at startup
    /// (a config file, a binding's settings object), where the whole set must
    /// be read before anything can fail: aliases are rewritten, a value of the
    /// wrong type is dropped and reported, and an unknown key is kept with a
    /// warning. The caller turns the report into an error at its next
    /// fallible step: a wrongly typed value always, an unknown key under
    /// strict mode.
    pub fn normalize_lenient(&self, values: &mut ConfigValues) -> LenientReport {
        let mut report = LenientReport::default();
        let entries = std::mem::take(values);
        for (key, value) in entries {
            match self.key_def(&key) {
                Some(def) => match (def.validate)(&value) {
                    Ok(canonical) => {
                        values.insert(def.key.to_string(), canonical);
                    }
                    Err(reason) => {
                        tracing::warn!(key, reason, "config value of the wrong type");
                        report.invalid.push((def.key.to_string(), reason));
                    }
                },
                None => {
                    // Warn now; strict mode turns it into an error at the
                    // caller's next fallible step.
                    let _ = unknown_key(&key, false);
                    report.unknown.push(key.clone());
                    values.insert(key, value);
                }
            }
        }
        report
    }

    /// Keys of `values` that match no registered setting.
    pub fn unknown_keys<'a>(&self, values: &'a ConfigValues) -> Vec<&'a str> {
        values
            .keys()
            .filter(|key| self.key_def(key).is_none())
            .map(String::as_str)
            .collect()
    }
}

/// Declare a typed configuration section.
///
/// ```
/// cognee_config_core::config_section! {
///     /// Ladybug embedded graph database.
///     pub struct LadybugConfig in "graph.ladybug" {
///         /// Maximum database size in bytes.
///         pub max_db_size: u64 = 1 << 30 => {
///             env: ["GRAPH_MAX_DB_SIZE"],
///             alias: ["graph_max_db_size"],
///             secret: false,
///         },
///     }
/// }
///
/// use cognee_config_core::{ConfigSection, ConfigValues};
/// assert_eq!(LadybugConfig::schema().keys[0].key, "graph.ladybug.max_db_size");
/// assert_eq!(LadybugConfig::from_values(&ConfigValues::new()).max_db_size, 1 << 30);
/// ```
///
/// Each field becomes the setting `"<section>.<field>"`, bound to `env` (in
/// priority order) and reachable under each `alias` (use the Python SDK's flat
/// name where there is one). The field type must implement
/// [`ConfigValue`](crate::ConfigValue).
#[macro_export]
macro_rules! config_section {
    (
        $(#[doc = $sdoc:literal])*
        $vis:vis struct $name:ident in $section:literal {
            $(
                $(#[doc = $doc:literal])*
                $fvis:vis $field:ident : $ty:ty = $default:expr => {
                    env: [$($env:literal),* $(,)?],
                    alias: [$($alias:literal),* $(,)?],
                    secret: $secret:literal $(,)?
                }
            ),* $(,)?
        }
    ) => {
        $(#[doc = $sdoc])*
        #[derive(Debug, Clone, PartialEq)]
        $vis struct $name {
            $(
                $(#[doc = $doc])*
                $fvis $field: $ty,
            )*
        }

        impl ::core::default::Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        impl $crate::ConfigSection for $name {
            fn schema() -> &'static $crate::SectionSchema {
                static SCHEMA: $crate::SectionSchema = $crate::SectionSchema {
                    name: $section,
                    doc: concat!($($sdoc, "\n",)*),
                    keys: &[
                        $(
                            $crate::KeyDef {
                                key: concat!($section, ".", stringify!($field)),
                                aliases: &[$($alias),*],
                                env: &[$($env),*],
                                secret: $secret,
                                doc: concat!($($doc, "\n",)*),
                                default: || {
                                    let value: $ty = $default;
                                    $crate::ConfigValue::to_json(&value)
                                },
                                parse_env: |raw| {
                                    <$ty as $crate::ConfigValue>::parse_env(raw)
                                        .map(|v| $crate::ConfigValue::to_json(&v))
                                },
                                validate: |value| {
                                    <$ty as $crate::ConfigValue>::from_json(value)
                                        .map(|v| $crate::ConfigValue::to_json(&v))
                                },
                            },
                        )*
                    ],
                };
                &SCHEMA
            }

            fn from_values(values: &$crate::ConfigValues) -> Self {
                #[allow(unused_mut, reason = "a section may have no fields")]
                let mut section = Self::default();
                $(
                    if let Some(value) = values.get(concat!($section, ".", stringify!($field)))
                        && let Ok(v) = <$ty as $crate::ConfigValue>::from_json(value)
                    {
                        section.$field = v;
                    }
                )*
                section
            }
        }
    };
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
    use crate::MapEnv;

    crate::config_section! {
        /// A test section.
        pub struct Demo in "demo" {
            /// A size.
            pub size: u32 = 7 => { env: ["DEMO_SIZE", "DEMO_SIZE_ALIAS"], alias: ["demo_size"], secret: false },
            /// A token.
            pub token: Option<String> = None => { env: ["DEMO_TOKEN"], alias: [], secret: true },
            /// A flag.
            pub flag: bool = false => { env: [], alias: [], secret: false },
        }
    }

    crate::config_section! {
        /// Collides with `Demo`.
        pub struct Clash in "clash" {
            /// Reuses Demo's alias.
            pub x: u32 = 0 => { env: [], alias: ["demo_size"], secret: false },
        }
    }

    fn schema() -> ConfigSchema {
        let mut schema = ConfigSchema::new();
        schema.add(Demo::schema()).unwrap();
        schema
    }

    #[test]
    fn section_declares_namespaced_keys() {
        let keys: Vec<_> = Demo::schema().keys.iter().map(|k| k.key).collect();
        assert_eq!(keys, ["demo.size", "demo.token", "demo.flag"]);
        assert!(Demo::schema().keys[1].secret);
        assert_eq!(Demo::schema().keys[0].doc.trim(), "A size.");
    }

    #[test]
    fn env_overlay_follows_priority_and_ignores_malformed_values() {
        let schema = schema();
        let mut values = ConfigValues::new();
        let env = MapEnv::new()
            .with("DEMO_SIZE", "")
            .with("DEMO_SIZE_ALIAS", "9")
            .with("DEMO_TOKEN", "t");
        schema.overlay_env(&mut values, &env);
        let demo = Demo::from_values(&values);
        assert_eq!(demo.size, 9, "empty primary falls through to the alias");
        assert_eq!(demo.token.as_deref(), Some("t"));

        let mut values = ConfigValues::new();
        schema.overlay_env(
            &mut values,
            &MapEnv::new()
                .with("DEMO_SIZE", "abc")
                .with("DEMO_SIZE_ALIAS", "9"),
        );
        assert_eq!(
            Demo::from_values(&values).size,
            7,
            "malformed primary is ignored, no fallthrough"
        );
    }

    #[test]
    fn set_accepts_aliases_and_checks_types() {
        let schema = schema();
        let mut values = ConfigValues::new();
        assert_eq!(
            schema.set(&mut values, "demo_size", &json!(3)),
            Ok("demo.size")
        );
        assert_eq!(schema.get(&values, "demo.size"), Some(json!(3)));
        assert_eq!(
            schema.get(&values, "demo.flag"),
            Some(json!(false)),
            "default"
        );
        assert!(matches!(
            schema.set(&mut values, "demo.size", &json!("x")),
            Err(KeyError::Invalid { .. })
        ));
        assert_eq!(
            schema.set(&mut values, "nope", &json!(1)),
            Err(KeyError::Unknown("nope".into()))
        );
    }

    #[test]
    fn normalize_canonicalizes_and_applies_the_unknown_key_policy() {
        let schema = schema();
        let mut values: ConfigValues = [
            ("demo_size".to_string(), json!(4)),
            ("stale.key".to_string(), json!(1)),
        ]
        .into();
        schema.normalize(&mut values, false).unwrap();
        assert_eq!(values.get("demo.size"), Some(&json!(4)));
        assert_eq!(values.get("stale.key"), Some(&json!(1)), "lenient keeps it");

        let mut values: ConfigValues = [("stale.key".to_string(), json!(1))].into();
        assert_eq!(
            schema.normalize(&mut values, true),
            Err(KeyError::Unknown("stale.key".into()))
        );
    }

    #[test]
    fn normalize_lenient_reports_wrong_types_and_keeps_unknown_keys() {
        let schema = schema();
        let mut values: ConfigValues = [
            ("demo_size".to_string(), json!("big")),
            ("demo.flag".to_string(), json!(true)),
            ("stale.key".to_string(), json!(1)),
        ]
        .into();
        let report = schema.normalize_lenient(&mut values);
        assert_eq!(report.unknown, vec!["stale.key".to_string()]);
        assert_eq!(report.invalid.len(), 1);
        assert_eq!(
            report.invalid[0].0, "demo.size",
            "reported under the canonical key"
        );
        assert!(
            !values.contains_key("demo.size"),
            "the wrong value is dropped"
        );
        assert!(!values.contains_key("demo_size"));
        assert_eq!(values.get("demo.flag"), Some(&json!(true)));
        assert_eq!(
            values.get("stale.key"),
            Some(&json!(1)),
            "unknown keys are kept"
        );
    }

    #[test]
    fn collisions_are_rejected_and_re_registration_is_idempotent() {
        let mut schema = schema();
        schema.add(Demo::schema()).unwrap();
        assert_eq!(schema.sections().len(), 1);
        let err = schema.add(Clash::schema()).unwrap_err();
        assert_eq!(err.name, "demo_size");

        let mut schema = ConfigSchema::new();
        schema.reserve("core Settings", ["demo_size"], []).unwrap();
        assert!(
            schema.add(Demo::schema()).is_err(),
            "a reserved key cannot be claimed"
        );
    }
}
