//! SeaORM entity for the `session_model_usage` table (LIB-03).
//!
//! Per-`(session_id, user_id, model)` token + cost aggregate, populated by
//! `accumulate_usage` (LIB-05) when an LLM call fires inside a tracked
//! session scope. Normalizing this out of `session_records` lets
//! mixed-model sessions (e.g. embedding calls + completion calls on
//! different models) attribute cost correctly via
//! `GET /api/v1/sessions/cost-by-model`.
//!
//! Mirrors Python's `SessionModelUsage` SQLAlchemy model at
//! `cognee/modules/session_lifecycle/models.py:89-126`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "session_model_usage")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    /// Owning user id, hex-encoded UUID (see `session_record.rs`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model: String,

    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cost_usd: f64,

    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Serialize to a JSON object whose key ordering matches Python's
    /// [`SessionModelUsage.to_dict()`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L116-L126).
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.session_id,
            "user_id": self.user_id,
            "model": self.model,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cost_usd": self.cost_usd,
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}
