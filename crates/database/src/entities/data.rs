use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "data")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    pub raw_data_location: String,
    pub original_data_location: String,
    pub extension: String,
    pub mime_type: String,
    pub content_hash: String,
    #[sea_orm(indexed)]
    pub owner_id: Uuid,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl Related<super::dataset::Entity> for Entity {
    fn to() -> RelationDef {
        super::dataset_data::Relation::Dataset.def()
    }
    fn via() -> Option<RelationDef> {
        Some(super::dataset_data::Relation::Data.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}
