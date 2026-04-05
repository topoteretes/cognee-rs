//! Fact extractor using LLM for knowledge graph extraction.
//!
//! Port of Python's cognee/infrastructure/llm/extraction/knowledge_graph/extract_content_graph.py
//! and cognee/tasks/graph/extract_graph_from_data.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use tracing::debug;

use super::models::KnowledgeGraph;
use crate::error::CognifyError;

/// Default system prompt for knowledge graph extraction.
///
/// Based on Python's generate_graph_prompt.txt.
/// Instructs the LLM to extract nodes (entities/concepts) and edges (relationships).
const DEFAULT_GRAPH_PROMPT: &str = r#"You are a top-tier algorithm designed for extracting information in structured formats to build a knowledge graph.
**Nodes** represent entities and concepts. They're akin to Wikipedia nodes.
**Edges** represent relationships between concepts. They're akin to Wikipedia links.

The aim is to achieve simplicity and clarity in the knowledge graph.

# 1. Node Fields
Each node must have exactly these fields:
  - **id**: the human-readable entity name as found in the text (e.g., "Alice Johnson", "TechCorp", "San Francisco")
  - **name**: the same human-readable entity name as `id` (e.g., "Alice Johnson", "TechCorp", "San Francisco")
  - **type**: the entity type label in uppercase (e.g., "PERSON", "ORGANIZATION", "LOCATION", "DATE", "CONCEPT")
  - **description**: a brief 1-2 sentence description of the entity

# 2. Labeling Nodes
**Consistency**: Ensure you use basic or elementary types for the `type` field.
  - For example, when you identify an entity representing a person, always set `type` to **"PERSON"**.
  - Avoid using more specific terms like "Mathematician" or "Scientist" in `type`, keep those in `description`.
  - Don't use too generic terms like "Entity".
**Node IDs**: Never utilize integers as node IDs.
  - Both `id` and `name` should be the entity's name or human-readable identifier found in the text.

# 3. Handling Numerical Data and Dates
  - For example, when you identify an entity representing a date, make sure it has type **"DATE"**.
  - Extract the date in the format "YYYY-MM-DD"
  - If not possible to extract the whole date, extract month or year, or both if available.
  - **Property Format**: Properties must be in a key-value format.
  - **Quotation Marks**: Never use escaped single or double quotes within property values.
  - **Naming Convention**: Use snake_case for relationship names, e.g., `works_at`.

# 4. Coreference Resolution
  - **Maintain Entity Consistency**: When extracting entities, it's vital to ensure consistency.
  If an entity, such as "John Doe", is mentioned multiple times in the text but is referred to by different names or pronouns (e.g., "Joe", "he"),
  always use the most complete identifier for that entity throughout the knowledge graph. In this example, use "John Doe" as the node ID.
Remember, the knowledge graph should be coherent and easily understandable, so maintaining consistency in entity references is crucial.

# 5. Strict Compliance
Adhere to the rules strictly. Non-compliance will result in termination.

Extract nodes and edges from the provided text."#;

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
        let system_prompt = custom_prompt.unwrap_or(DEFAULT_GRAPH_PROMPT);

        let mut graph: KnowledgeGraph = self
            .llm
            .create_structured_output(
                text,
                system_prompt,
                Some(GenerationOptions {
                    temperature: Some(0.1), // Slightly non-zero to improve extraction robustness
                    max_tokens: Some(2000), // Fits within small local model context windows (4096)
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| CognifyError::LlmError(e.to_string()))?;

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
}
