use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{edge_evidence, search_history};
use crate::types::{DatabaseError, EdgeEvidenceRecord, SearchHistoryEntry};

#[async_trait]
pub trait SearchHistoryDb: Send + Sync {
    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
        dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError>;

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
        dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError>;

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError>;

    /// Active source chunks of graph edges (the `provenance_edge_evidence`
    /// sidecar), for a search's `include_references`. A search-time read that
    /// rides on the database handle a search already holds; the default has no
    /// sidecar and finds nothing.
    async fn get_edge_evidence_records(
        &self,
        _dataset_id: Uuid,
        _edge_ids: &[Uuid],
        _per_edge_limit: usize,
        _total_limit: usize,
    ) -> Result<Vec<EdgeEvidenceRecord>, DatabaseError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl SearchHistoryDb for DatabaseConnection {
    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
        dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        search_history::log_query(self, query_text, query_type, user_id, dataset_id).await
    }

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
        dataset_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        search_history::log_result(self, query_id, serialized_result, user_id, dataset_id).await
    }

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
        search_history::get_history(self, user_id, limit).await
    }

    async fn get_edge_evidence_records(
        &self,
        dataset_id: Uuid,
        edge_ids: &[Uuid],
        per_edge_limit: usize,
        total_limit: usize,
    ) -> Result<Vec<EdgeEvidenceRecord>, DatabaseError> {
        edge_evidence::get_edge_evidence_records(
            self,
            dataset_id,
            edge_ids,
            per_edge_limit,
            total_limit,
        )
        .await
    }
}
