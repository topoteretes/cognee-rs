//! Shared helpers for the config-key golden tests: `config_key_golden` (the
//! default build) and `config_key_golden_session_stores` (the feature-gated
//! session-store sections). A directory module without a `main.rs`, so cargo
//! does not build it as a test target of its own.
#![allow(
    dead_code,
    reason = "each including test target uses a different subset"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use cognee::config::{ConfigError, ConfigManager, MapEnv, Settings};
use serde_json::{Map, Value, json};

/// A config section that only a cargo feature compiles in, with the env vars
/// it binds. The default-build golden leaves these out; a build that
/// registers one pins it through `config_key_golden_session_stores`.
pub struct FeatureSection {
    /// The `cognee` cargo feature that compiles the section in; also the
    /// golden file prefix.
    pub feature: &'static str,
    /// Key prefix of the section's settings (`<section>.`).
    pub key_prefix: &'static str,
    /// Every env var the section binds.
    pub env_vars: &'static [&'static str],
}

impl FeatureSection {
    /// Whether this build registers the section. This asks the registry
    /// rather than checking `cfg!(feature = ...)` on `cognee`. Feature
    /// unification can turn on `cognee-components/session-*` without
    /// `cognee`'s own feature, and the registry is what decides the bound keys.
    pub fn is_registered(&self) -> bool {
        cognee::config::builtin_schema()
            .key_defs()
            .any(|def| def.key.starts_with(self.key_prefix))
    }
}

/// Every feature-gated section, registered or not.
pub const FEATURE_SECTIONS: &[FeatureSection] = &[
    FeatureSection {
        feature: "session-fs",
        key_prefix: "session.fs.",
        env_vars: &["COGNEE_SESSION_DIR"],
    },
    FeatureSection {
        feature: "session-redis",
        key_prefix: "session.redis.",
        env_vars: &[
            "CACHE_HOST",
            "CACHE_PORT",
            "CACHE_USERNAME",
            "CACHE_PASSWORD",
        ],
    },
];

/// The feature-gated section a section key belongs to, if any.
pub fn feature_section_of(key: &str) -> Option<&'static FeatureSection> {
    FEATURE_SECTIONS
        .iter()
        .find(|fs| key.starts_with(fs.key_prefix))
}

/// The keys and flat aliases of every built-in section setting that belongs
/// to `section` (`None`: to no feature-gated section). Sorted and deduplicated.
pub fn section_set_keys(section: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    for def in cognee::config::builtin_schema().key_defs() {
        if feature_section_of(def.key).map(|fs| fs.feature) == section {
            keys.push(def.key.to_string());
            keys.extend(def.aliases.iter().map(|a| a.to_string()));
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Raw env values probed against every variable: empty and whitespace,
/// numbers in and out of each integer range, every bool spelling, and JSON.
pub const ENV_PROBES: &[&str] = &[
    "v",
    "",
    "  ",
    "0",
    "1",
    "42",
    "70000",
    "5000000000",
    "-1",
    "1.5",
    "true",
    "TRUE",
    "on",
    "yes",
    "no",
    "false",
    "abc",
    "{\"a\":1}",
    "[1]",
    "{\"ssl\":true}",
    "{\"ssl\":\"verify-full\"}",
    "require",
];

/// JSON values probed against every key.
pub fn set_probes() -> Vec<Value> {
    vec![
        json!("s"),
        json!(""),
        json!("1"),
        json!("true"),
        json!(0),
        json!(1),
        json!(70000),
        json!(5_000_000_000_u64),
        json!(-1),
        json!(1.5),
        json!(true),
        json!(false),
        Value::Null,
        json!({"a": 1}),
        json!([1]),
    ]
}

/// The effective values that differ from a default configuration. This covers
/// every `Settings` field and every built-in section setting, current or
/// default. `summarization_schema` is `#[serde(skip)]`, so it is compared by
/// hand.
pub fn diff_from_default(s: &Settings) -> Value {
    let default = Settings::default();
    let now = ConfigManager::new(s.clone()).snapshot(None);
    let base = ConfigManager::new(default.clone()).snapshot(None);
    let mut out = Map::new();
    for (key, value) in now.as_object().unwrap() {
        if base.get(key) != Some(value) {
            out.insert(key.clone(), value.clone());
        }
    }
    if s.summarization_schema != default.summarization_schema {
        out.insert(
            "summarization_schema".into(),
            s.summarization_schema.clone().unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

pub fn overlay(env: &MapEnv) -> Value {
    let mut s = Settings::default();
    s.overlay_from(env);
    diff_from_default(&s)
}

pub fn set_outcome(key: &str, value: Value) -> Value {
    let cm = ConfigManager::new(Settings::default());
    match cm.set(key, value) {
        Ok(()) => json!({ "ok": diff_from_default(&cm.read()) }),
        Err(ConfigError::UnknownKey(k)) => json!({ "err": "UnknownKey", "key": k }),
        Err(ConfigError::TypeMismatch { key, .. }) => {
            json!({ "err": "TypeMismatch", "key": key })
        }
    }
}

/// Env-overlay cases for every probe against each of `vars`.
pub fn env_probe_cases(vars: &[&str], cases: &mut BTreeMap<String, Value>) {
    for var in vars {
        for probe in ENV_PROBES {
            let env = MapEnv::new().with(*var, *probe);
            cases.insert(format!("{var}={probe:?}"), overlay(&env));
        }
    }
}

/// `ConfigManager::set` cases for every probe against each of `keys`.
pub fn set_probe_cases(keys: &[String], cases: &mut BTreeMap<String, Value>) {
    for key in keys {
        for probe in set_probes() {
            cases.insert(format!("{key} <- {probe}"), set_outcome(key, probe));
        }
    }
}

pub fn check_golden(name: &str, actual: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let rendered = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}. Run with UPDATE_GOLDEN=1 to create it.",
            path.display()
        )
    });
    if expected != rendered {
        let expected: Value = serde_json::from_str(&expected).unwrap();
        // Name the first differing case so the failure is actionable.
        let (exp, act) = (expected.as_object().unwrap(), actual.as_object().unwrap());
        let first = exp
            .keys()
            .chain(act.keys())
            .find(|k| exp.get(*k) != act.get(*k))
            .cloned()
            .unwrap_or_default();
        panic!(
            "golden {name} differs, first at {first:?}:\n  expected {}\n  actual   {}\n\
             If the change is intended, re-run with UPDATE_GOLDEN=1 and review the diff.",
            exp.get(&first).map(Value::to_string).unwrap_or_default(),
            act.get(&first).map(Value::to_string).unwrap_or_default(),
        );
    }
}
