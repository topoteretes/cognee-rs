use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;

use crate::ops::checkpoint::{CheckpointStore, SeaOrmCheckpointStore};
use crate::pipelines::{PipelineRunRepository, SeaOrmPipelineRunRepository};
use crate::sync::{SeaOrmSyncOperationRepository, SyncOperationRepository};
use crate::traits::{
    DatasetConfigDb, DeleteDb, IngestDb, NotebookDb, ProvenanceDb, SearchHistoryDb,
    SessionLifecycleDb, SessionQaDb,
};
use crate::types::DatabaseError;

/// Combined trait for dataset operations: a dataset facade needs both the
/// ingest and the delete side. Implemented for every type that has both.
pub trait DatasetDb: IngestDb + DeleteDb + Send + Sync {}
impl<T: IngestDb + DeleteDb + Send + Sync> DatasetDb for T {}

/// The relational database as one object: every domain trait cognee uses,
/// the repositories built over it, and its lifecycle.
///
/// Components hold an `Arc<dyn RelationalDb>` and upcast it to the narrower
/// trait they need (`Arc<dyn IngestDb>`, …). The built-in implementation is
/// SeaORM's [`DatabaseConnection`] (SQLite or Postgres); another backend
/// implements this trait and is registered as a relational factory.
///
/// Several domain traits declare a method of the same name (for example
/// `get_dataset_by_name` on both [`IngestDb`] and [`DeleteDb`]); call those
/// through the trait (`IngestDb::get_dataset_by_name(&*db, …)`).
///
/// [`AclDb`](crate::AclDb) is not part of it: no OSS backend implements it, a
/// downstream ACL layer owns it.
#[async_trait]
pub trait RelationalDb:
    DatasetDb
    + IngestDb
    + DeleteDb
    + SearchHistoryDb
    + NotebookDb
    + DatasetConfigDb
    + SessionLifecycleDb
    + SessionQaDb
    + ProvenanceDb
    + Send
    + Sync
{
    /// The pipeline-run repository over this database. Takes the `Arc` so the
    /// repository shares it (the owner sees every holder in its strong count).
    fn pipeline_runs(self: Arc<Self>) -> Arc<dyn PipelineRunRepository>;

    /// The cognify checkpoint store over this database.
    fn checkpoints(self: Arc<Self>) -> Arc<dyn CheckpointStore>;

    /// The sync-operation repository over this database.
    fn sync_operations(self: Arc<Self>) -> Arc<dyn SyncOperationRepository>;

    /// The `cognee.db.system` label of the backend (`sqlite`, `postgres`, …).
    fn system_label(&self) -> &'static str;

    /// Check the database is reachable.
    async fn ping(&self) -> Result<(), DatabaseError>;

    /// Close the connection pool. Later calls on this database fail.
    async fn close(&self) -> Result<(), DatabaseError>;

    /// The underlying SeaORM connection, for tests that inspect the pool
    /// itself (e.g. teardown races). Test support only (feature `testing`);
    /// `None` for any backend that is not SeaORM.
    #[cfg(feature = "testing")]
    fn as_sea_orm(&self) -> Option<&DatabaseConnection> {
        None
    }
}

#[async_trait]
impl RelationalDb for DatabaseConnection {
    fn pipeline_runs(self: Arc<Self>) -> Arc<dyn PipelineRunRepository> {
        Arc::new(SeaOrmPipelineRunRepository::new(self))
    }

    fn checkpoints(self: Arc<Self>) -> Arc<dyn CheckpointStore> {
        Arc::new(SeaOrmCheckpointStore::new(self))
    }

    fn sync_operations(self: Arc<Self>) -> Arc<dyn SyncOperationRepository> {
        Arc::new(SeaOrmSyncOperationRepository::new(self))
    }

    fn system_label(&self) -> &'static str {
        crate::connection_system_label(self)
    }

    async fn ping(&self) -> Result<(), DatabaseError> {
        DatabaseConnection::ping(self)
            .await
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
    }

    async fn close(&self) -> Result<(), DatabaseError> {
        crate::connection::close(self).await
    }

    #[cfg(feature = "testing")]
    fn as_sea_orm(&self) -> Option<&DatabaseConnection> {
        Some(self)
    }
}
