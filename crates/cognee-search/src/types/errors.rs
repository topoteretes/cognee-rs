use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("vector error: {0}")]
    VectorError(String),

    #[error("graph error: {0}")]
    GraphError(String),

    #[error("llm error: {0}")]
    LlmError(String),

    #[error("embedding error: {0}")]
    EmbeddingError(String),

    #[error("database error: {0}")]
    DatabaseError(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unsupported search type: {0:?}")]
    UnsupportedSearchType(crate::types::SearchType),

    #[error("serialization error: {0}")]
    SerializationError(String),
}

impl From<cognee_vector::VectorDBError> for SearchError {
    fn from(value: cognee_vector::VectorDBError) -> Self {
        Self::VectorError(value.to_string())
    }
}

impl From<cognee_graph::GraphDBError> for SearchError {
    fn from(value: cognee_graph::GraphDBError) -> Self {
        Self::GraphError(value.to_string())
    }
}

impl From<cognee_llm::LlmError> for SearchError {
    fn from(value: cognee_llm::LlmError) -> Self {
        Self::LlmError(value.to_string())
    }
}

impl From<cognee_embedding::EmbeddingError> for SearchError {
    fn from(value: cognee_embedding::EmbeddingError) -> Self {
        Self::EmbeddingError(value.to_string())
    }
}

impl From<serde_json::Error> for SearchError {
    fn from(value: serde_json::Error) -> Self {
        Self::SerializationError(value.to_string())
    }
}

impl From<cognee_database::DatabaseError> for SearchError {
    fn from(value: cognee_database::DatabaseError) -> Self {
        Self::DatabaseError(value.to_string())
    }
}
