//! Session management and QA-history storage for Cognee.
//!
//! Tracks search/answer history and feedback per session so retrieval can use
//! prior context. Trait-based so the store backend is pluggable.
//!
//! - `SessionStore` — async trait for session persistence
//! - `FsSessionStore` (feature `fs`), `RedisSessionStore`, `RelationalSessionStore`
//!   (feature `relational-store`, over any `cognee_database::SessionQaDb`)
//!   — the backing implementations

mod disabled_store;
mod error;
mod feedback;
mod improve_lock;
mod session_manager;
mod session_store;
mod types;

#[cfg(feature = "fs")]
mod fs_store;

#[cfg(feature = "redis")]
mod redis_store;

#[cfg(feature = "relational-store")]
mod relational_store;

pub use disabled_store::DisabledSessionStore;
pub use error::SessionError;
pub use improve_lock::{ImproveLockGuard, release_improve_lock, try_acquire_improve_lock};
pub use session_manager::SessionManager;
pub use session_store::{SessionQAUpdate, SessionStore};
pub use types::{SessionContext, SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

#[cfg(feature = "fs")]
pub use fs_store::FsSessionStore;

#[cfg(feature = "redis")]
pub use redis_store::RedisSessionStore;

#[cfg(feature = "relational-store")]
#[allow(deprecated)]
pub use relational_store::{RelationalSessionStore, SeaOrmSessionStore};
