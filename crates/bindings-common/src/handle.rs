//! `HandleState` — the portable inner state of the SDK handle.
//!
//! This type is shared between the Neon JS binding and the C API binding. It
//! wraps a `ComponentManager` (config + lazy engines) and lazily builds +
//! caches a [`CogneeServices`] bundle, version-invalidated whenever the config
//! changes.
//!
//! Neon-specific wrappers (`CogneeHandle`, `Finalize` impl, `cognee_new` /
//! `cognee_warm` / `cognee_owner_id` exports) stay in `cognee-neon` because
//! they depend on `neon::prelude::*`.
//!
//! C-specific wrappers (`CgSdk`) stay in `cognee-capi` (Phase 1 Part B).

use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use cognee_lib::ComponentManager;
use cognee_lib::config::{ConfigManager, Settings};

use crate::SdkError;
use crate::services::CogneeServices;

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
        }
    }

    /// Construct from the environment (defaults overlaid by env vars).
    pub fn from_env() -> Self {
        Self::from_settings(ConfigManager::from_env().read().clone())
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
