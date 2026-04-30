use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "session_qa_entries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub session_id: String,
    pub user_id: Option<String>,
    pub question: String,
    #[sea_orm(column_type = "Text")]
    pub answer: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub context: Option<String>,
    pub created_at: DateTimeUtc,
    /// User feedback text.
    #[sea_orm(column_type = "Text", nullable)]
    pub feedback_text: Option<String>,
    /// User feedback score (1-5).
    pub feedback_score: Option<i32>,
    /// JSON-serialised `UsedGraphElementIds`.
    #[sea_orm(column_type = "Text", nullable)]
    pub used_graph_element_ids: Option<String>,
    /// JSON-serialised `HashMap<String, bool>`.
    #[sea_orm(column_type = "Text", nullable)]
    pub memify_metadata: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Entity for the `session_trace_steps` table — agent trace steps persisted
/// alongside Q&A entries; mirrors Python's `SessionAgentTraceEntry`.
pub mod trace_step {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "session_trace_steps")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub trace_id: String,
        pub user_id: String,
        pub session_id: String,
        /// Monotonically-increasing sequence assigned at insert time. Used to
        /// guarantee stable insertion-order reads even when multiple inserts
        /// share a `created_at` timestamp.
        pub seq: i64,
        pub created_at: DateTimeUtc,
        pub origin_function: String,
        pub status: String,
        #[sea_orm(column_type = "Text")]
        pub memory_query: String,
        #[sea_orm(column_type = "Text")]
        pub memory_context: String,
        /// JSON-encoded `serde_json::Value` (default `{}`).
        #[sea_orm(column_type = "Text")]
        pub method_params: String,
        /// JSON-encoded optional `serde_json::Value`.
        #[sea_orm(column_type = "Text", nullable)]
        pub method_return_value: Option<String>,
        #[sea_orm(column_type = "Text")]
        pub error_message: String,
        #[sea_orm(column_type = "Text")]
        pub session_feedback: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Entity for the `session_graph_context` table.
pub mod graph_context {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "session_graph_context")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub session_id: String,
        pub user_id: Option<String>,
        #[sea_orm(column_type = "Text")]
        pub context: String,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
