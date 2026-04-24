//! Environment-variable driven configuration for the cloud integration.
//!
//! All variables mirror the Python `cognee/api/v1/serve/` tree:
//! - [`DEFAULT_AUTH0_DOMAIN`] / `COGNEE_AUTH0_DOMAIN` — `device_auth.py:16,31`
//! - [`DEFAULT_AUTH0_AUDIENCE`] / `COGNEE_AUTH0_AUDIENCE` — `device_auth.py:17,45`
//! - [`DEFAULT_MANAGEMENT_URL`] / `COGNEE_CLOUD_URL` — `management_api.py:18,22`
//! - `COGNEE_AUTH0_DEVICE_CLIENT_ID` — `device_auth.py:34–41` (required, no default)

use std::env;

use crate::error::{CloudError, CloudResult};

/// Default Auth0 domain used when `COGNEE_AUTH0_DOMAIN` is unset.
///
/// Matches `DEFAULT_AUTH0_DOMAIN` in Python's `device_auth.py:16`.
pub const DEFAULT_AUTH0_DOMAIN: &str = "cognee.eu.auth0.com";

/// Default Auth0 audience used when `COGNEE_AUTH0_AUDIENCE` is unset.
///
/// Matches `DEFAULT_AUTH0_AUDIENCE` in Python's `device_auth.py:17`.
pub const DEFAULT_AUTH0_AUDIENCE: &str = "cognee:api";

/// Default OAuth2 scope requested during device-code login.
///
/// Matches `DEFAULT_SCOPE` in Python's `device_auth.py:18`.
pub const DEFAULT_SCOPE: &str = "openid profile email offline_access";

/// Default management API URL used when `COGNEE_CLOUD_URL` is unset.
///
/// Matches `DEFAULT_MANAGEMENT_URL` in Python's `management_api.py:18`.
pub const DEFAULT_MANAGEMENT_URL: &str = "https://api.dev.cloud.topoteretes.com";

/// Read the Auth0 tenant domain from the environment, defaulting to
/// [`DEFAULT_AUTH0_DOMAIN`].
///
/// Mirrors `_get_auth0_domain()` in `device_auth.py:30–31`.
pub fn auth0_domain() -> String {
    env::var("COGNEE_AUTH0_DOMAIN").unwrap_or_else(|_| DEFAULT_AUTH0_DOMAIN.to_string())
}

/// Read the Auth0 native-app client ID from the environment.
///
/// There is no default — `COGNEE_AUTH0_DEVICE_CLIENT_ID` MUST be set for
/// cloud-mode `serve()`. Mirrors `_get_auth0_client_id()` in
/// `device_auth.py:34–41`.
///
/// # Errors
///
/// Returns [`CloudError::MissingEnv`] if the variable is unset or empty.
pub fn auth0_client_id() -> CloudResult<String> {
    match env::var("COGNEE_AUTH0_DEVICE_CLIENT_ID") {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(CloudError::MissingEnv("COGNEE_AUTH0_DEVICE_CLIENT_ID")),
    }
}

/// Read the Auth0 API audience from the environment, defaulting to
/// [`DEFAULT_AUTH0_AUDIENCE`].
///
/// Mirrors `_get_auth0_audience()` in `device_auth.py:44–45`.
pub fn auth0_audience() -> String {
    env::var("COGNEE_AUTH0_AUDIENCE").unwrap_or_else(|_| DEFAULT_AUTH0_AUDIENCE.to_string())
}

/// Read the Cognee Cloud management URL from the environment, defaulting
/// to [`DEFAULT_MANAGEMENT_URL`]. Any trailing `/` is stripped so URLs can
/// be concatenated with `"/path"` reliably.
///
/// Mirrors `_get_management_url()` in `management_api.py:21–22` which calls
/// `.rstrip('/')`.
pub fn cloud_url() -> String {
    env::var("COGNEE_CLOUD_URL")
        .unwrap_or_else(|_| DEFAULT_MANAGEMENT_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-var state is global; serialize these tests so they don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(keys: &[(&str, Option<&str>)], f: F) {
        let _guard = ENV_LOCK.lock().expect("env lock poison is unrecoverable");
        let previous: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|(k, _)| (k.to_string(), env::var(k).ok()))
            .collect();
        for (k, v) in keys {
            match v {
                Some(value) => unsafe { env::set_var(k, value) },
                None => unsafe { env::remove_var(k) },
            }
        }
        f();
        for (k, v) in previous {
            match v {
                Some(value) => unsafe { env::set_var(&k, value) },
                None => unsafe { env::remove_var(&k) },
            }
        }
    }

    #[test]
    fn auth0_domain_defaults_when_unset() {
        with_env(&[("COGNEE_AUTH0_DOMAIN", None)], || {
            assert_eq!(auth0_domain(), DEFAULT_AUTH0_DOMAIN);
        });
    }

    #[test]
    fn auth0_domain_honours_env() {
        with_env(
            &[("COGNEE_AUTH0_DOMAIN", Some("example.auth0.com"))],
            || {
                assert_eq!(auth0_domain(), "example.auth0.com");
            },
        );
    }

    #[test]
    fn auth0_client_id_errors_when_unset() {
        with_env(&[("COGNEE_AUTH0_DEVICE_CLIENT_ID", None)], || {
            let err = auth0_client_id().expect_err("must error without env var");
            assert!(matches!(
                err,
                CloudError::MissingEnv("COGNEE_AUTH0_DEVICE_CLIENT_ID")
            ));
        });
    }

    #[test]
    fn auth0_client_id_errors_when_empty() {
        with_env(&[("COGNEE_AUTH0_DEVICE_CLIENT_ID", Some(""))], || {
            let err = auth0_client_id().expect_err("must error on empty value");
            assert!(matches!(
                err,
                CloudError::MissingEnv("COGNEE_AUTH0_DEVICE_CLIENT_ID")
            ));
        });
    }

    #[test]
    fn auth0_client_id_reads_env() {
        with_env(&[("COGNEE_AUTH0_DEVICE_CLIENT_ID", Some("abc123"))], || {
            assert_eq!(auth0_client_id().expect("env set above"), "abc123");
        });
    }

    #[test]
    fn auth0_audience_defaults_when_unset() {
        with_env(&[("COGNEE_AUTH0_AUDIENCE", None)], || {
            assert_eq!(auth0_audience(), DEFAULT_AUTH0_AUDIENCE);
        });
    }

    #[test]
    fn auth0_audience_honours_env() {
        with_env(&[("COGNEE_AUTH0_AUDIENCE", Some("cognee:beta"))], || {
            assert_eq!(auth0_audience(), "cognee:beta");
        });
    }

    #[test]
    fn cloud_url_defaults_when_unset() {
        with_env(&[("COGNEE_CLOUD_URL", None)], || {
            assert_eq!(cloud_url(), DEFAULT_MANAGEMENT_URL);
        });
    }

    #[test]
    fn cloud_url_strips_trailing_slashes() {
        with_env(
            &[("COGNEE_CLOUD_URL", Some("https://example.com/////"))],
            || {
                assert_eq!(cloud_url(), "https://example.com");
            },
        );
    }

    #[test]
    fn cloud_url_honours_env() {
        with_env(
            &[("COGNEE_CLOUD_URL", Some("https://api.prod.example.com"))],
            || {
                assert_eq!(cloud_url(), "https://api.prod.example.com");
            },
        );
    }
}
