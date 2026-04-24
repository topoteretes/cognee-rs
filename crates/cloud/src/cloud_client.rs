//! Remote HTTP client that proxies V2 operations to a Cognee Cloud instance.
//!
//! Line-by-line port of `cognee/api/v1/serve/cloud_client.py`. All requests
//! use the `X-Api-Key` header for authentication, matching the SaaS
//! backend's API-key auth backend (cloud_client.py:17–31).
//!
//! # Wire compatibility
//!
//! The request / response shapes, field names, and paths match Python
//! byte-for-byte so that the Rust SDK and the Python SDK can share a
//! single deployed cloud tenant:
//!
//! - `POST /api/v1/remember` — multipart form with `datasetName`,
//!   optional `run_in_background`, optional `custom_prompt`, one or more
//!   `data` parts (text/plain for strings, file bytes for uploads).
//! - `POST /api/v1/recall` — JSON body with `query`, optional
//!   `search_type`, `datasets`, `top_k`, `system_prompt`.
//! - `POST /api/v1/improve` — JSON body with either `dataset_id` or
//!   `dataset_name`, optional `run_in_background`, optional `node_name`.
//! - `POST /api/v1/forget` — JSON body with optional `everything`,
//!   `dataset`, `data_id` flags.
//! - `GET  /health` — liveness probe.
//!
//! All four V2 operations return `serde_json::Value` to match Python's
//! dynamic return types (`dict` for remember/improve/forget, `list` for
//! recall). Typed wrappers can be layered on top if consumers ask for
//! them.

use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::error::{CloudError, CloudResult};

/// Async HTTP client for a remote Cognee Cloud tenant instance.
///
/// Holds a pre-configured [`reqwest::Client`] with the `X-Api-Key`
/// header baked in as a default header. The underlying connection pool
/// is reused across calls, matching the aiohttp `ClientSession`
/// semantics in Python (`cloud_client.py:26–31`).
#[derive(Debug, Clone)]
pub struct CloudClient {
    /// Base URL of the remote tenant instance (no trailing slash).
    pub service_url: String,
    /// API key sent on every request as `X-Api-Key`.
    pub api_key: String,
    /// Shared HTTP client with default auth header.
    client: reqwest::Client,
}

/// Input payload for [`CloudClient::remember`].
///
/// Richer than Python's dynamic `Any` argument, but maps cleanly onto
/// the same three branches in `cloud_client.py:62–83`:
///
/// - [`RememberData::Text`] — single string, sent as one `data` part
///   named `data.txt` with `Content-Type: text/plain`.
/// - [`RememberData::Texts`] — list of strings, each sent as its own
///   `data` part.
/// - [`RememberData::Files`] — one or more file paths whose bytes are
///   read and attached under the `data` field, keeping the original
///   filename.
#[derive(Debug, Clone)]
pub enum RememberData {
    /// A single text document.
    Text(String),
    /// A list of text documents.
    Texts(Vec<String>),
    /// One or more files on disk.
    Files(Vec<std::path::PathBuf>),
}

/// Dataset selector for [`CloudClient::improve`].
///
/// The Python client branches on `isinstance(dataset, UUID)` to choose
/// between `dataset_id` and `dataset_name` (`cloud_client.py:119–122`).
/// Modeling that as an enum in Rust avoids stringly-typed ambiguity.
#[derive(Debug, Clone)]
pub enum ImproveDataset {
    /// Dataset UUID (serialised as `dataset_id`).
    Id(Uuid),
    /// Human-readable dataset name (serialised as `dataset_name`).
    Name(String),
}

impl CloudClient {
    /// Construct a new client for the given tenant.
    ///
    /// Trailing slashes on `service_url` are trimmed to match
    /// `cloud_client.py:22` (`service_url.rstrip("/")`).
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Config`] if the API key contains bytes
    /// that cannot be expressed as an HTTP header value (e.g. newlines
    /// or non-visible ASCII), or [`CloudError::Http`] if the underlying
    /// reqwest builder fails.
    pub fn new(
        service_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> CloudResult<Arc<Self>> {
        let service_url = service_url.into().trim_end_matches('/').to_string();
        let api_key = api_key.into();

        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_str(&api_key).map_err(|e| {
            CloudError::Config(format!("api key is not a valid HTTP header value: {e}"))
        })?;
        headers.insert("X-Api-Key", value);

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        Ok(Arc::new(Self {
            service_url,
            api_key,
            client,
        }))
    }

    /// Close the HTTP client.
    ///
    /// Python explicitly closes the aiohttp session
    /// (`cloud_client.py:33–36`); reqwest's pool is dropped when the
    /// struct goes out of scope, so this method is a no-op that exists
    /// purely to match the Python API surface.
    pub async fn close(&self) {}

    /// `GET /health` — verify the remote instance is reachable.
    ///
    /// Returns `true` on a 200 response, `false` on any other status or
    /// any network error. Mirrors `cloud_client.py:38–45`.
    pub async fn health_check(&self) -> bool {
        self.client
            .get(format!("{}/health", self.service_url))
            .send()
            .await
            .map(|r| r.status() == 200)
            .unwrap_or(false)
    }

    /// `POST /api/v1/remember` — ingest data and build knowledge graph.
    ///
    /// Port of `cloud_client.py:49–89`.
    pub async fn remember(
        &self,
        data: RememberData,
        dataset_name: &str,
        run_in_background: bool,
        custom_prompt: Option<&str>,
    ) -> CloudResult<Value> {
        let mut form = Form::new().text("datasetName", dataset_name.to_string());
        if run_in_background {
            form = form.text("run_in_background", "true");
        }
        if let Some(p) = custom_prompt {
            form = form.text("custom_prompt", p.to_string());
        }

        match data {
            RememberData::Text(s) => {
                form = form.part(
                    "data",
                    Part::bytes(s.into_bytes())
                        .file_name("data.txt")
                        .mime_str("text/plain")?,
                );
            }
            RememberData::Texts(items) => {
                for s in items {
                    form = form.part(
                        "data",
                        Part::bytes(s.into_bytes())
                            .file_name("data.txt")
                            .mime_str("text/plain")?,
                    );
                }
            }
            RememberData::Files(paths) => {
                for path in paths {
                    let bytes = tokio::fs::read(&path).await?;
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("upload")
                        .to_string();
                    form = form.part("data", Part::bytes(bytes).file_name(name));
                }
            }
        }

        self.post_multipart("remember", "/api/v1/remember", form)
            .await
    }

    /// `POST /api/v1/recall` — query the knowledge graph.
    ///
    /// Port of `cloud_client.py:91–112`.
    pub async fn recall(
        &self,
        query_text: &str,
        query_type: Option<&str>,
        datasets: Option<Vec<String>>,
        top_k: Option<usize>,
        system_prompt: Option<&str>,
    ) -> CloudResult<Value> {
        let mut body = Map::new();
        body.insert("query".into(), Value::String(query_text.into()));
        if let Some(t) = query_type {
            body.insert("search_type".into(), Value::String(t.into()));
        }
        if let Some(d) = datasets {
            body.insert("datasets".into(), json!(d));
        }
        if let Some(k) = top_k {
            body.insert("top_k".into(), json!(k));
        }
        if let Some(p) = system_prompt {
            body.insert("system_prompt".into(), Value::String(p.into()));
        }
        self.post_json("recall", "/api/v1/recall", Value::Object(body))
            .await
    }

    /// `POST /api/v1/improve` — enrich the knowledge graph.
    ///
    /// Port of `cloud_client.py:114–135`.
    pub async fn improve(
        &self,
        dataset: ImproveDataset,
        run_in_background: bool,
        node_name: Option<&str>,
    ) -> CloudResult<Value> {
        let mut body = Map::new();
        match dataset {
            ImproveDataset::Id(id) => {
                body.insert("dataset_id".into(), Value::String(id.to_string()));
            }
            ImproveDataset::Name(n) => {
                body.insert("dataset_name".into(), Value::String(n));
            }
        }
        if run_in_background {
            body.insert("run_in_background".into(), Value::Bool(true));
        }
        if let Some(n) = node_name {
            body.insert("node_name".into(), Value::String(n.into()));
        }
        self.post_json("improve", "/api/v1/improve", Value::Object(body))
            .await
    }

    /// `POST /api/v1/forget` — delete data from the knowledge graph.
    ///
    /// Port of `cloud_client.py:137–157`. The Python implementation
    /// ignores any `dataset`/`data_id` kwargs when they are empty, so
    /// we use `Option<String>` rather than `&str`.
    pub async fn forget(
        &self,
        everything: bool,
        dataset: Option<String>,
        data_id: Option<String>,
    ) -> CloudResult<Value> {
        let mut body = Map::new();
        if everything {
            body.insert("everything".into(), Value::Bool(true));
        }
        if let Some(d) = dataset {
            body.insert("dataset".into(), Value::String(d));
        }
        if let Some(d) = data_id {
            body.insert("data_id".into(), Value::String(d));
        }
        self.post_json("forget", "/api/v1/forget", Value::Object(body))
            .await
    }

    // ---- helpers ----

    async fn post_multipart(&self, op: &'static str, path: &str, form: Form) -> CloudResult<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.service_url, path))
            .multipart(form)
            .send()
            .await?;
        Self::read_json_or_error(op, resp).await
    }

    async fn post_json(&self, op: &'static str, path: &str, body: Value) -> CloudResult<Value> {
        let resp = self
            .client
            .post(format!("{}{}", self.service_url, path))
            .json(&body)
            .send()
            .await?;
        Self::read_json_or_error(op, resp).await
    }

    async fn read_json_or_error(op: &'static str, resp: reqwest::Response) -> CloudResult<Value> {
        let status = resp.status();
        if status.as_u16() >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::RemoteOp {
                op,
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_trims_trailing_slash_and_stores_fields() {
        let client = CloudClient::new("https://cognee.example.com/", "secret-key")
            .expect("construction should succeed for a valid key");
        assert_eq!(client.service_url, "https://cognee.example.com");
        assert_eq!(client.api_key, "secret-key");
    }

    #[test]
    fn constructor_rejects_invalid_api_key_header() {
        // A raw newline cannot appear in an HTTP header value.
        let err = CloudClient::new("https://example.com", "bad\nkey")
            .expect_err("newline in api key must fail");
        match err {
            CloudError::Config(_) => {}
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn health_check_returns_true_on_200() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        assert!(client.health_check().await);
    }

    #[tokio::test]
    async fn health_check_returns_false_on_500() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/health")
            .with_status(500)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        assert!(!client.health_check().await);
    }

    #[tokio::test]
    async fn remember_sends_text_as_multipart_with_api_key_header() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/remember")
            .match_header("x-api-key", "secret-key")
            // multipart bodies are non-deterministic (random boundary),
            // so we just confirm the dataset name + data payload appear.
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("datasetName".into()),
                mockito::Matcher::Regex("main_dataset".into()),
                mockito::Matcher::Regex("hello world".into()),
                mockito::Matcher::Regex("data.txt".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ok","dataset_id":"abc"}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "secret-key").expect("construct ok");
        let out = client
            .remember(
                RememberData::Text("hello world".into()),
                "main_dataset",
                false,
                None,
            )
            .await
            .expect("200 response should deserialize");
        mock.assert_async().await;

        assert_eq!(out["status"], "ok");
        assert_eq!(out["dataset_id"], "abc");
    }

    #[tokio::test]
    async fn remember_includes_optional_flags() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/remember")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("run_in_background".into()),
                mockito::Matcher::Regex("true".into()),
                mockito::Matcher::Regex("custom_prompt".into()),
                mockito::Matcher::Regex("extract facts".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        client
            .remember(
                RememberData::Text("body".into()),
                "ds",
                true,
                Some("extract facts"),
            )
            .await
            .expect("should succeed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn recall_posts_expected_json_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/recall")
            .match_header("x-api-key", "key")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::Json(json!({
                "query": "what is cognee?",
                "search_type": "GRAPH_COMPLETION",
                "datasets": ["main_dataset"],
                "top_k": 5,
                "system_prompt": "be concise",
            })))
            .with_status(200)
            .with_body(r#"[{"answer":"it's an AI memory pipeline"}]"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let out = client
            .recall(
                "what is cognee?",
                Some("GRAPH_COMPLETION"),
                Some(vec!["main_dataset".into()]),
                Some(5),
                Some("be concise"),
            )
            .await
            .expect("200 should deserialize");
        mock.assert_async().await;

        assert!(out.is_array());
        assert_eq!(out[0]["answer"], "it's an AI memory pipeline");
    }

    #[tokio::test]
    async fn recall_minimal_body_only_has_query() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/recall")
            .match_body(mockito::Matcher::Json(json!({ "query": "hi" })))
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let out = client
            .recall("hi", None, None, None, None)
            .await
            .expect("200 ok");
        mock.assert_async().await;

        assert_eq!(out, json!([]));
    }

    #[tokio::test]
    async fn improve_with_dataset_id_sends_uuid_string() {
        let mut server = mockito::Server::new_async().await;
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("static uuid");
        let mock = server
            .mock("POST", "/api/v1/improve")
            .match_body(mockito::Matcher::Json(json!({
                "dataset_id": id.to_string(),
                "run_in_background": true,
                "node_name": "Person",
            })))
            .with_status(200)
            .with_body(r#"{"enriched":true}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let out = client
            .improve(ImproveDataset::Id(id), true, Some("Person"))
            .await
            .expect("200 ok");
        mock.assert_async().await;
        assert_eq!(out["enriched"], true);
    }

    #[tokio::test]
    async fn improve_with_dataset_name_uses_dataset_name_field() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/improve")
            .match_body(mockito::Matcher::Json(json!({
                "dataset_name": "main_dataset",
            })))
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        client
            .improve(ImproveDataset::Name("main_dataset".into()), false, None)
            .await
            .expect("200 ok");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn forget_with_everything_flag() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/forget")
            .match_body(mockito::Matcher::Json(json!({ "everything": true })))
            .with_status(200)
            .with_body(r#"{"deleted":"all"}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let out = client.forget(true, None, None).await.expect("200 ok");
        mock.assert_async().await;
        assert_eq!(out["deleted"], "all");
    }

    #[tokio::test]
    async fn forget_with_dataset_and_data_id() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/forget")
            .match_body(mockito::Matcher::Json(json!({
                "dataset": "main_dataset",
                "data_id": "abc-123",
            })))
            .with_status(200)
            .with_body(r#"{"deleted":1}"#)
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        client
            .forget(false, Some("main_dataset".into()), Some("abc-123".into()))
            .await
            .expect("200 ok");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn remote_op_surfaces_401_unauthorized() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/v1/recall")
            .with_status(401)
            .with_body("invalid api key")
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "wrong-key").expect("construct ok");
        let err = client
            .recall("q", None, None, None, None)
            .await
            .expect_err("401 must error");
        match err {
            CloudError::RemoteOp { op, status, body } => {
                assert_eq!(op, "recall");
                assert_eq!(status, 401);
                assert_eq!(body, "invalid api key");
            }
            other => panic!("expected RemoteOp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remote_op_surfaces_404_not_found() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/v1/improve")
            .with_status(404)
            .with_body("unknown dataset")
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let err = client
            .improve(ImproveDataset::Name("missing".into()), false, None)
            .await
            .expect_err("404 must error");
        match err {
            CloudError::RemoteOp { op, status, body } => {
                assert_eq!(op, "improve");
                assert_eq!(status, 404);
                assert_eq!(body, "unknown dataset");
            }
            other => panic!("expected RemoteOp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remote_op_surfaces_500_internal_error() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/v1/forget")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;

        let client = CloudClient::new(server.url(), "key").expect("construct ok");
        let err = client
            .forget(true, None, None)
            .await
            .expect_err("500 must error");
        match err {
            CloudError::RemoteOp { op, status, body } => {
                assert_eq!(op, "forget");
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected RemoteOp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn close_is_a_noop() {
        let client = CloudClient::new("https://example.com", "k").expect("construct ok");
        // Simply confirm close runs to completion without panicking.
        client.close().await;
    }
}
