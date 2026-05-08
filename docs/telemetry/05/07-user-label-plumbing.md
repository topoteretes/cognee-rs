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
grep -rn "cognee_core::PipelineContext\|core::PipelineContext\|core::TaskContextBuilder\|TaskContextBuilder::new" \
  crates/lib/ crates/cli/ crates/cognify/ crates/ingestion/ \
  --include="*.rs" \
  | grep -v "/tests/" | grep -v "test-utils"
```

The candidate set today (before the gap lands):

- [`crates/lib/src/api/...`](../../crates/lib/src/api/) — public
  cognify / memify / add / search / forget / improve entry points.
  These call `cognee_core::execute()` (or the convenience `cognify()`
  in `crates/cognify/src/tasks.rs`) with a `TaskContext` they build.
- [`crates/cli/src/commands/...`](../../crates/cli/src/commands/) —
  CLI subcommands that build a `ComponentManager` and call into the
  lib. These do **not** typically construct `PipelineContext` directly;
  they pass `user_id` and rely on the lib API to assemble the context.
- [`crates/cognify/src/tasks.rs::cognify`](../../crates/cognify/src/tasks.rs#L1729)
  and [`build_cognify_pipeline`](../../crates/cognify/src/tasks.rs#L2709)
  — sometimes called with a separately-built `TaskContext`.

If sub-agent A finds new construction sites, update this list.

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

If the lib API does not currently have a `lookup_user_email` helper,
add a thin wrapper in
[`crates/database/src/ops/users.rs`](../../crates/database/src/ops/users.rs)
(or wherever the user table ops live):

```rust
pub async fn get_user_email(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Option<String>, OpsError> {
    use crate::entities::user::Entity as UserEntity;

    UserEntity::find_by_id(user_id)
        .one(db.connection())
        .await
        .map(|opt| opt.map(|u| u.email))
        .map_err(OpsError::from)
}
```

The function returns `Option<String>` — `None` is "no user row found"
(legitimate for unauthenticated CLI runs), `Some(email)` is the value
to stamp.

### 4.3 Populate `user_email` at each construction site

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

Update the call sites of `cognify()`:

```bash
grep -rn "cognify::cognify\|tasks::cognify\|^[^/]*cognify(" \
  crates/lib/src/ crates/cli/src/ \
  --include="*.rs"
```

Each gains the new `user_email` argument. CLI sites pass `None`
unless they look up the email; lib API sites pass the looked-up email.

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

# 2. The new ops helper has a unit test that round-trips a user
#    insert and email lookup against an in-memory SQLite.
cargo test -p cognee-database get_user_email

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

- [`crates/database/src/ops/users.rs`](../../crates/database/src/ops/users.rs)
  (or the closest existing user-ops file) — add `get_user_email`.
- [`crates/lib/src/api/cognify.rs`](../../crates/lib/src/api/cognify.rs)
  (and any sibling `add.rs`, `memify.rs`, `search.rs`) — populate
  `user_email`.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) —
  add `user_email: Option<String>` parameter to `cognify()` (the
  convenience function) and propagate at the stamp call sites.
- All call sites of `cognify::cognify(...)` — pass the new argument.
- (Conditional) [`crates/visualization/src/lib.rs`](../../crates/visualization/src/lib.rs)
  if §4.6 finds it needs an update.

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
