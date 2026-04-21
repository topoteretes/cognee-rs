# Delete Operation — Rust vs Python Gap Analysis

This document tracks all identified gaps between the Rust SDK and the Python cognee
implementation for the delete operation. Each gap links to a detailed investigation
document with a step-by-step implementation plan.

## Gap Index

| # | Gap | Priority | Steps | Detail Doc | Status |
|---|-----|----------|-------|------------|--------|
| 1 | [Graph/vector cleanup is CLI-only, not in DeleteService](#1-graphvector-cleanup-is-cli-only-not-in-deleteservice) | P0 | 12 | [gap-01](gap-01-move-cleanup-to-service.md) | Done |
| 2 | [No `nodes`/`edges` table cleanup for data-scoped deletion](#2-no-nodesedges-table-cleanup-for-data-scoped-deletion) | P0 | 9 | [gap-02](gap-02-nodes-edges-cleanup.md) | Done |
| 3 | [Missing vector collections in `DeleteScope::All`](#3-missing-vector-collections-in-deletescopeall) | P0 | 7 | [gap-03](gap-03-missing-vector-collections.md) | Done |
| 4 | [No ACL-based authorization](#4-no-acl-based-authorization) | P1 | 10 | [gap-04](gap-04-acl-authorization.md) | Done |
| 5 | [`Hard` delete mode is unimplemented](#5-hard-delete-mode-is-unimplemented) | P1 | 11 | [gap-05](gap-05-hard-delete-mode.md) | Done |
| 6 | [No `pipeline_runs` cleanup](#6-no-pipeline_runs-cleanup) | P2 | 9 | [gap-06](gap-06-pipeline-runs-cleanup.md) | Done |
| 7 | [No `search_history` (queries/results) cleanup](#7-no-search_history-queriesresults-cleanup) | P2 | 10 | [gap-07](gap-07-search-history-cleanup.md) | Done |
| 8 | [No session cache deletion](#8-no-session-cache-deletion) | P2 | 8 | [gap-08](gap-08-session-cache-deletion.md) | Done |
| 9 | [No orphaned EdgeType node cleanup](#9-no-orphaned-edgetype-node-cleanup) | P2 | 9 | [gap-09](gap-09-orphaned-edgetype-cleanup.md) | Done |
| 10 | [Missing `delete_dataset_if_empty` option](#10-missing-delete_dataset_if_empty-option) | P2 | 13 | [gap-10](gap-10-delete-dataset-if-empty.md) | Done |
| 11 | [No per-tenant database isolation in deletion](#11-no-per-tenant-database-isolation-in-deletion) | P2 | 5 phases | [gap-11](gap-11-per-tenant-isolation.md) | Partial (quick wins done; full multi-tenant deferred) |
| 12 | [No database context variable setting](#12-no-database-context-variable-setting) | P3 | 7 | [gap-12](gap-12-database-context-variables.md) | Done (by design — Rust's explicit DI approach is correct; no ContextVar needed) |
| 13 | [No telemetry/observability on delete](#13-no-telemetryobservability-on-delete) | P3 | 11 | [gap-13](gap-13-telemetry-observability.md) | Not started |

## Dependency Graph

Some gaps must be addressed before others. Recommended implementation order:

```
Gap 3 (missing collections) ─── quick fix, no dependencies
Gap 13 (telemetry) ─────────── standalone, no dependencies
Gap 10 (dataset_if_empty) ──── standalone, no dependencies
Gap 6 (pipeline_runs) ──────── standalone, no dependencies
Gap 7 (search_history) ─────── standalone, no dependencies

Gap 1 (move cleanup to service) ─┬─> Gap 2 (nodes/edges cleanup)
                                  ├─> Gap 5 (hard delete mode)
                                  └─> Gap 9 (orphaned EdgeType)

Gap 9 (orphaned EdgeType) ─────── also needs EdgeType ID determinism fix

Gap 8 (session cache) ─────────── needs SessionStore in ComponentManager

Gap 4 (ACL authorization) ─────── needs DB migration, wrapper service

Gap 11 (per-tenant isolation) ──> Gap 12 (context variables)
```

---

## Gap Descriptions

### 1. Graph/vector cleanup is CLI-only, not in DeleteService

**Priority:** P0 (Critical architectural issue)
**Implementation:** 12 steps | [gap-01-move-cleanup-to-service.md](gap-01-move-cleanup-to-service.md)

Graph and vector artifact cleanup currently lives in `cleanup_graph_and_vector()` inside
the CLI command handler (`crates/cli/src/commands/delete.rs:176-303`), not in the
`DeleteService` (`crates/delete/src/lib.rs`). Any consumer calling
`DeleteService::execute()` gets **incomplete deletion** — relational DB and file storage
are cleaned up, but graph nodes and vector points are left behind.

**Key finding:** The plan injects `GraphDBTrait` and `VectorDB` into `DeleteService` as
`Option<Arc<dyn Trait>>` fields with builder methods, preserving backward compatibility.
The CLI handler becomes a thin wrapper. Also identified that `artifact_references` and
the `nodes`/`edges` provenance tables have overlapping responsibilities — the
implementation should query provenance before relational deletion.

---

### 2. No `nodes`/`edges` table cleanup for data-scoped deletion

**Priority:** P0
**Implementation:** 9 steps | [gap-02-nodes-edges-cleanup.md](gap-02-nodes-edges-cleanup.md)

The `nodes` and `edges` relational tables track graph provenance. Dataset deletion
cascades via FK, but data-scoped deletion (`DeleteScope::Data`) orphans rows because
there's no FK from `data` to `nodes`/`edges` (by design — provenance nodes use nil
`data_id` sentinels).

**Key finding:** Rust already has `delete_nodes_by_data()` and `delete_edges_by_data()`
in `crates/database/src/ops/graph_storage.rs`, but they filter only by `data_id`, not by
the compound `(data_id, dataset_id)` that Python uses. New compound-filtered versions are
needed, plus a `NOT EXISTS` subquery for shared-node detection.

---

### 3. Missing vector collections in `DeleteScope::All`

**Priority:** P0 (Bug)
**Implementation:** 7 steps | [gap-03-missing-vector-collections.md](gap-03-missing-vector-collections.md)

The hardcoded `known_collections` list is incomplete.

**Key finding:** Investigation identified **4 issues**, not 2:
1. Missing `("EntityType", "name")` — created by cognify step 2b
2. Missing `("EdgeType", "relationship_name")` — created by cognify step 5
3. Missing `("Event", "name")` — created by the temporal cognify pipeline
4. Phantom `("Entity", "description")` — listed but never created by any pipeline

**Recommended fix:** Replace the hardcoded list with `vector_db.list_collections()`,
which already exists on the `VectorDB` trait with working implementations. This matches
Python's `prune()` behavior and is future-proof.

---

### 4. No ACL-based authorization

**Priority:** P1
**Implementation:** 10 phases | [gap-04-acl-authorization.md](gap-04-acl-authorization.md)

Python has a full 8-table permission system (`principals`, `users`, `tenants`, `roles`,
`permissions`, `acls`, `user_roles`, `user_tenants`). Every delete operation calls
`get_authorized_dataset(user, dataset_id, "delete")`. Rust has zero ACL infrastructure.

**Key finding:** Recommended an `AuthorizedDeleteService` wrapper pattern (rather than
modifying `DeleteService` directly) to preserve backward compatibility for edge devices
that don't need ACL. Initial implementation can use a minimal 3-table subset
(`principals`, `permissions`, `acls`), deferring roles/tenants.

---

### 5. `Hard` delete mode is unimplemented

**Priority:** P1
**Implementation:** 11 steps | [gap-05-hard-delete-mode.md](gap-05-hard-delete-mode.md)
**Depends on:** Gap 1 (cleanup in service)

`DeleteMode::Hard` is a no-op placeholder. Python's hard mode performs a **global graph
sweep** for degree-one `Entity` and `EntityType` nodes using Cypher queries.

**Key finding:** Requires a new `get_degree_one_nodes(node_type)` method on
`GraphDBTrait` with adapter-specific implementations (Cypher for Ladybug, SQL for
PgGraph). The sweep runs after the standard soft-mode cleanup. Also depends on Gap 1
since it needs graph DB access in the service layer.

---

### 6. No `pipeline_runs` cleanup

**Priority:** P2
**Implementation:** 9 steps | [gap-06-pipeline-runs-cleanup.md](gap-06-pipeline-runs-cleanup.md)

**Key finding:** The main issue is actually about the `pipeline_status` JSON field on
`Data` records, not just `pipeline_runs` rows. Python explicitly scrubs this JSON column
to remove entries keyed by the deleted dataset's ID — this prevents stale "completed"
status from blocking re-processing during incremental loading. The scrubbing must happen
**before** junction rows are removed (while `dataset_data` still exists to locate related
`Data` records). `pipeline_runs` rows cascade via FK on dataset deletion. `task_runs` has
no FK to anything (same gap as Python — neither cleans it up).

---

### 7. No `search_history` (queries/results) cleanup

**Priority:** P2
**Implementation:** 10 steps | [gap-07-search-history-cleanup.md](gap-07-search-history-cleanup.md)

**Key finding:** The `queries` table has a `user_id` column (nullable), enabling
user-scoped cleanup. There is no `dataset_id` column, so dataset-scoped cleanup is
impossible — matching Python's own limitation (explicitly noted in their
`_forget_dataset()` docstring). The FK CASCADE from `results.query_id` to `queries.id`
means deleting query rows automatically cleans up result rows. Cleanup maps to:
User scope → delete by `user_id`, All scope → truncate, Data/Dataset scope → no-op.

---

### 8. No session cache deletion

**Priority:** P2
**Implementation:** 8 steps | [gap-08-session-cache-deletion.md](gap-08-session-cache-deletion.md)

**Key finding:** The initial gap description was partially outdated. Rust's
`SessionStore` trait already has `delete_session()` and `delete_qa_entry()` methods,
fully implemented across all three backends. The **remaining gaps** are:
1. No `prune()` method for full cache wipe (the main gap for `prune_system`)
2. No feedback operations (`add_feedback`, `delete_feedback`, `update_qa_entry`)
3. No session cleanup wired into `DeleteService`
4. No `SessionStore` in `ComponentManager`/`PipelineContext` — created ad-hoc in CLI only

---

### 9. No orphaned EdgeType node cleanup

**Priority:** P2
**Implementation:** 9 steps | [gap-09-orphaned-edgetype-cleanup.md](gap-09-orphaned-edgetype-cleanup.md)
**Depends on:** Gap 1 (cleanup in service)

**Key finding:** Two compounding root causes:
1. EdgeType nodes use **random UUIDs** (`Uuid::new_v4()`) in Rust vs deterministic
   `uuid5(NAMESPACE_OID, normalized_name)` in Python — making ID-based cleanup impossible
2. EdgeType nodes are **never recorded** in the `artifact_references` provenance table

Both must be fixed. Step 1 (deterministic IDs) is also a Python parity fix for cognify.
Requires a new `get_all_relationship_names()` method on `GraphDBTrait` for efficient
orphan detection.

---

### 10. Missing `delete_dataset_if_empty` option

**Priority:** P2
**Implementation:** 13 steps | [gap-10-delete-dataset-if-empty.md](gap-10-delete-dataset-if-empty.md)

**Key finding:** Clean design — add `delete_dataset_if_empty: bool` to
`DeleteScope::Data` variant with `#[serde(default)]` for backward compatibility. The
main logic change is in `resolve_data_scope()`: conditionally populate
`datasets_to_delete` when the flag is set and the dataset will become empty after detach.
The existing `execute()` flow handles the rest automatically since it already iterates
`datasets_to_delete`. New CLI flag: `--delete-dataset-if-empty`.

---

### 11. No per-tenant database isolation in deletion

**Priority:** P2
**Implementation:** 5 phases | [gap-11-per-tenant-isolation.md](gap-11-per-tenant-isolation.md)

Python's `dataset_database` table maps each dataset to dedicated graph/vector database
instances. Four handler implementations (`Kuzu`, `Neo4jAura`, `LanceDB`, `PGVector`)
implement `DatasetDatabaseHandlerInterface`.

**Key finding:** This is the largest gap — a 5-phase effort spanning the full
multi-tenant architecture. However, two **quick wins** can be done immediately:
1. Fix `DeleteDb::get_dataset_by_name` to accept `tenant_id` (it already does in
   `IngestDb` but not `DeleteDb`)
2. Add single-tenant graph/vector wipe to `DeleteScope::All` (partially covered by Gap 1)

---

### 12. No database context variable setting

**Priority:** P3 (downgraded from P2)
**Implementation:** 7 steps | [gap-12-database-context-variables.md](gap-12-database-context-variables.md)
**Depends on:** Gap 11 (per-tenant isolation)

**Key finding:** Python's `ContextVar` pattern is a workaround for Python's global
mutable state / `@lru_cache` architecture. Rust's explicit ID-based approach is actually
correct and sufficient for single-database deployments. The recommendation is to **not
replicate ContextVars** and instead extend `PipelineContext` with explicit typed methods
(`graph_db_for_dataset()`, `vector_db_for_dataset()`) when multi-tenant DB isolation
(Gap 11) is implemented. This is type-safe and impossible to misuse.

---

### 13. No telemetry/observability on delete

**Priority:** P3
**Implementation:** 11 steps | [gap-13-telemetry-observability.md](gap-13-telemetry-observability.md)

**Key finding:** `DeleteService` has **zero tracing** — no imports, no log statements,
no spans across 493 lines. The CLI has 4 plain `info!`/`warn!` calls. Foundational
infrastructure exists elsewhere (`crates/search/src/observability.rs` defines semantic
constants, search retrievers use `#[tracing::instrument]`). Plan adds `tracing`
instrumentation to 7 methods and 5 checkpoint events. Explicitly recommends **not**
implementing `send_telemetry` HTTP analytics — inappropriate for CLI/embedded SDK.
Estimated ~150-200 lines across 4 files, no architectural changes.

---

## Key Findings from Investigation

Several investigations uncovered facts not visible in the initial gap analysis:

1. **Gap 3** found 4 issues instead of 2 — includes missing `("Event", "name")` from
   temporal pipeline and phantom `("Entity", "description")` that's never created
2. **Gap 8** found that `SessionStore` already has `delete_session()`/`delete_qa_entry()`
   — the gap is narrower than originally assessed (missing `prune()` + feedback ops)
3. **Gap 9** found that EdgeType IDs are non-deterministic (`new_v4()`) — a prerequisite
   fix needed before orphan cleanup can work
4. **Gap 2** found existing `delete_nodes_by_data()`/`delete_edges_by_data()` ops
   functions that are never called from the delete pipeline
5. **Gap 12** was downgraded to P3 — Python's ContextVar is architecture-specific, and
   Rust's explicit ID approach is correct for current deployments

## References

- **Rust delete service:** `crates/delete/src/lib.rs`
- **Rust CLI delete command:** `crates/cli/src/commands/delete.rs`
- **Rust DeleteDb trait:** `crates/database/src/traits/delete_db.rs`
- **Rust DB schema:** `crates/database/src/migrator/m20250101_000001_initial_schema.rs`
- **Python delete API:** `cognee/api/v1/delete/`, `cognee/api/v1/forget/`
- **Python data deletion:** `cognee/modules/data/deletion/`, `cognee/modules/data/methods/`
- **Python graph deletion:** `cognee/modules/graph/methods/`
- **Python prune system:** `cognee/modules/data/deletion/prune_system.py`
