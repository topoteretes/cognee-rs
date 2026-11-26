#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The per-item outcome of a cognify run (SDK-1694 O8).
//!
//! Python's `run_tasks` reports one `data_ingestion_info` entry per data item:
//! `PipelineRunCompleted` for an item this run processed,
//! `PipelineRunAlreadyCompleted` for one the incremental check skipped, and
//! `PipelineRunErrored` for one that failed
//! (`run_tasks_data_item.py:217-301`). `CognifyResult::items`, and on an
//! errored run `CognifyError::item_outcomes`, say the same per item, in input
//! order, so a host can render those entries. Stated as what a caller reads
//! back; offline throughout (see `rollback_harness`).

mod rollback_harness;

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyError, ItemOutcome, ItemStatus, RollbackScope};
use rollback_harness::{
    CountingLlm, Harness, SummarizationFailingLlm, default_batch_extraction_config,
    extraction_config,
};
use uuid::Uuid;

fn failed_items(config: CognifyConfig) -> CognifyConfig {
    config.with_rollback_scope(RollbackScope::FailedItems)
}

fn outcomes(pairs: &[(Uuid, ItemStatus)]) -> Vec<ItemOutcome> {
    pairs
        .iter()
        .map(|&(data_id, status)| ItemOutcome::new(data_id, status))
        .collect()
}

/// The outcome list a failed run carries on its report.
fn errored_outcomes(
    result: Result<cognee_cognify::CognifyResult, CognifyError>,
) -> Vec<ItemOutcome> {
    match result {
        Err(error @ CognifyError::RunFailed { .. }) => error
            .item_outcomes()
            .expect("a RunFailed error carries the outcome")
            .to_vec(),
        other => panic!("expected RunFailed, got {other:?}"),
    }
}

/// A clean run: every item completed, in the order the caller passed them.
#[tokio::test]
async fn a_completed_run_reports_every_item_completed_in_input_order() {
    let mut harness = Harness::new().await;
    let a = harness.add_file("Alice works at Acme.").await;
    let b = harness.add_file("Bob works at Acme.").await;
    let c = harness.add_file("Carol works at Acme.").await;

    // Passed in reverse: the outcome follows the caller's order.
    let items = vec![harness.data(c), harness.data(a), harness.data(b)];
    let result = harness
        .run_with_llm(
            &extraction_config(),
            Arc::new(CountingLlm::lenient(8)),
            items,
        )
        .await
        .expect("the run completes");

    assert_eq!(
        result.items,
        outcomes(&[
            (c, ItemStatus::Completed),
            (a, ItemStatus::Completed),
            (b, ItemStatus::Completed),
        ])
    );
    assert!(result.items.iter().all(|item| item.status.is_complete()));
}

/// A rerun of a complete dataset does nothing, and says so per item: each is
/// already completed (Python's `PipelineRunAlreadyCompleted` entries).
#[tokio::test]
async fn a_rerun_reports_every_item_already_completed() {
    let mut harness = Harness::new().await;
    let a = harness.add_file("Alice works at Acme.").await;
    let b = harness.add_file("Bob works at Acme.").await;
    let config = extraction_config();
    harness.run(&config).await.expect("first run");

    let again = harness.run(&config).await.expect("the rerun completes");
    assert!(again.already_completed);
    assert_eq!(
        again.items,
        outcomes(&[
            (a, ItemStatus::AlreadyCompleted),
            (b, ItemStatus::AlreadyCompleted),
        ])
    );
}

/// A run over a dataset an earlier run partly completed: the old items are
/// already completed, the new one is completed by this run, each in place.
#[tokio::test]
async fn a_mixed_run_tells_skipped_items_from_processed_ones() {
    let mut harness = Harness::new().await;
    let a = harness.add_file("Alice works at Acme.").await;
    let config = extraction_config();
    harness.run(&config).await.expect("first run");
    let b = harness.add_file("Bob works at Acme.").await;
    let c = harness.add_file("Carol works at Acme.").await;

    let llm = Arc::new(CountingLlm::lenient(8));
    let items = vec![harness.data(b), harness.data(a), harness.data(c)];
    let result = harness
        .run_with_llm(&config, llm.clone(), items)
        .await
        .expect("the run completes");

    assert!(!result.already_completed);
    assert_eq!(llm.call_count(), 2, "only the new items reach the LLM");
    assert_eq!(
        result.items,
        outcomes(&[
            (b, ItemStatus::Completed),
            (a, ItemStatus::AlreadyCompleted),
            (c, ItemStatus::Completed),
        ])
    );
}

/// Under `failed_items` a failed item errors the run, which keeps the others.
/// The errored run's outcome says exactly that: the failed item failed, the
/// kept ones completed, and an item an earlier run completed is still
/// reported as such.
#[tokio::test]
async fn a_failed_item_under_failed_items_is_failed_and_the_kept_ones_completed() {
    let mut harness = Harness::new().await;
    let done = harness.add_file("Dave works at Acme.").await;
    let config = failed_items(default_batch_extraction_config());
    harness.run(&config).await.expect("first run");
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;

    let outcome = errored_outcomes(harness.run(&config).await);
    assert_eq!(
        outcome,
        outcomes(&[
            (done, ItemStatus::AlreadyCompleted),
            (first, ItemStatus::Completed),
            (bad, ItemStatus::Failed),
            (last, ItemStatus::Completed),
        ])
    );
    // `Completed` is what the run kept: the markers agree.
    for item in &outcome {
        assert_eq!(
            harness.is_marked(item.data_id).await,
            item.status.is_complete(),
            "{item:?}"
        );
    }
}

/// Fail-fast, one call at a time: the items before the failure are completed
/// and kept, the failing one failed, and the ones after it never reached.
#[tokio::test]
async fn an_unreached_item_under_fail_fast_is_reported_unreached() {
    let mut harness = Harness::new().await;
    let mut ids = Vec::new();
    for index in 0..5 {
        let text = if index == 2 {
            "Bob FAILMARKER breaks here.".to_string()
        } else {
            format!("Person {index} works at Acme.")
        };
        ids.push(harness.add_file(&text).await);
    }

    let outcome = errored_outcomes(harness.run(&failed_items(extraction_config())).await);
    assert_eq!(
        outcome,
        outcomes(&[
            (ids[0], ItemStatus::Completed),
            (ids[1], ItemStatus::Completed),
            (ids[2], ItemStatus::Failed),
            (ids[3], ItemStatus::Unreached),
            (ids[4], ItemStatus::Unreached),
        ])
    );
}

/// Under `whole_run` an errored run keeps nothing: the items it processed are
/// discarded with it, not completed.
#[tokio::test]
async fn an_errored_whole_run_discards_what_it_processed() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;

    let outcome = errored_outcomes(
        harness
            .run(&default_batch_extraction_config().with_rollback_scope(RollbackScope::WholeRun))
            .await,
    );
    assert_eq!(
        outcome,
        outcomes(&[
            (first, ItemStatus::Discarded),
            (bad, ItemStatus::Failed),
            (last, ItemStatus::Discarded),
        ])
    );
    for data_id in [first, bad, last] {
        assert!(!harness.is_marked(data_id).await, "{data_id}: nothing kept");
    }
}

/// An item no loader matches is left unreached by a run that still completes
/// (it fails nothing, and is not marked): the outcome says unreached, not
/// completed.
#[tokio::test]
async fn an_item_no_loader_matches_is_unreached_on_a_completed_run() {
    let mut harness = Harness::new().await;
    let text = harness.add_file("Alice works at Acme.").await;
    let unsupported = harness.add_file("Opaque bytes.").await;
    // An extension no document type maps.
    let mut opaque = harness.data(unsupported);
    opaque.extension = "xyz".into();
    opaque.mime_type = "application/octet-stream".into();

    let result = harness
        .run_with_llm(
            &extraction_config(),
            Arc::new(CountingLlm::lenient(4)),
            vec![harness.data(text), opaque],
        )
        .await
        .expect("an unsupported item fails nothing");

    assert_eq!(
        result.items,
        outcomes(&[
            (text, ItemStatus::Completed),
            (unsupported, ItemStatus::Unreached),
        ])
    );
    assert!(!harness.is_marked(unsupported).await);
}

// ── Probes from the O8 review ────────────────────────────────────────────────

fn statuses(items: &[ItemOutcome]) -> Vec<ItemStatus> {
    items.iter().map(|item| item.status).collect()
}

/// Without incremental loading nothing is skipped: a rerun processes every
/// item again, so both runs report them completed, never already completed.
#[tokio::test]
async fn without_incremental_loading_a_rerun_completes_every_item_again() {
    let mut harness = Harness::new().await;
    harness.add_file("Alice works at Acme.").await;
    harness.add_file("Bob works at Acme.").await;
    let config = extraction_config().with_incremental_loading(false);

    let first = harness.run(&config).await.expect("first run");
    let second = harness.run(&config).await.expect("second run");
    assert_eq!(statuses(&first.items), vec![ItemStatus::Completed; 2]);
    assert!(!second.already_completed);
    assert_eq!(statuses(&second.items), vec![ItemStatus::Completed; 2]);
}

/// Without incremental loading the default scope is `whole_run`: a failure
/// discards the items the run processed.
#[tokio::test]
async fn without_incremental_loading_a_failure_discards_the_run() {
    let mut harness = Harness::new().await;
    harness.add_file("Alice works at Acme.").await;
    harness.add_file("Bob FAILMARKER breaks here.").await;
    harness.add_file("Carol works at Acme.").await;
    let config = default_batch_extraction_config().with_incremental_loading(false);

    let outcome = errored_outcomes(harness.run(&config).await);
    assert_eq!(
        statuses(&outcome),
        vec![
            ItemStatus::Discarded,
            ItemStatus::Failed,
            ItemStatus::Discarded
        ]
    );
}

/// A summarization failure the policy tolerates fails no item: the run is
/// `Ok` and both items are completed.
#[tokio::test]
async fn a_tolerated_summarization_failure_completes_every_item() {
    let mut harness = Harness::new().await;
    let a = harness.add_file("Alice works at Acme.").await;
    let b = harness.add_file("Bob FAILMARKER breaks here.").await;
    let config = CognifyConfig::default()
        .with_chunk_size(1500)
        .with_summarization_failure_tolerance(true);

    let result = harness
        .run_with_llm(
            &config,
            Arc::new(SummarizationFailingLlm),
            vec![harness.data(a), harness.data(b)],
        )
        .await
        .expect("a tolerated failure completes the run");
    assert!(result.failures.total() > 0, "the failure is still reported");
    assert_eq!(statuses(&result.items), vec![ItemStatus::Completed; 2]);
}

/// The same failure, not tolerated, fails its item and errors the run, which
/// keeps the other one.
#[tokio::test]
async fn an_untolerated_summarization_failure_fails_its_item() {
    let mut harness = Harness::new().await;
    let a = harness.add_file("Alice works at Acme.").await;
    let b = harness.add_file("Bob FAILMARKER breaks here.").await;
    let config = CognifyConfig::default()
        .with_chunk_size(1500)
        .with_summarization_failure_tolerance(false);

    let outcome = errored_outcomes(
        harness
            .run_with_llm(
                &config,
                Arc::new(SummarizationFailingLlm),
                vec![harness.data(a), harness.data(b)],
            )
            .await,
    );
    assert_eq!(
        outcome,
        outcomes(&[(a, ItemStatus::Completed), (b, ItemStatus::Failed)])
    );
}
