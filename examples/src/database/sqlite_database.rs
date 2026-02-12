use super::database_trait::{DatabaseError, DatabaseTrait};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::{Data, Dataset};

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
        // Create datasets table
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

        // Create data table
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

        // Create dataset_data junction table
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
}
