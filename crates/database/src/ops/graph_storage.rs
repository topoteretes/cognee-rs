use chrono::{DateTime, Utc};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
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

    // First, get all nodes for this (data_id, dataset_id)
    let all_nodes = node::Entity::find()
        .filter(
            Condition::all()
                .add(node::Column::DataId.eq(&data_hex))
                .add(node::Column::DatasetId.eq(&dataset_hex)),
        )
        .all(db)
        .await
        .map_err(map_sea_err)?;

    if all_nodes.is_empty() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(vec![]);
    }

    // Get slugs that are shared with other data_ids in the same dataset
    let shared_slugs: Vec<String> = node::Entity::find()
        .filter(
            Condition::all()
                .add(node::Column::DatasetId.eq(&dataset_hex))
                .add(node::Column::DataId.ne(&data_hex)),
        )
        .column(node::Column::Slug)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(|m| m.slug)
        .collect();

    let shared_set: std::collections::HashSet<&str> =
        shared_slugs.iter().map(|s| s.as_str()).collect();

    let rows: Vec<GraphNode> = all_nodes
        .into_iter()
        .filter(|n| !shared_set.contains(n.slug.as_str()))
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

    // First, get all edges for this (data_id, dataset_id)
    let all_edges = edge::Entity::find()
        .filter(
            Condition::all()
                .add(edge::Column::DataId.eq(&data_hex))
                .add(edge::Column::DatasetId.eq(&dataset_hex)),
        )
        .all(db)
        .await
        .map_err(map_sea_err)?;

    if all_edges.is_empty() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(vec![]);
    }

    // Get slugs that are shared with other data_ids in the same dataset
    let shared_slugs: Vec<String> = edge::Entity::find()
        .filter(
            Condition::all()
                .add(edge::Column::DatasetId.eq(&dataset_hex))
                .add(edge::Column::DataId.ne(&data_hex)),
        )
        .column(edge::Column::Slug)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(|m| m.slug)
        .collect();

    let shared_set: std::collections::HashSet<&str> =
        shared_slugs.iter().map(|s| s.as_str()).collect();

    let rows: Vec<GraphEdge> = all_edges
        .into_iter()
        .filter(|e| !shared_set.contains(e.slug.as_str()))
        .map(GraphEdge::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
