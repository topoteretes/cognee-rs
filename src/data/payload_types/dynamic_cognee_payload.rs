// Unused import is neede here to pass clippy because of metaprogramming (Maybe there is a better way to do this?)

#[allow(unused_imports)]
use super::super::payload_base::PayloadBase;
#[allow(unused_imports)]
use super::cognee_payload::PropertyStatus;
#[allow(unused_imports)]
use std::collections::HashMap;
#[allow(unused_imports)]
use std::sync::{Arc, Mutex, RwLock};

#[macro_export]
macro_rules! create_cognee_payload {
    ($name:ident, $($result_name:ident: $result_type:ty),*) => {
        #[derive(Debug, Clone)]
        pub struct $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            base: Arc<RwLock<PayloadBase>>,
            chunks: Arc<RwLock<Vec<Arc<String>>>>,
            $(
                $result_name: Arc<RwLock<Vec<Arc<$result_type>>>>,
            )*
            property_status: Arc<Mutex<HashMap<String, PropertyStatus>>>,
        }

        // Implement the PayloadTrait for the generated payload type
        impl $crate::data::payload_trait::PayloadTrait for $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            fn payload_id(&self) -> uuid::Uuid {
                self.id()
            }

            fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus> {
                self.get_property_status(property)
            }

            fn payload_set_property_status(&self, property: &str, status: PropertyStatus) {
                self.set_property_status(property, status)
            }

            fn payload_get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                self.get_arc(property)
            }

            fn payload_get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                self.get_copy(property)
            }

            fn payload_get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
                self.get_all_property_statuses()
            }
        }

        // Implement the PayloadConstructor trait for the generated payload type
        impl $crate::data::payload_trait::PayloadConstructor for $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            fn new(chunks: Vec<std::sync::Arc<String>>) -> Self {
                Self::new(chunks)
            }
        }

        #[allow(dead_code)]
        impl $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            pub fn new(chunks: Vec<Arc<String>>) -> Self {
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
                $(
                    status.insert(stringify!($result_name).to_string(), PropertyStatus::Empty);
                )*

                Self {
                    base: Arc::new(RwLock::new(PayloadBase::new())),
                    chunks: Arc::new(RwLock::new(chunks)),
                    $(
                        $result_name: Arc::new(RwLock::new(Vec::new())),
                    )*
                    property_status: Arc::new(Mutex::new(status)),
                }
            }

            // Generic getter method (like get_arc in original)
            pub fn get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                match property {
                    "base" => Ok(Box::new(Arc::clone(&self.base))),
                    "chunks" => Ok(Box::new(Arc::clone(&self.chunks))),
                    $(
                        stringify!($result_name) => Ok(Box::new(Arc::clone(&self.$result_name))),
                    )*
                    "property_status" => Ok(Box::new(Arc::clone(&self.property_status))),
                    _ => Err(format!("Unknown property: {property}")),
                }
            }

            // Generic copy method (like get_copy in original)
            pub fn get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                match property {
                    "chunks" => {
                        let chunks = self.chunks.read().unwrap();
                        Ok(Box::new(chunks.clone()))
                    }
                    $(
                        stringify!($result_name) => {
                            let result = self.$result_name.read().unwrap();
                            Ok(Box::new(result.clone()))
                        }
                    )*
                    _ => Err(format!("Unknown property: {property}")),
                }
            }

            // Property status methods (exactly like in original CogneePayload)
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

            // Base method (like in original CogneePayload)
            pub fn base(&self) -> Arc<RwLock<PayloadBase>> {
                Arc::clone(&self.base)
            }

            // ID method (like in original CogneePayload) - returns Uuid
            pub fn id(&self) -> uuid::Uuid {
                let base = self.base.read().unwrap();
                base.metainfo.id
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // Create a custom payload type with 3 result fields for testing
    create_cognee_payload!(
        TestPayload,
        result1: String,
        result2: String,
        result3: u64
    );

    create_cognee_payload!(
        CogneePayload2,
        result1: String,
        result2: String
    );

    #[tokio::test]
    async fn parallel_readers_no_copy_with_dynamic_cognee_payload() {
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

        let payload = Arc::new(CogneePayload2::new(initial_chunks));

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

    #[test]
    fn test_initial_status_with_empty_chunks_with_dynamic_cognee_payload() {
        let payload = CogneePayload2::new(vec![]);

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
    fn test_initial_status_with_chunks_with_dynamic_cognee_payload() {
        let chunks = vec![Arc::new("test chunk".to_string())];
        let payload = CogneePayload2::new(chunks);

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
        let payload = CogneePayload2::new(vec![]);

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
    fn test_generic_get_arc() {
        let chunks = vec![
            Arc::new("chunk1".to_string()),
            Arc::new("chunk2".to_string()),
        ];
        let payload = CogneePayload2::new(chunks);

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

    #[tokio::test]
    async fn test_dynamic_cognee_payload() {
        println!("=== Dynamic CogneePayload Test (Thread-Safe) ===");

        // Create chunks (using Arc<String> like original)
        let chunks = vec![
            Arc::new("chunk1".to_string()),
            Arc::new("chunk2".to_string()),
        ];

        // Create payload using the macro
        let payload = TestPayload::new(chunks);

        // Test get_arc method (like original)
        assert!(
            payload.get_arc("chunks").is_ok(),
            "Should be able to get chunks arc"
        );

        assert!(
            payload.get_arc("result1").is_ok(),
            "Should be able to get result1 arc"
        );

        assert!(
            payload.get_arc("result2").is_ok(),
            "Should be able to get result2 arc"
        );

        assert!(
            payload.get_arc("result3").is_ok(),
            "Should be able to get result3 arc"
        );

        // Test get_copy method (like original)
        assert!(
            payload.get_copy("chunks").is_ok(),
            "Should be able to get chunks copy"
        );

        assert!(
            payload.get_copy("result1").is_ok(),
            "Should be able to get result1 copy"
        );

        // Test property status methods
        payload.set_property_status("result1", PropertyStatus::Processing);
        payload.set_property_status("result2", PropertyStatus::Done);
        payload.set_property_status("result3", PropertyStatus::Empty);

        if let Some(status) = payload.get_property_status("result1") {
            println!("Got result1 status: {status:?}");
        }

        if let Some(status) = payload.get_property_status("result2") {
            println!("Got result2 status: {status:?}");
        }

        if let Some(status) = payload.get_property_status("result3") {
            println!("Got result3 status: {status:?}");
        }

        // Test base method
        let _base_arc = payload.base();

        // Test ID method (returns Uuid like original)
        let _payload_id = payload.id();

        // Assertions to verify functionality
        assert_eq!(
            payload.get_property_status("result1"),
            Some(PropertyStatus::Processing)
        );
        assert_eq!(
            payload.get_property_status("result2"),
            Some(PropertyStatus::Done)
        );
        assert_eq!(
            payload.get_property_status("result3"),
            Some(PropertyStatus::Empty)
        );
        assert!(payload.get_arc("chunks").is_ok());
        assert!(payload.get_arc("result1").is_ok());
        assert!(payload.get_arc("result2").is_ok());
        assert!(payload.get_arc("result3").is_ok());
        assert!(payload.get_copy("chunks").is_ok());
        assert!(payload.get_copy("result1").is_ok());
    }

    #[tokio::test]
    async fn test_comparison_with_original_cognee_payload() {
        use super::super::cognee_payload::CogneePayload;
        use std::sync::{Arc, RwLock};

        // Create chunks for both payloads
        let chunks_original = vec![
            Arc::new("chunk1".to_string()),
            Arc::new("chunk2".to_string()),
        ];

        // Create original CogneePayload
        let original_payload =
            CogneePayload::<String, String, String>::new(chunks_original.clone());

        // Create dynamic payload using macro
        let dynamic_payload = CogneePayload2::new(chunks_original.clone());

        // Test 1: Compare field types using get_arc

        // Original payload field types (only properties that both support)
        let original_chunks_arc = original_payload.get_arc("chunks").unwrap();
        let original_result1_arc = original_payload.get_arc("result1").unwrap();
        let original_result2_arc = original_payload.get_arc("result2").unwrap();
        let original_status_arc = original_payload.get_arc("property_status").unwrap();

        // Dynamic payload field types
        let dynamic_chunks_arc = dynamic_payload.get_arc("chunks").unwrap();
        let dynamic_result1_arc = dynamic_payload.get_arc("result1").unwrap();
        let dynamic_result2_arc = dynamic_payload.get_arc("result2").unwrap();
        let dynamic_status_arc = dynamic_payload.get_arc("property_status").unwrap();

        // Test that dynamic also supports base (extra feature)
        let dynamic_base_arc = dynamic_payload.get_arc("base").unwrap();
        let dynamic_base_type = dynamic_base_arc.downcast::<Arc<RwLock<PayloadBase>>>();
        assert!(
            dynamic_base_type.is_ok(),
            "Dynamic base field should be Arc<RwLock<PayloadBase>>"
        );

        // Verify chunks field type
        let original_chunks_type = original_chunks_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_chunks_type = dynamic_chunks_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(
            original_chunks_type.is_ok(),
            "Original chunks field should be Arc<RwLock<Vec<Arc<String>>>>"
        );
        assert!(
            dynamic_chunks_type.is_ok(),
            "Dynamic chunks field should be Arc<RwLock<Vec<Arc<String>>>>"
        );

        // Verify result1 field type
        let original_result1_type =
            original_result1_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_result1_type = dynamic_result1_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(
            original_result1_type.is_ok(),
            "Original result1 field should be Arc<RwLock<Vec<Arc<String>>>>"
        );
        assert!(
            dynamic_result1_type.is_ok(),
            "Dynamic result1 field should be Arc<RwLock<Vec<Arc<String>>>>"
        );

        // Verify result2 field type
        let original_result2_type =
            original_result2_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_result2_type = dynamic_result2_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(
            original_result2_type.is_ok(),
            "Original result2 field should be Arc<RwLock<Vec<Arc<String>>>>"
        );
        assert!(
            dynamic_result2_type.is_ok(),
            "Dynamic result2 field should be Arc<RwLock<Vec<Arc<String>>>>"
        );

        // Verify property_status field type
        let original_status_type =
            original_status_arc.downcast::<Arc<Mutex<HashMap<String, PropertyStatus>>>>();
        let dynamic_status_type =
            dynamic_status_arc.downcast::<Arc<Mutex<HashMap<String, PropertyStatus>>>>();
        assert!(
            original_status_type.is_ok(),
            "Original property_status field should be Arc<Mutex<HashMap<String, PropertyStatus>>>"
        );
        assert!(
            dynamic_status_type.is_ok(),
            "Dynamic property_status field should be Arc<Mutex<HashMap<String, PropertyStatus>>>"
        );

        // Test get_copy for chunks
        let original_chunks_copy = original_payload.get_copy("chunks").unwrap();
        let dynamic_chunks_copy = dynamic_payload.get_copy("chunks").unwrap();

        let original_chunks_copy_type = original_chunks_copy.downcast::<Vec<Arc<String>>>();
        let dynamic_chunks_copy_type = dynamic_chunks_copy.downcast::<Vec<Arc<String>>>();
        assert!(
            original_chunks_copy_type.is_ok(),
            "Original chunks copy should be Vec<Arc<String>>"
        );
        assert!(
            dynamic_chunks_copy_type.is_ok(),
            "Dynamic chunks copy should be Vec<Arc<String>>"
        );

        // Test get_copy for result1
        let original_result1_copy = original_payload.get_copy("result1").unwrap();
        let dynamic_result1_copy = dynamic_payload.get_copy("result1").unwrap();

        let original_result1_copy_type = original_result1_copy.downcast::<Vec<Arc<String>>>();
        let dynamic_result1_copy_type = dynamic_result1_copy.downcast::<Vec<Arc<String>>>();
        assert!(
            original_result1_copy_type.is_ok(),
            "Original result1 copy should be Vec<Arc<String>>"
        );
        assert!(
            dynamic_result1_copy_type.is_ok(),
            "Dynamic result1 copy should be Vec<Arc<String>>"
        );

        // Test ID method return type
        let original_id = original_payload.id();
        let dynamic_id = dynamic_payload.id();

        // Both should return Uuid - verify by checking they can be converted to string
        let original_id_str = original_id.to_string();
        let dynamic_id_str = dynamic_id.to_string();
        assert!(
            !original_id_str.is_empty(),
            "Original ID should be a valid Uuid"
        );
        assert!(
            !dynamic_id_str.is_empty(),
            "Dynamic ID should be a valid Uuid"
        );

        // Test base method return type (only dynamic has this method)
        let dynamic_base = dynamic_payload.base();

        // Verify the dynamic base method returns the correct type by using it
        let base_arc = dynamic_base;
        let base_read = base_arc.read().unwrap();
        let _base_id = base_read.metainfo.id;

        // Test set_property_status (both should take &self, not &mut self)
        original_payload.set_property_status("result1", PropertyStatus::Processing);
        dynamic_payload.set_property_status("result1", PropertyStatus::Processing);

        // Test get_property_status return type
        let original_status = original_payload.get_property_status("result1");
        let dynamic_status = dynamic_payload.get_property_status("result1");

        assert_eq!(original_status, dynamic_status);
        assert!(original_status.is_some());
        assert_eq!(original_status.unwrap(), PropertyStatus::Processing);

        // Test that both can be wrapped in Arc for thread sharing
        let original_arc = Arc::new(original_payload);
        let dynamic_arc = Arc::new(dynamic_payload);

        // Clone the Arc to simulate thread sharing
        let original_clone = Arc::clone(&original_arc);
        let dynamic_clone = Arc::clone(&dynamic_arc);

        // Both should implement Send + Sync
        fn assert_send_sync<T: Send + Sync>(_t: T) {}
        assert_send_sync(original_clone);
        assert_send_sync(dynamic_clone);

        // Test that both have the same core field names (excluding base which is dynamic-only)
        let core_fields = ["chunks", "result1", "result2", "property_status"];

        for field in &core_fields {
            assert!(
                original_arc.get_arc(field).is_ok(),
                "Original should have field: {field}"
            );
            assert!(
                dynamic_arc.get_arc(field).is_ok(),
                "Dynamic should have field: {field}"
            );
        }

        // Test that dynamic has additional base field
        assert!(
            dynamic_arc.get_arc("base").is_ok(),
            "Dynamic should have base field"
        );
    }
}
