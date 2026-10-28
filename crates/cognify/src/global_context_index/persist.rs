//! Writing and deleting index nodes and edges (Python's `persist.py`).

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::Utc;
use cognee_database::{GraphNode, ProvenanceDb, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, GraphDBTraitExt};
use cognee_models::GlobalContextSummary;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

use super::index_data_item_id;
use crate::error::CognifyError;

/// The stores one index build reads and writes, and who it writes as.
pub struct IndexStore<'a> {
    pub graph_db: &'a dyn GraphDBTrait,
    pub vector_db: &'a dyn VectorDB,
    pub embedding_engine: &'a dyn EmbeddingEngine,
    /// Reads the dataset's data items and records the index's ownership rows.
    pub db: &'a dyn RelationalDb,
    pub dataset_id: Uuid,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
}

fn vector_error(e: impl std::fmt::Display) -> CognifyError {
    CognifyError::VectorDBError(e.to_string())
}

impl IndexStore<'_> {
    /// The dataset id as the index states it (Python's `str(dataset.id)`).
    pub fn dataset_key(&self) -> String {
        self.dataset_id.to_string()
    }

    /// Delete index nodes and their vectors (`delete_context_index_nodes`).
    /// An id that is not a UUID, or a missing collection, has no vector.
    pub async fn delete_nodes(&self, node_ids: &[String]) -> Result<(), CognifyError> {
        if node_ids.is_empty() {
            return Ok(());
        }
        self.graph_db.delete_nodes(node_ids).await?;
        let point_ids: Vec<Uuid> = node_ids
            .iter()
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect();
        let (data_type, field) = collection();
        if point_ids.is_empty()
            || !self
                .vector_db
                .has_collection(data_type, field)
                .await
                .map_err(vector_error)?
        {
            return Ok(());
        }
        self.vector_db
            .delete_points(data_type, field, &point_ids)
            .await
            .map_err(vector_error)
    }

    /// Store summaries as graph nodes and embed their text
    /// (`persist_context_summaries`, Python's `add_data_points`).
    ///
    /// The ownership rows go first, keyed to the dataset's index data item,
    /// so deleting the dataset removes the nodes and their vectors.
    pub async fn persist_summaries(
        &self,
        summaries: &[GlobalContextSummary],
    ) -> Result<(), CognifyError> {
        if summaries.is_empty() {
            return Ok(());
        }
        let data_id = index_data_item_id(self.dataset_id);
        let rows: Vec<GraphNode> = summaries
            .iter()
            .map(|summary| GraphNode {
                id: crate::tasks::provenance_node_id(
                    self.tenant_id,
                    self.owner_id,
                    self.dataset_id,
                    data_id,
                    summary.base.id,
                ),
                slug: summary.base.id,
                user_id: self.owner_id,
                data_id,
                dataset_id: self.dataset_id,
                pipeline_run_id: None,
                label: Some(summary.base.id.to_string()),
                node_type: GlobalContextSummary::TYPE.to_string(),
                indexed_fields: json!(GlobalContextSummary::INDEX_FIELDS),
                attributes: serde_json::to_value(summary).ok(),
                created_at: Utc::now(),
            })
            .collect();
        ProvenanceDb::upsert_provenance_graph(self.db, &rows, &[])
            .await
            .map_err(|e| CognifyError::DatabaseError(e.to_string()))?;

        let refs: Vec<&GlobalContextSummary> = summaries.iter().collect();
        self.graph_db.add_nodes(&refs).await?;

        let (data_type, field) = collection();
        if !self
            .vector_db
            .has_collection(data_type, field)
            .await
            .map_err(vector_error)?
        {
            self.vector_db
                .create_collection(data_type, field, self.embedding_engine.dimension())
                .await
                .map_err(vector_error)?;
        }
        for batch in summaries.chunks(self.embedding_engine.batch_size().max(1)) {
            let texts: Vec<&str> = batch.iter().map(|s| s.text.as_str()).collect();
            let vectors = self
                .embedding_engine
                .embed(&texts)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;
            let points: Vec<VectorPoint> = batch
                .iter()
                .zip(vectors)
                .map(|(summary, vector)| {
                    let mut point = VectorPoint::new(summary.base.id, vector);
                    // The full data point, as Python's payload carries it:
                    // the read side keys on `is_root` and `text`.
                    if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(summary) {
                        for (key, value) in fields {
                            point = point.with_metadata(key, value);
                        }
                    }
                    point.with_metadata("field", json!("text"))
                })
                .collect();
            self.vector_db
                .index_points(data_type, field, &points)
                .await
                .map_err(vector_error)?;
        }
        Ok(())
    }

    /// Write every kind of index edge in one call
    /// (`persist_context_index_edges`): relationship name → (source, target)
    /// pairs.
    pub async fn persist_edges(
        &self,
        edges_by_relationship: &[(&str, Vec<(String, String)>)],
    ) -> Result<(), CognifyError> {
        let updated_at = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let edges: Vec<EdgeData> = edges_by_relationship
            .iter()
            .flat_map(|(relationship, pairs)| {
                let updated_at = &updated_at;
                pairs.iter().map(move |(source, target)| {
                    (
                        source.clone(),
                        target.clone(),
                        (*relationship).to_string(),
                        HashMap::from([
                            (Cow::Borrowed("source_node_id"), json!(source)),
                            (Cow::Borrowed("target_node_id"), json!(target)),
                            (Cow::Borrowed("relationship_name"), json!(relationship)),
                            (Cow::Borrowed("updated_at"), json!(updated_at)),
                        ]),
                    )
                })
            })
            .collect();
        if edges.is_empty() {
            return Ok(());
        }
        self.graph_db.add_edges(&edges).await?;
        Ok(())
    }
}

/// `GlobalContextSummary_text` as (data type, field).
fn collection() -> (&'static str, &'static str) {
    (GlobalContextSummary::TYPE, "text")
}
