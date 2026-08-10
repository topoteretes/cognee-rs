//! `CogneeHandle` — the `JsBox`-boxed SDK handle the TypeScript SDK is built on.
//!
//! The portable inner state ([`HandleState`]) now lives in
//! `cognee-bindings-common` and is imported here. This module keeps only the
//! neon-specific parts: the `CogneeHandle` struct, its `Finalize` impl, and the
//! three Neon export functions (`cogneeNew`, `cogneeWarm`, `cogneeOwnerId`).
//!
//! Phase 1 exports: `cogneeNew`, `cogneeWarm`, `cogneeOwnerId`. SDK operations
//! (add/cognify/search/…) are later phases and follow the canonical pattern:
//! `let svc = handle.services().await?; <cognee api>(…, svc.*); serde_to_js`.

use std::sync::Arc;

use neon::prelude::*;

use cognee::config::{ConfigManager, Settings};

// Re-export HandleState at the crate level so sdk_*.rs modules that reference
// `crate::sdk::HandleState` continue to resolve.
pub use cognee_bindings_common::HandleState;

use crate::errors::throw_sdk_error;
use crate::json::stringify_js;
use crate::runtime::{ensure_runtime, runtime, try_runtime};

/// The boxed SDK handle. Survives across JS calls (held by a `JsBox`).
pub struct CogneeHandle {
    pub state: Arc<HandleState>,
}

impl Finalize for CogneeHandle {
    /// Give the resources back when V8 collects a handle the program never
    /// closed.
    ///
    /// Dropping the `Arc`s is not enough: the relational pool's destructor lets
    /// its connections tear down concurrently, so a SQLite database's
    /// `-wal`/`-shm` sidecars end up orphaned on disk (issue #132). This is the
    /// *implicit* teardown, so it releases without closing — anything still
    /// holding a clone of the state (an in-flight op) keeps working and simply
    /// re-warms.
    ///
    /// `finalize` runs on the JavaScript thread during garbage collection, where
    /// blocking would stall the event loop, so the teardown is handed to the
    /// tokio runtime instead. That makes it best-effort: a process that exits
    /// promptly afterwards may not give the task time to run, which is why
    /// [`Cognee.close()`](cognee_close) exists and is the documented path.
    fn finalize<'a, C: Context<'a>>(self, _cx: &mut C) {
        // Nothing was ever opened → nothing to release, and no reason to touch
        // the runtime (which may not even be initialised).
        if !self.state.has_open_resources() {
            return;
        }
        if let Some(rt) = try_runtime() {
            let state = self.state;
            rt.spawn(async move { state.release().await });
        }
    }
}

impl CogneeHandle {
    /// Build the handle from settings (sync, no I/O).
    fn new_from_settings(settings: Settings) -> Self {
        CogneeHandle {
            state: Arc::new(HandleState::from_settings(settings)),
        }
    }
}

/// `cogneeNew(settingsJson?) -> JsBox<CogneeHandle>`
///
/// Pure/sync: builds `Settings`, wraps in `ConfigManager` → `ComponentManager`.
/// Does NOT touch the DB and does NOT resolve `owner_id`. Ensures the global
/// tokio runtime exists so later async exports can `spawn` onto it.
///
/// **Precedence is a true 3-way overlay: `defaults < env < object`.**
/// - With no / `null` / `undefined` argument → `ConfigManager::from_env()`
///   (defaults overlaid by env).
/// - With an argument (a JS object or a JSON string whose keys are `Settings`
///   field names) → start from the env-derived `Settings` and apply **only the
///   keys the object actually provides** on top. Fields absent from the object
///   keep their env (or default) value; fields present in the object win.
///
/// The overlay is done at the `serde_json::Value` level (not via a
/// `serde(default)` re-deserialization of a partial object, which cannot tell
/// "absent" from "equal to the default" and would silently reset absent fields
/// to defaults): the env `Settings` is serialized to a JSON object, the
/// provided keys are merged onto it, then the merged object is deserialized
/// back into `Settings`.
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
            // Parse the argument as a JSON object (reject anything else).
            let provided = match serde_json::from_str::<serde_json::Value>(&json) {
                Ok(serde_json::Value::Object(map)) => map,
                Ok(_) => return cx.throw_error("settings must be a JSON object"),
                Err(e) => return cx.throw_error(format!("invalid settings JSON: {e}")),
            };

            // Start from the env-derived Settings (defaults + env overlay) and
            // merge ONLY the keys the object actually provides on top — so
            // env/default values for absent keys survive (defaults < env < object).
            let base = settings_from_env();
            let mut merged = serde_json::to_value(&base)
                .or_else(|e| cx.throw_error(format!("failed to serialize base settings: {e}")))?;
            if let serde_json::Value::Object(ref mut base_map) = merged {
                for (key, value) in provided {
                    base_map.insert(key, value);
                }
            }
            serde_json::from_value::<Settings>(merged)
                .or_else(|e| cx.throw_error(format!("invalid settings: {e}")))?
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

/// `cogneeClose(handle)`
///
/// Close the handle: release the relational connection pool and, with it, a
/// SQLite database's `-wal`/`-shm` sidecar files, then mark the handle closed.
///
/// **Synchronous and blocking** — deliberately. The point of a close is that the
/// files are gone when it returns (a caller's next act is often to delete the
/// directory they live in), and a synchronous close is also what `Symbol.dispose`
/// requires. The work is a pool drain, measured in microseconds, not a network
/// round-trip. Dropping the handle instead is a race that orphans the files
/// (issue #132).
///
/// Idempotent, and a no-op on a handle that was never warmed. Any op started
/// afterwards rejects with a "handle is closed" error rather than silently
/// reopening the database.
pub fn cognee_close(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    if let Some(rt) = try_runtime() {
        state.close_blocking(rt.handle());
    }
    Ok(cx.undefined())
}

/// `Settings` populated from the environment (defaults + env overlay).
fn settings_from_env() -> Settings {
    ConfigManager::from_env().read().clone()
}
