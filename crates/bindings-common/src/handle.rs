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

use cognee_lib::ComponentManager;
use cognee_lib::config::{ConfigManager, Settings};
use cognee_lib::database::DatabaseConnection;
use cognee_lib::models::User;

use crate::SdkError;
use crate::services::CogneeServices;

/// Optional bootstrap seam for resolving (and persisting) the default user.
///
/// OSS has no `users`-table writer, so the default binding behaviour is
/// **DB-free** (`HandleState`'s hook is `None` → the in-memory
/// `cognee_lib::api::get_or_create_default_user` UUID5 derivation is used,
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
        let cm = Arc::new(ComponentManager::new(ConfigManager::new(settings)));
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
