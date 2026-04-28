//! `sync_operations` repository surface.
//!
//! Mirrors Python's [`cognee/modules/sync/methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/sync/methods)
//! 1:1: create / mark started / mark completed / mark failed / update progress
//! / list running / lookup by run_id.

pub mod repository;
pub mod sea_orm_impl;

pub use repository::{SyncOperationRepository, SyncOperationRow, SyncOperationStatus};
pub use sea_orm_impl::SeaOrmSyncOperationRepository;
