//! `CogneeHandle` — the stateful, `JsBox`-boxed handle the TypeScript SDK is
//! built on. It owns the config (`ComponentManager`) and lazily builds + caches
//! a [`CogneeServices`] bundle, version-invalidated like `ComponentManager`.
//!
//! Phase 1 exports: `cogneeNew`, `cogneeWarm`, `cogneeOwnerId`. SDK operations
//! (add/cognify/search/…) are Phases 3–6 and follow the canonical pattern:
//! `let svc = handle.services().await?; <cognee-lib api>(…, svc.*); serde_to_js`.

use std::sync::Arc;

use neon::prelude::*;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use cognee_lib::ComponentManager;
use cognee_lib::config::{ConfigManager, Settings};

use crate::errors::{SdkError, throw_sdk_error};
use crate::runtime::{ensure_runtime, runtime};
use crate::services::CogneeServices;

/// The shareable inner state of a handle.
///
/// Kept in its own `Arc` (separate from the `JsBox`) so async native functions
/// can clone a `Send + Sync` reference into a spawned task — a `JsBox` itself is
/// not `Send` and cannot cross the spawn boundary.
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
    /// Return the cached services, rebuilding if the cache is empty or the config
    /// version advanced. On the (re)build path the resolved owner id is written
    /// back into `owner_id`.
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

        // Slow path: (re)build under the lock. Re-check first — another task may
        // have rebuilt while we were waiting.
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

/// The boxed SDK handle. Survives across JS calls (held by a `JsBox`).
pub struct CogneeHandle {
    pub state: Arc<HandleState>,
}

// Default no-op finalize is fine: the `Arc`s drop when the `JsBox` is GC'd.
impl Finalize for CogneeHandle {}

impl CogneeHandle {
    /// Build the handle from settings (sync, no I/O).
    fn new_from_settings(settings: Settings) -> Self {
        let cm = Arc::new(ComponentManager::new(ConfigManager::new(settings)));
        CogneeHandle {
            state: Arc::new(HandleState {
                cm,
                services: TokioMutex::new(None),
                owner_id: TokioMutex::new(None),
                tenant_id: None,
            }),
        }
    }
}

/// `cogneeNew(settingsJson?) -> JsBox<CogneeHandle>`
///
/// Pure/sync: builds `Settings` (from a JSON string/object if given, else
/// `from_env()`), wraps in `ConfigManager` → `ComponentManager`. Does NOT touch
/// the DB and does NOT resolve `owner_id`. Ensures the global tokio runtime
/// exists so later async exports can `spawn` onto it.
pub fn cognee_new(mut cx: FunctionContext) -> JsResult<JsBox<CogneeHandle>> {
    // Make sure the runtime is up — the handle path never requires init().
    ensure_runtime().or_else(|e| cx.throw_error(e))?;

    // Optional settings argument: either a JSON string or a JS object.
    let settings = match cx.argument_opt(0) {
        None => settings_from_env(),
        Some(arg) if arg.is_a::<JsUndefined, _>(&mut cx) || arg.is_a::<JsNull, _>(&mut cx) => {
            settings_from_env()
        }
        Some(arg) => {
            // Accept either a JSON string or a plain JS object; normalise to a
            // JSON string via JSON.stringify for the object case.
            let json = if let Ok(s) = arg.downcast::<JsString, _>(&mut cx) {
                s.value(&mut cx)
            } else {
                stringify_js(&mut cx, arg)?
            };
            // `Settings` is `#[serde(default)]`, so partial JSON overlays onto
            // the defaults.
            serde_json::from_str::<Settings>(&json)
                .or_else(|e| cx.throw_error(format!("invalid settings JSON: {e}")))?
        }
    };

    Ok(cx.boxed(CogneeHandle::new_from_settings(settings)))
}

/// `cogneeWarm(handle) -> Promise<void>`
///
/// Force `services()` to build now (async), surfacing config/connection errors
/// early and populating `owner_id`.
pub fn cognee_warm(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = state.services().await.map(|_| ());
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeOwnerId(handle) -> Promise<string>`
///
/// Async — the owner id is email-derived and requires the user row. Warms
/// lazily if needed, then returns the UUID string.
pub fn cognee_owner_id(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = state.owner_id().await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(id) => Ok(cx.string(id.to_string())),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `Settings` populated from the environment (defaults + env overlay).
fn settings_from_env() -> Settings {
    ConfigManager::from_env().read().clone()
}

/// Stringify a JS value via the global `JSON.stringify`.
fn stringify_js<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<String> {
    let global = cx.global_object();
    let json: Handle<JsObject> = global.get(cx, "JSON")?;
    let stringify: Handle<JsFunction> = json.get(cx, "stringify")?;
    let result: Handle<JsValue> = stringify.call_with(cx).arg(val).apply(cx)?;
    let s = result.downcast_or_throw::<JsString, _>(cx)?;
    Ok(s.value(cx))
}
