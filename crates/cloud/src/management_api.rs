//! Management API client for tenant discovery and API-key provisioning.
//!
//! Line-by-line port of `cognee/api/v1/serve/management_api.py`. The
//! wire format (paths, query params, JSON shapes) is identical — both
//! SDKs talk to the same `COGNEE_CLOUD_URL` and expect the same
//! responses.
//!
//! # Public surface
//!
//! Free functions mirroring Python one-to-one:
//! - [`get_current_tenant`] — `GET /api/tenants/current`.
//! - [`create_tenant`] — `POST /api/tenants?tenant_name=…` + poll.
//! - [`get_service_url`] — `GET /api/tenants/current/service-url`.
//! - [`get_or_create_api_key`] — `GET` then `POST /api/api-keys`.
//!
//! Plus a thin convenience wrapper:
//! - [`ManagementApiClient`] — holds a `reqwest::Client`, a base URL, and
//!   an access token so the call sites don't have to thread those three
//!   arguments through every call. Exposes higher-level idempotent
//!   helpers: [`ensure_tenant`](ManagementApiClient::ensure_tenant),
//!   [`ensure_api_key`](ManagementApiClient::ensure_api_key),
//!   [`list_tenants`](ManagementApiClient::list_tenants),
//!   [`get_tenant`](ManagementApiClient::get_tenant).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::config::cloud_url;
use crate::error::{CloudError, CloudResult};

/// Default overall timeout for tenant provisioning. Matches
/// `management_api.py:62` (`poll_timeout: int = 300`).
pub const DEFAULT_TENANT_POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// Default interval between provisioning polls. Matches
/// `management_api.py:63` (`poll_interval: int = 5`).
pub const DEFAULT_TENANT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Default max retries when creating an API key. Matches
/// `management_api.py:121` (`max_retries: int = 3`).
pub const DEFAULT_API_KEY_MAX_RETRIES: u32 = 3;

/// HTTP timeout applied to every management-API call. Tenant provisioning
/// polls are individual short requests — the overall provisioning wait
/// is handled separately via the polling loop.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Tenant descriptor returned by the management API.
///
/// The `id` field is `String` so we tolerate both UUID and integer
/// representations — Python does the same (`management_api.py:55`).
///
/// Extra fields that newer management-API versions may return (like
/// `created_at`) are preserved via the catch-all `extra` map so
/// callers don't lose data when re-serialising. Unknown fields do not
/// error out — this matches the Python dataclass which just ignores
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantInfo {
    /// Tenant UUID (as a string for the reason noted above).
    #[serde(default, deserialize_with = "de_id_to_string")]
    pub id: String,
    /// Human-readable tenant name. The SDK writes these as
    /// `tenant-<uuid5>` via [`email_to_tenant_name`].
    #[serde(default)]
    pub name: String,
    /// ISO-8601 `created_at` timestamp if the server supplies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Any additional fields the server returns — forward compat.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Accept either `{"id": "uuid-str"}` or `{"id": 123}` — the management
/// API has historically surfaced both.
fn de_id_to_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Null => Ok(String::new()),
        other => Err(D::Error::custom(format!(
            "tenant id must be string or number, got {other}"
        ))),
    }
}

/// Deterministic tenant name derived from an email address.
///
/// Matches the Python / frontend convention
/// `tenant-{uuid5(NAMESPACE_URL, email)}` in `management_api.py:31–36`.
///
/// ```
/// use cognee_cloud::management_api::email_to_tenant_name;
/// assert_eq!(
///     email_to_tenant_name("test@example.com"),
///     "tenant-5081778e-c036-5085-8adb-6f1892daaa73",
/// );
/// ```
pub fn email_to_tenant_name(email: &str) -> String {
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, email.as_bytes());
    format!("tenant-{id}")
}

fn auth_header(access_token: &str) -> String {
    format!("Bearer {access_token}")
}

fn build_http_client() -> CloudResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(CloudError::from)
}

/// Resolve the management URL: explicit arg wins, then env, then default.
fn resolve_management_url(override_url: Option<&str>) -> String {
    override_url
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(cloud_url)
}

/// `GET /api/tenants/current` — returns the user's active tenant or
/// `Ok(None)` if the server responds 404.
///
/// Mirrors `get_current_tenant()` in `management_api.py:39–55`.
pub async fn get_current_tenant(
    management_url: &str,
    access_token: &str,
) -> CloudResult<Option<TenantInfo>> {
    let http = build_http_client()?;
    let resp = http
        .get(format!("{management_url}/api/tenants/current"))
        .header("Authorization", auth_header(access_token))
        .send()
        .await?;

    match resp.status().as_u16() {
        404 => Ok(None),
        200 => {
            let tenant: TenantInfo = resp.json().await?;
            Ok(Some(tenant))
        }
        status => {
            let body = resp.text().await.unwrap_or_default();
            Err(CloudError::ManagementApi { status, body })
        }
    }
}

/// `POST /api/tenants?tenant_name=…` then poll [`get_current_tenant`]
/// until the record exists (tenant provisioning can take up to a couple
/// of minutes).
///
/// Mirrors `create_tenant()` in `management_api.py:58–95`.
///
/// # Errors
///
/// - [`CloudError::ManagementApi`] if the POST returns a non-200/201/202.
/// - [`CloudError::TenantProvisionTimeout`] if polling exceeds
///   `poll_timeout`.
pub async fn create_tenant(
    management_url: &str,
    access_token: &str,
    email: &str,
    poll_timeout: Duration,
    poll_interval: Duration,
) -> CloudResult<TenantInfo> {
    let name = email_to_tenant_name(email);
    tracing::info!(target: "cognee_cloud::management_api", tenant_name = %name, %email, "creating tenant");

    let http = build_http_client()?;
    let resp = http
        .post(format!("{management_url}/api/tenants"))
        .query(&[("tenant_name", name.as_str())])
        .header("Authorization", auth_header(access_token))
        .send()
        .await?;
    let status = resp.status().as_u16();
    if !matches!(status, 200..=202) {
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi { status, body });
    }

    let deadline = Instant::now() + poll_timeout;
    while Instant::now() < deadline {
        sleep(poll_interval).await;
        if let Some(t) = get_current_tenant(management_url, access_token).await?
            && !t.id.is_empty()
        {
            tracing::info!(
                target: "cognee_cloud::management_api",
                tenant_id = %t.id,
                tenant_name = %t.name,
                "tenant ready",
            );
            return Ok(t);
        }
    }
    Err(CloudError::TenantProvisionTimeout(poll_timeout.as_secs()))
}

/// `GET /api/tenants/current/service-url`.
///
/// Mirrors `get_service_url()` in `management_api.py:98–115`. The
/// response body may use either `service_url` or `url` as the key;
/// Python tolerates both, so we do too.
pub async fn get_service_url(management_url: &str, access_token: &str) -> CloudResult<String> {
    let http = build_http_client()?;
    let resp = http
        .get(format!("{management_url}/api/tenants/current/service-url"))
        .header("Authorization", auth_header(access_token))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi {
            status: status.as_u16(),
            body,
        });
    }
    let v: serde_json::Value = resp.json().await?;
    let url = v
        .get("service_url")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("url").and_then(|x| x.as_str()))
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    if url.is_empty() {
        return Err(CloudError::EmptyServiceUrl);
    }
    Ok(url)
}

/// Get an existing API key (first in the list) or POST a new one with
/// exponential-backoff retries.
///
/// Mirrors `get_or_create_api_key()` in `management_api.py:118–154`.
pub async fn get_or_create_api_key(
    management_url: &str,
    access_token: &str,
    max_retries: u32,
) -> CloudResult<String> {
    let http = build_http_client()?;
    let auth = auth_header(access_token);

    // Phase 1: GET existing keys.
    let resp = http
        .get(format!("{management_url}/api/api-keys"))
        .header("Authorization", &auth)
        .send()
        .await?;
    if resp.status().is_success() {
        let keys: serde_json::Value = resp.json().await?;
        if let Some(arr) = keys.as_array()
            && let Some(first) = arr.first()
        {
            let key = first
                .get("key")
                .and_then(|x| x.as_str())
                .or_else(|| first.get("api_key").and_then(|x| x.as_str()))
                .unwrap_or("");
            if !key.is_empty() {
                return Ok(key.to_string());
            }
        }
    }

    // Phase 2: POST a new key with exponential backoff.
    for attempt in 0..max_retries {
        let resp = http
            .post(format!("{management_url}/api/api-keys"))
            .header("Authorization", &auth)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 201) {
            let v: serde_json::Value = resp.json().await?;
            let key = v
                .get("key")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("api_key").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string();
            if !key.is_empty() {
                return Ok(key);
            }
        }
        if attempt + 1 < max_retries {
            // 2^attempt seconds — matches `management_api.py:152`.
            sleep(Duration::from_secs(1u64 << attempt)).await;
        }
    }
    Err(CloudError::ApiKeyCreation(max_retries))
}

/// Convenience wrapper that binds a base URL + access token to a set of
/// idempotent helpers: `ensure_tenant`, `ensure_api_key`, `list_tenants`,
/// `get_tenant`. The low-level free functions above are still the
/// primary port surface; this struct just packages them for call sites
/// that want a "client" handle.
#[derive(Debug, Clone)]
pub struct ManagementApiClient {
    http: reqwest::Client,
    base_url: String,
    access_token: String,
}

impl ManagementApiClient {
    /// Construct a new client.
    ///
    /// `base_url` defaults to [`cloud_url()`](crate::config::cloud_url)
    /// when `None`. A trailing `/` is stripped so URLs concatenate
    /// cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`CloudError::Http`] if the underlying `reqwest::Client`
    /// builder fails (shouldn't happen with our default config).
    pub fn new(base_url: Option<String>, access_token: String) -> CloudResult<Self> {
        let http = build_http_client()?;
        let base = resolve_management_url(base_url.as_deref());
        Ok(Self {
            http,
            base_url: base,
            access_token,
        })
    }

    /// Base management URL the client is bound to (trailing-slash
    /// stripped).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth(&self) -> String {
        auth_header(&self.access_token)
    }

    /// Idempotently ensure a tenant exists for `email`.
    ///
    /// Implementation: first calls [`get_current_tenant`]; if that
    /// returns `None`, calls [`create_tenant`] with the default polling
    /// parameters. The returned [`TenantInfo`] always has a non-empty
    /// `id` on success.
    pub async fn ensure_tenant(&self, email: &str) -> CloudResult<TenantInfo> {
        if let Some(t) = get_current_tenant(&self.base_url, &self.access_token).await?
            && !t.id.is_empty()
        {
            return Ok(t);
        }
        create_tenant(
            &self.base_url,
            &self.access_token,
            email,
            DEFAULT_TENANT_POLL_TIMEOUT,
            DEFAULT_TENANT_POLL_INTERVAL,
        )
        .await
    }

    /// Idempotently return an API key — re-uses the first existing key
    /// if present, otherwise creates one with retries. The `tenant_id`
    /// argument is currently only used for instrumentation; the
    /// management API binds API keys to the token's tenant context.
    pub async fn ensure_api_key(&self, tenant_id: Uuid) -> CloudResult<String> {
        tracing::debug!(
            target: "cognee_cloud::management_api",
            %tenant_id,
            "ensure_api_key",
        );
        get_or_create_api_key(
            &self.base_url,
            &self.access_token,
            DEFAULT_API_KEY_MAX_RETRIES,
        )
        .await
    }

    /// `GET /api/tenants` — list all tenants visible to the current
    /// access token.
    pub async fn list_tenants(&self) -> CloudResult<Vec<TenantInfo>> {
        let resp = self
            .http
            .get(format!("{}/api/tenants", self.base_url))
            .header("Authorization", self.auth())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::ManagementApi {
                status: status.as_u16(),
                body,
            });
        }
        let tenants: Vec<TenantInfo> = resp.json().await?;
        Ok(tenants)
    }

    /// `GET /api/tenants/{id}` — returns `Ok(None)` on 404, error on
    /// other non-success statuses.
    pub async fn get_tenant(&self, tenant_id: Uuid) -> CloudResult<Option<TenantInfo>> {
        let resp = self
            .http
            .get(format!("{}/api/tenants/{}", self.base_url, tenant_id))
            .header("Authorization", self.auth())
            .send()
            .await?;
        match resp.status().as_u16() {
            404 => Ok(None),
            200 => {
                let tenant: TenantInfo = resp.json().await?;
                Ok(Some(tenant))
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(CloudError::ManagementApi { status, body })
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Expected UUID5 for `test@example.com` under NAMESPACE_URL —
    /// precomputed via `python3 -c "import uuid; print(uuid.uuid5(
    /// uuid.NAMESPACE_URL, 'test@example.com'))"`. Guards us against
    /// any accidental UUID4/SHA1/namespace drift that would silently
    /// break Python compatibility.
    const EXPECTED_UUID5_TEST_EXAMPLE: &str = "5081778e-c036-5085-8adb-6f1892daaa73";

    #[test]
    fn uuid5_tenant_name_matches_python() {
        let name = email_to_tenant_name("test@example.com");
        assert_eq!(
            name,
            format!("tenant-{}", EXPECTED_UUID5_TEST_EXAMPLE),
            "uuid5(NAMESPACE_URL, 'test@example.com') must match Python exactly",
        );

        // Also verify the raw UUID (no prefix) matches so callers who
        // want the bare UUID don't have to string-slice.
        let bare = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"test@example.com");
        assert_eq!(bare.to_string(), EXPECTED_UUID5_TEST_EXAMPLE);
    }

    #[test]
    fn email_to_tenant_name_is_deterministic() {
        let a = email_to_tenant_name("user@example.com");
        let b = email_to_tenant_name("user@example.com");
        assert_eq!(a, b);
    }

    #[test]
    fn email_to_tenant_name_differs_per_email() {
        let a = email_to_tenant_name("alice@example.com");
        let b = email_to_tenant_name("bob@example.com");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn get_current_tenant_happy_path() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/tenants/current")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "11111111-2222-3333-4444-555555555555",
                    "name": "tenant-abc",
                    "created_at": "2026-01-01T00:00:00Z"
                }"#,
            )
            .create_async()
            .await;

        let tenant = get_current_tenant(&server.url(), "test-token")
            .await
            .expect("call succeeds")
            .expect("tenant should be present on 200");
        mock.assert_async().await;

        assert_eq!(tenant.id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(tenant.name, "tenant-abc");
        assert_eq!(tenant.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn get_current_tenant_404_returns_none() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current")
            .with_status(404)
            .with_body("")
            .create_async()
            .await;

        let out = get_current_tenant(&server.url(), "token")
            .await
            .expect("404 should be Ok(None)");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn get_current_tenant_surfaces_5xx() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current")
            .with_status(500)
            .with_body("boom")
            .create_async()
            .await;

        let err = get_current_tenant(&server.url(), "token")
            .await
            .expect_err("5xx must error");
        match err {
            CloudError::ManagementApi { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tenant_id_accepts_numeric() {
        // Exercise the custom `de_id_to_string` deserializer on a
        // number-typed `id`. The management API has returned both
        // shapes historically.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 42, "name": "numeric"}"#)
            .create_async()
            .await;

        let tenant = get_current_tenant(&server.url(), "token")
            .await
            .expect("call succeeds")
            .expect("tenant present");
        assert_eq!(tenant.id, "42");
        assert_eq!(tenant.name, "numeric");
    }

    #[tokio::test]
    async fn ensure_tenant_reuses_existing() {
        let mut server = mockito::Server::new_async().await;

        let current_mock = server
            .mock("GET", "/api/tenants/current")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "deadbeef-dead-beef-dead-beefdeadbeef",
                    "name": "existing"
                }"#,
            )
            .expect(1)
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "token".into()).expect("build client");
        let tenant = client
            .ensure_tenant("user@example.com")
            .await
            .expect("existing tenant returned");

        current_mock.assert_async().await;
        assert_eq!(tenant.id, "deadbeef-dead-beef-dead-beefdeadbeef");
        assert_eq!(tenant.name, "existing");
    }

    #[tokio::test]
    async fn ensure_api_key_returns_first_existing_key() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/api-keys")
            .match_header("authorization", "Bearer abc")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"key": "ck_live_1234"}]"#)
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "abc".into()).expect("build client");
        let key = client
            .ensure_api_key(Uuid::nil())
            .await
            .expect("existing key returned");
        mock.assert_async().await;
        assert_eq!(key, "ck_live_1234");
    }

    #[tokio::test]
    async fn ensure_api_key_creates_when_none_exists() {
        let mut server = mockito::Server::new_async().await;

        let list_mock = server
            .mock("GET", "/api/api-keys")
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;

        let create_mock = server
            .mock("POST", "/api/api-keys")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"api_key": "ck_live_created"}"#)
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "tok".into()).expect("build client");
        let key = client
            .ensure_api_key(Uuid::nil())
            .await
            .expect("new key created");

        list_mock.assert_async().await;
        create_mock.assert_async().await;
        assert_eq!(key, "ck_live_created");
    }

    #[tokio::test]
    async fn list_tenants_parses_array() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[
                    {"id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "name": "t1"},
                    {"id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "name": "t2"}
                ]"#,
            )
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "tok".into()).expect("build client");
        let tenants = client.list_tenants().await.expect("list tenants");
        assert_eq!(tenants.len(), 2);
        assert_eq!(tenants[0].name, "t1");
        assert_eq!(tenants[1].name, "t2");
    }

    #[tokio::test]
    async fn get_tenant_404_returns_none() {
        let mut server = mockito::Server::new_async().await;
        let tenant_id =
            Uuid::parse_str("99999999-9999-9999-9999-999999999999").expect("parse uuid");
        let _m = server
            .mock("GET", &*format!("/api/tenants/{tenant_id}"))
            .with_status(404)
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "tok".into()).expect("build client");
        let out = client.get_tenant(tenant_id).await.expect("404 → Ok(None)");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn get_tenant_happy_path() {
        let mut server = mockito::Server::new_async().await;
        let tenant_id =
            Uuid::parse_str("12345678-1234-1234-1234-123456789abc").expect("parse uuid");
        let _m = server
            .mock("GET", &*format!("/api/tenants/{tenant_id}"))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"id": "{tenant_id}", "name": "found"}}"#))
            .create_async()
            .await;

        let client =
            ManagementApiClient::new(Some(server.url()), "tok".into()).expect("build client");
        let tenant = client
            .get_tenant(tenant_id)
            .await
            .expect("call ok")
            .expect("200 → Some");
        assert_eq!(tenant.id, tenant_id.to_string());
        assert_eq!(tenant.name, "found");
    }

    #[tokio::test]
    async fn get_service_url_trims_trailing_slashes() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current/service-url")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"service_url": "https://tenant.example.com/////"}"#)
            .create_async()
            .await;

        let url = get_service_url(&server.url(), "tok")
            .await
            .expect("call succeeds");
        assert_eq!(url, "https://tenant.example.com");
    }

    #[tokio::test]
    async fn get_service_url_accepts_legacy_url_field() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current/service-url")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"url": "https://old.example.com"}"#)
            .create_async()
            .await;

        let url = get_service_url(&server.url(), "tok")
            .await
            .expect("call succeeds");
        assert_eq!(url, "https://old.example.com");
    }

    #[tokio::test]
    async fn get_service_url_empty_returns_error() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/api/tenants/current/service-url")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"service_url": ""}"#)
            .create_async()
            .await;

        let err = get_service_url(&server.url(), "tok")
            .await
            .expect_err("empty URL must error");
        assert!(matches!(err, CloudError::EmptyServiceUrl));
    }

    #[test]
    fn resolve_management_url_defaults_to_env_when_none() {
        let url = resolve_management_url(None);
        assert!(!url.is_empty());
    }

    #[test]
    fn resolve_management_url_strips_trailing_slash_from_override() {
        let url = resolve_management_url(Some("https://example.com///"));
        assert_eq!(url, "https://example.com");
    }
}
