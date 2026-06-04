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

/** All 15 search type wire names (SCREAMING_SNAKE_CASE, matching Rust serde). */
export type SearchTypeString =
  | "SUMMARIES"
  | "CHUNKS"
  | "RAG_COMPLETION"
  | "TRIPLET_COMPLETION"
  | "GRAPH_COMPLETION"
  | "GRAPH_SUMMARY_COMPLETION"
  | "CYPHER"
  | "NATURAL_LANGUAGE"
  | "GRAPH_COMPLETION_COT"
  | "GRAPH_COMPLETION_CONTEXT_EXTENSION"
  | "FEELING_LUCKY"
  | "FEEDBACK"
  | "TEMPORAL"
  | "CODING_RULES"
  | "CHUNKS_LEXICAL";

/** Recall scope wire names (snake_case; "all" expands to all four concrete scopes). */
export type RecallScopeString =
  | "auto"
  | "graph"
  | "session"
  | "trace"
  | "graph_context"
  | "all";

/** Options accepted by `cogneeSearch`. All fields are optional. */
export interface CogneeSearchOptions {
  /** SCREAMING_SNAKE_CASE search type. Defaults to "GRAPH_COMPLETION". */
  searchType?: SearchTypeString;
  /** Dataset names to restrict the search to. */
  datasets?: string[];
  /** Dataset UUIDs to restrict the search to. */
  datasetIds?: string[];
  /** Maximum number of results to return. */
  topK?: number;
  /** System prompt override for completion-generating retrievers. */
  systemPrompt?: string;
  /** Session ID for QA history persistence. */
  sessionId?: string;
  /** Filter results by node type. */
  nodeType?: string;
  /** Filter results by one or more node names. */
  nodeName?: string[];
  /** Return only the context without running completion. */
  onlyContext?: boolean;
  /** Combine context from multiple retrieval paths. */
  useCombinedContext?: boolean;
  /** Include verbose diagnostics in the response. */
  verbose?: boolean;
  /** Persist this query+result to search history (defaults to true). */
  saveInteraction?: boolean;
  /** Detect feedback about the previous response before searching. */
  autoFeedbackDetection?: boolean;
  /** User UUID override (defaults to the handle's owner). */
  userId?: string;
}

/** Options accepted by `cogneeRecall`. All fields are optional. */
export interface CogneeRecallOptions {
  /** SCREAMING_SNAKE_CASE search type for the graph retrieval leg. */
  searchType?: SearchTypeString;
  /** Dataset names to restrict graph search to. */
  datasets?: string[];
  /** Maximum number of results per source. Defaults to 10. */
  topK?: number;
  /** Automatically select the best search type (defaults to false). */
  autoRoute?: boolean;
  /** Session ID for session-first routing. */
  sessionId?: string;
  /**
   * Recall scope: a single scope string or an array.
   * "auto" (default) → session-first routing when sessionId is set, else graph.
   * "all" → fan out across all four concrete sources.
   */
  scope?: RecallScopeString | RecallScopeString[];
}

/**
 * Raw search response from `cogneeSearch`.
 * Mirrors `cognee_search::SearchResponse` (fully Serialize on the Rust side).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeSearchResponse = any;

/** Result returned by `cogneeRecall`. */
export interface CogneeRecallResult {
  /** Source-tagged result items from all contributing sources. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  items: any[];
  /** The search type used for the graph retrieval leg, or null. */
  searchTypeUsed: SearchTypeString | null;
  /** Whether auto-routing was applied. */
  autoRouted: boolean;
  /** The raw graph search response, or null if no graph leg ran. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  searchResponse: any | null;
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 5 types
// ─────────────────────────────────────────────────────────────────────────────

/** Options accepted by `cogneeRemember`. */
export interface CogneeRememberOptions {
  /** Session ID — switches to session-memory mode (no graph writes). */
  sessionId?: string;
  /** Run a memify pass after cognify (default false). */
  selfImprovement?: boolean;
  /** Tenant UUID string (multi-tenant scoping). */
  tenant?: string;
}

/** Result of `cogneeRemember` / `cogneeRememberEntry`. Mirrors `RememberResult` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeRememberResult = any;

/** A typed memory entry for `cogneeRememberEntry`. */
export type CogneeMemoryEntry =
  | {
      type: "qa";
      question?: string;
      answer?: string;
      context?: string;
      feedbackText?: string;
      feedbackScore?: number;
      usedGraphElementIds?: object;
    }
  | {
      type: "trace";
      originFunction: string;
      status?: string;
      memoryQuery?: string;
      memoryContext?: string;
      methodParams?: unknown;
      methodReturnValue?: unknown;
      errorMessage?: string;
      generateFeedbackWithLlm?: boolean;
    }
  | {
      type: "feedback";
      qaId: string;
      feedbackText?: string;
      feedbackScore?: number;
    };

/** Options accepted by `cogneeMemify`. */
export interface CogneeMemifyOptions {
  tripletBatchSize?: number;
  nodeTypeFilter?: string;
  nodeNameFilter?: string[];
  nodeNameFilterOperator?: string;
}

/** Result of `cogneeMemify`. Hand-built from `MemifyResult` (not Serialize). */
export interface CogneeMemifyResult {
  tripletCount: number;
  indexedCount: number;
  batchCount: number;
  alreadyCompleted: boolean;
  priorPipelineRunId: string | null;
}

/** Options accepted by `cogneeImprove`. */
export interface CogneeImproveOptions {
  datasetName: string;
  sessionIds?: string[];
  /** Node name filter for the memify stage. */
  nodeName?: string[];
  feedbackAlpha?: number;
  tenant?: string;
}

/** Result of `cogneeImprove`. Hand-built from `ImproveResult` (not Serialize). */
export interface CogneeImproveResult {
  stagesRun: string[];
  memifyResult: CogneeMemifyResult | null;
  feedbackEntriesProcessed: number;
  feedbackEntriesApplied: number;
  sessionsPersisted: number;
  edgesSynced: number;
}

/** Target for `cogneeForget`. */
export type CogneeForgetTarget =
  | { kind: "item"; dataId: string; dataset: { name: string } | { id: string } }
  | { kind: "dataset"; dataset: { name: string } | { id: string } }
  | { kind: "all" };

/** Result of `cogneeForget`. Hand-built JSON. */
export interface CogneeForgetResult {
  target: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  deleteResult: any;
}

/** Options accepted by `cogneeUpdate`. */
export interface CogneeUpdateOptions {
  tenant?: string;
}

/** Result of `cogneeUpdate`. Hand-built JSON. */
export interface CogneeUpdateResult {
  deletedDataId: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  deleteResult: any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  newData: any[];
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  cognifyResult: any | null;
}

/** Options accepted by `cogneePruneSystem`. */
export interface CogneePruneSystemOptions {
  pruneGraph?: boolean;
  pruneVector?: boolean;
  pruneMetadata?: boolean;
  pruneCache?: boolean;
}

/** Result of `cogneePruneSystem`. Hand-built JSON. */
export interface CogneePruneResult {
  dataPruned: boolean;
  graphPruned: boolean;
  vectorPruned: boolean;
  metadataPruned: boolean;
  cachePruned: boolean;
}

/** A dataset row. Mirrors `cognee_models::Dataset` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeDataset = any;

/** A data item row. Mirrors `cognee_models::Data` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeData = any;

/** Result of a delete operation. Mirrors `DeleteResult` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeDeleteResult = any;

/** A user row. Mirrors `cognee_models::User` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeUser = any;

/** A notebook row. Mirrors `Notebook` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeNotebook = any;

/** A session Q&A entry. Mirrors `SessionQAEntry` (Serialize). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeSessionQAEntry = any;

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
