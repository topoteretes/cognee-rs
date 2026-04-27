//! Custom JSON extractor that emits `ApiError::Validation` on deserialization
//! failure instead of axum's default 422/400.
//!
//! Use `middleware::validation::Json<T>` instead of `axum::Json<T>` in handlers
//! that need the Python-shaped error envelope.

use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    http::header,
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::error::{ApiError, ValidationDetails};

/// Drop-in replacement for `axum::Json` that converts `serde_json` parse errors
/// into `ApiError::Validation` with the Python-shaped `{detail: [...], body: ...}`
/// envelope.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Check content-type header
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_lowercase();

        if !content_type.contains("application/json") {
            return Err(ApiError::Validation(ValidationDetails {
                detail: json!([{
                    "loc": ["headers", "content-type"],
                    "msg": "content-type must be application/json",
                    "type": "value_error"
                }]),
                body: None,
            }));
        }

        // Read the raw body bytes
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            ApiError::Validation(ValidationDetails {
                detail: json!([{"loc": ["body"], "msg": e.to_string(), "type": "read_error"}]),
                body: None,
            })
        })?;

        // Try to parse the body as the target type
        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => Ok(Json(value)),
            Err(err) => {
                // Try to capture the raw body for debugging
                let raw_body = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
                Err(ApiError::Validation(ValidationDetails {
                    detail: json!([{
                        "loc": ["body"],
                        "msg": err.to_string(),
                        "type": "value_error.json_parse"
                    }]),
                    body: raw_body,
                }))
            }
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    struct Payload {
        name: String,
    }

    async fn handler(Json(p): Json<Payload>) -> String {
        p.name
    }

    fn app() -> Router {
        Router::new().route("/", post(handler))
    }

    #[tokio::test]
    async fn test_missing_required_field_yields_validation_error() {
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"other": "value"}"#))
            .expect("request");

        let resp = app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        // detail must be an array with at least one entry
        assert!(body["detail"].is_array(), "detail should be array: {body}");
    }

    #[tokio::test]
    async fn test_wrong_content_type_yields_validation_error() {
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "text/plain")
            .body(Body::from(r#"{"name": "test"}"#))
            .expect("request");

        let resp = app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_valid_json_succeeds() {
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name": "alice"}"#))
            .expect("request");

        let resp = app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
