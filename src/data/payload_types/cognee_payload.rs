use crate::data::payload_base::PayloadBase;
use crate::data::payloadbehavior::PayloadBehavior;
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Mutex};
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
        status.insert("chunks".to_string(), if chunks.is_empty() { PropertyStatus::Empty } else { PropertyStatus::Done });
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
    pub fn add_chunk(&self, item: Arc<TC>) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.push(item);
    }

    pub fn add_chunks_batch(&self, items: Vec<Arc<TC>>) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.extend(items);
    }

    pub fn get_chunks_copy(&self) -> Vec<Arc<TC>> {
        let chunks = self.chunks.read().unwrap();
        chunks.clone()
    }

    pub fn chunks_len(&self) -> usize {
        let chunks = self.chunks.read().unwrap();
        chunks.len()
    }

    pub fn clear_chunks(&self) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.clear();
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

    pub fn add_result1(&self, item: Arc<T1>) {
        let mut result1 = self.result1.write().unwrap();
        result1.push(item);
    }

    pub fn add_result1_batch(&self, items: Vec<Arc<T1>>) {
        let mut result1 = self.result1.write().unwrap();
        result1.extend(items);
    }
    pub fn get_result1_copy(&self) -> Vec<Arc<T1>> {
        let result1 = self.result1.read().unwrap();
        result1.clone()
    }

    pub fn result1_len(&self) -> usize {
        let result1 = self.result1.read().unwrap();
        result1.len()
    }

    pub fn clear_result1(&self) {
        let mut result1 = self.result1.write().unwrap();
        result1.clear();
    }

    pub fn add_result2(&self, item: Arc<T2>) {
        let mut result2 = self.result2.write().unwrap();
        result2.push(item);
    }

    pub fn add_result2_batch(&self, items: Vec<Arc<T2>>) {
        let mut result2 = self.result2.write().unwrap();
        result2.extend(items);
    }

    pub fn get_result2_copy(&self) -> Vec<Arc<T2>> {
        let result2 = self.result2.read().unwrap();
        result2.clone()
    }

    pub fn result2_len(&self) -> usize {
        let result2 = self.result2.read().unwrap();
        result2.len()
    }

    pub fn clear_result2(&self) {
        let mut result2 = self.result2.write().unwrap();
        result2.clear();
    }

    pub fn read_base<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&PayloadBase) -> R,
    {
        let base = self.base.read().unwrap();
        f(&base)
    }

    pub fn write_base<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut PayloadBase) -> R,
    {
        let mut base = self.base.write().unwrap();
        f(&mut base)
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

impl<TC, T1, T2> PayloadBehavior for CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    fn id(&self) -> Uuid {
        self.read_base(|base| base.metainfo.id)
    }

    fn task_done(&mut self) {
        self.write_base(|base| base.metainfo.task_done());
    }
}

#[tokio::test]
async fn parallel_readers_no_copy() {
    use std::time::Duration;
    let initial_chunks: Vec<Arc<String>> = (0..1023)
        .map(|i| {
            let content = match i % 5 {
                0 => format!("document_text_{:04}_analysis_ready", i),
                1 => format!("embedding_vector_{:04}_processed", i),
                2 => format!("memory_fragment_{:04}_indexed", i),
                3 => format!("knowledge_node_{:04}_connected", i),
                _ => format!("data_segment_{:04}_transformed", i),
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
        println!(
            "Task 1 starting - moving {} chunks to result1...",
            total_chunks
        );

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
        println!(
            "Task 2 starting - moving {} chunks to result2...",
            total_chunks
        );

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
        assert_eq!(payload.get_property_status("base"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Empty));
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Empty));
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Empty));
    }

    #[test]
    fn test_initial_status_with_chunks() {
        let chunks = vec![Arc::new("test chunk".to_string())];
        let payload = CogneePayload::<String, String, String>::new(chunks);
        
        // Check initial statuses
        assert_eq!(payload.get_property_status("base"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Empty));
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Empty));
    }

    #[test]
    fn test_get_all_property_statuses() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);
        let all_statuses = payload.get_all_property_statuses();
        
        assert_eq!(all_statuses.len(), 4);
        assert!(all_statuses.contains_key("base"));
        assert!(all_statuses.contains_key("chunks"));
        assert!(all_statuses.contains_key("result1"));
        assert!(all_statuses.contains_key("result2"));
    }

    #[test]
    fn test_set_and_get_property_status() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);
        
        // Set processing status
        payload.set_property_status("chunks", PropertyStatus::Processing);
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Processing));
        
        // Set error status
        payload.set_property_status("result1", PropertyStatus::Errored("test error".to_string()));
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Errored("test error".to_string())));
        
        // Set done status
        payload.set_property_status("result2", PropertyStatus::Done);
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Done));
    }

    #[test]
    fn test_manual_status_management() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);
        
        // Status should NOT automatically update when adding data
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Empty));
        
        // Add chunk - status should remain Empty (no automatic updates)
        payload.add_chunk(Arc::new("test".to_string()));
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Empty));
        
        // Manually update status
        payload.set_property_status("chunks", PropertyStatus::Done);
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Done));
        
        // Clear chunks - status should remain Done (no automatic updates)
        payload.clear_chunks();
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Done));
        
        // Manually update status back to Empty
        payload.set_property_status("chunks", PropertyStatus::Empty);
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Empty));
    }

    #[test]
    fn test_property_status_with_different_types() {
        // Test with different generic types
        let payload = CogneePayload::<i32, f64, bool>::new(vec![Arc::new(42)]);
        
        assert_eq!(payload.get_property_status("base"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("chunks"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Empty));
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Empty));
        
        // Manually set statuses
        payload.set_property_status("result1", PropertyStatus::Processing);
        payload.add_result1(Arc::new(3.14));
        payload.set_property_status("result1", PropertyStatus::Done);
        
        payload.add_result2(Arc::new(true));
        payload.set_property_status("result2", PropertyStatus::Done);
        
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Done));
    }

    #[test]
    fn test_any_property_name_allowed() {
        let payload = CogneePayload::<String, String, String>::new(vec![]);
        
        // Can set status for any property name (no validation)
        payload.set_property_status("custom_property", PropertyStatus::Processing);
        assert_eq!(payload.get_property_status("custom_property"), Some(PropertyStatus::Processing));
        
        payload.set_property_status("another_prop", PropertyStatus::Errored("custom error".to_string()));
        assert_eq!(payload.get_property_status("another_prop"), Some(PropertyStatus::Errored("custom error".to_string())));
    }
}
