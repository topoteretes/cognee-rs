//! Cloud integration for the Cognee Rust SDK.
//!
//! Ports `cognee/api/v1/serve/` from the Python SDK so that the Rust
//! implementation of `serve()` / `disconnect()` stays behavior- and
//! on-disk-format-compatible with the Python reference. Both SDKs can
//! share `~/.cognee/cloud_credentials.json`.
//!
//! This commit (C1 of 5) ships only the foundational pieces:
//! [`error`], [`config`], and [`credentials`]. The OAuth2 device-code
//! flow, management API client, HTTP proxy, and public `serve()` /
//! `disconnect()` entry points land in later commits.

pub mod config;
pub mod credentials;
pub mod error;

pub use credentials::CloudCredentials;
pub use error::{CloudError, CloudResult};
