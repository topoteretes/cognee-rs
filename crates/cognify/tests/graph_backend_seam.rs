#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The LLM-free graph-backend seam, end to end and **offline**.
//!
//! Three claims are pinned here, and each of them is a claim about something
//! *not* happening:
//!
//! 1. With a [`MockChunkGraphExtractor`] configured, extraction produces a full
//!    graph and the LLM is never called. The counter is
//!    [`MockLlm::structured_calls`], which covers every structured-output call —
//!    both `FactExtractor::extract_facts` and `SummaryExtractor` go through it,
//!    so `== 0` is exactly "no extraction and no summarization call was
//!    dispatched". (A future *plain* `generate()` call on these paths would not
//!    be counted; nothing on them makes one today.)
//! 2. A backend that reports `summarizes_chunks()` switches the LLM summarizer
//!    off for the whole run, and its summaries arrive in `SummarizedData` with
//!    the same uuid5 id, the same NodeSet scope and the same `source_task`
//!    stamp an LLM summary would have had.
//! 3. Nothing about the LLM path changed: with `graph_backend: None` the stage
//!    still calls the LLM and still produces the same shapes.
//!
//! Failure semantics get their own coverage because the whole point of routing
//! the backend through the *existing* loop's machinery is that a backend run and
//! an LLM run fail identically: per-chunk `StageFailure`s, `FailFast`, the
//! abort-time partition. Only a broken arity contract is a hard error.
//!
//! Failure comes at two granularities and both are pinned here. A
//! `ChunkExtractionError` in one chunk's result slot fails **that chunk only**
//! (T11) — the granularity the LLM path has always had, and the one that makes
//! the abort-time partition able to preserve anything, since
//! `chunks_per_batch` defaults to 2000 and a realistic run is a single batch.
//! An outer `GraphBackendError` is the whole-batch failure and is charged to
//! every chunk in the batch (T7).

use std::sync::Arc;

use cognee_cognify::graph_backend::MockChunkGraphExtractor;
use cognee_cognify::tasks::{
    ExtractedChunks, SUMMARIZE_TEXT_TASK_NAME, extract_graph_from_data,
    make_extract_graph_and_summarize_task, summarize_text,
};
use cognee_cognify::{
    CognifyConfig, CognifyError, FailureReport, FailureStage, FailureStop, RollbackScope,
};
use cognee_core::TypedTask;
use cognee_database::DatabaseConnection;
use cognee_database::ops::datasets::create_dataset;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::{DataPoint, Dataset, Document, DocumentChunk};
use cognee_ontology::NoOpOntologyResolver;
use cognee_test_utils::{MockLlm, test_task_context};
use serde_json::json;
use uuid::Uuid;

// ── Fixtures ───────────────────────────────────────────────────────────────

fn text_document(doc_id: Uuid) -> Document {
    document_of_type(doc_id, "text")
}

fn document_of_type(doc_id: Uuid, document_type: &str) -> Document {
    let mut base = DataPoint::new("TextDocument", None);
    base.id = doc_id;
    Document {
        base,
        document_type: document_type.to_string(),
        name: "test.txt".to_string(),
        raw_data_location: "file:///tmp/test.txt".to_string(),
        mime_type: "text/plain".to_string(),
        extension: "txt".to_string(),
        data_id: doc_id,
        external_metadata: None,
    }
}

fn chunk(doc_id: Uuid, text: &str) -> DocumentChunk {
    DocumentChunk::new(
        Uuid::new_v4(),
        text.to_string(),
        text.split_whitespace().count(),
        0,
        "paragraph_end".to_string(),
        doc_id,
    )
}

/// One chunk per document, so a per-chunk failure is also a per-file failure.
fn input_from(chunks: Vec<DocumentChunk>, documents: Vec<Document>) -> ExtractedChunks {
    ExtractedChunks {
        chunks,
        documents,
        dataset_id: Uuid::new_v4(),
        user_id: None,
        tenant_id: None,
        failures: FailureReport::default(),
    }
}

/// Two one-chunk files, `Alpha …` and `Beta …`, whose first words are what
/// [`MockChunkGraphExtractor`] derives its node names from.
fn two_file_input() -> ExtractedChunks {
    let doc_a = Uuid::new_v4();
    let doc_b = Uuid::new_v4();
    input_from(
        vec![
            chunk(doc_a, "Alpha owns the first file."),
            chunk(doc_b, "Beta owns the second file."),
        ],
        vec![text_document(doc_a), text_document(doc_b)],
    )
}

/// Web-page node creation off (no `external_metadata` here anyway) and a single
/// batch unless a test says otherwise.
fn config() -> CognifyConfig {
    CognifyConfig::default()
        .with_web_page_nodes(false)
        .with_max_parallel_extractions(1)
}

async fn seeded_db(dataset_id: Uuid) -> Arc<DatabaseConnection> {
    let (_handle, _ctx, db) = test_task_context().await;
    create_dataset(
        &db,
        Dataset::new("graph-backend".into(), Uuid::new_v4(), None, dataset_id),
    )
    .await
    .expect("seed dataset");
    db
}

fn graph_db() -> Arc<dyn GraphDBTrait> {
    Arc::new(MockGraphDB::new())
}

/// A graph response for the LLM path, so the no-backend regression test has
/// something to extract.
fn canned_graph_response() -> String {
    json!({
        "nodes": [
            {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
            {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "works_at",
            "description": "Alice works at Acme."
        }]
    })
    .to_string()
}

fn entity_names(result: &cognee_cognify::tasks::ExtractedGraphData) -> Vec<String> {
    result
        .entities
        .iter()
        .map(|pair| pair.entity.name.to_lowercase())
        .collect()
}

// ── T1 ─────────────────────────────────────────────────────────────────────

/// The headline of the extraction half: a full graph, zero LLM calls, and every
/// downstream step (expansion, dedup, `contains`, the graph writes) unchanged.
#[tokio::test]
async fn backend_extraction_runs_without_the_llm() {
    let input = two_file_input();
    let llm = Arc::new(MockLlm::empty());
    let backend = Arc::new(MockChunkGraphExtractor::new());
    let db = seeded_db(input.dataset_id).await;
    let config = config().with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        llm.clone(),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("the backend path must succeed");

    assert_eq!(
        llm.structured_calls(),
        0,
        "the whole point of the backend is that no structured-output call is made"
    );
    assert_eq!(backend.extract_calls(), 1, "both chunks fit in one batch");
    assert_eq!(backend.chunks_seen(), 2);
    assert_eq!(
        backend.summarize_calls(),
        0,
        "a backend that does not report summarizes_chunks() is never asked to summarize"
    );

    let names = entity_names(&result);
    assert!(
        names.iter().any(|n| n.contains("alpha")),
        "the derived node for chunk 1 must reach the entity list: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("beta")),
        "the derived node for chunk 2 must reach the entity list: {names:?}"
    );

    // `contains` is written from the deduplicated producers map, so a non-empty
    // value proves expansion → dedup → chunk_entity_links all ran unchanged.
    // The length is pinned first so the loop cannot go vacuously green if the
    // stage ever dropped every chunk.
    assert_eq!(result.chunks.len(), 2);
    for chunk in &result.chunks {
        assert!(
            !chunk.contains.is_empty(),
            "every chunk must carry the entity ids extracted from it"
        );
    }
    assert!(result.failures.is_empty());
    assert!(
        result.backend_summaries.is_empty(),
        "this backend does not summarize"
    );
}

// ── T2 ─────────────────────────────────────────────────────────────────────

/// The regression guard: `graph_backend: None` is the untouched LLM path.
#[tokio::test]
async fn no_backend_keeps_the_llm_path() {
    let doc_id = Uuid::new_v4();
    let input = input_from(
        vec![chunk(doc_id, "Alice works at Acme.")],
        vec![text_document(doc_id)],
    );
    let llm = Arc::new(MockLlm::new(vec![canned_graph_response()]));
    let db = seeded_db(input.dataset_id).await;

    let result = extract_graph_from_data(
        &input,
        llm.clone(),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config(),
        None,
        None,
    )
    .await
    .expect("the LLM path must still work");

    assert!(!result.entities.is_empty());
    assert_eq!(
        llm.structured_calls(),
        1,
        "one chunk, one batch — exactly one structured-output call, so a future \
         change that starts issuing extra LLM calls on this path is caught too"
    );
    assert!(
        result.backend_summaries.is_empty(),
        "the new field defaults to empty on the LLM path"
    );
}

// ── T3 ─────────────────────────────────────────────────────────────────────

/// A backend summary must be indistinguishable from an LLM one where it counts:
/// the uuid5 id Python derives, the chunk link, and — the easy thing to miss —
/// the `importance_weight` / `belongs_to_set` that `SummaryExtractor` stamps.
/// Without the latter the summary drops out of every node_name-scoped search.
#[tokio::test]
async fn backend_summary_is_uuid5_and_carries_chunk_scope() {
    let doc_id = Uuid::new_v4();
    let mut only_chunk = chunk(doc_id, "Alpha owns the only file.");
    only_chunk.base.importance_weight = Some(0.9);
    only_chunk.base.belongs_to_set = Some(vec![json!({"name": "my-node-set"})]);
    let chunk_id = only_chunk.base.id;
    let expected_scope = only_chunk.base.belongs_to_set.clone();

    let input = input_from(vec![only_chunk], vec![text_document(doc_id)]);
    let llm = Arc::new(MockLlm::empty());
    let backend = Arc::new(
        MockChunkGraphExtractor::new()
            .with_name("mock-backend")
            .with_summary("canned"),
    );
    let db = seeded_db(input.dataset_id).await;
    let config = config().with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        llm.clone(),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("the backend path must succeed");

    assert_eq!(llm.structured_calls(), 0);
    assert_eq!(result.backend_summaries.len(), 1);
    let summary = &result.backend_summaries[0];
    assert_eq!(
        summary.base.id,
        Uuid::new_v5(&chunk_id, b"TextSummary"),
        "the id must be the same uuid5 Python derives, byte for byte"
    );
    assert_eq!(summary.made_from, Some(chunk_id));
    assert_eq!(summary.source_chunk_id, Some(chunk_id));
    assert_eq!(summary.text, "canned");
    assert_eq!(summary.model, "mock-backend");
    assert_eq!(
        summary.base.importance_weight,
        Some(0.9),
        "parity with SummaryExtractor::summarize_chunks (summarize_text.py:81)"
    );
    assert_eq!(
        summary.base.belongs_to_set, expected_scope,
        "parity with summarize_text.py:79 — without it the summary loses its NodeSet scope"
    );
}

/// A backend that returns an empty summary for a chunk contributes no
/// `TextSummary` for it — an empty summary is not a summary.
#[tokio::test]
async fn an_empty_backend_summary_is_not_recorded() {
    let doc_id = Uuid::new_v4();
    let kept = chunk(doc_id, "Alpha is summarized.");
    let declined = chunk(doc_id, "Beta is not.");
    let declined_id = declined.base.id;
    let kept_id = kept.base.id;

    let input = input_from(vec![kept, declined], vec![text_document(doc_id)]);
    let backend = Arc::new(
        MockChunkGraphExtractor::new()
            .with_summary("canned")
            .declining(declined_id),
    );
    let db = seeded_db(input.dataset_id).await;
    let config = config().with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("a declined summary is not a failure");

    assert_eq!(result.backend_summaries.len(), 1);
    assert_eq!(result.backend_summaries[0].made_from, Some(kept_id));
    assert!(
        result.failures.is_empty(),
        "declining to summarize is not a chunk failure"
    );
}

// ── T4 ─────────────────────────────────────────────────────────────────────

/// The summary seam. The zero is only meaningful next to the non-zero, so both
/// halves run here: the same chunks, the same LLM, one backend that summarizes
/// and one that does not.
#[tokio::test]
async fn summarize_text_skips_the_llm_when_the_backend_summarizes() {
    let input = two_file_input();

    let llm = Arc::new(
        MockLlm::empty().with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    );
    let summarizing = Arc::new(MockChunkGraphExtractor::new().with_summary("canned"));
    let skipped = summarize_text(
        &input,
        llm.clone(),
        &config().with_graph_backend(summarizing),
    )
    .await
    .expect("the early return must be a pure Ok — an Err here tears down the writing branch");

    assert!(skipped.summaries.is_empty());
    assert!(skipped.failures.is_empty());
    assert_eq!(
        llm.structured_calls(),
        0,
        "delegating summarization must cost nothing"
    );

    // Control: the same stage, a backend that does not summarize, and the LLM
    // summarizer runs exactly as before.
    let llm = Arc::new(
        MockLlm::empty().with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    );
    let plain = Arc::new(MockChunkGraphExtractor::new());
    let summarized = summarize_text(&input, llm.clone(), &config().with_graph_backend(plain))
        .await
        .expect("the LLM summarizer must still run");

    assert_eq!(summarized.summaries.len(), 2);
    assert!(
        llm.structured_calls() > 0,
        "so the zero above is a real signal, not an unreachable assertion"
    );
}

// ── T5 ─────────────────────────────────────────────────────────────────────

/// **The headline claim of the stage.** The fused task, driven the way the
/// pipeline drives it: backend summaries reach `SummarizedData.summaries`,
/// carrying the `summarize_text` provenance stamp they would have had on the
/// LLM path — and not one structured-output call is made by either branch.
#[tokio::test]
async fn fused_stage_delivers_backend_summaries() {
    let input = two_file_input();
    let chunk_ids: Vec<Uuid> = input.chunks.iter().map(|c| c.base.id).collect();

    let llm = Arc::new(
        MockLlm::empty().with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    );
    let backend = Arc::new(
        MockChunkGraphExtractor::new()
            .with_name("mock-backend")
            .with_summary("canned"),
    );
    let (_handle, ctx, db) = test_task_context().await;
    create_dataset(
        &db,
        Dataset::new(
            "graph-backend".into(),
            Uuid::new_v4(),
            None,
            input.dataset_id,
        ),
    )
    .await
    .expect("seed dataset");

    let task = make_extract_graph_and_summarize_task(
        llm.clone(),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&db),
        config().with_graph_backend(backend.clone()),
    );
    let TypedTask::Async(run) = task else {
        panic!("the fused stage must be a single async task");
    };
    let out = run(&input, ctx).await.expect("neither branch failed");

    assert_eq!(
        llm.structured_calls(),
        0,
        "both halves must be LLM-free — the extraction branch uses the backend and \
         the summarization branch early-returns"
    );
    assert_eq!(
        out.summaries.len(),
        chunk_ids.len(),
        "one backend summary per chunk must survive the merge"
    );
    assert!(!out.entities.is_empty(), "the graphs survive the merge too");

    for (summary, chunk_id) in out.summaries.iter().zip(&chunk_ids) {
        assert_eq!(summary.base.id, Uuid::new_v5(chunk_id, b"TextSummary"));
        assert_eq!(summary.made_from, Some(*chunk_id));
        assert_eq!(summary.model, "mock-backend");
        assert_eq!(
            summary.base.source_task.as_deref(),
            Some(SUMMARIZE_TEXT_TASK_NAME),
            "backend summaries bypass make_summarize_text_task, so the extraction \
             task body has to stamp them — the provenance parity harness keys off \
             this literal"
        );
        assert_eq!(
            summary.base.topological_rank,
            Some(3),
            "both halves of the fused stage share rank 3"
        );
    }
}

// ── T6 ─────────────────────────────────────────────────────────────────────

/// Invariant I2: a file extraction abandoned must not leave a summary behind.
///
/// File 2's second chunk fails, which fails the whole file and (under
/// `FailFast`) leaves file 3 unreached — so only file 1 survives. File 2's
/// *first* chunk had already produced a backend summary; it must be dropped,
/// which is what `merge_graph_and_summaries`'s surviving-chunk filter does for
/// backend summaries for free.
#[tokio::test]
async fn aborted_files_lose_their_backend_summaries() {
    let doc_one = Uuid::new_v4();
    let doc_two = Uuid::new_v4();
    let doc_three = Uuid::new_v4();
    let survivor = chunk(doc_one, "Alpha survives.");
    let survivor_id = survivor.base.id;
    let input = input_from(
        vec![
            survivor,
            chunk(doc_two, "Beta is summarized then abandoned."),
            chunk(doc_two, "Gamma is the chunk that fails."),
            chunk(doc_three, "Delta is never reached."),
        ],
        vec![
            text_document(doc_one),
            text_document(doc_two),
            text_document(doc_three),
        ],
    );

    let backend = Arc::new(MockChunkGraphExtractor::new().with_summary("canned"));
    // One chunk per batch, so the third call — file 2's second chunk — is the
    // one that fails.
    backend.set_failure_after(2);

    let (_handle, ctx, db) = test_task_context().await;
    create_dataset(
        &db,
        Dataset::new(
            "graph-backend".into(),
            Uuid::new_v4(),
            None,
            input.dataset_id,
        ),
    )
    .await
    .expect("seed dataset");

    let config = config()
        .with_chunks_per_batch(1)
        .with_failure_stop(FailureStop::FailFast)
        .with_rollback_scope(RollbackScope::FailedItems)
        .with_graph_backend(backend.clone());

    let task = make_extract_graph_and_summarize_task(
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&db),
        config,
    );
    let TypedTask::Async(run) = task else {
        panic!("the fused stage must be a single async task");
    };
    let out = run(&input, ctx)
        .await
        .expect("FailedItems collects rather than propagates");

    assert_eq!(
        out.summaries.len(),
        1,
        "file 2's first chunk was summarized before the abort; that summary must not \
         outlive the file the sweep is about to remove"
    );
    assert_eq!(out.summaries[0].made_from, Some(survivor_id));
    assert_eq!(out.chunks.len(), 1);
    assert_eq!(out.chunks[0].base.id, survivor_id);
    assert!(out.failures.failed_items().contains(&doc_two));
    assert!(out.failures.unreached_items().contains(&doc_three));
}

// ── T7 ─────────────────────────────────────────────────────────────────────

/// A **whole-batch** backend failure — the outer `Err`, the "model will not
/// load" shape — is data, not an error: it becomes one `StageFailure` per chunk
/// of the failing batch, charged to that chunk's file, and the stage still
/// returns `Ok`. That is what keeps `RollbackScope`, the chunk-failure ratio and
/// the item-scoped sweep behaving identically to an LLM run.
///
/// Charging the whole batch is correct *here* precisely because the error says
/// the call could not be served at all. A failure that belongs to one chunk
/// must not take this route — see T11.
#[tokio::test]
async fn whole_batch_backend_failure_is_charged_to_every_chunk() {
    let input = two_file_input();
    let chunk_ids: Vec<Uuid> = input.chunks.iter().map(|c| c.base.id).collect();
    let doc_ids: Vec<Uuid> = input.chunks.iter().map(|c| c.document_id).collect();

    let backend = Arc::new(MockChunkGraphExtractor::new());
    backend.set_failure_after(0);
    let db = seeded_db(input.dataset_id).await;
    let config = config()
        .with_failure_stop(FailureStop::RunToEnd)
        .with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("a backend failure must not be returned as an Err");

    assert_eq!(result.failures.entries().len(), 2, "one entry per chunk");
    for (entry, (chunk_id, doc_id)) in result
        .failures
        .entries()
        .iter()
        .zip(chunk_ids.iter().zip(&doc_ids))
    {
        assert_eq!(entry.stage, FailureStage::GraphExtraction);
        assert_eq!(entry.chunk_id, Some(*chunk_id));
        assert_eq!(entry.data_id, *doc_id);
        assert!(entry.fails_item);
        assert!(
            entry.error.contains("graph backend 'mock' failed"),
            "the backend name belongs in the message: {}",
            entry.error
        );
    }
    assert!(result.entities.is_empty());
}

// ── T8 ─────────────────────────────────────────────────────────────────────

/// The arity guard is load-bearing, not defensive: the backend path zips a
/// returned `Vec` positionally onto its batch, so a short result would attach
/// graphs to the wrong chunks and nothing downstream would notice. A broken
/// contract is a backend defect, so unlike an extraction failure it is a hard
/// error — and nothing has been persisted by the time it fires.
#[tokio::test]
async fn backend_arity_mismatch_is_rejected() {
    let input = two_file_input();
    let backend = Arc::new(MockChunkGraphExtractor::new().breaking_arity());
    let db = seeded_db(input.dataset_id).await;
    let config = config().with_graph_backend(backend);
    let graph = Arc::new(MockGraphDB::new());

    let err = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph.clone(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect_err("a broken arity contract must not be silently absorbed");

    match err {
        CognifyError::GraphExtractionError(message) => assert!(
            message.contains("returned 1 graphs for 2 chunks"),
            "the message must name both counts: {message}"
        ),
        other => panic!("expected GraphExtractionError, got {other:?}"),
    }
    assert!(
        graph.node_count() == 0,
        "the stage must fail before any graph write"
    );
}

// ── T9 ─────────────────────────────────────────────────────────────────────

/// DLT chunks are filtered above the branch point, so the backend never sees
/// them — their graph is built deterministically by `extract_dlt_fk_edges`.
#[tokio::test]
async fn dlt_chunks_never_reach_the_backend() {
    let dlt_doc = Uuid::new_v4();
    let text_doc = Uuid::new_v4();
    let text_chunk = chunk(text_doc, "Alpha is real prose.");
    let text_chunk_id = text_chunk.base.id;
    let input = input_from(
        vec![chunk(dlt_doc, "row 1 col a"), text_chunk],
        vec![
            document_of_type(dlt_doc, "dlt_row"),
            text_document(text_doc),
        ],
    );

    let backend = Arc::new(MockChunkGraphExtractor::new().with_summary("canned"));
    let db = seeded_db(input.dataset_id).await;
    let config = config().with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("the backend path must succeed");

    assert_eq!(
        backend.chunks_seen(),
        1,
        "the DLT chunk must be filtered out"
    );
    assert_eq!(
        result.chunks.len(),
        2,
        "the DLT chunk is still carried forward"
    );
    assert_eq!(result.backend_summaries.len(), 1);
    assert_eq!(result.backend_summaries[0].made_from, Some(text_chunk_id));
}

// ── T10 ────────────────────────────────────────────────────────────────────

/// `enable_summarization: false` silences a *summarizing* backend, at both
/// seams and end to end.
///
/// The flag is the caller's "do not produce, embed or index summaries" switch;
/// the LLM summarizer has always honoured it, and consulting only
/// `summarizes_chunks()` on the backend path would hand that caller a
/// `TextSummary` per chunk anyway. The extraction seam is checked through
/// `summarize_calls() == 0` — the backend is not even asked — and the fused
/// stage proves none reaches `SummarizedData`, which is what gets embedded.
#[tokio::test]
async fn disabled_summarization_silences_a_summarizing_backend() {
    let input = two_file_input();
    let backend = Arc::new(
        MockChunkGraphExtractor::new()
            .with_name("mock-backend")
            .with_summary("canned"),
    );
    let config = config()
        .with_graph_backend(backend.clone())
        .with_summarization(false);

    // Seam 1 — the extraction branch.
    let db = seeded_db(input.dataset_id).await;
    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("extraction itself is unaffected by the flag");

    assert!(
        result.backend_summaries.is_empty(),
        "summarization is off, so the backend path must emit no summaries"
    );
    assert_eq!(
        backend.summarize_calls(),
        0,
        "with the flag off the backend must not even be asked to summarize"
    );
    assert!(
        !result.entities.is_empty(),
        "the flag gates summarization only — extraction still runs"
    );

    // Seam 2 — the summarization branch, which must not claim delegation.
    let llm = Arc::new(
        MockLlm::empty().with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    );
    let skipped = summarize_text(&input, llm.clone(), &config)
        .await
        .expect("the disabled path is a pure Ok");
    assert!(skipped.summaries.is_empty());
    assert_eq!(
        llm.structured_calls(),
        0,
        "no LLM summary either — the flag is off"
    );

    // End to end: nothing reaches `SummarizedData`, so nothing is embedded.
    let (_handle, ctx, db) = test_task_context().await;
    create_dataset(
        &db,
        Dataset::new(
            "graph-backend".into(),
            Uuid::new_v4(),
            None,
            input.dataset_id,
        ),
    )
    .await
    .expect("seed dataset");
    let llm = Arc::new(
        MockLlm::empty().with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
    );
    let task = make_extract_graph_and_summarize_task(
        llm,
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&db),
        config,
    );
    let TypedTask::Async(run) = task else {
        panic!("the fused stage must be a single async task");
    };
    let out = run(&input, ctx).await.expect("neither branch failed");
    assert!(
        out.summaries.is_empty(),
        "a summary that survives the merge is a summary that gets embedded and indexed"
    );
    assert!(!out.entities.is_empty(), "the graphs still survive");
}

// ── T11 ────────────────────────────────────────────────────────────────────

/// **The per-chunk granularity.** One chunk fails; its siblings — in the *same
/// batch* — still reach the graph, and only the failing chunk's file is
/// charged.
///
/// This is the review finding the trait signature changed for. `extract_graphs`
/// used to return one `Result` for the whole batch, and
/// `chunks_per_batch` defaults to 2000, so a realistic dataset is a single
/// batch: one transient error marked every document in the run as failed, where
/// the LLM path loses exactly one chunk. The default batch size is deliberately
/// left alone here — that is the point. Three files, one batch, one bad chunk.
#[tokio::test]
async fn one_failing_chunk_does_not_fail_its_batch() {
    let doc_a = Uuid::new_v4();
    let doc_b = Uuid::new_v4();
    let doc_c = Uuid::new_v4();
    let doomed = chunk(doc_b, "Beta is the one bad chunk.");
    let doomed_id = doomed.base.id;
    let input = input_from(
        vec![
            chunk(doc_a, "Alpha survives its sibling's failure."),
            doomed,
            chunk(doc_c, "Gamma survives it too."),
        ],
        vec![
            text_document(doc_a),
            text_document(doc_b),
            text_document(doc_c),
        ],
    );

    let backend = Arc::new(MockChunkGraphExtractor::new().failing_chunk(doomed_id));
    let db = seeded_db(input.dataset_id).await;
    // Default `chunks_per_batch` (2000): all three chunks go out in one call.
    let config = config()
        .with_failure_stop(FailureStop::RunToEnd)
        .with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("a per-chunk failure must not be returned as an Err");

    assert_eq!(
        backend.extract_calls(),
        1,
        "all three chunks must share one batch, or the test is not testing \
         batch-granularity at all"
    );

    assert_eq!(
        result.failures.entries().len(),
        1,
        "exactly one chunk failed, so exactly one StageFailure: {:?}",
        result.failures.entries()
    );
    let entry = &result.failures.entries()[0];
    assert_eq!(entry.stage, FailureStage::GraphExtraction);
    assert_eq!(entry.chunk_id, Some(doomed_id));
    assert_eq!(entry.data_id, doc_b);
    assert!(entry.fails_item);
    assert!(
        entry.error.contains("failed for this chunk"),
        "the per-chunk error must be the one recorded: {}",
        entry.error
    );
    assert_eq!(
        result
            .failures
            .failed_items()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![doc_b],
        "only the failing chunk's file is charged"
    );

    let names = entity_names(&result);
    assert!(
        names.iter().any(|n| n.contains("alpha")),
        "the sibling before the failure must still land in the graph: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("gamma")),
        "the sibling after the failure must still land in the graph: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("beta")),
        "the failing chunk must contribute nothing: {names:?}"
    );
}

// ── T12 ────────────────────────────────────────────────────────────────────

/// The abort-time partition can now actually preserve something.
///
/// Same single batch as T11, under `FailFast` + `FailedItems`: the file owning
/// the bad chunk is dropped and the other two are kept. With batch-granularity
/// failure this partition was unreachable in the common case — every file in
/// the batch was failed, so "complete" was always empty and the three-way split
/// the seam advertises had nothing to split.
#[tokio::test]
async fn the_abort_partition_keeps_the_batch_siblings() {
    let doc_a = Uuid::new_v4();
    let doc_b = Uuid::new_v4();
    let doc_c = Uuid::new_v4();
    let doomed = chunk(doc_b, "Beta is the one bad chunk.");
    let doomed_id = doomed.base.id;
    let input = input_from(
        vec![
            chunk(doc_a, "Alpha survives its sibling's failure."),
            doomed,
            chunk(doc_c, "Gamma survives it too."),
        ],
        vec![
            text_document(doc_a),
            text_document(doc_b),
            text_document(doc_c),
        ],
    );

    let backend = Arc::new(MockChunkGraphExtractor::new().failing_chunk(doomed_id));
    let db = seeded_db(input.dataset_id).await;
    let config = config()
        .with_failure_stop(FailureStop::FailFast)
        .with_rollback_scope(RollbackScope::FailedItems)
        .with_graph_backend(backend.clone());

    let result = extract_graph_from_data(
        &input,
        Arc::new(MockLlm::empty()),
        graph_db(),
        Arc::new(NoOpOntologyResolver::new()),
        &db,
        None,
        &config,
        None,
        None,
    )
    .await
    .expect("FailedItems collects rather than propagates");

    let kept: Vec<Uuid> = result.chunks.iter().map(|c| c.document_id).collect();
    assert_eq!(
        kept.len(),
        2,
        "the two complete files survive the abort: {kept:?}"
    );
    assert!(kept.contains(&doc_a) && kept.contains(&doc_c));
    assert!(!kept.contains(&doc_b), "the failed file is dropped");
    assert!(result.failures.failed_items().contains(&doc_b));

    let names = entity_names(&result);
    assert!(names.iter().any(|n| n.contains("alpha")));
    assert!(names.iter().any(|n| n.contains("gamma")));
}
