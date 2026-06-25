use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub slug: String,
    pub user_id: String,
    pub data_id: String,
    #[sea_orm(indexed)]
    pub dataset_id: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub label: Option<String>,
    #[sea_orm(column_name = "type", column_type = "Text")]
    pub node_type: String,
    #[sea_orm(column_type = "Json")]
    pub indexed_fields: Json,
    #[sea_orm(column_type = "Json", nullable)]
    pub attributes: Option<Json>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::dataset::Entity",
        from = "Column::DatasetId",
        to = "super::dataset::Column::Id"
    )]
    Dataset,
}

impl ActiveModelBehavior for ActiveModel {}
