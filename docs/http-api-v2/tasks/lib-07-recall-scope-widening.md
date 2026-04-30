# LIB-07 — `cognee_lib::api::recall::recall()` scope widening

| | |
|---|---|
| Scope | Library widening — extend `cognee_lib::api::recall::recall()` to accept a `scope` parameter and implement source fan-out across `graph` / `session` / `trace` / `graph_context`. Includes `RecallScope` enum, `normalize_scope()` helper, the missing `_search_trace` / `_fetch_graph_context` source helpers, and unit tests. **No HTTP changes** — E-04 consumes this in the next task. |
| Status | **Not Started** |
| Blocks | E-04 (`POST /recall` adds `session_id` + `scope`). |
| Depends on | LIB-02 (`SessionManager::add_agent_trace_step` / `get_agent_trace_session` — landed eec6f79); existing `SessionStore` for QA / trace lookups. |
| Effort | ~1 day. |
| Owner crate | `cognee-lib` (with new types in `cognee-models`) |

> **Decision (2026-04-30) — Decision 17**: split the original E-04 work into a library-widening prerequisite (this task, LIB-07) and the HTTP-layer task (E-04). Investigation 2026-04-30 found that the Rust `cognee_lib::api::recall::recall()` accepts only `session_id` + `auto_route` — it does NOT have a `scope` parameter, no `_search_trace` / `_fetch_graph_context` helpers, and no `RecallScope` enum. Honoring `scope` ∈ {trace, graph_context, all} requires this widening. The user (2026-04-30) chose the split (Option B from the investigation) over Option A (DTO + handler-only with a new D-2 wire divergence) so that v2 HTTP `POST /recall` retains strict Python parity. **No new wire divergence** is introduced. Investigation agent: do not re-litigate.

## 1. Goal

Bring `cognee_lib::api::recall::recall()` to byte-for-byte parity with Python's `cognee/api/v1/recall/recall.py` `recall()` function — specifically the four-source fan-out (`graph`, `session`, `trace`, `graph_context`) that scope-resolves and merges results across sources. Once landed, E-04 can plumb `session_id` + `scope` from the HTTP DTO straight through to the library function without any wire divergence.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `recall()` | `cognee/api/v1/recall/recall.py` | 373–475 |
| `normalize_scope()` | `cognee/memory/entries.py` | 81–115 |
| `_search_session()` helper | `cognee/api/v1/recall/recall.py` | ~280 |
| `_search_trace()` helper | `cognee/api/v1/recall/recall.py` | ~310 |
| `_fetch_graph_context()` helper | `cognee/api/v1/recall/recall.py` | ~340 |
| `_search_graph()` (existing) | `cognee/api/v1/recall/recall.py` | ~250 |

Clone the Python repo with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python` if not already present, then read the cited line ranges to confirm exact behavior.

### Behavior (Python parity)

`recall(query, *, session_id=None, scope=None, auto_route=False, ...)`:

1. Normalize `scope` via `normalize_scope` — `None` → `["auto"]`; `"all"` → `["graph", "session", "trace", "graph_context"]`; single string → `[string]`; list passes through after dedup; unknown values raise `ValueError`.
2. If `scope == ["auto"]`:
   - If `session_id` is set → behave as `["session"]` (session-first).
   - Else → behave as `["graph"]`.
3. For each scope source in canonical order (`session`, `trace`, `graph_context`, `graph`), run the corresponding helper:
   - `session` → look up QA history via `SessionStore::get_session_qa(session_id, query)`. Returns matched QA pairs.
   - `trace` → look up trace steps via `SessionManager::get_agent_trace_session(session_id, last_n=...)`. Returns trace entries.
   - `graph_context` → fetch graph context via `GraphDB`-driven walk from query-matched nodes. Returns subgraph snippets.
   - `graph` → existing `SearchOrchestrator::search` path.
4. Merge results into a single `RecallResult` with all sources represented.

### Edge cases

- `session_id` empty/missing + `scope` containing `session` or `trace` → return empty list for those sources (no error).
- Unknown scope values in the input list → `Err(ApiError::InvalidArgument(...))` with Python parity message.
- All four sources requested (`scope = "all"`) but session backend missing → degrade gracefully: graph + graph_context succeed, session + trace return empty.

## 3. Current Rust state (verified 2026-04-30)

- `cognee_lib::api::recall::recall` at [`crates/lib/src/api/recall.rs:69`](../../../crates/lib/src/api/recall.rs#L69):
  - Signature accepts `session_id: Option<String>` and `auto_route: bool` but no `scope` parameter.
  - Implements only the legacy session-first short-circuit (session OR graph).
- No `RecallScope` enum exists anywhere in `crates/`.
- No `normalize_scope` function exists anywhere in `crates/`.
- No `_search_trace` or `_fetch_graph_context` helpers exist.
- `SearchOrchestrator::search` at `crates/search/src/orchestration/search_orchestrator.rs` honors `session_id` for history loading and trace persistence but does NOT do source fan-out.
- `SessionManager::get_agent_trace_session` exists (landed at LIB-02 eec6f79) — usable for the `_search_trace` helper.
- `SessionStore` has `read_qa` (or equivalent) for the `_search_session` helper — already used elsewhere in `recall()`.
- `GraphDBTrait` exists for the `_fetch_graph_context` helper to walk graph context from matched nodes.

## 4. Implementation steps

1. **Add `RecallScope` enum** in `crates/lib/src/api/recall.rs` (or split into a sibling `recall_scope.rs` if it grows >100 lines):
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
   #[serde(rename_all = "snake_case")]
   pub enum RecallScope {
       Auto,
       Graph,
       Session,
       Trace,
       GraphContext,
   }
   ```
   Plus `RecallScope::ALL: &[Self] = &[Self::Graph, Self::Session, Self::Trace, Self::GraphContext]`.

2. **Add `normalize_scope()` helper** matching Python's `cognee/memory/entries.py:81-115`:
   ```rust
   /// Accepts None / single-string / list-of-string. Returns canonical
   /// dedup'd list of `RecallScope`. `"all"` expands to ALL four sources.
   /// `None` (or empty list) → `[Auto]`. Unknown strings → `Err`.
   pub fn normalize_scope(input: Option<ScopeInput>) -> Result<Vec<RecallScope>, RecallScopeError> { ... }
   ```
   Where `ScopeInput` is a `serde_untagged` enum accepting `String | Vec<String> | null`. Match Python's error message format byte-for-byte: `"Unknown recall scope(s): [...]. Valid values: [...]"`.

3. **Widen `recall()` signature** to accept `scope: Option<Vec<RecallScope>>`. Add the parameter at the end of the existing signature; preserve all existing call shapes via `None` default.

4. **Implement source fan-out** in the `recall()` body:
   - After scope normalization, if `[Auto]` → resolve to `[Session]` if `session_id.is_some()`, else `[Graph]`.
   - For each scope in canonical order, dispatch to a helper:
     - `Graph` → existing `SearchOrchestrator::search` path (unchanged).
     - `Session` → new `_search_session` helper using `SessionStore` lookups.
     - `Trace` → new `_search_trace` helper using `SessionManager::get_agent_trace_session`.
     - `GraphContext` → new `_fetch_graph_context` helper walking from query-matched nodes via `GraphDBTrait`.
   - Collect results into the existing `RecallResult` shape (extend if needed to carry per-source results, mirroring Python's structure).

5. **Add `_search_session`, `_search_trace`, `_fetch_graph_context` private helpers** in `recall.rs`. Each takes the relevant trait dyn refs (`SessionStore`, `SessionManager`, `GraphDB`) and returns a Vec of source-tagged result rows. Follow Python's logic line-for-line; cite line numbers in inline comments where the logic is non-obvious.

6. **Update `RecallResult`** if necessary to carry per-source results. If Python's response shape currently has per-source fields, mirror them; if it has a flat `results: List[...]` with `source` discriminator on each row, mirror that. Read `recall.py:373-475` to confirm.

7. **Migrate any internal callers** of `recall()` (if any exist outside `crates/lib/`). Grep: `grep -rn "cognee_lib::api::recall::recall\|api::recall::recall(" crates/` — fix each call site.

8. **Re-export** `RecallScope`, `normalize_scope`, `RecallScopeError` from `cognee_lib::api::recall::*`. Add to the crate-level prelude if appropriate.

## 5. Tests

5.1 **Unit tests** in `crates/lib/src/api/recall.rs` (inline `#[cfg(test)] mod tests`):
- `test_normalize_scope_none_returns_auto`
- `test_normalize_scope_string_passes_through`
- `test_normalize_scope_list_dedupes`
- `test_normalize_scope_all_expands`
- `test_normalize_scope_unknown_returns_error`
- `test_normalize_scope_error_message_matches_python`

5.2 **Integration tests** in new `crates/lib/tests/test_recall_scope.rs`:
- `test_scope_auto_with_session_id_uses_session_path` — assert session source is hit.
- `test_scope_auto_without_session_id_uses_graph_path` — assert only graph source is hit.
- `test_scope_session_returns_qa_pairs` — uses inline `InMemorySessionStore` mock.
- `test_scope_trace_returns_trace_entries` — uses `SessionManager::add_agent_trace_step` (LIB-02) to seed.
- `test_scope_graph_context_returns_subgraph` — uses `MockGraphDB`.
- `test_scope_all_merges_four_sources` — seed all backends, assert all four sources contribute.
- `test_scope_session_without_session_id_returns_empty` — graceful degradation.
- `test_scope_unknown_value_returns_error` — assert Python-parity error message.

## 6. Acceptance criteria

- [ ] `RecallScope` enum + `normalize_scope` helper exist in `cognee-lib` with Python-parity semantics.
- [ ] `recall()` signature widened to accept `scope: Option<Vec<RecallScope>>` with backwards-compatible default.
- [ ] All four source helpers (`_search_graph`, `_search_session`, `_search_trace`, `_fetch_graph_context`) implemented.
- [ ] All internal call sites of `recall()` migrated to the new signature.
- [ ] 6 unit tests + 8 integration tests pass.
- [ ] `cargo check --all-targets` clean.
- [ ] `scripts/check_all.sh` clean (Rust gates green; pre-existing JS jest issue safe to ignore per IMPLEMENTATION-PROMPT.md §0).
- [ ] No `unwrap()` in non-test code.
- [ ] `RecallResult` shape mirrors Python's per-source structure (extend if needed; cite Python line range).

## 7. References

- [Python `recall()`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py)
- [Python `normalize_scope()`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py)
- [E-04 — sibling HTTP task](e-04-recall-search.md) (consumes this widening)
- [LIB-02](lib-02-session-manager-trace-step.md) — `SessionManager::add_agent_trace_step` / `get_agent_trace_session` (prerequisite, landed eec6f79)
