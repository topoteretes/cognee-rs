use async_trait::async_trait;
use std::path::PathBuf;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncWriteExt};

#[derive(Debug, Clone, Error)]
pub enum StorageError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

/// A writer for storing data in chunks
/// This allows efficient streaming writes without loading entire content into memory
pub struct StorageWriter {
    file: File,
    location: String,
}

impl StorageWriter {
    pub(crate) fn new(file: File, location: String) -> Self {
        Self { file, location }
    }

    /// Write a chunk of data to storage
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), StorageError> {
        self.file
            .write_all(chunk)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write chunk: {}", e)))
    }

    /// Finish writing and return the storage location
    pub async fn finish(mut self) -> Result<String, StorageError> {
        self.file
            .flush()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to flush file: {}", e)))?;
        Ok(self.location)
    }
}

#[async_trait]
pub trait StorageTrait: Send + Sync {
    /// Store data at a specific path and return the storage location
    async fn store(&self, data: &[u8], file_name: &str) -> Result<String, StorageError>;

    /// Store data from an async reader (streaming) and return the storage location.
    /// This is the object-safe version; for a generic version see [`StorageExt::store_stream`].
    async fn store_stream_dyn(
        &self,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        file_name: &str,
    ) -> Result<String, StorageError>;

    /// Create a writer for chunk-based storage
    /// Allows writing data in chunks without loading entire content into memory
    async fn create_writer(&self, file_name: &str) -> Result<StorageWriter, StorageError>;

    /// Retrieve data from storage location
    async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError>;

    /// Check if data exists at location
    async fn exists(&self, location: &str) -> Result<bool, StorageError>;

    /// Delete data at location
    async fn delete(&self, location: &str) -> Result<(), StorageError>;

    /// Get the full path for a location
    fn get_full_path(&self, location: &str) -> PathBuf;

    /// Return the base directory of this storage backend as a string.
    /// Used to construct `file://` URIs for stored files.
    /// Returns an empty string for backends that have no filesystem path (e.g. mock, S3).
    fn base_path(&self) -> &str;

    /// Initialize storage (create directories, etc.)
    async fn initialize(&self) -> Result<(), StorageError>;
}

/// Extension trait providing generic convenience methods on top of [`StorageTrait`].
/// Auto-implemented for all types that implement `StorageTrait`.
#[async_trait]
pub trait StorageExt: StorageTrait {
    /// Store data from a typed async reader (streaming).
    /// Delegates to [`StorageTrait::store_stream_dyn`].
    async fn store_stream<R: AsyncRead + Unpin + Send>(
        &self,
        reader: &mut R,
        file_name: &str,
    ) -> Result<String, StorageError> {
        self.store_stream_dyn(reader, file_name).await
    }
}

impl<T: StorageTrait + ?Sized> StorageExt for T {}
