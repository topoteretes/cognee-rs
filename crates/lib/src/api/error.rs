use cognee_database::DatabaseError;
use cognee_delete::DeleteError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("permission denied")]
    PermissionDenied,

    #[error("dataset not found")]
    NotFound,

    #[error("database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("delete error: {0}")]
    Delete(#[from] DeleteError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
