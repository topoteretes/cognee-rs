# Implementation Plan: `forget()` (polish)

**Gap doc:** [../forget.md](../forget.md)  
**Python reference:** `cognee/api/v1/forget/forget.py`  
**Rust entry point:** `crates/lib/src/api/forget.rs`

---

## 1. Goal & Scope

The `forget()` API is **already implemented** end-to-end in Rust
(`crates/lib/src/api/forget.rs` + `crates/delete/src/lib.rs`). Three
deletion modes (item / dataset / everything), ACL enforcement, dry-run
preview, session-cache prune for `everything`, and 5-phase cascade
cleanup are all production-grade.

This plan covers **only** the two minor cosmetic gaps listed in
`docs/api-v2/forget.md`:

1. **UUID support in `ForgetTarget::Dataset`** — Python's `forget()` accepts
   `dataset: Union[str, UUID]`. The Rust enum currently accepts only a
   `dataset_name: String`. Add a UUID variant (or make the field a
   `DatasetRef` enum).
2. **External telemetry event export** — Python calls
   `send_telemetry("cognee.forget", ...)` (external event log) in addition
   to the OTEL span. Rust currently emits only the `tracing` span via
   `#[tracing::instrument]`. Add a telemetry event hook, gated on a new
   `telemetry` feature flag.

**Explicitly out of scope (see §6).**

---

## 2. Design Overview

### 2.1 `ForgetTarget::Dataset` — accept UUID or name

Change `ForgetTarget::Dataset { dataset_name: String }` to carry a small
enum `DatasetRef` that can be either a UUID or a name. The dispatcher in
`forget()` resolves the UUID path (skipping the name-lookup branch) and
feeds `DeleteScope::Dataset` — but `DeleteScope::Dataset` only accepts
`dataset_name: String` today (see `crates/delete/src/lib.rs:63-65`). So
we also need a way to dispatch a UUID down to the delete service.

Two viable shapes:

**Option A (minimal, recommended):** Resolve UUID → name inside
`forget()` by reverse-lookup on `IngestDb` (`get_dataset_by_id`), same
validation flow already used for the name path. Requires `db: Some(..)`
when a UUID is supplied; returns `ApiError::InvalidArgument` otherwise.
Zero change to `DeleteScope`.

**Option B:** Extend `DeleteScope::Dataset` to accept
`dataset_id: Option<Uuid>`. More invasive — affects `DeleteService`,
`AuthorizedDeleteService`, preview, tests. Not justified for an
"API convenience" polish.

**Choose Option A.** Ship a thin adapter in `forget.rs`.

Public enum shape:

```rust
#[derive(Debug, Clone)]
pub enum DatasetRef {
    Name(String),
    Id(Uuid),
}

#[derive(Debug, Clone)]
pub enum ForgetTarget {
    Item { data_id: Uuid, dataset: DatasetRef },
    Dataset { dataset: DatasetRef },
    All,
}
```

This matches Python's `Union[str, UUID]` API semantics. Existing
callers constructing `ForgetTarget::Item { data_id, dataset_name: "x" }`
must migrate to `dataset: DatasetRef::Name("x".into())`. That's a
breaking signature change scoped to one crate, so update `cognee-cli`
(`crates/cli/src/commands/delete.rs`) and any doctest sites in the same
PR.

### 2.2 Telemetry export integration

Python's `send_telemetry("cognee.forget", user_or_sdk, {target,
dataset, data_id, cognee_version})` is an **event** log distinct from
the OTEL span. In Rust we have:

- `#[tracing::instrument]` spans already wired on `DeleteService::preview`
  and `::execute` (`crates/delete/src/lib.rs:176-183` and 242-250).
- Constant attribute keys in `crates/search/src/observability.rs:38-48`
  (`COGNEE_FORGET_TARGET`, `COGNEE_DATASET_NAME`, `COGNEE_RESULT_COUNT`).
- A `telemetry` feature flag in `crates/core/Cargo.toml:7` used by the
  pipeline executor to gate per-task spans.

The API layer (`crates/lib/src/api/forget.rs`) is the right place to
emit the **event** — it matches Python's placement. Emit via
`tracing::info!( target: "cognee.telemetry", event = "cognee.forget",
target = %label, dataset = ?dataset, data_id = ?data_id, cognee_version
= env!("CARGO_PKG_VERSION"))`. No new dependency; consumers that wire
an OTEL log exporter or a `tracing_subscriber::Layer` listening on the
`cognee.telemetry` target will capture it. Gate behind
`#[cfg(feature = "telemetry")]` and thread the feature flag through
`cognee-lib`'s `Cargo.toml` (non-default, opt-in).

---

## 3. Step-by-Step Implementation

### Step 1 — Add `DatasetRef` and rework `ForgetTarget`
**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/forget.rs`  
**Lines touched:** 14-23 (enum), 50-86 (dispatch), 101-130 (tests)

Sketch:

```rust
#[derive(Debug, Clone)]
pub enum DatasetRef {
    Name(String),
    Id(Uuid),
}

impl DatasetRef {
    async fn to_name(
        &self,
        owner_id: Uuid,
        db: Option<&dyn IngestDb>,
    ) -> Result<String, ApiError> {
        match self {
            DatasetRef::Name(n) => Ok(n.clone()),
            DatasetRef::Id(id) => {
                let db = db.ok_or_else(|| ApiError::InvalidArgument(
                    "db connection required to resolve dataset UUID".into()
                ))?;
                db.get_dataset_by_id(*id, owner_id)
                    .await
                    .map_err(|e| ApiError::InvalidArgument(
                        format!("Dataset {id} not found: {e}")))?
                    .map(|d| d.name)
                    .ok_or_else(|| ApiError::InvalidArgument(
                        format!("Dataset {id} not found")))
            }
        }
    }
}

pub enum ForgetTarget {
    Item { data_id: Uuid, dataset: DatasetRef },
    Dataset { dataset: DatasetRef },
    All,
}
```

In the `forget()` body, replace the two `dataset_name` match arms with
`let dataset_name = dataset.to_name(owner_id, db).await?;` before
constructing the `DeleteScope`.

**Note:** `IngestDb::get_dataset_by_id` may not exist; confirm against
`crates/database/src/lib.rs`. If missing, add it as part of this step.

### Step 2 — Emit external telemetry event
**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/forget.rs`  
**Location:** top of `pub async fn forget(...)` body, after computing
the label string (around what is currently line 50).

Sketch:

```rust
#[cfg(feature = "telemetry")]
{
    let (target_label, dataset_dbg, data_id_dbg) = match &target {
        ForgetTarget::Item { data_id, dataset } =>
            ("data_item", format!("{dataset:?}"), data_id.to_string()),
        ForgetTarget::Dataset { dataset } =>
            ("dataset", format!("{dataset:?}"), String::new()),
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

### Step 3 — Feature flag propagation
**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/lib/Cargo.toml`

Add:

```toml
[features]
telemetry = []
```

Per CLAUDE.md §"Architecture Patterns → Feature strategy", also add
`telemetry` to `cognee-lib`'s and `cognee-cli`'s `default` lists if we
want CI to exercise it — but telemetry export is arguably opt-in.
Recommend **non-default** here.

### Step 4 — Update call sites for the new enum shape
**Files:**
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/commands/delete.rs`  
  Convert `ForgetTarget::Item { ..., dataset_name }` and
  `ForgetTarget::Dataset { dataset_name }` → `DatasetRef::Name(...)`.
  Add a `--dataset-id <uuid>` CLI flag (mutually exclusive with
  `--dataset-name`) that produces `DatasetRef::Id(uuid)`.
- Any doctest / example in `crates/lib/src/api/forget.rs` docstring.

### Step 5 — Re-export `DatasetRef`
**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/lib.rs:117`

Add `DatasetRef` to the existing pub-use list alongside `ForgetTarget`
and `ForgetResult`.

---

## 4. Test Plan

All tests go in `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/forget.rs`
under `mod tests`, or in `/home/dmytro/dev/cognee/cognee-rust/crates/lib/tests/dataset_deletion.rs`
(existing integration test file) when a real `DeleteService` is needed.

Unit tests (synchronous, in-module):
1. **`dataset_ref_name_passthrough`** — `DatasetRef::Name("x").to_name(..)` returns `"x"` with `db=None` (no DB lookup).
2. **`dataset_ref_id_requires_db`** — `DatasetRef::Id(uuid).to_name(_, None)` returns `ApiError::InvalidArgument`.
3. **`forget_target_item_uuid_variant`** — construct `ForgetTarget::Item { data_id, dataset: DatasetRef::Id(..) }`, verify debug format.

Integration tests (in `crates/lib/tests/dataset_deletion.rs`):
4. **`forget_by_dataset_uuid_succeeds`** — seed a dataset, call `forget(ForgetTarget::Dataset { dataset: DatasetRef::Id(ds.id) }, owner, &svc, Some(&db)).await`, assert deletion.
5. **`forget_by_dataset_uuid_missing_returns_err`** — random UUID → `ApiError::InvalidArgument`.

Telemetry tests — behind `#[cfg(feature = "telemetry")]`:
6. **`forget_emits_telemetry_event`** — install a `tracing_subscriber::fmt::test::TestSubscriber` filtered to `cognee.telemetry`, call `forget(ForgetTarget::All, ..)`, assert the event was recorded with field `event = "cognee.forget"` and `forget_target = "everything"`.

No changes needed to existing tests `delete_all_prunes_session_store` (crates/delete/src/lib.rs:3639) or the authorization/orphan-sweep/error-paths integration tests under `crates/delete/tests/` — both gaps are confined to the `cognee-lib` API wrapper.

---

## 5. Effort Breakdown

| Step | Hours |
|------|-------|
| 1. `DatasetRef` enum + `to_name()` + `forget()` dispatch rework (incl. possible `get_dataset_by_id` trait method) | 1.5 |
| 2. Telemetry event emission in `forget()` | 0.5 |
| 3. Feature-flag plumbing in `cognee-lib/Cargo.toml` | 0.25 |
| 4. CLI call-site updates + `--dataset-id` flag | 1.0 |
| 5. Re-exports in `cognee-lib` prelude | 0.1 |
| 6. Tests (5 new + 1 telemetry) | 1.5 |
| **Total** | **~5 h** |

Fits comfortably in a single afternoon; zero cross-crate coupling besides the CLI signature bump.

---

## 6. Out of Scope

The gap doc lists several items explicitly marked *not* to be done:

- **Session cleanup for Dataset/Data modes** (`docs/api-v2/forget.md` §4, "Critical Gap"). The doc closes with "This is NOT a bug; it matches Python's design limitation. No action needed." Skipped.
- **Default user auto-resolution** (§4, "Low Gap"). Marked "By design"; `owner_id: Uuid` stays a required explicit argument. Skipped.
- **Session-tagging-by-dataset** (future enhancement, would enable targeted session pruning). Not touched.
- **OTEL span enrichment** — the existing `#[tracing::instrument]` attrs on `DeleteService::preview`/`execute` already record `cognee.forget.target`, `cognee.operation.mode`, and `cognee.result.count`; no additional span attributes needed.
- **`DeleteScope::Dataset` accepting `Uuid`** (Option B in §2.1) — deliberately kept to API-layer adaptation.

---

## Critical Files for Implementation
- /home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/forget.rs
- /home/dmytro/dev/cognee/cognee-rust/crates/lib/Cargo.toml
- /home/dmytro/dev/cognee/cognee-rust/crates/cli/src/commands/delete.rs
- /home/dmytro/dev/cognee/cognee-rust/crates/lib/src/lib.rs
- /home/dmytro/dev/cognee/cognee-rust/crates/lib/tests/dataset_deletion.rs
