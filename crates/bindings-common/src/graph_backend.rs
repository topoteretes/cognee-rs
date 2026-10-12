//! Process-wide registry of graph-backend factories, keyed by name.
//!
//! [`cognee::cognify::CognifyConfig::with_graph_backend`] (added in #239) lets a
//! **Rust** caller swap the LLM fact extractor for a
//! [`ChunkGraphExtractor`]. Every other caller — the C API, the Python wheel,
//! the Neon addon, the JNI cdylib — reaches cognify through
//! [`crate::ops::pipeline`] with a JSON `opts` object, and had no way to say
//! which extractor to use: the seam was reachable from Rust only.
//!
//! This module closes that gap without teaching the bindings facade about any
//! particular backend. A backend crate registers a
//! [`GraphExtractorFactory`](cognee::GraphExtractorFactory) on the
//! handle's [`ComponentRegistry`](cognee::ComponentRegistry) (which
//! also registers the backend's config sections); the process-wide
//! [`register_graph_backend`] slot predates that and is kept as a shim, consulted
//! after the registry. [`crate::ops::pipeline::cognify_config_with_opts`] then
//! honours `{"graphBackend": …}` in the opts by looking that kind up, falling
//! back to the configured default `graph_extraction.backend` (`GRAPH_EXTRACTOR`,
//! default `llm`, the built-in extractor). No backend is registered in this
//! repository, so every current caller is unaffected: the only reachable new
//! behaviour is an error for a kind nobody can serve.
//!
//! ## Shape
//!
//! This mirrors the `set_handle_factory` seam the JNI crate already exposes
//! (`cognee-java-jni::handle`): a process-wide slot installed once, before the
//! first call that reads it, by whoever links the implementation in. The
//! differences are that there are many named slots rather than one, and that a
//! factory is `async` because building a backend can do real I/O (resolve a
//! model directory, open a runtime).
//!
//! ## An unknown kind is an error, never a fallback
//!
//! A caller who asked for a specific extractor and silently got the LLM one
//! would be billed for tokens they deliberately opted out of, and would get a
//! graph built by something other than what they asked for, with nothing in the
//! result saying so. [`graph_backend_from_opts`] therefore fails, naming the
//! kind and listing what this build does register.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use serde_json::Value;

use cognee::ComponentManager;
use cognee::LLM_GRAPH_EXTRACTOR;
use cognee::cognify::ChunkGraphExtractor;

use crate::SdkError;

/// The opts key carrying the backend selection.
pub const GRAPH_BACKEND_OPT: &str = "graphBackend";

/// What a [`GraphBackendFactory`] returns.
///
/// Boxed rather than `impl Future` because the factory is stored as a plain
/// `fn` pointer in a `static`, which cannot name an opaque return type.
pub type GraphBackendFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ChunkGraphExtractor>, SdkError>> + Send>>;

/// Builds one backend instance from the `"graphBackend"` opts value.
///
/// The argument is the raw value the caller passed — the bare kind string, or
/// the whole `{"kind": …, …}` object including whatever tuning keys the backend
/// defines. This crate reads only `kind`; everything else is the factory's to
/// validate, and an unrecognised key there should be an error rather than a
/// silently ignored setting.
///
/// A factory is called once per cognify, so it is also the right place to reset
/// any per-run state the backend carries.
pub type GraphBackendFactory = fn(Value) -> GraphBackendFuture;

/// `BTreeMap` (not `HashMap`) so [`registered_graph_backend_kinds`] — which
/// only ever appears in an error message — is ordered, and the message is
/// reproducible.
static REGISTRY: RwLock<BTreeMap<String, GraphBackendFactory>> = RwLock::new(BTreeMap::new());

/// Register `factory` under `kind`, e.g. `"gliner"`, process-wide.
///
/// Prefer registering a `GraphExtractorFactory` on the handle's
/// `ComponentRegistry`: it is per handle and brings the backend's config
/// sections. This slot is consulted after the registry.
///
/// Call before the first cognify that could name this kind. A crate that is
/// linked in purely to provide a backend has no call site of its own to do it
/// from; the JNI cloud cdylib's `.init_array` module initializer is the
/// established pattern for that case.
///
/// # Errors
/// Returns `Err(factory)` if `kind` is already registered (or is `llm`, the
/// built-in extractor's reserved kind), leaving the existing
/// registration in place — the same first-writer-wins contract, and the same
/// "hand the loser back" signature, as `set_handle_factory`. Callers that
/// register from a module initializer, where there is nothing to report an
/// error to, can discard it: a second call cannot happen.
pub fn register_graph_backend(
    kind: &str,
    factory: GraphBackendFactory,
) -> Result<(), GraphBackendFactory> {
    // A poisoned registry only means some other thread panicked while holding
    // the lock; the map itself is a plain insert away from consistent, and
    // refusing to register would be a worse outcome than reading it.
    let mut registry = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if registry.contains_key(kind) || kind.eq_ignore_ascii_case(LLM_GRAPH_EXTRACTOR) {
        return Err(factory);
    }
    registry.insert(kind.to_owned(), factory);
    Ok(())
}

/// Every kind registered through [`register_graph_backend`], sorted. Empty in
/// an OSS build. Kinds on a handle's registry are not included; see
/// [`available_graph_backend_kinds`].
#[must_use]
pub fn registered_graph_backend_kinds() -> Vec<String> {
    let registry = REGISTRY.read().unwrap_or_else(|e| e.into_inner());
    registry.keys().cloned().collect()
}

/// The `(kind, spec)` the opts ask for, or `None` when they ask for nothing.
///
/// `None` — no `graphBackend` key, or an explicit `null` — is the ordinary
/// path: the LLM fact extractor, unchanged.
///
/// # Errors
/// [`SdkError::Validation`] if `graphBackend` is present but is neither a
/// string nor an object carrying a string `kind`. A malformed selection is
/// never read as "no selection": that is the silent-fallback this seam exists
/// to prevent.
pub fn graph_backend_spec(opts: &Value) -> Result<Option<(String, Value)>, SdkError> {
    let Some(spec) = opts.get(GRAPH_BACKEND_OPT) else {
        return Ok(None);
    };
    if spec.is_null() {
        return Ok(None);
    }
    match spec {
        Value::String(kind) => Ok(Some((kind.clone(), spec.clone()))),
        Value::Object(map) => {
            let kind = map
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SdkError::Validation(format!(
                        "`{GRAPH_BACKEND_OPT}` object requires a string `kind`{}",
                        known_kinds_suffix(&registered_graph_backend_kinds())
                    ))
                })?
                .to_owned();
            Ok(Some((kind, spec.clone())))
        }
        other => Err(SdkError::Validation(format!(
            "`{GRAPH_BACKEND_OPT}` must be a kind string or an object with a string \
             `kind`, got {other}{}",
            known_kinds_suffix(&registered_graph_backend_kinds())
        ))),
    }
}

/// The extractor this call uses: the opts' selection, else the configured
/// `graph_extraction.backend`. `None` is the built-in LLM extractor.
///
/// # Errors
/// As [`graph_backend_spec`].
fn resolve_graph_backend(
    cm: &ComponentManager,
    opts: &Value,
) -> Result<Option<(String, Value)>, SdkError> {
    let (kind, spec) = match graph_backend_spec(opts)? {
        Some(selected) => selected,
        None => {
            let kind = cm.settings().graph_extraction().backend;
            let spec = Value::String(kind.clone());
            (kind, spec)
        }
    };
    let trimmed = kind.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(LLM_GRAPH_EXTRACTOR) {
        return Ok(None);
    }
    Ok(Some((kind, spec)))
}

/// Whether the process-wide shim registry serves `kind`.
fn legacy_registered(kind: &str) -> bool {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(kind)
}

/// Validate the selection without building anything: the shape parses and a
/// factory is registered for the kind (the opts' or the configured default).
///
/// Cheap enough to call before side effects. `add_and_cognify` uses it so a
/// caller who misspelled a backend does not get their documents ingested by a
/// call that was always going to fail.
///
/// # Errors
/// As [`graph_backend_spec`], plus [`SdkError::Validation`] for a kind nothing
/// is registered under.
pub fn check_graph_backend_registered(cm: &ComponentManager, opts: &Value) -> Result<(), SdkError> {
    let Some((kind, _)) = resolve_graph_backend(cm, opts)? else {
        return Ok(());
    };
    if cm.registry().has_graph_extractor(&kind) || legacy_registered(&kind) {
        return Ok(());
    }
    Err(unknown_kind(cm, &kind))
}

/// Build the backend this call uses, or `None` for the built-in LLM
/// extractor. The handle's registry is consulted first, then the
/// process-wide [`register_graph_backend`] shim.
///
/// # Errors
/// As [`check_graph_backend_registered`], plus whatever the factory returns.
pub async fn graph_backend_from_opts(
    cm: &ComponentManager,
    opts: &Value,
) -> Result<Option<Arc<dyn ChunkGraphExtractor>>, SdkError> {
    let Some((kind, spec)) = resolve_graph_backend(cm, opts)? else {
        return Ok(None);
    };
    if cm.registry().has_graph_extractor(&kind) {
        return Ok(cm.graph_extractor(Some(&kind), &spec).await?);
    }
    // Copy the fn pointer out and drop the guard: a factory may await, and the
    // registry lock must not be held across that.
    let factory = {
        let registry = REGISTRY.read().unwrap_or_else(|e| e.into_inner());
        registry.get(&kind).copied()
    };
    let factory = factory.ok_or_else(|| unknown_kind(cm, &kind))?;
    factory(spec).await.map(Some)
}

/// Every kind `cm` can serve — its registry's and the shim's — sorted.
#[must_use]
pub fn available_graph_backend_kinds(cm: &ComponentManager) -> Vec<String> {
    let mut kinds = cm.registry().graph_extractor_kinds();
    kinds.extend(registered_graph_backend_kinds());
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The refusal for a kind nothing is registered under.
///
/// Spells out that cognify did **not** run, because the alternative reading —
/// "warning, used the LLM instead" — is the one that costs money.
fn unknown_kind(cm: &ComponentManager, kind: &str) -> SdkError {
    SdkError::Validation(format!(
        "no graph backend is registered for `{GRAPH_BACKEND_OPT}` kind {kind:?}{}. \
         Cognify was NOT run with the LLM extractor instead.",
        known_kinds_suffix(&available_graph_backend_kinds(cm))
    ))
}

/// `"; this build registers: …"`, or a plain statement that it registers none.
fn known_kinds_suffix(kinds: &[String]) -> String {
    if kinds.is_empty() {
        return "; this build registers no graph backends".to_owned();
    }
    format!("; this build registers: {}", kinds.join(", "))
}
