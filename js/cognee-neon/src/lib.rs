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
mod default_subscriber;
mod error;
mod errors;
mod logging;
mod pipeline;
mod pipeline_exec;
mod progress;
mod run_handle;
mod runtime;
mod sdk;
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

    // Logging entrypoint (gap-06): argument-less, idempotent.
    cx.export_function("setupLogging", logging::setup_logging)?;

    // Telemetry (OTLP) entrypoint (gap-07 task 05): argument-less,
    // idempotent. Composes the OTEL layer on top of the default
    // stderr subscriber installed above.
    cx.export_function("setupTelemetry", telemetry_otlp::setup_telemetry)?;

    // Analytics entrypoint (gap-07 task 06): argument-less, idempotent.
    // Arms `send_telemetry` per the Neon default policy (ON unless
    // TELEMETRY_DISABLED / ENV in {test,dev} / COGNEE_HOST_SDK is set).
    // Decisions 10, 11, 12.
    cx.export_function(
        "setupTelemetryAnalytics",
        telemetry_analytics::setup_telemetry_analytics,
    )?;

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
