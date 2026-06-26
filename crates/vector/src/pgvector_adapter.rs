//! PGVector adapter — stores vectors in PostgreSQL via the `pgvector` extension.
//!
//! Each `(data_type, field_name)` pair maps to a dedicated PostgreSQL table with
//! columns: `id UUID PRIMARY KEY`, `vector vector(N)`, `metadata JSONB`.
//! A `_vector_collections` bookkeeping table tracks which collection tables exist.

use async_trait::async_trait;
use sea_orm::sea_query::{
    Alias, Asterisk, Expr, Func, Iden, OnConflict, Order, PostgresQueryBuilder, Query, Table,
};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;
use std::fmt;
use tracing::{Span, debug, instrument};
use uuid::Uuid;

use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

/// Max points per INSERT batch (300 params = 100 rows × 3 columns).
const BATCH_SIZE: usize = 100;

// ---------------------------------------------------------------------------
// Table / column identifiers for sea_query (`_vector_collections`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum VColl {
    Table,
    CollectionName,
    DataType,
    FieldName,
    Dimension,
}

impl Iden for VColl {
    #[allow(
        clippy::expect_used,
        reason = "writing a static &str into the fmt::Write sink is infallible"
    )]
    fn unquoted(&self, s: &mut dyn fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "_vector_collections",
                Self::CollectionName => "collection_name",
                Self::DataType => "data_type",
                Self::FieldName => "field_name",
                Self::Dimension => "dimension",
            }
        )
        .expect("write to string cannot fail");
    }
}

/// Vector database backed by PostgreSQL + pgvector extension.
///
/// Requires a PostgreSQL instance with the `vector` extension installed (the
/// adapter will attempt `CREATE EXTENSION IF NOT EXISTS vector` on startup).
pub struct PgVectorAdapter {
    db: DatabaseConnection,
    dimension: usize,
}

impl PgVectorAdapter {
    /// Connect to an existing PostgreSQL database and run pgvector migrations.
    ///
    /// The database must already exist. Use [`Self::from_connection`] to share
    /// a connection that was established elsewhere (e.g. by the database crate).
    ///
    /// # Arguments
    /// * `database_url` — Postgres connection string, e.g.
    ///   `postgres://user:pass@localhost:5432/mydb`
    /// * `dimension` — default vector dimension (e.g. 384 for BGE-Small)
    pub async fn new(database_url: &str, dimension: usize) -> VectorDBResult<Self> {
        let db = Database::connect(database_url)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector connect failed: {e}")))?;

        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        debug!("PgVectorAdapter initialised (dimension={dimension})");
        Ok(Self { db, dimension })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` (must be Postgres).
    ///
    /// The caller is responsible for ensuring the database already exists
    /// (the connection proves it does). Only the pgvector extension and
    /// bookkeeping table are created if missing.
    pub async fn from_connection(db: DatabaseConnection, dimension: usize) -> VectorDBResult<Self> {
        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        Ok(Self { db, dimension })
    }

    /// Returns the default vector dimension this adapter was configured with.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    // -- helpers ----------------------------------------------------------

    /// Build a SeaORM [`Statement`] from a `sea_query` query.
    fn build<S: sea_orm::StatementBuilder>(&self, query: &S) -> Statement {
        self.db.get_database_backend().build(query)
    }

    /// Build a validated table name from a `(data_type, field_name)` pair.
    ///
    /// Returns an error if the resulting name contains characters outside
    /// `[a-zA-Z0-9_]`, preventing SQL injection in dynamic DDL.
    fn collection_name(data_type: &str, field_name: &str) -> VectorDBResult<String> {
        let name = format!("{data_type}_{field_name}");
        Self::validate_identifier(&name)?;
        Ok(name)
    }

    /// Reject identifiers that could cause SQL-injection via dynamic DDL.
    fn validate_identifier(name: &str) -> VectorDBResult<()> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(VectorDBError::StorageError(format!(
                "Invalid identifier: {name}"
            )));
        }
        Ok(())
    }

    /// Format a vector as pgvector text literal: `[1.0,2.0,3.0]`
    fn format_vector(v: &[f32]) -> String {
        let inner: String = v
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!("[{inner}]")
    }

    /// Fetch the current `metadata` JSONB for the given points (by id) from
    /// `coll`, keyed by id. Used to union dataset membership before an upsert so
    /// re-indexing a content-addressed point under a new dataset does not drop
    /// the datasets it already belonged to.
    async fn fetch_metadata(
        &self,
        coll: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<HashMap<Uuid, HashMap<String, serde_json::Value>>> {
        let mut out: HashMap<Uuid, HashMap<String, serde_json::Value>> = HashMap::new();
        if points.is_empty() {
            return Ok(out);
        }
        let placeholders: Vec<String> = (1..=points.len()).map(|i| format!("${i}::uuid")).collect();
        let sql = format!(
            r#"SELECT id, metadata FROM "{coll}" WHERE id IN ({})"#,
            placeholders.join(", ")
        );
        let values: Vec<sea_orm::Value> = points.iter().map(|p| p.id.into()).collect();
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        for row in &rows {
            let id: Uuid = row
                .try_get("", "id")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let metadata_val: serde_json::Value = row
                .try_get("", "metadata")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            if let serde_json::Value::Object(map) = metadata_val {
                out.insert(id, map.into_iter().collect());
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl VectorDB for PgVectorAdapter {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;

        if self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionExists(coll));
        }

        // Create the vector table.
        let ddl = format!(
            r#"CREATE TABLE "{coll}" (
                id UUID PRIMARY KEY,
                vector vector({dimension}),
                metadata JSONB NOT NULL DEFAULT '{{}}'
            )"#
        );
        self.db
            .execute_unprepared(&ddl)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        // Register in bookkeeping table.
        let insert = Query::insert()
            .into_table(VColl::Table)
            .columns([
                VColl::CollectionName,
                VColl::DataType,
                VColl::FieldName,
                VColl::Dimension,
            ])
            .values_panic([
                coll.clone().into(),
                data_type.to_string().into(),
                field_name.to_string().into(),
                (dimension as i32).into(),
            ])
            .on_conflict(
                OnConflict::column(VColl::CollectionName)
                    .do_nothing()
                    .to_owned(),
            )
            .to_owned();

        self.db
            .execute(self.build(&insert))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        debug!("created collection {coll} (dim={dimension})");
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let coll = Self::collection_name(data_type, field_name)?;

        let inner = Query::select()
            .expr(Expr::val(1))
            .from(VColl::Table)
            .and_where(Expr::col(VColl::CollectionName).eq(coll))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("exists"))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        match row {
            Some(r) => {
                let exists: bool = r
                    .try_get("", "exists")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                Ok(exists)
            }
            None => Ok(false),
        }
    }

    #[instrument(
        name = "cognee.db.vector.upsert",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        // Dimension check.
        let expected_dim = points[0].vector.len();
        for p in points {
            if p.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    collection: coll.clone(),
                    expected: expected_dim,
                    actual: p.vector.len(),
                });
            }
        }

        // Batch upsert in chunks to stay within parameter limits.
        for chunk in points.chunks(BATCH_SIZE) {
            // Point IDs are content-addressed, so the same point is re-indexed
            // once per dataset. A plain `metadata = EXCLUDED.metadata` overwrite
            // would drop earlier datasets' `dataset_id` (cross-dataset dedup
            // bug). Read the existing rows' membership and union it into the
            // incoming points before upserting, mirroring the in-memory /
            // lancedb adapters and Python's union semantics.
            let existing = self.fetch_metadata(&coll, chunk).await?;

            let mut sql = format!(r#"INSERT INTO "{coll}" (id, vector, metadata) VALUES "#);
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(chunk.len() * 3);
            let mut idx = 1u32;

            for (i, pt) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!(
                    "(${}, ${}::vector, ${}::jsonb)",
                    idx,
                    idx + 1,
                    idx + 2
                ));
                idx += 3;

                let mut merged = pt.clone();
                if let Some(prev_meta) = existing.get(&pt.id) {
                    let prev = VectorPoint {
                        id: pt.id,
                        vector: Vec::new(),
                        metadata: prev_meta.clone(),
                    };
                    merged.merge_dataset_membership(&prev);
                }

                values.push(pt.id.into());
                values.push(Self::format_vector(&pt.vector).into());
                let metadata_obj: serde_json::Value = serde_json::Value::Object(
                    merged
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                values.push(metadata_obj.into());
            }

            sql.push_str(
                " ON CONFLICT (id) DO UPDATE SET vector = EXCLUDED.vector, metadata = EXCLUDED.metadata",
            );

            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }

        Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.search",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        let vec_str = Self::format_vector(query_vector);

        // cosine distance `<=>` returns 0..2 (0 = identical).
        // Convert to similarity: score = 1 - distance.
        let sql = format!(
            r#"SELECT id, 1 - (vector <=> $1::vector) AS score, metadata
               FROM "{coll}"
               ORDER BY vector <=> $1::vector
               LIMIT $2"#
        );

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                [vec_str.into(), (top_k as i64).into()],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let mut results = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: Uuid = row
                .try_get("", "id")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let score: f64 = row
                .try_get("", "score")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let metadata_val: serde_json::Value = row
                .try_get("", "metadata")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

            let metadata = match metadata_val {
                serde_json::Value::Object(map) => map
                    .into_iter()
                    .collect::<HashMap<String, serde_json::Value>>(),
                _ => HashMap::new(),
            };

            results.push(SearchResult {
                id,
                score: score as f32,
                metadata,
            });
        }

        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, results.len() as i64);
        Ok(results)
    }

    #[instrument(
        name = "cognee.db.vector.delete_collection",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        let drop = Table::drop()
            .table(Alias::new(&coll))
            .if_exists()
            .to_owned();

        self.db
            .execute_unprepared(&drop.to_string(PostgresQueryBuilder))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let delete = Query::delete()
            .from_table(VColl::Table)
            .and_where(Expr::col(VColl::CollectionName).eq(&coll))
            .to_owned();

        self.db
            .execute(self.build(&delete))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.delete",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
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
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        let query = Query::delete()
            .from_table(Alias::new(&coll))
            .and_where(
                Expr::col(Alias::new("id"))
                    .is_in(point_ids.iter().copied().map(sea_orm::Value::from)),
            )
            .to_owned();

        self.db
            .execute(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Span::current().record(COGNEE_DB_ROW_COUNT, point_ids.len() as i64);
        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let coll = Self::collection_name(data_type, field_name)?;

        let query = Query::select()
            .expr_as(Func::count(Expr::col(Asterisk)), Alias::new("count"))
            .from(Alias::new(&coll))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        match row {
            Some(r) => {
                let count: i64 = r
                    .try_get("", "count")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                Ok(count as usize)
            }
            None => Ok(0),
        }
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let query = Query::select()
            .columns([VColl::DataType, VColl::FieldName])
            .from(VColl::Table)
            .order_by(VColl::CollectionName, Order::Asc)
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let mut pairs = Vec::with_capacity(rows.len());
        for row in &rows {
            let dt: String = row
                .try_get("", "data_type")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let fn_: String = row
                .try_get("", "field_name")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            pairs.push((dt, fn_));
        }
        Ok(pairs)
    }
}

// ---------------------------------------------------------------------------
// SeaORM migration — creates the `vector` extension and bookkeeping table.
// ---------------------------------------------------------------------------
mod migrator {
    use sea_orm_migration::prelude::*;

    pub struct Migrator;

    #[async_trait::async_trait]
    impl MigratorTrait for Migrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(CreatePgVectorExtension)]
        }
    }

    struct CreatePgVectorExtension;

    impl MigrationName for CreatePgVectorExtension {
        fn name(&self) -> &str {
            "m20250101_000001_create_pgvector_extension"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreatePgVectorExtension {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();

            conn.execute_unprepared("CREATE EXTENSION IF NOT EXISTS vector")
                .await?;

            conn.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS _vector_collections (
                    collection_name TEXT PRIMARY KEY,
                    data_type       TEXT    NOT NULL,
                    field_name      TEXT    NOT NULL,
                    dimension       INTEGER NOT NULL,
                    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )",
            )
            .await?;

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();
            conn.execute_unprepared("DROP TABLE IF EXISTS _vector_collections")
                .await?;
            conn.execute_unprepared("DROP EXTENSION IF EXISTS vector")
                .await?;
            Ok(())
        }
    }
}
