//! Fact extractor using LLM for knowledge graph extraction.
//!
//! Port of Python's cognee/infrastructure/llm/extraction/knowledge_graph/extract_content_graph.py
//! and cognee/tasks/graph/extract_graph_from_data.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm};
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

# 1. Labeling Nodes
**Consistency**: Ensure you use basic or elementary types for node labels.
  - For example, when you identify an entity representing a person, always label it as **"PERSON"**.
  - Avoid using more specific terms like "Mathematician" or "Scientist", keep those as "description" property.
  - Don't use too generic terms like "Entity".
**Node IDs**: Never utilize integers as node IDs.
  - Node IDs should be names or human-readable identifiers found in the text.

# 2. Handling Numerical Data and Dates
  - For example, when you identify an entity representing a date, make sure it has type **"DATE"**.
  - Extract the date in the format "YYYY-MM-DD"
  - If not possible to extract the whole date, extract month or year, or both if available.
  - **Property Format**: Properties must be in a key-value format.
  - **Quotation Marks**: Never use escaped single or double quotes within property values.
  - **Naming Convention**: Use snake_case for relationship names, e.g., `works_at`.

# 3. Coreference Resolution
  - **Maintain Entity Consistency**: When extracting entities, it's vital to ensure consistency.
  If an entity, such as "John Doe", is mentioned multiple times in the text but is referred to by different names or pronouns (e.g., "Joe", "he"),
  always use the most complete identifier for that entity throughout the knowledge graph. In this example, use "John Doe" as the node ID.
Remember, the knowledge graph should be coherent and easily understandable, so maintaining consistency in entity references is crucial.

# 4. Strict Compliance
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
pub struct FactExtractor<L: Llm> {
    llm: Arc<L>,
}

impl<L: Llm + 'static> FactExtractor<L> {
    /// Create a new fact extractor with the given LLM.
    ///
    /// # Arguments
    /// * `llm` - An LLM implementation (e.g., OpenAIAdapter, OllamaAdapter)
    ///
    /// # Returns
    /// A new FactExtractor instance
    pub fn new(llm: Arc<L>) -> Self {
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
    ///
    /// # Python equivalent
    /// ```python
    /// async def extract_content_graph(
    ///     content: str,
    ///     response_model: Type[BaseModel],
    ///     custom_prompt: Optional[str] = None,
    /// ):
    ///     system_prompt = custom_prompt or render_prompt(llm_config.graph_prompt_path, {})
    ///     content_graph = await LLMGateway.acreate_structured_output(
    ///         content, system_prompt, response_model
    ///     )
    ///     return content_graph
    /// ```
    pub async fn extract_facts(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<KnowledgeGraph, CognifyError> {
        debug!("Extracting facts from text: {}", text);
        let system_prompt = custom_prompt.unwrap_or(DEFAULT_GRAPH_PROMPT);

        let graph = self
            .llm
            .create_structured_output::<KnowledgeGraph>(
                text,
                system_prompt,
                Some(GenerationOptions {
                    temperature: Some(0.1), // Slightly non-zero to improve extraction robustness
                    max_tokens: Some(4000), // Generous limit for complex graphs
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
    ///
    /// # Python equivalent
    /// ```python
    /// chunk_graphs = await asyncio.gather(
    ///     *[extract_content_graph(chunk.text, graph_model) for chunk in data_chunks]
    /// )
    /// ```
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
    pub fn llm(&self) -> &Arc<L> {
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

        async fn create_structured_output<T>(
            &self,
            _text_input: &str,
            _system_prompt: &str,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<T>
        where
            T: serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema + Send,
        {
            // Return a mock knowledge graph
            let graph = KnowledgeGraph {
                nodes: vec![super::super::models::Node {
                    id: "test_node".to_string(),
                    name: "Test Node".to_string(),
                    node_type: "TEST".to_string(),
                    description: "A test node".to_string(),
                }],
                edges: vec![],
            };

            // This is a bit of a hack because we can't return a typed T directly
            // In real tests, you'd use a proper mocking library
            let json = serde_json::to_string(&graph).unwrap();
            let result: T = serde_json::from_str(&json).unwrap();
            Ok(result)
        }

        async fn create_structured_output_with_messages<T>(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<T>
        where
            T: serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema + Send,
        {
            unimplemented!()
        }

        fn model(&self) -> &str {
            "mock"
        }

        fn supports_streaming(&self) -> bool {
            false
        }

        fn supports_function_calling(&self) -> bool {
            false
        }

        fn max_context_length(&self) -> u32 {
            4096
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
}
