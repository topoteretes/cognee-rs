use super::database_trait::{
    ArtifactReference, DatabaseError, DatabaseTrait, SearchHistoryEntry, SearchHistoryEntryType,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use cognee_models::{Data, Dataset};

pub struct SqliteDatabase {
    pool: SqlitePool,
}

impl SqliteDatabase {
    pub async fn new(database_url: &str) -> Result<Self, DatabaseError> {
        let pool = SqlitePool::connect(database_url).await?;

        Ok(Self { pool })
    }

    /// Parse DateTime from SQLite TEXT format (ISO 8601)
    fn parse_datetime(s: &str) -> Result<DateTime<Utc>, DatabaseError> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| DatabaseError::QueryError(format!("Failed to parse datetime: {}", e)))
    }
}

impl From<sqlx::Error> for DatabaseError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::RowNotFound => DatabaseError::NotFound("Row not found".to_string()),
            sqlx::Error::Database(db_err) => {
                let err_str = db_err.to_string();
                if err_str.contains("UNIQUE constraint failed") {
                    DatabaseError::UniqueViolation(err_str)
                } else {
                    DatabaseError::QueryError(err_str)
                }
            }
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
                DatabaseError::ConnectionError(error.to_string())
            }
            _ => DatabaseError::QueryError(error.to_string()),
        }
    }
}

#[async_trait]
impl DatabaseTrait for SqliteDatabase {
    async fn initialize(&self) -> Result<(), DatabaseError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS datasets (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS data (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                raw_data_location TEXT NOT NULL,
                original_data_location TEXT NOT NULL,
                extension TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS dataset_data (
                dataset_id TEXT NOT NULL,
                data_id TEXT NOT NULL,
                PRIMARY KEY (dataset_id, data_id),
                FOREIGN KEY (dataset_id) REFERENCES datasets(id) ON DELETE CASCADE,
                FOREIGN KEY (data_id) REFERENCES data(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS queries (
                id TEXT PRIMARY KEY,
                query_text TEXT NOT NULL,
                query_type TEXT NOT NULL,
                user_id TEXT,
                created_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS results (
                id TEXT PRIMARY KEY,
                query_id TEXT NOT NULL,
                serialized_result TEXT NOT NULL,
                user_id TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (query_id) REFERENCES queries(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS artifact_references (
                id TEXT PRIMARY KEY,
                owner_id TEXT NOT NULL,
                dataset_id TEXT NOT NULL,
                data_id TEXT,
                artifact_kind TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                collection_name TEXT,
                created_at TEXT NOT NULL,
                UNIQUE(dataset_id, data_id, artifact_kind, artifact_id, collection_name),
                FOREIGN KEY (dataset_id) REFERENCES datasets(id) ON DELETE CASCADE,
                FOREIGN KEY (data_id) REFERENCES data(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn create_data(&self, data: Data) -> Result<Data, DatabaseError> {
        let id = data.id.to_string();
        let owner_id = data.owner_id.to_string();
        let created_at = data.created_at.to_rfc3339();
        let updated_at = data.updated_at.map(|dt| dt.to_rfc3339());

        sqlx::query(
            r#"
            INSERT INTO data (id, name, raw_data_location, original_data_location,
                            extension, mime_type, content_hash, owner_id, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )
        .bind(&id)
        .bind(&data.name)
        .bind(&data.raw_data_location)
        .bind(&data.original_data_location)
        .bind(&data.extension)
        .bind(&data.mime_type)
        .bind(&data.content_hash)
        .bind(&owner_id)
        .bind(&created_at)
        .bind(&updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE constraint failed") {
                DatabaseError::UniqueViolation(format!("Data with id {} already exists", id))
            } else {
                DatabaseError::QueryError(e.to_string())
            }
        })?;

        Ok(data)
    }

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        let id_str = id.to_string();

        let row = sqlx::query(
            r#"
            SELECT id, name, raw_data_location, original_data_location,
                   extension, mime_type, content_hash, owner_id, created_at, updated_at
            FROM data
            WHERE id = ?1
            "#,
        )
        .bind(&id_str)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let id: String = row.get("id");
                let owner_id: String = row.get("owner_id");
                let created_at: String = row.get("created_at");
                let updated_at: Option<String> = row.get("updated_at");

                Ok(Some(Data {
                    id: Uuid::parse_str(&id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    name: row.get("name"),
                    raw_data_location: row.get("raw_data_location"),
                    original_data_location: row.get("original_data_location"),
                    extension: row.get("extension"),
                    mime_type: row.get("mime_type"),
                    content_hash: row.get("content_hash"),
                    owner_id: Uuid::parse_str(&owner_id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    created_at: Self::parse_datetime(&created_at)?,
                    updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
                }))
            }
            None => Ok(None),
        }
    }

    async fn delete_data(&self, id: Uuid) -> Result<(), DatabaseError> {
        let id_str = id.to_string();

        sqlx::query(
            r#"
            DELETE FROM data
            WHERE id = ?1
            "#,
        )
        .bind(&id_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn update_data(&self, data: Data) -> Result<Data, DatabaseError> {
        let id = data.id.to_string();
        let owner_id = data.owner_id.to_string();
        let created_at = data.created_at.to_rfc3339();
        let updated_at = Some(Utc::now().to_rfc3339());

        sqlx::query(
            r#"
            UPDATE data
            SET name = ?2, raw_data_location = ?3, original_data_location = ?4,
                extension = ?5, mime_type = ?6, content_hash = ?7, owner_id = ?8,
                created_at = ?9, updated_at = ?10
            WHERE id = ?1
            "#,
        )
        .bind(&id)
        .bind(&data.name)
        .bind(&data.raw_data_location)
        .bind(&data.original_data_location)
        .bind(&data.extension)
        .bind(&data.mime_type)
        .bind(&data.content_hash)
        .bind(&owner_id)
        .bind(&created_at)
        .bind(&updated_at)
        .execute(&self.pool)
        .await?;

        let mut updated_data = data;
        updated_data.updated_at = updated_at.map(|s| {
            DateTime::parse_from_rfc3339(&s)
                .unwrap()
                .with_timezone(&Utc)
        });

        Ok(updated_data)
    }

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
        let dataset_id_str = dataset_id.to_string();

        let rows = sqlx::query(
            r#"
            SELECT d.id, d.name, d.raw_data_location, d.original_data_location,
                   d.extension, d.mime_type, d.content_hash, d.owner_id, d.created_at, d.updated_at
            FROM data d
            INNER JOIN dataset_data dd ON d.id = dd.data_id
            WHERE dd.dataset_id = ?1
            "#,
        )
        .bind(&dataset_id_str)
        .fetch_all(&self.pool)
        .await?;

        let mut data_list = Vec::new();

        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let created_at: String = row.get("created_at");
            let updated_at: Option<String> = row.get("updated_at");

            data_list.push(Data {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                name: row.get("name"),
                raw_data_location: row.get("raw_data_location"),
                original_data_location: row.get("original_data_location"),
                extension: row.get("extension"),
                mime_type: row.get("mime_type"),
                content_hash: row.get("content_hash"),
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                created_at: Self::parse_datetime(&created_at)?,
                updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
            });
        }

        Ok(data_list)
    }

    async fn count_data_dataset_links(&self, data_id: Uuid) -> Result<usize, DatabaseError> {
        let data_id_str = data_id.to_string();
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS count
            FROM dataset_data
            WHERE data_id = ?1
            "#,
        )
        .bind(&data_id_str)
        .fetch_one(&self.pool)
        .await?;

        let count: i64 = row.get("count");
        Ok(count as usize)
    }

    async fn list_datasets_for_data(&self, data_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        let data_id_str = data_id.to_string();

        let rows = sqlx::query(
            r#"
            SELECT ds.id, ds.name, ds.owner_id, ds.created_at, ds.updated_at
            FROM datasets ds
            INNER JOIN dataset_data dd ON ds.id = dd.dataset_id
            WHERE dd.data_id = ?1
            ORDER BY ds.created_at ASC
            "#,
        )
        .bind(&data_id_str)
        .fetch_all(&self.pool)
        .await?;

        let mut datasets = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let created_at: String = row.get("created_at");
            let updated_at: Option<String> = row.get("updated_at");

            datasets.push(Dataset {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                name: row.get("name"),
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                created_at: Self::parse_datetime(&created_at)?,
                updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
            });
        }

        Ok(datasets)
    }

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError> {
        let id = dataset.id.to_string();
        let owner_id = dataset.owner_id.to_string();
        let created_at = dataset.created_at.to_rfc3339();
        let updated_at = dataset.updated_at.map(|dt| dt.to_rfc3339());

        sqlx::query(
            r#"
            INSERT INTO datasets (id, name, owner_id, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(&id)
        .bind(&dataset.name)
        .bind(&owner_id)
        .bind(&created_at)
        .bind(&updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE constraint failed") {
                DatabaseError::UniqueViolation(format!("Dataset with id {} already exists", id))
            } else {
                DatabaseError::QueryError(e.to_string())
            }
        })?;

        Ok(dataset)
    }

    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        let id_str = id.to_string();

        let row = sqlx::query(
            r#"
            SELECT id, name, owner_id, created_at, updated_at
            FROM datasets
            WHERE id = ?1
            "#,
        )
        .bind(&id_str)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let id: String = row.get("id");
                let owner_id: String = row.get("owner_id");
                let created_at: String = row.get("created_at");
                let updated_at: Option<String> = row.get("updated_at");

                Ok(Some(Dataset {
                    id: Uuid::parse_str(&id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    name: row.get("name"),
                    owner_id: Uuid::parse_str(&owner_id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    created_at: Self::parse_datetime(&created_at)?,
                    updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
                }))
            }
            None => Ok(None),
        }
    }

    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
    ) -> Result<Option<Dataset>, DatabaseError> {
        let owner_id_str = owner_id.to_string();

        let row = sqlx::query(
            r#"
            SELECT id, name, owner_id, created_at, updated_at
            FROM datasets
            WHERE name = ?1 AND owner_id = ?2
            "#,
        )
        .bind(name)
        .bind(&owner_id_str)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let id: String = row.get("id");
                let owner_id: String = row.get("owner_id");
                let created_at: String = row.get("created_at");
                let updated_at: Option<String> = row.get("updated_at");

                Ok(Some(Dataset {
                    id: Uuid::parse_str(&id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    name: row.get("name"),
                    owner_id: Uuid::parse_str(&owner_id)
                        .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                    created_at: Self::parse_datetime(&created_at)?,
                    updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
                }))
            }
            None => Ok(None),
        }
    }

    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        let owner_id_str = owner_id.to_string();

        let rows = sqlx::query(
            r#"
            SELECT id, name, owner_id, created_at, updated_at
            FROM datasets
            WHERE owner_id = ?1
            ORDER BY created_at ASC
            "#,
        )
        .bind(&owner_id_str)
        .fetch_all(&self.pool)
        .await?;

        let mut datasets = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let created_at: String = row.get("created_at");
            let updated_at: Option<String> = row.get("updated_at");

            datasets.push(Dataset {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                name: row.get("name"),
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                created_at: Self::parse_datetime(&created_at)?,
                updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
            });
        }

        Ok(datasets)
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, owner_id, created_at, updated_at
            FROM datasets
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut datasets = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let created_at: String = row.get("created_at");
            let updated_at: Option<String> = row.get("updated_at");

            datasets.push(Dataset {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                name: row.get("name"),
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                created_at: Self::parse_datetime(&created_at)?,
                updated_at: updated_at.map(|s| Self::parse_datetime(&s)).transpose()?,
            });
        }

        Ok(datasets)
    }

    async fn delete_dataset(&self, id: Uuid) -> Result<(), DatabaseError> {
        let id_str = id.to_string();

        sqlx::query(
            r#"
            DELETE FROM datasets
            WHERE id = ?1
            "#,
        )
        .bind(&id_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let dataset_id_str = dataset_id.to_string();
        let data_id_str = data_id.to_string();

        sqlx::query(
            r#"
            INSERT OR IGNORE INTO dataset_data (dataset_id, data_id)
            VALUES (?1, ?2)
            "#,
        )
        .bind(&dataset_id_str)
        .bind(&data_id_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn detach_data_from_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let dataset_id_str = dataset_id.to_string();
        let data_id_str = data_id.to_string();

        sqlx::query(
            r#"
            DELETE FROM dataset_data
            WHERE dataset_id = ?1 AND data_id = ?2
            "#,
        )
        .bind(&dataset_id_str)
        .bind(&data_id_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn upsert_artifact_references(
        &self,
        references: &[ArtifactReference],
    ) -> Result<(), DatabaseError> {
        for reference in references {
            sqlx::query(
                r#"
                INSERT OR IGNORE INTO artifact_references (
                    id, owner_id, dataset_id, data_id, artifact_kind, artifact_id, collection_name, created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )
            .bind(reference.id.to_string())
            .bind(reference.owner_id.to_string())
            .bind(reference.dataset_id.to_string())
            .bind(reference.data_id.map(|id| id.to_string()))
            .bind(&reference.artifact_kind)
            .bind(&reference.artifact_id)
            .bind(&reference.collection_name)
            .bind(reference.created_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    async fn list_artifact_references_for_data(
        &self,
        data_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError> {
        let rows = sqlx::query(
            r#"
            SELECT id, owner_id, dataset_id, data_id, artifact_kind, artifact_id, collection_name, created_at
            FROM artifact_references
            WHERE data_id = ?1
            ORDER BY created_at ASC
            "#,
        )
        .bind(data_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut references = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let dataset_id: String = row.get("dataset_id");
            let data_id_value: Option<String> = row.get("data_id");
            let created_at: String = row.get("created_at");

            references.push(ArtifactReference {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                dataset_id: Uuid::parse_str(&dataset_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                data_id: data_id_value
                    .map(|value| {
                        Uuid::parse_str(&value)
                            .map_err(|e| DatabaseError::QueryError(e.to_string()))
                    })
                    .transpose()?,
                artifact_kind: row.get("artifact_kind"),
                artifact_id: row.get("artifact_id"),
                collection_name: row.get("collection_name"),
                created_at: Self::parse_datetime(&created_at)?,
            });
        }

        Ok(references)
    }

    async fn list_artifact_references_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError> {
        let rows = sqlx::query(
            r#"
            SELECT id, owner_id, dataset_id, data_id, artifact_kind, artifact_id, collection_name, created_at
            FROM artifact_references
            WHERE dataset_id = ?1
            ORDER BY created_at ASC
            "#,
        )
        .bind(dataset_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut references = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            let owner_id: String = row.get("owner_id");
            let dataset_id_value: String = row.get("dataset_id");
            let data_id_value: Option<String> = row.get("data_id");
            let created_at: String = row.get("created_at");

            references.push(ArtifactReference {
                id: Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                owner_id: Uuid::parse_str(&owner_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                dataset_id: Uuid::parse_str(&dataset_id_value)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                data_id: data_id_value
                    .map(|value| {
                        Uuid::parse_str(&value)
                            .map_err(|e| DatabaseError::QueryError(e.to_string()))
                    })
                    .transpose()?,
                artifact_kind: row.get("artifact_kind"),
                artifact_id: row.get("artifact_id"),
                collection_name: row.get("collection_name"),
                created_at: Self::parse_datetime(&created_at)?,
            });
        }

        Ok(references)
    }

    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        let query_id = Uuid::new_v4();
        let created_at = Utc::now().to_rfc3339();
        let user_id_str = user_id.map(|id| id.to_string());

        sqlx::query(
            r#"
            INSERT INTO queries (id, query_text, query_type, user_id, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(query_id.to_string())
        .bind(query_text)
        .bind(query_type)
        .bind(user_id_str)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(query_id)
    }

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        let result_id = Uuid::new_v4();
        let created_at = Utc::now().to_rfc3339();
        let user_id_str = user_id.map(|id| id.to_string());

        sqlx::query(
            r#"
            INSERT INTO results (id, query_id, serialized_result, user_id, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(result_id.to_string())
        .bind(query_id.to_string())
        .bind(serialized_result)
        .bind(user_id_str)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(result_id)
    }

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
        let user_id_str = user_id.map(|id| id.to_string());
        let history_limit = limit.unwrap_or(100) as i64;

        let rows = if let Some(user_id) = user_id_str {
            sqlx::query(
                r#"
                SELECT
                    q.id AS entry_id,
                    q.id AS query_id,
                    'query' AS entry_type,
                    q.query_text AS content,
                    q.query_type AS query_type,
                    q.user_id AS user_id,
                    q.created_at AS created_at
                FROM queries q
                WHERE q.user_id = ?1
                UNION ALL
                SELECT
                    r.id AS entry_id,
                    r.query_id AS query_id,
                    'result' AS entry_type,
                    r.serialized_result AS content,
                    q.query_type AS query_type,
                    r.user_id AS user_id,
                    r.created_at AS created_at
                FROM results r
                INNER JOIN queries q ON q.id = r.query_id
                WHERE r.user_id = ?1
                ORDER BY created_at DESC
                LIMIT ?2
                "#,
            )
            .bind(user_id)
            .bind(history_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    q.id AS entry_id,
                    q.id AS query_id,
                    'query' AS entry_type,
                    q.query_text AS content,
                    q.query_type AS query_type,
                    q.user_id AS user_id,
                    q.created_at AS created_at
                FROM queries q
                UNION ALL
                SELECT
                    r.id AS entry_id,
                    r.query_id AS query_id,
                    'result' AS entry_type,
                    r.serialized_result AS content,
                    q.query_type AS query_type,
                    r.user_id AS user_id,
                    r.created_at AS created_at
                FROM results r
                INNER JOIN queries q ON q.id = r.query_id
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .bind(history_limit)
            .fetch_all(&self.pool)
            .await?
        };

        let mut history_entries = Vec::with_capacity(rows.len());
        for row in rows {
            let entry_id: String = row.get("entry_id");
            let query_id: String = row.get("query_id");
            let entry_type: String = row.get("entry_type");
            let created_at: String = row.get("created_at");
            let user_id_raw: Option<String> = row.get("user_id");

            history_entries.push(SearchHistoryEntry {
                entry_id: Uuid::parse_str(&entry_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                query_id: Uuid::parse_str(&query_id)
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?,
                entry_type: if entry_type == "query" {
                    SearchHistoryEntryType::Query
                } else {
                    SearchHistoryEntryType::Result
                },
                content: row.get("content"),
                query_type: row.get("query_type"),
                user_id: user_id_raw
                    .map(|id| {
                        Uuid::parse_str(&id).map_err(|e| DatabaseError::QueryError(e.to_string()))
                    })
                    .transpose()?,
                created_at: Self::parse_datetime(&created_at)?,
            });
        }

        Ok(history_entries)
    }
}
