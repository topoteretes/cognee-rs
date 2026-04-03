use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "artifact_references")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub owner_id: Uuid,
    pub dataset_id: Uuid,
    pub data_id: Option<Uuid>,
    pub artifact_kind: String,
    pub artifact_id: String,
    pub collection_name: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::dataset::Entity",
        from = "Column::DatasetId",
        to = "super::dataset::Column::Id",
        on_delete = "Cascade"
    )]
    Dataset,
    #[sea_orm(
        belongs_to = "super::data::Entity",
        from = "Column::DataId",
        to = "super::data::Column::Id",
        on_delete = "Cascade"
    )]
    Data,
}

impl ActiveModelBehavior for ActiveModel {}
