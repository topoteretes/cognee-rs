use crate::data::payload_base::PayloadBase;
use crate::data::traits::PayloadBehavior;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CogneePayload {
    base: PayloadBase<Vec<String>>,
    pub title: String,
}

impl CogneePayload {
    pub fn new(task_counter: u32, title: impl Into<String>, chunks: Vec<String>) -> Self {
        Self {
            base: PayloadBase::new(task_counter, chunks),
            title: title.into(),
        }
    }

    pub fn word_count(&self) -> usize {
        self.base
            .data
            .iter()
            .map(|c| c.split_whitespace().count())
            .sum()
    }
}

impl PayloadBehavior for CogneePayload {
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
