//! Test utilities for cognee-rust crates.
//!
//! Re-exports mock implementations and provides helpers for constructing
//! [`TaskContext`] in tests without requiring real database backends.

use std::sync::Arc;

use cognee_core::{CancellationHandle, RayonThreadPool, TaskContext, TaskContextBuilder};
use cognee_database::DatabaseConnection;

pub use cognee_graph::MockGraphDB;
pub use cognee_storage::MockStorage;
pub use cognee_vector::MockVectorDB;

/// Build a [`TaskContext`] with all-mock backends and an in-memory SQLite database.
///
/// Returns `(CancellationHandle, Arc<TaskContext>, Arc<DatabaseConnection>)`.
/// The `DatabaseConnection` is exposed so callers can perform direct DB queries
/// in assertions.
pub async fn test_task_context() -> (
    CancellationHandle,
    Arc<TaskContext>,
    Arc<DatabaseConnection>,
) {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let db = Arc::new(db);

    let (handle, ctx) = TaskContextBuilder::new()
        .thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
        .database(db.clone())
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .build()
        .unwrap();

    (handle, Arc::new(ctx), db)
}
