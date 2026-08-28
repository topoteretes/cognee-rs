//! Failure handling vocabulary for the cognify pipeline.
//!
//! A stage that cannot process one chunk or one data item no longer aborts the
//! whole batch by propagating. It *records* what failed — which stage, which
//! file, which chunk, and why — into a [`FailureReport`] that rides along every
//! stage output to the run result. Whether the run then errors is a separate
//! decision, made once at the end of the run from the two configured axes.
//!
//! Two independent axes control it (see `docs/configuration.md`):
//!
//! * [`FailureStop`] — *when to stop*: [`FailureStop::FailFast`] (default) stops
//!   scheduling further work at the first stage-level failure;
//!   [`FailureStop::RunToEnd`] keeps going and decides at the end.
//! * [`RollbackScope`] — *what to sweep*: [`RollbackScope::WholeRun`] (default),
//!   [`RollbackScope::FailedItems`], or [`RollbackScope::Nothing`].
//!
//! The default pair is Python's default behaviour in both execution and end
//! state: abort at the first failed item, error the run.
//!
//! Failure policy is a property of the stage, not a user choice — with one
//! exception. Summarization failures are fatal by default (Python parity) and
//! become tolerated when
//! [`FailurePolicy::tolerate_summarization_failures`] is set. That flag governs
//! **fatality only**: summarization failures are *always* collected rather than
//! propagated, so the reported list is complete and no already-paid-for summary
//! is discarded.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Axis 1 — when a run stops scheduling further work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FailureStop {
    /// Stop the failing stage's own loop at the first failure. The survivors
    /// still travel down the rest of the pipeline — a later stage never aborts
    /// merely because an earlier one recorded a failure, or the files that did
    /// complete could never be persisted at all.
    #[default]
    FailFast,

    /// Continue past failures, collecting them, and decide at the end. Costs a
    /// full run's worth of LLM calls but produces the complete failure list.
    RunToEnd,
}

/// Axis 2 — what a failed run removes.
///
/// Read twice: during the run for its execution consequences — most visibly
/// the abort-time partition of [`FailureStop::FailFast`] +
/// [`RollbackScope::FailedItems`] — and at the end by [`crate::rollback`],
/// which turns it into the scope handed to the sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RollbackScope {
    /// Remove everything this run created.
    #[default]
    WholeRun,

    /// Remove only the contributions of the files that failed; keep and mark
    /// the rest.
    FailedItems,

    /// Remove nothing. The escape hatch.
    Nothing,
}

/// Which stage produced a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureStage {
    /// Reading or chunking a data item. Failure unit is the file.
    Chunking,
    /// LLM graph extraction. Failure unit is the chunk.
    GraphExtraction,
    /// LLM summarization. Failure unit is the chunk; fatality is configurable.
    Summarization,
    /// LLM temporal event extraction. Failure unit is the chunk.
    TemporalExtraction,
    /// LLM temporal entity enrichment. One call covers a whole batch of
    /// chunks, so one failure is recorded against every chunk that fed it —
    /// each of those chunks genuinely did fail to be fully processed, and
    /// attributing the failure to the batch alone would leave it out of the
    /// chunk failure ratio entirely.
    TemporalEnrichment,
}

impl fmt::Display for FailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FailureStage::Chunking => "chunking",
            FailureStage::GraphExtraction => "graph extraction",
            FailureStage::Summarization => "summarization",
            FailureStage::TemporalExtraction => "temporal event extraction",
            FailureStage::TemporalEnrichment => "temporal entity enrichment",
        };
        f.write_str(name)
    }
}

/// One recorded failure: which stage, which file, which chunk, what error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageFailure {
    /// The stage that produced it.
    pub stage: FailureStage,

    /// The data item (file) it belongs to — `Document.base.id`, which equals
    /// `Document.data_id` and `Data.id`.
    pub data_id: Uuid,

    /// The chunk it belongs to. `None` for file-unit failures (chunking).
    pub chunk_id: Option<Uuid>,

    /// The rendered error. Rendered rather than typed because the report is
    /// carried through stage outputs and serialised to the caller.
    pub error: String,

    /// Whether this failure makes its data item fail. `false` only for
    /// tolerated summarization failures, which are recorded and reported but
    /// count toward nothing.
    pub fails_item: bool,
}

impl fmt::Display for StageFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.chunk_id {
            Some(chunk_id) => write!(
                f,
                "{} failed for data {} chunk {}: {}",
                self.stage, self.data_id, chunk_id, self.error
            ),
            None => write!(
                f,
                "{} failed for data {}: {}",
                self.stage, self.data_id, self.error
            ),
        }
    }
}

/// Everything a run learned about what went wrong.
///
/// The entry list is capped (a 100 000-chunk disaster must not become a
/// 100 000-line error message), but every counter and every id set is not: the
/// sweep selects by file, so truncating [`Self::failed_items`] would silently
/// under-sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureReport {
    entries: Vec<StageFailure>,
    total: usize,
    failed_items: BTreeSet<Uuid>,
    unreached_items: BTreeSet<Uuid>,
    failed_chunks: usize,
    summarization_failures: usize,
    total_chunks: usize,
    total_items: usize,
    cap: usize,
}

impl Default for FailureReport {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            total: 0,
            failed_items: BTreeSet::new(),
            unreached_items: BTreeSet::new(),
            failed_chunks: 0,
            summarization_failures: 0,
            total_chunks: 0,
            total_items: 0,
            cap: FailurePolicy::DEFAULT_REPORT_CAP,
        }
    }
}

impl FailureReport {
    /// An empty report that will honour `policy`'s entry cap.
    ///
    /// The cap travels with the report so every downstream stage pushes under
    /// the same bound without re-reading configuration.
    pub fn with_policy(policy: &FailurePolicy) -> Self {
        Self {
            cap: policy.report_cap,
            ..Self::default()
        }
    }

    /// Record one failure.
    ///
    /// The single mutation point, so the counters cannot drift from the entry
    /// list: it always bumps [`Self::total`], and only appends to the capped
    /// entry list while there is room.
    pub fn record(&mut self, failure: StageFailure) {
        self.total += 1;
        if failure.stage == FailureStage::Summarization {
            self.summarization_failures += 1;
        }
        if failure.fails_item {
            self.failed_items.insert(failure.data_id);
            // A file that failed is no longer "unreached" — a chunk of it was
            // attempted.
            self.unreached_items.remove(&failure.data_id);
            if failure.chunk_id.is_some() {
                self.failed_chunks += 1;
            }
        }
        if self.entries.len() < self.cap {
            self.entries.push(failure);
        }
    }

    /// Note that `data_id`'s work was never attempted, because an earlier
    /// failure stopped the run first. Unreached files are not failures — they
    /// are simply redone by the next run — but they must not be marked
    /// complete either.
    pub fn mark_unreached(&mut self, data_id: Uuid) {
        if self.failed_items.contains(&data_id) {
            return;
        }
        self.unreached_items.insert(data_id);
    }

    /// Record the denominators of the failure ratio, once per run, from the
    /// only stage that knows both counts.
    pub fn note_totals(&mut self, items: usize, chunks: usize) {
        self.total_items = items;
        self.total_chunks = chunks;
    }

    /// `true` when nothing failed and nothing was left unreached.
    pub fn is_empty(&self) -> bool {
        self.total == 0 && self.unreached_items.is_empty()
    }

    /// The recorded failures, capped at the configured report cap.
    pub fn entries(&self) -> &[StageFailure] {
        &self.entries
    }

    /// How many failures happened in total, cap or no cap.
    pub fn total(&self) -> usize {
        self.total
    }

    /// How many failures the cap kept out of [`Self::entries`].
    pub fn truncated(&self) -> usize {
        self.total.saturating_sub(self.entries.len())
    }

    /// The data items at least one of whose failures fails the item. Never
    /// capped — a later sweep selects by exactly this set.
    pub fn failed_items(&self) -> &BTreeSet<Uuid> {
        &self.failed_items
    }

    /// The data items whose work was never attempted. Never capped.
    pub fn unreached_items(&self) -> &BTreeSet<Uuid> {
        &self.unreached_items
    }

    /// How many summarization failures were recorded, tolerated or not. Kept
    /// separate because tolerated ones count toward nothing else.
    pub fn summarization_failures(&self) -> usize {
        self.summarization_failures
    }

    /// Total items the run started with, as reported by the chunking stage.
    pub fn total_items(&self) -> usize {
        self.total_items
    }

    /// Total chunks the run produced, as reported by the chunking stage.
    pub fn total_chunks(&self) -> usize {
        self.total_chunks
    }

    /// Item-failing chunk failures over the run's chunk count.
    ///
    /// Summarization failures are excluded by construction: a tolerated one
    /// never enters [`Self::failed_chunks`], and the ratio only gates the
    /// `FailedItems` scope. `0.0` when the run produced no chunks.
    pub fn chunk_failure_ratio(&self) -> f64 {
        if self.total_chunks == 0 {
            return 0.0;
        }
        self.failed_chunks as f64 / self.total_chunks as f64
    }

    /// Whether this report makes the run fail, under `policy`.
    ///
    /// * No failed item — only tolerated summarization failures, or nothing at
    ///   all — is never fatal.
    /// * `WholeRun` and `Nothing` are fatal on any failed item: the first
    ///   sweeps everything, and the second knowingly leaves an incomplete
    ///   dataset in place, so neither can honestly report success.
    /// * `FailedItems` tolerates failures below the ratio threshold, with one
    ///   backstop: if no item survived, the run failed regardless of the ratio.
    ///   Survival subtracts both failed and *unreached* items. That backstop
    ///   matters twice over: a file that fails *at the chunk stage* produced no
    ///   chunks so it contributes to neither side of the ratio, and a `FailFast`
    ///   abort in the first batch leaves the rest undispatched — a run that
    ///   completed nothing would otherwise score a tiny ratio and report `Ok`.
    pub fn is_fatal(&self, policy: &FailurePolicy) -> bool {
        if self.failed_items.is_empty() {
            return false;
        }
        match policy.scope {
            RollbackScope::WholeRun | RollbackScope::Nothing => true,
            RollbackScope::FailedItems => {
                // No item survived — fatal regardless of the ratio. Unreached
                // items count here: a `FailFast` abort in the first batch
                // leaves the rest of the dataset undispatched, and a run that
                // completed *nothing* cannot honestly report success even
                // though its chunk ratio is tiny. Files that did complete keep
                // the run truthful, which is what makes FailFast + FailedItems
                // useful rather than merely quiet.
                let survived = self
                    .total_items
                    .saturating_sub(self.failed_items.len() + self.unreached_items.len());
                if self.total_items > 0 && survived == 0 {
                    return true;
                }
                self.chunk_failure_ratio() > policy.chunk_failure_ratio_threshold
            }
        }
    }
}

impl fmt::Display for FailureReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} failure(s) ({} shown, {} omitted)",
            self.total,
            self.entries.len(),
            self.truncated()
        )?;
        for entry in &self.entries {
            write!(f, "; {entry}")?;
        }
        if !self.unreached_items.is_empty() {
            write!(
                f,
                "; {} item(s) never attempted",
                self.unreached_items.len()
            )?;
        }
        Ok(())
    }
}

/// The two axes plus the knobs that qualify them, resolved from
/// [`crate::CognifyConfig`] once per run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FailurePolicy {
    /// Axis 1 — when to stop.
    pub stop: FailureStop,

    /// Axis 2 — what to sweep.
    pub scope: RollbackScope,

    /// Whether a summarization failure leaves its data item intact. Collection
    /// is unconditional either way; this governs fatality only.
    pub tolerate_summarization_failures: bool,

    /// The share of chunks that may fail before a `FailedItems` run is called
    /// failed. Counted in chunks, evaluated per run, summarization excluded.
    pub chunk_failure_ratio_threshold: f64,

    /// How many individual failures the report lists before it starts counting
    /// them only in the total.
    pub report_cap: usize,
}

impl FailurePolicy {
    /// Default share of failed chunks a `FailedItems` run tolerates.
    pub const DEFAULT_CHUNK_FAILURE_RATIO_THRESHOLD: f64 = 0.05;

    /// Default number of failures the report lists individually.
    pub const DEFAULT_REPORT_CAP: usize = 100;
}

impl Default for FailurePolicy {
    fn default() -> Self {
        Self {
            stop: FailureStop::default(),
            scope: RollbackScope::default(),
            tolerate_summarization_failures: false,
            chunk_failure_ratio_threshold: Self::DEFAULT_CHUNK_FAILURE_RATIO_THRESHOLD,
            report_cap: Self::DEFAULT_REPORT_CAP,
        }
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

    fn item_failure(stage: FailureStage, data_id: Uuid, chunk_id: Option<Uuid>) -> StageFailure {
        StageFailure {
            stage,
            data_id,
            chunk_id,
            error: "boom".to_string(),
            fails_item: true,
        }
    }

    /// Every stage renders as prose, because a `StageFailure` is read by a
    /// user in an error message. Exhaustive on purpose: a new variant added
    /// without a `Display` arm would not compile, but one added with a
    /// copy-pasted arm would, and the report would then mislabel it.
    #[test]
    fn every_stage_renders_its_own_name() {
        let names = [
            (FailureStage::Chunking, "chunking"),
            (FailureStage::GraphExtraction, "graph extraction"),
            (FailureStage::Summarization, "summarization"),
            (
                FailureStage::TemporalExtraction,
                "temporal event extraction",
            ),
            (
                FailureStage::TemporalEnrichment,
                "temporal entity enrichment",
            ),
        ];
        for (stage, expected) in names {
            assert_eq!(stage.to_string(), expected);
        }
    }

    #[test]
    fn record_caps_entries_but_never_the_totals() {
        let policy = FailurePolicy {
            report_cap: 2,
            ..FailurePolicy::default()
        };
        let mut report = FailureReport::with_policy(&policy);
        for _ in 0..5 {
            report.record(item_failure(
                FailureStage::GraphExtraction,
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
            ));
        }

        assert_eq!(report.entries().len(), 2, "the entry list honours the cap");
        assert_eq!(report.total(), 5);
        assert_eq!(report.truncated(), 3);
        assert_eq!(
            report.failed_items().len(),
            5,
            "the failed-item set is never capped — a sweep selects by it"
        );
    }

    #[test]
    fn tolerated_summarization_failures_stay_out_of_every_denominator() {
        let mut report = FailureReport::default();
        report.note_totals(3, 60);
        for _ in 0..3 {
            report.record(StageFailure {
                stage: FailureStage::Summarization,
                data_id: Uuid::new_v4(),
                chunk_id: Some(Uuid::new_v4()),
                error: "429".to_string(),
                fails_item: false,
            });
        }

        assert_eq!(report.total(), 3, "they are still reported");
        assert_eq!(report.summarization_failures(), 3);
        assert!(report.failed_items().is_empty());
        assert_eq!(report.chunk_failure_ratio(), 0.0);
        assert!(!report.is_fatal(&FailurePolicy::default()));
    }

    #[test]
    fn untolerated_summarization_failures_fail_their_item() {
        let mut report = FailureReport::default();
        report.note_totals(3, 60);
        let data_id = Uuid::new_v4();
        report.record(StageFailure {
            stage: FailureStage::Summarization,
            data_id,
            chunk_id: Some(Uuid::new_v4()),
            error: "429".to_string(),
            fails_item: true,
        });

        assert_eq!(report.summarization_failures(), 1);
        assert_eq!(
            report.failed_items().iter().copied().collect::<Vec<_>>(),
            [data_id]
        );
        assert!(report.is_fatal(&FailurePolicy::default()));
    }

    #[test]
    fn chunk_failure_ratio_is_chunks_over_chunks() {
        let mut report = FailureReport::default();
        report.note_totals(10, 60);
        for _ in 0..3 {
            report.record(item_failure(
                FailureStage::GraphExtraction,
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
            ));
        }
        assert!((report.chunk_failure_ratio() - 0.05).abs() < f64::EPSILON);

        let empty = FailureReport::default();
        assert_eq!(
            empty.chunk_failure_ratio(),
            0.0,
            "no chunks means no division"
        );
    }

    #[test]
    fn is_fatal_matrix() {
        // One failed chunk out of 60 — well under the 5 % default threshold —
        // across four surviving items.
        let mut report = FailureReport::default();
        report.note_totals(4, 60);
        report.record(item_failure(
            FailureStage::GraphExtraction,
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
        ));

        for stop in [FailureStop::FailFast, FailureStop::RunToEnd] {
            for (scope, expected) in [
                (RollbackScope::WholeRun, true),
                (RollbackScope::Nothing, true),
                (RollbackScope::FailedItems, false),
            ] {
                let policy = FailurePolicy {
                    stop,
                    scope,
                    ..FailurePolicy::default()
                };
                assert_eq!(
                    report.is_fatal(&policy),
                    expected,
                    "stop={stop:?} scope={scope:?}"
                );
            }
        }

        // Over the threshold, `FailedItems` fails too.
        let mut over = FailureReport::default();
        over.note_totals(4, 10);
        for _ in 0..2 {
            over.record(item_failure(
                FailureStage::GraphExtraction,
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
            ));
        }
        assert!(over.is_fatal(&FailurePolicy {
            scope: RollbackScope::FailedItems,
            ..FailurePolicy::default()
        }));

        // A report with nothing in it is never fatal, under any scope.
        let clean = FailureReport::default();
        for scope in [
            RollbackScope::WholeRun,
            RollbackScope::FailedItems,
            RollbackScope::Nothing,
        ] {
            assert!(!clean.is_fatal(&FailurePolicy {
                scope,
                ..FailurePolicy::default()
            }));
        }
    }

    /// A `FailFast` abort in the first batch completes nothing, so the run
    /// must not report success — even though its chunk ratio is tiny.
    #[test]
    fn a_run_that_completed_nothing_is_fatal_even_below_the_ratio() {
        let mut report = FailureReport::default();
        report.note_totals(100, 100);
        report.record(item_failure(
            FailureStage::GraphExtraction,
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
        ));
        for _ in 0..99 {
            report.mark_unreached(Uuid::new_v4());
        }

        assert!(
            report.chunk_failure_ratio() < 0.05,
            "precondition: the ratio alone would tolerate this"
        );
        assert!(
            report.is_fatal(&FailurePolicy {
                stop: FailureStop::FailFast,
                scope: RollbackScope::FailedItems,
                ..FailurePolicy::default()
            }),
            "no file completed, so the run failed"
        );
    }

    /// …but a run where some files DID complete stays tolerant. This is the
    /// whole point of FailFast + FailedItems: stop spending, keep what is done.
    #[test]
    fn a_failfast_abort_that_completed_some_files_is_not_fatal() {
        let mut report = FailureReport::default();
        report.note_totals(100, 100);
        report.record(item_failure(
            FailureStage::GraphExtraction,
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
        ));
        for _ in 0..50 {
            report.mark_unreached(Uuid::new_v4());
        }

        assert!(
            !report.is_fatal(&FailurePolicy {
                stop: FailureStop::FailFast,
                scope: RollbackScope::FailedItems,
                ..FailurePolicy::default()
            }),
            "49 files completed — the run is genuinely partial, not failed"
        );
    }

    #[test]
    fn is_fatal_when_no_item_survives() {
        // Three files that all failed to open: zero chunks, so the ratio is
        // 0.0 and cannot catch this. The backstop must.
        let mut report = FailureReport::default();
        report.note_totals(3, 0);
        for _ in 0..3 {
            report.record(item_failure(FailureStage::Chunking, Uuid::new_v4(), None));
        }

        assert_eq!(report.chunk_failure_ratio(), 0.0);
        assert!(report.is_fatal(&FailurePolicy {
            scope: RollbackScope::FailedItems,
            ..FailurePolicy::default()
        }));
    }

    #[test]
    fn mark_unreached_never_shadows_a_real_failure() {
        let data_id = Uuid::new_v4();
        let mut report = FailureReport::default();
        report.record(item_failure(FailureStage::Chunking, data_id, None));
        report.mark_unreached(data_id);
        assert!(report.unreached_items().is_empty());

        // …and recording a failure later clears an earlier unreached mark.
        let other = Uuid::new_v4();
        let mut report = FailureReport::default();
        report.mark_unreached(other);
        assert_eq!(report.unreached_items().len(), 1);
        report.record(item_failure(FailureStage::GraphExtraction, other, None));
        assert!(report.unreached_items().is_empty());
        assert_eq!(report.failed_items().len(), 1);
    }

    #[test]
    fn display_is_bounded() {
        let policy = FailurePolicy {
            report_cap: 2,
            ..FailurePolicy::default()
        };
        let mut report = FailureReport::with_policy(&policy);
        for _ in 0..500 {
            report.record(item_failure(
                FailureStage::GraphExtraction,
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
            ));
        }

        let rendered = report.to_string();
        assert!(rendered.starts_with("500 failure(s) (2 shown, 498 omitted)"));
        assert!(
            rendered.len() < 500,
            "the error message must stay bounded, got {} chars",
            rendered.len()
        );
    }

    #[test]
    fn is_empty_sees_unreached_items() {
        let mut report = FailureReport::default();
        assert!(report.is_empty());
        report.mark_unreached(Uuid::new_v4());
        assert!(!report.is_empty());
    }
}
