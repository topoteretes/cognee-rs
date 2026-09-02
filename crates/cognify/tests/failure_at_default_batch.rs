#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Failure handling at the batch size production actually runs at.
//!
//! Every other failure and rollback fixture on this branch pins
//! `chunks_per_batch` to 1, so the abort boundary falls between files and the
//! three-way partition — complete / failed / unreached — is observed at a
//! granularity no deployment has. The shipped default is
//! [`DEFAULT_CHUNKS_PER_BATCH`] = 2000 (Python's own default), so any dataset
//! under 2000 chunks reaches graph extraction as *one* batch.
//!
//! That changes the partition qualitatively rather than by degree: the loop
//! collects the whole batch before inspecting any result, so at the moment a
//! `FailFast` abort fires every call has already been dispatched and the
//! unreached set is **empty**. A file that a batch-of-1 run would have skipped
//! entirely is, at the default, a *complete* file — persisted, indexed and
//! marked.
//!
//! `docs/configuration.md` says exactly this ("any dataset under 2000 chunks
//! *is* one batch … on such a dataset it saves nothing"); these tests are what
//! make the claim load-bearing.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::sync::Arc;

use cognee_cognify::config::DEFAULT_CHUNKS_PER_BATCH;
use cognee_cognify::{CognifyError, FailureStop, RollbackScope};
use cognee_database::PipelineRunStatus;
use rollback_harness::{CountingLlm, Harness, default_batch_extraction_config, extraction_config};

/// The fixture every test here shares: three one-chunk files, the *middle* one
/// failing. The third file is the interesting one — it is dispatched after the
/// failing chunk, so whether it survives is decided entirely by the batch size.
async fn three_files_with_a_failing_middle(
    harness: &mut Harness,
) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;
    (first, bad, last)
}

/// The helper must keep tracking the shipped constant: a fixture that silently
/// drifted to some other batch size would test nothing this file claims.
#[test]
fn the_fixture_runs_at_the_shipped_default_batch_size() {
    assert_eq!(
        default_batch_extraction_config().chunks_per_batch,
        DEFAULT_CHUNKS_PER_BATCH,
        "these tests are only meaningful at the batch size production runs at"
    );
    let fixture_chunks = 3;
    assert!(
        default_batch_extraction_config().chunks_per_batch > fixture_chunks,
        "…and only if the fixture's three chunks really are one batch"
    );
}

/// The default pair — `FailFast` + `WholeRun` — at the default batch size.
///
/// End state is the same as the batch-of-1 fixture's: nothing persisted,
/// nothing marked, `ERRORED`, `Err`. What differs is the report and the bill:
/// no file is *unreached*, and every extraction call was paid for.
///
/// Worth being precise about *how* the empty end state is reached, because it
/// is not by sweeping. Under this pair the extraction stage returns the error
/// straight out of the abort branch, before anything is persisted, so the
/// whole-run sweep runs over a run that wrote nothing. "Converges to its
/// pre-run state" is therefore satisfied here by never writing — the sweep
/// itself only has real work when the fatal failure is discovered after
/// persistence (an untolerated summarization failure, say). The batch size
/// changes none of that; what it changes is the two lines below it.
#[tokio::test]
async fn the_default_pair_in_one_batch_reaches_every_file_and_still_converges() {
    let mut harness = Harness::new().await;
    let (first, bad, last) = three_files_with_a_failing_middle(&mut harness).await;

    let counting = Arc::new(CountingLlm::new(8));
    let result = harness
        .run_over(
            &default_batch_extraction_config(),
            &[first, bad, last],
            counting.clone(),
        )
        .await;

    let report = match result {
        Err(CognifyError::RunFailed { report }) => report,
        other => panic!("a sweeping scope that ends errored returns Err, got: {other:?}"),
    };

    // The partition, at the default: one failed file and *no* unreached one.
    assert_eq!(report.failed_items().len(), 1);
    assert!(report.failed_items().contains(&bad));
    assert!(
        report.unreached_items().is_empty(),
        "one batch means nothing is left undispatched — the unreached set only exists \
         when a later batch is skipped: {:?}",
        report.unreached_items()
    );
    assert_eq!(report.total_items(), 3);
    assert_eq!(report.total_chunks(), 3);

    // …and `FailFast` saved nothing, which is the honest cost of the default.
    assert_eq!(
        counting.call_count(),
        3,
        "every chunk's extraction call is dispatched before the batch is inspected, \
         including the file that comes after the failing one"
    );

    // The end state is the whole-run one: the stores are as the run found them.
    assert!(
        harness.graph_node_ids().await.is_empty(),
        "nothing this run produced survives: {:?}",
        harness.graph_node_ids().await
    );
    assert_eq!(harness.vector_point_count().await, 0);
    assert!(harness.ledger_nodes().await.is_empty());
    assert!(harness.ledger_edges().await.is_empty());
    for data_id in [first, bad, last] {
        assert!(
            !harness.is_marked(data_id).await,
            "a failed run marks nothing, so the next run redoes every file"
        );
    }
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);
}

/// `FailFast` + `FailedItems` at the default batch size: the whole dataset is
/// one batch, so *every* file except the failed one is complete — including the
/// one dispatched after it. The run returns `Ok` carrying the failed file.
#[tokio::test]
async fn fail_fast_with_failed_items_in_one_batch_keeps_every_file_but_the_failed_one() {
    let mut harness = Harness::new().await;
    let (first, bad, last) = three_files_with_a_failing_middle(&mut harness).await;

    let config = default_batch_extraction_config()
        .with_failure_stop(FailureStop::FailFast)
        .with_rollback_scope(RollbackScope::FailedItems)
        // One failed chunk in three is 33 %, over the 5 % default; this fixture
        // is about the partition, not about the escalation ratio.
        .with_chunk_failure_ratio_threshold(0.5);

    let counting = Arc::new(CountingLlm::new(8));
    let result = harness
        .run_over(&config, &[first, bad, last], counting.clone())
        .await
        .expect("a tolerated failure below the ratio completes the run, and returns Ok");

    assert_eq!(result.failures.failed_items().len(), 1);
    assert!(result.failures.failed_items().contains(&bad));
    assert!(
        result.failures.unreached_items().is_empty(),
        "nothing is unreached in a single batch: {:?}",
        result.failures.unreached_items()
    );
    assert_eq!(counting.call_count(), 3);

    // The persisted artifacts: both survivors' Document nodes, not the failed
    // file's — and the entities they share are still there, claimed by the
    // survivors.
    let nodes = harness.graph_node_ids().await;
    assert!(
        !nodes.contains(&bad.to_string()),
        "the failed file was never persisted: {nodes:?}"
    );
    assert!(
        nodes.contains(&first.to_string()),
        "the file before the failure is complete: {nodes:?}"
    );
    assert!(
        nodes.contains(&last.to_string()),
        "and so is the one after it — at batch-of-1 this file would have been unreached \
         and dropped: {nodes:?}"
    );

    let owning_data_ids: Vec<_> = harness
        .ledger_nodes()
        .await
        .into_iter()
        .map(|row| row.data_id)
        .collect();
    assert!(!owning_data_ids.contains(&bad));
    assert!(owning_data_ids.contains(&first) && owning_data_ids.contains(&last));

    // The markers: exactly the two complete files, so the next run redoes
    // exactly the failed one.
    assert!(harness.is_marked(first).await);
    assert!(harness.is_marked(last).await);
    assert!(!harness.is_marked(bad).await);
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Completed);
}

/// The two fixtures side by side, on the same three files: the batch size, and
/// nothing else, decides whether the file after the failure is *unreached* or
/// *complete*.
///
/// This is the claim the rest of the suite could not make, because it only ever
/// ran the left-hand column.
#[tokio::test]
async fn the_batch_size_alone_decides_whether_the_later_file_survives() {
    let tolerant = |config: cognee_cognify::CognifyConfig| {
        config
            .with_failure_stop(FailureStop::FailFast)
            .with_rollback_scope(RollbackScope::FailedItems)
            .with_chunk_failure_ratio_threshold(0.5)
    };

    // One chunk per batch: the abort lands between files, so the third one is
    // never dispatched.
    let mut narrow = Harness::new().await;
    let (n_first, n_bad, n_last) = three_files_with_a_failing_middle(&mut narrow).await;
    let narrow_llm = Arc::new(CountingLlm::new(8));
    let narrow_result = narrow
        .run_over(
            &tolerant(extraction_config()),
            &[n_first, n_bad, n_last],
            narrow_llm.clone(),
        )
        .await
        .expect("below the ratio, the narrow run completes too");

    assert_eq!(
        narrow_result.failures.unreached_items(),
        &std::iter::once(n_last).collect(),
        "the file after the failing batch was never dispatched"
    );
    assert_eq!(
        narrow_llm.call_count(),
        2,
        "…which is exactly what a small batch buys: one fewer LLM call"
    );
    assert!(
        !narrow.is_marked(n_last).await,
        "an unreached file is left for the next run, not marked complete"
    );
    assert!(
        !narrow.graph_node_ids().await.contains(&n_last.to_string()),
        "and nothing of it is persisted"
    );

    // The default batch: the same three files are one batch, so the third is
    // complete rather than unreached.
    let mut wide = Harness::new().await;
    let (w_first, w_bad, w_last) = three_files_with_a_failing_middle(&mut wide).await;
    let wide_llm = Arc::new(CountingLlm::new(8));
    let wide_result = wide
        .run_over(
            &tolerant(default_batch_extraction_config()),
            &[w_first, w_bad, w_last],
            wide_llm.clone(),
        )
        .await
        .expect("the default-batch run completes too");

    assert!(wide_result.failures.unreached_items().is_empty());
    assert_eq!(wide_llm.call_count(), 3);
    assert!(
        wide.is_marked(w_last).await,
        "the same file, at the shipped batch size, is complete and marked"
    );
    assert!(wide.graph_node_ids().await.contains(&w_last.to_string()));

    // What both agree on: the failed file itself.
    for (harness, bad) in [(&narrow, n_bad), (&wide, w_bad)] {
        assert!(!harness.is_marked(bad).await);
        assert!(!harness.graph_node_ids().await.contains(&bad.to_string()));
    }
}
