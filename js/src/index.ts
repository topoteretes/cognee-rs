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
