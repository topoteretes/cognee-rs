//! Phase 6 — visualization ops: `cogneeVisualize` / `cogneeVisualizeToFile`.
//!
//! Both functions are gated behind `#[cfg(feature = "visualization")]`.
//! When the feature is absent, the exported functions throw a typed JS error
//! with `code = "FEATURE_NOT_BUILT"` so the caller can detect the missing
//! feature rather than receiving a cryptic "not a function".
//!
//! ## Function shapes
//!
//! - `cogneeVisualize(handle, opts?) -> Promise<string>` — calls
//!   `cognee::visualization::render(&*graph_db)` and returns the HTML
//!   as a string.  No disk I/O in the binding layer; callers can stream,
//!   embed, or persist the HTML as they see fit.
//! - `cogneeVisualizeToFile(handle, opts?) -> Promise<string>` — calls
//!   `cognee::visualize(&*graph_db, destination_path)` and returns the
//!   absolute path of the written file as a string.  `opts.destinationPath`
//!   is optional; when absent the default output path
//!   (`~/graph_visualization.html`) is used.

use neon::prelude::*;

use crate::errors::{SdkError, throw_sdk_error};

// These are only used in the feature-enabled paths.
#[cfg(feature = "visualization")]
use crate::json::read_opts;
#[cfg(feature = "visualization")]
use crate::runtime::runtime;
#[cfg(feature = "visualization")]
use crate::sdk::CogneeHandle;
#[cfg(feature = "visualization")]
use cognee_bindings_common::ops::visualization;
#[cfg(feature = "visualization")]
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Exported native functions (always registered in lib.rs; body is cfg-gated).
// ---------------------------------------------------------------------------

/// `cogneeVisualize(handle, opts?) -> Promise<string>`
///
/// Returns the d3.js force-directed HTML visualization of the current
/// knowledge graph as a string.  No files are written by this binding.
pub fn cognee_visualize(mut cx: FunctionContext) -> JsResult<JsPromise> {
    #[cfg(feature = "visualization")]
    {
        let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
        let state = Arc::clone(&handle.state);

        let opts = read_opts(&mut cx, 1)?;

        let channel = cx.channel();
        let (deferred, promise) = cx.promise();

        runtime().spawn(async move {
            let result = visualization::visualize(&state, Some(&opts)).await;
            deferred.settle_with(&channel, move |mut cx| match result {
                Ok(html) => Ok(cx.string(html).as_value(&mut cx)),
                Err(e) => throw_sdk_error(&mut cx, e),
            });
        });

        Ok(promise)
    }

    #[cfg(not(feature = "visualization"))]
    {
        let _ = cx; // suppress unused warning
        let (deferred, promise) = cx.promise();
        let channel = cx.channel();
        deferred.settle_with(&channel, |mut cx| -> JsResult<JsValue> {
            throw_sdk_error(
                &mut cx,
                SdkError::FeatureNotBuilt(
                    "visualization feature not compiled in this build of cognee-ts-neon"
                        .to_string(),
                ),
            )
        });
        Ok(promise)
    }
}

/// `cogneeVisualizeToFile(handle, opts?) -> Promise<string>`
///
/// Writes the d3.js HTML visualization to disk and returns the absolute path
/// of the file.  `opts.destinationPath` (optional) overrides the default
/// `~/graph_visualization.html` destination.
pub fn cognee_visualize_to_file(mut cx: FunctionContext) -> JsResult<JsPromise> {
    #[cfg(feature = "visualization")]
    {
        let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
        let state = Arc::clone(&handle.state);

        let opts = read_opts(&mut cx, 1)?;

        let channel = cx.channel();
        let (deferred, promise) = cx.promise();

        runtime().spawn(async move {
            let result = visualization::visualize_to_file(&state, Some(&opts)).await;
            deferred.settle_with(&channel, move |mut cx| match result {
                Ok(path_str) => Ok(cx.string(&path_str).as_value(&mut cx)),
                Err(e) => throw_sdk_error(&mut cx, e),
            });
        });

        Ok(promise)
    }

    #[cfg(not(feature = "visualization"))]
    {
        let _ = cx;
        let (deferred, promise) = cx.promise();
        let channel = cx.channel();
        deferred.settle_with(&channel, |mut cx| -> JsResult<JsValue> {
            throw_sdk_error(
                &mut cx,
                SdkError::FeatureNotBuilt(
                    "visualization feature not compiled in this build of cognee-ts-neon"
                        .to_string(),
                ),
            )
        });
        Ok(promise)
    }
}
