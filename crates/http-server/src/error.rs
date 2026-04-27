//! Error types for the cognee HTTP server.
//!
//! `ApiError` mirrors Python's `CogneeApiError` / `RequestValidationError` shapes
//! (see `cognee/api/client.py`). `ServerError` is used for startup/runtime failures.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

// ─── Validation details ──────────────────────────────────────────────────────

/// Carries the Python-shaped validation error payload:
/// `{"detail": [...], "body": <original_body>}`.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationDetails {
    /// List of validation errors (each entry mirrors a Pydantic error item).
    pub detail: Value,
    /// The original request body that caused the error, if available.
    pub body: Option<Value>,
}

// ─── ApiError ────────────────────────────────────────────────────────────────

/// HTTP-layer error type.  Every handler returns `Result<T, ApiError>` and the
/// `IntoResponse` impl maps variants to the Python-compatible JSON envelopes.
#[derive(Debug, Error)]
pub enum ApiError {
    /// 400 `{"detail": "..."}` — generic bad-request condition.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// 401 `{"detail": "Unauthorized"}`.
    #[error("unauthorized")]
    Unauthorized,

    /// 403 `{"detail": "..."}`.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// 404 `{"detail": "..."}`.
    #[error("not found: {0}")]
    NotFound(String),

    /// 409 `{"detail": "..."}`.
    #[error("conflict: {0}")]
    Conflict(String),

    /// 400 `{"detail": [...], "body": ...}` — serde/Pydantic-style validation.
    #[error("validation error")]
    Validation(ValidationDetails),

    /// 400 `{"detail": "LOGIN_BAD_CREDENTIALS"}` — fastapi-users compat.
    #[error("login bad credentials")]
    LoginBadCredentials,

    /// 400 `{"detail": "LOGIN_USER_NOT_VERIFIED"}` — fastapi-users compat.
    #[error("login user not verified")]
    LoginUserNotVerified,

    /// 400 `{"detail": "REGISTER_USER_ALREADY_EXISTS"}` — fastapi-users compat.
    #[error("register user already exists")]
    RegisterUserAlreadyExists,

    /// 400 `{"detail": {"code": "REGISTER_INVALID_PASSWORD", "reason": "..."}}`.
    #[error("register invalid password: {0}")]
    RegisterInvalidPassword(String),

    /// 400 `{"detail": "RESET_PASSWORD_BAD_TOKEN"}` — fastapi-users compat.
    #[error("reset password bad token")]
    ResetPasswordBadToken,

    /// 400 `{"detail": {"code": "RESET_PASSWORD_INVALID_PASSWORD", "reason": "..."}}`.
    #[error("reset password invalid password: {0}")]
    ResetPasswordInvalidPassword(String),

    /// 400 `{"detail": "VERIFY_USER_BAD_TOKEN"}` — fastapi-users compat.
    #[error("verify user bad token")]
    VerifyUserBadToken,

    /// 400 `{"detail": "VERIFY_USER_ALREADY_VERIFIED"}` — fastapi-users compat.
    #[error("verify user already verified")]
    VerifyUserAlreadyVerified,

    /// 400 `{"detail": "UPDATE_USER_EMAIL_ALREADY_EXISTS"}` — fastapi-users compat.
    #[error("update user email already exists")]
    UpdateUserEmailAlreadyExists,

    /// 400 `{"detail": {"code": "UPDATE_USER_INVALID_PASSWORD", "reason": "..."}}`.
    #[error("update user invalid password: {0}")]
    UpdateUserInvalidPassword(String),

    /// 400 `{"error": {"message": "..."}}` — unique envelope used ONLY by the api-keys router.
    /// **Never** use this variant outside `routers/api_keys.rs`.
    #[error("api key error: {0}")]
    ApiKeyEnvelope(String),

    /// 420/500 — pipeline job returned an error status.
    #[error("pipeline errored: {0}")]
    PipelineErrored(String),

    /// 418 `{"detail": "<msg>"}` — Python fallback for uncategorized errors.
    ///
    /// Changed from a unit variant to carry a message string for Python parity
    /// (`datasets` and `add` routers use this with dynamic messages).
    #[error("teapot: {0}")]
    Teapot(String),

    // ── P2 error variants ─────────────────────────────────────────────────────
    /// `add`/`update` error envelope: `{"error": "...", "detail": "..."}`.
    ///
    /// Used only by `routers/add.rs` and `routers/update.rs` — the only routes
    /// that deviate from the canonical `{"detail": "..."}` shape.
    #[error("write endpoint error: {error}")]
    WriteEndpointError {
        error: String,
        detail: Option<String>,
        status: StatusCode,
    },

    /// `<status> {"error": "..."}` — used by datasets 2.2 catch-all (409),
    /// datasets 2.6 (404), and datasets 2.8 (404).
    #[error("write envelope error: {0}")]
    WriteEnvelopeError(String, StatusCode),

    /// `<status> {"message": "..."}` — used by datasets 2.3 (404).
    #[error("error message: {0}")]
    ErrorMessageError(String, StatusCode),

    /// `<status> {"error": "..."}` — used by ontologies and forget.
    #[error("error envelope: {0}")]
    OntologyEnvelope(String, StatusCode),

    /// `409 {"error": "..."}` — used by the deprecated `/delete` endpoint.
    #[error("deprecated conflict: {0}")]
    DeprecatedConflict(String),

    /// 501 `{"detail": "..."}` — not implemented (e.g. unsupported storage scheme).
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// 500 — unhandled internal error.
    #[error("internal server error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, json!({"detail": msg})),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, json!({"detail": "Unauthorized"})),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, json!({"detail": msg})),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, json!({"detail": msg})),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, json!({"detail": msg})),
            ApiError::Validation(details) => {
                let mut map = serde_json::Map::new();
                map.insert("detail".into(), details.detail);
                if let Some(body) = details.body {
                    map.insert("body".into(), body);
                }
                (StatusCode::BAD_REQUEST, Value::Object(map))
            }
            ApiError::LoginBadCredentials => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "LOGIN_BAD_CREDENTIALS"}),
            ),
            ApiError::LoginUserNotVerified => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "LOGIN_USER_NOT_VERIFIED"}),
            ),
            ApiError::RegisterUserAlreadyExists => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "REGISTER_USER_ALREADY_EXISTS"}),
            ),
            ApiError::RegisterInvalidPassword(reason) => (
                StatusCode::BAD_REQUEST,
                json!({"detail": {"code": "REGISTER_INVALID_PASSWORD", "reason": reason}}),
            ),
            ApiError::ResetPasswordBadToken => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "RESET_PASSWORD_BAD_TOKEN"}),
            ),
            ApiError::ResetPasswordInvalidPassword(reason) => (
                StatusCode::BAD_REQUEST,
                json!({"detail": {"code": "RESET_PASSWORD_INVALID_PASSWORD", "reason": reason}}),
            ),
            ApiError::VerifyUserBadToken => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "VERIFY_USER_BAD_TOKEN"}),
            ),
            ApiError::VerifyUserAlreadyVerified => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "VERIFY_USER_ALREADY_VERIFIED"}),
            ),
            ApiError::UpdateUserEmailAlreadyExists => (
                StatusCode::BAD_REQUEST,
                json!({"detail": "UPDATE_USER_EMAIL_ALREADY_EXISTS"}),
            ),
            ApiError::UpdateUserInvalidPassword(reason) => (
                StatusCode::BAD_REQUEST,
                json!({"detail": {"code": "UPDATE_USER_INVALID_PASSWORD", "reason": reason}}),
            ),
            ApiError::ApiKeyEnvelope(message) => (
                StatusCode::BAD_REQUEST,
                json!({"error": {"message": message}}),
            ),
            ApiError::PipelineErrored(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, json!({"detail": msg}))
            }
            ApiError::Teapot(msg) => (StatusCode::IM_A_TEAPOT, json!({"detail": msg})),
            ApiError::WriteEndpointError {
                error,
                detail,
                status,
            } => (status, json!({"error": error, "detail": detail})),
            ApiError::WriteEnvelopeError(msg, status) => (status, json!({"error": msg})),
            ApiError::ErrorMessageError(msg, status) => (status, json!({"message": msg})),
            ApiError::OntologyEnvelope(msg, status) => (status, json!({"error": msg})),
            ApiError::DeprecatedConflict(msg) => (StatusCode::CONFLICT, json!({"error": msg})),
            ApiError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, json!({"detail": msg})),
            ApiError::Internal(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"detail": err.to_string()}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

// ─── ServerError ─────────────────────────────────────────────────────────────

/// Errors that can occur at server startup or runtime (not per-request).
#[derive(Debug, Error)]
pub enum ServerError {
    /// An I/O error (e.g. bind failure).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Lifecycle startup failed.
    #[error("lifecycle error: {0}")]
    Lifecycle(#[from] crate::lifecycle::LifecycleError),

    /// Catch-all for other startup failures.
    #[error("server error: {0}")]
    Other(#[from] anyhow::Error),
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn test_bad_request() {
        let resp = ApiError::BadRequest("oops".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["detail"], "oops");
    }

    #[tokio::test]
    async fn test_unauthorized() {
        let resp = ApiError::Unauthorized.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = body_json(resp).await;
        assert_eq!(body["detail"], "Unauthorized");
    }

    #[tokio::test]
    async fn test_validation() {
        let details = ValidationDetails {
            detail: serde_json::json!([{"loc": ["field"], "msg": "required"}]),
            body: Some(serde_json::json!({"x": 1})),
        };
        let resp = ApiError::Validation(details).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["detail"].is_array());
        assert!(body["body"].is_object());
    }

    #[tokio::test]
    async fn test_login_bad_credentials() {
        let resp = ApiError::LoginBadCredentials.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
    }

    #[tokio::test]
    async fn test_teapot_with_message() {
        let resp = ApiError::Teapot("Error retrieving datasets: db error".into()).into_response();
        assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
        let body = body_json(resp).await;
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or("")
                .contains("retrieving datasets")
        );
    }

    #[tokio::test]
    async fn test_write_endpoint_error() {
        let resp = ApiError::WriteEndpointError {
            error: "Pipeline run errored".into(),
            detail: Some("inner".into()),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Pipeline run errored");
    }

    #[tokio::test]
    async fn test_write_envelope_error() {
        let resp = ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Dataset not found");
    }

    #[tokio::test]
    async fn test_error_message_error() {
        let resp =
            ApiError::ErrorMessageError("Dataset (abc) not found.".into(), StatusCode::NOT_FOUND)
                .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["message"], "Dataset (abc) not found.");
    }

    #[tokio::test]
    async fn test_deprecated_conflict() {
        let resp = ApiError::DeprecatedConflict("some error".into()).into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "some error");
    }

    #[tokio::test]
    async fn test_not_implemented() {
        let resp =
            ApiError::NotImplemented("Storage scheme 's3' not supported".into()).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let body = body_json(resp).await;
        assert_eq!(body["detail"], "Storage scheme 's3' not supported");
    }
}
