use crate::data::payload_base::PayloadBase;
use crate::data::traits::PayloadBehavior;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CogneePayload {
    base: PayloadBase,
    pub chunks: Vec<String>,
}

impl CogneePayload {
    pub fn new(chunks: Vec<String>) -> Self {
        Self {
            base: PayloadBase::new(),
            chunks,
        }
    }


}

impl PayloadBehavior for CogneePayload {
    fn id(&self) -> Uuid {
        self.base.metainfo.id
    }
    fn task_done(&mut self) {
        self.base.metainfo.task_done();
    }
}
