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
//! - [`improve`] -- Staged self-improvement orchestrator

pub mod auto_improve_debounce;
pub mod datasets;
pub mod error;
pub mod forget;
pub mod improve;
pub mod notebooks;
pub mod pipeline_runs;
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
    ImproveResult, ImproveStatus, REASON_ABORTED_BY_FATAL_STAGE, REASON_BACKEND_UNSUPPORTED,
    REASON_BUDGET_EXHAUSTED, REASON_DISABLED_BY_CONFIG, REASON_NO_SESSION_IDS,
    REASON_OPT_IN_DISABLED, StageKind, StageResult, StageStatus, TRUTH_SUBSPACE_STAGE, improve,
};
pub use pipeline_runs::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};
pub use prune::{PruneResult, PruneTarget, prune_data, prune_system, prune_system_with_storage};
pub use recall::{
    RecallItem, RecallOptions, RecallResult, RecallScope, RecallSource, ScopeInput,
    normalize_scope, recall,
};
pub use remember::{
    RememberItemInfo, RememberResult, RememberStatus, SessionAutoImprove,
    launch_session_auto_improve, remember, remember_entry,
};
pub use update::{UpdateResult, update};
pub use user::get_or_create_default_user;
