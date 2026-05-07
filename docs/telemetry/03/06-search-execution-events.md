# Task 03-06 — `cognee.search EXECUTION STARTED` (paired with existing COMPLETED)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 03-01 — `tenant_id` plumbing](01-tenant-id-plumbing.md) (backfills `tenant_id` on the existing `EXECUTION COMPLETED` emitter, which currently sends `Null`).

**Blocks**:
- [Task 03-08 — Tests](08-tests.md).

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decision**: #3 — implement both `STARTED` and `COMPLETED` for byte-equal Python parity.

---

## 1. Goal

Two small edits to
[`crates/search/src/orchestration/search_orchestrator.rs`](../../crates/search/src/orchestration/search_orchestrator.rs):

1. **Add `cognee.search EXECUTION STARTED`** at the top of
   `SearchOrchestrator::search` ([line 136](../../crates/search/src/orchestration/search_orchestrator.rs#L136))
   — paired emission with the existing `EXECUTION COMPLETED` helper
   that gap 02-07 shipped at
   [line 18](../../crates/search/src/orchestration/search_orchestrator.rs#L18).

2. **Backfill `tenant_id`** on the existing `emit_search_completed`
   helper so it sends a real value (or `"Single User Tenant"` literal)
   instead of `serde_json::Value::Null`. Today the helper hard-codes
   `"tenant_id": serde_json::Value::Null` — replace with the
   `tenant_id_for_telemetry` helper introduced by [task 03-01](01-tenant-id-plumbing.md).

Both events carry the same payload: `cognee_version`, `tenant_id`.
`SearchRequest` already exposes `user_id` for the identity layer.

## 2. Rationale

### Why pair STARTED with the existing COMPLETED

Python emits both events around its inner `search()` method
([search.py:74 / search.py:115](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L74)).
Gap 02-07 shipped only the `COMPLETED` half (in three `Ok(...)`
return paths). The `STARTED` half is missing. Locked decision 3
chose Python parity over the originally-recommended drop, so
ship the pair.

### Where `tenant_id` comes from in `SearchRequest`

`SearchRequest` already carries `user_id` (used by the existing
`emit_search_completed`). It does **not** carry `tenant_id` today.
Two options:

- **Option A — extend `SearchRequest`** with `tenant_id: Option<Uuid>`.
  Larger surface change; touches all `SearchRequest` literal sites.
- **Option B — keep `tenant_id` out of `SearchRequest`** and emit
  the literal `"Single User Tenant"` until search has its own
  `PipelineContext` to read from.

**Recommend Option B.** The locked decision 1 fall-through to the
literal `"Single User Tenant"` is exactly designed for cases where
the caller has no tenant in scope. Adding `tenant_id` to
`SearchRequest` is a separate refactor that should land alongside
the broader API-events backfill (out of scope per the parent doc).

Sub-agent A should re-confirm before sub-agent B edits — if
`SearchRequest` already gained a `tenant_id` field by the time this
task lands, prefer Option A and update this doc in place.

## 3. Pre-conditions

- [Task 03-01](01-tenant-id-plumbing.md) merged — `tenant_id_for_telemetry`
  helper exists.
- A clean `cargo check --all-targets` on the post-task-01 tree.

## 4. Step-by-step

### 4.1 Add the `emit_search_started` helper

Edit [`crates/search/src/orchestration/search_orchestrator.rs`](../../crates/search/src/orchestration/search_orchestrator.rs).
Just below the existing `emit_search_completed` at lines 11-31, add
a parallel helper:

```rust
/// Fire-and-forget product analytics event for the start of a
/// search.
///
/// Mirrors Python `send_telemetry("cognee.search EXECUTION STARTED", ...)`
/// from `cognee/api/v1/search/search.py:74`. Called once at the top
/// of [`SearchOrchestrator::search`].
#[cfg(feature = "telemetry")]
fn emit_search_started(request: &SearchRequest) {
    cognee_telemetry::send_telemetry(
        "cognee.search EXECUTION STARTED",
        request.user_id,
        Some(serde_json::json!({
            "cognee_version": cognee_telemetry::cognee_version(),
            "tenant_id": "Single User Tenant",
        })),
    );
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_search_started(_request: &SearchRequest) {}
```

### 4.2 Wire it at the top of `search`

In `pub async fn search(...)` at
[line 136](../../crates/search/src/orchestration/search_orchestrator.rs#L136),
add the emit as the **first** statement of the function — before any
work, including dataset resolution:

```rust
pub async fn search(
    &self,
    request: &SearchRequest,
) -> Result<SearchResponse, crate::types::SearchError> {
    emit_search_started(request);

    let retriever: crate::retrievers::SearchRetrieverRef =
        if let Some(ref custom_type) = request.custom_search_type {
            // existing code unchanged…
```

This guarantees the event fires even when the function returns an
early error (matching Python: `EXECUTION STARTED` is emitted
unconditionally before any work).

### 4.3 Backfill `tenant_id` on the existing emitter

Edit `emit_search_completed` at lines 18-27. The current body sends
`Null`:

```rust
fn emit_search_completed(request: &SearchRequest) {
    cognee_telemetry::send_telemetry(
        "cognee.search EXECUTION COMPLETED",
        request.user_id,
        Some(serde_json::json!({
            "cognee_version": env!("CARGO_PKG_VERSION"),
            "tenant_id": serde_json::Value::Null,    // <-- replace
        })),
    );
}
```

Replace the `Null` with the literal `"Single User Tenant"` (matches
`emit_search_started` and Python parity), and switch
`env!("CARGO_PKG_VERSION")` to `cognee_telemetry::cognee_version()`
for consistency with the new `STARTED` helper:

```rust
fn emit_search_completed(request: &SearchRequest) {
    cognee_telemetry::send_telemetry(
        "cognee.search EXECUTION COMPLETED",
        request.user_id,
        Some(serde_json::json!({
            "cognee_version": cognee_telemetry::cognee_version(),
            "tenant_id": "Single User Tenant",
        })),
    );
}
```

> The `env!()` → `cognee_version()` swap is for symmetry only —
> both produce the same string. If sub-agent B prefers minimal-diff
> behaviour, leave the `env!` form untouched.

### 4.4 (Optional) Centralise the literal

If both helpers (and future ones) need `"Single User Tenant"`,
consider lifting the helper from
[task 03-01 §4.5](01-tenant-id-plumbing.md#45-optional-helper-for-the-wire-format-string)
out of `cognee-core` into `cognee-telemetry`. Then both
`crates/core/src/pipeline.rs` and
`crates/search/src/orchestration/search_orchestrator.rs` can call
`cognee_telemetry::tenant_id_for_telemetry(None)`. Sub-agent A
should confirm whether the helper landed in `cognee-telemetry` or
`cognee-core` (per task 03-01's open recommendation) and route
accordingly.

## 5. Verification

```bash
# 1. Compile both feature states.
cargo check --all-targets
cargo check --all-targets --no-default-features

# 2. Run the existing search tests.
cargo test -p cognee-search

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

End-to-end assertion (a search call emits exactly one
`EXECUTION STARTED` and one `EXECUTION COMPLETED` against a mockito
proxy) lives in [task 03-08](08-tests.md).

## 6. Files modified

- [`crates/search/src/orchestration/search_orchestrator.rs`](../../crates/search/src/orchestration/search_orchestrator.rs)
  — add `emit_search_started` helper (and noop fallback); call it as
  the first statement of `search`; backfill `tenant_id` on
  `emit_search_completed`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Early-return paths in `search()` cause `STARTED` to fire without `COMPLETED` | Intentional and matches Python. | Document in commit body: "STARTED fires unconditionally; COMPLETED only on success — same as Python." |
| `SearchRequest::user_id` shape changes | Low — tracked by existing `emit_search_completed`. | Compiler enforces. |
| Doubled emission via `search_batch` (line 117) | `search_batch` calls `search` internally — emission already happens once per inner call. Worth verifying. | Sub-agent C runs the `cargo test -p cognee-search` suite and looks for any new assertion failures around emission counts. |

## 8. Out of scope

- Threading `tenant_id` into `SearchRequest` (separate gap; preserves
  Option B fallback for now).
- `cognee.search EXECUTION ERRORED` — Python does not emit it; we
  match.
- Adding `query_length`, `top_k`, etc. to these events — those live
  on `cognee.recall` (already implemented in gap 02-07) and on the
  per-search OTEL span, not on the inner-search analytics event.

**Status**: implemented in commit 6be3e8e
