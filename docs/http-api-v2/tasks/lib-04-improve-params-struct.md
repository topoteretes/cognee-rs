# LIB-04 — Refactor `cognee_lib::api::improve::improve()` to `ImproveParams` struct

| | |
|---|---|
| Scope | Mechanical refactor — replace 18 positional parameters with a single `ImproveParams` struct. |
| Status | **Done (commit 9f1879e)** — `ImproveParams<'a>` struct lands in `crates/lib/src/api/improve.rs` with all 18 fields, no `Default` derive (option (b)), and preserved borrow lifetimes on `cognify_config` and `add_pipeline`. All 5 call sites migrated (1 in `remember.rs::self_improvement` + 2 in `improve_e2e.rs` + 2 in `improve_sync_only.rs`); `#[allow(clippy::too_many_arguments)]` removed; `dataset_name` shifted from `&str` to `String` with `&dataset_name` borrows at use sites. Wire shape unchanged — pure-Rust refactor. |
| Blocks | LIB-01 (remember.rs:455 call site), E-05 (HTTP handler call site). |
| Depends on | none. |
| Effort | ~0.5 day. |
| Owner crate | `cognee-lib` |

> **Decision (2026-04-29) — Decision 8**: this task is the dedicated home for the `improve()` signature refactor. Splitting it from E-05 keeps that task scoped to "DTO + handler" and lets the four-agent pipeline review the refactor independently. Investigation agent: do not re-litigate.

## 1. Goal

`cognee_lib::api::improve::improve()` currently has **18 positional parameters** ([`crates/lib/src/api/improve.rs:55-74`](../../../crates/lib/src/api/improve.rs#L55-L74)). E-05 will add 3 more for v2 parity (`extraction_tasks`, `enrichment_tasks`, `data`), bringing the total to 21. Replace the positional list with an `ImproveParams` struct so:

- Adding new fields is non-breaking (callers ignoring them keep working with `..Default::default()`).
- Diff readability — call sites become field-name → value pairs instead of position-keyed.
- E-05 lands as a small "two new fields" diff instead of a 20-arg signature change buried in the HTTP work.

## 2. Current Rust state

Current signature ([`crates/lib/src/api/improve.rs:55`](../../../crates/lib/src/api/improve.rs#L55)):

```rust
pub async fn improve(
    dataset_name: &str,
    session_ids: Option<Vec<String>>,
    node_name: Option<Vec<String>>,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    feedback_alpha: f64,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    add_pipeline: Option<&AddPipeline>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    cognify_config: &CognifyConfig,
) -> Result<ImproveResult, ApiError> { ... }
```

Five call sites (`grep -rn "\bimprove(" crates/`):

| Site | File | Notes |
|---|---|---|
| 1 | `crates/lib/tests/improve_sync_only.rs:103` | test |
| 2 | `crates/lib/tests/improve_sync_only.rs:145` | test |
| 3 | `crates/lib/tests/improve_e2e.rs:88` | test |
| 4 | `crates/lib/tests/improve_e2e.rs:124` | test |
| 5 | `crates/lib/src/api/remember.rs:497` | inside `remember()` self-improvement path — also touched by LIB-01 |

`crates/cloud/src/cloud_client.rs` calls a *different* `improve()` (the HTTP proxy method) — out of scope for this task.

## 3. Implementation steps

1. **Define `ImproveParams`** in `crates/lib/src/api/improve.rs`:
   ```rust
   /// Parameters for [`improve`].
   ///
   /// Default values match the pre-refactor positional defaults.
   #[derive(Default)]
   pub struct ImproveParams<'a> {
       pub dataset_name: String,                 // was: &str — owned for ergonomics; callers borrow if hot-path
       pub session_ids: Option<Vec<String>>,
       pub node_name: Option<Vec<String>>,
       pub owner_id: Uuid,                       // required, no Default-able value — see step 2
       pub tenant_id: Option<Uuid>,
       pub feedback_alpha: f64,                  // default 0.3 (current Python parity value — confirm before landing)

       pub llm: Arc<dyn Llm>,
       pub storage: Arc<dyn StorageTrait>,
       pub graph_db: Arc<dyn GraphDBTrait>,
       pub vector_db: Arc<dyn VectorDB>,
       pub embedding_engine: Arc<dyn EmbeddingEngine>,
       pub ontology_resolver: Arc<dyn OntologyResolver>,

       pub db: Option<Arc<DatabaseConnection>>,
       pub session_store: Option<Arc<dyn SessionStore>>,
       pub session_manager: Option<Arc<SessionManager>>,
       pub add_pipeline: Option<&'a AddPipeline>,
       pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,

       pub cognify_config: &'a CognifyConfig,    // borrow — large struct, never owned
   }
   ```

2. **Required vs optional fields.** `Default` cannot be auto-derived because several fields (`owner_id`, the `Arc<dyn ...>` engine handles, `cognify_config`) have no sensible default. Two patterns to choose from:
   - (a) Hand-write `Default` that panics for required fields with a helpful message. Discourages misuse but lets `..Default::default()` work for tests.
   - (b) Drop `#[derive(Default)]` and require all fields at construction. Safer, but tests have to spell out every Arc handle.
   - **Pick (b)** — `cognee-lib` is internal API; safer to force callers to think about every dependency. (a) is a footgun if a future field is added with a panic-on-default and someone misses the `..Default::default()` migration.

3. **New signature**:
   ```rust
   pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError>;
   ```
   Function body keeps its existing destructuring at the top:
   ```rust
   let ImproveParams {
       dataset_name, session_ids, node_name, owner_id, tenant_id, feedback_alpha,
       llm, storage, graph_db, vector_db, embedding_engine, ontology_resolver,
       db, session_store, session_manager, add_pipeline, checkpoint_store, cognify_config,
   } = params;
   ```
   No behavior change.

4. **Migrate the 5 call sites.** Each becomes:
   ```rust
   improve(ImproveParams {
       dataset_name: ...,
       session_ids: ...,
       // ...all 17 fields named...
   }).await
   ```
   - The 4 test sites are mechanical translations.
   - `crates/lib/src/api/remember.rs:497` — same translation. **Coordination note**: LIB-01 also modifies this file; both tasks must be aware. Land LIB-04 first (per §0 phase order — B-4 before B-5) so LIB-01's edit to `remember.rs:497` directly produces the `ImproveParams { ... }` shape.

5. **Update doc comments** on `improve()` to reference the new struct. Move the per-parameter docs from the old positional comments onto the struct fields.

6. **No public-API exports change.** `cognee_lib::api::improve::{improve, ImproveParams, ImproveResult}` are all already accessible via the existing module; just add `ImproveParams` to the `pub use` re-export list at the top of `lib.rs` if there is one.

## 4. Tests

- **No new tests** — the existing 4 test functions in `improve_e2e.rs` and `improve_sync_only.rs` are migrated to the new shape; their assertions don't change. The fact that they still pass is the regression test.
- **Compile check**: `cargo check --all-targets` must pass after each call site is migrated. Don't migrate them in one shot — do them one-by-one and verify between each.
- **Behavioral check**: `cargo test -p cognee-lib --test improve_e2e --test improve_sync_only` must pass with identical output before/after.

## 5. Acceptance criteria

- [x] `ImproveParams<'_>` struct defined in `crates/lib/src/api/improve.rs` with all 18 (existing) fields.
- [x] `improve()` signature is `pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError>`.
- [x] All 5 call sites migrated.
- [x] `cargo test -p cognee-lib --test improve_e2e --test improve_sync_only` shows 4/4 passing — zero behavioral change.
- [x] `scripts/check_all.sh` passes (Rust gates green; pre-existing JS jest `node:path` issue safe to ignore per IMPLEMENTATION-PROMPT.md §0).
- [x] No new `unwrap()` introduced; no panic-on-Default behavior (option (b) — required-fields construction).

## 6. References

- [Rust `improve()` source](../../../crates/lib/src/api/improve.rs)
- [LIB-01 — `remember_entry()` facade](lib-01-remember-entry-facade.md) (touches one of the 5 call sites)
- [E-05 — `POST /improve` DTO additions](e-05-improve.md) (consumer; adds 3 new fields after this refactor lands)
- [Decision 8 (in [`README §1.1`](../README.md))](../README.md)
