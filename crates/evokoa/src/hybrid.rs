use cognee_graph::{GraphDBResult, GraphDBTrait};
use cognee_vector::{VectorDBError, VectorDBResult};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::Value;
use uuid::Uuid;

use crate::{EvokoaGraphAdapter, EvokoaVectorAdapter};

#[derive(Debug, Clone)]
pub struct HybridGraphVectorHit {
    pub id: Uuid,
    pub score: f32,
    pub metadata: Value,
    pub neighbor_id: Option<String>,
    pub neighbor: Option<Value>,
    pub edge_path: Option<Value>,
}

/// A single pool serving pgGraph and pgContext, plus combined SQL operations.
pub struct EvokoaHybridAdapter {
    db: DatabaseConnection,
    graph: EvokoaGraphAdapter,
    vector: EvokoaVectorAdapter,
}

impl EvokoaHybridAdapter {
    pub async fn new(database_url: &str) -> Result<Self, String> {
        let db = Database::connect(database_url)
            .await
            .map_err(|e| e.to_string())?;
        let graph = EvokoaGraphAdapter::from_connection(db.clone())
            .await
            .map_err(|e| e.to_string())?;
        let vector = EvokoaVectorAdapter::from_connection(db.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { db, graph, vector })
    }

    pub async fn initialize(&self) -> GraphDBResult<()> {
        self.graph.initialize().await
    }
    pub fn graph(&self) -> &EvokoaGraphAdapter {
        &self.graph
    }
    pub fn vector(&self) -> &EvokoaVectorAdapter {
        &self.vector
    }

    /// Search a pgContext collection and expand every hit through pgGraph in
    /// one PostgreSQL statement and one MVCC snapshot.
    ///
    /// Vector source keys must also be `graph_node.id`, as they are for Cognee
    /// entity collections. Callers must not use this operation for chunk
    /// collections, whose point IDs do not identify graph nodes.
    pub async fn search_graph_with_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        neighbors_per_hit: usize,
    ) -> VectorDBResult<Vec<HybridGraphVectorHit>> {
        let coll = EvokoaVectorAdapter::collection_name(data_type, field_name)?;
        let top_k = EvokoaVectorAdapter::pg_limit(top_k, "top_k")?;
        let neighbors_per_hit =
            EvokoaVectorAdapter::pg_limit(neighbors_per_hit, "neighbors_per_hit")?;
        let vector = format!(
            "[{}]",
            query_vector
                .iter()
                .map(f32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let sql = format!(
            r#"
            WITH vector_hits AS MATERIALIZED (
              SELECT s.source_key, s.score, t.metadata
              FROM pgcontext.search($1, 'embedding', $2::pgcontext.vector, $3) s
              JOIN "{coll}" t ON t.id::text = s.source_key
            ), active_graph AS (
              SELECT graph_name FROM graph.current_graph() LIMIT 1
            )
            SELECT h.source_key, h.score, h.metadata,
                   n.node_id AS neighbor_id, n.node AS neighbor, n.edge_path
            FROM vector_hits h
            LEFT JOIN LATERAL graph.get_neighbors(
              (SELECT graph_name FROM active_graph),
              'graph_node', h.source_key, 'any', NULL, NULL, true, $4
            ) n ON true
            ORDER BY h.score ASC, h.source_key, n.node_id
        "#
        );
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [
                    coll.into(),
                    vector.into(),
                    top_k.into(),
                    neighbors_per_hit.into(),
                ],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        rows.iter()
            .map(|row| {
                let source_key: String = row
                    .try_get("", "source_key")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                let distance: f32 = row
                    .try_get("", "score")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                Ok(HybridGraphVectorHit {
                    id: Uuid::parse_str(&source_key)
                        .map_err(|e| VectorDBError::StorageError(e.to_string()))?,
                    score: 1.0 - distance,
                    metadata: row
                        .try_get("", "metadata")
                        .map_err(|e| VectorDBError::StorageError(e.to_string()))?,
                    neighbor_id: row.try_get("", "neighbor_id").ok(),
                    neighbor: row.try_get("", "neighbor").ok(),
                    edge_path: row.try_get("", "edge_path").ok(),
                })
            })
            .collect()
    }

    pub async fn close(self) -> Result<(), String> {
        self.db.close().await.map_err(|e| e.to_string())
    }
}
