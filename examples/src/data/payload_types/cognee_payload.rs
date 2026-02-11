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
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    base: Arc<RwLock<PayloadBase>>,
    chunks: Arc<RwLock<Vec<Arc<TC>>>>,
    result1: Arc<RwLock<Vec<Arc<T1>>>>,
    result2: Arc<RwLock<Vec<Arc<T2>>>>,
    property_status: Arc<Mutex<HashMap<String, PropertyStatus>>>,
}

impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
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

    pub fn get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        match property {
            "chunks" => Ok(Box::new(Arc::clone(&self.chunks))),
            "result1" => Ok(Box::new(Arc::clone(&self.result1))),
            "result2" => Ok(Box::new(Arc::clone(&self.result2))),
            "property_status" => Ok(Box::new(Arc::clone(&self.property_status))),
            _ => Err(format!("Unknown property: {property}")),
        }
    }

    pub fn get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        match property {
            "chunks" => {
                let chunks = self.chunks.read().unwrap();
                Ok(Box::new(chunks.clone()))
            }
            "result1" => {
                let result1 = self.result1.read().unwrap();
                Ok(Box::new(result1.clone()))
            }
            "result2" => {
                let result2 = self.result2.read().unwrap();
                Ok(Box::new(result2.clone()))
            }
            _ => Err(format!("Unknown property: {property}")),
        }
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

    pub fn get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
        let status = self.property_status.lock().unwrap();
        status.clone()
    }
}

impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    pub fn id(&self) -> Uuid {
        let base = self.base.read().unwrap();
        base.metainfo.id
    }
}

// Implement PayloadTrait for CogneePayload
impl<TC, T1, T2> crate::data::payload_trait::PayloadTrait for CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    fn payload_id(&self) -> Uuid {
        self.id()
    }

    fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus> {
        self.get_property_status(property)
    }

    fn payload_set_property_status(&self, property: &str, status: PropertyStatus) {
        self.set_property_status(property, status)
    }

    fn payload_get_arc(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        self.get_arc(property)
    }

    fn payload_get_copy(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        self.get_copy(property)
    }

    fn payload_get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
        self.get_all_property_statuses()
    }
}

// Note: PayloadConstructor implementation removed due to type mismatch
// The trait expects Vec<Arc<String>> but CogneePayload is generic over TC
// This would need a redesign of the trait to be truly generic

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

    let chunks_arc: Arc<RwLock<Vec<Arc<String>>>> =
        *payload.get_arc("chunks").unwrap().downcast().unwrap();
    let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
        *payload.get_arc("result1").unwrap().downcast().unwrap();
    let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
        *payload.get_arc("result2").unwrap().downcast().unwrap();

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

    #[test]
    fn test_generic_get_arc() {
        let chunks = vec![
            Arc::new("chunk1".to_string()),
            Arc::new("chunk2".to_string()),
        ];
        let payload = CogneePayload::<String, String, String>::new(chunks);

        // Test generic Arc access
        let chunks_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("chunks").unwrap().downcast().unwrap();
        let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let property_status_arc: Arc<Mutex<HashMap<String, PropertyStatus>>> = *payload
            .get_arc("property_status")
            .unwrap()
            .downcast()
            .unwrap();

        // Verify we got the right types
        assert!(chunks_arc.read().unwrap().len() == 2);
        assert!(result1_arc.read().unwrap().is_empty());
        assert!(result2_arc.read().unwrap().is_empty());
        let status_len = property_status_arc.lock().unwrap().len();
        println!("Property status length: {status_len}");
        assert!(status_len >= 3); // chunks, result1, result2

        // Test error case
        let error_result = payload.get_arc("invalid_property");
        assert!(error_result.is_err());
        assert!(error_result.unwrap_err().contains("Unknown property"));
    }

    #[test]
    fn test_generic_get_copy() {
        let chunks = vec![
            Arc::new("chunk1".to_string()),
            Arc::new("chunk2".to_string()),
        ];
        let payload = CogneePayload::<String, String, String>::new(chunks);

        // Test generic copy access
        let chunks_copy: Vec<Arc<String>> =
            *payload.get_copy("chunks").unwrap().downcast().unwrap();
        let result1_copy: Vec<Arc<String>> =
            *payload.get_copy("result1").unwrap().downcast().unwrap();
        let result2_copy: Vec<Arc<String>> =
            *payload.get_copy("result2").unwrap().downcast().unwrap();

        // Verify we got the right types and data
        assert!(chunks_copy.len() == 2);
        assert!(result1_copy.is_empty());
        assert!(result2_copy.is_empty());
        assert_eq!(chunks_copy[0].as_str(), "chunk1");
        assert_eq!(chunks_copy[1].as_str(), "chunk2");

        // Test error case
        let error_result = payload.get_copy("invalid_property");
        assert!(error_result.is_err());
        assert!(error_result.unwrap_err().contains("Unknown property"));
    }
}
