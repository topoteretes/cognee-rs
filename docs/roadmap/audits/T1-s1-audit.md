# T1 — S1 adapter-injection audit

**Result:** `S1-fix` set is **empty**. The plan's "(Mostly already true.)"
([oss-split-plan.md §4 S1](../oss-split-plan.md#4-seams-that-must-exist-before-the-split))
turns out to be *fully* already true. T1 lands no source-code changes; it
records the audit so later seam tasks (S2/S3/S4/S6/S7) can lean on it.

## What S1 requires

Every backend reachable through a builder must take `Arc<dyn Trait>`, and OSS
must not select a *closed* adapter via `#[cfg(feature = …)]`. The six target
traits are `VectorDB`, `GraphDBTrait`, `EmbeddingEngine`, `Llm`,
`SessionStore`, `AclDb`.

## Trait-by-trait verdict

All six traits are `Send + Sync`, every consumer takes them as `Arc<dyn …>`
(or `Option<Arc<dyn …>>` for the optional ACL path). The injection surface
is already correct.

| Trait | Definition | Representative injection sites |
|---|---|---|
| `VectorDB` | `crates/vector/src/vector_db_trait.rs:8` | `AddPipeline::with_vector_db` `crates/ingestion/src/pipeline.rs:1082`; `DeleteService::with_vector_db` `crates/delete/src/lib.rs:185`; `SearchBuilder::new` `crates/search/src/orchestration/search_execution_builder.rs:30`; `CogneeServices.vector_db` `crates/bindings-common/src/services.rs:53` |
| `GraphDBTrait` | `crates/graph/src/traits.rs:54` | `AddPipeline::with_graph_db` `crates/ingestion/src/pipeline.rs:1076`; `DeleteService::with_graph_db` `crates/delete/src/lib.rs:180`; `SearchBuilder::new` `…/search_execution_builder.rs:32`; `CogneeServices.graph_db` `…/services.rs:51` |
| `EmbeddingEngine` | `crates/embedding/src/engine.rs:11` | `SearchBuilder::new` `…/search_execution_builder.rs:31`; `CogneeServices.embedding_engine` `…/services.rs:54` |
| `Llm` | `crates/llm/src/llm_trait.rs:17` | `SearchBuilder::new`; `SearchOrchestrator::with_llm` `crates/search/src/orchestration/search_orchestrator.rs:165`; `CogneeServices.llm` `…/services.rs:55` |
| `SessionStore` | `crates/session/src/session_store.rs:31` | `CogneeServices.session_store` `…/services.rs:62`; `DeleteService::with_session_store` `crates/delete/src/lib.rs:190`; `api::{improve, remember, recall}` |
| `AclDb` | `crates/database/src/traits/acl_db.rs:14` | `AddPipeline::with_acl_db` `crates/ingestion/src/pipeline.rs:1064`; `AuthorizedDeleteService::new(…, Arc<dyn AclDb>, …)` `crates/delete/src/authorized.rs:31`; `DatasetManager.with_acl` `crates/lib/src/api/datasets.rs:32,42` |

## Surviving closed-named selections — out of scope for S1

Every `#[cfg(feature)]` selection of a closed adapter is owned by a *later*
seam task. Listing them here so those tasks can find them without re-search.

### T2 (S2 / S2b / S2d / S2e — access-control extraction)
- `crates/database/src/traits/acl_db.rs:80` — `impl AclDb for DatabaseConnection`
  (orphan-rule blocker). Move to the closed newtype `AccessControl`.
- Production self-built `Arc<dyn AclDb>` casts (S2d):
  - `crates/http-server/src/routers/add.rs:265`
  - `crates/http-server/src/routers/update.rs:282`
  - `crates/http-server/src/routers/remember.rs:279,1063`
  - `crates/cli/src/commands/delete.rs:78` (`--enforce-acl`)
- Test self-built casts (resolved by `MockAclDb` once the impl is removed):
  - `crates/lib/src/api/datasets.rs:471, 658, 683`
  - `crates/delete/tests/authorized_delete_integration.rs:72, 90`
- Auth domain re-exports in OSS `prelude` to be moved (S2e):
  - `crates/lib/src/lib.rs:210–212` re-exports `RoleDb, TenantDb, UserDb`
    (inside the `crate::database::{…}` `pub use` block).

### T3 (S3 — HTTP router injection)
- `crates/http-server/Cargo.toml:93` — `cognee-cloud` hard dep, used only by
  `routers/sync.rs` + `routers/checks.rs`. Both routers move/gate closed.
- `crates/http-server/src/auth/context.rs:27,64,144` — `ExtraAuthValidator`
  slot exists (`Option<Arc<dyn ExtraAuthValidator>>`) but is `None`-hardcoded;
  T3 adds `with_extra_validator` to the router builder.

### T4 (S4 — qdrant + litert adapter extraction)
- `crates/lib/src/component_manager.rs:23,343–349` — `use cognee_vector::QdrantAdapter`
  + the `"qdrant"` provider arm gated on `#[cfg(feature = "qdrant")]`.
- `crates/lib/src/component_manager.rs:18,582–611` — `use cognee_llm::LiteRtAdapter`
  + the `"litert"` arm gated on `#[cfg(all(feature = "android-litert", target_os = "android"))]`.

### T7 (S6 / S7 — cloud re-exports + default-feature hygiene)
- `crates/lib/Cargo.toml:22,99` — `qdrant` in `default` and `android-default`
  feature sets; companion re-export at `crates/lib/src/lib.rs:100`.
- `crates/lib/Cargo.toml:36` — `cognee-cloud` optional dep + `cloud` in
  `default`; cloud re-exports in `crates/lib/src/lib.rs:143–155`.
- `crates/lib/src/api/mod.rs:23` — `#[cfg(feature = "cloud")] pub mod serve`
  and `crates/lib/src/api/serve.rs:13` `pub use cognee_cloud::…`.

## Already-existing injection precedents (informational)

- `crates/http-server/src/cloud_client.rs:20` — `CloudDeleteClient` is a
  local trait carried as `Option<Arc<dyn CloudDeleteClient>>` in
  `crates/http-server/src/components.rs:45`. This is the documented
  precedent S5/T6 leans on.
- `CogneeServices` (`crates/bindings-common/src/services.rs:45–67`) has 16
  `pub` fields, so a closed caller can directly assemble a `CogneeServices {
  … }` with closed `Arc<dyn …>` values without needing a new
  `build_with_overrides` API.

## Nice-to-haves explicitly deferred

- `CogneeServices` / CLI commands hard-code `SeaOrmSessionStore::new(database)`
  (`crates/bindings-common/src/services.rs:136–139`,
  `crates/cli/src/commands/{recall,improve,remember,search,bench}.rs`).
  Since `SeaOrmSessionStore` stays OSS (§3 / §6), this is not an S1-fix; a
  `with_session_store` builder is a future ergonomic, not a closed-vs-open
  concern.
- `ComponentManager` builds adapters from `Settings` provider strings rather
  than from injected `Arc<dyn …>`. The closed assembly path will construct
  its own `CogneeServices` directly (the struct's `pub` fields support this);
  reshaping `ComponentManager` itself is T4/T6 territory, not S1.
