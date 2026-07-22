//! Memify pipeline orchestration.
//!
//! The `memify` function extracts triplets from an existing knowledge graph
//! and indexes them into the vector database for semantic search.

use std::sync::Arc;

use cognee_core::pipeline::DataIdFn;
use cognee_core::pipeline_run_registry::DbPipelineWatcher;
use cognee_core::task::Value;
use cognee_core::{
    CpuPool, Pipeline, PipelineBuilder, PipelineContext, TaskContextBuilder, TypedTask,
};
use cognee_database::{DatabaseConnection, PipelineRunRepository};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::{Entity, Triplet};
use cognee_vector::VectorDB;
use tracing::info;
use uuid::Uuid;

use super::config::MemifyConfig;
use super::error::MemifyError;
use super::extract_triplets::extract_triplets_from_graph_db;
use super::index_triplets::{IndexResult, index_triplets};
use crate::qualification::{Qualification, check_pipeline_run_qualification};

/// Result of the memify pipeline.
#[derive(Debug, Clone)]
pub struct MemifyResult {
    /// Number of triplets extracted from the graph.
    pub triplet_count: usize,

    /// Details about vector indexing.
    pub index_result: IndexResult,

    /// `true` when this result was synthesised by the
    /// `check_pipeline_run_qualification` short-circuit (latest
    /// `pipeline_runs` row was `COMPLETED`). All other fields are zero.
    pub already_completed: bool,

    /// The `pipeline_run_id` of the prior completed run that triggered the
    /// short-circuit. `None` on normal results.
    pub prior_pipeline_run_id: Option<Uuid>,
}

impl MemifyResult {
    /// Create an empty result with zeroed counts.
    pub fn empty() -> Self {
        Self {
            triplet_count: 0,
            index_result: IndexResult {
                indexed_count: 0,
                batch_count: 0,
            },
            already_completed: false,
            prior_pipeline_run_id: None,
        }
    }

    /// Create a short-circuit "already completed" result tagged with the
    /// prior `pipeline_run_id`. Mirrors
    /// [`crate::CognifyResult::already_completed`]. See doc 08-08 §4.4.
    pub fn already_completed(pipeline_run_id: Uuid) -> Self {
        Self {
            already_completed: true,
            prior_pipeline_run_id: Some(pipeline_run_id),
            ..Self::empty()
        }
    }
}

// ---------------------------------------------------------------------------
// Typed task: Vec<Triplet> -> IndexResult
// ---------------------------------------------------------------------------

/// Build the one-task closure that indexes a pre-extracted batch of
/// [`Triplet`]s into the vector database.
///
/// Locked Decision 8 (LIB-06, 2026-05-13): memify's executor-routed
/// pipeline is a single "index-only" task whose input is the
/// `Vec<Triplet>` produced by the pre-flight extraction step in
/// [`memify`].
fn make_index_triplets_task(
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) -> TypedTask<Vec<Triplet>, IndexResult> {
    TypedTask::async_fn(move |triplets: &Vec<Triplet>, _ctx| {
        let triplets = triplets.clone();
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        Box::pin(async move {
            index_triplets(
                &triplets,
                &*vector_db,
                &*embedding_engine,
                dataset_id,
                user_id,
                tenant_id,
            )
            .await
            .map(Box::new)
            .map_err(|e| format!("{e}").into())
        })
    })
}

/// Build the executor-routed memify pipeline.
///
/// One task: `Vec<Triplet>` -> [`IndexResult`]. Triplet extraction
/// (graph-DB query or custom-data synthesis) and the empty-triplets
/// short-circuit happen *outside* this pipeline in [`memify`] — see
/// locked Decision 8 of [LIB-06-02][doc].
///
/// [doc]: ../../../../docs/telemetry/lib-06/02-memify-executor-route.md
pub fn build_memify_index_only_pipeline(
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) -> Pipeline {
    // Decision 4 (LIB-06): memify has no `Data` inputs. The watcher's
    // run_info `data_ids` carrier stays empty (Python's `"None"` branch).
    let data_id_fn: DataIdFn = Arc::new(|_v: Arc<dyn Value>| None);
    PipelineBuilder::new_with_task(
        "memify",
        make_index_triplets_task(vector_db, embedding_engine, dataset_id, user_id, tenant_id),
    )
    .with_name("memify")
    .with_data_id(data_id_fn)
    .build()
}

/// Run the memify pipeline: extract triplets from the graph and index them.
///
/// # Algorithm
/// 1. Validate configuration.
/// 2. Pre-flight: extract triplets from the graph database (or synthesise
///    them from `config.custom_data`).
/// 3. If no triplets found, return early with zeros.
/// 4. Build the one-task index-only pipeline (Decision 8) and route through
///    [`cognee_core::pipeline::execute`] with a
///    [`cognee_core::pipeline_run_registry::DbPipelineWatcher`] backed by
///    the caller-supplied `pipeline_run_repo` (Decision 11, gap 08-07).
/// 5. Downcast the executor output back to [`IndexResult`] and return a
///    [`MemifyResult`].
///
/// # Arguments
/// * `graph_db` — Graph database containing the knowledge graph.
/// * `vector_db` — Vector database for storing triplet embeddings.
/// * `embedding_engine` — Engine to generate text embeddings.
/// * `thread_pool` — CPU pool for [`cognee_core::TaskContext`] (LIB-06
///   Decision 1).
/// * `database` — Relational [`DatabaseConnection`] for
///   [`cognee_core::TaskContext`] (LIB-06 Decision 1).
/// * `dataset_id` — Optional dataset ID for metadata tagging.
/// * `user_id` — Optional user ID for metadata tagging.
/// * `tenant_id` — Optional tenant ID for metadata tagging.
/// * `config` — Pipeline configuration.
///
/// # Returns
/// A [`MemifyResult`] with counts of extracted and indexed triplets.
#[allow(clippy::too_many_arguments)]
pub async fn memify(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    thread_pool: Arc<dyn CpuPool>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    config: &MemifyConfig,
) -> Result<MemifyResult, MemifyError> {
    // 1. Validate configuration.
    config.validate()?;

    // 1b. Qualification gate (gap 08-08, locked decision 3) ────────────────
    // Skip when `dataset_id` is `None` — Python's gate only applies per
    // dataset, and ad-hoc memify runs without a dataset cannot be looked up
    // via `get_pipeline_run_by_dataset`. The pipeline name used here matches
    // what the executor-routed pipeline persists (`build_memify_index_only_pipeline`
    // sets `with_name("memify")`).
    let pipeline_name = "memify";
    if let Some(ds_id) = dataset_id {
        match check_pipeline_run_qualification(pipeline_run_repo.as_ref(), ds_id, pipeline_name)
            .await
            .map_err(|e| MemifyError::Database(e.to_string()))?
        {
            Qualification::AlreadyCompleted(prior) => {
                info!(
                    dataset_id = %ds_id,
                    pipeline_run_id = %prior.pipeline_run_id,
                    "memify: dataset already completed; short-circuiting (Python parity)"
                );
                return Ok(MemifyResult::already_completed(prior.pipeline_run_id));
            }
            Qualification::AlreadyRunning(_prior) => {
                return Err(MemifyError::PipelineAlreadyRunning {
                    pipeline_name: pipeline_name.to_string(),
                    dataset_id: Some(ds_id),
                });
            }
            Qualification::Proceed => {}
        }
    }

    // 2. Pre-flight: extract triplets from the graph database (or use custom data).
    let triplets = if let Some(ref custom_data) = config.custom_data {
        // When custom data is provided, convert JSON values to Triplet objects.
        // Each value should be a JSON object with "source_node", "relationship_name",
        // and "target_node" fields. UUIDs are generated deterministically from the
        // text values.
        let mut custom_triplets = Vec::new();
        for value in custom_data {
            let source = value
                .get("source_node")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let relationship = value
                .get("relationship_name")
                .and_then(|v| v.as_str())
                .unwrap_or("related_to")
                .to_string();
            let target = value
                .get("target_node")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            // Custom-triplet endpoints use the same `Entity::id_for` scheme as
            // graph entities, so a custom node connects to an existing entity
            // when its name matches that entity's LLM node id (which
            // `Entity::from_node` hashes). When the graph entity's node id
            // differs from its display name, the ids won't coincide — but this
            // is still strictly better than the previous bare `to_lowercase`
            // hash, which shared no id space with entities at all.
            let source_id = Entity::id_for(&source);
            let target_id = Entity::id_for(&target);
            let text = format!("{source}-\u{203A}{relationship}-\u{203A}{target}");
            custom_triplets.push(
                Triplet::new(source_id, target_id, relationship, text).with_names(source, target),
            );
        }
        info!(
            "Using {} custom triplets instead of graph extraction",
            custom_triplets.len()
        );
        custom_triplets
    } else {
        extract_triplets_from_graph_db(&*graph_db, config).await?
    };

    let triplet_count = triplets.len();

    // 3. If empty, return early with zeros — skip the executor entirely.
    if triplets.is_empty() {
        info!("No triplets extracted from graph; nothing to index");
        return Ok(MemifyResult::empty());
    }

    // 4. Build the one-task index-only pipeline and run it through
    //    `pipeline::execute` (Decision 8 / 11).
    let pipeline = build_memify_index_only_pipeline(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        dataset_id,
        user_id,
        tenant_id,
    );

    // The executor re-derives `PipelineRunInfo.pipeline_id` from
    // `(pipeline.name, user_id, dataset_id)`; we still carry `pipeline.id`
    // through `PipelineContext` as the placeholder.
    let pipeline_ctx = PipelineContext {
        pipeline_id: pipeline.id,
        pipeline_name: pipeline.name.clone().unwrap_or_default(),
        user_id,
        tenant_id,
        dataset_id,
        current_data: None,
        run_id: None,
        user_email: None,
        provenance_visited: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    };

    let (_cancel_handle, ctx) = TaskContextBuilder::new()
        .thread_pool(thread_pool)
        .database(database)
        .graph_db(Arc::clone(&graph_db))
        .vector_db(Arc::clone(&vector_db))
        .pipeline_context(pipeline_ctx)
        .build()
        .map_err(|e| MemifyError::Context(e.to_string()))?;
    let ctx = Arc::new(ctx);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(triplets) as Arc<dyn Value>];

    // Decision 11 (gap 08-07): persist the four-state `pipeline_runs` trail
    // through the caller-supplied repository.
    let watcher = DbPipelineWatcher::new(pipeline_run_repo);
    let outputs = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &watcher)
        .await
        .map_err(|e| MemifyError::Execute(e.to_string()))?;

    let index_result = extract_memify_outputs(outputs)?;

    // 5. Log summary and return.
    info!(
        "Memify complete: {} triplets extracted, {} indexed",
        triplet_count, index_result.indexed_count
    );

    Ok(MemifyResult {
        triplet_count,
        index_result,
        already_completed: false,
        prior_pipeline_run_id: None,
    })
}

// ---------------------------------------------------------------------------
// Output extraction (Decision 9)
// ---------------------------------------------------------------------------

/// Downcast the executor's [`Arc<dyn Value>`] outputs back to the concrete
/// [`IndexResult`] the memify convenience function promises.
///
/// Returns [`MemifyError::OutputTypeMismatch`] when the downcast fails — a
/// programmer error indicating the pipeline's last task does not emit
/// `IndexResult`.
fn extract_memify_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<IndexResult, MemifyError> {
    let first = outputs
        .into_iter()
        .next()
        .ok_or(MemifyError::OutputTypeMismatch {
            expected: "IndexResult",
            actual: "empty",
        })?;
    // Explicit deref through `Arc` to reach the inner `dyn Value`, then call
    // `as_any` via vtable dispatch — mirrors the pattern in
    // `cognee_ingestion::pipeline::extract_data_outputs` (LIB-06-01).
    (*first)
        .as_any()
        .downcast_ref::<IndexResult>()
        .cloned()
        .ok_or(MemifyError::OutputTypeMismatch {
            expected: "IndexResult",
            actual: "unknown",
        })
}
