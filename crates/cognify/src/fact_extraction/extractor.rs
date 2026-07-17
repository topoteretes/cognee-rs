//! Fact extractor using LLM for knowledge graph extraction.
//!
//! Port of Python's cognee/infrastructure/llm/extraction/knowledge_graph/extract_content_graph.py
//! and cognee/tasks/graph/extract_graph_from_data.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmError, LlmExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::models::{GraphModel, KnowledgeGraph};
use crate::error::CognifyError;

/// Default system prompt for knowledge graph extraction.
///
/// Vendored from Python's
/// `cognee/infrastructure/llm/prompts/generate_graph_prompt.txt`, with one
/// intentional Rust-only addition: the "Node Descriptions" instruction (see the
/// drift guard in the inline `#[cfg(test)]` block below). Python advertises
/// `Node.description` as required only via the injected schema in instructor's
/// JSON mode, which is absent in tool/function/`json_schema` modes and on
/// providers that reject `json_schema` (e.g. Groq) — so a non-strict model omits
/// the field and hard-fails cognify (issue #66). Reinforcing it in the prompt
/// text closes that gap. Any future re-sync from Python MUST preserve this line.
const DEFAULT_GRAPH_PROMPT: &str = include_str!("prompts/generate_graph_prompt.txt");

/// Appended to the system prompt when extracting a group of chunks in one call.
/// It turns the single-graph task into a per-chunk one so results map back by
/// index (issue #19, multi-chunk batching).
const BATCH_GRAPH_INSTRUCTIONS: &str = "\n\n# Batch mode\n\
You are given a JSON array of text chunks, each an object with an \"index\" and \
a \"text\". Apply all the rules above to EACH chunk independently and extract a \
separate knowledge graph for each. Do not merge entities across chunks. Return a \
JSON object of the form {\"graphs\": [ ... ]} whose \"graphs\" array has exactly \
one entry per input chunk, in the SAME order as the input (graphs[i] is the graph \
for the chunk with index i).";

/// Structured-output wrapper: one [`KnowledgeGraph`] per chunk in a batched call.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct BatchedGraphs {
    /// One graph per input chunk, in input order.
    #[serde(default)]
    graphs: Vec<KnowledgeGraph>,
}

/// Ensure every node has a non-empty name (Python `Node.__init__` compat: name
/// defaults to id when empty). Shared by the per-chunk and batched paths so the
/// rule can only ever live in one place.
fn normalize_node_names(graph: &mut KnowledgeGraph) {
    for node in &mut graph.nodes {
        if node.name.is_empty() {
            node.name = node.id.clone();
        }
    }
}

/// The `GenerationOptions` shared by the per-chunk and batched extraction calls.
///
/// `max_tokens: None` is a documented invariant (Python parity): the Python
/// `acreate_structured_output` passes no cap, so the response uses the model's
/// full default output budget. A small cap truncates a dense graph mid-JSON and
/// aborts cognify with a deserialization error. Kept in one place so the two
/// call sites cannot drift.
fn extraction_options() -> GenerationOptions {
    GenerationOptions {
        temperature: Some(0.1),
        max_tokens: None,
        ..Default::default()
    }
}

/// Why a batched group call could not be used as-is, so the caller can react to
/// each cause differently.
enum GroupError {
    /// The provider rate-limited the batched call. Falling back to per-chunk
    /// would issue `group_size` more requests and amplify the 429, so the caller
    /// propagates this instead of fanning out.
    RateLimited(CognifyError),
    /// A parse / serialization / other failure. Safe to fall back to per-chunk
    /// extraction, since that is a different (smaller) request shape.
    Other(CognifyError),
}

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
            .create_structured_output(text, system_prompt, Some(extraction_options()))
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
        normalize_node_names(&mut graph);

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
                result.map_err(|e| CognifyError::LlmError(format!("Task join error: {e}")))??;
            graphs.push(graph);
        }

        Ok(graphs)
    }

    /// Extract facts from many chunks, sending `group_size` chunks per LLM call.
    ///
    /// This cuts the number of extraction requests from `texts.len()` to
    /// `ceil(texts.len() / group_size)`: each call asks the model for one graph
    /// per chunk in the group, and the graphs are mapped back to their chunks by
    /// input order (issue #19, multi-chunk batching).
    ///
    /// Failure isolation: if a group's batched response fails to parse or does
    /// not return exactly one graph per chunk, that group alone falls back to
    /// per-chunk extraction, so one bad group can never drop the others. Output
    /// order always matches input order, so callers that pair graphs with chunk
    /// ids by position stay correct.
    ///
    /// `group_size <= 1` reduces to per-chunk extraction (one request per chunk).
    /// That equivalence is on request count only: groups here are dispatched
    /// serially, whereas [`extract_facts_batch`](Self::extract_facts_batch) runs
    /// chunks concurrently, so the two are not latency-equivalent. Callers that
    /// wire this into a concurrent stage should group chunks and push each group
    /// through their existing parallelism bound rather than hand a whole document
    /// to this serial loop.
    ///
    /// Note: this trades requests for weaker per-chunk isolation and a larger
    /// per-call prompt. Whether it preserves extraction quality depends on the
    /// model and is a data-driven decision (see the parity harness in the tests);
    /// keep it opt-in until a calibration run on your corpus supports the chosen
    /// `group_size`. The count win is also not unconditional: a mis-counted group
    /// falls back to per-chunk, so a model that persistently mis-counts issues
    /// `N + ceil(N / group_size)` requests, worse than plain per-chunk `N`. The
    /// calibration run should therefore watch the mismatch rate, not just
    /// node/edge parity.
    pub async fn extract_facts_grouped(
        &self,
        texts: Vec<String>,
        custom_prompt: Option<String>,
        group_size: usize,
    ) -> Result<Vec<KnowledgeGraph>, CognifyError> {
        let group_size = group_size.max(1);
        let mut out: Vec<KnowledgeGraph> = Vec::with_capacity(texts.len());

        for group in texts.chunks(group_size) {
            // A single-chunk group (group_size == 1, or a trailing remainder like
            // n=7,k=3 -> [3,3,1]) takes the per-chunk path: batching one chunk
            // saves no requests and would send it as a `[{index,text}]` array with
            // the batch instructions, a different prompt than plain `extract_facts`.
            if group.len() == 1 {
                out.push(
                    self.extract_facts(&group[0], custom_prompt.as_deref())
                        .await?,
                );
                continue;
            }

            // A batched group either succeeds with the right count, or we decide
            // whether to fall back to per-chunk. We never fall back on a provider
            // rate limit: that would issue `group_size` more requests and amplify
            // the 429 the batched call already hit, so it propagates instead.
            let fall_back = match self.extract_group(group, custom_prompt.as_deref()).await {
                Ok(graphs) if graphs.len() == group.len() => {
                    out.extend(graphs);
                    false
                }
                Ok(_) => {
                    warn!(
                        "Batched extraction returned the wrong graph count; \
                         falling back to per-chunk for this group."
                    );
                    true
                }
                Err(GroupError::RateLimited(e)) => return Err(e),
                Err(GroupError::Other(e)) => {
                    warn!("Batched extraction failed ({e}); falling back to per-chunk.");
                    true
                }
            };

            if fall_back {
                for text in group {
                    out.push(self.extract_facts(text, custom_prompt.as_deref()).await?);
                }
            }
        }

        Ok(out)
    }

    /// One batched LLM call for a group of chunks. Returns one graph per chunk,
    /// in input order. Caller is responsible for the per-chunk fallback when the
    /// returned count does not match the group size. A provider rate limit is
    /// surfaced as [`GroupError::RateLimited`] so the caller does not fan out.
    async fn extract_group(
        &self,
        group: &[String],
        custom_prompt: Option<&str>,
    ) -> Result<Vec<KnowledgeGraph>, GroupError> {
        let base = custom_prompt.unwrap_or(DEFAULT_GRAPH_PROMPT);
        let system_prompt = format!("{base}{BATCH_GRAPH_INSTRUCTIONS}");

        let input: Vec<serde_json::Value> = group
            .iter()
            .enumerate()
            .map(|(index, text)| serde_json::json!({ "index": index, "text": text }))
            .collect();
        let user_prompt = serde_json::to_string(&input)
            .map_err(|e| GroupError::Other(CognifyError::SerializationError(e.to_string())))?;

        let batched: BatchedGraphs = self
            .llm
            .create_structured_output(
                &user_prompt,
                &system_prompt,
                // Same options as the per-chunk path (no output cap); see
                // `extraction_options`.
                Some(extraction_options()),
            )
            .await
            .map_err(|e| {
                let ce = CognifyError::LlmError(e.to_string());
                if matches!(e, LlmError::RateLimitExceeded(_)) {
                    GroupError::RateLimited(ce)
                } else {
                    GroupError::Other(ce)
                }
            })?;

        let mut graphs = batched.graphs;
        // Same post-processing as extract_facts (shared helper keeps them in lockstep).
        for graph in &mut graphs {
            normalize_node_names(graph);
        }
        debug!(
            "Batched extraction produced {} graph(s) for {} chunk(s)",
            graphs.len(),
            group.len()
        );
        Ok(graphs)
    }

    /// Get a reference to the underlying LLM.
    pub fn llm(&self) -> &Arc<dyn Llm> {
        &self.llm
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
        // Re-sync from Python, then RE-APPLY the Rust-only "Node Descriptions"
        // addition (issue #66) — do not blindly overwrite:
        //   cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt \
        //     crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt
        //   # then restore the "Node Descriptions" block under "# 1. Labeling Nodes"
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
        // Rust-only addition (issue #66): non-strict LLMs (e.g. Groq) omit node
        // `description` unless the prompt demands it. Guard against a re-sync
        // from Python silently dropping this line.
        assert!(
            vendored.contains(r#"Every node MUST include a "description" field"#),
            "Node-description instruction missing — issue #66 fix regressed"
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

    // ----- Multi-chunk batching (issue #19) -----

    /// Mock that answers both extraction shapes. A batched call sends the last
    /// message as a JSON array of `{index, text}` chunks; the mock returns one
    /// graph per chunk (with an empty node name, to exercise post-processing). A
    /// per-chunk call sends raw text; the mock returns a single graph. With
    /// `bad_batch`, batched calls return the wrong graph count to drive the
    /// per-chunk fallback.
    struct AdaptiveMock {
        bad_batch: bool,
    }

    #[async_trait::async_trait]
    impl Llm for AdaptiveMock {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unreachable!("extraction uses structured output, not generate")
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let last = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            // A batched call serialises the chunks as a JSON array.
            if let Ok(chunks) = serde_json::from_str::<Vec<serde_json::Value>>(last) {
                let count = if self.bad_batch { 0 } else { chunks.len() };
                let graphs: Vec<serde_json::Value> = (0..count)
                    .map(|_| {
                        serde_json::json!({
                            "nodes": [{"id": "n", "name": "", "type": "T", "description": ""}],
                            "edges": []
                        })
                    })
                    .collect();
                return Ok(serde_json::json!({ "graphs": graphs }));
            }
            // Per-chunk call: one graph.
            Ok(serde_json::json!({ "nodes": [], "edges": [] }))
        }

        fn model(&self) -> &str {
            "adaptive-mock"
        }
    }

    /// The headline number for #19: batching cuts the request count from N to
    /// ceil(N / group_size), counted through the ThrottleLlm instrument.
    #[tokio::test]
    async fn grouped_extraction_reduces_request_count() {
        use cognee_llm::mock::{ThrottleConfig, ThrottleLlm};

        let n = 12usize;
        let k = 4usize;
        let texts: Vec<String> = (0..n).map(|i| format!("chunk number {i}")).collect();

        let per_chunk_throttle = Arc::new(ThrottleLlm::new(
            Arc::new(AdaptiveMock { bad_batch: false }) as Arc<dyn Llm>,
            ThrottleConfig::default(),
        ));
        let per_chunk = FactExtractor::new(per_chunk_throttle.clone() as Arc<dyn Llm>)
            .extract_facts_batch(texts.clone(), None)
            .await
            .unwrap();
        let per_chunk_calls = per_chunk_throttle.metrics().allowed;

        let grouped_throttle = Arc::new(ThrottleLlm::new(
            Arc::new(AdaptiveMock { bad_batch: false }) as Arc<dyn Llm>,
            ThrottleConfig::default(),
        ));
        let grouped = FactExtractor::new(grouped_throttle.clone() as Arc<dyn Llm>)
            .extract_facts_grouped(texts.clone(), None, k)
            .await
            .unwrap();
        let grouped_calls = grouped_throttle.metrics().allowed;

        println!("\ngraph extraction, {n}-chunk document, group_size={k}:");
        println!("{:<24} {:>10}", "scenario", "requests");
        println!("{:<24} {:>10}", "per-chunk (pre-#19)", per_chunk_calls);
        println!("{:<24} {:>10}", "batched", grouped_calls);

        assert_eq!(per_chunk.len(), n);
        assert_eq!(grouped.len(), n);
        assert_eq!(
            per_chunk_calls, n as u64,
            "per-chunk = one request per chunk"
        );
        assert_eq!(
            grouped_calls,
            n.div_ceil(k) as u64,
            "batched = ceil(n / group_size) requests"
        );
    }

    /// Batching returns exactly one graph per chunk in input order, and applies
    /// the same empty-name to id post-processing as the per-chunk path.
    #[tokio::test]
    async fn grouped_extraction_returns_one_graph_per_chunk() {
        let texts: Vec<String> = (0..7).map(|i| format!("chunk {i}")).collect();
        let graphs = FactExtractor::new(Arc::new(AdaptiveMock { bad_batch: false }))
            .extract_facts_grouped(texts.clone(), None, 3)
            .await
            .unwrap();

        assert_eq!(
            graphs.len(),
            texts.len(),
            "one graph per chunk, order preserved"
        );
        for graph in &graphs {
            for node in &graph.nodes {
                assert!(
                    !node.name.is_empty(),
                    "empty node name should default to id"
                );
                assert_eq!(
                    node.name, node.id,
                    "post-processing must run on the batched path"
                );
            }
        }
    }

    /// Failure isolation: when every batched response has the wrong graph count,
    /// each group falls back to per-chunk extraction, so all chunks still produce
    /// a graph. Requests = one failed batched attempt per group + N fallbacks.
    #[tokio::test]
    async fn grouped_extraction_falls_back_on_bad_batch() {
        use cognee_llm::mock::{ThrottleConfig, ThrottleLlm};

        let n = 6usize;
        let k = 3usize;
        let texts: Vec<String> = (0..n).map(|i| format!("chunk {i}")).collect();

        let throttle = Arc::new(ThrottleLlm::new(
            Arc::new(AdaptiveMock { bad_batch: true }) as Arc<dyn Llm>,
            ThrottleConfig::default(),
        ));
        let graphs = FactExtractor::new(throttle.clone() as Arc<dyn Llm>)
            .extract_facts_grouped(texts.clone(), None, k)
            .await
            .unwrap();

        assert_eq!(
            graphs.len(),
            n,
            "fallback must still produce one graph per chunk"
        );
        let groups = n.div_ceil(k) as u64;
        assert_eq!(
            throttle.metrics().allowed,
            groups + n as u64,
            "expected {groups} batched attempts + {n} per-chunk fallbacks"
        );
    }

    /// Rate-limits a batched call but would succeed on a per-chunk call, so an
    /// `Ok` result would mean the fallback fired.
    struct RateLimitBatchMock;

    #[async_trait::async_trait]
    impl Llm for RateLimitBatchMock {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unreachable!("extraction uses structured output, not generate")
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let last = messages.last().map(|m| m.content.as_str()).unwrap_or("");
            // A batched call serialises the chunks as a JSON array: simulate a 429.
            if serde_json::from_str::<Vec<serde_json::Value>>(last).is_ok() {
                return Err(LlmError::RateLimitExceeded("simulated 429".to_string()));
            }
            // A per-chunk fallback call would succeed, so reaching here means we
            // fell back — which this test asserts must NOT happen.
            Ok(serde_json::json!({ "nodes": [], "edges": [] }))
        }

        fn model(&self) -> &str {
            "rate-limit-batch-mock"
        }
    }

    /// A provider rate limit on a batched call must propagate, not fan out to
    /// per-chunk extraction (which would issue `group_size` more requests and
    /// amplify the 429). The per-chunk path here would succeed, so an `Err`
    /// proves the fallback did not fire.
    #[tokio::test]
    async fn grouped_extraction_propagates_rate_limit_without_fallback() {
        let texts: Vec<String> = (0..4).map(|i| format!("chunk {i}")).collect();
        let result = FactExtractor::new(Arc::new(RateLimitBatchMock))
            .extract_facts_grouped(texts, None, 2)
            .await;

        let err = result.expect_err("rate limit must propagate instead of falling back");
        assert!(
            err.to_string().contains("simulated 429"),
            "expected the propagated rate-limit error, got: {err}"
        );
    }
}
