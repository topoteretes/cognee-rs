//! Configuration for the cognify pipeline.
//!
//! CRITICAL: This is the SINGLE SOURCE OF TRUTH for all pipeline configuration.
//! NO hardcoded values should exist in pipeline components.
//! NO environment variables should be read in pipeline components.
//! ALL configuration flows through this struct.

use cognee_chunking::TokenCounterKind;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_llm::Llm;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    /// Maximum chunk size in tokens.
    /// Python default: 1500 (from ChunkConfig.chunk_size)
    /// Note: In Python, can be auto-calculated from LLM max_completion_tokens if None
    pub max_chunk_size: usize,

    /// Overlap between chunks (in tokens).
    /// Python default: 10 (from ChunkConfig.chunk_overlap)
    /// Used when chunk_strategy is RECURSIVE or LANGCHAIN
    pub chunk_overlap: usize,

    /// Chunking strategy.
    /// Python default: ChunkStrategy.PARAGRAPH
    /// Options: Paragraph (sentence-aware), Recursive (character-based with overlap)
    pub chunk_strategy: ChunkStrategy,

    /// Number of chunks to process in a single batch during graph extraction.
    /// Python default: 100 (cognify parameter)
    /// Controls memory usage vs parallelism tradeoff
    pub chunks_per_batch: usize,

    /// Maximum number of parallel tasks for graph extraction within a batch.
    /// Python default: No explicit limit (uses asyncio.gather)
    /// Rust: Prevents spawning too many tokio tasks
    pub max_parallel_extractions: usize,

    /// Custom prompt for entity/relationship extraction.
    /// Python parameter: custom_prompt (optional)
    /// If None, uses default prompts from cognee_llm
    pub custom_extraction_prompt: Option<String>,

    /// Enable text summarization stage.
    /// Python behavior: Always runs if summarization_model is set
    /// Default: true (matches Python)
    pub enable_summarization: bool,

    /// Batch size for summarization (parallel summary generation).
    /// Python default: No explicit batching (processes all chunks in parallel)
    /// Rust: Prevents spawning too many tasks
    pub summarization_batch_size: usize,

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

    /// Batch size for data processing in temporal cognify.
    /// Python parameter: data_per_batch (default: 20)
    pub data_per_batch: usize,

    /// How to count tokens when chunking text.
    /// Default is determined at construction time via [`TokenCounterKind::from_env`].
    pub token_counter_kind: TokenCounterKind,
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
            max_chunk_size: 1500,
            chunk_overlap: 10,
            chunk_strategy: ChunkStrategy::Paragraph,

            chunks_per_batch: 100,
            max_parallel_extractions: 20,
            custom_extraction_prompt: None,

            enable_summarization: true,
            summarization_batch_size: 50,

            embed_triplets: false,
            embedding_batch_size: 100,
            vector_collection_prefix: String::new(),

            incremental_loading: true,

            use_pipeline_cache: false,

            temporal_cognify: false,
            data_per_batch: 20,

            token_counter_kind: TokenCounterKind::from_env(),
        }
    }
}

impl CognifyConfig {
    /// Set maximum chunk size in tokens.
    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.max_chunk_size = size;
        self
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

    /// Set summarization batch size.
    pub fn with_summarization_batch_size(mut self, batch_size: usize) -> Self {
        self.summarization_batch_size = batch_size;
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

    /// Auto-calculate max_chunk_size from embedding and LLM capabilities.
    ///
    /// Formula: `min(embedding_engine.max_sequence_length(), llm.max_context_length() / 2)`
    ///
    /// Matches Python's `get_max_chunk_tokens()` from
    /// `cognee/infrastructure/llm/utils.py`:
    /// - Chunk size must not exceed half of the LLM's max context token size
    /// - Chunk size must not exceed the embedding engine's max token size
    /// - Result is at least 1
    pub fn auto_chunk_size(embedding_engine: &dyn EmbeddingEngine, llm: &dyn Llm) -> usize {
        let llm_cutoff = (llm.max_context_length() / 2) as usize;
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
        self.max_chunk_size = Self::auto_chunk_size(embedding_engine, llm);
        self
    }

    /// Validate configuration parameters.
    ///
    /// Returns an error if any parameters are invalid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_chunk_size == 0 {
            return Err(ConfigError::InvalidParameter(
                "max_chunk_size must be greater than 0".to_string(),
            ));
        }

        if self.chunk_overlap >= self.max_chunk_size {
            return Err(ConfigError::InvalidParameter(
                "chunk_overlap must be less than max_chunk_size".to_string(),
            ));
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

        if self.summarization_batch_size == 0 {
            return Err(ConfigError::InvalidParameter(
                "summarization_batch_size must be greater than 0".to_string(),
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

    // Minimal mock for Llm — only max_context_length() matters.
    struct MockLlm {
        max_ctx: u32,
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
    }

    #[test]
    fn test_default_config() {
        let config = CognifyConfig::default();

        // Chunking defaults
        assert_eq!(config.max_chunk_size, 1500);
        assert_eq!(config.chunk_overlap, 10);
        assert_eq!(config.chunk_strategy, ChunkStrategy::Paragraph);

        // Graph extraction defaults
        assert_eq!(config.chunks_per_batch, 100);
        assert_eq!(config.max_parallel_extractions, 20);
        assert!(config.custom_extraction_prompt.is_none());

        // Summarization defaults
        assert!(config.enable_summarization);
        assert_eq!(config.summarization_batch_size, 50);

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

        assert_eq!(config.max_chunk_size, 2000);
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

        assert_eq!(config.max_chunk_size, 2000);
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
            max_chunk_size: 0,
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
            max_chunk_size: 100,
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

        let config3 = CognifyConfig {
            summarization_batch_size: 0,
            ..Default::default()
        };
        assert!(config3.validate().is_err());
    }

    #[test]
    fn test_auto_chunk_size_embed_is_smaller() {
        // embed_max=512, llm_context=4096 → llm_cutoff=2048 → result=512
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm { max_ctx: 4096 };
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 512);
    }

    #[test]
    fn test_auto_chunk_size_llm_cutoff_is_smaller() {
        // embed_max=512, llm_context=256 → llm_cutoff=128 → result=128
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm { max_ctx: 256 };
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 128);
    }

    #[test]
    fn test_auto_chunk_size_equal_values() {
        // embed_max=1024, llm_context=2048 → llm_cutoff=1024 → result=1024
        let embed = MockEmbedding { max_seq: 1024 };
        let llm = MockLlm { max_ctx: 2048 };
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 1024);
    }

    #[test]
    fn test_auto_chunk_size_floor_at_one() {
        // embed_max=0, llm_context=0 → both 0 → result clamped to 1
        let embed = MockEmbedding { max_seq: 0 };
        let llm = MockLlm { max_ctx: 0 };
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 1);
    }

    #[test]
    fn test_auto_chunk_size_odd_llm_context() {
        // llm_context=4097 → llm_cutoff=2048 (integer division), embed_max=3000 → result=2048
        let embed = MockEmbedding { max_seq: 3000 };
        let llm = MockLlm { max_ctx: 4097 };
        assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 2048);
    }

    #[test]
    fn test_with_auto_chunk_size_builder() {
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm { max_ctx: 4096 };
        let config = CognifyConfig::default().with_auto_chunk_size(&embed, &llm);
        assert_eq!(config.max_chunk_size, 512);
        // Other fields should remain at defaults
        assert_eq!(config.chunk_overlap, 10);
        assert_eq!(config.chunks_per_batch, 100);
    }
}
