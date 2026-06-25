use super::storage_trait::{StorageError, StorageTrait, StorageWriter};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tracing::{debug, instrument};
use uuid::Uuid;

pub struct LocalStorage {
    base_path: PathBuf,
}

impl LocalStorage {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// Generate a UUID-based subdirectory structure for organizing files
    /// Returns a relative path like "ab/cd/filename.txt"
    fn generate_storage_path(&self, file_name: &str) -> String {
        let uuid = Uuid::new_v4();
        let uuid_str = uuid.to_string();

        // Use first 4 chars for first directory, next 4 for second
        let dir1 = &uuid_str[..2];
        let dir2 = &uuid_str[2..4];

        format!("{dir1}/{dir2}/{file_name}")
    }

    /// Resolve a location string into a filesystem path.
    ///
    /// Mirrors Python's `get_data_file_path()` + `open_data_file()` which
    /// strips the `file://` scheme and uses the resulting absolute path
    /// directly.
    ///
    /// Accepted inputs:
    /// - plain relative path: `ab/cd/file.txt`  → `base_path/ab/cd/file.txt`
    /// - absolute `file://` URI: `file:///data/ab/cd/file.txt` → `/data/ab/cd/file.txt`
    fn resolve_location(&self, location: &str) -> PathBuf {
        let path_str = location.strip_prefix("file://").unwrap_or(location);
        let path = Path::new(path_str);

        if path.is_absolute() {
            // Absolute path (from a file:// URI) — use directly, just like
            // Python's `open_data_file` does after `get_data_file_path()`.
            path.to_path_buf()
        } else {
            // Relative path (plain storage location) — join with base.
            self.base_path.join(path)
        }
    }
}

#[async_trait]
impl StorageTrait for LocalStorage {
    async fn initialize(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.base_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create base directory: {e}")))
    }

    #[instrument(name = "storage.store", skip(self, data), fields(file_name, bytes = data.len()))]
    async fn store(&self, data: &[u8], file_name: &str) -> Result<String, StorageError> {
        let relative_path = self.generate_storage_path(file_name);
        let full_path = self.base_path.join(&relative_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {e}")))?;
        }

        // Write file
        let mut file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {e}")))?;

        file.write_all(data)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write file: {e}")))?;

        file.flush()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to flush file: {e}")))?;

        Ok(relative_path)
    }

    #[instrument(name = "storage.store_stream", skip(self, reader), fields(file_name))]
    async fn store_stream_dyn(
        &self,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        file_name: &str,
    ) -> Result<String, StorageError> {
        let relative_path = self.generate_storage_path(file_name);
        let full_path = self.base_path.join(&relative_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {e}")))?;
        }

        // Create file
        let mut file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {e}")))?;

        // Stream copy from reader to file
        tokio::io::copy(reader, &mut file)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write file: {e}")))?;

        file.flush()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to flush file: {e}")))?;

        Ok(relative_path)
    }

    #[instrument(name = "storage.create_writer", skip(self), fields(file_name))]
    async fn create_writer(&self, file_name: &str) -> Result<StorageWriter, StorageError> {
        let relative_path = self.generate_storage_path(file_name);
        let full_path = self.base_path.join(&relative_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {e}")))?;
        }

        // Create file
        let file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {e}")))?;

        Ok(StorageWriter::new(file, relative_path))
    }

    #[instrument(name = "storage.retrieve", skip(self), fields(location))]
    async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError> {
        let full_path = self.resolve_location(location);

        let bytes = fs::read(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {location}"))
            } else {
                StorageError::IoError(format!("Failed to read file: {e}"))
            }
        })?;
        debug!(bytes = bytes.len(), "file retrieved");
        Ok(bytes)
    }

    async fn exists(&self, location: &str) -> Result<bool, StorageError> {
        let full_path = self.resolve_location(location);

        fs::try_exists(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to check file existence: {e}")))
    }

    #[instrument(name = "storage.delete", skip(self), fields(location))]
    async fn delete(&self, location: &str) -> Result<(), StorageError> {
        let full_path = self.resolve_location(location);

        fs::remove_file(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {location}"))
            } else {
                StorageError::IoError(format!("Failed to delete file: {e}"))
            }
        })
    }

    fn get_full_path(&self, location: &str) -> PathBuf {
        self.resolve_location(location)
    }

    fn base_path(&self) -> &str {
        self.base_path.to_str().unwrap_or("")
    }

    async fn remove_all(&self) -> Result<(), StorageError> {
        let mut entries = fs::read_dir(&self.base_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                // Directory doesn't exist — nothing to remove.
                return StorageError::NotFound(format!(
                    "Base directory not found: {}",
                    self.base_path.display()
                ));
            }
            StorageError::IoError(format!("Failed to read directory: {e}"))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to iterate directory entry: {e}")))?
        {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to get file type: {e}")))?;
            if file_type.is_dir() {
                fs::remove_dir_all(&path).await.map_err(|e| {
                    StorageError::IoError(format!(
                        "Failed to remove directory {}: {}",
                        path.display(),
                        e
                    ))
                })?;
            } else {
                fs::remove_file(&path).await.map_err(|e| {
                    StorageError::IoError(format!(
                        "Failed to remove file {}: {}",
                        path.display(),
                        e
                    ))
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_store_and_retrieve() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"Hello, World!";
        let location = storage.store(data, "test.txt").await.unwrap();

        let retrieved = storage.retrieve(&location).await.unwrap();
        assert_eq!(data.to_vec(), retrieved);
    }

    #[tokio::test]
    async fn test_exists() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"Test data";
        let location = storage.store(data, "exists.txt").await.unwrap();

        assert!(storage.exists(&location).await.unwrap());
        assert!(!storage.exists("nonexistent.txt").await.unwrap());
    }

    #[test]
    fn resolve_plain_relative_path() {
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("ab/cd/file.txt"),
            PathBuf::from("/data/ab/cd/file.txt")
        );
    }

    #[test]
    fn resolve_absolute_file_uri() {
        // file:// URI with an absolute path — strip scheme, use path as-is
        // (mirrors Python's get_data_file_path for file:///abs/path)
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("file:///data/ab/cd/file.txt"),
            PathBuf::from("/data/ab/cd/file.txt")
        );
    }

    #[test]
    fn resolve_absolute_file_uri_different_base() {
        // URI points to a different directory than base_path — still works
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("file:///other/ab/cd/file.txt"),
            PathBuf::from("/other/ab/cd/file.txt")
        );
    }

    #[tokio::test]
    async fn test_retrieve_with_file_uri() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        storage.initialize().await.unwrap();

        let data = b"URI test data";
        let relative = storage.store(data, "uri_test.txt").await.unwrap();

        // Build a file:// URI the same way the ingestion pipeline does
        let uri = format!("file://{}", temp_dir.path().join(&relative).display());

        let retrieved = storage.retrieve(&uri).await.unwrap();
        assert_eq!(data.to_vec(), retrieved);
    }

    #[tokio::test]
    async fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"To be deleted";
        let location = storage.store(data, "delete.txt").await.unwrap();

        assert!(storage.exists(&location).await.unwrap());

        storage.delete(&location).await.unwrap();

        assert!(!storage.exists(&location).await.unwrap());
    }
}
