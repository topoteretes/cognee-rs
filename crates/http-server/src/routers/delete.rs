//! `DELETE /api/v1/delete` — deprecated single-data delete alias.
//!
//! Python parity: `cognee/api/v1/delete/routers/get_delete_router.py`.
//! Rust delegation: `cognee_delete::DeleteService` (via `state.components()`).
//!
//! This router is kept for backwards compatibility with clients pinned to
//! cognee ≤ 0.3.8. New clients should use
//! `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` instead.

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    routing::delete,
};
use cognee_database::IngestDb;
use cognee_delete::{DeleteMode as SvcDeleteMode, DeleteRequest, DeleteScope};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::delete::{DeleteMode, DeleteQuery, DeleteSuccessResponseDTO};
use crate::permissions::check_permission;
use crate::state::AppState;

// ─── Deprecation headers ──────────────────────────────────────────────────────

/// RFC 8594 deprecation headers sent on every response (success and error).
///
/// `Deprecation: true`, `Sunset: <date>`, `Link: <successor>`.
fn deprecation_headers(dataset_id: Uuid, data_id: Uuid) -> HeaderMap {
    let sunset = std::env::var("COGNEE_DEPRECATED_SUNSET_DELETE")
        .unwrap_or_else(|_| "2026-12-01".to_owned());

    let mut map = HeaderMap::new();
    // Infallible parses: all values are ASCII literals or formatted UUIDs.
    map.insert("Deprecation", HeaderValue::from_static("true"));
    if let Ok(v) = HeaderValue::from_str(&sunset) {
        map.insert("Sunset", v);
    }
    let link_val =
        format!("</api/v1/datasets/{dataset_id}/data/{data_id}>; rel=\"successor-version\"");
    if let Ok(v) = HeaderValue::from_str(&link_val) {
        map.insert("Link", v);
    }
    map
}

// ─── delete_data_deprecated ───────────────────────────────────────────────────

/// `DELETE /api/v1/delete` — Deprecated single-data delete.
///
/// Identical in effect to `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}`.
/// Adds `Deprecation`, `Sunset`, and `Link` headers on every response.
pub async fn delete_data_deprecated(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<DeleteQuery>,
) -> axum::response::Response {
    tracing::warn!(
        target: "deprecated",
        user_id = %user.id,
        "Deprecated route DELETE /v1/delete invoked"
    );

    let headers = deprecation_headers(query.dataset_id, query.data_id);

    // Resolve components — errors treated as 409 (Python parity catch-all).
    let components = match state.components() {
        Some(c) => c,
        None => {
            return build_conflict_response("components not initialized", headers);
        }
    };

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    // TODO(P5): wire full PermissionsRepository once tenants_rbac migration lands
    if let Err(e) = check_permission(&db, user.id, query.dataset_id, "delete").await {
        return build_conflict_response(&e.to_string(), headers);
    }

    // Look up dataset name.
    let dataset_name = match db.get_dataset(query.dataset_id).await {
        Ok(Some(ds)) => Some(ds.name),
        Ok(None) => None,
        Err(e) => {
            return build_conflict_response(&e.to_string(), headers);
        }
    };

    // Map DTO DeleteMode → service DeleteMode.
    let svc_mode = match query.mode {
        DeleteMode::Soft => SvcDeleteMode::Soft,
        DeleteMode::Hard => SvcDeleteMode::Hard,
    };

    let request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id: user.id,
            data_id: query.data_id,
            dataset_name,
            delete_dataset_if_empty: query.delete_dataset_if_empty,
        },
        mode: svc_mode,
    };

    match delete_service.execute(&request).await {
        Ok(_) => build_success_response(headers),
        Err(e) => build_conflict_response(&e.to_string(), headers),
    }
}

// ─── response builders ───────────────────────────────────────────────────────

fn build_success_response(headers: HeaderMap) -> axum::response::Response {
    let body = serde_json::to_string(&DeleteSuccessResponseDTO::ok())
        .unwrap_or_else(|_| r#"{"status":"success"}"#.to_owned());

    let mut builder = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json");
    for (key, val) in &headers {
        builder = builder.header(key, val);
    }
    builder
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| {
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .expect("static response cannot fail")
        })
}

fn build_conflict_response(error: &str, headers: HeaderMap) -> axum::response::Response {
    let body = serde_json::to_string(&json!({"error": error}))
        .unwrap_or_else(|_| r#"{"error":"internal error"}"#.to_owned());

    let mut builder = axum::response::Response::builder()
        .status(StatusCode::CONFLICT)
        .header("Content-Type", "application/json");
    for (key, val) in &headers {
        builder = builder.header(key, val);
    }
    builder
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| {
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .expect("static response cannot fail")
        })
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", delete(delete_data_deprecated))
}
