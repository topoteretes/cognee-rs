# Task 02-07 — Replace `forget.rs` placeholder + port the callsite catalog

**Status**: implemented in commit 8b096bb (note: routers are deferred per Option A — SDK callsites fire once and the ~32 HTTP router endpoints will add a thin endpoint property in a follow-up. Search orchestrator's three Ok(...) return paths route through a new emit_search_completed helper to avoid drift. cognee-lib/telemetry now activates cognee-search/telemetry and cognee-session/telemetry so the workspace toggle stays a single switch).
**Owner**: _unassigned_
**Depends on**: [Task 02-06 — Public API + noop fallback](06-public-api-and-noop.md)
**Blocks**:
- [Task 02-09 — Integration tests](09-integration-tests.md) (the integration test exercises a real call site).
- [Task 02-10 — Cross-SDK parity](10-cross-sdk-parity.md) (Rust must emit *some* event for the parity assertion to fire).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

This task does the actual *integration* of `send_telemetry` with the
existing cognee API surface. Three deliverables:

1. **Replace** the `tracing::info!(target: "cognee.telemetry", ...)`
   placeholder at
   [`crates/lib/src/api/forget.rs:103-123`](../../../crates/lib/src/api/forget.rs#L103-L123)
   with a real call to `cognee_telemetry::send_telemetry`. This is
   the canonical example for every other call site.
2. **Port the SDK-level catalog** — the events Python fires from the
   library API (`cognee.recall`, `cognee.remember`, `cognee.improve`,
   plus the search and session add_qa equivalents). One emission per
   public function, just before or after the main work.
3. **Port the HTTP-router catalog** — Python fires `... API Endpoint
   Invoked` events from every `aiohttp` route handler. Decision 4
   says routers add a thin `endpoint` property and **do not**
   duplicate the SDK event. Our job: thread the `endpoint` property
   through the existing routers so the SDK call sees it.

**Out of scope per decision 9:** pipeline-run-status events
(`Pipeline Run Started/Completed/Errored`) and per-task lifecycle
events (`${task_type} Task Started/Completed/Errored`). Those are
high-volume and best handled by gap
[`03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md).

## 2. Rationale

### Why `forget.rs` is the canonical example

The existing placeholder at lines 103-123 already gates emission
behind `#[cfg(feature = "telemetry")]` and constructs structured
fields (`target_label`, `dataset_dbg`, `data_id_dbg`,
`cognee_version`, `owner_id`). Replacing that single block exercises
the full pipeline (identity derivation → payload → dispatch) and
serves as the pattern for every sibling SDK function. Reviewers can
then verify `recall`, `remember`, etc. by analogy.

### Why one event per SDK function, not per router AND SDK

Decision 4: emission ownership lives at the SDK layer. Routers add a
property (e.g. `endpoint = "POST /api/v1/forget"`) and pass it
through; the SDK function merges it into `additional_properties`
before calling `send_telemetry`. This avoids double-counting when a
single user request travels CLI → SDK → router (the CLI shells the
HTTP endpoint, the router dispatches to the SDK function, and only
the SDK fires).

### Why we accept Python's event-name conventions wholesale

Python uses verbose strings like `"cognee.search EXECUTION STARTED"`
that look unusual. Cross-SDK identity grouping requires byte-equal
event names so the proxy treats them as the same dashboard row. We
copy the strings verbatim — no normalisation, no abbreviation.

## 3. Pre-conditions

- [Task 02-06](06-public-api-and-noop.md) merged — the public
  surface `cognee_lib::telemetry::send_telemetry` exists and is
  reachable.
- Each target file builds with `--features telemetry` and
  `--no-default-features`.

## 4. Step-by-step

### 4.1 Replace `forget.rs` placeholder

[`crates/lib/src/api/forget.rs:103-123`](../../../crates/lib/src/api/forget.rs#L103-L123)
currently reads (per the explore report):

```rust
// Telemetry: emit an external event log (gated behind the `telemetry`
// feature flag). Mirrors Python's `send_telemetry("cognee.forget", ...)`.
#[cfg(feature = "telemetry")]
{
    let (target_label, dataset_dbg, data_id_dbg) = match &target {
        ForgetTarget::Item { data_id, dataset } => {
            ("data_item", format!("{dataset:?}"), data_id.to_string())
        }
        ForgetTarget::Dataset { dataset } => ("dataset", format!("{dataset:?}"), String::new()),
        ForgetTarget::All => ("everything", String::new(), String::new()),
    };
    tracing::info!(
        target: "cognee.telemetry",
        event = "cognee.forget",
        forget_target = target_label,
        dataset = %dataset_dbg,
        data_id = %data_id_dbg,
        cognee_version = env!("CARGO_PKG_VERSION"),
        owner_id = %owner_id,
    );
}
```

Replace with:

```rust
// Mirrors Python `send_telemetry("cognee.forget", ...)` from
// cognee/api/v1/forget/forget.py:79.
#[cfg(feature = "telemetry")]
{
    use cognee_telemetry::send_telemetry;
    let (target_label, dataset_dbg, data_id_dbg) = match &target {
        ForgetTarget::Item { data_id, dataset } => {
            ("data_item", format!("{dataset:?}"), data_id.to_string())
        }
        ForgetTarget::Dataset { dataset } => ("dataset", format!("{dataset:?}"), String::new()),
        ForgetTarget::All => ("everything", String::new(), String::new()),
    };
    send_telemetry(
        "cognee.forget",
        owner_id,
        Some(serde_json::json!({
            "target": target_label,
            "dataset": dataset_dbg,
            "data_id": data_id_dbg,
            "cognee_version": env!("CARGO_PKG_VERSION"),
        })),
    );
}
```

Field naming matches Python forget.py:79 (`target`, `dataset`,
`data_id`, `cognee_version`). The `cognee_version` field is also
populated by the payload struct itself (decision 11) — keeping it in
`additional_properties` is harmless redundancy and matches Python.

### 4.2 SDK-level catalog

For each row below, locate the file/function listed in the explore
report (column "Rust source"), insert a `send_telemetry` call **once
per public function**, gated by `#[cfg(feature = "telemetry")]`,
with the `additional_properties` listed.

| Event | Python source | Rust source | `additional_properties` |
|---|---|---|---|
| `cognee.forget` | forget.py:79 | [`crates/lib/src/api/forget.rs:103`](../../../crates/lib/src/api/forget.rs#L103) | `target`, `dataset`, `data_id`, `cognee_version` (already done in §4.1) |
| `cognee.recall` | recall.py:402 | [`crates/lib/src/api/recall.rs:65`](../../../crates/lib/src/api/recall.rs#L65) | `query_length`, `scope`, `auto_route`, `top_k`, `search_type`, `session_id`, `datasets`, `dataset_ids` |
| `cognee.remember` | remember.py:624 | [`crates/lib/src/api/remember.rs:198`](../../../crates/lib/src/api/remember.rs#L198) (`pub async fn remember`). Note also `remember_entry` at line 603 — the typed-entry path. Decide whether to emit from one or both per Python parity (Python emits in the single `remember` function regardless of path). | `mode`, `data_size_bytes`, `item_count`, `session_id` |
| `cognee.improve` | improve.py:91 | [`crates/lib/src/api/improve.rs:125`](../../../crates/lib/src/api/improve.rs#L125) (`pub async fn improve(params: ImproveParams<'_>)`). | `dataset`, `session_count`, `session_ids`, `run_in_background`, `cognee_version` |
| `cognee.search EXECUTION STARTED` | search.py:74 | [`crates/search/src/orchestration/search_orchestrator.rs:114`](../../../crates/search/src/orchestration/search_orchestrator.rs#L114) — `#[tracing::instrument]` is at line 106; `pub async fn search` signature is at line 114; emit just inside the function body after `{`. | `cognee_version`, `tenant_id` |
| `cognee.search EXECUTION COMPLETED` | search.py:115 | same file — three `Ok(...)` returns exist (early short-circuits at lines 294 and 337, final at line 385). Cleanest pattern: bind `let response = ...;` then emit once before `Ok(response)` at line 385, *or* extract a small `emit_completed(...)` helper called from each return path. Implementer's choice; document in commit message. | `cognee_version`, `tenant_id` |
| `cognee.session.add_qa` | session_manager.py:171 | [`crates/session/src/session_manager.rs:96`](../../../crates/session/src/session_manager.rs#L96) (`save_qa` method, after the row is persisted) | `session_id`, `data_size_bytes`, `has_feedback`, `has_graph_elements` |

**Search EXECUTION started/completed pair** — emit at the boundaries
of the orchestrator. Skip the `EXECUTION FAILED` analogue for now;
it requires a `?`-tracked error path that is cleaner to add in a
follow-up.

**Pattern** (recall as the template):

```rust
#[cfg(feature = "telemetry")]
cognee_telemetry::send_telemetry(
    "cognee.recall",
    owner_id, // or `&owner_id.to_string()`, depending on signature
    Some(serde_json::json!({
        "query_length": query.len(),
        "scope": scope_label,         // e.g. "global" / "dataset"
        "auto_route": auto_route,
        "top_k": top_k,
        "search_type": search_type_label,
        "session_id": session_id,
        "datasets": datasets,         // Vec<String> serializes as array
        "dataset_ids": dataset_ids,   // Vec<Uuid> serializes via serde
    })),
);
```

Each call must:

- Be gated by `#[cfg(feature = "telemetry")]`.
- Use the `cognee_lib::telemetry::send_telemetry` re-export (or
  `cognee_telemetry::send_telemetry` directly within crates that
  depend on `cognee-telemetry`).
- Pass the **caller's `user_id`** as the second argument when
  available (`&User.id`, `owner_id`); pass the symbolic string
  `"sdk"` for codepaths with no user.
- Place the call **just before** the function returns successfully.
  Failed paths can be added later (Python has them; gap 02 covers
  successes only — see "Out of scope" below).

### 4.3 HTTP-router catalog

Per decision 4, routers add an `endpoint` property and **do not**
emit a duplicate event. Concretely: routers wrap the SDK call and
pass `endpoint` via the SDK function's existing
`additional_properties`. Two integration shapes are possible:

**Option A — extend the SDK signature** (clean, breaking):
```rust
pub fn forget(... , extra_telemetry: Option<Value>) -> Result<...>;
```
Routers pass `Some(json!({ "endpoint": "POST /api/v1/forget" }))`.

**Option B — thread via a thread-local** (non-breaking, magic):
A `tokio::task_local!` hint that the SDK reads when building its
`additional_properties`.

**Recommendation: Option A**, but defer the implementation of the
extra-arg threading to a follow-up PR. For this gap, **routers do
nothing extra** — the SDK fires the event without an `endpoint`
property. Document the gap in `additional_properties.endpoint` as a
known follow-up.

**Why defer:** changing the SDK signatures touches every public API
and every binding (PyO3, Neon, C). That's too much surface for a
single gap. The `endpoint` property is a nice-to-have, not a parity
requirement — Python's analytics dashboards already group by
`event_name`, and the per-endpoint context is captured implicitly
because each endpoint hits a distinct event name (`cognee.forget`,
`cognee.recall`, etc.).

If a future task wants per-endpoint differentiation:

| Python event | Rust event added | Routers that fire it |
|---|---|---|
| `Add API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/add.rs::post_add` |
| `Cognify API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/cognify.rs` |
| `Search API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/search.rs::post_search` |
| `Recall API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/recall.rs` |
| `Remember API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/remember.rs` |
| `Forget API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/forget.rs` |
| `Improve API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/improve.rs` |
| `Update API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/update.rs` |
| `Memify API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/memify.rs` |
| `Sync API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/sync.rs` |
| `Datasets API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/datasets.rs` |
| `Ontology API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/ontologies.rs` |
| `LLM API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/llm.rs` |
| `API Keys API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/api_keys.rs` |
| `Permissions API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/permissions.rs` |
| `Visualize API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/visualize.rs` |
| `Delete API Endpoint Invoked` | (defer) | `crates/http-server/src/routers/delete.rs` |

A single follow-up sub-doc (e.g. `02b-router-endpoint-events.md`)
can cover the whole table after gap 02 lands.

### 4.4 Update `cognee-search`, `cognee-session` to depend on `cognee-telemetry`

Two crates need a new dep:

#### `crates/search/Cargo.toml`

```toml
[features]
telemetry = ["dep:cognee-telemetry", "cognee-telemetry/telemetry"]

[dependencies]
cognee-telemetry = { path = "../telemetry", optional = true }
```

#### `crates/session/Cargo.toml`

Same pattern.

#### Re-export through `cognee-lib`

Update `crates/lib/Cargo.toml` `telemetry` feature to also pull the
new feature flags:

```toml
telemetry = [
    "dep:cognee-observability",
    "cognee-observability/telemetry",
    "cognee-core/telemetry",
    "dep:cognee-telemetry",
    "cognee-telemetry/telemetry",
    "cognee-search/telemetry",
    "cognee-session/telemetry",
]
```

If `cognee-search` and `cognee-session` are not directly
re-exported from `cognee-lib`, this still works because Cargo
resolves the feature graph transitively — but the unification
prevents accidental drift if a binary opts into a sub-crate's
`telemetry` without going through the umbrella.

### 4.5 Verify

```bash
# 1. All feature combinations still build.
cargo check --workspace --all-targets
cargo check --workspace --all-targets --features telemetry
cargo check --workspace --all-targets --no-default-features

# 2. The forget event fires when called.
RUST_LOG=cognee.telemetry=trace,debug \
  cargo test -p cognee-lib --features telemetry test_forget
# (Look for the dispatcher's debug logs in the output.)

# 3. Existing tests still pass.
scripts/check_all.sh
```

## 5. Verification

```bash
# 1. Format + compile + clippy + bindings (the standard gate).
scripts/check_all.sh

# 2. Each new emission is exercised at least once in the existing
#    test suite. Spot-check by grepping for the event name and
#    confirming a test imports the corresponding API.
grep -rn '"cognee.forget"' crates/lib/src/api/forget.rs
grep -rn '"cognee.recall"' crates/lib/src/api/recall.rs
# ... etc.

# 3. The mockito-based integration test in [task 02-09](09-integration-tests.md)
#    will assert that one of these emissions actually fires; this
#    task only ensures the call-site code compiles and runs.
```

## 6. Files modified

- [`crates/lib/src/api/forget.rs`](../../../crates/lib/src/api/forget.rs)
  — replace placeholder block (lines 103-123).
- [`crates/lib/src/api/recall.rs`](../../../crates/lib/src/api/recall.rs)
  — add `send_telemetry` call.
- [`crates/lib/src/api/remember.rs`](../../../crates/lib/src/api/remember.rs)
  — add `send_telemetry` call.
- [`crates/lib/src/api/improve.rs`](../../../crates/lib/src/api/improve.rs)
  — add `send_telemetry` call.
- [`crates/search/src/orchestration/search_orchestrator.rs`](../../../crates/search/src/orchestration/search_orchestrator.rs)
  — add two emissions (started + completed).
- [`crates/session/src/session_manager.rs`](../../../crates/session/src/session_manager.rs)
  — add `send_telemetry` call after `save_qa`.
- [`crates/search/Cargo.toml`](../../../crates/search/Cargo.toml)
  — add `telemetry` feature + dep.
- [`crates/session/Cargo.toml`](../../../crates/session/Cargo.toml)
  — add `telemetry` feature + dep.
- [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml)
  — propagate feature.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Search orchestrator already has `#[tracing::instrument]` — adding `send_telemetry` doubles emission visibility (one tracing event + one HTTP POST) | This is intentional — tracing is for in-process diagnostics, `send_telemetry` is for product analytics | Documented in [task 02-11](11-user-docs.md). |
| `recall.rs:65` line offset shifts before merge | Likely — the codebase is active | Sub-agent A re-validates the line numbers when running task 02-07 and updates this doc in place. |
| New deps trip `cargo deny` or workspace lockfile drift | Possible | Run `cargo update -p cognee-search` only if needed; rely on the workspace lockfile to converge. |
| The `cognee.search EXECUTION COMPLETED` event fires before the response is fully serialised, mismatching Python's exact timing | Negligible — Python emits at the same logical position (just before the return) | Acceptable parity gap; document in cross-SDK test if it surfaces. |
| `#[cfg(feature = "telemetry")]` blocks pollute readability of small functions | Mild | Consider extracting `fn telemetry_for_recall(...)` helpers in `crates/lib/src/api/recall.rs` if any one site grows past ~15 lines. Defer to reviewer judgement. |
| HTTP routers not getting per-endpoint differentiation today is a regression vs Python | Low — Python events differ by name, not by `endpoint` property in most cases | Documented as deferred follow-up; gap 02 still ships meaningful parity for the SDK layer. |

## 8. Out of scope

- Pipeline lifecycle events (`Pipeline Run Started/Completed/Errored`)
  and per-task lifecycle (`${task_type} Task Started/Completed/Errored`)
  — handled by [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  per decision 9.
- Failure-path events (`cognee.search EXECUTION FAILED`,
  `... Errored`) — defer to gap 03 because they share the
  pipeline-status lifecycle plumbing.
- HTTP-router per-endpoint events — defer to a follow-up sub-doc.
- The `code_description_to_code_part_search EXECUTION` events
  (Python ports without a Rust analogue today) — emit when the
  feature lands.
