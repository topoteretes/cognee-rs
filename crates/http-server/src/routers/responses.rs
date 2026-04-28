//! `/api/v1/responses` — OpenAI-compatible Responses API.
//!
//! **Stage A stub**: route registered, DTOs validated, always returns `501`
//! with the documented body.  No OpenAI client code is shipped in this phase.
//!
//! Stage B will replace the 501 with a real upstream OpenAI call + function-call
//! dispatcher.  See `docs/http-server/routers/responses.md §2.1.3`.

use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};

use crate::auth::AuthenticatedUser;
use crate::dto::responses::ResponseRequestDTO;
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(create_response))
}

// ─── POST / — Stage A 501 stub ────────────────────────────────────────────────

/// `POST /api/v1/responses` — create a response (OpenAI-compatible).
///
/// **Stage A**: auth and request-body validation are fully enforced.
/// Authenticated, well-formed requests receive `501` with a stable `code`
/// field that downstream clients can match on.
///
/// Stage B will dispatch to the OpenAI Responses API and fold function-call
/// results into `ResponseBodyDTO`.
#[utoipa::path(
    post,
    path = "/api/v1/responses",
    tag = "responses",
    operation_id = "create_response",
    request_body = ResponseRequestDTO,
    responses(
        (status = 200, description = "response (Stage B only)"),
        (status = 400, description = "validation error"),
        (status = 401, description = "unauthorized"),
        (status = 501, description = "not implemented (Stage A stub)"),
    ),
    extensions(("x-cognee-stub" = json!(true)))
)]
#[tracing::instrument(
    name = "cognee.api.responses.create",
    skip(_state, _req),
    fields(cognee.user.id = %user.id)
)]
async fn create_response(
    user: AuthenticatedUser,
    State(_state): State<AppState>,
    ValidatedJson(_req): ValidatedJson<ResponseRequestDTO>,
) -> Result<Response, ApiError> {
    Ok(ApiError::NotImplementedStub {
        code: "RESPONSES_NOT_IMPLEMENTED",
        detail: "OpenAI Responses API surface is not implemented in this build",
    }
    .into_response())
}

// ─── Inline tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    #[tokio::test]
    async fn stub_body_bytes() {
        let resp = ApiError::NotImplementedStub {
            code: "RESPONSES_NOT_IMPLEMENTED",
            detail: "OpenAI Responses API surface is not implemented in this build",
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            bytes.as_ref(),
            br#"{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}"#
        );
    }
}
