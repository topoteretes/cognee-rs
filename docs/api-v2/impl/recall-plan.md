# Implementation Plan: `recall()`

**Gap doc:** [../recall.md](../recall.md)  
**Python reference:** `cognee/api/v1/recall/recall.py` (+ `query_router.py`)  
**Rust entry point:** `crates/lib/src/api/recall.rs` (+ `crates/search/src/query_router.rs`)

---

## 1. Goal & Scope

Bring the Rust port of `recall()` from its current ~95%-implemented state up to full Python parity on both behaviour and observability so the two SDKs can be treated as interchangeable from a caller's perspective.

### Final state

- `crates/search/src/query_router.rs` classifies queries with the **same rule set, weights, and scoring semantics** as `cognee/api/v1/recall/query_router.py` — including anchored lexical phrase match, reasoning connectives, explicit year-range boost, and the full relationship rule split. All fourteen Python rules have direct counterparts.
- A new module `crates/search/src/query_router_stats.rs` exposes a process-global override counter, matching `override_counts` / `record_override()` in `query_router.py` (lines 152–168).
- `crates/lib/src/api/recall.rs` records a router override whenever the caller passes an explicit `query_type` while `auto_route=true`, exactly mirroring the Python dispatch in `recall.py:225–231`.
- `recall()` emits a `tracing` span (`cognee.api.recall`) carrying the semantic attributes from `crates/search/src/observability.rs` — query (truncated to 500 chars), scope, session_id, top_k, search_type, result count, recall scope, and recall source — the Rust equivalent of Python's `new_span("cognee.api.recall")` span attributes (`recall.py:183–260`).
- Ports of every Python test class in `cognee/tests/unit/api/v2/test_query_router.py` live next to the Rust router as inline unit tests.

### Scope boundaries

In scope: rule-set parity, override tracking, span-attribute parity via `tracing`, test parity.  
Out of scope: a `send_telemetry`-style analytics backend, a `RecallKwargs` TypedDict analogue, cloud delegation (`get_remote_client`). Each is flagged in `recall.md` §5 as either N/A or optional.

---

## 2. Design Overview

### 2.1 `override_counts` — how to realize it in Rust

Python keeps a module-level `dict[(SearchType, SearchType), int]` (`query_router.py:154`) that survives for the lifetime of the interpreter and is directly mutable by `record_override()`. Tests (`test_query_router.py::TestOverrideTracking` lines 154–164) import and clear that dict.

**Decision:** use `std::sync::OnceLock<std::sync::Mutex<HashMap<(SearchType, SearchType), u64>>>` in a new file `crates/search/src/query_router_stats.rs`. Rationale:

1. `OnceLock` is in `std` on edition 2024; the workspace deliberately avoids `once_cell` / `lazy_static` where possible.
2. `SearchType` already derives `Copy + Eq + Hash` (`crates/search/src/types/search_type.rs:3`), so it can key a `HashMap`.
3. A `Mutex` (not `RwLock`) is correct: all traffic is short write-heavy increments; contention is negligible.
4. `tracing::info!` is the Rust analog of Python's `logger.info(...)` inside `record_override()`.

A `clear_override_counts()` helper is exposed for tests — matching `override_counts.clear()` in the Python tests.

### 2.2 Rule-set parity in `route_query`

The Rust router (`crates/search/src/query_router.rs:29–174`) uses substring matching and is structurally divergent in several places. The gap doc flags this as "mild false positives" but the behavioural tests in `test_query_router.py` assume Python's exact semantics.

**Divergences to fix:**

1. **Drop the always-present `(GraphCompletion, 2.0)` seed.** Python only falls back to that when the `scores` dict is empty.
2. **Replace the ad-hoc Cypher keyword counter.** Python fires the Cypher rule as a single binary match at weight 10.0 whenever the regex `(^MATCH\s|^RETURN\s|^CREATE\s|^MERGE\s|--\(|\)--)` matches.
3. **Anchor the lexical-quoted rule** to `^"[^"]+"$` — use `regex`.
4. **Add the lexical-intent rule** (`exact/verbatim/literal/word.for.word`) at weight 4.0 — currently absent.
5. **Add the reasoning-connectives rule** (`because/therefore/consequently`) at weight 2.0.
6. **Split the relationship rule into the three Python rules** (5.0 + 5.0 + 3.0) so "path between" / "degree of separation" / "what connects" get their distinct 5.0 weights.
7. **Add the year-range rule** `between \d{4} and \d{4}` at weight 6.0, compiled via `regex`.
8. **Temporal keywords** with per-keyword weights 3.0, except `timeline/chronolog/era/decade/century` which get 4.0, and the 4-digit year pattern which gets 3.0.
9. **Narrow the negation window from 25 → 20 chars** to match Python exactly.
10. **Word-boundary check** — implement a helper `contains_word(text, kw)` that verifies the char before the match is not alphanumeric and the char after the last char of the match is not alphanumeric.

**Decision:** port the Python `_RULES` list verbatim. Keep substring matching, but add word-boundary helpers. For two rules pull in `regex` (anchored quote and year-range).

### 2.3 Recording overrides from `recall()`

In `crates/lib/src/api/recall.rs` the branch that picks the search type (lines 101–113) currently drops the router entirely when `query_type` is explicit. Python runs the router **anyway** when `auto_route=True` so it can call `record_override()` (`recall.py:225–231`). We replicate that: if `query_type` is `Some(...)` **and** `auto_route` is true, still call `route_query()`, then `record_override(routed, user_choice)`.

### 2.4 `tracing` spans vs Python telemetry

Python does two separate things in `recall()`:

1. `send_telemetry("cognee.recall", ...)` — pushes an analytics event to a PostHog-style backend. This requires a network-reachable analytics endpoint; in Rust we have no equivalent and no plan to add one.
2. `new_span("cognee.api.recall")` with `span.set_attribute(...)` — OpenTelemetry-style tracing span used for in-process debugging.

**Decision:**

- **Skip** `send_telemetry`. Justified by `recall.md` §5B and the absence of any analytics backend in the Rust crate graph.
- **Implement** the span via `tracing::info_span!("cognee.api.recall", ...)` and `span.record()` for attributes set after the span opens. Use the attribute-name constants already declared in `crates/search/src/observability.rs`. Add four new constants to that file to match Python's `COGNEE_SEARCH_QUERY`, `COGNEE_RECALL_SCOPE`, `COGNEE_RECALL_SOURCE`, `COGNEE_SESSION_ENTRY_COUNT`.

---

## 3. Step-by-Step Implementation

### Step 1 — Add semantic-attribute constants for recall

**File:** `crates/search/src/observability.rs`  
**Depends on:** none.

Add four constants after the existing list (currently ends at line 47):

```rust
/// The natural-language query text (truncated to 500 chars for PII control).
pub const COGNEE_SEARCH_QUERY: &str = "cognee.search.query";

/// Recall scope — "session", "auto", or "graph".
pub const COGNEE_RECALL_SCOPE: &str = "cognee.recall.scope";

/// Recall result source — "session", "graph", or "cloud".
pub const COGNEE_RECALL_SOURCE: &str = "cognee.recall.source";

/// Number of session Q&A entries that matched the keyword search.
pub const COGNEE_SESSION_ENTRY_COUNT: &str = "cognee.session.entry_count";
```

### Step 2 — Create the override-tracking module

**File (new):** `crates/search/src/query_router_stats.rs`  
**Depends on:** none.

```rust
//! Process-global router-override counters.
//! Ports `override_counts` / `record_override()` from
//! `cognee/api/v1/recall/query_router.py` (lines 152–168).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::types::SearchType;

static OVERRIDE_COUNTS: OnceLock<Mutex<HashMap<(SearchType, SearchType), u64>>> = OnceLock::new();

fn counts() -> &'static Mutex<HashMap<(SearchType, SearchType), u64>> {
    OVERRIDE_COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn record_override(routed: SearchType, override_type: SearchType) {
    if routed == override_type {
        return;
    }
    // lock poison is unrecoverable
    let mut guard = counts().lock().unwrap();
    let key = (routed, override_type);
    let entry = guard.entry(key).or_insert(0);
    *entry += 1;
    tracing::info!(
        routed = ?routed,
        user_chose = ?override_type,
        total = *entry,
        "Router override recorded"
    );
}

pub fn override_counts_snapshot() -> HashMap<(SearchType, SearchType), u64> {
    counts().lock().unwrap().clone()
}

pub fn clear_override_counts() {
    counts().lock().unwrap().clear();
}
```

### Step 3 — Export the stats module

**File:** `crates/search/src/lib.rs`  
**Depends on:** Step 2.

After the existing `pub mod query_router;` at line 4, add:

```rust
pub mod query_router_stats;
```

And extend the re-export group (currently line 11):

```rust
pub use query_router::{RouteResult, route_query};
pub use query_router_stats::{
    clear_override_counts, override_counts_snapshot, record_override,
};
```

### Step 4 — Port Python's rule set into the Rust router

**File:** `crates/search/src/query_router.rs`  
**Depends on:** none.

This is the largest change. Replace the body of `route_query()` (currently lines 29–174) with a table-driven implementation that mirrors Python's `_RULES` verbatim (`query_router.py:49–147`). See §2.2 for the 10 specific adjustments.

Sketch of the new core:

```rust
use std::sync::OnceLock;
use regex::Regex;
use crate::types::SearchType;

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub search_type: SearchType,
    pub confidence: f32,
    pub runner_up: SearchType,
    pub runner_up_score: f32,
    pub all_scores: Vec<(SearchType, f32)>,
}

impl RouteResult {
    pub fn is_confident(&self) -> bool {
        self.confidence >= 2.0 * self.runner_up_score.max(1.0)
    }
}

const DEFAULT_TYPE: SearchType = SearchType::GraphCompletion;
const DEFAULT_BASE_SCORE: f32 = 2.0;
const NEGATION_WINDOW: usize = 20;
const NEGATION_WORDS: &[&str] = &["not", "n't", "no", "never", "without", "lack"];

enum Matcher {
    Keywords(&'static [&'static str]),
    Regex(&'static OnceLock<Regex>, &'static str),
}

struct Rule { matcher: Matcher, target: SearchType, weight: f32, respects_negation: bool }

fn rules() -> &'static [Rule] { /* static table mirroring _RULES */ }

pub fn route_query(query: &str) -> RouteResult {
    let q = query.trim();
    let lower = q.to_lowercase();
    let mut scores: Vec<(SearchType, f32)> = Vec::new();

    for rule in rules() {
        if let Some(m_start) = rule_matches(rule, q, &lower) {
            if rule.respects_negation && is_negated(&lower, m_start) { continue; }
            add_score(&mut scores, rule.target, rule.weight);
        }
    }

    if scores.is_empty() {
        return RouteResult {
            search_type: DEFAULT_TYPE,
            confidence: DEFAULT_BASE_SCORE,
            runner_up: DEFAULT_TYPE,
            runner_up_score: 0.0,
            all_scores: Vec::new(),
        };
    }

    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (best_type, best_score) = scores[0];
    let (ru_type, ru_score) = scores.get(1).copied().unwrap_or((DEFAULT_TYPE, 0.0));

    if best_score < DEFAULT_BASE_SCORE {
        return RouteResult {
            search_type: DEFAULT_TYPE,
            confidence: best_score,
            runner_up: best_type,
            runner_up_score: best_score,
            all_scores: scores,
        };
    }

    RouteResult { search_type: best_type, confidence: best_score, runner_up: ru_type, runner_up_score: ru_score, all_scores: scores }
}
```

Add `regex = "1"` to `crates/search/Cargo.toml` if not already present.

### Step 5 — Wire override tracking into `recall()`

**File:** `crates/lib/src/api/recall.rs`  
**Depends on:** Steps 2, 3.

Update the `use` line 13 to import `record_override`:

```rust
use cognee_search::{
    SearchOrchestrator, SearchRequest, SearchResponse, SearchType, record_override, route_query,
};
```

Rewrite the branch at lines 100–113 to match Python's `recall.py:225–238`:

```rust
let (search_type, auto_routed) = match (query_type, auto_route) {
    (Some(qt), true) => {
        let routed = route_query(query_text);
        record_override(routed.search_type, qt);
        (qt, false)
    }
    (Some(qt), false) => (qt, false),
    (None, true) => {
        let routed = route_query(query_text);
        info!(
            search_type = ?routed.search_type,
            confidence = routed.confidence,
            "recall: auto-routed query"
        );
        (routed.search_type, true)
    }
    (None, false) => (SearchType::GraphCompletion, false),
};
```

### Step 6 — Emit a `cognee.api.recall` span with Python-parity attributes

**File:** `crates/lib/src/api/recall.rs`  
**Depends on:** Step 1, Step 5.

At the top of `recall()` after computing `scope`, open an info-level span and record attributes as they become known:

```rust
use tracing::field;
use cognee_search::observability::{
    COGNEE_RECALL_SCOPE, COGNEE_RECALL_SOURCE, COGNEE_RESULT_COUNT, COGNEE_SEARCH_QUERY,
    COGNEE_SEARCH_TYPE, COGNEE_SESSION_ENTRY_COUNT, COGNEE_SESSION_ID,
};

let scope = match (session_id, datasets.as_deref(), query_type) {
    (Some(_), None, None) => "session",
    (Some(_), Some(_), _) => "auto",
    _ => "graph",
};

let query_preview = &query_text[..query_text.len().min(500)];

let span = tracing::info_span!(
    "cognee.api.recall",
    { COGNEE_SEARCH_QUERY } = query_preview,
    { COGNEE_RECALL_SCOPE } = scope,
    { COGNEE_SESSION_ID } = session_id.unwrap_or(""),
    "cognee.recall.top_k" = top_k,
    { COGNEE_SEARCH_TYPE } = field::Empty,
    { COGNEE_RECALL_SOURCE } = field::Empty,
    { COGNEE_RESULT_COUNT } = field::Empty,
    { COGNEE_SESSION_ENTRY_COUNT } = field::Empty,
);
let _enter = span.enter();
```

Then at each return site record the empty fields.

### Step 7 — Port `test_query_router.py` into Rust

**File:** `crates/search/src/query_router.rs` (the existing `#[cfg(test)] mod tests` block)  
**Depends on:** Step 4.

Translate each class in `test_query_router.py` (lines 7–164) into `#[test]` functions, grouped by `mod factual_queries { ... }` / `mod cypher { ... }` / etc.

### Step 8 — Port `TestOverrideTracking`

**File:** `crates/search/src/query_router_stats.rs` (inline `#[cfg(test)]`)  
**Depends on:** Step 2.

```rust
#[test]
#[serial_test::serial]
fn record_override_increments() {
    clear_override_counts();
    record_override(SearchType::GraphCompletion, SearchType::Temporal);
    record_override(SearchType::GraphCompletion, SearchType::Temporal);
    let snap = override_counts_snapshot();
    assert_eq!(snap[&(SearchType::GraphCompletion, SearchType::Temporal)], 2);
}

#[test]
#[serial_test::serial]
fn same_type_not_recorded() {
    clear_override_counts();
    record_override(SearchType::Temporal, SearchType::Temporal);
    assert!(override_counts_snapshot().is_empty());
}
```

### Step 9 — Add an integration test for `recall()` override dispatch

**File (new):** `crates/lib/tests/recall_override.rs`  
**Depends on:** Steps 4–6.

Drive `recall()` with a `MockGraphDB` / mocked orchestrator, pass `query_type=Some(SearchType::Temporal)` on a query like "give me a summary of X" (which the router picks as `GraphSummaryCompletion`), and assert `override_counts_snapshot()` contains `(GraphSummaryCompletion, Temporal) = 1`.

### Step 10 — Span-attribute smoke test (optional, low cost)

**File:** same test file as Step 9.  
**Depends on:** Step 6.

Use `tracing-subscriber::fmt::TestWriter` + `tracing_test::traced_test` to capture emitted events and assert the span fields are populated.

---

## 4. Test Plan Summary

| Origin | Rust location | Count | Notes |
|---|---|---|---|
| `test_query_router.py::TestFactualQueries` | `query_router.rs` tests | 3 | direct ports |
| `TestCypherQueries` | same | 2 | |
| `TestCodingRules` (incl. 2 negatives) | same | 4 | requires word-boundary code-syntax rule |
| `TestLexical` | same | 2 | anchored-quote test requires regex rule |
| `TestSummary` | same | 3 | |
| `TestReasoning` | same | 2 | "why/explain" weighted 4.0 |
| `TestRelationship` (incl. `between_not_temporal`) | same | 3 | requires split rules |
| `TestTemporal` (incl. year-range) | same | 4 | requires year-range regex rule |
| `TestNegation` (3 cases) | same | 3 | 20-char window |
| `TestConfidence` | same | 3 | `is_confident()` method |
| `TestAmbiguousQueries` | same | 3 | relies on weight parity |
| `TestOverrideTracking` | `query_router_stats.rs` tests | 2 | process-global counter |
| Override-dispatch integration | `crates/lib/tests/recall_override.rs` | 1 | end-to-end `recall()` |
| Span-attribute smoke | same | 1 | optional |

---

## 5. Effort Breakdown

| Step | Rough hours |
|---|---:|
| 1. Observability constants | 0.25 |
| 2. `query_router_stats` module | 1.0 |
| 3. Search crate re-exports | 0.25 |
| 4. Port Python rules into `query_router.rs` | 3.5 |
| 5. Override wiring in `recall()` | 0.5 |
| 6. `tracing` span with Python-parity attributes | 1.0 |
| 7. Port `test_query_router.py` unit tests | 2.0 |
| 8. `TestOverrideTracking` port | 0.25 |
| 9. Recall override integration test | 1.0 |
| 10. Span-attribute smoke test (optional) | 1.0 |
| **Total** | **~10.75 h** (~1.5 working days) |

Aligns with the "Medium, ~8–12 hours" estimate in `recall.md` §6.

---

## 6. Out of Scope

1. **`send_telemetry`-style analytics emission** — no equivalent backend in Rust.
2. **`RecallKwargs` TypedDict analogue** — callers in Rust build a `SearchRequest` explicitly.
3. **Cloud delegation (`get_remote_client`)** — `recall.md` §5D marks this N/A for Rust.
4. **LLM-based query routing** — the router is deliberately rule-based on both sides.
5. **New search types** — SearchType enum already matches Python (15 variants).

---

## 7. Open Questions

1. Should `override_counts_snapshot()` be gated behind a `diagnostics` feature? For now, expose unconditionally — it is pure data.
2. Should we add a periodic flush (e.g. log top-N overrides every hour)? Python does not; we match Python exactly and defer.
3. Does any downstream crate already depend on the exact `RouteResult` shape? `crates/lib/src/api/recall.rs:104` reads `.search_type` and `.confidence` only; adding `all_scores: Vec<(SearchType, f32)>` is additive.
