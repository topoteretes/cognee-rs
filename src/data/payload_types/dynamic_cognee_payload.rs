// Dynamic CogneePayload generation using macros
// This allows you to create payloads with configurable result fields
// Generates EXACTLY the same thread-safe structure as original CogneePayload

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use super::super::payload_base::PayloadBase;
use super::cognee_payload::PropertyStatus;

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
    use crate::create_cognee_payload;

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
    async fn test_dynamic_cognee_payload() {
        println!("=== Dynamic CogneePayload Test (Thread-Safe) ===");

        // Create chunks (using Arc<String> like original)
        let chunks = vec![Arc::new("chunk1".to_string()), Arc::new("chunk2".to_string())];

        // Create payload using the macro
        let payload = TestPayload::new(chunks);

        // Test get_arc method (like original)
        assert!(payload.get_arc("chunks").is_ok(), "Should be able to get chunks arc");
        
        assert!(payload.get_arc("result1").is_ok(), "Should be able to get result1 arc");

        assert!(payload.get_arc("result2").is_ok(), "Should be able to get result2 arc");

        assert!(payload.get_arc("result3").is_ok(), "Should be able to get result3 arc");

        // Test get_copy method (like original)
        assert!(payload.get_copy("chunks").is_ok(), "Should be able to get chunks copy");

        assert!(payload.get_copy("result1").is_ok(), "Should be able to get result1 copy");

        // Test property status methods
        payload.set_property_status("result1", PropertyStatus::Processing);
        payload.set_property_status("result2", PropertyStatus::Done);
        payload.set_property_status("result3", PropertyStatus::Empty);
        
        if let Some(status) = payload.get_property_status("result1") {
            println!("Got result1 status: {:?}", status);
        }
        
        if let Some(status) = payload.get_property_status("result2") {
            println!("Got result2 status: {:?}", status);
        }

        if let Some(status) = payload.get_property_status("result3") {
            println!("Got result3 status: {:?}", status);
        }

        // Test base method
        let base_arc = payload.base();

        // Test ID method (returns Uuid like original)
        let payload_id = payload.id();

        // Assertions to verify functionality
        assert_eq!(payload.get_property_status("result1"), Some(PropertyStatus::Processing));
        assert_eq!(payload.get_property_status("result2"), Some(PropertyStatus::Done));
        assert_eq!(payload.get_property_status("result3"), Some(PropertyStatus::Empty));
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
        let chunks_original = vec![Arc::new("chunk1".to_string()), Arc::new("chunk2".to_string())];

        // Create original CogneePayload
        let original_payload = CogneePayload::<String, String, String>::new(chunks_original.clone());
        
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
        assert!(dynamic_base_type.is_ok(), "Dynamic base field should be Arc<RwLock<PayloadBase>>");

        // Verify chunks field type
        let original_chunks_type = original_chunks_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_chunks_type = dynamic_chunks_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(original_chunks_type.is_ok(), "Original chunks field should be Arc<RwLock<Vec<Arc<String>>>>");
        assert!(dynamic_chunks_type.is_ok(), "Dynamic chunks field should be Arc<RwLock<Vec<Arc<String>>>>");

        // Verify result1 field type
        let original_result1_type = original_result1_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_result1_type = dynamic_result1_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(original_result1_type.is_ok(), "Original result1 field should be Arc<RwLock<Vec<Arc<String>>>>");
        assert!(dynamic_result1_type.is_ok(), "Dynamic result1 field should be Arc<RwLock<Vec<Arc<String>>>>");

        // Verify result2 field type
        let original_result2_type = original_result2_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        let dynamic_result2_type = dynamic_result2_arc.downcast::<Arc<RwLock<Vec<Arc<String>>>>>();
        assert!(original_result2_type.is_ok(), "Original result2 field should be Arc<RwLock<Vec<Arc<String>>>>");
        assert!(dynamic_result2_type.is_ok(), "Dynamic result2 field should be Arc<RwLock<Vec<Arc<String>>>>");
        
        // Verify property_status field type
        let original_status_type = original_status_arc.downcast::<Arc<Mutex<HashMap<String, PropertyStatus>>>>();
        let dynamic_status_type = dynamic_status_arc.downcast::<Arc<Mutex<HashMap<String, PropertyStatus>>>>();
        assert!(original_status_type.is_ok(), "Original property_status field should be Arc<Mutex<HashMap<String, PropertyStatus>>>");
        assert!(dynamic_status_type.is_ok(), "Dynamic property_status field should be Arc<Mutex<HashMap<String, PropertyStatus>>>");

        
        // Test get_copy for chunks
        let original_chunks_copy = original_payload.get_copy("chunks").unwrap();
        let dynamic_chunks_copy = dynamic_payload.get_copy("chunks").unwrap();
        
        let original_chunks_copy_type = original_chunks_copy.downcast::<Vec<Arc<String>>>();
        let dynamic_chunks_copy_type = dynamic_chunks_copy.downcast::<Vec<Arc<String>>>();
        assert!(original_chunks_copy_type.is_ok(), "Original chunks copy should be Vec<Arc<String>>");
        assert!(dynamic_chunks_copy_type.is_ok(), "Dynamic chunks copy should be Vec<Arc<String>>");
        
        // Test get_copy for result1
        let original_result1_copy = original_payload.get_copy("result1").unwrap();
        let dynamic_result1_copy = dynamic_payload.get_copy("result1").unwrap();
        
        let original_result1_copy_type = original_result1_copy.downcast::<Vec<Arc<String>>>();
        let dynamic_result1_copy_type = dynamic_result1_copy.downcast::<Vec<Arc<String>>>();
        assert!(original_result1_copy_type.is_ok(), "Original result1 copy should be Vec<Arc<String>>");
        assert!(dynamic_result1_copy_type.is_ok(), "Dynamic result1 copy should be Vec<Arc<String>>");
        
        // Test ID method return type
        let original_id = original_payload.id();
        let dynamic_id = dynamic_payload.id();
        
        // Both should return Uuid - verify by checking they can be converted to string
        let original_id_str = original_id.to_string();
        let dynamic_id_str = dynamic_id.to_string();
        assert!(original_id_str.len() > 0, "Original ID should be a valid Uuid");
        assert!(dynamic_id_str.len() > 0, "Dynamic ID should be a valid Uuid");
        
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
            assert!(original_arc.get_arc(field).is_ok(), "Original should have field: {}", field);
            assert!(dynamic_arc.get_arc(field).is_ok(), "Dynamic should have field: {}", field);
        }
        
        // Test that dynamic has additional base field
        assert!(dynamic_arc.get_arc("base").is_ok(), "Dynamic should have base field");
    }
}
