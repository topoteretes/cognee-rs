use crate::data::payload_types::cognee_payload::PropertyStatus;
use uuid::Uuid;

pub trait PayloadTrait: Send + Sync + 'static {
    fn payload_id(&self) -> Uuid;
    fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus>;
    fn payload_set_property_status(&self, property: &str, status: PropertyStatus);
    fn payload_get_arc(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String>;
    fn payload_get_copy(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String>;
}

pub trait PayloadConstructor: PayloadTrait {
    fn new(chunks: Vec<std::sync::Arc<String>>) -> Self;
}
