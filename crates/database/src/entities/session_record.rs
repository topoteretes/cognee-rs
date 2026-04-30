//! SeaORM entity for the `session_records` table (LIB-03).
//!
//! Mirrors Python's `SessionRecord` SQLAlchemy model at
//! `cognee/modules/session_lifecycle/models.py:10-86` byte-for-byte:
//!
//! - Composite primary key on `(session_id, user_id)`.
//! - `status` defaults to `"running"`. The `"abandoned"` value is **never**
//!   written to the row — it is inferred at read time by LIB-05's
//!   `effective_status` SQL expression based on `last_activity_at`.
//! - Aggregate counters (`tokens_in`, `tokens_out`, `cost_usd`,
//!   `error_count`) accumulate via `LLMGateway` / `SessionManager` hooks.
//!
//! UUIDs (`user_id`, `dataset_id`) are persisted as 32-char hex strings to
//! match the rest of the schema (see `uuid_hex.rs`); LIB-05's trait
//! converts `uuid::Uuid` ↔ `String` at the repository boundary.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "session_records")]
pub struct Model {
    /// Caller-provided session id (e.g. `"cc_myproj_ab12cd34ef56"`).
    /// Scoped per user — same string from two different users is two
    /// different sessions.
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    /// Owning user id, hex-encoded UUID.
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: String,

    /// Optional dataset scope (hex UUID); used by ACL filtering when
    /// listing/aggregating.
    pub dataset_id: Option<String>,

    /// Stored status. `"abandoned"` is inferred at read time, never
    /// written (see LIB-05).
    pub status: String,

    pub started_at: DateTimeUtc,
    pub last_activity_at: DateTimeUtc,
    pub ended_at: Option<DateTimeUtc>,

    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cost_usd: f64,

    pub error_count: i32,

    pub last_model: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Serialize to a JSON object whose key ordering matches Python's
    /// [`SessionRecord.to_dict()`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L68-L86).
    ///
    /// Used by LIB-05's repository / E-09's HTTP DTO. Field order is
    /// load-bearing: clients (e.g. the dashboard) snapshot the response
    /// shape in test fixtures.
    pub fn to_dict(&self) -> serde_json::Value {
        // Insertion order matters for Python parity: callers (E-09's HTTP
        // DTO + dashboard test snapshots) compare the rendered JSON
        // shape literally. The `cognee-database` crate enables
        // `serde_json/preserve_order` so `json!` emits keys in the
        // order they appear here.
        serde_json::json!({
            "session_id": self.session_id,
            "user_id": self.user_id,
            "dataset_id": self.dataset_id,
            "status": self.status,
            "started_at": self.started_at.to_rfc3339(),
            "last_activity_at": self.last_activity_at.to_rfc3339(),
            "ended_at": self.ended_at.map(|t| t.to_rfc3339()),
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cost_usd": self.cost_usd,
            "error_count": self.error_count,
            "last_model": self.last_model,
        })
    }
}
