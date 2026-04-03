use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "edges")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub slug: Uuid,
    pub user_id: Uuid,
    #[sea_orm(indexed)]
    pub data_id: Uuid,
    #[sea_orm(indexed)]
    pub dataset_id: Uuid,
    pub source_node_id: Uuid,
    pub destination_node_id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub relationship_name: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub label: Option<String>,
    #[sea_orm(column_type = "Json", nullable)]
    pub attributes: Option<Json>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::node::Entity",
        from = "Column::SourceNodeId",
        to = "super::node::Column::Id"
    )]
    SourceNode,
    #[sea_orm(
        belongs_to = "super::node::Entity",
        from = "Column::DestinationNodeId",
        to = "super::node::Column::Id"
    )]
    DestinationNode,
    #[sea_orm(
        belongs_to = "super::dataset::Entity",
        from = "Column::DatasetId",
        to = "super::dataset::Column::Id"
    )]
    Dataset,
    #[sea_orm(
        belongs_to = "super::data::Entity",
        from = "Column::DataId",
        to = "super::data::Column::Id"
    )]
    Data,
}

impl ActiveModelBehavior for ActiveModel {}
