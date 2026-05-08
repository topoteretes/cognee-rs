//! Checkpoint store abstraction for Stage 4 of `improve()`.
//!
//! Provides a generic key/timestamp storage interface used by
//! `sync_graph_to_session` to track the high-water mark of edges that have
//! already been merged into a session's graph context.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tracing::{Span, instrument};

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::graph_sync_checkpoint;
use crate::types::DatabaseError;

/// Abstraction over persistent timestamp checkpoints keyed by string.
///
/// Analogous to Python's cache-engine interface used for
/// `graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}` keys.
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Read the timestamp stored under `key`, or `None` if missing.
    async fn load(&self, key: &str) -> Result<Option<DateTime<Utc>>, DatabaseError>;

    /// Write `ts` under `key`, overwriting any previous value.
    async fn save(&self, key: &str, ts: DateTime<Utc>) -> Result<(), DatabaseError>;
}

/// Load the checkpoint timestamp for a key from the `graph_sync_checkpoints`
/// table, or `None` if the key does not exist.
#[instrument(
    name = "cognee.db.relational.checkpoint.load_checkpoint",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn load_checkpoint(
    db: &DatabaseConnection,
    key: &str,
) -> Result<Option<DateTime<Utc>>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let row = graph_sync_checkpoint::Entity::find()
        .filter(graph_sync_checkpoint::Column::Key.eq(key))
        .one(db)
        .await
        .map_err(map_sea_err)?;
    let result = row.map(|m| m.ts);
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}

/// Persist `ts` under `key` in the `graph_sync_checkpoints` table. Inserts
/// a new row or updates the existing one (upsert on the primary key).
#[instrument(
    name = "cognee.db.relational.checkpoint.save_checkpoint",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn save_checkpoint(
    db: &DatabaseConnection,
    key: &str,
    ts: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let model = graph_sync_checkpoint::ActiveModel {
        key: ActiveValue::Set(key.to_string()),
        ts: ActiveValue::Set(ts),
    };
    graph_sync_checkpoint::Entity::insert(model)
        .on_conflict(
            OnConflict::column(graph_sync_checkpoint::Column::Key)
                .update_column(graph_sync_checkpoint::Column::Ts)
                .to_owned(),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// SeaORM-backed implementation of [`CheckpointStore`] that writes to the
/// `graph_sync_checkpoints` table.
pub struct SeaOrmCheckpointStore {
    db: std::sync::Arc<DatabaseConnection>,
}

impl SeaOrmCheckpointStore {
    pub fn new(db: std::sync::Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl CheckpointStore for SeaOrmCheckpointStore {
    async fn load(&self, key: &str) -> Result<Option<DateTime<Utc>>, DatabaseError> {
        load_checkpoint(self.db.as_ref(), key).await
    }

    async fn save(&self, key: &str, ts: DateTime<Utc>) -> Result<(), DatabaseError> {
        save_checkpoint(self.db.as_ref(), key, ts).await
    }
}
