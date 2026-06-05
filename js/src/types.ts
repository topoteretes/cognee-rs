/**
 * User-facing TypeScript type surface for the cognee SDK.
 *
 * These types are extracted from `native.ts` so that `cognee.ts` and
 * user code can import them without pulling in the full `NativeBindings`
 * interface. `native.ts` re-exports everything from here for backward
 * compatibility.
 */

// ─────────────────────────────────────────────────────────────────────────────
// Phase 3 types
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Phase 4 types
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Phase 6 types
// ─────────────────────────────────────────────────────────────────────────────

/** Options accepted by `cogneeVisualize` / `cogneeVisualizeToFile`. */
export interface CogneeVisualizeOptions {
  /**
   * Absolute path for the output HTML file.  Used only by
   * `cogneeVisualizeToFile`; ignored by `cogneeVisualize`.
   * Defaults to `~/graph_visualization.html` when absent.
   */
  destinationPath?: string;
}

/**
 * Result of `cogneeServe`.  The `CloudClient` handle is not exposed to JS;
 * use `serviceUrl` to log / verify the connection.
 */
export interface CogneeServeResult {
  connected: true;
  serviceUrl: string;
}

/** Options accepted by `cogneeServe`. */
export interface CogneeServeOptions {
  /**
   * Direct service URL.  When set, selects **direct mode** — no Auth0
   * device-code flow; requires a running Cognee HTTP server at this URL.
   * When absent, **cloud mode** is used (device-code flow, requires a TTY).
   */
  url?: string;
  /** API key for authenticating against the service URL. */
  apiKey?: string;
  /** Override for the management API base URL (cloud mode only). */
  cloudUrl?: string;
  /** Override for the Auth0 tenant domain (cloud mode only). */
  auth0Domain?: string;
  /** Override for the Auth0 native-app client ID (cloud mode only). */
  auth0ClientId?: string;
  /** Override for the Auth0 API audience (cloud mode only). */
  auth0Audience?: string;
}

/** Options accepted by `cogneeDisconnect`. */
export interface CogneeDisconnectOptions {
  /**
   * When `true`, the on-disk credential cache
   * (`~/.cognee/cloud_credentials.json`) is deleted so the next
   * `cogneeServe()` must re-authenticate.  Defaults to `false`.
   */
  wipeCredentials?: boolean;
}
