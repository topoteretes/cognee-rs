//! ComponentManager: lazy-initializing, shared component store.
//!
//! Construction logic lives in `cognee-components`; this type owns the
//! version-keyed cache and delegates each backend build to a
//! [`ComponentRegistry`]. Supply a custom registry via [`ComponentManager::with_registry`]
//! to plug in external adapters (e.g. the closed qdrant / litert factories);
//! [`ComponentManager::new`] uses [`ComponentRegistry::with_builtins`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock as TokioRwLock;
use tracing::instrument;

use cognee_components::ComponentRegistry;
use cognee_database::DatabaseConnection;
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
    database: TokioRwLock<Option<(u64, Arc<DatabaseConnection>)>>,
    graph_db: TokioRwLock<Option<(u64, Arc<dyn GraphDBTrait>)>>,
    vector_db: TokioRwLock<Option<(u64, Arc<dyn VectorDB>)>>,
    embedding_engine: TokioRwLock<Option<(u64, Arc<dyn EmbeddingEngine>)>>,
    llm: TokioRwLock<Option<(u64, Arc<dyn Llm>)>>,
    // Stores Option<Arc<dyn Transcriber>>: None when the provider does not
    // support transcription (e.g. litert). The outer Option<(ver, ...)> is
    // the version-keyed cache envelope.
    #[allow(clippy::type_complexity)]
    transcriber: TokioRwLock<Option<(u64, Option<Arc<dyn Transcriber>>)>>,
    // Version-keyed cache of the lowered build context, so `Settings::backend_context`
    // (env reads + the Postgres credential-fallback warning) runs once per config
    // version instead of once per component (7×).
    context: TokioRwLock<Option<(u64, cognee_components::BackendBuildContext)>>,
}

impl ComponentManager {
    /// Construct with the OSS built-in registry
    /// ([`ComponentRegistry::with_builtins`]).
    pub fn new(config: ConfigManager) -> Self {
        Self::with_registry(config, ComponentRegistry::with_builtins())
    }

    /// Construct with an explicit registry. Use this to inject external adapter
    /// factories (register them on the registry before passing it in).
    pub fn with_registry(config: ConfigManager, registry: ComponentRegistry) -> Self {
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
            context: TokioRwLock::new(None),
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
    async fn build_context(&self) -> cognee_components::BackendBuildContext {
        let current_ver = self.config.version();
        {
            let guard = self.context.read().await;
            if let Some((ver, ctx)) = &*guard
                && *ver == current_ver
            {
                return ctx.clone();
            }
        }
        let mut guard = self.context.write().await;
        if let Some((ver, ctx)) = &*guard
            && *ver == current_ver
        {
            return ctx.clone();
        }
        // No `.await` between the config read and storing the result, so the
        // (non-`Send`) settings guard never crosses an await point.
        let ctx = self.config.read().backend_context();
        *guard = Some((current_ver, ctx.clone()));
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
        let ctx = self.build_context().await;
        cognee_components::build_storage(&ctx).await
    }

    #[instrument(name = "cognee.component.database", level = "info", skip_all, err)]
    async fn init_database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        let ctx = self.build_context().await;
        cognee_components::build_database(&ctx).await
    }

    #[instrument(name = "cognee.component.graph_db", level = "info", skip_all, err)]
    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_graph(&ctx).await
    }

    #[instrument(name = "cognee.component.vector_db", level = "info", skip_all, err)]
    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_vector(&ctx).await
    }

    #[instrument(
        name = "cognee.component.embedding_engine",
        level = "info",
        skip_all,
        err
    )]
    async fn init_embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_embedding(&ctx).await
    }

    #[instrument(name = "cognee.component.llm", level = "info", skip_all, err)]
    async fn init_llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_llm(&ctx).await
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
        let ctx = self.build_context().await;
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
        let database = self.database.write().await.take();
        let graph = self.graph_db.write().await.take();
        let vector = self.vector_db.write().await.take();
        let embedding = self.embedding_engine.write().await.take();
        let llm = self.llm.write().await.take();
        let transcriber = self.transcriber.write().await.take();
        let storage = self.storage.write().await.take();
        // The lowered build context holds no OS resource, but a stale one would
        // outlive the config version it was built for; clear it with the rest.
        let _ = self.context.write().await.take();

        // Relational FIRST: the caller may be bounding this whole teardown with a
        // timeout, and the SQLite sidecars are the leak the bound exists to fix,
        // so they must not be queued behind a graph checkpoint or an ONNX thread
        // join. See the ordering note in this method's docs.
        if let Some((_, db)) = database {
            if shared.may_close(&db)
                && let Err(e) = cognee_database::close(&db).await
            {
                tracing::warn!(error = %e, "failed to close the relational connection pool");
            }
            // The connection has no blocking destructor of its own; dropping it
            // inline is fine, and doing so here (rather than at the end of the
            // function) keeps the "take, close, release" shape uniform.
            drop(db);
        }

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

    async fn database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::config::{ConfigManager, Settings};

    fn cm_with_provider(provider: &str) -> ComponentManager {
        let settings = Settings {
            llm_provider: provider.to_string(),
            llm_api_key: "sk-test".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
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
            llm_provider: "mock".to_string(),
            llm_api_key: String::new(),
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

    // -- resolved graph/vector Postgres URL / PgGraph provider dispatch -------

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_returns_explicit_url_as_is() {
        let settings = Settings {
            graph_database_url: "postgres://user:pw@myhost:5432/graphs".to_string(),
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
            graph_database_host: "graphhost".to_string(),
            graph_database_port: 5432,
            graph_database_name: "mygraph".to_string(),
            graph_database_username: "guser".to_string(),
            graph_database_password: "gpass".to_string(),
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
            db_host: "relhost".to_string(),
            db_port: 5432,
            db_name: "reldb".to_string(),
            db_username: "reluser".to_string(),
            db_password: "relpass".to_string(),
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
            db_host: String::new(),
            db_name: String::new(),
            db_username: String::new(),
            ..Settings::default()
        };
        let result = settings.resolved_graph_postgres_url();
        assert!(result.is_err(), "should error when no creds available");
    }

    #[tokio::test]
    async fn init_graph_db_rejects_unsupported_provider() {
        let settings = Settings {
            graph_database_provider: "neo4j".to_string(),
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
            llm_mock: true,
            llm_cassette: cassette.to_string_lossy().into_owned(),
            llm_api_key: String::new(),
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
            llm_provider: "mock".to_string(),
            llm_cassette: cassette.to_string_lossy().into_owned(),
            llm_api_key: String::new(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm
            .llm()
            .await
            .expect("provider=mock should initialize offline");
        assert_eq!(llm.model(), "mock-model");
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_errors_when_mock_set_but_cassette_empty() {
        let settings = Settings {
            llm_mock: true,
            llm_cassette: String::new(),
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
            llm_provider: "openai".to_string(),
            llm_api_key: "sk-test".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
            llm_record_path: record_path.to_string_lossy().into_owned(),
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
