//! Custom JSON extractor that emits `ApiError::Validation` on deserialization
//! failure instead of axum's default 422/400.
//!
//! Use `middleware::validation::Json<T>` instead of `axum::Json<T>` in handlers
//! that need the Python-shaped error envelope.

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Request},
    http::{header, request::Parts},
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::error::{ApiError, ValidationDetails};

// ─── LoginForm extractor ──────────────────────────────────────────────────────

/// Path-scoped `Form<T>` extractor for `POST /api/v1/auth/login`.
///
/// Maps any deserialization failure to `ApiError::LoginBadCredentials`
/// (the `{"detail":"LOGIN_BAD_CREDENTIALS"}` shape) instead of the
/// generic `ValidationDetails` array.  Only use this on the login handler —
/// applying it elsewhere would suppress structured validation errors on
/// `/register` etc.
///
/// Python reference: the custom `RequestValidationError` handler in
/// `client.py:165-176` overrides 422 → 400 with `LOGIN_BAD_CREDENTIALS`
/// specifically for `/api/v1/auth/login`.
pub struct LoginForm<T>(pub T);

impl<T, S> FromRequest<S> for LoginForm<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Form::<T>::from_request(req, state).await {
            Ok(axum::extract::Form(value)) => Ok(LoginForm(value)),
            Err(_) => Err(ApiError::LoginBadCredentials),
        }
    }
}

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

// ─── Query extractor (E-09 / Decision 9) ─────────────────────────────────────

/// Query-string extractor that maps `serde_urlencoded` failures to
/// `ApiError::Validation` with the same Python-shaped `{detail: [...], body: ...}`
/// envelope used by [`Json<T>`].
///
/// Sibling to [`Json<T>`] for query-string parameters. Lands as project-wide
/// infrastructure per Decision 9 (acknowledged divergence D-1) and is owned by
/// E-09 — every later v2 task with query-param validation needs reuses it.
///
/// On parse failure the extractor:
///   - sets HTTP status to 400 (Python's global 422→400 override applies);
///   - best-effort extracts the field name from the `serde_urlencoded` error
///     message and emits `loc = ["query", "<field>"]`. Falls back to
///     `loc = ["query"]` when the field cannot be determined.
///   - sets `type = "value_error"`.
///
/// Re-exported as `ValidatedQuery` at the module root.
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.uri.query().unwrap_or("");
        // Use `serde_path_to_error` to recover the path of the offending field
        // since `serde_urlencoded` does not include the field name in its
        // error messages for typed deserialization failures (e.g. unknown
        // enum variants on a `#[serde(rename = ...)]` field).
        let de = serde_urlencoded::Deserializer::new(form_urlencoded::parse(raw.as_bytes()));
        match serde_path_to_error::deserialize::<_, T>(de) {
            Ok(value) => Ok(Query(value)),
            Err(err) => {
                let path = err.path().to_string();
                let inner_msg = err.into_inner().to_string();
                let loc = if path.is_empty() || path == "." {
                    json!(["query"])
                } else {
                    // `serde_path_to_error` returns dotted paths like
                    // `order_by` for top-level fields. Take the leaf segment.
                    let leaf = path.rsplit('.').next().unwrap_or(path.as_str());
                    json!(["query", leaf])
                };
                Err(ApiError::Validation(ValidationDetails {
                    detail: json!([{
                        "loc": loc,
                        "msg": inner_msg,
                        "type": "value_error"
                    }]),
                    body: None,
                }))
            }
        }
    }
}

// ─── Re-exports ──────────────────────────────────────────────────────────────

/// Re-export of [`Query`] for handlers/tests that prefer the unambiguous name
/// over the bare `Query` (which can clash with `axum::extract::Query`).
pub use Query as ValidatedQuery;

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::{get, post},
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

    // ── ValidatedQuery<T> tests (E-09 / Decision 9) ────────────────────────

    #[derive(Deserialize, Default)]
    enum TestOrderBy {
        #[default]
        #[serde(rename = "last_activity_at")]
        LastActivityAt,
        #[serde(rename = "started_at")]
        StartedAt,
    }

    #[derive(Deserialize)]
    struct TestQuery {
        #[serde(default)]
        order_by: TestOrderBy,
        #[serde(default = "default_limit")]
        limit: u32,
    }

    fn default_limit() -> u32 {
        50
    }

    async fn query_handler(Query(q): Query<TestQuery>) -> String {
        format!(
            "limit={} ord={}",
            q.limit,
            matches!(q.order_by, TestOrderBy::LastActivityAt)
        )
    }

    fn query_app() -> Router {
        Router::new().route("/", get(query_handler))
    }

    #[tokio::test]
    async fn valid_query_succeeds() {
        let req = Request::builder()
            .method("GET")
            .uri("/?order_by=started_at&limit=42")
            .body(Body::empty())
            .expect("request");

        let resp = query_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_order_by_returns_400_with_python_envelope() {
        let req = Request::builder()
            .method("GET")
            .uri("/?order_by=banana")
            .body(Body::empty())
            .expect("request");

        let resp = query_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert!(body["detail"].is_array(), "detail should be array: {body}");
        let entry = &body["detail"][0];
        let loc = entry["loc"].as_array().expect("loc array");
        assert_eq!(loc[0], "query");
        // Best-effort field name; should resolve to `order_by`.
        assert_eq!(loc[1], "order_by", "loc should target order_by: {body}");
        let ty = entry["type"].as_str().expect("type str");
        assert!(
            ty.ends_with("value_error"),
            "type should be value_error: {ty}"
        );
    }

    #[tokio::test]
    async fn out_of_range_limit_returns_400_with_python_envelope() {
        // u32 deserialization rejects negative values; this asserts the
        // envelope shape on parse failures driven by serde_urlencoded.
        let req = Request::builder()
            .method("GET")
            .uri("/?limit=-1")
            .body(Body::empty())
            .expect("request");

        let resp = query_app().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert!(body["detail"].is_array());
        let entry = &body["detail"][0];
        let loc = entry["loc"].as_array().expect("loc array");
        assert_eq!(loc[0], "query");
        assert_eq!(loc[1], "limit", "loc should target limit: {body}");
        let ty = entry["type"].as_str().expect("type str");
        assert!(ty.ends_with("value_error"));
    }
}
