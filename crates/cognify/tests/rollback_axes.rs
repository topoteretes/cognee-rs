#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The two axes take effect: what a finished run actually removes.
//!
//! Steps 1–5 built the parts — ownership rows that name their run, a sweep that
//! takes a scope, stages that collect failures instead of throwing them. This
//! is where run orchestration reads the axes and decides, so these tests are
//! stated as end states a caller can observe rather than as calls into the
//! machinery:
//!
//! * a failed run converges to its pre-run state (invariant I1);
//! * a tolerant run completes with only its failed files removed and listed
//!   (invariant I2);
//! * an artifact a surviving file also produced stays, ownership row and all.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::sync::Arc;

use cognee_cognify::{CognifyError, RollbackScope};
use rollback_harness::{Harness, extraction_config};

use cognee_database::PipelineRunStatus;
use cognee_vector::VectorDB;

/// The default pair — `FailFast` + `WholeRun` — leaves the stores exactly as it
/// found them. Python's default in both execution and end state.
#[tokio::test]
async fn a_failed_default_run_converges_to_its_pre_run_state() {
    let mut harness = Harness::new().await;
    let good = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let unreached = harness.add_file("Carol also works at Acme.").await;

    let result = harness.run(&extraction_config()).await;

    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "a sweeping scope that ends errored returns Err, matching Python's re-raise: {result:?}"
    );
    assert!(
        harness.graph_node_ids().await.is_empty(),
        "every node this run wrote is gone: {:?}",
        harness.graph_node_ids().await
    );
    assert_eq!(
        harness.vector_point_count().await,
        0,
        "and so is every vector point"
    );
    assert!(
        harness.ledger_nodes().await.is_empty(),
        "the ownership rows go last, but they do go"
    );
    assert!(harness.ledger_edges().await.is_empty());

    for data_id in [good, bad, unreached] {
        assert!(
            !harness.is_marked(data_id).await,
            "no item may be marked complete after a failed run — the next run redoes them all"
        );
    }
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// A run that failed before persisting anything still ends clean, and the
/// sweep's own "selected nothing" is not mistaken for a problem: the caller
/// gets the pipeline's error, not a sweep error.
#[tokio::test]
async fn a_failed_run_that_never_persisted_is_still_a_clean_no_op() {
    let mut harness = Harness::new().await;
    // Every file fails, so extraction never reaches the persistence stage and
    // no ownership row is ever written.
    harness.add_file("Alice FAILMARKER one.").await;
    harness.add_file("Bob FAILMARKER two.").await;

    let result = harness.run(&extraction_config()).await;

    assert!(matches!(result, Err(CognifyError::RunFailed { .. })));
    assert!(harness.ledger_nodes().await.is_empty());
    assert!(harness.graph_node_ids().await.is_empty());
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// The headline of the tolerant combination: the run completes, the failed
/// file's contributions go, the survivors' stay and are marked — and the
/// entities *both* produced survive, because a surviving file still claims
/// them.
#[tokio::test]
async fn a_tolerant_run_sweeps_only_the_failed_file_and_marks_the_rest() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;

    let config = extraction_config()
        .with_failure_stop(cognee_cognify::FailureStop::RunToEnd)
        .with_rollback_scope(RollbackScope::FailedItems)
        .with_chunk_failure_ratio_threshold(0.5);
    let result = harness
        .run(&config)
        .await
        .expect("a tolerated failure below the ratio completes the run, and returns Ok");

    // The SDK return contract: Ok, *carrying* the failed-file list.
    assert_eq!(result.failures.failed_items().len(), 1);
    assert!(result.failures.failed_items().contains(&bad));

    let nodes = harness.graph_node_ids().await;
    assert!(
        !nodes.contains(&bad.to_string()),
        "the failed file's Document node is gone: {nodes:?}"
    );
    assert!(
        nodes.contains(&first.to_string()) && nodes.contains(&last.to_string()),
        "both survivors' Document nodes stay: {nodes:?}"
    );
    let survivor_docs = [first.to_string(), last.to_string()];
    assert!(
        nodes
            .iter()
            .any(|id| !survivor_docs.contains(id) && id != &bad.to_string()),
        "the chunks and entities the survivors produced — including the ones the failed file \
         also produced — are still there: {nodes:?}"
    );

    let owning_data_ids: Vec<_> = harness
        .ledger_nodes()
        .await
        .into_iter()
        .map(|row| row.data_id)
        .collect();
    assert!(
        !owning_data_ids.contains(&bad),
        "the failed file's ownership rows are gone"
    );
    assert!(
        owning_data_ids.contains(&first) && owning_data_ids.contains(&last),
        "the survivors' are not"
    );

    assert!(harness.is_marked(first).await);
    assert!(harness.is_marked(last).await);
    assert!(
        !harness.is_marked(bad).await,
        "the failed file stays unmarked, so the next run redoes exactly it"
    );
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Completed);
}

/// A `FailedItems` run at the **default** 5 % threshold escalates: one bad file
/// out of three is 33 % of the chunks, so the run errors and sweeps everything.
/// The escalation, end to end.
#[tokio::test]
async fn an_escalating_tolerant_run_sweeps_everything() {
    let mut harness = Harness::new().await;
    let good = harness.add_file("Alice works at Acme.").await;
    harness.add_file("Bob FAILMARKER breaks here.").await;
    harness.add_file("Carol also works at Acme.").await;

    let config = extraction_config()
        .with_failure_stop(cognee_cognify::FailureStop::RunToEnd)
        .with_rollback_scope(RollbackScope::FailedItems);
    let result = harness.run(&config).await;

    assert!(
        matches!(result, Err(CognifyError::RunFailed { .. })),
        "over the ratio, a FailedItems run errors — and an errored run returns Err"
    );
    assert!(
        harness.graph_node_ids().await.is_empty(),
        "…and sweeps as thoroughly as a WholeRun failure would"
    );
    assert!(harness.ledger_nodes().await.is_empty());
    assert!(!harness.is_marked(good).await);
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// The escape hatch, pinned so nobody "fixes" it: `Nothing` leaves the run's
/// artifacts and their ownership rows exactly where they are.
///
/// `RunToEnd` so the run actually persists something before it fails —
/// `FailFast` aborts before the persistence stage, and "nothing was deleted"
/// is not much of an assertion when nothing was written.
#[tokio::test]
async fn the_nothing_scope_leaves_everything_where_it_is() {
    let mut harness = Harness::new().await;
    harness.add_file("Alice works at Acme.").await;
    harness.add_file("Bob FAILMARKER breaks here.").await;

    let config = extraction_config()
        .with_failure_stop(cognee_cognify::FailureStop::RunToEnd)
        .with_rollback_scope(RollbackScope::Nothing);
    let result = harness.run(&config).await;

    assert!(matches!(result, Err(CognifyError::RunFailed { .. })));
    assert!(
        !harness.graph_node_ids().await.is_empty(),
        "today's behaviour, deliberately preserved"
    );
    assert!(
        !harness.ledger_nodes().await.is_empty(),
        "the artifacts keep their ownership rows, so a later repair tool can still find them"
    );
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// A sweep that fails must never replace the pipeline error that triggered it.
/// The caller asked about the run, not about the cleanup.
#[tokio::test]
async fn a_sweep_never_replaces_the_pipeline_error() {
    let mut harness = Harness::new().await;
    harness.add_file("Alice works at Acme.").await;
    harness.add_file("Bob FAILMARKER breaks here.").await;

    // Nothing in the run itself deletes nodes, so arming this up front trips
    // only the sweep. `RunToEnd` so the run persists the good file first and
    // the sweep therefore has real work to fail at.
    let graph_db = Arc::clone(&harness.graph_db);
    graph_db.set_delete_nodes_error("simulated graph store outage");

    let config = extraction_config().with_failure_stop(cognee_cognify::FailureStop::RunToEnd);
    let result = harness.run(&config).await;

    match result {
        Err(CognifyError::RunFailed { report }) => {
            assert_eq!(
                report.failed_items().len(),
                1,
                "the original report survives intact"
            );
        }
        other => panic!("expected the pipeline's own RunFailed, got: {other:?}"),
    }
    assert!(
        !harness.ledger_nodes().await.is_empty(),
        "artifacts first, ownership rows second: a failed artifact delete leaves the rows as \
         the record of what still needs sweeping, so re-running converges"
    );
}

/// A persistence failure is fatal under every combination — a store that
/// cannot be written cannot be reasoned about — and the run is swept, not left
/// half-written.
#[tokio::test]
async fn a_persistence_failure_is_fatal_and_swept() {
    let mut harness = Harness::new().await;
    let only = harness.add_file("Alice works at Acme.").await;

    harness
        .graph_db
        .set_add_edges_error("simulated edge outage");

    let result = harness.run(&extraction_config()).await;

    assert!(result.is_err(), "a persistence failure is always fatal");
    assert!(
        harness.ledger_nodes().await.is_empty() && harness.ledger_edges().await.is_empty(),
        "the ownership rows written *before* the failing artifact write are swept — which is \
         exactly what step 3's inversion made possible"
    );
    assert!(!harness.is_marked(only).await);
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// Invariant I1's "surplus artifact keeps an owner", observed from the caller's
/// side: a whole-run sweep must not touch what an *earlier* run still claims.
#[tokio::test]
async fn a_run_that_shares_an_entity_with_an_earlier_run_keeps_it() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;

    // Run A: one clean file.
    let run_a = harness
        .run_over(
            &extraction_config(),
            &[first],
            Arc::new(cognee_test_utils::MockLlm::new(vec![
                rollback_harness::canned_graph_response();
                4
            ])),
        )
        .await
        .expect("run A completes");
    let run_a_id = run_a.pipeline_run_id.expect("run A recorded its run id");
    let nodes_after_a = harness.graph_node_id_set().await;
    assert!(!nodes_after_a.is_empty());
    let rows_after_a = harness.ledger_nodes().await.len();

    // Run B: a second file producing the *same* entities, and failing.
    let second = harness.add_file("Bob FAILMARKER breaks here.").await;
    let third = harness.add_file("Carol also works at Acme.").await;
    let result = harness
        .run_over(
            &extraction_config(),
            &[second, third],
            Arc::new(
                cognee_test_utils::MockLlm::new(vec![rollback_harness::canned_graph_response(); 6])
                    .with_failing_markers(vec![rollback_harness::FAIL_MARKER.to_string()]),
            ),
        )
        .await;
    assert!(result.is_err());

    let nodes_after_b = harness.graph_node_id_set().await;
    assert!(
        nodes_after_a.is_subset(&nodes_after_b),
        "run A still claims every node it wrote — including the entities run B also produced — \
         so run B's whole-run sweep leaves them alone. missing: {:?}",
        nodes_after_a.difference(&nodes_after_b).collect::<Vec<_>>()
    );
    let rows: Vec<_> = harness.ledger_nodes().await;
    assert_eq!(
        rows.len(),
        rows_after_a,
        "run B's ownership rows are gone; run A's are all that remain"
    );
    assert!(
        rows.iter().all(|row| row.pipeline_run_id == Some(run_a_id)),
        "…and every remaining row belongs to run A. Shared `EntityType` rows carry the nil data \
         id, so the run id is what identifies their owner: {rows:?}"
    );
    assert!(
        harness.is_marked(first).await,
        "run A's completion marker survives a later run's failure"
    );
}

/// The case where an item-scoped sweep has real work to do: an untolerated
/// summarization failure is judged *after* `add_data_points` has already
/// persisted everything the item produced, vectors included. Both stores must
/// give the failed item's contributions back.
#[tokio::test]
async fn an_untolerated_summarization_failure_is_swept_item_scoped() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;

    // Summarization on, and fatal — the default. The failing chunk's *graph*
    // extraction still succeeds, so the item is fully persisted before the
    // policy calls it failed.
    let config = cognee_cognify::CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_failure_stop(cognee_cognify::FailureStop::RunToEnd)
        .with_rollback_scope(RollbackScope::FailedItems)
        .with_chunk_failure_ratio_threshold(0.5);
    let result = harness
        .run_over(
            &config,
            &[first, bad, last],
            Arc::new(rollback_harness::SummarizationFailingLlm),
        )
        .await
        .expect("one tolerated-by-ratio summarization failure still completes the run");

    assert_eq!(result.failures.summarization_failures(), 1);
    assert!(result.failures.failed_items().contains(&bad));

    let nodes = harness.graph_node_ids().await;
    assert!(
        !nodes.contains(&bad.to_string()),
        "the failed item's Document node goes, even though it was fully written: {nodes:?}"
    );
    assert!(nodes.contains(&first.to_string()) && nodes.contains(&last.to_string()));

    assert_eq!(
        harness
            .vector_db
            .collection_size("TextSummary", "text")
            .await
            .expect("collection size"),
        2,
        "the two survivors' summaries stay; the failed item's vector points go"
    );
    assert!(harness.is_marked(first).await && harness.is_marked(last).await);
    assert!(!harness.is_marked(bad).await);
}
