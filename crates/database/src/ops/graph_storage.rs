use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::{edge, node};
use crate::types::{DatabaseError, GraphEdge, GraphNode};
use crate::uuid_hex;

pub async fn upsert_nodes(
    db: &DatabaseConnection,
    nodes: &[GraphNode],
) -> Result<(), DatabaseError> {
    if nodes.is_empty() {
        return Ok(());
    }
    let models: Vec<node::ActiveModel> = nodes.iter().map(node::ActiveModel::from).collect();
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
    Ok(())
}

pub async fn get_nodes_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
    node::Entity::find()
        .filter(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(GraphNode::from).collect())
}

pub async fn delete_nodes_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    node::Entity::delete_many()
        .filter(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

pub async fn upsert_edges(
    db: &DatabaseConnection,
    edges: &[GraphEdge],
) -> Result<(), DatabaseError> {
    if edges.is_empty() {
        return Ok(());
    }
    let models: Vec<edge::ActiveModel> = edges.iter().map(edge::ActiveModel::from).collect();
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
    Ok(())
}

pub async fn get_edges_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    edge::Entity::find()
        .filter(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(GraphEdge::from).collect())
}

pub async fn delete_edges_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
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
pub async fn get_nodes_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
    node::Entity::find()
        .filter(
            Condition::all()
                .add(node::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .order_by_asc(node::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(GraphNode::from).collect())
}

/// Get all provenance edges for a specific `(data_id, dataset_id)` pair.
pub async fn get_edges_by_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    edge::Entity::find()
        .filter(
            Condition::all()
                .add(edge::Column::DataId.eq(uuid_hex::to_hex(data_id)))
                .add(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))),
        )
        .order_by_asc(edge::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(GraphEdge::from).collect())
}

// ---------------------------------------------------------------------------
// Dataset-scoped deletion of provenance rows
// ---------------------------------------------------------------------------

/// Delete all provenance node rows for a given dataset.
pub async fn delete_nodes_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    node::Entity::delete_many()
        .filter(node::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Delete all provenance edge rows for a given dataset.
pub async fn delete_edges_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
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
pub async fn delete_nodes_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
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
pub async fn delete_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
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
// Unique (non-shared) node/edge queries for safe single-data deletion
// ---------------------------------------------------------------------------

/// Return nodes belonging to `(data_id, dataset_id)` whose slug does NOT
/// appear in any other row within the same dataset with a different `data_id`.
///
/// This is the Rust equivalent of Python's shared-slug exclusion logic.
pub async fn get_unique_nodes_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphNode>, DatabaseError> {
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

    Ok(all_nodes
        .into_iter()
        .filter(|n| !shared_set.contains(n.slug.as_str()))
        .map(GraphNode::from)
        .collect())
}

/// Return edges belonging to `(data_id, dataset_id)` whose slug does NOT
/// appear in any other row within the same dataset with a different `data_id`.
pub async fn get_unique_edges_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
    dataset_id: Uuid,
) -> Result<Vec<GraphEdge>, DatabaseError> {
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

    Ok(all_edges
        .into_iter()
        .filter(|e| !shared_set.contains(e.slug.as_str()))
        .map(GraphEdge::from)
        .collect())
}
