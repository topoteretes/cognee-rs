// Implemented in phase 4.
use std::sync::Arc;

use cognee_llm::Llm;

#[allow(dead_code)]
const TEMPORAL_ENTITY_ENRICHMENT_PROMPT: &str =
    include_str!("prompts/temporal_entity_enrichment.txt");

#[allow(dead_code)]
pub struct TemporalEntityEnricher {
    pub(crate) llm: Arc<dyn Llm>,
}

impl TemporalEntityEnricher {
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }
}
