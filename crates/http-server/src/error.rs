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

// ─── PipelineErrorSource ──────────────────────────────────────────────────────

/// Which pipeline router produced a `PipelineErrored` event.
///
/// The `IntoResponse` implementation of `ApiError::PipelineErrored` uses this
/// to choose the HTTP status code and body shape per Python parity:
///
/// | Source | Status | Body |
/// |---|---|---|
/// | `Improve` | **420** | raw serialised `PipelineRunInfoDTO` |
/// | all others | 500 | `{"error": "Pipeline run errored", "detail": "<msg>"}` |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineErrorSource {
    /// `/cognify` — 500, canonical envelope.
    Cognify,
    /// `/memify` — 500, canonical envelope.
    Memify,
    /// `/improve` — **420**, raw run-info body (Python parity quirk).
    Improve,
    /// `/remember` — does NOT use this variant; its catch-all is 409.
    /// Included for completeness and future use.
    Remember,
    /// `/sync` — 500, canonical envelope (not yet wired).
    Sync,
}

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

    /// 420 (Improve) or 500 (all others) — pipeline job returned an error.
    ///
    /// * `pipeline_source = Improve` → HTTP 420, body is the raw `run_info` value
    ///   (Python's `/improve` returns the `PipelineRunInfo` object directly,
    ///   not the canonical envelope).
    /// * all other sources → HTTP 500, body is
    ///   `{"error": "Pipeline run errored", "detail": "<msg>"}`.
    ///
    /// `/remember` does **not** use this variant — its catch-all is
    /// `ApiError::DeprecatedConflict("An error occurred during remember.")` per
    /// Python parity (produces `{"error": "..."}`, not `{"detail": "..."}`).
    ///
    /// `/improve`'s generic catch-all similarly uses `ApiError::DeprecatedConflict`.
    ///
    /// Note: the field is named `pipeline_source` (not `source`) to prevent
    /// `thiserror` from treating it as an error-chain source.
    #[error("pipeline errored ({pipeline_source:?})")]
    PipelineErrored {
        pipeline_source: PipelineErrorSource,
        /// For `Improve`: the full serialised `PipelineRunInfoDTO`.
        /// For others: `serde_json::json!({"error": "Pipeline run errored", "detail": "<msg>"})`.
        run_info: serde_json::Value,
    },

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

    /// `409 {"error": "..."}` — used by the deprecated `/delete` endpoint and by
    /// the `/remember` and `/improve` catch-all error paths.
    ///
    /// Python parity: those routers return `JSONResponse({"error": "..."})` with
    /// status 409, **not** `HTTPException(409, detail=...)`, so the body key is
    /// `"error"` rather than `"detail"`.  This is intentionally different from
    /// `ApiError::Conflict` which uses the `"detail"` key.
    #[error("conflict error: {0}")]
    DeprecatedConflict(String),

    /// 501 `{"detail": "..."}` — not implemented (e.g. unsupported storage scheme).
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// 503 `{"error": "..."}` — service unavailable.
    ///
    /// Used by `/api/v1/remember/entry` when the session cache is not
    /// configured. Mirrors Python's
    /// `JSONResponse(status_code=503, content={"error": str(error)})`
    /// (`get_remember_router.py:158-160`).
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// 501 `{"detail": "...", "code": "..."}` — structured stub envelope.
    ///
    /// Field order is wire-load-bearing: `detail` first, then `code`, matching
    /// Python's `JSONResponse({"detail": ..., "code": ...})` insertion order.
    /// Used by the notebooks `/run` stub and the responses stub.
    #[error("not implemented stub: {detail}")]
    NotImplementedStub {
        code: &'static str,
        detail: &'static str,
    },

    // ── P4 read-path error variants ───────────────────────────────────────────
    //
    // DO NOT NORMALIZE — these envelopes are wire-compatibility constraints
    // pinned to Python. See `routers/README.md §3.1` and the per-router specs
    // (`routers/search.md`, `routers/recall.md`, `routers/llm.md`,
    // `routers/visualize.md`) before touching them.
    /// `<status> {"error": "<error>", "detail": "<detail>"}` — used by
    /// `/api/v1/search` (403/422/500) and `GET /api/v1/search` (500).
    /// Mirrors Python's `ErrorResponse` model.
    #[error("search error: {error}")]
    SearchError {
        status: StatusCode,
        error: String,
        detail: Option<String>,
    },

    /// Three-shaped envelope used only by `/api/v1/recall`.
    ///
    /// - `WithHint { error, hint }` for 422 prerequisite errors.
    /// - `JustError { error }` for the 409 catch-all and the GET-history 500.
    ///
    /// Permission denied is NOT encoded here — the recall handler returns
    /// `200 []` directly without going through `ApiError`.
    #[error("recall error")]
    RecallError {
        status: StatusCode,
        body: RecallErrorBody,
    },

    /// `<status> {"error": "<msg>"}` — used by `/api/v1/llm/*` for 400/409/422.
    #[error("llm error: {1}")]
    LlmError(StatusCode, String),

    /// `<status> {"error": "<msg>"}` — used by `/api/v1/visualize/*` for the
    /// 409 catch-all and the 403 superuser-only failure.
    #[error("visualize error: {1}")]
    VisualizeError(StatusCode, String),

    /// 500 — unhandled internal error.
    #[error("internal server error: {0}")]
    Internal(#[from] anyhow::Error),
}

// ─── RecallErrorBody ──────────────────────────────────────────────────────────

/// Recall-router-specific error envelopes.
///
/// Python produces three distinct shapes for `/api/v1/recall`:
/// - `{error, hint}` for 422 prerequisite errors.
/// - `{error}` for the 409 catch-all and the GET-history 500.
/// - silent `200 []` for permission denied (NOT encoded here — handled at the
///   handler layer by returning `Ok(Json(vec![]))` without an `ApiError`).
///
/// Serialized as an `untagged` enum so the JSON shape per variant matches Python.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum RecallErrorBody {
    /// `{"error": "...", "hint": "..."}` — 422 prerequisite errors.
    WithHint { error: String, hint: String },
    /// `{"error": "..."}` — 409 catch-all and GET-history 500.
    JustError { error: String },
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
            ApiError::PipelineErrored {
                pipeline_source,
                run_info,
            } => match pipeline_source {
                PipelineErrorSource::Improve => {
                    // Python parity: /improve returns the raw PipelineRunInfo
                    // object as the body with HTTP 420, not the canonical
                    // {"error":..., "detail":...} envelope.
                    // StatusCode 420 is not a standard IANA code; construct
                    // it from the raw integer.
                    let status =
                        StatusCode::from_u16(420).expect("420 is a valid HTTP status code");
                    return (status, Json(run_info)).into_response();
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, run_info),
            },
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
            ApiError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, json!({"error": msg}))
            }
            ApiError::NotImplementedStub { code, detail } => {
                // Field order is load-bearing: detail first, then code
                // (matches Python's JSONResponse dict insertion order).
                // serde_json::Map uses BTreeMap (no preserve_order feature),
                // so we emit a raw JSON string to guarantee field order.
                let raw = format!(
                    "{{\"detail\":{},\"code\":{}}}",
                    serde_json::Value::String(detail.to_string()),
                    serde_json::Value::String(code.to_string()),
                );
                return (
                    StatusCode::NOT_IMPLEMENTED,
                    axum::response::Response::builder()
                        .status(StatusCode::NOT_IMPLEMENTED)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(axum::body::Body::from(raw))
                        .expect("valid response builder args"),
                )
                    .into_response();
            }
            ApiError::SearchError {
                status,
                error,
                detail,
            } => (status, json!({"error": error, "detail": detail})),
            ApiError::RecallError { status, body } => {
                let value = serde_json::to_value(&body).unwrap_or_else(|_| json!({}));
                (status, value)
            }
            ApiError::LlmError(status, msg) => (status, json!({"error": msg})),
            ApiError::VisualizeError(status, msg) => (status, json!({"error": msg})),
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

    #[tokio::test]
    async fn test_not_implemented_stub_status_and_field_order() {
        let resp = ApiError::NotImplementedStub {
            code: "X",
            detail: "y",
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

        // Verify exact byte output — field order is load-bearing.
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let body_str = std::str::from_utf8(&bytes).expect("utf8");
        assert_eq!(body_str, r#"{"detail":"y","code":"X"}"#);
    }

    #[tokio::test]
    async fn test_not_implemented_stub_notebook_run() {
        let resp = ApiError::NotImplementedStub {
            code: "NOTEBOOK_RUN_NOT_IMPLEMENTED",
            detail: "Notebook cell execution is not implemented in this build",
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let body_str = std::str::from_utf8(&bytes).expect("utf8");
        assert_eq!(
            body_str,
            r#"{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}"#
        );
    }

    #[tokio::test]
    async fn test_not_implemented_stub_responses() {
        let resp = ApiError::NotImplementedStub {
            code: "RESPONSES_NOT_IMPLEMENTED",
            detail: "OpenAI Responses API surface is not implemented in this build",
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let body_str = std::str::from_utf8(&bytes).expect("utf8");
        assert_eq!(
            body_str,
            r#"{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}"#
        );
    }

    // ── PipelineErrored variant tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_pipeline_errored_cognify_returns_500() {
        let resp = ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Cognify,
            run_info: serde_json::json!({"error": "Pipeline run errored", "detail": "boom"}),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Pipeline run errored");
        assert_eq!(body["detail"], "boom");
    }

    #[tokio::test]
    async fn test_pipeline_errored_memify_returns_500() {
        let resp = ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Memify,
            run_info: serde_json::json!({"error": "Pipeline run errored", "detail": "memify fail"}),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_pipeline_errored_improve_returns_420() {
        let run_info = serde_json::json!({
            "status": "PipelineRunErrored",
            "pipeline_run_id": "00000000-0000-0000-0000-000000000001",
            "dataset_id": "00000000-0000-0000-0000-000000000002",
            "dataset_name": "test",
            "error": "improve failed"
        });
        let resp = ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Improve,
            run_info: run_info.clone(),
        }
        .into_response();
        // 420 is the Python-parity quirk for /improve
        assert_eq!(resp.status().as_u16(), 420);
        // Body is the raw PipelineRunInfoDTO, NOT the canonical envelope
        let body = body_json(resp).await;
        assert_eq!(body["status"], "PipelineRunErrored");
        assert_eq!(body["error"], "improve failed");
        // Must NOT have the canonical {"error": "Pipeline run errored"} wrapper
        assert_ne!(body["error"], "Pipeline run errored");
    }

    // ── P4 envelope tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_search_error_envelope() {
        let resp = ApiError::SearchError {
            status: StatusCode::FORBIDDEN,
            error: "Permission denied".into(),
            detail: Some("No read on dataset".into()),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Permission denied");
        assert_eq!(body["detail"], "No read on dataset");
    }

    #[tokio::test]
    async fn test_search_error_with_null_detail() {
        let resp = ApiError::SearchError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "Internal server error".into(),
            detail: None,
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Internal server error");
        assert!(body["detail"].is_null());
    }

    #[tokio::test]
    async fn test_recall_error_with_hint_envelope() {
        let resp = ApiError::RecallError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: RecallErrorBody::WithHint {
                error: "Recall prerequisites not met".into(),
                hint: "Run cognify first".into(),
            },
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Recall prerequisites not met");
        assert_eq!(body["hint"], "Run cognify first");
        // No `detail` key — recall uses `hint`, not `detail`.
        assert!(body.get("detail").is_none());
    }

    #[tokio::test]
    async fn test_recall_error_just_error_envelope() {
        let resp = ApiError::RecallError {
            status: StatusCode::CONFLICT,
            body: RecallErrorBody::JustError {
                error: "An error occurred during recall.".into(),
            },
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "An error occurred during recall.");
        // Single-field envelope — no `detail`, no `hint`.
        assert!(body.get("detail").is_none());
        assert!(body.get("hint").is_none());
    }

    #[tokio::test]
    async fn test_llm_error_envelope() {
        let resp =
            ApiError::LlmError(StatusCode::CONFLICT, "Network failure".into()).into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "Network failure");
        assert!(body.get("detail").is_none());
    }

    #[tokio::test]
    async fn test_visualize_error_envelope() {
        let resp = ApiError::VisualizeError(
            StatusCode::FORBIDDEN,
            "Superuser privileges required for multi-user visualization".into(),
        )
        .into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(
            body["error"],
            "Superuser privileges required for multi-user visualization"
        );
        assert!(body.get("detail").is_none());
    }

    #[tokio::test]
    async fn test_pipeline_errored_sync_returns_500() {
        let resp = ApiError::PipelineErrored {
            pipeline_source: PipelineErrorSource::Sync,
            run_info: serde_json::json!({"error": "Pipeline run errored", "detail": "sync fail"}),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
