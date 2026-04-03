use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

// Note: no Eq because f64 fields don't implement Eq.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "graph_metrics")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub num_tokens: Option<i32>,
    pub num_nodes: Option<i32>,
    pub num_edges: Option<i32>,
    pub mean_degree: Option<f64>,
    pub edge_density: Option<f64>,
    pub num_connected_components: Option<i32>,
    #[sea_orm(column_type = "Json", nullable)]
    pub sizes_of_connected_components: Option<Json>,
    pub num_selfloops: Option<i32>,
    pub diameter: Option<i32>,
    pub avg_shortest_path_length: Option<f64>,
    pub avg_clustering: Option<f64>,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
