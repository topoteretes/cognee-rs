//! `HandleState` — the portable inner state of the SDK handle.
//!
//! This type is shared between the Neon JS binding and the C API binding. It
//! wraps a `ComponentManager` (config + lazy engines) and lazily builds +
//! caches a [`CogneeServices`] bundle, version-invalidated whenever the config
//! changes.
//!
//! Neon-specific wrappers (`CogneeHandle`, `Finalize` impl, `cognee_new` /
//! `cognee_warm` / `cognee_owner_id` exports) stay in `cognee-ts-neon` because
//! they depend on `neon::prelude::*`.
//!
//! C-specific wrappers (`CgSdk`) stay in `cognee-capi` (Phase 1 Part B).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use cognee::ComponentManager;
use cognee::ComponentRegistry;
use cognee::config::{ConfigManager, Settings};
use cognee::database::DatabaseConnection;
use cognee::models::User;

use crate::SdkError;
use crate::services::CogneeServices;

/// How long a teardown waits for already-dispatched telemetry POSTs to leave the
/// process. Small on purpose: a binding's `close()` must never block on an
/// analytics collector.
const TELEMETRY_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Optional bootstrap seam for resolving (and persisting) the default user.
///
/// OSS has no `users`-table writer, so the default binding behaviour is
/// **DB-free** (`HandleState`'s hook is `None` → the in-memory
/// `cognee::api::get_or_create_default_user` UUID5 derivation is used,
/// with no DB write). The closed cloud build attaches an implementation that
/// upserts a real `users` row through `cognee-access-control`, so warm/admin
/// paths persist the default user for downstream ACL / API-key FK integrity.
///
/// This is the OSS-local analogue of the `with_*` builder convention used
/// elsewhere (e.g. `DatasetManager::with_acl`): the trait lives in OSS so the
/// closed crate can implement it, but OSS itself never provides an impl.
#[async_trait::async_trait]
pub trait DefaultUserBootstrap: Send + Sync {
    /// Resolve (and optionally persist) the default user, returning the row.
    async fn bootstrap(&self, db: &Arc<DatabaseConnection>, email: &str) -> Result<User, SdkError>;
}

/// The shareable inner state of a binding handle.
///
/// Kept in its own `Arc` (separate from the Neon `JsBox` or C opaque pointer)
/// so async operations can clone a `Send + Sync` reference into a spawned task.
pub struct HandleState {
    /// Owns config + the 6 lazy engines.
    pub cm: Arc<ComponentManager>,
    /// Cached services + the config version they were built at. `None` until the
    /// first warm.
    services: TokioMutex<Option<(u64, Arc<CogneeServices>)>>,
    /// Resolved on first warm (the id of the default `User` row). `None` until
    /// then.
    owner_id: TokioMutex<Option<Uuid>>,
    /// The default user carries no tenant (see `get_or_create_default_user`).
    #[allow(dead_code)] // consumed by SDK ops in later phases
    tenant_id: Option<Uuid>,
    /// Optional DB-backed default-user bootstrap hook. `None` (the OSS default)
    /// keeps the DB-free in-memory derivation; the closed cloud build attaches
    /// an impl that persists the `users` row.
    bootstrap: Option<Arc<dyn DefaultUserBootstrap>>,
    /// Set by [`HandleState::close`] (the *explicit* teardown). Once set,
    /// [`HandleState::services`] fails instead of re-warming, so a use-after-close
    /// surfaces as a clear error rather than silently reopening the database.
    /// [`HandleState::release`] deliberately leaves this alone — see the two
    /// methods' docs.
    closed: AtomicBool,
}

impl HandleState {
    /// Construct from a fully-populated `Settings` (sync, no I/O).
    ///
    /// Applies the **3-way overlay** `defaults < env < object`: the caller is
    /// responsible for building `Settings` with the desired precedence before
    /// calling this method. The neon binding performs the overlay in
    /// `cognee_new`; the C binding does it in `cg_sdk_new`.
    ///
    /// For env-only construction use [`HandleState::from_env`].
    pub fn from_settings(settings: Settings) -> Self {
        Self::from_settings_with_registry(settings, ComponentRegistry::with_builtins())
    }

    /// Construct from `Settings` with an explicit component registry.
    ///
    /// This is the injection seam for external adapters: a closed cloud build
    /// registers its qdrant / litert factories on a `ComponentRegistry` and
    /// passes it here so a configured `vector_provider="qdrant"` (etc.)
    /// resolves through the same construction path the py/ts/c SDKs use. With
    /// the OSS built-in registry this is byte-for-byte equivalent to
    /// [`from_settings`](Self::from_settings).
    pub fn from_settings_with_registry(settings: Settings, registry: ComponentRegistry) -> Self {
        let cm = Arc::new(ComponentManager::with_registry(
            ConfigManager::new(settings),
            registry,
        ));
        HandleState {
            cm,
            services: TokioMutex::new(None),
            owner_id: TokioMutex::new(None),
            tenant_id: None,
            bootstrap: None,
            closed: AtomicBool::new(false),
        }
    }

    /// Construct from the environment (defaults overlaid by env vars).
    pub fn from_env() -> Self {
        Self::from_settings(ConfigManager::from_env().read().clone())
    }

    /// Attach a DB-backed default-user bootstrap hook (builder).
    ///
    /// With a hook set, the warm path and the admin op resolve the owner via
    /// `hook.bootstrap(db, email)` — persisting the `users` row — instead of
    /// the DB-free in-memory derivation. The closed cloud build uses this to
    /// restore the original monorepo's persisted default-user behaviour.
    pub fn with_default_user_bootstrap(mut self, hook: Arc<dyn DefaultUserBootstrap>) -> Self {
        self.bootstrap = Some(hook);
        self
    }

    /// The configured default-user bootstrap hook, if any.
    pub(crate) fn default_user_bootstrap(&self) -> Option<&Arc<dyn DefaultUserBootstrap>> {
        self.bootstrap.as_ref()
    }

    /// Return the cached services, rebuilding if the cache is empty or the
    /// config version advanced. On the (re)build path the resolved owner id is
    /// written back into `owner_id`.
    pub async fn services(&self) -> Result<Arc<CogneeServices>, SdkError> {
        // A handle closed through the explicit teardown must not silently reopen
        // the database on the next op. This is the single choke point every op
        // goes through, which is why the guard lives here and not in each binding.
        if self.is_closed() {
            return Err(SdkError::Runtime(
                "cognee handle is closed (close() was called); create a new handle".to_string(),
            ));
        }
        let current_ver = self.cm.config().version();

        // Fast path: cache hit at the current version.
        {
            let guard = self.services.lock().await;
            if let Some((ver, ref svc)) = *guard
                && ver == current_ver
            {
                return Ok(Arc::clone(svc));
            }
        }

        // Slow path: (re)build under the lock. Re-check first — another task
        // may have rebuilt while we were waiting.
        let mut guard = self.services.lock().await;
        if let Some((ver, ref svc)) = *guard
            && ver == current_ver
        {
            return Ok(Arc::clone(svc));
        }

        let (svc, owner_id) = CogneeServices::build(&self.cm).await?;
        let svc = Arc::new(svc);

        // When a DB-backed bootstrap hook is attached (closed cloud build),
        // resolve the owner through it so the `users` row is persisted. The
        // hook is keyed on the same email and yields the same UUID5 id, but
        // additionally writes the row. With no hook (OSS default), keep the
        // DB-free `owner_id` returned by `build` — byte-for-byte unchanged.
        let owner_id = if let Some(hook) = self.default_user_bootstrap() {
            let email = {
                let settings = self.cm.settings();
                settings.default_user_email.clone()
            };
            hook.bootstrap(&svc.database, &email).await?.id
        } else {
            owner_id
        };

        *guard = Some((current_ver, Arc::clone(&svc)));
        // Publish the resolved owner id (idempotent: email-derived UUID5).
        *self.owner_id.lock().await = Some(owner_id);
        Ok(svc)
    }

    /// Whether [`close`](Self::close) has been called on this handle.
    ///
    /// Bindings use this for a cheap synchronous guard before dispatching an op,
    /// so a use-after-close reports itself without building a future first.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Whether this handle currently holds built services — i.e. whether a
    /// teardown would have anything to release.
    ///
    /// Cheap and non-blocking, for use from a synchronous finalizer that wants to
    /// skip the teardown entirely (and, with it, spinning up a runtime to drive
    /// it) for a handle that never warmed. A contended lock reports `true`: the
    /// safe direction is to attempt the release rather than skip a real one.
    pub fn has_open_resources(&self) -> bool {
        let bundle_cached = match self.services.try_lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => true,
        };
        // The bundle alone is not the answer. `CogneeServices::build` warms the
        // engines one at a time and can fail part-way — a bad `llm_api_key` fails
        // *after* the SQLite pool and the embedded graph are cached — leaving the
        // bundle `None` while real resources are open. Asking only about the
        // bundle reported "nothing to release" for exactly that handle, and the
        // finalizer skipped the teardown.
        bundle_cached || self.cm.has_cached_components()
    }

    /// Release the resources this handle opened, leaving it usable.
    ///
    /// Dropping a handle is **not** a close: the relational pool's destructor only
    /// flags the pool closed and lets its connections tear down concurrently,
    /// which for SQLite leaves the `-wal`/`-shm` sidecars orphaned on disk (see
    /// `cognee::database::close` for the mechanism, topoteretes/cognee-rs#132 for
    /// the measurements). So every teardown path has to call one of these two
    /// methods, and which one depends on whether the user asked:
    ///
    /// - `release` is for the **implicit** paths — a GC finalizer (`Drop for
    ///   PyCognee`, neon's `Finalize`) reclaiming a handle the program dropped on
    ///   the floor. The user never said "close", so this only gives the resources
    ///   back, and only the ones nobody else is using: the handle stays warm-able,
    ///   a sub-handle like `cognee.datasets` that outlived its parent keeps
    ///   working (it re-warms against a fresh connection), and an operation still
    ///   in flight keeps the components it holds — those are released when it
    ///   finishes. See [`ComponentManager::release`](cognee::ComponentManager::release)
    ///   for how "nobody else is using it" is decided.
    /// - [`close`](Self::close) is for the **explicit** paths (`Cognee.close()`,
    ///   `cg_sdk_close`). It marks the handle closed, so later ops fail with a
    ///   clear error instead of silently reopening the database, and it closes the
    ///   components **unconditionally** — including ones an in-flight operation is
    ///   still holding, which will then fail on its next query. That is the point:
    ///   the caller said they were done, and a resource that outlives an explicit
    ///   close is the bug this exists to prevent.
    ///
    /// Both are idempotent. Neither waits for concurrent operations to *finish*: an
    /// op that is mid-flight holds its own `Arc<CogneeServices>` and will see a
    /// closed pool on its next query. So a binding should only call `close` when its
    /// own contract says the handle is done, and use `release` where it cannot know
    /// — which is exactly the split between an explicit `close()` call and a
    /// garbage collector.
    ///
    /// What the teardown *does* wait for — and has to, or the SQLite sidecars
    /// outlive it — is the pool's connections coming back and closing. That is
    /// normally microseconds; a connection genuinely held by an in-flight op bounds
    /// it at `cognee_database`'s drain timeout, after which the teardown gives up on
    /// that connection and logs rather than hanging.
    pub async fn release(&self) {
        self.teardown(false).await;
    }

    /// Release the handle's resources **and** mark it closed: every later op fails
    /// with an explicit "handle is closed" error rather than re-warming.
    ///
    /// This is the explicit user-facing teardown — see [`release`](Self::release)
    /// for how the two differ and why both exist.
    pub async fn close(&self) {
        self.teardown(true).await;
    }

    /// Shared body of [`release`](Self::release) / [`close`](Self::close).
    async fn teardown(&self, mark_closed: bool) {
        // Mark first: a concurrent op then fails fast in `services()` instead of
        // racing to warm a pool that is about to be closed underneath it.
        if mark_closed {
            self.closed.store(true, Ordering::Release);
        }
        // Clear the cached bundle: it holds an `Arc` clone of the connection, and
        // dropping it is what lets `release` leave the handle re-warmable rather
        // than holding a closed pool. The lock is held across the close so a
        // concurrent `services()` cannot slip a freshly built pool into the cache
        // in between and have it closed underneath it. Lock order (services →
        // owner id → the manager's caches) is the same one `services()` takes, so
        // this cannot deadlock.
        //
        // **This ordering is load-bearing and must not be reversed.** The bundle
        // holds an `Arc` clone of *every* engine, not just the connection, so
        // dropping it first is what leaves the manager's own cache as the last
        // strong reference — which is the reference `cm.close()` releases, and
        // therefore the only reason the engines' destructors run at all (measured:
        // exactly one live relational connection at this point, and the embedded
        // graph's `.wal` released by the `cm.close()` below rather than at process
        // exit). Calling `cm.close()` while the bundle is still cached would close
        // the stores underneath a live bundle and leak the rest.
        let mut guard = self.services.lock().await;
        drop(guard.take());
        *self.owner_id.lock().await = None;
        // Which teardown tier the manager runs is the whole difference between
        // `release` and `close`: a store's `close()` mutates state behind the
        // shared `Arc` and would break an operation still holding a clone, so the
        // implicit tier closes only components nobody else is using.
        if mark_closed {
            self.cm.close().await;
        } else {
            self.cm.release().await;
        }

        // Finally, let the analytics POSTs already in flight finish. `send_telemetry`
        // is fire-and-forget, so a binding whose embedder exits (or drops its
        // runtime) right after closing otherwise discards its last event —
        // measured 0 of 1 delivered without a flush, 1 of 1 with one. Hard-bounded
        // and ignored on timeout: a slow collector must never make a `close()` hang,
        // and telemetry must never make one fail.
        if !cognee::cognee_telemetry::flush(TELEMETRY_FLUSH_TIMEOUT).await {
            tracing::debug!(
                "telemetry still in flight after {TELEMETRY_FLUSH_TIMEOUT:?}; \
                 dropping the remainder rather than delaying teardown"
            );
        }
    }

    /// Blocking [`close`](Self::close), for a synchronous binding teardown hook
    /// (the JNI `destroy`, `cg_sdk_close`, Python's `close()`) that cannot
    /// `.await`.
    ///
    /// See [`release_blocking`](Self::release_blocking) for how the thread is
    /// handled.
    pub fn close_blocking(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        self.teardown_blocking(rt, true);
    }

    /// Blocking [`release`](Self::release), for a synchronous finalizer
    /// (`Drop for PyCognee`) that cannot `.await`.
    ///
    /// `rt` is the binding's own runtime handle, used when the calling thread has
    /// no runtime of its own — the normal case, since teardown runs on the
    /// embedder's thread (a Java `close()` call or the `Cleaner` thread, a Python
    /// interpreter thread running a `__del__`). Teardown invoked from *inside* the
    /// async runtime is also possible, though — a Java `CompletableFuture`
    /// continuation runs on a worker thread — and blocking a worker outright would
    /// stall the runtime, so that case hands the thread over to the blocking pool
    /// first and still finishes before returning.
    pub fn release_blocking(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        self.teardown_blocking(rt, false);
    }

    fn teardown_blocking(self: &Arc<Self>, rt: &tokio::runtime::Handle, mark_closed: bool) {
        use tokio::runtime::{Handle, RuntimeFlavor};

        match Handle::try_current() {
            Err(_) => rt.block_on(self.teardown(mark_closed)),
            Ok(current) if current.runtime_flavor() == RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| current.block_on(self.teardown(mark_closed)));
            }
            Ok(current) => {
                // A current-thread runtime cannot hand the thread off
                // (`block_in_place` panics there), so the teardown cannot be
                // awaited here without deadlocking the very runtime that has to
                // drive it. Spawn it instead: the pool closes moments later. The
                // closed flag is still set synchronously, so a use-after-close is
                // reported even before the spawned teardown lands.
                if mark_closed {
                    self.closed.store(true, Ordering::Release);
                }
                let this = Arc::clone(self);
                current.spawn(async move { this.teardown(mark_closed).await });
            }
        }
    }

    /// Resolve the owner id, warming lazily if necessary.
    pub async fn owner_id(&self) -> Result<Uuid, SdkError> {
        // `services()` guarantees `owner_id` is populated on its build path.
        self.services().await?;
        let guard = self.owner_id.lock().await;
        guard.ok_or_else(|| {
            SdkError::Runtime("owner_id unresolved after warm (internal invariant)".to_string())
        })
    }
}
