//! ComponentManager: lazy-initializing, shared component store.
//!
//! Construction logic lives in `cognee-components`; this type owns the
//! version-keyed cache and delegates each backend build to a
//! [`ComponentRegistry`]. Supply a custom registry via [`ComponentManager::with_registry`]
//! to plug in external adapter factories;
//! [`ComponentManager::new`] uses [`ComponentRegistry::with_builtins`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock as TokioRwLock;
use tracing::instrument;

use cognee_components::{ComponentRegistry, DatasetStorage};
use cognee_database::RelationalDb;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, Transcriber};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

use crate::config::{ConfigManager, Settings};
use crate::context::PipelineContext;
use crate::error::ComponentError;

/// Whether a teardown may call `close()` on state that surviving clones can
/// observe.
///
/// A store's `close()` mutates the object behind the `Arc` (an embedded graph
/// empties its inner handle, a pool flags itself closed), so it is visible to
/// every clone. The explicit teardown is entitled to that; the implicit one is
/// not, and closes a component only when the cache holds its last reference —
/// the case where nobody can tell the difference. See
/// [`ComponentManager::close`] and [`ComponentManager::release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseSharedStores {
    Yes,
    No,
}

impl CloseSharedStores {
    /// Whether `component` may be closed under this policy.
    ///
    /// `strong_count == 1` means the reference passed in (already taken out of the
    /// cache) is the only one left, so a `close()` is unobservable. The count can
    /// only be raced *upwards* by a task that already holds a clone, and such a
    /// task is exactly the one we are protecting, so a stale-high count errs
    /// toward leaving the component open — the safe direction.
    fn may_close<T: ?Sized>(self, component: &Arc<T>) -> bool {
        match self {
            CloseSharedStores::Yes => true,
            CloseSharedStores::No => Arc::strong_count(component) == 1,
        }
    }
}

/// Manages shared, lazily-initialized pipeline components.
///
/// Each component is created on first access and cached for subsequent calls.
/// When the underlying [`ConfigManager`]'s version advances (due to a setter
/// call), cached components are lazily re-created on the next access.
///
/// Backend construction is delegated to a [`ComponentRegistry`]; the cache
/// policy and the transcriber's bespoke `Option<Arc>` slot live here.
pub struct ComponentManager {
    config: ConfigManager,
    registry: ComponentRegistry,
    // Each cached component stores (version_at_creation, component_arc).
    // When the config version advances past the cached version, the
    // component is lazily re-created on next access.
    storage: TokioRwLock<Option<(u64, Arc<dyn StorageTrait>)>>,
    database: TokioRwLock<Option<(u64, Arc<dyn RelationalDb>)>>,
    graph_db: TokioRwLock<Option<(u64, Arc<dyn GraphDBTrait>)>>,
    vector_db: TokioRwLock<Option<(u64, Arc<dyn VectorDB>)>>,
    embedding_engine: TokioRwLock<Option<(u64, Arc<dyn EmbeddingEngine>)>>,
    llm: TokioRwLock<Option<(u64, Arc<dyn Llm>)>>,
    // Stores Option<Arc<dyn Transcriber>>: None when the provider does not
    // support transcription (e.g. litert). The outer Option<(ver, ...)> is
    // the version-keyed cache envelope.
    #[allow(clippy::type_complexity)]
    transcriber: TokioRwLock<Option<(u64, Option<Arc<dyn Transcriber>>)>>,
    // Per-dataset storage (`enable_access_control`), behind the routing graph
    // and vector engines; `None` inside when the flag is off.
    #[allow(clippy::type_complexity)]
    dataset_storage: TokioRwLock<Option<(u64, Option<Arc<DatasetStorage>>)>>,
    // Version-keyed cache of the lowered build context, so `Settings::backend_context`
    // (env reads + the Postgres credential-fallback warning) runs once per config
    // version instead of once per component (7×).
    context: TokioRwLock<Option<(u64, cognee_components::BackendBuildContext)>>,
    // This manager's resolved `llm.api_key`, registered with product analytics
    // (hashed into `api_key_tracking_id`) for as long as the manager lives.
    // The configured key (env, config file or code), never a raw env read.
    telemetry_key: cognee_telemetry::LlmKeyRegistration,
}

impl ComponentManager {
    /// Construct with the OSS built-in registry
    /// ([`ComponentRegistry::with_builtins`]).
    pub fn new(config: ConfigManager) -> Self {
        Self::with_registry(config, ComponentRegistry::with_builtins())
    }

    /// Construct with an explicit registry. Use this to inject external adapter
    /// factories (register them on the registry before passing it in).
    ///
    /// The registry's config sections (see `ComponentRegistry::config_schema`)
    /// are added to `config`, so the settings of every registered factory,
    /// built-in or not, are settable through it. A collision between two
    /// sections is a programming error in the registering crates; it is logged
    /// and the colliding registry's sections are not added.
    pub fn with_registry(config: ConfigManager, registry: ComponentRegistry) -> Self {
        match registry.config_schema() {
            Ok(schema) => {
                if let Err(error) = config.extend_schema(&schema) {
                    tracing::error!(%error, "component config sections not registered");
                }
            }
            Err(error) => tracing::error!(%error, "component config sections not registered"),
        }
        let telemetry_key =
            cognee_telemetry::LlmKeyRegistration::new(Some(&config.read().llm().api_key));
        Self {
            config,
            registry,
            storage: TokioRwLock::new(None),
            database: TokioRwLock::new(None),
            graph_db: TokioRwLock::new(None),
            vector_db: TokioRwLock::new(None),
            embedding_engine: TokioRwLock::new(None),
            llm: TokioRwLock::new(None),
            transcriber: TokioRwLock::new(None),
            dataset_storage: TokioRwLock::new(None),
            context: TokioRwLock::new(None),
            telemetry_key,
        }
    }

    /// Read-only snapshot of current settings.
    ///
    /// Returns a `RwLockReadGuard` that auto-derefs to `&Settings`.
    /// Most call sites that use `cm.settings().field_name` work unchanged.
    pub fn settings(&self) -> std::sync::RwLockReadGuard<'_, Settings> {
        self.config.read()
    }

    /// Access the underlying [`ConfigManager`] for runtime mutation.
    pub fn config(&self) -> &ConfigManager {
        &self.config
    }

    /// Access the component registry (e.g. to inspect registered providers).
    pub fn registry(&self) -> &ComponentRegistry {
        &self.registry
    }

    /// Return the lowered build context for the current config version.
    ///
    /// Cached per config version: `Settings::backend_context` reads several env
    /// vars and may emit the Postgres credential-fallback warning, so building it
    /// once per version (rather than once per component) avoids duplicated work
    /// and duplicated warnings. The returned owned context is `Send`, so binding
    /// it to a local before an `.await` keeps the delegating futures `Send`.
    ///
    /// Fails when the configuration holds a value this build does not support
    /// ([`ConfigManager::check_supported`]: `enable_access_control = true`
    /// without application-provided access control), so no component is ever
    /// built from such a configuration, whether or not the caller ran
    /// [`ConfigManager::check_strict`].
    async fn build_context(
        &self,
    ) -> Result<cognee_components::BackendBuildContext, ComponentError> {
        self.config
            .check_supported()
            .map_err(|e| ComponentError::Config(e.to_string()))?;
        let current_ver = self.config.version();
        {
            let guard = self.context.read().await;
            if let Some((ver, ctx)) = &*guard
                && *ver == current_ver
            {
                return Ok(ctx.clone());
            }
        }
        let mut guard = self.context.write().await;
        if let Some((ver, ctx)) = &*guard
            && *ver == current_ver
        {
            return Ok(ctx.clone());
        }
        // No `.await` between the config read and storing the result, so the
        // (non-`Send`) settings guard never crosses an await point.
        let ctx = self.lower();
        *guard = Some((current_ver, ctx.clone()));
        Ok(ctx)
    }

    /// Lower the current settings into a build context, with the keyless
    /// embedding switch applied. The one place a context is lowered: callers
    /// cache the result per config version.
    fn lower(&self) -> cognee_components::BackendBuildContext {
        let settings = self.config.read();
        // The configuration changed (or this is the first build): keep the
        // key reported by product analytics in step with it.
        self.telemetry_key.update(Some(&settings.llm().api_key));
        let mut ctx = settings.backend_context();
        if self.keyless(&settings, &ctx) && !settings.embedding_settings_configured() {
            apply_local_embedding(&mut ctx);
        }
        ctx
    }

    // Each `init_*` below carries an INFO span so that engine construction —
    // the bulk of a cold `warm()` — is visible in a trace.
    //
    // The spans go on these private constructors rather than on the public
    // accessors because the accessors run `versioned_accessor!`, whose read-lock
    // fast path returns a cached `Arc`. Instrumenting there would emit a span per
    // cache *hit* — thousands per pipeline run, all ~0s — drowning the handful
    // that represent real work. `init_*` runs only on the slow path: once per
    // component per config version, which is exactly the event worth timing.

    #[instrument(name = "cognee.component.storage", level = "info", skip_all, err)]
    async fn init_storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.build_storage(&ctx).await
    }

    #[instrument(name = "cognee.component.database", level = "info", skip_all, err)]
    async fn init_database(&self) -> Result<Arc<dyn RelationalDb>, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.build_database(&ctx).await
    }

    /// The shared graph store, or — with `enable_access_control` — the engine
    /// that routes every call to the current dataset's own graph.
    #[instrument(name = "cognee.component.graph_db", level = "info", skip_all, err)]
    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        if let Some(storage) = self.dataset_storage().await? {
            return Ok(storage.graph_db());
        }
        let ctx = self.build_context().await?;
        self.registry.build_graph(&ctx).await
    }

    /// The shared vector store, or the per-dataset routing engine; see
    /// [`Self::init_graph_db`].
    #[instrument(name = "cognee.component.vector_db", level = "info", skip_all, err)]
    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        if let Some(storage) = self.dataset_storage().await? {
            return Ok(storage.vector_db());
        }
        let ctx = self.build_context().await?;
        self.registry.build_vector(&ctx).await
    }

    /// Per-dataset storage when `enable_access_control`
    /// (`ENABLE_BACKEND_ACCESS_CONTROL`) is on, else `None`: every dataset's
    /// own graph and vector database, provisioned on first use
    /// (`docs/design/per-dataset-storage.md`). The graph and vector engines
    /// this manager hands out then route each call to the dataset of the
    /// current [`cognee_models::dataset_scope`]; this handle is for what spans
    /// datasets — deleting one's storage, prune.
    ///
    /// Cached per config version, like the components.
    ///
    /// # Errors
    /// [`ComponentError::Config`] when the configured graph or vector
    /// provider has no dataset database handler (Python's
    /// `multi_user_support_possible`).
    pub async fn dataset_storage(&self) -> Result<Option<Arc<DatasetStorage>>, ComponentError> {
        let current_ver = self.config.version();
        {
            let guard = self.dataset_storage.read().await;
            if let Some((ver, storage)) = &*guard
                && *ver == current_ver
            {
                return Ok(storage.clone());
            }
        }
        // The relational database first, without holding this slot's lock:
        // its own accessor takes the database slot.
        let enabled = self.settings().enable_access_control;
        let database = if enabled {
            Some(self.database().await?)
        } else {
            None
        };
        let mut guard = self.dataset_storage.write().await;
        if let Some((ver, storage)) = &*guard
            && *ver == current_ver
        {
            return Ok(storage.clone());
        }
        let storage = match database {
            Some(database) => {
                let ctx = self.build_context().await?;
                // Kept across configuration versions while it still serves the
                // configuration: a second storage would open every dataset's
                // stores again beside the first (an embedded graph allows one
                // writer), and a delete through one would not evict the other.
                match guard.as_ref().and_then(|(_, storage)| storage.clone()) {
                    Some(storage) if storage.serves(&ctx) => Some(storage),
                    _ => Some(self.registry.build_dataset_storage(&ctx, database)?),
                }
            }
            None => None,
        };
        *guard = Some((current_ver, storage.clone()));
        Ok(storage)
    }

    #[instrument(
        name = "cognee.component.embedding_engine",
        level = "info",
        skip_all,
        err
    )]
    async fn init_embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.build_embedding(&ctx).await
    }

    #[instrument(name = "cognee.component.llm", level = "info", skip_all, err)]
    async fn init_llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.build_llm(&ctx).await
    }

    /// Whether the configured LLM can make calls: its provider needs no API
    /// key, or one is set. Without one, [`PipelineContext::llm`] returns an
    /// LLM whose every call fails with "LLM API key is not set.".
    pub async fn llm_available(&self) -> Result<bool, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.llm_available(&ctx)
    }

    /// Build the session store selected by `session.store` (default
    /// `relational`: the session tables in this manager's relational
    /// database). Not cached: each pipeline/service build owns its store.
    #[instrument(name = "cognee.component.session_store", level = "info", skip_all, err)]
    pub async fn session_store(
        &self,
    ) -> Result<Arc<dyn cognee_session::SessionStore>, ComponentError> {
        let database = self.database().await?;
        let ctx = self.build_context().await?;
        self.registry.build_session_store(&ctx, database).await
    }

    /// Build the ontology resolver selected by `ontology.resolver`, loading
    /// `file_path` when given (a per-call override such as the CLI's
    /// `--ontology-file`) or else `ontology.file_path`. No path means the
    /// no-op resolver. Not cached: loading is per pipeline build.
    #[instrument(name = "cognee.component.ontology", level = "info", skip_all, err)]
    pub async fn ontology_resolver(
        &self,
        file_path: Option<&str>,
    ) -> Result<Arc<dyn cognee_ontology::OntologyResolver>, ComponentError> {
        let ctx = self.build_context().await?;
        self.registry.build_ontology_resolver(&ctx, file_path).await
    }

    /// Python's `keyless_local_defaults_apply` over a lowered context: no usable
    /// LLM ([`ComponentRegistry::llm_available`]) and the keyless defaults not
    /// switched off ([`Settings::keyless_defaults_disabled`]).
    ///
    /// A provider nothing is registered under counts as usable when a key is
    /// set (Python: a provider it does not know needs one); building it then
    /// fails, naming what is registered.
    fn keyless(&self, settings: &Settings, ctx: &cognee_components::BackendBuildContext) -> bool {
        let available = self
            .registry
            .llm_available(ctx)
            .unwrap_or_else(|_| !ctx.llm.api_key.trim().is_empty());
        !settings.keyless_defaults_disabled() && !available
    }

    /// The lowered context for the current config version without awaiting:
    /// the cached one when it is current, else a fresh lowering, cached when
    /// the slot is free (so the async accessors do not lower it again).
    fn current_context(&self) -> cognee_components::BackendBuildContext {
        let current_ver = self.config.version();
        if let Ok(guard) = self.context.try_read()
            && let Some((ver, ctx)) = &*guard
            && *ver == current_ver
        {
            return ctx.clone();
        }
        let ctx = self.lower();
        if let Ok(mut guard) = self.context.try_write() {
            *guard = Some((current_ver, ctx.clone()));
        }
        ctx
    }

    /// The embedding inputs cognify runs with: the configured embedder, or
    /// the local model when the keyless defaults switched to it. Prefer this
    /// over `settings().embedding()` wherever the embedder in use matters
    /// (e.g. labelling exported vectors).
    pub fn effective_embedding(&self) -> cognee_components::EmbeddingInputs {
        self.current_context().embedding
    }

    /// Whether cognify runs on local models because no LLM is usable: the
    /// `auto` graph extractor then resolves to GLiNER, and an unconfigured
    /// embedder to the local ONNX model (Python's
    /// `keyless_local_defaults_apply`).
    pub fn keyless_local_defaults_apply(&self) -> bool {
        let ctx = self.current_context();
        let settings = self.settings();
        self.keyless(&settings, &ctx)
    }

    /// The graph extractor a cognify run uses (Python's `resolve_extractor`):
    /// `selected` (the caller's per-call choice) unless blank, else
    /// `graph_extraction.backend`, resolved by
    /// [`cognee_components::resolve_graph_extractor_kind`] with `auto` decided
    /// by [`Self::keyless_local_defaults_apply`].
    ///
    /// `also_registered` reports kinds served outside this manager's registry
    /// (the bindings' process-wide shim), so a backend registered under
    /// `gliner_demo` there is not redirected to `gliner` either.
    pub fn resolve_graph_extractor(
        &self,
        selected: Option<&str>,
        also_registered: &dyn Fn(&str) -> bool,
    ) -> GraphExtractorChoice {
        let selected = match selected {
            Some(kind) if !kind.trim().is_empty() => kind.to_string(),
            _ => self.settings().graph_extraction().backend,
        };
        let keyless = self.keyless_local_defaults_apply();
        let is_registered =
            |kind: &str| self.registry.has_graph_extractor(kind) || also_registered(kind);
        let kind =
            cognee_components::resolve_graph_extractor_kind(&selected, keyless, &is_registered);
        let from_auto = cognee_components::is_auto_graph_extractor(&selected);
        if from_auto && kind == cognee_components::GLINER_GRAPH_EXTRACTOR {
            static NOTICE: std::sync::Once = std::sync::Once::new();
            NOTICE.call_once(|| {
                tracing::info!(
                    "no usable LLM is configured: the knowledge graph is extracted with the \
                     local GLiNER extractor"
                );
            });
        }
        GraphExtractorChoice { kind, from_auto }
    }

    /// Build the graph-extraction backend for one cognify run.
    ///
    /// `kind` is the caller's selection, or `graph_extraction.backend` when
    /// `None`; it is resolved by [`Self::resolve_graph_extractor`]. `Ok(None)`
    /// means cognify's own LLM extractor (`llm`). The factory sees the
    /// resolved kind: as the spec when `spec` is a bare string, as its `kind`
    /// when it is an object.
    ///
    /// # Errors
    /// [`ComponentError::GraphExtractor`] when no backend is registered under
    /// the resolved kind (never a silent fallback to the LLM extractor), or
    /// whatever the factory returns.
    pub async fn graph_extractor(
        &self,
        kind: Option<&str>,
        spec: &serde_json::Value,
    ) -> Result<Option<Arc<dyn cognee_cognify::graph_backend::ChunkGraphExtractor>>, ComponentError>
    {
        let choice = self.resolve_graph_extractor(kind, &|_| false);
        if choice.is_llm() {
            return Ok(None);
        }
        let spec = choice.spec_for(spec);
        let ctx = self.build_context().await?;
        match self
            .registry
            .build_graph_extractor(&ctx, &choice.kind, &spec)
            .await
        {
            Some(built) => built.map(Some),
            None if choice.from_auto => Err(ComponentError::GraphExtractor(
                cognee_components::keyless_extractor_unavailable(
                    &self.registry.graph_extractor_kinds(),
                ),
            )),
            None => Err(ComponentError::GraphExtractor(format!(
                "no graph extractor is registered for kind {:?}; registered: {}",
                choice.kind,
                self.registry.graph_extractor_kinds().join(", ")
            ))),
        }
    }

    /// The configured `graph_extraction.backend`, resolved and built — what a
    /// cognify run uses when the caller made no per-call selection.
    ///
    /// The resolved kind is passed to the factory as its spec (a bare
    /// string), the same shape the bindings use for the configured default.
    /// `Ok(None)` is cognify's own LLM extractor; every entry point that builds
    /// a [`cognee_cognify::CognifyConfig`] from configuration attaches a
    /// `Some` with `with_graph_backend`, so the key is honoured everywhere.
    ///
    /// # Errors
    /// As [`Self::graph_extractor`]: an unregistered kind is an error, never a
    /// silent fallback to the LLM extractor.
    pub async fn configured_graph_extractor(
        &self,
    ) -> Result<Option<Arc<dyn cognee_cognify::graph_backend::ChunkGraphExtractor>>, ComponentError>
    {
        self.graph_extractor(None, &serde_json::Value::String(String::new()))
            .await
    }

    /// Return the [`Transcriber`] for the configured LLM provider, if supported.
    ///
    /// Returns `Ok(Some(_))` for OpenAI-compatible providers that expose audio
    /// transcription; `Ok(None)` for providers that do not (e.g. `litert`), so
    /// callers can skip registering the `AudioLoader` rather than failing.
    pub async fn transcriber(&self) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        let current_ver = self.config.version();
        // Fast path: read lock
        {
            let guard = self.transcriber.read().await;
            if let Some((ver, ref opt)) = *guard
                && ver == current_ver
            {
                return Ok(opt.clone());
            }
        }
        // Slow path: write lock with double-check
        let mut guard = self.transcriber.write().await;
        if let Some((ver, ref opt)) = *guard
            && ver == current_ver
        {
            return Ok(opt.clone());
        }
        let ctx = self.build_context().await?;
        let new = self.registry.build_transcriber(&ctx).await?;
        *guard = Some((current_ver, new.clone()));
        Ok(new)
    }

    /// Release every cached component, so the OS resources they hold go away now
    /// instead of at an unspecified later time — or never.
    ///
    /// # Why every slot, not just the database
    ///
    /// The relational pool is the one that *cannot* be fixed by a drop: sqlx's
    /// destructor only flags the pool closed and lets its connections tear down
    /// concurrently, which for SQLite orphans the `-wal`/`-shm` sidecars (see
    /// [`cognee_database::close`], topoteretes/cognee-rs#132). It is closed
    /// explicitly, **first** — see the ordering note below.
    ///
    /// The other slots do release their resources on drop — but this cache **is
    /// the last strong reference**, so leaving them cached means they are never
    /// dropped and therefore never released. Measured on a warm manager: the
    /// embedded graph keeps an un-checkpointed `.wal` and a write lock on its
    /// database file; each `reqwest`-based engine keeps its idle keep-alive
    /// connections (one embedding pool, one LLM pool and a second, easily-missed
    /// LLM pool inside the transcriber); the ONNX embedding engine keeps an
    /// `ort::Session` whose destructor joins its worker threads. Emptying the
    /// slots is what runs those destructors.
    ///
    /// Slots that own something *closable* (graph, vector) get an awaited
    /// `close()` before the drop; the rest are just dropped. Both the graph
    /// `close()` and the plain drops run on [`tokio::task::spawn_blocking`],
    /// which is load-bearing rather than hygiene: the `ort::Session` destructor
    /// joins worker threads and the embedded graph's destructor runs a
    /// synchronous checkpoint, so dropping either inline would block a runtime
    /// worker for as long as that takes.
    ///
    /// The vector slot is dropped and closed like the rest, but note that the
    /// in-tree vector backends were **measured to own nothing closable**: the
    /// brute-force store is in-memory, and LanceDB holds no descriptor open
    /// between calls (its manifest writes are crash-safe renames). They inherit
    /// the no-op default [`cognee_vector::VectorDB::close`] deliberately — do not
    /// bolt a close tier onto LanceDB on the assumption that it must need one.
    /// Lance does install a process-lifetime CPU thread pool that no `close()`
    /// can reclaim; those threads are not a leak and not a regression.
    ///
    /// # Reuse and ordering
    ///
    /// Every component is **taken out of the cache** before it is closed, so this
    /// leaves the manager reusable — just cold: the next access rebuilds. Calling
    /// this twice is a no-op the second time, and calling it on a manager that
    /// never warmed does nothing at all. The slots are taken **one at a time**
    /// (acquire → `take()` → release), so no two write guards are ever held at
    /// once and none is held across an `.await`: the lock order against
    /// `services()` / `build_context` is unchanged and this cannot deadlock.
    ///
    /// **The relational close goes first, and that ordering is load-bearing.**
    /// Callers bound this teardown — the CLI wraps it in a `timeout` because a
    /// pool close waits for connections to come back and a command's runtime may
    /// have been dropped with one still checked out. Whatever runs first is the
    /// part that survives a budget that runs out, so the slot whose leak started
    /// all of this (the SQLite sidecars of #132) is closed before the graph
    /// checkpoint, the vector pool, and the ONNX thread join get their turn.
    /// Putting it last, as the first version of this did, let a timeout skip
    /// exactly the close the fix existed for.
    ///
    /// Nothing is mid-write against the relational pool by this point: the caller
    /// has already dropped the service bundle (see `HandleState::teardown`), and
    /// the HTTP server drains its pipeline registry before calling this. The
    /// stores are independent systems — the graph and vector backends do not write
    /// through this pool — so closing it first cannot cut a store's own teardown
    /// short.
    ///
    /// # Cost
    ///
    /// This is no longer a cheap relational reset. A caller that closes and then
    /// keeps using the manager pays a full re-warm: a fresh TLS handshake per HTTP
    /// engine, and for the ONNX provider a re-read of the model file.
    ///
    /// Failures are logged rather than returned: the caller is tearing the manager
    /// down and has no remedy, and the component is unusable either way.
    pub async fn close(&self) {
        self.teardown(CloseSharedStores::Yes).await;
    }

    /// Whether any component is currently cached — i.e. whether a teardown would
    /// have anything to release.
    ///
    /// Cheap and **non-blocking**, for a synchronous finalizer deciding whether to
    /// spin up a runtime at all. Every slot is probed, not just one: warming is
    /// slot-by-slot and can fail part-way (a bad LLM key fails after the SQLite
    /// pool and the embedded graph are already cached), and a probe that only
    /// looked at one slot would report "nothing to release" for a handle holding
    /// an open database.
    ///
    /// A contended lock counts as occupied: the safe direction is to attempt a
    /// teardown that turns out to be a no-op rather than to skip a real one.
    pub fn has_cached_components(&self) -> bool {
        fn occupied<T>(slot: &TokioRwLock<Option<T>>) -> bool {
            match slot.try_read() {
                Ok(guard) => guard.is_some(),
                Err(_) => true,
            }
        }

        occupied(&self.database)
            || occupied(&self.graph_db)
            || occupied(&self.vector_db)
            || occupied(&self.embedding_engine)
            || occupied(&self.llm)
            || occupied(&self.transcriber)
            || occupied(&self.storage)
    }

    /// Evict every cached component **without** closing anything that is still
    /// shared, for the implicit teardown paths (a GC finalizer reclaiming a handle
    /// the program dropped on the floor).
    ///
    /// The difference from [`close`](Self::close) is only about *shared* state. A
    /// store's `close()` mutates the object behind the `Arc` — `LadybugAdapter`
    /// empties its inner slot, a Postgres adapter closes its pool, sqlx flags
    /// `PoolInner` closed — so it is visible to **every** surviving clone, and an
    /// in-flight operation holding one would fail its next query. `close()` is
    /// entitled to do that because the user asked; a finalizer is not.
    ///
    /// So this closes a component only when the cache holds its **last** strong
    /// reference, which is precisely the case where nobody can observe the
    /// difference. In the ordinary finalizer case — no operation in flight — that
    /// is every slot, so the resources (including the SQLite sidecars) are
    /// released exactly as `close()` would release them. When an operation *is* in
    /// flight, its components are left open and released when it finishes and its
    /// clone drops.
    ///
    /// The one thing not covered by that rule is the drop itself: a slot whose
    /// last reference this evicts is dropped on the blocking pool as usual.
    pub async fn release(&self) {
        self.teardown(CloseSharedStores::No).await;
    }

    /// Shared body of [`close`](Self::close) / [`release`](Self::release).
    async fn teardown(&self, shared: CloseSharedStores) {
        // Take each slot out sequentially. Never two guards at once, never a
        // guard across an `.await`.
        //
        // The **database slot goes first**, and the order is load-bearing rather
        // than cosmetic. Every `write().await` here can block on a concurrent
        // accessor, so acquiring the other six before it meant a caller that
        // bounds this teardown with a timeout (the CLI does) could spend its whole
        // budget waiting for, say, the graph lock and never reach the relational
        // close at all — leaving exactly the SQLite `-wal`/`-shm` sidecars the
        // bound exists to remove (topoteretes/cognee-rs#132). Taking it first
        // means the one teardown with an on-disk consequence is never queued
        // behind another slot's contention.
        // Take the relational slot AND close it before touching any other lock.
        //
        // Taking it first is not enough, and getting that wrong is worse than the
        // original order: `versioned_accessor!` holds a slot's write lock across
        // the whole `init_*().await`, so a concurrent warm (a cold ONNX session
        // load, a Postgres connect burning its timeout) can block the *next*
        // acquisition for seconds. With the close deferred until after all seven
        // takes, a caller that bounds this teardown — `cognee_cli::teardown` wraps
        // it in a 5 s timeout — can have its budget expire while blocked on, say,
        // the embedding lock. The future is dropped, `cognee_database::close`
        // never runs, and the SQLite `-wal`/`-shm` sidecars survive: the #132 leak
        // this method exists to fix. And because the slot was already emptied, no
        // later `close()` can reach that pool either — it is orphaned for the life
        // of the process.
        //
        // So the relational close is fully sequenced here, before any other lock
        // is requested, and cancellation past this point can only cost the
        // components that have no on-disk consequence.
        if let Some((_, db)) = self.database.write().await.take() {
            if shared.may_close(&db)
                && let Err(e) = db.close().await
            {
                tracing::warn!(error = %e, "failed to close the relational connection pool");
            }
            // The connection has no blocking destructor of its own, so dropping
            // it inline is fine.
            drop(db);
        }

        let graph = self.graph_db.write().await.take();
        let vector = self.vector_db.write().await.take();
        let embedding = self.embedding_engine.write().await.take();
        let llm = self.llm.write().await.take();
        let transcriber = self.transcriber.write().await.take();
        let storage = self.storage.write().await.take();
        // Closed through the routing graph and vector engines below.
        let _ = self.dataset_storage.write().await.take();
        // The lowered build context holds no OS resource, but a stale one would
        // outlive the config version it was built for; clear it with the rest.
        let _ = self.context.write().await.take();

        if let Some((_, graph)) = graph {
            if shared.may_close(&graph)
                && let Err(e) = graph.close().await
            {
                tracing::warn!(error = %e, "failed to close the graph database");
            }
            Self::drop_off_runtime(graph, "graph database").await;
        }

        if let Some((_, vector)) = vector {
            if shared.may_close(&vector)
                && let Err(e) = vector.close().await
            {
                tracing::warn!(error = %e, "failed to close the vector database");
            }
            Self::drop_off_runtime(vector, "vector database").await;
        }

        if let Some((_, embedding)) = embedding {
            Self::drop_off_runtime(embedding, "embedding engine").await;
        }
        if let Some((_, llm)) = llm {
            Self::drop_off_runtime(llm, "llm").await;
        }
        if let Some((_, Some(transcriber))) = transcriber {
            Self::drop_off_runtime(transcriber, "transcriber").await;
        }
        if let Some((_, storage)) = storage {
            Self::drop_off_runtime(storage, "storage").await;
        }
    }

    /// Run a component's destructor on the blocking pool and wait for it.
    ///
    /// Not hygiene: the destructors behind these `Arc`s block. An `ort::Session`
    /// joins its inference worker threads and an embedded graph runs a
    /// synchronous WAL checkpoint, either of which would stall an async worker if
    /// dropped inline. Awaiting the join (instead of firing and forgetting) is
    /// what makes a subsequent re-warm on the same path safe — it must not race
    /// the destructor it is replacing.
    ///
    /// A `JoinError` is only reachable if the destructor itself panicked; log it
    /// and continue, since the caller is tearing down and has no remedy.
    async fn drop_off_runtime<T: Send + Sync + ?Sized + 'static>(
        component: Arc<T>,
        what: &'static str,
    ) {
        if let Err(e) = tokio::task::spawn_blocking(move || drop(component)).await {
            tracing::warn!(error = %e, component = what, "component destructor panicked during close");
        }
    }
}

/// Attach a [`ComponentManager`]'s configured [`Transcriber`] as one more step
/// of a builder chain.
pub trait WithTranscriber: Sized {
    /// `self` with the configured transcriber attached, so `add` runs the audio
    /// loader.
    ///
    /// A provider without transcription leaves `self` unchanged, so audio
    /// inputs fail with `UnsupportedDocumentType`, as without the
    /// `audio-loader` feature. A transcriber that cannot be built (no API key,
    /// say) is replaced by one that fails with the build error: a text `add`
    /// needs no LLM, stays quiet and must not fail over one, while an audio
    /// input says what is missing.
    fn with_configured_transcriber(
        self,
        cm: &ComponentManager,
    ) -> impl Future<Output = Self> + Send;
}

impl WithTranscriber for cognee_ingestion::AddPipeline {
    #[cfg_attr(not(feature = "audio-loader"), allow(unused_variables))]
    async fn with_configured_transcriber(self, cm: &ComponentManager) -> Self {
        #[cfg(feature = "audio-loader")]
        match cm.transcriber().await {
            Ok(Some(transcriber)) => return self.with_transcriber(transcriber),
            Ok(None) => {}
            Err(error) => {
                return self.with_transcriber(Arc::new(UnavailableTranscriber(error.to_string())));
            }
        }
        self
    }
}

/// Stands in for a transcriber that could not be built, so that only an audio
/// input fails, with the reason.
#[cfg(feature = "audio-loader")]
struct UnavailableTranscriber(String);

#[cfg(feature = "audio-loader")]
#[async_trait]
impl Transcriber for UnavailableTranscriber {
    async fn transcribe_audio(
        &self,
        _audio: &[u8],
        _format: &str,
        _language_hint: Option<&str>,
        _prompt_hint: Option<&str>,
    ) -> cognee_llm::LlmResult<cognee_llm::TranscriptionOutput> {
        Err(cognee_llm::LlmError::ConfigError(format!(
            "audio needs a transcriber, which could not be built: {}",
            self.0
        )))
    }

    fn transcription_model(&self) -> &str {
        ""
    }
}

// Versioned accessor helper macro — avoids repeating the double-checked
// locking pattern for each component.
macro_rules! versioned_accessor {
    ($self:ident, $field:ident, $init_fn:ident) => {{
        let current_ver = $self.config.version();
        // Fast path: read lock to check cache hit
        {
            let guard = $self.$field.read().await;
            if let Some((ver, ref component)) = *guard {
                if ver == current_ver {
                    return Ok(Arc::clone(component));
                }
            }
        }
        // Slow path: write lock to reinitialize
        let mut guard = $self.$field.write().await;
        // Double-check (another task may have reinitialized while we waited)
        if let Some((ver, ref component)) = *guard {
            if ver == current_ver {
                return Ok(Arc::clone(component));
            }
        }
        let new = $self.$init_fn().await?;
        *guard = Some((current_ver, Arc::clone(&new)));
        Ok(new)
    }};
}

#[async_trait]
impl PipelineContext for ComponentManager {
    async fn storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        versioned_accessor!(self, storage, init_storage)
    }

    async fn database(&self) -> Result<Arc<dyn RelationalDb>, ComponentError> {
        versioned_accessor!(self, database, init_database)
    }

    async fn graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        versioned_accessor!(self, graph_db, init_graph_db)
    }

    async fn vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        versioned_accessor!(self, vector_db, init_vector_db)
    }

    async fn embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        versioned_accessor!(self, embedding_engine, init_embedding_engine)
    }

    async fn llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        versioned_accessor!(self, llm, init_llm)
    }
}

/// A resolved graph-extractor selection: see
/// [`ComponentManager::resolve_graph_extractor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExtractorChoice {
    /// The kind to build; [`cognee_components::LLM_GRAPH_EXTRACTOR`] is
    /// cognify's own LLM extractor.
    pub kind: String,
    /// Reached through `auto`: a missing backend is then a keyless-setup
    /// problem (no usable LLM), not an unknown kind the caller named.
    pub from_auto: bool,
}

impl GraphExtractorChoice {
    /// Whether this is cognify's own LLM extractor, the only one that needs
    /// the LLM to build a graph.
    #[must_use]
    pub fn is_llm(&self) -> bool {
        self.kind == cognee_components::LLM_GRAPH_EXTRACTOR
    }

    /// `spec` as the factory receives it: a bare string becomes the resolved
    /// kind, an object gets the resolved kind as its `kind`, so the factory
    /// never sees `auto` or an alias however the caller spelled it.
    #[must_use]
    pub fn spec_for(&self, spec: &serde_json::Value) -> serde_json::Value {
        match spec {
            serde_json::Value::Object(map) => {
                let mut map = map.clone();
                map.insert(
                    "kind".to_string(),
                    serde_json::Value::String(self.kind.clone()),
                );
                serde_json::Value::Object(map)
            }
            _ => serde_json::Value::String(self.kind.clone()),
        }
    }
}

/// Switch `ctx`'s embedder to the local ONNX model: the embedding half of
/// cognify's keyless defaults (Python's `resolve_embedding_defaults`), applied
/// only when no embedding setting was configured.
fn apply_local_embedding(ctx: &mut cognee_components::BackendBuildContext) {
    let (provider, model, dimensions) = cognee_components::LOCAL_EMBEDDING;
    tracing::info!(
        provider,
        model,
        "no usable LLM and no embedding settings are configured: embeddings run on the \
         local model"
    );
    ctx.embedding.provider = provider.to_string();
    ctx.embedding.model = model.to_string();
    ctx.embedding.dimensions = dimensions as usize;
    ctx.embedding.endpoint = None;
    ctx.embedding.api_key = None;
    ctx.embedding_dimensions = dimensions as usize;
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::config::{ConfigManager, Settings};

    /// A registry that also serves a stub `gliner` graph extractor.
    fn registry_with_gliner() -> ComponentRegistry {
        struct GlinerStub;
        #[async_trait]
        impl cognee_components::GraphExtractorFactory for GlinerStub {
            fn provider(&self) -> &str {
                cognee_components::GLINER_GRAPH_EXTRACTOR
            }
            async fn build(
                &self,
                _ctx: &cognee_components::BackendBuildContext,
                spec: &serde_json::Value,
            ) -> Result<Arc<dyn cognee_cognify::graph_backend::ChunkGraphExtractor>, ComponentError>
            {
                // The factory sees the resolved kind, never `auto` or the alias.
                assert_eq!(spec, &serde_json::json!("gliner"));
                Ok(Arc::new(
                    cognee_cognify::graph_backend::MockChunkGraphExtractor::new(),
                ))
            }
        }
        let mut registry = ComponentRegistry::with_builtins();
        registry
            .register_graph_extractor(Arc::new(GlinerStub))
            .unwrap();
        registry
    }

    /// A manager over stock settings: no LLM key, no embedding setting.
    fn keyless_cm(registry: ComponentRegistry) -> ComponentManager {
        ComponentManager::with_registry(ConfigManager::new(Settings::default()), registry)
    }

    /// Python's `resolve_extractor` cases: `auto` (the default) follows
    /// whether an LLM is usable, an explicit selection beats
    /// `GRAPH_EXTRACTOR`, `gliner_demo` is `gliner`, and any other kind is
    /// passed through for the registry to judge.
    #[test]
    fn the_extractor_resolves_like_python() {
        let cm = keyless_cm(ComponentRegistry::with_builtins());
        assert_eq!(cm.settings().graph_extraction().backend, "auto");
        assert!(cm.keyless_local_defaults_apply());
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "gliner");
        assert_eq!(
            cm.resolve_graph_extractor(Some(" AUTO "), &|_| false).kind,
            "gliner"
        );
        assert_eq!(
            cm.resolve_graph_extractor(Some(""), &|_| false).kind,
            "gliner"
        );
        assert_eq!(
            cm.resolve_graph_extractor(Some("LLM"), &|_| false).kind,
            "llm"
        );
        assert_eq!(
            cm.resolve_graph_extractor(Some("gliner_demo"), &|_| false)
                .kind,
            "gliner"
        );
        assert_eq!(
            cm.resolve_graph_extractor(Some("custom-kind"), &|_| false)
                .kind,
            "custom-kind"
        );

        cm.config().set_llm_api_key("sk-test");
        assert!(!cm.keyless_local_defaults_apply());
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "llm");
        assert_eq!(
            cm.resolve_graph_extractor(Some("gliner"), &|_| false).kind,
            "gliner"
        );

        cm.config()
            .set("graph_extractor", serde_json::json!("gliner_demo"))
            .unwrap();
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "gliner");
        assert_eq!(
            cm.resolve_graph_extractor(Some("llm"), &|_| false).kind,
            "llm",
            "the per-call selection beats GRAPH_EXTRACTOR"
        );
    }

    /// The registry stays the authority: a backend registered under
    /// `gliner_demo` (here, or in the caller's own registry) is not redirected
    /// to `gliner`; `auto` cannot be registered; the choice records whether
    /// it came from `auto`, and the factory's spec carries the resolved kind.
    #[test]
    fn a_registered_kind_is_never_rewritten() {
        let cm = keyless_cm(ComponentRegistry::with_builtins());
        let served = |kind: &str| kind == "gliner_demo";
        assert_eq!(
            cm.resolve_graph_extractor(Some("gliner_demo"), &served)
                .kind,
            "gliner_demo"
        );

        let choice = cm.resolve_graph_extractor(None, &|_| false);
        assert!(choice.from_auto && !choice.is_llm());
        let choice = cm.resolve_graph_extractor(Some("gliner_demo"), &|_| false);
        assert!(!choice.from_auto);
        assert_eq!(
            choice.spec_for(&serde_json::json!({ "kind": "gliner_demo", "threshold": 0.4 })),
            serde_json::json!({ "kind": "gliner", "threshold": 0.4 })
        );
        assert_eq!(
            choice.spec_for(&serde_json::json!("x")),
            serde_json::json!("gliner")
        );

        struct AutoNamed;
        #[async_trait]
        impl cognee_components::GraphExtractorFactory for AutoNamed {
            fn provider(&self) -> &str {
                "Auto"
            }
            async fn build(
                &self,
                _ctx: &cognee_components::BackendBuildContext,
                _spec: &serde_json::Value,
            ) -> Result<Arc<dyn cognee_cognify::graph_backend::ChunkGraphExtractor>, ComponentError>
            {
                unreachable!("never registered")
            }
        }
        let mut registry = ComponentRegistry::with_builtins();
        assert!(
            registry
                .register_graph_extractor(Arc::new(AutoNamed))
                .is_err()
        );
    }

    /// An LLM that needs no key keeps `auto` on the LLM: the mock LLM, and
    /// Bedrock (AWS credentials).
    #[test]
    fn a_keyless_llm_provider_counts_as_usable() {
        let cm = keyless_cm(ComponentRegistry::with_builtins());
        cm.config()
            .set("llm_mock", serde_json::json!(true))
            .unwrap();
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "llm");

        let cm = keyless_cm(ComponentRegistry::with_builtins());
        cm.config()
            .set("llm_provider", serde_json::json!("bedrock"))
            .unwrap();
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "llm");

        // A key-requiring provider without its key is not usable.
        let cm = keyless_cm(ComponentRegistry::with_builtins());
        cm.config()
            .set("llm_provider", serde_json::json!("anthropic"))
            .unwrap();
        assert_eq!(cm.resolve_graph_extractor(None, &|_| false).kind, "gliner");
    }

    /// The preflight-skip switches and a mock embedder keep the given config:
    /// no rerouting to local models (Python's `_skip_preflight`).
    #[test]
    fn the_skip_switches_keep_the_llm_extractor() {
        for (key, value) in [
            ("skip_preflight", serde_json::json!(true)),
            ("skip_connection_test", serde_json::json!(true)),
            ("mock_embedding", serde_json::json!("true")),
        ] {
            let cm = keyless_cm(ComponentRegistry::with_builtins());
            cm.config().set(key, value).unwrap();
            assert!(!cm.keyless_local_defaults_apply(), "{key}");
            assert_eq!(
                cm.resolve_graph_extractor(None, &|_| false).kind,
                "llm",
                "{key}"
            );
        }
    }

    /// With no usable LLM, `auto` builds the registered GLiNER extractor; an
    /// explicit `llm` still means the built-in one.
    #[tokio::test]
    async fn auto_builds_gliner_without_an_llm() {
        let cm = keyless_cm(registry_with_gliner());
        assert!(cm.configured_graph_extractor().await.unwrap().is_some());
        assert!(
            cm.graph_extractor(Some("gliner_demo"), &serde_json::json!("gliner_demo"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            cm.graph_extractor(Some("llm"), &serde_json::json!("llm"))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// `auto` never falls back to the LLM extractor: with no LLM and no GLiNER
    /// registered it fails, saying how to fix the setup. A GLiNER selection
    /// made by name gets the plain unknown-kind error.
    #[tokio::test]
    async fn auto_without_gliner_is_a_keyless_setup_error() {
        let cm = keyless_cm(ComponentRegistry::with_builtins());
        let Err(error) = cm.configured_graph_extractor().await else {
            panic!("auto must not fall back to the LLM extractor");
        };
        let message = error.to_string();
        assert!(message.contains("no usable LLM is configured"), "{message}");
        assert!(message.contains("LLM_API_KEY"), "{message}");

        let Err(error) = cm
            .graph_extractor(Some("gliner"), &serde_json::json!("gliner"))
            .await
        else {
            panic!("nothing serves gliner");
        };
        assert!(
            error
                .to_string()
                .contains("no graph extractor is registered"),
            "{error}"
        );
    }

    /// The embedding half: with no usable LLM and no embedding setting, the
    /// embedder is the local ONNX model; any embedding setting, a usable LLM
    /// or a skip switch keeps the configured one.
    #[tokio::test]
    async fn keyless_embeddings_use_the_local_model_only_when_unconfigured() {
        let (local_provider, local_model, local_dims) = cognee_components::LOCAL_EMBEDDING;

        let cm = keyless_cm(ComponentRegistry::with_builtins());
        let ctx = cm.build_context().await.unwrap();
        assert_eq!(ctx.embedding.provider, local_provider);
        assert_eq!(ctx.embedding.model, local_model);
        assert_eq!(ctx.embedding.dimensions, local_dims as usize);
        assert_eq!(ctx.embedding_dimensions, local_dims as usize);
        assert!(ctx.embedding.api_key.is_none());

        let stock = cognee_components::EmbeddingConfig::default();
        let configured: [(&str, serde_json::Value); 5] = [
            ("llm_api_key", serde_json::json!("sk-test")),
            (
                "embedding_model",
                serde_json::json!("text-embedding-3-large"),
            ),
            (
                "embedding_endpoint",
                serde_json::json!("http://localhost:11434"),
            ),
            ("embedding_api_key", serde_json::json!("sk-embed")),
            ("skip_preflight", serde_json::json!(true)),
        ];
        for (key, value) in configured {
            let cm = keyless_cm(ComponentRegistry::with_builtins());
            cm.config().set(key, value).unwrap();
            let ctx = cm.build_context().await.unwrap();
            assert_eq!(ctx.embedding.provider, stock.provider, "{key}");
        }
    }

    /// The analytics key is the manager's resolved `llm.api_key` (here from
    /// `LLM_API_KEY`, the variable Python hashes, so one key gives one
    /// `api_key_tracking_id` in both SDKs), it follows a key set from code once
    /// the config is rebuilt, and it is unregistered with the manager. Checked
    /// on the manager's own registration: other tests in this binary build
    /// managers concurrently, so the process-wide aggregate is covered in
    /// `cognee-telemetry` instead.
    #[test]
    fn telemetry_key_is_the_resolved_llm_key_of_this_manager() {
        let mut settings = Settings::default();
        settings.overlay_from(&cognee_config_core::MapEnv::new().with("LLM_API_KEY", "sk-x"));
        let cm = ComponentManager::new(ConfigManager::new(settings));
        assert_eq!(cm.telemetry_key.registered().as_deref(), Some("sk-x"));

        cm.config().set_llm_api_key("sk-from-code");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(cm.build_context()).unwrap();
        assert_eq!(
            cm.telemetry_key.registered().as_deref(),
            Some("sk-from-code")
        );
    }

    /// `ENABLE_BACKEND_ACCESS_CONTROL=true` builds per-dataset storage, and
    /// the graph and vector engines route through it: a call outside every
    /// dataset scope fails, one inside reaches that dataset's own store.
    #[cfg(feature = "testing")]
    #[test]
    fn access_control_builds_per_dataset_storage() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.overlay_from(
            &cognee_config_core::MapEnv::new()
                .with("ENABLE_BACKEND_ACCESS_CONTROL", "true")
                .with("GRAPH_DATABASE_PROVIDER", "mock")
                .with("VECTOR_DB_PROVIDER", "mock"),
        );
        let cm = ComponentManager::new(ConfigManager::new(settings));
        cm.config()
            .set_system_root_directory(&root.path().to_string_lossy());
        cm.config()
            .set(
                "relational_db_url",
                serde_json::json!(format!(
                    "sqlite:{}?mode=rwc",
                    root.path().join("cognee.db").display()
                )),
            )
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let storage = cm
                .dataset_storage()
                .await
                .unwrap()
                .expect("per-dataset storage");
            assert_eq!(storage.handler_names(), ("memory", "memory"));
            let graph = cm.graph_db().await.unwrap();
            assert!(
                graph.is_empty().await.is_err(),
                "unscoped access fails closed"
            );

            let owner = uuid::Uuid::new_v4();
            let dataset =
                cognee_models::Dataset::new("ds".into(), owner, None, uuid::Uuid::new_v4());
            let id = dataset.id;
            let db = cm.database().await.unwrap();
            cognee_database::IngestDb::create_dataset(&*db, dataset)
                .await
                .unwrap();
            let empty = cognee_models::dataset_scope::with_dataset(id, graph.is_empty())
                .await
                .unwrap();
            assert!(empty);
            cm.close().await;
        });
    }

    /// A configuration change that does not touch storage keeps the same
    /// per-dataset storage; one that does builds a new one.
    #[cfg(feature = "testing")]
    #[test]
    fn per_dataset_storage_survives_unrelated_config_changes() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.overlay_from(
            &cognee_config_core::MapEnv::new()
                .with("ENABLE_BACKEND_ACCESS_CONTROL", "true")
                .with("GRAPH_DATABASE_PROVIDER", "mock")
                .with("VECTOR_DB_PROVIDER", "mock")
                // Off the keyless defaults, under which an LLM key does
                // switch the embedder back from the local model.
                .with("COGNEE_SKIP_PREFLIGHT", "true"),
        );
        let cm = ComponentManager::new(ConfigManager::new(settings));
        cm.config()
            .set_system_root_directory(&root.path().to_string_lossy());
        cm.config()
            .set(
                "relational_db_url",
                serde_json::json!(format!(
                    "sqlite:{}?mode=rwc",
                    root.path().join("cognee.db").display()
                )),
            )
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let first = cm.dataset_storage().await.unwrap().unwrap();
            cm.config().set_llm_api_key("sk-other");
            let same = cm.dataset_storage().await.unwrap().unwrap();
            assert!(
                Arc::ptr_eq(&first, &same),
                "an LLM key does not touch storage"
            );
            cm.config().set_embedding_dimensions(7);
            let rebuilt = cm.dataset_storage().await.unwrap().unwrap();
            assert!(!Arc::ptr_eq(&first, &rebuilt), "the embedding width does");
            cm.close().await;
        });
    }

    /// The flag off (the default) leaves the shared stores and no storage.
    #[test]
    fn no_access_control_means_shared_stores() {
        let cm = ComponentManager::new(ConfigManager::new(Settings::default()));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(rt.block_on(cm.dataset_storage()).unwrap().is_none());
    }

    fn cm_with_provider(provider: &str) -> ComponentManager {
        let settings = Settings {
            sections: [
                (
                    "llm.provider".to_string(),
                    serde_json::json!(provider.to_string()),
                ),
                (
                    "llm.api_key".to_string(),
                    serde_json::json!("sk-test".to_string()),
                ),
                (
                    "llm.model".to_string(),
                    serde_json::json!("gpt-4o-mini".to_string()),
                ),
            ]
            .into(),
            ..Settings::default()
        };
        ComponentManager::new(ConfigManager::new(settings))
    }

    #[tokio::test]
    async fn transcriber_returns_some_for_openai() {
        let cm = cm_with_provider("openai");
        let result = cm
            .transcriber()
            .await
            .expect("transcriber() should not error");
        assert!(
            result.is_some(),
            "openai provider must yield Some(transcriber)"
        );
    }

    #[tokio::test]
    async fn transcriber_returns_none_for_unknown_provider() {
        // Any non-openai provider (e.g. "mock") returns None — audio gracefully unsupported.
        let settings = Settings {
            sections: [
                (
                    "llm.provider".to_string(),
                    serde_json::json!("mock".to_string()),
                ),
                ("llm.api_key".to_string(), serde_json::json!(String::new())),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let result = cm
            .transcriber()
            .await
            .expect("transcriber() should not error for mock");
        assert!(result.is_none(), "non-openai provider must yield None");
    }

    #[tokio::test]
    async fn transcriber_is_cached_across_calls() {
        let cm = cm_with_provider("openai");
        let first = cm.transcriber().await.expect("first call").unwrap();
        let second = cm.transcriber().await.expect("second call").unwrap();
        // Both calls return an Arc pointing to the same allocation.
        assert!(Arc::ptr_eq(&first, &second), "transcriber should be cached");
    }

    #[cfg(feature = "audio-loader")]
    #[tokio::test]
    async fn a_transcriber_that_cannot_be_built_fails_audio_with_the_reason() {
        let settings = Settings {
            sections: [(
                "llm.provider".to_string(),
                serde_json::json!("openai".to_string()),
            )]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let reason = cm
            .transcriber()
            .await
            .err()
            .expect("no key, no transcriber");

        let error = UnavailableTranscriber(reason.to_string())
            .transcribe_audio(b"audio", "mp3", None, None)
            .await
            .expect_err("only an audio input fails");
        let message = error.to_string();
        assert!(message.contains("audio needs a transcriber"), "{message}");
        assert!(
            message.contains("llm_api_key must be configured"),
            "{message}"
        );
    }

    // -- resolved graph/vector Postgres URL / PgGraph provider dispatch -------

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_returns_explicit_url_as_is() {
        let settings = Settings {
            sections: [(
                "graph.url".to_string(),
                serde_json::json!("postgres://user:pw@myhost:5432/graphs".to_string()),
            )]
            .into(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
            .expect("should succeed with full URL");
        assert_eq!(url, "postgres://user:pw@myhost:5432/graphs");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_builds_from_graph_creds() {
        let settings = Settings {
            sections: [
                (
                    "graph.host".to_string(),
                    serde_json::json!("graphhost".to_string()),
                ),
                ("graph.port".to_string(), serde_json::json!(5432)),
                (
                    "graph.name".to_string(),
                    serde_json::json!("mygraph".to_string()),
                ),
                (
                    "graph.username".to_string(),
                    serde_json::json!("guser".to_string()),
                ),
                (
                    "graph.password".to_string(),
                    serde_json::json!("gpass".to_string()),
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
            .expect("should build from graph creds");
        assert!(url.contains("guser"), "URL should contain username");
        assert!(url.contains("graphhost"), "URL should contain host");
        assert!(url.contains("mygraph"), "URL should contain db name");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_falls_back_to_relational_creds() {
        let settings = Settings {
            sections: [
                (
                    "relational.host".to_string(),
                    serde_json::json!("relhost".to_string()),
                ),
                ("relational.port".to_string(), serde_json::json!(5432)),
                (
                    "relational.name".to_string(),
                    serde_json::json!("reldb".to_string()),
                ),
                (
                    "relational.username".to_string(),
                    serde_json::json!("reluser".to_string()),
                ),
                (
                    "relational.password".to_string(),
                    serde_json::json!("relpass".to_string()),
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
            .expect("should fall back to relational creds");
        assert!(
            url.contains("reluser"),
            "URL should contain relational username"
        );
        assert!(
            url.contains("relhost"),
            "URL should contain relational host"
        );
        assert!(
            url.contains("reldb"),
            "URL should contain relational db name"
        );
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_errors_when_no_creds() {
        let settings = Settings {
            sections: [
                (
                    "relational.host".to_string(),
                    serde_json::json!(String::new()),
                ),
                (
                    "relational.name".to_string(),
                    serde_json::json!(String::new()),
                ),
                (
                    "relational.username".to_string(),
                    serde_json::json!(String::new()),
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let result = settings.resolved_graph_postgres_url();
        assert!(result.is_err(), "should error when no creds available");
    }

    #[tokio::test]
    async fn init_graph_db_rejects_unsupported_provider() {
        let settings = Settings {
            sections: [(
                "graph.provider".to_string(),
                serde_json::json!("neo4j".to_string()),
            )]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let result = cm.graph_db().await;
        assert!(result.is_err());
        let err_msg = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err_msg.contains("neo4j"),
            "error message should name the unsupported provider: {err_msg}"
        );
    }

    // -- Mock LLM factory wiring (MOCK_LLM / COGNEE_RECORD_LLM) ----------------

    /// Write a minimal valid cassette to a temp file and return (dir, path).
    #[cfg(feature = "mock-llm")]
    fn write_cassette() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cassette.json");
        let body = r#"{"version":1,"model":"mock-model","entries":{}}"#;
        std::fs::write(&path, body).expect("write cassette");
        (dir, path)
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_uses_replay_mock_when_llm_mock_set_without_api_key() {
        let (_dir, cassette) = write_cassette();
        let settings = Settings {
            sections: [
                ("llm.mock".to_string(), serde_json::json!(true)),
                (
                    "llm.cassette".to_string(),
                    serde_json::json!(cassette.to_string_lossy().into_owned()),
                ),
                ("llm.api_key".to_string(), serde_json::json!(String::new())),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm.llm().await.expect("mock llm should initialize offline");
        assert_eq!(
            llm.model(),
            "mock-model",
            "replay mock reports cassette model"
        );
        let resp = llm
            .generate(
                vec![cognee_llm::Message {
                    role: cognee_llm::MessageRole::User,
                    content: "hello".to_string(),
                }],
                None,
            )
            .await
            .expect("offline generate should succeed");
        assert_eq!(resp.model, "mock-model");
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_selects_mock_when_provider_is_mock() {
        let (_dir, cassette) = write_cassette();
        let settings = Settings {
            sections: [
                (
                    "llm.provider".to_string(),
                    serde_json::json!("mock".to_string()),
                ),
                (
                    "llm.cassette".to_string(),
                    serde_json::json!(cassette.to_string_lossy().into_owned()),
                ),
                ("llm.api_key".to_string(), serde_json::json!(String::new())),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm
            .llm()
            .await
            .expect("provider=mock should initialize offline");
        assert_eq!(llm.model(), "mock-model");
    }

    #[tokio::test]
    async fn llm_without_an_api_key_is_unavailable_but_builds() {
        let settings = Settings {
            sections: [
                ("llm.provider".to_string(), serde_json::json!("openai")),
                ("llm.api_key".to_string(), serde_json::json!("")),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        assert!(
            !cm.llm_available()
                .await
                .expect("openai is a built-in provider")
        );
        cm.llm()
            .await
            .expect("a missing key should not fail the build");
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_errors_when_mock_set_but_cassette_empty() {
        let settings = Settings {
            sections: [
                ("llm.mock".to_string(), serde_json::json!(true)),
                ("llm.cassette".to_string(), serde_json::json!(String::new())),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let err = match cm.llm().await {
            Err(e) => e,
            Ok(_) => panic!("empty cassette must error"),
        };
        assert!(
            err.to_string().contains("MOCK_LLM_CASSETTE"),
            "error should mention the missing cassette env: {err}"
        );
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_wraps_real_adapter_in_recorder_when_record_path_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record_path = dir.path().join("recorded.json");
        let settings = Settings {
            sections: [
                (
                    "llm.provider".to_string(),
                    serde_json::json!("openai".to_string()),
                ),
                (
                    "llm.api_key".to_string(),
                    serde_json::json!("sk-test".to_string()),
                ),
                (
                    "llm.model".to_string(),
                    serde_json::json!("gpt-4o-mini".to_string()),
                ),
                (
                    "llm.record_path".to_string(),
                    serde_json::json!(record_path.to_string_lossy().into_owned()),
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm
            .llm()
            .await
            .expect("recording wrap should initialize without network");
        assert_eq!(
            llm.model(),
            "gpt-4o-mini",
            "recorder delegates model() to the wrapped adapter"
        );
    }
}
