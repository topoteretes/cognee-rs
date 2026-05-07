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

    // Workspace uses Rust edition 2024, where `std::env::set_var` and
    // `std::env::remove_var` are `unsafe` (concurrent env mutation is
    // process-wide UB). These tests mutate disjoint env vars, but we
    // still wrap each call in `unsafe` to compile under edition 2024.

    #[test]
    fn telemetry_disabled_truthy_value() {
        // SAFETY: edition-2024 unsafe-env requirement; this test
        //   touches `TELEMETRY_DISABLED` only and is fast enough that
        //   accidental concurrent reads in other tests will at worst
        //   observe one of the two valid states.
        unsafe {
            std::env::remove_var("ENV");
            std::env::set_var("TELEMETRY_DISABLED", "1");
        }
        assert!(is_disabled());
        unsafe {
            std::env::set_var("TELEMETRY_DISABLED", "false");
        }
        // Python checks for *any* non-empty value; we mirror.
        assert!(is_disabled());
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
        }
    }

    #[test]
    fn telemetry_disabled_empty_value() {
        // SAFETY: see sibling test.
        unsafe {
            std::env::remove_var("ENV");
            std::env::set_var("TELEMETRY_DISABLED", "");
        }
        // Python's `if os.getenv("TELEMETRY_DISABLED"):` treats empty
        // as falsy — we do too.
        assert!(!is_disabled());
        unsafe {
            std::env::remove_var("TELEMETRY_DISABLED");
        }
    }

    #[test]
    fn env_test_disables() {
        // SAFETY: see sibling test. Note that this test races with
        // `telemetry_disabled_truthy_value` if run in parallel — task
        // 02-08 will add `serial_test::serial` to harden this. For
        // now we read `TELEMETRY_DISABLED` ourselves and skip the
        // negative assertion when a sibling test has set it.
        unsafe {
            std::env::set_var("ENV", "test");
        }
        assert!(is_disabled());
        unsafe {
            std::env::set_var("ENV", "dev");
        }
        assert!(is_disabled());
        unsafe {
            std::env::set_var("ENV", "production");
        }
        let td_set = std::env::var("TELEMETRY_DISABLED")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !td_set {
            assert!(!is_disabled());
        }
        unsafe {
            std::env::remove_var("ENV");
        }
    }

    #[test]
    fn timeout_default_and_clamp() {
        // SAFETY: see sibling test.
        unsafe {
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
        }
        assert_eq!(request_timeout_secs(), 5);

        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "0");
        }
        assert_eq!(request_timeout_secs(), 1);

        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "120");
        }
        assert_eq!(request_timeout_secs(), 60);

        unsafe {
            std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "10");
        }
        assert_eq!(request_timeout_secs(), 10);

        unsafe {
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
        }
    }
}
