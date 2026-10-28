//! Session management and QA-history storage for Cognee.
//!
//! Tracks search/answer history and feedback per session so retrieval can use
//! prior context. Trait-based so the store backend is pluggable.
//!
//! - `SessionStore` — async trait for session persistence
//! - `SessionContextEntry` / `SessionContextRow` — session-context rows
//!   (guidance entries and internal state rows), and `StateRowWatermark`,
//!   the state-row watermark the improve stages share
//! - `FsSessionStore` (feature `fs`), `RedisSessionStore`, `RelationalSessionStore`
//!   (feature `relational-store`, over any `cognee_database::SessionQaDb`)
//!   — the backing implementations

mod context_entry;
mod disabled_store;
mod error;
mod feedback;
mod improve_lock;
mod session_manager;
mod session_store;
mod types;
mod watermark;

#[cfg(feature = "fs")]
mod fs_store;

#[cfg(feature = "redis")]
mod redis_store;

#[cfg(feature = "relational-store")]
mod relational_store;

pub use context_entry::{
    AGENT_CONTEXT_SECTIONS, CONTEXT_ENTRY_KIND, CONTEXT_PROFILE_AGENT, CONTEXT_PROFILE_QA,
    FEEDBACK_ENTRY_KIND, MAX_CONTEXT_CONTENT_CHARS, QA_CONTEXT_SECTIONS, SessionContextEntry,
    SessionContextRow, coerce_active_context_entries, normalize_content, valid_sections_for,
};
pub use disabled_store::DisabledSessionStore;
pub use error::SessionError;
pub use improve_lock::{ImproveClaim, acquire_improve_lock_many, improve_lock_keys};
pub use session_manager::SessionManager;
pub use session_store::{SessionQAUpdate, SessionStore};
pub use types::{SessionContext, SessionQAEntry, SessionTraceStep, UsedGraphElementIds};
pub use watermark::{
    SESSION_DISTILL_FIELD, SESSION_DISTILL_STATE_ID_PREFIX, SESSION_DISTILL_STATE_KIND,
    SESSION_PERSIST_STATE_ID, SESSION_PERSIST_STATE_KIND, SESSION_PERSIST_WATERMARK,
    StateRowWatermark, TRACE_EXTRACTION_STATE_ID, TRACE_EXTRACTION_STATE_KIND,
    TRACE_EXTRACTION_WATERMARK, TRACE_PERSIST_STATE_ID, TRACE_PERSIST_STATE_KIND,
    TRACE_PERSIST_WATERMARK, distill_watermark, get_distilled_entry_ids, save_distilled_entry_ids,
};

#[cfg(feature = "fs")]
pub use fs_store::FsSessionStore;

#[cfg(feature = "redis")]
pub use redis_store::RedisSessionStore;

#[cfg(feature = "relational-store")]
#[allow(deprecated)]
pub use relational_store::{RelationalSessionStore, SeaOrmSessionStore};
