//! Environment-driven configuration for `send_telemetry`.
//!
//! Pure-function helpers that read process env vars. Kept separate
//! from the dispatcher so they can be exercised without spinning up
//! a tokio runtime — and so the public entry point in task 02-06 can
//! short-circuit on `is_disabled()` *before* deriving identities or
//! sanitizing properties.

/// Returns `true` if the user has explicitly disabled telemetry, or
/// if the process is running in a `test` or `dev` environment.
///
/// Mirrors Python utils.py:194-199 — `TELEMETRY_DISABLED` is treated
/// as truthy whenever it is set to a non-empty string, and `ENV` is
/// treated as disabling for the literal values `test` and `dev`.
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
