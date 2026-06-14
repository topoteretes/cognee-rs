//! `cognee-bindings-common` — shared SDK facade for the Neon JS and C API
//! bindings.
//!
//! ## What lives here
//!
//! | Module | Contents |
//! |---|---|
//! | [`error`] | [`SdkError`] enum + [`SdkError::code()`] — portable, no neon/FFI imports |
//! | [`handle`] | [`HandleState`] struct — shareable inner state of the SDK handle |
//! | [`services`] | [`CogneeServices`] struct — fully-wired engine + service bundle |
//! | [`wire`] | neon-free JSON helpers: `cognify_result_json`, `marshal_inputs`, `marshal_one`, `marshal_bytes` |
//!
//! ## What does NOT live here
//!
//! This crate is the *bindings facade*, not a new user-facing Rust API (that
//! remains `cognee_lib::api`). Binding-specific types that require
//! `neon::prelude::*` (`throw_sdk_error`, `throw_config_error`, `stringify_js`,
//! `parse_js`, `js_to_serde`, `js_to_value`, `read_opts`) stay in
//! `cognee-neon`. FFI helpers (`CgSdk`, `cg_sdk_*`) stay in `cognee-capi`.

pub mod error;
pub mod handle;
pub mod ops;
pub mod redact;
pub mod services;
pub mod wire;

// Top-level re-exports for ergonomic `use cognee_bindings_common::SdkError` etc.
pub use error::SdkError;
pub use handle::HandleState;
pub use redact::redact_config_json;
pub use services::CogneeServices;
