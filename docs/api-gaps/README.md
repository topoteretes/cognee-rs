# Cognee Rust SDK — API v1 Gap Analysis

This document catalogs the verified differences between the Python cognee SDK's v1 library API and the Rust implementation. The focus is on **library-level methods** exposed through the umbrella crate (`cognee-lib`), not HTTP endpoints or horizontal feature breadth (number of search types, file formats, LLM providers, etc.).

Each gap has a description document and a separate implementation plan in [impl/](impl/).

**Reference:** Python SDK at [github.com/topoteretes/cognee](https://github.com/topoteretes/cognee) (`cognee/api/v1/`)

---

## 1. Missing Parameters on Existing Functions

The core pipeline functions (`add`, `cognify`, `search`, `memify`) exist in both SDKs but the Rust versions accept fewer parameters than their Python equivalents.

| Function | Python Params | Rust Params | Missing |
|----------|---------------|-------------|---------|
| `add()` | 11 + kwargs | 4 | 8 params |
| `cognify()` | 14 + kwargs | 12 (via CognifyConfig) | 6 params (some partially present) |
| `search()` | 19 | 1 (SearchRequest struct) | 3 params |
| `memify()` | 10 | 7 (via MemifyConfig) | 5 params |

**Key gaps by function:**

- **`add()`** — Missing `node_set` (field exists on `Data` model but `add()` never populates it), `dataset_id` (target by UUID), `preferred_loaders`, `incremental_loading` (dedup is always on), `data_per_batch`, `importance_weight`, and per-call backend overrides.

- **`cognify()`** — `cognify_datasets()` exists in `dataset_resolver.rs` for name-based resolution but not UUID-based. `GraphModel` trait and generic `FactExtractor` exist but are not wired into the top-level pipeline (hardcoded to `KnowledgeGraph`). Auto `chunk_size` calculation already works. Per-call backend overrides missing.

- **`search()`** — `node_name_filter_operator` exists in `SearchParams` but is not on `SearchRequest` (hardcoded to `None` in the `From` impl). `neighborhood_depth` and `neighborhood_seed_top_k` are missing entirely.

- **`memify()`** — Missing `extraction_tasks`/`enrichment_tasks` (custom pipeline tasks), `data` (custom input), and per-call backend overrides.

**Cross-cutting:** `vector_db_config`/`graph_db_config` per-call overrides appear on 3 of 4 functions. A single `BackendOverrides` pattern would cover all.

**Gap details:** [01-missing-parameters.md](01-missing-parameters.md) | **Impl plan:** [impl/01-missing-parameters-plan.md](impl/01-missing-parameters-plan.md)

---

## 2. Functions Missing from Rust Entirely

Six high-level API functions from the Python SDK have no equivalent in Rust. These are convenience compositions and session-integration layers built on top of the core primitives.

| Function | Purpose | Complexity | Depends On |
|----------|---------|------------|------------|
| `forget()` | Unified deletion: item / dataset / everything | Low | Existing `DeleteService` |
| `update()` | Delete old data → re-add → re-cognify | Low | Existing primitives |
| `prune` | Selective backend cleanup (graph/vector/metadata/cache) | Low-Medium | `GraphDBTrait::delete_graph()` already exists; need `VectorDB` prune |
| `recall()` | Smart search: session-first routing + rule-based auto query-type selection | Medium | Session keyword search |
| `remember()` | One-call `add` + `cognify` + optional `improve` with session bridging | Medium | `improve()` |
| `improve()` | Bidirectional session-graph bridge (4 stages, only Stage 3 exists as `memify()`) | High | Feedback system, graph property updates |

Notable findings from verification:
- `GraphDBTrait::delete_graph()` already exists (covers `prune_system(graph=True)`)
- `VectorDB::list_collections()` exists, enabling iterative collection deletion
- `feedback_weight` field exists on `DataPoint` model and is already consumed by triplet ranking — the read-side of `improve()` Stage 1 is partially in place
- `used_graph_element_ids` exists in FS/Redis store internals but is not exposed on public `SessionQAEntry`

**Gap details:** [02-missing-functions.md](02-missing-functions.md) | **Impl plan:** [impl/02-missing-functions-plan.md](impl/02-missing-functions-plan.md)

---

## 3. Configuration API

The Python SDK provides 33 runtime setter methods on a `config` class. The Rust SDK has **no setter methods**. Configuration is read once from environment variables at startup, and `ComponentManager` caches components in `tokio::sync::OnceCell` fields (one-time lazy init, no reinitialization).

| Capability | Python | Rust |
|------------|--------|------|
| Runtime setters | 33 methods on `config` class | None (CLI has pre-construction `config set` only) |
| Cascading path updates | `system_root_directory()` cascades to 3 backends | N/A |
| Bulk config | `set_llm_config(dict)` with validation | N/A |
| Embedding config | In `Settings`-equivalent | Bypasses `Settings` — `EmbeddingConfig::from_env()` called directly |
| Component reinitialization | Implicit (Python recreates on access) | `OnceCell` — no reinitialization possible |

Additional finding: `Settings` struct lacks `embedding_provider`, `embedding_endpoint`, and `embedding_api_key` fields entirely — the embedding engine initialization bypasses `Settings` and reads env vars directly.

**Gap details:** [03-configuration-api.md](03-configuration-api.md) | **Impl plan:** [impl/03-configuration-api-plan.md](impl/03-configuration-api-plan.md)

---

## 4. User / Authentication / Multi-Tenancy

The Python SDK has a full user management layer with polymorphic principal hierarchy (`User`/`Tenant`/`Role` all inheriting `Principal`), 3-level permission resolution (user → tenant → role), and auto-created default user.

The Rust SDK has **no user model**. It uses a static `default_user_id` UUID from config. However, the ACL infrastructure is more complete than initially described:
- `principals`, `permissions`, and `acls` tables already exist (from `m20250201_000001_acl_tables` migration)
- `AclDb` trait with `has_permission`, `authorized_dataset_ids`, `grant_permission`, `revoke_permission`
- `PERMISSION_NAMES` constants defined in `ops/acl.rs`
- Auto-grant on dataset creation via `AddPipeline::with_acl_db()`

| Feature | Python | Rust | Status |
|---------|--------|------|--------|
| User/Tenant/Role models | Full ORM models | None | Missing |
| Default user creation | Auto-created on first call | Static UUID | Missing |
| Permission constants | `PERMISSION_TYPES` list | `PERMISSION_NAMES` in ops | Partial |
| ACL tables | Full schema | `principals`/`permissions`/`acls` exist | Implemented |
| Permission inheritance | User → Tenant → Role chain | Flat principal → dataset | Missing |
| Auto-grant on dataset creation | Yes | Yes (via `with_acl_db()`) | Implemented |

**Gap details:** [04-user-auth-tenancy.md](04-user-auth-tenancy.md) | **Impl plan:** [impl/04-user-auth-tenancy-plan.md](impl/04-user-auth-tenancy-plan.md)

---

## 5. Dataset Management

The Python SDK provides a `datasets` class with 8 methods for dataset CRUD. Rust has extensive low-level infrastructure but **no high-level facade** composing it with permission checks.

Existing Rust infrastructure (verified):
- `DeleteDb` has `list_datasets_by_owner()`, `get_dataset_data()`, `delete_dataset()`, `delete_data()`
- `AclDb` has `authorized_dataset_ids()`, `has_permission()`
- `AuthorizedDeleteService` already wraps `DeleteService` with ACL enforcement
- `PipelineRunStatus` enum and `pipeline_runs` table already exist
- `get_dataset(db, id)` exists in `ops/datasets.rs` but not on any trait
- `get_latest_pipeline_status()` exists in ops but not on any trait

The gap is a `DatasetManager` facade that unifies these into Python-equivalent convenience methods.

**Gap details:** [05-dataset-management.md](05-dataset-management.md) | **Impl plan:** [impl/05-dataset-management-plan.md](impl/05-dataset-management-plan.md)

---

## 6. Session Management

Both SDKs have session storage (Fs, Redis, SeaOrm backends). The Python SDK builds a feedback-driven enrichment cycle on top that Rust lacks.

Key finding from verification: FS and Redis store implementations already have internal structs (`FsQAEntry`, `RedisQAEntry`) with `feedback_text`, `feedback_score`, `used_graph_element_ids`, and `memify_metadata` fields — but these are **discarded** during conversion to the public `SessionQAEntry` type. The SeaORM store schema lacks feedback columns entirely.

| Feature | Python | Rust | Status |
|---------|--------|------|--------|
| Save/load/delete Q&A entries | Yes | Yes | Implemented |
| Feedback text/score fields | On `SessionQAEntry` model | Fields exist in FS/Redis internals but not on domain type | Partially wired |
| `update_qa_entry()` | On `SessionStore` + `SessionManager` | Not on trait | Missing |
| `add_feedback()` / `delete_feedback()` | Public API | Not implemented | Missing |
| Graph context storage | `get_graph_context()` / `set_graph_context()` | Not implemented | Missing |
| Session keyword search | Used by `recall()` | Not implemented | Missing |

**Gap details:** [06-session-management.md](06-session-management.md) | **Impl plan:** [impl/06-session-management-plan.md](impl/06-session-management-plan.md)

---

## 7. Ontology Management

Core ontology resolution is **fully implemented** in both SDKs (with Rust having broader format support). The gap is in **file management** — CRUD operations on ontology files.

Verification corrections:
- Python API is `OntologyService` class methods (not standalone functions), with `ontology_key` parameter for keying files
- Rust matching algorithm is Gestalt (Ratcliff/Obershelp), not Jaro-Winkler (matches Python's `difflib.SequenceMatcher.ratio()`)
- Rust `find_closest_match()` takes `(name, category)`, not just `(entity_name)` — matches Python signature

| Feature | Python | Rust | Status |
|---------|--------|------|--------|
| Fuzzy matching + subgraph extraction | Yes | Yes | Implemented |
| File loading (multiple formats) | Via rdflib | Via sophia (broader) | Implemented |
| Upload / list / get / delete ontology files | `OntologyService` class | None | Missing |
| Per-user file storage | `tempdir/ontologies/<user_id>/` | None | Missing |

**Gap details:** [07-ontology-management.md](07-ontology-management.md) | **Impl plan:** [impl/07-ontology-management-plan.md](impl/07-ontology-management-plan.md)

---

## 8. Environment Variable Coverage

The Python SDK reads 105 environment variables. The Rust SDK covers 43 (counting both central `Settings` and crate-local configs).

| Category | Python Vars | Rust Covered | Coverage |
|----------|-------------|-------------|----------|
| Core LLM | 14 | 8 | 57% |
| Embedding | 13 | 8 | 62% |
| Graph DB | 9 | 8 | 89% |
| Vector DB | 8 | 5 | 63% |
| Relational DB | 7 | 7 | 100% |
| Session/Cache | 11 | 0 | 0% |
| System/Storage | 7 | 4 | 57% |
| Ontology | 3 | 3 | 100% |
| Observability | 7 | 0 | 0% |
| Authentication | 3 | 0 | 0% |
| AWS/S3 | 7 | 0 | 0% |
| Logging | 5 | 0 | 0% |
| **Total** | **105** | **43** | **41%** |

Key finding: Several embedding env vars (`EMBEDDING_API_KEY`, `EMBEDDING_ENDPOINT`, `EMBEDDING_PROVIDER`) are read in crate-local configs but not in the central `Settings` struct, creating a split config path. Rust also has 16 env vars with no Python equivalent (legacy aliases, ONNX paths, etc.).

**Gap details:** [08-env-variables.md](08-env-variables.md) | **Impl plan:** [impl/08-env-variables-plan.md](impl/08-env-variables-plan.md)

---

## Recommended Implementation Order

Based on dependency chains and impact:

| Phase | Gap | Rationale |
|-------|-----|-----------|
| 1 | **Environment Variables** (Gap 8) | Foundation — unblocks session and auth config |
| 2 | **Session Management** (Gap 6) | Add feedback fields and update methods — unblocks `improve()` |
| 3 | **Configuration API** (Gap 3) | Runtime config mutation — unblocks per-call overrides |
| 4 | **Missing Parameters** (Gap 1) | Extend existing function signatures |
| 5 | **Dataset Management** (Gap 5) | `DatasetManager` facade over existing DB traits |
| 6 | **Missing Functions** (Gap 2) | `forget` → `update` → `prune` → `recall` → `remember` → `improve` |
| 7 | **Ontology Management** (Gap 7) | File management layer |
| 8 | **User/Auth/Tenancy** (Gap 4) | User model + permission system — largest scope |

---

## Document Structure

```
docs/api-gaps/
├── README.md                              # This overview
├── 01-missing-parameters.md               # Gap descriptions (verified)
├── 02-missing-functions.md                # Gap descriptions (verified)
├── 03-configuration-api.md                # Gap descriptions (verified)
├── 04-user-auth-tenancy.md                # Gap descriptions (verified)
├── 05-dataset-management.md               # Gap descriptions (verified)
├── 06-session-management.md               # Gap descriptions (verified)
├── 07-ontology-management.md              # Gap descriptions (verified)
├── 08-env-variables.md                    # Gap descriptions (verified)
└── impl/
    ├── 01-missing-parameters-plan.md      # Step-by-step implementation plan
    ├── 02-missing-functions-plan.md
    ├── 03-configuration-api-plan.md
    ├── 04-user-auth-tenancy-plan.md
    ├── 05-dataset-management-plan.md
    ├── 06-session-management-plan.md
    ├── 07-ontology-management-plan.md
    └── 08-env-variables-plan.md
```
