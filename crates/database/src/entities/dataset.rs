// Phase 2: full implementation
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "datasets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    #[sea_orm(indexed)]
    pub owner_id: Uuid,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl Related<super::data::Entity> for Entity {
    fn to() -> RelationDef {
        super::dataset_data::Relation::Data.def()
    }
    fn via() -> Option<RelationDef> {
        Some(super::dataset_data::Relation::Dataset.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}
