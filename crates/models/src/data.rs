use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a piece of data in the system, such as a file or a text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Data {
    pub id: Uuid,
    pub name: String,
    pub raw_data_location: String,
    pub original_data_location: String,
    pub extension: String,
    pub mime_type: String,
    pub content_hash: String,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Data {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        name: String,
        raw_data_location: String,
        original_data_location: String,
        extension: String,
        mime_type: String,
        content_hash: String,
        owner_id: Uuid,
    ) -> Self {
        Self {
            id,
            name,
            raw_data_location,
            original_data_location,
            extension,
            mime_type,
            content_hash,
            owner_id,
            created_at: Utc::now(),
            updated_at: None,
        }
    }
}
