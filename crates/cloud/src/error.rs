//! Error types for the `cognee-cloud` crate.
//!
//! Mirrors the failure modes surfaced by the Python `cognee/api/v1/serve/`
//! module tree so that higher-level code can distinguish between HTTP,
//! auth, and credential-storage problems.

use thiserror::Error;

/// All failure modes surfaced by the cloud integration layer.
///
/// Variants cover every `raise` site in the Python reference tree:
/// `device_auth.py`, `management_api.py`, `cloud_client.py`, plus the
/// credential-file IO path in `credentials.py`.
#[derive(Debug, Error)]
pub enum CloudError {
    /// Generic configuration error (e.g. malformed env-var value).
    #[error("cloud configuration error: {0}")]
    Config(String),

    /// Filesystem or network IO error when reading/writing credentials.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization error — credentials file or HTTP response body.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Underlying HTTP-client failure (connection refused, TLS, etc.).
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// Required environment variable is missing or empty.
    #[error("missing env var: {0}")]
    MissingEnv(&'static str),

    /// Generic authentication error (catch-all for token/credential problems).
    #[error("authentication error: {0}")]
    Auth(String),

    /// Error raised specifically by the OAuth2 device-code flow.
    #[error("device code flow error: {0}")]
    DeviceAuth(String),

    /// Device code flow timed out waiting for user authorization.
    #[error("device code flow timed out")]
    DeviceCodeTimeout,

    /// Device code expired before authorization completed.
    #[error("device code expired — please try again")]
    DeviceCodeExpired,

    /// User denied the authorization request.
    #[error("authorization denied by user")]
    AuthDenied,

    /// Error polling the Auth0 token endpoint.
    #[error("token polling error: {0}")]
    TokenPolling(String),

    /// Management API error — tenant provisioning, API-key creation, etc.
    #[error("management API error: {0}")]
    Management(String),

    /// Tenant did not finish provisioning within the timeout.
    #[error("tenant provisioning timed out after {0}s")]
    TenantProvisionTimeout(u64),

    /// Failed to create an API key after repeated retries.
    #[error("failed to create API key after {0} retries")]
    ApiKeyCreation(u32),

    /// Management API returned a non-success HTTP status.
    #[error("management API returned {status}: {body}")]
    ManagementApi { status: u16, body: String },

    /// Tenant record had an empty `service_url` — likely still provisioning.
    #[error("service URL empty — tenant may still be provisioning")]
    EmptyServiceUrl,

    /// The Auth0 id_token is missing the `email` claim.
    #[error("could not extract email from id_token (add 'email' to Auth0 scope)")]
    MissingEmailClaim,

    /// Local credential file exists but is malformed.
    #[error("invalid credentials file: {0}")]
    InvalidCredentials(String),

    /// Remote SDK operation proxied through the cloud failed.
    #[error("remote operation '{op}' failed ({status}): {body}")]
    RemoteOp {
        op: &'static str,
        status: u16,
        body: String,
    },

    /// Requested resource (tenant, credential, etc.) was not found.
    #[error("resource not found")]
    NotFound,

    /// Operation failed authentication (401/403 from a remote endpoint).
    #[error("unauthorized")]
    Unauthorized,

    /// Access token has expired and must be refreshed.
    #[error("credentials expired")]
    Expired,
}

/// Convenience alias for `Result<T, CloudError>`.
pub type CloudResult<T> = Result<T, CloudError>;
