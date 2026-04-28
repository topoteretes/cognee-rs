//! `POST /api/v1/checks/connection` — validate a cloud API key.
//!
//! See [`docs/http-server/routers/checks.md`](../../../../docs/http-server/routers/checks.md).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::auth::AuthenticatedUser;
use crate::dto::checks::CloudConfigErrorDTO;
use crate::state::AppState;

// ─── Mount ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/connection", post(post_connection))
}

// ─── POST /connection ────────────────────────────────────────────────────────

/// `POST /api/v1/checks/connection` — proxy the caller's `X-Api-Key` header
/// to the cloud control-plane and surface the result.
///
/// Skip the headers in the recorded span attributes — they carry the secret;
/// the redaction layer is the backstop, but we don't auto-record the
/// `HeaderMap` argument either way.
#[tracing::instrument(name = "cognee.api.checks.connection", skip(_state, _user, headers))]
pub async fn post_connection(
    State(_state): State<AppState>,
    _user: AuthenticatedUser,
    headers: HeaderMap,
) -> Response {
    let api_key = headers
        .get("X-Api-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if api_key.is_empty() {
        let body = CloudConfigErrorDTO {
            detail:
                "Failed to connect to the cloud service. Please add your API key to local instance."
                    .into(),
            name: "CloudApiKeyMissingError".into(),
        };
        return (StatusCode::BAD_REQUEST, Json(body)).into_response();
    }

    let cloud_url = cognee_cloud::config::cloud_url();
    tracing::Span::current().record("cognee.cloud.url", cloud_url.as_str());

    match cognee_cloud::check_api_key(&api_key).await {
        Ok(()) => {
            tracing::Span::current().record("cognee.cloud.status", 200_i64);
            (StatusCode::OK, Json(serde_json::Value::Null)).into_response()
        }
        Err(e) => {
            tracing::Span::current().record("cognee.cloud.status", upstream_status(&e) as i64);
            let detail = format!("Failed to connect to cloud instance: {e}");
            let body = CloudConfigErrorDTO {
                detail,
                // sic — Python typo replicated for wire parity (three "n"s).
                name: "CloudConnnectionError".into(),
            };
            (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
        }
    }
}

fn upstream_status(err: &cognee_cloud::CloudError) -> u16 {
    match err {
        cognee_cloud::CloudError::ManagementApi { status, .. } => *status,
        _ => 0,
    }
}
