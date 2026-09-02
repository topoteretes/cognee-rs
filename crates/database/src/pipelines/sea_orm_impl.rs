use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use cognee_utils::sanitize::sanitize_json;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, RelationTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::conversions::{domain_status_to_entity, entity_status_to_domain};
use crate::entities::{dataset, pipeline_run, pipeline_run_claim, pipeline_run_payload_field};
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
    /// `INSERT ... ON CONFLICT DO NOTHING`, returning whether the row was
    /// written. `false` means a claim already exists for the pair.
    ///
    /// SeaORM signals a do-nothing conflict either as `Ok(0)` rows affected or
    /// as `DbErr::RecordNotInserted` depending on backend and version; both
    /// mean the same thing here.
    async fn insert_claim(
        &self,
        dataset_hex: &str,
        pipeline_name: &str,
        claim_id: Uuid,
    ) -> Result<bool, DatabaseError> {
        let row = pipeline_run_claim::ActiveModel {
            dataset_id: sea_orm::ActiveValue::Set(dataset_hex.to_string()),
            pipeline_name: sea_orm::ActiveValue::Set(pipeline_name.to_string()),
            claim_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(claim_id)),
            claimed_at: sea_orm::ActiveValue::Set(Utc::now()),
        };

        match pipeline_run_claim::Entity::insert(row)
            .on_conflict(
                OnConflict::columns([
                    pipeline_run_claim::Column::DatasetId,
                    pipeline_run_claim::Column::PipelineName,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(self.db.as_ref())
            .await
        {
            Ok(rows) => Ok(rows > 0),
            Err(DbErr::RecordNotInserted) => Ok(false),
            Err(e) => Err(DatabaseError::QueryError(format!(
                "insert pipeline_run_claim failed: {e}"
            ))),
        }
    }

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

        // `dataset_id` is nullable post-08-01; ad-hoc runs without a dataset
        // persist with `NULL` in the column rather than being silently dropped.
        let active = pipeline_run::ActiveModel {
            id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(row_id)),
            created_at: sea_orm::ActiveValue::Set(Utc::now()),
            status: sea_orm::ActiveValue::Set(domain_status_to_entity(status)),
            pipeline_run_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(pipeline_run_id)),
            pipeline_name: sea_orm::ActiveValue::Set(pipeline_name.to_string()),
            pipeline_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(pipeline_id)),
            dataset_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex_opt(dataset_id)),
            // This is the write path every production caller takes — the
            // watchers in `cognee_core::pipeline_run_registry` call this method
            // directly, so the `From<&PipelineRun>` conversion (which sanitizes
            // separately) never sees their payloads. `run_info_for_errored`
            // embeds an arbitrary error string that can quote offending source
            // text, and `run_info` is a `json` column whose Postgres parser
            // rejects the `\u0000` escape `serde_json` emits for an embedded
            // NUL. The watchers log-and-ignore a failure here, so an unstripped
            // NUL would leave the run recorded as `Started` with the error
            // never stored.
            run_info: sea_orm::ActiveValue::Set(run_info.map(sanitize_json)),
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
            // Ad-hoc rows (dataset_id = None) are not surfaced by
            // latest_status — they don't belong to any dataset bucket the
            // caller asked about (the input filter was already keyed by
            // dataset_id, so a None row would not have matched the IN clause
            // anyway; we filter defensively here for clarity).
            if let Some(did) = run.dataset_id {
                result.entry(did).or_insert(run.status);
            }
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
        // perform a single LEFT JOIN to `datasets`. Owner-email attribution
        // requires the `users` table which now lives in the closed
        // `cognee-access-control` crate; OSS callers receive `owner_email =
        // None` and are expected to resolve emails out-of-band (or via the
        // closed `cognee-access-control::auth::UserAuthRepository`). The
        // dataset/owner_id columns continue to flow through this query so
        // downstream UIs can render attribution without the email.
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
            .join(JoinType::LeftJoin, pipeline_run::Relation::Dataset.def())
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .limit(u64::from(limit));

        if let Some(did) = dataset_id {
            query = query.filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(did)));
        }

        let raw = query
            .into_tuple::<(
                String,
                chrono::DateTime<Utc>,
                pipeline_run::PipelineRunStatus,
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
        ) in raw
        {
            // `dataset_id` is nullable post-08-01 — the column may genuinely
            // be NULL (ad-hoc run without a dataset), and the LEFT JOIN may
            // also yield NULL when the referenced dataset has been deleted.
            // Both cases collapse to `None` in the projection.
            let dataset_uuid = dataset_id_hex
                .as_deref()
                .and_then(|s| uuid_hex::from_hex(s).ok());
            let owner_uuid = owner_id_hex
                .as_deref()
                .and_then(|s| uuid_hex::from_hex(s).ok());
            // Determine dataset attribution presence: when dataset_name is
            // None the LEFT JOIN didn't match (orphan or NULL dataset_id).
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
                owner_email: None,
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
        // `reason` is caller-supplied and lands in the `run_info` json column,
        // which Postgres rejects on an embedded NUL. Today's only production
        // caller passes a literal, but the trait takes `&str` from anyone.
        let reason_info = sanitize_json(json!({"reason": reason}));
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
            // `value` is arbitrary task-published JSON (see
            // `TaskContext::publish_payload_field`) and can carry extracted
            // document text; the column is `json`, which Postgres will not
            // accept with a NUL inside. Both watchers log-and-ignore a failure
            // here, so the field would silently vanish.
            value: sea_orm::ActiveValue::Set(sanitize_json(value)),
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

    async fn get_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        let row = pipeline_run::Entity::find()
            .filter(pipeline_run::Column::PipelineRunId.eq(uuid_hex::to_hex(pipeline_run_id)))
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("get_pipeline_run query failed: {e}"))
            })?;
        Ok(row.map(PipelineRun::from))
    }

    async fn get_pipeline_run_by_dataset(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        // `dataset_id` is the function parameter (non-nullable `Uuid`); we
        // match the column against the hex string. Per decision 4 the column
        // is `Option<String>` post-08-01 but a literal `eq(...)` only matches
        // non-NULL rows — exactly what we want here.
        let row = pipeline_run::Entity::find()
            .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .filter(pipeline_run::Column::PipelineName.eq(pipeline_name))
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("get_pipeline_run_by_dataset query failed: {e}"))
            })?;
        Ok(row.map(PipelineRun::from))
    }

    async fn get_pipeline_runs_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<PipelineRun>, DatabaseError> {
        // Fetch every row for `dataset_id`, newest first, then collapse to
        // one entry per distinct `pipeline_name` (keeping the first / newest
        // seen). Matches Python's behaviour where the helper groups by
        // pipeline_name and picks the latest row.
        let rows = pipeline_run::Entity::find()
            .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("get_pipeline_runs_by_dataset query failed: {e}"))
            })?;

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for row in rows {
            if seen.insert(row.pipeline_name.clone()) {
                out.push(PipelineRun::from(row));
            }
        }
        Ok(out)
    }

    /// Take the claim by inserting the `(dataset_id, pipeline_name)` row. The
    /// composite primary key is the mutual exclusion: concurrent inserts race
    /// in the database and exactly one commits, so `ON CONFLICT DO NOTHING`
    /// reporting zero rows written *is* the "someone else holds it" signal.
    /// Portable across SQLite and Postgres.
    async fn try_claim_pipeline_run(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
        claim_id: Uuid,
        stale_after: std::time::Duration,
    ) -> Result<bool, DatabaseError> {
        let dataset_hex = uuid_hex::to_hex(dataset_id);

        if self
            .insert_claim(&dataset_hex, pipeline_name, claim_id)
            .await?
        {
            return Ok(true);
        }

        // Someone holds it. Reclaim only if their claim has aged out — a
        // killed process cannot release, and a release is scoped to its own
        // `claim_id`, so without this the pair would wedge permanently.
        let Some(existing) = pipeline_run_claim::Entity::find_by_id((
            dataset_hex.clone(),
            pipeline_name.to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| DatabaseError::QueryError(format!("find pipeline_run_claim failed: {e}")))?
        else {
            // Released between our insert and this read — retry once.
            return self
                .insert_claim(&dataset_hex, pipeline_name, claim_id)
                .await;
        };

        let age = Utc::now().signed_duration_since(existing.claimed_at);
        let stale_after = chrono::Duration::from_std(stale_after)
            .map_err(|e| DatabaseError::QueryError(format!("stale_after out of range: {e}")))?;
        if age < stale_after {
            return Ok(false);
        }

        // Delete scoped to the `claim_id` we observed, so we lose the race
        // rather than stomping a claim taken since the read.
        let deleted = pipeline_run_claim::Entity::delete_many()
            .filter(pipeline_run_claim::Column::DatasetId.eq(dataset_hex.clone()))
            .filter(pipeline_run_claim::Column::PipelineName.eq(pipeline_name.to_string()))
            .filter(pipeline_run_claim::Column::ClaimId.eq(existing.claim_id.clone()))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("delete stale pipeline_run_claim failed: {e}"))
            })?;
        if deleted.rows_affected == 0 {
            return Ok(false);
        }

        tracing::warn!(
            dataset_id = %dataset_id,
            pipeline_name = %pipeline_name,
            stale_claim_id = %existing.claim_id,
            age_seconds = age.num_seconds(),
            "reclaimed a stale pipeline-run claim; the previous holder never released it"
        );
        self.insert_claim(&dataset_hex, pipeline_name, claim_id)
            .await
    }

    async fn release_pipeline_run_claim(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
        claim_id: Uuid,
    ) -> Result<(), DatabaseError> {
        pipeline_run_claim::Entity::delete_many()
            .filter(pipeline_run_claim::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .filter(pipeline_run_claim::Column::PipelineName.eq(pipeline_name.to_string()))
            .filter(pipeline_run_claim::Column::ClaimId.eq(uuid_hex::to_hex(claim_id)))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("release pipeline_run_claim failed: {e}"))
            })?;
        Ok(())
    }
}
