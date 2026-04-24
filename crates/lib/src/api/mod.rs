//! High-level API functions for Cognee-Rust.
//!
//! These are the top-level convenience functions that compose the lower-level
//! pipeline primitives (add, cognify, search, delete, memify) into
//! user-friendly operations matching the Python SDK.
//!
//! - [`forget`] -- Unified deletion API
//! - [`update`] -- Data replacement (delete + re-add + re-cognify)
//! - [`prune`] -- Selective backend cleanup
//! - [`recall`] -- Smart search with session routing
//! - [`remember`] -- One-call add + cognify + optional improve
//! - [`improve`] -- Bidirectional session-graph bridge

pub mod datasets;
pub mod error;
pub mod forget;
pub mod improve;
pub mod prune;
pub mod recall;
pub mod remember;
pub mod update;
pub mod user;

pub use datasets::{DatasetDb, DatasetManager};
pub use error::{ApiError, DatasetError};
pub use forget::{DatasetRef, ForgetResult, ForgetTarget, forget};
pub use improve::{ImproveResult, improve};
pub use prune::{PruneResult, PruneTarget, prune_data, prune_system};
pub use recall::{RecallItem, RecallResult, RecallSource, recall};
pub use remember::{RememberItemInfo, RememberResult, RememberStatus, remember};
pub use update::{UpdateResult, update};
pub use user::get_or_create_default_user;
