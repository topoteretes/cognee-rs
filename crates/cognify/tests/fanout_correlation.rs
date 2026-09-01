#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Correlation and liveness coverage for the three fan-out loops in
//! [`cognee_cognify::tasks`] — **offline**, no LLM key, no skip path.
//!
//! Each of `extract_graph_from_data`, `extract_custom_graph_from_data` and
//! `extract_temporal_events` fans a batch of chunks out over a bounded stream
//! and then puts the results back against their chunks. Two properties have to
//! hold and neither was covered before:
//!
//! 1. **Correlation.** A result must land against the chunk that produced it.
//!    The loops used to rely on the stream yielding in input order; they now
//!    carry an index through the future and re-sort. A positional zip against
//!    completion order would attach an extracted graph to the wrong chunk, and
//!    nothing downstream would notice — the graph is well-formed, just
//!    misattributed.
//!
//! 2. **Liveness above `max_parallel`.** The loops used `.buffered`, whose
//!    `FuturesOrdered` counts *completed-but-undrained* outputs against its
//!    limit: a future that finishes out of order keeps occupying its slot until
//!    the consumer drains it in order. One chunk stuck in the LLM retry cascade
//!    therefore pinned its own slot *and* every slot filled behind it, and no
//!    further chunk was dispatched until it returned — head-of-line blocking
//!    that only appears once a batch exceeds `max_parallel` chunks.
//!
//! [`Gate`] pins both at once. It holds chunk 0's response until every *other*
//! chunk in the batch has been served, which forces completion to happen out of
//! order (so a positional zip mis-correlates and the assertions below catch it)
//! and can only be satisfied at all if chunks past `max_parallel` are dispatched
//! while chunk 0 is still in flight. Under `.buffered` the gate is never
//! released and the test hangs, so each case runs under a `tokio::time::timeout`
//! and reports the stall as a failure rather than blocking the suite forever.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cognee_cognify::tasks::{
    ExtractedChunks, extract_custom_graph_from_data, extract_graph_from_data,
    extract_temporal_events,
};
use cognee_cognify::{CognifyConfig, fact_extraction::GraphModel};
use cognee_database::ops::datasets::create_dataset;
use cognee_llm::error::{LlmError, LlmResult};
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, MessageRole};
use cognee_models::{Dataset, DocumentChunk};
use cognee_ontology::NoOpOntologyResolver;
use cognee_test_utils::MockGraphDB;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

/// Chunks per batch. Deliberately more than [`MAX_PARALLEL`] so the stream has
/// to refill its slots mid-batch — the case `.buffered` cannot serve.
const CHUNKS: usize = 6;

/// Concurrency ceiling the fan-out loops are configured with.
const MAX_PARALLEL: usize = 2;

/// How long a case may run before its stall is reported as a failure.
///
/// A passing run finishes in milliseconds (nothing here sleeps); this only has
/// to be long enough that a loaded CI machine is not mistaken for a deadlock.
const STALL_TIMEOUT: Duration = Duration::from_secs(20);

/// The text of chunk `index`, carrying the marker the mock LLM answers on.
fn chunk_text(index: usize) -> String {
    format!("CHUNK-{index:04} is a sentence about a marker.")
}

/// The marker index encoded in a prompt by [`chunk_text`].
fn marker_index(text: &str) -> Option<usize> {
    let start = text.find("CHUNK-")? + "CHUNK-".len();
    text.get(start..start + 4)?.parse().ok()
}

/// Forces out-of-order completion, and refuses to be satisfied at all unless the
/// stream keeps dispatching past its concurrency limit.
///
/// Chunk 0 — the first future the stream polls — parks until every other chunk
/// has been served. With `buffer_unordered` those chunks free their slots as
/// they finish, chunks past `MAX_PARALLEL` get dispatched, the counter reaches
/// `CHUNKS - 1` and chunk 0 is released; it then completes *last*, so results
/// arrive in an order that no positional zip could correlate. With `.buffered`
/// the finished-but-undrained outputs hold their slots behind chunk 0, nothing
/// past `MAX_PARALLEL` is ever dispatched, and the counter stops short forever.
struct Gate {
    served_others: watch::Sender<usize>,
}

impl Gate {
    fn new() -> Self {
        Self {
            served_others: watch::channel(0usize).0,
        }
    }

    /// Serve `index`, blocking chunk 0 until the rest have been through.
    async fn pass(&self, index: usize) {
        if index == 0 {
            let mut rx = self.served_others.subscribe();
            // `borrow_and_update` copies out and drops its guard before the
            // await, so the watch lock is never held across a suspension point.
            while *rx.borrow_and_update() < CHUNKS - 1 {
                rx.changed()
                    .await
                    .expect("the sender is owned by this Gate, which outlives every request");
            }
        } else {
            self.served_others.send_modify(|served| *served += 1);
        }
    }
}

/// An LLM that answers structured-output calls from the marker in the prompt,
/// through a [`Gate`].
///
/// The response is derived from the *chunk's own text*, which is what makes the
/// assertions a correlation check rather than a shape check: a result attached
/// to the wrong chunk carries the wrong marker and is caught.
struct MarkerLlm {
    gate: Gate,
    respond: Box<dyn Fn(usize) -> Value + Send + Sync>,
}

impl MarkerLlm {
    fn new(respond: impl Fn(usize) -> Value + Send + Sync + 'static) -> Self {
        Self {
            gate: Gate::new(),
            respond: Box::new(respond),
        }
    }
}

#[async_trait]
impl Llm for MarkerLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported(
            "MarkerLlm only serves structured output".to_string(),
        ))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let system = messages
            .iter()
            .find(|m| matches!(m.role, MessageRole::System))
            .map(|m| m.content.as_str())
            .unwrap_or_default();

        // The temporal stage runs one aggregate enrichment call per batch after
        // its fan-out has drained. It is not gated and not indexed: returning an
        // empty list leaves every event untouched and in order, which is what
        // the correlation assertion downstream reads.
        if system.contains("entities from events") {
            return Ok(json!({ "events": [] }));
        }

        let user = messages
            .iter()
            .find(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        let index = marker_index(user).ok_or_else(|| {
            LlmError::InvalidResponse(format!("no CHUNK-nnnn marker in prompt: {user}"))
        })?;

        self.gate.pass(index).await;
        Ok((self.respond)(index))
    }

    fn model(&self) -> &str {
        "marker-llm"
    }
}

/// `CHUNKS` chunks, each its own document so a per-chunk result can also be
/// checked against a per-chunk *file*.
fn make_input() -> (ExtractedChunks, Vec<Uuid>, Vec<Uuid>) {
    let mut chunks = Vec::with_capacity(CHUNKS);
    let mut chunk_ids = Vec::with_capacity(CHUNKS);
    let mut document_ids = Vec::with_capacity(CHUNKS);

    for index in 0..CHUNKS {
        let document_id = Uuid::new_v4();
        let text = chunk_text(index);
        let chunk = DocumentChunk::new(
            Uuid::new_v4(),
            text.clone(),
            text.split_whitespace().count(),
            index,
            "paragraph_end".to_string(),
            document_id,
        );
        chunk_ids.push(chunk.base.id);
        document_ids.push(document_id);
        chunks.push(chunk);
    }

    let input = ExtractedChunks {
        chunks,
        // No Documents: nothing here is a DLT row, and web-page node creation
        // is switched off in `config()` below.
        documents: vec![],
        dataset_id: Uuid::new_v4(),
        user_id: None,
        tenant_id: None,
        failures: Default::default(),
    };

    (input, chunk_ids, document_ids)
}

/// One batch, fanned out `MAX_PARALLEL` at a time.
fn config() -> CognifyConfig {
    let mut config = CognifyConfig::default().with_web_page_nodes(false);
    config.chunks_per_batch = CHUNKS;
    config.data_per_batch = CHUNKS;
    config.max_parallel_extractions = MAX_PARALLEL;
    config
}

// ── Loop 1: extract_graph_from_data ────────────────────────────────────────

/// The knowledge graph the mock returns for chunk `index`: one node whose name
/// encodes the chunk it came from.
fn marker_graph(index: usize) -> Value {
    json!({
        "nodes": [{
            "id": format!("marker-{index:04}"),
            "name": format!("MARKER-{index:04}"),
            "type": "Marker",
            "description": "A marker entity.",
        }],
        "edges": [],
    })
}

#[tokio::test]
async fn graph_extraction_attributes_each_graph_to_its_own_chunk() {
    let llm: Arc<dyn Llm> = Arc::new(MarkerLlm::new(marker_graph));
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let (input, chunk_ids, _) = make_input();

    let (_handle, _ctx, db) = cognee_test_utils::test_task_context().await;
    create_dataset(
        &db,
        Dataset::new("fanout".into(), Uuid::new_v4(), None, input.dataset_id),
    )
    .await
    .expect("seed dataset");

    let result = tokio::time::timeout(
        STALL_TIMEOUT,
        extract_graph_from_data(
            &input,
            llm,
            graph_db,
            Arc::new(NoOpOntologyResolver::new()),
            &db,
            None,
            &config(),
            None,
            None,
        ),
    )
    .await
    .expect(
        "the fan-out stalled: chunk 0 was never released, so chunks past \
         max_parallel were never dispatched — the `.buffered` head-of-line block",
    )
    .expect("extraction should succeed");

    assert_eq!(
        result.entities.len(),
        CHUNKS,
        "one marker entity per chunk, none merged"
    );

    for (index, chunk_id) in chunk_ids.iter().enumerate() {
        let expected = format!("MARKER-{index:04}");
        let pair = result
            .entities
            .iter()
            .find(|pair| pair.entity.name == expected)
            .unwrap_or_else(|| panic!("no entity named {expected} in the output"));
        assert_eq!(
            result.producers.entity_chunks(pair.entity.base.id),
            [*chunk_id],
            "{expected} must be attributed to chunk {index}, the chunk whose text \
             produced it — a result correlated by stream position instead of by \
             carried index lands on whichever chunk finished in that slot",
        );
    }
}

// ── Loop 2: extract_custom_graph_from_data ─────────────────────────────────

/// A custom (non-`KnowledgeGraph`) model, stored verbatim in
/// [`DocumentChunk::contains`] rather than expanded into graph nodes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct MarkerModel {
    marker: String,
}

impl GraphModel for MarkerModel {}

#[tokio::test]
async fn custom_graph_extraction_writes_each_model_to_its_own_chunk() {
    let llm: Arc<dyn Llm> = Arc::new(MarkerLlm::new(
        |index| json!({ "marker": format!("MARKER-{index:04}") }),
    ));
    let (input, chunk_ids, _) = make_input();

    let result = tokio::time::timeout(
        STALL_TIMEOUT,
        extract_custom_graph_from_data::<MarkerModel>(&input, llm, &config()),
    )
    .await
    .expect(
        "the fan-out stalled: chunk 0 was never released, so chunks past \
         max_parallel were never dispatched — the `.buffered` head-of-line block",
    )
    .expect("extraction should succeed");

    assert_eq!(result.chunks.len(), CHUNKS);
    for (index, chunk_id) in chunk_ids.iter().enumerate() {
        let chunk = result
            .chunks
            .iter()
            .find(|c| c.base.id == *chunk_id)
            .unwrap_or_else(|| panic!("chunk {index} missing from the output"));
        assert_eq!(
            chunk.contains,
            vec![json!({ "marker": format!("MARKER-{index:04}") })],
            "chunk {index} must carry the model extracted from its own text; this \
             loop indexes results back onto `batch_indices`, so a completion-order \
             zip writes each model onto the wrong chunk",
        );
    }
}

// ── Loop 3: extract_temporal_events ────────────────────────────────────────

/// The event list the mock returns for chunk `index`: one event named after it.
fn marker_events(index: usize) -> Value {
    json!({
        "events": [{
            "name": format!("EVENT-{index:04}"),
            "description": format!("Something happened in chunk {index}."),
            "time_from": { "year": 1900 + index },
            "time_to": null,
            "location": null,
        }],
    })
}

#[tokio::test]
async fn temporal_extraction_attributes_each_event_to_its_own_file() {
    let llm: Arc<dyn Llm> = Arc::new(MarkerLlm::new(marker_events));
    let (input, _, document_ids) = make_input();

    let result = tokio::time::timeout(
        STALL_TIMEOUT,
        extract_temporal_events(&input, llm, &config()),
    )
    .await
    .expect(
        "the fan-out stalled: chunk 0 was never released, so chunks past \
         max_parallel were never dispatched — the `.buffered` head-of-line block",
    )
    .expect("extraction should succeed");

    assert_eq!(result.events.len(), CHUNKS, "one event per chunk");

    for (index, document_id) in document_ids.iter().enumerate() {
        let attributed = &result.events[index];
        assert_eq!(
            attributed.event.name,
            format!("EVENT-{index:04}"),
            "`all_events` must stay in input order — the batch is re-sorted by \
             carried index precisely so this stays deterministic",
        );
        assert_eq!(
            attributed.data_id, *document_id,
            "event {index} must be attributed to the file of the chunk that \
             produced it; the data ids are zipped onto the results positionally, \
             so completion order would misattribute every event",
        );
    }
}
