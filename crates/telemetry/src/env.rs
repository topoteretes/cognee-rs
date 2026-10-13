//! Binding arming, plus thin accessors kept for compatibility.
//!
//! Configuration itself lives in [`crate::config`]: [`crate::TelemetryConfig`]
//! is resolved from the environment once, at its boundary
//! ([`crate::TelemetryConfig::from_env`]), and can be replaced from code with
//! [`crate::configure`] / [`crate::set_enabled`]. Nothing in this module reads
//! the process environment.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set to `true` when a binding (PyO3, Neon, JNI or C API) has armed analytics
/// for its process.
///
/// Gates the host-SDK sentinel ([`crate::TelemetryConfig::host_sdk`],
/// `COGNEE_HOST_SDK`): pure-Rust embedders (the CLI, a server) using
/// `cognee::api::*` do not set this flag and are therefore not suppressed by it.
///
/// Mutated exclusively via [`arm_binding_emission`]; read via
/// [`is_binding_armed`] and inside [`crate::is_enabled`].
static BINDING_ARMED: AtomicBool = AtomicBool::new(false);

/// Called from a binding entrypoint (module load, or its
/// `setup_telemetry_analytics` / `cognee_init_telemetry`). Idempotent —
/// subsequent calls are no-ops.
///
/// Gap 07 decisions 10 + 11: the host-SDK sentinel suppresses only
/// binding-armed emissions. Arming only ever *adds* suppression; it never
/// enables emission.
pub fn arm_binding_emission() {
    BINDING_ARMED.store(true, Ordering::SeqCst);
}

/// Returns the current value of the binding-armed flag.
///
/// See [`arm_binding_emission`] for the lifecycle.
pub fn is_binding_armed() -> bool {
    BINDING_ARMED.load(Ordering::SeqCst)
}

/// Test-only reset for the `BINDING_ARMED` flag.
#[cfg(test)]
pub(crate) fn reset_binding_armed() {
    BINDING_ARMED.store(false, Ordering::SeqCst);
}

/// `true` when an event sent now would be dropped. The negation of
/// [`crate::is_enabled`].
#[deprecated(
    note = "use `cognee_telemetry::is_enabled()`; analytics configuration is \
                     process-wide and no longer read from the environment per call"
)]
pub fn is_disabled() -> bool {
    !crate::is_enabled()
}

/// Total HTTP request timeout in seconds, clamped to `[1, 60]`.
#[deprecated(note = "use `cognee_telemetry::current_config().effective_request_timeout()`")]
pub fn request_timeout_secs() -> u64 {
    crate::current_config()
        .effective_request_timeout()
        .as_secs()
}

/// The collector URL events are posted to.
#[deprecated(note = "use `cognee_telemetry::current_config().endpoint()`")]
pub fn proxy_url() -> String {
    crate::current_config().endpoint().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg_attr(not(feature = "telemetry"), allow(unused_imports))]
    use crate::{TelemetryConfig, configure, current_config, is_enabled, set_enabled};
    use serial_test::serial;

    /// Every test here mutates the process-wide configuration, so all of them
    /// are `#[serial]` and return to the environment-following state on exit.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::reset_to_env();
            reset_binding_armed();
        }
    }
    fn snapshot() -> Restore {
        crate::reset_to_env();
        Restore
    }

    /// The environment layer is live: a variable set after the first event
    /// (e.g. by a `.env` loaded when the first handle is built) is honoured,
    /// while a pinned switch still wins over it.
    #[test]
    #[serial]
    #[cfg(feature = "telemetry")]
    #[allow(
        clippy::disallowed_methods,
        reason = "tests drive configuration through the process env"
    )]
    fn env_set_after_first_use_is_honoured_and_switch_wins() {
        let _r = snapshot();
        // SAFETY: `#[serial]` orders this against every other env-mutating
        //   test in the crate.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::remove_var("ENV");
        }
        assert!(is_enabled(), "first use: default on");
        // SAFETY: as above.
        unsafe { std::env::set_var("TELEMETRY_DISABLED", "1") };
        assert!(!is_enabled(), "a later opt-out is honoured");
        set_enabled(true);
        assert!(is_enabled(), "the explicit switch wins over the env");
        crate::reset_to_env();
        assert!(!is_enabled(), "reset returns to the env layer");
        // SAFETY: as above.
        unsafe { std::env::remove_var("TELEMETRY_DISABLED") };
        assert!(is_enabled());
    }

    /// `set_enabled` pins only the switch; other fields keep following the env.
    #[test]
    #[serial]
    #[allow(
        clippy::disallowed_methods,
        reason = "tests drive configuration through the process env"
    )]
    fn set_enabled_keeps_env_for_other_fields() {
        let _r = snapshot();
        set_enabled(false);
        // SAFETY: `#[serial]` orders this against every other env-mutating test.
        unsafe { std::env::set_var("TRACKING_ID", "from-env-later") };
        let tracking = current_config().tracking_id;
        // SAFETY: as above.
        unsafe { std::env::remove_var("TRACKING_ID") };
        assert_eq!(tracking.as_deref(), Some("from-env-later"));
        assert!(!current_config().enabled);
    }

    #[test]
    #[serial]
    #[cfg(feature = "telemetry")]
    fn set_enabled_wins_over_configured_state() {
        let _r = snapshot();
        configure(TelemetryConfig {
            enabled: false,
            ..TelemetryConfig::default()
        });
        assert!(!is_enabled());
        set_enabled(true);
        assert!(is_enabled());
        set_enabled(false);
        assert!(!is_enabled());
        // Other settings survive the switch.
        configure(TelemetryConfig {
            tracking_id: Some("t".into()),
            ..TelemetryConfig::default()
        });
        set_enabled(false);
        assert_eq!(current_config().tracking_id.as_deref(), Some("t"));
    }

    #[test]
    #[serial]
    #[cfg(feature = "telemetry")]
    fn host_sdk_suppresses_only_when_binding_armed() {
        let _r = snapshot();
        reset_binding_armed();
        configure(TelemetryConfig {
            host_sdk: Some("python".into()),
            ..TelemetryConfig::default()
        });
        assert!(is_enabled(), "unarmed: the sentinel does not apply");
        arm_binding_emission();
        assert!(!is_enabled(), "armed: the sentinel suppresses");
        configure(TelemetryConfig::default());
        assert!(is_enabled(), "armed without a host SDK emits");
    }

    #[test]
    #[serial]
    #[cfg(not(feature = "telemetry"))]
    fn never_enabled_without_the_feature() {
        let _r = snapshot();
        set_enabled(true);
        assert!(!is_enabled());
    }
}
