# LIB-07 — `cognee_lib::api::recall::recall()` scope widening

| | |
|---|---|
| Scope | Library widening — extend `cognee_lib::api::recall::recall()` to accept a `scope` parameter and implement source fan-out across `graph` / `session` / `trace` / `graph_context`. Includes `RecallScope` enum, `normalize_scope()` helper, the missing `_search_trace` / `_fetch_graph_context` source helpers, and unit tests. **No HTTP changes** — E-04 consumes this in the next task. |
| Status | **Done (commit 7d25c0b)** — `RecallScope` enum + `normalize_scope()` (Python-byte-exact error message) + `RecallSource` extended with `Trace`/`GraphContext`; `recall()` widened with `scope: Option<Vec<RecallScope>>` and `session_manager: Option<&SessionManager>`; four-source fan-out implemented; `auto_fallthrough` short-circuit per Python `recall.py:374-386, 508-509`; `_fetch_graph_context` reads `SessionManager::get_graph_context` snapshot (NOT a graph-DB walk); 14 unit tests + 8 integration tests pass; 3 existing call sites in `recall_override.rs` migrated. No HTTP changes — E-04 consumes this. No new wire divergence. |
| Blocks | E-04 (`POST /recall` adds `session_id` + `scope`). |
| Depends on | LIB-02 (`SessionManager::add_agent_trace_step` / `get_agent_trace_session` — landed eec6f79); existing `SessionStore` for QA / trace lookups. |
| Effort | ~1 day. |
| Owner crate | `cognee-lib` (with new types in `cognee-models`) |

> **Decision (2026-04-30) — Decision 17**: split the original E-04 work into a library-widening prerequisite (this task, LIB-07) and the HTTP-layer task (E-04). Investigation 2026-04-30 found that the Rust `cognee_lib::api::recall::recall()` accepts only `session_id` + `auto_route` — it does NOT have a `scope` parameter, no `_search_trace` / `_fetch_graph_context` helpers, and no `RecallScope` enum. Honoring `scope` ∈ {trace, graph_context, all} requires this widening. The user (2026-04-30) chose the split (Option B from the investigation) over Option A (DTO + handler-only with a new D-2 wire divergence) so that v2 HTTP `POST /recall` retains strict Python parity. **No new wire divergence** is introduced. Investigation agent: do not re-litigate.

## 1. Goal

Bring `cognee_lib::api::recall::recall()` to byte-for-byte parity with Python's `cognee/api/v1/recall/recall.py` `recall()` function — specifically the four-source fan-out (`graph`, `session`, `trace`, `graph_context`) that scope-resolves and merges results across sources. Once landed, E-04 can plumb `session_id` + `scope` from the HTTP DTO straight through to the library function without any wire divergence.

## 2. Python source-of-truth

Verified line ranges against `/tmp/cognee-python` (commit at clone time, 2026-04-30; Python `recall.py` is 531 lines, `entries.py` is 115 lines).

| Symbol | File | Lines |
|---|---|---|
| `recall()` | `cognee/api/v1/recall/recall.py` | 317–531 |
| `_search_session()` helper | `cognee/api/v1/recall/recall.py` | 146–208 |
| `_search_trace()` helper | `cognee/api/v1/recall/recall.py` | 211–286 |
| `_fetch_graph_context()` helper | `cognee/api/v1/recall/recall.py` | 289–314 |
| `_run_graph` runner (inline closure inside `recall()`) | `cognee/api/v1/recall/recall.py` | 455–493 |
| `_tokenize` helper | `cognee/api/v1/recall/recall.py` | 50–52 |
| `_resolve_user_id` helper | `cognee/api/v1/recall/recall.py` | 55–61 |
| `_resolve_session_cache_user_id` helper | `cognee/api/v1/recall/recall.py` | 64–143 |
| `RecallScope` `Literal` alias | `cognee/memory/entries.py` | 75 |
| `_VALID_SCOPES` set | `cognee/memory/entries.py` | 78 |
| `normalize_scope()` | `cognee/memory/entries.py` | 81–115 |

Clone the Python repo with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python` if not already present, then read the cited line ranges to confirm exact behavior.

> Note: there is no standalone `_search_graph()` function in Python — graph-source dispatch is an inline `_run_graph` async closure (lines 455–493) that calls `cognee.api.v1.search.search(...)`. The Rust port already does the equivalent via `SearchOrchestrator::search` — keep that call path for the `Graph` source; only the dispatcher around it changes.

### Behavior (Python parity)

`recall(query_text, query_type=None, *, datasets=None, top_k=10, auto_route=True, scope=None, **kwargs)` (kwargs include `session_id`, `user`, etc.) — `recall.py:317-531`:

1. Normalize `scope` via `normalize_scope` (`recall.py:373`):
   - `None` → `["auto"]`.
   - `"all"` → `["graph", "session", "trace", "graph_context"]` (entries.py:105-106).
   - Single string → `[string]`.
   - List of strings passes through with order-preserving dedup (entries.py:108-115).
   - Unknown scope values → `ValueError("Unknown recall scope(s): [...]. Valid values: [...]")` (entries.py:99-103).
2. If normalized scope is `["auto"]` (`recall.py:374-386`):
   - `session_id` set AND no `datasets` AND no `query_type` → `sources = ["session", "graph"]` with `auto_fallthrough = True` (a non-empty session result short-circuits the graph runner).
   - `session_id` set otherwise → `sources = ["session", "graph"]` with `auto_fallthrough = False` (both sources contribute).
   - No `session_id` → `sources = ["graph"]`, no fallthrough.
   - Explicit non-`auto` scope bypasses this entirely.
3. Iterate `sources` **in caller-supplied order** (after normalize), dispatching via the runners dict (`recall.py:495-513`):
   - `session` → `_search_session()` (`recall.py:146-208`): tokenize `query_text` and each session entry's `question + context + answer`, rank by token-set intersection size, keep top_k, tag with `_source: "session"`. Uses `SessionManager::get_session(user_id, session_id, formatted=False)` — Rust equivalent today is `SessionStore::get_all_qa_entries`.
   - `trace` → `_search_trace()` (`recall.py:211-286`): same tokenization scheme over `origin_function`, `status`, `memory_query`, `memory_context`, `session_feedback`, `error_message` plus JSON-serialized `method_params` and `method_return_value`. Uses `SessionManager::get_agent_trace_session(user_id, session_id)` — Rust equivalent landed in LIB-02 (`crates/session/src/session_manager.rs:284-297`).
   - `graph_context` → `_fetch_graph_context()` (`recall.py:289-314`): **returns a one-item list `[{"_source":"graph_context","content":snapshot}]`** by reading the pre-computed snapshot via `SessionManager::get_graph_context(user_id, session_id)`. **It is NOT a graph-DB walk from query-matched nodes** — `improve()` writes the distilled summary into `graph_knowledge:{user}:{session}` and this helper just surfaces it. Rust equivalent already exists at `crates/session/src/session_manager.rs:221-228`.
   - `graph` → existing `SearchOrchestrator::search` path (the `_run_graph` closure at `recall.py:455-493`); each result dict gets `r["_source"] = "graph"` if it's a dict.
4. Auto-mode short-circuit: if `auto_fallthrough && src == "graph" && !merged.is_empty()` → break (skip graph runner) (`recall.py:508-509`).
5. Append each runner's results to the flat `merged: list` (`recall.py:513`); record `session_result_count` for telemetry (`recall.py:511-512`).
6. Set telemetry attrs: `COGNEE_RECALL_SCOPE` = comma-joined sources; `COGNEE_RECALL_SOURCE` = `sources[0]` if single-source else `"multi"` (`recall.py:520-521`); `COGNEE_RESULT_COUNT` = `len(merged)`; `COGNEE_SESSION_ENTRY_COUNT` = `session_result_count` if non-zero (`recall.py:515-522`).
7. Return the flat `merged` list. **The wire/return shape is a flat list of dicts each carrying a `_source` discriminator — there are NO per-source fields.** Rust's existing `RecallResult { items: Vec<RecallItem>, ... }` with `RecallItem { source: RecallSource, content, score }` already mirrors this; widening just means extending `RecallSource` with `Trace` and `GraphContext` variants.

### Edge cases

- `session_id` empty/missing + `scope` containing `session` / `trace` / `graph_context` → those runners return empty lists (no error) — see `recall.py:431, 441, 451`.
- Session backend not available (`get_session_manager()` returns a manager with `is_available=False`, or in Rust: `session_store: None`) → `_search_session` / `_search_trace` / `_fetch_graph_context` short-circuit empty (`recall.py:170-171, 234-235, 306-307`).
- Unknown scope values in the input list → propagate as `Err(ApiError::InvalidArgument(...))` with the Python parity message verbatim: `"Unknown recall scope(s): [\"foo\"]. Valid values: [\"all\", \"auto\", \"graph\", \"graph_context\", \"session\", \"trace\"]"` (entries.py:99-103).
- All four sources requested (`scope = "all"`) — graceful degradation: any subset whose backend is missing returns empty; the rest still contribute.
- Source order in the response is caller-supplied (after dedup); a caller that asks for `["graph", "session"]` gets graph results first, while `["session", "graph"]` gets session first. `"all"` always expands to `["graph", "session", "trace", "graph_context"]` (entries.py:106), so the `"all"` order is fixed.

## 3. Current Rust state (verified 2026-04-30)

- `cognee_lib::api::recall::recall` at [`crates/lib/src/api/recall.rs:69`](../../../crates/lib/src/api/recall.rs#L69):
  - Signature: `recall(query_text, query_type, datasets, top_k, auto_route, session_id, user_id, search_orchestrator, session_store)` — 9 positional args, **no `scope` parameter**.
  - Implements only the legacy session-first short-circuit (session OR graph) — see `crates/lib/src/api/recall.rs:80-150`.
  - Inline `session_keyword_search` helper (lines 259-316) already does the Python `_search_session` tokenize-and-rank logic against `SessionStore::get_all_qa_entries`. It is suitable to be lifted into the new fan-out path with minimal changes (rename to `_search_session`, return `RecallItem`s tagged with `RecallSource::Session`, drop the `top_k`-then-return-empty short-circuit at the call site).
  - Inline `tokenize` helper (lines 319-324) implements word-boundary lowercase tokenization with `len >= 2` filter — matches Python's `_tokenize` (`recall.py:50-52`) closely enough to reuse.
- `RecallSource` enum (`crates/lib/src/api/recall.rs:27-32`) currently has only `Session` and `Graph`. **Must extend** with `Trace` and `GraphContext` variants (snake_case serde already configured).
- `RecallResult` (`crates/lib/src/api/recall.rs:46-56`) has `items: Vec<RecallItem>`, `search_type_used: Option<SearchType>`, `auto_routed: bool`, `search_response: Option<SearchResponse>`. Shape is already correct (flat list with `source` discriminator) — **no struct widening required**, only the `RecallSource` enum extension and minor semantics: `search_type_used`/`auto_routed`/`search_response` should be `None`/`false`/`None` when the graph source is not part of the resolved scope.
- No `RecallScope` enum exists anywhere in `crates/` (verified `grep -rn "RecallScope" crates/` returns no hits).
- No `normalize_scope` function exists anywhere in `crates/` (verified `grep -rn "normalize_scope" crates/` returns no hits).
- No `_search_trace` or `_fetch_graph_context` helpers exist.
- `SearchOrchestrator::search` at `crates/search/src/orchestration/search_orchestrator.rs` honors `session_id` for history loading and trace persistence but does NOT do source fan-out — keep using it for the `Graph` source dispatch.
- **`SessionManager::get_agent_trace_session`** exists at [`crates/session/src/session_manager.rs:284-297`](../../../crates/session/src/session_manager.rs#L284) (landed at LIB-02 eec6f79). Signature: `(&self, user_id: &str, session_id: Option<&str>, last_n: Option<usize>) -> Result<Vec<SessionTraceStep>, SessionError>`. Use this for `_search_trace`. **Note:** the helper takes a `&SessionManager` rather than a `&dyn SessionStore` — see step 5 below for the dependency-injection question this raises.
- **`SessionManager::get_graph_context`** exists at [`crates/session/src/session_manager.rs:221-228`](../../../crates/session/src/session_manager.rs#L221). Signature: `(&self, session_id: Option<&str>, user_id: Option<&str>) -> Result<Option<String>, SessionError>`. Returns a stored snapshot or `None` — exactly what Python's `_fetch_graph_context` reads. Use this directly; **no `GraphDBTrait` walk is required**.
- `SessionStore::get_all_qa_entries` ([`crates/session/src/session_store.rs:51-55`](../../../crates/session/src/session_store.rs#L51)) is the existing method used by today's `session_keyword_search` — keep using it for `_search_session`.
- **Existing call sites of `recall()`**: only `crates/lib/tests/recall_override.rs` (3 invocations at lines 64, 108, 144) — each passes 9 positional args. These need to be updated when the signature widens. **No production-code call sites** outside `crates/lib/`. (The cloud client at `crates/cloud/src/cloud_client.rs:203` defines its own `recall()` method, distinct from this one.)

## 4. Implementation steps

1. **Add `RecallScope` enum** in `crates/lib/src/api/recall.rs`:
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
   Plus `RecallScope::ALL: &[Self] = &[Self::Graph, Self::Session, Self::Trace, Self::GraphContext]` (matching the order Python's `normalize_scope` produces for `"all"` at `entries.py:106`). `Auto` is kept as a sentinel — it never appears in a fully resolved source list, only in the intermediate `Vec<RecallScope>` returned by `normalize_scope` for `None` input. Resolution into concrete sources happens inside `recall()` (step 4 below).

2. **Add `normalize_scope()` helper** matching Python's `cognee/memory/entries.py:81-115`:
   ```rust
   /// Accepts None / single-string / list-of-string. Returns canonical
   /// dedup'd list of `RecallScope`. `"all"` expands to ALL four sources
   /// in the order [Graph, Session, Trace, GraphContext] (entries.py:106).
   /// `None` → `[Auto]`. Unknown strings → `Err`.
   pub fn normalize_scope(input: Option<ScopeInput>) -> Result<Vec<RecallScope>, ApiError> { ... }
   ```
   Where `ScopeInput` is a small enum accepting `String | Vec<String>` (the `Option<...>` carries the `null` case). Use `ApiError::InvalidArgument` (already exists in `crates/lib/src/api/error.rs`) so callers do not have to learn a new error type. Match Python's error message format byte-for-byte: `"Unknown recall scope(s): {:?}. Valid values: {:?}"` where the second `{:?}` produces the sorted list `["all", "auto", "graph", "graph_context", "session", "trace"]`. (E-04 surfaces this same string at the HTTP layer per its tests.)

3. **Extend `RecallSource` enum** at `crates/lib/src/api/recall.rs:27-32` to add `Trace` and `GraphContext` variants. Existing snake_case serde rename produces `"trace"` and `"graph_context"` automatically. **No `RecallResult` struct widening is required** — Python returns a flat list and Rust's `RecallResult.items` already mirrors that.

4. **Widen `recall()` signature** to accept `scope: Option<Vec<RecallScope>>`. Add the parameter at the end of the existing 9-arg positional signature (preserve `None` as the default-equivalent behavior). The single existing call site (`crates/lib/tests/recall_override.rs`, 3 invocations) gains a trailing `None` argument. Also accept a `&SessionManager` in addition to the existing `&dyn SessionStore` (or thread one through internally), since `_search_trace` and `_fetch_graph_context` need `SessionManager::get_agent_trace_session` and `SessionManager::get_graph_context` respectively. Suggested signature:
   ```rust
   pub async fn recall(
       query_text: &str,
       query_type: Option<SearchType>,
       datasets: Option<Vec<String>>,
       top_k: usize,
       auto_route: bool,
       scope: Option<Vec<RecallScope>>,           // NEW
       session_id: Option<&str>,
       user_id: Option<&str>,
       search_orchestrator: &SearchOrchestrator,
       session_manager: Option<&SessionManager>,  // NEW (replaces or supplements session_store)
       session_store: Option<&dyn SessionStore>,
   ) -> Result<RecallResult, ApiError>
   ```
   (Decision for the implementation agent: collapse to a `RecallParams<'_>` struct following the LIB-04 pattern is **acceptable** — there are only 3 in-tree call sites — but is **not required** by this task.)

5. **Implement scope resolution + source fan-out** in the `recall()` body (mirror `recall.py:373-386, 495-513`):
   1. `let normalized = normalize_scope(scope_input)?;` — yields `Vec<RecallScope>` possibly containing only `Auto`.
   2. If `normalized == [Auto]`:
      - `session_id.is_some() && datasets.is_none() && query_type.is_none()` → `sources = [Session, Graph]`, `auto_fallthrough = true`.
      - `session_id.is_some()` (other cases) → `sources = [Session, Graph]`, `auto_fallthrough = false`.
      - else → `sources = [Graph]`, `auto_fallthrough = false`.
   3. Else `sources = normalized`, `auto_fallthrough = false`.
   4. Iterate `sources` in caller-supplied order (NOT a fixed canonical order — Python honors input order). For each:
      - `Graph` → existing `SearchOrchestrator::search` path (extract today's body into a `_run_graph` helper). Tag results with `RecallSource::Graph`.
      - `Session` → new `_search_session` helper using `SessionStore::get_all_qa_entries` (today's `session_keyword_search` lifted out — match Python's `_search_session` line-for-line).
      - `Trace` → new `_search_trace` helper using `SessionManager::get_agent_trace_session(user_id, session_id, last_n=None)`. Tokenize `origin_function`, `status`, `memory_query`, `memory_context`, `session_feedback`, `error_message` plus JSON-serialized `method_params` and `method_return_value`; rank by token-set intersection; keep top_k; tag with `RecallSource::Trace`.
      - `GraphContext` → new `_fetch_graph_context` helper that calls `SessionManager::get_graph_context(session_id, user_id)` and returns a one-element list `[RecallItem { source: GraphContext, content: snapshot, score: 1.0 }]` if `Some(snapshot)` else empty. **No graph-DB walk; no query matching** — it's a literal pass-through of the snapshot written by `improve()`.
      - Auto-mode short-circuit (`recall.py:508-509`): if `auto_fallthrough && src == Graph && !merged.is_empty()` → break.
      - Append each runner's results to a flat `merged: Vec<RecallItem>`.
   5. Set telemetry: `COGNEE_RECALL_SCOPE` = comma-joined source names; `COGNEE_RECALL_SOURCE` = single source name if one source contributed, else `"multi"`; `COGNEE_RESULT_COUNT` = `merged.len()`; `COGNEE_SESSION_ENTRY_COUNT` = the count from the `Session` runner if non-zero.
   6. Return `RecallResult { items: merged, search_type_used: <Some(t) if Graph ran else None>, auto_routed, search_response: <Some if Graph ran else None> }`.

6. **Add `_search_session`, `_search_trace`, `_fetch_graph_context` private helpers** in `recall.rs`. Each takes the relevant trait/struct refs and returns a `Result<Vec<RecallItem>, ApiError>` of source-tagged result rows. Follow Python's logic line-for-line and cite Python line numbers in inline comments. The existing `tokenize()` helper at `crates/lib/src/api/recall.rs:319-324` is reusable for both `_search_session` and `_search_trace`.

7. **Migrate the single existing caller** at `crates/lib/tests/recall_override.rs` (lines 64, 108, 144). Each call adds the new `scope` parameter (use `None`) and any added/reordered DI parameters. **No production-code call sites** require migration.

8. **Re-export** `RecallScope`, `normalize_scope` from `crates/lib/src/api/mod.rs` (extend the existing `pub use recall::{...}` line). Existing exports of `RecallItem`, `RecallResult`, `RecallSource`, `recall` stay.

## 5. Tests

5.1 **Unit tests** in `crates/lib/src/api/recall.rs` (inline `#[cfg(test)] mod tests`, alongside the existing `tokenize_*` and `recall_source_serializes_correctly` tests):
- `test_normalize_scope_none_returns_auto`
- `test_normalize_scope_single_string_returns_singleton` (covers `"graph"`, `"session"`, `"trace"`, `"graph_context"`)
- `test_normalize_scope_list_dedupes_preserving_order`
- `test_normalize_scope_all_expands_to_four_sources` — order = `[Graph, Session, Trace, GraphContext]`.
- `test_normalize_scope_unknown_returns_error`
- `test_normalize_scope_error_message_matches_python` — exact-string assert: `Unknown recall scope(s): ["foo"]. Valid values: ["all", "auto", "graph", "graph_context", "session", "trace"]`.
- `test_recall_source_trace_serializes_correctly` and `test_recall_source_graph_context_serializes_correctly` — assert `"trace"` and `"graph_context"` snake_case.

5.2 **Integration tests** in new `crates/lib/tests/test_recall_scope.rs`:
- `test_scope_auto_with_session_id_runs_session_then_graph` — seed both backends; assert order is `[Session*, Graph*]` and `auto_fallthrough` short-circuits when session has hits.
- `test_scope_auto_without_session_id_runs_graph_only` — assert only `RecallSource::Graph` items present.
- `test_scope_session_returns_tagged_qa_entries` — uses `FsSessionStore` (or `MemorySessionStore` if available); seed via `SessionStore::create_qa_entry`; assert items carry `RecallSource::Session`.
- `test_scope_trace_returns_tagged_trace_entries` — uses `SessionManager::add_agent_trace_step` (LIB-02) to seed; assert items carry `RecallSource::Trace`.
- `test_scope_graph_context_returns_one_item_with_snapshot` — seed via `SessionManager::set_graph_context`; assert single item with `RecallSource::GraphContext` and `content == snapshot`. **Do NOT use `MockGraphDB`** — `_fetch_graph_context` does not touch the graph DB.
- `test_scope_all_merges_all_four_sources_in_canonical_order` — seed all backends; assert sources appear in `[Graph, Session, Trace, GraphContext]` order.
- `test_scope_session_without_session_id_returns_empty_for_session_runner` — graceful degradation when `session_id = None`.
- `test_scope_unknown_value_returns_invalid_argument_error` — Python-parity error message via `ApiError::InvalidArgument`.

## 6. Acceptance criteria

- [x] `RecallScope` enum + `normalize_scope` helper exist in `cognee-lib` with Python-parity semantics (per `entries.py:75-115`); error message byte-exact via `test_normalize_scope_error_message_matches_python`.
- [x] `RecallSource` enum extended with `Trace` and `GraphContext` variants; `RecallResult` struct shape unchanged.
- [x] `recall()` signature widened to accept `scope: Option<Vec<RecallScope>>` and `session_manager: Option<&SessionManager>` (passing `None` for both reproduces today's behaviour).
- [x] Three new helpers (`search_session`, `search_trace`, `fetch_graph_context`) implemented and the existing graph path extracted to a `run_graph` helper. `fetch_graph_context` reads `SessionManager::get_graph_context` — does NOT walk the graph DB.
- [x] All internal call sites of `recall()` migrated to the new signature (only `crates/lib/tests/recall_override.rs`, 3 calls — lines 64, 108, 144 each gain `None, None`).
- [x] 14 unit tests + 8 integration tests pass (commit landed 22 tests; spec asked for 7+8=15; over-delivered).
- [x] `cargo check --all-targets` clean.
- [x] `scripts/check_all.sh` clean (Rust gates green; pre-existing JS jest issue safe to ignore per IMPLEMENTATION-PROMPT.md §0).
- [x] No `unwrap()` in non-test code.
- [x] Python parity: ordering, `auto`/`auto_fallthrough` short-circuit, telemetry attrs (`COGNEE_RECALL_SCOPE`, `COGNEE_RECALL_SOURCE`, `COGNEE_RESULT_COUNT`, `COGNEE_SESSION_ENTRY_COUNT`) all match `recall.py:373-531`.

## 7. References

- [Python `recall()`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py)
- [Python `normalize_scope()`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py)
- [E-04 — sibling HTTP task](e-04-recall-search.md) (consumes this widening)
- [LIB-02](lib-02-session-manager-trace-step.md) — `SessionManager::add_agent_trace_step` / `get_agent_trace_session` (prerequisite, landed eec6f79)
