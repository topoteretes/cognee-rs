use crate::models::{Data, Dataset};
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum DatabaseError {
    NotFound(String),
    ConnectionError(String),
    QueryError(String),
    UniqueViolation(String),
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseError::NotFound(msg) => write!(f, "Not found: {}", msg),
            DatabaseError::ConnectionError(msg) => write!(f, "Connection error: {}", msg),
            DatabaseError::QueryError(msg) => write!(f, "Query error: {}", msg),
            DatabaseError::UniqueViolation(msg) => write!(f, "Unique violation: {}", msg),
        }
    }
}

impl std::error::Error for DatabaseError {}

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

    // Initialize schema
    async fn initialize(&self) -> Result<(), DatabaseError>;
}
