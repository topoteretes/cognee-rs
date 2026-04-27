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

    /// 418 — fallback for improperly formed errors (Python parity).
    #[error("unexpected error")]
    Teapot,

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
            ApiError::Teapot => (
                StatusCode::IM_A_TEAPOT,
                json!({"detail": "An unexpected error occurred."}),
            ),
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
    async fn test_teapot() {
        let resp = ApiError::Teapot.into_response();
        assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
    }
}
