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
use cognee_database::ops::pipeline_runs::{create_pipeline_run, get_latest_pipeline_status};
use cognee_database::{DatabaseConnection, PipelineRun, PipelineRunStatus};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_models::{Data, Dataset};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::info;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::pipeline::CognifyResult;
use crate::tasks::cognify;

/// Pipeline name used for cognify pipeline run records (matches Python convention).
const COGNIFY_PIPELINE_NAME: &str = "cognify_pipeline";

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
    db: Option<Arc<DatabaseConnection>>,
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
        if config.use_pipeline_cache
            && let Some(ref db_conn) = db
        {
            let status =
                get_latest_pipeline_status(db_conn, COGNIFY_PIPELINE_NAME, dataset.id).await?;
            if matches!(status, Some(PipelineRunStatus::Completed)) {
                info!(
                    dataset_name = %dataset.name,
                    dataset_id = %dataset.id,
                    "Skipping already-processed dataset (pipeline cache hit)"
                );
                continue;
            }
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

        let result = cognify(
            data_items,
            dataset.id,
            Some(user_id),
            tenant_id,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            db.clone(),
            config,
        )
        .await?;

        // --- Record successful pipeline run ---
        if let Some(ref db_conn) = db {
            let pipeline_run_id = Uuid::new_v4();
            let run = PipelineRun {
                id: Uuid::new_v4(),
                created_at: Utc::now(),
                status: PipelineRunStatus::Completed,
                pipeline_run_id,
                pipeline_name: COGNIFY_PIPELINE_NAME.to_string(),
                pipeline_id: pipeline_run_id,
                dataset_id: dataset.id,
                run_info: None,
            };
            create_pipeline_run(db_conn, run).await?;
        }

        results.push(result);
    }

    info!(
        "cognify_datasets complete: {} dataset(s) processed",
        results.len()
    );
    Ok(results)
}

#[cfg(test)]
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
