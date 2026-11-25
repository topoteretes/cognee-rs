#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The default rollback scope, end to end (SDK-1694 D4): with
//! `rollback_scope` unset and `incremental_loading` on, a cognify run behaves
//! as `FailedItems`. Kept apart from `per_document_completion` so that the D4
//! commits can be dropped together with it.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;

use cognee_database::PipelineRunStatus;
use rollback_harness::{Harness, extraction_config};

/// The shipped default: an unset `rollback_scope` with `incremental_loading`
/// on is `FailedItems`. One call at a time, the document before the failure
/// is saved and marked, the failed one is swept, the one after it is never
/// reached, and the run errors, as Python's does.
#[tokio::test]
async fn the_unset_default_scope_keeps_what_the_run_completed_and_errors() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let bad = harness.add_file("Bob FAILMARKER breaks here.").await;
    let last = harness.add_file("Carol also works at Acme.").await;
    let config = extraction_config();
    assert_eq!(config.rollback_scope, None, "the scope is left unset");
    assert!(config.incremental_loading);

    let report = rollback_harness::failed_report(
        harness.run(&config).await,
        "a failed document errors the run",
    );
    assert_eq!(report.failed_items(), &BTreeSet::from([bad]));
    assert_eq!(report.unreached_items(), &BTreeSet::from([last]));
    assert_eq!(harness.latest_status().await, PipelineRunStatus::Errored);

    let nodes = harness.graph_node_id_set().await;
    assert!(
        nodes.contains(&first.to_string()),
        "the completed document stays"
    );
    assert!(harness.is_marked(first).await, "and stays marked");
    for data_id in [bad, last] {
        assert!(!nodes.contains(&data_id.to_string()));
        assert!(!harness.is_marked(data_id).await);
    }
}
