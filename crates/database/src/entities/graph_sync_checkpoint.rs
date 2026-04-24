//! Graph sync checkpoint entity for Stage 4 of `improve()`.
//!
//! Stores high-water mark timestamps per `(user_id, dataset_id, session_id)`
//! triple so that `sync_graph_to_session()` can process only new edges
//! on re-runs.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "graph_sync_checkpoints")]
pub struct Model {
    /// Checkpoint key of the form
    /// `graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// Last-synced edge `created_at` timestamp.
    pub ts: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
