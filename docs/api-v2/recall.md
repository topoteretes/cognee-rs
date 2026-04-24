# API v2: `recall()`

**Python source:** `cognee/api/v1/recall/recall.py` (+ `query_router.py`)
**Rust status:** **Implemented**
**Implementation plan:** [impl/recall-plan.md](impl/recall-plan.md)

---

## 1. What it does

`recall()` is a smart search wrapper that combines **session-first retrieval** with **automatic query-type selection**. It aims to improve UX by:

1. **Session-first routing** — When `session_id` is provided without explicit `datasets` or `query_type`, it searches the session's cached Q&A entries by word-boundary keyword matching before hitting the graph. Results are tagged with `_source: "session"`.

2. **Fall-through to graph search** — If session search yields no matches (or is not applicable), it falls through to the permanent graph via the standard `search()` pipeline.

3. **Auto query-type selection (auto-routing)** — When `query_type` is omitted and `auto_route=True`, calls `route_query()` to classify the query into the optimal `SearchType` using rule-based heuristics (regex pattern matching, weighted scoring, negation detection) **without an LLM call**.

4. **Result tagging** — All returned items include `_source: "session"` or `_source: "graph"` so callers know where results came from.

5. **Override tracking** — When an explicit `query_type` is provided **and** `auto_route=True`, the router still runs to compare its choice against the user's override; mismatches are logged for telemetry and pattern analysis.

6. **Scope detection** — Distinguishes three scopes:
   - `"session"` — session-only search (when `session_id` provided, no `datasets`, no `query_type`)
   - `"auto"` — both session and graph available (when `session_id` and `datasets` both provided)
   - `"graph"` — graph-only search (default)

### Python signature
```python
async def recall(
    query_text: str,
    query_type: Optional[SearchType] = None,
    *,
    datasets: Optional[list[str]] = None,
    top_k: int = 10,
    auto_route: bool = True,
    **kwargs: Unpack[RecallKwargs],
) -> list
```

### Inputs
- **query_text** — The user's natural-language query
- **query_type** — Explicit search strategy (optional)
- **datasets** — Dataset names to search within (optional)
- **top_k** — Maximum results (default 10)
- **auto_route** — If True, auto-classify the query when `query_type` is None (default True)
- **session_id** — Session cache ID (via `**kwargs`)
- **user** — User object or "sdk" for telemetry (via `**kwargs`)
- Other config overrides: `system_prompt`, `node_name`, `wide_search_top_k`, `triplet_distance_penalty`, etc.

### Outputs
- List of result dicts (SearchResult objects or dicts from session)
- Each dict includes `_source: "session"` or `_source: "graph"` field
- When session-only: list of matching Q&A entry dicts with fields `question`, `context`, `answer`, `_source`
- When graph: native SearchResult objects from the search pipeline

### Side effects
- Telemetry sent via `send_telemetry("cognee.recall", ...)` with scope, auto_route, top_k, search_type, session_id, datasets, cognee_version
- Tracing spans created via `new_span("cognee.api.recall")` with attributes: query (first 500 chars), scope, session_id, top_k, search_type, result count
- Optional: override counts logged when user override differs from auto-routed type

---

## 2. Query router internals

The query router is a **rule-based weighted-scoring classifier** that needs no LLM call. It's the core innovation enabling deterministic, instant query classification.

### Routing algorithm

1. **Pattern matching phase** — For each rule `(pattern, search_type, weight)`:
   - Test if the regex `pattern` matches the query
   - Check if the match is "negated" (a negation word within 20 chars before the match)
   - If match and not negated, add `weight` to the score for that `search_type`

2. **Aggregation phase** — Sum scores per search type (multiple rules can contribute to the same type)

3. **Ranking phase** — Sort search types by cumulative score (descending)

4. **Fallback** — If best score < threshold (2.0), return default `SearchType.GRAPH_COMPLETION`

5. **Confidence calc** — `confidence = best_score`; `is_confident = confidence >= 2.0 * max(runner_up_score, 1.0)` (best at least 2x runner-up)

### Categories and rules

| Category | Pattern | Weight | Notes |
|----------|---------|--------|-------|
| **Cypher** | `^MATCH\|^RETURN\|^CREATE\|^MERGE\|--(\|)--)` | 10.0 | Raw Cypher syntax (highest priority) |
| **Coding rules** | `\b(coding rules?\|code review\|best practice\|lint(ing)?\|refactor(ing)?)\b` | 5.0 | Programming-context keywords |
| **Coding rules** | `\b(def \|return \|async \|await \|import \|class \w+\(\|\.py\b\|function\s+\w+\()\b` | 3.0 | Code syntax patterns |
| **Lexical** | `^"[^"]+"$` (full query is quoted phrase) | 8.0 | Exact phrase search |
| **Lexical** | `\b(exact\|verbatim\|literal\|word.for.word)\b` | 4.0 | Exact match intent keywords |
| **Summary** | `\b(summarize\|summary\|overview\|outline\|tl;?dr\|gist\|main points?\|key takeaways?\|high.?level)\b` | 5.0 | Summarization intent |
| **Reasoning (CoT)** | `\b(why\|explain\|reasoning\|step.by.step\|chain of thought)\b` | 4.0 | Causal/explanatory queries |
| **Reasoning (CoT)** | `\b(because\|therefore\|consequently)\b` | 2.0 | Causal connectives |
| **Relationship (Context Extension)** | `\b(how (is\|are\|does\|do)\s+\w+\s+(related\|connected\|linked))\b` | 5.0 | Graph traversal intent |
| **Relationship** | `\b(what (connects\|links\|ties)\|path between\|degree of separation)\b` | 5.0 | Connection/path queries |
| **Relationship** | `\b(connection\|relationship\|related to\|linked to)\b` | 3.0 | Relationship keywords |
| **Temporal** | `\b(when\|before\|after\|during\|since\|until)\b` | 3.0 | Time-related keywords |
| **Temporal** | `\b(timeline\|chronolog\|era\|decade\|century)\b` | 4.0 | Timeline/era keywords |
| **Temporal** | `\b\d{4}s?\b` | 3.0 | Year patterns (1900s, 2023, etc.) |
| **Temporal** | `\bbetween\s+\d{4}\s+and\s+\d{4}\b` | 6.0 | Explicit year ranges |

### Negation suppression

```python
_NEGATION = re.compile(r"\b(not|n't|no|never|without|lack)\b", re.IGNORECASE)
_NEGATION_WINDOW = 20  # characters

def _is_negated(query: str, match: re.Match) -> bool:
    """Suppress match if negation word within 20 chars before the match."""
    start = max(0, match.start() - _NEGATION_WINDOW)
    prefix = query[start : match.start()]
    return bool(_NEGATION.search(prefix))
```

Example: `"I don't want a summary"` — "summary" keyword matches but is negated, so no `GRAPH_SUMMARY_COMPLETION` boost.

### Confidence & runner-up tracking

```python
@property
def is_confident(self) -> bool:
    """True if winning score is well above the runner-up."""
    return self.confidence >= 2.0 * max(self.runner_up_score, 1.0)
```

### Default fallback

- When no patterns match: `SearchType.GRAPH_COMPLETION` with `confidence = 2.0`
- When best score < 2.0: still return `GRAPH_COMPLETION` but with `runner_up` = the best matched type (for diagnostics)

### RouteResult dataclass

```python
@dataclass
class RouteResult:
    search_type: SearchType
    confidence: float
    runner_up: SearchType = SearchType.GRAPH_COMPLETION
    runner_up_score: float = 0.0
    all_scores: dict = field(default_factory=dict)  # {SearchType.value -> score}
```

---

## 3. Building blocks (Python)

| Component | Python path | Type | Purpose |
|-----------|------------|------|---------|
| **`recall()` main** | `cognee/api/v1/recall/recall.py` (lines 122–261) | async function | Entry point; orchestrates session search, auto-routing, graph search |
| **`_search_session()`** | `cognee/api/v1/recall/recall.py` (lines 54–119) | async function | Keyword-based Q&A entry search in session |
| **`_tokenize()`** | `cognee/api/v1/recall/recall.py` (lines 49–51) | function | Word-boundary tokenization (min length 2) |
| **`route_query()`** | `cognee/api/v1/recall/query_router.py` (lines 171–232) | function | Rule-based query classification |
| **`record_override()`** | `cognee/api/v1/recall/query_router.py` (lines 157–168) | function | Log when user overrides auto-routed type |
| **`_is_negated()`** | `cognee/api/v1/recall/query_router.py` (lines 39–43) | function | Negation suppression check |
| **`RouteResult`** | `cognee/api/v1/recall/query_router.py` (lines 17–30) | dataclass | Routing decision with confidence metadata |
| **`RecallKwargs`** | `cognee/api/v1/recall/recall.py` (lines 31–46) | TypedDict | Power-user kwargs (datasets, session_id, system_prompt, etc.) |
| **`SearchType` enum** | `cognee/modules/search/types/SearchType.py` | enum | 15 search strategies (GraphCompletion, Cypher, Temporal, etc.) |
| **`search()`** | `cognee/api/v1/search/search.py` (lines 27–48) | async function | Core search pipeline |
| **`SessionManager`** | `cognee/infrastructure/session/session_manager.py` | class | Session management facade |
| **`SessionManager.get_session()`** | `cognee/infrastructure/session/session_manager.py` | async method | Retrieve session Q&A entries |
| **`get_session_manager()`** | `cognee/infrastructure/session/get_session_manager.py` | function | Singleton accessor |
| **`get_remote_client()`** | `cognee/api/v1/serve/state.py` | function | Cloud client if available (for delegation) |
| **`get_default_user()`** | `cognee/modules/users/methods` | async function | Current user fallback |
| **`send_telemetry()`** | `cognee/shared/utils.py` | function | Analytics event emit |
| **`new_span()`** | `cognee/modules/observability` | context manager | Tracing span creation |
| **`override_counts` dict** | `cognee/api/v1/recall/query_router.py` (line 154) | dict | In-memory misrouting tracker |

---

## 4. Rust status per building block

| Building Block | Python Path | Rust Path | Status | Notes |
|---|---|---|---|---|
| **`recall()` main** | `cognee/api/v1/recall/recall.py:122–261` | `crates/lib/src/api/recall.rs:63–189` | ✓ **Implemented** | Public async fn; core logic complete |
| **`_search_session()`** | `cognee/api/v1/recall/recall.py:54–119` | `crates/lib/src/api/recall.rs:195–252` (as `session_keyword_search()`) | ✓ **Implemented** | Keyword overlap scoring with min token length 2 |
| **`_tokenize()`** | `cognee/api/v1/recall/recall.py:49–51` | `crates/lib/src/api/recall.rs:255–260` (as `tokenize()`) | ✓ **Implemented** | Alphanumeric split, >= 2 char filter |
| **`route_query()`** | `cognee/api/v1/recall/query_router.py:171–232` | `crates/search/src/query_router.rs:29–174` | ✓ **Implemented** | Rule-based classifier; routes to SearchType |
| **`record_override()`** | `cognee/api/v1/recall/query_router.py:157–168` | `crates/search/src/query_router_stats.rs` | ~~Missing~~ — done in commit 598d553 | Process-global `OnceLock<Mutex<HashMap<(SearchType, SearchType), u64>>>` counter with `record_override`, `override_counts_snapshot`, `clear_override_counts` helpers |
| **`_is_negated()`** | `cognee/api/v1/recall/query_router.py:39–43` | `crates/search/src/query_router.rs:176–198` (as `has_negation_nearby()`) | ✓ **Implemented** | Negation suppression via word search |
| **`RouteResult`** | `cognee/api/v1/recall/query_router.py:17–30` | `crates/search/src/query_router.rs:9–19` | ✓ **Implemented** | Struct with `search_type`, `confidence`, `runner_up`, `runner_up_score` |
| **`RecallKwargs`** | `cognee/api/v1/recall/recall.py:31–46` | **Missing** | ✗ **Not Implemented** | Rust uses `SearchRequest` directly; no dedicated kwargs type |
| **`SearchType` enum** | `cognee/modules/search/types/SearchType.py` | `crates/search/src/types/search_type.rs` | ✓ **Implemented** | All 15 variants present (Cypher, Temporal, CodingRules, ChunksLexical, etc.) |
| **`search()`** | `cognee/api/v1/search/search.py:27–48` | `crates/search/src/orchestration/search_orchestrator.rs:114–200+` (as `SearchOrchestrator::search()`) | ✓ **Implemented** | Core search pipeline; takes `SearchRequest` |
| **`SessionManager`** | `cognee/infrastructure/session/session_manager.py` | `crates/session/src/session_manager.rs` | ✓ **Implemented** | Orchestrates session store operations |
| **`SessionManager.get_session()`** | `cognee/infrastructure/session/session_manager.py` | `crates/session/src/session_manager.rs:52–93` (as `load_history_both()`, `load_history_messages()`) | ✓ **Implemented** | Returns `Vec<SessionQAEntry>` |
| **`get_session_manager()`** | `cognee/infrastructure/session/get_session_manager.py` | `crates/lib/src/lib.rs` (re-exported) | ✓ **Implemented** | Available via public API |
| **`get_remote_client()`** | `cognee/api/v1/serve/state.py` | **Missing** | ✗ **Not Implemented** | Cloud delegation not applicable to Rust (local-only SDK) |
| **`get_default_user()`** | `cognee/modules/users/methods` | **Missing** | ✗ **Not Implemented** | Not needed; Rust API is stateless |
| **`send_telemetry()`** | `cognee/shared/utils.py` | **Not found** | ✗ **Not Implemented** | No telemetry layer in Rust; could use tracing instead |
| **`new_span()`** | `cognee/modules/observability` | `crates/search/src/observability.rs` | ~~Partial~~ — done in commit 598d553 | `cognee.api.recall` span with Python-parity attributes (query, scope, session_id, top_k, search_type, result count, recall source, session entry count) |
| **`override_counts` dict** | `cognee/api/v1/recall/query_router.py:154` | `crates/search/src/query_router_stats.rs` | ~~Missing~~ — done in commit 598d553 | Backed by `OnceLock<Mutex<HashMap<(SearchType, SearchType), u64>>>` |
| **`SessionQAEntry` type** | `cognee/infrastructure/session/models` | `crates/session/src/types.rs` | ✓ **Implemented** | Struct with id, session_id, question, answer, context, feedback fields, created_at |
| **`SessionStore` trait** | `cognee/infrastructure/session/session_store.rs` (Python interface) | `crates/session/src/session_store.rs` | ✓ **Implemented** | `async fn get_all_qa_entries()`, `create_qa_entry()`, `update_qa_entry()`, `prune()` |
| **`RecallItem` struct** | (Python doesn't have explicit struct) | `crates/lib/src/api/recall.rs:29–37` | ✓ **Rust-only** | `{ source, content, score }` for tagged results |
| **`RecallResult` struct** | (Python returns raw list) | `crates/lib/src/api/recall.rs:40–50` | ✓ **Rust-only** | Enhanced return type with metadata: `{ items, search_type_used, auto_routed, search_response }` |
| **`RecallSource` enum** | (Python uses string `_source`) | `crates/lib/src/api/recall.rs:21–26` | ✓ **Rust-only** | Serializable enum: `Session \| Graph` |

---

## 5. Gaps — what Rust needs

Based on the analysis, the Rust `recall()` implementation is **functionally complete** for core use cases, but is missing:

### A. Override tracking (Python parity) — ~~Missing~~ done in commit 598d553

**What's missing:** ~~The `record_override()` function and the in-memory `override_counts` dict that log misrouting patterns when a user provides an explicit `query_type` that differs from the auto-routed type.~~

**Impact:** Telemetry and debugging. Missing this means you cannot detect if the auto-router is systematically misrouting certain query patterns.

**Effort:** Low — add a new module `crates/search/src/query_router_stats.rs` with:
- ~~Global `lazy_static` or `parking_lot::Mutex<HashMap<(SearchType, SearchType), u64>>` for override counts~~ — done in commit 598d553 (uses `OnceLock<Mutex<HashMap<...>>>`)
- ~~Public function `record_override(routed: SearchType, override: SearchType)` that bumps the count and logs~~ — done in commit 598d553
- ~~Public function `get_override_stats() -> HashMap<...>` to expose for diagnostics~~ — done in commit 598d553 (exposed as `override_counts_snapshot()`)

**File:** ~~Would be `crates/search/src/query_router_stats.rs` (new), re-exported via `crates/search/src/lib.rs`.~~ Created in commit 598d553.

### B. Telemetry (Python parity)

**What's missing:** The `send_telemetry()` call that emits analytics events with scope, auto_route, search_type, session_id, datasets, cognee_version.

**Impact:** Usage analytics and monitoring. The Rust SDK can't track adoption or behavior patterns across users.

**Effort:** Medium — integrate with a telemetry backend (e.g., Sentry, Datadog, or custom HTTP endpoint). Alternatively, skip for now and rely on tracing logs (which are already in place via `#[tracing::instrument]`).

**Note:** The Python implementation uses `cognee/shared/utils.py::send_telemetry()`, which is an optional feature. Rust does NOT need this for correctness.

### C. RecallKwargs TypedDict (convenience, not strictly necessary)

**What's missing:** Rust doesn't have a dedicated `RecallKwargs` type. Instead, callers build a `SearchRequest` directly.

**Impact:** Discoverability. Python users get IDE autocomplete for valid kwargs; Rust users must read the `SearchRequest` struct definition.

**Effort:** Low — add a doc comment or a builder pattern wrapper around `SearchRequest`.

**Note:** This is a **nice-to-have**, not a blocker. The current Rust API is more explicit (fewer "magic" kwargs).

### D. Cloud delegation (Python feature, Rust N/A)

**What's missing:** The `get_remote_client()` check that delegates to a cloud service if available.

**Impact:** None — Rust SDK is designed for local/edge deployments. Cloud delegation is not applicable.

**Verdict:** Skip.

### E. Tracing/observability span attributes (enhancement) — ~~Missing~~ done in commit 598d553

**Current:** ~~Rust has `#[tracing::instrument]` on `recall()` in the library API layer, but the top-level public API (in `crates/lib/src/api/recall.rs`) does not emit detailed span attributes (query, scope, search_type, result count).~~ — done in commit 598d553: `cognee.api.recall` span opened with Python-parity attributes (query, scope, session_id, top_k, search_type, recall source, result count, session entry count).

**Enhancement:** ~~Add manual `tracing::info!()` calls or use `tracing::Span` API to set attributes, mirroring Python's:~~
```rust
span.set_attribute(COGNEE_RECALL_SCOPE, scope);
span.set_attribute(COGNEE_SEARCH_TYPE, search_type);
span.set_attribute(COGNEE_RESULT_COUNT, items.len());
```
~~**Effort:** Low — add `use tracing::{info, debug};` and call `info!()` at key decision points.~~ — done in commit 598d553.

**Priority:** ~~Low (optional enhancement).~~ Landed.

---

## 6. Effort estimate

| Task | Effort | Rationale |
|------|--------|-----------|
| **Core functionality (already complete)** | **Done** | Rust `recall()` is feature-complete for session search + auto-routing + graph fallback |
| **Override tracking** | **S** (Small, ~1–2 hours) | Add a stats module with a global `Mutex<HashMap>`; wire into `route_query()` call site |
| **Telemetry integration** | **M** (Medium, ~4–8 hours if integrating new backend; 1 hour if skipping) | Depends on telemetry strategy; optional for MVP |
| **Tracing span attributes** | **S** (Small, ~1 hour) | Add `tracing::info!()` calls at decision points |
| **RecallKwargs convenience wrapper** | **S** (Small, ~30 min) | Document `SearchRequest` fields better or add builder pattern |
| **Total to parity** | **M** (Medium, ~8–12 hours) | Mainly override tracking + optional telemetry |

### Why "Partial" status, not "Implemented"?

- **Core logic:** ✓ Complete (session search, auto-routing, graph fallback, result tagging all working)
- **Testing:** ✓ Unit tests exist for `tokenize()`, `route_query()`, `RecallSource` serialization
- **Integration:** ✓ Wired into library API layer (`crates/lib/src/api/recall.rs`)
- **Missing:** Override tracking (low impact, low effort), telemetry (optional), span attributes (nice-to-have)

**Recommendation:** Mark as **"Implemented"** for shipping; track the override tracking + telemetry as post-MVP improvements.

---

## Implementation notes

### Session search behavior

Rust implementation (`session_keyword_search()` in `crates/lib/src/api/recall.rs:195–252`):
- Tokenizes via alphanumeric split (same as Python's word-boundary regex)
- Filters tokens with length >= 2 (matches Python min length)
- Scores entries by intersection size (overlap count)
- Returns top_k sorted descending by overlap
- Tags results with `{ source: Session, content: JSON, score: overlap_count }`

**Difference from Python:** Python uses `set intersection` on word tokens; Rust uses `HashSet::intersection()`. Both are equivalent.

### Query router behavior

Rust implementation (`crates/search/src/query_router.rs`):
- ~~Uses substring matching (`contains()`) instead of regex (simpler, faster)~~ — done in commit 598d553: now ports Python's `_RULES` verbatim (14 rules) with word-boundary helper and two compiled regexes (anchored lexical quote + explicit year range)
- Negation detection via `has_negation_nearby()` (20-char window walk, matches Python `_NEGATION_WINDOW = 20`)
- Weights and categories match Python exactly (factual 3.0+3.0, cypher 10.0, coding 5.0+3.0, lexical 8.0+4.0, summary 5.0, reasoning 4.0+2.0, relationship 5.0+5.0+3.0, temporal 3.0+4.0+3.0+6.0)
- Aggregates scores per SearchType correctly
- Defaults to `GraphCompletion` on no-match or low-score fallback

**~~Difference from Python~~:** ~~Rust uses `contains()` for pattern matching vs. Python's compiled regexes. This makes Rust's implementation slightly less precise (e.g., will not distinguish word boundaries), but good enough for the use case and faster. Example: Python's `\b(why|explain|...\b` matches word boundaries; Rust's `lower.contains("why")` will match "somehow_why_not" as well. This is acceptable for auto-routing (false positives are mild).~~ — done in commit 598d553: word-boundary helper now checks non-alphanumeric char boundaries on either side of each keyword match, so Rust no longer produces the "somehow_why_not" false positives.

### SearchRequest mapping

Rust `recall()` builds a `SearchRequest` from the parameters and passes it to `SearchOrchestrator::search()`. The `SearchRequest` struct (in `crates/search/src/types/`) is the canonical representation for all search operations.

---

## Testing

Current test coverage in Rust:

| File | Tests | Coverage |
|------|-------|----------|
| `crates/lib/src/api/recall.rs` | 3 unit tests | `tokenize()`, `recall_source_serialization()` |
| `crates/search/src/query_router.rs` | ~~8~~ 34 tests (done in commit 598d553) | Full port of `test_query_router.py` — factual, cypher, coding, lexical, summary, reasoning, relationship, temporal, negation, confidence, ambiguous queries |
| `crates/search/src/query_router_stats.rs` | 2 tests (done in commit 598d553) | `TestOverrideTracking` port — increment behavior + same-type no-op |
| `crates/lib/tests/recall_override.rs` | 3 tests (done in commit 598d553) | Override dispatch integration through `recall()` |

### Integration test candidates

- Session + graph fallthrough (when session empty, ensure graph search runs)
- ~~Override tracking (capture misrouting stats)~~ — done in commit 598d553 (`crates/lib/tests/recall_override.rs`)
- Auto-routing confidence scoring (verify `confidence >= 2.0 * runner_up_score` holds)
- Result tagging (`_source` field present on all items)

---

## Summary for shipping

**Status:** Feature-complete (core logic implemented)

**Ready to ship?** Yes, with caveats:
1. Override tracking is nice-to-have, not required for MVP
2. Telemetry is optional (Rust uses structured logging via `tracing` instead)
3. Span attributes can be added as a follow-up enhancement

**What's working:**
- ✓ Session-first search by keyword overlap
- ✓ Auto query-type routing via `route_query()`
- ✓ Fall-through to graph when session empty
- ✓ Result tagging with `_source` field
- ✓ Negation suppression
- ✓ All SearchType categories covered

**What's missing (post-MVP):**
- Misrouting stats tracking (override_counts dict)
- Telemetry backend integration
- Detailed span attribute logging

**Recommendation:** Ship as-is; file follow-up tickets for override tracking and telemetry.

---

## Implementation notes

**Commit:** `598d5538b6b19a3df2095e9ef032c52c597fb861`

Ported Python's `_RULES` rule-set verbatim into `crates/search/src/query_router.rs` (14 rules: factual, cypher regex at 10.0, coding, anchored lexical quote + lexical intent, summary, reasoning connectives, three split relationship rules at 5.0+5.0+3.0, temporal with year-range regex at 6.0, plus tier weights). Added `crates/search/src/query_router_stats.rs` with `OnceLock<Mutex<HashMap<(SearchType, SearchType), u64>>>` backing `record_override` / `override_counts_snapshot` / `clear_override_counts`. `crates/lib/src/api/recall.rs` now runs the router even when `query_type` is supplied (when `auto_route=true`) so it can record overrides, matching Python `recall.py:225–231`. Emits `cognee.api.recall` span with 4 new constants added to `observability.rs` (`COGNEE_SEARCH_QUERY`, `COGNEE_RECALL_SCOPE`, `COGNEE_RECALL_SOURCE`, `COGNEE_SESSION_ENTRY_COUNT`). 39 new tests total (34 router + 2 stats + 3 integration).

### Deviations

- **Step 10 (span-attribute smoke test) skipped** — optional per plan, would require `tracing-subscriber` test-writer setup for minimal value.
- **`Matcher::AnyString` enum variant from plan sketch omitted** — all non-keyword rules use regex; the variant would have been dead code.

