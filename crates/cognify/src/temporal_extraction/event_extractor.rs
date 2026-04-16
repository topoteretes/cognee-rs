// Implemented in phase 3.
use std::sync::Arc;

use cognee_llm::Llm;

#[allow(dead_code)]
const TEMPORAL_EVENT_EXTRACTION_PROMPT: &str =
    include_str!("prompts/temporal_event_extraction.txt");

#[allow(dead_code)]
pub struct TemporalEventExtractor {
    pub(crate) llm: Arc<dyn Llm>,
}

impl TemporalEventExtractor {
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }
}
