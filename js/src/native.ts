/** Opaque native handle types returned by the Neon addon. */
export type NativeBox = object;

/** A single `add` input (discriminated union; see `cogneeAdd`). */
export type CogneeDataInput =
  | { type: "text"; text: string }
  | { type: "file"; path: string }
  | { type: "url"; url: string }
  | { type: "binary"; bytes: Buffer | number[] | string; name: string };

/** Options accepted by `cogneeAdd` / the add phase of `cogneeAddAndCognify`. */
export interface CogneeAddOptions {
  /** Tenant UUID string (multi-tenant scoping); defaults to none. */
  tenant?: string;
}

/** Per-call cognify config overrides (applied on top of the handle config). */
export interface CogneeCognifyOptions {
  tenant?: string;
  chunkSize?: number;
  chunkOverlap?: number;
  summarization?: boolean;
  temporalCognify?: boolean;
  /** Index `"source → relation → target"` triplet embeddings. */
  triplet?: boolean;
}

/**
 * Result of `cogneeAdd`.
 *
 * `AddPipeline::add` returns one row per input including duplicates (the
 * duplicate path returns the pre-existing row), so the binding pre-scans the
 * dataset and partitions the result: `added` holds only the items newly created
 * by this call, `deduplicated` holds the ones that already existed. An empty
 * `added` array (`addedCount === 0`) means every submitted item was a duplicate.
 */
export interface CogneeAddResult {
  datasetName: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  added: any[];
  addedCount: number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  deduplicated: any[];
  deduplicatedCount: number;
}

/** Result of `cogneeCognify` — counts hand-built from the pipeline result. */
export interface CogneeCognifyResult {
  chunks: number;
  entities: number;
  edges: number;
  summaries: number;
  embeddings: number;
  alreadyCompleted: boolean;
  priorPipelineRunId: string | null;
}

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
  //   { type: "url"; url: string }        // ingestion-only; not wired e2e yet
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

// eslint-disable-next-line @typescript-eslint/no-var-requires
export const native: NativeBindings = require("../cognee_neon.node");
