//! Dataset resolution trait and `cognify_datasets` entry point.
//!
//! Mirrors Python's `resolve_authorized_user_datasets` + per-dataset loop
//! in `cognee/modules/pipelines/operations/pipeline.py`.
//!
//! The [`DatasetResolver`] trait abstracts how dataset names are turned into
//! concrete [`Dataset`] and [`Data`] objects so the cognify pipeline stays
//! independent of any specific database backend.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use cognee_core::CpuPool;
use cognee_database::ops::pipeline_runs::{create_pipeline_run, get_latest_pipeline_run};
use cognee_database::{DatabaseConnection, PipelineRun, PipelineRunRepository, PipelineRunStatus};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_models::{Data, Dataset};
use cognee_ontology::OntologyResolver;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::info;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::pipeline::CognifyResult;
use crate::rollback;
use crate::tasks::cognify;

/// Pipeline name used for cognify pipeline run records (matches Python
/// convention). Aliased to the single source of truth so the pipeline-cache
/// lookups below cannot drift from what the executor actually persists.
use crate::tasks::COGNIFY_PIPELINE_STAMP_NAME as COGNIFY_PIPELINE_NAME;

// ---------------------------------------------------------------------------
// DatasetRef — identify a dataset by name or UUID
// ---------------------------------------------------------------------------

/// Reference to a dataset, either by name or by UUID.
///
/// Mirrors Python's `Union[str, list[str], list[UUID]]` parameter on `cognify()`.
#[derive(Debug, Clone)]
pub enum DatasetRef {
    /// Identify a dataset by its human-readable name.
    ByName(String),
    /// Identify a dataset by its UUID.
    ById(Uuid),
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Resolve dataset names (or all datasets) to concrete [`Dataset`] and
/// [`Data`] objects.
///
/// Implementations are expected to enforce authorization (the `permission`
/// parameter mirrors Python's `get_authorized_existing_datasets`).
#[async_trait]
pub trait DatasetResolver: Send + Sync {
    /// Resolve dataset names to [`Dataset`] objects for a given user.
    ///
    /// * If `datasets` is empty, implementations should return **all** datasets
    ///   the user has access to (matching Python behaviour when `datasets=None`).
    /// * `permission` is a hint for access control (e.g. `"read"`, `"write"`).
    async fn resolve_datasets(
        &self,
        datasets: &[String],
        user_id: Uuid,
        permission: &str,
    ) -> Result<Vec<Dataset>, CognifyError>;

    /// Return all [`Data`] items attached to the given dataset.
    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, CognifyError>;

    /// Resolve a single dataset by its UUID.
    ///
    /// Default implementation returns `None` (not supported). Implementors
    /// backed by a real database should override.
    async fn resolve_dataset_by_id(
        &self,
        _id: Uuid,
        _user_id: Uuid,
        _permission: &str,
    ) -> Result<Option<Dataset>, CognifyError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Shared per-dataset helpers
// ---------------------------------------------------------------------------

/// Whether the pipeline cache may skip `dataset_id` entirely.
///
/// Reads the whole latest row rather than its status alone: a run that
/// completed with files still outstanding is *not* a cache hit — the next run
/// has exactly those files to redo — and reading the status and the `run_info`
/// with two queries would let the two answers come from different rows.
async fn dataset_is_cached(
    database: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<bool, CognifyError> {
    let latest = get_latest_pipeline_run(database, COGNIFY_PIPELINE_NAME, dataset_id).await?;
    Ok(latest.is_some_and(|run| {
        matches!(run.status, PipelineRunStatus::Completed)
            && !rollback::run_info_has_outstanding_failures(run.run_info.as_ref())
    }))
}

/// Append the dataset-level `COMPLETED` row this loop writes after a run.
///
/// Carries the run's *real* `pipeline_run_id` whenever the run reported one.
/// This row is the latest one for the dataset, so a cache check reads it
/// rather than the executor's; with a fabricated id it named a run no
/// ownership row belongs to, and its `run_info` said nothing at all about the
/// run's failures. Both now match what the run actually did.
///
/// One edge to that claim, stated rather than papered over: **a result flagged
/// `already_completed` is not a run**, so nothing is appended. Either the
/// pipeline cache short-circuited it or the completion markers covered every
/// item; in both cases the run that did the work already has its own rows, and
/// appending one here would put a fabricated id at the head of the dataset's
/// trail — exactly what carrying the real id exists to avoid.
///
/// `data_ids` is the dataset's items as this loop found them, which is what it
/// has to hand before the run. When completion markers skip part of the
/// dataset it is a superset of what the run actually processed; the executor's
/// own rows, written from the filtered input, carry the exact set.
async fn record_dataset_run(
    database: &DatabaseConnection,
    dataset_id: Uuid,
    data_ids: &[Uuid],
    result: &CognifyResult,
) -> Result<(), CognifyError> {
    if result.already_completed {
        return Ok(());
    }
    let pipeline_run_id = result.pipeline_run_id.unwrap_or_else(Uuid::new_v4);
    let run_info = if result.failures.is_empty() {
        // A clean run's row keeps exactly the shape it had before this
        // change.
        None
    } else {
        Some(rollback::run_info_with_failures(data_ids, &result.failures))
    };
    let run = PipelineRun {
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        status: PipelineRunStatus::Completed,
        pipeline_run_id,
        pipeline_name: COGNIFY_PIPELINE_NAME.to_string(),
        pipeline_id: pipeline_run_id,
        dataset_id: Some(dataset_id),
        run_info,
    };
    create_pipeline_run(database, run).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// cognify_datasets
// ---------------------------------------------------------------------------

/// High-level entry point: resolve dataset names, then run [`cognify`] for
/// each dataset.
///
/// This mirrors the Python `cognify(datasets, user, ...)` API which:
/// 1. Resolves dataset names to `Dataset` objects via the database.
/// 2. For each dataset, fetches its `Data` items.
/// 3. Runs the full cognify pipeline per dataset.
///
/// Empty datasets (no data items) are silently skipped.
#[allow(clippy::too_many_arguments)]
pub async fn cognify_datasets(
    dataset_names: Vec<String>,
    user_id: Uuid,
    tenant_id: Option<Uuid>,
    resolver: Arc<dyn DatasetResolver>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<Vec<CognifyResult>, CognifyError> {
    let datasets = resolver
        .resolve_datasets(&dataset_names, user_id, "read")
        .await?;

    info!(
        dataset_count = datasets.len(),
        "Resolved {} dataset(s) for cognify",
        datasets.len()
    );

    let mut results = Vec::new();

    for dataset in &datasets {
        // --- Pipeline cache check ---
        if config.use_pipeline_cache && dataset_is_cached(&database, dataset.id).await? {
            info!(
                dataset_name = %dataset.name,
                dataset_id = %dataset.id,
                "Skipping already-processed dataset (pipeline cache hit)"
            );
            continue;
        }

        let data_items = resolver.get_dataset_data(dataset.id).await?;

        if data_items.is_empty() {
            info!(
                dataset_name = %dataset.name,
                dataset_id = %dataset.id,
                "Skipping empty dataset"
            );
            continue;
        }

        info!(
            dataset_name = %dataset.name,
            dataset_id = %dataset.id,
            data_items = data_items.len(),
            "Running cognify for dataset"
        );

        // Captured before the items move into the run: the row appended below
        // names them, the way the executor's own rows do.
        let data_ids: Vec<Uuid> = data_items.iter().map(|item| item.id).collect();

        let result = cognify(
            data_items,
            dataset.id,
            Some(user_id),
            None,
            tenant_id,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&database),
            Arc::clone(&pipeline_run_repo),
            Arc::clone(&thread_pool),
            Arc::clone(&ontology_resolver),
            config,
        )
        .await?;

        // --- Record successful pipeline run ---
        record_dataset_run(&database, dataset.id, &data_ids, &result).await?;

        results.push(result);
    }

    info!(
        "cognify_datasets complete: {} dataset(s) processed",
        results.len()
    );
    Ok(results)
}

/// Like [`cognify_datasets`], but accepts [`DatasetRef`] values (by name or
/// by UUID).
///
/// UUID-based refs are resolved via [`DatasetResolver::resolve_dataset_by_id`].
/// Name-based refs are collected and resolved via [`DatasetResolver::resolve_datasets`].
#[allow(clippy::too_many_arguments)]
pub async fn cognify_dataset_refs(
    refs: Vec<DatasetRef>,
    user_id: Uuid,
    tenant_id: Option<Uuid>,
    resolver: Arc<dyn DatasetResolver>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<Vec<CognifyResult>, CognifyError> {
    // Split refs into name-based and id-based.
    let mut names = Vec::new();
    let mut id_datasets = Vec::new();

    for r in refs {
        match r {
            DatasetRef::ByName(n) => names.push(n),
            DatasetRef::ById(id) => {
                let ds = resolver
                    .resolve_dataset_by_id(id, user_id, "read")
                    .await?
                    .ok_or_else(|| {
                        CognifyError::DatasetResolutionError(format!(
                            "Dataset with id {id} not found"
                        ))
                    })?;
                id_datasets.push(ds);
            }
        }
    }

    // Resolve name-based refs.
    let name_datasets = resolver.resolve_datasets(&names, user_id, "read").await?;

    // Merge both sets and delegate to the core loop via cognify_datasets.
    // To avoid duplicating the per-dataset loop, we just call cognify_datasets
    // with a fake name list (empty) and handle both sets directly.
    let mut all_datasets = name_datasets;
    all_datasets.extend(id_datasets);

    info!(
        dataset_count = all_datasets.len(),
        "Resolved {} dataset(s) for cognify (via refs)",
        all_datasets.len()
    );

    let mut results = Vec::new();
    for dataset in &all_datasets {
        if config.use_pipeline_cache && dataset_is_cached(&database, dataset.id).await? {
            info!(
                dataset_name = %dataset.name,
                dataset_id = %dataset.id,
                "Skipping already-processed dataset (pipeline cache hit)"
            );
            continue;
        }

        let data_items = resolver.get_dataset_data(dataset.id).await?;
        if data_items.is_empty() {
            info!(
                dataset_name = %dataset.name,
                dataset_id = %dataset.id,
                "Skipping empty dataset"
            );
            continue;
        }

        info!(
            dataset_name = %dataset.name,
            dataset_id = %dataset.id,
            data_items = data_items.len(),
            "Running cognify for dataset"
        );

        // Captured before the items move into the run: the row appended below
        // names them, the way the executor's own rows do.
        let data_ids: Vec<Uuid> = data_items.iter().map(|item| item.id).collect();

        let result = cognify(
            data_items,
            dataset.id,
            Some(user_id),
            None,
            tenant_id,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&database),
            Arc::clone(&pipeline_run_repo),
            Arc::clone(&thread_pool),
            Arc::clone(&ontology_resolver),
            config,
        )
        .await?;

        record_dataset_run(&database, dataset.id, &data_ids, &result).await?;

        results.push(result);
    }

    info!(
        "cognify_dataset_refs complete: {} dataset(s) processed",
        results.len()
    );
    Ok(results)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// A trivial in-memory resolver for testing.
    struct MockResolver {
        datasets: Vec<Dataset>,
        data: std::collections::HashMap<Uuid, Vec<Data>>,
    }

    #[async_trait]
    impl DatasetResolver for MockResolver {
        async fn resolve_datasets(
            &self,
            names: &[String],
            _user_id: Uuid,
            _permission: &str,
        ) -> Result<Vec<Dataset>, CognifyError> {
            if names.is_empty() {
                return Ok(self.datasets.clone());
            }
            Ok(self
                .datasets
                .iter()
                .filter(|ds| names.contains(&ds.name))
                .cloned()
                .collect())
        }

        async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, CognifyError> {
            Ok(self.data.get(&dataset_id).cloned().unwrap_or_default())
        }
    }

    #[test]
    fn test_mock_resolver_filters_by_name() {
        let owner = Uuid::new_v4();
        let ds1 = Dataset::new("alpha".to_string(), owner, None, Uuid::new_v4());
        let ds2 = Dataset::new("beta".to_string(), owner, None, Uuid::new_v4());
        let resolver = MockResolver {
            datasets: vec![ds1.clone(), ds2],
            data: std::collections::HashMap::new(),
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(resolver.resolve_datasets(&["alpha".to_string()], owner, "read"));
        let datasets = result.unwrap();
        assert_eq!(datasets.len(), 1);
        assert_eq!(datasets[0].name, "alpha");
    }

    #[test]
    fn test_mock_resolver_returns_all_when_empty() {
        let owner = Uuid::new_v4();
        let ds1 = Dataset::new("alpha".to_string(), owner, None, Uuid::new_v4());
        let ds2 = Dataset::new("beta".to_string(), owner, None, Uuid::new_v4());
        let resolver = MockResolver {
            datasets: vec![ds1, ds2],
            data: std::collections::HashMap::new(),
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(resolver.resolve_datasets(&[], owner, "read"));
        let datasets = result.unwrap();
        assert_eq!(datasets.len(), 2);
    }

    #[test]
    fn test_mock_resolver_get_data_empty_dataset() {
        let resolver = MockResolver {
            datasets: vec![],
            data: std::collections::HashMap::new(),
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(resolver.get_dataset_data(Uuid::new_v4()));
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_mock_resolver_get_data_with_items() {
        let dataset_id = Uuid::new_v4();
        let owner_id = Uuid::new_v4();
        let data_item = Data::builder(
            Uuid::new_v4(),
            "test.txt",
            "/storage/test.txt",
            "file://test.txt",
            "txt",
            "text/plain",
            "hash123",
            owner_id,
        )
        .build();

        let mut data_map = std::collections::HashMap::new();
        data_map.insert(dataset_id, vec![data_item]);

        let resolver = MockResolver {
            datasets: vec![Dataset::new(
                "ds".to_string(),
                owner_id,
                None,
                Uuid::new_v4(),
            )],
            data: data_map,
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(resolver.get_dataset_data(dataset_id));
        let items = result.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "test.txt");
    }
}
