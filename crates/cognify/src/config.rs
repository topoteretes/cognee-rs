//! Configuration for the cognify pipeline.
//!
//! CRITICAL: This is the SINGLE SOURCE OF TRUTH for all pipeline configuration.
//! NO hardcoded values should exist in pipeline components.
//! NO environment variables should be read in pipeline components.
//! ALL configuration flows through this struct.

use std::sync::Arc;

use cognee_chunking::TokenCounterKind;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_llm::{Llm, Transcriber};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Chunks handed to the graph-extraction stage in one batch, and — because
/// Python imposes no in-batch concurrency ceiling — the default in-flight cap
/// as well.
///
/// This is the literal Python falls back to in
/// `cognee/api/v1/cognify/cognify.py:367-370` when neither the `chunks_per_batch`
/// argument nor `CognifyConfig.chunks_per_batch` is set (both default to `None`;
/// see `cognee/api/v1/cognify/cognify.py:60` and
/// `cognee/modules/cognify/config.py:12`). It is exported so the CLI/bindings
/// default for `LLM_MAX_PARALLEL_REQUESTS` and `SummaryExtractor`'s own default
/// stay single-sourced with it.
pub const PYTHON_CHUNKS_PER_BATCH: usize = 2000;

/// Configuration for the cognify pipeline.
///
/// Design Principles:
/// 1. NO hardcoded values in pipeline code - everything flows through config
/// 2. NO environment variable reading in components (only in config construction if needed)
/// 3. Sensible defaults matching `cognee` behavior
/// 4. Builder pattern for easy customization
///
/// What is NOT in this config:
/// - Storage/Database/LLM/Embedding instances (passed as Arc<T> to pipeline constructor)
/// - Runtime data (data_items, dataset_id, etc. - passed to cognify() method)
/// - Provider-specific API keys (handled by provider implementations, not pipeline config)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognifyConfig {
    /// Maximum chunk size in tokens. `None` means "auto-calculate at pipeline
    /// time" via [`CognifyConfig::auto_chunk_size`].
    ///
    /// Mirrors Python's `chunk_size: int = None` on the `cognify()` entry point,
    /// which resolves as `chunk_size or await get_max_chunk_tokens()`
    /// (`api/v1/cognify/cognify.py:270`). The pipeline in `tasks.rs` fills a
    /// `None` in before executing; the computed value depends on the active
    /// embedding engine and the configured LLM ceiling — ≈512 for the local
    /// ONNX/BGE default (512-token sequence limit), 8191 for an OpenAI-compatible
    /// engine at its default `max_completion_tokens`, both clamped by the LLM
    /// term (`llm_max_completion_tokens / 2`).
    ///
    /// Pass an explicit value via [`CognifyConfig::with_chunk_size`] to override
    /// the auto-calculation. Unlike the previous `1500`-sentinel encoding, an
    /// explicit `Some(1500)` is honoured rather than treated as "unset".
    pub max_chunk_size: Option<usize>,

    /// Overlap between chunks (in tokens).
    /// Python default: 10 (from ChunkConfig.chunk_overlap)
    /// Used when chunk_strategy is RECURSIVE or LANGCHAIN
    pub chunk_overlap: usize,

    /// Chunking strategy.
    /// Python default: ChunkStrategy.PARAGRAPH
    /// Options: Paragraph (sentence-aware), Recursive (character-based with overlap)
    pub chunk_strategy: ChunkStrategy,

    /// Number of chunks to process in a single batch during graph extraction.
    ///
    /// This is the direct analogue of Python's pipeline batch size — the
    /// `task_config={"batch_size": chunks_per_batch}` attached to the
    /// `extract_graph_and_summarize` task in `cognee/api/v1/cognify/cognify.py:388`.
    /// Python resolves it to **2000** by default (`cognify.py:367-370` falls back
    /// to a 2000 literal because `CognifyConfig.chunks_per_batch` is `None` in
    /// `cognee/modules/cognify/config.py:12`), so any document under 2000 chunks
    /// reaches the extraction stage as a single batch with no barrier in the
    /// middle. We match that literal.
    pub chunks_per_batch: usize,

    /// Maximum number of parallel extraction calls in flight within a batch.
    ///
    /// Python has no equivalent ceiling: `extract_graph_from_data.py:191` and
    /// `summarize_text.py:58` both `asyncio.gather` over every chunk in the
    /// batch, and `llm_rate_limit_enabled` is `False` by default
    /// (`cognee/infrastructure/llm/config.py:151`). Defaulting this to the same
    /// value as `chunks_per_batch` therefore makes it non-binding — identical
    /// behaviour to Python's unbounded gather — while keeping a real, reachable
    /// bound for operators on rate-limited keys (`LLM_MAX_PARALLEL_REQUESTS=1`
    /// is the documented way to record replay cassettes; see
    /// `scripts/perf/README.md`).
    pub max_parallel_extractions: usize,

    /// Custom prompt for entity/relationship extraction.
    /// Python parameter: custom_prompt (optional)
    /// If None, uses default prompts from cognee_llm
    pub custom_extraction_prompt: Option<String>,

    /// Enable text summarization stage.
    /// Python behavior: Always runs if summarization_model is set
    /// Default: true (matches Python)
    pub enable_summarization: bool,

    /// Whether to generate and index triplet embeddings.
    /// Triplets are formatted as "source › relationship › target"
    /// Python config: CognifyConfig.triplet_embedding (default: False)
    pub embed_triplets: bool,

    /// Batch size for embedding generation (all types: chunks, entities, summaries, triplets).
    /// Python default: varies by provider (36 for OpenAI, 100 for others)
    /// Controls how many texts are embedded in a single API call
    pub embedding_batch_size: usize,

    /// Vector collection name prefix.
    /// Python default: Uses type names directly ("Entity", "DocumentChunk", etc.)
    /// Allows customization for multi-tenant or versioned deployments
    pub vector_collection_prefix: String,

    /// Enable incremental loading - only process new/changed data.
    /// When true, tracks processed data IDs to avoid reprocessing.
    /// Python parameter: incremental_loading (default: True)
    pub incremental_loading: bool,

    /// Enable pipeline-level caching.
    /// When true, skips datasets whose latest pipeline run status is `Completed`.
    /// Requires a database connection to be provided.
    /// Python parameter: use_pipeline_cache (default: False)
    pub use_pipeline_cache: bool,

    /// Enable temporal graph construction.
    /// Python parameter: temporal_cognify (default: False)
    /// Extracts events and timestamps for temporal reasoning
    pub temporal_cognify: bool,

    /// Create WebPage/WebSite provenance nodes for URL-sourced documents.
    ///
    /// When true, documents whose external metadata was produced by URL
    /// ingestion create deterministic WebPage and WebSite nodes plus
    /// `DocumentChunk -> SOURCED_FROM -> WebPage` and
    /// `WebPage -> PART_OF -> WebSite` edges.
    pub create_web_page_nodes: bool,

    /// Batch size for data processing in temporal cognify.
    /// Python parameter: data_per_batch (default: 20)
    pub data_per_batch: usize,

    /// How to count tokens when chunking text.
    /// Default is determined at construction time via [`TokenCounterKind::from_env`].
    pub token_counter_kind: TokenCounterKind,

    /// Optional JSON Schema for custom graph extraction model.
    ///
    /// When `Some`, the LLM uses this schema instead of the default
    /// `KnowledgeGraph` schema for entity/relationship extraction.
    /// Extracted data is stored as-is in chunk metadata.
    ///
    /// Mirrors Python's `graph_model` parameter.
    #[serde(skip)]
    pub graph_schema: Option<serde_json::Value>,

    /// Optional JSON schema for the summarization output.
    ///
    /// Mirrors Python's `CognifyConfig.summarization_model` (a Pydantic class,
    /// default `SummarizedContent`). When `Some`, the summarization stage
    /// requests this schema from the LLM instead of the built-in
    /// `SummarizedContent` shape. The schema **must** contain a string
    /// `summary` field — the pipeline reads `summary` to build each
    /// `TextSummary` (Python parity).
    ///
    /// Validated at setter/builder time via `validate_summary_schema`.
    #[serde(skip)]
    pub summary_schema: Option<serde_json::Value>,

    /// Pluggable chunker callback.
    ///
    /// When `Some`, this function is called instead of the built-in
    /// paragraph/recursive chunking. The callback receives the text and
    /// max token count, and returns a list of chunk strings.
    ///
    /// Mirrors Python's `chunker` parameter.
    #[serde(skip)]
    pub custom_chunker: Option<CustomChunker>,

    /// Optional transcriber for audio/video document processing.
    ///
    /// When `Some`, this transcriber is used to convert audio content into
    /// text before chunking and graph extraction. Only takes effect when
    /// processing documents classified as audio type.
    #[serde(skip)]
    pub transcriber: Option<TranscriberHandle>,
}

/// Opaque wrapper around a custom chunker callback.
///
/// Implements [`Debug`] (prints `"CustomChunker(…)"`) and [`Clone`] (cheap
/// `Arc` clone), keeping [`CognifyConfig`] derivable.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct CustomChunker(pub Arc<dyn Fn(&str, usize) -> Vec<String> + Send + Sync>);

impl std::fmt::Debug for CustomChunker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CustomChunker(…)")
    }
}

/// Opaque wrapper around a [`Transcriber`] implementation.
///
/// Implements [`Debug`] (prints `"TranscriberHandle(…)"`) and [`Clone`] (cheap
/// `Arc` clone), keeping [`CognifyConfig`] derivable.
#[derive(Clone)]
pub struct TranscriberHandle(pub Arc<dyn Transcriber>);

impl std::fmt::Debug for TranscriberHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TranscriberHandle(…)")
    }
}

/// Chunking strategy options.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChunkStrategy {
    /// Paragraph-based chunking (sentence-aware, no overlap).
    /// Python: ChunkStrategy.PARAGRAPH
    /// Default and most reliable for semantic coherence.
    Paragraph,

    /// Recursive character-based chunking with overlap.
    /// Python: ChunkStrategy.RECURSIVE (via LangchainChunker)
    /// Better for preserving context across chunk boundaries.
    Recursive,
}

impl Default for CognifyConfig {
    fn default() -> Self {
        Self {
            max_chunk_size: None,
            chunk_overlap: 10,
            chunk_strategy: ChunkStrategy::Paragraph,

            chunks_per_batch: PYTHON_CHUNKS_PER_BATCH,
            max_parallel_extractions: PYTHON_CHUNKS_PER_BATCH,
            custom_extraction_prompt: None,

            enable_summarization: true,

            embed_triplets: false,
            embedding_batch_size: 100,
            vector_collection_prefix: String::new(),

            incremental_loading: true,

            use_pipeline_cache: false,

            temporal_cognify: false,
            create_web_page_nodes: true,
            data_per_batch: 20,

            token_counter_kind: TokenCounterKind::from_env(),

            graph_schema: None,
            summary_schema: None,
            custom_chunker: None,
            transcriber: None,
        }
    }
}

impl CognifyConfig {
    /// Fallback chunk size (in tokens) for callers that build a pipeline
    /// directly, without going through [`crate::cognify`]'s auto-calculation —
    /// e.g. `update`/`improve`. Matches the historical `CognifyConfig::default()`
    /// value so those paths are unchanged.
    pub const DEFAULT_MAX_CHUNK_SIZE: usize = 1500;

    /// Set maximum chunk size in tokens. An explicit value always wins over the
    /// auto-calculation, including [`Self::DEFAULT_MAX_CHUNK_SIZE`] itself.
    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.max_chunk_size = Some(size);
        self
    }

    /// Set the chunk size from an optional source (a CLI flag, a settings entry,
    /// an HTTP payload field), so a caller can thread "unset" through without
    /// inventing a sentinel.
    ///
    /// `None` means "unresolved", not "auto-calculated": only [`crate::cognify`]
    /// turns it into the auto-calculated value. A caller that builds a pipeline
    /// directly leaves it unresolved, and [`Self::chunk_size`] then yields
    /// [`Self::DEFAULT_MAX_CHUNK_SIZE`].
    pub fn with_chunk_size_opt(mut self, size: Option<usize>) -> Self {
        self.max_chunk_size = size;
        self
    }

    /// The chunk size to actually chunk with: the explicit value when set,
    /// otherwise [`Self::DEFAULT_MAX_CHUNK_SIZE`].
    ///
    /// [`crate::cognify`] resolves `None` to the auto-calculated value before
    /// building the pipeline, so the fallback only applies to callers that
    /// bypass that entry point.
    pub fn chunk_size(&self) -> usize {
        self.max_chunk_size.unwrap_or(Self::DEFAULT_MAX_CHUNK_SIZE)
    }

    /// Set chunk overlap (for recursive chunking).
    pub fn with_chunk_overlap(mut self, overlap: usize) -> Self {
        self.chunk_overlap = overlap;
        self
    }

    /// Set chunking strategy.
    pub fn with_chunk_strategy(mut self, strategy: ChunkStrategy) -> Self {
        self.chunk_strategy = strategy;
        self
    }

    /// Set number of chunks per batch during graph extraction.
    pub fn with_chunks_per_batch(mut self, batch_size: usize) -> Self {
        self.chunks_per_batch = batch_size;
        self
    }

    /// Set maximum parallel extractions.
    pub fn with_max_parallel_extractions(mut self, limit: usize) -> Self {
        self.max_parallel_extractions = limit;
        self
    }

    /// Set custom extraction prompt.
    pub fn with_custom_prompt(mut self, prompt: String) -> Self {
        self.custom_extraction_prompt = Some(prompt);
        self
    }

    /// Enable or disable summarization.
    pub fn with_summarization(mut self, enable: bool) -> Self {
        self.enable_summarization = enable;
        self
    }

    /// Enable or disable triplet embeddings.
    pub fn with_triplet_embeddings(mut self, enable: bool) -> Self {
        self.embed_triplets = enable;
        self
    }

    /// Set embedding batch size.
    pub fn with_embedding_batch_size(mut self, batch_size: usize) -> Self {
        self.embedding_batch_size = batch_size;
        self
    }

    /// Set vector collection prefix.
    pub fn with_collection_prefix(mut self, prefix: String) -> Self {
        self.vector_collection_prefix = prefix;
        self
    }

    /// Enable or disable incremental loading.
    pub fn with_incremental_loading(mut self, enable: bool) -> Self {
        self.incremental_loading = enable;
        self
    }

    /// Enable or disable pipeline-level caching.
    pub fn with_pipeline_cache(mut self, enable: bool) -> Self {
        self.use_pipeline_cache = enable;
        self
    }

    /// Enable or disable temporal cognify.
    pub fn with_temporal_cognify(mut self, enable: bool) -> Self {
        self.temporal_cognify = enable;
        self
    }

    /// Enable or disable WebPage/WebSite provenance graph construction.
    pub fn with_web_page_nodes(mut self, enable: bool) -> Self {
        self.create_web_page_nodes = enable;
        self
    }

    /// Set data per batch for temporal processing.
    pub fn with_data_per_batch(mut self, batch_size: usize) -> Self {
        self.data_per_batch = batch_size;
        self
    }

    /// Set the token counter implementation to use during chunking.
    pub fn with_token_counter(mut self, kind: TokenCounterKind) -> Self {
        self.token_counter_kind = kind;
        self
    }

    /// Set a custom JSON Schema for graph extraction.
    pub fn with_graph_schema(mut self, schema: serde_json::Value) -> Self {
        self.graph_schema = Some(schema);
        self
    }

    /// Set a custom JSON schema for summarization output (Python `summarization_model` parity).
    ///
    /// The schema must contain a string `summary` field — the pipeline reads
    /// `summary` to build each `TextSummary`. Returns an error if the schema
    /// lacks that field so callers catch the misconfiguration early rather than
    /// mid-pipeline.
    pub fn with_summary_schema(mut self, schema: serde_json::Value) -> Result<Self, ConfigError> {
        validate_summary_schema(&schema)?;
        self.summary_schema = Some(schema);
        Ok(self)
    }

    /// Set a custom chunker callback.
    #[allow(clippy::type_complexity)]
    pub fn with_custom_chunker(
        mut self,
        chunker: Arc<dyn Fn(&str, usize) -> Vec<String> + Send + Sync>,
    ) -> Self {
        self.custom_chunker = Some(CustomChunker(chunker));
        self
    }

    /// Set a transcriber for audio document processing.
    pub fn with_transcriber(mut self, transcriber: Arc<dyn Transcriber>) -> Self {
        self.transcriber = Some(TranscriberHandle(transcriber));
        self
    }

    /// Auto-calculate `max_chunk_size`, mirroring Python's `get_max_chunk_tokens()`
    /// from `cognee/infrastructure/llm/utils.py`:
    ///
    /// ```text
    /// llm_cutoff_point = llm_max_completion_tokens // 2   # Python default: 16384 → 8192
    /// max_chunk_tokens = min(embedding_engine.max_completion_tokens, llm_cutoff_point)
    /// ```
    ///
    /// Both terms are **completion-token** budgets, not context windows:
    /// - `embedding_engine.max_sequence_length()` — the engine's configured token
    ///   limit. Python's `EmbeddingConfig` default is **8191**
    ///   (`embeddings/config.py:81`), passed to the engine by the factory; the
    ///   engine class's own `__init__` default of 512 is overridden in that path.
    ///   Rust mirrors this: `EmbeddingConfig.max_completion_tokens` defaults to 8191.
    /// - `llm.max_completion_tokens()` — the configured `LLM_MAX_COMPLETION_TOKENS`
    ///   (default **16384**, `infrastructure/llm/config.py:120`), already clamped by
    ///   the model's documented output cap on adapters that track one. Python clamps
    ///   the same way via `min(model_max, llm_config.llm_max_completion_tokens)`.
    /// - So at the defaults, for an OpenAI-compatible engine: `min(8191, 8192) = 8191`.
    ///   For the local ONNX/BGE engine, `max_sequence_length()` is the model's
    ///   512-token limit, so `min(512, 8192) = 512`. Lowering the LLM ceiling for a
    ///   small-context endpoint makes the LLM term binding instead: at
    ///   `LLM_MAX_COMPLETION_TOKENS=4096` the cap becomes `min(8191, 2048) = 2048`.
    ///
    /// Not ported: Python's Ollama branch (`llm_config.ollama_num_ctx` when the model
    /// is absent from litellm's `model_cost`) has no Rust counterpart — there is no
    /// `num_ctx` setting — so an Ollama endpoint uses the configured ceiling like any
    /// other OpenAI-compatible provider.
    ///
    /// Result is at least 1.
    pub fn auto_chunk_size(embedding_engine: &dyn EmbeddingEngine, llm: &dyn Llm) -> usize {
        // Python rounds the halving down (`llm_max_completion_tokens // 2`).
        let llm_cutoff = llm.max_completion_tokens() as usize / 2;
        let embed_max = embedding_engine.max_sequence_length();
        llm_cutoff.min(embed_max).max(1)
    }

    /// Set max_chunk_size by auto-calculating from embedding and LLM capabilities.
    ///
    /// See [`auto_chunk_size`](Self::auto_chunk_size) for the formula used.
    pub fn with_auto_chunk_size(
        mut self,
        embedding_engine: &dyn EmbeddingEngine,
        llm: &dyn Llm,
    ) -> Self {
        self.max_chunk_size = Some(Self::auto_chunk_size(embedding_engine, llm));
        self
    }

    /// Validate configuration parameters.
    ///
    /// Returns an error if any parameters are invalid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Only an *explicit* chunk size can be validated here: an auto-calculated
        // one is derived from the engines at pipeline time and is always >= 1.
        if let Some(max_chunk_size) = self.max_chunk_size {
            if max_chunk_size == 0 {
                return Err(ConfigError::InvalidParameter(
                    "max_chunk_size must be greater than 0".to_string(),
                ));
            }

            if self.chunk_overlap >= max_chunk_size {
                return Err(ConfigError::InvalidParameter(
                    "chunk_overlap must be less than max_chunk_size".to_string(),
                ));
            }
        }

        if self.chunks_per_batch == 0 {
            return Err(ConfigError::InvalidParameter(
                "chunks_per_batch must be greater than 0".to_string(),
            ));
        }

        if self.max_parallel_extractions == 0 {
            return Err(ConfigError::InvalidParameter(
                "max_parallel_extractions must be greater than 0".to_string(),
            ));
        }

        if self.embedding_batch_size == 0 {
            return Err(ConfigError::InvalidParameter(
                "embedding_batch_size must be greater than 0".to_string(),
            ));
        }

        if self.data_per_batch == 0 {
            return Err(ConfigError::InvalidParameter(
                "data_per_batch must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }
}

/// Configuration error types.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid configuration parameter: {0}")]
    InvalidParameter(String),
    #[error("Invalid summary schema: {0}")]
    InvalidSummarySchema(String),
}

/// Validate that a JSON schema supplied for `summary_schema` has a string
/// `summary` property, so misconfigurations are caught at builder/setter time
/// rather than mid-pipeline.
pub fn validate_summary_schema(schema: &serde_json::Value) -> Result<(), ConfigError> {
    let obj = schema.as_object().ok_or_else(|| {
        ConfigError::InvalidSummarySchema("schema must be a JSON object".to_string())
    })?;

    let props = obj
        .get("properties")
        .and_then(|p| p.as_object())
        .ok_or_else(|| {
            ConfigError::InvalidSummarySchema("schema must have a 'properties' object".to_string())
        })?;

    let summary_prop = props.get("summary").ok_or_else(|| {
        ConfigError::InvalidSummarySchema(
            "schema 'properties' must include a 'summary' field".to_string(),
        )
    })?;

    // Accept either {"type": "string"} or no type constraint at all.
    if let Some(type_val) = summary_prop.get("type")
        && type_val.as_str() != Some("string")
    {
        return Err(ConfigError::InvalidSummarySchema(
            "'summary' field must be of type 'string'".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cognee_embedding::error::EmbeddingResult;
    use cognee_llm::types::GenerationOptions;

    // Minimal mock for EmbeddingEngine — only max_sequence_length() matters.
    struct MockEmbedding {
        max_seq: usize,
    }

    #[async_trait]
    impl EmbeddingEngine for MockEmbedding {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![])
        }
        fn dimension(&self) -> usize {
            384
        }
        fn batch_size(&self) -> usize {
            32
        }
        fn max_sequence_length(&self) -> usize {
            self.max_seq
        }
    }

    // Minimal mock for Llm — only the two token-budget accessors matter.
    struct MockLlm {
        max_ctx: u32,
        /// `None` exercises the trait's default ceiling (16384).
        max_completion: Option<u32>,
    }

    impl MockLlm {
        /// A mock at the default completion ceiling.
        fn with_ctx(max_ctx: u32) -> Self {
            Self {
                max_ctx,
                max_completion: None,
            }
        }

        /// A mock with an operator-configured completion ceiling.
        fn with_completion_ceiling(ceiling: u32) -> Self {
            Self {
                max_ctx: 4096,
                max_completion: Some(ceiling),
            }
        }
    }

    #[async_trait]
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
            unimplemented!()
        }
        fn model(&self) -> &str {
            "mock"
        }
        fn max_context_length(&self) -> u32 {
            self.max_ctx
        }
        fn max_completion_tokens(&self) -> u32 {
            self.max_completion
                .unwrap_or(cognee_llm::types::DEFAULT_MAX_COMPLETION_TOKENS)
        }
    }

    #[test]
    fn test_default_config() {
        let config = CognifyConfig::default();

        // Chunking defaults: unset = auto-calculated at pipeline time.
        assert_eq!(config.max_chunk_size, None);
        assert_eq!(config.chunk_size(), CognifyConfig::DEFAULT_MAX_CHUNK_SIZE);
        assert_eq!(config.chunk_overlap, 10);
        assert_eq!(config.chunk_strategy, ChunkStrategy::Paragraph);

        // Graph extraction defaults. Both track Python's resolved pipeline batch
        // size (2000); max_parallel_extractions matching it makes the in-flight
        // cap non-binding at the default batch size, i.e. equivalent to Python's
        // unbounded `asyncio.gather`.
        assert_eq!(config.chunks_per_batch, PYTHON_CHUNKS_PER_BATCH);
        assert_eq!(config.max_parallel_extractions, PYTHON_CHUNKS_PER_BATCH);
        assert!(config.custom_extraction_prompt.is_none());

        // Summarization defaults
        assert!(config.enable_summarization);

        // Embedding defaults
        assert!(!config.embed_triplets);
        assert_eq!(config.embedding_batch_size, 100);
        assert_eq!(config.vector_collection_prefix, "");

        // Incremental defaults
        assert!(config.incremental_loading);

        // Pipeline cache defaults
        assert!(!config.use_pipeline_cache);

        // Advanced defaults
        assert!(!config.temporal_cognify);
        assert_eq!(config.data_per_batch, 20);
    }

    #[test]
    fn test_config_builder_chunking() {
        let config = CognifyConfig::default()
            .with_chunk_size(2000)
            .with_chunk_overlap(50)
            .with_chunk_strategy(ChunkStrategy::Recursive);

        assert_eq!(config.max_chunk_size, Some(2000));
        assert_eq!(config.chunk_overlap, 50);
        assert_eq!(config.chunk_strategy, ChunkStrategy::Recursive);
    }

    #[test]
    fn test_config_builder_graph_extraction() {
        let config = CognifyConfig::default()
            .with_chunks_per_batch(50)
            .with_max_parallel_extractions(25)
            .with_custom_prompt("Extract entities:".to_string());

        assert_eq!(config.chunks_per_batch, 50);
        assert_eq!(config.max_parallel_extractions, 25);
        assert_eq!(
            config.custom_extraction_prompt,
            Some("Extract entities:".to_string())
        );
    }

    #[test]
    fn test_config_builder_all_features() {
        let config = CognifyConfig::default()
            .with_chunk_size(2000)
            .with_triplet_embeddings(true)
            .with_incremental_loading(false)
            .with_summarization(false)
            .with_temporal_cognify(true);

        assert_eq!(config.max_chunk_size, Some(2000));
        assert!(config.embed_triplets);
        assert!(!config.incremental_loading);
        assert!(!config.enable_summarization);
        assert!(config.temporal_cognify);
    }

    #[test]
    fn test_config_validation_success() {
        let config = CognifyConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_zero_chunk_size() {
        let config = CognifyConfig {
            max_chunk_size: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidParameter(_))
        ));
    }

    #[test]
    fn test_config_validation_overlap_too_large() {
        let config = CognifyConfig {
            max_chunk_size: Some(100),
            chunk_overlap: 100,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidParameter(_))
        ));
    }

    #[test]
    fn test_config_validation_zero_batch_sizes() {
        let config1 = CognifyConfig {
            chunks_per_batch: 0,
            ..Default::default()
        };
        assert!(config1.validate().is_err());

        let config2 = CognifyConfig {
            embedding_batch_size: 0,
            ..Default::default()
        };
        assert!(config2.validate().is_err());
    }

    /// Local ONNX/BGE default: the model's 512-token sequence limit binds →
    /// min(512, 8192) = 512, where 8192 is the default completion ceiling
    /// (16384) halved. For an OpenAI-compatible engine at its default
    /// max_completion_tokens (8191), the result would be min(8191, 8192) = 8191
    /// — see `test_auto_chunk_size_large_embedding` for the >8192 case.
    #[test]
    fn auto_chunk_size_matches_python_default() {
        // BGE-like embedding: max_seq=512 binds → result=512.
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(4096);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 512);
    }

    #[test]
    fn test_auto_chunk_size_embed_is_smaller() {
        // embed_max=512, LLM cutoff=8192 → result=512 (embedding term dominates).
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(4096);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 512);
    }

    #[test]
    fn test_auto_chunk_size_ignores_context_window() {
        // The LLM *context window* is not the cutoff term — the *completion*
        // ceiling is. A tiny context window therefore does not restrict the chunk
        // size; the embedding term (512) still dominates.
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(256);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 512);
    }

    /// TOP-8 regression: the configured `LLM_MAX_COMPLETION_TOKENS` must reach the
    /// chunk cap. Before the fix this term was hardcoded to Python's 16384 default,
    /// so lowering the ceiling for a small-context endpoint changed nothing.
    #[test]
    fn auto_chunk_size_honours_configured_llm_ceiling() {
        // Embedding is generous, so the LLM term binds: 4096 / 2 = 2048.
        let embed = MockEmbedding { max_seq: 8191 };
        let llm = MockLlm::with_completion_ceiling(4096);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 2048);
    }

    #[test]
    fn auto_chunk_size_rounds_the_halving_down() {
        // Python computes `llm_max_completion_tokens // 2` (floor division).
        let embed = MockEmbedding { max_seq: 8191 };
        let llm = MockLlm::with_completion_ceiling(4097);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 2048);
    }

    #[test]
    fn auto_chunk_size_embedding_still_binds_under_a_high_ceiling() {
        // Raising the LLM ceiling never lifts the cap past the embedding limit.
        let embed = MockEmbedding { max_seq: 8191 };
        let llm = MockLlm::with_completion_ceiling(200_000);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 8191);
    }

    /// TOP-8 regression: `1500` used to be a sentinel meaning "unset", so a caller
    /// explicitly asking for it was silently overridden by the auto-calculation.
    #[test]
    fn explicit_default_sized_chunk_is_not_treated_as_unset() {
        let explicit =
            CognifyConfig::default().with_chunk_size(CognifyConfig::DEFAULT_MAX_CHUNK_SIZE);
        assert_eq!(
            explicit.max_chunk_size,
            Some(CognifyConfig::DEFAULT_MAX_CHUNK_SIZE),
            "an explicit chunk size must survive as Some(..) so cognify() does not auto-override it"
        );
        // The unset case is what selects the auto-calculation.
        assert_eq!(CognifyConfig::default().max_chunk_size, None);
    }

    #[test]
    fn with_chunk_size_opt_threads_unset_through() {
        assert_eq!(
            CognifyConfig::default()
                .with_chunk_size_opt(Some(2048))
                .max_chunk_size,
            Some(2048)
        );
        assert_eq!(
            CognifyConfig::default()
                .with_chunk_size_opt(None)
                .max_chunk_size,
            None
        );
    }

    /// An auto-calculated (unset) chunk size must not trip the overlap check —
    /// the real value is only known at pipeline time.
    #[test]
    fn validation_skips_overlap_check_when_chunk_size_is_auto() {
        let config = CognifyConfig {
            max_chunk_size: None,
            chunk_overlap: 100_000,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    /// ...but once resolved it must be enforced, which is why `cognify()`
    /// re-validates after auto-calculation rather than only before.
    #[test]
    fn validation_catches_a_too_large_overlap_once_the_size_is_resolved() {
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(4096);
        let resolved = CognifyConfig {
            max_chunk_size: None,
            chunk_overlap: 100_000,
            ..Default::default()
        }
        .with_auto_chunk_size(&embed, &llm);
        assert_eq!(resolved.max_chunk_size, Some(512));
        assert!(matches!(
            resolved.validate(),
            Err(ConfigError::InvalidParameter(_))
        ));
    }

    #[test]
    fn test_auto_chunk_size_large_embedding() {
        // embed_max=10000 > llm_cutoff (8192) → result=8192 (LLM constant dominates).
        let embed = MockEmbedding { max_seq: 10_000 };
        let llm = MockLlm::with_ctx(4096);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 8192);
    }

    #[test]
    fn test_auto_chunk_size_equal_values() {
        // embed_max=1024 < 8192 → result=1024 (embedding term dominates).
        let embed = MockEmbedding { max_seq: 1024 };
        let llm = MockLlm::with_ctx(2048);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 1024);
    }

    #[test]
    fn test_auto_chunk_size_floor_at_one() {
        // embed_max=0 → min(0, 8192)=0 → clamped to 1.
        let embed = MockEmbedding { max_seq: 0 };
        let llm = MockLlm::with_ctx(0);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 1);
    }

    #[test]
    fn test_auto_chunk_size_embed_exactly_at_llm_cutoff() {
        // embed_max=8192 == llm_cutoff → result=8192.
        let embed = MockEmbedding { max_seq: 8192 };
        let llm = MockLlm::with_ctx(4096);
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 8192);
    }

    #[test]
    fn test_with_auto_chunk_size_builder() {
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(4096);
        let config = CognifyConfig::default().with_auto_chunk_size(&embed, &llm);
        assert_eq!(config.max_chunk_size, Some(512));
        // Other fields should remain at defaults
        assert_eq!(config.chunk_overlap, 10);
        assert_eq!(config.chunks_per_batch, PYTHON_CHUNKS_PER_BATCH);
    }
}
