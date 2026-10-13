//! The configuration boundary's only window onto the process environment.
//!
//! cognee resolves configuration **once**, at a boundary, into a typed config
//! struct (`Settings`, `LoggingConfig`, …); components receive values through
//! constructors and never read the environment themselves. The environment is
//! a source of *defaults* — everything it can set must also be settable from
//! code. See `docs/configuration.md` § "Configuration model".
//!
//! This crate is how that rule is enforced:
//!
//! * The root `clippy.toml` bans `std::env::{var, var_os, vars, vars_os,
//!   set_var, remove_var}` and `dotenv::*` everywhere. The single sanctioned
//!   read lives in [`ProcessEnv`]'s [`EnvSource`] impl.
//! * The same file also bans [`ProcessEnv::new`]. A config boundary — the
//!   place that turns the environment into a config struct — constructs one
//!   under `#[allow(clippy::disallowed_methods, reason = "config boundary: …")]`,
//!   so every boundary in the workspace is explicit and grep-able. Code below
//!   the boundary takes `&dyn EnvSource` (for tests) or, better, the already
//!   resolved value.
//! * Tests build a [`MapEnv`] instead of mutating the shared process env.
//!
//! It also holds the machinery for **component-owned configuration**: a
//! component declares the settings it reads as a typed section with
//! [`config_section!`], and registering the component registers the section
//! in a [`ConfigSchema`]. See [`section`].

use std::collections::BTreeMap;

pub mod section;
pub use section::{
    ConfigSchema, ConfigSection, ConfigValue, ConfigValues, KeyDef, KeyError, LenientReport,
    SchemaError, SectionSchema, unknown_key,
};

/// Read-only view over an environment.
///
/// Config loaders take the environment as a parameter rather than calling
/// [`std::env::var`] directly, so the resolution rules can be tested without
/// mutating shared process state (the workspace test runner is parallel;
/// `set_var` in one test is visible to every other test in the same binary).
pub trait EnvSource: Send + Sync {
    /// Return the value of `key`, or `None` when it is unset or not valid
    /// UTF-8 (the same cases in which [`std::env::var`] returns `Err`).
    fn get(&self, key: &str) -> Option<String>;
}

impl<F> EnvSource for F
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn get(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// The real process environment.
///
/// Deliberately not constructible by a unit literal or `Default`: the only
/// constructor, [`ProcessEnv::new`], is banned by the root `clippy.toml`, so
/// every place that reads the process environment carries an explicit
/// `#[allow(clippy::disallowed_methods, reason = "config boundary: …")]`.
#[derive(Clone, Copy, Debug)]
pub struct ProcessEnv {
    _boundary: (),
}

impl ProcessEnv {
    /// Open the process environment. Call this only at a configuration
    /// boundary (see the crate docs); everything else should receive resolved
    /// values, or a `&dyn EnvSource` from its caller.
    #[must_use]
    #[allow(
        clippy::new_without_default,
        reason = "a `Default` impl would be an unbanned way to open the process env"
    )]
    pub const fn new() -> Self {
        Self { _boundary: () }
    }
}

impl EnvSource for ProcessEnv {
    #[allow(
        clippy::disallowed_methods,
        reason = "the one sanctioned process-env read in the workspace"
    )]
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// An in-memory environment, for tests and for callers that assemble
/// configuration from somewhere other than the process env.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapEnv {
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    /// An empty environment: every key reads as unset.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `key` to `value`, returning `self` for chaining.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.set(key, value);
        self
    }

    /// Set `key` to `value`.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    /// Remove `key`, so it reads as unset.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(key);
    }
}

impl<K, V> FromIterator<(K, V)> for MapEnv
where
    K: Into<String>,
    V: Into<String>,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            vars: iter
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
}

/// Load a `.env` file from the current directory (or any ancestor) into the
/// process environment. Variables already set in the process env win; a
/// missing file is not an error.
///
/// This keeps the long-standing behaviour of `Settings::overlay_from_env`
/// (which called `dotenv::dotenv()` on every overlay), now routed through the
/// boundary crate so `dotenv` can be banned everywhere else.
#[cfg(feature = "dotenv")]
#[allow(
    clippy::disallowed_methods,
    reason = "the one sanctioned .env load in the workspace"
)]
pub fn load_dotenv() {
    // A missing or unreadable `.env` is the common case, not an error.
    let _ = dotenv::dotenv();
}

#[cfg(test)]
mod tests {
    use super::{EnvSource, MapEnv};

    #[test]
    fn map_env_reads_back_what_was_set() {
        let mut env = MapEnv::new().with("A", "1").with("B", "");
        assert_eq!(env.get("A").as_deref(), Some("1"));
        // An exported-but-empty variable is still "set": filtering empties is
        // the loader's decision, exactly as with `std::env::var`.
        assert_eq!(env.get("B").as_deref(), Some(""));
        assert_eq!(env.get("C"), None);

        env.remove("A");
        assert_eq!(env.get("A"), None);
    }

    #[test]
    fn map_env_collects_from_pairs() {
        let env: MapEnv = [("X", "x"), ("Y", "y")].into_iter().collect();
        assert_eq!(env.get("X").as_deref(), Some("x"));
        assert_eq!(env.get("Y").as_deref(), Some("y"));
    }

    #[test]
    fn closures_are_env_sources() {
        let env = |key: &str| (key == "K").then(|| "v".to_string());
        let dyn_env: &dyn EnvSource = &env;
        assert_eq!(dyn_env.get("K").as_deref(), Some("v"));
        assert_eq!(dyn_env.get("other"), None);
    }
}
