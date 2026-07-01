//! Relational metadata persistence (SeaORM/SQLite) for ingestion, search history, and deletion.

mod connection;
mod conversions;
pub mod entities;
pub mod migrator;
pub mod ops;
pub mod pipelines;
pub mod sync;
mod traits;
mod types;
pub mod uuid_hex;

// Re-export tutorial seeder for use by cognee-http-server (which can't depend on cognee-lib).
pub use ops::tutorial_seeder::{
    TUTORIAL_BASICS_ID, TUTORIAL_PYTHON_DEV_ID, seed_tutorials_if_first_call,
};

pub use connection::{PoolConfig, connect, connect_with_pool, initialize};

/// Map the active SeaORM backend to a `cognee.db.system` string
/// matching the values used by the vector / graph adapters.
///
/// The tag values mirror Python's observability layer
/// (`cognee/modules/observability/tracing.py`) and the
/// `cognee.db.system` attribute exposed by every relational op span.
pub fn database_system_label<C: sea_orm::ConnectionTrait>(db: &C) -> &'static str {
    use sea_orm::DatabaseBackend;
    match db.get_database_backend() {
        DatabaseBackend::Sqlite => "sqlite",
        DatabaseBackend::Postgres => "postgres",
        DatabaseBackend::MySql => "mysql",
    }
}
pub use ops::checkpoint::{CheckpointStore, SeaOrmCheckpointStore};
pub use pipelines::sea_orm_impl::SeaOrmPipelineRunRepository;
pub use pipelines::{
    NoopPipelineRunRepository, PipelineRunRepository, PipelineRunWithAttributionRow,
};
pub use sea_orm::{DatabaseConnection, TransactionTrait};
pub use sync::{
    SeaOrmSyncOperationRepository, SyncOperationRepository, SyncOperationRow, SyncOperationStatus,
};
pub use traits::{
    AclDb, CostByModelRow, DatasetConfigDb, DatasetConfiguration, DatasetConfigurationPatch,
    DeleteDb, IngestDb, Notebook, NotebookDb, NotebookUpdatePatch, SearchHistoryDb,
    SessionLifecycleDb, SessionListFilters, SessionListPage, SessionRowWithStatus, SessionStats,
};
pub use types::{
    DatabaseError, GraphEdge, GraphMetrics, GraphNode, PipelineRun, PipelineRunStatus,
    SearchHistoryEntry, SearchHistoryEntryType, TaskRun,
};

// The `auth`, `permissions`, `UserDb`/`RoleDb`/`TenantDb`,
// `SeaOrmUserAuthRepository`, `SeaOrmApiKeyRepository`, `ApiKey`, `AuthUser`,
// `CreateUserPayload`, `UpdateUserPayload`, `ActiveUserWithApiKeyCount` items
// moved to the closed `cognee-access-control` crate
//. The `types` module deliberately remains private —
// closed callers reach `DatabaseError` via the top-level re-export above.
