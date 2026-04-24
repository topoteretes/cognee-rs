//! Top-level `serve()` orchestrator — connects the SDK to a remote Cognee
//! instance.
//!
//! Line-by-line port of `cognee/api/v1/serve/serve.py`. Two execution
//! modes:
//!
//! - **Direct mode** ([`ServeConfig::direct`]) — caller supplies an
//!   explicit URL (and optional API key). No Auth0, no management API.
//!   Matches Python's `_serve_direct` (`serve.py:75–101`).
//! - **Cloud mode** ([`ServeConfig::cloud`]) — full OAuth2 device-code
//!   flow, tenant provisioning via the management API, and on-disk
//!   credential caching. Matches Python's `_serve_cloud`
//!   (`serve.py:104–230`).
//!
//! On success the resulting [`CloudClient`] is both returned (so the
//! caller can hold a handle) and installed via [`set_client`] into the
//! process-wide singleton so that the V2 API functions
//! (`remember` / `recall` / `improve` / `forget`) route through it.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use crate::cloud_client::CloudClient;
use crate::credentials::{self, CloudCredentials, is_token_expired};
use crate::device_auth::{device_code_login, extract_email_from_id_token, refresh_access_token};
use crate::error::{CloudError, CloudResult};
use crate::management_api::{
    DEFAULT_API_KEY_MAX_RETRIES, DEFAULT_TENANT_POLL_INTERVAL, DEFAULT_TENANT_POLL_TIMEOUT,
    create_tenant, get_current_tenant, get_or_create_api_key, get_service_url,
};
use crate::state::set_client;

/// Environment variable consulted when [`ServeConfig::url`] is not set.
///
/// Matches Python's `os.getenv("COGNEE_SERVICE_URL")` in `serve.py:61`.
const ENV_SERVICE_URL: &str = "COGNEE_SERVICE_URL";

/// Environment variable consulted when [`ServeConfig::api_key`] is not set.
///
/// Matches Python's `os.getenv("COGNEE_API_KEY", "")` in `serve.py:62`.
const ENV_API_KEY: &str = "COGNEE_API_KEY";

/// Configuration for a single [`serve`] invocation.
///
/// Two construction helpers pick the execution mode up front:
/// - [`ServeConfig::direct`] selects direct mode with an explicit URL.
/// - [`ServeConfig::cloud`] selects cloud mode (no URL, falls back to
///   env / device-code flow).
///
/// The builder methods (`.api_key`, `.auth0_domain`, ...) layer
/// additional optional overrides. All fields default to `None`, which
/// means "use the env var / hard-coded default for that setting".
#[derive(Debug, Default, Clone)]
pub struct ServeConfig {
    /// Direct service URL. Presence of a value flips [`serve`] to
    /// direct mode. Matches Python's `url` argument (`serve.py:17`).
    pub url: Option<String>,
    /// API key for authenticating against the service URL. Used by both
    /// direct and cloud mode (cloud mode overwrites this with the key
    /// minted by the management API). Matches Python's `api_key`
    /// argument (`serve.py:18`).
    pub api_key: Option<String>,
    /// Override for the management API base URL (cloud mode only).
    /// Matches Python's `management_url` kwarg (`serve.py:21`). An
    /// explicit value here wins over both `COGNEE_CLOUD_URL` and the
    /// compiled-in default.
    pub cloud_url: Option<String>,
    /// Override for the Auth0 tenant domain (cloud mode only). Matches
    /// Python's `auth0_domain` kwarg (`serve.py:22`).
    pub auth0_domain: Option<String>,
    /// Override for the Auth0 native-app client ID used in the device
    /// code flow (cloud mode only). Matches Python's `auth0_client_id`
    /// kwarg (`serve.py:23`).
    pub auth0_client_id: Option<String>,
    /// Override for the Auth0 API audience (cloud mode only). Matches
    /// Python's `auth0_audience` kwarg (`serve.py:24`).
    pub auth0_audience: Option<String>,
}

impl ServeConfig {
    /// Build a config for **direct mode** — connect to `url` without
    /// running the Auth0 device flow.
    ///
    /// Equivalent to Python `cognee.serve(url=...)`.
    pub fn direct(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            ..Default::default()
        }
    }

    /// Build a config for **cloud mode** — no URL, full device-code
    /// flow will run unless a valid cached credential exists.
    ///
    /// Equivalent to Python `cognee.serve()`.
    pub fn cloud() -> Self {
        Self::default()
    }

    /// Set the API key. Wins over the `COGNEE_API_KEY` env var.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the Auth0 tenant domain (cloud mode only).
    pub fn auth0_domain(mut self, domain: impl Into<String>) -> Self {
        self.auth0_domain = Some(domain.into());
        self
    }

    /// Override the Auth0 native-app client ID (cloud mode only).
    pub fn auth0_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.auth0_client_id = Some(client_id.into());
        self
    }

    /// Override the Auth0 API audience (cloud mode only).
    pub fn auth0_audience(mut self, audience: impl Into<String>) -> Self {
        self.auth0_audience = Some(audience.into());
        self
    }

    /// Override the management API base URL (cloud mode only).
    pub fn cloud_url(mut self, url: impl Into<String>) -> Self {
        self.cloud_url = Some(url.into());
        self
    }
}

/// Connect the Cognee Rust SDK to a Cognee instance.
///
/// Dispatches on the presence of a service URL:
///
/// - `ServeConfig::url` set, or `COGNEE_SERVICE_URL` in the env →
///   [`serve_direct`] is called with that URL plus the resolved API key.
/// - Otherwise → [`serve_cloud_inner`] runs the full device-code + tenant
///   provisioning flow.
///
/// On success the resulting [`CloudClient`] is installed via
/// [`crate::state::set_client`] so that subsequent V2 API calls route
/// through it.
///
/// Mirrors Python's top-level `serve()` in `serve.py:17–72`.
///
/// # Errors
///
/// Propagates any [`CloudError`] raised by the underlying steps:
/// credential IO, Auth0 device flow, management API, or service URL
/// health probe.
pub async fn serve(config: ServeConfig) -> CloudResult<Arc<CloudClient>> {
    // serve.py:61–62 — resolve URL / API key from the config, falling
    // back to the env vars.
    let service_url = config
        .url
        .clone()
        .or_else(|| env::var(ENV_SERVICE_URL).ok())
        .filter(|s| !s.is_empty());
    let resolved_api_key = config
        .api_key
        .clone()
        .or_else(|| env::var(ENV_API_KEY).ok())
        .unwrap_or_default();

    match service_url {
        Some(url) => serve_direct(&url, &resolved_api_key).await,
        None => serve_cloud_inner(&config).await,
    }
}

/// Direct-mode connection — build a [`CloudClient`] for the given URL,
/// probe `/health`, persist a minimal credential record, and install the
/// client as the process-wide remote singleton.
///
/// Line-by-line port of Python's `_serve_direct` in `serve.py:75–101`.
///
/// The health probe is advisory — a failure is logged via `tracing::warn`
/// but does NOT abort the connect (matches `serve.py:84–86`).
pub async fn serve_direct(service_url: &str, api_key: &str) -> CloudResult<Arc<CloudClient>> {
    let service_url = service_url.trim_end_matches('/');
    let client = CloudClient::new(service_url, api_key)?;

    if !client.health_check().await {
        tracing::warn!(
            target: "cognee_cloud::serve",
            %service_url,
            "Instance at {service_url} did not respond to health check",
        );
    }

    // Persist so subsequent `serve()` calls reconnect without args.
    // Mirrors `serve.py:88–96` — note the sentinel `email = "local"`.
    let creds = CloudCredentials {
        service_url: service_url.to_string(),
        api_key: api_key.to_string(),
        email: "local".to_string(),
        ..CloudCredentials::default()
    };
    credentials::save(&creds).await?;

    set_client(client.clone()).await;
    let mode = if service_url.contains("localhost") || service_url.contains("127.0.0.1") {
        "local"
    } else {
        "remote"
    };
    println!("  Connected to Cognee ({mode}) at {service_url}");
    Ok(client)
}

/// Full cloud flow — saved credentials → optional token refresh → Auth0
/// device-code login → tenant discovery / provisioning → API-key
/// provisioning → install & return client.
///
/// Line-by-line port of Python's `_serve_cloud` in `serve.py:104–230`.
async fn serve_cloud_inner(config: &ServeConfig) -> CloudResult<Arc<CloudClient>> {
    // serve.py:131–134 — resolve management URL, prefer explicit arg
    // over env, trim trailing slash.
    let management_url = config
        .cloud_url
        .clone()
        .unwrap_or_else(crate::config::cloud_url)
        .trim_end_matches('/')
        .to_string();

    let auth0_domain = config.auth0_domain.as_deref();
    let auth0_client_id = config.auth0_client_id.as_deref();
    let auth0_audience = config.auth0_audience.as_deref();

    // Step 1: saved credentials (serve.py:137–173).
    if let Some(mut creds) = credentials::load().await?
        && !creds.service_url.is_empty()
        && !creds.api_key.is_empty()
    {
        if !is_token_expired(&creds) {
            tracing::info!(
                target: "cognee_cloud::serve",
                email = %creds.email,
                "Using saved credentials for {}",
                creds.email,
            );
            let client = CloudClient::new(&creds.service_url, &creds.api_key)?;
            if client.health_check().await {
                set_client(client.clone()).await;
                println!("  Connected to Cognee Cloud at {}", creds.service_url);
                return Ok(client);
            }
            tracing::warn!(
                target: "cognee_cloud::serve",
                service_url = %creds.service_url,
                "Saved service URL unreachable, re-authenticating",
            );
            client.close().await;
        } else if let Some(rt) = creds.refresh_token.clone() {
            match refresh_access_token(&rt, auth0_domain, auth0_client_id).await {
                Ok(tok) => {
                    tracing::info!(
                        target: "cognee_cloud::serve",
                        email = %creds.email,
                        "Refreshed expired token",
                    );
                    creds.access_token = tok.access_token;
                    if let Some(new_rt) = tok.refresh_token {
                        creds.refresh_token = Some(new_rt);
                    }
                    creds.expires_at =
                        chrono::Utc::now().timestamp() as f64 + tok.expires_in as f64;
                    credentials::save(&creds).await?;

                    let client = CloudClient::new(&creds.service_url, &creds.api_key)?;
                    if client.health_check().await {
                        set_client(client.clone()).await;
                        println!("  Connected to Cognee Cloud at {}", creds.service_url);
                        return Ok(client);
                    }
                    client.close().await;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "cognee_cloud::serve",
                        error = %e,
                        "Token refresh failed, re-authenticating",
                    );
                }
            }
        }
    }

    // Step 2: Device code flow (serve.py:175–180).
    println!("  Authenticating with Cognee Cloud...");
    let token = device_code_login(auth0_domain, auth0_client_id, auth0_audience).await?;

    // Step 3: extract email (serve.py:183). Python treats a missing
    // id_token or missing `email` claim as `None`; we match that by
    // swallowing the decode error here and only raising later if the
    // management API needs the email to create a new tenant.
    let email: Option<String> = token
        .id_token
        .as_deref()
        .and_then(|t| extract_email_from_id_token(t).ok());

    // Step 4: tenant discovery / provisioning (serve.py:186–193).
    let tenant = match get_current_tenant(&management_url, &token.access_token).await? {
        Some(t) if !t.id.is_empty() => t,
        _ => {
            let email_ref = email.as_deref().ok_or_else(|| {
                CloudError::Auth(
                    "Could not extract email from token. \
                     Ensure the Auth0 app includes 'email' in the scope."
                        .to_string(),
                )
            })?;
            create_tenant(
                &management_url,
                &token.access_token,
                email_ref,
                DEFAULT_TENANT_POLL_TIMEOUT,
                DEFAULT_TENANT_POLL_INTERVAL,
            )
            .await?
        }
    };

    // Step 5: service URL (serve.py:196).
    let service_url = get_service_url(&management_url, &token.access_token).await?;

    // Step 6: API key (serve.py:199).
    let api_key = get_or_create_api_key(
        &management_url,
        &token.access_token,
        DEFAULT_API_KEY_MAX_RETRIES,
    )
    .await?;

    // Step 7: persist + connect (serve.py:202–230).
    let creds = CloudCredentials {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        expires_at: chrono::Utc::now().timestamp() as f64 + token.expires_in as f64,
        service_url: service_url.clone(),
        api_key: api_key.clone(),
        management_url: management_url.clone(),
        tenant_id: tenant.id.clone(),
        tenant_name: tenant.name.clone(),
        email: email.clone().unwrap_or_default(),
    };
    credentials::save(&creds).await?;

    let client = CloudClient::new(&service_url, &api_key)?;
    if !client.health_check().await {
        tracing::warn!(
            target: "cognee_cloud::serve",
            %service_url,
            "Service URL {service_url} not responding to health check — may still be starting",
        );
    }
    set_client(client.clone()).await;
    println!("  Connected to Cognee Cloud at {service_url}");
    if let Some(e) = &email {
        println!("  Tenant: {} ({})", tenant.name, e);
    }
    Ok(client)
}

/// Convenience wrapper for direct-mode [`serve`]. Equivalent to
/// `serve(ServeConfig::direct(url).api_key(api_key))` when `api_key` is
/// non-empty.
pub async fn serve_url(
    url: impl Into<String>,
    api_key: Option<impl Into<String>>,
) -> CloudResult<Arc<CloudClient>> {
    let mut cfg = ServeConfig::direct(url);
    if let Some(k) = api_key {
        cfg = cfg.api_key(k);
    }
    serve(cfg).await
}

/// Convenience wrapper for cloud-mode [`serve`]. Equivalent to
/// `serve(ServeConfig::cloud())`.
pub async fn serve_cloud() -> CloudResult<Arc<CloudClient>> {
    serve(ServeConfig::cloud()).await
}

/// Ensures unused imports compile out cleanly if downstream users enable
/// only a subset of features. Keeps `Duration` referenced even when the
/// doc examples are stripped.
const _: fn() = || {
    let _ = std::mem::size_of::<Duration>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{clear_client, get_client, is_connected};
    use std::sync::Mutex;

    // `serve_direct` both mutates the process-wide CLOUD_CLIENT singleton
    // and writes to `$HOME/.cognee/cloud_credentials.json`. All tests in
    // this module must hold `ENV_LOCK` to serialize env-var + singleton
    // access. `cargo test -p cognee-cloud` runs with `--test-threads=1`
    // in CI per the top-level harness, but the explicit lock makes the
    // tests robust to local runs too.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Test fixture that isolates `$HOME` (so credential writes hit a
    /// temp dir) and clears the singleton before/after the closure.
    /// Restores `$HOME` when it returns.
    fn with_isolated_env<F, Fut>(tmp: &std::path::Path, body: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());

        let prev_home = env::var("HOME").ok();
        let prev_service_url = env::var(ENV_SERVICE_URL).ok();
        let prev_api_key = env::var(ENV_API_KEY).ok();

        // SAFETY: we hold ENV_LOCK for the duration of this call, so no
        // other test in this module is racing us on the env table.
        unsafe {
            env::set_var("HOME", tmp);
            env::remove_var(ENV_SERVICE_URL);
            env::remove_var(ENV_API_KEY);
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime for serve tests");

        rt.block_on(async {
            clear_client().await;
            body().await;
            clear_client().await;
        });

        // Restore env vars (set/remove in reverse order).
        unsafe {
            match prev_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match prev_service_url {
                Some(v) => env::set_var(ENV_SERVICE_URL, v),
                None => env::remove_var(ENV_SERVICE_URL),
            }
            match prev_api_key {
                Some(v) => env::set_var(ENV_API_KEY, v),
                None => env::remove_var(ENV_API_KEY),
            }
        }
    }

    #[test]
    fn serve_config_direct_sets_url_and_no_other_fields() {
        let cfg = ServeConfig::direct("https://example.com");
        assert_eq!(cfg.url.as_deref(), Some("https://example.com"));
        assert!(cfg.api_key.is_none());
        assert!(cfg.cloud_url.is_none());
        assert!(cfg.auth0_domain.is_none());
        assert!(cfg.auth0_client_id.is_none());
        assert!(cfg.auth0_audience.is_none());
    }

    #[test]
    fn serve_config_cloud_is_all_none() {
        let cfg = ServeConfig::cloud();
        assert!(cfg.url.is_none());
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn serve_config_builders_chain() {
        let cfg = ServeConfig::direct("https://example.com")
            .api_key("my-key")
            .auth0_domain("auth.example.com")
            .auth0_client_id("client-123")
            .auth0_audience("cognee:test")
            .cloud_url("https://mgmt.example.com");
        assert_eq!(cfg.url.as_deref(), Some("https://example.com"));
        assert_eq!(cfg.api_key.as_deref(), Some("my-key"));
        assert_eq!(cfg.auth0_domain.as_deref(), Some("auth.example.com"));
        assert_eq!(cfg.auth0_client_id.as_deref(), Some("client-123"));
        assert_eq!(cfg.auth0_audience.as_deref(), Some("cognee:test"));
        assert_eq!(cfg.cloud_url.as_deref(), Some("https://mgmt.example.com"));
    }

    #[test]
    fn serve_direct_installs_client_and_persists_credentials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Use a fake URL that definitely won't respond to /health — we
        // still expect the direct-mode path to proceed, just with the
        // health check logged as a warning (matches `serve.py:84–86`).
        with_isolated_env(tmp.path(), || async {
            let client = serve_direct("http://127.0.0.1:1/", "api-key-xyz")
                .await
                .expect("serve_direct should succeed even when /health fails");

            // Trailing slash is stripped.
            assert_eq!(client.service_url, "http://127.0.0.1:1");
            assert_eq!(client.api_key, "api-key-xyz");

            // Singleton populated.
            assert!(is_connected().await);
            let installed = get_client().await.expect("singleton set");
            assert!(
                Arc::ptr_eq(&installed, &client),
                "serve_direct must install the returned client"
            );

            // Credentials file written under $HOME.
            let path = tmp.path().join(".cognee").join("cloud_credentials.json");
            assert!(path.exists(), "credentials file should be written");
            let loaded = credentials::load()
                .await
                .expect("load credentials")
                .expect("file present");
            assert_eq!(loaded.service_url, "http://127.0.0.1:1");
            assert_eq!(loaded.api_key, "api-key-xyz");
            assert_eq!(loaded.email, "local");
        });
    }

    #[test]
    fn serve_direct_via_mockito_health_check_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let mut server = mockito::Server::new_async().await;
            let _m = server
                .mock("GET", "/health")
                .with_status(200)
                .create_async()
                .await;

            let client = serve_direct(&server.url(), "k")
                .await
                .expect("serve_direct ok");
            assert_eq!(client.service_url, server.url().trim_end_matches('/'));
            assert!(is_connected().await);
        });
    }

    #[test]
    fn serve_dispatches_to_direct_when_url_given() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let client = serve(ServeConfig::direct("http://127.0.0.1:1").api_key("k"))
                .await
                .expect("direct mode dispatch");
            assert_eq!(client.api_key, "k");
        });
    }

    #[test]
    fn serve_direct_picks_up_env_service_url() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            // SAFETY: ENV_LOCK is held inside with_isolated_env.
            unsafe {
                env::set_var(ENV_SERVICE_URL, "http://127.0.0.1:1");
                env::set_var(ENV_API_KEY, "env-api-key");
            }
            let client = serve(ServeConfig::cloud())
                .await
                .expect("env var picks direct path");
            assert_eq!(client.service_url, "http://127.0.0.1:1");
            assert_eq!(client.api_key, "env-api-key");
        });
    }

    #[test]
    fn serve_url_wrapper_passes_api_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let client = serve_url("http://127.0.0.1:1", Some("wrapper-key"))
                .await
                .expect("serve_url ok");
            assert_eq!(client.api_key, "wrapper-key");

            // Also: serve_url with None api_key works.
            clear_client().await;
            let client = serve_url("http://127.0.0.1:1", Option::<String>::None)
                .await
                .expect("serve_url no key ok");
            assert_eq!(client.api_key, "");
        });
    }

    #[test]
    fn serve_direct_marks_localhost_mode() {
        // This test doesn't actually observe the printed mode (stdout is
        // hard to capture portably), but it exercises the localhost
        // branch to make sure the predicate doesn't panic.
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let c = serve_direct("http://localhost:1234/", "k")
                .await
                .expect("localhost direct ok");
            assert_eq!(c.service_url, "http://localhost:1234");
        });
    }

    #[test]
    fn serve_cloud_uses_cached_valid_credentials_and_skips_device_flow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let mut server = mockito::Server::new_async().await;
            let _m = server
                .mock("GET", "/health")
                .with_status(200)
                .create_async()
                .await;

            // Write a valid (non-expired) credential pointing at the
            // mockito server. serve_cloud should short-circuit on it
            // without touching Auth0.
            let future_ts = chrono::Utc::now().timestamp() as f64 + 86_400.0;
            let creds = CloudCredentials {
                access_token: "at".into(),
                refresh_token: Some("rt".into()),
                expires_at: future_ts,
                service_url: server.url(),
                api_key: "cached-key".into(),
                management_url: "https://mgmt.example".into(),
                tenant_id: "tid".into(),
                tenant_name: "tname".into(),
                email: "u@example.com".into(),
            };
            credentials::save(&creds).await.expect("save fixture");

            let client = serve(ServeConfig::cloud())
                .await
                .expect("cached creds should short-circuit");
            assert_eq!(client.api_key, "cached-key");
            assert!(is_connected().await);
        });
    }

    #[test]
    fn serve_cloud_propagates_credentials_load_error_as_invalid() {
        // Write a malformed credentials file and ensure serve() surfaces
        // the InvalidCredentials error rather than silently proceeding.
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let path = tmp.path().join(".cognee");
            tokio::fs::create_dir_all(&path)
                .await
                .expect("create parent");
            tokio::fs::write(path.join("cloud_credentials.json"), b"not json")
                .await
                .expect("write bad file");

            let err = serve(ServeConfig::cloud())
                .await
                .expect_err("malformed creds must surface");
            assert!(
                matches!(err, CloudError::InvalidCredentials(_)),
                "unexpected err: {err:?}"
            );
        });
    }
}
