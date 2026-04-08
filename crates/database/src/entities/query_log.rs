use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "queries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub query_text: String,
    pub query_type: String,
    pub user_id: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::result_log::Entity")]
    Results,
}

impl Related<super::result_log::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Results.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
