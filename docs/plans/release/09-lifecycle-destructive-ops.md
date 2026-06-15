# 09 — Fix destructive / silent lifecycle ops

> Wave 2 · Priority P0 · Track A · Release-blocking: yes · Effort: 0.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B6.1, B6.3 (+ related B6.4), [release-readiness-plan.md](../release-readiness-plan.md) T8.2

[← back to index](00-INDEX.md)

## Goal

Close two lifecycle correctness gaps that are either **silently dishonest** or
**more destructive than Python**:

1. **`prune_system(metadata=true)` is a silent no-op** — `PruneTarget::all()` advertises
   `metadata: true` but the relational DB is never dropped (just a warning). Make it
   honest: either implement the relational drop (Python parity), or stop advertising
   `metadata: true` so callers cannot believe a wipe happened when it did not.
2. **`forget()` and `update()` hardcode `DeleteMode::Hard`** while Python defaults to
   **soft** delete (and warns that hard is dangerous). Switch both to `Soft` to match
   Python's default destructiveness.

Plus a flagged related sub-item (B6.4): under soft delete, Rust skips the orphan-entity
sweep that Python performs — so after this change Rust soft-delete will leave orphans
that Python removes. This task documents and (recommended) fixes that follow-on.

## Background & why

These are *safety*/parity bugs, not features. `prune` and `forget` are the destructive
end of the lifecycle; the project's "drop-in replacement for Python cognee" promise
means a user who runs `cognee.forget(...)` against the Rust SDK must not silently lose
more (or different) data than against Python.

### Python vs Rust — current state

| Concern | Python | Rust (now) | Gap |
|---|---|---|---|
| `prune_system` metadata | drops the relational DB (`prune_system.py:81-83 → delete_database()`) | logs a warning, does nothing (`prune.rs:114-121`) | **B6.1** — silent no-op behind an honest-looking flag |
| `forget` delete mode | `mode="soft"` default (`datasets.py:147`) | `DeleteMode::Hard` (`forget.rs:166`) | **B6.3** — Rust over-destructive |
| `update` delete mode | `mode="soft"` default (`datasets.py:147` via `update.py:80`) | `DeleteMode::Hard` (`update.rs:78`) | **B6.3** |
| soft-delete orphan cleanup | soft delete still prunes orphan entities/types via graph traversal | orphan sweep gated to `Hard` only (`delete/src/lib.rs:524`) | **B6.4** — after the B6.3 fix, Rust soft leaves orphans Python removes |

## Prerequisites

```bash
git checkout main && git pull
git checkout -b task/09-lifecycle-destructive-ops
```

Read first:
- Rust: [crates/lib/src/api/prune.rs](../../../crates/lib/src/api/prune.rs) (whole file, ~158 lines).
- Rust: [crates/lib/src/api/forget.rs](../../../crates/lib/src/api/forget.rs) line ~164-167.
- Rust: [crates/lib/src/api/update.rs](../../../crates/lib/src/api/update.rs) line ~70-80.
- Rust: [crates/delete/src/lib.rs](../../../crates/delete/src/lib.rs) the `DeleteMode` enum + the orphan sweep at line ~516-533.
- Rust binding glue: [crates/bindings-common/src/ops/data.rs](../../../crates/bindings-common/src/ops/data.rs) `prune_system` (line ~235) — the `cognee_prune_system` caller that constructs `PruneTarget` and must be updated if its signature changes.
- Python: `/tmp/cognee-python/cognee/api/v1/prune/prune.py`,
  `/tmp/cognee-python/cognee/modules/data/deletion/prune_system.py` (line 81-83),
  `/tmp/cognee-python/cognee/api/v1/datasets/datasets.py` (`delete_data`, line 143-148),
  `/tmp/cognee-python/cognee/api/v1/forget/forget.py`,
  `/tmp/cognee-python/cognee/api/v1/update/update.py`,
  `/tmp/cognee-python/cognee/infrastructure/databases/relational/sqlalchemy/SqlAlchemyAdapter.py` (`delete_database`, line 574-607).

## Python reference

### prune metadata (B6.1)

`/tmp/cognee-python/cognee/modules/data/deletion/prune_system.py`

```python
async def prune_system(graph=True, vector=True, metadata=True, cache=True):
    ...
    if metadata:
        db_engine = get_relational_engine()
        await db_engine.delete_database()   # line ~81-83
    ...
```

`/tmp/cognee-python/cognee/api/v1/prune/prune.py` — the public entry defaults
`metadata=False`:
```python
async def prune_system(graph=True, vector=True, metadata=False, cache=True):
    await _prune_system(graph, vector, metadata, cache)
```

`delete_database()` for SQLite **physically removes the DB file**
(`SqlAlchemyAdapter.py:579-586`); for Postgres it `DROP TABLE IF EXISTS ... CASCADE`
over every reflected table (`:587-601`). Python does **not** recreate the schema in
`delete_database` — the engine recreates tables on the next initialization.

**Behavior to match (if implementing):** when `metadata=true`, drop the relational
metadata DB so a fresh schema is bootstrapped on next use. The public `prune` default
is `metadata=False`, which Rust already mirrors in `PruneTarget::default_system()`.

### forget / update delete mode (B6.3)

`/tmp/cognee-python/cognee/api/v1/datasets/datasets.py` — the shared `delete_data`:
```python
    async def delete_data(
        ...
        mode: str = "soft",  # mode is there for backwards compatibility. Don't use "hard", it is dangerous.
        delete_dataset_if_empty: bool = False,
        ...
```
Both `forget` (`forget.py`, via `datasets.delete_data`/`empty_dataset`/`delete_all`) and
`update` (`update.py:80`, calling `datasets.delete_data(...)`) inherit this **soft**
default. **Behavior to match:** Rust `forget`/`update` must use `DeleteMode::Soft`.

### soft-delete orphan cleanup (B6.4)

Python soft delete still removes orphaned graph entities/types — `forget.py` calls
`delete_dataset_nodes_and_edges` / `delete_data_nodes_and_edges`
(`/tmp/cognee-python/cognee/modules/graph/methods/`) regardless of mode. Rust gates its
orphan sweep to `DeleteMode::Hard` (`delete/src/lib.rs:524`), so after the B6.3 switch
to Soft, Rust will leave orphans Python removes.

## Files to change

| Path | Change |
|---|---|
| `crates/lib/src/api/prune.rs` | Implement metadata drop **or** make `all()` honest (pick one — see options). Update the field/struct doc comments. |
| `crates/bindings-common/src/ops/data.rs` | Only if Option A is chosen and `prune_system`'s signature gains a DB param — thread `svc.database` through. |
| `crates/lib/src/api/forget.rs` | `DeleteMode::Hard` → `DeleteMode::Soft`. |
| `crates/lib/src/api/update.rs` | `DeleteMode::Hard` → `DeleteMode::Soft`. |
| `crates/delete/src/lib.rs` | (B6.4, recommended) run the orphan sweep for Soft as well as Hard. |

---

## Part 1 — prune metadata (B6.1): choose an option

> **Recommendation: Option B (make `all()` honest) for 0.1.0.** It is the smaller,
> lower-risk change, it removes the silent-data-loss-illusion, and the public `prune`
> default is already `metadata=False`, so almost no caller relies on `metadata=true`.
> Implement Option A only if a reviewer wants full functional parity in 0.1.0.

### Option A — implement the relational drop (full Python parity)

Implementing requires a relational handle inside `prune_system`. Today the signature is:

```rust
pub async fn prune_system(
    target: &PruneTarget,
    graph_db: Option<&dyn GraphDBTrait>,
    vector_db: Option<&dyn VectorDB>,
    session_store: Option<&dyn SessionStore>,
) -> Result<PruneResult, ApiError>
```

Steps:

1. Add a metadata-drop capability to the relational layer if one does not already exist
   (re-grep first: `grep -rn "delete_database\|drop_all\|fn fresh" crates/database/src/`).
   For SQLite the simplest parity-faithful approach mirrors Python: drop every table via
   the SeaORM `Migrator` `down`+`up`, or remove the DB file. Prefer a
   `DatabaseConnection::reset_schema()` that runs `Migrator::fresh(&conn)` (drops all
   tables then re-applies the baseline migration) so the schema is immediately usable
   again — this is *more* convenient than Python (which lazily recreates) and stays
   cross-SDK safe because the resulting schema is identical.
2. Add a parameter to `prune_system` for the DB handle:
   ```rust
   pub async fn prune_system(
       target: &PruneTarget,
       graph_db: Option<&dyn GraphDBTrait>,
       vector_db: Option<&dyn VectorDB>,
       session_store: Option<&dyn SessionStore>,
       database: Option<&DatabaseConnection>,   // NEW
   ) -> Result<PruneResult, ApiError>
   ```
3. Replace the no-op block:
   ```rust
   if target.metadata {
       if let Some(db) = database {
           db.reset_schema().await?;            // drop+recreate (or delete file)
           result.metadata_pruned = true;
           info!("prune_system: relational metadata DB reset");
       } else {
           tracing::warn!("prune_system: metadata=true but no database provided; skipping");
       }
   }
   ```
4. Update **every caller**, notably `crates/bindings-common/src/ops/data.rs:261`
   (`cognee_prune_system(...)`), passing `Some(svc.database.as_ref())`. Grep for all
   callers: `grep -rn "prune_system(" crates/ capi/ js/ python/`.
5. Update the `PruneTarget::metadata` doc (`prune.rs:21`) to drop the
   "(not yet implemented)" note.

> Determinism note: dropping+recreating via `Migrator::fresh` must land on the **same
> baseline schema** that cross-SDK parity depends on — do not alter columns. See task
> 11 (migration baseline) for the schema source of truth.

### Option B — make `all()` honest (recommended for 0.1.0)

> **Existing test to update:** `prune_target_all_enables_everything` (prune.rs:150)
> currently asserts `assert!(target.metadata)`. When Option B is implemented, rename this
> test to `prune_target_all_does_not_advertise_metadata` and flip the assertion to
> `assert!(!target.metadata)`. This is an in-place update, not a new test.

1. Remove `metadata: true` from `PruneTarget::all()` so it no longer advertises a wipe
   that does not happen, and rename/relabel for clarity.

   **Before** (`prune.rs:37-46`):
   ```rust
       /// All backends.
       pub fn all() -> Self {
           Self {
               graph: true,
               vector: true,
               metadata: true,
               cache: true,
           }
       }
   ```
   **After:**
   ```rust
       /// All backends Rust currently supports wiping (graph, vector, cache).
       ///
       /// `metadata` is intentionally `false`: dropping the relational DB is not yet
       /// implemented (Python's `prune_system(metadata=True)` drops it; the Rust
       /// public `prune` default is `metadata=False`, which this matches). Tracked as
       /// audit B6.1 / task 09 Option A.
       pub fn all() -> Self {
           Self {
               graph: true,
               vector: true,
               metadata: false,
               cache: true,
           }
       }
   ```

2. Make the no-op **loud and truthful** so a caller who explicitly sets `metadata: true`
   by hand learns it did nothing. Change the warning block in `prune_system` to also
   return a flag the caller can inspect, and never set `result.metadata_pruned = true`:

   **Before** (`prune.rs:114-121`):
   ```rust
   if target.metadata {
       // Deferred -- dropping and recreating the DB is complex and rarely
       // needed (Python also defaults metadata=False).
       tracing::warn!(
           "prune_system: metadata pruning is not yet implemented; \
            the relational database was NOT dropped"
       );
   }
   ```
   **After:**
   ```rust
   if target.metadata {
       // NOT IMPLEMENTED: dropping the relational DB is deferred (audit B6.1 /
       // task 09 Option A). We intentionally do NOT set result.metadata_pruned,
       // so the returned PruneResult truthfully reports metadata_pruned=false
       // even when a caller forced target.metadata=true by hand.
       tracing::warn!(
           "prune_system: metadata pruning is NOT implemented; the relational \
            database was NOT dropped (result.metadata_pruned stays false)"
       );
   }
   ```

3. Update the `PruneTarget::metadata` field doc (`prune.rs:21`) to be explicit:
   `/// Drop the relational metadata database. NOT IMPLEMENTED (audit B6.1) — setting
   this has no effect and result.metadata_pruned stays false.`

---

## Part 2 — forget / update soft delete (B6.3)

5. **`forget.rs`** — switch the mode.
   **Before** (`forget.rs:164-167`):
   ```rust
       let request = DeleteRequest {
           scope,
           mode: DeleteMode::Hard,
       };
   ```
   **After:**
   ```rust
       let request = DeleteRequest {
           scope,
           // Python `datasets.delete_data` defaults mode="soft" and warns hard is
           // dangerous (datasets.py:147). Match the safer default.
           mode: DeleteMode::Soft,
       };
   ```

6. **`update.rs`** — switch the mode.
   **Before** (`update.rs:71-79`):
   ```rust
       let delete_request = DeleteRequest {
           scope: DeleteScope::Data {
               owner_id,
               data_id,
               dataset_name: Some(dataset_name.to_string()),
               delete_dataset_if_empty: false,
           },
           mode: DeleteMode::Hard,
       };
   ```
   **After:**
   ```rust
       let delete_request = DeleteRequest {
           scope: DeleteScope::Data {
               owner_id,
               data_id,
               dataset_name: Some(dataset_name.to_string()),
               delete_dataset_if_empty: false,
           },
           // Python update() → datasets.delete_data defaults mode="soft" (datasets.py:147).
           mode: DeleteMode::Soft,
       };
   ```

---

## Part 3 — soft-delete orphan cleanup (B6.4, deferred for strict parity)

> **Decision (0.1.0): defer, keep the sweep Hard-gated.** The premise below
> ("run the sweep under Soft for parity") is *incorrect* about Python: Python's
> degree-one Entity/EntityType sweep is itself `if mode == "hard"`
> (`legacy_delete.py`) and the production soft path calls `legacy_delete(data,
> "soft")`, so it never fires. Running the global degree sweep on Soft would make
> Rust soft-delete *more* destructive than Python. The genuine Python soft-path
> cleanup is provenance/slug-scoped (`delete_from_graph_and_vector` excludes
> co-owned slugs), not a global degree heuristic — closing that gap needs a
> deletion-scoped sweep, tracked via the `TODO(B6.4)` in `delete/src/lib.rs`.

> Original (superseded) flag: after Part 2, Rust soft-delete will leave orphan
> entities/types that Python removes. To stay parity-correct, run the orphan sweep
> under Soft as well.

7. In `crates/delete/src/lib.rs`, broaden the sweep gate.
   **Before** (line ~524):
   ```rust
       if matches!(request.mode, DeleteMode::Hard) {
           let (oe, oet, sweep_warnings) = self.sweep_orphan_nodes().await?;
           ...
       }
   ```
   **After:**
   ```rust
       // Python removes orphaned entities/types on soft delete too (it traverses
       // graph nodes/edges regardless of mode — see forget.py →
       // delete_{dataset,data}_nodes_and_edges). Sweep on both modes for parity.
       // audit B6.4.
       {
           let (oe, oet, sweep_warnings) = self.sweep_orphan_nodes().await?;
           ...
       }
   ```
   Remove the now-unused `DeleteMode` match if nothing else uses it in this scope, and
   re-check the existing delete tests for any that assert "soft leaves orphans" — update
   them to the new parity behavior.

   > If you defer Part 3, add a `// TODO(B6.4): soft delete should sweep orphans like
   > Python` next to the gate and record it as a tracked issue, but do not silently
   > leave the divergence undocumented.

## Verification

```bash
# 1. Confirm Python defaults (source of truth).
grep -n 'mode: str = "soft"' /tmp/cognee-python/cognee/api/v1/datasets/datasets.py
grep -n "delete_database()" /tmp/cognee-python/cognee/modules/data/deletion/prune_system.py

# 2. No remaining DeleteMode::Hard in forget/update.
grep -n "DeleteMode::Hard" crates/lib/src/api/forget.rs crates/lib/src/api/update.rs
# Expected: no matches.

# 3. Option B: all() no longer advertises metadata.
grep -n "metadata: true" crates/lib/src/api/prune.rs
# Expected (Option B): no matches.

# 4. Build + test the touched crates.
cargo test -p cognee-lib
cargo test -p cognee-delete
cargo test -p cognee-bindings-common   # prune_system glue, if Option A changed the signature

# 5. Gate.
scripts/check_all.sh
```

### Tests to add

- In `crates/lib/src/api/prune.rs` tests:
  - **Option B:** `prune_target_all_does_not_advertise_metadata` — assert
    `!PruneTarget::all().metadata`.
  - Both options: a test asserting that after `prune_system` with `metadata=true` but no
    metadata implementation (Option B), `result.metadata_pruned == false`.
- In `crates/lib/src/api/forget.rs` / `update.rs`: a test (or doc assertion) verifying
  the constructed `DeleteRequest.mode == DeleteMode::Soft`. If the request is built
  inline and not easily extractable, refactor the request construction into a small
  `fn build_delete_request(...) -> DeleteRequest` and unit-test that.
- In `crates/delete/src/lib.rs` (Part 3): a test that a **soft** delete of a dataset
  whose entities become orphaned removes those orphan entities (mirrors Python). Use
  `MockGraphDB` / in-memory SQLite per the project test patterns.

## Acceptance criteria

- [ ] `forget()` and `update()` use `DeleteMode::Soft`; no `DeleteMode::Hard` remains in
      either file.
- [ ] prune metadata is **honest**: either implemented (Option A — `result.metadata_pruned`
      is `true` only when the DB was actually reset) or `PruneTarget::all().metadata == false`
      and `result.metadata_pruned` stays `false` with a loud warning (Option B).
- [ ] `PruneTarget::metadata` field doc accurately states current behavior.
- [ ] B6.4 either fixed (orphan sweep runs on Soft) or explicitly TODO-flagged + tracked.
- [ ] New tests pass; `cargo test -p cognee-lib -p cognee-delete` green; `scripts/check_all.sh` passes.

## Gotchas / do-not

- **Do not** change `PruneTarget::default_system()` — it already matches Python
  (`metadata: false`). Only `all()` is dishonest.
- **Option A determinism:** a metadata reset must reproduce the *exact* baseline schema
  (see task 11). Never drop+recreate with a different column set — cross-SDK DB reads
  depend on byte-identical schema. Prefer `Migrator::fresh` over hand-written DDL.
- **Soft vs hard semantics:** confirm what `DeleteMode::Soft` actually does in
  `crates/delete/src/lib.rs` before assuming it mirrors Python's soft (Python soft marks
  data deleted but still cleans graph/vector + orphans). If Rust's `Soft` currently
  skips graph/vector cleanup, that is a deeper divergence — note it; the minimum for
  this task is matching the *default mode*, with B6.4 covering orphan parity.
- **Binding wire contract:** the prune result JSON keys (`metadataPruned`, etc.) are a
  cross-SDK contract — do not rename them. Only their *values* change.
- **Telemetry:** `forget` emits `cognee.forget` telemetry; do not remove or alter that
  while changing the delete mode.

## Rollback

```bash
git checkout main -- \
  crates/lib/src/api/prune.rs \
  crates/lib/src/api/forget.rs \
  crates/lib/src/api/update.rs \
  crates/delete/src/lib.rs \
  crates/bindings-common/src/ops/data.rs
```
or drop the branch. Option A touches more call sites; if reverting partially, revert
the signature change to `prune_system` and all its callers together.
