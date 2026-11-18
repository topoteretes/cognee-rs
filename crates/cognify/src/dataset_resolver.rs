//! Dataset resolution trait and `cognify_datasets` entry point.
//!
//! Mirrors Python's `resolve_authorized_user_datasets` + per-dataset loop
//! in `cognee/modules/pipelines/operations/pipeline.py`.
//!
//! The [`DatasetResolver`] trait abstracts how dataset names are turned into
//! concrete [`Dataset`] and [`Data`] objects so the cognify pipeline stays
//! independent of any specific database backend.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_core::CpuPool;
use cognee_database::{PipelineRunRepository, RelationalDb};
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
use crate::tasks::cognify;

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
/// Empty datasets (no data items) are silently skipped, and a dataset named
/// twice runs once.
///
/// The `pipeline_runs` trail is the one [`cognify`]'s executor writes
/// (`STARTED` → `COMPLETED`/`ERRORED`, sharing one `pipeline_run_id`); this
/// loop appends nothing of its own. Python's per-dataset loop writes no
/// dataset-level row either, so an extra `COMPLETED` here was a cross-SDK
/// row-shape divergence (SDK-946).
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
    database: Arc<dyn RelationalDb>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<Vec<CognifyResult>, CognifyError> {
    let datasets = resolver
        .resolve_datasets(&dataset_names, user_id, "read")
        .await?;

    cognify_resolved(
        datasets,
        "cognify_datasets",
        user_id,
        tenant_id,
        &*resolver,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        config,
    )
    .await
}

/// Like [`cognify_datasets`], but accepts [`DatasetRef`] values (by name or
/// by UUID).
///
/// UUID-based refs are resolved via [`DatasetResolver::resolve_dataset_by_id`].
/// Name-based refs are collected and resolved via [`DatasetResolver::resolve_datasets`].
/// A dataset reached more than once (by name and by id, or by a repeated ref)
/// runs once.
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
    database: Arc<dyn RelationalDb>,
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

    // Resolve name-based refs. An empty name list means "every dataset" to
    // `resolve_datasets`, which is right only when no ref was given at all:
    // id-only refs must not widen to the caller's whole dataset list (and
    // cognify the named ones twice).
    let name_datasets = if names.is_empty() && !id_datasets.is_empty() {
        Vec::new()
    } else {
        resolver.resolve_datasets(&names, user_id, "read").await?
    };

    let mut all_datasets = name_datasets;
    all_datasets.extend(id_datasets);

    cognify_resolved(
        all_datasets,
        "cognify_dataset_refs",
        user_id,
        tenant_id,
        &*resolver,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        config,
    )
    .await
}

/// The per-dataset loop both entry points share: run [`cognify`] once for each
/// distinct dataset, in the order given.
///
/// Duplicates are dropped by id, keeping the first occurrence, so a dataset
/// named twice, or by both name and id, runs once and yields one result.
/// Without this, the second pass found every item already completed but still
/// wrote a `STARTED`/`COMPLETED` pair and returned a second result. Python does
/// not dedupe here (`["docs", "docs"]` runs twice; mixed names and ids, or a
/// repeated id, raise instead), so this is stricter than Python on input
/// Python treats as an error or a repeat.
///
/// Empty datasets (no data items) are skipped. The loop writes no
/// `pipeline_runs` row of its own (SDK-946).
#[allow(clippy::too_many_arguments)]
async fn cognify_resolved(
    datasets: Vec<Dataset>,
    entry_point: &'static str,
    user_id: Uuid,
    tenant_id: Option<Uuid>,
    resolver: &dyn DatasetResolver,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<dyn RelationalDb>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<Vec<CognifyResult>, CognifyError> {
    let mut seen = HashSet::new();
    let datasets: Vec<Dataset> = datasets
        .into_iter()
        .filter(|dataset| seen.insert(dataset.id))
        .collect();

    info!(
        entry_point,
        dataset_count = datasets.len(),
        "Resolved {} dataset(s) for cognify",
        datasets.len()
    );

    let mut results = Vec::new();
    for dataset in &datasets {
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

        results.push(result);
    }

    info!(
        entry_point,
        "{entry_point} complete: {} dataset(s) processed",
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
