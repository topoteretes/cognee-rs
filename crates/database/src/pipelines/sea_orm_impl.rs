use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, RelationTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::conversions::{domain_status_to_entity, entity_status_to_domain};
use crate::entities::{dataset, pipeline_run, pipeline_run_payload_field, user};
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};
use crate::uuid_hex;

use super::repository::{PipelineRunRepository, PipelineRunRow, PipelineRunWithAttributionRow};

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

    async fn list_recent_with_attribution(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DatabaseError> {
        use sea_orm::JoinType;

        // SeaORM JOIN — uses the relationships defined on the entities. We
        // perform a single LEFT JOIN to `datasets` and a second LEFT JOIN to
        // `users` keyed on `datasets.owner_id`. Orphaned runs (dataset deleted)
        // surface with NULL columns.
        let mut query = pipeline_run::Entity::find()
            .select_only()
            .column(pipeline_run::Column::Id)
            .column(pipeline_run::Column::CreatedAt)
            .column(pipeline_run::Column::Status)
            .column(pipeline_run::Column::PipelineRunId)
            .column(pipeline_run::Column::PipelineName)
            .column(pipeline_run::Column::PipelineId)
            .column(pipeline_run::Column::DatasetId)
            .column_as(dataset::Column::Name, "dataset_name")
            .column_as(dataset::Column::OwnerId, "dataset_owner_id")
            .column_as(user::Column::Email, "owner_email")
            .join(JoinType::LeftJoin, pipeline_run::Relation::Dataset.def())
            .join(
                JoinType::LeftJoin,
                dataset::Entity::belongs_to(user::Entity)
                    .from(dataset::Column::OwnerId)
                    .to(user::Column::Id)
                    .into(),
            )
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .limit(u64::from(limit));

        if let Some(did) = dataset_id {
            query = query.filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(did)));
        }

        // Build the row tuple manually — SeaORM's JOIN needs `into_tuple` /
        // `into_model` to surface the joined columns. We use `into_tuple`
        // mapped to a positional vector of optional strings/types.
        let raw = query
            .into_tuple::<(
                String,
                chrono::DateTime<Utc>,
                pipeline_run::PipelineRunStatus,
                String,
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            )>()
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("list_recent_with_attribution query failed: {e}"))
            })?;

        let mut rows = Vec::with_capacity(raw.len());
        for (
            id_hex,
            created_at,
            status,
            pipeline_run_hex,
            pipeline_name,
            pipeline_id_hex,
            dataset_id_hex,
            dataset_name,
            owner_id_hex,
            owner_email,
        ) in raw
        {
            // `dataset_id` is NOT NULL in our schema, but the LEFT JOIN may
            // produce a row with no dataset attribution (orphaned). Treat
            // empty/zero dataset_id as None so the wire shape matches Python.
            let dataset_uuid = uuid_hex::from_hex(&dataset_id_hex).ok();
            let owner_uuid = owner_id_hex
                .as_deref()
                .and_then(|s| uuid_hex::from_hex(s).ok());
            // Determine dataset attribution presence: when dataset_name is
            // None the LEFT JOIN didn't match (orphan).
            let (dataset_id_field, dataset_name_field) = if dataset_name.is_some() {
                (dataset_uuid, dataset_name)
            } else {
                (dataset_uuid, None)
            };

            rows.push(PipelineRunWithAttributionRow {
                id: uuid_hex::from_hex(&id_hex)
                    .map_err(|e| DatabaseError::QueryError(format!("invalid id hex: {e}")))?,
                created_at,
                status: entity_status_to_domain(status),
                pipeline_run_id: uuid_hex::from_hex(&pipeline_run_hex).map_err(|e| {
                    DatabaseError::QueryError(format!("invalid pipeline_run_id hex: {e}"))
                })?,
                pipeline_name,
                pipeline_id: uuid_hex::from_hex(&pipeline_id_hex).map_err(|e| {
                    DatabaseError::QueryError(format!("invalid pipeline_id hex: {e}"))
                })?,
                dataset_id: dataset_id_field,
                dataset_name: dataset_name_field,
                owner_id: owner_uuid,
                owner_email,
            });
        }
        Ok(rows)
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

    async fn set_payload_field(
        &self,
        run_id: Uuid,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), DatabaseError> {
        use sea_orm::sea_query::OnConflict;

        let now = Utc::now();
        let model = pipeline_run_payload_field::ActiveModel {
            pipeline_run_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(run_id)),
            key: sea_orm::ActiveValue::Set(key.to_owned()),
            value: sea_orm::ActiveValue::Set(value),
            created_at: sea_orm::ActiveValue::Set(now),
            updated_at: sea_orm::ActiveValue::Set(now),
        };

        pipeline_run_payload_field::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([
                    pipeline_run_payload_field::Column::PipelineRunId,
                    pipeline_run_payload_field::Column::Key,
                ])
                .update_columns([
                    pipeline_run_payload_field::Column::Value,
                    pipeline_run_payload_field::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("set_payload_field upsert failed: {e}"))
            })?;
        Ok(())
    }

    async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DatabaseError> {
        let rows = pipeline_run_payload_field::Entity::find()
            .filter(pipeline_run_payload_field::Column::PipelineRunId.eq(uuid_hex::to_hex(run_id)))
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("get_payload query failed: {e}")))?;

        Ok(rows.into_iter().map(|m| (m.key, m.value)).collect())
    }
}
