//! Neon (Node.js) bindings for the cognee-core pipeline engine.
//!
//! This crate exposes the cognee-core API through Node.js native functions:
//!
//! - **Values**: Type-erased data containers
//! - **Tasks**: JS function → Rust Task bridging
//! - **Pipeline**: Builder + blocking/async/background execution
//! - **Context**: Task context with mock database backends
//! - **Cancellation**: Cooperative cancellation via handle/token pairs
//! - **Progress**: Lock-free progress tracking
//! - **Watcher**: Pipeline event observer via JS callbacks

mod cancellation;
mod config;
mod default_subscriber;
mod error;
mod errors;
mod json;
mod logging;
mod pipeline;
mod pipeline_exec;
mod progress;
mod run_handle;
mod runtime;
mod sdk;
mod sdk_admin;
mod sdk_data;
mod sdk_datasets;
mod sdk_memory;
mod sdk_ops;
mod sdk_retrieval;
mod sdk_visualization;
mod services;
mod task;
mod task_context;
mod task_info;
mod telemetry_analytics;
mod telemetry_otlp;
mod value;
mod watcher;

use neon::prelude::*;

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    // gap 07 decision 1: install the default stderr subscriber before
    // any function is registered so events emitted during export
    // setup are captured. Honours `COGNEE_BINDING_SUPPRESS_LOGS=1`
    // and is idempotent / composes with `setupLogging()` (gap 06)
    // via `try_init` semantics.
    default_subscriber::install();

    // Runtime
    cx.export_function("init", runtime::init)?;
    cx.export_function("initWithThreads", runtime::init_with_threads)?;
    cx.export_function("shutdown", runtime::shutdown)?;

    // SDK handle & service facade (Phase 1).
    cx.export_function("cogneeNew", sdk::cognee_new)?;
    cx.export_function("cogneeWarm", sdk::cognee_warm)?;
    cx.export_function("cogneeOwnerId", sdk::cognee_owner_id)?;

    // Pipeline ops (Phase 3): add / cognify / add-and-cognify.
    cx.export_function("cogneeAdd", sdk_ops::cognee_add)?;
    cx.export_function("cogneeCognify", sdk_ops::cognee_cognify)?;
    cx.export_function("cogneeAddAndCognify", sdk_ops::cognee_add_and_cognify)?;

    // Retrieval ops (Phase 4): search / recall.
    cx.export_function("cogneeSearch", sdk_retrieval::cognee_search)?;
    cx.export_function("cogneeRecall", sdk_retrieval::cognee_recall)?;

    // Memory ops (Phase 5): remember / remember_entry / memify / improve.
    cx.export_function("cogneeRemember", sdk_memory::cognee_remember)?;
    cx.export_function("cogneeRememberEntry", sdk_memory::cognee_remember_entry)?;
    cx.export_function("cogneeMemify", sdk_memory::cognee_memify)?;
    cx.export_function("cogneeImprove", sdk_memory::cognee_improve)?;

    // Data ops (Phase 5): forget / update / prune.
    cx.export_function("cogneeForget", sdk_data::cognee_forget)?;
    cx.export_function("cogneeUpdate", sdk_data::cognee_update)?;
    cx.export_function("cogneePruneData", sdk_data::cognee_prune_data)?;
    cx.export_function("cogneePruneSystem", sdk_data::cognee_prune_system)?;

    // Dataset manager ops (Phase 5).
    cx.export_function("cogneeListDatasets", sdk_datasets::cognee_list_datasets)?;
    cx.export_function("cogneeListData", sdk_datasets::cognee_list_data)?;
    cx.export_function("cogneeHasData", sdk_datasets::cognee_has_data)?;
    cx.export_function("cogneeDatasetStatus", sdk_datasets::cognee_dataset_status)?;
    cx.export_function("cogneeEmptyDataset", sdk_datasets::cognee_empty_dataset)?;
    cx.export_function("cogneeDeleteData", sdk_datasets::cognee_delete_data)?;
    cx.export_function(
        "cogneeDeleteAllDatasets",
        sdk_datasets::cognee_delete_all_datasets,
    )?;

    // Admin / session / pipeline-run / user / notebook ops (Phase 5).
    cx.export_function(
        "cogneeResetPipelineRunStatus",
        sdk_admin::cognee_reset_pipeline_run_status,
    )?;
    cx.export_function(
        "cogneeResetDatasetPipelineRunStatus",
        sdk_admin::cognee_reset_dataset_pipeline_run_status,
    )?;
    cx.export_function(
        "cogneeGetOrCreateDefaultUser",
        sdk_admin::cognee_get_or_create_default_user,
    )?;
    cx.export_function("cogneeListNotebooks", sdk_admin::cognee_list_notebooks)?;
    cx.export_function("cogneeCreateNotebook", sdk_admin::cognee_create_notebook)?;
    cx.export_function("cogneeUpdateNotebook", sdk_admin::cognee_update_notebook)?;
    cx.export_function("cogneeDeleteNotebook", sdk_admin::cognee_delete_notebook)?;
    cx.export_function("cogneeGetSession", sdk_admin::cognee_get_session)?;
    cx.export_function("cogneeAddFeedback", sdk_admin::cognee_add_feedback)?;
    cx.export_function("cogneeDeleteFeedback", sdk_admin::cognee_delete_feedback)?;
    cx.export_function("cogneeGetGraphContext", sdk_admin::cognee_get_graph_context)?;
    cx.export_function("cogneeSetGraphContext", sdk_admin::cognee_set_graph_context)?;

    // Visualization ops (Phase 6): render HTML string / write to file.
    // Always registered; throws FEATURE_NOT_BUILT when the feature is absent.
    cx.export_function("cogneeVisualize", sdk_visualization::cognee_visualize)?;
    cx.export_function(
        "cogneeVisualizeToFile",
        sdk_visualization::cognee_visualize_to_file,
    )?;

    // Cloud ops (`cogneeServe` / `cogneeDisconnect`) are exposed by the
    // closed `cognee-ts-cloud` cdylib (T15e), not by this OSS `cognee-ts-neon`
    // binding. The closed cdylib depends on `cognee-bindings-cloud`.

    // Config surface (Phase 2): granular + bulk + generic setters, read-back.
    // LLM
    cx.export_function("configSetLlmProvider", config::config_set_llm_provider)?;
    cx.export_function("configSetLlmModel", config::config_set_llm_model)?;
    cx.export_function("configSetLlmApiKey", config::config_set_llm_api_key)?;
    cx.export_function("configSetLlmEndpoint", config::config_set_llm_endpoint)?;
    cx.export_function("configSetLlmApiVersion", config::config_set_llm_api_version)?;
    cx.export_function(
        "configSetLlmTemperature",
        config::config_set_llm_temperature,
    )?;
    cx.export_function("configSetLlmStreaming", config::config_set_llm_streaming)?;
    cx.export_function(
        "configSetLlmMaxCompletionTokens",
        config::config_set_llm_max_completion_tokens,
    )?;
    cx.export_function("configSetLlmMaxRetries", config::config_set_llm_max_retries)?;
    cx.export_function(
        "configSetLlmMaxParallelRequests",
        config::config_set_llm_max_parallel_requests,
    )?;
    // Embedding
    cx.export_function(
        "configSetEmbeddingProvider",
        config::config_set_embedding_provider,
    )?;
    cx.export_function(
        "configSetEmbeddingModel",
        config::config_set_embedding_model,
    )?;
    cx.export_function(
        "configSetEmbeddingDimensions",
        config::config_set_embedding_dimensions,
    )?;
    cx.export_function(
        "configSetEmbeddingEndpoint",
        config::config_set_embedding_endpoint,
    )?;
    cx.export_function(
        "configSetEmbeddingApiKey",
        config::config_set_embedding_api_key,
    )?;
    cx.export_function(
        "configSetEmbeddingModelPath",
        config::config_set_embedding_model_path,
    )?;
    cx.export_function(
        "configSetEmbeddingTokenizerPath",
        config::config_set_embedding_tokenizer_path,
    )?;
    // Vector DB
    cx.export_function(
        "configSetVectorDbProvider",
        config::config_set_vector_db_provider,
    )?;
    cx.export_function("configSetVectorDbUrl", config::config_set_vector_db_url)?;
    cx.export_function("configSetVectorDbKey", config::config_set_vector_db_key)?;
    cx.export_function("configSetVectorDbHost", config::config_set_vector_db_host)?;
    cx.export_function("configSetVectorDbPort", config::config_set_vector_db_port)?;
    cx.export_function("configSetVectorDbName", config::config_set_vector_db_name)?;
    // Graph DB
    cx.export_function(
        "configSetGraphDatabaseProvider",
        config::config_set_graph_database_provider,
    )?;
    cx.export_function("configSetGraphModel", config::config_set_graph_model)?;
    cx.export_function("configSetGraphFilePath", config::config_set_graph_file_path)?;
    // Chunking
    cx.export_function("configSetChunkStrategy", config::config_set_chunk_strategy)?;
    cx.export_function("configSetChunkEngine", config::config_set_chunk_engine)?;
    cx.export_function("configSetChunkSize", config::config_set_chunk_size)?;
    cx.export_function("configSetChunkOverlap", config::config_set_chunk_overlap)?;
    // Paths
    cx.export_function(
        "configSetSystemRootDirectory",
        config::config_set_system_root_directory,
    )?;
    cx.export_function(
        "configSetDataRootDirectory",
        config::config_set_data_root_directory,
    )?;
    cx.export_function(
        "configSetCacheRootDirectory",
        config::config_set_cache_root_directory,
    )?;
    cx.export_function(
        "configSetLogsRootDirectory",
        config::config_set_logs_root_directory,
    )?;
    // Ontology
    cx.export_function(
        "configSetOntologyFilePath",
        config::config_set_ontology_file_path,
    )?;
    cx.export_function(
        "configSetOntologyResolver",
        config::config_set_ontology_resolver,
    )?;
    cx.export_function(
        "configSetOntologyMatchingStrategy",
        config::config_set_ontology_matching_strategy,
    )?;
    // Other
    cx.export_function(
        "configSetMonitoringTool",
        config::config_set_monitoring_tool,
    )?;
    cx.export_function(
        "configSetClassificationModel",
        config::config_set_classification_model,
    )?;
    cx.export_function(
        "configSetSummarizationModel",
        config::config_set_summarization_model,
    )?;
    // Generic + bulk + read-back
    cx.export_function("configSet", config::config_set)?;
    cx.export_function("configSetLlmConfig", config::config_set_llm_config)?;
    cx.export_function(
        "configSetEmbeddingConfig",
        config::config_set_embedding_config,
    )?;
    cx.export_function(
        "configSetVectorDbConfig",
        config::config_set_vector_db_config,
    )?;
    cx.export_function("configSetGraphDbConfig", config::config_set_graph_db_config)?;
    cx.export_function("getConfig", config::get_config)?;

    // Logging entrypoint (gap-06): argument-less, idempotent.
    cx.export_function("setupLogging", logging::setup_logging)?;

    // Telemetry (OTLP) entrypoint (gap-07 task 05): argument-less,
    // idempotent. Composes the OTEL layer on top of the default
    // stderr subscriber installed above.
    cx.export_function("setupTelemetry", telemetry_otlp::setup_telemetry)?;

    // Analytics entrypoint (gap-07 task 06): argument-less, idempotent.
    // Evaluates `send_telemetry` under the fail-closed policy (explicit
    // opt-in required; TELEMETRY_DISABLED / ENV / COGNEE_HOST_SDK suppress).
    // Decisions 10, 12.
    cx.export_function(
        "setupTelemetryAnalytics",
        telemetry_analytics::setup_telemetry_analytics,
    )?;
    // Evaluate analytics automatically on module load without granting
    // opt-in. Idempotent; `is_disabled()` remains authoritative per event.
    let _ = telemetry_analytics::arm();

    // Values
    cx.export_function("valueFromNumber", value::value_from_number)?;
    cx.export_function("valueFromBool", value::value_from_bool)?;
    cx.export_function("valueFromString", value::value_from_string)?;
    cx.export_function("valueFromBuffer", value::value_from_buffer)?;
    cx.export_function("valueAsNumber", value::value_as_number)?;
    cx.export_function("valueAsBool", value::value_as_bool)?;
    cx.export_function("valueAsString", value::value_as_string)?;
    cx.export_function("valueAsBuffer", value::value_as_buffer)?;
    cx.export_function("valueClone", value::value_clone)?;

    // Tasks
    cx.export_function("createTask", task::create_task)?;
    cx.export_function("createIterTask", task::create_iter_task)?;
    cx.export_function("createBatchTask", task::create_batch_task)?;

    // TaskInfo
    cx.export_function("taskInfoNew", task_info::task_info_new)?;

    // Pipeline
    cx.export_function("pipelineNew", pipeline::pipeline_new)?;
    cx.export_function("pipelineSetName", pipeline::pipeline_set_name)?;
    cx.export_function("pipelineAddTask", pipeline::pipeline_add_task)?;
    cx.export_function("pipelineSetBatchSize", pipeline::pipeline_set_batch_size)?;
    cx.export_function("pipelineSetConcurrency", pipeline::pipeline_set_concurrency)?;
    cx.export_function("pipelineSetRetry", pipeline::pipeline_set_retry)?;

    // Pipeline execution
    cx.export_function("pipelineExecute", pipeline_exec::pipeline_execute)?;
    cx.export_function(
        "pipelineExecuteAsync",
        pipeline_exec::pipeline_execute_async,
    )?;
    cx.export_function(
        "pipelineExecuteBackground",
        pipeline_exec::pipeline_execute_background,
    )?;
    cx.export_function(
        "pipelineExecuteWithWatcher",
        pipeline_exec::pipeline_execute_with_watcher,
    )?;

    // Run handle
    cx.export_function("runHandleIsFinished", run_handle::run_handle_is_finished)?;
    cx.export_function("runHandleAbort", run_handle::run_handle_abort)?;
    cx.export_function("runHandleWait", run_handle::run_handle_wait)?;

    // Task context
    cx.export_function("taskContextMock", task_context::task_context_mock)?;
    cx.export_function("taskContextClone", task_context::task_context_clone)?;

    // Cancellation
    cx.export_function("cancellationPair", cancellation::cancellation_pair_new)?;
    cx.export_function(
        "cancellationHandleCancel",
        cancellation::cancellation_handle_cancel,
    )?;
    cx.export_function(
        "cancellationHandleIsCancelled",
        cancellation::cancellation_handle_is_cancelled,
    )?;
    cx.export_function(
        "cancellationTokenIsCancelled",
        cancellation::cancellation_token_is_cancelled,
    )?;
    cx.export_function(
        "cancellationHandleClone",
        cancellation::cancellation_handle_clone,
    )?;
    cx.export_function(
        "cancellationTokenClone",
        cancellation::cancellation_token_clone,
    )?;

    // Progress
    cx.export_function("progressNew", progress::progress_new)?;
    cx.export_function("progressSet", progress::progress_set)?;
    cx.export_function("progressFraction", progress::progress_fraction)?;
    cx.export_function("progressWidth", progress::progress_width)?;
    cx.export_function("progressIsComplete", progress::progress_is_complete)?;
    cx.export_function("progressRootFraction", progress::progress_root_fraction)?;
    cx.export_function("progressSplit", progress::progress_split)?;
    cx.export_function("progressSubtoken", progress::progress_subtoken)?;
    cx.export_function("progressClone", progress::progress_clone)?;

    // Watcher
    cx.export_function("watcherNew", watcher::watcher_new)?;
    cx.export_function("watcherNoop", watcher::watcher_noop)?;

    Ok(())
}
