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
pub use traits::{AclDb, DeleteDb, IngestDb, RoleDb, SearchHistoryDb, TenantDb, UserDb};
pub use types::{
    ArtifactReference, DatabaseError, GraphEdge, GraphMetrics, GraphNode, PipelineRun,
    PipelineRunStatus, SearchHistoryEntry, SearchHistoryEntryType, TaskRun,
};
