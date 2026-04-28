//! Cloud-side operations exposed to the HTTP server.
//!
//! Ports Python's [`cognee/modules/cloud/operations/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/cloud/operations)
//! tree.

use crate::config::cloud_url;
use crate::error::{CloudError, CloudResult};

/// Validate a cloud API key by POSTing to `{cloud_url}/api/api-keys/check`
/// with `X-Api-Key: <api_key>`.
///
/// On HTTP 200 → `Ok(())`.
/// On any other status → `Err(CloudError::ManagementApi { status, body })`.
/// On reqwest errors (DNS, TLS, refused) → `Err(CloudError::Http(_))`.
///
/// Mirrors Python's [`check_api_key`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py)
/// — the cloud URL comes from `COGNEE_CLOUD_URL` (env-driven, with package
/// default).
pub async fn check_api_key(api_key: &str) -> CloudResult<()> {
    if api_key.is_empty() {
        return Err(CloudError::Auth("missing API key".into()));
    }

    let url = format!("{}/api/api-keys/check", cloud_url());
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(CloudError::Http)?;

    let response = client
        .post(&url)
        .header("X-Api-Key", api_key)
        .send()
        .await
        .map_err(CloudError::Http)?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let body = response
        .text()
        .await
        .unwrap_or_else(|e| format!("<read error: {e}>"));
    Err(CloudError::ManagementApi {
        status: status.as_u16(),
        body,
    })
}
