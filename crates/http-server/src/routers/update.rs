//! `PATCH /api/v1/update` — replace an existing document and re-cognify.
//!
//! Python parity: `cognee/api/v1/update/routers/get_update_router.py`.
//! Rust delegation: re-implemented inline using cognee-ingestion + cognee-delete.

use axum::{
    Router,
    extract::{Multipart, Query, State},
    http::StatusCode,
    routing::patch,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::update::UpdateQuery;
use crate::error::ApiError;
use crate::multipart::{MultipartOpts, UploadGuard, check_filename_traversal, parse_multipart};
use crate::state::AppState;

// ─── patch_update handler ─────────────────────────────────────────────────────

/// `PATCH /api/v1/update` — Replace an existing document and re-cognify the dataset.
///
/// # Note
/// Full re-cognify is not implemented yet (requires cognify pipeline components).
/// This stub accepts the multipart, validates it, then returns 501.
pub async fn patch_update(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<UpdateQuery>,
    multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let opts = MultipartOpts::default();
    let parsed = parse_multipart(multipart, &opts, &request_id).await?;
    let _guard = UploadGuard::new(parsed.spool_dir.clone());

    // Validate uploaded files for traversal.
    if let Some(files) = parsed.files.get("data") {
        for f in files {
            if let Some(ref name) = f.filename {
                check_filename_traversal(name)?;
            }
        }
    }

    // The full update pipeline (delete + re-add + cognify) is not yet ported
    // (TODO(update)). Once it lands, the gate will be:
    //   check_permission_via_handles(components, user.id, dataset_id, "write").await?;
    // For now this endpoint short-circuits with 501.
    let _ = (user, state, query);

    let body = json!({
        "error": "Update endpoint not fully implemented yet.",
        "detail": "Re-cognify pipeline (P3) is required for full update support."
    });
    let resp = axum::response::Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_string(&body).expect("static json"),
        ))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;
    Ok(resp)
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", patch(patch_update))
}
