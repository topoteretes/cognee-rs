//! Embedded LanceDB vector store — the OSS default on non-Android targets.
//!
//! Each `(data_type, field_name)` pair maps to one LanceDB table named
//! `"{data_type}_{field_name}"` (matching `BruteForceVectorDB`'s naming so
//! a backend switch keeps existing on-disk data discoverable). Tables hold
//! three columns:
//!
//! | column     | Arrow type                        | semantics                |
//! |------------|-----------------------------------|--------------------------|
//! | `id`       | `FixedSizeBinary(16)`             | UUID bytes (primary key) |
//! | `vector`   | `FixedSizeList<Float32, dim>`     | embedding                |
//! | `metadata` | `Utf8`                            | JSON blob                |
//!
//! Persistence: the LanceDB `connect(uri).execute()` call points at a
//! filesystem directory (defaults to `{system_root_directory}/databases/cognee.lancedb`,
//! matching the Python SDK file layout — Python parity is intentional).
//! All writes go through LanceDB's transactional writer, so crashes mid-write
//! don't corrupt prior versions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::{
    Array, FixedSizeBinaryArray, FixedSizeListArray, Float32Array, RecordBatch, StringArray,
    types::Float32Type,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::{
    DistanceType, connect,
    connection::Connection,
    query::{ExecutableQuery, QueryBase},
};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

fn collection_name(data_type: &str, field_name: &str) -> String {
    format!("{data_type}_{field_name}")
}

fn map_lance_err(e: lancedb::Error) -> VectorDBError {
    VectorDBError::StorageError(format!("lancedb: {e}"))
}

/// Dimension of a `FixedSizeList<Float32, _>` field, or `None` if it's some
/// other type. Used when opening a pre-existing table to recover the dim.
fn dimension_from_schema(schema: &SchemaRef) -> Option<usize> {
    schema.field_with_name("vector").ok().and_then(|f| {
        if let DataType::FixedSizeList(_, dim) = f.data_type() {
            usize::try_from(*dim).ok()
        } else {
            None
        }
    })
}

fn build_schema(dimension: usize) -> SchemaRef {
    let vector_field = Arc::new(Field::new("item", DataType::Float32, true));
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::FixedSizeBinary(16), false),
        Field::new(
            "vector",
            DataType::FixedSizeList(vector_field, dimension as i32),
            false,
        ),
        Field::new("metadata", DataType::Utf8, false),
    ]))
}

fn points_to_batch(
    schema: SchemaRef,
    dimension: usize,
    collection: &str,
    points: &[VectorPoint],
) -> VectorDBResult<RecordBatch> {
    if let Some(p) = points.iter().find(|p| p.vector.len() != dimension) {
        return Err(VectorDBError::DimensionMismatch {
            collection: collection.to_string(),
            expected: dimension,
            actual: p.vector.len(),
        });
    }

    let id_array = FixedSizeBinaryArray::try_from_iter(points.iter().map(|p| *p.id.as_bytes()))
        .map_err(|e| VectorDBError::StorageError(format!("id column build: {e}")))?;

    let vector_array = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        points
            .iter()
            .map(|p| Some(p.vector.iter().map(|v| Some(*v)).collect::<Vec<_>>())),
        dimension as i32,
    );

    let metadata_array = StringArray::from(
        points
            .iter()
            .map(|p| serde_json::to_string(&p.metadata))
            .collect::<Result<Vec<_>, _>>()?,
    );

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(id_array),
            Arc::new(vector_array),
            Arc::new(metadata_array),
        ],
    )
    .map_err(|e| VectorDBError::StorageError(format!("record batch build: {e}")))
}

fn search_results_from_batches(batches: Vec<RecordBatch>) -> VectorDBResult<Vec<SearchResult>> {
    let mut out = Vec::new();
    for batch in batches {
        let id_col = batch
            .column_by_name("id")
            .ok_or_else(|| VectorDBError::StorageError("missing id column".to_string()))?
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| VectorDBError::StorageError("id column type mismatch".to_string()))?;

        let metadata_col = batch
            .column_by_name("metadata")
            .ok_or_else(|| VectorDBError::StorageError("missing metadata column".to_string()))?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                VectorDBError::StorageError("metadata column type mismatch".to_string())
            })?;

        // LanceDB's `nearest_to` appends a `_distance` column with the
        // distance from the query (lower = closer for Cosine/L2). Convert
        // distance back to a similarity score so callers can sort descending.
        let distance_col = batch
            .column_by_name("_distance")
            .ok_or_else(|| VectorDBError::StorageError("missing _distance column".to_string()))?
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or_else(|| {
                VectorDBError::StorageError("_distance column type mismatch".to_string())
            })?;

        for row in 0..batch.num_rows() {
            let id_bytes = id_col.value(row);
            let id = Uuid::from_slice(id_bytes)
                .map_err(|e| VectorDBError::StorageError(format!("id is not a valid UUID: {e}")))?;

            let metadata: HashMap<String, serde_json::Value> =
                serde_json::from_str(metadata_col.value(row))?;

            // Cosine distance is in [0, 2]; clamp + invert to similarity.
            let distance = distance_col.value(row).max(0.0);
            let score = (1.0 - distance).clamp(-1.0, 1.0);

            out.push(SearchResult {
                id,
                score,
                metadata,
            });
        }
    }
    Ok(out)
}

/// LanceDB-backed vector store.
pub struct LanceDbAdapter {
    connection: Connection,
    /// Cached per-collection dimensions so we can rebuild Arrow schemas without
    /// re-opening each table on every write/search call.
    dimensions: Arc<RwLock<HashMap<String, usize>>>,
}

impl LanceDbAdapter {
    /// Open (or create) a LanceDB store at the given filesystem path.
    pub async fn new(path: PathBuf) -> VectorDBResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let uri = path.to_str().ok_or_else(|| {
            VectorDBError::StorageError(format!("lancedb path is not valid UTF-8: {path:?}"))
        })?;
        let connection = connect(uri).execute().await.map_err(map_lance_err)?;
        Ok(Self {
            connection,
            dimensions: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    async fn cached_dimension(&self, table_name: &str) -> Option<usize> {
        self.dimensions.read().await.get(table_name).copied()
    }

    async fn resolved_dimension(&self, table_name: &str) -> VectorDBResult<usize> {
        if let Some(dim) = self.cached_dimension(table_name).await {
            return Ok(dim);
        }
        let table = self
            .connection
            .open_table(table_name)
            .execute()
            .await
            .map_err(|e| match e {
                lancedb::Error::TableNotFound { .. } => {
                    VectorDBError::CollectionNotFound(table_name.to_string())
                }
                other => map_lance_err(other),
            })?;
        let schema = table.schema().await.map_err(map_lance_err)?;
        let dim = dimension_from_schema(&schema).ok_or_else(|| {
            VectorDBError::StorageError(format!(
                "table '{table_name}' has no FixedSizeList<Float32, _> vector column"
            ))
        })?;
        self.dimensions
            .write()
            .await
            .insert(table_name.to_string(), dim);
        Ok(dim)
    }
}

#[async_trait]
impl VectorDB for LanceDbAdapter {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let name = collection_name(data_type, field_name);
        if self.has_collection(data_type, field_name).await? {
            // Idempotent: matches BruteForceVectorDB.create_collection semantics.
            return Ok(());
        }
        let schema = build_schema(dimension);
        self.connection
            .create_empty_table(&name, schema)
            .execute()
            .await
            .map_err(map_lance_err)?;
        self.dimensions.write().await.insert(name, dimension);
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let target = collection_name(data_type, field_name);
        let names = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(map_lance_err)?;
        Ok(names.iter().any(|n| n == &target))
    }

    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }
        let name = collection_name(data_type, field_name);
        let dimension = self.resolved_dimension(&name).await?;
        let schema = build_schema(dimension);
        let batch = points_to_batch(schema.clone(), dimension, &name, points)?;
        let table = self
            .connection
            .open_table(&name)
            .execute()
            .await
            .map_err(map_lance_err)?;
        // Upsert by id so re-indexing existing points replaces them.
        let id_values: Vec<String> = points
            .iter()
            .map(|p| {
                let bytes = p.id.as_bytes();
                // SQL hex literal: X'…' over the 16 UUID bytes.
                let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
                format!("X'{hex}'")
            })
            .collect();
        if !id_values.is_empty() {
            let predicate = format!("id IN ({})", id_values.join(", "));
            table
                .delete(predicate.as_str())
                .await
                .map_err(map_lance_err)?;
        }
        let _ = schema; // schema lives on the RecordBatch; nothing else needs it.
        table
            .add(vec![batch])
            .execute()
            .await
            .map_err(map_lance_err)?;
        Ok(())
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let name = collection_name(data_type, field_name);
        let table = self
            .connection
            .open_table(&name)
            .execute()
            .await
            .map_err(|e| match e {
                lancedb::Error::TableNotFound { .. } => {
                    VectorDBError::CollectionNotFound(name.clone())
                }
                other => map_lance_err(other),
            })?;
        let stream = table
            .query()
            .limit(top_k)
            .nearest_to(query_vector)
            .map_err(map_lance_err)?
            .distance_type(DistanceType::Cosine)
            .execute()
            .await
            .map_err(map_lance_err)?;
        let batches: Vec<RecordBatch> = stream.try_collect().await.map_err(map_lance_err)?;
        search_results_from_batches(batches)
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let name = collection_name(data_type, field_name);
        match self.connection.drop_table(&name, &[]).await {
            Ok(()) => {
                self.dimensions.write().await.remove(&name);
                Ok(())
            }
            Err(lancedb::Error::TableNotFound { .. }) => Ok(()),
            Err(other) => Err(map_lance_err(other)),
        }
    }

    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }
        let name = collection_name(data_type, field_name);
        let table = self
            .connection
            .open_table(&name)
            .execute()
            .await
            .map_err(|e| match e {
                lancedb::Error::TableNotFound { .. } => {
                    VectorDBError::CollectionNotFound(name.clone())
                }
                other => map_lance_err(other),
            })?;
        let id_values: Vec<String> = point_ids
            .iter()
            .map(|id| {
                let hex: String = id.as_bytes().iter().map(|b| format!("{b:02X}")).collect();
                format!("X'{hex}'")
            })
            .collect();
        let predicate = format!("id IN ({})", id_values.join(", "));
        table
            .delete(predicate.as_str())
            .await
            .map_err(map_lance_err)?;
        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let name = collection_name(data_type, field_name);
        let table = self
            .connection
            .open_table(&name)
            .execute()
            .await
            .map_err(|e| match e {
                lancedb::Error::TableNotFound { .. } => {
                    VectorDBError::CollectionNotFound(name.clone())
                }
                other => map_lance_err(other),
            })?;
        table.count_rows(None).await.map_err(map_lance_err)
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let names = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(map_lance_err)?;
        Ok(names
            .into_iter()
            .filter_map(|n| {
                n.find('_')
                    .map(|i| (n[..i].to_string(), n[i + 1..].to_string()))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn point(id: Uuid, vector: Vec<f32>, kind: &str) -> VectorPoint {
        VectorPoint::new(id, vector).with_metadata("kind", json!(kind))
    }

    async fn fresh_adapter() -> (LanceDbAdapter, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store.lance");
        let adapter = LanceDbAdapter::new(path).await.unwrap();
        (adapter, dir)
    }

    #[tokio::test]
    async fn create_and_has_collection_roundtrip() {
        let (adapter, _dir) = fresh_adapter().await;
        assert!(!adapter.has_collection("Chunk", "text").await.unwrap());
        adapter.create_collection("Chunk", "text", 4).await.unwrap();
        assert!(adapter.has_collection("Chunk", "text").await.unwrap());
        // Idempotent.
        adapter.create_collection("Chunk", "text", 4).await.unwrap();
    }

    #[tokio::test]
    async fn index_and_search_finds_closest_point() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 3).await.unwrap();

        let target = Uuid::new_v4();
        let other = Uuid::new_v4();
        let points = vec![
            point(target, vec![1.0, 0.0, 0.0], "target"),
            point(other, vec![0.0, 1.0, 0.0], "other"),
        ];
        adapter
            .index_points("Chunk", "text", &points)
            .await
            .unwrap();

        let results = adapter
            .search_similar("Chunk", "text", &[1.0, 0.0, 0.0], 2)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, target, "nearest point should be the target");
        assert_eq!(results[0].metadata.get("kind").unwrap(), &json!("target"));
        // Cosine distance from target to itself ~= 0 → score ~= 1.
        assert!(results[0].score > 0.99);
    }

    #[tokio::test]
    async fn collection_size_reports_row_count() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 2).await.unwrap();
        let points = vec![
            point(Uuid::new_v4(), vec![0.0, 1.0], "a"),
            point(Uuid::new_v4(), vec![1.0, 0.0], "b"),
        ];
        adapter
            .index_points("Chunk", "text", &points)
            .await
            .unwrap();
        assert_eq!(adapter.collection_size("Chunk", "text").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn delete_points_removes_by_id() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 2).await.unwrap();
        let keep = Uuid::new_v4();
        let drop = Uuid::new_v4();
        adapter
            .index_points(
                "Chunk",
                "text",
                &[
                    point(keep, vec![1.0, 0.0], "keep"),
                    point(drop, vec![0.0, 1.0], "drop"),
                ],
            )
            .await
            .unwrap();

        adapter
            .delete_points("Chunk", "text", &[drop])
            .await
            .unwrap();

        assert_eq!(adapter.collection_size("Chunk", "text").await.unwrap(), 1);
        let results = adapter
            .search_similar("Chunk", "text", &[0.0, 1.0], 5)
            .await
            .unwrap();
        assert!(results.iter().all(|r| r.id != drop));
    }

    #[tokio::test]
    async fn index_points_replaces_existing_id() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 2).await.unwrap();
        let id = Uuid::new_v4();
        adapter
            .index_points("Chunk", "text", &[point(id, vec![1.0, 0.0], "v1")])
            .await
            .unwrap();
        adapter
            .index_points("Chunk", "text", &[point(id, vec![0.0, 1.0], "v2")])
            .await
            .unwrap();
        assert_eq!(adapter.collection_size("Chunk", "text").await.unwrap(), 1);

        let results = adapter
            .search_similar("Chunk", "text", &[0.0, 1.0], 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
        assert_eq!(results[0].metadata.get("kind").unwrap(), &json!("v2"));
    }

    #[tokio::test]
    async fn delete_collection_drops_table_and_is_idempotent() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 2).await.unwrap();
        assert!(adapter.has_collection("Chunk", "text").await.unwrap());
        adapter.delete_collection("Chunk", "text").await.unwrap();
        assert!(!adapter.has_collection("Chunk", "text").await.unwrap());
        // Idempotent on a missing table.
        adapter.delete_collection("Chunk", "text").await.unwrap();
    }

    #[tokio::test]
    async fn list_and_prune_collections() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 2).await.unwrap();
        adapter
            .create_collection("Entity", "name", 2)
            .await
            .unwrap();

        let mut listed: Vec<_> = adapter.list_collections().await.unwrap();
        listed.sort();
        assert_eq!(
            listed,
            vec![
                ("Chunk".to_string(), "text".to_string()),
                ("Entity".to_string(), "name".to_string()),
            ]
        );

        adapter.prune().await.unwrap();
        assert_eq!(adapter.list_collections().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn dimension_mismatch_returns_error() {
        let (adapter, _dir) = fresh_adapter().await;
        adapter.create_collection("Chunk", "text", 3).await.unwrap();
        let err = adapter
            .index_points(
                "Chunk",
                "text",
                &[point(Uuid::new_v4(), vec![1.0, 0.0], "bad")],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                VectorDBError::DimensionMismatch {
                    expected: 3,
                    actual: 2,
                    ..
                }
            ),
            "expected DimensionMismatch, got {err:?}"
        );
    }

    #[tokio::test]
    async fn store_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("persist.lance");
        let id = Uuid::new_v4();

        {
            let adapter = LanceDbAdapter::new(path.clone()).await.unwrap();
            adapter.create_collection("Chunk", "text", 2).await.unwrap();
            adapter
                .index_points("Chunk", "text", &[point(id, vec![1.0, 0.0], "v1")])
                .await
                .unwrap();
        }

        // Re-open at the same path; the table and row should still be there.
        let adapter = LanceDbAdapter::new(path).await.unwrap();
        assert!(adapter.has_collection("Chunk", "text").await.unwrap());
        let results = adapter
            .search_similar("Chunk", "text", &[1.0, 0.0], 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
    }
}
