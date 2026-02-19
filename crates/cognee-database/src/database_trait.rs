use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_models::{Data, Dataset};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Error)]
pub enum DatabaseError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Unique violation: {0}")]
    UniqueViolation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchHistoryEntryType {
    Query,
    Result,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub entry_id: Uuid,
    pub query_id: Uuid,
    pub entry_type: SearchHistoryEntryType,
    pub content: String,
    pub query_type: Option<String>,
    pub user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait DatabaseTrait: Send + Sync {
    // Data operations
    async fn create_data(&self, data: Data) -> Result<Data, DatabaseError>;
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError>;
    async fn update_data(&self, data: Data) -> Result<Data, DatabaseError>;
    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError>;

    // Dataset operations
    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError>;
    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError>;
    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
    ) -> Result<Option<Dataset>, DatabaseError>;
    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError>;

    // Search persistence operations
    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError>;

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError>;

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError>;

    // Initialize schema
    async fn initialize(&self) -> Result<(), DatabaseError>;
}
