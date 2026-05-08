# Task 05-07 — User-label plumbing (`User.email` → `PipelineContext::user_email`)

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**:
- [Task 05-05 — `PipelineContext` fields](05-pipeline-context-fields.md) (the `user_email` field exists).

**Blocks**:
- [Task 05-10 — Tests](10-tests.md) (cross-SDK parity test asserts `source_user` equals the user email).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #4 — `user_email` field + `user_label()` helper.

---

## 1. Goal

Wire the `User.email` value (already on
[`cognee_models::User`](../../crates/models/src/user.rs#L13)) into
`cognee_core::PipelineContext::user_email` at every API entry point that
constructs a pipeline run. After this task:

- `cognify(...)`, `add(...)`, `memify(...)`, `search(...)` factory
  paths populate `user_email` when they have a `User` row.
- Fallback to `user_id.to_string()` happens only when no `User` row was
  fetched (e.g. CLI runs without an authenticated user). `user_label()`
  on `PipelineContext` does this fallback transparently — callers just
  set what they know.
- The cross-SDK parity test (05-10) can assert
  `node.source_user == "alice@example.com"` for a Rust-cognified graph
  the same way it does for Python.

## 2. Rationale

- Python populates `source_user` with the email-or-id string at the
  call site of `_stamp_provenance` in `run_tasks_base.py`, after
  loading a `User` SQLAlchemy row. Rust today has only `user_id:
  Option<Uuid>` on `PipelineContext`; without the email plumbing, every
  Rust-stamped DataPoint will have `source_user` equal to a UUID
  string, and the cross-SDK parity test fails on mismatched labels
  even though the algorithm is correct.
- The plumbing is mechanical: every site that already fetches a `User`
  passes its `email` along; sites that don't have a User pass `None`
  and the `user_label()` helper falls back to the UUID.

## 3. Pre-conditions

- [Task 05-05](05-pipeline-context-fields.md) is committed
  (`PipelineContext::user_email` exists; `user_label()` helper exists).
- Clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Inventory `PipelineContext` construction sites

Run:

```bash
grep -rn "cognee_core::PipelineContext\|core::PipelineContext\|core::TaskContextBuilder\|TaskContextBuilder::new\|pipeline_context(" \
  crates/lib/ crates/cli/ crates/cognify/ crates/ingestion/ \
  --include="*.rs" \
  | grep -v "/tests/" | grep -v "test-utils"
```

**Reality (verified 2026-05):** No production code constructs a `PipelineContext`
or calls `TaskContextBuilder::pipeline_context(...)`. The builder
method exists at
[`crates/core/src/task_context.rs:275`](../../crates/core/src/task_context.rs#L275)
but is never invoked outside tests. Lib API and CLI both bypass
`cognee_core::execute()` entirely and call the convenience function
[`cognify::cognify()`](../../crates/cognify/src/tasks.rs#L1729) directly
(this is the LIB-06 follow-up TODO at `crates/cognify/src/tasks.rs:1718`
and `crates/ingestion/src/pipeline.rs:772`).

So the actual call sites that need plumbing today are the ones that
invoke `cognify::cognify()`, where `user_str = user_id.to_string()` is
hand-derived at line 1769 of `tasks.rs`. Those callers are:

- [`crates/lib/src/api/remember.rs:354`](../../crates/lib/src/api/remember.rs#L354)
  — the `remember()` entry point.
- [`crates/lib/src/api/update.rs:91`](../../crates/lib/src/api/update.rs#L91)
  — the `update()` entry point.
- [`crates/cli/src/commands/cognify.rs:146`](../../crates/cli/src/commands/cognify.rs#L146).
- [`crates/cli/src/commands/add_and_cognify.rs:146`](../../crates/cli/src/commands/add_and_cognify.rs#L146).

Note: `crates/lib/src/api/cognify.rs` does **not** exist; the lib API
modules are named `remember.rs`, `recall.rs`, `update.rs`, `forget.rs`,
`improve.rs`, etc. (Python-style verbs, not pipeline names).

When `cognee_core::execute()` does become the canonical path (gap
LIB-06 follow-up), the builder-side plumbing in 4.3 below applies.
Until then, 4.5 (the convenience-function parameter) is the meaningful
work for this task.

### 4.2 Inventory who has access to `User.email`

Trace upward from each `PipelineContext` construction site to find the
nearest function that has either a `User` value or a `&DatabaseConnection`
that can fetch one.

Typical pattern in the lib API:

```rust
pub async fn cognify(
    args: CognifyArgs,
    user_id: Uuid,
    cm: &ComponentManager,
) -> Result<…, …> {
    // … existing code …
    let user_email = lookup_user_email(&cm.database, user_id).await.ok();
    let pipeline_ctx = PipelineContext {
        // … existing fields …
        user_email,
        provenance_visited: Arc::new(Mutex::new(HashSet::new())),
    };
    // … existing code …
}
```

**No new helper needed.** `crates/database/src/ops/user.rs` (singular)
already implements `UserDb::get_user(uuid) -> Result<Option<User>, _>`
and the returned `User` has an `email: String` field
([`crates/models/src/user.rs:13`](../../crates/models/src/user.rs#L13)).
Call it directly:

```rust
let user_email = match user_id {
    Some(uid) => database
        .get_user(uid)
        .await
        .ok()           // best-effort: log+swallow DB errors
        .flatten()      // Option<Option<User>> -> Option<User>
        .map(|u| u.email),
    None => None,
};
```

`UserDb` is implemented for `sea_orm::DatabaseConnection`, so any
caller already holding `Arc<DatabaseConnection>` can invoke it after
`use cognee_database::traits::UserDb;`.

### 4.3 (Future) Populate `user_email` at each `PipelineContext` construction site

> **Status today:** no such construction site exists. This subsection
> is the template for when `cognee_core::execute()` becomes the
> canonical path (LIB-06 follow-up). Skip in favour of §4.5 for the
> work that actually lands in this gap.

For each site identified in §4.1, replace:

```rust
// before
let pipeline_ctx = PipelineContext {
    // … existing fields …
};
```

with:

```rust
// after
let user_email = if let Some(uid) = user_id {
    cognee_database::ops::users::get_user_email(&database, uid)
        .await
        .ok()
        .flatten()
} else {
    None
};

let pipeline_ctx = PipelineContext {
    // … existing fields, with user_email and provenance_visited …
    user_email,
    provenance_visited: Arc::new(Mutex::new(HashSet::new())),
};
```

The `.ok().flatten()` pattern silently degrades to `None` when the
DB lookup fails — provenance stamping is best-effort, mirroring
Python's `try / except` around `_stamp_provenance`.

### 4.4 CLI default user — wire `user_email` even when there is none

Most CLI runs do not authenticate against a User row; the resolved
`owner_id` is the default user (see
[`crates/cli/src/commands/...`](../../crates/cli/src/commands/) for the
patterns that call `resolve_owner_id` or similar). For these runs:

- If the default user's email is known (some CLI bootstrap helpers
  insert a `default@cognee.local` row), pass it through.
- Otherwise leave `user_email = None` and rely on the `user_label()`
  fallback to `user_id.to_string()`.

Do **not** invent a synthetic email. Python has the same behaviour:
unauthenticated runs end up with `source_user` set to the user UUID.

### 4.5 Add a `user_email` parameter to the `cognify()` convenience

[`crates/cognify/src/tasks.rs::cognify`](../../crates/cognify/src/tasks.rs#L1729)
takes `user_id: Option<Uuid>`. Add a sibling parameter:

```rust
pub async fn cognify(
    data_items: Vec<Data>,
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    user_email: Option<String>,        // NEW
    tenant_id: Option<Uuid>,
    // …
) -> Result<CognifyResult, CognifyError> {
    // …
    let user_str = user_email
        .clone()
        .or_else(|| user_id.as_ref().map(|id| id.to_string()));
    let user_str_ref = user_str.as_deref();
    // … unchanged: stamp_provenance(... user_str_ref) …
}
```

This mirrors `user_label()` at the call site so both code paths
(executor-driven and convenience) attribute the same way.

Update the call sites of `cognify()` (verified 2026-05):

| File | Line | Notes |
|---|---|---|
| `crates/lib/src/api/remember.rs` | 354 | Pass looked-up email (has `database` Arc in scope). |
| `crates/lib/src/api/update.rs` | 91 | Pass looked-up email (has `db` Arc in scope). |
| `crates/cli/src/commands/cognify.rs` | 146 | Look up via `database.get_user(owner_id)`. |
| `crates/cli/src/commands/add_and_cognify.rs` | 146 | Same as above. |

Each gains the new `user_email` argument. All four sites already
have a `database: Arc<DatabaseConnection>` in scope, so the lookup is
free. Pass `None` only if the lookup fails or the user row is missing
(intentional — Python falls back to UUID for unauthenticated runs).

### 4.6 Update the visualisation patch-in fallback

[`crates/visualization/src/lib.rs:124-147`](../../crates/visualization/src/lib.rs#L124-L147)
already has a "patch missing `source_user`" code path (mirrors
Python's belt-and-braces). Confirm it still uses the same caller-supplied
`user_label` and does not need to change. Note the file in §6 only if a
change is required.

### 4.7 Update integration tests

Tests that build `PipelineContext` literally (the four sites named in
[task 05-05 §4.4](05-pipeline-context-fields.md#44-update-test-sites-that-construct-pipelinecontext-literally))
already have `user_email: None` from that earlier task. No changes
needed here.

API-level tests that look up a user (e.g.
[`crates/lib/tests/ingest_pipeline_tests.rs`](../../crates/lib/tests/ingest_pipeline_tests.rs))
may want to assert `source_user == user.email` after a run. This belongs
to [task 05-10](10-tests.md), not here.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Existing UserDb tests cover get_user; no new helper to test in
#    this task. (If a regression test is desired for the email-vs-id
#    fallback in cognify::cognify, it belongs to task 05-10.)

# 3. Every CLI subcommand still runs end-to-end on the host demo path.
#    (Manual smoke; not part of CI in this gap.)

# 4. Existing pipeline integration tests pass.
cargo test -p cognee-lib --test ingest_pipeline_tests
cargo test -p cognee-lib --test dataset_deletion

# 5. Clippy.
cargo clippy --all-targets -- -D warnings

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs#L1729) —
  add `user_email: Option<String>` parameter to `cognify()` (the
  convenience function); update `user_str` derivation at line 1769 to
  prefer `user_email` over `user_id.to_string()` (mirrors `user_label()`).
- [`crates/lib/src/api/remember.rs`](../../crates/lib/src/api/remember.rs#L354) —
  look up email via `database.get_user(owner_id)` and pass.
- [`crates/lib/src/api/update.rs`](../../crates/lib/src/api/update.rs#L91) —
  same.
- [`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs#L146) —
  same.
- [`crates/cli/src/commands/add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs#L146) —
  same.
- (Conditional) [`crates/visualization/src/lib.rs`](../../crates/visualization/src/lib.rs)
  if §4.6 finds it needs an update.

**Not modified:** `crates/database/src/ops/user.rs` already exposes
`UserDb::get_user(uuid) -> Option<User>` with `User.email`; no new
helper required.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| The DB lookup adds latency to every cognify run | Low — one SELECT-by-PK query per run, cached by SeaORM | Acceptable. If profiling shows it dominates, cache `user_email` on `ComponentManager`. |
| Adding a parameter to `cognify()` breaks downstream callers (Python / JS / capi bindings) | Medium — bindings cross-call this | Bindings call the lib API path, not `cognify::cognify` directly. Verify by greppping the binding crates; if a binding does call directly, update its FFI wrapper. |
| `get_user_email` returns `None` silently when the user row is missing, masking a config error | Medium | The tracing layer logs a warn when stamping with `None` user — the existing telemetry catches this without us adding more error paths. |
| `User` table schema differs between SQLite (test) and Postgres (prod) | None — SeaORM model is shared | n/a |

## 8. Out of scope

- Caching `User.email` lookups across pipeline runs.
- Adding `User.email` to the `pipeline_runs` lifecycle row (gap-08
  territory).
- Removing the `user_id` field from `PipelineContext`. We need both
  for now — the lib API populates `user_email` when known, the
  visualization fallback uses `user_id`.
- Renaming `user_id` to `user_uuid` for clarity. Out of scope.
