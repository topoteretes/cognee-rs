use super::storage_trait::{StorageError, StorageTrait, StorageWriter};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncRead;

/// Mock storage for testing
/// Stores data in memory using a HashMap
#[derive(Clone)]
pub struct MockStorage {
    data: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    counter: Arc<Mutex<usize>>,
}

impl MockStorage {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(Mutex::new(0)),
        }
    }

    fn generate_location(&self) -> String {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        format!("mock/{}.bin", counter)
    }

    pub fn get_stored_data(&self, location: &str) -> Option<Vec<u8>> {
        self.data.lock().unwrap().get(location).cloned()
    }

    pub fn get_all_locations(&self) -> Vec<String> {
        self.data.lock().unwrap().keys().cloned().collect()
    }
}

impl Default for MockStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StorageTrait for MockStorage {
    async fn initialize(&self) -> Result<(), StorageError> {
        Ok(())
    }

    async fn store(&self, data: &[u8], _file_name: &str) -> Result<String, StorageError> {
        let location = self.generate_location();
        self.data
            .lock()
            .unwrap()
            .insert(location.clone(), data.to_vec());
        Ok(location)
    }

    async fn store_stream<R: AsyncRead + Unpin + Send>(
        &self,
        reader: &mut R,
        _file_name: &str,
    ) -> Result<String, StorageError> {
        use tokio::io::AsyncReadExt;

        let mut buffer = Vec::new();
        reader
            .read_to_end(&mut buffer)
            .await
            .map_err(|e| StorageError::IoError(e.to_string()))?;

        let location = self.generate_location();
        self.data.lock().unwrap().insert(location.clone(), buffer);
        Ok(location)
    }

    async fn create_writer(&self, _file_name: &str) -> Result<StorageWriter, StorageError> {
        // For mock storage, we'll create a temporary file and then move it to memory
        // This is a simplified implementation for testing
        let location = self.generate_location();
        let temp_file =
            tempfile::NamedTempFile::new().map_err(|e| StorageError::IoError(e.to_string()))?;

        // We need to store the data reference and location for later
        // In a real implementation, we'd have a custom writer that writes directly to the HashMap
        // For now, we'll use a file-backed approach
        Ok(StorageWriter::new(
            tokio::fs::File::from_std(
                temp_file
                    .reopen()
                    .map_err(|e| StorageError::IoError(e.to_string()))?,
            ),
            location,
        ))
    }

    async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError> {
        self.data
            .lock()
            .unwrap()
            .get(location)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(format!("Location not found: {}", location)))
    }

    async fn exists(&self, location: &str) -> Result<bool, StorageError> {
        Ok(self.data.lock().unwrap().contains_key(location))
    }

    async fn delete(&self, location: &str) -> Result<(), StorageError> {
        self.data
            .lock()
            .unwrap()
            .remove(location)
            .ok_or_else(|| StorageError::NotFound(format!("Location not found: {}", location)))?;
        Ok(())
    }

    fn get_full_path(&self, location: &str) -> PathBuf {
        PathBuf::from(format!("/mock/{}", location))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_storage_store_and_retrieve() {
        let storage = MockStorage::new();
        let data = b"test data";

        let location = storage.store(data, "test.txt").await.unwrap();
        let retrieved = storage.retrieve(&location).await.unwrap();

        assert_eq!(data.to_vec(), retrieved);
    }

    #[tokio::test]
    async fn test_mock_storage_exists() {
        let storage = MockStorage::new();
        let data = b"test data";

        let location = storage.store(data, "test.txt").await.unwrap();

        assert!(storage.exists(&location).await.unwrap());
        assert!(!storage.exists("nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_storage_delete() {
        let storage = MockStorage::new();
        let data = b"test data";

        let location = storage.store(data, "test.txt").await.unwrap();
        assert!(storage.exists(&location).await.unwrap());

        storage.delete(&location).await.unwrap();
        assert!(!storage.exists(&location).await.unwrap());
    }
}
