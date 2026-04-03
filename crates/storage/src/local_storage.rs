use super::storage_trait::{StorageError, StorageTrait, StorageWriter};
use async_trait::async_trait;
use std::path::PathBuf;
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

        format!("{}/{}/{}", dir1, dir2, file_name)
    }
}

#[async_trait]
impl StorageTrait for LocalStorage {
    async fn initialize(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.base_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create base directory: {}", e)))
    }

    #[instrument(name = "storage.store", skip(self, data), fields(file_name, bytes = data.len()))]
    async fn store(&self, data: &[u8], file_name: &str) -> Result<String, StorageError> {
        let relative_path = self.generate_storage_path(file_name);
        let full_path = self.base_path.join(&relative_path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {}", e)))?;
        }

        // Write file
        let mut file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {}", e)))?;

        file.write_all(data)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write file: {}", e)))?;

        file.flush()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to flush file: {}", e)))?;

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
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {}", e)))?;
        }

        // Create file
        let mut file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {}", e)))?;

        // Stream copy from reader to file
        tokio::io::copy(reader, &mut file)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to write file: {}", e)))?;

        file.flush()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to flush file: {}", e)))?;

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
                .map_err(|e| StorageError::IoError(format!("Failed to create directory: {}", e)))?;
        }

        // Create file
        let file = fs::File::create(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {}", e)))?;

        Ok(StorageWriter::new(file, relative_path))
    }

    #[instrument(name = "storage.retrieve", skip(self), fields(location))]
    async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError> {
        let full_path = self.base_path.join(location);

        let bytes = fs::read(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {}", location))
            } else {
                StorageError::IoError(format!("Failed to read file: {}", e))
            }
        })?;
        debug!(bytes = bytes.len(), "file retrieved");
        Ok(bytes)
    }

    async fn exists(&self, location: &str) -> Result<bool, StorageError> {
        let full_path = self.base_path.join(location);

        fs::try_exists(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to check file existence: {}", e)))
    }

    #[instrument(name = "storage.delete", skip(self), fields(location))]
    async fn delete(&self, location: &str) -> Result<(), StorageError> {
        let full_path = self.base_path.join(location);

        fs::remove_file(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {}", location))
            } else {
                StorageError::IoError(format!("Failed to delete file: {}", e))
            }
        })
    }

    fn get_full_path(&self, location: &str) -> PathBuf {
        self.base_path.join(location)
    }

    fn base_path(&self) -> &str {
        self.base_path.to_str().unwrap_or("")
    }
}

#[cfg(test)]
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
