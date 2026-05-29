//! `DatasetConfigDb` trait — CRUD operations for dataset schema configuration.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::DatabaseError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetConfiguration {
    pub id: Uuid,
    pub dataset_id: Uuid,
    pub graph_schema: Option<serde_json::Value>,
    pub custom_prompt: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct DatasetConfigurationPatch {
    pub graph_schema: Option<serde_json::Value>,
    pub custom_prompt: Option<String>,
}

#[async_trait]
pub trait DatasetConfigDb: Send + Sync + 'static {
    async fn get_by_dataset_id(
        &self,
        dataset_id: Uuid,
    ) -> Result<Option<DatasetConfiguration>, DatabaseError>;

    async fn upsert(
        &self,
        dataset_id: Uuid,
        patch: DatasetConfigurationPatch,
    ) -> Result<DatasetConfiguration, DatabaseError>;
}
