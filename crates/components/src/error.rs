//! Error type shared by all component factories.
//!
//! This type is re-exported by `cognee` as `cognee::ComponentError`, so
//! it stays the identical type across the OSS crates and downstream consumers.

use thiserror::Error;

/// Errors that can occur during component initialization or access.
#[derive(Debug, Error)]
pub enum ComponentError {
    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Graph database error: {0}")]
    GraphDb(String),

    #[error("Vector database error: {0}")]
    VectorDb(String),

    #[error("Embedding engine error: {0}")]
    EmbeddingEngine(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Ontology resolver error: {0}")]
    Ontology(String),

    #[error("Graph extractor error: {0}")]
    GraphExtractor(String),

    #[error("Session store error: {0}")]
    SessionStore(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// An embedded graph or vector store is already open in this process: a
    /// second live engine on the same path was refused rather than opened
    /// next to the first (see [`cognee_utils::store_claim`]). The bindings
    /// report it as `STORE_IN_USE`.
    #[error(transparent)]
    StoreInUse(#[from] cognee_utils::StoreInUse),
}

impl ComponentError {
    /// A graph engine's construction failure, for a factory: a refused second
    /// open stays [`ComponentError::StoreInUse`] (so the bindings can report
    /// `STORE_IN_USE`); anything else becomes [`ComponentError::GraphDb`]
    /// prefixed with `context`.
    pub fn from_graph_db(context: &str, error: cognee_graph::GraphDBError) -> Self {
        match error {
            cognee_graph::GraphDBError::StoreInUse(in_use) => Self::StoreInUse(in_use),
            other => Self::GraphDb(format!("{context}: {other}")),
        }
    }

    /// The vector twin of [`ComponentError::from_graph_db`].
    pub fn from_vector_db(context: &str, error: cognee_vector::VectorDBError) -> Self {
        match error {
            cognee_vector::VectorDBError::StoreInUse(in_use) => Self::StoreInUse(in_use),
            other => Self::VectorDb(format!("{context}: {other}")),
        }
    }
}
