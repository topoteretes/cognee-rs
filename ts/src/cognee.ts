/**
 * High-level TypeScript SDK for the cognee AI-memory pipeline.
 *
 * ```ts
 * import { Cognee } from 'cognee';
 *
 * const c = new Cognee({ llmModel: "gpt-4o-mini" });
 * await c.warm();
 * await c.add({ type: "text", text: "Hello, world!" }, "main");
 * await c.cognify("main");
 * const results = await c.search("Hello");
 * ```
 */

import { native, NativeBox } from "./native";
import type {
  CogneeDataInput,
  CogneeAddOptions,
  CogneeAddResult,
  CogneeCognifyOptions,
  CogneeCognifyResult,
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
import { wrapNativeError } from "./errors";

// Cloud module-level functions (`serve` / `disconnect`) live in the closed
// `cognee-ts-cloud` package (T15e). The OSS `cognee` package does not
// expose them.

/** Convert a single `snake_case` key to `camelCase`. */
function snakeToCamel(key: string): string {
  return key.replace(/_([a-z0-9])/g, (_m, c: string) => c.toUpperCase());
}

/**
 * Rewrite the top-level keys of a config snapshot from `snake_case` (the wire
 * shape returned by the native `getConfig`) to the documented `camelCase` API
 * keys (`llm_model` -> `llmModel`, `chunk_size` -> `chunkSize`, ...), so the
 * object returned by `config.get()` matches `CogneeConfigObject` / the setters.
 */
function configToCamel(obj: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) {
    out[snakeToCamel(k)] = v;
  }
  // The `setEmbeddingModel` setter writes the Rust `embedding_model_name`
  // field, so the snapshot surfaces it as `embeddingModelName`. Expose an
  // `embeddingModel` alias too so the read-back key matches the setter name.
  if ("embeddingModelName" in out && !("embeddingModel" in out)) {
    out.embeddingModel = out.embeddingModelName;
  }
  return out;
}

/** Convert a single `camelCase` key to `snake_case`. */
function camelToSnake(key: string): string {
  return key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/**
 * Public camelCase setting names whose `snake_case` form does NOT match the
 * native `Settings` field. Keyed by the camelCase name the SDK documents
 * (matching the `config.set*` setters and `config.get()` read-back); the value
 * is the actual `Settings` field. Only `embeddingModel` differs (the field is
 * `embedding_model_name`, mirroring `setEmbeddingModel`).
 */
const SETTINGS_KEY_ALIASES: Record<string, string> = {
  embeddingModel: "embedding_model_name",
};

/**
 * Normalize a constructor settings object so the documented camelCase keys
 * (`llmModel`, `embeddingProvider`, `chunkSize`, ...) reach the native
 * snake_case `Settings` deserializer. snake_case keys pass through unchanged
 * (camel→snake is idempotent on them), so both spellings are accepted — fixing
 * silently-ignored camelCase constructor args.
 */
function normalizeSettings(settings: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(settings)) {
    const key = SETTINGS_KEY_ALIASES[k] ?? camelToSnake(k);
    out[key] = v;
  }
  return out;
}

// ─────────────────────────────────────────────────────────────────────────────
// Cognee class
// ─────────────────────────────────────────────────────────────────────────────

/** Config sub-object type. */
export interface CogneeConfigObject {
  // LLM
  setLlmProvider(v: string): void;
  setLlmModel(v: string): void;
  setLlmApiKey(v: string): void;
  setLlmEndpoint(v: string): void;
  setLlmApiVersion(v: string): void;
  setLlmTemperature(v: number): void;
  setLlmStreaming(v: boolean): void;
  setLlmMaxCompletionTokens(v: number): void;
  setLlmMaxRetries(v: number): void;
  setLlmMaxParallelRequests(v: number): void;
  // Embedding
  setEmbeddingProvider(v: string): void;
  setEmbeddingModel(v: string): void;
  setEmbeddingDimensions(v: number): void;
  setEmbeddingEndpoint(v: string): void;
  setEmbeddingApiKey(v: string): void;
  setEmbeddingModelPath(v: string): void;
  setEmbeddingTokenizerPath(v: string): void;
  // Vector DB
  setVectorDbProvider(v: string): void;
  setVectorDbUrl(v: string): void;
  setVectorDbKey(v: string): void;
  setVectorDbHost(v: string): void;
  setVectorDbPort(v: number): void;
  setVectorDbName(v: string): void;
  // Graph DB
  setGraphDatabaseProvider(v: string): void;
  setGraphModel(v: string): void;
  setGraphFilePath(v: string): void;
  // Chunking
  setChunkStrategy(v: string): void;
  setChunkEngine(v: string): void;
  setChunkSize(v: number): void;
  setChunkOverlap(v: number): void;
  // Paths
  setSystemRootDirectory(v: string): void;
  setDataRootDirectory(v: string): void;
  setCacheRootDirectory(v: string): void;
  setLogsRootDirectory(v: string): void;
  // Ontology
  setOntologyFilePath(v: string): void;
  setOntologyResolver(v: string): void;
  setOntologyMatchingStrategy(v: string): void;
  // Other
  setMonitoringTool(v: string): void;
  setClassificationModel(v: string): void;
  setSummarizationModel(v: string): void;
  // Generic + bulk
  set(key: string, value: unknown): void;
  setLlmConfig(values: object): void;
  setEmbeddingConfig(values: object): void;
  setVectorDbConfig(values: object): void;
  setGraphDbConfig(values: object): void;
  // Read-back
  get(): Record<string, unknown>;
}

/** Datasets sub-object type. */
export interface CogneeDatasetObject {
  list(): Promise<CogneeDataset[]>;
  listData(datasetId: string): Promise<CogneeData[]>;
  has(datasetId: string): Promise<boolean>;
  status(datasetIds: string[]): Promise<Record<string, string>>;
  empty(datasetId: string): Promise<CogneeDeleteResult>;
  deleteData(
    datasetId: string,
    dataId: string,
    opts?: { softDelete?: boolean; deleteDatasetIfEmpty?: boolean }
  ): Promise<CogneeDeleteResult>;
  deleteAll(): Promise<CogneeDeleteResult[]>;
}

/** Sessions sub-object type. */
export interface CogneeSessionObject {
  get(sessionId: string, opts?: { lastN?: number }): Promise<CogneeSessionQAEntry[]>;
  addFeedback(
    sessionId: string,
    qaId: string,
    feedbackText?: string,
    feedbackScore?: number,
    opts?: object
  ): Promise<boolean>;
  deleteFeedback(sessionId: string, qaId: string, opts?: object): Promise<boolean>;
  getGraphContext(sessionId: string, opts?: object): Promise<string | null>;
  setGraphContext(sessionId: string, context: string, opts?: object): Promise<void>;
}

/**
 * Notebooks sub-object type.
 *
 * Mirrors Python's `cognee.notebooks` sub-object (see `python/src/sdk_admin.rs`).
 * All operations forward to the corresponding `native.cognee*Notebook` functions.
 */
export interface CogneeNotebookObject {
  /**
   * Return all notebooks owned by this handle's user.
   */
  list(): Promise<CogneeNotebook[]>;
  /**
   * Create a new notebook.
   *
   * @param name      Display name for the notebook.
   * @param cells     Initial cells array (default empty).
   * @param deletable Whether the notebook can be deleted (default `true`).
   */
  create(name: string, cells?: unknown, deletable?: boolean): Promise<CogneeNotebook>;
  /**
   * Apply a partial update to an existing notebook.
   *
   * Returns the updated notebook, or `null` if no notebook with the given `id`
   * exists for this owner.
   */
  update(id: string, patch: { name?: string; cells?: unknown }): Promise<CogneeNotebook | null>;
  /**
   * Delete a notebook by ID.
   *
   * Returns `true` if a row was removed, `false` if not found.
   */
  delete(id: string): Promise<boolean>;
}

/**
 * Users sub-object type.
 *
 * Exposes user- and pipeline-run-management operations that Python surfaces on
 * the `Cognee` class. These forward to the corresponding `native.cognee*` functions.
 */
export interface CogneeUserObject {
  /**
   * Resolve (or lazily create) the default user for this handle.
   *
   * The default user is determined by `Settings.default_user_email`
   * (Python default-user semantics). This is the same user whose UUID is
   * returned by `c.ownerId()`.
   */
  getOrCreateDefault(): Promise<CogneeUser>;
  /**
   * Reset the pipeline-run status for a single pipeline within a dataset.
   *
   * Unblocks a dataset that is stuck in the "running" state so it can be
   * re-cognified. Equivalent to Python's `reset_pipeline_run_status`.
   *
   * @param datasetId    UUID of the dataset.
   * @param pipelineName Name of the pipeline (e.g. `"cognify_pipeline"`).
   */
  resetPipelineRunStatus(datasetId: string, pipelineName: string): Promise<void>;
  /**
   * Reset the pipeline-run status for **all** pipelines in a dataset.
   *
   * Equivalent to Python's `reset_dataset_pipeline_run_status`.
   *
   * @param datasetId UUID of the dataset.
   */
  resetDatasetPipelineRunStatus(datasetId: string): Promise<void>;
}

/**
 * The main Cognee SDK class.
 *
 * Construct with an optional settings object (or JSON string) to override
 * env-derived defaults. Settings keys are the canonical `Settings` / env-var
 * field names (`llmModel`, `embeddingProvider`, etc.).
 *
 * ```ts
 * import { Cognee } from 'cognee';
 *
 * const c = new Cognee({ llmModel: "gpt-4o-mini", llmApiKey: process.env.OPENAI_API_KEY });
 * await c.warm();
 * await c.add({ type: "text", text: "The quick brown fox" }, "demo");
 * await c.cognify("demo");
 * const answer = await c.search("What does the fox do?");
 * ```
 */
export class Cognee {
  /** @internal Opaque Rust handle. Not part of the public API. */
  private readonly _handle: NativeBox;

  /** Granular config setters and a read-back `get()`. */
  readonly config: CogneeConfigObject;

  /** Dataset management operations. */
  readonly datasets: CogneeDatasetObject;

  /** Session management operations. */
  readonly sessions: CogneeSessionObject;

  /** Notebook management operations. */
  readonly notebooks: CogneeNotebookObject;

  /** User and pipeline-run management operations. */
  readonly users: CogneeUserObject;

  constructor(settings?: object | string) {
    // Accept the documented camelCase setting keys (e.g. `{ llmModel: ... }`)
    // as well as raw snake_case. The native `cogneeNew` deserializes a
    // snake_case `Settings`, so camelCase keys would otherwise be silently
    // ignored. A JSON string is parsed, normalized, and forwarded as an object.
    let normalized: object | string | undefined = settings;
    if (settings != null) {
      const obj = typeof settings === "string" ? JSON.parse(settings) : settings;
      normalized = normalizeSettings(obj as Record<string, unknown>);
    }
    this._handle = native.cogneeNew(normalized);

    // ── config sub-object ────────────────────────────────────────────────────
    // Granular setters are sync and do not need try/catch.
    // Fallible setters (configSet, bulk setters) are wrapped.
    const h = this._handle;
    this.config = {
      // LLM
      setLlmProvider:             (v) => { native.configSetLlmProvider(h, v); },
      setLlmModel:                (v) => { native.configSetLlmModel(h, v); },
      setLlmApiKey:               (v) => { native.configSetLlmApiKey(h, v); },
      setLlmEndpoint:             (v) => { native.configSetLlmEndpoint(h, v); },
      setLlmApiVersion:           (v) => { native.configSetLlmApiVersion(h, v); },
      setLlmTemperature:          (v) => { native.configSetLlmTemperature(h, v); },
      setLlmStreaming:            (v) => { native.configSetLlmStreaming(h, v); },
      setLlmMaxCompletionTokens:  (v) => { native.configSetLlmMaxCompletionTokens(h, v); },
      setLlmMaxRetries:           (v) => { native.configSetLlmMaxRetries(h, v); },
      setLlmMaxParallelRequests:  (v) => { native.configSetLlmMaxParallelRequests(h, v); },
      // Embedding
      setEmbeddingProvider:       (v) => { native.configSetEmbeddingProvider(h, v); },
      setEmbeddingModel:          (v) => { native.configSetEmbeddingModel(h, v); },
      setEmbeddingDimensions:     (v) => { native.configSetEmbeddingDimensions(h, v); },
      setEmbeddingEndpoint:       (v) => { native.configSetEmbeddingEndpoint(h, v); },
      setEmbeddingApiKey:         (v) => { native.configSetEmbeddingApiKey(h, v); },
      setEmbeddingModelPath:      (v) => { native.configSetEmbeddingModelPath(h, v); },
      setEmbeddingTokenizerPath:  (v) => { native.configSetEmbeddingTokenizerPath(h, v); },
      // Vector DB
      setVectorDbProvider:        (v) => { native.configSetVectorDbProvider(h, v); },
      setVectorDbUrl:             (v) => { native.configSetVectorDbUrl(h, v); },
      setVectorDbKey:             (v) => { native.configSetVectorDbKey(h, v); },
      setVectorDbHost:            (v) => { native.configSetVectorDbHost(h, v); },
      setVectorDbPort:            (v) => { native.configSetVectorDbPort(h, v); },
      setVectorDbName:            (v) => { native.configSetVectorDbName(h, v); },
      // Graph DB
      setGraphDatabaseProvider:   (v) => { native.configSetGraphDatabaseProvider(h, v); },
      setGraphModel:              (v) => { native.configSetGraphModel(h, v); },
      setGraphFilePath:           (v) => { native.configSetGraphFilePath(h, v); },
      // Chunking
      setChunkStrategy:           (v) => { native.configSetChunkStrategy(h, v); },
      setChunkEngine:             (v) => { native.configSetChunkEngine(h, v); },
      setChunkSize:               (v) => { native.configSetChunkSize(h, v); },
      setChunkOverlap:            (v) => { native.configSetChunkOverlap(h, v); },
      // Paths
      setSystemRootDirectory:     (v) => { native.configSetSystemRootDirectory(h, v); },
      setDataRootDirectory:       (v) => { native.configSetDataRootDirectory(h, v); },
      setCacheRootDirectory:      (v) => { native.configSetCacheRootDirectory(h, v); },
      setLogsRootDirectory:       (v) => { native.configSetLogsRootDirectory(h, v); },
      // Ontology
      setOntologyFilePath:        (v) => { native.configSetOntologyFilePath(h, v); },
      setOntologyResolver:        (v) => { native.configSetOntologyResolver(h, v); },
      setOntologyMatchingStrategy:(v) => { native.configSetOntologyMatchingStrategy(h, v); },
      // Other
      setMonitoringTool:          (v) => { native.configSetMonitoringTool(h, v); },
      setClassificationModel:     (v) => { native.configSetClassificationModel(h, v); },
      setSummarizationModel:      (v) => { native.configSetSummarizationModel(h, v); },
      // Generic + bulk (fallible — wrap errors)
      set: (key, value) => {
        try { native.configSet(h, key, value); }
        catch (e) { throw wrapNativeError(e); }
      },
      setLlmConfig: (values) => {
        try { native.configSetLlmConfig(h, values); }
        catch (e) { throw wrapNativeError(e); }
      },
      setEmbeddingConfig: (values) => {
        try { native.configSetEmbeddingConfig(h, values); }
        catch (e) { throw wrapNativeError(e); }
      },
      setVectorDbConfig: (values) => {
        try { native.configSetVectorDbConfig(h, values); }
        catch (e) { throw wrapNativeError(e); }
      },
      setGraphDbConfig: (values) => {
        try { native.configSetGraphDbConfig(h, values); }
        catch (e) { throw wrapNativeError(e); }
      },
      // Read-back
      get: () => configToCamel(native.getConfig(h) as Record<string, unknown>),
    };

    // ── datasets sub-object ──────────────────────────────────────────────────
    this.datasets = {
      list:       () => native.cogneeListDatasets(h),
      listData:   (datasetId) => native.cogneeListData(h, datasetId),
      has:        (datasetId) => native.cogneeHasData(h, datasetId),
      status:     (datasetIds) => native.cogneeDatasetStatus(h, datasetIds),
      empty:      (datasetId) => native.cogneeEmptyDataset(h, datasetId),
      deleteData: (datasetId, dataId, opts) => native.cogneeDeleteData(h, datasetId, dataId, opts),
      deleteAll:  () => native.cogneeDeleteAllDatasets(h),
    };

    // ── sessions sub-object ──────────────────────────────────────────────────
    this.sessions = {
      get:              (sessionId, opts) => native.cogneeGetSession(h, sessionId, opts),
      addFeedback:      (sessionId, qaId, text, score, opts) =>
                          native.cogneeAddFeedback(h, sessionId, qaId, text, score, opts),
      deleteFeedback:   (sessionId, qaId, opts) =>
                          native.cogneeDeleteFeedback(h, sessionId, qaId, opts),
      getGraphContext:  (sessionId, opts) => native.cogneeGetGraphContext(h, sessionId, opts),
      setGraphContext:  (sessionId, context, opts) =>
                          native.cogneeSetGraphContext(h, sessionId, context, opts),
    };

    // ── notebooks sub-object ─────────────────────────────────────────────────
    this.notebooks = {
      list:   ()                  => native.cogneeListNotebooks(h),
      create: (name, cells, del)  => native.cogneeCreateNotebook(h, name, cells, del),
      update: (id, patch)         => native.cogneeUpdateNotebook(h, id, patch),
      delete: (id)                => native.cogneeDeleteNotebook(h, id),
    };

    // ── users sub-object ─────────────────────────────────────────────────────
    this.users = {
      getOrCreateDefault:             ()                           => native.cogneeGetOrCreateDefaultUser(h),
      resetPipelineRunStatus:         (datasetId, pipelineName)   => native.cogneeResetPipelineRunStatus(h, datasetId, pipelineName),
      resetDatasetPipelineRunStatus:  (datasetId)                 => native.cogneeResetDatasetPipelineRunStatus(h, datasetId),
    };
  }

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  /**
   * Warm up the SDK: build embedding/LLM engines and resolve the default user.
   * Call this once after construction before running the pipeline.
   */
  async warm(): Promise<void> {
    try {
      await native.cogneeWarm(this._handle);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /** Return the UUID of the owner this handle is scoped to. */
  async ownerId(): Promise<string> {
    try {
      return await native.cogneeOwnerId(this._handle);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  // ── Pipeline ops ──────────────────────────────────────────────────────────

  /**
   * Ingest data into a named dataset.
   *
   * `dataInput` is a single item or an array:
   * - `{ type: "text", text: "…" }`
   * - `{ type: "file", path: "/abs/path" }`
   * - `{ type: "url", url: "https://…" }`
   * - `{ type: "binary", bytes: Buffer, name: "doc.pdf" }`
   */
  async add(
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeAddOptions
  ): Promise<CogneeAddResult> {
    try {
      return await native.cogneeAdd(this._handle, dataInput, datasetName, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Run the knowledge-graph extraction pipeline on a previously-added dataset.
   */
  async cognify(
    dataset: string,
    opts?: CogneeCognifyOptions
  ): Promise<CogneeCognifyResult> {
    try {
      return await native.cogneeCognify(this._handle, dataset, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Ingest data and immediately run knowledge-graph extraction in a single call.
   */
  async addAndCognify(
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeAddOptions & CogneeCognifyOptions
  ): Promise<{ add: CogneeAddResult; cognify: CogneeCognifyResult }> {
    try {
      return await native.cogneeAddAndCognify(this._handle, dataInput, datasetName, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  // ── Retrieval ops ─────────────────────────────────────────────────────────

  /**
   * Search the knowledge graph.
   *
   * `opts.searchType` defaults to `"GRAPH_COMPLETION"`. All 15
   * SCREAMING_SNAKE_CASE variants are supported.
   */
  async search(
    query: string,
    opts?: CogneeSearchOptions
  ): Promise<CogneeSearchResponse> {
    try {
      return await native.cogneeSearch(this._handle, query, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Recall information using session-first routing.
   *
   * Checks session QA history first (if `opts.sessionId` is set), then falls
   * back to graph search. Use `opts.scope` to control which sources contribute.
   */
  async recall(
    query: string,
    opts?: CogneeRecallOptions
  ): Promise<CogneeRecallResult> {
    try {
      return await native.cogneeRecall(this._handle, query, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  // ── Memory ops ────────────────────────────────────────────────────────────

  /**
   * Composite add+cognify with optional graph-improvement pass.
   *
   * `opts.selfImprovement` triggers a memify pass after cognify.
   * `opts.sessionId` switches to session-memory mode (no graph writes).
   */
  async remember(
    dataInput: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeRememberOptions
  ): Promise<CogneeRememberResult> {
    try {
      return await native.cogneeRemember(this._handle, dataInput, datasetName, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Store a typed memory entry in a session.
   *
   * `entry` is a discriminated union: `"qa"`, `"trace"`, or `"feedback"`.
   */
  async rememberEntry(
    entry: CogneeMemoryEntry,
    datasetName: string,
    sessionId: string,
    opts?: { tenant?: string }
  ): Promise<CogneeRememberResult> {
    try {
      return await native.cogneeRememberEntry(this._handle, entry, datasetName, sessionId, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Index triplet embeddings from the existing knowledge graph.
   *
   * Idempotent (re-runnable). Enables `SearchType::TripletCompletion`.
   */
  async memify(opts?: CogneeMemifyOptions): Promise<CogneeMemifyResult> {
    try {
      return await native.cogneeMemify(this._handle, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Run the four-stage session-graph bridge pipeline:
   * session collection → memify → feedback integration → edge synchronisation.
   */
  async improve(opts: CogneeImproveOptions): Promise<CogneeImproveResult> {
    try {
      return await native.cogneeImprove(this._handle, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  // ── Data ops ──────────────────────────────────────────────────────────────

  /**
   * Delete data scoped by the `target` discriminated union:
   * - `{ kind: "item", dataId, dataset }` — remove a single data item
   * - `{ kind: "dataset", dataset }` — remove an entire dataset
   * - `{ kind: "all" }` — remove everything
   */
  async forget(
    target: CogneeForgetTarget,
    opts?: { tenant?: string }
  ): Promise<CogneeForgetResult> {
    try {
      return await native.cogneeForget(this._handle, target, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Replace an existing data item: delete it, re-add the new content, and
   * re-run knowledge-graph extraction.
   */
  async update(
    dataId: string,
    newData: CogneeDataInput | CogneeDataInput[],
    datasetName: string,
    opts?: CogneeUpdateOptions
  ): Promise<CogneeUpdateResult> {
    try {
      return await native.cogneeUpdate(this._handle, dataId, newData, datasetName, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Remove all files from storage (non-cascading; metadata DB is not touched).
   */
  async pruneData(): Promise<void> {
    try {
      await native.cogneePruneData(this._handle);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Selectively wipe graph, vector, metadata, and/or cache backends.
   *
   * All flags default to `true` when not specified.
   */
  async pruneSystem(opts?: CogneePruneSystemOptions): Promise<CogneePruneResult> {
    try {
      return await native.cogneePruneSystem(this._handle, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  // ── Visualization ─────────────────────────────────────────────────────────

  /**
   * Render the current knowledge graph as a self-contained d3.js HTML string.
   *
   * Throws `FeatureNotBuiltError` when the `visualization` feature was not
   * compiled into this build.
   */
  async visualize(opts?: CogneeVisualizeOptions): Promise<string> {
    try {
      return await native.cogneeVisualize(this._handle, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }

  /**
   * Write the knowledge-graph visualization to an HTML file and return its
   * absolute path.
   *
   * `opts.destinationPath` overrides the default `~/graph_visualization.html`.
   *
   * Throws `FeatureNotBuiltError` when the `visualization` feature was not
   * compiled into this build.
   */
  async visualizeToFile(opts?: CogneeVisualizeOptions): Promise<string> {
    try {
      return await native.cogneeVisualizeToFile(this._handle, opts);
    } catch (e) {
      throw wrapNativeError(e);
    }
  }
}
