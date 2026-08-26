pub mod add;
pub mod add_and_cognify;
#[cfg(feature = "bench")]
pub mod bench;
#[cfg(feature = "profiling")]
pub mod bench_telemetry;
pub mod cognify;
pub mod config;
pub mod delete;
pub mod forget;
pub mod improve;
pub mod memify;
pub mod recall;
pub mod remember;
pub mod run_sequence;
pub mod search;
#[cfg(feature = "visualization")]
pub mod visualize;

use std::sync::Arc;

use cognee::database::{DeleteDb, PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee::delete::DeleteService;
use cognee::{ComponentManager, PipelineContext};

use crate::error::CliError;

/// Build the `DeleteService` shared by every deletion path in the CLI.
///
/// `delete`, `forget` and the `bench` dataset-delete phase must wire the same
/// backends or they stop doing (and, for the benchmark, measuring) the same
/// thing. Two parts are easy to omit and quietly change behaviour:
///
/// - the graph and vector backends — without them a deletion drops relational
///   rows only, which for the benchmark would turn a populated-dataset delete
///   into a metadata-row delete;
/// - the pipeline-runs repository — without it a deletion writes no fresh
///   `INITIATED` row. That only changes behaviour for a caller that enabled
///   `CognifyConfig::use_pipeline_cache`: since the cache became opt-in, a
///   `COMPLETED` row no longer short-circuits the default re-cognify path.
pub(crate) async fn build_delete_service(
    cm: &Arc<ComponentManager>,
) -> Result<DeleteService, CliError> {
    let storage = cm
        .storage()
        .await
        .map_err(|e| CliError::Runtime(format!("{e}")))?;
    let database = cm
        .database()
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

    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

    Ok(DeleteService::new(storage, database as Arc<dyn DeleteDb>)
        .with_graph_db(graph_db)
        .with_vector_db(vector_db)
        .with_pipeline_run_repo(pipeline_run_repo))
}
