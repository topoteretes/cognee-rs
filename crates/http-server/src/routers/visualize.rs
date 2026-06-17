//! `/api/v1/visualize` — knowledge-graph HTML visualization router.
//!
//! - `GET /` renders a single-dataset visualization (HTML body).
//! - `POST /multi` aggregates multiple `(user_id, dataset_id)` pairs into one
//!   visualization. Superuser-only.
//!
//! Both endpoints emit `text/html` on success and JSON on error. Permission
//! denied / dataset-not-found / internal errors all collapse into a single
//! 409 envelope per Python parity — see
//! `docs/http-server/routers/visualize.md` §2.
//!
//! **Per-router parity quirk** — Python's broad `except Exception` swallows
//! 403/404/500 into 409. Do NOT "fix" it; cross-SDK parity tests assert this
//! behavior.

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
};

use cognee_database::{AclDb, IngestDb};

use crate::auth::{AuthenticatedUser, SuperuserOnly};
use crate::dto::visualize::{UserDatasetPairDTO, VisualizeQueryDTO};
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

/// Build the `/api/v1/visualize` sub-router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_visualize))
        .route("/multi", post(post_visualize_multi))
}

// ─── GET /api/v1/visualize ────────────────────────────────────────────────────

/// `GET /api/v1/visualize?dataset_id=<uuid>` — render a single-dataset HTML.
///
/// Permission denied, dataset not found, graph DB read errors, render
/// failures — all collapse into a 409 with the `{error}` envelope. Python
/// parity quirk; see module docs.
#[utoipa::path(
    get,
    path = "/api/v1/visualize",
    tag = "visualize",
    params(("dataset_id" = uuid::Uuid, Query, description = "Target dataset")),
    responses(
        (status = 200, description = "HTML visualization", content_type = "text/html"),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "catch-all"),
        (status = 422, description = "missing or malformed dataset_id"),
    )
)]
#[tracing::instrument(name = "cognee.api.visualize", skip(state), fields(cognee.dataset.id = %query.dataset_id))]
pub async fn get_visualize(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<VisualizeQueryDTO>,
) -> Result<Html<String>, ApiError> {
    crate::telemetry::emit(
        "Visualize API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "GET /v1/visualize",
            "dataset_id": query.dataset_id.to_string(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::VisualizeError(StatusCode::CONFLICT, "components not wired".into())
    })?;

    // Resolve and authorize. Permission denied collapses into 409 — see module
    // docs. Do NOT return 403 here.
    let db = components.database.clone();
    let dataset = IngestDb::get_dataset(db.as_ref(), query.dataset_id)
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?
        .ok_or_else(|| {
            ApiError::VisualizeError(
                StatusCode::CONFLICT,
                format!("dataset {} not found", query.dataset_id),
            )
        })?;
    if !AclDb::has_permission(db.as_ref(), user.id, dataset.id, "read")
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?
    {
        return Err(ApiError::VisualizeError(
            StatusCode::CONFLICT,
            "permission denied".to_string(),
        ));
    }

    let Some(graph_db) = components.graph_db.clone() else {
        return Err(ApiError::VisualizeError(
            StatusCode::CONFLICT,
            "graph database is not wired".to_string(),
        ));
    };

    let html = cognee_visualization::render(graph_db.as_ref())
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?;
    Ok(Html(html))
}

// ─── POST /api/v1/visualize/multi ─────────────────────────────────────────────

/// `POST /api/v1/visualize/multi` — render a combined multi-user visualization.
///
/// Superuser-only. The 403 envelope is emitted by the `SuperuserOnly`
/// extractor and uses `{error}`, NOT `{detail}`.
#[utoipa::path(
    post,
    path = "/api/v1/visualize/multi",
    tag = "visualize",
    request_body = Vec<UserDatasetPairDTO>,
    responses(
        (status = 200, description = "HTML visualization", content_type = "text/html"),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "superuser required"),
        (status = 409, description = "catch-all"),
    )
)]
#[tracing::instrument(name = "cognee.api.visualize.multi", skip(state, pairs))]
pub async fn post_visualize_multi(
    SuperuserOnly(user): SuperuserOnly,
    State(state): State<AppState>,
    ValidatedJson(pairs): ValidatedJson<Vec<UserDatasetPairDTO>>,
) -> Result<Html<String>, ApiError> {
    crate::telemetry::emit(
        "Visualize Multi API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/visualize/multi",
            "pair_count": pairs.len(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::VisualizeError(StatusCode::CONFLICT, "components not wired".into())
    })?;

    // Per Python parity, permission is resolved against the *target* user, not
    // the caller — so the superuser does not implicitly elevate access.
    let db = components.database.clone();
    let mut user_pairs: Vec<(String, std::sync::Arc<dyn cognee_graph::GraphDBTrait>)> = Vec::new();
    for pair in &pairs {
        let dataset = IngestDb::get_dataset(db.as_ref(), pair.dataset_id)
            .await
            .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?
            .ok_or_else(|| {
                ApiError::VisualizeError(
                    StatusCode::CONFLICT,
                    format!("dataset {} not found", pair.dataset_id),
                )
            })?;
        let allowed = AclDb::has_permission(db.as_ref(), pair.user_id, dataset.id, "read")
            .await
            .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?;
        if !allowed {
            return Err(ApiError::VisualizeError(
                StatusCode::CONFLICT,
                "permission denied".to_string(),
            ));
        }
        let Some(graph_db) = components.graph_db.clone() else {
            return Err(ApiError::VisualizeError(
                StatusCode::CONFLICT,
                "graph database is not wired".to_string(),
            ));
        };

        // Resolve the target user to a human-readable label so the
        // `userColors` palette key matches Python's
        // `getattr(user, "email", None) or str(user.id)` at
        // `cognee_network_visualization.py:138`. Lookup failure (or a missing
        // auth context) collapses into the existing 409 catch-all.
        let user_label = if let Some(auth) = state.auth.as_ref() {
            match auth
                .user_repo
                .find_by_id(pair.user_id)
                .await
                .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?
            {
                Some(u) if !u.email.is_empty() => u.email,
                _ => pair.user_id.to_string(),
            }
        } else {
            pair.user_id.to_string()
        };

        user_pairs.push((user_label, graph_db));
    }

    let html = cognee_visualization::render_multi_user(&user_pairs)
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?;
    Ok(Html(html))
}
