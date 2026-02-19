use super::database_trait::{
    DatabaseError, DatabaseTrait, SearchHistoryEntry, SearchHistoryEntryType,
};
use async_trait::async_trait;
use chrono::Utc;
use cognee_models::{Data, Dataset};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
struct QueryLog {
    id: Uuid,
    query_text: String,
    query_type: String,
    user_id: Option<Uuid>,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct ResultLog {
    id: Uuid,
    query_id: Uuid,
    serialized_result: String,
    user_id: Option<Uuid>,
    created_at: chrono::DateTime<Utc>,
}

/// Mock database for testing
/// Stores data in memory using HashMaps
#[derive(Clone)]
pub struct MockDatabase {
    datasets: Arc<Mutex<HashMap<Uuid, Dataset>>>,
    data: Arc<Mutex<HashMap<Uuid, Data>>>,
    dataset_data: Arc<Mutex<HashMap<Uuid, Vec<Uuid>>>>, // dataset_id -> vec of data_ids
    dataset_by_name: Arc<Mutex<HashMap<(String, Uuid), Uuid>>>, // (name, owner_id) -> dataset_id
    query_logs: Arc<Mutex<Vec<QueryLog>>>,
    result_logs: Arc<Mutex<Vec<ResultLog>>>,
}

impl MockDatabase {
    pub fn new() -> Self {
        Self {
            datasets: Arc::new(Mutex::new(HashMap::new())),
            data: Arc::new(Mutex::new(HashMap::new())),
            dataset_data: Arc::new(Mutex::new(HashMap::new())),
            dataset_by_name: Arc::new(Mutex::new(HashMap::new())),
            query_logs: Arc::new(Mutex::new(Vec::new())),
            result_logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn get_dataset_count(&self) -> usize {
        self.datasets.lock().unwrap().len()
    }

    pub fn get_data_count(&self) -> usize {
        self.data.lock().unwrap().len()
    }

    pub fn get_all_datasets(&self) -> Vec<Dataset> {
        self.datasets.lock().unwrap().values().cloned().collect()
    }

    pub fn get_all_data(&self) -> Vec<Data> {
        self.data.lock().unwrap().values().cloned().collect()
    }
}

impl Default for MockDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DatabaseTrait for MockDatabase {
    async fn initialize(&self) -> Result<(), DatabaseError> {
        // Nothing to initialize for in-memory storage
        Ok(())
    }

    async fn create_data(&self, data: Data) -> Result<Data, DatabaseError> {
        let mut data_map = self.data.lock().unwrap();

        if data_map.contains_key(&data.id) {
            return Err(DatabaseError::UniqueViolation(format!(
                "Data with id {} already exists",
                data.id
            )));
        }

        data_map.insert(data.id, data.clone());
        Ok(data)
    }

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        Ok(self.data.lock().unwrap().get(&id).cloned())
    }

    async fn update_data(&self, data: Data) -> Result<Data, DatabaseError> {
        let mut data_map = self.data.lock().unwrap();

        if !data_map.contains_key(&data.id) {
            return Err(DatabaseError::NotFound(format!(
                "Data with id {} not found",
                data.id
            )));
        }

        data_map.insert(data.id, data.clone());
        Ok(data)
    }

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
        let dataset_data = self.dataset_data.lock().unwrap();
        let data_map = self.data.lock().unwrap();

        let data_ids = dataset_data.get(&dataset_id).cloned().unwrap_or_default();

        let mut result = Vec::new();
        for data_id in data_ids {
            if let Some(data) = data_map.get(&data_id) {
                result.push(data.clone());
            }
        }

        Ok(result)
    }

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError> {
        let mut datasets = self.datasets.lock().unwrap();
        let mut dataset_by_name = self.dataset_by_name.lock().unwrap();

        if datasets.contains_key(&dataset.id) {
            return Err(DatabaseError::UniqueViolation(format!(
                "Dataset with id {} already exists",
                dataset.id
            )));
        }

        let key = (dataset.name.clone(), dataset.owner_id);
        dataset_by_name.insert(key, dataset.id);
        datasets.insert(dataset.id, dataset.clone());

        Ok(dataset)
    }

    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        Ok(self.datasets.lock().unwrap().get(&id).cloned())
    }

    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
    ) -> Result<Option<Dataset>, DatabaseError> {
        let dataset_by_name = self.dataset_by_name.lock().unwrap();
        let datasets = self.datasets.lock().unwrap();

        let key = (name.to_string(), owner_id);
        if let Some(dataset_id) = dataset_by_name.get(&key) {
            Ok(datasets.get(dataset_id).cloned())
        } else {
            Ok(None)
        }
    }

    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let mut dataset_data = self.dataset_data.lock().unwrap();

        let data_ids = dataset_data.entry(dataset_id).or_default();

        // Only add if not already present
        if !data_ids.contains(&data_id) {
            data_ids.push(data_id);
        }

        Ok(())
    }

    async fn log_query(
        &self,
        query_text: &str,
        query_type: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        let query_id = Uuid::new_v4();

        self.query_logs.lock().unwrap().push(QueryLog {
            id: query_id,
            query_text: query_text.to_string(),
            query_type: query_type.to_string(),
            user_id,
            created_at: Utc::now(),
        });

        Ok(query_id)
    }

    async fn log_result(
        &self,
        query_id: Uuid,
        serialized_result: &str,
        user_id: Option<Uuid>,
    ) -> Result<Uuid, DatabaseError> {
        let result_id = Uuid::new_v4();

        self.result_logs.lock().unwrap().push(ResultLog {
            id: result_id,
            query_id,
            serialized_result: serialized_result.to_string(),
            user_id,
            created_at: Utc::now(),
        });

        Ok(result_id)
    }

    async fn get_history(
        &self,
        user_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
        let query_logs = self.query_logs.lock().unwrap().clone();
        let result_logs = self.result_logs.lock().unwrap().clone();

        let mut history_entries = Vec::new();

        for query in query_logs {
            if user_id.is_none() || query.user_id == user_id {
                history_entries.push(SearchHistoryEntry {
                    entry_id: query.id,
                    query_id: query.id,
                    entry_type: SearchHistoryEntryType::Query,
                    content: query.query_text,
                    query_type: Some(query.query_type),
                    user_id: query.user_id,
                    created_at: query.created_at,
                });
            }
        }

        let query_type_by_id: HashMap<Uuid, String> = self
            .query_logs
            .lock()
            .unwrap()
            .iter()
            .map(|q| (q.id, q.query_type.clone()))
            .collect();

        for result in result_logs {
            if user_id.is_none() || result.user_id == user_id {
                history_entries.push(SearchHistoryEntry {
                    entry_id: result.id,
                    query_id: result.query_id,
                    entry_type: SearchHistoryEntryType::Result,
                    content: result.serialized_result,
                    query_type: query_type_by_id.get(&result.query_id).cloned(),
                    user_id: result.user_id,
                    created_at: result.created_at,
                });
            }
        }

        history_entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        if let Some(limit) = limit {
            history_entries.truncate(limit);
        }

        Ok(history_entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_database_create_and_get_data() {
        let db = MockDatabase::new();
        let owner_id = Uuid::new_v4();

        let data = Data::new(
            Uuid::new_v4(),
            "test.txt".to_string(),
            "storage/test.txt".to_string(),
            "file://test.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "hash123".to_string(),
            owner_id,
        );

        let created = db.create_data(data.clone()).await.unwrap();
        assert_eq!(created.id, data.id);

        let retrieved = db.get_data(data.id).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, data.id);
    }

    #[tokio::test]
    async fn test_mock_database_create_and_get_dataset() {
        let db = MockDatabase::new();
        let owner_id = Uuid::new_v4();

        let dataset = Dataset::new("test_dataset".to_string(), owner_id);

        let created = db.create_dataset(dataset.clone()).await.unwrap();
        assert_eq!(created.id, dataset.id);

        let retrieved = db.get_dataset(dataset.id).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test_dataset");

        let by_name = db
            .get_dataset_by_name("test_dataset", owner_id)
            .await
            .unwrap();
        assert!(by_name.is_some());
        assert_eq!(by_name.unwrap().id, dataset.id);
    }

    #[tokio::test]
    async fn test_mock_database_attach_data_to_dataset() {
        let db = MockDatabase::new();
        let owner_id = Uuid::new_v4();

        let dataset = Dataset::new("test_dataset".to_string(), owner_id);
        db.create_dataset(dataset.clone()).await.unwrap();

        let data = Data::new(
            Uuid::new_v4(),
            "test.txt".to_string(),
            "storage/test.txt".to_string(),
            "file://test.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "hash123".to_string(),
            owner_id,
        );
        db.create_data(data.clone()).await.unwrap();

        db.attach_data_to_dataset(dataset.id, data.id)
            .await
            .unwrap();

        let dataset_data = db.get_dataset_data(dataset.id).await.unwrap();
        assert_eq!(dataset_data.len(), 1);
        assert_eq!(dataset_data[0].id, data.id);
    }

    #[tokio::test]
    async fn test_mock_database_search_history() {
        let db = MockDatabase::new();
        let query_id = db
            .log_query("what is cognee", "CHUNKS", None)
            .await
            .unwrap();

        db.log_result(query_id, "{\"result\":\"ok\"}", None)
            .await
            .unwrap();

        let history = db.get_history(None, Some(10)).await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .any(|entry| entry.entry_type == SearchHistoryEntryType::Query)
        );
        assert!(
            history
                .iter()
                .any(|entry| entry.entry_type == SearchHistoryEntryType::Result)
        );
    }
}
