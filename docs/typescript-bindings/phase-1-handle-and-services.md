# Phase 1 — Handle & service facade (keystone)

← [Index](../typescript-bindings-plan.md)

**Goal:** a single stateful handle that owns config and a fully-wired bundle of engines +
derived services, so every later SDK function is a thin call through it. This phase de-risks the
whole plan: if the facade is right, Phases 3–6 are mechanical.

## Scope

- **In:** the `CogneeHandle` (a `JsBox`), the `CogneeServices` facade, version-based cache
  invalidation, owner/tenant bootstrap, runtime integration.
- **Out:** the SDK operations themselves (Phases 3–6), config setters (Phase 2), marshalling
  helpers (Phase 8 — use a minimal inline version here).

## Structures

### `js/cognee-neon/src/sdk.rs` — `CogneeHandle`
A boxed, `Finalize`-able struct holding:
- `cm: Arc<ComponentManager>` — owns config + lazy engines.
- a cached `CogneeServices` (behind a `tokio::Mutex<Option<Arc<CogneeServices>>>` plus the
  config version it was built at).
- `owner_id: Uuid`, `tenant_id: Option<Uuid>` — resolved once at construction.

Key method (Rust-internal, not exported): `async fn services(&self) -> Result<Arc<CogneeServices>, ApiError>`
— double-checked: if the cache is empty or the `ConfigManager` version advanced, rebuild;
otherwise return the cached `Arc`.

### `js/cognee-neon/src/services.rs` — `CogneeServices`
The one place construction happens. Fields = the 6 engines from `PipelineContext` **plus** the
derived services:

```
storage, database, graph_db, vector_db, embedding_engine, llm,   // from ComponentManager
thread_pool, pipeline_run_repo, add_pipeline, delete_service,
search_orchestrator, session_store, session_manager,
ontology_resolver, cognify_config, checkpoint_store              // derived
```

`async fn build(cm: &ComponentManager) -> Result<Self, ApiError>`:
1. Resolve the 6 engines via `cm.storage()`, `.database()`, … (errors map to `ComponentError`).
2. Construct derived services using the same builders the CLI uses:
   - `thread_pool` = `RayonThreadPool::with_default_threads()`.
   - `pipeline_run_repo` = `SeaOrmPipelineRunRepository` over the relational DB.
   - `add_pipeline` = `AddPipeline::new(storage, db)` + `.with_thread_pool / .with_graph_db /
     .with_vector_db / .with_database / .with_pipeline_run_repo`.
   - `delete_service` = `DeleteService::new(...)` across relational/graph/vector/storage.
   - `search_orchestrator` = `SearchBuilder::new(vector, embedding, graph, llm, db)
     .with_session_manager(...).with_dataset_resolver(db as IngestDb).build()`.
   - `session_store` / `session_manager` selected by `Settings.cache_backend` (fs / redis /
     seaorm).
   - `ontology_resolver` = `RdfLibOntologyResolver` when an ontology file is configured, else
     `NoOpOntologyResolver`.
   - `cognify_config` = built from `Settings` (chunk size/strategy/overlap, token-counter kind,
     summarization flags, …).
   - `checkpoint_store` = optional, per config.

**v1 invalidation policy:** rebuild the whole bundle on any config-version bump. Simple and
correct; optimize to per-field rebuilds only if profiling demands it.

## Functionalities (exported native functions)

- `cogneeNew(settingsJson?) -> JsBox<CogneeHandle>` — build `Settings` (from a JS object, else
  `from_env()`), wrap in `ConfigManager`, then `ComponentManager`; bootstrap the owner via
  `get_or_create_default_user` (surface #15); defer or eagerly warm `services`.
- `cogneeWarm(handle) -> Promise<void>` — force `services()` to build now, surfacing config/connection
  errors early instead of on first op.
- `cogneeOwnerId(handle) -> string` — expose the resolved owner UUID.

The **canonical pattern** all later `sdk_*` functions follow:
```
let svc = handle.services().await?;
let out = <cognee-lib api>(…, svc.add_pipeline.clone(), svc.llm.clone(), …).await?;
serde_to_js(&mut cx, &out)
```

## Runtime

Reuse the existing global tokio runtime (`runtime.rs`). The handle must not require a separate
`init()` — either auto-initialize the runtime inside `cogneeNew`, or make `init()` idempotent so
calling it twice is safe.

## Dependencies & ordering

Needs Phase 0. Provides the foundation for Phases 2–6. The single most important phase.

## Risks

- Services hold live DB connections + the embedding model in memory — define lifecycle and
  `Finalize` behavior.
- Owner bootstrap requires a working relational DB at construction time.
- Thread-safety: `Arc` everywhere; guard the cache with a `tokio::Mutex`.

## Done when

- A handle constructs from TS and survives across calls.
- `services()` returns a fully-wired bundle; a config-version bump triggers a rebuild.
- A Tier-A test constructs a handle and `cogneeWarm`s it with `MOCK_EMBEDDING=true` and a temp
  data dir (no LLM needed).
