//! Phase 6 — cloud ops: `cogneeServe` / `cogneeDisconnect`.
//!
//! Both functions are gated behind `#[cfg(feature = "cloud")]`.
//! When the feature is absent the exported functions throw a typed JS error
//! with `code = "FEATURE_NOT_BUILT"`.
//!
//! ## Function shapes
//!
//! - `cogneeServe(opts?) -> Promise<{ connected: true, serviceUrl: string }>`
//!   — deserialises JSON opts into `ServeConfig` fields and calls
//!   `cognee_lib::serve(config)`. Returns a minimal status object so callers
//!   can log the connection URL without depending on the opaque `CloudClient`.
//!   `opts.url` → direct mode; absent → cloud (device-code) mode.
//!   `opts.apiKey`, `opts.cloudUrl`, `opts.auth0Domain`, `opts.auth0ClientId`,
//!   `opts.auth0Audience` map to the corresponding `ServeConfig` builder methods.
//!
//! - `cogneeDisconnect(opts?) -> Promise<void>` — calls
//!   `cognee_lib::disconnect(wipe_credentials)`.  `opts.wipeCredentials`
//!   (boolean, default `false`) controls whether the on-disk credential cache
//!   is erased.
//!
//! ## Process-wide singletons
//!
//! `serve()` / `disconnect()` operate on the process-wide `CloudClient`
//! singleton, not on a `CogneeHandle`.  These functions therefore do NOT
//! accept a handle as their first argument — they take only the `opts` object.
//! Document in calling code that cloud mode (`cogneeServe()` with no URL)
//! triggers an Auth0 device-code flow that requires a TTY; direct mode
//! (`opts.url` set) works headlessly with `opts.apiKey`.

use neon::prelude::*;

use crate::errors::{SdkError, throw_sdk_error};
// Only used in the feature-enabled paths.
#[cfg(feature = "cloud")]
use crate::json::{parse_js, read_opts};
#[cfg(feature = "cloud")]
use crate::runtime::runtime;

// ---------------------------------------------------------------------------
// Feature-gated implementations.
// ---------------------------------------------------------------------------

#[cfg(feature = "cloud")]
mod inner {
    use cognee_lib::{ServeConfig, disconnect, serve};

    use super::*;

    /// Build a [`ServeConfig`] from a `serde_json::Value` opts object.
    pub(super) fn build_serve_config(opts: &serde_json::Value) -> ServeConfig {
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

    /// Call `serve(config)` and return `{ connected: true, serviceUrl }`.
    pub(super) async fn run_serve(opts: serde_json::Value) -> Result<String, SdkError> {
        let config = build_serve_config(&opts);
        let client = serve(config)
            .await
            .map_err(|e| SdkError::Runtime(format!("serve failed: {e}")))?;

        let result = serde_json::json!({
            "connected": true,
            "serviceUrl": client.service_url,
        });
        serde_json::to_string(&result)
            .map_err(|e| SdkError::Runtime(format!("failed to serialize serve result: {e}")))
    }

    /// Call `disconnect(wipe_credentials)`.
    pub(super) async fn run_disconnect(opts: serde_json::Value) -> Result<(), SdkError> {
        let wipe = opts
            .get("wipeCredentials")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        disconnect(wipe)
            .await
            .map_err(|e| SdkError::Runtime(format!("disconnect failed: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Exported native functions.
// ---------------------------------------------------------------------------

/// `cogneeServe(opts?) -> Promise<{ connected: true, serviceUrl: string }>`
///
/// Connect the SDK to a Cognee Cloud instance.
///
/// `opts.url` (string) selects **direct mode** — no Auth0 flow, just an HTTP
/// connection to the given URL.  When absent, **cloud mode** runs the Auth0
/// device-code flow which requires a TTY.  Direct mode works headlessly and
/// is suitable for CI/testing with a local Cognee HTTP server.
///
/// Optional fields: `apiKey`, `cloudUrl`, `auth0Domain`, `auth0ClientId`,
/// `auth0Audience`.
pub fn cognee_serve(mut cx: FunctionContext) -> JsResult<JsPromise> {
    #[cfg(feature = "cloud")]
    {
        let opts = read_opts(&mut cx, 0)?;

        let channel = cx.channel();
        let (deferred, promise) = cx.promise();

        runtime().spawn(async move {
            let result = inner::run_serve(opts).await;
            deferred.settle_with(&channel, move |mut cx| match result {
                Ok(json_str) => parse_js(&mut cx, &json_str),
                Err(e) => throw_sdk_error(&mut cx, e),
            });
        });

        Ok(promise)
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = cx;
        let (deferred, promise) = cx.promise();
        let channel = cx.channel();
        deferred.settle_with(&channel, |mut cx| -> JsResult<JsValue> {
            throw_sdk_error(
                &mut cx,
                SdkError::FeatureNotBuilt(
                    "cloud feature not compiled in this build of cognee-neon".to_string(),
                ),
            )
        });
        Ok(promise)
    }
}

/// `cogneeDisconnect(opts?) -> Promise<void>`
///
/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// `opts.wipeCredentials` (boolean, default `false`) — when `true`, the
/// on-disk credential cache (`~/.cognee/cloud_credentials.json`) is deleted
/// so the next `cogneeServe()` must re-authenticate.
pub fn cognee_disconnect(mut cx: FunctionContext) -> JsResult<JsPromise> {
    #[cfg(feature = "cloud")]
    {
        let opts = read_opts(&mut cx, 0)?;

        let channel = cx.channel();
        let (deferred, promise) = cx.promise();

        runtime().spawn(async move {
            let result = inner::run_disconnect(opts).await;
            deferred.settle_with(&channel, move |mut cx| match result {
                Ok(()) => Ok(cx.undefined()),
                Err(e) => throw_sdk_error(&mut cx, e),
            });
        });

        Ok(promise)
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = cx;
        let (deferred, promise) = cx.promise();
        let channel = cx.channel();
        deferred.settle_with(&channel, |mut cx| -> JsResult<JsValue> {
            throw_sdk_error(
                &mut cx,
                SdkError::FeatureNotBuilt(
                    "cloud feature not compiled in this build of cognee-neon".to_string(),
                ),
            )
        });
        Ok(promise)
    }
}

