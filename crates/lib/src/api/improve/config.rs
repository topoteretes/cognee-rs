//! The improve loop's own settings, resolved and validated — Python's
//! `ImproveConfig` and `get_improve_config()`
//! (`cognee/modules/improve/config.py`).
//!
//! The raw values live in the `improve` config section
//! ([`cognee_components::ImproveSettings`], env prefix `IMPROVE_`);
//! [`ImproveConfig::from_settings`] turns them into checked values, which is
//! what [`Settings::improve_config`](crate::config::Settings::improve_config)
//! returns. Knobs other parts of cognee own (`auto_feedback`,
//! `personalization_enabled`, …) stay with their owners.

use std::sync::Once;

use super::stages::{DEFAULT_STAGES, REMOVED_STAGE_NAMES, StageKind};

/// The `feedback_weights` learning rate when neither the run nor the
/// configuration sets one (Python's `DEFAULT_FEEDBACK_ALPHA`).
pub const DEFAULT_FEEDBACK_ALPHA: f64 = 0.1;

/// A value of the `improve` section that Python's `ImproveConfig` rejects.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("invalid improve configuration: {0}")]
pub struct ImproveConfigError(pub String);

/// The improve settings, validated. `Default` is Python's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct ImproveConfig {
    /// Run `improve()` automatically after a session `remember()`.
    pub auto_enabled: bool,
    /// Opt in to `review_conflicts` for every run.
    pub review_conflicts: bool,
    /// Document metadata key holding a source's effective date.
    pub effective_date_key: String,
    /// Auto-improve fires after this many new session entries (0: off).
    pub debounce_entries: u64,
    /// ... or after this many seconds since the last one (0: off).
    pub debounce_seconds: f64,
    /// Stage names skipped with `disabled_by_config`.
    pub stages_disabled: Vec<String>,
    /// Learning rate of `feedback_weights`, in (0, 1].
    pub feedback_alpha: f64,
    /// Agent trace steps per persisted document (0: all pending).
    pub trace_window_steps: u64,
    /// Agent trace documents per run (0: drain the backlog).
    pub trace_windows_per_run: u64,
    /// Curator batches per distillation run (0: uncapped).
    pub distill_max_batches_per_run: u64,
}

impl Default for ImproveConfig {
    fn default() -> Self {
        Self {
            auto_enabled: true,
            review_conflicts: false,
            effective_date_key: "effective_date".to_string(),
            debounce_entries: 1,
            debounce_seconds: 0.0,
            stages_disabled: Vec::new(),
            feedback_alpha: DEFAULT_FEEDBACK_ALPHA,
            trace_window_steps: 0,
            trace_windows_per_run: 0,
            distill_max_batches_per_run: 0,
        }
    }
}

/// `IMPROVE_STAGES_DISABLED`'s comma-separated names, trimmed, empties dropped.
pub fn parse_stages_disabled(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn non_negative(value: i64, field: &str, meaning: &str) -> Result<u64, ImproveConfigError> {
    u64::try_from(value)
        .map_err(|_| ImproveConfigError(format!("{field} must be >= 0{meaning}, got {value}")))
}

/// Check `feedback_alpha` is in (0, 1], Python's `_alpha_in_unit_interval`.
pub fn validate_feedback_alpha(alpha: f64) -> Result<f64, ImproveConfigError> {
    if alpha > 0.0 && alpha <= 1.0 {
        Ok(alpha)
    } else {
        Err(ImproveConfigError(format!(
            "feedback_alpha must be in range (0, 1], got {alpha}"
        )))
    }
}

/// Raise unless every disabled stage name can be honoured — Python's
/// `validate_stages_disabled` (`cognee/modules/improve/registry.py`).
///
/// A typo would otherwise disable nothing, silently, and the fatal stage
/// guards against data loss, so configuration must not bypass it. A removed
/// stage name (`triplet_enrichment`) is ignored.
pub fn validate_stages_disabled<S: AsRef<str>>(disabled: &[S]) -> Result<(), ImproveConfigError> {
    let names = disabled
        .iter()
        .map(AsRef::as_ref)
        .filter(|name| !name.is_empty() && !REMOVED_STAGE_NAMES.contains(name));
    let mut unknown = Vec::new();
    let mut fatal = Vec::new();
    for name in names {
        match StageKind::from_name(name) {
            None => unknown.push(name),
            Some(stage) if stage.fatal() => fatal.push(name),
            Some(_) => {}
        }
    }
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let mut valid: Vec<&str> = DEFAULT_STAGES.iter().map(|s| s.name()).collect();
        valid.sort_unstable();
        return Err(ImproveConfigError(format!(
            "IMPROVE_STAGES_DISABLED names unknown stage(s) {unknown:?}; valid names: {valid:?}"
        )));
    }
    if !fatal.is_empty() {
        fatal.sort_unstable();
        return Err(ImproveConfigError(format!(
            "IMPROVE_STAGES_DISABLED cannot disable fatal stage(s) {fatal:?}: skipping them \
             would silently lose session data"
        )));
    }
    Ok(())
}

impl ImproveConfig {
    /// Resolve and check the raw `improve` section — Python's `ImproveConfig`
    /// field validators plus `get_improve_config()`'s registry check. A removed
    /// stage name in `stages_disabled` is logged once per process and ignored.
    pub fn from_settings(
        raw: &cognee_components::ImproveSettings,
    ) -> Result<Self, ImproveConfigError> {
        if raw.debounce_seconds.is_nan() || raw.debounce_seconds < 0.0 {
            return Err(ImproveConfigError(format!(
                "debounce_seconds must be >= 0, got {}",
                raw.debounce_seconds
            )));
        }
        let config = Self {
            auto_enabled: raw.auto_enabled.get(),
            review_conflicts: raw.review_conflicts.get(),
            effective_date_key: raw.effective_date_key.clone(),
            debounce_entries: non_negative(raw.debounce_entries, "debounce_entries", "")?,
            debounce_seconds: raw.debounce_seconds,
            stages_disabled: parse_stages_disabled(&raw.stages_disabled),
            feedback_alpha: validate_feedback_alpha(raw.feedback_alpha)?,
            trace_window_steps: non_negative(
                raw.trace_window_steps,
                "trace_window_steps",
                " (0 = all pending steps)",
            )?,
            trace_windows_per_run: non_negative(
                raw.trace_windows_per_run,
                "trace_windows_per_run",
                " (0 = drain the backlog)",
            )?,
            distill_max_batches_per_run: non_negative(
                raw.distill_max_batches_per_run,
                "distill_max_batches_per_run",
                " (0 = uncapped)",
            )?,
        };
        validate_stages_disabled(&config.stages_disabled)?;
        let removed: Vec<&str> = config
            .stages_disabled
            .iter()
            .map(String::as_str)
            .filter(|name| REMOVED_STAGE_NAMES.contains(name))
            .collect();
        if !removed.is_empty() {
            // Once per process, as Python warns behind its cached getter.
            static WARNED: Once = Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    stages = ?removed,
                    "IMPROVE_STAGES_DISABLED names removed stage(s); ignoring them"
                );
            });
        }
        Ok(config)
    }

    /// Check a hand-built configuration the way [`Self::from_settings`]
    /// checks the section: `improve()` runs it before the first stage, so an
    /// explicit run fails loudly on a bad value.
    pub fn validate(&self) -> Result<(), ImproveConfigError> {
        if self.debounce_seconds.is_nan() || self.debounce_seconds < 0.0 {
            return Err(ImproveConfigError(format!(
                "debounce_seconds must be >= 0, got {}",
                self.debounce_seconds
            )));
        }
        validate_feedback_alpha(self.feedback_alpha)?;
        validate_stages_disabled(&self.stages_disabled)
    }

    /// Whether `stage` is switched off by `stages_disabled`.
    pub fn is_disabled(&self, stage: StageKind) -> bool {
        self.stages_disabled.iter().any(|name| name == stage.name())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    //! Ported from Python's `cognee/tests/unit/modules/improve/test_improve_config.py`
    //! and the `validate_stages_disabled` cases of `test_registry_order.py`.

    use cognee_components::ImproveSettings;
    use cognee_config_core::{ConfigSchema, ConfigSection, ConfigValues, MapEnv};

    use super::*;

    fn from_env(vars: &[(&str, &str)]) -> Result<ImproveConfig, ImproveConfigError> {
        let mut schema = ConfigSchema::new();
        schema.add(ImproveSettings::schema()).unwrap();
        let mut env = MapEnv::new();
        for (key, value) in vars {
            env.set(*key, *value);
        }
        let mut values = ConfigValues::new();
        schema.overlay_env(&mut values, &env);
        ImproveConfig::from_settings(&ImproveSettings::from_values(&values))
    }

    fn with(edit: impl FnOnce(&mut ImproveSettings)) -> Result<ImproveConfig, ImproveConfigError> {
        let mut raw = ImproveSettings::default();
        edit(&mut raw);
        ImproveConfig::from_settings(&raw)
    }

    #[test]
    fn defaults() {
        let config = from_env(&[]).unwrap();
        assert_eq!(config, ImproveConfig::default());
        assert!(config.auto_enabled);
        assert_eq!(config.debounce_entries, 1);
        assert_eq!(config.debounce_seconds, 0.0);
        assert!(config.stages_disabled.is_empty());
        assert_eq!(config.feedback_alpha, DEFAULT_FEEDBACK_ALPHA);
        assert_eq!(DEFAULT_FEEDBACK_ALPHA, 0.1);
        assert_eq!(config.distill_max_batches_per_run, 0);
        assert!(!config.review_conflicts);
        assert_eq!(config.effective_date_key, "effective_date");
    }

    #[test]
    fn env_prefix_and_csv_parsing() {
        let config = from_env(&[
            ("IMPROVE_AUTO_ENABLED", "false"),
            ("IMPROVE_DEBOUNCE_ENTRIES", "5"),
            ("IMPROVE_DEBOUNCE_SECONDS", "2.5"),
            (
                "IMPROVE_STAGES_DISABLED",
                " feedback_weights, distill_sessions ,,",
            ),
            ("IMPROVE_FEEDBACK_ALPHA", "0.25"),
            ("IMPROVE_REVIEW_CONFLICTS", "true"),
            ("IMPROVE_EFFECTIVE_DATE_KEY", "published"),
            ("IMPROVE_TRACE_WINDOW_STEPS", "4"),
            ("IMPROVE_TRACE_WINDOWS_PER_RUN", "2"),
        ])
        .unwrap();
        assert!(!config.auto_enabled);
        assert_eq!(config.debounce_entries, 5);
        assert_eq!(config.debounce_seconds, 2.5);
        assert_eq!(
            config.stages_disabled,
            ["feedback_weights", "distill_sessions"]
        );
        assert_eq!(config.feedback_alpha, 0.25);
        assert!(config.review_conflicts);
        assert_eq!(config.effective_date_key, "published");
        assert_eq!(config.trace_window_steps, 4);
        assert_eq!(config.trace_windows_per_run, 2);
    }

    #[test]
    fn stages_disabled_parsing_trims_and_drops_empties() {
        assert_eq!(parse_stages_disabled("a, b ,,"), ["a", "b"]);
        assert!(parse_stages_disabled("").is_empty());
    }

    #[test]
    fn feedback_alpha_must_be_in_unit_interval() {
        for alpha in [0.0, -0.1, 1.0001, 5.0, f64::NAN] {
            let err = with(|raw| raw.feedback_alpha = alpha).unwrap_err();
            assert!(err.0.contains("feedback_alpha"), "{err}");
        }
    }

    #[test]
    fn feedback_alpha_upper_bound_is_inclusive() {
        assert_eq!(
            with(|raw| raw.feedback_alpha = 1.0).unwrap().feedback_alpha,
            1.0
        );
    }

    #[test]
    fn negative_debounce_rejected() {
        assert!(with(|raw| raw.debounce_entries = -1).is_err());
        assert!(with(|raw| raw.debounce_seconds = -0.5).is_err());
        // From env too: a negative number parses, then fails validation.
        assert!(from_env(&[("IMPROVE_DEBOUNCE_ENTRIES", "-1")]).is_err());
    }

    #[test]
    fn negative_caps_rejected() {
        let err = with(|raw| raw.distill_max_batches_per_run = -1).unwrap_err();
        assert!(err.0.contains("distill_max_batches_per_run"), "{err}");
        assert!(with(|raw| raw.trace_window_steps = -1).is_err());
        assert!(with(|raw| raw.trace_windows_per_run = -1).is_err());
    }

    #[test]
    fn distill_cap_reads_env() {
        let config = from_env(&[("IMPROVE_DISTILL_MAX_BATCHES_PER_RUN", "3")]).unwrap();
        assert_eq!(config.distill_max_batches_per_run, 3);
    }

    // test_config_declares_no_shared_knobs: the section holds only the loop's
    // own knobs.
    #[test]
    fn section_declares_no_shared_knobs() {
        let keys: Vec<&str> = ImproveSettings::schema()
            .keys
            .iter()
            .map(|def| def.key.trim_start_matches("improve."))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [
                "auto_enabled",
                "debounce_entries",
                "debounce_seconds",
                "distill_max_batches_per_run",
                "effective_date_key",
                "feedback_alpha",
                "review_conflicts",
                "stages_disabled",
                "trace_window_steps",
                "trace_windows_per_run",
            ]
        );
        for shared in ["caching", "auto_feedback", "personalization_enabled"] {
            assert!(!keys.contains(&shared));
        }
    }

    // test_validate_stages_disabled_accepts_real_non_fatal_names
    #[test]
    fn validate_stages_disabled_accepts_real_non_fatal_names() {
        validate_stages_disabled::<&str>(&[]).unwrap();
        validate_stages_disabled(&["review_conflicts", "distill_sessions"]).unwrap();
    }

    // test_validate_stages_disabled_ignores_the_removed_triplet_enrichment
    #[test]
    fn validate_stages_disabled_ignores_removed_stage() {
        validate_stages_disabled(&["triplet_enrichment", "distill_sessions"]).unwrap();
        let config = from_env(&[("IMPROVE_STAGES_DISABLED", "triplet_enrichment")]).unwrap();
        assert_eq!(config.stages_disabled, ["triplet_enrichment"]);
    }

    // test_validate_stages_disabled_rejects_unknown_and_fatal_names
    #[test]
    fn validate_stages_disabled_rejects_unknown_and_fatal_names() {
        let err = validate_stages_disabled(&["persist_sesion_qa"]).unwrap_err();
        assert!(err.0.contains("persist_sesion_qa"), "{err}");
        assert!(err.0.contains("valid names"), "{err}");
        let err = validate_stages_disabled(&["persist_session_qa"]).unwrap_err();
        assert!(err.0.contains("fatal"), "{err}");
    }

    // test_env_typo_in_stages_disabled_fails_the_first_config_read
    #[test]
    fn env_typo_in_stages_disabled_fails_the_config_read() {
        let err = from_env(&[("IMPROVE_STAGES_DISABLED", "triplet_enrichmnt")]).unwrap_err();
        assert!(err.0.contains("triplet_enrichmnt"), "{err}");
    }

    #[test]
    fn validate_checks_a_hand_built_config() {
        ImproveConfig::default().validate().unwrap();
        let bad = ImproveConfig {
            stages_disabled: vec!["persist_session_qa".into()],
            ..ImproveConfig::default()
        };
        assert!(bad.validate().is_err());
        let bad = ImproveConfig {
            feedback_alpha: 0.0,
            ..ImproveConfig::default()
        };
        assert!(bad.validate().is_err());
    }
}
