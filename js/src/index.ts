/**
 * Environment variables consumed by the Rust core on import:
 *
 *   COGNEE_BINDING_SUPPRESS_LOGS=1  — suppress the default
 *     tracing-subscriber stderr install (gap 07 decision 1). Set
 *     before `require`ing this module if your host owns its logger.
 *
 * After import, call `setupLogging()` to add file logging,
 * `setupTelemetry()` to add OTLP export, and
 * `setupTelemetryAnalytics()` to enable product-analytics emission.
 */
import { native } from "./native";

// Runtime
export function init(): void {
  native.init();
}
export function initWithThreads(n: number): void {
  native.initWithThreads(n);
}
export function shutdown(): void {
  native.shutdown();
}

/**
 * Initialize cognee's logging subsystem from environment variables.
 *
 * All configuration is via env vars (`COGNEE_LOG_*`, `LOG_FILE_NAME`,
 * `LOG_LEVEL`, `RUST_LOG`); set them before calling. Calling this
 * function more than once is a no-op (idempotent).
 */
export function setupLogging(): void {
  native.setupLogging();
}

// Re-exports
export { CogneeValue } from "./value";
export {
  TaskFn,
  IterTaskFn,
  BatchTaskFn,
  TaskOptions,
  TaskInfo,
  createTask,
  createIterTask,
  createBatchTask,
} from "./task";
export { TaskContext } from "./task-context";
export { Pipeline, RetryPolicy } from "./pipeline";
export {
  CancellationHandle,
  CancellationToken,
  createCancellationPair,
} from "./cancellation";
export { ProgressToken } from "./progress";
export {
  WatcherEvents,
  Watcher,
  createWatcher,
  createNoopWatcher,
} from "./watcher";
export { RunHandle } from "./run-handle";
export { RunResult } from "./run-result";
