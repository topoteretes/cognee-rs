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

use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use cognee::ComponentManager;
use cognee::ComponentRegistry;
use cognee::config::{ConfigManager, Settings};
use cognee::database::DatabaseConnection;
use cognee::models::User;

use crate::SdkError;
use crate::services::CogneeServices;

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

    /// Release the resources this handle opened, now rather than at `Drop`.
    ///
    /// Dropping the handle is **not** a close: the relational pool's destructor
    /// only flags the pool closed and lets its connections tear down
    /// concurrently, which for SQLite leaves the `-wal`/`-shm` sidecars orphaned
    /// on disk (see `cognee::database::close` for the full mechanism and
    /// topoteretes/cognee-rs#132 for the measurements). A binding with an explicit
    /// teardown entry point calls this from it — the Java binding does, from
    /// `Cognee.close()` — so the files are gone by the time that entry point
    /// returns. Bindings whose only teardown is a GC finalizer (Python, TS) have
    /// no such entry point yet and are still subject to the drop race.
    ///
    /// Non-poisoning and idempotent: the cached service bundle and the resolved
    /// owner id are cleared too, so a later op on the same handle re-warms from
    /// scratch against a fresh connection instead of failing. What it does *not*
    /// do is wait for concurrent operations — an op that is mid-flight when this
    /// runs holds its own `Arc<CogneeServices>` and will see a closed pool on its
    /// next query, so bindings should only close once their own contract says the
    /// handle is done.
    pub async fn close(&self) {
        // Clear the cached bundle: it holds an `Arc` clone of the connection, and
        // dropping it is what makes the close non-poisoning — the next
        // `services()` rebuilds instead of handing out the closed pool. The lock
        // is held across the close so a concurrent `services()` cannot slip a
        // freshly built pool into the cache in between and have it closed
        // underneath it. Lock order (services → owner id → the manager's caches)
        // is the same one `services()` takes, so this cannot deadlock.
        let mut guard = self.services.lock().await;
        drop(guard.take());
        *self.owner_id.lock().await = None;
        self.cm.close().await;
    }

    /// Blocking wrapper around [`close`](Self::close) for a synchronous binding
    /// teardown hook (the JNI `destroy`), which cannot `.await`.
    ///
    /// `rt` is the binding's own runtime handle, used when the calling thread has
    /// no runtime of its own — the normal case, since teardown runs on the
    /// embedder's thread (a Java `close()` call or the `Cleaner` thread). Teardown
    /// invoked from *inside* the async runtime is also possible, though — a Java
    /// `CompletableFuture` continuation runs on a worker thread — and blocking a
    /// worker outright would stall the runtime, so that case hands the thread over
    /// to the blocking pool first and still closes before returning.
    pub fn close_blocking(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        use tokio::runtime::{Handle, RuntimeFlavor};

        match Handle::try_current() {
            Err(_) => rt.block_on(self.close()),
            Ok(current) if current.runtime_flavor() == RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| current.block_on(self.close()));
            }
            Ok(current) => {
                // A current-thread runtime cannot hand the thread off
                // (`block_in_place` panics there), so the close cannot be awaited
                // here without deadlocking the very runtime that has to drive it.
                // Spawn it instead: the pool closes moments later.
                let this = Arc::clone(self);
                current.spawn(async move { this.close().await });
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
