use async_trait::async_trait;
use std::path::{Path, PathBuf};
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

/// A temp file that is removed when dropped, unless [`TempFile::commit`]
/// renamed it into place: an error, a cancelled future or an abandoned
/// writer never leaves it in the storage root.
pub(crate) struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl TempFile {
    /// Create `path`, which must not exist yet, already guarded. The create
    /// is a direct, synchronous call: `tokio::fs` would create the file on
    /// a background thread that a cancelled caller cannot stop, after the
    /// guard's cleanup had already run.
    pub(crate) fn create(path: PathBuf) -> std::io::Result<(Self, File)> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let guard = Self {
            path,
            committed: false,
        };
        Ok((guard, File::from_std(file)))
    }

    /// Rename the temp file over `target`; only a finished rename keeps it
    /// from being removed on drop.
    pub(crate) async fn commit(mut self, target: &Path) -> std::io::Result<()> {
        tokio::fs::rename(&self.path, target).await?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A writer for storing data in chunks
/// This allows efficient streaming writes without loading entire content into memory
pub struct StorageWriter {
    file: File,
    location: String,
    /// `(temp, target)`: `finish` renames the temp file being written over
    /// the target, so the target is never seen half-written; the guard
    /// removes the temp file on any other way out.
    rename_on_finish: Option<(TempFile, PathBuf)>,
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
    pub(crate) fn atomic(file: File, location: String, temp: TempFile, target: PathBuf) -> Self {
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
        let to_io_error = |e| StorageError::IoError(format!("Failed to write file: {e}"));
        self.file.flush().await.map_err(to_io_error)?;
        if let Some((temp, target)) = self.rename_on_finish {
            temp.commit(&target).await.map_err(to_io_error)?;
        }
        Ok(self.location)
    }
}

#[async_trait]
pub trait StorageTrait: Send + Sync {
    /// Store data at a specific path and return the storage location
    async fn store(&self, data: &[u8], file_name: &str) -> Result<String, StorageError>;

    /// [`store`](Self::store) unless a file already exists at `file_name`,
    /// which is then kept as it is (Python `store(..., overwrite=False)`,
    /// its default; plain `store` is `overwrite=True`).
    async fn store_if_absent(&self, data: &[u8], file_name: &str) -> Result<String, StorageError> {
        if self.exists(file_name).await? {
            return Ok(file_name.to_string());
        }
        self.store(data, file_name).await
    }

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

    /// Remove the folder `location` is in when it is empty and is not the
    /// storage root. A backend without folders has nothing to remove.
    async fn remove_empty_parent(&self, _location: &str) -> Result<(), StorageError> {
        Ok(())
    }

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
