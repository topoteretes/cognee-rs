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

/**
 * Initialize OpenTelemetry / OTLP export from environment variables.
 *
 * Reads `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
 * `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and related
 * `OTEL_*` env vars. When neither `COGNEE_TRACING_ENABLED=true` nor a
 * non-empty `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the call returns
 * successfully without installing anything (no-config = no-op).
 *
 * When `OTEL_SERVICE_NAME` is unset, defaults to `cognee.node-binding`
 * (gap 07 decision 8). The user's explicit value always wins.
 *
 * Calling more than once is a no-op (idempotent).
 */
export function setupTelemetry(): void {
  native.setupTelemetry();
}

/**
 * Arm cognee product-analytics emission for this Node.js process.
 *
 * Default policy (gap 07 decision 11): ON unless `TELEMETRY_DISABLED`
 * is set, `ENV` is `"test"`/`"dev"`, or `COGNEE_HOST_SDK` is set —
 * Neon is the canonical sender in the JS ecosystem.
 *
 * Returns `true` if analytics were armed by this call (or a prior
 * call), `false` if the policy suppressed emission. Idempotent.
 */
export function setupTelemetryAnalytics(): boolean {
  return native.setupTelemetryAnalytics();
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
