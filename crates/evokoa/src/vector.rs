use std::collections::HashMap;

use async_trait::async_trait;
use cognee_utils::sanitize::sanitize_json;
use cognee_vector::{SearchResult, VectorDB, VectorDBError, VectorDBResult, VectorPoint};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::Value as JsonValue;
use uuid::Uuid;

const BATCH_SIZE: usize = 100;

/// pgContext vector adapter backed by ordinary per-collection source tables.
pub struct EvokoaVectorAdapter {
    db: DatabaseConnection,
}

impl EvokoaVectorAdapter {
    pub async fn new(database_url: &str) -> VectorDBResult<Self> {
        let db = Database::connect(database_url).await.map_err(storage)?;
        Self::from_connection(db).await
    }

    pub async fn from_connection(db: DatabaseConnection) -> VectorDBResult<Self> {
        db.execute_unprepared(
            "CREATE EXTENSION IF NOT EXISTS pgcontext; \
             CREATE TABLE IF NOT EXISTS cognee_pgcontext_collections ( \
               collection_name text PRIMARY KEY, data_type text NOT NULL, \
               field_name text NOT NULL, dimension integer NOT NULL, \
               UNIQUE(data_type, field_name));",
        )
        .await
        .map_err(storage)?;
        Ok(Self { db })
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    pub(crate) fn collection_name(data_type: &str, field_name: &str) -> VectorDBResult<String> {
        let name = format!("ctx_{data_type}_{field_name}").to_ascii_lowercase();
        if name.len() > 63 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(VectorDBError::StorageError(format!(
                "invalid collection identifier: {name}"
            )));
        }
        Ok(name)
    }

    pub(crate) fn pg_limit(value: usize, name: &str) -> VectorDBResult<i32> {
        i32::try_from(value).map_err(|_| {
            VectorDBError::StorageError(format!("{name} exceeds PostgreSQL's integer limit"))
        })
    }

    fn vector_literal(vector: &[f32]) -> String {
        // pgContext 0.3.0 treats small-but-nonzero vectors as zero during
        // cosine normalization (observed at squared norm 5.97e-10). Scaling to
        // unit length is cosine-invariant and avoids that numeric threshold.
        let norm = vector
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        let body = vector
            .iter()
            .map(|value| {
                if norm > 0.0 {
                    (f64::from(*value) / norm).to_string()
                } else {
                    value.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("[{body}]")
    }

    fn metadata_json(metadata: &HashMap<String, JsonValue>) -> JsonValue {
        sanitize_json(JsonValue::Object(
            metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ))
    }

    fn row_result(row: &sea_orm::QueryResult, score: f32) -> VectorDBResult<SearchResult> {
        let source_key: String = row.try_get("", "source_key").map_err(storage)?;
        let id =
            Uuid::parse_str(&source_key).map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let metadata: JsonValue = row.try_get("", "metadata").map_err(storage)?;
        Ok(SearchResult {
            id,
            score,
            metadata: metadata
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        })
    }

    async fn upsert(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
        merge_membership: bool,
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }
        let coll = Self::collection_name(data_type, field_name)?;
        if !self.has_collection(data_type, field_name).await? {
            self.create_collection(data_type, field_name, points[0].vector.len())
                .await?;
        }
        for point in points {
            if point.vector.len() != points[0].vector.len() {
                return Err(VectorDBError::DimensionMismatch {
                    collection: coll,
                    expected: points[0].vector.len(),
                    actual: point.vector.len(),
                });
            }
        }
        for chunk in points.chunks(BATCH_SIZE) {
            for point in chunk {
                let mut incoming = point.clone();
                if merge_membership {
                    let prior = self.retrieve(data_type, field_name, &[point.id]).await?;
                    if let Some(previous) = prior.first() {
                        incoming.merge_dataset_membership(&VectorPoint {
                            id: previous.id,
                            vector: vec![],
                            metadata: previous.metadata.clone(),
                        });
                    }
                }
                let sql = format!(
                    "INSERT INTO \"{coll}\" (id, embedding, metadata) \
                     VALUES ($1::uuid, $2::pgcontext.vector, $3::jsonb) \
                     ON CONFLICT (id) DO UPDATE SET embedding=EXCLUDED.embedding, metadata=EXCLUDED.metadata"
                );
                self.db
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        sql,
                        [
                            point.id.into(),
                            Self::vector_literal(&point.vector).into(),
                            Self::metadata_json(&incoming.metadata).into(),
                        ],
                    ))
                    .await
                    .map_err(storage)?;
            }
            let (indexable, zero): (Vec<_>, Vec<_>) = chunk
                .iter()
                .partition(|point| point.vector.iter().any(|value| *value != 0.0));
            if !indexable.is_empty() {
                let keys = indexable
                    .iter()
                    .map(|point| point.id.to_string())
                    .collect::<Vec<_>>();
                self.db
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT pgcontext.upsert_points($1, $2::text[])",
                        [coll.clone().into(), keys.into()],
                    ))
                    .await
                    .map_err(storage)?;
            }
            if !zero.is_empty() {
                // pgContext 0.3.0 aborts every cosine search in a collection
                // containing even one zero vector. Keep the source row for
                // retrieve/size semantics, but ensure it is absent from the
                // derived index (including nonzero -> zero overwrites).
                let keys = zero
                    .iter()
                    .map(|point| point.id.to_string())
                    .collect::<Vec<_>>();
                self.db
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT pgcontext.delete_points($1, $2::text[])",
                        [coll.clone().into(), keys.into()],
                    ))
                    .await
                    .map_err(storage)?;
            }
        }
        Ok(())
    }

    async fn search_unfiltered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let coll = Self::collection_name(data_type, field_name)?;
        if !self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionNotFound(coll));
        }
        let top_k = Self::pg_limit(top_k, "top_k")?;
        let sql = format!(
            "SELECT s.source_key, s.score, t.metadata \
             FROM pgcontext.search($1, 'embedding', $2::pgcontext.vector, $3) s \
             JOIN \"{coll}\" t ON t.id::text=s.source_key \
             ORDER BY s.score ASC, s.point_id ASC"
        );
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [
                    coll.into(),
                    Self::vector_literal(query_vector).into(),
                    top_k.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let distance: f32 = row.try_get("", "score").map_err(storage)?;
                Self::row_result(row, 1.0 - distance)
            })
            .collect()
    }
}

fn storage<E: std::fmt::Display>(error: E) -> VectorDBError {
    VectorDBError::StorageError(error.to_string())
}

#[async_trait]
impl VectorDB for EvokoaVectorAdapter {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;
        let dimension = Self::pg_limit(dimension, "vector dimension")?;
        if dimension == 0 {
            return Err(VectorDBError::StorageError(
                "vector dimension must be greater than zero".to_string(),
            ));
        }
        if self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionExists(coll));
        }
        self.db.execute_unprepared(&format!(
            "CREATE TABLE \"{coll}\" (id uuid PRIMARY KEY, embedding pgcontext.vector({dimension}) NOT NULL, metadata jsonb NOT NULL DEFAULT '{{}}');"
        )).await.map_err(storage)?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pgcontext.create_collection($1, $2)",
                [coll.clone().into(), format!("public.{coll}").into()],
            ))
            .await
            .map_err(storage)?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pgcontext.register_vector($1, 'embedding', 'embedding', $2, 'cosine')",
                [coll.clone().into(), dimension.into()],
            ))
            .await
            .map_err(storage)?;
        self.db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT pgcontext.register_jsonb_path($1, 'dataset_ids', 'metadata', ARRAY['dataset_ids'])",
            [coll.clone().into()]
        )).await.map_err(storage)?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO cognee_pgcontext_collections VALUES ($1,$2,$3,$4)",
                [
                    coll.into(),
                    data_type.into(),
                    field_name.into(),
                    dimension.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let coll = Self::collection_name(data_type, field_name)?;
        let row = self.db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT EXISTS(SELECT 1 FROM cognee_pgcontext_collections WHERE collection_name=$1) AS present", [coll.into()]
        )).await.map_err(storage)?;
        Ok(row
            .and_then(|r| r.try_get::<bool>("", "present").ok())
            .unwrap_or(false))
    }

    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        self.upsert(data_type, field_name, points, true).await
    }
    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        self.upsert(data_type, field_name, points, false).await
    }
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.search_unfiltered(data_type, field_name, query_vector, top_k)
            .await
    }

    async fn search_similar_filtered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        node_name: Option<&[String]>,
        op: &str,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let Some(names) = node_name.filter(|n| !n.is_empty()) else {
            return self
                .search_similar(data_type, field_name, query_vector, top_k)
                .await;
        };
        // pgContext 0.3.0's Qdrant-style grammar compares JSON arrays as whole
        // values; it has no contains-element/contains-all predicate. Cognee's
        // `belongs_to_set` contract therefore uses an exact source-table scan until
        // the extension grows that operator. This preserves filter-before-limit
        // correctness instead of silently losing recall through post-filtering.
        let coll = Self::collection_name(data_type, field_name)?;
        let membership_predicate = if op == "AND" {
            "NOT EXISTS (\
               SELECT 1 FROM unnest($2::text[]) requested(name) \
               WHERE NOT EXISTS (\
                 SELECT 1 \
                 FROM jsonb_array_elements(COALESCE(metadata->'belongs_to_set', '[]'::jsonb)) entry \
                 WHERE CASE jsonb_typeof(entry) \
                   WHEN 'string' THEN entry #>> '{}' \
                   WHEN 'object' THEN entry->>'name' \
                 END = requested.name\
               )\
             )"
        } else {
            "EXISTS (\
               SELECT 1 \
               FROM jsonb_array_elements(COALESCE(metadata->'belongs_to_set', '[]'::jsonb)) entry \
               WHERE CASE jsonb_typeof(entry) \
                 WHEN 'string' THEN entry #>> '{}' \
                 WHEN 'object' THEN entry->>'name' \
               END = ANY($2::text[])\
             )"
        };
        let sql = format!(
            "SELECT id::text AS source_key, \
                    pgcontext.cosine_distance(embedding, $1::pgcontext.vector)::real AS score, metadata \
             FROM \"{coll}\" \
             WHERE {membership_predicate} \
             ORDER BY score ASC, id LIMIT $3"
        );
        let top_k = i64::try_from(top_k).map_err(|_| {
            VectorDBError::StorageError("top_k exceeds PostgreSQL's bigint limit".to_string())
        })?;
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [
                    Self::vector_literal(query_vector).into(),
                    names.to_vec().into(),
                    top_k.into(),
                ],
            ))
            .await
            .map_err(storage)?;
        rows.iter()
            .map(|row| {
                let distance: f32 = row.try_get("", "score").map_err(storage)?;
                Self::row_result(row, 1.0 - distance)
            })
            .collect()
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;
        if !self.has_collection(data_type, field_name).await? {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pgcontext.drop_collection($1)",
                [coll.clone().into()],
            ))
            .await
            .map_err(storage)?;
        self.db
            .execute_unprepared(&format!("DROP TABLE IF EXISTS \"{coll}\""))
            .await
            .map_err(storage)?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM cognee_pgcontext_collections WHERE collection_name=$1",
                [coll.into()],
            ))
            .await
            .map_err(storage)?;
        Ok(())
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
        let coll = Self::collection_name(data_type, field_name)?;
        // Pipeline cleanup may name the same point through multiple graph
        // relationships. pgContext rejects duplicate source keys in one batch,
        // so normalize the trait input before calling the extension.
        let mut unique_ids = point_ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        let keys = unique_ids.iter().map(Uuid::to_string).collect::<Vec<_>>();
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pgcontext.delete_points($1,$2::text[])",
                [coll.clone().into(), keys.into()],
            ))
            .await
            .map_err(storage)?;
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("DELETE FROM \"{coll}\" WHERE id = ANY($1::uuid[])"),
                [unique_ids.into()],
            ))
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        if ids.is_empty() || !self.has_collection(data_type, field_name).await? {
            return Ok(vec![]);
        }
        let coll = Self::collection_name(data_type, field_name)?;
        let rows = self.db.query_all(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            format!("SELECT id::text AS source_key, metadata FROM \"{coll}\" WHERE id = ANY($1::uuid[])"), [ids.to_vec().into()]
        )).await.map_err(storage)?;
        rows.iter().map(|r| Self::row_result(r, 0.0)).collect()
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let coll = Self::collection_name(data_type, field_name)?;
        if !self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionNotFound(coll));
        }
        let row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!("SELECT count(*) AS count FROM \"{coll}\""),
            ))
            .await
            .map_err(storage)?;
        let count = row
            .ok_or_else(|| VectorDBError::StorageError("collection count returned no row".into()))?
            .try_get::<i64>("", "count")
            .map_err(storage)?;
        usize::try_from(count)
            .map_err(|_| VectorDBError::StorageError(format!("invalid collection count: {count}")))
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let rows = self.db.query_all(Statement::from_string(DatabaseBackend::Postgres, "SELECT data_type,field_name FROM cognee_pgcontext_collections ORDER BY data_type,field_name")).await.map_err(storage)?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("", "data_type").map_err(storage)?,
                    r.try_get("", "field_name").map_err(storage)?,
                ))
            })
            .collect()
    }
}
