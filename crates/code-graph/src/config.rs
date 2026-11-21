//! The `code_graph.*` config section.

use std::time::Duration;

use cognee_config_core::config_section;

use crate::error::{CodeGraphError, CodeGraphResult};

/// Default enola run timeout, Python's `get_code_graph_tasks(timeout=600.0)`.
pub const DEFAULT_ENOLA_TIMEOUT_SECS: f64 = 600.0;

config_section! {
    /// The deterministic code graph: where the enola binary is, how long
    /// one `enola --generate` run may take, where remote repositories are
    /// cloned and whether they may be. An engine section, registered with
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
        /// Python's `ALLOW_HTTP_REQUESTS` for remote repositories: only
        /// `true`, `1`, `yes` or `on` (any case) allow cloning them, any
        /// other value refuses it. Unset or blank, SDK, CLI and binding calls
        /// may clone and HTTP API requests may not. Gates repository clones
        /// only; web-page URLs `add` fetches are not gated by it yet.
        pub allow_http_requests: String = String::new() => {
            env: ["ALLOW_HTTP_REQUESTS"], alias: ["allow_http_requests"], secret: false,
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
        assert_eq!(config.allow_http_requests, "");
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
                    "code_graph.allow_http_requests",
                    &["ALLOW_HTTP_REQUESTS"][..]
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
