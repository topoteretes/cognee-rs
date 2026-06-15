//! Fact extractor using LLM for knowledge graph extraction.
//!
//! Port of Python's cognee/infrastructure/llm/extraction/knowledge_graph/extract_content_graph.py
//! and cognee/tasks/graph/extract_graph_from_data.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use tracing::debug;

use super::models::{GraphModel, KnowledgeGraph};
use crate::error::CognifyError;

/// Default system prompt for knowledge graph extraction.
///
/// Vendored byte-for-byte from Python's
/// `cognee/infrastructure/llm/prompts/generate_graph_prompt.txt` (kept in sync via
/// the prompt-parity drift guard in the inline `#[cfg(test)]` block below).
const DEFAULT_GRAPH_PROMPT: &str = include_str!("prompts/generate_graph_prompt.txt");

/// Fact extractor for knowledge graph generation.
///
/// Uses an LLM (via the Llm trait) to extract structured facts from text.
/// Produces a KnowledgeGraph containing nodes (entities) and edges (relationships).
///
/// # Example
/// ```ignore
/// use cognee_cognify::FactExtractor;
/// use cognee_llm::OpenAIAdapter;
/// use std::sync::Arc;
///
/// let llm = Arc::new(OpenAIAdapter::new("gpt-4", "sk-...", None)?);
/// let extractor = FactExtractor::new(llm);
///
/// let text = "Alice works at TechCorp in San Francisco.";
/// let graph = extractor.extract_facts(text, None).await?;
///
/// println!("Extracted {} nodes and {} edges", graph.node_count(), graph.edge_count());
/// ```
#[derive(Clone)]
pub struct FactExtractor {
    llm: Arc<dyn Llm>,
}

impl FactExtractor {
    /// Create a new fact extractor with the given LLM.
    ///
    /// # Arguments
    /// * `llm` - An LLM implementation (e.g., OpenAIAdapter, OllamaAdapter)
    ///
    /// # Returns
    /// A new FactExtractor instance
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }

    /// Return the default graph extraction prompt used by `extract_facts`.
    pub fn default_graph_prompt() -> &'static str {
        DEFAULT_GRAPH_PROMPT
    }

    /// Extract a structured model from text via LLM.
    ///
    /// Generic counterpart of [`extract_facts`](Self::extract_facts).
    /// Works with any type implementing [`GraphModel`], which requires
    /// `Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync`.
    ///
    /// The LLM's [`create_structured_output`](cognee_llm::LlmExt::create_structured_output)
    /// method infers the JSON schema from `M` and deserializes the response
    /// into the concrete type.
    ///
    /// No post-processing is applied; for the default [`KnowledgeGraph`] flow
    /// with name-fallback logic, use [`extract_facts`](Self::extract_facts).
    ///
    /// # Arguments
    /// * `text` - Input text to extract from
    /// * `custom_prompt` - Optional custom system prompt (uses [`DEFAULT_GRAPH_PROMPT`] if None)
    ///
    /// # Errors
    /// Returns [`CognifyError::LlmError`] if the LLM call fails
    pub async fn extract<M: GraphModel>(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<M, CognifyError> {
        debug!("Extracting model {} from text", std::any::type_name::<M>());
        let system_prompt = custom_prompt.unwrap_or(DEFAULT_GRAPH_PROMPT);

        let result: M = self
            .llm
            .create_structured_output(
                text,
                system_prompt,
                Some(GenerationOptions {
                    temperature: Some(0.1),
                    max_tokens: Some(2000),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| CognifyError::LlmError(e.to_string()))?;

        debug!("Extracted model {}", std::any::type_name::<M>());
        Ok(result)
    }

    /// Extract facts (knowledge graph) from text.
    ///
    /// Mirrors Python's `extract_content_graph` function.
    /// Uses the LLM to extract structured Node and Edge objects from the input text.
    ///
    /// # Arguments
    /// * `text` - Input text to extract facts from
    /// * `custom_prompt` - Optional custom system prompt (uses DEFAULT_GRAPH_PROMPT if None)
    ///
    /// # Returns
    /// A KnowledgeGraph containing extracted nodes and edges
    ///
    /// # Errors
    /// Returns CognifyError::LlmError if the LLM call fails
    pub async fn extract_facts(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<KnowledgeGraph, CognifyError> {
        debug!("Extracting facts from text: {}", text);

        let mut graph: KnowledgeGraph = self.extract(text, custom_prompt).await?;

        debug!(
            "Extracted graph with {} nodes and {} edges",
            graph.node_count(),
            graph.edge_count()
        );

        // Post-processing: ensure every node has a non-empty name (Python compat).
        // In Python's Node.__init__, name defaults to id when empty.
        for node in &mut graph.nodes {
            if node.name.is_empty() {
                node.name = node.id.clone();
            }
        }

        Ok(graph)
    }

    /// Extract facts from multiple text chunks in parallel.
    ///
    /// Mirrors Python's pattern in extract_graph_from_data.py where all chunks
    /// are processed concurrently using asyncio.gather.
    ///
    /// # Arguments
    /// * `texts` - Slice of text strings to extract facts from
    /// * `custom_prompt` - Optional custom system prompt
    ///
    /// # Returns
    /// A vector of KnowledgeGraphs, one per input text
    ///
    /// # Errors
    /// Returns CognifyError::LlmError if any LLM call fails
    pub async fn extract_facts_batch(
        &self,
        texts: Vec<String>, // Changed to owned Vec<String> to avoid lifetime issues
        custom_prompt: Option<String>, // Changed to owned String
    ) -> Result<Vec<KnowledgeGraph>, CognifyError> {
        let mut tasks = Vec::new();

        for text in texts {
            let llm_clone = Arc::clone(&self.llm);
            let prompt_clone = custom_prompt.clone();

            let task = tokio::spawn(async move {
                let extractor = FactExtractor { llm: llm_clone };
                extractor
                    .extract_facts(&text, prompt_clone.as_deref())
                    .await
            });

            tasks.push(task);
        }

        let results = futures::future::join_all(tasks).await;

        let mut graphs = Vec::new();
        for result in results {
            let graph =
                result.map_err(|e| CognifyError::LlmError(format!("Task join error: {}", e)))??;
            graphs.push(graph);
        }

        Ok(graphs)
    }

    /// Get a reference to the underlying LLM.
    pub fn llm(&self) -> &Arc<dyn Llm> {
        &self.llm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock LLM for testing
    #[derive(Clone)]
    struct MockLlm;

    #[async_trait::async_trait]
    impl Llm for MockLlm {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let graph = KnowledgeGraph {
                nodes: vec![super::super::models::Node {
                    id: "test_node".to_string(),
                    name: "Test Node".to_string(),
                    node_type: "TEST".to_string(),
                    description: "A test node".to_string(),
                }],
                edges: vec![],
            };
            Ok(serde_json::to_value(&graph).unwrap())
        }

        fn model(&self) -> &str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_fact_extractor_creation() {
        let llm = Arc::new(MockLlm);
        let extractor = FactExtractor::new(llm);
        assert_eq!(extractor.llm().model(), "mock");
    }

    #[tokio::test]
    async fn test_extract_facts() {
        let llm = Arc::new(MockLlm);
        let extractor = FactExtractor::new(llm);

        let result = extractor.extract_facts("Test text", None).await;
        assert!(result.is_ok());

        let graph = result.unwrap();
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.nodes[0].id, "test_node");
    }

    /// Mock LLM that returns a node with an empty name to test the fallback.
    #[derive(Clone)]
    struct MockLlmEmptyName;

    #[async_trait::async_trait]
    impl Llm for MockLlmEmptyName {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let graph = KnowledgeGraph {
                nodes: vec![
                    super::super::models::Node {
                        id: "alice_johnson".to_string(),
                        name: "".to_string(), // Empty name — should be set to id
                        node_type: "PERSON".to_string(),
                        description: "A person".to_string(),
                    },
                    super::super::models::Node {
                        id: "techcorp".to_string(),
                        name: "TechCorp".to_string(), // Non-empty — should stay unchanged
                        node_type: "ORGANIZATION".to_string(),
                        description: "A company".to_string(),
                    },
                ],
                edges: vec![],
            };
            Ok(serde_json::to_value(&graph).unwrap())
        }

        fn model(&self) -> &str {
            "mock-empty-name"
        }
    }

    #[tokio::test]
    async fn test_empty_node_name_defaults_to_id() {
        let llm = Arc::new(MockLlmEmptyName);
        let extractor = FactExtractor::new(llm);

        let graph = extractor.extract_facts("Test text", None).await.unwrap();

        assert_eq!(graph.node_count(), 2);

        // Node with empty name should have name set to its id
        assert_eq!(graph.nodes[0].id, "alice_johnson");
        assert_eq!(graph.nodes[0].name, "alice_johnson");

        // Node with non-empty name should remain unchanged
        assert_eq!(graph.nodes[1].id, "techcorp");
        assert_eq!(graph.nodes[1].name, "TechCorp");
    }

    // ── Tests for the generic extract<M> method ────────────────────────

    /// A custom graph model used to verify generic extraction.
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct CustomEvent {
        event_name: String,
        participants: Vec<String>,
    }

    impl super::super::models::GraphModel for CustomEvent {}

    /// Mock LLM that returns a `CustomEvent` JSON payload.
    #[derive(Clone)]
    struct MockLlmCustom;

    #[async_trait::async_trait]
    impl Llm for MockLlmCustom {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let event = CustomEvent {
                event_name: "Conference".to_string(),
                participants: vec!["Alice".to_string(), "Bob".to_string()],
            };
            Ok(serde_json::to_value(&event).unwrap())
        }

        fn model(&self) -> &str {
            "mock-custom"
        }
    }

    #[tokio::test]
    async fn test_extract_generic_custom_model() {
        let llm = Arc::new(MockLlmCustom);
        let extractor = FactExtractor::new(llm);

        let event: CustomEvent = extractor.extract("Test text", None).await.unwrap();
        assert_eq!(event.event_name, "Conference");
        assert_eq!(event.participants, vec!["Alice", "Bob"]);
    }

    #[tokio::test]
    async fn test_extract_generic_knowledge_graph() {
        // Verify that extract::<KnowledgeGraph> works (without post-processing)
        let llm = Arc::new(MockLlmEmptyName);
        let extractor = FactExtractor::new(llm);

        let graph: KnowledgeGraph = extractor.extract("Test text", None).await.unwrap();
        // No post-processing: empty name stays empty (unlike extract_facts)
        assert_eq!(graph.nodes[0].name, "");
    }

    #[tokio::test]
    async fn test_extract_facts_delegates_to_extract() {
        // Verify extract_facts still applies post-processing on top of extract
        let llm = Arc::new(MockLlm);
        let extractor = FactExtractor::new(llm);

        let via_extract: KnowledgeGraph = extractor.extract("Test text", None).await.unwrap();
        let via_facts = extractor.extract_facts("Test text", None).await.unwrap();

        // Both should get the same node
        assert_eq!(via_extract.node_count(), via_facts.node_count());
        assert_eq!(via_extract.nodes[0].id, via_facts.nodes[0].id);
    }

    #[test]
    fn graph_prompt_matches_vendored_txt() {
        // Drift guard: const must equal the vendored .txt byte-for-byte.
        // Manual re-sync: cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt \
        //   crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt
        let vendored = include_str!("prompts/generate_graph_prompt.txt");
        assert_eq!(
            DEFAULT_GRAPH_PROMPT, vendored,
            "const drifted from vendored .txt"
        );
        // Python markers the old Rust prompt did NOT have:
        assert!(
            vendored.contains("Every edge should include a description"),
            "edge-description paragraph missing — not the Python prompt"
        );
        assert!(
            vendored.contains(r#"label it as **"Person"**"#),
            "Title-case 'Person' missing — UPPERCASE Rust prompt regressed"
        );
        assert!(
            !vendored.contains("the entity type label in uppercase"),
            "old UPPERCASE-forcing line still present"
        );
    }
}
