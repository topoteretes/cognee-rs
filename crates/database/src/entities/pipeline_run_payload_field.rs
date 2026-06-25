//! SeaORM entity for the `pipeline_run_payload_fields` table.
//!
//! Backs the DB-backed default accumulator for pipeline payload events
//! (LIB-06). Composite primary key on `(pipeline_run_id, key)` — concurrent
//! inserts with the same key upsert (last-write-wins per row); inserts with
//! different keys do not contend.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_run_payload_fields")]
pub struct Model {
    /// Random per-invocation run id (matches
    /// `cognee_core::PipelineRunInfo.run_id`) stored as a string for
    /// cross-DB portability.
    #[sea_orm(primary_key, auto_increment = false)]
    pub pipeline_run_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    #[sea_orm(column_type = "Json")]
    pub value: Json,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
