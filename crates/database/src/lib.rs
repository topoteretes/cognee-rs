pub mod auth;
mod connection;
mod conversions;
pub mod entities;
pub mod migrator;
pub mod ops;
pub mod permissions;
pub mod pipelines;
pub mod sync;
mod traits;
mod types;
pub mod uuid_hex;

// Re-export tutorial seeder for use by cognee-http-server (which can't depend on cognee-lib).
pub use ops::tutorial_seeder::{
    TUTORIAL_BASICS_ID, TUTORIAL_PYTHON_DEV_ID, seed_tutorials_if_first_call,
};

pub use auth::{
    ActiveUserWithApiKeyCount, ApiKey, ApiKeyRepository, AuthUser, CreateUserPayload,
    SeaOrmApiKeyRepository, SeaOrmUserAuthRepository, UpdateUserPayload, UserAuthRepository,
};
pub use connection::{connect, initialize};

/// Map the active SeaORM backend to a `cognee.db.system` string
/// matching the values used by the vector / graph adapters.
///
/// The tag values mirror Python's observability layer
/// (`cognee/modules/observability/tracing.py`) and the
/// `cognee.db.system` attribute exposed by every relational op span.
pub fn database_system_label(db: &sea_orm::DatabaseConnection) -> &'static str {
    use sea_orm::{ConnectionTrait, DatabaseBackend};
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
pub use sea_orm::DatabaseConnection;
pub use sync::{
    SeaOrmSyncOperationRepository, SyncOperationRepository, SyncOperationRow, SyncOperationStatus,
};
pub use traits::{
    AclDb, CostByModelRow, DeleteDb, IngestDb, Notebook, NotebookDb, NotebookUpdatePatch, RoleDb,
    SearchHistoryDb, SessionLifecycleDb, SessionListFilters, SessionListPage, SessionRowWithStatus,
    SessionStats, TenantDb, UserDb,
};
pub use types::{
    DatabaseError, GraphEdge, GraphMetrics, GraphNode, PipelineRun, PipelineRunStatus,
    SearchHistoryEntry, SearchHistoryEntryType, TaskRun,
};
