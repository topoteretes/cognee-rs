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
- `cogneeSearch(handle, query, opts?) -> Promise<SearchResponse>`
  - Build a `SearchRequest` from `query` + `opts` (search type, datasets, top_k, filters).
  - Call `svc.search_orchestrator.search(&request)`.
- `cogneeRecall(handle, query, opts?) -> Promise<RecallResult>`
  - Call `api::recall(query, queryType?, datasets?, topK, autoRoute, sessionId?, userId?,
    svc.search_orchestrator, svc.session_store, svc.session_manager, scope?)`.

### Type marshalling
- **`SearchType`** — 15 variants serialized as SCREAMING_SNAKE_CASE strings (e.g.
  `GRAPH_COMPLETION`, `RAG_COMPLETION`, `CHUNKS`, `TEMPORAL`, `CYPHER`, …). Expose as a TS string
  union / enum mirroring the serde names exactly (Python wire parity).
- **`RecallScope` / `ScopeInput`** — `Auto` | `Session` | `Trace` | `GraphContext` | `Graph`;
  accept an array of scope strings, normalize via `normalize_scope`.
- **`SearchRequest`** in / **`SearchResponse`** out — both serde; marshal as JSON. `RecallResult`
  carries `items` (source-tagged), `search_type_used`, `auto_routed`, and the raw
  `search_response`.

## Functionalities

- `search` is the direct, type-explicit query; `recall` adds session-first routing (keyword
  overlap against session QA), then falls back to graph search, honoring `scope`.
- Default search type = `GraphCompletion` when unset, matching the SDK default.

## Dependencies & ordering

Needs Phases 1–3 (data must be added + cognified to return meaningful results).

## Risks

- `SearchType` string drift vs the serde rename — assert exact strings in a test.
- `recall` needs the session store/manager wired in `CogneeServices`; verify the `cache_backend`
  selection from Phase 1 works for the chosen test backend (fs is simplest).

## Done when

- A live `add → cognify → search` round-trip returns results from Node, and `recall` returns
  session-routed results — both verified in the Phase 9 Tier-B e2e (retrieval needs cognified
  data + LLM).
- A small Tier-A assertion locks the `SearchType` ↔ string mapping (no backend needed).
