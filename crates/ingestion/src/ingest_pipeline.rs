use std::path::Path;
use std::sync::Arc;
use tracing::{info, info_span, instrument};
use uuid::Uuid;

use cognee_database::DatabaseTrait;
use cognee_models::{Data, DataInput, Dataset};
use cognee_storage::StorageTrait;

pub struct IngestPipeline {
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn DatabaseTrait>,
}

impl IngestPipeline {
    pub fn new(storage: Arc<dyn StorageTrait>, database: Arc<dyn DatabaseTrait>) -> Self {
        Self { storage, database }
    }

    #[instrument(name = "ingestion.add", skip(self, inputs), fields(dataset_name, owner_id = %owner_id, inputs_count = inputs.len()))]
    pub async fn add(
        &self,
        inputs: Vec<DataInput>,
        dataset_name: &str,
        owner_id: Uuid,
    ) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
        let dataset = match self
            .database
            .get_dataset_by_name(dataset_name, owner_id)
            .await?
        {
            Some(ds) => ds,
            None => {
                let new_dataset = Dataset::new(dataset_name.to_string(), owner_id);
                self.database.create_dataset(new_dataset).await?
            }
        };
        info!(dataset_id = %dataset.id, "dataset resolved");

        let mut created_data = Vec::new();

        for (idx, input) in inputs.into_iter().enumerate() {
            let _input_span = info_span!("ingestion.process_input", idx).entered();

            let (content_hash, data_id, storage_location) =
                self.process_input_streaming(&input, owner_id).await?;

            if let Some(existing_data) = self.database.get_data(data_id).await? {
                self.database
                    .attach_data_to_dataset(dataset.id, data_id)
                    .await?;
                info!(data_id = %data_id, is_duplicate = true, "input processed");
                created_data.push(existing_data);
                continue;
            }

            let data = Data::new(
                data_id,
                self.extract_name(&input),
                storage_location.clone(),
                self.extract_original_location(&input),
                self.extract_extension(&input),
                self.extract_mime_type(&input),
                content_hash,
                owner_id,
            );

            let saved_data = self.database.create_data(data).await?;

            self.database
                .attach_data_to_dataset(dataset.id, data_id)
                .await?;

            info!(data_id = %data_id, is_duplicate = false, "input processed");
            created_data.push(saved_data);
        }

        Ok(created_data)
    }

    /// Process input using streaming to avoid loading large files into memory
    /// Returns (content_hash, data_id, storage_location)
    #[instrument(name = "ingestion.process_input_streaming", skip(self, input), fields(owner_id = %owner_id))]
    async fn process_input_streaming(
        &self,
        input: &DataInput,
        owner_id: Uuid,
    ) -> Result<(String, Uuid, String), Box<dyn std::error::Error>> {
        use sha2::{Digest, Sha256};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let file_name = match input {
            DataInput::FilePath(path) => {
                let clean_path = path.strip_prefix("file://").unwrap_or(path);
                Path::new(clean_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file.bin")
                    .to_string()
            }
            DataInput::Text(_) => format!("text_{}.txt", Uuid::new_v4()),
            DataInput::Url(_) => return Err("URL fetching not yet implemented".into()),
        };

        let hasher = Arc::new(Mutex::new(Sha256::new()));
        let writer = Arc::new(Mutex::new(self.storage.create_writer(&file_name).await?));

        let hasher_clone = hasher.clone();
        let writer_clone = writer.clone();

        input
            .process_by_chunks(move |chunk| {
                let hasher = hasher_clone.clone();
                let writer = writer_clone.clone();
                let chunk_owned = chunk.to_vec(); // Copy chunk to own the data
                async move {
                    hasher.lock().await.update(&chunk_owned);

                    writer.lock().await.write_chunk(&chunk_owned).await?;

                    Ok::<(), Box<dyn std::error::Error>>(())
                }
            })
            .await?;

        let mut hasher = Arc::try_unwrap(hasher)
            .map_err(|_| "Failed to unwrap hasher")?
            .into_inner();
        hasher.update(owner_id.as_bytes());
        let hash_result = hasher.finalize();
        let content_hash = format!("{:x}", hash_result);
        let data_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, content_hash.as_bytes());

        let writer = Arc::try_unwrap(writer)
            .map_err(|_| "Failed to unwrap writer")?
            .into_inner();
        let storage_location = writer.finish().await?;

        Ok((content_hash, data_id, storage_location))
    }

    fn extract_name(&self, input: &DataInput) -> String {
        match input {
            DataInput::Text(_) => "inline_text".to_string(),
            DataInput::FilePath(path) => {
                let clean_path = path.strip_prefix("file://").unwrap_or(path);
                Path::new(clean_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            }
            DataInput::Url(url) => url
                .split('/')
                .next_back()
                .unwrap_or("url_content")
                .to_string(),
        }
    }

    fn extract_original_location(&self, input: &DataInput) -> String {
        match input {
            DataInput::Text(_) => "text://inline".to_string(),
            DataInput::FilePath(path) => {
                if path.starts_with("file://") {
                    path.clone()
                } else {
                    format!("file://{}", path)
                }
            }
            DataInput::Url(url) => url.clone(),
        }
    }

    fn extract_extension(&self, input: &DataInput) -> String {
        match input {
            DataInput::Text(_) => "txt".to_string(),
            DataInput::FilePath(path) => {
                let clean_path = path.strip_prefix("file://").unwrap_or(path);
                Path::new(clean_path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("txt")
                    .to_string()
            }
            DataInput::Url(url) => url
                .split('/')
                .next_back()
                .and_then(|s| s.split('.').next_back())
                .unwrap_or("html")
                .to_string(),
        }
    }

    fn extract_mime_type(&self, input: &DataInput) -> String {
        match input {
            DataInput::Text(_) => "text/plain".to_string(),
            DataInput::FilePath(path) => {
                let clean_path = path.strip_prefix("file://").unwrap_or(path);
                mime_guess::from_path(clean_path)
                    .first_or_octet_stream()
                    .to_string()
            }
            DataInput::Url(_) => "text/html".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::MockDatabase;
    use cognee_storage::MockStorage;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_pipeline() -> (IngestPipeline, Arc<MockDatabase>) {
        let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let pipeline = IngestPipeline::new(storage, database.clone() as Arc<dyn DatabaseTrait>);
        (pipeline, database)
    }

    #[tokio::test]
    async fn test_add_text_input() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let inputs = vec![DataInput::Text("Hello, world!".to_string())];

        let result = pipeline.add(inputs, "test_dataset", owner_id).await;
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].name, "inline_text");
        assert_eq!(data[0].mime_type, "text/plain");
        assert_eq!(data[0].extension, "txt");

        // Verify data was stored
        assert_eq!(database.get_data_count(), 1);
        assert_eq!(database.get_dataset_count(), 1);
    }

    #[tokio::test]
    async fn test_add_file_input() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        // Create a temporary file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "Test file content").unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let inputs = vec![DataInput::FilePath(file_path.clone())];

        let result = pipeline.add(inputs, "test_dataset", owner_id).await;
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 1);
        assert!(!data[0].name.is_empty());
        // Extension could vary based on temp file implementation
        assert!(!data[0].extension.is_empty());

        // Verify data was stored
        assert_eq!(database.get_data_count(), 1);
        assert_eq!(database.get_dataset_count(), 1);
    }

    #[tokio::test]
    async fn test_add_multiple_inputs() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let inputs = vec![
            DataInput::Text("First text".to_string()),
            DataInput::Text("Second text".to_string()),
        ];

        let result = pipeline.add(inputs, "test_dataset", owner_id).await;
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 2);

        // Verify all data was stored
        assert_eq!(database.get_data_count(), 2);
        assert_eq!(database.get_dataset_count(), 1);
    }

    #[tokio::test]
    async fn test_deduplication_same_content() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let content = "Duplicate content";
        let inputs1 = vec![DataInput::Text(content.to_string())];
        let inputs2 = vec![DataInput::Text(content.to_string())];

        // Add first time
        let result1 = pipeline.add(inputs1, "test_dataset", owner_id).await;
        assert!(result1.is_ok());
        let data1 = result1.unwrap();

        // Add second time (should deduplicate)
        let result2 = pipeline.add(inputs2, "test_dataset", owner_id).await;
        assert!(result2.is_ok());
        let data2 = result2.unwrap();

        // Should have same ID (deduplication)
        assert_eq!(data1[0].id, data2[0].id);
        assert_eq!(data1[0].content_hash, data2[0].content_hash);

        // Should only have one data record
        assert_eq!(database.get_data_count(), 1);

        // But both should be attached to the dataset
        let dataset = pipeline
            .database
            .get_dataset_by_name("test_dataset", owner_id)
            .await
            .unwrap()
            .unwrap();
        let dataset_data = pipeline
            .database
            .get_dataset_data(dataset.id)
            .await
            .unwrap();
        assert_eq!(dataset_data.len(), 1);
    }

    #[tokio::test]
    async fn test_different_owners_different_hash() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id1 = Uuid::new_v4();
        let owner_id2 = Uuid::new_v4();

        let content = "Same content";
        let inputs1 = vec![DataInput::Text(content.to_string())];
        let inputs2 = vec![DataInput::Text(content.to_string())];

        // Add with first owner
        let result1 = pipeline.add(inputs1, "dataset1", owner_id1).await.unwrap();

        // Add with second owner
        let result2 = pipeline.add(inputs2, "dataset2", owner_id2).await.unwrap();

        // Should have different IDs (different owners)
        assert_ne!(result1[0].id, result2[0].id);
        assert_ne!(result1[0].content_hash, result2[0].content_hash);

        // Should have two separate data records
        assert_eq!(database.get_data_count(), 2);
    }

    #[tokio::test]
    async fn test_multiple_datasets() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let inputs1 = vec![DataInput::Text("Content 1".to_string())];
        let inputs2 = vec![DataInput::Text("Content 2".to_string())];

        // Add to first dataset
        let result1 = pipeline.add(inputs1, "dataset1", owner_id).await;
        assert!(result1.is_ok());

        // Add to second dataset
        let result2 = pipeline.add(inputs2, "dataset2", owner_id).await;
        assert!(result2.is_ok());

        // Should have two datasets
        assert_eq!(database.get_dataset_count(), 2);
        assert_eq!(database.get_data_count(), 2);
    }

    #[tokio::test]
    async fn test_reuse_dataset() {
        let (pipeline, database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let inputs1 = vec![DataInput::Text("Content 1".to_string())];
        let inputs2 = vec![DataInput::Text("Content 2".to_string())];

        // Add to same dataset twice
        let _result1 = pipeline
            .add(inputs1, "same_dataset", owner_id)
            .await
            .unwrap();
        let _result2 = pipeline
            .add(inputs2, "same_dataset", owner_id)
            .await
            .unwrap();

        // Should only have one dataset
        assert_eq!(database.get_dataset_count(), 1);
        assert_eq!(database.get_data_count(), 2);

        // Both data should be attached to the same dataset
        let dataset = pipeline
            .database
            .get_dataset_by_name("same_dataset", owner_id)
            .await
            .unwrap()
            .unwrap();
        let dataset_data = pipeline
            .database
            .get_dataset_data(dataset.id)
            .await
            .unwrap();
        assert_eq!(dataset_data.len(), 2);
    }

    #[tokio::test]
    async fn test_extract_name_from_text() {
        let (pipeline, _database) = create_test_pipeline();
        let input = DataInput::Text("test".to_string());
        assert_eq!(pipeline.extract_name(&input), "inline_text");
    }

    #[tokio::test]
    async fn test_extract_name_from_file_path() {
        let (pipeline, _database) = create_test_pipeline();
        let input = DataInput::FilePath("/path/to/file.txt".to_string());
        assert_eq!(pipeline.extract_name(&input), "file.txt");
    }

    #[tokio::test]
    async fn test_extract_extension_from_text() {
        let (pipeline, _database) = create_test_pipeline();
        let input = DataInput::Text("test".to_string());
        assert_eq!(pipeline.extract_extension(&input), "txt");
    }

    #[tokio::test]
    async fn test_extract_extension_from_file_path() {
        let (pipeline, _database) = create_test_pipeline();
        let input = DataInput::FilePath("/path/to/file.rs".to_string());
        assert_eq!(pipeline.extract_extension(&input), "rs");
    }

    #[tokio::test]
    async fn test_extract_mime_type_from_text() {
        let (pipeline, _database) = create_test_pipeline();
        let input = DataInput::Text("test".to_string());
        assert_eq!(pipeline.extract_mime_type(&input), "text/plain");
    }

    #[tokio::test]
    async fn test_content_hash_deterministic() {
        let (pipeline, _database) = create_test_pipeline();
        let owner_id = Uuid::new_v4();

        let inputs1 = vec![DataInput::Text("Test content".to_string())];
        let inputs2 = vec![DataInput::Text("Test content".to_string())];

        let result1 = pipeline.add(inputs1, "dataset1", owner_id).await.unwrap();
        let result2 = pipeline.add(inputs2, "dataset1", owner_id).await.unwrap();

        // Same content and owner should produce same hash
        assert_eq!(result1[0].content_hash, result2[0].content_hash);
        assert_eq!(result1[0].id, result2[0].id);
    }
}
