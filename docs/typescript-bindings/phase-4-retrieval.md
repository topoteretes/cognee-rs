# Phase 4 — Retrieval: search, recall

← [Index](../typescript-bindings-plan.md)

**Goal:** query the knowledge graph from Node. Surfaces **#3 `search`** (raw, 15 search types)
and **#4 `recall`** (smart routing with session + scope awareness).

## Scope

- **In:** `search` over `SearchOrchestrator`, `recall` over `api::recall`, the `SearchType` and
  `RecallScope` marshalling, session wiring for QA history.
- **Out:** writing back QA/feedback into sessions (that is `improve`/sessions in Phase 5).

## Structures

### `js/cognee-neon/src/sdk_retrieval.rs`

**`cogneeSearch(handle, query, opts?) -> Promise<SearchResponse>`**
- Deserialize `opts` (optional JS object stringified via `JSON.stringify`) into a
  `serde_json::Value`; build a `SearchRequest` from it using the same
  `js_to_value` / `parse_js` helpers already in `sdk_ops.rs`.
- Accepted `opts` fields (all optional, mirror `SearchRequest` field names):
  `searchType` (SCREAMING_SNAKE_CASE string, default `"GRAPH_COMPLETION"`),
  `datasets` (string array), `datasetIds` (UUID-string array), `topK` (number),
  `systemPrompt`, `sessionId`, `nodeType`, `nodeName` (string array),
  `onlyContext` (bool), `useCombinedContext` (bool), `verbose` (bool),
  `saveInteraction` (bool, default true), `autoFeedbackDetection` (bool),
  `userId` (UUID string).
- Populate `SearchRequest.user_id` from `state.owner_id()` (type `Option<Uuid>`).
  The `datasets` name-resolution path inside `SearchOrchestrator::search` requires
  `user_id` to be set when `datasets` is supplied.
- Call `svc.search_orchestrator.search(&request)`.
- `SearchResponse` **is** `Serialize` — marshal via `serde_json::to_string` +
  `parse_js`.

**`cogneeRecall(handle, query, opts?) -> Promise<RecallResult>`**
- Exact `api::recall` signature (all positional, no builder):
  ```
  recall(
      query_text: &str,
      query_type: Option<SearchType>,
      datasets: Option<Vec<String>>,
      top_k: usize,
      auto_route: bool,
      session_id: Option<&str>,
      user_id: Option<&str>,           // &str, NOT Uuid
      search_orchestrator: &SearchOrchestrator,
      session_store: Option<&dyn SessionStore>,
      session_manager: Option<&SessionManager>,
      scope: Option<Vec<RecallScope>>,
  ) -> Result<RecallResult, ApiError>
  ```
- `user_id` param is `Option<&str>`, not `Option<Uuid>`. Convert from
  `state.owner_id()` via `owner_id.to_string()` and pass `Some(&owner_str)`.
- `session_store` and `session_manager` are both `Option<&…>`. Pass
  `Some(svc.session_store.as_ref())` and `Some(svc.session_manager.as_ref())`.
- `scope` is built from `opts.scope` (string or string array) via
  `api::normalize_scope(Some(ScopeInput::…))` — `ScopeInput` is in
  `cognee_lib::api::{ScopeInput, normalize_scope}`.
- **`RecallResult` does NOT derive `Serialize`** (derives only `Debug, Clone`).
  Hand-build the JSON from its fields:
  - `items: Vec<RecallItem>` — `RecallItem` IS `Serialize`, so
    `serde_json::to_value(&result.items)` works.
  - `search_type_used: Option<SearchType>` — `SearchType` IS `Serialize`.
  - `auto_routed: bool`
  - `search_response: Option<SearchResponse>` — `SearchResponse` IS `Serialize`.
  Result JSON:
  ```json
  {
    "items": [...],
    "searchTypeUsed": "GRAPH_COMPLETION" | null,
    "autoRouted": false,
    "searchResponse": { ... } | null
  }
  ```

### Type marshalling

- **`SearchType`** — 15 variants, `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`.
  Exact wire names (verified in `search_type.rs` tests):
  `SUMMARIES`, `CHUNKS`, `RAG_COMPLETION`, `TRIPLET_COMPLETION`,
  `GRAPH_COMPLETION` (default), `GRAPH_SUMMARY_COMPLETION`, `CYPHER`,
  `NATURAL_LANGUAGE`, `GRAPH_COMPLETION_COT`,
  `GRAPH_COMPLETION_CONTEXT_EXTENSION`, `FEELING_LUCKY`, `FEEDBACK`,
  `TEMPORAL`, `CODING_RULES`, `CHUNKS_LEXICAL`. Marshal `opts.searchType`
  string → `SearchType` via `serde_json::from_value(json!(opts_str))`.
- **`RecallScope`** — 5 variants (wire names in `snake_case`):
  `auto`, `graph`, `session`, `trace`, `graph_context`.
  The value `"all"` is a valid `ScopeInput` wire value that `normalize_scope`
  expands to all four concrete scopes. Accept `opts.scope` as a single string
  or an array of strings; convert via `ScopeInput::Single` / `ScopeInput::Many`.
  `ScopeInput` does **not** implement `Serialize`/`Deserialize` — it is a
  plain Rust enum with `From<&str>` / `From<String>` / `From<Vec<String>>`
  impls. Build it directly from the string(s) extracted from the opts JSON.
- **`SearchRequest`** in — IS `Serialize`/`Deserialize`; hand-populate the
  struct from parsed opts (do not use `serde_json::from_value` on the whole
  opts object because of camelCase ↔ snake_case mismatch).
- **`SearchResponse`** out — IS `Serialize`; pass through `serde_json::to_string`
  + `parse_js`.
- **`RecallResult`** — NOT `Serialize`; hand-build JSON as described above.
- **`RecallItem`** — IS `Serialize` (`#[derive(Debug, Clone, Serialize, Deserialize)]`).

### JS-side types to add in `js/src/native.ts`

```typescript
export type SearchTypeString =
  | "SUMMARIES" | "CHUNKS" | "RAG_COMPLETION" | "TRIPLET_COMPLETION"
  | "GRAPH_COMPLETION" | "GRAPH_SUMMARY_COMPLETION" | "CYPHER"
  | "NATURAL_LANGUAGE" | "GRAPH_COMPLETION_COT"
  | "GRAPH_COMPLETION_CONTEXT_EXTENSION" | "FEELING_LUCKY"
  | "FEEDBACK" | "TEMPORAL" | "CODING_RULES" | "CHUNKS_LEXICAL";

export type RecallScopeString =
  | "auto" | "graph" | "session" | "trace" | "graph_context" | "all";

export interface CogneeSearchOptions {
  searchType?: SearchTypeString;
  datasets?: string[];
  datasetIds?: string[];
  topK?: number;
  systemPrompt?: string;
  sessionId?: string;
  nodeType?: string;
  nodeName?: string[];
  onlyContext?: boolean;
  useCombinedContext?: boolean;
  verbose?: boolean;
  saveInteraction?: boolean;
  autoFeedbackDetection?: boolean;
  userId?: string;
}

export interface CogneeRecallOptions {
  searchType?: SearchTypeString;
  datasets?: string[];
  topK?: number;
  autoRoute?: boolean;
  sessionId?: string;
  scope?: RecallScopeString | RecallScopeString[];
}

// SearchResponse mirrors cognee_search::SearchResponse (serde JSON).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type CogneeSearchResponse = any;

export interface CogneeRecallResult {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  items: any[];
  searchTypeUsed: SearchTypeString | null;
  autoRouted: boolean;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  searchResponse: any | null;
}
```

Add to `NativeBindings`:
```typescript
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
```

## Functionalities

- `search` is the direct, type-explicit query; `recall` adds session-first routing
  (keyword overlap against session QA), then falls back to graph search, honoring
  `scope`.
- Default search type = `GraphCompletion` when unset, matching the SDK default
  (`#[default]` on `SearchType::GraphCompletion`).
- The `SearchOrchestrator` in `CogneeServices` is already wired with
  `session_manager` and `dataset_resolver` (built in Phase 1 `services.rs`); no
  additional wiring is needed in this phase.
- Session QA history persistence happens **inside** `SearchOrchestrator::search`
  (auto-saves `Q→A` pairs when `session_id` + `SearchOutput::Text`). No extra
  code needed for session writes.

## Dependencies & ordering

Needs Phases 1–3 (data must be added + cognified to return meaningful results).
`CogneeServices` (Phase 1) already has `search_orchestrator`, `session_store`,
`session_manager`.

## Risks

- **`RecallResult` is not `Serialize`** — must be hand-serialized. `RecallItem`,
  `RecallSource`, `SearchResponse`, `SearchType` are all `Serialize`, so the
  hand-built JSON is straightforward. Do not add `#[derive(Serialize)]` to
  `RecallResult` in this phase (out of scope; would require a `crates/lib`
  change that risks other consumers).
- **`SearchType` string drift** — assert exact SCREAMING_SNAKE_CASE strings in a
  Tier-A test (no backend needed). A test already exists in
  `crates/search/src/types/search_type.rs`; add a binding-level assertion to
  `js/cognee-neon/src/sdk_retrieval.rs` or a Jest Tier-A test.
- **`user_id` type in `recall`** — takes `Option<&str>`, not `Option<Uuid>`. Call
  site must convert `Uuid` → `String` and borrow `&str`.
- **`datasets` name filter requires `user_id` in `SearchRequest`** — the
  orchestrator's dataset-resolution path errors with `InvalidInput` when
  `user_id` is `None` and `datasets` is set. Always populate
  `SearchRequest.user_id` from `state.owner_id()`.
- **`session_store` / `session_manager` are `Option<&dyn …>`** in `api::recall` —
  not `Arc`. Pass references via `svc.session_store.as_ref()` and
  `svc.session_manager.as_ref()`.
- **`SearchType` parse from opts string** — use
  `serde_json::from_value::<SearchType>(serde_json::Value::String(s))` to
  deserialize; this is the exact same path the HTTP server uses and is
  guaranteed to match the serde wire names.

## Done when

- A live `add → cognify → search` round-trip returns results from Node, and
  `recall` returns session-routed results — both verified in the Phase 9 Tier-B
  e2e (retrieval needs cognified data + LLM).
- A Tier-A test (no backend) asserts every `SearchType` variant round-trips
  through the SCREAMING_SNAKE_CASE string form used at the JS boundary.
- A Tier-A test verifies `RecallScope` wire strings (`"auto"`, `"graph"`,
  `"session"`, `"trace"`, `"graph_context"`, `"all"`) are accepted without error.
