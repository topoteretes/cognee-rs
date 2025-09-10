use crate::data::payload_base::PayloadBase;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyStatus {
    Empty,
    Processing,
    Done,
    Errored(String),
}

#[derive(Debug, Clone)]
pub struct CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    base: Arc<RwLock<PayloadBase>>,
    chunks: Arc<RwLock<Vec<Arc<TC>>>>,
    result1: Arc<RwLock<Vec<Arc<T1>>>>,
    result2: Arc<RwLock<Vec<Arc<T2>>>>,
    property_status: Arc<Mutex<HashMap<String, PropertyStatus>>>,
}

impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    pub fn new(chunks: Vec<Arc<TC>>) -> Self {
        let mut status = HashMap::new();
        status.insert("base".to_string(), PropertyStatus::Done);
        status.insert(
            "chunks".to_string(),
            if chunks.is_empty() {
                PropertyStatus::Empty
            } else {
                PropertyStatus::Done
            },
        );
        status.insert("result1".to_string(), PropertyStatus::Empty);
        status.insert("result2".to_string(), PropertyStatus::Empty);

        Self {
            base: Arc::new(RwLock::new(PayloadBase::new())),
            chunks: Arc::new(RwLock::new(chunks)),
            result1: Arc::new(RwLock::new(Vec::new())),
            result2: Arc::new(RwLock::new(Vec::new())),
            property_status: Arc::new(Mutex::new(status)),
        }
    }

    pub fn chunks_arc(&self) -> Arc<RwLock<Vec<Arc<TC>>>> {
        Arc::clone(&self.chunks)
    }

    pub fn get_chunks_copy(&self) -> Vec<Arc<TC>> {
        let chunks = self.chunks.read().unwrap();
        chunks.clone()
    }

    pub fn result1_arc(&self) -> Arc<RwLock<Vec<Arc<T1>>>> {
        Arc::clone(&self.result1)
    }

    pub fn result2_arc(&self) -> Arc<RwLock<Vec<Arc<T2>>>> {
        Arc::clone(&self.result2)
    }

    pub fn property_status_arc(&self) -> Arc<Mutex<HashMap<String, PropertyStatus>>> {
        Arc::clone(&self.property_status)
    }

    pub fn get_result1_copy(&self) -> Vec<Arc<T1>> {
        let result1 = self.result1.read().unwrap();
        result1.clone()
    }

    pub fn get_result2_copy(&self) -> Vec<Arc<T2>> {
        let result2 = self.result2.read().unwrap();
        result2.clone()
    }

    // Status dictionary methods
    pub fn get_property_status(&self, property: &str) -> Option<PropertyStatus> {
        let status = self.property_status.lock().unwrap();
        status.get(property).cloned()
    }

    pub fn set_property_status(&self, property: &str, status: PropertyStatus) {
        let mut status_map = self.property_status.lock().unwrap();
        status_map.insert(property.to_string(), status);
    }
}

impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    pub fn id(&self) -> Uuid {
        let base = self.base.read().unwrap();
        base.metainfo.id
    }
}

#[tokio::test]
async fn parallel_readers_no_copy() {
    use std::time::Duration;
    let initial_chunks: Vec<Arc<String>> = (0..1023)
        .map(|i| {
            let content = match i % 5 {
                0 => format!("document_text_{i:04}_analysis_ready"),
                1 => format!("embedding_vector_{i:04}_processed"),
                2 => format!("memory_fragment_{i:04}_indexed"),
                3 => format!("knowledge_node_{i:04}_connected"),
                _ => format!("data_segment_{i:04}_transformed"),
            };
            Arc::new(content)
        })
        .collect();

    let payload = Arc::new(CogneePayload::<String, String, String>::new(initial_chunks));

    let chunks_arc = payload.chunks_arc();
    let result1_arc = payload.result1_arc();
    let result2_arc = payload.result2_arc();

    let mut handles = Vec::new();

    // ---- Task 1: process chunks in batches and move to result1 ----
    let result1 = Arc::clone(&result1_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t1 = tokio::spawn(async move {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!("Task 1 starting - moving {total_chunks} chunks to result1...");

        const BATCH_SIZE: usize = 100;
        let mut total_processed = 0;

        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                {
                    let chunks_guard = chunks_ref.read().unwrap();
                    for i in batch_start..batch_end {
                        let chunk = Arc::clone(&chunks_guard[i]);
                        batch_results.push(chunk);
                    }
                }
                println!("Batch processing starts");
                let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                println!("Batch processing ends");

                {
                    let mut result1_guard = result1.write().unwrap();
                    result1_guard.extend(batch_results);
                }
            }

            total_processed += batch_end - batch_start;
            println!(
                "Task 1: processed {}/{} chunks (batch size: {})",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 1 completed - moved chunks to result1");
    });
    handles.push(t1);

    // ---- Task 2: process chunks in batches and move to result2 ----
    let result2 = Arc::clone(&result2_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t2 = tokio::spawn(async move {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!("Task 2 starting - moving {total_chunks} chunks to result2...");

        const BATCH_SIZE: usize = 50;
        let mut total_processed = 0;

        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                {
                    let chunks_guard = chunks_ref.read().unwrap();
                    for i in batch_start..batch_end {
                        let chunk = Arc::clone(&chunks_guard[i]);
                        batch_results.push(chunk);
                    }
                }

                println!("Batch processing starts");
                let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                println!("Batch processing ends");

                {
                    let mut result2_guard = result2.write().unwrap();
                    result2_guard.extend(batch_results);
                }
            }

            total_processed += batch_end - batch_start;
            println!(
                "Task 2: processed {}/{} chunks (batch size: {})",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 2 completed - moved chunks to result2");
    });
    handles.push(t2);

    println!(
        "Phase 1: Waiting for {} initial tasks to complete...",
        handles.len()
    );
    for (i, handle) in handles.into_iter().enumerate() {
        handle.await.unwrap();
        println!("Task {} completed!", i + 1);
    }
    println!("All processing completed!");
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn test_property_status_enum() {
        // Test PropertyStatus enum variants
        let empty = PropertyStatus::Empty;
        let processing = PropertyStatus::Processing;
        let done = PropertyStatus::Done;
        let errored = PropertyStatus::Errored("test error".to_string());

        assert_eq!(empty, PropertyStatus::Empty);
        assert_eq!(processing, PropertyStatus::Processing);
        assert_eq!(done, PropertyStatus::Done);
        assert_eq!(errored, PropertyStatus::Errored("test error".to_string()));
    }

    #[test]
    fn test_initial_status_with_empty_chunks() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);

        // Check initial statuses
        assert_eq!(
            payload.get_property_status("base"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("chunks"),
            Some(PropertyStatus::Empty)
        );
        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Empty)
        );
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Empty)
        );
    }

    #[test]
    fn test_initial_status_with_chunks() {
        let chunks = vec![Arc::new("test chunk".to_string())];
        let payload = CogneePayload::<String, String, String>::new(chunks);

        // Check initial statuses
        assert_eq!(
            payload.get_property_status("base"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("chunks"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Empty)
        );
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Empty)
        );
    }

    #[test]
    fn test_set_and_get_property_status() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);

        // Set processing status
        payload.set_property_status("chunks", PropertyStatus::Processing);
        assert_eq!(
            payload.get_property_status("chunks"),
            Some(PropertyStatus::Processing)
        );

        // Set error status
        payload.set_property_status("result1", PropertyStatus::Errored("test error".to_string()));
        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Errored("test error".to_string()))
        );

        // Set done status
        payload.set_property_status("result2", PropertyStatus::Done);
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Done)
        );
    }

    #[test]
    fn test_property_status_with_different_types() {
        // Test with different generic types
        let payload = CogneePayload::<i32, f64, bool>::new(vec![Arc::new(42)]);

        assert_eq!(
            payload.get_property_status("base"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("chunks"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Empty)
        );
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Empty)
        );

        // Manually set statuses
        payload.set_property_status("result1", PropertyStatus::Processing);
        payload.set_property_status("result1", PropertyStatus::Done);

        payload.set_property_status("result2", PropertyStatus::Done);

        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Done)
        );
    }

    #[test]
    fn test_any_property_name_allowed() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);

        // Can set status for any property name (no validation)
        payload.set_property_status("custom_property", PropertyStatus::Processing);
        assert_eq!(
            payload.get_property_status("custom_property"),
            Some(PropertyStatus::Processing)
        );

        payload.set_property_status(
            "another_prop",
            PropertyStatus::Errored("custom error".to_string()),
        );
        assert_eq!(
            payload.get_property_status("another_prop"),
            Some(PropertyStatus::Errored("custom error".to_string()))
        );
    }
}
