//! A deterministic, in-process [`ChunkGraphExtractor`] for tests and examples.
//!
//! # Why this is compiled unconditionally
//!
//! Mirroring `cognee_embedding::mock`, and unlike `cognee_graph::mock`, this is
//! not behind `feature = "testing"`. A backend seam nothing outside
//! `#[cfg(test)]` can reach is a seam that rots, and this is the crate's only
//! implementor of [`ChunkGraphExtractor`], so it is also the only worked
//! example a reader has of what implementing one looks like.
//!
//! The `testing` feature is not a free alternative here. This crate's own
//! `tests/graph_backend_seam.rs` drives the whole extraction stage through this
//! mock, and gating the mock leaves only two ways to keep that suite
//! compiling, both worse:
//!
//! 1. Declare `required-features = ["testing"]` on the suite — which takes the
//!    seam's only end-to-end coverage out of a plain
//!    `cargo test -p cognee-cognify`. A test that does not run by default is
//!    not coverage.
//! 2. Turn `testing` on for the whole test build (a self dev-dependency) —
//!    which also arms the six `required-features = ["testing"]` suites this
//!    crate's `Cargo.toml` deliberately keeps off by default.
//!
//! The cost of the present arrangement is one extra public type in release
//! builds of dependents. That is the cheaper side of the trade.
//!
//! Lint note: `unwrap_used` is allowed per function below rather than
//! file-wide, so the guardrail stays on for future edits. Every allowed site is
//! a `Mutex::lock()`; the test module keeps a module-scoped allow.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use super::{
    ChunkExtractionError, ChunkGraphExtractor, ChunkGraphResult, ChunkRef, ExtractionContext,
    GraphBackendError,
};
use crate::fact_extraction::{KnowledgeGraph, Node};

/// A mock graph backend.
///
/// By default it derives a one-node [`KnowledgeGraph`] from each chunk's first
/// word and does **not** summarize. Builders switch on canned graphs
/// ([`Self::with_graphs`]), summarization ([`Self::with_summary`]), an arity
/// violation ([`Self::breaking_arity`]) and failure injection — per chunk
/// ([`Self::failing_chunk`]) or for the whole call
/// ([`Self::set_failure_after`]); the counters record what the seam actually
/// dispatched.
pub struct MockChunkGraphExtractor {
    name: String,
    /// When non-empty, graphs are popped from here in order instead of being
    /// derived from the chunk text.
    canned: Mutex<VecDeque<KnowledgeGraph>>,
    /// `Some` ⇒ [`ChunkGraphExtractor::summarizes_chunks`] is true and this
    /// text is returned for every chunk. `None` ⇒ the backend does not
    /// summarize at all.
    summary: Option<String>,
    /// Chunk ids the backend returns an empty summary for even when `summary`
    /// is set — the seam records no `TextSummary` for those.
    declines: HashSet<Uuid>,
    /// `Some(n)` ⇒ the `n+1`-th `extract_graphs` call and every later one fails
    /// **as a whole batch**, with [`GraphBackendError::Extraction`].
    failure_after: Mutex<Option<usize>>,
    /// Chunk ids the backend returns an `Err(ChunkExtractionError)` for, in
    /// that chunk's own slot — the per-chunk granularity, which leaves the
    /// chunk's siblings untouched.
    failing_chunks: HashSet<Uuid>,
    extract_calls: Mutex<usize>,
    chunks_seen: Mutex<usize>,
    summarize_calls: Mutex<usize>,
    /// Return one graph FEWER than asked for — drives the seam's arity guard.
    break_arity: bool,
}

impl Default for MockChunkGraphExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MockChunkGraphExtractor {
    /// A backend named `"mock"` that derives one node per chunk and does not
    /// summarize.
    pub fn new() -> Self {
        Self {
            name: "mock".to_string(),
            canned: Mutex::new(VecDeque::new()),
            summary: None,
            declines: HashSet::new(),
            failure_after: Mutex::new(None),
            failing_chunks: HashSet::new(),
            extract_calls: Mutex::new(0),
            chunks_seen: Mutex::new(0),
            summarize_calls: Mutex::new(0),
            break_arity: false,
        }
    }

    /// Override [`ChunkGraphExtractor::name`], which is also what every
    /// produced `TextSummary::model` carries.
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Serve these graphs, in order, instead of the derived ones. Once the
    /// queue is exhausted the derived graph is used again, so a short queue
    /// never breaks the arity contract.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    #[must_use]
    pub fn with_graphs(self, graphs: Vec<KnowledgeGraph>) -> Self {
        {
            // lock poison is unrecoverable
            let mut canned = self.canned.lock().unwrap();
            *canned = VecDeque::from(graphs);
        }
        self
    }

    /// Take over summarization: [`ChunkGraphExtractor::summarizes_chunks`]
    /// becomes `true` and every chunk gets this text.
    #[must_use]
    pub fn with_summary(mut self, text: impl Into<String>) -> Self {
        self.summary = Some(text.into());
        self
    }

    /// Return an empty summary for `chunk_id`, so the seam records no
    /// `TextSummary` for that chunk. Only meaningful together with
    /// [`Self::with_summary`].
    #[must_use]
    pub fn declining(mut self, chunk_id: Uuid) -> Self {
        self.declines.insert(chunk_id);
        self
    }

    /// Return one graph fewer than asked for, violating the trait's arity
    /// contract so the seam's guard can be exercised.
    #[must_use]
    pub fn breaking_arity(mut self) -> Self {
        self.break_arity = true;
        self
    }

    /// Fail `chunk_id` **on its own**: the backend still returns a full-length
    /// result vector, with an `Err(`[`ChunkExtractionError`]`)` in this chunk's
    /// slot and a graph in every sibling's. This is the granularity a real
    /// backend should use for a chunk it cannot handle.
    #[must_use]
    pub fn failing_chunk(mut self, chunk_id: Uuid) -> Self {
        self.failing_chunks.insert(chunk_id);
        self
    }

    /// Make the `n+1`-th [`ChunkGraphExtractor::extract_graphs`] call, and
    /// every call after it, fail **as a whole batch** with
    /// [`GraphBackendError::Extraction`] — the "model will not load" shape, not
    /// the "this chunk is bad" one. `set_failure_after(0)` fails the very first
    /// call.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn set_failure_after(&self, n: usize) {
        // lock poison is unrecoverable
        *self.failure_after.lock().unwrap() = Some(n);
    }

    /// How many `extract_graphs` calls (batches) this backend has served.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn extract_calls(&self) -> usize {
        // lock poison is unrecoverable
        *self.extract_calls.lock().unwrap()
    }

    /// How many chunks this backend has been handed across all calls.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn chunks_seen(&self) -> usize {
        // lock poison is unrecoverable
        *self.chunks_seen.lock().unwrap()
    }

    /// How many times `summarize_chunk` has been called.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn summarize_calls(&self) -> usize {
        // lock poison is unrecoverable
        *self.summarize_calls.lock().unwrap()
    }

    /// One node per chunk, named after the chunk's first word — deterministic,
    /// with no uuid and no randomness, so a test can name the entity it expects.
    fn derive(chunk: &ChunkRef<'_>) -> KnowledgeGraph {
        let name = chunk
            .text
            .split_whitespace()
            .next()
            .unwrap_or("mock-entity");
        KnowledgeGraph {
            nodes: vec![Node {
                id: name.to_ascii_lowercase(),
                name: name.to_string(),
                node_type: "MockEntity".to_string(),
                description: chunk.text.chars().take(64).collect(),
            }],
            edges: vec![],
        }
    }
}

#[async_trait]
impl ChunkGraphExtractor for MockChunkGraphExtractor {
    fn name(&self) -> &str {
        &self.name
    }

    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    async fn extract_graphs<'c, 'x>(
        &self,
        chunks: &[ChunkRef<'c>],
        _ctx: &ExtractionContext<'x>,
    ) -> Result<Vec<ChunkGraphResult>, GraphBackendError> {
        let call_index = {
            // lock poison is unrecoverable
            let mut calls = self.extract_calls.lock().unwrap();
            let index = *calls;
            *calls += 1;
            index
        };
        {
            // lock poison is unrecoverable
            *self.chunks_seen.lock().unwrap() += chunks.len();
        }

        // lock poison is unrecoverable
        let failure_after = *self.failure_after.lock().unwrap();
        if failure_after.is_some_and(|threshold| call_index >= threshold) {
            return Err(GraphBackendError::Extraction {
                backend: self.name.clone(),
                message: format!("injected failure on call {}", call_index + 1),
            });
        }

        let take = if self.break_arity {
            chunks.len().saturating_sub(1)
        } else {
            chunks.len()
        };

        let mut graphs = Vec::with_capacity(take);
        for chunk in chunks.iter().take(take) {
            if self.failing_chunks.contains(&chunk.chunk_id) {
                // The slot is still occupied — omitting it would be an arity
                // violation, not a failure report.
                graphs.push(Err(ChunkExtractionError::new(
                    self.name.clone(),
                    format!("injected per-chunk failure for {}", chunk.chunk_id),
                )));
                continue;
            }
            // lock poison is unrecoverable
            let canned = self.canned.lock().unwrap().pop_front();
            graphs.push(Ok(canned.unwrap_or_else(|| Self::derive(chunk))));
        }
        Ok(graphs)
    }

    fn summarizes_chunks(&self) -> bool {
        self.summary.is_some()
    }

    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    fn summarize_chunk(&self, chunk: &ChunkRef<'_>, _graph: &KnowledgeGraph) -> String {
        {
            // lock poison is unrecoverable
            *self.summarize_calls.lock().unwrap() += 1;
        }
        if self.declines.contains(&chunk.chunk_id) {
            return String::new();
        }
        self.summary.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "unit test code — panics are acceptable failures"
    )]

    use super::*;
    use cognee_ontology::NoOpOntologyResolver;

    fn chunk_ref<'a>(text: &'a str, chunk_id: Uuid) -> ChunkRef<'a> {
        ChunkRef {
            chunk_id,
            document_id: Uuid::new_v4(),
            text,
        }
    }

    async fn run<'a>(
        backend: &MockChunkGraphExtractor,
        chunks: &[ChunkRef<'a>],
    ) -> Result<Vec<ChunkGraphResult>, GraphBackendError> {
        let ontology = NoOpOntologyResolver::new();
        let ctx = ExtractionContext {
            documents: &[],
            ontology: &ontology,
            dataset_id: Uuid::new_v4(),
        };
        backend.extract_graphs(chunks, &ctx).await
    }

    #[tokio::test]
    async fn derived_graphs_follow_input_order() {
        let backend = MockChunkGraphExtractor::new();
        let chunks = vec![
            chunk_ref("alpha one", Uuid::new_v4()),
            chunk_ref("beta two", Uuid::new_v4()),
            chunk_ref("gamma three", Uuid::new_v4()),
        ];
        let graphs = run(&backend, &chunks).await.unwrap();
        let names: Vec<String> = graphs
            .iter()
            .map(|g| g.as_ref().unwrap().nodes[0].name.clone())
            .collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
        assert_eq!(backend.extract_calls(), 1);
        assert_eq!(backend.chunks_seen(), 3);
    }

    #[tokio::test]
    async fn canned_graphs_are_popped_in_order_then_fall_back() {
        let canned = |name: &str| KnowledgeGraph {
            nodes: vec![Node {
                id: name.to_string(),
                name: name.to_string(),
                node_type: "Canned".to_string(),
                description: String::new(),
            }],
            edges: vec![],
        };
        let backend =
            MockChunkGraphExtractor::new().with_graphs(vec![canned("first"), canned("second")]);
        let chunks = vec![
            chunk_ref("alpha", Uuid::new_v4()),
            chunk_ref("beta", Uuid::new_v4()),
            chunk_ref("gamma", Uuid::new_v4()),
        ];
        let graphs = run(&backend, &chunks).await.unwrap();
        let names: Vec<String> = graphs
            .iter()
            .map(|g| g.as_ref().unwrap().nodes[0].name.clone())
            .collect();
        // The queue runs out on the third chunk, which falls back to derived —
        // so a short queue never breaks the arity contract.
        assert_eq!(names, ["first", "second", "gamma"]);
    }

    #[tokio::test]
    async fn failure_after_zero_fails_the_first_call() {
        let backend = MockChunkGraphExtractor::new();
        backend.set_failure_after(0);
        let chunks = vec![chunk_ref("alpha", Uuid::new_v4())];
        let err = run(&backend, &chunks).await.unwrap_err();
        assert!(matches!(err, GraphBackendError::Extraction { .. }));
        // The call is still counted: the seam dispatched it.
        assert_eq!(backend.extract_calls(), 1);
    }

    #[tokio::test]
    async fn failure_after_one_lets_the_first_call_through() {
        let backend = MockChunkGraphExtractor::new();
        backend.set_failure_after(1);
        let chunks = vec![chunk_ref("alpha", Uuid::new_v4())];
        assert!(run(&backend, &chunks).await.is_ok());
        assert!(run(&backend, &chunks).await.is_err());
    }

    #[tokio::test]
    async fn a_failing_chunk_does_not_take_its_siblings_down() {
        let doomed = Uuid::new_v4();
        let backend = MockChunkGraphExtractor::new().failing_chunk(doomed);
        let chunks = vec![
            chunk_ref("alpha", Uuid::new_v4()),
            chunk_ref("beta", doomed),
            chunk_ref("gamma", Uuid::new_v4()),
        ];
        let results = run(&backend, &chunks).await.unwrap();
        // The failed chunk keeps its slot, so the arity contract still holds.
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].as_ref().unwrap().nodes[0].name, "alpha");
        assert!(results[1].is_err());
        assert_eq!(results[2].as_ref().unwrap().nodes[0].name, "gamma");
        let err = results[1].as_ref().unwrap_err();
        assert_eq!(err.backend, "mock");
        assert!(err.message.contains(&doomed.to_string()));
    }

    #[tokio::test]
    async fn breaking_arity_returns_one_graph_fewer() {
        let backend = MockChunkGraphExtractor::new().breaking_arity();
        let chunks = vec![
            chunk_ref("alpha", Uuid::new_v4()),
            chunk_ref("beta", Uuid::new_v4()),
        ];
        assert_eq!(run(&backend, &chunks).await.unwrap().len(), 1);
    }

    #[test]
    fn summarization_is_off_by_default_and_opt_in() {
        let plain = MockChunkGraphExtractor::new();
        assert!(!plain.summarizes_chunks());
        assert_eq!(plain.name(), "mock");

        let chunk_id = Uuid::new_v4();
        let declined = Uuid::new_v4();
        let summarizing = MockChunkGraphExtractor::new()
            .with_name("mock-backend")
            .with_summary("canned summary")
            .declining(declined);
        assert!(summarizing.summarizes_chunks());
        assert_eq!(summarizing.name(), "mock-backend");

        let graph = KnowledgeGraph {
            nodes: vec![],
            edges: vec![],
        };
        assert_eq!(
            summarizing.summarize_chunk(&chunk_ref("alpha", chunk_id), &graph),
            "canned summary"
        );
        assert_eq!(
            summarizing.summarize_chunk(&chunk_ref("beta", declined), &graph),
            ""
        );
        assert_eq!(summarizing.summarize_calls(), 2);
    }
}
