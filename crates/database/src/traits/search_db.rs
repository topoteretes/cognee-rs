use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::search_history;
use crate::types::{DatabaseError, SearchHistoryEntry};

#[async_trait]
pub trait SearchHistoryDb: Send + Sync {
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
}

#[async_trait]
impl SearchHistoryDb for DatabaseConnection {
    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        search_history::log_query(self, query_text, query_type, user_id).await
    }

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        search_history::log_result(self, query_id, serialized_result, user_id).await
    }

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
        search_history::get_history(self, user_id, limit).await
    }
}
