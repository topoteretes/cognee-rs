use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::graph_storage::RunScope;
use crate::ops::{data, graph_storage, pipeline_runs};
use crate::types::{DatabaseError, GraphEdge, GraphNode, PipelineRun};

/// The relational provenance ledger cognify writes and run rollback reads:
/// which graph nodes and edges each pipeline run produced, per data item,
/// which data items a run completed, and the dataset-level run rows the
/// pipeline cache checks.
#[async_trait]
pub trait ProvenanceDb: Send + Sync {
    /// Upsert the provenance rows for a batch of graph nodes and edges, in one
    /// transaction.
    async fn upsert_provenance_graph(
        &self,
        nodes: &[GraphNode],
        edges: &[GraphEdge],
    ) -> Result<(), DatabaseError>;

    /// Edges of `dataset_id` recorded after `since` (all when `None`), oldest
    /// first, at most `limit`.
    async fn get_edges_since(
        &self,
        dataset_id: Uuid,
        since: Option<DateTime<Utc>>,
        limit: u64,
    ) -> Result<Vec<GraphEdge>, DatabaseError>;

    /// The recorded nodes with these ids.
    async fn get_nodes_by_ids(&self, ids: &[String]) -> Result<Vec<GraphNode>, DatabaseError>;

    /// Data items the run touched within the scope.
    async fn get_data_ids_for_run(&self, scope: &RunScope<'_>) -> Result<Vec<Uuid>, DatabaseError>;

    /// Nodes only this run (within the scope) produced.
    async fn get_unique_nodes_for_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<GraphNode>, DatabaseError>;

    /// Edges only this run (within the scope) produced.
    async fn get_unique_edges_for_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<GraphEdge>, DatabaseError>;

    /// Delete the run's node provenance rows within the scope; returns the
    /// number removed.
    async fn delete_nodes_for_run(&self, scope: &RunScope<'_>) -> Result<u64, DatabaseError>;

    /// Delete the run's edge provenance rows within the scope; returns the
    /// number removed.
    async fn delete_edges_for_run(&self, scope: &RunScope<'_>) -> Result<u64, DatabaseError>;

    /// Relationship names the run used that some other run also claims.
    async fn get_relationship_names_claimed_outside_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<String>, DatabaseError>;

    /// Record the data item's token count.
    async fn update_data_token_count(
        &self,
        data_id: Uuid,
        token_count: i64,
    ) -> Result<(), DatabaseError>;

    /// Mark the data item's cognify pipeline status complete for the dataset.
    async fn mark_cognify_pipeline_status_complete(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Which of `data_ids` cognify already completed in the dataset.
    async fn get_cognify_completed_data_ids(
        &self,
        dataset_id: Uuid,
        data_ids: &[Uuid],
    ) -> Result<BTreeSet<Uuid>, DatabaseError>;

    /// Append one `pipeline_runs` row.
    async fn create_pipeline_run(&self, run: PipelineRun) -> Result<PipelineRun, DatabaseError>;

    /// The newest `pipeline_runs` row of the pipeline for the dataset.
    async fn get_latest_pipeline_run(
        &self,
        pipeline_name: &str,
        dataset_id: Uuid,
    ) -> Result<Option<PipelineRun>, DatabaseError>;
}

#[async_trait]
impl ProvenanceDb for DatabaseConnection {
    async fn upsert_provenance_graph(
        &self,
        nodes: &[GraphNode],
        edges: &[GraphEdge],
    ) -> Result<(), DatabaseError> {
        graph_storage::upsert_provenance_graph(self, nodes, edges).await
    }

    async fn get_edges_since(
        &self,
        dataset_id: Uuid,
        since: Option<DateTime<Utc>>,
        limit: u64,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        graph_storage::get_edges_since(self, dataset_id, since, limit).await
    }

    async fn get_nodes_by_ids(&self, ids: &[String]) -> Result<Vec<GraphNode>, DatabaseError> {
        graph_storage::get_nodes_by_ids(self, ids).await
    }

    async fn get_data_ids_for_run(&self, scope: &RunScope<'_>) -> Result<Vec<Uuid>, DatabaseError> {
        graph_storage::get_data_ids_for_run(self, scope).await
    }

    async fn get_unique_nodes_for_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<GraphNode>, DatabaseError> {
        graph_storage::get_unique_nodes_for_run(self, scope).await
    }

    async fn get_unique_edges_for_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        graph_storage::get_unique_edges_for_run(self, scope).await
    }

    async fn delete_nodes_for_run(&self, scope: &RunScope<'_>) -> Result<u64, DatabaseError> {
        graph_storage::delete_nodes_for_run(self, scope).await
    }

    async fn delete_edges_for_run(&self, scope: &RunScope<'_>) -> Result<u64, DatabaseError> {
        graph_storage::delete_edges_for_run(self, scope).await
    }

    async fn get_relationship_names_claimed_outside_run(
        &self,
        scope: &RunScope<'_>,
    ) -> Result<Vec<String>, DatabaseError> {
        graph_storage::get_relationship_names_claimed_outside_run(self, scope).await
    }

    async fn update_data_token_count(
        &self,
        data_id: Uuid,
        token_count: i64,
    ) -> Result<(), DatabaseError> {
        data::update_data_token_count(self, data_id, token_count).await
    }

    async fn mark_cognify_pipeline_status_complete(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        data::mark_cognify_pipeline_status_complete(self, data_id, dataset_id).await
    }

    async fn get_cognify_completed_data_ids(
        &self,
        dataset_id: Uuid,
        data_ids: &[Uuid],
    ) -> Result<BTreeSet<Uuid>, DatabaseError> {
        data::get_cognify_completed_data_ids(self, dataset_id, data_ids).await
    }

    async fn create_pipeline_run(&self, run: PipelineRun) -> Result<PipelineRun, DatabaseError> {
        pipeline_runs::create_pipeline_run(self, run).await
    }

    async fn get_latest_pipeline_run(
        &self,
        pipeline_name: &str,
        dataset_id: Uuid,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        pipeline_runs::get_latest_pipeline_run(self, pipeline_name, dataset_id).await
    }
}
