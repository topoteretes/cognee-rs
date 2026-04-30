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
pub use ops::checkpoint::{CheckpointStore, SeaOrmCheckpointStore};
pub use pipelines::sea_orm_impl::SeaOrmPipelineRunRepository;
pub use pipelines::{PipelineRunRepository, PipelineRunWithAttributionRow};
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
