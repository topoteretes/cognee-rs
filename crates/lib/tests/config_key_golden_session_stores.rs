//! Golden tests for the config sections the optional session stores add:
//! `session.fs` (feature `session-fs`) and `session.redis` (feature
//! `session-redis`). These are the sections `config_key_golden` leaves out.
//!
//! This target declares `required-features` in `Cargo.toml`. A build without
//! the features omits it, and naming it with `--test` then fails with the
//! missing features. It cannot report a green `running 0 tests`.
//!
//! Regenerate the snapshots with
//! `UPDATE_GOLDEN=1 cargo test -p cognee --features session-fs,session-redis --test config_key_golden_session_stores`.
#![cfg(all(feature = "session-fs", feature = "session-redis"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]

mod config_key_golden_support;

use std::collections::BTreeMap;

use config_key_golden_support::{
    FEATURE_SECTIONS, check_golden, env_probe_cases, section_set_keys, set_probe_cases,
};
use serde_json::Value;

/// Each feature-gated section, with its env binding and keys pinned in
/// `<feature>.env_overlay.json` and `<feature>.generic_set.json`.
#[test]
fn session_store_sections_match_golden() {
    for fs in FEATURE_SECTIONS {
        assert!(
            fs.is_registered(),
            "`{}` is enabled but the registry has no `{}*` keys",
            fs.feature,
            fs.key_prefix
        );

        let mut env_cases = BTreeMap::new();
        env_probe_cases(fs.env_vars, &mut env_cases);
        check_golden(
            &format!("{}.env_overlay.json", fs.feature),
            &Value::Object(env_cases.into_iter().collect()),
        );

        let mut set_cases = BTreeMap::new();
        set_probe_cases(&section_set_keys(Some(fs.feature)), &mut set_cases);
        check_golden(
            &format!("{}.generic_set.json", fs.feature),
            &Value::Object(set_cases.into_iter().collect()),
        );
    }
}
