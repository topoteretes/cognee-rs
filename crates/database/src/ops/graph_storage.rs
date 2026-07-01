use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::{Alias, Expr, OnConflict, Query};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
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
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if nodes.is_empty() {
        return Ok(());
    }
    // Chunk so a single statement never exceeds the DB's bound-variable cap.
    for batch in nodes.chunks(PROVENANCE_INSERT_BATCH) {
        let models: Vec<node::ActiveModel> = batch.iter().map(node::ActiveModel::from).collect();
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
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if edges.is_empty() {
        return Ok(());
    }
    // Chunk so a single statement never exceeds the DB's bound-variable cap.
    for batch in edges.chunks(PROVENANCE_INSERT_BATCH) {
        let models: Vec<edge::ActiveModel> = batch.iter().map(edge::ActiveModel::from).collect();
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
