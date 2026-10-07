use std::sync::Arc;

use cognee::cognify::{ChunkStrategy, CognifyConfig, CognifyResult, FailureReport, cognify};
use cognee::database::{
    DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository, ops,
};
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver, RdfLibOntologyResolver};
use cognee::{ComponentManager, PipelineContext};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::cli::{ChunkerArg, CognifyArgs};
use crate::error::CliError;

/// The distinctive prefix every failure summary carries.
///
/// Named rather than inlined because it is the string an operator greps for
/// and the string the regression test asserts is *absent* after a clean run.
const FAILURE_SUMMARY_MARKER: &str = "cognify completed with failures";

/// How many failed data ids the summary lists before it switches to counting.
///
/// A run over 171 888 documents can fail thousands of them; the full set is on
/// disk in `pipeline_runs.run_info.cognify_failures.failed_data_ids`, so the
/// console line only needs enough ids to start with.
const FAILED_ID_PREVIEW: usize = 10;

/// Render the operator-facing summary of a run that tolerated failures.
///
/// `None` for a clean run — the caller prints nothing at all in that case, so
/// the success path keeps exactly the output it had before this existed.
///
/// The counts come straight from the [`FailureReport`] the pipeline returns,
/// which is the same data
/// `cognee_cognify::rollback::run_info_with_failures` persists under the
/// `cognify_failures` key. Nothing here is recomputed.
pub fn format_failure_summary(dataset_name: &str, report: &FailureReport) -> Option<String> {
    if report.is_empty() {
        return None;
    }

    let failed = report.failed_items();
    let unreached = report.unreached_items();

    let ids = if failed.is_empty() {
        // No document was *failed* by what went wrong. Two ways to get here:
        // everything outstanding went unreached (an early stop), or every
        // recorded failure was a tolerated one — a summarization failure under
        // `tolerate_summarization_failures`, which is counted but fails
        // nothing. Saying "none" beats printing an empty list.
        "none".to_string()
    } else {
        let preview: Vec<String> = failed
            .iter()
            .take(FAILED_ID_PREVIEW)
            .map(Uuid::to_string)
            .collect();
        if failed.len() > FAILED_ID_PREVIEW {
            format!(
                "{} (first {} of {})",
                preview.join(", "),
                FAILED_ID_PREVIEW,
                failed.len()
            )
        } else {
            preview.join(", ")
        }
    };

    // Whether anything is actually left to redo. This is the same condition
    // `cognee_cognify::rollback` uses to decide whether to persist a
    // `cognify_failures` record at all (`failed ∪ unreached`, non-empty), so
    // the advice printed here cannot contradict what was written down.
    //
    // It matters because a report can be non-empty with nothing outstanding:
    // under `tolerate_summarization_failures` a failed summary is recorded but
    // fails no item, so every document still ends the run marked complete.
    // Telling an operator to re-run in that case would be advice that does
    // nothing — the completion markers make the next run a no-op.
    let advice = if failed.is_empty() && unreached.is_empty() {
        "No documents are outstanding; these failures were tolerated and a re-run would skip the dataset."
    } else {
        "Re-run cognify for this dataset to retry them."
    };

    Some(format!(
        "Dataset '{dataset_name}': {FAILURE_SUMMARY_MARKER} — {} document(s) failed, \
         {} never attempted, {} failure(s) recorded, chunk failure ratio {:.4}. \
         Failed data ids: {ids}. {advice}",
        failed.len(),
        unreached.len(),
        report.total(),
        report.chunk_failure_ratio(),
    ))
}

/// What one dataset's `cognify()` result means for the run-wide totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatasetOutcome {
    /// The pipeline did work; its counts belong in the totals.
    Ran,
    /// The pipeline short-circuited on an already-complete dataset; every
    /// payload vector is empty and there is nothing to add.
    AlreadyComplete,
}

/// Print everything the console says about one dataset's `cognify()` result.
///
/// Split out of [`run`] so it can be driven without a live pipeline: the
/// per-dataset reporting used to be inline in `run()`'s loop, where deleting
/// the failure-summary `warn!` left the whole suite green (SDK-700 gap 1).
/// The `report_dataset_outcome_*` tests below capture the tracing events this
/// emits, so dropping the `warn!` — or the summary it carries — now fails a
/// named test.
fn report_dataset_outcome(dataset_name: &str, result: &CognifyResult) -> DatasetOutcome {
    // Gap 08-08: surface the short-circuit verdict (Python parity).
    if result.already_completed {
        if let Some(prior) = result.prior_pipeline_run_id {
            info!(
                "Dataset '{dataset_name}': already complete (prior pipeline_run_id={prior}); skipping cognify."
            );
        } else {
            info!("Dataset '{dataset_name}': already complete; skipping cognify.");
        }
        return DatasetOutcome::AlreadyComplete;
    }

    // A run that reaches here completed; the policy tolerated whatever
    // failed. Until now that verdict was visible only to an in-process
    // caller reading `result.failures`, so an operator had no way to
    // learn *which* documents were left behind short of reading
    // `pipeline_runs.run_info` out of the database by hand.
    if let Some(summary) = format_failure_summary(dataset_name, &result.failures) {
        warn!("{summary}");
    }

    debug!(
        "Dataset '{}' -> chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
        dataset_name,
        result.chunks.len(),
        result.entities.len(),
        result.edges.len(),
        result.summaries.len(),
        result.embeddings.len()
    );
    DatasetOutcome::Ran
}

pub fn run(args: CognifyArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    // `None` all the way down selects the auto-calculation (Python `chunk_size=None`).
    let effective_chunk_size = args.chunk_size.or(settings.chunk_size);
    let effective_max_parallel = args
        .llm_max_parallel_requests
        .unwrap_or(settings.llm_max_parallel_requests)
        .max(1) as usize;
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    let settings_ontology_path = settings.ontology_file_path.clone();

    if args.background {
        warn!(
            "Warning: --background is accepted for compatibility, but execution remains synchronous and in-process."
        );
    }

    match args.chunker {
        ChunkerArg::Text => {}
        ChunkerArg::Langchain | ChunkerArg::Csv => {
            warn!(
                "Warning: selected chunker is not natively available in Rust yet; using TextChunker-compatible flow."
            );
        }
    }

    let requested_datasets = args.datasets.clone();

    crate::teardown::run_command(Arc::clone(&cm), async {
        // Resolve datasets first (cheap) — fail early before initializing heavy components
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let dataset_names = resolve_dataset_names(&database, owner_id, requested_datasets).await?;

        let storage = cm
            .storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let embedding_engine = cm
            .embedding_engine()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let llm = cm
            .llm()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let ontology_path = args.ontology_file.as_deref().or({
            if settings_ontology_path.is_empty() {
                None
            } else {
                Some(settings_ontology_path.as_str())
            }
        });
        let ontology_resolver: Arc<dyn OntologyResolver> = match ontology_path {
            Some(path) => Arc::new(RdfLibOntologyResolver::new(path).map_err(|error| {
                CliError::Runtime(format!("Ontology initialization failed: {error}"))
            })?),
            None => Arc::new(NoOpOntologyResolver::new()),
        };

        let chunk_strategy = match cm.settings().chunk_strategy.to_uppercase().as_str() {
            "RECURSIVE" => ChunkStrategy::Recursive,
            _ => ChunkStrategy::Paragraph,
        };

        let mut cognify_config = CognifyConfig::default()
            .with_chunk_size_opt(effective_chunk_size.map(|n| n as usize))
            .with_chunk_overlap(cm.settings().chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(effective_max_parallel)
            .with_temporal_cognify(args.temporal_cognify);
        if let Some(transcriber) = cm
            .transcriber()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
        {
            cognify_config = cognify_config.with_transcriber(transcriber);
        }

        let mut total_chunks = 0usize;
        let mut total_entities = 0usize;
        let mut total_edges = 0usize;
        let mut total_summaries = 0usize;
        let mut total_embeddings = 0usize;

        for dataset_name in &dataset_names {
            let dataset =
                ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
                    .await
                    .map_err(|error| {
                        CliError::Runtime(format!(
                            "Failed to resolve dataset '{dataset_name}': {error}"
                        ))
                    })?
                    .ok_or_else(|| {
                        CliError::Validation(format!(
                            "Dataset '{dataset_name}' was not found for owner {owner_id}"
                        ))
                    })?;

            let data_items = ops::datasets::get_dataset_data(&database, dataset.id)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to load data for dataset '{dataset_name}': {error}"
                    ))
                })?;

            if data_items.is_empty() {
                info!("Dataset '{dataset_name}': no data to cognify, skipping.");
                continue;
            }

            info!(
                "Dataset '{dataset_name}': cognifying {} data item(s)",
                data_items.len()
            );

            // OSS build has no DB-backed user lookup (there is no `users`
            // table), so `user_email` always falls back to `None`. `cognify()`
            // then uses `user_id.to_string()` as the provenance stamp.
            let user_email: Option<String> = None;

            let thread_pool: Arc<dyn cognee::core::CpuPool> = Arc::new(
                cognee::core::RayonThreadPool::with_default_threads().map_err(|e| {
                    CliError::Runtime(format!("failed to construct thread pool: {e}"))
                })?,
            );

            // Gap 08-07: persist the four-state `pipeline_runs` trail so
            // CLI cognify shows up in `/api/v1/activity/pipeline-runs`.
            let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

            let result = cognify(
                data_items,
                dataset.id,
                Some(owner_id),
                user_email,
                dataset.tenant_id,
                llm.clone(),
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&database),
                pipeline_run_repo,
                thread_pool,
                Arc::clone(&ontology_resolver),
                &cognify_config,
            )
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Cognify execution failed for dataset '{dataset_name}': {error}"
                ))
            })?;

            if report_dataset_outcome(dataset_name, &result) == DatasetOutcome::AlreadyComplete {
                continue;
            }

            total_chunks += result.chunks.len();
            total_entities += result.entities.len();
            total_edges += result.edges.len();
            total_summaries += result.summaries.len();
            total_embeddings += result.embeddings.len();
        }

        info!(
            "Cognify completed. chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
            total_chunks, total_entities, total_edges, total_summaries, total_embeddings
        );

        Ok(())
    })
}

pub(crate) async fn resolve_dataset_names(
    database: &DatabaseConnection,
    owner_id: Uuid,
    requested_datasets: Vec<String>,
) -> Result<Vec<String>, CliError> {
    if !requested_datasets.is_empty() {
        return Ok(requested_datasets);
    }

    let datasets = ops::datasets::list_datasets_by_owner(database, owner_id)
        .await
        .map_err(|error| {
            CliError::Runtime(format!(
                "Failed to list datasets for owner {owner_id}: {error}"
            ))
        })?;

    if datasets.is_empty() {
        return Err(CliError::Validation(format!(
            "No datasets found for owner {owner_id}. Add data first or pass --datasets."
        )));
    }

    Ok(datasets.into_iter().map(|dataset| dataset.name).collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! SDK-700 gap 1: the per-dataset console reporting is wired, not just
    //! formatted. `cli_e2e.rs` pins what `format_failure_summary` renders;
    //! these pin that `report_dataset_outcome` actually emits it at `WARN`,
    //! by capturing the tracing events it produces.

    use std::sync::{Arc, Mutex};

    use cognee::cognify::{FailureStage, StageFailure};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Subscriber};
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    use super::*;

    /// Every event emitted while a capture is installed, as `(level, message)`.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<(Level, String)>>>);

    struct MessageVisitor(String);

    impl Visit for MessageVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S: Subscriber> Layer<S> for Captured {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), visitor.0));
        }
    }

    /// Run `report_dataset_outcome` under a capturing subscriber.
    fn report_captured(
        dataset_name: &str,
        result: &CognifyResult,
    ) -> (DatasetOutcome, Vec<(Level, String)>) {
        let captured = Captured::default();
        let subscriber = Registry::default().with(captured.clone());
        let outcome = tracing::subscriber::with_default(subscriber, || {
            report_dataset_outcome(dataset_name, result)
        });
        let events = captured.0.lock().unwrap().clone();
        (outcome, events)
    }

    fn warnings(events: &[(Level, String)]) -> Vec<&str> {
        events
            .iter()
            .filter(|(level, _)| *level == Level::WARN)
            .map(|(_, message)| message.as_str())
            .collect()
    }

    #[test]
    fn report_dataset_outcome_warns_the_failure_summary_for_a_tolerated_failure() {
        let mut result = CognifyResult::empty();
        result.failures.record(StageFailure {
            stage: FailureStage::GraphExtraction,
            data_id: Uuid::from_u128(1),
            chunk_id: Some(Uuid::from_u128(0xC0FFEE)),
            error: "llm refused".to_string(),
            fails_item: true,
        });
        result.failures.note_totals(4, 8);
        let expected = format_failure_summary("papers", &result.failures)
            .expect("a recorded failure renders a summary");

        let (outcome, events) = report_captured("papers", &result);

        assert_eq!(outcome, DatasetOutcome::Ran);
        assert_eq!(
            warnings(&events),
            [expected.as_str()],
            "exactly one WARN, carrying the failure summary; got events: {events:?}"
        );
    }

    #[test]
    fn report_dataset_outcome_does_not_warn_for_a_clean_run() {
        let (outcome, events) = report_captured("papers", &CognifyResult::empty());

        assert_eq!(outcome, DatasetOutcome::Ran);
        assert!(
            warnings(&events).is_empty(),
            "a clean run must not warn; got events: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|(_, message)| message.contains(FAILURE_SUMMARY_MARKER)),
            "a clean run leaked the failure marker: {events:?}"
        );
    }

    #[test]
    fn report_dataset_outcome_skips_an_already_complete_dataset() {
        let prior = Uuid::from_u128(0xABC);
        let (outcome, events) = report_captured("papers", &CognifyResult::already_completed(prior));

        assert_eq!(outcome, DatasetOutcome::AlreadyComplete);
        assert!(
            events.iter().any(|(level, message)| *level == Level::INFO
                && message.contains(&format!(
                    "Dataset 'papers': already complete (prior pipeline_run_id={prior})"
                ))),
            "the short-circuit verdict must be reported; got events: {events:?}"
        );
        assert!(
            warnings(&events).is_empty(),
            "an already-complete dataset has nothing to warn about: {events:?}"
        );
    }
}
