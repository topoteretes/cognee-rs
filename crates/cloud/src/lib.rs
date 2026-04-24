//! Cloud integration for the Cognee Rust SDK.
//!
//! Ports `cognee/api/v1/serve/` from the Python SDK so that the Rust
//! implementation of `serve()` / `disconnect()` stays behavior- and
//! on-disk-format-compatible with the Python reference. Both SDKs can
//! share `~/.cognee/cloud_credentials.json`.
//!
//! # Public surface
//!
//! - [`serve`] / [`serve_url`] / [`serve_cloud`] — orchestrators that
//!   connect to a remote Cognee instance (direct or cloud mode) and
//!   install the resulting [`CloudClient`] as the process-wide remote
//!   client via [`state`].
//! - [`disconnect`] — tear down the remote routing and optionally wipe
//!   the on-disk credential cache.
//! - [`ServeConfig`] — builder describing a single `serve()` invocation.
//! - [`CloudClient`] — HTTP proxy for the V2 operations
//!   (`remember` / `recall` / `improve` / `forget`).
//! - [`CloudCredentials`] — JSON-compatible on-disk credential record.
//! - [`state`] — process-wide singleton holding the active client.
//!
//! Commits so far:
//! - C1 — [`error`], [`config`], [`credentials`].
//! - C2 — [`device_auth`] (OAuth2 device-code flow, RFC 8628, plus
//!   JWT-payload / email-claim helpers) and [`management_api`]
//!   (tenant / API-key / service-URL client).
//! - C3 — [`cloud_client`] (reqwest-based HTTP proxy for the V2
//!   operations remember / recall / improve / forget, plus health
//!   check) and [`state`] (async-safe singleton holding the remote
//!   [`CloudClient`] so `serve()` / `disconnect()` can toggle
//!   cloud-routed mode).
//! - C4 (this commit) — [`serve`] (direct + cloud mode orchestrator)
//!   and [`disconnect`] (tear-down + optional credential wipe).
//!
//! The CLI wiring + integration tests land in C5.

pub mod cloud_client;
pub mod config;
pub mod credentials;
pub mod device_auth;
pub mod disconnect;
pub mod error;
pub mod management_api;
pub mod serve;
pub mod state;

pub use cloud_client::{CloudClient, ImproveDataset, RememberData};
pub use credentials::CloudCredentials;
pub use device_auth::{
    DeviceCodeResponse, TokenResponse, decode_jwt_payload, device_code_login,
    extract_email_from_id_token, initiate_device_authorization, poll_for_token,
    refresh_access_token,
};
pub use disconnect::disconnect;
pub use error::{CloudError, CloudResult};
pub use management_api::{
    ManagementApiClient, TenantInfo, create_tenant, email_to_tenant_name, get_current_tenant,
    get_or_create_api_key, get_service_url,
};
pub use serve::{ServeConfig, serve, serve_cloud, serve_url};
pub use state::{
    clear_client, get_client, get_remote_client, is_connected, is_remote_mode, set_client,
    set_remote_client,
};
