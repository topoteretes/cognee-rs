//! Library seam for the `cognee-cli` binary.
//!
//! The binary (`src/main.rs`) and any downstream binaries that reuse the OSS
//! command handlers and arg structs share this single source of truth. The modules are
//! re-exported unchanged; behavior lives in the same files the binary used to
//! declare with `mod`.
//!
//! The `bench` / `visualization` feature gates are preserved exactly as they
//! were on the binary — see `commands/mod.rs` and `cli.rs`.

pub mod cli;
pub mod commands;
pub mod config_store;
pub mod error;
pub mod teardown;
