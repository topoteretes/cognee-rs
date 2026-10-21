use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::{Alias, Expr, Func, OnConflict, Query, SimpleExpr};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use tracing::{Span, instrument, warn};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{edge, node};
use crate::types::{DatabaseError, GraphEdge, GraphNode};
use crate::uuid_hex;
use cognee_models::EdgeType;

/// Max rows per provenance INSERT. A multi-row `insert_many` binds
/// `rows × columns` parameters in one statement, and SQLite caps that at
/// `SQLITE_MAX_VARIABLE_NUMBER` (999 on very old builds, 32766 since 3.32).
/// The node/edge tables have ~10 columns, so 500 rows ≈ 5 000 bound values —
/// comfortably under SQLite's cap and Postgres' 65 535. Without batching, a
/// large graph (e.g. a full-length book) overflows the cap and the upsert
/// fails with "too many SQL variables".
const PROVENANCE_INSERT_BATCH: usize = 500;

/// Return references to `items` de-duplicated by `key`, keeping the **last**
/// occurrence of each key, in forward order.
///
/// Postgres rejects an `INSERT … ON CONFLICT (id) DO UPDATE` whose VALUES list
/// names the same conflict-target row more than once ("ON CONFLICT DO UPDATE
/// command cannot affect row a second time"). The provenance upserts can be
/// handed a batch that repeats a node/edge id (e.g. the same entity re-emitted
/// within one chunk); collapsing to the last occurrence keeps the upsert legal
/// while preserving update semantics — the last write wins, exactly as it would
/// if the duplicates landed in separate statements. SQLite tolerates the
/// duplicate, so this only matters on Postgres, but de-duplicating for both
/// keeps behaviour identical across backends.
fn dedup_keeping_last<T, F>(items: &[T], key: F) -> Vec<&T>
where
    F: Fn(&T) -> Uuid,
{
    // Walk backwards: the first time a key is seen is its last occurrence.
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(items.len());
    let mut out: Vec<&T> = Vec::with_capacity(items.len());
    for item in items.iter().rev() {
        if seen.insert(key(item)) {
            out.push(item);
        }
    }
    out.reverse();
    out
}

/// Upsert node provenance rows on the given connection.
///
/// Delegates to the connection-generic impl; this concrete signature is the
/// published API (cognee-database is on crates.io, and generalizing the
/// parameter would break `&Arc<DatabaseConnection>` callers via lost deref
/// coercion). Transactional callers go through [`upsert_provenance_graph`].
///
/// The `err`-recording span lives here on the public entry point (and on
/// [`upsert_provenance_graph`]), not on the generic `_on` impl, so a single
/// failure records exactly one ERROR event whether the caller is a direct
/// upsert or the transactional provenance path.
#[instrument(
    name = "cognee.db.relational.graph_storage.upsert_nodes",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn upsert_nodes(
    db: &DatabaseConnection,
    nodes: &[GraphNode],
) -> Result<(), DatabaseError> {
    upsert_nodes_on(db, nodes).await
}

async fn upsert_nodes_on<C: ConnectionTrait>(
    db: &C,
    nodes: &[GraphNode],
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, crate::connection_system_label(db));
    if nodes.is_empty() {
        return Ok(());
    }
    // Chunk so a single statement never exceeds the DB's bound-variable cap.
    for batch in nodes.chunks(PROVENANCE_INSERT_BATCH) {
        // Collapse duplicate ids within the batch (keep last) so Postgres' ON
        // CONFLICT DO UPDATE never touches the same row twice.
        let models: Vec<node::ActiveModel> = dedup_keeping_last(batch, |n| n.id)
            .into_iter()
            .map(node::ActiveModel::from)
            .collect();
        node::Entity::insert_many(models)
            .on_conflict(
                // `PipelineRunId` is deliberately absent from this list: the
                // row id is keyed by logical identity, not by run, so the row
                // records the run that *created* the artifact. Letting a later
                // run overwrite the tag would make that run's rollback delete
                // a node an earlier, successful run created. Python's
                // `upsert_nodes.py` switched to `on_conflict_do_nothing` for
                // the same reason.
                OnConflict::column(node::Column::Id)
                    .update_columns([
                        node::Column::Slug,
                        node::Column::UserId,
                        node::Column::DataId,
                        node::Column::DatasetId,
                        node::Column::Label,
                        node::Column::NodeType,
                        node::Column::IndexedFields,
                        node::Column::Attributes,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await
            .map_err(map_sea_err)?;
    }
    Ok(())
}

#[instrument(
    name = "cognee.db.relational.graph_storage.get_nodes_by_dataset",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_nodes_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

#[instrument(
    name = "cognee.db.relational.graph_storage.delete_nodes_by_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_nodes_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    node::Entity::delete_many()
        .filter(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Upsert edge provenance rows on the given connection.
///
/// Delegates to the connection-generic impl; this concrete signature is the
/// published API (see [`upsert_nodes`]). Transactional callers go through
/// [`upsert_provenance_graph`]. The `err` span lives here, not on the generic
/// `_on` impl, so one failure records exactly one ERROR event (see
/// [`upsert_nodes`]).
#[instrument(
    name = "cognee.db.relational.graph_storage.upsert_edges",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn upsert_edges(
    db: &DatabaseConnection,
    edges: &[GraphEdge],
) -> Result<(), DatabaseError> {
    upsert_edges_on(db, edges).await
}

async fn upsert_edges_on<C: ConnectionTrait>(
    db: &C,
    edges: &[GraphEdge],
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, crate::connection_system_label(db));
    if edges.is_empty() {
        return Ok(());
    }
    // Chunk so a single statement never exceeds the DB's bound-variable cap.
    for batch in edges.chunks(PROVENANCE_INSERT_BATCH) {
        // Collapse duplicate ids within the batch (keep last) so Postgres' ON
        // CONFLICT DO UPDATE never touches the same row twice.
        let models: Vec<edge::ActiveModel> = dedup_keeping_last(batch, |e| e.id)
            .into_iter()
            .map(edge::ActiveModel::from)
            .collect();
        edge::Entity::insert_many(models)
            .on_conflict(
                // `PipelineRunId` is deliberately absent — see `upsert_nodes_on`.
                OnConflict::column(edge::Column::Id)
                    .update_columns([
                        edge::Column::Slug,
                        edge::Column::UserId,
                        edge::Column::DataId,
                        edge::Column::DatasetId,
                        edge::Column::SourceNodeId,
                        edge::Column::DestinationNodeId,
                        edge::Column::RelationshipName,
                        edge::Column::Label,
                        edge::Column::Attributes,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await
            .map_err(map_sea_err)?;
    }
    Ok(())
}

/// Upsert a provenance node+edge group atomically in one transaction.
///
/// A failure partway through rolls the whole group back (the transaction is
/// dropped uncommitted, which sea-orm turns into a rollback), so the
/// provenance graph never ends up half-written.
///
/// `begin()` issues a deferred `BEGIN`, but this transaction is write-first:
/// the first statement is an upsert, which takes SQLite's write lock
/// immediately, so there is no read-to-write lock upgrade to deadlock on.
///
/// On SQLite this holds the single writer lock for the whole group (all node
/// batches, then all edge batches) — a deliberate trade for atomicity. Under
/// WAL, readers are never blocked; a concurrent writer on the same file waits
/// out the 120s `busy_timeout` (`SQLITE_BUSY_TIMEOUT`, see `connect_sqlite`)
/// rather than failing with `SQLITE_BUSY`, since the batches are pre-built
/// local inserts that commit well within that window.
#[instrument(
    name = "cognee.db.relational.graph_storage.upsert_provenance_graph",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn upsert_provenance_graph(
    db: &DatabaseConnection,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if nodes.is_empty() && edges.is_empty() {
        return Ok(());
    }
    let txn = db.begin().await.map_err(map_sea_err)?;
    upsert_nodes_on(&txn, nodes).await?;
    upsert_edges_on(&txn, edges).await?;
    txn.commit().await.map_err(map_sea_err)?;
    Ok(())
}

#[instrument(
    name = "cognee.db.relational.graph_storage.get_edges_by_dataset",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edges_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Return edges for `dataset_id` created strictly after `since`, ordered by
/// `created_at` ascending and limited to `limit` rows. Used by Stage 4 of
/// `improve()` for incremental graph→session synchronisation.
///
/// When `since` is `None`, returns the oldest `limit` edges in the dataset.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_edges_since",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edges_since(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    since: Option<DateTime<Utc>>,
    limit: u64,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut q = edge::Entity::find()
        .filter(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(edge::Column::CreatedAt)
        .limit(limit);
    if let Some(ts) = since {
        q = q.filter(edge::Column::CreatedAt.gt(ts));
    }
    let rows: Vec<GraphEdge> = q
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Batch-fetch nodes by their string IDs (hex form). Used by Stage 4 to
/// resolve edge endpoints to full node metadata for JSON-line rendering.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_nodes_by_ids",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_nodes_by_ids(
    db: &DatabaseConnection,
    ids: &[String],
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if ids.is_empty() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(node::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

#[instrument(
    name = "cognee.db.relational.graph_storage.delete_edges_by_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_edges_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    edge::Entity::delete_many()
        .filter(edge::Column::DataId.eq(uuid_hex::to_hex(data_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Queries scoped by (data_id, dataset_id)
// ---------------------------------------------------------------------------

/// Get all provenance nodes for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_nodes_by_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_nodes_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(
            Condition::all()
                .add(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Get all provenance edges for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_edges_by_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edges_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(
            Condition::all()
                .add(edge::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Dataset-scoped deletion of provenance rows
// ---------------------------------------------------------------------------

/// Delete all provenance node rows for a given dataset.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_nodes_by_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_nodes_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    node::Entity::delete_many()
        .filter(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Delete all provenance edge rows for a given dataset.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_edges_by_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_edges_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    edge::Entity::delete_many()
        .filter(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Data-scoped deletion of provenance rows
// ---------------------------------------------------------------------------

/// Delete provenance node rows for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_nodes_for_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_nodes_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    node::Entity::delete_many()
        .filter(
            Condition::all()
                .add(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Delete provenance edge rows for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_edges_for_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    edge::Entity::delete_many()
        .filter(
            Condition::all()
                .add(edge::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Count queries scoped by (data_id, dataset_id)
// ---------------------------------------------------------------------------

/// Count provenance node rows for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.count_nodes_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_nodes_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<usize, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count = node::Entity::find()
        .filter(
            Condition::all()
                .add(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count as usize)
}

/// Count provenance edge rows for a specific `(data_id, dataset_id)` pair.
#[instrument(
    name = "cognee.db.relational.graph_storage.count_edges_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<usize, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count = edge::Entity::find()
        .filter(
            Condition::all()
                .add(edge::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count as usize)
}

// ---------------------------------------------------------------------------
// Unique (non-shared) node/edge queries for safe single-data deletion
// ---------------------------------------------------------------------------

/// Return nodes belonging to `(data_id, dataset_id)` whose slug does NOT
/// appear in any other row within the same dataset with a different `data_id`.
///
/// This is the Rust equivalent of Python's shared-slug exclusion logic.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_unique_nodes_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_unique_nodes_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let data_hex = uuid_hex::to_hex(data_id);
    let dataset_hex = uuid_hex::to_hex(dataset_id);

    // One query instead of two: select the (data_id, dataset_id) nodes whose slug
    // is NOT shared by any other data_id in the same dataset. The correlated
    // NOT EXISTS replaces the previous fetch-all + fetch-shared-slugs + in-memory
    // filter.
    let n2 = Alias::new("n2");
    let shared = Query::select()
        .expr(Expr::val(1))
        .from_as(Alias::new("nodes"), n2.clone())
        .and_where(Expr::col((n2.clone(), node::Column::DatasetId)).eq(dataset_hex.clone()))
        .and_where(Expr::col((n2.clone(), node::Column::DataId)).ne(data_hex.clone()))
        .and_where(
            Expr::col((n2.clone(), node::Column::Slug))
                .equals((Alias::new("nodes"), node::Column::Slug)),
        )
        .to_owned();

    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(node::Column::DataId.eq(&data_hex))
        .filter(node::Column::DatasetId.eq(&dataset_hex))
        .filter(Expr::exists(shared).not())
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Return edges belonging to `(data_id, dataset_id)` whose slug does NOT
/// appear in any other row within the same dataset with a different `data_id`.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_unique_edges_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_unique_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let data_hex = uuid_hex::to_hex(data_id);
    let dataset_hex = uuid_hex::to_hex(dataset_id);

    // One query instead of two (see `get_unique_nodes_for_data`): edges for
    // (data_id, dataset_id) whose slug is not shared by another data_id in the
    // same dataset, via a correlated NOT EXISTS.
    let e2 = Alias::new("e2");
    let shared = Query::select()
        .expr(Expr::val(1))
        .from_as(Alias::new("edges"), e2.clone())
        .and_where(Expr::col((e2.clone(), edge::Column::DatasetId)).eq(dataset_hex.clone()))
        .and_where(Expr::col((e2.clone(), edge::Column::DataId)).ne(data_hex.clone()))
        .and_where(
            Expr::col((e2.clone(), edge::Column::Slug))
                .equals((Alias::new("edges"), edge::Column::Slug)),
        )
        .to_owned();

    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(edge::Column::DataId.eq(&data_hex))
        .filter(edge::Column::DatasetId.eq(&dataset_hex))
        .filter(Expr::exists(shared).not())
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// `EXISTS (SELECT 1 FROM nodes AS n WHERE n.slug = edges.<endpoint> AND ...)`:
/// whether some node row, narrowed by `narrow`, claims the edge row's
/// `endpoint`. Correlated, so both backends can plan it per edge row through
/// `ix_nodes_slug` (or `index_node_dataset_slug` when `narrow` pins the
/// dataset) instead of materializing a slug list.
fn endpoint_claimed(
    endpoint: edge::Column,
    narrow: impl FnOnce(&Alias) -> Vec<SimpleExpr>,
) -> SimpleExpr {
    let n = Alias::new("n");
    let mut claim = Query::select();
    claim
        .expr(Expr::val(1))
        .from_as(Alias::new("nodes"), n.clone())
        .and_where(
            Expr::col((n.clone(), node::Column::Slug)).equals((Alias::new("edges"), endpoint)),
        );
    for condition in narrow(&n) {
        claim.and_where(condition);
    }
    Expr::exists(claim.to_owned())
}

/// The edge row's `endpoint` is exclusively owned by `(data_hex, dataset_hex)`:
/// that item's node rows claim it and no other item's node rows in the dataset
/// do. This is exactly the set [`get_unique_nodes_for_data`] returns, which is
/// the set a data-scoped delete detach-deletes from the graph.
fn endpoint_exclusive_to(endpoint: edge::Column, data_hex: &str, dataset_hex: &str) -> Condition {
    let in_dataset_with = |data_eq: bool| {
        move |n: &Alias| {
            let data = Expr::col((n.clone(), node::Column::DataId));
            vec![
                Expr::col((n.clone(), node::Column::DatasetId)).eq(dataset_hex),
                if data_eq {
                    data.eq(data_hex)
                } else {
                    data.ne(data_hex)
                },
            ]
        }
    };
    Condition::all()
        .add(endpoint_claimed(endpoint, in_dataset_with(true)))
        .add(endpoint_claimed(endpoint, in_dataset_with(false)).not())
}

/// Selection predicate for the structural edge rows (`is_part_of`, `contains`,
/// `made_from`, `is_a`, ...) that belong to `(data_id, dataset_id)` although
/// their `data_id` column is nil.
///
/// Cognify writes those rows with a nil `data_id` ("structural edges span
/// multiple DataPoints"), so `data_id = :data` never selects them. Their owner
/// is recoverable from their endpoints instead: a row belongs to the data item
/// when *either* endpoint is exclusive to it (see [`endpoint_exclusive_to`]).
/// The delete detach-deletes every such node from the graph, and a graph edge
/// cannot outlive either of its endpoints, so the row's edge is gone with the
/// item whichever end was exclusive. A chunk, summary or document is
/// exclusive to one item. An entity shared by several items is not, and an
/// `EntityType` never is (its node row carries a nil `data_id`), so an edge
/// joining two such nodes keeps its row, and its claim on its retrieval text,
/// until one of its endpoints goes.
///
/// Because the predicate reads the node rows, it has to be evaluated before
/// the data item's node rows are deleted.
fn structural_edges_owned_by(data_hex: &str, dataset_hex: &str) -> Condition {
    Condition::all()
        .add(edge::Column::DataId.eq(uuid_hex::to_hex(Uuid::nil())))
        .add(edge::Column::DatasetId.eq(dataset_hex))
        .add(
            Condition::any()
                .add(endpoint_exclusive_to(
                    edge::Column::SourceNodeId,
                    data_hex,
                    dataset_hex,
                ))
                .add(endpoint_exclusive_to(
                    edge::Column::DestinationNodeId,
                    data_hex,
                    dataset_hex,
                )),
        )
}

/// Whether an edge row still stands for a graph edge that may exist, and so
/// may claim an `EdgeType` retrieval text.
///
/// A row with a real `data_id` always does: it leaves the ledger with its data
/// item. A nil-`data_id` structural row does not, because no data-scoped delete
/// selected it before structural rows were attributed through their endpoints:
/// a database whose items were deleted earlier still holds their structural
/// rows, with every node row of both endpoints gone. Such a row can never be
/// selected again, and its graph edge was detach-deleted with its endpoints, so
/// counting it as a live claim would pin its text's `EdgeType` point forever.
///
/// The test is deliberately weak: a nil-`data_id` row is live while *either*
/// endpoint is claimed by any node row in any dataset. Every structural edge's
/// source is a chunk, summary, entity or entity type, and each of those gets a
/// node row in the same ledger write as the edge row, so a row whose edge is
/// still in the graph always has at least that claimed endpoint. Asking for
/// both endpoints would be stronger but unsafe: a document written before
/// documents got node rows would orphan every `is_part_of` row pointing at it.
fn edge_row_is_live_claim() -> Condition {
    Condition::any()
        .add(edge::Column::DataId.ne(uuid_hex::to_hex(Uuid::nil())))
        .add(endpoint_claimed(edge::Column::SourceNodeId, |_| Vec::new()))
        .add(endpoint_claimed(edge::Column::DestinationNodeId, |_| {
            Vec::new()
        }))
}

/// The nil-`data_id` structural edge rows that belong to `(data_id, dataset_id)`
/// by their endpoints; see [`structural_edges_owned_by`].
#[instrument(
    name = "cognee.db.relational.graph_storage.get_structural_edges_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_structural_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(structural_edges_owned_by(
            &uuid_hex::to_hex(data_id),
            &uuid_hex::to_hex(dataset_id),
        ))
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Delete the provenance edge rows with these ids, returning how many went.
///
/// By id rather than by re-running a selection predicate, so a caller that
/// selected rows, acted on what they name, and then deletes them removes
/// exactly the rows it acted on: a row written in between is left for a later
/// delete instead of vanishing with no cleanup of its artifacts. Ids that no
/// longer exist are not an error; the count says how many did.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_edges_by_ids",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_edges_by_ids(
    db: &DatabaseConnection,
    ids: &[Uuid],
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut deleted = 0u64;
    if !ids.is_empty() {
        // One transaction, so a failure part-way through leaves every row.
        let txn = db.begin().await.map_err(map_sea_err)?;
        for batch in ids.chunks(PROVENANCE_INSERT_BATCH) {
            deleted += edge::Entity::delete_many()
                .filter(edge::Column::Id.is_in(batch.iter().map(|id| uuid_hex::to_hex(*id))))
                .exec(&txn)
                .await
                .map_err(map_sea_err)?
                .rows_affected;
        }
        txn.commit().await.map_err(map_sea_err)?;
    }
    Span::current().record(COGNEE_DB_ROW_COUNT, deleted as i64);
    Ok(deleted)
}

/// Every character Rust's `str::trim` strips: the Unicode `White_Space`
/// property. [`EdgeType::retrieval_text`] trims with it, so a SQL filter that
/// matches a row's text against candidate texts has to trim with the same set.
const UNICODE_WHITE_SPACE: &str = "\u{9}\u{A}\u{B}\u{C}\u{D}\u{20}\u{85}\u{A0}\u{1680}\
    \u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200A}\
    \u{2028}\u{2029}\u{202F}\u{205F}\u{3000}";

/// Candidate texts per query of
/// [`get_edge_retrieval_texts_claimed_outside_data`]. Each one is bound twice,
/// so 250 keeps a statement near 500 parameters, under SQLite's
/// `SQLITE_MAX_VARIABLE_NUMBER` on any build and far under Postgres' 65 535.
const CANDIDATE_TEXT_BATCH: usize = 250;

/// The `candidates` that edge rows surviving the deletion of
/// `(data_id, dataset_id)` still claim as `EdgeType` retrieval texts.
///
/// A row survives unless it is the item's own (`data_id`/`dataset_id` match) or
/// a structural row [`get_structural_edges_for_data`] attributes to it. Rows of
/// the same data item in another dataset, and of other items anywhere, survive;
/// a nil-`data_id` row whose endpoints are both gone does not claim anything
/// (see [`edge_row_is_live_claim`]).
///
/// The `EdgeType` point id is many-to-one over edges (every `is_part_of` shares
/// one), so slug exclusivity does not say whether the point may go; a caller
/// keeps the point of every text returned here. See
/// [`get_edge_retrieval_texts_claimed_outside_run`] for the run-scoped sibling.
///
/// Only rows that could produce a candidate are read: a row's retrieval text is
/// its trimmed `edge_text` attribute or, failing that, its trimmed
/// `relationship_name`, so `trim(relationship_name) IN (...) OR
/// trim(edge_text) IN (...)` is a superset of the producing rows, and the text
/// is then derived exactly as [`retrieval_texts_where`] always does. A backend
/// with no JSON extraction reads every surviving row instead, which gives the
/// same answer.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_edge_retrieval_texts_claimed_outside_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edge_retrieval_texts_claimed_outside_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
    candidates: &[String],
) -> Result<Vec<String>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let data_hex = uuid_hex::to_hex(data_id);
    let dataset_hex = uuid_hex::to_hex(dataset_id);
    let wanted: BTreeSet<&str> = candidates.iter().map(String::as_str).collect();
    let surviving = || {
        Condition::all()
            .add(
                Condition::any()
                    .add(edge::Column::DataId.ne(&data_hex))
                    .add(edge::Column::DatasetId.ne(&dataset_hex)),
            )
            .add(structural_edges_owned_by(&data_hex, &dataset_hex).not())
            .add(edge_row_is_live_claim())
    };

    let mut claimed = BTreeSet::new();
    if wanted.is_empty() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let backend = db.get_database_backend();
    match edge_text_sql(backend) {
        Some(edge_text) => {
            // `trim(x, chars)` on SQLite, `btrim(x, chars)` on Postgres (the
            // only backends `edge_text_sql` answers for).
            let trim = Alias::new(if backend == DbBackend::Postgres {
                "btrim"
            } else {
                "trim"
            });
            let trimmed = |expr: SimpleExpr| {
                Expr::expr(
                    Func::cust(trim.clone())
                        .arg(expr)
                        .arg(Expr::val(UNICODE_WHITE_SPACE)),
                )
            };
            let wanted: Vec<&str> = wanted.iter().copied().collect();
            for batch in wanted.chunks(CANDIDATE_TEXT_BATCH) {
                let produces_candidate = Condition::any()
                    .add(
                        trimmed(Expr::col(edge::Column::RelationshipName).into())
                            .is_in(batch.iter().copied()),
                    )
                    .add(trimmed(Expr::cust(edge_text)).is_in(batch.iter().copied()));
                for text in retrieval_texts_where(db, surviving().add(produces_candidate)).await? {
                    if batch.contains(&text.as_str()) {
                        claimed.insert(text);
                    }
                }
            }
        }
        None => {
            for text in retrieval_texts_where(db, surviving()).await? {
                if wanted.contains(text.as_str()) {
                    claimed.insert(text);
                }
            }
        }
    }
    Span::current().record(COGNEE_DB_ROW_COUNT, claimed.len() as i64);
    Ok(claimed.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Run-scoped provenance queries
// ---------------------------------------------------------------------------

/// The provenance rows one sweep owns: this run, in this dataset, optionally
/// narrowed to these data items.
///
/// Every run-scoped query below takes this and nothing else, so selection,
/// exclusivity and deletion can never drift apart.
///
/// `pipeline_run_id` is the run *correlation* id (`pipeline_runs.pipeline_run_id`),
/// not the `pipeline_runs.id` row key — `pipeline_runs` holds one row per status
/// transition, so only the correlation id identifies a run.
#[derive(Debug, Clone, Copy)]
pub struct RunScope<'a> {
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    /// `None` selects every data item the run touched in this dataset;
    /// `Some(ids)` narrows to those items, and `Some(&[])` selects nothing.
    pub data_ids: Option<&'a [Uuid]>,
}

impl<'a> RunScope<'a> {
    /// Everything the run created in `dataset_id`.
    pub fn whole_run(pipeline_run_id: Uuid, dataset_id: Uuid) -> Self {
        Self {
            pipeline_run_id,
            dataset_id,
            data_ids: None,
        }
    }

    /// Only what the run created for `data_ids` in `dataset_id`.
    pub fn for_data(pipeline_run_id: Uuid, dataset_id: Uuid, data_ids: &'a [Uuid]) -> Self {
        Self {
            pipeline_run_id,
            dataset_id,
            data_ids: Some(data_ids),
        }
    }

    /// An empty narrowing set selects no rows at all — distinct from `None`,
    /// which selects the whole run.
    fn selects_nothing(&self) -> bool {
        matches!(self.data_ids, Some(ids) if ids.is_empty())
    }

    fn run_hex(&self) -> String {
        uuid_hex::to_hex(self.pipeline_run_id)
    }

    fn dataset_hex(&self) -> String {
        uuid_hex::to_hex(self.dataset_id)
    }

    fn data_hexes(&self) -> Option<Vec<String>> {
        self.data_ids
            .map(|ids| ids.iter().copied().map(uuid_hex::to_hex).collect())
    }
}

/// The selection predicate over `nodes`: `pipeline_run_id = :run AND
/// dataset_id = :dataset [AND data_id IN (:data…)]`.
///
/// `pipeline_run_id = :run` is NULL-safe by construction — a `NULL` column
/// never equals anything — so rows written before run ownership existed drop
/// out of every select and every delete without a special case.
fn node_selection(scope: &RunScope<'_>) -> Condition {
    let mut cond = Condition::all()
        .add(node::Column::PipelineRunId.eq(scope.run_hex()))
        .add(node::Column::DatasetId.eq(scope.dataset_hex()));
    if let Some(data_hexes) = scope.data_hexes() {
        cond = cond.add(node::Column::DataId.is_in(data_hexes));
    }
    cond
}

/// The selection predicate over `edges`; see [`node_selection`].
fn edge_selection(scope: &RunScope<'_>) -> Condition {
    let mut cond = Condition::all()
        .add(edge::Column::PipelineRunId.eq(scope.run_hex()))
        .add(edge::Column::DatasetId.eq(scope.dataset_hex()));
    if let Some(data_hexes) = scope.data_hexes() {
        cond = cond.add(edge::Column::DataId.is_in(data_hexes));
    }
    cond
}

/// Return the provenance nodes the run owns within `scope`.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_nodes_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_nodes_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(node_selection(scope))
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Return the provenance edges the run owns within `scope`.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_edges_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edges_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(edge_selection(scope))
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Return the nodes of [`get_nodes_for_run`] whose `slug` — the graph store's
/// real node id — is claimed by no row *outside* `scope`.
///
/// "Outside" is the negation of the selection predicate: another run, a `NULL`
/// run, another dataset, or (when narrowed) a data item this sweep is keeping.
/// Only those rows are safe to remove from the graph and vector stores.
///
/// Note the dataset clause runs the opposite way to [`get_unique_nodes_for_data`],
/// which only treats *same-dataset* rows as claimants. OSS Rust has one graph
/// store shared by every dataset and `slug` is the content-addressed graph node
/// id, so two datasets mentioning the same entity share one physical node;
/// deleting it while another dataset's ownership row still names it would
/// corrupt that dataset. Leaving an extra artifact behind is recoverable —
/// a later sweep or a dataset delete removes it — so the predicate errs that
/// way.
///
/// The check is a correlated `NOT EXISTS` over the *predicate*, never over a
/// list of the selected row ids: a large run owns millions of rows and an
/// `IN (…)` of ids would blow past SQLite's `SQLITE_MAX_VARIABLE_NUMBER` and
/// Postgres' 65 535. No `n2.id <> nodes.id` term is needed — the four
/// disjuncts *are* the negation of the selection, so every selected row (the
/// outer row included) fails all of them.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_unique_nodes_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_unique_nodes_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<GraphNode>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let n2 = Alias::new("n2");

    // `IS NULL` is NOT redundant with `<> :run`: `NULL <> :run` evaluates to
    // NULL, not TRUE, so without it a slug also claimed by a pre-ownership row
    // would look unclaimed and the sweep would delete an artifact that
    // predates every run.
    let mut outside = Condition::any()
        .add(Expr::col((n2.clone(), node::Column::PipelineRunId)).is_null())
        .add(Expr::col((n2.clone(), node::Column::PipelineRunId)).ne(scope.run_hex()))
        .add(Expr::col((n2.clone(), node::Column::DatasetId)).ne(scope.dataset_hex()));
    if let Some(data_hexes) = scope.data_hexes() {
        outside = outside.add(Expr::col((n2.clone(), node::Column::DataId)).is_not_in(data_hexes));
    }

    let claimed = Query::select()
        .expr(Expr::val(1))
        .from_as(Alias::new("nodes"), n2.clone())
        .cond_where(
            Condition::all()
                .add(
                    Expr::col((n2.clone(), node::Column::Slug))
                        .equals((Alias::new("nodes"), node::Column::Slug)),
                )
                .add(outside),
        )
        .to_owned();

    let rows: Vec<GraphNode> = node::Entity::find()
        .filter(node_selection(scope))
        .filter(Expr::exists(claimed).not())
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphNode::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// Return the edges of [`get_edges_for_run`] whose `slug` is claimed by no row
/// outside `scope`; see [`get_unique_nodes_for_run`] for the reasoning.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_unique_edges_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_unique_edges_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }
    let e2 = Alias::new("e2");

    let mut outside = Condition::any()
        .add(Expr::col((e2.clone(), edge::Column::PipelineRunId)).is_null())
        .add(Expr::col((e2.clone(), edge::Column::PipelineRunId)).ne(scope.run_hex()))
        .add(Expr::col((e2.clone(), edge::Column::DatasetId)).ne(scope.dataset_hex()));
    if let Some(data_hexes) = scope.data_hexes() {
        outside = outside.add(Expr::col((e2.clone(), edge::Column::DataId)).is_not_in(data_hexes));
    }

    let claimed = Query::select()
        .expr(Expr::val(1))
        .from_as(Alias::new("edges"), e2.clone())
        .cond_where(
            Condition::all()
                .add(
                    Expr::col((e2.clone(), edge::Column::Slug))
                        .equals((Alias::new("edges"), edge::Column::Slug)),
                )
                .add(outside),
        )
        .to_owned();

    let rows: Vec<GraphEdge> = edge::Entity::find()
        .filter(edge_selection(scope))
        .filter(Expr::exists(claimed).not())
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

/// The `EdgeType` retrieval texts edge rows *outside* `scope` still claim.
///
/// A second exclusivity check, for a second identity. The
/// `EdgeType_relationship_name` vector point is keyed on an edge's *retrieval
/// text*, not on its slug, and that key is many-to-one over edges: every `is_a`
/// edge in the store shares one point. So slug exclusivity — what
/// [`get_unique_edges_for_run`] answers — says nothing about whether that point
/// may go, and a sweep of one file's `is_a` edge would strip the point every
/// surviving `is_a` edge still needs. Callers subtract this set before issuing
/// any `EdgeType` delete.
///
/// Each row's text is derived exactly as cognify derived the point's key:
/// [`EdgeType::retrieval_text`] over the row's `edge_text` attribute and its
/// `relationship_name` column — the same derivation the delete lane uses to
/// compute the candidate ids. Reading the column alone is not enough: custom
/// graph-model field edges all carry the field name (`"employees"`) in the
/// column and their stamped text only in `attributes.edge_text`, so a column
/// read would leave every such outside edge's point unprotected.
///
/// "Outside" is the same negation [`get_unique_nodes_for_run`] uses, but
/// *uncorrelated*, rather than a per-row `NOT EXISTS` or an `IN` list of
/// candidate texts — a run owns one text per edge, and that list would blow
/// past SQLite's `SQLITE_MAX_VARIABLE_NUMBER` and Postgres' 65 535. It runs as
/// one `SELECT DISTINCT` of `(relationship_name, edge_text)` pairs, with the
/// string `edge_text` pulled out of the JSON attributes by the backend
/// ([`edge_text_sql`]) so neither duplicate rows nor attribute blobs cross the
/// wire; the retrieval text is then derived from each pair here. A backend
/// with no JSON extraction falls back to reading the attributes and
/// deduplicating in Rust.
///
/// No `selects_nothing` early return, deliberately: an empty selection makes
/// every row an outside row, and "everything is claimed" is both the honest and
/// the safe answer. A caller with nothing to filter should skip the call.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_edge_retrieval_texts_claimed_outside_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_edge_retrieval_texts_claimed_outside_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<String>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));

    // `IS NULL` is not redundant with `<> :run`; see `get_unique_nodes_for_run`.
    let mut outside = Condition::any()
        .add(edge::Column::PipelineRunId.is_null())
        .add(edge::Column::PipelineRunId.ne(scope.run_hex()))
        .add(edge::Column::DatasetId.ne(scope.dataset_hex()));
    if let Some(data_hexes) = scope.data_hexes() {
        outside = outside.add(edge::Column::DataId.is_not_in(data_hexes));
    }

    let texts = retrieval_texts_where(db, outside).await?;
    Span::current().record(COGNEE_DB_ROW_COUNT, texts.len() as i64);
    Ok(texts)
}

/// The distinct `EdgeType` retrieval texts of the edge rows matching `filter`,
/// derived exactly as cognify derived the point's key; see
/// [`get_edge_retrieval_texts_claimed_outside_run`] for why it runs as a
/// `SELECT DISTINCT` of `(relationship_name, edge_text)` pairs.
async fn retrieval_texts_where(
    db: &DatabaseConnection,
    filter: Condition,
) -> Result<Vec<String>, DatabaseError> {
    let pairs: Vec<(String, Option<String>)> = match edge_text_sql(db.get_database_backend()) {
        Some(edge_text) => edge::Entity::find()
            .select_only()
            .column(edge::Column::RelationshipName)
            .column_as(Expr::cust(edge_text), "edge_text")
            .distinct()
            .filter(filter)
            .into_tuple::<(String, Option<String>)>()
            .all(db)
            .await
            .map_err(map_sea_err)?,
        None => edge::Entity::find()
            .select_only()
            .column(edge::Column::RelationshipName)
            .column(edge::Column::Attributes)
            .filter(filter)
            .into_tuple::<(String, Option<serde_json::Value>)>()
            .all(db)
            .await
            .map_err(map_sea_err)?
            .into_iter()
            .map(|(name, attributes)| {
                let edge_text = attributes
                    .as_ref()
                    .and_then(|attributes| attributes.get("edge_text"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                (name, edge_text)
            })
            .collect(),
    };

    let texts: BTreeSet<String> = pairs
        .iter()
        .map(|(name, edge_text)| EdgeType::retrieval_text(edge_text.as_deref(), name))
        .collect();
    Ok(texts.into_iter().collect())
}

/// SQL for an edge row's `attributes.edge_text` when it is a JSON string, else
/// `NULL` — the value `attributes.get("edge_text").and_then(Value::as_str)`
/// reads. `None` for a backend without JSON functions.
///
/// SQLite guards with `json_valid` because its JSON functions raise on a
/// malformed document instead of returning `NULL`. Postgres casts to `json`
/// so a `jsonb` column (a database first written by another tool) reads the
/// same way.
fn edge_text_sql(backend: DbBackend) -> Option<&'static str> {
    match backend {
        DbBackend::Sqlite => Some(
            "CASE WHEN json_valid(attributes) THEN \
             CASE WHEN json_type(attributes, '$.edge_text') = 'text' \
             THEN json_extract(attributes, '$.edge_text') END END",
        ),
        DbBackend::Postgres => Some(
            "CASE WHEN json_typeof(CAST(attributes AS json) -> 'edge_text') = 'string' \
             THEN CAST(attributes AS json) ->> 'edge_text' END",
        ),
        _ => None,
    }
}

/// Delete the provenance node rows within `scope`, returning how many went.
///
/// The plain selection predicate, with no exclusivity check: the ownership row
/// belongs to this run whether or not the artifact it names survives.
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_nodes_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_nodes_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(0);
    }
    let deleted = node::Entity::delete_many()
        .filter(node_selection(scope))
        .exec(db)
        .await
        .map_err(map_sea_err)?
        .rows_affected;
    Span::current().record(COGNEE_DB_ROW_COUNT, deleted as i64);
    Ok(deleted)
}

/// Delete the provenance edge rows within `scope`, returning how many went;
/// see [`delete_nodes_for_run`].
#[instrument(
    name = "cognee.db.relational.graph_storage.delete_edges_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_edges_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(0);
    }
    let deleted = edge::Entity::delete_many()
        .filter(edge_selection(scope))
        .exec(db)
        .await
        .map_err(map_sea_err)?
        .rows_affected;
    Span::current().record(COGNEE_DB_ROW_COUNT, deleted as i64);
    Ok(deleted)
}

/// The distinct data items the run touched within `scope` — the set whose
/// completion markers a sweep has to clear.
///
/// Read *before* the ownership rows go: they are what carries the answer.
/// A `SELECT DISTINCT` rather than a load of every selected row (which is what
/// Python's `rollback.py` does): a large run owns millions of rows, while the
/// distinct data ids are bounded by the files in the dataset.
///
/// Exclusivity is deliberately *not* applied. A data item whose every artifact
/// is also claimed elsewhere still had its work rolled back, so its marker must
/// go even though none of its artifacts do.
///
/// A row whose stored id will not parse is skipped with a warning rather than
/// failing the call: this feeds an error-path operation that has to make
/// progress on everything else.
#[instrument(
    name = "cognee.db.relational.graph_storage.get_data_ids_for_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_data_ids_for_run(
    db: &DatabaseConnection,
    scope: &RunScope<'_>,
) -> Result<Vec<Uuid>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if scope.selects_nothing() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(Vec::new());
    }

    // Two queries folded together in Rust rather than a SQL `UNION`: this
    // reuses `node_selection` / `edge_selection` verbatim, so the set of data
    // items reported can never drift from the set of rows deleted.
    let node_hexes: Vec<String> = node::Entity::find()
        .select_only()
        .column(node::Column::DataId)
        .distinct()
        .filter(node_selection(scope))
        .into_tuple::<String>()
        .all(db)
        .await
        .map_err(map_sea_err)?;

    let edge_hexes: Vec<String> = edge::Entity::find()
        .select_only()
        .column(edge::Column::DataId)
        .distinct()
        .filter(edge_selection(scope))
        .into_tuple::<String>()
        .all(db)
        .await
        .map_err(map_sea_err)?;

    // BTreeSet, not HashSet: a deterministic order keeps the sweep's marker
    // clearing (and its tests) reproducible.
    let mut data_ids = BTreeSet::new();
    for hex in node_hexes.into_iter().chain(edge_hexes) {
        match uuid_hex::from_hex(&hex) {
            Ok(id) => {
                data_ids.insert(id);
            }
            Err(e) => warn!("Skipping unparseable provenance data_id {hex:?}: {e}"),
        }
    }

    Span::current().record(COGNEE_DB_ROW_COUNT, data_ids.len() as i64);
    Ok(data_ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::UNICODE_WHITE_SPACE;

    /// The SQL trim set is exactly what `str::trim` strips, so the candidate
    /// filter matches a row iff `EdgeType::retrieval_text` would.
    #[test]
    fn sql_trim_set_is_rusts_whitespace() {
        let expected: String = (0..=u32::from(char::MAX))
            .filter_map(char::from_u32)
            .filter(|c| c.is_whitespace())
            .collect();
        assert_eq!(UNICODE_WHITE_SPACE, expected);
    }
}
