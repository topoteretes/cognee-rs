//! Shared async cloud operations: `run_serve`, `run_disconnect`.
//!
//! These functions contain the pure-Rust async logic shared between every
//! language binding surface (C API, Neon JS, Python). Each function takes a
//! `serde_json::Value` opts argument, performs the operation against the
//! underlying cognee-lib APIs, and returns a result (or a [`SdkError`]).
//!
//! Both functions are gated behind `#[cfg(feature = "cloud")]`. When the
//! feature is absent the functions return [`SdkError::FeatureNotBuilt`],
//! which each binding converts to its native feature-not-built error type.
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## opts shapes
//!
//! `run_serve`:
//!   `{"url?":"…","apiKey?":"…","cloudUrl?":"…","auth0Domain?":"…",
//!    "auth0ClientId?":"…","auth0Audience?":"…"}`
//!
//! `run_disconnect`:
//!   `{"wipeCredentials?":false}`

use crate::SdkError;

// ---------------------------------------------------------------------------
// Feature-gated helpers (build_serve_config is pub so bindings can reuse it
// if they need fine-grained control, but run_serve / run_disconnect are the
// primary entry points).
// ---------------------------------------------------------------------------

/// Build a [`ServeConfig`] from a `serde_json::Value` opts object.
///
/// Presence of a non-empty `"url"` key selects **direct mode** (no Auth0
/// flow). Absence selects **cloud mode** (Auth0 device-code flow).
///
/// Optional keys: `apiKey`, `cloudUrl`, `auth0Domain`, `auth0ClientId`,
/// `auth0Audience`.
#[cfg(feature = "cloud")]
pub fn build_serve_config(opts: &serde_json::Value) -> cognee_lib::ServeConfig {
    use cognee_lib::ServeConfig;

    let url = opts
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let mut cfg = match url {
        Some(u) => ServeConfig::direct(u),
        None => ServeConfig::cloud(),
    };

    if let Some(k) = opts.get("apiKey").and_then(|v| v.as_str()) {
        cfg = cfg.api_key(k);
    }
    if let Some(u) = opts.get("cloudUrl").and_then(|v| v.as_str()) {
        cfg = cfg.cloud_url(u);
    }
    if let Some(d) = opts.get("auth0Domain").and_then(|v| v.as_str()) {
        cfg = cfg.auth0_domain(d);
    }
    if let Some(c) = opts.get("auth0ClientId").and_then(|v| v.as_str()) {
        cfg = cfg.auth0_client_id(c);
    }
    if let Some(a) = opts.get("auth0Audience").and_then(|v| v.as_str()) {
        cfg = cfg.auth0_audience(a);
    }

    cfg
}

/// Connect to a Cognee Cloud instance and return a status object.
///
/// On success, returns:
/// ```json
/// {"connected": true, "serviceUrl": "https://…"}
/// ```
///
/// When the `cloud` feature is not compiled in, returns
/// [`SdkError::FeatureNotBuilt`].
pub async fn run_serve(opts: serde_json::Value) -> Result<serde_json::Value, SdkError> {
    #[cfg(feature = "cloud")]
    {
        use cognee_lib::serve;

        let config = build_serve_config(&opts);
        let client = serve(config)
            .await
            .map_err(|e| SdkError::Runtime(format!("serve failed: {e}")))?;

        Ok(serde_json::json!({
            "connected": true,
            "serviceUrl": client.service_url,
        }))
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = opts;
        Err(SdkError::FeatureNotBuilt(
            "cloud feature not compiled in this build".to_string(),
        ))
    }
}

/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// `opts["wipeCredentials"]` (boolean, default `false`) controls whether the
/// on-disk credential cache is deleted.
///
/// On success returns `serde_json::Value::Null` (void op).
///
/// When the `cloud` feature is not compiled in, returns
/// [`SdkError::FeatureNotBuilt`].
pub async fn run_disconnect(opts: serde_json::Value) -> Result<serde_json::Value, SdkError> {
    #[cfg(feature = "cloud")]
    {
        use cognee_lib::disconnect;

        let wipe = opts
            .get("wipeCredentials")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        disconnect(wipe)
            .await
            .map_err(|e| SdkError::Runtime(format!("disconnect failed: {e}")))?;

        // D9 — void op returns null.
        Ok(serde_json::Value::Null)
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = opts;
        Err(SdkError::FeatureNotBuilt(
            "cloud feature not compiled in this build".to_string(),
        ))
    }
}
