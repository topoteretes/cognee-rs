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

use cognee::cognify::ChunkGraphExtractor;
use cognee::{
    AUTO_GRAPH_EXTRACTOR, ComponentManager, GraphExtractorChoice, LLM_GRAPH_EXTRACTOR,
    keyless_extractor_unavailable,
};

use crate::SdkError;

/// The opts key carrying the backend selection.
pub const GRAPH_BACKEND_OPT: &str = "graphBackend";

/// The opts key carrying Python's per-call `extractor` selection: `auto`,
/// `llm`, `gliner` (alias `gliner_demo`) or another registered kind, as a
/// bare string. [`GRAPH_BACKEND_OPT`] wins when both are given.
pub const EXTRACTOR_OPT: &str = "extractor";

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
/// Returns `Err(factory)` if `kind` is already registered (any case), or is
/// `llm` (the built-in extractor) or `auto` (the selection between it and
/// `gliner`), both reserved, leaving the existing
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
    if legacy_key(&registry, kind).is_some()
        || kind.eq_ignore_ascii_case(LLM_GRAPH_EXTRACTOR)
        || kind.eq_ignore_ascii_case(AUTO_GRAPH_EXTRACTOR)
    {
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

/// The `extractor` opt as a kind string, `None` when absent or `null`.
///
/// # Errors
/// [`SdkError::Validation`] when present but not a string.
fn extractor_opt(opts: &Value) -> Result<Option<String>, SdkError> {
    match opts.get(EXTRACTOR_OPT) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(kind)) => Ok(Some(kind.clone())),
        Some(other) => Err(SdkError::Validation(format!(
            "`{EXTRACTOR_OPT}` must be a kind string (`auto`, `llm`, `gliner`, …), got {other}"
        ))),
    }
}

/// A selection resolved to a backend other than the LLM extractor.
struct Resolved {
    /// The kind (`auto` and the `gliner_demo` alias already resolved).
    choice: GraphExtractorChoice,
    /// What the factory receives (see [`GraphExtractorChoice::spec_for`]).
    spec: Value,
}

/// The extractor this call uses: the opts' `graphBackend`, else their
/// `extractor`, else the configured `graph_extraction.backend`, resolved by
/// [`ComponentManager::resolve_graph_extractor`]. `None` is the built-in LLM
/// extractor, as is a `graphBackend` naming no kind (`""`), which has always
/// meant it. A blank `extractor` defers to the setting, as in Python.
///
/// # Errors
/// As [`graph_backend_spec`], plus [`SdkError::Validation`] for a
/// non-string `extractor`.
fn resolve_graph_backend(
    cm: &ComponentManager,
    opts: &Value,
) -> Result<Option<Resolved>, SdkError> {
    let (selected, spec) = match graph_backend_spec(opts)? {
        Some((kind, _)) if kind.trim().is_empty() => return Ok(None),
        Some((kind, spec)) => (Some(kind), spec),
        None => (extractor_opt(opts)?, Value::Null),
    };
    let choice = cm.resolve_graph_extractor(selected.as_deref(), &legacy_registered);
    if choice.is_llm() {
        return Ok(None);
    }
    let spec = choice.spec_for(&spec);
    Ok(Some(Resolved { choice, spec }))
}

/// Whether this call selects a graph-extraction backend other than the
/// built-in LLM extractor (the opts' selection, else the configured default),
/// without checking that anything is registered under it.
///
/// # Errors
/// As [`graph_backend_spec`].
pub fn selects_graph_backend(cm: &ComponentManager, opts: &Value) -> Result<bool, SdkError> {
    Ok(selected_graph_backend(cm, opts)?.is_some())
}

/// The backend this call selects other than the built-in LLM extractor, and
/// whether it came from `auto`, without checking that anything is
/// registered under it.
///
/// # Errors
/// As [`selects_graph_backend`].
pub fn selected_graph_backend(
    cm: &ComponentManager,
    opts: &Value,
) -> Result<Option<GraphExtractorChoice>, SdkError> {
    Ok(resolve_graph_backend(cm, opts)?.map(|resolved| resolved.choice))
}

/// The shim registry's key for `kind`, matched without regard to case: the
/// selection is lowercased when it is resolved, but a shim kind is stored as
/// registered.
fn legacy_key(registry: &BTreeMap<String, GraphBackendFactory>, kind: &str) -> Option<String> {
    registry
        .keys()
        .find(|key| key.eq_ignore_ascii_case(kind))
        .cloned()
}

/// Whether the process-wide shim registry serves `kind` (any case).
fn legacy_registered(kind: &str) -> bool {
    legacy_key(&REGISTRY.read().unwrap_or_else(|e| e.into_inner()), kind).is_some()
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
    let Some(resolved) = resolve_graph_backend(cm, opts)? else {
        return Ok(());
    };
    let kind = &resolved.choice.kind;
    if cm.registry().has_graph_extractor(kind) || legacy_registered(kind) {
        return Ok(());
    }
    Err(unknown_kind(cm, &resolved))
}

/// [`check_graph_backend_registered`] for a selection the opts make
/// themselves (a `graphBackend`, or a non-blank `extractor`); the configured
/// default is not checked.
///
/// For a cognify that needs no extractor (every item takes a code route): a
/// keyless deployment's default (`auto`) may name an extractor this build
/// does not serve, and must keep working, but a misspelled or malformed
/// selection the caller passed is still refused rather than ignored.
///
/// # Errors
/// As [`check_graph_backend_registered`], for an explicit selection only.
pub fn check_explicit_graph_backend(cm: &ComponentManager, opts: &Value) -> Result<(), SdkError> {
    let explicit = graph_backend_spec(opts)?.is_some()
        || extractor_opt(opts)?.is_some_and(|kind| !kind.trim().is_empty());
    if explicit {
        check_graph_backend_registered(cm, opts)?;
    }
    Ok(())
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
    let Some(resolved) = resolve_graph_backend(cm, opts)? else {
        return Ok(None);
    };
    let kind = &resolved.choice.kind;
    if cm.registry().has_graph_extractor(kind) {
        return Ok(cm.graph_extractor(Some(kind), &resolved.spec).await?);
    }
    // Copy the fn pointer out and drop the guard: a factory may await, and the
    // registry lock must not be held across that.
    let factory = {
        let registry = REGISTRY.read().unwrap_or_else(|e| e.into_inner());
        legacy_key(&registry, kind).and_then(|key| registry.get(&key).copied())
    };
    let factory = factory.ok_or_else(|| unknown_kind(cm, &resolved))?;
    factory(resolved.spec).await.map(Some)
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
fn unknown_kind(cm: &ComponentManager, resolved: &Resolved) -> SdkError {
    if resolved.choice.from_auto {
        return SdkError::Validation(format!(
            "{} Cognify was NOT run with the LLM extractor instead.",
            keyless_extractor_unavailable(&available_graph_backend_kinds(cm))
        ));
    }
    let kind = &resolved.choice.kind;
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
