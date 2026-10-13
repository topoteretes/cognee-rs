/**
 * cognee — Node.js bindings for the cognee AI-memory SDK.
 *
 * Quick start:
 * ```ts
 * import { Cognee } from 'cognee';
 *
 * const c = new Cognee({ llmModel: "gpt-4o-mini" });
 * await c.warm();
 * await c.add({ type: "text", text: "…" }, "main");
 * await c.cognify("main");
 * const results = await c.search("my query");
 * ```
 *
 * Migration: Before: import { Pipeline } from '@cognee/pipeline'
 *            After:  import { pipeline } from 'cognee'
 *                    const { Pipeline } = pipeline
 *
 * Environment variables consumed by the Rust core on import:
 *
 *   COGNEE_BINDING_SUPPRESS_LOGS=1  — suppress the default
 *     tracing-subscriber stderr install (gap 07 decision 1). Set
 *     before `require`ing this module if your host owns its logger.
 *
 * After import, call `setupLogging()` to add file logging and
 * `setupTelemetry()` to add OTLP export. Product analytics are on by
 * default (opt-out); `setTelemetryEnabled(false)` turns them off for the
 * whole process.
 *
 * This module also attempts to load `.env` automatically (first from
 * `process.cwd()`, then from its parent directory). Set
 * `COGNEE_DISABLE_DOTENV=1` to disable auto-loading.
 */
import "./env";
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
 * Options for {@link setupLogging}. Each wins over the matching environment
 * variable. snake_case spellings (`log_level`) are accepted too.
 */
export interface LoggingOptions {
  /** `EnvFilter` directive (`debug`, `info,cognee=trace`). Env: `RUST_LOG` / `LOG_LEVEL`. */
  logLevel?: string;
  /** Log directory. Env: `COGNEE_LOGS_DIR`. Default `~/.cognee/logs`. */
  logsRootDirectory?: string;
  /** File sink on/off. Env: `COGNEE_LOG_FILE`. */
  logFile?: boolean;
  /** Log file path. Env: `LOG_FILE_NAME`. */
  logFileName?: string;
  /** Env: `COGNEE_LOG_ROTATION`. */
  logRotation?: "daily" | "hourly" | "minutely" | "never";
  /** Env: `COGNEE_LOG_FORMAT`. */
  logFormat?: "plain" | "json";
  /** Env: `COGNEE_LOG_BACKUP_COUNT`. */
  logBackupCount?: number;
  /** Env: `COGNEE_LOG_MAX_FILES`. */
  logMaxFiles?: number;
  /** Console stream. Default `stdout`. */
  consoleStream?: "stdout" | "stderr";
}

/**
 * Options for {@link setupTelemetry}: the `otel_*` settings, camelCased.
 * Each wins over the matching environment variable. snake_case spellings
 * (`otel_service_name`) are accepted too.
 */
export interface TelemetryOptions {
  /** Env: `COGNEE_TRACING_ENABLED`. */
  cogneeTracingEnabled?: boolean;
  /** Env: `OTEL_SERVICE_NAME`. Default `cognee.node-binding`. */
  otelServiceName?: string;
  /** Env: `OTEL_EXPORTER_OTLP_ENDPOINT` (non-empty also enables tracing). */
  otelExporterOtlpEndpoint?: string;
  /** Env: `OTEL_EXPORTER_OTLP_HEADERS`. */
  otelExporterOtlpHeaders?: string;
  /** `grpc` (default) or `http/protobuf`. Env: `OTEL_EXPORTER_OTLP_PROTOCOL`. */
  otelExporterOtlpProtocol?: string;
  /** `batch` (default) or `simple`. Env: `OTEL_SPAN_PROCESSOR`. */
  otelSpanProcessor?: string;
  /** Env: `OTEL_TRACES_SAMPLER`. */
  otelTracesSampler?: string;
  /** Env: `OTEL_TRACES_SAMPLER_ARG`. */
  otelTracesSamplerArg?: string;
  /** `deployment.environment.name`. Env: `ENV`. Default `development`. */
  otelDeploymentEnvironment?: string;
}

/**
 * Initialize cognee's logging subsystem.
 *
 * Logging is process-wide and configured here, not from a `Cognee` handle's
 * settings: values resolve as defaults < environment (`COGNEE_LOG_*`,
 * `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`) < `options`. Throws on an unknown
 * option or an invalid value. Calling this function more than once is a
 * no-op (idempotent; the first call wins).
 */
export function setupLogging(options?: LoggingOptions): void {
  native.setupLogging((options ?? null) as Record<string, unknown> | null);
}

/**
 * Initialize OpenTelemetry / OTLP export.
 *
 * Process-wide. Values resolve as the binding default service name
 * (`cognee.node-binding`) < environment (`COGNEE_TRACING_ENABLED`, `OTEL_*`,
 * `ENV`) < `options`. The environment is never modified. When tracing is
 * neither enabled nor given an endpoint, the call returns without installing
 * anything (no-config = no-op).
 *
 * Calling more than once is a no-op (idempotent; the first call wins).
 */
export function setupTelemetry(options?: TelemetryOptions): void {
  native.setupTelemetry((options ?? null) as Record<string, unknown> | null);
}

/**
 * Arm cognee product-analytics emission for this Node.js process (also done
 * automatically on import) and report whether analytics are enabled — the
 * same value as {@link isTelemetryEnabled}.
 *
 * Default policy: ON unless switched off with {@link setTelemetryEnabled},
 * `TELEMETRY_DISABLED` is set, `ENV` is `"test"`/`"dev"`, or
 * `COGNEE_HOST_SDK` is set.
 */
export function setupTelemetryAnalytics(): boolean {
  return native.setupTelemetryAnalytics();
}

/**
 * Turn product analytics on or off for the whole process (every `Cognee`
 * handle). Wins over `TELEMETRY_DISABLED` / `ENV` in the environment;
 * disabling also drops events queued but not yet sent.
 */
export function setTelemetryEnabled(enabled: boolean): void {
  native.setTelemetryEnabled(enabled);
}

/** Whether product analytics are currently enabled for this process. */
export function isTelemetryEnabled(): boolean {
  return native.isTelemetryEnabled();
}

// Re-exports
export { CogneeValue } from "./value";
export {
  TaskFn,
  IterTaskFn,
  BatchTaskFn,
  IterBatchTaskFn,
  StreamOutput,
  TaskOptions,
  TaskInfo,
  createTask,
  createIterTask,
  createBatchTask,
  createIterBatchTask,
  createAsyncTask,
  createAsyncBatchTask,
  createAsyncStreamTask,
  createAsyncStreamBatchTask,
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
export {
  CogneeError,
  ComponentError,
  ServiceBuildError,
  UserBootstrapError,
  RuntimeError,
  ValidationError,
  UnsupportedError,
  FeatureNotBuiltError,
  UnknownConfigKeyError,
  ConfigTypeMismatchError,
  wrapNativeError,
} from "./errors";

// ─────────────────────────────────────────────────────────────────────────────
// SDK-level exports (Phase 7)
// ─────────────────────────────────────────────────────────────────────────────

/** The main Cognee SDK class and sub-object types. Cloud module-level
 * functions (`serve` / `disconnect`) are not part of this package. */
export {
  Cognee,
  type CogneeConfigObject,
  type CogneeDatasetObject,
  type CogneeSessionObject,
  type CogneeNotebookObject,
  type CogneeUserObject,
} from "./cognee";

/** All user-facing Cognee* TypeScript types. */
export * from "./types";

/**
 * Low-level pipeline engine API (legacy).
 *
 * ```ts
 * import { pipeline } from 'cognee';
 * const p = new pipeline.Pipeline();
 * ```
 */
export * as pipeline from "./pipeline";
