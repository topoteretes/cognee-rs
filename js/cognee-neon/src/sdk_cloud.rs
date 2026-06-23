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
//!   `cognee_cloud::serve(config)`. Returns a minimal status object so callers
//!   can log the connection URL without depending on the opaque `CloudClient`.
//!   `opts.url` → direct mode; absent → cloud (device-code) mode.
//!   `opts.apiKey`, `opts.cloudUrl`, `opts.auth0Domain`, `opts.auth0ClientId`,
//!   `opts.auth0Audience` map to the corresponding `ServeConfig` builder methods.
//!
//! - `cogneeDisconnect(opts?) -> Promise<void>` — calls
//!   `cognee_cloud::disconnect(wipe_credentials)`.  `opts.wipeCredentials`
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

use crate::errors::throw_sdk_error;
use crate::json::{parse_js, read_opts};
use crate::runtime::runtime;
use cognee_bindings_common::ops::cloud;

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
    let opts = read_opts(&mut cx, 0)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = cloud::run_serve(opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                // Serialising a serde_json::Value cannot fail (no non-string
                // map keys are possible), so the fallback is unreachable.
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeDisconnect(opts?) -> Promise<void>`
///
/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// `opts.wipeCredentials` (boolean, default `false`) — when `true`, the
/// on-disk credential cache (`~/.cognee/cloud_credentials.json`) is deleted
/// so the next `cogneeServe()` must re-authenticate.
pub fn cognee_disconnect(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let opts = read_opts(&mut cx, 0)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = cloud::run_disconnect(opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(_) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
