//! Shared async visualization operations: `visualize`, `visualize_to_file`.
//!
//! These functions contain the pure-Rust async logic that is shared between
//! every language binding surface (C API, Neon JS, Python). Each function takes
//! a [`HandleState`] reference and `serde_json::Value` arguments, performs the
//! operation against the underlying cognee APIs, and returns a result (or
//! an [`SdkError`]).
//!
//! Both functions are gated behind `#[cfg(feature = "visualization")]`. When
//! the feature is absent the functions return [`SdkError::FeatureNotBuilt`],
//! which each binding converts to its native feature-not-built error type.
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## opts shape
//!
//! `{"destinationPath?": "<path>"}` — only `destinationPath` is parsed;
//! unknown keys are ignored.

use crate::{HandleState, SdkError};

/// Render the knowledge graph as a self-contained HTML document.
///
/// Returns the full HTML string on success. `_opts` is accepted for API
/// symmetry but no keys are currently consumed.
///
/// When the `visualization` feature is not compiled in, returns
/// [`SdkError::FeatureNotBuilt`].
pub async fn visualize(
    state: &HandleState,
    _opts: Option<&serde_json::Value>,
) -> Result<String, SdkError> {
    #[cfg(feature = "visualization")]
    {
        use std::sync::Arc;

        use cognee::visualization::render;

        let svc = state.services().await?;
        let graph_db = Arc::clone(&svc.graph_db);
        let html = render(&*graph_db)
            .await
            .map_err(|e| SdkError::Runtime(format!("visualization render failed: {e}")))?;
        Ok(html)
    }

    #[cfg(not(feature = "visualization"))]
    {
        let _ = (state, _opts);
        Err(SdkError::FeatureNotBuilt(
            "visualization feature not compiled in this build".to_string(),
        ))
    }
}

/// Render the knowledge graph to a file and return the written path.
///
/// `opts` may contain `"destinationPath"` (string) to override the default
/// `~/graph_visualization.html` output location.
///
/// When the `visualization` feature is not compiled in, returns
/// [`SdkError::FeatureNotBuilt`].
pub async fn visualize_to_file(
    state: &HandleState,
    opts: Option<&serde_json::Value>,
) -> Result<String, SdkError> {
    #[cfg(feature = "visualization")]
    {
        use std::path::PathBuf;
        use std::sync::Arc;

        use cognee::visualize;

        let dest: Option<PathBuf> = opts
            .and_then(|v| v.get("destinationPath"))
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

    #[cfg(not(feature = "visualization"))]
    {
        let _ = (state, opts);
        Err(SdkError::FeatureNotBuilt(
            "visualization feature not compiled in this build".to_string(),
        ))
    }
}
