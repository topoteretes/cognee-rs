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
  /** Accepted but inert on the default chunking path: no chunker reads it, so it changes no chunk boundary (Python's DefaultChunkEngine does consume it, but that engine is unreachable). Only validation reads it, rejecting overlap >= chunk size. Kept for API compatibility; see CognifyConfig::chunk_overlap in crates/cognify/src/config.rs. */
  chunkOverlap?: number;
  summarization?: boolean;
  temporalCognify?: boolean;
  /** Index `"source → relation → target"` triplet embeddings. */
  triplet?: boolean;
  /**
   * A custom graph model's JSON Schema (an object, or a string holding one),
   * extracted instead of the default `KnowledgeGraph` — Python's `graph_model`.
   * `{}` means the default model. Not supported with `temporalCognify`; see
   * docs/guides/custom-graph-schema.md for the schema contract.
   */
  graphSchema?: Record<string, unknown> | string;
  /**
   * The same custom graph model written in the spec DSL (an object, or a
   * string holding one), compiled natively into a `graphSchema` — Python's
   * `cognee.low_level.GraphSchemaSpec`. Not together with `graphSchema`.
   */
  graphSpec?: GraphSchemaSpec | string;
  /**
   * How each chunk links to the custom graph extracted from it: `"direct"`
   * (the root, the default) or `"all"` (every stored node). Requires
   * `graphSchema` or `graphSpec`.
   */
  chunkAttachment?: "direct" | "all";
  /**
   * Graph extractor for this run — Python's `extractor`: `"auto"` (the
   * LLM when one is usable, else the local GLiNER extractor), `"llm"`,
   * `"gliner"` (alias `"gliner_demo"`) or another registered kind.
   * Overrides `GRAPH_EXTRACTOR`.
   */
  extractor?: string;
}

/**
 * A custom graph model in the spec DSL: entities with typed fields and
 * relations. Mirrors Python's `GraphSchemaSpec`. Every key may be spelled in
 * camelCase (as the cognee frontend writes it) or snake_case (as Python and
 * the docs write it), but not both at once; unknown keys are rejected. See
 * docs/guides/custom-graph-schema.md.
 */
export interface GraphSchemaSpec {
  options?: { autoTypeNodes?: boolean } | { auto_type_nodes?: boolean };
  /** Entity used as the top-level model; defaults to the first entity. */
  root?: string | null;
  /** 1 to 50 entities. */
  entities: GraphEntitySpec[];
}

/** `camel` or `snake`: one spelling of an optional key, never both. */
type EitherKey<Camel extends string, Snake extends string, T> =
  | ({ [K in Camel]?: T } & { [K in Snake]?: never })
  | ({ [K in Snake]?: T } & { [K in Camel]?: never });

/** `camel` or `snake`: one spelling of a required key. */
type OneKey<Camel extends string, Snake extends string, T> =
  | ({ [K in Camel]: T } & { [K in Snake]?: never })
  | ({ [K in Snake]: T } & { [K in Camel]?: never });

/** One entity (node type) of a {@link GraphSchemaSpec}. */
export type GraphEntitySpec = {
  /** An identifier: a letter, then letters, digits or underscores. */
  name: string;
  description?: string | null;
  /** Up to 40 fields. */
  fields?: GraphFieldSpec[];
} & EitherKey<"primaryLabelField", "primary_label_field", string | null> &
  /** Fields embedded for search; empty or unset means `["name"]`. */
  EitherKey<"indexFields", "index_fields", string[] | null> &
  /** Fields that merge equal nodes; unset means `["name"]`, `[]` opts out. */
  EitherKey<"identityFields", "identity_fields", string[] | null>;

/** Accepted for frontend parity; not compiled. */
export interface GraphInverseSpec {
  enabled?: boolean;
  name?: string | null;
  cardinality?: "one" | "many" | null;
}

/** One field of a {@link GraphEntitySpec}, discriminated by `kind`. */
export type GraphFieldSpec =
  | ({
      kind: "primitive";
      name: string;
      required?: boolean;
      description?: string | null;
    } & EitherKey<"primitiveType", "primitive_type", "string" | "number" | "boolean" | "date">)
  | ({
      kind: "enum";
      name: string;
      required?: boolean;
      description?: string | null;
    } & OneKey<"enumValues", "enum_values", string[]>)
  | {
      kind: "relation";
      name: string;
      relation: {
        cardinality?: "one" | "many";
        inverse?: GraphInverseSpec | null;
      } & OneKey<"targetEntityName", "target_entity_name", string>;
      required?: boolean;
      description?: string | null;
    };

/**
 * A data item row. Mirrors `cognee_models::Data` (Serialize).
 *
 * All fields are snake_case (Rust's default `serde` serialization).
 */
export interface CogneeDataRecord {
  id: string;
  name: string;
  raw_data_location: string;
  original_data_location: string;
  extension: string;
  mime_type: string;
  content_hash: string;
  owner_id: string;
  created_at: string;
  updated_at: string | null;
  label: string | null;
  original_extension: string | null;
  original_mime_type: string | null;
  loader_engine: string | null;
  raw_content_hash: string | null;
  tenant_id: string | null;
  external_metadata: string | null;
  node_set: string | null;
  pipeline_status: string | null;
  token_count: number;
  data_size: number;
  last_accessed: string | null;
  importance_weight: number | null;
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
  added: CogneeDataRecord[];
  addedCount: number;
  deduplicated: CogneeDataRecord[];
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

/** All 18 search type wire names (SCREAMING_SNAKE_CASE, matching Rust serde). */
export type SearchTypeString =
  | "SUMMARIES"
  | "CHUNKS"
  | "RAG_COMPLETION"
  | "TRIPLET_COMPLETION"
  | "GRAPH_COMPLETION"
  | "GRAPH_COMPLETION_DECOMPOSITION"
  | "GRAPH_SUMMARY_COMPLETION"
  | "CYPHER"
  | "NATURAL_LANGUAGE"
  | "GRAPH_COMPLETION_COT"
  | "GRAPH_COMPLETION_CONTEXT_EXTENSION"
  | "FEELING_LUCKY"
  | "FEEDBACK"
  | "TEMPORAL"
  | "CODING_RULES"
  | "CHUNKS_LEXICAL"
  | "GRAPH_REPORT"
  | "HYBRID_COMPLETION";

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
  /** SCREAMING_SNAKE_CASE search type. Defaults to "HYBRID_COMPLETION". */
  searchType?: SearchTypeString;
  /** Dataset names to restrict the search to. */
  datasets?: string[];
  /** Dataset UUIDs to restrict the search to. */
  datasetIds?: string[];
  /**
   * Tenant UUID string, matching the `tenant` passed to `add`/`cognify`.
   * Scopes `datasets` name resolution: one handle can hold same-named
   * datasets in several tenants, and without this a name may resolve to the
   * wrong one. Omit for the single-tenant default.
   */
  tenant?: string;
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
  /** How `nodeName` combines: `"OR"` (any name, default) or `"AND"` (all names). */
  nodeNameFilterOperator?: "AND" | "OR";
  /**
   * Skip the completion and return what the LLM would have received: the
   * response's `user_prompt` / `system_prompt` (the result is the user prompt)
   * for the completion types, else the bare context.
   */
  onlyContext?: boolean;
  /** Combine context from multiple retrieval paths. */
  useCombinedContext?: boolean;
  /** Include verbose diagnostics in the response. */
  verbose?: boolean;
  /** Detect feedback about the previous response before searching. */
  autoFeedbackDetection?: boolean;
  /**
   * Graph searches only: rank the triplets within this many hops of the
   * nearest vector-search hits, not just the edges touching them. A positive
   * integer; larger values load more of the graph.
   */
  neighborhoodDepth?: number;
  /**
   * With `neighborhoodDepth`: expand from at most this many hits, nearest
   * first (all of them when omitted). A positive integer.
   */
  neighborhoodSeedTopK?: number;
  /**
   * Per-retriever knobs, forwarded verbatim to the retriever the search
   * resolves to. Keys are snake_case and retriever-defined; a key the chosen
   * retriever does not know is ignored rather than rejected.
   *
   * The hybrid retriever reads `text_summaries_top_k` here — how many
   * `TextSummary` candidates its chunk lane fuses against the `DocumentChunk`
   * lane, defaulting to the chunk lane's own top-k.
   */
  retrieverSpecificConfig?: Record<string, unknown>;
  /** User UUID override (defaults to the handle's owner). */
  userId?: string;
}

/** Options accepted by `cogneeRecall`. All fields are optional. */
export interface CogneeRecallOptions {
  /** SCREAMING_SNAKE_CASE search type for the graph retrieval leg. */
  searchType?: SearchTypeString;
  /** Dataset names to restrict graph search to. */
  datasets?: string[];
  /**
   * Dataset UUIDs to restrict graph search to. Takes precedence over
   * `datasets` when both are given; an empty array is no filter.
   * Every entry must be a valid UUID — a malformed one is rejected rather
   * than dropped, so a typo cannot silently widen the query.
   */
  datasetIds?: string[];
  /**
   * Tenant UUID string, matching the `tenant` passed to `add`/`cognify`.
   * Scopes `datasets` name resolution the same way it does for search.
   */
  tenant?: string;
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
  /** As `CogneeSearchOptions.neighborhoodDepth`. */
  neighborhoodDepth?: number;
  /** As `CogneeSearchOptions.neighborhoodSeedTopK`. */
  neighborhoodSeedTopK?: number;
}

/**
 * A single item in a `SearchResponse.result` when `kind === "Items"`.
 *
 * Mirrors `cognee_search::SearchItem` (Serialize). `payload` is a
 * heterogeneous JSON object whose shape depends on the search type.
 */
export interface CogneeSearchItem {
  id: string | null;
  score: number | null;
  payload: Record<string, unknown>;
}

/** A knowledge-graph node returned in the `context` / `graphs` maps. */
export interface CogneeSearchGraphNode {
  id: string;
  label: string;
}

/** A knowledge-graph edge returned in the `context` / `graphs` maps. */
export interface CogneeSearchGraphEdge {
  source: string;
  target: string;
  relationship: string;
  weight: number | null;
}

/** A named graph (nodes + edges) attached to a `CogneeSearchResponse`. */
export interface CogneeSearchGraph {
  nodes: CogneeSearchGraphNode[];
  edges: CogneeSearchGraphEdge[];
}

/**
 * The discriminated `result` field of `CogneeSearchResponse`.
 *
 * Mirrors the `SearchOutput` enum (`#[serde(tag = "kind", content = "data")]`).
 * When `kind === "Items"`, `data` is `CogneeSearchItem[]`.
 * When `kind === "Text"`, `data` is `string`.
 * When `kind === "Texts"`, `data` is `string[]`.
 * When `kind === "GraphQueryRows"`, `data` is a two-dimensional JSON array.
 * When `kind === "Rules"`, `data` is `Array<{ node_set: string; text: string }>`.
 * When `kind === "Ack"`, `data` is `{ message: string }`.
 * When `kind === "Structured"`, `data` is an arbitrary JSON value.
 */
export type CogneeSearchOutput =
  | { kind: "Items"; data: CogneeSearchItem[] }
  | { kind: "Text"; data: string }
  | { kind: "Texts"; data: string[] }
  | { kind: "GraphQueryRows"; data: unknown[][] }
  | { kind: "Rules"; data: Array<{ node_set: string; text: string }> }
  | { kind: "Ack"; data: { message: string } }
  | { kind: "Structured"; data: unknown };

/**
 * Search response from `cogneeSearch`.
 *
 * Mirrors `cognee_search::SearchResponse` (Serialize, snake_case fields).
 */
export interface CogneeSearchResponse {
  search_type: SearchTypeString;
  result: CogneeSearchOutput;
  context: Record<string, CogneeSearchItem[]> | null;
  graphs: Record<string, CogneeSearchGraph> | null;
  diagnostics: Record<string, unknown> | null;
  datasets: string[] | null;
  only_context: boolean;
  use_combined_context: boolean;
  verbose: boolean;
  /**
   * With `only_context`: the user message the completion would have sent (also
   * the `result`, as `Text`). Null when the retriever sends no single templated
   * prompt or retrieval found nothing — `result` is then the bare context.
   */
  user_prompt?: string | null;
  /** The system message paired with `user_prompt`; set and null together. */
  system_prompt?: string | null;
  /**
   * With `only_context` over several datasets (not combined): each dataset's
   * own prompt, keyed by dataset id like `context`. A dataset with no context
   * has no entry; `user_prompt` is the first dataset's.
   */
  prompts?: Record<string, { user_prompt: string; system_prompt: string }> | null;
}

/**
 * A single recall result item. Mirrors `cognee_search::RecallItem` (Serialize).
 *
 * `source` is one of `"session"`, `"graph"`, `"trace"`, `"graph_context"`
 * (snake_case, matching the `RecallSource` serde rename).
 * `content` is a heterogeneous JSON value whose shape depends on the source.
 */
export interface CogneeRecallItem {
  source: "session" | "graph" | "trace" | "graph_context";
  content: Record<string, unknown>;
  score: number;
}

/** Result returned by `cogneeRecall`. */
export interface CogneeRecallResult {
  /** Source-tagged result items from all contributing sources. */
  items: CogneeRecallItem[];
  /** The search type used for the graph retrieval leg, or null. */
  searchTypeUsed: SearchTypeString | null;
  /** Whether auto-routing was applied. */
  autoRouted: boolean;
  /** The raw graph search response, or null if no graph leg ran. */
  searchResponse: CogneeSearchResponse | null;
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
  /**
   * Graph extractor for the cognify step, as for `cognify`. Not
   * together with `sessionId`.
   */
  extractor?: string;
}

/**
 * Terminal status of a `remember` operation. Mirrors the Rust `RememberStatus`
 * enum wire names (`crates/lib/src/api/remember.rs`).
 *
 * The synchronous SDK only ever returns a terminal state; `PipelineRunStarted`
 * exists for symmetry with the async/HTTP background path and is not emitted
 * here.
 */
export type CogneeRememberStatus =
  | "PipelineRunStarted"
  | "PipelineRunCompleted"
  | "PipelineRunErrored"
  | "SessionStored";

/**
 * Per-item information in a `CogneeRememberResult`.
 *
 * Mirrors the Rust `RememberItemInfo` struct (Serialize, snake_case fields).
 */
export interface CogneeRememberItemInfo {
  id: string | null;
  name: string | null;
  content_hash: string | null;
  /** Token count (`null` when not yet computed). */
  token_count: number | null;
  /** Size of the raw data in bytes (`null` when unknown). */
  data_size: number | null;
  mime_type: string | null;
}

/**
 * Result of `cogneeRemember` / `cogneeRememberEntry`.
 *
 * Mirrors the Rust `RememberResult` struct (Serialize). **Fields are
 * snake_case** — unlike the camelCase surface of `config.get()`, `add`, and
 * `cognify` — because `remember` deliberately preserves Python-SDK wire parity:
 * Python's `RememberResult.to_dict()` is a plain-class dict, not a pydantic
 * alias-converted model, so it emits snake_case keys. The HTTP v2 `remember`
 * DTO makes the same carve-out, and this binding matches it. See
 * issue #46.
 *
 * Both the file/text path (`remember`) and the typed-entry path
 * (`rememberEntry`) return this same shape; every key is always present
 * (`null` when not applicable), so which path ran is read off the populated
 * fields — the graph path fills `dataset_id` / `pipeline_run_id` /
 * `content_hash` / `items`, the session path fills `session_ids` / `entry_type`
 * / `entry_id`.
 */
export interface CogneeRememberResult {
  status: CogneeRememberStatus;
  dataset_name: string;
  dataset_id: string | null;
  session_ids: string[] | null;
  pipeline_run_id: string | null;
  /** Wall-clock seconds the operation took (`null` when not measured). */
  elapsed_seconds: number | null;
  /** Content hash of the first item (`null` on the session path). */
  content_hash: string | null;
  items_processed: number;
  items: CogneeRememberItemInfo[];
  error: string | null;
  /** `"qa"` / `"trace"` / `"feedback"` on the typed-entry path; `null` otherwise. */
  entry_type: string | null;
  /** Typed-entry id from the session manager; `null` on the file/text path. */
  entry_id: string | null;
}

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

/** What one improve stage did (Python's `StageResult`). */
export interface CogneeImproveStage {
  stage: string;
  status: "completed" | "already_completed" | "skipped" | "errored";
  /** Why the stage was skipped; set whenever `status` is `"skipped"`. */
  reason: string | null;
  error: string | null;
  /** Integer counts the stage reported, keyed as the stage names them. */
  counts: Record<string, number>;
  /** Wall time of the stage's run; 0 for a stage its gate skipped. */
  durationMs: number;
}

/** Result of `cogneeImprove`. Hand-built from `ImproveResult` (not Serialize). */
export interface CogneeImproveResult {
  stagesRun: string[];
  /** Per-stage records; only `build_truth_subspace` reports one so far. */
  stages: CogneeImproveStage[];
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

/**
 * Result of a delete operation. Mirrors `cognee_delete::DeleteResult` (Serialize,
 * snake_case fields).
 */
export interface CogneeDeleteResultDetail {
  deleted_datasets: number;
  deleted_dataset_links: number;
  deleted_data: number;
  deleted_storage_files: number;
  deleted_graph_nodes: number;
  deleted_vector_points: number;
  deleted_provenance_nodes: number;
  deleted_provenance_edges: number;
  deleted_orphan_entities: number;
  deleted_orphan_entity_types: number;
  deleted_orphan_edge_types: number;
  deleted_pipeline_runs: number;
  cleared_pipeline_statuses: number;
  deleted_search_queries: number;
  pruned_sessions: boolean;
  warnings: string[];
}

/** Result of `cogneeForget`. Hand-built JSON. */
export interface CogneeForgetResult {
  target: string;
  deleteResult: CogneeDeleteResultDetail;
}

/** Options accepted by `cogneeUpdate`. */
export interface CogneeUpdateOptions {
  tenant?: string;
}

/** Result of `cogneeUpdate`. Hand-built JSON. */
export interface CogneeUpdateResult {
  deletedDataId: string;
  deleteResult: CogneeDeleteResultDetail;
  newData: CogneeDataRecord[];
  cognifyResult: CogneeCognifyResult | null;
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

/**
 * A dataset row. Mirrors `cognee_models::Dataset` (Serialize, snake_case fields).
 */
export interface CogneeDataset {
  id: string;
  name: string;
  owner_id: string;
  tenant_id: string | null;
  created_at: string;
  updated_at: string | null;
}

/**
 * A data item row as returned by the dataset listing APIs.
 * Alias of `CogneeDataRecord` for backward-compatibility with existing code
 * that references `CogneeData`.
 */
export type CogneeData = CogneeDataRecord;

/**
 * Result of a delete operation.
 * Alias of `CogneeDeleteResultDetail` for backward-compatibility with existing
 * code that references `CogneeDeleteResult`.
 */
export type CogneeDeleteResult = CogneeDeleteResultDetail;

/**
 * A user row. Mirrors `cognee_models::User` (Serialize, snake_case fields).
 *
 * `hashed_password` is intentionally omitted — the Rust SDK does not implement
 * authentication; password handling is delegated to the HTTP/auth layer.
 */
export interface CogneeUser {
  id: string;
  email: string;
  is_active: boolean;
  is_superuser: boolean;
  tenant_id: string | null;
  created_at: string;
  updated_at: string | null;
}

/**
 * A notebook row. Mirrors `cognee_database::Notebook` (Serialize, snake_case fields).
 *
 * `cells` is a JSON array of notebook cells; its inner shape is opaque and
 * defined by the application layer.
 */
export interface CogneeNotebook {
  id: string;
  owner_id: string;
  name: string;
  cells: unknown[];
  deletable: boolean;
  created_at: string;
}

/**
 * Graph element IDs used to produce a Q&A answer.
 * Mirrors `cognee_session::UsedGraphElementIds` (Serialize, snake_case fields).
 */
export interface CogneeUsedGraphElementIds {
  node_ids: string[];
  edge_ids: string[];
}

/**
 * A session Q&A entry. Mirrors `cognee_session::SessionQAEntry` (Serialize,
 * snake_case fields).
 */
export interface CogneeSessionQAEntry {
  id: string;
  session_id: string;
  user_id: string | null;
  question: string;
  answer: string;
  context: string | null;
  created_at: string;
  feedback_text: string | null;
  feedback_score: number | null;
  used_graph_element_ids: CogneeUsedGraphElementIds | null;
  memify_metadata: Record<string, boolean> | null;
}

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

// Cloud-related types (`CogneeServeOptions`, `CogneeServeResult`,
// `CogneeDisconnectOptions`) are not part of this package, along with the
// `serve` / `disconnect` functions that consume them.
