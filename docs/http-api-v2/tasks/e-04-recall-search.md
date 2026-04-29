# E-04 — `POST /api/v1/recall`

| | |
|---|---|
| Wire path | `POST /api/v1/recall` |
| Status | **Partial** — DTO deliberately excludes `session_id` and `scope`. |
| Depends on | none on the HTTP side; uses `cognee-search` query router which already supports session-first lookups. |
| Effort | ~1 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Reverse the deliberate "do not add session_id" guard in [`crates/http-server/src/dto/recall.rs:29`](../../../crates/http-server/src/dto/recall.rs#L29) and add the two v2-defining parameters to the recall request: `session_id` (string) and `scope` (string OR list of strings, expanded via `cognee_search::query_router::normalize_scope`).

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `RecallPayloadDTO` | `cognee/api/v1/recall/routers/get_recall_router.py` | 23–48 |
| `POST ""` handler | same | 78–145 |
| `normalize_scope` | `cognee/memory/entries.py` | 81–115 |

Request body (additions only, all other fields already match):

```json
{
  ...existing v1 fields...,
  "session_id": "abc123",                      // optional
  "scope": "graph_context"                     // OR ["graph", "session"] OR "all"
}
```

`scope` semantics:
- `null` or `"auto"` → session-first if `session_id` present, else `graph`.
- `"all"` → expands to `["graph", "session", "trace", "graph_context"]`.
- Otherwise validated against the same allowlist (`graph`, `session`, `trace`, `graph_context`). Unknown → `ValueError` → 422 in Python; Rust must match.

## 3. Current Rust state

- Handler `post_recall` at `crates/http-server/src/routers/recall.rs:117`.
- `SearchOrchestrator::search` already accepts `session_id: Option<String>` (the field is plumbed; just hardcoded to `None` at line 146).
- `crates/search/src/query_router.rs` implements scope routing with a `RecallScope` enum — verify the enum names match Python literals exactly.

## 4. Implementation steps

1. **Extend the DTO** at `crates/http-server/src/dto/recall.rs:32`:
   ```rust
   pub struct RecallPayloadDTO {
       ...existing fields...
       #[serde(default)]
       pub session_id: Option<String>,

       #[serde(default, deserialize_with = "deserialize_scope")]
       pub scope: Option<Vec<String>>,        // string OR list, normalized in serde
   }
   ```
   The custom `deserialize_with` accepts both `"all"` and `["graph"]` shapes. After deserialization, run through `normalize_scope` to expand `"all"` and dedupe. Reject unknown values with `serde::de::Error` → 422.

2. **Remove the guard** in the same file. Delete the comment "Do NOT add `session_id`" and the negative test `test_recall_dto_does_not_accept_session_id` (line 93). Replace with a positive test that confirms `session_id` deserializes.

3. **Plumb through the handler** at `crates/http-server/src/routers/recall.rs:146`:
   ```rust
   session_id: payload.session_id.clone(),
   ```
   Pass `payload.scope` into the request as well — extend `SearchRequest` with a `scope: Option<Vec<String>>` field if not present, and have `SearchOrchestrator` honor it (the routing already exists in `query_router.rs`; just thread the override through).

4. **Validation** for unknown scope values, per Decision 7 (see [`../README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)):
   - Status code: **`400`** (Python overrides FastAPI's default 422 globally).
   - Body:
     ```json
     {
       "detail": [{
         "loc": ["body", "scope"],
         "msg": "Unknown recall scope(s): ['foo']. Valid values: ['graph', 'graph_context', 'session', 'trace']",
         "type": "value_error"
       }],
       "body": <raw input>
     }
     ```
   - Implementation: the custom `deserialize_with` for `scope` (step 1) returns `serde::de::Error::custom(...)` with the message above; the `ValidatedJson` extractor (already in v1) wraps it into the `[{loc, msg, type}]` envelope automatically. Confirm by reading [`crates/http-server/src/middleware/validation.rs:88-101`](../../../crates/http-server/src/middleware/validation.rs#L88-L101) — the existing wrapper produces exactly this shape with `type: "value_error.json_parse"`. The `scope`-specific message must surface from the `serde::de::Error::custom(...)` call so it reaches `err.to_string()` in the `msg` field.

## 5. Tests

- Update `crates/http-server/src/dto/recall.rs` tests:
  - `recall_dto_accepts_session_id` (replaces the deleted negative test).
  - `recall_dto_accepts_scope_as_string`.
  - `recall_dto_accepts_scope_as_list`.
- Update `crates/http-server/tests/test_recall.rs`:
  - `session_first_lookup_when_session_id_present_and_scope_auto`.
  - `scope_all_fans_out_to_four_sources`.
  - `scope_graph_only_skips_session_cache`.
  - `unknown_scope_returns_400_with_python_validation_envelope` — **integration test** that POSTs `{"query":"x","scope":"foo"}` and asserts:
    - Status `400` (NOT 422).
    - Body has `detail` array of length 1.
    - `body.detail[0].loc` equals `["body","scope"]`.
    - `body.detail[0].msg` contains the `"Unknown recall scope(s)"` substring.
    - `body.detail[0].type` is a string ending in `value_error` (Python emits `"value_error"`; tolerate Rust's `"value_error.json_parse"` only if the v1 envelope already does — verify against an existing v1 test).
    - Top-level `body.body` echoes the raw input JSON.
- Cross-SDK parity in `e2e-cross-sdk/harness/test_http_v2_recall.py`:
  - Send `{"query": "...", "session_id": "s1", "scope": "auto"}` to both servers; structurally diff the `[SearchResult]` lists.
  - Send `{"query":"x","scope":"foo"}` to both servers; assert the `400` body structurally diffs equal (with the `msg` field treated as substring-match — Python's exact wording may evolve).

## 6. Acceptance criteria

- [ ] `RecallPayloadDTO` accepts `session_id` and `scope`.
- [ ] Recall handler passes both through to `SearchOrchestrator`.
- [ ] `scope: null` defaults to Python's `"auto"` semantics.
- [ ] `scope: "all"` expands to all four sources.
- [ ] Unknown scope returns **400** (not 422) with the Python FastAPI-shaped envelope per [`../README.md §1.1` Decision 7](../README.md#11-wire-conventions-project-wide-set-by-decision-6).
- [ ] Integration test asserts byte-shape parity on the `400` envelope (`detail` array, `loc`, `msg`, `type`, `body`).
- [ ] Cross-SDK parity test passes for both happy path and `400` validation path.
- [ ] **The negative-test guardrail is gone** (no comment in the codebase still claims `session_id` should not be on the DTO).

## 7. References

- [Python recall handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L78)
- [Python `normalize_scope`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py#L81)
- [Rust query router](../../../crates/search/src/query_router.rs)
- [Rust DTO](../../../crates/http-server/src/dto/recall.rs)
