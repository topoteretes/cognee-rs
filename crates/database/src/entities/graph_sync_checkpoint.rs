//! Checkpoint entity behind [`CheckpointStore`](crate::CheckpointStore): one
//! timestamp per string key.
//!
//! The table name is historical: it was added for the removed
//! graph-to-session sync. `improve()` now keeps its stage watermarks here.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "graph_sync_checkpoints")]
pub struct Model {
    /// Checkpoint key, e.g. `improve_stage:{stage}:{dataset_id}`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// Stored timestamp.
    pub ts: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
