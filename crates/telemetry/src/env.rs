//! Environment-driven configuration for `send_telemetry`.
//!
//! Pure-function helpers that read process env vars. Kept separate
//! from the dispatcher so they can be exercised without spinning up
//! a tokio runtime — and so the public entry point in task 02-06 can
//! short-circuit on `is_disabled()` *before* deriving identities or
//! sanitizing properties.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set to `true` when a binding (PyO3, Neon, or C API) has called its
/// `setup_telemetry_analytics` / `cognee_init_telemetry` entrypoint
/// and the binding-specific policy allowed emission.
///
/// Gates the `COGNEE_HOST_SDK` sentinel in [`is_disabled`]: pure-Rust
/// embedders (CLI, http-server) using `cognee_lib::api::*` do not set
/// this flag and are therefore not suppressed by `COGNEE_HOST_SDK`.
///
/// Mutated exclusively via [`arm_binding_emission`]; read via
/// [`is_binding_armed`] and inside [`is_disabled`].
static BINDING_ARMED: AtomicBool = AtomicBool::new(false);

/// Called from a binding entrypoint after the per-binding policy
/// permits emission. Idempotent — subsequent calls are no-ops.
///
/// Gap 07 decisions 10 + 11: the `COGNEE_HOST_SDK` sentinel must
/// suppress only binding-armed emissions. Bindings call this from
/// `setup_telemetry_analytics` / `cognee_init_telemetry` once the
/// per-binding policy has decided to arm analytics.
pub fn arm_binding_emission() {
    BINDING_ARMED.store(true, Ordering::SeqCst);
}

/// Returns the current value of the binding-armed flag.
///
/// See [`arm_binding_emission`] for the lifecycle.
pub fn is_binding_armed() -> bool {
    BINDING_ARMED.load(Ordering::SeqCst)
}

/// Test-only reset for the `BINDING_ARMED` flag. Keeps env-mutating
/// unit tests independent of one another. Not exposed outside `cfg(test)`.
#[cfg(test)]
pub(crate) fn reset_binding_armed() {
    BINDING_ARMED.store(false, Ordering::SeqCst);
}

/// Returns `true` if the user has explicitly disabled telemetry, or
/// if the process is running in a `test` or `dev` environment.
///
/// Mirrors Python utils.py:194-199 — `TELEMETRY_DISABLED` is treated
/// as truthy whenever it is set to a non-empty string, and `ENV` is
/// treated as disabling for the literal values `test` and `dev`.
///
/// Additionally — per gap 07 decision 10 — returns `true` when a
/// binding has armed analytics (see [`arm_binding_emission`]) AND
/// `COGNEE_HOST_SDK` is set to any non-empty value. The sentinel
/// scope is narrowed to binding-armed callers so the pure-Rust
/// embedder path (CLI, http-server) is unaffected even when an
/// upstream process has set `COGNEE_HOST_SDK`.
pub fn is_disabled() -> bool {
    if let Ok(v) = std::env::var("TELEMETRY_DISABLED")
        && !v.is_empty()
    {
        return true;
    }
    if let Ok(env) = std::env::var("ENV")
        && (env == "test" || env == "dev")
    {
        return true;
    }
    // Decision 10: COGNEE_HOST_SDK only suppresses emissions armed by a
    // binding, never the pure-Rust embedder path.
    if is_binding_armed()
        && let Ok(v) = std::env::var("COGNEE_HOST_SDK")
        && !v.is_empty()
    {
        return true;
    }
    false
}

/// Total HTTP request timeout in seconds, clamped to `[1, 60]`.
///
/// Mirrors Python utils.py:24 — default 5s, env override
/// `TELEMETRY_REQUEST_TIMEOUT`. The clamp is a hardening choice:
/// Python accepts arbitrary values, but the runtime fallback
/// (decision 5) is synchronous, and a 60s upper bound prevents a
/// misconfiguration from blocking shutdown indefinitely.
pub fn request_timeout_secs() -> u64 {
    std::env::var("TELEMETRY_REQUEST_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(1, 60))
        .unwrap_or(5)
}

/// The proxy URL. Hard-coded per decision 2 of the locked decisions
/// table — reuse Python's `https://test.prometh.ai` so cross-SDK
/// identity grouping works.
///
/// A test-only override (`COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`) is
/// honoured **only** when `cfg(test)` is active or the env var
/// `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty. Production
/// builds without `cfg(test)` and without that env var ignore the
/// override entirely.
pub fn proxy_url() -> String {
    #[cfg(test)]
    {
        if let Ok(v) = std::env::var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS")
            && !v.is_empty()
        {
            return v;
        }
    }
    if std::env::var("COGNEE_TELEMETRY_INTEGRATION_TEST")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && let Ok(v) = std::env::var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS")
        && !v.is_empty()
    {
        return v;
    }
    "https://test.prometh.ai".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Workspace uses Rust edition 2024, where `std::env::set_var` and
    // `std::env::remove_var` are `unsafe` (concurrent env mutation is
    // process-wide UB). `#[serial]` orders these tests against every
    // other env-mutating test in the crate, which is the soundness
    // argument for the `unsafe` blocks below.

    #[test]
    #[serial]
    fn telemetry_disabled_truthy_value() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate, so no concurrent reader/writer
        //   of TELEMETRY_DISABLED / ENV exists while this body runs.
        unsafe {
            std::env::remove_var("ENV");
            std::env::set_var("TELEMETRY_DISABLED", "1");
        }
        assert!(is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("TELEMETRY_DISABLED", "false");
        }
        // Python checks for *any* non-empty value; we mirror.
        assert!(is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
        }
    }

    #[test]
    #[serial]
    fn telemetry_disabled_empty_value() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate.
        unsafe {
            std::env::remove_var("ENV");
            std::env::set_var("TELEMETRY_DISABLED", "");
        }
        // Python's `if os.getenv("TELEMETRY_DISABLED"):` treats empty
        // as falsy — we do too.
        assert!(!is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
        }
    }

    #[test]
    #[serial]
    fn env_test_disables() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate; no need for the previous
        //   hand-rolled race-guard read of `TELEMETRY_DISABLED`.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::set_var("ENV", "test");
        }
        assert!(is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("ENV", "dev");
        }
        assert!(is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("ENV", "production");
        }
        assert!(!is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("ENV");
        }
    }

    #[test]
    #[serial]
    fn is_disabled_when_binding_armed_and_host_sdk_set() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::remove_var("ENV");
        }
        reset_binding_armed();
        arm_binding_emission();
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("COGNEE_HOST_SDK", "python");
        }
        assert!(is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("COGNEE_HOST_SDK");
        }
        reset_binding_armed();
    }

    #[test]
    #[serial]
    fn is_not_disabled_when_only_host_sdk_set_without_arming() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate.
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::remove_var("ENV");
        }
        reset_binding_armed();
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("COGNEE_HOST_SDK", "python");
        }
        assert!(!is_disabled());
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("COGNEE_HOST_SDK");
        }
    }

    #[test]
    #[serial]
    fn timeout_default_and_clamp() {
        // SAFETY: `#[serial]` orders this test against every other
        //   env-mutating test in the crate.
        unsafe {
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
        }
        assert_eq!(request_timeout_secs(), 5);

        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "0");
        }
        assert_eq!(request_timeout_secs(), 1);

        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "120");
        }
        assert_eq!(request_timeout_secs(), 60);

        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "10");
        }
        assert_eq!(request_timeout_secs(), 10);

        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
        }
    }
}
