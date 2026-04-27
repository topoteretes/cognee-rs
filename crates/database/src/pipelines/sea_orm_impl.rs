use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde_json::json;
use uuid::Uuid;

use crate::conversions::domain_status_to_entity;
use crate::entities::pipeline_run;
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};
use crate::uuid_hex;

use super::repository::{PipelineRunRepository, PipelineRunRow};

/// SeaORM-backed implementation of [`PipelineRunRepository`].
///
/// Wraps a shared `DatabaseConnection`. All methods write or query the
/// `pipeline_runs` table using the "new row per status transition" pattern,
/// matching both Python's writing pattern and the cross-SDK audit trail
/// requirement.
pub struct SeaOrmPipelineRunRepository {
    db: Arc<DatabaseConnection>,
}

impl SeaOrmPipelineRunRepository {
    /// Create a new repository backed by the given database connection.
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PipelineRunRepository for SeaOrmPipelineRunRepository {
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        pipeline_id: Uuid,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        status: PipelineRunStatus,
        run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DatabaseError> {
        let row_id = Uuid::new_v4();

        // The `pipeline_runs` table has a NOT NULL FK to `datasets.id`.
        // When `dataset_id` is `None` (ad-hoc run without a dataset), we skip
        // the durable write and return the generated id. Ad-hoc runs are tracked
        // in-memory only; callers that need persistence must supply a valid dataset_id.
        let dataset_id_val = match dataset_id {
            Some(id) => id,
            None => return Ok(row_id),
        };

        let active = pipeline_run::ActiveModel {
            id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(row_id)),
            created_at: sea_orm::ActiveValue::Set(Utc::now()),
            status: sea_orm::ActiveValue::Set(domain_status_to_entity(status)),
            pipeline_run_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(pipeline_run_id)),
            pipeline_name: sea_orm::ActiveValue::Set(pipeline_name.to_string()),
            pipeline_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(pipeline_id)),
            dataset_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(dataset_id_val)),
            run_info: sea_orm::ActiveValue::Set(run_info),
        };

        active.insert(self.db.as_ref()).await.map_err(|e| {
            DatabaseError::QueryError(format!("log_pipeline_run insert failed: {e}"))
        })?;

        Ok(row_id)
    }

    async fn latest_status(
        &self,
        dataset_ids: &[Uuid],
        pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DatabaseError> {
        if dataset_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let hex_ids: Vec<String> = dataset_ids.iter().map(|id| uuid_hex::to_hex(*id)).collect();

        // Fetch all matching rows, ordered by created_at DESC.
        // We then pick the first (most recent) per dataset_id.
        let rows = pipeline_run::Entity::find()
            .filter(pipeline_run::Column::PipelineName.eq(pipeline_name))
            .filter(pipeline_run::Column::DatasetId.is_in(hex_ids))
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("latest_status query failed: {e}")))?;

        let mut result: HashMap<Uuid, PipelineRunStatus> = HashMap::new();
        for row in rows {
            let run: PipelineRun = row.into();
            // Only keep the first (most recent) entry per dataset_id.
            result.entry(run.dataset_id).or_insert(run.status);
        }

        Ok(result)
    }

    async fn list_recent(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DatabaseError> {
        let mut query = pipeline_run::Entity::find()
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .limit(u64::from(limit));

        if let Some(did) = dataset_id {
            query = query.filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(did)));
        }

        let rows = query
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("list_recent query failed: {e}")))?;

        Ok(rows.into_iter().map(PipelineRun::from).collect())
    }

    async fn reset_orphans(&self, reason: &str) -> Result<u64, DatabaseError> {
        // Find all pipeline_run_ids that have INITIATED or STARTED status
        // and do NOT have a more recent COMPLETED or ERRORED row with the same
        // pipeline_run_id. We implement this by fetching the latest row per
        // pipeline_run_id and checking its status.
        //
        // Strategy: fetch all rows ordered by (pipeline_run_id, created_at DESC),
        // then for each unique pipeline_run_id, check if the latest row is stuck.

        let all_rows = pipeline_run::Entity::find()
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("reset_orphans fetch failed: {e}")))?;

        // Collect the latest row per pipeline_run_id.
        let mut latest_per_run: HashMap<String, pipeline_run::Model> = HashMap::new();
        for row in all_rows {
            latest_per_run
                .entry(row.pipeline_run_id.clone())
                .or_insert(row);
        }

        // Find rows that are stuck in INITIATED or STARTED.
        let orphan_ids: Vec<String> = latest_per_run
            .into_values()
            .filter(|row| {
                matches!(
                    row.status,
                    pipeline_run::PipelineRunStatus::Initiated
                        | pipeline_run::PipelineRunStatus::Started
                )
            })
            .map(|row| row.id)
            .collect();

        if orphan_ids.is_empty() {
            return Ok(0);
        }

        // Write new ERRORED rows for each orphan (new-row-per-transition pattern).
        let reason_info = json!({"reason": reason});
        let mut count = 0u64;
        for orphan_id in &orphan_ids {
            // Fetch the orphan row to get all its fields.
            let orphan_opt = pipeline_run::Entity::find_by_id(orphan_id.clone())
                .one(self.db.as_ref())
                .await
                .map_err(|e| {
                    DatabaseError::QueryError(format!("reset_orphans fetch orphan failed: {e}"))
                })?;

            if let Some(orphan) = orphan_opt {
                let new_id = Uuid::new_v4();
                let active = pipeline_run::ActiveModel {
                    id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(new_id)),
                    created_at: sea_orm::ActiveValue::Set(Utc::now()),
                    status: sea_orm::ActiveValue::Set(pipeline_run::PipelineRunStatus::Errored),
                    pipeline_run_id: sea_orm::ActiveValue::Set(orphan.pipeline_run_id),
                    pipeline_name: sea_orm::ActiveValue::Set(orphan.pipeline_name),
                    pipeline_id: sea_orm::ActiveValue::Set(orphan.pipeline_id),
                    dataset_id: sea_orm::ActiveValue::Set(orphan.dataset_id),
                    run_info: sea_orm::ActiveValue::Set(Some(reason_info.clone())),
                };
                active.insert(self.db.as_ref()).await.map_err(|e| {
                    DatabaseError::QueryError(format!("reset_orphans insert failed: {e}"))
                })?;
                count += 1;
            }
        }

        Ok(count)
    }
}
