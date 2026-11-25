#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Per-document completion and rollback (SDK-1694 O5b), end to end.
//!
//! Python runs one task chain per data item and stamps an item complete as
//! soon as its own tasks ran; when the run fails, its rollback keeps exactly
//! the items it stamped (`cognify/rollback.py` `_kept_data_ids`). Under
//! `FailedItems` Rust now does the same inside its one pipeline: the document
//! stage's writer completes each document right after saving it, and a run
//! that fails later keeps what it completed. These tests are stated as the
//! end state a caller observes: graph nodes, ledger rows, completion markers,
//! the run's status, and what the next run sends to the LLM.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_cognify::{CognifyConfig, CognifyError, FailureStage, FailureStop, RollbackScope};
use cognee_database::PipelineRunStatus;
use cognee_database::ops::edge_evidence::get_edge_evidence_records;
use cognee_graph::GraphDBTrait;
use rollback_harness::{
    CountingLlm, Harness, SummarizationFailingLlm, default_batch_extraction_config,
    extraction_config,
};
use sea_orm::EntityTrait;
use serde_json::{Value, json};
use uuid::Uuid;

fn failed_items(config: CognifyConfig) -> CognifyConfig {
    config.with_rollback_scope(RollbackScope::FailedItems)
}

/// The data items the node ledger names.
async fn owners(harness: &Harness) -> BTreeSet<Uuid> {
    harness
        .ledger_nodes()
        .await
        .into_iter()
        .map(|row| row.data_id)
        .collect()
}

/// `kept` stayed — Document node, ledger rows, completion marker — and
/// `swept` went, marker included.
async fn assert_kept_and_swept(harness: &Harness, kept: &[Uuid], swept: &[Uuid]) {
    let nodes = harness.graph_node_id_set().await;
    let owners = owners(harness).await;
    for data_id in kept {
        assert!(
            nodes.contains(&data_id.to_string()),
            "{data_id}: a completed document's node stays"
        );
        assert!(owners.contains(data_id), "{data_id}: and its ledger rows");
        assert!(
            harness.is_marked(*data_id).await,
            "{data_id}: and its marker"
        );
    }
    for data_id in swept {
        assert!(
            !nodes.contains(&data_id.to_string()),
            "{data_id}: an unfinished document's node goes"
        );
        assert!(!owners.contains(data_id), "{data_id}: and its ledger rows");
        assert!(!harness.is_marked(*data_id).await, "{data_id}: unmarked");
    }
}

// ── Ticket test 7: failure isolation ───────────────────────────────────────

/// Ticket test 7, the LLM failure: a chunk of document 2 of 3 fails. Under
/// `FailFast` at the shipped in-flight cap every call is in flight at once,
/// so documents 1 and 3 complete, are saved and marked; one failed chunk in
/// three is over the 5 % threshold, so the run errors — and keeps them. The
/// second run sends only document 2 to the LLM.
#[tokio::test]
async fn test_7_an_llm_failure_keeps_the_other_documents_and_the_retry_redoes_only_it() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;
    let config = failed_items(default_batch_extraction_config());

    let result = harness.run(&config).await;
    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "over the ratio the run errors: {result:?}"
    );
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
    assert_kept_and_swept(&harness, &[first, last], &[bad]).await;

    let llm = Arc::new(CountingLlm::lenient(4));
    harness
        .run_over(&config, &[first, bad, last], llm.clone())
        .await
        .expect("the retry completes");
    assert_eq!(
        llm.call_count(),
        1,
        "the retry calls the LLM for the failed document only"
    );
    assert_kept_and_swept(&harness, &[first, bad, last], &[]).await;
}

/// Ticket test 7, the indexing failure: the vector store fails while
/// indexing document 3. Only document 3 fails, at the persistence stage;
/// documents 1 and 2 stay, marked, and the run completes with the failure
/// reported. Before O5b the save failure failed the whole run.
#[tokio::test]
async fn test_7_an_indexing_failure_fails_only_its_document() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Bob works at Acme.").await;
    let third = harness.add_file("Carol works at Acme.").await;
    // The third document's own vector point: its save fails half-way, after
    // its ledger rows and graph nodes were written.
    harness
        .vector_db
        .set_index_error_for_point(third, "the vector store is down for document 3");

    let result = harness
        .run(&failed_items(default_batch_extraction_config()))
        .await
        .expect("a failed save fails its document only; no chunk failed");

    assert_eq!(
        result.failures.failed_items(),
        &BTreeSet::from([third]),
        "{}",
        result.failures
    );
    let entry = &result.failures.entries()[0];
    assert_eq!(entry.stage, FailureStage::Persistence);
    assert!(entry.error.contains("document 3"), "{}", entry.error);
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Completed);
    assert_kept_and_swept(&harness, &[first, second], &[third]).await;
}

// ── Ticket test 13: saved documents survive ────────────────────────────────

/// Ticket test 13, a failed summary: under `FailFast` the first document is
/// complete (and saved) before the second one's summary fails. The run
/// errors and keeps it.
#[tokio::test]
async fn test_13_a_failed_summary_keeps_the_documents_saved_before_it() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;
    let config = failed_items(
        CognifyConfig::default()
            .with_chunk_size(1500)
            .with_max_parallel_extractions(1),
    );

    let result = harness
        .run_over(
            &config,
            &[first, bad, last],
            Arc::new(SummarizationFailingLlm),
        )
        .await;
    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "{result:?}"
    );
    assert_kept_and_swept(&harness, &[first], &[bad, last]).await;
}

/// Ticket test 13, one failed chunk in a ten-chunk run: one call at a time,
/// so the five documents before it are saved and the four after it are
/// never reached. 10 % is over the threshold; the run errors and keeps the
/// five.
#[tokio::test]
async fn test_13_one_failed_chunk_in_ten_keeps_the_documents_saved_before_it() {
    let mut harness = Harness::new().await;
    let mut ids = Vec::new();
    for index in 0..10 {
        let text = if index == 5 {
            "Bob FAILMARKER breaks here.".to_string()
        } else {
            format!("Person {index} works at Acme.")
        };
        ids.push(harness.add_file(&text).await);
    }

    let result = harness.run(&failed_items(extraction_config())).await;
    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "{result:?}"
    );
    assert_kept_and_swept(&harness, &ids[..5], &ids[5..]).await;
}

/// Ticket test 13, a failure in the chunk batch after a document's first
/// chunks: with one chunk per block, document A's three chunks are saved
/// before document B's second chunk fails. A stays; B — whose first chunk did
/// succeed — goes entirely.
#[tokio::test]
async fn test_13_a_failure_in_a_later_chunk_batch_keeps_the_saved_document() {
    let mut harness = Harness::new().await;
    let a = harness
        .add_file("Alice works at Acme.\n\nDana works at Acme.\n\nErin works at Acme.")
        .await;
    let b = harness
        .add_file("Bob works at Acme.\n\nFrank FAILMARKER breaks here.")
        .await;
    let config = failed_items(
        extraction_config()
            .with_chunk_size(8)
            .with_chunks_per_batch(1),
    );

    let result = harness.run(&config).await;
    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "{result:?}"
    );
    let a_chunks = harness
        .ledger_nodes()
        .await
        .into_iter()
        .filter(|row| row.data_id == a && row.node_type == "DocumentChunk")
        .count();
    assert!(a_chunks > 1, "document A spans several chunks: {a_chunks}");
    assert_kept_and_swept(&harness, &[a], &[b]).await;
}

// ── DLT documents ──────────────────────────────────────────────────────────

fn dlt_row_metadata(table: &str, key: &str) -> String {
    json!({
        "source": "dlt",
        "table_name": table,
        "primary_key_value": key,
        "dlt_db_name": "shop",
        "schema_info": [{"name": "id"}],
        "foreign_keys": [],
        "column_values": {},
    })
    .to_string()
}

/// A DLT document is completed only after its foreign-key edges exist: the
/// writer saves it but leaves it to the post-run step. When that step fails,
/// the run errors, the DLT document is swept unmarked, and the text document
/// the writer completed stays.
#[tokio::test]
async fn a_dlt_document_is_marked_only_after_its_foreign_key_edges() {
    let table_node = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"dlt:orders");
    for fk_step_fails in [false, true] {
        let mut harness = Harness::new().await;
        let text = harness.add_file("Alice works at Acme.").await;
        let row = harness
            .add_file_with_metadata(
                "Table: orders\nRow Data:\n  id: 1\n  status: shipped",
                Some(dlt_row_metadata("orders", "1")),
            )
            .await;
        if fk_step_fails {
            harness
                .vector_db
                .set_index_error_for_point(table_node, "the schema index is down");
        }

        let result = harness.run(&failed_items(extraction_config())).await;
        let (_, edges) = harness.graph_db.get_graph_data().await.unwrap();
        let row_of = edges.iter().any(|(_, target, relationship, _)| {
            relationship == "is_row_of" && target == &table_node.to_string()
        });
        if fk_step_fails {
            assert!(result.is_err(), "the post-run step fails the run");
            assert!(!harness.is_marked(row).await, "the DLT row is not marked");
            assert!(
                !owners(&harness).await.contains(&row),
                "and it is swept with what the run did not complete"
            );
            assert_kept_and_swept(&harness, &[text], &[row]).await;
        } else {
            result.expect("the run completes");
            assert!(row_of, "the row's `is_row_of` edge exists");
            assert!(
                harness.is_marked(row).await,
                "the DLT row is marked once its edges are written"
            );
            assert!(harness.is_marked(text).await);
        }
    }
}

// ── Edge evidence ──────────────────────────────────────────────────────────

/// The evidence of the documents an errored run kept stays readable (Python
/// `keep_completed_items_evidence`): lookups hide an errored run's rows, and
/// the next run skips the kept documents, so nothing would record it again.
#[tokio::test]
async fn the_kept_documents_evidence_is_readable_after_an_errored_run() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;

    let result = harness
        .run(&failed_items(default_batch_extraction_config()))
        .await;
    assert!(result.is_err(), "{result:?}");

    let rows = cognee_database::entities::provenance_edge_evidence::Entity::find()
        .all(harness.db.as_ref())
        .await
        .unwrap();
    assert!(!rows.is_empty(), "the kept documents recorded evidence");
    let edge_ids: Vec<Uuid> = rows
        .iter()
        .map(|row| Uuid::parse_str(&row.edge_id).unwrap())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let records = get_edge_evidence_records(&harness.db, harness.dataset_id, &edge_ids, 10, 1000)
        .await
        .unwrap();
    let cited: BTreeSet<Uuid> = records.iter().map(|record| record.data_id).collect();
    assert_eq!(
        cited,
        BTreeSet::from([first, last]),
        "both kept documents' evidence is readable, and only theirs"
    );
    assert!(!cited.contains(&bad));
}

// ── No orphaned ledger rows ────────────────────────────────────────────────

/// Answers each chunk from its text, `Name:Type Name:Type …`: one node per
/// `Name:Type`, an edge between consecutive ones. A summary fails when the
/// chunk holds `FAILSUM`.
struct TypedLlm {
    calls: AtomicUsize,
}

fn user_text(messages: &[cognee_llm::Message]) -> String {
    messages
        .iter()
        .filter(|message| message.role == cognee_llm::MessageRole::User)
        .map(|message| message.content.as_str())
        .collect()
}

#[async_trait::async_trait]
impl cognee_llm::Llm for TypedLlm {
    async fn generate(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        unreachable!("the cognify pipeline only uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<cognee_llm::Message>,
        json_schema: &Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = user_text(&messages);
        if json_schema
            .get("properties")
            .and_then(|properties| properties.get("summary"))
            .is_some()
        {
            if text.contains("FAILSUM") {
                return Err(cognee_llm::LlmError::ApiError(
                    "the summary fails".to_string(),
                ));
            }
            return Ok(json!({"summary": "A summary.", "description": "d"}));
        }
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut previous: Option<String> = None;
        for token in text.split_whitespace() {
            let Some((name, kind)) = token.split_once(':') else {
                continue;
            };
            let kind = kind.trim_end_matches(|c: char| !c.is_alphanumeric());
            let id = name.to_lowercase();
            nodes.push(json!({"id": id, "name": name, "type": kind,
                              "description": format!("{name} as a {kind}.")}));
            if let Some(source) = previous.replace(id.clone()) {
                edges.push(json!({"source_node_id": source, "target_node_id": id,
                                  "relationship_name": "knows"}));
            }
        }
        Ok(json!({"nodes": nodes, "edges": edges}))
    }

    fn model(&self) -> &str {
        "typed"
    }
}

/// Every ledger row still names something: no row of a swept document, every
/// node row's artifact is in the graph, and every nil row — the delete guard
/// of a type, a structural edge, an ontology-first entity — has a document
/// row beside it (nodes by slug, edges by source, target and relationship).
async fn assert_no_orphaned_rows(harness: &Harness, swept: Uuid) {
    let nodes = harness.ledger_nodes().await;
    let edges = harness.ledger_edges().await;
    let graph = harness.graph_node_id_set().await;
    assert!(nodes.iter().all(|row| row.data_id != swept));
    assert!(edges.iter().all(|row| row.data_id != swept));
    for row in &nodes {
        assert!(
            graph.contains(&row.slug.to_string()),
            "a node row whose artifact is gone: {row:?}"
        );
        if row.data_id.is_nil() {
            assert!(
                nodes
                    .iter()
                    .any(|other| other.slug == row.slug && !other.data_id.is_nil()),
                "a nil node row no document row stands beside: {row:?}"
            );
        }
    }
    for row in edges.iter().filter(|row| row.data_id.is_nil()) {
        assert!(
            edges.iter().any(|other| {
                !other.data_id.is_nil()
                    && other.source_node_id == row.source_node_id
                    && other.destination_node_id == row.destination_node_id
                    && other.relationship_name == row.relationship_name
            }),
            "a nil edge row no document row stands beside: {row:?}"
        );
    }
}

/// No ledger row is orphaned after a partial failure. The middle document is
/// saved in full — its summary failure is judged once its results are in,
/// and under `RunToEnd` an incomplete document is still saved — and then
/// swept, both when the run completes (below the threshold) and when it
/// errors (above it). It alone declares the `Robot` and `Alien` types, so
/// their nil rows must go with it.
#[tokio::test]
async fn no_ledger_row_is_orphaned_after_a_partial_failure() {
    for threshold in [0.5, 0.05] {
        let mut harness = Harness::new().await;
        let first = harness.add_file("Alice:Person Acme:Company").await;
        let bad = harness.add_file("Bob:Robot Zed:Alien FAILSUM").await;
        let last = harness.add_file("Carol:Person Acme:Company").await;
        let config = failed_items(
            CognifyConfig::default()
                .with_chunk_size(1500)
                .with_failure_stop(FailureStop::RunToEnd)
                .with_chunk_failure_ratio_threshold(threshold),
        );

        let result = harness
            .run_over(
                &config,
                &[first, bad, last],
                Arc::new(TypedLlm {
                    calls: AtomicUsize::new(0),
                }),
            )
            .await;
        assert_eq!(result.is_ok(), threshold > 0.4, "{result:?}");
        assert_kept_and_swept(&harness, &[first, last], &[bad]).await;
        let robot = cognee_models::EntityType::id_for("robot").to_string();
        assert!(
            !harness.graph_node_id_set().await.contains(&robot),
            "threshold {threshold}: the type only the swept document declared is gone"
        );
        assert_no_orphaned_rows(&harness, bad).await;
    }
}

// ── RollbackScope::Nothing ─────────────────────────────────────────────────

/// A save fails mid-run under `Nothing`: the run errors, the document saved
/// before it keeps its artifacts and gets no marker, the document after it is
/// discarded, and nothing is swept.
#[tokio::test]
async fn under_nothing_a_failed_save_keeps_what_was_saved_and_marks_nothing() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Bob works at Acme.").await;
    let third = harness.add_file("Carol works at Acme.").await;
    harness
        .vector_db
        .set_index_error_for_point(second, "the vector store is down for document 2");

    let result = harness
        .run(&extraction_config().with_rollback_scope(RollbackScope::Nothing))
        .await;
    let error = result.expect_err("a failed save fails the run");
    assert!(error.to_string().contains("document 2"), "{error}");
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);

    let nodes = harness.graph_node_id_set().await;
    let owners = owners(&harness).await;
    assert!(
        nodes.contains(&first.to_string()) && owners.contains(&first),
        "the document saved before the failure keeps its artifacts"
    );
    assert!(
        !nodes.contains(&third.to_string()) && !owners.contains(&third),
        "the document after it is never saved"
    );
    for data_id in [first, second, third] {
        assert!(
            !harness.is_marked(data_id).await,
            "a failed run marks nothing under Nothing"
        );
    }
}
