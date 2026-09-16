//! LLM-free graph extraction backends.
//!
//! A [`ChunkGraphExtractor`] produces one [`KnowledgeGraph`] per document chunk
//! without calling an LLM, and may also produce that chunk's summary from the
//! graph it just extracted. It is injected through
//! [`CognifyConfig::with_graph_backend`](crate::CognifyConfig::with_graph_backend)
//! and consumed inside [`crate::tasks::extract_graph_from_data`]. Everything
//! downstream of extraction — the abort-time partition, DB-aware edge dedup,
//! node/edge expansion, ownership rows, graph writes — is backend-neutral and
//! shared with the LLM path.
//!
//! # Why summaries are produced here and not in `summarize_text`
//!
//! [`crate::tasks::summarize_text`] reads [`crate::tasks::ExtractedChunks`] and
//! runs *concurrently* with extraction inside
//! [`crate::tasks::make_extract_graph_and_summarize_task`]. It therefore cannot
//! see a per-chunk graph, and the two branches cannot negotiate per chunk. A
//! backend that summarizes says so up front via
//! [`ChunkGraphExtractor::summarizes_chunks`]; summarization then produces
//! nothing and makes no LLM call, and the summaries travel with the graphs
//! through [`crate::tasks::ExtractedGraphData::backend_summaries`].
//!
//! The trait lives in `cognee-cognify` rather than its own crate because it
//! speaks [`KnowledgeGraph`] / [`crate::Node`] / [`crate::Edge`], which are
//! defined here.

/// Deterministic in-process backend for tests and examples.
mod mock;

pub use mock::MockChunkGraphExtractor;

use async_trait::async_trait;
use cognee_models::Document;
use cognee_ontology::OntologyResolver;
use thiserror::Error;
use uuid::Uuid;

use crate::fact_extraction::KnowledgeGraph;

/// Errors a [`ChunkGraphExtractor`] can raise.
#[derive(Debug, Error)]
pub enum GraphBackendError {
    /// The backend could not extract graphs for a batch. Recorded per chunk as
    /// a [`crate::failure::StageFailure`] by the extraction seam, exactly as an
    /// LLM extraction failure is.
    #[error("graph backend '{backend}' failed: {message}")]
    Extraction {
        /// [`ChunkGraphExtractor::name`] of the failing backend.
        backend: String,
        /// Backend-specific failure detail.
        message: String,
    },

    /// The backend broke the "exactly one graph per input, in input order"
    /// contract. Raised by the extraction seam, not by backends, and treated as
    /// a hard error rather than a per-chunk failure: it is a defect in the
    /// backend, and zipping a short or long result would attribute graphs to
    /// the wrong chunks with nothing downstream noticing.
    #[error("graph backend '{backend}' returned {got} graphs for {expected} chunks")]
    ArityMismatch {
        /// [`ChunkGraphExtractor::name`] of the offending backend.
        backend: String,
        /// Number of chunks handed to the backend.
        expected: usize,
        /// Number of graphs it returned.
        got: usize,
    },

    /// The backend exists but its runtime or model is unavailable (e.g. a
    /// feature-gated ONNX backend compiled without the feature).
    #[error("graph backend is not available: {0}")]
    NotAvailable(String),
}

/// A borrowed view of one document chunk handed to a backend.
#[derive(Debug, Clone, Copy)]
pub struct ChunkRef<'a> {
    /// `DocumentChunk::base.id`.
    pub chunk_id: Uuid,
    /// `DocumentChunk::document_id` — the data item a failure is charged to.
    pub document_id: Uuid,
    /// The chunk text.
    pub text: &'a str,
}

/// Run-level context shared by every chunk in one extraction call.
#[derive(Clone, Copy)]
pub struct ExtractionContext<'a> {
    /// The classified documents of this run, for per-document schema sketches.
    pub documents: &'a [Document],
    /// The ontology in force, for schema and label derivation.
    pub ontology: &'a dyn OntologyResolver,
    /// The dataset being cognified.
    pub dataset_id: Uuid,
}

impl std::fmt::Debug for ExtractionContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractionContext")
            .field("documents", &self.documents.len())
            .field("dataset_id", &self.dataset_id)
            .finish_non_exhaustive()
    }
}

/// An LLM-free per-chunk knowledge-graph extractor.
#[async_trait]
pub trait ChunkGraphExtractor: Send + Sync {
    /// Short, stable identifier used in logs and as `TextSummary::model`.
    fn name(&self) -> &str;

    /// Extract one graph per chunk.
    ///
    /// # Contract
    /// MUST return exactly `chunks.len()` graphs, in `chunks` order. The seam
    /// checks this and fails the whole stage with
    /// [`GraphBackendError::ArityMismatch`] rather than mis-pairing graphs with
    /// chunks.
    ///
    /// # Errors
    /// [`GraphBackendError::Extraction`] or
    /// [`GraphBackendError::NotAvailable`] when the batch cannot be served. The
    /// seam charges either one to every chunk in the batch as a
    /// [`crate::failure::StageFailure`], exactly as it charges an LLM failure.
    async fn extract_graphs<'c, 'x>(
        &self,
        chunks: &[ChunkRef<'c>],
        ctx: &ExtractionContext<'x>,
    ) -> Result<Vec<KnowledgeGraph>, GraphBackendError>;

    /// Whether this backend takes over chunk summarization entirely.
    ///
    /// `false` (the default) leaves summarization exactly as it is: the LLM
    /// summarizer runs over every non-DLT chunk and [`Self::summarize_chunk`]
    /// is never called.
    ///
    /// `true` means the LLM summarizer is **switched off for the whole run** —
    /// [`crate::tasks::summarize_text`] returns no summaries and makes no LLM
    /// call — and every summary comes from [`Self::summarize_chunk`]. This is
    /// declared up front, not per chunk, because summarization runs
    /// concurrently with extraction and the two branches cannot negotiate mid
    /// run; it is also what Python does, whose
    /// `extract_graph_and_summarize_with_gliner` summarizes every chunk
    /// unconditionally and has no per-chunk LLM fallback either.
    fn summarizes_chunks(&self) -> bool {
        false
    }

    /// Summarize one chunk from the graph just extracted for it.
    ///
    /// Infallible by design: a summary a backend cannot produce is not a reason
    /// to fail a chunk whose graph already succeeded, and there is no per-chunk
    /// fallback to the LLM to hand it to (see [`Self::summarizes_chunks`]).
    ///
    /// Only called when [`Self::summarizes_chunks`] is `true`, which is why the
    /// default body is an empty string rather than a panic — a backend that
    /// leaves `summarizes_chunks` at `false` never reaches it. An empty (or
    /// whitespace-only) return is **not** recorded as a summary, so a backend
    /// that has nothing to say about one chunk simply contributes no
    /// `TextSummary` for it instead of an empty one.
    fn summarize_chunk(&self, _chunk: &ChunkRef<'_>, _graph: &KnowledgeGraph) -> String {
        String::new()
    }
}
