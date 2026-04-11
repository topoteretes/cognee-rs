//! PGVector adapter — stores vectors in PostgreSQL via the `pgvector` extension.
//!
//! Each `(data_type, field_name)` pair maps to a dedicated PostgreSQL table with
//! columns: `id UUID PRIMARY KEY`, `vector vector(N)`, `metadata JSONB`.
//! A `_vector_collections` bookkeeping table tracks which collection tables exist.

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;
use tracing::debug;
use url::Url;
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

/// Max points per INSERT batch (300 params = 100 rows × 3 columns).
const BATCH_SIZE: usize = 100;

/// Vector database backed by PostgreSQL + pgvector extension.
///
/// Requires a PostgreSQL instance with the `vector` extension installed (the
/// adapter will attempt `CREATE EXTENSION IF NOT EXISTS vector` on startup).
pub struct PgVectorAdapter {
    db: DatabaseConnection,
    dimension: usize,
}

impl PgVectorAdapter {
    /// Connect to PostgreSQL and run the pgvector migration.
    ///
    /// The target database is created automatically if it does not exist yet
    /// (connects to the `postgres` maintenance database to issue the DDL).
    ///
    /// # Arguments
    /// * `database_url` — Postgres connection string, e.g.
    ///   `postgres://user:pass@localhost:5432/mydb`
    /// * `dimension` — default vector dimension (e.g. 384 for BGE-Small)
    pub async fn new(database_url: &str, dimension: usize) -> VectorDBResult<Self> {
        Self::ensure_database_exists(database_url).await?;

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

    /// Ensure the target database exists, creating it if necessary.
    ///
    /// Connects to the `postgres` maintenance database (same host/credentials)
    /// and runs `CREATE DATABASE "…"` if the target does not exist yet.
    async fn ensure_database_exists(database_url: &str) -> VectorDBResult<()> {
        let (maintenance_url, db_name) = Self::parse_maintenance_url(database_url)?;
        Self::validate_identifier(&db_name)?;

        let admin = Database::connect(&maintenance_url).await.map_err(|e| {
            VectorDBError::StorageError(format!("Failed to connect to maintenance database: {e}"))
        })?;

        // Check if the database already exists.
        let row = admin
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT 1 FROM pg_database WHERE datname = $1",
                [db_name.clone().into()],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        if row.is_none() {
            debug!("database \"{db_name}\" does not exist — creating");
            let ddl = format!(r#"CREATE DATABASE "{db_name}""#);
            admin
                .execute_unprepared(&ddl)
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }

        admin.close().await.map_err(|e| {
            VectorDBError::StorageError(format!("Failed to close maintenance connection: {e}"))
        })?;

        Ok(())
    }

    /// Split a database URL into a maintenance URL (pointing at `postgres`)
    /// and the target database name.
    ///
    /// `postgres://user:pass@host:5432/mydb` →
    ///   (`postgres://user:pass@host:5432/postgres`, `"mydb"`)
    fn parse_maintenance_url(raw: &str) -> VectorDBResult<(String, String)> {
        let mut parsed = Url::parse(raw)
            .map_err(|e| VectorDBError::StorageError(format!("invalid database URL: {e}")))?;

        let db_name = parsed.path().trim_start_matches('/').to_string();
        if db_name.is_empty() {
            return Err(VectorDBError::StorageError(
                "database URL must contain a database name".into(),
            ));
        }

        parsed.set_path("/postgres");
        Ok((parsed.to_string(), db_name))
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
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"INSERT INTO _vector_collections (collection_name, data_type, field_name, dimension)
                   VALUES ($1, $2, $3, $4)
                   ON CONFLICT (collection_name) DO NOTHING"#,
                [
                    coll.clone().into(),
                    data_type.to_string().into(),
                    field_name.to_string().into(),
                    (dimension as i32).into(),
                ],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        debug!("created collection {coll} (dim={dimension})");
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let coll = Self::collection_name(data_type, field_name)?;

        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM _vector_collections WHERE collection_name = $1) AS exists",
                [coll.into()],
            ))
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

        // Dimension check.
        let expected_dim = points[0].vector.len();
        for p in points {
            if p.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    expected: expected_dim,
                    actual: p.vector.len(),
                });
            }
        }

        // Batch upsert in chunks to stay within parameter limits.
        for chunk in points.chunks(BATCH_SIZE) {
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

                values.push(pt.id.into());
                values.push(Self::format_vector(&pt.vector).into());
                let metadata_obj: serde_json::Value = serde_json::Value::Object(
                    pt.metadata
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

        Ok(())
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let coll = Self::collection_name(data_type, field_name)?;

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

        Ok(results)
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;

        let ddl = format!(r#"DROP TABLE IF EXISTS "{coll}""#);
        self.db
            .execute_unprepared(&ddl)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM _vector_collections WHERE collection_name = $1",
                [coll.into()],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

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

        let placeholders: String = (1..=point_ids.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(r#"DELETE FROM "{coll}" WHERE id IN ({placeholders})"#);

        let values: Vec<sea_orm::Value> = point_ids.iter().map(|id| (*id).into()).collect();

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let coll = Self::collection_name(data_type, field_name)?;

        let sql = format!(r#"SELECT COUNT(*) AS count FROM "{coll}""#);
        let row = self
            .db
            .query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
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
        let rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT data_type, field_name FROM _vector_collections ORDER BY collection_name"
                    .to_string(),
            ))
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
