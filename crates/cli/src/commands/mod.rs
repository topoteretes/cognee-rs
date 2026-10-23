pub mod add;
pub mod add_and_cognify;
#[cfg(feature = "bench")]
pub mod bench;
/// Peak-RSS sampling for the `bench` subcommand's memory report (SDK-507).
/// Gated with `bench` rather than `profiling`: unlike the flamegraph layer this
/// costs one syscall per phase, so it is always on for a bench run.
#[cfg(feature = "bench")]
pub mod bench_rss;
#[cfg(feature = "profiling")]
pub mod bench_telemetry;
pub mod cognify;
pub mod config;
pub mod delete;
pub mod edge_reindex;
pub mod export;
pub mod forget;
pub mod improve;
pub mod memify;
pub mod pipeline_unblock;
pub mod recall;
pub mod remember;
pub mod run_sequence;
pub mod search;
pub mod vector_reindex;
#[cfg(feature = "visualization")]
pub mod visualize;

use std::sync::Arc;

use cognee::database::{DeleteDb, PipelineRunRepository};
use cognee::delete::DeleteService;
use cognee::{ComponentManager, PipelineContext};

use crate::error::CliError;

/// Parse an optional `--tenant-id` flag — the one parser every command
/// (`add`, `remember`, `improve`, `forget`, `search`, `recall`, `cognify`,
/// `memify`, `add-and-cognify`, `pipeline-unblock`, `delete`) uses.
///
/// The tenant is applied by strict equality wherever a dataset is looked up
/// (search/recall scoping, `forget --dataset-name`): data written with
/// `--tenant-id T` is reachable only with the same `--tenant-id T`, and a
/// command without the flag reaches only tenant-less data (Python `get_dataset_ids.py:34`,
/// `get_all_user_permission_datasets.py:49-53` @37f788bd2).
pub fn parse_tenant_id(raw: Option<&str>) -> Result<Option<uuid::Uuid>, CliError> {
    raw.map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|error| CliError::Validation(format!("Invalid --tenant-id: {error}")))
}

/// Apply `--graph-schema` / `--graph-spec` / `--chunk-attachment` to `cfg`
/// and validate the result — the step `cognify` and `add-and-cognify` run
/// before building any component, so a bad flag fails fast with exit code 2.
///
/// The schema file is parsed by [`cognee::cognify::parse_graph_schema`] (`{}`
/// is the default model); a spec file is compiled into a schema by
/// [`cognee::cognify::graph_schema_from_spec_value`] (clap rejects both flags
/// together). The schema is compiled by `CognifyConfig::validate`, which also
/// rejects `--chunk-attachment` without `--graph-schema` and either with
/// `--temporal-cognify`.
pub(crate) fn apply_graph_schema_args(
    mut cfg: cognee::cognify::CognifyConfig,
    args: &crate::cli::GraphSchemaArgs,
) -> Result<cognee::cognify::CognifyConfig, CliError> {
    if let Some(path) = &args.graph_schema {
        let text = std::fs::read_to_string(path).map_err(|error| {
            CliError::Validation(format!(
                "Cannot read --graph-schema {}: {error}",
                path.display()
            ))
        })?;
        cfg.graph_schema = cognee::cognify::parse_graph_schema(&serde_json::Value::String(text))
            .map_err(|error| {
                CliError::Validation(format!(
                    "Invalid --graph-schema {}: {error}",
                    path.display()
                ))
            })?;
    }
    if let Some(path) = &args.graph_spec {
        let text = std::fs::read_to_string(path).map_err(|error| {
            CliError::Validation(format!(
                "Cannot read --graph-spec {}: {error}",
                path.display()
            ))
        })?;
        let schema =
            cognee::cognify::graph_schema_from_spec_value(&serde_json::Value::String(text))
                .map_err(|error| {
                    CliError::Validation(format!(
                        "Invalid --graph-spec {}: {error}",
                        path.display()
                    ))
                })?;
        cfg.graph_schema = Some(schema);
    }
    if let Some(attachment) = args.chunk_attachment {
        cfg = cfg.with_chunk_attachment(attachment.into());
    }
    validate_cognify_config(&cfg)?;
    Ok(cfg)
}

/// `CognifyConfig::validate` as a CLI validation error (exit code 2).
pub(crate) fn validate_cognify_config(
    cfg: &cognee::cognify::CognifyConfig,
) -> Result<(), CliError> {
    cfg.validate()
        .map_err(|error| CliError::Validation(format!("Invalid cognify options: {error}")))
}

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

    let pipeline_run_repo: Arc<dyn PipelineRunRepository> = Arc::clone(&database).pipeline_runs();

    let service = DeleteService::new(storage, database as Arc<dyn DeleteDb>)
        .with_graph_db(graph_db)
        .with_vector_db(vector_db)
        .with_pipeline_run_repo(pipeline_run_repo);
    // Per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`): deleting a
    // dataset drops its own databases.
    Ok(
        match cm
            .dataset_storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
        {
            Some(dataset_storage) => service.with_dataset_storage(dataset_storage),
            None => service,
        },
    )
}
