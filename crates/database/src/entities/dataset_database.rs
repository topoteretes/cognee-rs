//! Where a dataset's own graph and vector databases live, under per-dataset
//! storage (`ENABLE_BACKEND_ACCESS_CONTROL`).
//!
//! Column for column Python's `DatasetDatabase`
//! (`cognee/modules/users/models/DatasetDatabase.py`), so the two SDKs can
//! share a relational database. One row per dataset, shared by its owner and
//! every grantee; provisioned on first use. See
//! `docs/design/per-dataset-storage.md`.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "dataset_database")]
pub struct Model {
    /// The dataset, as 32-char un-hyphenated hex (`uuid_hex::to_hex`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub dataset_id: String,
    /// The dataset's owner: storage is namespaced by it, never by a caller.
    #[sea_orm(indexed)]
    pub owner_id: Option<String>,
    pub vector_database_name: String,
    pub graph_database_name: String,
    pub vector_database_provider: String,
    pub graph_database_provider: String,
    pub graph_dataset_database_handler: String,
    pub vector_dataset_database_handler: String,
    pub vector_database_url: Option<String>,
    pub graph_database_url: Option<String>,
    pub vector_database_key: Option<String>,
    pub graph_database_key: Option<String>,
    /// The release that provisioned these databases (audit only).
    pub cognee_version: Option<String>,
    /// Python's per-dataset data-migration revision. cognee-rs has no such
    /// chain and leaves it `NULL`.
    pub migration_revision: Option<String>,
    pub migration_last_error: Option<String>,
    pub graph_database_connection_info: Json,
    pub vector_database_connection_info: Json,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
