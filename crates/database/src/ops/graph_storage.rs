use std::collections::HashSet;

use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::{Alias, Expr, OnConflict, Query};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{edge, node};
use crate::types::{DatabaseError, GraphEdge, GraphNode};
use crate::uuid_hex;

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
