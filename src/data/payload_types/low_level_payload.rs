use crate::data::payload_base::PayloadBase;
use crate::data::traits::PayloadBehavior;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
//Example Low level payload with strictly defined properties following the PayloadBehaviour
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowLevelPayload {
    base: PayloadBase<Vec<String>>,
    pub property1: u32,
    pub property2: u32,
}

impl LowLevelPayload {
    pub fn new(task_counter: u32, property1: u32, property2: u32, chunks: Vec<String>) -> Self {
        Self {
            base: PayloadBase::new(task_counter, chunks),
            property1,
            property2,
        }
    }

    pub fn properties(&self) -> f32 {
        self.property1 as f32 / self.property2 as f32
    }
}

impl PayloadBehavior for LowLevelPayload {
    fn id(&self) -> Uuid {
        self.base.metainfo.id
    }
    fn task_counter(&self) -> u32 {
        self.base.metainfo.task_counter
    }
    fn task_done(&mut self) {
        self.base.metainfo.task_done();
    }
    fn chunks(&self) -> &[String] {
        &self.base.data
    }
}
