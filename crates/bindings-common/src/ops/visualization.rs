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
//! `{"destinationPath?": "<path>", "dataset?": "<name or id>"}`; unknown keys
//! are ignored. `dataset` defaults to the `default_dataset_name` setting
//! (Python's `visualize_graph(dataset=DEFAULT_DATASET_NAME)`): with per-dataset
//! storage (`ENABLE_BACKEND_ACCESS_CONTROL`) the render is that dataset's own
//! graph and the dataset must exist; over the shared store every dataset's
//! graph is rendered, as before.

use crate::{HandleState, SdkError};

/// The dataset a render runs in: `opts.dataset` (an id or a name, of one of
/// the handle owner's datasets in its tenant), else `default_dataset_name`.
/// `None` when the name resolves to nothing — an error under per-dataset
/// storage, where there is no store outside a dataset.
#[cfg(feature = "visualization")]
async fn render_dataset(
    state: &HandleState,
    svc: &crate::services::CogneeServices,
    opts: Option<&serde_json::Value>,
) -> Result<Option<uuid::Uuid>, SdkError> {
    let dataset = match opts.and_then(|v| v.get("dataset")).and_then(|v| v.as_str()) {
        Some(dataset) => dataset.to_string(),
        None => svc
            .component_manager
            .settings()
            .effective_default_dataset_name()
            .to_string(),
    };
    // An id is the handle owner's like a name is: its dataset must be theirs
    // and in their tenant, or it is "not found" — never another user's graph.
    let owner_id = state.owner_id().await?;
    let tenant_id = state.tenant_id();
    let lookup_error = |e: cognee::database::DatabaseError| {
        SdkError::Runtime(format!("failed to resolve dataset '{dataset}': {e}"))
    };
    let dataset_id = match uuid::Uuid::parse_str(&dataset) {
        Ok(id) => cognee::database::IngestDb::get_dataset(svc.database.as_ref(), id)
            .await
            .map_err(lookup_error)?
            .filter(|ds| ds.owner_id == owner_id && ds.tenant_id == tenant_id)
            .map(|ds| ds.id),
        Err(_) => cognee::database::IngestDb::get_dataset_by_name_in_tenant(
            svc.database.as_ref(),
            &dataset,
            owner_id,
            tenant_id,
        )
        .await
        .map_err(lookup_error)?
        .map(|ds| ds.id),
    };
    let per_dataset = svc
        .component_manager
        .dataset_storage()
        .await
        .map_err(|e| SdkError::Runtime(format!("dataset storage: {e}")))?
        .is_some();
    if per_dataset && dataset_id.is_none() {
        return Err(SdkError::Validation(format!(
            "dataset '{dataset}' not found"
        )));
    }
    Ok(dataset_id)
}

/// Render the knowledge graph as a self-contained HTML document.
///
/// Returns the full HTML string on success. `_opts` may name the `dataset`
/// (see the module docs).
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

        use cognee::models::dataset_scope::maybe_with_dataset;
        use cognee::visualization::render;

        let svc = state.services().await?;
        let dataset_id = render_dataset(state, &svc, _opts).await?;
        let graph_db = Arc::clone(&svc.graph_db);
        let html = maybe_with_dataset(dataset_id, render(&*graph_db))
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

        use cognee::models::dataset_scope::maybe_with_dataset;
        use cognee::visualize;

        let dest: Option<PathBuf> = opts
            .and_then(|v| v.get("destinationPath"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let svc = state.services().await?;
        let dataset_id = render_dataset(state, &svc, opts).await?;
        let graph_db = Arc::clone(&svc.graph_db);
        let path = maybe_with_dataset(dataset_id, visualize(&*graph_db, dest.as_deref()))
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
