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
- `owner_id: tokio::Mutex<Option<Uuid>>`, `tenant_id: Option<Uuid>` — `owner_id` is resolved
  **on first warm** (it is the id of the `User` row created/fetched by
  `get_or_create_default_user`), so it is `None` until `cogneeWarm`/`services()` has run at least
  once; `tenant_id` is `None` (the default user carries no tenant — see `get_or_create_default_user`,
  which sets `tenant_id: None`).

Key method (Rust-internal, not exported): `async fn services(&self) -> Result<Arc<CogneeServices>, ApiError>`
— double-checked: if the cache is empty or the `ConfigManager` version advanced, rebuild;
otherwise return the cached `Arc`. On the (re)build path, after the DB engine is available, call
`get_or_create_default_user(database, &settings.default_user_email)` and store the returned `User.id`
into `owner_id` (idempotent — UUID5 from the email, so repeated warms return the same id).

### `js/cognee-neon/src/services.rs` — `CogneeServices`
The one place construction happens. Fields = the 6 engines from `PipelineContext` **plus** the
derived services:

```
storage, database, graph_db, vector_db, embedding_engine, llm,   // from ComponentManager
thread_pool, pipeline_run_repo, add_pipeline, delete_service,
search_orchestrator, session_store, session_manager,
ontology_resolver, cognify_config, checkpoint_store              // derived
```

**Concrete-type note:** `cm.database()` returns `Arc<DatabaseConnection>` (the concrete SeaORM
connection), **not** an `Arc<dyn IngestDb>`. `DatabaseConnection` implements every DB trait
(`IngestDb`, `DeleteDb`, `SearchHistoryDb`, `AclDb`, `UserDb`), so derived services receive it via
explicit unsized coercions, e.g. `Arc::clone(&database) as Arc<dyn IngestDb>`. Store the concrete
`Arc<DatabaseConnection>` in the field and coerce at each call site (matches the CLI).

`async fn build(cm: &ComponentManager) -> Result<(Self, Uuid), ApiError>` — returns the bundle
**and** the resolved owner id (the handle stores the id; `services()` writes it back into the
handle's `owner_id` mutex):
1. Resolve the engines via `cm.storage()`, `.database()`, `.graph_db()`, `.vector_db()`,
   `.embedding_engine()` (errors map to `ComponentError`). **Caveat:** `cm.llm()` errors when
   `llm_api_key` is empty (`init_llm` in `component_manager.rs`). To keep `cogneeWarm` usable in
   keyless/CI environments, `build()` must resolve the LLM **leniently** — either resolve it lazily
   on first use, or resolve-and-tolerate-error here (store `Result`/`Option` and surface the error
   only when an LLM-requiring op actually runs). Pick one; do not hard-fail warm on a missing LLM
   key. `search_orchestrator` registration needs an `Arc<dyn Llm>`, so if the LLM is unavailable at
   build time, the orchestrator must be built lazily too (i.e. tie `search_orchestrator` +
   `add_pipeline`/`delete_service` that don't need the LLM into separate resolution paths), OR
   accept that `cogneeWarm` requires a valid LLM and have the Tier-A test set a dummy `llm_api_key`.
   **Implementation note:** the simplest correct v1 is to set a placeholder `llm_api_key` in the
   Tier-A test (a non-empty dummy string) so the OpenAI adapter constructs without a network call,
   keeping `build()` strict and the facade simple. Confirm `OpenAIAdapter::new` does not perform
   I/O at construction (it does not — it only builds the client).
2. **Resolve the owner id (Python default-user semantics).** Call
   `cognee_lib::api::get_or_create_default_user(&*database, &settings.default_user_email)` (coerce
   `Arc<DatabaseConnection>` → `&dyn UserDb`) and take the returned `User.id` as `owner_id`. This
   guarantees a real `User` row exists (so cognify/email provenance and ACL lookups work) and gives
   parity with Python-written data. The id is `uuid5(NAMESPACE_OID, default_user_email)`, so the
   call is idempotent across warms. Return this `owner_id` from `build()` alongside the bundle.
   **Note:** this is deliberately *not* the CLI's `settings.default_user_id` parse — the binding
   tracks Python's default-user model, where the owning user is keyed by email. `tenant_id` stays
   `None` (the default user has no tenant).
3. Construct derived services using the same builders the CLI uses
   (`crates/cli/src/commands/{add,cognify,search,delete}.rs` are the authoritative reference):
   - `thread_pool` = `Arc::new(RayonThreadPool::with_default_threads()?)` — returns
     `Result`, so propagate the error. Note the CLI stores it as both
     `Arc<RayonThreadPool>` (add) and `Arc<dyn CpuPool>` (cognify); store the concrete type and
     coerce as needed.
   - `pipeline_run_repo` = `Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)))`,
     held as `Arc<dyn PipelineRunRepository>`.
   - `add_pipeline` = `AddPipeline::new(storage, Arc::clone(&database) as Arc<dyn IngestDb>)`
     `.with_thread_pool(thread_pool).with_graph_db(graph_db).with_vector_db(vector_db)`
     `.with_database(database).with_pipeline_run_repo(pipeline_run_repo)`.
   - `delete_service` = `DeleteService::new(storage, Arc::clone(&database) as Arc<dyn DeleteDb>)`
     `.with_graph_db(graph_db).with_vector_db(vector_db).with_pipeline_run_repo(pipeline_run_repo)`
     (graph/vector/repo are `.with_*()`, not constructor args). The ACL-enforcing
     `AuthorizedDeleteService` wrapper is a Phase-5 concern; store the unauthorized
     `DeleteService` here.
   - `search_orchestrator` = `SearchBuilder::new(vector, embedding, graph, llm,
     Arc::clone(&database) /* coerces to Arc<dyn SearchHistoryDb> */)
     .with_session_manager(session_manager).with_dataset_resolver(database as Arc<dyn IngestDb>)
     .build()`.
   - `session_store` / `session_manager`: **v1 = always `SeaOrmSessionStore` (do NOT branch on
     `cache_backend` yet).** Verified feature reality: `FsSessionStore`/`RedisSessionStore` are
     gated behind `cognee-session`'s `fs`/`redis` features, which are **not enabled** in the
     `cognee-neon` build — only `sea-orm-store` is (it is unioned in transitively because
     `cognee-search` enables `cognee-session/sea-orm-store`). So only `SeaOrmSessionStore` is
     reachable today, re-exported as `cognee_lib::search::SeaOrmSessionStore`. Build it exactly as
     the search CLI does: `SeaOrmSessionStore::new(Arc::clone(&database)).await?` (async, returns
     `Result`), then `let session_manager = Arc::new(SessionManager::new(Arc::new(store)))`. Keep
     a clone of the `Arc<dyn SessionStore>` in `session_store` for the API functions that take it
     (recall/improve/prune). A real `cache_backend` switch (fs/redis) is deferred — it needs the
     binding to opt into those `cognee-session` features first (out of scope for Phase 1; note it
     in the decision log).
   - `ontology_resolver` = `RdfLibOntologyResolver::new(path)?` when `Settings.ontology_file_path`
     is non-empty, else `NoOpOntologyResolver::new()`; held as `Arc<dyn OntologyResolver>`.
   - `cognify_config` = `CognifyConfig::default().with_chunk_size(settings.chunk_size as usize)
     .with_chunk_overlap(settings.chunk_overlap as usize).with_chunk_strategy(...)
     .with_max_parallel_extractions(settings.llm_max_parallel_requests.max(1) as usize)`. Map
     `Settings.chunk_strategy` ("RECURSIVE" → `ChunkStrategy::Recursive`, else `Paragraph`).
     `with_temporal_cognify(...)` is a per-call flag, not a Settings field — leave at default here.
   - `checkpoint_store` = `Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&database)))` (from
     `cognee-database`; used by `remember`/`improve`). Store unconditionally — it is cheap and the
     API takes it as `Option<Arc<dyn …>>`, so callers pass `Some(svc.checkpoint_store.clone())`.

**v1 invalidation policy:** rebuild the whole bundle on any config-version bump. Simple and
correct; optimize to per-field rebuilds only if profiling demands it.

## Functionalities (exported native functions)

- `cogneeNew(settingsJson?) -> JsBox<CogneeHandle>` — build `Settings` (from a JS object, else
  `from_env()`), wrap in `ConfigManager`, then `ComponentManager`. **Construction must not block on
  I/O** (Neon constructors run synchronously on the JS thread and a `JsBox` ctor cannot `.await`).
  The ctor stays **pure/sync**: it does **not** resolve `owner_id` and does **not** touch the DB.
  It initialises `owner_id` to `None` (a `tokio::Mutex<Option<Uuid>>`) and sets `tenant_id = None`.
  All owner resolution and user-row creation are deferred to `cogneeWarm` / first `services()`.

  **Owner bootstrap — Python default-user semantics (resolved).** The handle's `owner_id` is the
  id of the `User` row returned by `get_or_create_default_user(db, &settings.default_user_email)`,
  i.e. `uuid5(NAMESPACE_OID, default_user_email)`. This is deliberately **not** the CLI's
  `settings.default_user_id` parse: the binding tracks Python's model, where the owning user is
  keyed by email and a real `User` row is guaranteed to exist. Because creating/fetching that row is
  async DB work, `owner_id` is only known **after warm completes** — see `cogneeOwnerId` below.
- `cogneeWarm(handle) -> Promise<void>` — force `services()` to build now (async), surfacing
  config/connection errors early instead of on first op. This is where `get_or_create_default_user`
  runs (inside `CogneeServices::build`) and where the handle's `owner_id` mutex is populated.
- `cogneeOwnerId(handle) -> Promise<string>` — **async** (returns a `Promise`, not a sync string),
  because the owner id is email-derived and requires the user row to be resolved. It calls
  `handle.services().await?` (warming on demand if not already warmed), which guarantees `owner_id`
  is populated, then returns the resolved UUID string. Calling it without a prior `cogneeWarm` is
  fine — it warms lazily. (Rationale for making it a `Promise`: the alternative — a sync getter that
  returns the Settings `default_user_id` until warmed and the email-derived id afterward — would
  return two different values for the same handle over its lifetime, which is a footgun. A single
  always-correct async accessor is the cleaner contract.)

The **canonical pattern** all later `sdk_*` functions follow:
```
let svc = handle.services().await?;
let out = <cognee-lib api>(…, svc.add_pipeline.clone(), svc.llm.clone(), …).await?;
serde_to_js(&mut cx, &out)
```

## Runtime

Reuse the existing global tokio runtime (`runtime.rs`: `runtime()` returns the `OnceLock` runtime
and `expect()`s that `init()` ran first). The handle must not require a separate `init()`.

**Current `init()` is NOT idempotent** — it does `RUNTIME.set(...)` and throws
`"runtime already initialised"` on a second call (`runtime.rs:30`). Two safe fixes (pick one in
implementation; this is a small, internal change, not a user-facing fork):
1. Make `init`/`init_with_threads` idempotent — treat an already-set runtime as success (return
   `undefined` instead of throwing), so the TS layer can always call `init()` lazily.
2. Add an internal `ensure_runtime()` helper that initialises the `OnceLock` on first use (via
   `get_or_init`) and have `cogneeNew`/`cogneeWarm` call it, leaving the existing exported `init`
   semantics for the legacy engine path untouched.

Either way, `cogneeNew` must not assume `init()` was already called. Async native functions
(`cogneeWarm`, all Phase 3–6 `sdk_*`) must use the proven bridge from `pipeline_exec.rs`:
`let (deferred, promise) = cx.promise(); let channel = cx.channel();
runtime().spawn(async move { …; deferred.settle_with(&channel, move |mut cx| { … }) }); Ok(promise)`.

## Dependencies & ordering

Needs Phase 0. Provides the foundation for Phases 2–6. The single most important phase.

## Risks

- Services hold live DB connections + the embedding model in memory — define lifecycle and
  `Finalize` behavior. `CogneeHandle` must impl `Finalize` (it owns `Arc<ComponentManager>` and the
  cached `Arc<CogneeServices>`); `Finalize`'s default no-op drop is fine — the `Arc`s drop when the
  `JsBox` is GC'd. Confirm `ComponentManager`/`CogneeServices` are `Send + Sync` (all fields are
  `Arc<dyn … Send + Sync>` / `Arc<TokioRwLock<…>>`, so they are).
- Owner bootstrap requires a working relational DB — **but only at warm time, not construction**.
  The sync ctor leaves `owner_id = None`; the email-keyed `get_or_create_default_user` bootstrap
  (which creates the `User` row and yields `owner_id`) runs inside `CogneeServices::build` during
  `cogneeWarm`/first `services()`. Consequence: `cogneeOwnerId` is async (see Functionalities).
- Thread-safety: `Arc` everywhere; guard the services cache with a `tokio::Mutex<Option<(u64,
  Arc<CogneeServices>)>>` (store the config version alongside the cached `Arc`, mirroring
  `ComponentManager`'s versioned pattern).
- A blocking constructor: Neon `JsBox` ctors run on the JS thread and cannot `.await`. Keep
  `cogneeNew` pure/sync; push all I/O into `cogneeWarm`/`sdk_*` promises.

## Done when

- A handle constructs synchronously from TS (no I/O) and survives across calls; `owner_id` starts
  unresolved (`None`) and is populated on warm.
- `cogneeWarm` builds `services()` (async, via the runtime), runs
  `get_or_create_default_user(db, default_user_email)` (creating the `User` row and populating the
  handle's `owner_id`), and surfaces config/connection errors; a subsequent config-version bump
  triggers a rebuild on the next `services()` call (cache stores `(version, Arc<CogneeServices>)`).
- `cogneeOwnerId` (async) resolves to the email-derived UUID, warming lazily if needed, and the
  returned id equals `uuid5(NAMESPACE_OID, settings.default_user_email)`.
- `init()` (or an internal `ensure_runtime`) is safe to call when the runtime is already
  initialised — no "already initialised" throw on the handle path.
- A Tier-A test constructs a handle and `cogneeWarm`s it with `MOCK_EMBEDDING=true` and a temp
  data dir (no LLM needed). `await cogneeOwnerId(handle)` resolves to a valid UUID equal to
  `uuid5(NAMESPACE_OID, settings.default_user_email)`, and a `User` row for that email exists in the
  relational DB after warm. The test must skip cleanly in CI
  where no LLM/embedding env is present (warming with `MOCK_EMBEDDING=true` avoids the embedding
  model download; the LLM engine is only constructed lazily and need not be exercised here — but
  note `cm.llm()` errors if `llm_api_key` is empty, so warming the LLM engine specifically must be
  guarded or skipped when no key is set).
