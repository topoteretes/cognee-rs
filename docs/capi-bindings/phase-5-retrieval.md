# Phase 5 — Retrieval: search / recall

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the round-trip closes: `add → cognify → search`/`recall` from C, over all 15
search types. Reference: `js/cognee-neon/src/sdk_retrieval.rs` + `js/src/types.ts`
(`CogneeSearchOptions`, `CogneeRecallOptions`, `RecallScopeString`, `SearchTypeString`).

## Prerequisites

Phase 4 (search needs cognified data).

## Exported functions

Async-only (D4), Phase-2 conventions:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_search` | `query`, `opts_json` | `CogneeSearchResponse` (pass-through serde of the orchestrator response, same shape as TS) |
| `cg_sdk_recall` | `query`, `opts_json` | `{items[], searchTypeUsed, autoRouted, searchResponse}` |

`opts_json` fields (identical to TS, camelCase):

- search: `searchType` (one of the 15 `SCREAMING_SNAKE_CASE` strings), `datasets[]`,
  `datasetIds[]`, `topK`, `systemPrompt`, `sessionId`, `nodeType`, `nodeName[]`,
  `onlyContext`, `useCombinedContext`, `verbose`, `saveInteraction`,
  `autoFeedbackDetection`, `userId`.
- recall: `searchType`, `datasets[]`, `topK`, `autoRoute`, `sessionId`,
  `scope` (string or string array: `auto|graph|session|trace|graph_context|all`).

## Inherit TS decisions verbatim

- `SearchType` parsed via `serde_json::from_value(Value::String(s))` with the
  `SCREAMING_SNAKE_CASE` serde attribute — the same path as the HTTP server, guaranteed sync.
  Invalid string → `CG_ERR_VALIDATION` listing valid values.
- `ScopeInput::Single/Many` built directly from strings; empty array → `None` (recall applies
  its Auto default).
- `RecallResult` hand-built JSON with camelCase keys.

## Tasks

1. `capi/cognee-capi/src/sdk_retrieval.rs` — two async ops via the shared facade
   (`svc.search_orchestrator`, `api::recall`).
2. Tier-A smoke: invalid `searchType` → `CG_ERR_VALIDATION`; valid type against an empty
   mock-backed store returns a well-formed (possibly empty) response.
3. Tier-B example `capi/examples/example_sdk_add_cognify_search.c` — the flagship C example
   (mirrors `js/examples/add-cognify-search.ts`): env-driven config, add → cognify → search
   (`GRAPH_COMPLETION`) → recall; SKIPs without credentials.

## Exit criteria

- [ ] all 15 search-type strings accepted (Tier-A string-mapping check, mirroring TS's
      locked `SearchType ↔ string` test)
- [ ] recall with scopes + session routing
- [ ] live round-trip from C verified (Tier-B, gated in capi-check per D12)
- [ ] `cognee_sdk.h` regenerated
