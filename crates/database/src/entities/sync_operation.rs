use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sync_operations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    #[sea_orm(unique, indexed)]
    pub run_id: String,
    pub status: String,
    pub progress_percentage: i32,
    #[sea_orm(column_type = "Json", nullable)]
    pub dataset_ids: Option<Json>,
    #[sea_orm(column_type = "Json", nullable)]
    pub dataset_names: Option<Json>,
    #[sea_orm(indexed)]
    pub user_id: String,
    pub created_at: DateTimeUtc,
    pub started_at: Option<DateTimeUtc>,
    pub completed_at: Option<DateTimeUtc>,
    pub total_records_to_sync: Option<i32>,
    pub total_records_to_download: Option<i32>,
    pub total_records_to_upload: Option<i32>,
    pub records_downloaded: i32,
    pub records_uploaded: i32,
    pub bytes_downloaded: i64,
    pub bytes_uploaded: i64,
    #[sea_orm(column_type = "Json", nullable)]
    pub dataset_sync_hashes: Option<Json>,
    pub error_message: Option<String>,
    pub retry_count: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
