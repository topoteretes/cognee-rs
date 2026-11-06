//! High-level API functions for Cognee-Rust.
//!
//! These are the top-level convenience functions that compose the lower-level
//! pipeline primitives (add, cognify, search, delete) into
//! user-friendly operations matching the Python SDK.
//!
//! - [`forget`] -- Unified deletion API
//! - [`update`] -- Data replacement (delete + re-add + re-cognify)
//! - [`prune`] -- Selective backend cleanup
//! - [`recall`] -- Smart search with session routing
//! - [`remember`] -- One-call add + cognify + optional improve
//! - [`improve`] -- Staged self-improvement orchestrator

pub mod auto_improve_debounce;
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
pub use improve::{
    DEFAULT_FEEDBACK_ALPHA, DEFAULT_STAGES, ImproveConfig, ImproveConfigError, ImproveParams,
    ImproveResult, ImproveStatus, OwnedImproveParams, REASON_ABORTED_BY_FATAL_STAGE,
    REASON_BACKEND_UNSUPPORTED, REASON_BUDGET_EXHAUSTED, REASON_DISABLED_BY_CONFIG,
    REASON_NO_SESSION_IDS, REASON_OPT_IN_DISABLED, StageKind, StageResult, StageStatus,
    TRUTH_SUBSPACE_STAGE, improve, improve_in_background, improve_in_background_then,
};
pub use prune::{PruneResult, PruneTarget, prune_data, prune_system, prune_system_with_storage};
pub use recall::{
    RecallItem, RecallOptions, RecallResult, RecallScope, RecallSource, ScopeInput,
    normalize_scope, recall,
};
pub use remember::{
    AutoImproveAdmission, AutoImproveContext, RememberItemInfo, RememberResult, RememberStatus,
    SessionAutoImprove, SessionAutoImproveLaunch, auto_improve_skip_reason,
    clear_auto_improve_admission, launch_session_auto_improve, register_auto_improve_admission,
    remember, remember_entry,
};
pub use update::{UpdateResult, update};
pub use user::get_or_create_default_user;
