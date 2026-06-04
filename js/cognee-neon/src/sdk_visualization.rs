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
//!   `cognee_lib::visualization::render(&*graph_db)` and returns the HTML
//!   as a string.  No disk I/O in the binding layer; callers can stream,
//!   embed, or persist the HTML as they see fit.
//! - `cogneeVisualizeToFile(handle, opts?) -> Promise<string>` — calls
//!   `cognee_lib::visualize(&*graph_db, destination_path)` and returns the
//!   absolute path of the written file as a string.  `opts.destinationPath`
//!   is optional; when absent the default output path
//!   (`~/graph_visualization.html`) is used.

use neon::prelude::*;

use crate::errors::{SdkError, throw_sdk_error};

// These are only used in the feature-enabled paths.
#[cfg(feature = "visualization")]
use std::sync::Arc;
#[cfg(feature = "visualization")]
use crate::json::read_opts;
#[cfg(feature = "visualization")]
use crate::runtime::runtime;
#[cfg(feature = "visualization")]
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// Feature-gated implementations.
// ---------------------------------------------------------------------------

#[cfg(feature = "visualization")]
mod inner {
    use std::path::PathBuf;
    use std::sync::Arc;

    use cognee_lib::visualization::render;
    use cognee_lib::visualize;

    use super::*;

    /// Run `render()` and return the HTML string.
    pub(super) async fn run_visualize(
        state: &crate::sdk::HandleState,
        _opts: serde_json::Value,
    ) -> Result<String, SdkError> {
        let svc = state.services().await?;
        let graph_db = Arc::clone(&svc.graph_db);
        let html = render(&*graph_db)
            .await
            .map_err(|e| SdkError::Runtime(format!("visualization render failed: {e}")))?;
        Ok(html)
    }

    /// Run `visualize()` and return the written path as a string.
    pub(super) async fn run_visualize_to_file(
        state: &crate::sdk::HandleState,
        opts: serde_json::Value,
    ) -> Result<String, SdkError> {
        let dest: Option<PathBuf> = opts
            .get("destinationPath")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let svc = state.services().await?;
        let graph_db = Arc::clone(&svc.graph_db);
        let path = visualize(&*graph_db, dest.as_deref())
            .await
            .map_err(|e| SdkError::Runtime(format!("visualize to file failed: {e}")))?;
        path.to_str()
            .map(|s| s.to_string())
            .ok_or_else(|| SdkError::Runtime("visualization path is not valid UTF-8".to_string()))
    }
}

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
            let result = inner::run_visualize(&state, opts).await;
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
                    "visualization feature not compiled in this build of cognee-neon".to_string(),
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
            let result = inner::run_visualize_to_file(&state, opts).await;
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
                    "visualization feature not compiled in this build of cognee-neon".to_string(),
                ),
            )
        });
        Ok(promise)
    }
}

