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
    /// `(temp, target)`: `finish` renames the temp file being written over
    /// the target, so the target is never seen half-written.
    rename_on_finish: Option<(PathBuf, PathBuf)>,
}

impl StorageWriter {
    /// A plain writer, used by `MockStorage`.
    #[cfg(feature = "testing")]
    pub(crate) fn new(file: File, location: String) -> Self {
        Self {
            file,
            location,
            rename_on_finish: None,
        }
    }

    /// A writer into `temp`, the file open as `file`, that `finish` renames
    /// over `target`.
    pub(crate) fn atomic(file: File, location: String, temp: PathBuf, target: PathBuf) -> Self {
        Self {
            file,
            location,
            rename_on_finish: Some((temp, target)),
        }
    }

    /// Write a chunk of data to storage
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), StorageError> {
        self.file
            .write_all(chunk)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write chunk: {e}")))
    }

    /// Finish writing and return the storage location
    pub async fn finish(mut self) -> Result<String, StorageError> {
        // Taken, so `Drop` leaves the temp file to the cleanup below.
        let rename = self.rename_on_finish.take();
        let flushed = self.file.flush().await;
        let written = match (&rename, flushed) {
            (_, Err(e)) => Err(e),
            (Some((temp, target)), Ok(())) => tokio::fs::rename(temp, target).await,
            (None, Ok(())) => Ok(()),
        };
        if let Err(e) = written {
            if let Some((temp, _)) = &rename {
                let _ = tokio::fs::remove_file(temp).await;
            }
            return Err(StorageError::IoError(format!("Failed to write file: {e}")));
        }
        Ok(std::mem::take(&mut self.location))
    }
}

impl Drop for StorageWriter {
    /// A writer dropped without `finish` removes its temp file, so an
    /// abandoned write leaves nothing in the storage root.
    fn drop(&mut self) {
        if let Some((temp, _)) = self.rename_on_finish.take() {
            let _ = std::fs::remove_file(temp);
        }
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

    /// Remove all files from storage.
    ///
    /// Equivalent to Python's `get_file_storage(data_root_directory).remove_all()`.
    /// The storage directory itself is preserved; only its contents are deleted.
    async fn remove_all(&self) -> Result<(), StorageError>;
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
