//! Cloud connect / disconnect — thin re-export layer over [`cognee_cloud`].
//!
//! This module is compiled only when the `cloud` feature is enabled. It
//! exposes the [`serve`] / [`serve_url`] / [`serve_cloud`] / [`disconnect`]
//! entry points so consumers can write
//! `use cognee::{serve, disconnect, ServeConfig};` without reaching
//! into the `cognee_cloud` crate directly.
//!
//! The orchestration logic lives in `cognee_cloud::serve` — this file
//! is just a public re-export surface. CLI subcommand wiring is part of
//! commit C5.

pub use cognee_cloud::{
    CloudClient, CloudCredentials, CloudError, CloudResult, ServeConfig, disconnect, serve,
    serve_cloud, serve_url,
};
