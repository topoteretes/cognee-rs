use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Dataset {
    /// Create a new Dataset.
    ///
    /// `id` must be passed by the caller (use `generate_dataset_id` for Python-compatible
    /// deterministic IDs, or `Uuid::new_v4()` for a random ID).
    pub fn new(name: String, owner_id: Uuid, tenant_id: Option<Uuid>, id: Uuid) -> Self {
        Self {
            id,
            name,
            owner_id,
            tenant_id,
            created_at: Utc::now(),
            updated_at: None,
        }
    }
}
