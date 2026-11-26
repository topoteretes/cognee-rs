//! The `code_graph.*` config section.

use std::time::Duration;

use cognee_config_core::config_section;

use crate::error::{CodeGraphError, CodeGraphResult};
use crate::query::{DEFAULT_CACHE_MAX_ENTRIES, DEFAULT_CACHE_TTL, SnapshotCacheLimits};

/// Default enola run timeout, Python's `get_code_graph_tasks(timeout=600.0)`.
pub const DEFAULT_ENOLA_TIMEOUT_SECS: f64 = 600.0;

/// Default CODE snapshot time limit, in seconds
/// (`_DEFAULT_CODE_GRAPH_CACHE_TTL_SECONDS`).
const DEFAULT_CACHE_TTL_SECS: f64 = 60.0;

config_section! {
    /// The deterministic code graph: where the enola binary is, how long
    /// one `enola --generate` run may take, where remote repositories are
    /// cloned. (Whether they may be is the cognee-wide
    /// `allow_http_requests` setting.) An engine section, registered with
    /// the kind sections.
    pub struct CodeGraphConfig in "code_graph" {
        /// Path to the enola binary (Python's `ENOLA_PATH`). Must name an
        /// existing file when set; empty looks `enola` up on `PATH` when the
        /// process is spawned.
        pub enola_path: String = String::new() => {
            env: ["ENOLA_PATH"], alias: ["enola_path"], secret: false,
        },
        /// Seconds one `enola --generate` run may take before it is killed
        /// (Python's `timeout`, default 600).
        pub enola_timeout_secs: f64 = DEFAULT_ENOLA_TIMEOUT_SECS => {
            env: [], alias: [], secret: false,
        },
        /// Where remote git repositories are shallow-cloned, for
        /// `remember(content_type="code")` and the GitHub/GitLab repository
        /// URLs `add` takes (Python's `BaseConfig.repos_root_directory`,
        /// `COGNEE_REPOS_DIR`). Always a local directory. Empty is
        /// `~/.cognee/repos`, Python's default.
        pub clones_dir: String = String::new() => {
            env: ["COGNEE_REPOS_DIR"], alias: ["repos_root_directory"], secret: false,
        },
        /// The most parsed code-graph snapshots a CODE search keeps cached,
        /// least recently used evicted first (Python's
        /// `CODE_GRAPH_CACHE_MAX_ENTRIES`, default 16). A value below 1 is
        /// the default.
        pub snapshot_cache_max_entries: u32 = DEFAULT_CACHE_MAX_ENTRIES as u32 => {
            env: ["CODE_GRAPH_CACHE_MAX_ENTRIES"], alias: [], secret: false,
        },
        /// Seconds a parsed code-graph snapshot is served before a CODE
        /// search reads the graph again (Python's
        /// `CODE_GRAPH_CACHE_TTL_SECONDS`, default 60). A value that is not a
        /// finite, positive number is the default. Writes and deletes of the
        /// dataset invalidate it sooner.
        pub snapshot_cache_ttl_secs: f64 = DEFAULT_CACHE_TTL_SECS => {
            env: ["CODE_GRAPH_CACHE_TTL_SECONDS"], alias: [], secret: false,
        },
    }
}

impl CodeGraphConfig {
    /// [`Self::enola_timeout_secs`] as a [`Duration`].
    ///
    /// # Errors
    ///
    /// [`CodeGraphError::InvalidConfig`] for a timeout that is not a finite,
    /// positive number of seconds.
    pub fn enola_timeout(&self) -> CodeGraphResult<Duration> {
        let secs = self.enola_timeout_secs;
        if !(secs.is_finite() && secs > 0.0) {
            return Err(CodeGraphError::InvalidConfig(format!(
                "code_graph.enola_timeout_secs must be a positive number of seconds, got {secs}"
            )));
        }
        Duration::try_from_secs_f64(secs).map_err(|error| {
            CodeGraphError::InvalidConfig(format!(
                "code_graph.enola_timeout_secs ({secs}) is out of range: {error}"
            ))
        })
    }
}

impl CodeGraphConfig {
    /// The bounds of the CODE snapshot cache, with Python's fallbacks: a
    /// count below 1, or a time limit that is not a finite positive number of
    /// seconds, is the default (`_positive_int_env` / `_positive_float_env`).
    pub fn snapshot_cache_limits(&self) -> SnapshotCacheLimits {
        let max_entries = match self.snapshot_cache_max_entries {
            0 => DEFAULT_CACHE_MAX_ENTRIES,
            entries => usize::try_from(entries).unwrap_or(DEFAULT_CACHE_MAX_ENTRIES),
        };
        let secs = self.snapshot_cache_ttl_secs;
        let ttl = if secs.is_finite() && secs > 0.0 {
            Duration::try_from_secs_f64(secs).unwrap_or(DEFAULT_CACHE_TTL)
        } else {
            DEFAULT_CACHE_TTL
        };
        SnapshotCacheLimits { max_entries, ttl }
    }

    /// [`Self::snapshot_cache_limits`] when they differ from the defaults,
    /// else `None`: the bounds a search handle built from this config
    /// applies to the process-wide cache. A config that leaves both keys at
    /// their defaults does not reset bounds another handle set.
    pub fn snapshot_cache_override(&self) -> Option<SnapshotCacheLimits> {
        let limits = self.snapshot_cache_limits();
        (limits != SnapshotCacheLimits::default()).then_some(limits)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_config_core::{ConfigSection, ConfigValues};
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_match_python() {
        let config = CodeGraphConfig::default();
        assert_eq!(config.enola_path, "");
        assert_eq!(config.enola_timeout().unwrap(), Duration::from_secs(600));
        assert_eq!(config.clones_dir, "");
    }

    #[test]
    fn keys_and_env_binding() {
        let keys: Vec<_> = CodeGraphConfig::schema()
            .keys
            .iter()
            .map(|key| (key.key, key.env))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("code_graph.enola_path", &["ENOLA_PATH"][..]),
                ("code_graph.enola_timeout_secs", &[][..]),
                ("code_graph.clones_dir", &["COGNEE_REPOS_DIR"][..]),
                (
                    "code_graph.snapshot_cache_max_entries",
                    &["CODE_GRAPH_CACHE_MAX_ENTRIES"][..]
                ),
                (
                    "code_graph.snapshot_cache_ttl_secs",
                    &["CODE_GRAPH_CACHE_TTL_SECONDS"][..]
                ),
            ]
        );
        let mut values = ConfigValues::new();
        values.insert("enola_path".into(), json!("/opt/enola"));
        values.insert("code_graph.enola_timeout_secs".into(), json!(2.5));
        let config = CodeGraphConfig::from_values(&values);
        assert_eq!(config.enola_path, "/opt/enola");
        assert_eq!(config.enola_timeout().unwrap(), Duration::from_millis(2500));
    }

    #[test]
    fn snapshot_cache_limits_fall_back_like_python() {
        let defaults = CodeGraphConfig::default().snapshot_cache_limits();
        assert_eq!(defaults, SnapshotCacheLimits::default());
        assert_eq!(defaults.max_entries, 16);
        assert_eq!(defaults.ttl, Duration::from_secs(60));
        let mut values = ConfigValues::new();
        values.insert("code_graph.snapshot_cache_max_entries".into(), json!(4));
        values.insert("code_graph.snapshot_cache_ttl_secs".into(), json!(1.5));
        let limits = CodeGraphConfig::from_values(&values).snapshot_cache_limits();
        assert_eq!(limits.max_entries, 4);
        assert_eq!(limits.ttl, Duration::from_millis(1500));
        for secs in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let config = CodeGraphConfig {
                snapshot_cache_max_entries: 0,
                snapshot_cache_ttl_secs: secs,
                ..CodeGraphConfig::default()
            };
            assert_eq!(
                config.snapshot_cache_limits(),
                SnapshotCacheLimits::default()
            );
            assert_eq!(config.snapshot_cache_override(), None);
        }
    }

    #[test]
    fn only_bounds_off_the_defaults_override_the_cache() {
        assert_eq!(CodeGraphConfig::default().snapshot_cache_override(), None);
        let config = CodeGraphConfig {
            snapshot_cache_max_entries: 4,
            ..CodeGraphConfig::default()
        };
        assert_eq!(
            config.snapshot_cache_override(),
            Some(SnapshotCacheLimits {
                max_entries: 4,
                ttl: Duration::from_secs(60),
            })
        );
    }

    #[test]
    fn a_non_positive_timeout_is_a_configuration_error() {
        for secs in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let config = CodeGraphConfig {
                enola_timeout_secs: secs,
                ..CodeGraphConfig::default()
            };
            assert!(matches!(
                config.enola_timeout(),
                Err(CodeGraphError::InvalidConfig(_))
            ));
        }
    }
}
