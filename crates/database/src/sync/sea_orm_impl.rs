//! SeaORM-backed [`SyncOperationRepository`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use uuid::Uuid;

use crate::entities::sync_operation;
use crate::types::DatabaseError;
use crate::uuid_hex;

use super::repository::{SyncOperationRepository, SyncOperationRow, SyncOperationStatus};

/// SeaORM impl of [`SyncOperationRepository`]. Cheap to clone (interior `Arc`).
#[derive(Clone)]
pub struct SeaOrmSyncOperationRepository {
    db: Arc<DatabaseConnection>,
}

impl SeaOrmSyncOperationRepository {
    /// Build a new repository wrapping the supplied connection.
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

fn parse_uuid_list(json: Option<&serde_json::Value>) -> Vec<Uuid> {
    match json {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_string_list(json: Option<&serde_json::Value>) -> Vec<String> {
    match json {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

fn row_from_model(m: sync_operation::Model) -> Result<SyncOperationRow, DatabaseError> {
    let id = uuid_hex::from_hex(&m.id)
        .map_err(|e| DatabaseError::QueryError(format!("invalid sync_operations.id: {e}")))?;
    let user_id = uuid_hex::from_hex(&m.user_id)
        .map_err(|e| DatabaseError::QueryError(format!("invalid sync_operations.user_id: {e}")))?;
    let dataset_ids = parse_uuid_list(m.dataset_ids.as_ref());
    let dataset_names = parse_string_list(m.dataset_names.as_ref());
    Ok(SyncOperationRow {
        id,
        run_id: m.run_id,
        status: m.status,
        progress_percentage: m.progress_percentage.max(0) as u32,
        dataset_ids,
        dataset_names,
        user_id,
        created_at: m.created_at,
        started_at: m.started_at,
        completed_at: m.completed_at,
        total_records_to_sync: m.total_records_to_sync,
        total_records_to_download: m.total_records_to_download,
        total_records_to_upload: m.total_records_to_upload,
        records_downloaded: m.records_downloaded,
        records_uploaded: m.records_uploaded,
        bytes_downloaded: m.bytes_downloaded,
        bytes_uploaded: m.bytes_uploaded,
        dataset_sync_hashes: m.dataset_sync_hashes,
        error_message: m.error_message,
        retry_count: m.retry_count,
    })
}

async fn fetch_by_run_id(
    db: &DatabaseConnection,
    run_id: &str,
) -> Result<Option<sync_operation::Model>, DatabaseError> {
    sync_operation::Entity::find()
        .filter(sync_operation::Column::RunId.eq(run_id))
        .one(db)
        .await
        .map_err(|e| DatabaseError::QueryError(format!("sync_operations lookup failed: {e}")))
}

#[async_trait]
impl SyncOperationRepository for SeaOrmSyncOperationRepository {
    async fn create_operation(
        &self,
        run_id: &str,
        dataset_ids: &[Uuid],
        dataset_names: &[String],
        user_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let row_id = uuid_hex::to_hex(Uuid::new_v4());
        let dataset_ids_json = serde_json::Value::Array(
            dataset_ids
                .iter()
                .map(|u| serde_json::Value::String(u.to_string()))
                .collect(),
        );
        let dataset_names_json = serde_json::Value::Array(
            dataset_names
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        );
        let am = sync_operation::ActiveModel {
            id: Set(row_id),
            run_id: Set(run_id.to_string()),
            status: Set(SyncOperationStatus::Started.as_str().to_string()),
            progress_percentage: Set(0),
            dataset_ids: Set(Some(dataset_ids_json)),
            dataset_names: Set(Some(dataset_names_json)),
            user_id: Set(uuid_hex::to_hex(user_id)),
            created_at: Set(Utc::now()),
            started_at: Set(None),
            completed_at: Set(None),
            total_records_to_sync: Set(None),
            total_records_to_download: Set(None),
            total_records_to_upload: Set(None),
            records_downloaded: Set(0),
            records_uploaded: Set(0),
            bytes_downloaded: Set(0),
            bytes_uploaded: Set(0),
            dataset_sync_hashes: Set(None),
            error_message: Set(None),
            retry_count: Set(0),
        };
        sync_operation::Entity::insert(am)
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("create_operation insert failed: {e}"))
            })?;
        Ok(())
    }

    async fn mark_started(&self, run_id: &str) -> Result<(), DatabaseError> {
        let Some(row) = fetch_by_run_id(self.db.as_ref(), run_id).await? else {
            return Err(DatabaseError::NotFound(format!(
                "sync_operations row not found: {run_id}"
            )));
        };
        let mut am: sync_operation::ActiveModel = row.into();
        am.status = Set(SyncOperationStatus::InProgress.as_str().to_string());
        am.started_at = Set(Some(Utc::now()));
        am.update(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("mark_started update failed: {e}")))?;
        Ok(())
    }

    async fn mark_completed(
        &self,
        run_id: &str,
        records_uploaded: i32,
        records_downloaded: i32,
        bytes_uploaded: i64,
        bytes_downloaded: i64,
        dataset_sync_hashes: Option<serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        let Some(row) = fetch_by_run_id(self.db.as_ref(), run_id).await? else {
            return Err(DatabaseError::NotFound(format!(
                "sync_operations row not found: {run_id}"
            )));
        };
        let mut am: sync_operation::ActiveModel = row.into();
        am.status = Set(SyncOperationStatus::Completed.as_str().to_string());
        am.progress_percentage = Set(100);
        am.completed_at = Set(Some(Utc::now()));
        am.records_uploaded = Set(records_uploaded);
        am.records_downloaded = Set(records_downloaded);
        am.bytes_uploaded = Set(bytes_uploaded);
        am.bytes_downloaded = Set(bytes_downloaded);
        am.dataset_sync_hashes = Set(dataset_sync_hashes);
        am.update(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("mark_completed update failed: {e}")))?;
        Ok(())
    }

    async fn mark_failed(&self, run_id: &str, error_message: &str) -> Result<(), DatabaseError> {
        let Some(row) = fetch_by_run_id(self.db.as_ref(), run_id).await? else {
            return Err(DatabaseError::NotFound(format!(
                "sync_operations row not found: {run_id}"
            )));
        };
        let mut am: sync_operation::ActiveModel = row.into();
        am.status = Set(SyncOperationStatus::Failed.as_str().to_string());
        am.completed_at = Set(Some(Utc::now()));
        am.error_message = Set(Some(error_message.to_string()));
        am.update(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("mark_failed update failed: {e}")))?;
        Ok(())
    }

    async fn update_progress(&self, run_id: &str, percent: u32) -> Result<(), DatabaseError> {
        let Some(row) = fetch_by_run_id(self.db.as_ref(), run_id).await? else {
            return Err(DatabaseError::NotFound(format!(
                "sync_operations row not found: {run_id}"
            )));
        };
        let mut am: sync_operation::ActiveModel = row.into();
        am.progress_percentage = Set(percent.min(100) as i32);
        am.update(self.db.as_ref()).await.map_err(|e| {
            DatabaseError::QueryError(format!("update_progress update failed: {e}"))
        })?;
        Ok(())
    }

    async fn running_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<SyncOperationRow>, DatabaseError> {
        let user_hex = uuid_hex::to_hex(user_id);
        let rows = sync_operation::Entity::find()
            .filter(sync_operation::Column::UserId.eq(user_hex))
            .filter(sync_operation::Column::Status.is_in([
                SyncOperationStatus::Started.as_str(),
                SyncOperationStatus::InProgress.as_str(),
            ]))
            .order_by_desc(sync_operation::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("running_for_user failed: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row_from_model(row)?);
        }
        Ok(out)
    }

    async fn get_by_run_id(&self, run_id: &str) -> Result<Option<SyncOperationRow>, DatabaseError> {
        match fetch_by_run_id(self.db.as_ref(), run_id).await? {
            Some(model) => Ok(Some(row_from_model(model)?)),
            None => Ok(None),
        }
    }
}
