//! Cloud integration for the Cognee Rust SDK.
//!
//! Ports `cognee/api/v1/serve/` from the Python SDK so that the Rust
//! implementation of `serve()` / `disconnect()` stays behavior- and
//! on-disk-format-compatible with the Python reference. Both SDKs can
//! share `~/.cognee/cloud_credentials.json`.
//!
//! This commit (C2 of 5) extends C1's foundation ([`error`], [`config`],
//! [`credentials`]) with:
//! - [`device_auth`] — OAuth2 device-code flow (RFC 8628) plus helpers
//!   to decode JWT payloads and extract the `email` claim.
//! - [`management_api`] — tenant / API-key / service-URL client for the
//!   Cognee Cloud management API.
//!
//! The HTTP proxy ([`cloud_client`] land in C3) and the public
//! `serve()` / `disconnect()` entry points (C4) are still to come.

pub mod config;
pub mod credentials;
pub mod device_auth;
pub mod error;
pub mod management_api;

pub use credentials::CloudCredentials;
pub use device_auth::{
    DeviceCodeResponse, TokenResponse, decode_jwt_payload, device_code_login,
    extract_email_from_id_token, initiate_device_authorization, poll_for_token,
    refresh_access_token,
};
pub use error::{CloudError, CloudResult};
pub use management_api::{
    ManagementApiClient, TenantInfo, create_tenant, email_to_tenant_name, get_current_tenant,
    get_or_create_api_key, get_service_url,
};
