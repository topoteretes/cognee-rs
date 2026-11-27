//! Cognify pipeline result types.
//!
//! The actual pipeline orchestration lives in [`crate::tasks`]. This module
//! defines the output types shared across the pipeline.

use std::collections::{BTreeSet, HashMap};

use cognee_models::{Document, DocumentChunk, EdgeType, Embedding};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::failure::FailureReport;
use crate::graph_integration::{GraphEdgePair, GraphNodePair};
use crate::summarization::TextSummary;

/// Result of the cognify pipeline.
#[derive(Debug, Clone)]
pub struct CognifyResult {
    /// Text chunks extracted from documents
    pub chunks: Vec<DocumentChunk>,

    /// Entities (nodes) with their types, deduplicated
    pub entities: Vec<GraphNodePair>,

    /// Edges (relationships) between entities, deduplicated
    pub edges: Vec<GraphEdgePair>,

    /// Text summaries generated from chunks
    pub summaries: Vec<TextSummary>,

    /// Edge types aggregated from relationship names
    pub edge_types: Vec<EdgeType>,

    /// Embeddings for chunks, entities, and summaries — **empty unless**
    /// [`CognifyConfig::retain_embeddings`](crate::CognifyConfig::retain_embeddings)
    /// is set. The vectors are in the vector store either way; read
    /// [`Self::embedding_count`] for how many there were.
    pub embeddings: Vec<Embedding>,

    /// How many chunk, entity and summary embeddings the run generated — what
    /// `embeddings.len()` reported before the vectors stopped being retained by
    /// default. Set whether or not [`Self::embeddings`] is populated.
    pub embedding_count: usize,

    /// Statistics about indexed fields
    pub indexed_fields: IndexedFieldsStats,

    /// Documents needed by the post-pipeline
    /// [`crate::tasks::extract_dlt_fk_edges`] teardown step. Populated by the
    /// final task in [`crate::tasks::build_cognify_pipeline`]. The matching
    /// chunk list reuses the existing [`Self::chunks`] field.
    ///
    /// Not serialised — internal teardown carrier, not part of the public
    /// result shape.
    pub documents_for_dlt: Vec<Document>,

    /// `true` when this run had nothing left to do. All other fields are
    /// empty.
    ///
    /// Set when every data item of the dataset was already cognified by an
    /// earlier run (the completion markers cover it). Python skips such items
    /// one by one inside the run; Rust skips them before building the
    /// pipeline, so the run writes no rows of its own.
    ///
    /// CLI prints "already complete" when set; HTTP-server returns
    /// `200 OK` with `status = "PipelineRunAlreadyCompleted"`. See doc 08-08
    /// §4.3 and locked decision 13.
    pub already_completed: bool,

    /// The `pipeline_run_id` of the prior completed run this result deferred
    /// to. `None` when no prior completed run was found — including on the
    /// marker path, where every item may have been completed by several
    /// different earlier runs.
    pub prior_pipeline_run_id: Option<Uuid>,

    /// The `pipeline_runs.pipeline_run_id` of the run that produced this
    /// result — the same value stamped on every ownership row the run wrote.
    ///
    /// Set by the final task from its `PipelineContext`; `None` for
    /// [`Self::empty`] / [`Self::already_completed`] and for a result produced
    /// outside a pipeline executor. The post-pipeline
    /// [`crate::tasks::extract_dlt_fk_edges`] teardown runs outside the
    /// executor and reads the run id from here.
    pub pipeline_run_id: Option<Uuid>,

    /// Everything that went wrong during the run: which chunk, which file,
    /// which stage, and why — plus a total count when the list was capped.
    ///
    /// A non-empty report on an `Ok` result means the run completed with
    /// failures the configured policy tolerated. When the policy judged them
    /// fatal the caller gets [`crate::CognifyError::RunFailed`] carrying the
    /// same report instead. See [`crate::failure`].
    pub failures: FailureReport,

    /// What became of each data item handed to [`crate::cognify`], one entry
    /// per item in input order (SDK-1694 O8): processed and completed by this
    /// run, skipped because an earlier run completed it, failed, or never
    /// reached. Python's per-item `data_ingestion_info` entries, which hosts
    /// render with the item's own source fields.
    ///
    /// Ids and statuses only (no documents), so it stays small however large
    /// the run. Empty for a result built outside [`crate::cognify`]. A run
    /// that errors carries the same list on its failure report
    /// ([`crate::CognifyError::item_outcomes`]).
    pub items: Vec<ItemOutcome>,
}

/// What one data item's cognify run came to. See [`CognifyResult::items`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemStatus {
    /// Processed by this run and completed: saved, and marked complete when
    /// `incremental_loading` is on. Python's `PipelineRunCompleted` entry.
    ///
    /// It does not promise a completion marker. None is written without
    /// `incremental_loading` (also under an explicit
    /// [`crate::RollbackScope::FailedItems`]), and a failed marker write
    /// only warns, so the next run may process the item again.
    Completed,
    /// Skipped: an earlier run completed it (the completion markers cover
    /// it), so this run did nothing for it. Python's
    /// `PipelineRunAlreadyCompleted` entry. Only with `incremental_loading`.
    AlreadyCompleted,
    /// Its processing failed ([`crate::FailureReport::failed_items`]).
    /// Python's `PipelineRunErrored` entry. Not kept; the next run redoes it.
    Failed,
    /// Never processed ([`crate::FailureReport::unreached_items`]): a
    /// fail-fast stop came first, or it is a legacy binary row classification
    /// skips ([`cognee_models::is_legacy_binary_row`]: raw bytes an earlier
    /// Rust `add` stored without a loader; delete or re-add it). Not marked;
    /// the next run sends it again. An item whose extension the
    /// document-type table does not list is otherwise read as text, as Python
    /// does (SDK-1694 O10). Python has no such entry: its fail-fast run
    /// cancels the items in flight and reports none for them, and it has no
    /// legacy binary rows.
    Unreached,
    /// Processed by a run that then errored and did not keep it: swept with
    /// the run (`whole_run`, or a document `failed_items` had not completed
    /// yet), or, under the code-only [`crate::RollbackScope::Nothing`], left
    /// in place without a completion marker. The next run redoes it.
    Discarded,
}

impl ItemStatus {
    /// Whether the item's work is in the graph and done: completed by this
    /// run or by an earlier one. Python's `data_ids` keep exactly these.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Completed | Self::AlreadyCompleted)
    }
}

/// One data item's outcome: its id and [`ItemStatus`]. A
/// [`ItemStatus::Completed`] item is saved, but not necessarily marked
/// complete (see there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ItemOutcome {
    /// The data item (`Data.id`).
    pub data_id: Uuid,
    /// What became of it.
    pub status: ItemStatus,
}

impl ItemOutcome {
    /// An outcome for `data_id`.
    pub fn new(data_id: Uuid, status: ItemStatus) -> Self {
        Self { data_id, status }
    }
}

/// The sets a run's per-item outcome is read from, all as data ids.
///
/// Built by [`crate::cognify`] once the run has ended, after any rollback, so
/// [`ItemStatus::Completed`] names only what the run kept.
pub(crate) struct ItemOutcomeSets<'a> {
    /// Skipped by the completion markers before the run.
    pub already_completed: &'a BTreeSet<Uuid>,
    /// Completed as the run went (the completion log), kept whatever happened
    /// to the run afterwards.
    pub completed: &'a BTreeSet<Uuid>,
    /// The failure report's failed and unreached items.
    pub report: &'a FailureReport,
    /// Whether the run ended `Ok`: then every processed item that neither
    /// failed nor went unreached completed; otherwise only the logged ones did.
    pub run_completed: bool,
}

/// Each of `input` (in order) with its [`ItemStatus`]. See [`ItemOutcomeSets`].
///
/// Precedence: already completed, then completed (a document the run kept and
/// marked is done, whatever a later stage reported against it), then failed,
/// then unreached; what is left completed on a run that ended `Ok` and was
/// discarded on one that errored.
pub(crate) fn item_outcomes(input: &[Uuid], sets: &ItemOutcomeSets<'_>) -> Vec<ItemOutcome> {
    input
        .iter()
        .map(|&data_id| {
            let status = if sets.already_completed.contains(&data_id) {
                ItemStatus::AlreadyCompleted
            } else if sets.completed.contains(&data_id) {
                ItemStatus::Completed
            } else if sets.report.failed_items().contains(&data_id) {
                ItemStatus::Failed
            } else if sets.report.unreached_items().contains(&data_id) {
                ItemStatus::Unreached
            } else if sets.run_completed {
                ItemStatus::Completed
            } else {
                ItemStatus::Discarded
            };
            ItemOutcome::new(data_id, status)
        })
        .collect()
}

impl CognifyResult {
    /// Create an empty result (no data to process).
    pub fn empty() -> Self {
        Self {
            chunks: vec![],
            entities: vec![],
            edges: vec![],
            summaries: vec![],
            edge_types: vec![],
            embeddings: vec![],
            embedding_count: 0,
            indexed_fields: IndexedFieldsStats::default(),
            documents_for_dlt: vec![],
            already_completed: false,
            prior_pipeline_run_id: None,
            pipeline_run_id: None,
            failures: FailureReport::default(),
            items: vec![],
        }
    }

    /// Create a short-circuit "already completed" result tagged with the
    /// prior `pipeline_run_id`. All payload vectors are empty — callers that
    /// need the prior run's outputs should query the graph / vector store
    /// directly (matches Python parity). See doc 08-08 §4.3.
    pub fn already_completed(pipeline_run_id: Uuid) -> Self {
        Self {
            already_completed: true,
            prior_pipeline_run_id: Some(pipeline_run_id),
            ..Self::empty()
        }
    }
}

/// Statistics about indexed fields.
///
/// Tracks how many data points were indexed for each field type.
/// Uses dynamic `{TypeName}_{field_name}` keys matching the Python SDK's
/// `metadata["index_fields"]`-driven approach, plus legacy convenience accessors.
#[derive(Debug, Clone, Default)]
pub struct IndexedFieldsStats {
    /// Dynamic per-collection counts keyed by `"{TypeName}_{field_name}"`.
    ///
    /// E.g. `"DocumentChunk_text" -> 42`, `"Entity_name" -> 7`.
    pub field_counts: HashMap<String, usize>,
}

impl IndexedFieldsStats {
    /// Record that `count` items were indexed for `collection`.
    pub fn record(&mut self, data_type: &str, field_name: &str, count: usize) {
        let key = format!("{data_type}_{field_name}");
        *self.field_counts.entry(key).or_insert(0) += count;
    }

    /// Get count for a specific `{type}_{field}` collection, or 0 if absent.
    pub fn get(&self, data_type: &str, field_name: &str) -> usize {
        let key = format!("{data_type}_{field_name}");
        self.field_counts.get(&key).copied().unwrap_or(0)
    }

    // -- Convenience accessors (backward-compatible with old named fields) --

    /// Number of DocumentChunk.text fields indexed.
    pub fn chunk_text_count(&self) -> usize {
        self.get("DocumentChunk", "text")
    }

    /// Number of Entity.name fields indexed.
    pub fn entity_name_count(&self) -> usize {
        self.get("Entity", "name")
    }

    /// Number of EntityType.name fields indexed.
    pub fn entity_type_name_count(&self) -> usize {
        self.get("EntityType", "name")
    }

    /// Number of TextSummary.text fields indexed.
    pub fn summary_text_count(&self) -> usize {
        self.get("TextSummary", "text")
    }

    /// Number of EdgeType.relationship_name fields indexed.
    pub fn edge_type_count(&self) -> usize {
        self.get("EdgeType", "relationship_name")
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::failure::{FailureStage, StageFailure};

    fn ids(n: u128) -> Vec<Uuid> {
        (1..=n).map(Uuid::from_u128).collect()
    }

    fn failure(data_id: Uuid) -> StageFailure {
        StageFailure {
            stage: FailureStage::GraphExtraction,
            data_id,
            chunk_id: None,
            error: "boom".into(),
            fails_item: true,
        }
    }

    /// Every status, in input order, with its precedence: a document the run
    /// kept is completed even when a later stage reported against it.
    #[test]
    fn item_outcomes_follow_input_order_and_precedence() {
        let id = ids(6);
        let mut report = FailureReport::default();
        report.record(failure(id[2]));
        report.record(failure(id[5]));
        report.mark_unreached(id[3]);
        let already = BTreeSet::from([id[0]]);
        let completed = BTreeSet::from([id[1], id[5]]);
        let sets = |run_completed| ItemOutcomeSets {
            already_completed: &already,
            completed: &completed,
            report: &report,
            run_completed,
        };

        let statuses = |run_completed| -> Vec<ItemStatus> {
            item_outcomes(&id, &sets(run_completed))
                .iter()
                .map(|outcome| outcome.status)
                .collect()
        };
        assert_eq!(
            statuses(false),
            vec![
                ItemStatus::AlreadyCompleted,
                ItemStatus::Completed,
                ItemStatus::Failed,
                ItemStatus::Unreached,
                ItemStatus::Discarded,
                ItemStatus::Completed,
            ]
        );
        assert_eq!(statuses(true)[4], ItemStatus::Completed);
        assert_eq!(
            item_outcomes(&id, &sets(false))
                .iter()
                .map(|outcome| outcome.data_id)
                .collect::<Vec<_>>(),
            id
        );
    }

    #[test]
    fn only_completed_statuses_are_complete() {
        assert!(ItemStatus::Completed.is_complete());
        assert!(ItemStatus::AlreadyCompleted.is_complete());
        for status in [
            ItemStatus::Failed,
            ItemStatus::Unreached,
            ItemStatus::Discarded,
        ] {
            assert!(!status.is_complete(), "{status:?}");
        }
    }

    #[test]
    fn item_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(ItemStatus::AlreadyCompleted).unwrap(),
            serde_json::json!("already_completed")
        );
    }
}
