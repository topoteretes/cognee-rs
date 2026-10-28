use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use cognee_utils::sanitize::sanitize_json;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, DbErr, EntityTrait, JoinType,
    QueryFilter, QueryOrder, QuerySelect, RelationTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::conversions::{domain_status_to_entity, entity_status_to_domain};
use crate::entities::{dataset, pipeline_run, pipeline_run_claim, pipeline_run_payload_field};
use crate::pipelines::repository::PipelineRunClaim;
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};
use crate::uuid_hex;

use super::repository::{
    PipelineRunEntry, PipelineRunListQuery, PipelineRunRepository, PipelineRunRow,
    PipelineRunWithAttributionRow,
};

/// Bound parameters [`PipelineRunRepository::list_runs`] puts into **one**
/// statement for its visibility lists. SQLite (≥ 3.32) accepts 32 766 binds
/// per statement and Postgres 65 535; this leaves headroom for the few other
/// binds of the statement (dataset, pipeline name, limit, offset).
pub const LIST_RUNS_MAX_BINDS: usize = 30_000;

/// Largest `offset + limit` the listing serves once the visibility lists no
/// longer fit one statement: each chunked statement then fetches that many
/// rows to merge, so the window bounds the memory a single request can pin.
pub const LIST_RUNS_CHUNKED_WINDOW: u64 = 5_000;

/// A row of the runs projection, before conversion.
type RawRunRow = (
    String,
    chrono::DateTime<Utc>,
    pipeline_run::PipelineRunStatus,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// SeaORM-backed implementation of [`PipelineRunRepository`].
///
/// Wraps a shared `DatabaseConnection`. All methods write or query the
/// `pipeline_runs` table using the "new row per status transition" pattern,
/// matching both Python's writing pattern and the cross-SDK audit trail
/// requirement.
pub struct SeaOrmPipelineRunRepository {
    db: Arc<DatabaseConnection>,
    /// See [`Self::with_list_bind_limit`].
    list_bind_limit: usize,
}

impl SeaOrmPipelineRunRepository {
    /// The `pipeline_runs` rows that are still in flight: the latest row per
    /// `pipeline_run_id`, kept only when that row is `Initiated` or `Started`.
    ///
    /// Find all `pipeline_run_id`s whose latest row has no more recent
    /// `Completed` / `Errored` successor. Implemented by fetching every row
    /// ordered by `created_at DESC` and keeping the first one seen per
    /// `pipeline_run_id`.
    ///
    /// **One definition, two callers, deliberately.** Both
    /// [`PipelineRunRepository::list_orphan_runs`] and
    /// [`PipelineRunRepository::reset_orphans`] go through this, because
    /// startup recovery calls them in sequence: list the orphans, roll back
    /// each one's graph/vector artifacts, *then* retire the rows. If the two
    /// selections could drift, a row the list missed would still be retired —
    /// re-opening its dataset with the dead run's partial artifacts left in
    /// the graph, which is precisely the state the rollback exists to remove.
    async fn latest_in_flight_rows(&self) -> Result<Vec<pipeline_run::Model>, DatabaseError> {
        let all_rows = pipeline_run::Entity::find()
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("in-flight pipeline-run scan failed: {e}"))
            })?;

        // Collect the latest row per pipeline_run_id.
        let mut latest_per_run: HashMap<String, pipeline_run::Model> = HashMap::new();
        for row in all_rows {
            latest_per_run
                .entry(row.pipeline_run_id.clone())
                .or_insert(row);
        }

        Ok(latest_per_run
            .into_values()
            .filter(|row| {
                matches!(
                    row.status,
                    pipeline_run::PipelineRunStatus::Initiated
                        | pipeline_run::PipelineRunStatus::Started
                )
            })
            .collect())
    }

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
        Self {
            db,
            list_bind_limit: LIST_RUNS_MAX_BINDS,
        }
    }

    /// Override [`LIST_RUNS_MAX_BINDS`], the visibility binds
    /// [`PipelineRunRepository::list_runs`] puts into one statement before it
    /// switches to the chunked path (at least 1). Lower it for a backend
    /// built with a smaller bind limit, or to exercise the chunked path in
    /// tests.
    pub fn with_list_bind_limit(mut self, binds: usize) -> Self {
        self.list_bind_limit = binds.max(1);
        self
    }

    /// One statement of the runs projection (`pipeline_runs` LEFT JOIN
    /// `datasets`), newest first with `id` breaking `created_at` ties (Python
    /// `get_activity_router.py:164-167@37f788bd2`: without the tie-break,
    /// OFFSET paging re-serves rows sharing a timestamp).
    async fn fetch_runs(
        &self,
        dataset_id: Option<Uuid>,
        pipeline_name: Option<&str>,
        visibility: Option<Condition>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DatabaseError> {
        // The `users` table is not part of this schema: `owner_email` is
        // always `None`, and callers resolve emails out of band.
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
            .column(pipeline_run::Column::UserId)
            .join(JoinType::LeftJoin, pipeline_run::Relation::Dataset.def())
            .order_by_desc(pipeline_run::Column::CreatedAt)
            .order_by_desc(pipeline_run::Column::Id)
            .limit(limit);
        if offset > 0 {
            query = query.offset(offset);
        }
        if let Some(did) = dataset_id {
            query = query.filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(did)));
        }
        if let Some(name) = pipeline_name {
            query = query.filter(pipeline_run::Column::PipelineName.eq(name));
        }
        if let Some(condition) = visibility {
            query = query.filter(condition);
        }

        let raw: Vec<RawRunRow> = query
            .into_tuple()
            .all(self.db.as_ref())
            .await
            .map_err(|e| DatabaseError::QueryError(format!("pipeline-runs listing failed: {e}")))?;

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
            user_id_hex,
        ) in raw
        {
            // `dataset_id` is nullable — an ad-hoc run without a dataset — and
            // the LEFT JOIN yields NULL name/owner when the dataset is gone.
            let lenient =
                |hex: Option<String>| hex.as_deref().and_then(|s| uuid_hex::from_hex(s).ok());
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
                dataset_id: lenient(dataset_id_hex),
                dataset_name,
                owner_id: lenient(owner_id_hex),
                owner_email: None,
                user_id: lenient(user_id_hex),
            });
        }
        Ok(rows)
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
        self.log_pipeline_run_entry(PipelineRunEntry {
            pipeline_run_id,
            pipeline_id,
            pipeline_name,
            dataset_id,
            status,
            run_info,
            user_id: None,
        })
        .await
    }

    async fn log_pipeline_run_entry(
        &self,
        entry: PipelineRunEntry<'_>,
    ) -> Result<Uuid, DatabaseError> {
        let PipelineRunEntry {
            pipeline_run_id,
            pipeline_id,
            pipeline_name,
            dataset_id,
            status,
            run_info,
            user_id,
        } = entry;
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
            user_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex_opt(user_id)),
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
        self.fetch_runs(dataset_id, None, None, u64::from(limit), 0)
            .await
    }

    /// One statement with database-side `LIMIT` / `OFFSET` whenever the
    /// visibility lists fit [`LIST_RUNS_MAX_BINDS`] (see
    /// [`Self::with_list_bind_limit`]) — every realistic caller. Above that,
    /// the lists are split into chunks of at most that many ids, each chunk
    /// fetches its newest `offset + limit` rows, and the union is
    /// de-duplicated, re-sorted and re-paged here; that path requires
    /// `offset + limit <= LIST_RUNS_CHUNKED_WINDOW` and answers a
    /// `QueryError` beyond it rather than buffering an unbounded window.
    async fn list_runs(
        &self,
        query: &PipelineRunListQuery,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DatabaseError> {
        let name = query.pipeline_name.as_deref();
        let Some(visibility) = &query.visibility else {
            return self
                .fetch_runs(query.dataset_id, name, None, query.limit, query.offset)
                .await;
        };
        if visibility.is_empty() {
            return Ok(Vec::new());
        }
        let hex =
            |ids: &[Uuid]| -> Vec<String> { ids.iter().map(|id| uuid_hex::to_hex(*id)).collect() };
        let user_hex = hex(&visibility.user_ids);
        let dataset_hex = hex(&visibility.dataset_ids);
        let user_term = |ids: &[String]| pipeline_run::Column::UserId.is_in(ids.to_vec());
        let dataset_term = |ids: &[String]| pipeline_run::Column::DatasetId.is_in(ids.to_vec());

        if user_hex.len() + dataset_hex.len() <= self.list_bind_limit {
            let mut any = Condition::any();
            if !user_hex.is_empty() {
                any = any.add(user_term(&user_hex));
            }
            if !dataset_hex.is_empty() {
                any = any.add(dataset_term(&dataset_hex));
            }
            return self
                .fetch_runs(query.dataset_id, name, Some(any), query.limit, query.offset)
                .await;
        }

        let window = query.offset.saturating_add(query.limit);
        if window > LIST_RUNS_CHUNKED_WINDOW {
            return Err(DatabaseError::QueryError(format!(
                "pipeline-runs listing: offset + limit ({window}) exceeds \
                 {LIST_RUNS_CHUNKED_WINDOW} for a visibility set of {} ids",
                user_hex.len() + dataset_hex.len()
            )));
        }
        let per = self.list_bind_limit;
        let conditions = user_hex
            .chunks(per)
            .map(|chunk| Condition::all().add(user_term(chunk)))
            .chain(
                dataset_hex
                    .chunks(per)
                    .map(|chunk| Condition::all().add(dataset_term(chunk))),
            );
        let mut merged: std::collections::BTreeMap<Uuid, PipelineRunWithAttributionRow> =
            std::collections::BTreeMap::new();
        for condition in conditions {
            for row in self
                .fetch_runs(query.dataset_id, name, Some(condition), window, 0)
                .await?
            {
                merged.entry(row.id).or_insert(row);
            }
        }
        let mut rows: Vec<_> = merged.into_values().collect();
        // The statement's own order: `created_at DESC, id DESC`. Hex ids sort
        // like the `Uuid`s they encode (fixed width, lowercase).
        rows.sort_by(|a, b| (b.created_at, b.id).cmp(&(a.created_at, a.id)));
        let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
        Ok(rows.into_iter().skip(offset).take(limit).collect())
    }

    async fn list_orphan_runs(&self) -> Result<Vec<PipelineRunRow>, DatabaseError> {
        Ok(self
            .latest_in_flight_rows()
            .await?
            .into_iter()
            .map(PipelineRun::from)
            .collect())
    }

    async fn reset_orphans(&self, reason: &str) -> Result<u64, DatabaseError> {
        let orphan_ids: Vec<String> = self
            .latest_in_flight_rows()
            .await?
            .into_iter()
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
                    user_id: sea_orm::ActiveValue::Set(orphan.user_id),
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

    async fn count_runs_created_after(
        &self,
        since: chrono::DateTime<Utc>,
        dataset_id: Option<Uuid>,
        exclude_pipeline_names: &[&str],
    ) -> Result<u64, DatabaseError> {
        use sea_orm::PaginatorTrait;

        let mut query =
            pipeline_run::Entity::find().filter(pipeline_run::Column::CreatedAt.gt(since));
        if let Some(dataset_id) = dataset_id {
            query = query.filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)));
        }
        if !exclude_pipeline_names.is_empty() {
            query = query.filter(
                pipeline_run::Column::PipelineName
                    .is_not_in(exclude_pipeline_names.iter().copied()),
            );
        }
        query.count(self.db.as_ref()).await.map_err(|e| {
            DatabaseError::QueryError(format!("count_runs_created_after query failed: {e}"))
        })
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

    async fn get_pipeline_run_claim(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
    ) -> Result<Option<PipelineRunClaim>, DatabaseError> {
        let Some(row) = pipeline_run_claim::Entity::find_by_id((
            uuid_hex::to_hex(dataset_id),
            pipeline_name.to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| DatabaseError::QueryError(format!("find pipeline_run_claim failed: {e}")))?
        else {
            return Ok(None);
        };

        // The column is the same un-hyphenated hex every id column in this
        // schema uses. A row that does not parse is a corrupt claim rather than
        // an absent one, and reporting it as absent would tell an operator the
        // pair is free while the row keeps refusing their runs.
        let claim_id = uuid_hex::from_hex(&row.claim_id).map_err(|e| {
            DatabaseError::QueryError(format!(
                "pipeline_run_claim for dataset {dataset_id} / {pipeline_name} holds an \
                 unparseable claim_id {:?}: {e}",
                row.claim_id
            ))
        })?;

        Ok(Some(PipelineRunClaim {
            claim_id,
            claimed_at: row.claimed_at,
        }))
    }

    async fn try_release_pipeline_run_claim(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
        claim_id: Uuid,
    ) -> Result<bool, DatabaseError> {
        // Scoped to the holder the caller observed. An unscoped delete would be
        // simpler and wrong: between their read and this call the holder can
        // have finished and a new run taken the pair, and this would then kill
        // a live run while reporting the dead holder's id.
        let deleted = pipeline_run_claim::Entity::delete_many()
            .filter(pipeline_run_claim::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .filter(pipeline_run_claim::Column::PipelineName.eq(pipeline_name.to_string()))
            .filter(pipeline_run_claim::Column::ClaimId.eq(uuid_hex::to_hex(claim_id)))
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("release pipeline_run_claim failed: {e}"))
            })?;

        if deleted.rows_affected > 0 {
            tracing::warn!(
                dataset_id = %dataset_id,
                pipeline_name = %pipeline_name,
                claim_id = %claim_id,
                "released a pipeline-run claim on operator request; if its holder is still \
                 alive, a concurrent run is now possible on this pair"
            );
        }
        Ok(deleted.rows_affected > 0)
    }

    /// Read the claims first, then delete them all, so every dropped pair can
    /// be named in the log. The read/delete pair is not atomic and does not
    /// need to be: the caller has asserted it is the only process on this
    /// database and has not started a run yet, so nothing can be writing
    /// claims concurrently. The delete's own `rows_affected` — not the length
    /// of the read — is what is returned, so a row that vanished between the
    /// two is not counted as released.
    async fn release_all_pipeline_run_claims(&self, reason: &str) -> Result<u64, DatabaseError> {
        let rows = pipeline_run_claim::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("list pipeline_run_claims failed: {e}"))
            })?;

        if rows.is_empty() {
            return Ok(0);
        }

        let now = Utc::now();
        for row in &rows {
            // One line per pair, at `warn`: a claim being dropped without its
            // holder's consent is exactly the kind of thing an operator needs
            // to find in the log after an unexplained concurrent run. The age
            // is what tells them whether this was a crash leftover (hours or
            // days) or something that should not have been swept at all.
            tracing::warn!(
                dataset_id = %row.dataset_id,
                pipeline_name = %row.pipeline_name,
                claim_id = %row.claim_id,
                age_seconds = now.signed_duration_since(row.claimed_at).num_seconds(),
                reason = %reason,
                "releasing a pipeline-run claim left behind by a previous process; this is \
                 only sound because the deployment asserts a single process per database"
            );
        }

        let deleted = pipeline_run_claim::Entity::delete_many()
            .exec(self.db.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::QueryError(format!("release all pipeline_run_claims failed: {e}"))
            })?;

        Ok(deleted.rows_affected)
    }

    async fn reset_orphan_run(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
        run_row_id: Uuid,
        reason: &str,
    ) -> Result<bool, DatabaseError> {
        // The same row `check_pipeline_run_qualification` reads, so what is
        // retired here is exactly what would have refused the next run.
        let Some(latest) = self
            .get_pipeline_run_by_dataset(dataset_id, pipeline_name)
            .await?
        else {
            return Ok(false);
        };

        // Scoped to the row the caller observed. Without this, a real run
        // started between an operator's report and their clear would be the
        // latest row by the time we get here, and would be marked `Errored`
        // while healthy and in flight.
        //
        // Compared on `id`, the row primary key, because `pipeline_run_id` does
        // not identify a run: it is `uuid5(OID, "{pipeline_id}_{dataset_id}")`,
        // so every run of the same pipeline on the same dataset dispatched by
        // the HTTP server carries the same value. Comparing that would pass the
        // guard for precisely the healthy newcomer it exists to spare.
        if latest.id != run_row_id {
            return Ok(false);
        }

        if !matches!(
            latest.status,
            PipelineRunStatus::Initiated | PipelineRunStatus::Started
        ) {
            return Ok(false);
        }

        // New-row-per-transition, matching `reset_orphans`: an `Errored`
        // successor rather than a delete, so the audit trail still shows the
        // run started and how it ended. `Errored` is also what qualification
        // needs to see to return `Proceed`.
        let active = pipeline_run::ActiveModel {
            id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(Uuid::new_v4())),
            created_at: sea_orm::ActiveValue::Set(Utc::now()),
            status: sea_orm::ActiveValue::Set(pipeline_run::PipelineRunStatus::Errored),
            pipeline_run_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(latest.pipeline_run_id)),
            pipeline_name: sea_orm::ActiveValue::Set(latest.pipeline_name),
            pipeline_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(latest.pipeline_id)),
            dataset_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex_opt(latest.dataset_id)),
            // Operator-supplied, and `run_info` is a json column whose Postgres
            // parser rejects the `\u0000` escape — the same reason
            // `reset_orphans` sanitizes its own literal reason.
            run_info: sea_orm::ActiveValue::Set(Some(sanitize_json(json!({"reason": reason})))),
            user_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex_opt(latest.user_id)),
        };
        active.insert(self.db.as_ref()).await.map_err(|e| {
            DatabaseError::QueryError(format!("reset_orphan_run insert failed: {e}"))
        })?;

        tracing::warn!(
            dataset_id = %dataset_id,
            pipeline_name = %pipeline_name,
            run_row_id = %latest.id,
            pipeline_run_id = %latest.pipeline_run_id,
            reason,
            "retired an orphaned pipeline run on operator request"
        );
        Ok(true)
    }
}
