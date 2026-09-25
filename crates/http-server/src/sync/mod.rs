//! In-memory sync registry — one running cloud sync per user.
//!
//! See [`docs/http-server/routers/sync.md §3.1`](../../../../docs/http-server/routers/sync.md#31-concurrency-one-running-sync-per-user)
//! for the concurrency model.

pub mod registry;

pub use registry::{AlreadyRunning, RunningSyncSnapshot, SyncRegistry};
