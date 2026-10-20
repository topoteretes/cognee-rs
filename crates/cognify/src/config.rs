//! Configuration for the cognify pipeline.
//!
//! CRITICAL: This is the SINGLE SOURCE OF TRUTH for all pipeline configuration.
//! NO hardcoded values should exist in pipeline components.
//! NO environment variables should be read in pipeline components.
//! ALL configuration flows through this struct.

use std::sync::Arc;

use cognee_chunking::{TokenCountMode, TokenCounterKind};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_llm::{Llm, Transcriber};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dynamic_graph::{DynamicGraphError, DynamicGraphSchema};
use crate::failure::{FailurePolicy, FailureStop, RollbackScope};
use crate::graph_backend::ChunkGraphExtractor;

/// Chunks handed to the graph-extraction stage in one batch.
///
/// Sized so that a batch boundary is rare rather than routine: the stage awaits
/// each batch before starting the next, so a small batch turns one document into
/// a series of serialised waves, each paying the latency of its slowest call.
///
/// Matches Python, which resolves its extraction `task_config` batch size to
/// this same 2000 literal in `cognee/api/v1/cognify/cognify.py:367-370` when
/// neither the `chunks_per_batch` argument nor `CognifyConfig.chunks_per_batch`
/// is set (both default to `None` — `cognify.py:60`,
/// `modules/cognify/config.py:12`). Any document under 2000 chunks therefore
/// reaches extraction as a single batch, in both SDKs.
pub const DEFAULT_CHUNKS_PER_BATCH: usize = 2000;

/// Default ceiling on LLM calls in flight within one extraction batch.
///
/// Matches Python's *effective* ceiling, which is not the absent one its
/// `asyncio.gather` suggests. `extract_graph_from_data.py:191` and
/// `summarize_text.py:58` do gather over every chunk in the batch with no
/// semaphore, and `llm_rate_limit_enabled` is `False` by default
/// (`cognee/infrastructure/llm/config.py:136`) — but the coroutines then contend
/// for an HTTP connection pool that admits a bounded number at a time:
///
/// | layer | `max_connections` |
/// |---|---|
/// | bare `httpx` | 100 (`httpx._config.DEFAULT_LIMITS`) |
/// | `openai-python` | 1000 (`openai._constants.DEFAULT_CONNECTION_LIMITS`) |
/// | `litellm` | 1000 (`HTTPHandler(concurrent_limit=1000)`) |
///
/// So Python never issues more than ~1000 concurrent LLM requests, whatever the
/// batch size. Rust has no equivalent transport default — `reqwest`/`hyper` bound
/// idle pooling, not in-flight requests — which is why
/// [`cognee_llm::in_flight`] exists to reproduce that ceiling, and why this
/// stage-level semaphore is set to the same number rather than to
/// [`DEFAULT_CHUNKS_PER_BATCH`]. Setting it to the batch size would let Rust do
/// something Python cannot.
///
/// Exported so
/// [`SummaryExtractor::DEFAULT_MAX_PARALLEL`](crate::summarization::SummaryExtractor::DEFAULT_MAX_PARALLEL)
/// stays single-sourced with it.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const DEFAULT_MAX_PARALLEL_EXTRACTIONS: usize = 1000;

/// Mobile default ceiling on in-flight LLM calls — see the desktop constant of
/// the same name for the parity argument.
///
/// Lowered to 128 on Android and iOS because 1000 concurrent sockets is not a
/// safe default on a mobile process: the typical descriptor budget is 1024, and
/// the graph, vector and relational stores plus open files draw from the same
/// pool, so a full-width extraction wave can exhaust it. An `EMFILE` is also the
/// one overload the retry path cannot absorb — `cognee-utils`' policy keys on
/// 429/503/529 and request timeouts, and a local descriptor failure is none of
/// those, so it surfaces as a hard error rather than backpressure.
///
/// Python does not face this: cognee's Python SDK does not target mobile.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const DEFAULT_MAX_PARALLEL_EXTRACTIONS: usize = 128;

/// How a chunk links to the custom DataPoint graph extracted from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkAttachment {
    /// Link to the stored root, or to the children replacing a transparent root.
    Direct,
    /// Link to every node the custom graph storage walk persists.
    All,
}

impl ChunkAttachment {
    /// Parse Python's `chunk_attachment` spelling: exactly `"direct"` or
    /// `"all"`, case-sensitive as `cognify.py` compares it.
    ///
    /// # Errors
    /// [`ConfigError::InvalidParameter`] naming the accepted values, the
    /// message Python raises for the same input.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "direct" => Ok(Self::Direct),
            "all" => Ok(Self::All),
            other => Err(ConfigError::InvalidParameter(format!(
                "Invalid chunk_attachment {}; expected 'direct', 'all', or None.",
                crate::dynamic_graph::python_str_repr(other)
            ))),
        }
    }

    /// The spelling [`Self::parse`] accepts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::All => "all",
        }
    }
}

impl std::str::FromStr for ChunkAttachment {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Read a caller-supplied graph schema for [`CognifyConfig::graph_schema`]:
/// a JSON Schema object, or a string holding one (what a CLI file, a C string
/// or a JSON-only binding carries).
///
/// `null` and an empty object are `None`, the default `KnowledgeGraph`, as
/// Python's HTTP cognify route treats an omitted or `{}` `graph_model`. The
/// schema itself is compiled by [`CognifyConfig::validate`], not here.
///
/// # Errors
/// [`ConfigError::InvalidParameter`] for a string that is not JSON, or for any
/// value (direct or parsed) that is not an object.
pub fn parse_graph_schema(
    value: &serde_json::Value,
) -> Result<Option<serde_json::Value>, ConfigError> {
    let parsed;
    let value = match value {
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).map_err(|error| {
                ConfigError::InvalidParameter(format!(
                    "graph_schema string is not valid JSON: {error}"
                ))
            })?;
            &parsed
        }
        other => other,
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Object(map) if map.is_empty() => Ok(None),
        serde_json::Value::Object(_) => Ok(Some(value.clone())),
        other => Err(ConfigError::InvalidParameter(format!(
            "graph_schema must be a JSON Schema object, got {}",
            json_kind(other)
        ))),
    }
}

/// The JSON type name of `value`, for error messages.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

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

    /// Overlap between chunks (in tokens). Python default: 10 (from
    /// `ChunkConfig.chunk_overlap`).
    ///
    /// # Inert — kept deliberately, on both sides
    ///
    /// **No chunking code reads this.** It is defaulted here, plumbed through
    /// `Settings`, the CLI and `bindings-common`, and then never consulted:
    /// `crates/chunking/` contains zero references to it, so setting it changes
    /// no chunk boundary. (An earlier version of this comment said "Used when
    /// chunk_strategy is RECURSIVE or LANGCHAIN", which was not true of any
    /// code path.)
    ///
    /// Nothing reads it at all, [`CognifyConfig::validate`] included. That
    /// validator used to reject `chunk_overlap >= max_chunk_size`, which made
    /// an inert knob able to fail a whole pipeline — and Python has no such
    /// rule anywhere. (The only upstream check is langchain's own, inside the
    /// opt-in `LangchainChunker` against its own `chunk_size=1024`, and it is
    /// `>` not `>=`.) The rule is gone, so this field is now genuinely
    /// unobservable: any value parses, and none changes a chunk boundary.
    ///
    /// It is a faithful port of a knob that is dead in Python too — though dead
    /// by unreachability, not by absence. Python declares
    /// `ChunkConfig.chunk_overlap = 10`
    /// (`cognee/infrastructure/data/chunking/config.py:16`) and
    /// `DefaultChunkEngine` genuinely consumes it — but
    /// `get_chunking_engine()` has zero callers, and the default path is
    /// `TextChunker` (`cognee/api/v1/cognify/cognify.py:62`), which never reads
    /// it. (`text_chunker_with_overlap.py` uses a *ratio*, a different opt-in
    /// knob.)
    ///
    /// Kept rather than removed because removal is a SemVer break across four
    /// public binding surfaces — `python/cognee_py/types.py`, `ts/src/types.ts`
    /// (plus `configSetChunkOverlap`), `java/…/CognifyOptions.java` and
    /// `capi/include/cognee_sdk.h` — for a knob that costs nothing to keep, and
    /// would diverge from Python, which still declares it.
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
    /// Independent of [`Self::chunks_per_batch`]: the batch size decides how
    /// often the stage hits a barrier, this decides how fast one batch drains.
    /// Setting it *below* the batch size is the supported way to pace a
    /// rate-limited key or record replay cassettes
    /// (`LLM_MAX_PARALLEL_REQUESTS=1`; see `scripts/perf/README.md`).
    ///
    /// This is the *stage-level* bound and only binds where a caller threads the
    /// setting in — the CLI and bindings do. It is not the last line of defence:
    /// `cognee_llm::in_flight` applies the same ceiling inside the LLM transport,
    /// so a caller that never reads this field (the HTTP cognify routers build
    /// `CognifyConfig::default()`) is still bounded, and an operator's
    /// `LLM_MAX_PARALLEL_REQUESTS` still takes effect there.
    ///
    /// Defaults to [`DEFAULT_MAX_PARALLEL_EXTRACTIONS`], which documents why that
    /// number tracks Python's connection-pool ceiling rather than its batch size.
    ///
    /// # Applies to the LLM path only
    ///
    /// This bounds concurrent *LLM* calls. A run configured with a
    /// [`Self::graph_backend`] does not use it: the backend seam hands the
    /// backend one batch at a time, strictly sequentially, because a backend is
    /// in-process and typically batches internally — an ONNX session or a rule
    /// engine is not helped by caller-side fan-out and can be actively hurt by
    /// it (each concurrent batch is another set of intermediate tensors
    /// resident at once). A backend that wants parallelism owns that decision
    /// inside [`ChunkGraphExtractor::extract_graphs`], where it can size it
    /// against its own runtime. Summarization is unaffected either way.
    pub max_parallel_extractions: usize,

    /// Custom prompt for entity/relationship extraction.
    /// Python parameter: custom_prompt (optional)
    /// If None, uses default prompts from cognee_llm
    pub custom_extraction_prompt: Option<String>,

    /// How the LLM graph extraction builds each chunk's graph: one
    /// structured-output call (the default, Python's `cognify()`), or the
    /// multi-round cascade (Python's `extract_graph_from_data_v2`). Ignored
    /// when a [`Self::graph_backend`] replaces LLM extraction.
    #[serde(default)]
    pub extraction_strategy: ExtractionStrategy,

    /// Enable text summarization stage.
    /// Python behavior: Always runs if summarization_model is set
    /// Default: true (matches Python)
    pub enable_summarization: bool,

    /// Whether to generate and index triplet embeddings.
    /// Triplets are formatted as "source › relationship › target"
    /// Python config: CognifyConfig.triplet_embedding (default: False)
    pub embed_triplets: bool,

    /// Keep every chunk, entity and summary vector in
    /// [`CognifyResult::embeddings`](crate::CognifyResult::embeddings) when
    /// the run ends.
    ///
    /// Off by default: the vectors are already in the vector store, and
    /// holding them in the result kept a full copy of them alive for the whole
    /// of `add_data_points` and after it — memory that grew with the corpus
    /// (SDK-507 / SDK-711). [`CognifyResult::embedding_count`](crate::CognifyResult::embedding_count)
    /// reports how many were generated either way. Turn it on to inspect the
    /// vectors themselves, as the embedding integration tests do.
    pub retain_embeddings: bool,

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

    /// How to count tokens when chunking text. Default `TikToken` (what an
    /// unconfigured, OpenAI-family deployment resolves to); cognee sets it
    /// from `cognify.token_counter` and the embedding provider via
    /// [`TokenCounterKind::resolve`].
    pub token_counter_kind: TokenCounterKind,

    /// Whether a chunk's size is the tokenizer's verdict on the whole span
    /// (the default) or the legacy sum over words counted in isolation.
    ///
    /// Default [`TokenCountMode::Span`]; the
    /// `cognify.legacy_per_word_token_count` setting
    /// (`COGNEE_LEGACY_PER_WORD_TOKEN_COUNT`) opts back in. Per-word counting
    /// over-states a span by ~1.7x on English prose, so it fills chunks to
    /// roughly 60% of the configured budget; it is retained because it is what
    /// Python 1.5.x still does, and is therefore the setting that reproduces
    /// Python's chunk boundaries byte for byte.
    #[serde(default)]
    pub token_count_mode: TokenCountMode,

    /// Axis 1 of failure handling — when a run stops scheduling further work.
    ///
    /// Default [`FailureStop::FailFast`], which with the default
    /// [`Self::rollback_scope`] is Python's behaviour: stop at the first failed
    /// batch, error the run. See [`FailureStop::FailFast`] for what "batch"
    /// means in each stage — it is [`Self::chunks_per_batch`] for graph
    /// extraction, and not a single file.
    #[serde(default)]
    pub failure_stop: FailureStop,

    /// Axis 2 of failure handling — what a failed run removes.
    ///
    /// Default [`RollbackScope::WholeRun`]. Read at the end of the run by
    /// [`crate::rollback`], which turns it into a sweep scope — and, under
    /// [`RollbackScope::FailedItems`], during the run for the abort-time
    /// partition.
    #[serde(default)]
    pub rollback_scope: RollbackScope,

    /// Whether a summarization failure leaves its data item intact.
    ///
    /// Default `false` — Python parity, where a summarization failure is fatal
    /// like any other. Setting it makes summarization failures reported-only:
    /// they never fail the item or the run and never count toward
    /// [`Self::chunk_failure_ratio_threshold`]. Either way they are *collected*
    /// rather than propagated, so the reported list is complete.
    #[serde(default)]
    pub tolerate_summarization_failures: bool,

    /// The share of chunks that may fail before a
    /// [`RollbackScope::FailedItems`] run is called failed.
    ///
    /// Counted in chunks, evaluated per run, summarization excluded. Default
    /// 0.05. Ignored under the other two scopes, which fail on any failed item.
    #[serde(default = "default_chunk_failure_ratio_threshold")]
    pub chunk_failure_ratio_threshold: f64,

    /// How many individual failures the run's failure report lists before it
    /// starts counting them only in the total. Default 100.
    #[serde(default = "default_failure_report_cap")]
    pub failure_report_cap: usize,

    /// Optional JSON Schema for custom graph extraction model.
    ///
    /// When `Some`, the LLM answers this Pydantic-compatible JSON Schema
    /// instead of the default `KnowledgeGraph`, and each answer is stored,
    /// vector-indexed and ownership-ledgered as a runtime DataPoint graph (see
    /// [`crate::dynamic_graph`]). Only the single-pass LLM extractor honours
    /// it, so [`Self::validate`] rejects it together with `graph_backend`,
    /// [`ExtractionStrategy::Cascade`] or `temporal_cognify`, and compiles it
    /// so an invalid schema fails before the pipeline starts.
    ///
    /// Mirrors Python's `graph_model` parameter.
    #[serde(skip)]
    pub graph_schema: Option<serde_json::Value>,

    /// Attachment breadth for custom runtime graph models. `None` has the same
    /// behavior as [`ChunkAttachment::Direct`], matching Python.
    #[serde(skip)]
    pub chunk_attachment: Option<ChunkAttachment>,

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

    /// Optional LLM-free graph backend.
    ///
    /// When `Some`, [`crate::tasks::extract_graph_from_data`] asks this backend
    /// for one [`crate::KnowledgeGraph`] per chunk instead of calling the LLM,
    /// and records a [`crate::graph_backend::ChunkExtractionError`] from any
    /// single chunk as that chunk's failure alone. When the backend also
    /// reports [`ChunkGraphExtractor::summarizes_chunks`], it produces the
    /// chunk summaries too and [`crate::tasks::summarize_text`] makes no LLM
    /// call at all. Chunk filtering, failure policy, edge dedup, expansion and
    /// the graph writes are unchanged.
    ///
    /// This field means "**this run's extraction stage is the backend**", and
    /// [`crate::tasks::extract_graph_from_data`] is the only reader. Setting it
    /// on a pipeline that does not run that stage configures a backend nothing
    /// drives — and if the backend summarizes, it also switches the LLM
    /// summarizer off for a run in which nothing then produces a summary. See
    /// [`crate::tasks::make_summarize_text_task`] for the pairings this
    /// supports.
    #[serde(skip)]
    pub graph_backend: Option<GraphBackendHandle>,
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

/// Opaque wrapper around a [`ChunkGraphExtractor`] implementation.
///
/// Implements [`Debug`] (prints `"GraphBackendHandle(…)"`) and [`Clone`] (cheap
/// `Arc` clone), keeping [`CognifyConfig`] derivable — `Arc<dyn Trait>` is not
/// `Debug` on its own.
#[derive(Clone)]
pub struct GraphBackendHandle(pub Arc<dyn ChunkGraphExtractor>);

impl std::fmt::Debug for GraphBackendHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GraphBackendHandle(…)")
    }
}

/// How graph extraction asks the LLM for each chunk's graph.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtractionStrategy {
    /// One structured-output call per chunk with the default graph prompt
    /// (and its date hints). Python's `cognify()`.
    #[default]
    SinglePass,
    /// Three stages — nodes, relationship names, edge triplets — each run for
    /// `rounds` rounds, so `3 × rounds` calls per chunk. Python's
    /// `extract_graph_from_data_v2` (`n_rounds`, default 2); see
    /// [`crate::fact_extraction::cascade`]. A custom extraction prompt does not
    /// apply: each stage has its own prompts.
    Cascade { rounds: usize },
}

impl ExtractionStrategy {
    /// Parse a configured name: `single_pass` (or empty) and `cascade`,
    /// case-insensitive. `None` for anything else.
    pub fn parse(name: &str, cascade_rounds: usize) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "single_pass" | "single-pass" => Some(Self::SinglePass),
            "cascade" => Some(Self::Cascade {
                rounds: cascade_rounds.max(1),
            }),
            _ => None,
        }
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

/// Serde default for [`CognifyConfig::chunk_failure_ratio_threshold`].
fn default_chunk_failure_ratio_threshold() -> f64 {
    FailurePolicy::DEFAULT_CHUNK_FAILURE_RATIO_THRESHOLD
}

/// Serde default for [`CognifyConfig::failure_report_cap`].
fn default_failure_report_cap() -> usize {
    FailurePolicy::DEFAULT_REPORT_CAP
}

impl Default for CognifyConfig {
    fn default() -> Self {
        Self {
            max_chunk_size: None,
            chunk_overlap: 10,
            chunk_strategy: ChunkStrategy::Paragraph,

            chunks_per_batch: DEFAULT_CHUNKS_PER_BATCH,
            max_parallel_extractions: DEFAULT_MAX_PARALLEL_EXTRACTIONS,
            custom_extraction_prompt: None,
            extraction_strategy: ExtractionStrategy::default(),

            enable_summarization: true,

            embed_triplets: false,
            retain_embeddings: false,
            embedding_batch_size: 100,
            vector_collection_prefix: String::new(),

            incremental_loading: true,

            use_pipeline_cache: false,

            temporal_cognify: false,
            create_web_page_nodes: true,
            data_per_batch: 20,

            // Plain defaults: cognee fills these from the `cognify` config
            // section when it lowers `Settings` (`Settings::cognify_config`).
            token_counter_kind: TokenCounterKind::TikToken,
            token_count_mode: TokenCountMode::Span,
            failure_stop: FailureStop::default(),
            rollback_scope: RollbackScope::default(),
            tolerate_summarization_failures: false,
            chunk_failure_ratio_threshold: default_chunk_failure_ratio_threshold(),
            failure_report_cap: default_failure_report_cap(),

            graph_schema: None,
            chunk_attachment: None,
            summary_schema: None,
            custom_chunker: None,
            transcriber: None,
            graph_backend: None,
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

    /// Set the graph extraction strategy (see [`ExtractionStrategy`]).
    pub fn with_extraction_strategy(mut self, strategy: ExtractionStrategy) -> Self {
        self.extraction_strategy = strategy;
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

    /// Keep the run's vectors in [`CognifyResult::embeddings`](crate::CognifyResult::embeddings);
    /// see [`Self::retain_embeddings`].
    pub fn with_retained_embeddings(mut self, retain: bool) -> Self {
        self.retain_embeddings = retain;
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

    /// Set how chunk sizes are measured during chunking.
    pub fn with_token_count_mode(mut self, mode: TokenCountMode) -> Self {
        self.token_count_mode = mode;
        self
    }

    /// Set a custom JSON Schema for graph extraction.
    pub fn with_graph_schema(mut self, schema: serde_json::Value) -> Self {
        self.graph_schema = Some(schema);
        self
    }

    /// Set how chunks attach to a custom runtime graph.
    pub fn with_chunk_attachment(mut self, attachment: ChunkAttachment) -> Self {
        self.chunk_attachment = Some(attachment);
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

    /// Set an LLM-free graph backend (see [`ChunkGraphExtractor`]).
    pub fn with_graph_backend(mut self, backend: Arc<dyn ChunkGraphExtractor>) -> Self {
        self.graph_backend = Some(GraphBackendHandle(backend));
        self
    }

    /// Set axis 1 — when the run stops scheduling further work.
    pub fn with_failure_stop(mut self, stop: FailureStop) -> Self {
        self.failure_stop = stop;
        self
    }

    /// Set axis 2 — what a failed run removes.
    pub fn with_rollback_scope(mut self, scope: RollbackScope) -> Self {
        self.rollback_scope = scope;
        self
    }

    /// Set whether a summarization failure leaves its data item intact.
    pub fn with_summarization_failure_tolerance(mut self, tolerate: bool) -> Self {
        self.tolerate_summarization_failures = tolerate;
        self
    }

    /// Set the share of chunks a [`RollbackScope::FailedItems`] run tolerates.
    pub fn with_chunk_failure_ratio_threshold(mut self, threshold: f64) -> Self {
        self.chunk_failure_ratio_threshold = threshold;
        self
    }

    /// Set how many failures the run's failure report lists individually.
    pub fn with_failure_report_cap(mut self, cap: usize) -> Self {
        self.failure_report_cap = cap;
        self
    }

    /// The failure-handling policy this configuration resolves to, read once
    /// per run by the stages that collect failures.
    pub fn failure_policy(&self) -> FailurePolicy {
        FailurePolicy {
            stop: self.failure_stop,
            scope: self.rollback_scope,
            tolerate_summarization_failures: self.tolerate_summarization_failures,
            chunk_failure_ratio_threshold: self.chunk_failure_ratio_threshold,
            report_cap: self.failure_report_cap,
        }
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
        let budget = Self::auto_chunk_size(embedding_engine, llm);
        self.max_chunk_size = Some(self.token_counter_kind.fit_token_budget(budget));
        self
    }

    /// Validate configuration parameters.
    ///
    /// Returns an error if any parameters are invalid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chunk_attachment.is_some() && self.graph_schema.is_none() {
            return Err(ConfigError::InvalidParameter(
                "chunk_attachment requires graph_schema".to_string(),
            ));
        }
        // Python raises rather than run an extractor other than the one the
        // caller selected (`cognify.py`: a custom `graph_model` is only
        // supported by the `llm` extractor). The custom-schema branch drives
        // neither a graph backend nor the cascade stages, so either would be
        // silently ignored — and a summarizing backend would also suppress
        // every summary.
        if self.graph_schema.is_some() && self.graph_backend.is_some() {
            return Err(ConfigError::InvalidParameter(
                "graph_schema is only supported by the LLM extractor; unset graph_backend"
                    .to_string(),
            ));
        }
        if self.graph_schema.is_some()
            && matches!(self.extraction_strategy, ExtractionStrategy::Cascade { .. })
        {
            return Err(ConfigError::InvalidParameter(
                "graph_schema is only supported by the single_pass extraction strategy".to_string(),
            ));
        }
        // Python pops `temporal_cognify` and runs its custom-model pipeline
        // regardless; the Rust temporal pipeline never reads a graph schema, so
        // accepting the pair would silently drop the caller's model. Chunk
        // attachment needs `graph_schema` (checked above), so this also
        // rejects chunk attachment under temporal cognify.
        if self.graph_schema.is_some() && self.temporal_cognify {
            return Err(ConfigError::InvalidParameter(
                "graph_schema is not supported with temporal_cognify".to_string(),
            ));
        }
        // Python builds the model class before any task runs. Compiling here
        // fails a bad schema before classify and chunk, and even when every
        // chunk is a DLT row and extraction never runs. The extraction stage
        // compiles again; that costs microseconds and keeps the public
        // `graph_schema` field the single source of truth.
        if let Some(schema) = &self.graph_schema {
            DynamicGraphSchema::compile(schema)?;
        }
        // Only an *explicit* chunk size can be validated here: an auto-calculated
        // one is derived from the engines at pipeline time and is always >= 1.
        if self.max_chunk_size == Some(0) {
            return Err(ConfigError::InvalidParameter(
                "max_chunk_size must be greater than 0".to_string(),
            ));
        }
        // No `chunk_overlap` rule here on purpose. Python validates
        // `chunk_overlap` against a chunk size nowhere; the only such check
        // upstream is langchain's own, inside the opt-in `LangchainChunker`
        // against its own `chunk_size=1024`, and it is `>` not `>=`. The
        // field is inert on both sides (see its doc comment), so the rule
        // failed whole pipelines over a knob that moves no chunk boundary —
        // most easily via an unset `max_chunk_size`, where
        // `with_auto_chunk_size` puts the budget through `fit_token_budget`
        // and a word counter turns 512 into 341.

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

        // Zero rounds would run no cascade stage at all: every chunk would come
        // back as an empty graph, with no LLM call and no error.
        if self.extraction_strategy == (ExtractionStrategy::Cascade { rounds: 0 }) {
            return Err(ConfigError::InvalidParameter(
                "extraction_strategy cascade rounds must be greater than 0".to_string(),
            ));
        }

        if !self.chunk_failure_ratio_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.chunk_failure_ratio_threshold)
        {
            return Err(ConfigError::InvalidParameter(
                "chunk_failure_ratio_threshold must be a finite value between 0.0 and 1.0"
                    .to_string(),
            ));
        }

        if self.failure_report_cap == 0 {
            return Err(ConfigError::InvalidParameter(
                "failure_report_cap must be greater than 0".to_string(),
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
    #[error("Invalid graph schema: {0}")]
    InvalidGraphSchema(#[from] DynamicGraphError),
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
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn zero_cascade_rounds_are_rejected() {
        let config = CognifyConfig::default()
            .with_extraction_strategy(ExtractionStrategy::Cascade { rounds: 0 });
        assert!(config.validate().is_err());
        let config = CognifyConfig::default()
            .with_extraction_strategy(ExtractionStrategy::Cascade { rounds: 1 });
        assert!(config.validate().is_ok());
    }
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
        assert_eq!(config.chunks_per_batch, DEFAULT_CHUNKS_PER_BATCH);
        assert_eq!(
            config.max_parallel_extractions,
            DEFAULT_MAX_PARALLEL_EXTRACTIONS
        );
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

    #[test]
    fn test_config_validation_chunk_failure_ratio_threshold() {
        for threshold in [f64::NAN, 1.5, -0.1, f64::INFINITY] {
            let config = CognifyConfig {
                chunk_failure_ratio_threshold: threshold,
                ..Default::default()
            };
            assert!(
                matches!(config.validate(), Err(ConfigError::InvalidParameter(_))),
                "{threshold} is not a valid ratio"
            );
        }

        for threshold in [0.0, 0.05, 1.0] {
            let config = CognifyConfig {
                chunk_failure_ratio_threshold: threshold,
                ..Default::default()
            };
            assert!(config.validate().is_ok(), "{threshold} is a valid ratio");
        }
    }

    /// A cap of zero would produce a report that lists nothing at all, so it is
    /// rejected rather than silently swallowing every entry.
    #[test]
    fn test_config_validation_zero_failure_report_cap() {
        let config = CognifyConfig {
            failure_report_cap: 0,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidParameter(_))
        ));
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

    /// `chunk_overlap` is never validated against the chunk size, resolved or
    /// not. Python has no such rule anywhere — the only upstream check lives in
    /// the opt-in `LangchainChunker` against its own `chunk_size=1024` — and
    /// nothing in Rust reads the field, so there is nothing to protect.
    #[test]
    fn an_overlap_larger_than_the_chunk_size_is_not_an_error() {
        // Explicit size.
        assert!(
            CognifyConfig {
                max_chunk_size: Some(100),
                chunk_overlap: 100,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        // Auto (unset) size.
        assert!(
            CognifyConfig {
                max_chunk_size: None,
                chunk_overlap: 100_000,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    /// The pipeline-breaking case: a caller leaves `max_chunk_size` unset and
    /// sets `chunk_overlap = 400`. On the local ONNX/BGE path
    /// `with_auto_chunk_size` runs the 512-token budget through
    /// `fit_token_budget`, a word counter divides by 1.5 down to 341, and
    /// `cognify()`'s post-auto-calc re-validate then failed the entire run over
    /// a knob that moves no chunk boundary. It must survive.
    #[test]
    fn a_400_token_overlap_survives_the_auto_calculated_chunk_size() {
        let embed = MockEmbedding { max_seq: 512 };
        let llm = MockLlm::with_ctx(4096);
        // Counter pinned explicitly: once budgets became unit-converted, the
        // expected value depends on which cargo features back the counter.
        let resolved = CognifyConfig {
            max_chunk_size: None,
            chunk_overlap: 400,
            token_counter_kind: TokenCounterKind::Word,
            ..Default::default()
        }
        .with_auto_chunk_size(&embed, &llm);
        // 512 TOKENS converted for a word counter at the pessimistic 1.50
        // tokens/word: 512 * 100 / 150 = 341 — i.e. below the 400 overlap.
        assert_eq!(resolved.max_chunk_size, Some(341));
        assert!(
            resolved.validate().is_ok(),
            "an inert knob must not fail the pipeline"
        );
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
        // Counter pinned so the expectation does not depend on ambient env or
        // on the feature set this test binary was built with.
        let config = CognifyConfig {
            token_counter_kind: TokenCounterKind::Word,
            ..Default::default()
        }
        .with_auto_chunk_size(&embed, &llm);
        // 512 tokens -> 341 words at the pessimistic 1.50 tokens/word.
        assert_eq!(config.max_chunk_size, Some(341));
        // And a BPE counter spends the same budget untouched.
        let bpe = CognifyConfig {
            token_counter_kind: TokenCounterKind::TikToken,
            ..Default::default()
        }
        .with_auto_chunk_size(&embed, &llm);
        // Predicted from the counter itself, not from `cfg!(feature = "tiktoken")`,
        // which inside cognify names COGNIFY's passthrough rather than the feature
        // cognee-chunking compiles the counter behind. The two differ, and reading
        // the wrong one is the cross-crate confusion this bug came from.
        assert_eq!(
            bpe.max_chunk_size,
            Some(TokenCounterKind::TikToken.fit_token_budget(512)),
            "a TikToken request only keeps the token budget when the feature is compiled in",
        );
        // Other fields should remain at defaults
        assert_eq!(config.chunk_overlap, 10);
        assert_eq!(config.chunks_per_batch, DEFAULT_CHUNKS_PER_BATCH);
    }

    /// The in-flight ceiling must stay *reachable*. `scripts/perf/README.md`
    /// documents `LLM_MAX_PARALLEL_REQUESTS=1` as the way to record cassettes on
    /// a rate-limited key, and operators on low-TPM keys depend on it. Raising
    /// the default to a non-binding value must not turn the knob into a no-op.
    #[test]
    fn max_parallel_extractions_can_still_be_lowered() {
        let config = CognifyConfig::default().with_max_parallel_extractions(1);
        assert_eq!(config.max_parallel_extractions, 1);
        assert!(config.validate().is_ok());

        // The batch size stays independent of the concurrency cap.
        assert_eq!(config.chunks_per_batch, DEFAULT_CHUNKS_PER_BATCH);
    }

    /// Both LLM stages must share one in-flight ceiling. If these drift, a
    /// document would be extracted at one width and summarised at another.
    #[test]
    fn summarization_default_parallelism_matches_extraction() {
        assert_eq!(
            crate::summarization::SummaryExtractor::DEFAULT_MAX_PARALLEL,
            CognifyConfig::default().max_parallel_extractions,
        );
    }

    #[test]
    fn graph_schema_requires_the_single_pass_llm_extractor() {
        let schema = serde_json::json!({"title": "Graph", "properties": {}});
        assert!(
            CognifyConfig::default()
                .with_graph_schema(schema.clone())
                .with_graph_backend(Arc::new(
                    crate::graph_backend::MockChunkGraphExtractor::new()
                ))
                .validate()
                .is_err()
        );
        assert!(
            CognifyConfig::default()
                .with_graph_schema(schema.clone())
                .with_extraction_strategy(ExtractionStrategy::Cascade { rounds: 2 })
                .validate()
                .is_err()
        );
        assert!(
            CognifyConfig::default()
                .with_graph_schema(schema)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn chunk_attachment_requires_custom_non_temporal_graph() {
        let message = |config: CognifyConfig| match config.validate() {
            Err(ConfigError::InvalidParameter(message)) => message,
            other => panic!("expected InvalidParameter, got {other:?}"),
        };
        assert_eq!(
            message(CognifyConfig::default().with_chunk_attachment(ChunkAttachment::Direct)),
            "chunk_attachment requires graph_schema",
        );
        // Chunk attachment under temporal cognify is rejected because its
        // required `graph_schema` is.
        assert_eq!(
            message(
                CognifyConfig::default()
                    .with_graph_schema(serde_json::json!({"title": "Graph", "properties": {}}))
                    .with_chunk_attachment(ChunkAttachment::All)
                    .with_temporal_cognify(true)
            ),
            "graph_schema is not supported with temporal_cognify",
        );
        assert!(
            CognifyConfig::default()
                .with_graph_schema(serde_json::json!({"title": "Graph", "properties": {}}))
                .with_chunk_attachment(ChunkAttachment::Direct)
                .validate()
                .is_ok()
        );
    }

    /// The temporal pipeline never reads `graph_schema`, so the pair would
    /// silently drop the caller's model.
    #[test]
    fn graph_schema_is_rejected_with_temporal_cognify() {
        let error = CognifyConfig::default()
            .with_graph_schema(serde_json::json!({"title": "Graph", "properties": {}}))
            .with_temporal_cognify(true)
            .validate()
            .expect_err("graph_schema + temporal_cognify must be rejected");
        assert!(
            matches!(&error, ConfigError::InvalidParameter(message)
                if message.contains("graph_schema") && message.contains("temporal_cognify")),
            "{error}"
        );
    }

    /// Python compares `chunk_attachment` against exactly `"direct"` / `"all"`
    /// and raises for anything else, with this message.
    #[test]
    fn chunk_attachment_parses_python_spellings_only() {
        assert_eq!(
            ChunkAttachment::parse("direct").expect("direct"),
            ChunkAttachment::Direct
        );
        assert_eq!(
            "all".parse::<ChunkAttachment>().expect("all"),
            ChunkAttachment::All
        );
        for bad in ["Direct", "ALL", "", "everything"] {
            let error = ChunkAttachment::parse(bad).expect_err("must be rejected");
            assert!(
                error
                    .to_string()
                    .contains("expected 'direct', 'all', or None"),
                "{error}"
            );
        }
        // The value is quoted with Python's `repr`, as `cognify.py`'s `!r`.
        assert_eq!(
            ChunkAttachment::parse("everything")
                .expect_err("must be rejected")
                .to_string(),
            ConfigError::InvalidParameter(
                "Invalid chunk_attachment 'everything'; expected 'direct', 'all', or None."
                    .to_string()
            )
            .to_string()
        );
        assert_eq!(ChunkAttachment::All.as_str(), "all");
        assert_eq!(ChunkAttachment::Direct.as_str(), "direct");
    }

    #[test]
    fn parse_graph_schema_accepts_objects_and_json_strings() {
        let schema = serde_json::json!({"title": "Graph", "properties": {}});
        assert_eq!(
            parse_graph_schema(&schema).expect("object"),
            Some(schema.clone())
        );
        assert_eq!(
            parse_graph_schema(&serde_json::Value::String(schema.to_string())).expect("string"),
            Some(schema)
        );
        // Omitted and `{}` both mean the default model, as Python's HTTP route.
        assert_eq!(
            parse_graph_schema(&serde_json::Value::Null).expect("null"),
            None
        );
        assert_eq!(
            parse_graph_schema(&serde_json::json!({})).expect("{}"),
            None
        );
        assert_eq!(
            parse_graph_schema(&serde_json::json!("{}")).expect("\"{}\""),
            None
        );
        for bad in [
            serde_json::json!([1]),
            serde_json::json!(3),
            serde_json::json!(true),
            serde_json::json!("not json"),
            serde_json::json!("[1]"),
        ] {
            assert!(
                matches!(
                    parse_graph_schema(&bad),
                    Err(ConfigError::InvalidParameter(_))
                ),
                "{bad} must be rejected"
            );
        }
    }

    /// The schema compiles in `validate()`, so a bad one fails before classify
    /// and chunk run rather than at the extraction stage.
    #[test]
    fn an_invalid_graph_schema_fails_validation() {
        for (schema, expected) in [
            (
                serde_json::json!({"title": "graph", "properties": {}}),
                crate::dynamic_graph::DynamicGraphError::InvalidTitle("graph".to_string()),
            ),
            (
                serde_json::json!({"title": "Graph", "properties": {"id": {"type": "string"}}}),
                crate::dynamic_graph::DynamicGraphError::ReservedField {
                    model: "Graph".to_string(),
                    field: "id".to_string(),
                },
            ),
            (
                serde_json::json!({"title": "Graph"}),
                crate::dynamic_graph::DynamicGraphError::MissingProperties {
                    model: "Graph".to_string(),
                },
            ),
        ] {
            let error = CognifyConfig::default()
                .with_graph_schema(schema)
                .validate()
                .expect_err("an invalid graph schema must fail validation");
            assert!(
                matches!(&error, ConfigError::InvalidGraphSchema(inner) if *inner == expected),
                "{error}"
            );
        }
    }
}
