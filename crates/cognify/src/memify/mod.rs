//! Memify pipeline -- graph enrichment via triplet embedding.
//!
//! Reads an existing knowledge graph and creates searchable vector
//! embeddings for triplets (subject-relationship-object), enabling
//! `SearchType::TripletCompletion` queries.
//!
//! # Usage
//!
//! ```ignore
//! use cognee_cognify::memify::{memify, MemifyConfig};
//!
//! let result = memify(
//!     &*graph_db, &*vector_db, &*embedding_engine,
//!     Some(dataset_id), Some(owner_id), None,
//!     &MemifyConfig::default(),
//! ).await?;
//! ```

pub mod config;
pub mod error;
pub mod extract_triplets;
pub mod feedback_weights;
pub mod index_triplets;
pub mod persist_sessions;
pub mod pipeline;
pub mod sync_graph_session;

pub use config::{MemifyConfig, MemifyTask};
pub use error::MemifyError;
pub use feedback_weights::{
    FEEDBACK_WEIGHTS_APPLIED_KEY, FeedbackApplyResult, FeedbackError,
    apply_feedback_weights_pipeline, normalize_feedback_score, stream_update_weight,
};
pub use index_triplets::IndexResult;
pub use persist_sessions::{
    PersistSessionsError, PersistSessionsResult, USER_SESSIONS_NODE_SET,
    persist_sessions_in_knowledge_graph,
};
pub use pipeline::{MemifyResult, memify};
pub use sync_graph_session::{
    BATCH_SIZE, DEFAULT_MAX_LINES, SyncError, SyncResult, checkpoint_key, sync_graph_to_session,
};
