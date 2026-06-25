// Import all user-facing Cognee* types so they are available within this file.
// Re-exported below so that existing `import { CogneeDataInput, … } from './native'`
// paths continue to work without change.
import type {
  CogneeDataInput,
  CogneeAddOptions,
  CogneeAddResult,
  CogneeCognifyOptions,
  CogneeCognifyResult,
  SearchTypeString,
  RecallScopeString,
  CogneeSearchOptions,
  CogneeSearchResponse,
  CogneeRecallOptions,
  CogneeRecallResult,
  CogneeRememberOptions,
  CogneeRememberResult,
  CogneeMemoryEntry,
  CogneeMemifyOptions,
  CogneeMemifyResult,
  CogneeImproveOptions,
  CogneeImproveResult,
  CogneeForgetTarget,
  CogneeForgetResult,
  CogneeUpdateOptions,
  CogneeUpdateResult,
  CogneePruneSystemOptions,
  CogneePruneResult,
  CogneeDataset,
  CogneeData,
  CogneeDeleteResult,
  CogneeUser,
  CogneeNotebook,
  CogneeSessionQAEntry,
  CogneeVisualizeOptions,
} from "./types";

export type {
  CogneeDataInput,
  CogneeAddOptions,
  CogneeAddResult,
  CogneeCognifyOptions,
  CogneeCognifyResult,
  SearchTypeString,
  RecallScopeString,
  CogneeSearchOptions,
  CogneeSearchResponse,
  CogneeRecallOptions,
  CogneeRecallResult,
  CogneeRememberOptions,
  CogneeRememberResult,
  CogneeMemoryEntry,
  CogneeMemifyOptions,
  CogneeMemifyResult,
  CogneeImproveOptions,
  CogneeImproveResult,
  CogneeForgetTarget,
  CogneeForgetResult,
  CogneeUpdateOptions,
  CogneeUpdateResult,
  CogneePruneSystemOptions,
  CogneePruneResult,
  CogneeDataset,
  CogneeData,
  CogneeDeleteResult,
  CogneeUser,
  CogneeNotebook,
  CogneeSessionQAEntry,
  CogneeVisualizeOptions,
} from "./types";

/** Opaque native handle types returned by the Neon addon. */
export type NativeBox = object;

/** Shape of the native Neon module. */
export interface NativeBindings {
  // Runtime
  init(): void;
  initWithThreads(n: number): void;
  shutdown(): void;

  // SDK handle & service facade (Phase 1). `cogneeNew` is sync (no I/O);
  // `cogneeWarm`/`cogneeOwnerId` are async (build engines + resolve the
  // default user via Python default-user semantics).
  //
  // `cogneeNew` precedence is a true 3-way overlay: `defaults < env < object`.
  // With no/`null`/`undefined` argument the env-derived Settings are used.
  // With an object/JSON-string argument, ONLY the keys it provides override the
  // env-derived Settings; absent keys keep their env (or default) value.
  cogneeNew(settings?: object | string): NativeBox;
  cogneeWarm(handle: NativeBox): Promise<void>;
  cogneeOwnerId(handle: NativeBox): Promise<string>;

  // Pipeline ops (Phase 3). All async (build engines + run the pipeline).
  //
  // `dataInput` is a discriminated union (single item or an array):
  //   { type: "text"; text: string }
  //   { type: "file"; path: string }
  //   { type: "url"; url: string }        // resolved by AddPipeline; cognify/search need normal setup
  //   { type: "binary"; bytes: Buffer | number[] | string /* base64 */; name: string }
  // (`name` is REQUIRED for binary — used for MIME detection. `s3` and the
  // recursive `dataItem` variant are not supported.)
  //
  // `add` returns `{ added, deduplicated, … }`: `added` holds only the items
  // newly created by this call, `deduplicated` the ones that already existed.
  // An empty `added` array (`addedCount === 0`) means every submitted item was a
  // pre-existing duplicate.
  cogneeAdd(
    handle: NativeBox,
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeAddOptions
  ): Promise<CogneeAddResult>;
  cogneeCognify(
    handle: NativeBox,
    dataset: string,
    opts?: CogneeCognifyOptions
  ): Promise<CogneeCognifyResult>;
  cogneeAddAndCognify(
    handle: NativeBox,
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeAddOptions & CogneeCognifyOptions
  ): Promise<{ add: CogneeAddResult; cognify: CogneeCognifyResult }>;

  // Retrieval ops (Phase 4): search / recall.
  //
  // `cogneeSearch` sends a typed query to the knowledge graph.  `searchType`
  // defaults to "GRAPH_COMPLETION"; all 15 SCREAMING_SNAKE_CASE variants are
  // accepted. `SearchResponse` is passed through Rust serde so its shape
  // mirrors `cognee_search::SearchResponse`.
  //
  // `cogneeRecall` adds session-first routing: it checks session QA history by
  // keyword overlap, then falls back to graph search. `scope` controls which
  // sources contribute; "auto" (default) picks based on the presence of
  // `sessionId` and other opts.
  cogneeSearch(
    handle: NativeBox,
    query: string,
    opts?: CogneeSearchOptions
  ): Promise<CogneeSearchResponse>;
  cogneeRecall(
    handle: NativeBox,
    query: string,
    opts?: CogneeRecallOptions
  ): Promise<CogneeRecallResult>;

  // Memory ops (Phase 5): remember / remember_entry / memify / improve.
  //
  // `cogneeRemember` is a one-call add+cognify+optional-improve composite.
  // opts.selfImprovement triggers a memify pass after cognify.
  // opts.sessionId switches to session-memory mode (no graph writes).
  //
  // `cogneeRememberEntry` stores a typed MemoryEntry in a session. The
  // `entry` discriminated union supports "qa", "trace", and "feedback" types.
  //
  // `cogneeMemify` indexes triplet embeddings from the existing knowledge graph.
  // NOTE: extraction_tasks, enrichment_tasks, custom_data fields in MemifyConfig
  // are closures and cannot be passed from JS.
  //
  // `cogneeImprove` runs the four-stage session-graph bridge pipeline.
  cogneeRemember(
    handle: NativeBox,
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeRememberOptions
  ): Promise<CogneeRememberResult>;
  cogneeRememberEntry(
    handle: NativeBox,
    entry: CogneeMemoryEntry,
    datasetName: string,
    sessionId: string,
    opts?: { tenant?: string }
  ): Promise<CogneeRememberResult>;
  cogneeMemify(
    handle: NativeBox,
    opts?: CogneeMemifyOptions
  ): Promise<CogneeMemifyResult>;
  cogneeImprove(
    handle: NativeBox,
    opts: CogneeImproveOptions
  ): Promise<CogneeImproveResult>;

  // Data ops (Phase 5): forget / update / prune.
  //
  // `cogneeForget` deletes data scoped by the ForgetTarget union.
  // `cogneeUpdate` is a delete-then-re-add-then-re-cognify composite.
  // `cogneePruneData` removes all files from storage.
  // `cogneePruneSystem` wipes graph/vector/session backends selectively.
  cogneeForget(
    handle: NativeBox,
    target: CogneeForgetTarget,
    opts?: { tenant?: string }
  ): Promise<CogneeForgetResult>;
  cogneeUpdate(
    handle: NativeBox,
    dataId: string,
    newData: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeUpdateOptions
  ): Promise<CogneeUpdateResult>;
  cogneePruneData(handle: NativeBox): Promise<void>;
  cogneePruneSystem(
    handle: NativeBox,
    opts?: CogneePruneSystemOptions
  ): Promise<CogneePruneResult>;

  // Dataset manager ops (Phase 5).
  cogneeListDatasets(handle: NativeBox): Promise<CogneeDataset[]>;
  cogneeListData(handle: NativeBox, datasetId: string): Promise<CogneeData[]>;
  cogneeHasData(handle: NativeBox, datasetId: string): Promise<boolean>;
  cogneeDatasetStatus(
    handle: NativeBox,
    datasetIds: string[]
  ): Promise<Record<string, string>>;
  cogneeEmptyDataset(
    handle: NativeBox,
    datasetId: string
  ): Promise<CogneeDeleteResult>;
  cogneeDeleteData(
    handle: NativeBox,
    datasetId: string,
    dataId: string,
    opts?: { softDelete?: boolean; deleteDatasetIfEmpty?: boolean }
  ): Promise<CogneeDeleteResult>;
  cogneeDeleteAllDatasets(handle: NativeBox): Promise<CogneeDeleteResult[]>;

  // Pipeline-run resets (Phase 5).
  cogneeResetPipelineRunStatus(
    handle: NativeBox,
    datasetId: string,
    pipelineName: string
  ): Promise<void>;
  cogneeResetDatasetPipelineRunStatus(
    handle: NativeBox,
    datasetId: string
  ): Promise<void>;

  // Default user (Phase 5).
  cogneeGetOrCreateDefaultUser(handle: NativeBox): Promise<CogneeUser>;

  // Notebooks (Phase 5).
  cogneeListNotebooks(handle: NativeBox): Promise<CogneeNotebook[]>;
  cogneeCreateNotebook(
    handle: NativeBox,
    name: string,
    cells?: unknown,
    deletable?: boolean
  ): Promise<CogneeNotebook>;
  cogneeUpdateNotebook(
    handle: NativeBox,
    id: string,
    patch: { name?: string; cells?: unknown }
  ): Promise<CogneeNotebook | null>;
  cogneeDeleteNotebook(handle: NativeBox, id: string): Promise<boolean>;

  // Session ops (Phase 5).
  cogneeGetSession(
    handle: NativeBox,
    sessionId: string,
    opts?: { lastN?: number }
  ): Promise<CogneeSessionQAEntry[]>;
  cogneeAddFeedback(
    handle: NativeBox,
    sessionId: string,
    qaId: string,
    feedbackText?: string,
    feedbackScore?: number,
    opts?: object
  ): Promise<boolean>;
  cogneeDeleteFeedback(
    handle: NativeBox,
    sessionId: string,
    qaId: string,
    opts?: object
  ): Promise<boolean>;
  cogneeGetGraphContext(
    handle: NativeBox,
    sessionId: string,
    opts?: object
  ): Promise<string | null>;
  cogneeSetGraphContext(
    handle: NativeBox,
    sessionId: string,
    context: string,
    opts?: object
  ): Promise<void>;

  // Visualization ops (Phase 6).
  //
  // `cogneeVisualize` returns the d3.js force-directed HTML visualization of
  // the current knowledge graph as a string (no disk I/O in the binding layer).
  // `cogneeVisualizeToFile` writes the HTML to disk and returns the absolute
  // path; `opts.destinationPath` overrides the default `~/graph_visualization.html`.
  //
  // Both functions throw a typed error with `code = "FEATURE_NOT_BUILT"` when the
  // `visualization` feature was not compiled into this build of cognee-ts-neon.
  cogneeVisualize(
    handle: NativeBox,
    opts?: CogneeVisualizeOptions
  ): Promise<string>;
  cogneeVisualizeToFile(
    handle: NativeBox,
    opts?: CogneeVisualizeOptions
  ): Promise<string>;

  // Cloud ops (`cogneeServe` / `cogneeDisconnect`) live in the closed
  // `cognee-ts-cloud` cdylib (T15e). The OSS `cognee-ts-neon` native module
  // does not export them.

  // Config surface (Phase 2). Granular setters are synchronous and return
  // `void`; each bumps the config version, which version-invalidates the
  // cached services so the next op rebuilds the engines. The generic `configSet`
  // and the bulk setters are fallible and throw a typed `Error` (with a `code`
  // of `UNKNOWN_CONFIG_KEY` / `CONFIG_TYPE_MISMATCH`) on a bad key/value.
  // Keys are the canonical `Settings`/env field names.
  //
  // LLM
  configSetLlmProvider(handle: NativeBox, value: string): void;
  configSetLlmModel(handle: NativeBox, value: string): void;
  configSetLlmApiKey(handle: NativeBox, value: string): void;
  configSetLlmEndpoint(handle: NativeBox, value: string): void;
  configSetLlmApiVersion(handle: NativeBox, value: string): void;
  configSetLlmTemperature(handle: NativeBox, value: number): void;
  configSetLlmStreaming(handle: NativeBox, value: boolean): void;
  configSetLlmMaxCompletionTokens(handle: NativeBox, value: number): void;
  configSetLlmMaxRetries(handle: NativeBox, value: number): void;
  configSetLlmMaxParallelRequests(handle: NativeBox, value: number): void;
  // Embedding
  configSetEmbeddingProvider(handle: NativeBox, value: string): void;
  configSetEmbeddingModel(handle: NativeBox, value: string): void;
  configSetEmbeddingDimensions(handle: NativeBox, value: number): void;
  configSetEmbeddingEndpoint(handle: NativeBox, value: string): void;
  configSetEmbeddingApiKey(handle: NativeBox, value: string): void;
  configSetEmbeddingModelPath(handle: NativeBox, value: string): void;
  configSetEmbeddingTokenizerPath(handle: NativeBox, value: string): void;
  // Vector DB
  configSetVectorDbProvider(handle: NativeBox, value: string): void;
  configSetVectorDbUrl(handle: NativeBox, value: string): void;
  configSetVectorDbKey(handle: NativeBox, value: string): void;
  configSetVectorDbHost(handle: NativeBox, value: string): void;
  configSetVectorDbPort(handle: NativeBox, value: number): void;
  configSetVectorDbName(handle: NativeBox, value: string): void;
  // Graph DB
  configSetGraphDatabaseProvider(handle: NativeBox, value: string): void;
  configSetGraphModel(handle: NativeBox, value: string): void;
  configSetGraphFilePath(handle: NativeBox, value: string): void;
  // Chunking
  configSetChunkStrategy(handle: NativeBox, value: string): void;
  configSetChunkEngine(handle: NativeBox, value: string): void;
  configSetChunkSize(handle: NativeBox, value: number): void;
  configSetChunkOverlap(handle: NativeBox, value: number): void;
  // Paths
  configSetSystemRootDirectory(handle: NativeBox, value: string): void;
  configSetDataRootDirectory(handle: NativeBox, value: string): void;
  configSetCacheRootDirectory(handle: NativeBox, value: string): void;
  configSetLogsRootDirectory(handle: NativeBox, value: string): void;
  // Ontology
  configSetOntologyFilePath(handle: NativeBox, value: string): void;
  configSetOntologyResolver(handle: NativeBox, value: string): void;
  configSetOntologyMatchingStrategy(handle: NativeBox, value: string): void;
  // Other
  configSetMonitoringTool(handle: NativeBox, value: string): void;
  configSetClassificationModel(handle: NativeBox, value: string): void;
  configSetSummarizationModel(handle: NativeBox, value: string): void;
  // Generic + bulk + read-back
  configSet(handle: NativeBox, key: string, value: unknown): void;
  configSetLlmConfig(handle: NativeBox, values: object): void;
  configSetEmbeddingConfig(handle: NativeBox, values: object): void;
  configSetVectorDbConfig(handle: NativeBox, values: object): void;
  configSetGraphDbConfig(handle: NativeBox, values: object): void;
  // `getConfig` returns a snapshot of the current Settings with secret fields
  // (api keys, passwords, OTLP headers) blanked to "***REDACTED***".
  getConfig(handle: NativeBox): Record<string, unknown>;

  // Logging (gap-06): argument-less, idempotent.
  setupLogging(): void;

  // Telemetry (gap-07 task 05): argument-less, idempotent.
  setupTelemetry(): void;

  // Analytics (gap-07 task 06): argument-less, idempotent. Returns
  // `true` if armed by this call (or a prior call), `false` if the
  // per-binding policy suppressed emission.
  setupTelemetryAnalytics(): boolean;

  // Values
  valueFromNumber(n: number): NativeBox;
  valueFromBool(b: boolean): NativeBox;
  valueFromString(s: string): NativeBox;
  valueFromBuffer(buf: Buffer): NativeBox;
  valueAsNumber(val: NativeBox): number;
  valueAsBool(val: NativeBox): boolean;
  valueAsString(val: NativeBox): string;
  valueAsBuffer(val: NativeBox): Buffer;
  valueClone(val: NativeBox): NativeBox;

  // Tasks
  createTask(fn: Function): NativeBox;
  createIterTask(fn: Function): NativeBox;
  createBatchTask(fn: Function): NativeBox;

  // TaskInfo
  taskInfoNew(
    task: NativeBox,
    options?: { name?: string; batchSize?: number; weight?: number; summaryTemplate?: string }
  ): NativeBox;

  // Pipeline
  pipelineNew(description?: string): NativeBox;
  pipelineSetName(pipeline: NativeBox, name: string): void;
  pipelineAddTask(pipeline: NativeBox, taskInfo: NativeBox): void;
  pipelineSetBatchSize(pipeline: NativeBox, size: number): void;
  pipelineSetConcurrency(pipeline: NativeBox, n: number): void;
  pipelineSetRetry(pipeline: NativeBox, policy: object): void;

  // Pipeline execution
  pipelineExecute(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): Promise<unknown[]>;
  pipelineExecuteAsync(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): Promise<unknown[]>;
  pipelineExecuteBackground(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): NativeBox;
  pipelineExecuteWithWatcher(
    pipeline: NativeBox,
    inputs: unknown[],
    ctx: NativeBox,
    watcher: NativeBox
  ): Promise<unknown[]>;

  // Run handle
  runHandleIsFinished(handle: NativeBox): boolean;
  runHandleAbort(handle: NativeBox): void;
  runHandleWait(handle: NativeBox): Promise<unknown[]>;

  // Task context
  taskContextMock(): { handle: NativeBox; context: NativeBox };
  taskContextClone(ctx: NativeBox): NativeBox;

  // Cancellation
  cancellationPair(): { handle: NativeBox; token: NativeBox };
  cancellationHandleCancel(handle: NativeBox): void;
  cancellationHandleIsCancelled(handle: NativeBox): boolean;
  cancellationTokenIsCancelled(token: NativeBox): boolean;
  cancellationHandleClone(handle: NativeBox): NativeBox;
  cancellationTokenClone(token: NativeBox): NativeBox;

  // Progress
  progressNew(): NativeBox;
  progressSet(token: NativeBox, fraction: number): void;
  progressFraction(token: NativeBox): number;
  progressWidth(token: NativeBox): number;
  progressIsComplete(token: NativeBox): boolean;
  progressRootFraction(token: NativeBox): number;
  progressSplit(token: NativeBox, weights: number[]): NativeBox[];
  progressSubtoken(token: NativeBox, fracWidth: number): NativeBox;
  progressClone(token: NativeBox): NativeBox;

  // Watcher
  watcherNew(obj: object): NativeBox;
  watcherNoop(): NativeBox;
}

// Load the native addon via @neon-rs/load so the correct per-platform binary
// (published as an optional dependency) is selected automatically at runtime.
// If no matching prebuilt package is installed, we fall back to a locally-built
// `cognee_ts_neon.node` in the package root (produced by `npm run build:rust`).
//
// The optional dependency package names follow the @neon-rs/load platform key
// convention: `@cognee-ts/neon-<platform>` (e.g. `@cognee-ts/neon-linux-x64-gnu`).
// N-API version: napi-6 (Node >= 14.6 / 16.0).  See `engines` in package.json.
//
// eslint-disable-next-line @typescript-eslint/no-var-requires
const { proxy } = require("@neon-rs/load") as typeof import("@neon-rs/load");

// Mapping from @neon-rs/load platform key → optional dependency package name.
const platforms: Record<string, () => NativeBindings> = {
  "linux-x64-gnu": () => require("@cognee-ts/neon-linux-x64-gnu"),
  "linux-arm64-gnu": () => require("@cognee-ts/neon-linux-arm64-gnu"),
  "linux-x64-musl": () => require("@cognee-ts/neon-linux-x64-musl"),
  "linux-arm64-musl": () => require("@cognee-ts/neon-linux-arm64-musl"),
  "darwin-x64": () => require("@cognee-ts/neon-darwin-x64"),
  "darwin-arm64": () => require("@cognee-ts/neon-darwin-arm64"),
  "win32-x64-msvc": () => require("@cognee-ts/neon-win32-x64-msvc"),
};

// Fallback: try the locally-built artifact (produced by `npm run build:rust`).
// This is the path for consumers who installed without a matching prebuilt
// optional dep and ran the source-build postinstall step.
// eslint-disable-next-line @typescript-eslint/no-require-imports
const localDebug = () => require("../cognee_ts_neon.node") as NativeBindings;

export const native: NativeBindings = proxy({
  platforms,
  debug: localDebug,
}) as NativeBindings;
