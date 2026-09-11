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

use cognee_database::IngestDb;

use crate::auth::AuthenticatedUser;
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
    // No ACL backend wired (pure-OSS) → allow.
    require_read_permission(components.acl_db.as_deref(), user.id, dataset.id).await?;

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
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(pairs): ValidatedJson<Vec<UserDatasetPairDTO>>,
) -> Result<Html<String>, ApiError> {
    if !user.is_superuser {
        // Python parity: superuser gate is a 403 with the
        // `VisualizeError` envelope, not the canonical 403 detail body.
        return Err(ApiError::VisualizeError(
            StatusCode::FORBIDDEN,
            "Superuser privileges required for multi-user visualization".to_string(),
        ));
    }
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
        // OSS does not bundle an ACL backend — when no `acl_db` is wired
        // (the pure-OSS case), allow the read. Closed embedders install
        // a real `AclDb` impl via `ComponentHandles::acl_db`.
        require_read_permission(components.acl_db.as_deref(), pair.user_id, dataset.id).await?;
        let Some(graph_db) = components.graph_db.clone() else {
            return Err(ApiError::VisualizeError(
                StatusCode::CONFLICT,
                "graph database is not wired".to_string(),
            ));
        };

        // The closed-side `users` table moved out of OSS, so
        // OSS falls back to the user id as the palette key. Closed
        // embedders that want the email-keyed palette wrap this router
        // and substitute their own email lookup.
        let user_label = pair.user_id.to_string();

        user_pairs.push((user_label, graph_db));
    }

    let html = cognee_visualization::render_multi_user(&user_pairs)
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?;
    Ok(Html(html))
}

/// Deny the visualization unless `user_id` can `read` `dataset_id`.
///
/// `acl` is `None` when no ACL backend is wired (pure-OSS) — the read is
/// allowed. Otherwise the roles-aware check is used, matching Python's
/// `get_authorized_existing_datasets([dataset_id], "read", user)`, which
/// resolves tenant and role grants alongside direct ones. Any failure —
/// backend error or denial — collapses into the 409 envelope per the
/// module-level parity note.
async fn require_read_permission(
    acl: Option<&dyn cognee_database::AclDb>,
    user_id: uuid::Uuid,
    dataset_id: uuid::Uuid,
) -> Result<(), ApiError> {
    let Some(acl) = acl else {
        return Ok(());
    };
    let allowed = acl
        .has_permission_with_roles(user_id, dataset_id, "read")
        .await
        .map_err(|err| ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string()))?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::VisualizeError(
            StatusCode::CONFLICT,
            "permission denied".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::AclDb;
    use cognee_test_utils::MockAclDb;
    use uuid::Uuid;

    /// Scenario: the caller can read the dataset only through a tenant grant.
    /// Expected: allowed — Python's visualize path resolves tenant/role grants.
    #[tokio::test]
    async fn read_grant_via_tenant_is_admitted() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        acl.add_user_to_tenant(user, tenant);
        assert!(acl.grant_permission(tenant, dataset, "read").await.is_ok());

        assert!(
            require_read_permission(Some(&acl), user, dataset)
                .await
                .is_ok(),
            "a tenant-held read grant must satisfy the visualize gate"
        );
    }

    /// Scenario: direct `read` grant. Expected: allowed (unchanged).
    #[tokio::test]
    async fn direct_read_grant_is_admitted() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        assert!(acl.grant_permission(user, dataset, "read").await.is_ok());

        assert!(
            require_read_permission(Some(&acl), user, dataset)
                .await
                .is_ok()
        );
    }

    /// Scenario: no grant at all. Expected: the 409 parity envelope, not 403.
    #[tokio::test]
    async fn missing_read_grant_is_a_409() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();

        let err = require_read_permission(Some(&acl), user, dataset).await;
        assert!(
            matches!(err, Err(ApiError::VisualizeError(StatusCode::CONFLICT, _))),
            "got {err:?}"
        );
    }

    /// Scenario: no ACL backend wired (pure-OSS). Expected: allowed.
    #[tokio::test]
    async fn no_acl_backend_allows() {
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        assert!(require_read_permission(None, user, dataset).await.is_ok());
    }
}
