use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "acls")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub principal_id: String,
    pub permission_id: String,
    pub dataset_id: String,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::principal::Entity",
        from = "Column::PrincipalId",
        to = "super::principal::Column::Id"
    )]
    Principal,

    #[sea_orm(
        belongs_to = "super::permission::Entity",
        from = "Column::PermissionId",
        to = "super::permission::Column::Id"
    )]
    Permission,

    #[sea_orm(
        belongs_to = "super::dataset::Entity",
        from = "Column::DatasetId",
        to = "super::dataset::Column::Id"
    )]
    Dataset,
}

impl Related<super::principal::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Principal.def()
    }
}

impl Related<super::permission::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Permission.def()
    }
}

impl Related<super::dataset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Dataset.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
