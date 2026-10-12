//! Golden tests for `cognee-cli config`'s key surface: `known_keys`
//! (`config list`), `as_flat_map` (`config get <key>`), `set_value`
//! (`config set`) and `unset_key` (`config unset`).
//!
//! Like `crates/lib/tests/config_key_golden.rs`, results are recorded as the
//! fields that differ from `Settings::default()`, so the snapshots do not
//! depend on target- or feature-dependent defaults. A change to a snapshot is a
//! behaviour change; review the diff. Regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cognee-cli --test config_key_golden`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use cognee::config::Settings;
use cognee_cli::config_store::{as_flat_map, known_keys, set_value, unset_key};
use serde_json::{Map, Value, json};

fn set_probes() -> Vec<Value> {
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

/// Effective values that differ from a default configuration (see the
/// `cognee` golden test).
fn diff_from_default(s: &Settings) -> Value {
    let now = cognee::ConfigManager::new(s.clone()).snapshot(None);
    let base = cognee::ConfigManager::new(Settings::default()).snapshot(None);
    let mut out = Map::new();
    for (key, value) in now.as_object().unwrap() {
        if base.get(key) != Some(value) {
            out.insert(key.clone(), value.clone());
        }
    }
    Value::Object(out)
}

fn all_keys() -> Vec<String> {
    let mut keys: Vec<String> = serde_json::to_value(Settings::default())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.extend(
        [
            "llm_args",
            "summarization_schema",
            "embedding_model",
            "no_such_key",
        ]
        .into_iter()
        .map(String::from),
    );
    for def in cognee::config::builtin_schema().key_defs() {
        keys.push(def.key.to_string());
        keys.extend(def.aliases.iter().map(|a| a.to_string()));
    }
    keys.sort();
    keys.dedup();
    keys
}

fn check_golden(name: &str, actual: &Value) {
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

#[test]
fn cli_config_keys_match_golden() {
    let mut cases = BTreeMap::new();

    let listed: Vec<Value> = known_keys().into_iter().map(Value::from).collect();
    cases.insert("<config list>".to_string(), Value::Array(listed));

    // `config get` on a non-default document: which keys it can show.
    let flat = as_flat_map(&Settings::default());
    let shown: Vec<Value> = flat.keys().map(|k| Value::from(*k)).collect();
    cases.insert("<config get keys>".to_string(), Value::Array(shown));

    for key in all_keys() {
        for probe in set_probes() {
            let mut s = Settings::default();
            let outcome = match set_value(&mut s, &key, probe.clone()) {
                Ok(()) => json!({ "ok": diff_from_default(&s) }),
                Err(_) => json!({ "err": true }),
            };
            cases.insert(format!("set {key} <- {probe}"), outcome);
        }

        // `config unset` restores the default from a changed document.
        let mut s = Settings {
            sections: [("llm.model".to_string(), json!("changed"))].into(),
            db_port: 1,
            chunk_size: Some(7),
            ..Settings::default()
        };
        let outcome = match unset_key(&mut s, &key) {
            Ok(()) => json!({ "ok": diff_from_default(&s) }),
            Err(_) => json!({ "err": true }),
        };
        cases.insert(format!("unset {key}"), outcome);
    }
    check_golden(
        "cli_config_keys.json",
        &Value::Object(cases.into_iter().collect()),
    );
}
