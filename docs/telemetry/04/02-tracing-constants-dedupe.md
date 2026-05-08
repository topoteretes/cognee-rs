# Task 04-02 — Deduplicate `cognee.*` tracing-key constants

**Status**: ✅ implemented in commit 1e03ac9
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 04-04 — Instrument `QdrantAdapter`](04-qdrant-instrumentation.md) (uses `COGNEE_DB_SYSTEM`, `COGNEE_VECTOR_COLLECTION`).
- [Task 04-05 — Instrument `LadybugAdapter`](05-ladybug-instrumentation.md) (uses `COGNEE_DB_SYSTEM`, `COGNEE_DB_QUERY`, `COGNEE_DB_ROW_COUNT`).
- [Task 04-06 — OpenAI LLM fields](06-openai-llm-fields.md) (uses `COGNEE_LLM_MODEL`, `COGNEE_LLM_PROVIDER`).
- [Task 04-07 — LiteRT LLM fields](07-litert-llm-fields.md) (same).
- [Task 04-08 — PG adapters](08-pg-adapters.md) (same).
- [Task 04-09 — SeaORM ops](09-seaorm-ops-instrumentation.md) (`COGNEE_DB_SYSTEM`).

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decision**: #7 — foundation cleanups split into two tasks; this is the second.

---

## 1. Goal

Make [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
the **single source of truth** for `cognee.*` semantic-attribute key
constants by:

1. Adding the seven keys that exist only in
   [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs)
   (`COGNEE_RESULT_COUNT`, `COGNEE_RESULT_SUMMARY`, `COGNEE_RETRIEVER`,
   `COGNEE_VECTOR_COLLECTION`, `COGNEE_USER_ID`,
   `COGNEE_DATA_ITEM_COUNT`, `COGNEE_SEARCH_QUERY`,
   `COGNEE_RECALL_SOURCE`, `COGNEE_SESSION_ENTRY_COUNT`) to the
   canonical set in `cognee_utils::tracing_keys`.
2. Adding the three vector-specific keys this gap will use
   (`COGNEE_VECTOR_RESULT_COUNT` for search results;
   `COGNEE_DB_ROW_COUNT` already exists; `COGNEE_VECTOR_COLLECTION`
   moves from `cognee-search`).
3. Replacing the body of
   [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs)
   with `pub use cognee_utils::tracing_keys::*;` so existing search
   call sites compile unchanged.
4. Verifying that the **exact string values** of the moved constants
   match — they already do (cross-check below) — so no on-the-wire
   span attribute renames result.

## 2. Rationale

The duplication came from gap-01 / search-instrumentation history;
both files declare overlapping constants with the same string values.
With six adapter tasks landing in this gap, every adapter call site
imports either set, and both sites now drift independently. Picking
`cognee_utils::tracing_keys` as canonical is the natural choice
because:

- `cognee-utils` is a leaf dep that everyone already pulls in.
- `cognee-search` already depends on `cognee-utils` transitively.
- The redaction helper (task 04-01) lives in `cognee-utils` too, so
  adapters import keys + redaction from the same crate.

Replacing rather than deleting `crates/search/src/observability.rs`
keeps the existing 14+ search-side imports working without a
mass-rename PR.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- Task 04-01 *not* required — these two tasks are independent. The
  runbook drives them in order for determinism only.
- No outstanding edits to
  [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
  or [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs).

## 4. Step-by-step

### 4.1 Cross-check overlap before editing

The current state (verified 2026-05-07) — values for the constants
that appear in both files:

| Constant | `cognee_utils::tracing_keys` value | `cognee-search::observability` value | Match? |
|---|---|---|---|
| `COGNEE_DB_SYSTEM` | `"cognee.db.system"` | `"cognee.db.system"` | ✅ |
| `COGNEE_LLM_MODEL` | `"cognee.llm.model"` | `"cognee.llm.model"` | ✅ |
| `COGNEE_SEARCH_TYPE` | `"cognee.search.type"` | `"cognee.search.type"` | ✅ |
| `COGNEE_PIPELINE_TASK_NAME` | `"cognee.pipeline.task_name"` | `"cognee.pipeline.task_name"` | ✅ |
| `COGNEE_OPERATION_MODE` | `"cognee.operation.mode"` | `"cognee.operation.mode"` | ✅ |
| `COGNEE_FORGET_TARGET` | `"cognee.forget.target"` | `"cognee.forget.target"` | ✅ |
| `COGNEE_DATASET_NAME` | `"cognee.dataset.name"` | `"cognee.dataset.name"` | ✅ |
| `COGNEE_SESSION_ID` | `"cognee.session.id"` | `"cognee.session.id"` | ✅ |
| `COGNEE_RECALL_SCOPE` | `"cognee.recall.scope"` | `"cognee.recall.scope"` | ✅ |

Sub-agent A must re-run the cross-check at task time and abort if any
value drifted. The implementation only re-exports — it must not
silently change a wire value.

### 4.2 Extend `crates/utils/src/tracing_keys.rs`

Append the eight constants that exist only in `cognee-search` plus
the new vector-result-count constant needed for task 04-04. Order
groups by namespace for readability:

```rust
// Append to crates/utils/src/tracing_keys.rs

// --- Vector / DB extras ----------------------------------------------------

/// The vector collection name queried (e.g. `"DocumentChunk_text"`).
pub const COGNEE_VECTOR_COLLECTION: &str = "cognee.vector.collection";

/// Number of results returned by a vector search call. Distinct from
/// `COGNEE_DB_ROW_COUNT` — vector search has *similarity hits* rather
/// than *rows*. Mirrors Python's `cognee.vector.result_count`
/// (LanceDB adapter).
pub const COGNEE_VECTOR_RESULT_COUNT: &str = "cognee.vector.result_count";

// --- Search retrieval ------------------------------------------------------

/// The number of results returned by a retriever (search-orchestrator
/// level, distinct from `COGNEE_VECTOR_RESULT_COUNT` which is
/// adapter-level).
pub const COGNEE_RESULT_COUNT: &str = "cognee.result.count";

/// A short human-readable summary of the search result (truncated).
pub const COGNEE_RESULT_SUMMARY: &str = "cognee.result.summary";

/// The retriever class or struct name handling this request
/// (e.g. `"GraphCompletionRetriever"`).
pub const COGNEE_RETRIEVER: &str = "cognee.retrieval.retriever";

/// The natural-language query text (truncated to 500 chars for PII
/// control). Apply `cognee_utils::redact::redact` before recording.
pub const COGNEE_SEARCH_QUERY: &str = "cognee.search.query";

/// Recall result source — `"session"`, `"graph"`, or `"cloud"`.
pub const COGNEE_RECALL_SOURCE: &str = "cognee.recall.source";

/// Number of session Q&A entries that matched the keyword search.
pub const COGNEE_SESSION_ENTRY_COUNT: &str = "cognee.session.entry_count";

// --- Identity --------------------------------------------------------------

/// The user identifier for the request.
pub const COGNEE_USER_ID: &str = "cognee.user.id";

// --- Data lifecycle --------------------------------------------------------

/// The number of data items affected by a delete operation.
pub const COGNEE_DATA_ITEM_COUNT: &str = "cognee.data.item_count";
```

Use the same string values that
`crates/search/src/observability.rs` currently declares. The
authoritative list:

| Constant | String value |
|---|---|
| `COGNEE_VECTOR_COLLECTION` | `"cognee.vector.collection"` |
| `COGNEE_VECTOR_RESULT_COUNT` | `"cognee.vector.result_count"` |
| `COGNEE_RESULT_COUNT` | `"cognee.result.count"` |
| `COGNEE_RESULT_SUMMARY` | `"cognee.result.summary"` |
| `COGNEE_RETRIEVER` | `"cognee.retrieval.retriever"` |
| `COGNEE_USER_ID` | `"cognee.user.id"` |
| `COGNEE_DATA_ITEM_COUNT` | `"cognee.data.item_count"` |
| `COGNEE_SEARCH_QUERY` | `"cognee.search.query"` |
| `COGNEE_RECALL_SOURCE` | `"cognee.recall.source"` |
| `COGNEE_SESSION_ENTRY_COUNT` | `"cognee.session.entry_count"` |

### 4.3 Replace `crates/search/src/observability.rs` body with re-export

Replace the full file contents with:

```rust
//! Semantic attribute constant names for cognee search telemetry.
//!
//! The canonical declarations live in
//! [`cognee_utils::tracing_keys`](../../utils/src/tracing_keys.rs) so
//! adapter and search call sites import the same constants. This
//! module re-exports them as a backwards-compat alias for existing
//! `cognee_search::observability::COGNEE_*` users.

pub use cognee_utils::tracing_keys::*;
```

This keeps the 14+ existing imports
(e.g. [`crates/search/src/recall_scope.rs:18`](../../crates/search/src/recall_scope.rs#L18))
compiling unchanged. New code in adapters should import
`cognee_utils::tracing_keys::*` directly.

### 4.4 Verify `cognee-search` still has the dep

Confirm [`crates/search/Cargo.toml`](../../crates/search/Cargo.toml)
already lists `cognee-utils = { path = "../utils" }`. (At time of
writing, the dep is **not** explicit on `cognee-search` and the
constants were duplicated precisely to avoid that edge.)

If the dep is missing, add it:

```toml
[dependencies]
# ... existing ...
cognee-utils = { path = "../utils" }
```

This is the only new graph edge introduced by this task. `cognee-utils`
is a leaf with three deps (`tokio`, `log`, `rand`, `uuid`); adding it
to `cognee-search` cannot create a cycle.

### 4.5 Sanity-grep for stragglers

```bash
# Should return only the new re-export module + the canonical file.
grep -rn 'pub const COGNEE_' crates/

# Should return only call sites importing from utils/tracing_keys
# or from search::observability (both legal post-task).
grep -rn 'COGNEE_DB_SYSTEM\|COGNEE_VECTOR_COLLECTION\|COGNEE_LLM_MODEL' crates/
```

If a third file declares its own duplicate, escalate — that file was
not surveyed by the gap analysis and may have a different value.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Search tests still pass (they import from search::observability).
cargo test -p cognee-search --no-run

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

A targeted smoke is enough — no behavioural change, only constant
re-export. Real coverage in [task 04-10](10-tests.md).

## 6. Files modified

- [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
  — append 10 new constants (see 4.2). Doc-comment each one with the
  same wording as the originals in `cognee-search/observability.rs`.
- [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs)
  — replace body with `pub use cognee_utils::tracing_keys::*;`.
- [`crates/search/Cargo.toml`](../../crates/search/Cargo.toml) — add
  `cognee-utils = { path = "../utils" }` only if not already present.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Constant value drift between the two declarations | Already verified equal in 4.1 — sub-agent A re-runs the cross-check at task time. | Abort the task if drift is found; the wire value is more important than the dedup. |
| `cognee-search` unexpectedly already had `cognee-utils` as a dep | None — checked at 2026-05-07. | If 4.4 finds it present, skip the dep addition. |
| Downstream crate depends on `crates/search/src/observability.rs` containing `pub const COGNEE_*` directly (rather than via `pub use`) | Very low — Rust's `pub use` is API-equivalent for value imports. | If any rustdoc link rot happens, accept it; doc paths through re-exports are stable since Rust 1.34. |
| Constants used in `cognee-search` that are *not* in `cognee-utils` and vice versa would produce import errors | Low — the dedupe explicitly adds the search-only ones to utils, so the re-export covers all callers. | The grep in 4.5 catches stragglers before the build. |

## 8. Out of scope

- Migrating individual call sites to import from `cognee_utils::tracing_keys`
  rather than `cognee_search::observability`. Both paths resolve to the
  same constants after this task. New code in the adapter tasks
  (04-04…04-09) imports from `cognee_utils::tracing_keys` directly;
  existing code stays.
- Adding entirely new semantic conventions. The constants added in
  4.2 are restricted to ones already present in `cognee-search` plus
  `COGNEE_VECTOR_RESULT_COUNT` (the only genuinely new one, needed
  by Qdrant search instrumentation in 04-04).
- Removing `crates/search/src/observability.rs` entirely. Its
  removal would force a rename PR across every search call site;
  cheap to defer.
