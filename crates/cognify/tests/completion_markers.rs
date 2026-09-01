#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Completion markers become live: `incremental_loading` finally means
//! something.
//!
//! A dormant `Data.pipeline_status` column has existed since the schema was
//! ported, with two clearers and no writer. Run orchestration now writes it
//! after a successful outcome and reads it at entry, which makes
//! re-cognifying an already-complete dataset a **no-op** — a behaviour change
//! for every existing deployment, because the configuration has claimed
//! incremental loading was on all along.
//!
//! These tests pin both halves: what gets marked, and what a marker makes the
//! next run skip.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::sync::Arc;

use cognee_cognify::{FailureStop, RollbackScope};
use cognee_test_utils::MockLlm;
use rollback_harness::{
    CountingLlm, Harness, TemporalFixtureLlm, canned_graph_response, extraction_config,
    temporal_config,
};

/// A clean LLM: canned graph responses, no failing markers.
fn clean_llm(files: usize) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::new(vec![canned_graph_response(); files * 2 + 4]))
}

/// The same, but failing on [`rollback_harness::FAIL_MARKER`].
fn failing_llm(files: usize) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(
        MockLlm::new(vec![canned_graph_response(); files * 2 + 4])
            .with_failing_markers(vec![rollback_harness::FAIL_MARKER.to_string()]),
    )
}

#[tokio::test]
async fn a_successful_run_marks_every_item_it_processed() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Carol also works at Acme.").await;

    harness
        .run_over(&extraction_config(), &[first, second], clean_llm(2))
        .await
        .expect("a clean run completes");

    assert!(harness.is_marked(first).await);
    assert!(harness.is_marked(second).await);
}

/// **The headline behaviour change.** A second cognify over an
/// already-complete dataset does nothing at all: no classification, no
/// chunking, and — the part that costs money — no LLM call.
#[tokio::test]
async fn re_cognifying_a_complete_dataset_is_a_no_op() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Carol also works at Acme.").await;
    let items = [first, second];

    harness
        .run_over(&extraction_config(), &items, clean_llm(2))
        .await
        .expect("the first run completes");
    let nodes_after_first = harness.graph_node_id_set().await;
    assert!(!nodes_after_first.is_empty());

    let counting = Arc::new(CountingLlm::new(8));
    let second_run = harness
        .run_over(&extraction_config(), &items, counting.clone())
        .await
        .expect("the second run is a no-op, not an error");

    assert!(
        second_run.already_completed,
        "the caller is told the run had nothing to do — the CLI and every binding \
         already render this field"
    );
    assert!(second_run.chunks.is_empty());
    assert!(second_run.entities.is_empty());
    assert_eq!(
        counting.call_count(),
        0,
        "not one LLM call: the items are filtered out before the pipeline is built"
    );
    assert_eq!(
        harness.graph_node_id_set().await,
        nodes_after_first,
        "and the graph is untouched"
    );
}

/// The `integration_repeat_cognify` scenario under the new rule: a second wave
/// carrying one old file and one new one processes only the new one.
#[tokio::test]
async fn a_second_wave_processes_only_the_new_file() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;

    harness
        .run_over(&extraction_config(), &[first], clean_llm(1))
        .await
        .expect("wave 1 completes");

    let second = harness.add_file("Carol also works at Acme.").await;
    let result = harness
        .run_over(&extraction_config(), &[first, second], clean_llm(2))
        .await
        .expect("wave 2 completes");

    assert!(
        !result.already_completed,
        "there was real work to do, so this is not a no-op"
    );
    assert_eq!(result.chunks.len(), 1, "only the new file was chunked");
    assert!(
        result
            .chunks
            .iter()
            .all(|chunk| chunk.document_id == second),
        "…and the chunk belongs to the new file"
    );
    assert!(harness.is_marked(first).await && harness.is_marked(second).await);
}

/// The escape hatch for the behaviour change: with incremental loading off,
/// the second run reprocesses everything, exactly as it did before this
/// commit.
#[tokio::test]
async fn incremental_loading_off_reprocesses_everything() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Carol also works at Acme.").await;
    let items = [first, second];
    let config = extraction_config().with_incremental_loading(false);

    harness
        .run_over(&config, &items, clean_llm(2))
        .await
        .expect("the first run completes");
    let result = harness
        .run_over(&config, &items, clean_llm(2))
        .await
        .expect("the second run completes");

    assert!(!result.already_completed);
    assert_eq!(result.chunks.len(), 2, "both files are chunked again");
    assert!(
        !harness.is_marked(first).await && !harness.is_marked(second).await,
        "no marker is written either, so turning the flag back on does not skip these files"
    );
}

/// A failed run marks nothing, so the next run redoes everything it was
/// responsible for. Without this, invariant I2 would be violated in the worst
/// possible direction: a swept item that still looks complete is skipped
/// forever.
#[tokio::test]
async fn markers_are_not_written_for_a_failed_run() {
    let mut harness = Harness::new().await;
    let good = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;

    harness
        .run(&extraction_config())
        .await
        .expect_err("the default pair errors on the first failure");

    assert!(!harness.is_marked(good).await);
    assert!(!harness.is_marked(bad).await);
}

/// A later run's failure does not unmark — or undo — what an earlier run
/// completed.
///
/// Ownership rows are content-addressed, so re-cognifying an unchanged item
/// writes no new row: the earlier run stays its owner, the failing run's
/// whole-run sweep selects nothing of it, and its marker and artifacts both
/// survive. That is the correct outcome — nothing of the item was deleted, so
/// skipping it next time is not over-skipping.
///
/// The sweep's own marker clearing is pinned where it lives: `cognee-delete`'s
/// sweep tests, and the writer/clearer round trip in
/// `cognee-database`'s `pipeline_status_key_compat`.
#[tokio::test]
async fn an_earlier_runs_marker_survives_a_later_runs_failure() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;

    harness
        .run_over(&extraction_config(), &[first], clean_llm(1))
        .await
        .expect("run 1 completes and marks the file");
    assert!(harness.is_marked(first).await);
    let nodes_after_first = harness.graph_node_id_set().await;

    // Run 2 reprocesses the same file (incremental loading off) alongside one
    // that fails, and is swept.
    let second = harness.add_file("Bob FAILMARKER breaks here.").await;
    let config = extraction_config()
        .with_incremental_loading(false)
        .with_failure_stop(FailureStop::RunToEnd);
    harness
        .run_over(&config, &[first, second], failing_llm(2))
        .await
        .expect_err("run 2 fails");

    assert!(
        harness.is_marked(first).await,
        "run 1 still owns this item's artifacts, so its marker stays honest"
    );
    assert!(
        nodes_after_first.is_subset(&harness.graph_node_id_set().await),
        "…and so do the artifacts themselves"
    );
    assert!(
        !harness.is_marked(second).await,
        "the file run 2 failed on is left unmarked, so the next run redoes it"
    );

    // The next run therefore has exactly one file's worth of work.
    let result = harness
        .run_over(&extraction_config(), &[first, second], clean_llm(2))
        .await
        .expect("run 3 completes");
    assert_eq!(result.chunks.len(), 1);
    assert!(
        result
            .chunks
            .iter()
            .all(|chunk| chunk.document_id == second)
    );
}

/// A run that completed with files still outstanding is not a pipeline-cache
/// hit. The cache would otherwise skip the dataset entirely and the failed
/// file would never be redone.
#[tokio::test]
async fn the_pipeline_cache_does_not_short_circuit_a_run_with_outstanding_failures() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;
    let items = [first, bad, last];

    let tolerant = extraction_config()
        .with_pipeline_cache(true)
        .with_failure_stop(FailureStop::RunToEnd)
        .with_rollback_scope(RollbackScope::FailedItems)
        .with_chunk_failure_ratio_threshold(0.5);
    let first_run = harness
        .run(&tolerant)
        .await
        .expect("the tolerant run completes below the ratio");
    assert_eq!(first_run.failures.failed_items().len(), 1);

    // Same items, cache on. The dataset's latest run says COMPLETED, but it
    // also says a file is outstanding — so this must run, and must process
    // exactly the file that failed.
    let second_run = harness
        .run_over(&tolerant, &items, clean_llm(3))
        .await
        .expect("the second run completes");

    assert!(
        !second_run.already_completed,
        "a completed-with-failures run is not a cache hit"
    );
    assert_eq!(
        second_run.chunks.len(),
        1,
        "only the previously-failed file is redone"
    );
    assert!(
        second_run
            .chunks
            .iter()
            .all(|chunk| chunk.document_id == bad)
    );
    assert!(harness.is_marked(bad).await);
}

/// Temporal marks and skips exactly like a standard run.
///
/// This reverses what the previous commit pinned, deliberately. Python's
/// temporal cognify runs under `pipeline_name="cognify_pipeline"` — the same
/// string as its standard branch, one `pipeline_executor_func` call for both —
/// so Python writes and reads one set of markers on both branches. Rust now
/// matches, which is also what makes the sweep's marker-clearing phase
/// meaningful for a temporal run instead of a guaranteed no-op.
///
/// The user-visible consequence, which the release notes state: a `cognify()`
/// followed by a temporal `cognify()` over the same dataset is a no-op, and the
/// other way round. Callers who want both graphs over one dataset set
/// `with_incremental_loading(false)`.
#[tokio::test]
async fn a_temporal_run_marks_and_skips_like_a_standard_one() {
    let mut harness = Harness::new().await;
    let only = harness.add_file("Alice joined Acme in 2020.").await;

    let config = temporal_config();
    harness
        .run_over(&config, &[only], Arc::new(TemporalFixtureLlm::new()))
        .await
        .expect("the temporal run completes");

    assert!(
        harness.is_marked(only).await,
        "a completed temporal run marks the items it finished"
    );

    // Second wave over the same dataset: nothing left to do, and no LLM spend.
    let second_llm = Arc::new(TemporalFixtureLlm::new());
    let second_run = harness
        .run_over(
            &config,
            &[only],
            Arc::clone(&second_llm) as Arc<dyn cognee_llm::Llm>,
        )
        .await
        .expect("the second temporal run is a no-op");

    assert!(
        second_run.already_completed,
        "re-cognifying a complete dataset is a no-op on the temporal branch too"
    );
    assert_eq!(
        second_llm.call_count(),
        0,
        "a skipped item is never sent to an LLM"
    );
}
