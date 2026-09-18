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
//!
//! # Why this ships with no in-tree implementor — and must not be deleted
//!
//! [`ChunkGraphExtractor`] is an **extension point**. It is implemented in this
//! repository only by [`MockChunkGraphExtractor`], and that is deliberate: the
//! trait exists for implementations that live outside this crate, present and
//! future. It is not an unfinished feature, and it is **not dead code** —
//! please do not remove it while tidying up.
//!
//! The gap it closes is a real one in this crate. Graph extraction in
//! [`crate::tasks::extract_graph_from_data`] is otherwise hard-wired to
//! [`crate::FactExtractor`]: one structured-output LLM call per chunk, with no
//! way to substitute anything else. That single hard-wiring is what makes the
//! cognify pipeline impossible to run offline, impossible to run where a
//! data-residency rule forbids sending chunk text to a third party, and
//! expensive at volume — embeddings are the only other network cost and they
//! are far cheaper per chunk. Swapping in a local NER/relation model, an
//! on-device runtime, a rule engine or even a regex pass meant forking
//! `tasks.rs` and re-implementing the abort-time partition, the per-chunk
//! failure accounting, the DB-aware edge dedup, the ownership rows and the
//! graph writes alongside it. This seam removes that fork: only extraction is
//! replaced, and everything after it stays shared and stays tested once.
//!
//! An extension point earns its keep only if it is actually reachable, which is
//! why [`MockChunkGraphExtractor`] is a first-class, exercised implementor
//! rather than a `#[cfg(test)]` fixture: `tests/graph_backend_seam.rs` drives
//! the entire stage — summaries, failures, arity violations, DLT filtering,
//! abort partitions — through it, so the seam cannot silently rot.
//!
//! The distinction worth holding on to is *unreachable* versus *implemented
//! elsewhere*. This crate already carries an example of the first:
//! [`CognifyConfig::custom_chunker`](crate::CognifyConfig::custom_chunker) is a
//! public field with a public builder and **no read site anywhere in the
//! workspace** — configuration that silently does nothing. A seam with a
//! working implementor and an integration suite is the opposite case, and the
//! comment you are reading exists so the two do not get confused.

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
///
/// # Stability
///
/// `#[non_exhaustive]` on the enum, not on its variants: backends construct
/// [`Self::Extraction`] and [`Self::NotAvailable`] by struct literal and must
/// keep being able to, but a `match` outside this crate must not go exhaustive.
/// `Timeout`, `ModelLoad` and `Cancelled` are all foreseeable additions, and
/// each one would otherwise be a breaking change for every downstream `match`.
#[derive(Debug, Error)]
#[non_exhaustive]
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
///
/// # Stability
///
/// `#[non_exhaustive]`: the seam **produces** this and backends **read** it, so
/// in production it is constructed in exactly one place —
/// [`crate::tasks::extract_graph_from_data`]. `chunk_index`, `token_count` and
/// chunk metadata are all plausible additions, and because the type is `Copy` a
/// future non-`Copy` field would be a second break on top of the field
/// addition. Out-of-tree backends still need to build one to unit-test their
/// own [`ChunkGraphExtractor::extract_graphs`], so [`Self::new`] is the
/// supported constructor and stays source-compatible across such an addition.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ChunkRef<'a> {
    /// `DocumentChunk::base.id`.
    pub chunk_id: Uuid,
    /// `DocumentChunk::document_id` — the data item a failure is charged to.
    pub document_id: Uuid,
    /// The chunk text.
    pub text: &'a str,
}

impl<'a> ChunkRef<'a> {
    /// Build a chunk view from its three identifying parts.
    pub fn new(chunk_id: Uuid, document_id: Uuid, text: &'a str) -> Self {
        Self {
            chunk_id,
            document_id,
            text,
        }
    }
}

/// Run-level context shared by every chunk in one extraction call.
///
/// # Stability
///
/// `#[non_exhaustive]`, for the same reason as [`ChunkRef`]: the seam builds it
/// once per call and backends only read it. A cancellation token, the run
/// config and the user/tenant identity are all plausible additions. Use
/// [`Self::new`] to construct one — out-of-tree backends need to for their own
/// tests, and it survives a field being added.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct ExtractionContext<'a> {
    /// The classified documents of this run, for per-document schema sketches.
    pub documents: &'a [Document],
    /// The ontology in force, for schema and label derivation.
    pub ontology: &'a dyn OntologyResolver,
    /// The dataset being cognified.
    pub dataset_id: Uuid,
}

impl<'a> ExtractionContext<'a> {
    /// Build a run context from the documents, ontology and dataset in force.
    pub fn new(
        documents: &'a [Document],
        ontology: &'a dyn OntologyResolver,
        dataset_id: Uuid,
    ) -> Self {
        Self {
            documents,
            ontology,
            dataset_id,
        }
    }
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
///
/// This is the crate's extension point for replacing the hard-wired
/// [`crate::FactExtractor`] call with something local, offline or on-device.
/// [`MockChunkGraphExtractor`] is the only implementor in this repository, by
/// design — see the [module docs](self#why-this-ships-with-no-in-tree-implementor--and-must-not-be-deleted)
/// before concluding it is dead code.
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
