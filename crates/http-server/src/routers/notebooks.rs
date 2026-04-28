//! `/api/v1/notebooks` — per-user notebook CRUD + Stage-A `/run` stub.
//!
//! All endpoints require authentication. The 404 envelope deviates from the
//! global `{"detail": "..."}` shape — Python uses `{"error": "Notebook not found"}`
//! for this router specifically.  See `docs/http-server/routers/notebooks.md §3`.
//!
//! Stage A deliverable: CRUD + stubbed `/run` (501 with documented body).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde_json::json;
use uuid::Uuid;

use cognee_database::{NotebookDb, seed_tutorials_if_first_call};

use crate::auth::AuthenticatedUser;
use crate::dto::notebooks::{NotebookDTO, NotebookDataDTO, RunCodeDataDTO};
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_notebooks))
        .route("/", post(create_notebook))
        .route("/{notebook_id}", put(update_notebook))
        .route("/{notebook_id}", delete(delete_notebook))
        .route("/{notebook_id}/{cell_id}/run", post(run_notebook_cell))
}

// ─── 404 helper ───────────────────────────────────────────────────────────────

/// Returns `404 {"error": "Notebook not found"}`.
///
/// This router uses the `"error"` key per Python parity — NOT the global
/// `{"detail": "..."}` shape.  Do NOT route through `ApiError`.
fn notebook_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "Notebook not found"})),
    )
        .into_response()
}

// ─── GET / — list notebooks ───────────────────────────────────────────────────

/// `GET /api/v1/notebooks` — list all notebooks for the authenticated user.
///
/// On first call for a fresh user, seeds the two tutorial notebooks with
/// deterministic UUID5 ids and `deletable=false`.
#[utoipa::path(
    get,
    path = "/api/v1/notebooks",
    tag = "notebooks",
    operation_id = "list_notebooks",
    responses(
        (status = 200, description = "list of notebooks", body = Vec<NotebookDTO>),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "internal server error"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.notebooks.list",
    skip(state),
    fields(cognee.user.id = %user.id)
)]
async fn list_notebooks(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<NotebookDTO>>, ApiError> {
    let db = notebooks_db(&state)?;
    seed_tutorials_if_first_call(db.as_ref(), user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("tutorial seed failed: {e}")))?;
    let notebooks = db
        .list_by_owner(user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("Failed to list notebooks: {e}")))?;
    Ok(Json(
        notebooks.into_iter().map(NotebookDTO::from_db).collect(),
    ))
}

// ─── POST / — create notebook ─────────────────────────────────────────────────

/// `POST /api/v1/notebooks` — create a new notebook.
///
/// `name` is required (Python's `Field(...)` semantics — a missing name is a 400).
/// `deletable` is always `true` regardless of the request value (Python truthiness bug).
#[utoipa::path(
    post,
    path = "/api/v1/notebooks",
    tag = "notebooks",
    operation_id = "create_notebook",
    request_body = NotebookDataDTO,
    responses(
        (status = 200, description = "created notebook", body = NotebookDTO),
        (status = 400, description = "validation error"),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "internal server error"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.notebooks.create",
    skip(state, body),
    fields(cognee.user.id = %user.id)
)]
async fn create_notebook(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(body): ValidatedJson<NotebookDataDTO>,
) -> Result<Json<NotebookDTO>, ApiError> {
    let name = body
        .name
        .ok_or_else(|| ApiError::BadRequest("name is required".to_owned()))?;

    let db = notebooks_db(&state)?;
    let cells = serde_json::to_value(&body.cells)
        .map_err(|e| ApiError::BadRequest(format!("invalid cells: {e}")))?;

    // Always deletable=true — Python's `deletable or True` truthiness bug.
    let nb = db
        .create(user.id, name, cells, true)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("Failed to create notebook: {e}")))?;

    tracing::Span::current().record("cognee.notebook.id", nb.id.to_string());
    tracing::Span::current().record(
        "cognee.notebook.cell_count",
        nb.cells.as_array().map(|a| a.len()).unwrap_or(0),
    );

    Ok(Json(NotebookDTO::from_db(nb)))
}

// ─── PUT /{notebook_id} — update notebook ────────────────────────────────────

/// `PUT /api/v1/notebooks/{notebook_id}` — update a notebook's name and/or cells.
///
/// Empty cells list does NOT clear cells (Python truthiness bug — replicated).
#[utoipa::path(
    put,
    path = "/api/v1/notebooks/{notebook_id}",
    tag = "notebooks",
    operation_id = "update_notebook",
    params(("notebook_id" = Uuid, Path, description = "notebook id")),
    request_body = NotebookDataDTO,
    responses(
        (status = 200, description = "updated notebook", body = NotebookDTO),
        (status = 400, description = "validation error"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "not found"),
        (status = 500, description = "internal server error"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.notebooks.update",
    skip(state, body),
    fields(cognee.user.id = %user.id, cognee.notebook.id = %notebook_id)
)]
async fn update_notebook(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(notebook_id): Path<Uuid>,
    ValidatedJson(body): ValidatedJson<NotebookDataDTO>,
) -> Result<Response, ApiError> {
    let db = notebooks_db(&state)?;

    // Fetch existing row to check ownership.
    let existing = db
        .get_by_id_and_owner(notebook_id, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let Some(existing) = existing else {
        return Ok(notebook_not_found());
    };

    // Build patch with Python truthiness semantics:
    // - name: only update when Some and non-empty and different from existing.
    // - cells: only update when non-empty list.
    let new_name = body.name.filter(|n| !n.is_empty() && n != &existing.name);
    let new_cells = if body.cells.is_empty() {
        None
    } else {
        Some(
            serde_json::to_value(&body.cells)
                .map_err(|e| ApiError::BadRequest(format!("invalid cells: {e}")))?,
        )
    };

    let patch = cognee_database::NotebookUpdatePatch {
        name: new_name,
        cells: new_cells,
    };

    let updated = db
        .update(notebook_id, user.id, patch)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    match updated {
        Some(nb) => Ok(Json(NotebookDTO::from_db(nb)).into_response()),
        None => Ok(notebook_not_found()),
    }
}

// ─── DELETE /{notebook_id} — delete notebook ─────────────────────────────────

/// `DELETE /api/v1/notebooks/{notebook_id}` — delete a notebook.
///
/// Returns `200 {}` on success (not 204) — Python parity.
#[utoipa::path(
    delete,
    path = "/api/v1/notebooks/{notebook_id}",
    tag = "notebooks",
    operation_id = "delete_notebook",
    params(("notebook_id" = Uuid, Path, description = "notebook id")),
    responses(
        (status = 200, description = "deleted"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "not found"),
        (status = 500, description = "internal server error"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.notebooks.delete",
    skip(state),
    fields(cognee.user.id = %user.id, cognee.notebook.id = %notebook_id)
)]
async fn delete_notebook(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(notebook_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let db = notebooks_db(&state)?;

    let deleted = db
        .delete(notebook_id, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if deleted {
        Ok((StatusCode::OK, Json(json!({}))).into_response())
    } else {
        Ok(notebook_not_found())
    }
}

// ─── POST /{notebook_id}/{cell_id}/run — Stage A 501 stub ────────────────────

/// `POST /api/v1/notebooks/{notebook_id}/{cell_id}/run` — execute a code cell.
///
/// **Stage A stub**: auth, path parsing, body validation, and notebook lookup
/// all run normally.  A found notebook returns `501` with the documented body.
/// A missing notebook returns `404` first (404 beats 501 in priority order).
///
/// Stage B will replace the 501 with real subprocess execution.
#[utoipa::path(
    post,
    path = "/api/v1/notebooks/{notebook_id}/{cell_id}/run",
    tag = "notebooks",
    operation_id = "run_notebook_cell",
    params(
        ("notebook_id" = Uuid, Path, description = "notebook id"),
        ("cell_id" = Uuid, Path, description = "cell id (addressing only; not validated against stored cells)"),
    ),
    request_body = RunCodeDataDTO,
    responses(
        (status = 200, description = "execution result (Stage B only)"),
        (status = 400, description = "validation error"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "notebook not found"),
        (status = 501, description = "not implemented (Stage A stub)"),
    ),
    extensions(("x-cognee-stub" = json!(true)))
)]
#[tracing::instrument(
    name = "cognee.api.notebooks.run_cell",
    skip(state, _body),
    fields(
        cognee.user.id = %user.id,
        cognee.notebook.id = %notebook_id,
        cognee.notebook.cell_id = %cell_id,
        cognee.notebook.run_outcome = "stubbed",
    )
)]
async fn run_notebook_cell(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((notebook_id, cell_id)): Path<(Uuid, Uuid)>,
    ValidatedJson(_body): ValidatedJson<RunCodeDataDTO>,
) -> Result<Response, ApiError> {
    let _ = cell_id; // addressing only — not validated against stored cells

    let db = notebooks_db(&state)?;

    let notebook = db
        .get_by_id_and_owner(notebook_id, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if notebook.is_none() {
        return Ok(notebook_not_found());
    }

    // Stage A: always 501 for authenticated, well-formed, found-notebook calls.
    Ok(ApiError::NotImplementedStub {
        code: "NOTEBOOK_RUN_NOT_IMPLEMENTED",
        detail: "Notebook cell execution is not implemented in this build",
    }
    .into_response())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Extract the notebook DB handle from `AppState`.
#[allow(clippy::result_large_err)]
fn notebooks_db(state: &AppState) -> Result<std::sync::Arc<dyn NotebookDb>, ApiError> {
    state
        .components()
        .map(|c| {
            let db: std::sync::Arc<dyn NotebookDb> = c.database.clone();
            db
        })
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("notebooks DB is not wired")))
}

// ─── Inline tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn notebook_not_found_response_shape() {
        let resp = notebook_not_found();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"], "Notebook not found");
        // Must NOT have a "detail" key — this router uses "error", not "detail".
        assert!(body.get("detail").is_none());
    }
}
