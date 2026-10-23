use async_trait::async_trait;
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
use sea_orm::{DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set, prelude::*};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::dataset_database;
use crate::traits::{DatasetDatabaseDb, DatasetDatabaseRecord};
use crate::types::DatabaseError;
use crate::uuid_hex;

fn parse_hex(field: &str, value: &str) -> Result<Uuid, DatabaseError> {
    uuid_hex::from_hex(value).map_err(|e| {
        DatabaseError::QueryError(format!("Invalid dataset_database.{field} hex: {e}"))
    })
}

/// A connection-info column read back as an object. Python writes `{}` by
/// default; a `NULL` or non-object value from an older row reads as `{}` too.
fn connection_info(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(_) => value,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

fn model_to_record(m: dataset_database::Model) -> Result<DatasetDatabaseRecord, DatabaseError> {
    Ok(DatasetDatabaseRecord {
        dataset_id: parse_hex("dataset_id", &m.dataset_id)?,
        owner_id: m
            .owner_id
            .as_deref()
            .map(|owner| parse_hex("owner_id", owner))
            .transpose()?,
        graph_database_name: m.graph_database_name,
        vector_database_name: m.vector_database_name,
        graph_database_provider: m.graph_database_provider,
        vector_database_provider: m.vector_database_provider,
        graph_dataset_database_handler: m.graph_dataset_database_handler,
        vector_dataset_database_handler: m.vector_dataset_database_handler,
        graph_database_url: m.graph_database_url,
        vector_database_url: m.vector_database_url,
        graph_database_key: m.graph_database_key,
        vector_database_key: m.vector_database_key,
        graph_database_connection_info: connection_info(m.graph_database_connection_info),
        vector_database_connection_info: connection_info(m.vector_database_connection_info),
        cognee_version: m.cognee_version,
        migration_revision: m.migration_revision,
        migration_last_error: m.migration_last_error,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

fn record_to_active(r: &DatasetDatabaseRecord) -> dataset_database::ActiveModel {
    dataset_database::ActiveModel {
        dataset_id: Set(uuid_hex::to_hex(r.dataset_id)),
        owner_id: Set(uuid_hex::to_hex_opt(r.owner_id)),
        graph_database_name: Set(r.graph_database_name.clone()),
        vector_database_name: Set(r.vector_database_name.clone()),
        graph_database_provider: Set(r.graph_database_provider.clone()),
        vector_database_provider: Set(r.vector_database_provider.clone()),
        graph_dataset_database_handler: Set(r.graph_dataset_database_handler.clone()),
        vector_dataset_database_handler: Set(r.vector_dataset_database_handler.clone()),
        graph_database_url: Set(r.graph_database_url.clone()),
        vector_database_url: Set(r.vector_database_url.clone()),
        graph_database_key: Set(r.graph_database_key.clone()),
        vector_database_key: Set(r.vector_database_key.clone()),
        graph_database_connection_info: Set(r.graph_database_connection_info.clone()),
        vector_database_connection_info: Set(r.vector_database_connection_info.clone()),
        cognee_version: Set(r.cognee_version.clone()),
        migration_revision: Set(r.migration_revision.clone()),
        migration_last_error: Set(r.migration_last_error.clone()),
        created_at: Set(r.created_at),
        updated_at: Set(r.updated_at),
    }
}

#[async_trait]
impl DatasetDatabaseDb for DatabaseConnection {
    #[instrument(
        name = "cognee.db.relational.dataset_database.get",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn get_dataset_database(
        &self,
        dataset_id: Uuid,
    ) -> Result<Option<DatasetDatabaseRecord>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = dataset_database::Entity::find_by_id(uuid_hex::to_hex(dataset_id))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        let record = model.map(model_to_record).transpose()?;
        Span::current().record(COGNEE_DB_ROW_COUNT, i64::from(record.is_some()));
        Ok(record)
    }

    /// `INSERT … ON CONFLICT (dataset_id) DO NOTHING`. SeaORM reports the
    /// do-nothing case as `Ok(0)` or as `DbErr::RecordNotInserted` depending on
    /// backend and version; both mean another caller's row is already there.
    #[instrument(
        name = "cognee.db.relational.dataset_database.insert",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn insert_dataset_database(
        &self,
        record: &DatasetDatabaseRecord,
    ) -> Result<bool, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        match dataset_database::Entity::insert(record_to_active(record))
            .on_conflict(
                OnConflict::column(dataset_database::Column::DatasetId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(self)
            .await
        {
            Ok(rows) => Ok(rows > 0),
            Err(DbErr::RecordNotInserted) => Ok(false),
            Err(e) => Err(map_sea_err(e)),
        }
    }

    #[instrument(
        name = "cognee.db.relational.dataset_database.delete",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_dataset_database(&self, dataset_id: Uuid) -> Result<bool, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let result = dataset_database::Entity::delete_many()
            .filter(dataset_database::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .exec(self)
            .await
            .map_err(map_sea_err)?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            i64::try_from(result.rows_affected).unwrap_or(i64::MAX),
        );
        Ok(result.rows_affected > 0)
    }

    #[instrument(
        name = "cognee.db.relational.dataset_database.list",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn list_dataset_databases(&self) -> Result<Vec<DatasetDatabaseRecord>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let models = dataset_database::Entity::find()
            .all(self)
            .await
            .map_err(map_sea_err)?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            i64::try_from(models.len()).unwrap_or(i64::MAX),
        );
        models.into_iter().map(model_to_record).collect()
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use chrono::Utc;
    use cognee_models::Dataset;
    use serde_json::json;

    use super::*;
    use crate::IngestDb;

    async fn db_with_dataset() -> (DatabaseConnection, Uuid, Uuid) {
        let db = crate::connect("sqlite::memory:").await.unwrap();
        crate::initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let dataset = Dataset::new("ds".to_string(), owner, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        IngestDb::create_dataset(&db, dataset).await.unwrap();
        (db, dataset_id, owner)
    }

    fn record(dataset_id: Uuid, owner: Uuid, graph_name: &str) -> DatasetDatabaseRecord {
        DatasetDatabaseRecord {
            dataset_id,
            owner_id: Some(owner),
            graph_database_name: graph_name.to_string(),
            vector_database_name: format!("{dataset_id}.lance.db"),
            graph_database_provider: "ladybug".to_string(),
            vector_database_provider: "lancedb".to_string(),
            graph_dataset_database_handler: "ladybug".to_string(),
            vector_dataset_database_handler: "lancedb".to_string(),
            graph_database_url: None,
            vector_database_url: Some("/tmp/x.lance.db".to_string()),
            graph_database_key: None,
            vector_database_key: None,
            graph_database_connection_info: json!({}),
            vector_database_connection_info: json!({"embedding_dimensions": 384}),
            cognee_version: Some("test".to_string()),
            migration_revision: None,
            migration_last_error: None,
            created_at: Some(Utc::now()),
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn insert_get_list_delete_round_trip() {
        let (db, dataset_id, owner) = db_with_dataset().await;
        assert!(db.get_dataset_database(dataset_id).await.unwrap().is_none());

        let row = record(dataset_id, owner, "a.lbug");
        assert!(db.insert_dataset_database(&row).await.unwrap());
        let read = db.get_dataset_database(dataset_id).await.unwrap().unwrap();
        assert_eq!(read.graph_database_name, "a.lbug");
        assert_eq!(read.owner_id, Some(owner));
        assert_eq!(
            read.vector_database_connection_info,
            json!({"embedding_dimensions": 384})
        );
        assert_eq!(db.list_dataset_databases().await.unwrap().len(), 1);

        assert!(db.delete_dataset_database(dataset_id).await.unwrap());
        assert!(!db.delete_dataset_database(dataset_id).await.unwrap());
        assert!(db.get_dataset_database(dataset_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_second_insert_loses_and_keeps_the_first_row() {
        let (db, dataset_id, owner) = db_with_dataset().await;
        assert!(
            db.insert_dataset_database(&record(dataset_id, owner, "first"))
                .await
                .unwrap()
        );
        assert!(
            !db.insert_dataset_database(&record(dataset_id, owner, "second"))
                .await
                .unwrap()
        );
        let read = db.get_dataset_database(dataset_id).await.unwrap().unwrap();
        assert_eq!(read.graph_database_name, "first");
    }
}
