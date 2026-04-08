use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "data")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
    pub raw_data_location: String,
    pub original_data_location: String,
    pub extension: String,
    pub mime_type: String,
    pub content_hash: String,
    #[sea_orm(indexed)]
    pub owner_id: String,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
    pub label: Option<String>,
    pub original_extension: Option<String>,
    pub original_mime_type: Option<String>,
    pub loader_engine: Option<String>,
    pub raw_content_hash: Option<String>,
    pub tenant_id: Option<String>,
    pub external_metadata: Option<String>,
    pub node_set: Option<String>,
    pub pipeline_status: Option<String>,
    pub token_count: i64,
    pub data_size: i64,
    pub last_accessed: Option<DateTimeUtc>,
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
