use crate::data::payload_base::PayloadBase;
use crate::data::traits::PayloadBehavior;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
//ContinueHere
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowLevelPayload {
    base: PayloadBase<Vec<String>>,
    pub width: u32,
    pub height: u32,
}

impl LowLevelPayload {
    pub fn new(task_counter: u32, width: u32, height: u32, chunks: Vec<String>) -> Self {
        Self {
            base: PayloadBase::new(task_counter, chunks),
            width,
            height,
        }
    }

    pub fn aspect_ratio(&self) -> f32 {
        self.width as f32 / self.height as f32
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
