//! SeaORM entity for the `notebooks` table.
//!
//! Stores per-user Jupyter-like notebooks with typed cells persisted as JSON.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "notebooks")]
pub struct Model {
    /// UUID stored as hex string (matches the rest of the schema).
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub owner_id: String,
    pub name: String,
    /// JSON array of NotebookCell objects.
    #[sea_orm(column_type = "Json")]
    pub cells: serde_json::Value,
    pub deletable: bool,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
