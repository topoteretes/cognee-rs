//! The `provenance_edge_evidence` sidecar: capture, lookup and cleanup.
//!
//! Port of Python's `cognee/modules/provenance/edge_evidence/`
//! (`persistence.py`, `lookup.py`, `cleanup.py`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::pipeline_run::PipelineRunStatus as RunStatus;
use crate::entities::{data, pipeline_run, provenance_edge_evidence as evidence};
use crate::types::{DatabaseError, EdgeEvidence, EdgeEvidenceRecord};
use crate::uuid_hex;

/// Rows per INSERT: 15 bound columns each, so 500 rows stay far below
/// SQLite's and Postgres' bound-variable caps.
const INSERT_BATCH: usize = 500;
/// Ids per `IN (...)` list (Python `_EDGE_SOURCE_BATCH_SIZE`).
const IN_BATCH: usize = 500;

impl From<&EdgeEvidence> for evidence::ActiveModel {
    fn from(row: &EdgeEvidence) -> Self {
        use sea_orm::ActiveValue::Set;
        Self {
            id: Set(uuid_hex::to_hex(row.id)),
            tenant_id: Set(uuid_hex::to_hex_opt(row.tenant_id)),
            user_id: Set(uuid_hex::to_hex(row.user_id)),
            dataset_id: Set(uuid_hex::to_hex(row.dataset_id)),
            data_id: Set(uuid_hex::to_hex(row.data_id)),
            pipeline_run_id: Set(uuid_hex::to_hex_opt(row.pipeline_run_id)),
            chunk_id: Set(uuid_hex::to_hex(row.chunk_id)),
            chunk_index: Set(row.chunk_index),
            edge_id: Set(uuid_hex::to_hex(row.edge_id)),
            source_node_id: Set(uuid_hex::to_hex(row.source_node_id)),
            destination_node_id: Set(uuid_hex::to_hex(row.destination_node_id)),
            relationship_name: Set(row.relationship_name.replace('\0', "")),
            evidence_kind: Set(row.evidence_kind.clone()),
            source_task: Set(row.source_task.clone()),
            confidence: Set(row.confidence),
            created_at: Set(row.created_at),
        }
    }
}

/// Insert evidence rows idempotently (a row whose id exists is kept), in
/// batches, in one transaction. Returns the number of rows offered.
///
/// Python `persist_provenance_batch`: `INSERT … ON CONFLICT (id) DO NOTHING`.
#[instrument(
    name = "cognee.db.relational.edge_evidence.insert_edge_evidence",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn insert_edge_evidence(
    db: &DatabaseConnection,
    rows: &[EdgeEvidence],
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if rows.is_empty() {
        return Ok(0);
    }
    let txn = db.begin().await.map_err(map_sea_err)?;
    for batch in rows.chunks(INSERT_BATCH) {
        // Two rows with one id in a single statement are one row to the
        // conflict clause; keep the first, as the second would be skipped.
        let mut seen = HashSet::with_capacity(batch.len());
        let models: Vec<evidence::ActiveModel> = batch
            .iter()
            .filter(|row| seen.insert(row.id))
            .map(evidence::ActiveModel::from)
            .collect();
        evidence::Entity::insert_many(models)
            .on_conflict(
                OnConflict::column(evidence::Column::Id)
                    .do_nothing()
                    .to_owned(),
            )
            .do_nothing()
            .exec(&txn)
            .await
            .map_err(map_sea_err)?;
    }
    txn.commit().await.map_err(map_sea_err)?;
    Ok(rows.len() as u64)
}

/// Active source chunks for graph edges of one dataset (Python
/// `get_edge_evidence_records`).
///
/// An observation is active when it has no run id or its pipeline run has a
/// `DATASET_PROCESSING_COMPLETED` row, so failed and rolled-back runs need no
/// delete-side mutation. Supports are the distinct `(edge, data, chunk,
/// chunk_index)` groups, ranked per edge by first sighting; the first
/// `per_edge_limit` of each edge whose `Data` row still exists in the dataset
/// are returned, ordered by edge id then rank, at most `total_limit`.
#[instrument(
    name = "cognee.db.relational.edge_evidence.get_edge_evidence_records",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edge_evidence_records(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    edge_ids: &[Uuid],
    per_edge_limit: usize,
    total_limit: usize,
) -> Result<Vec<EdgeEvidenceRecord>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let unique: Vec<Uuid> = {
        let mut seen = HashSet::new();
        edge_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect()
    };
    if unique.is_empty() || per_edge_limit == 0 || total_limit == 0 {
        return Ok(Vec::new());
    }

    let dataset_hex = uuid_hex::to_hex(dataset_id);
    // Distinct active supports with their first sighting, grouped in SQL as
    // Python's `lookup.py` does: every completed re-cognify adds a full set of
    // rows (the run id is part of the row id), so fetching raw rows would grow
    // with history while the answer is bounded by the limits. An observation
    // is active with no run id, or when its run has a COMPLETED row.
    let completed_runs = sea_orm::sea_query::Query::select()
        .column(pipeline_run::Column::PipelineRunId)
        .from(pipeline_run::Entity)
        .and_where(pipeline_run::Column::Status.eq(RunStatus::Completed))
        .to_owned();
    type SupportKey = (String, String, String, Option<i32>);
    type SupportRow = (String, String, String, Option<i32>, DateTime<Utc>);
    let mut supports: HashMap<SupportKey, DateTime<Utc>> = HashMap::new();
    for batch in unique.chunks(IN_BATCH) {
        let found: Vec<SupportRow> = evidence::Entity::find()
            .select_only()
            .column(evidence::Column::EdgeId)
            .column(evidence::Column::DataId)
            .column(evidence::Column::ChunkId)
            .column(evidence::Column::ChunkIndex)
            .column_as(evidence::Column::CreatedAt.min(), "first_seen")
            .filter(evidence::Column::DatasetId.eq(dataset_hex.clone()))
            .filter(evidence::Column::EdgeId.is_in(batch.iter().map(|id| uuid_hex::to_hex(*id))))
            .filter(
                sea_orm::Condition::any()
                    .add(evidence::Column::PipelineRunId.is_null())
                    .add(evidence::Column::PipelineRunId.in_subquery(completed_runs.clone())),
            )
            .group_by(evidence::Column::EdgeId)
            .group_by(evidence::Column::DataId)
            .group_by(evidence::Column::ChunkId)
            .group_by(evidence::Column::ChunkIndex)
            .into_tuple()
            .all(db)
            .await
            .map_err(map_sea_err)?;
        for (edge, data, chunk, chunk_index, first_seen) in found {
            // Edge batches are disjoint, so a key is seen once.
            supports.insert((edge, data, chunk, chunk_index), first_seen);
        }
    }

    // Rank per edge by first sighting (ties by chunk id, for determinism).
    let mut by_edge: BTreeMap<String, Vec<(DateTime<Utc>, SupportKey)>> = BTreeMap::new();
    for (key, first_seen) in supports {
        by_edge
            .entry(key.0.clone())
            .or_default()
            .push((first_seen, key));
    }
    let mut ranked: Vec<SupportKey> = Vec::new();
    for supports in by_edge.values_mut() {
        supports.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.2.cmp(&b.1.2)));
        ranked.extend(
            supports
                .iter()
                .take(per_edge_limit)
                .map(|(_, key)| key.clone()),
        );
    }

    // Join the documents that still exist in the dataset.
    let data_ids: BTreeSet<String> = ranked.iter().map(|key| key.1.clone()).collect();
    let data_ids: Vec<String> = data_ids.into_iter().collect();
    let mut names: HashMap<String, String> = HashMap::new();
    for batch in data_ids.chunks(IN_BATCH) {
        let found: Vec<(String, String)> = data::Entity::find()
            .select_only()
            .column(data::Column::Id)
            .column(data::Column::Name)
            .filter(data::Column::DatasetId.eq(dataset_hex.clone()))
            .filter(data::Column::Id.is_in(batch.iter().cloned()))
            .into_tuple()
            .all(db)
            .await
            .map_err(map_sea_err)?;
        names.extend(found);
    }

    let mut records = Vec::new();
    for (edge, data_id, chunk, chunk_index) in ranked {
        let Some(name) = names.get(&data_id) else {
            continue;
        };
        if records.len() >= total_limit {
            break;
        }
        records.push(EdgeEvidenceRecord {
            edge_id: parse_hex(&edge)?,
            data_id: parse_hex(&data_id)?,
            chunk_id: parse_hex(&chunk)?,
            chunk_index,
            document_name: Some(name.clone()),
        });
    }
    Span::current().record(COGNEE_DB_ROW_COUNT, records.len() as i64);
    Ok(records)
}

fn parse_hex(value: &str) -> Result<Uuid, DatabaseError> {
    uuid_hex::from_hex(value)
        .map_err(|error| DatabaseError::QueryError(format!("invalid uuid {value:?}: {error}")))
}

/// Remove the evidence rows of a dataset, or of one document in it (Python
/// `delete_edge_evidence`). Returns the number removed.
///
/// The rows have no foreign key to `data`, so the delete paths sweep them
/// explicitly: lookups already skip rows whose `Data` row is gone, so this is
/// hygiene against unbounded growth, not correctness.
#[instrument(
    name = "cognee.db.relational.edge_evidence.delete_edge_evidence",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_edge_evidence<C: sea_orm::ConnectionTrait>(
    db: &C,
    dataset_id: Uuid,
    data_id: Option<Uuid>,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, crate::connection_system_label(db));
    let mut delete = evidence::Entity::delete_many()
        .filter(evidence::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)));
    if let Some(data_id) = data_id {
        delete = delete.filter(evidence::Column::DataId.eq(uuid_hex::to_hex(data_id)));
    }
    let result = delete.exec(db).await.map_err(map_sea_err)?;
    Ok(result.rows_affected)
}

/// Remove the evidence rows of one `Data` row, whichever dataset the row's
/// evidence names. Used by the data-row delete, which knows only the row id.
pub(crate) async fn delete_edge_evidence_for_data<C: sea_orm::ConnectionTrait>(
    db: &C,
    data_id: Uuid,
) -> Result<u64, DatabaseError> {
    let result = evidence::Entity::delete_many()
        .filter(evidence::Column::DataId.eq(uuid_hex::to_hex(data_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(result.rows_affected)
}
