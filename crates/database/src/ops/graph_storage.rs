use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
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
