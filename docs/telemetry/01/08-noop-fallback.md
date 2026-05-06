# Task 08 — No-deps fallback for `cognee-observability`

**Status:** Not started
**Owner:** _unassigned_
**Depends on:**
- [Task 02 — Scaffold the `cognee-observability` workspace crate](./02-observability-crate-scaffold.md) (provides the crate skeleton, `TelemetryGuard`, `OtelSettings`, and the `#[cfg]`-gated `real` / `noop` module split).
- [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-implement-init-otel-and-guard.md) (defines the `BoxedTelemetryLayer` type alias and the public return signature `Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError>` the noop path must mirror exactly).

**Blocks:**
- [Task 05 — `cognee-lib` re-exports & subscriber composition helper](./05-cognee-lib-reexports.md)
- [Task 06 — Refactor CLI subscriber](./06-refactor-cli-subscriber.md)
- [Task 07 — Refactor HTTP server subscriber](./07-refactor-http-server-subscriber.md)

**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md)

---

## 1. Goal

When `cognee-observability` is built **without** `--features telemetry` (the default per [decision 1](../01-otel-otlp-export.md#design-decisions-locked) of the locked design table), the public surface — `init_telemetry`, `TelemetryGuard`, `TelemetryInitError`, `is_tracing_enabled`, `parse_otlp_headers`, `BoxedTelemetryLayer` — must still exist with **identical signatures** to the `telemetry`-on path, with noop semantics:

- `init_telemetry(_: &OtelSettings) -> Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError>` always succeeds with an identity layer plus an empty guard.
- `TelemetryGuard::noop()` is a zero-sized struct whose `Drop` is empty.
- The returned `BoxedTelemetryLayer` is type-erased so callers in `cognee-cli`, `cognee-http-server`, and `cognee-lib` write `Registry::default().with(...).with(layer)` regardless of feature state.

This is the foundation for [decision 1](../01-otel-otlp-export.md#design-decisions-locked) — every default `cargo build` hits this path. If it fails to compile, the whole project fails to build by default.

## 2. Rationale

### 2.1 Why a uniform API across feature states

[Tasks 06](./06-refactor-cli-subscriber.md) and [07](./07-refactor-http-server-subscriber.md) refactor the CLI and HTTP server `init_tracing()` functions to compose an OTEL bridge layer into the existing `tracing-subscriber::Registry`. If `init_telemetry` had a different signature in the two feature states (or only existed in one), every call site would need a `#[cfg(feature = "telemetry")]` fork — multiplying compile-time configurations and creating a class of bugs where the off-path is rarely exercised. By keeping the signature identical, the `with(...)` chain in those two binaries compiles unchanged whether telemetry is on or off.

This mirrors the established pattern of `tracing_subscriber::layer::Identity` (returned by `Layer::and_then` and similar combinators when one side is a noop) and `tracing_appender::non_blocking::WorkerGuard` (always returned, even when writes go nowhere meaningful).

### 2.2 Why type erasure (`Box<dyn Layer<...>>`)

The real `tracing-opentelemetry::OpenTelemetryLayer<S, Tracer>` is a concrete generic type with two type parameters; the noop path wants to return either `tracing_subscriber::layer::Identity` (the canonical noop layer) or a custom `NoopLayer<S>`. These types are **structurally different**, so the only way to give callers a uniform return type is type erasure.

Two options exist:

1. **`Box<dyn Layer<Registry> + Send + Sync + 'static>`** — heap allocation, dynamic dispatch on every `on_event` / `on_enter` / `on_exit`. The vtable cost is dwarfed by anything OTEL does, and the noop arm pays at most one box allocation at process start.
2. **An `enum BoxedTelemetryLayer { Real(...), Noop(Identity) }`** — would require mirroring `Layer<S>` by hand and would bake `OpenTelemetryLayer`'s exact generics into the enum, defeating the goal of cfg-isolation.

Option 1 wins. The alias is:

```rust
pub type BoxedTelemetryLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry>
        + Send
        + Sync
        + 'static>;
```

This alias is defined **regardless of feature state** (it depends only on `tracing-subscriber`, which is an always-on dep per [task 02 §4.2](./02-observability-crate-scaffold.md#42-create-cratesobservabilitycargotoml)). Both the `real` and `noop` modules box their layer into it.

### 2.3 Why `tracing_subscriber::layer::Identity` is the right noop

Per `tracing-subscriber` 0.3, [`tracing_subscriber::layer::Identity`](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/layer/struct.Identity.html) is a unit struct that implements `Layer<S>` for **any** `S: Subscriber` with all methods left as the trait defaults (i.e. it observes nothing and forwards nothing). Concretely:

- `Identity` is `Send + Sync + 'static` (no fields).
- `impl<S: Subscriber> Layer<S> for Identity` exists upstream — verified against the published rustdoc; it has been stable since 0.3.0.
- `Identity::new()` is a `const fn`; we call it once and box the result.

Therefore we don't need to hand-roll a `NoopLayer<S>`. If a future `tracing-subscriber` release changes `Identity`'s bounds, swap to a tiny `pub struct NoopLayer; impl<S: Subscriber> Layer<S> for NoopLayer {}` — the `Layer` trait's default methods all return `()` so the impl body is empty.

### 2.4 Why `parse_otlp_headers` lives in both paths

`parse_otlp_headers(&str) -> Vec<(String, String)>` is pure string-splitting logic (parses the comma-separated `key=value` form Python uses for `OTEL_EXPORTER_OTLP_HEADERS`). It has zero OTEL dependencies — the function builds plain `String` tuples. Hosting it in both paths means:

- Unit tests for header parsing (per [parent doc §Testing strategy](../01-otel-otlp-export.md#testing-strategy)) run under default features (no OTEL deps in the test build → faster CI).
- Future code that wants to *display* configured headers (e.g. a `cognee config show` command) can call it without flipping `--features telemetry`.

Same argument applies to `is_tracing_enabled(&OtelSettings) -> bool` — a 3-line function that checks a flag and a string emptiness check.

## 3. Pre-conditions

- [Task 02](./02-observability-crate-scaffold.md) is merged: the crate exists, `OtelSettings` and `TelemetryGuard` stubs are in place, `lib.rs` has the `#[cfg]`-gated `real` / `noop` module split, and `cargo check -p cognee-observability` passes.
- [Task 04](./04-implement-init-otel-and-guard.md) is merged or in flight: it defines `BoxedTelemetryLayer`, fills `real::init_telemetry`, and pins the `Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError>` return signature and the `TelemetryInitError` enum's variants.
  - **Coordination note:** if task 04 lands first, this task only fills `noop.rs`. If they land in parallel, this task introduces `BoxedTelemetryLayer` in `lib.rs` (so it is visible to both arms) and task 04 reuses it. The author of whichever PR merges second rebases.
- A clean `cargo check --workspace` on `main`.

## 4. Step-by-step

### 4.1 Define `BoxedTelemetryLayer` in `lib.rs`

The alias must be visible regardless of feature state, so it lives in `lib.rs` (not in `real.rs` or `noop.rs`):

```rust
// crates/observability/src/lib.rs (additions)

use tracing_subscriber::Registry;

/// Boxed `tracing-subscriber` layer returned by [`init_telemetry`].
///
/// The concrete type differs by feature state:
///
/// - With `--features telemetry`: a boxed `tracing_opentelemetry::OpenTelemetryLayer<Registry, Tracer>`.
/// - Without: a boxed [`tracing_subscriber::layer::Identity`] (observes nothing).
///
/// Type erasure lets call sites compose the layer uniformly:
///
/// ```ignore
/// let (layer, _guard) = cognee_observability::init_telemetry(&settings)?;
/// let subscriber = tracing_subscriber::Registry::default()
///     .with(filter)
///     .with(fmt_layer)
///     .with(layer); // <-- same code regardless of feature state
/// ```
pub type BoxedTelemetryLayer =
    Box<dyn tracing_subscriber::Layer<Registry> + Send + Sync + 'static>;
```

### 4.2 Update the `init_telemetry` return signature in `lib.rs`

Replace the placeholder from task 02 (which returned `Result<TelemetryGuard, OtelInitError>`) with the final form coordinated with task 04:

```rust
pub fn init_telemetry(
    settings: &OtelSettings,
) -> Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError> {
    #[cfg(feature = "telemetry")]
    {
        real::init(settings)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::init(settings)
    }
}
```

Also rename `OtelInitError` → `TelemetryInitError` to match [decision 10](../01-otel-otlp-export.md#design-decisions-locked) (broader name to allow future log/metric error variants).

### 4.3 Flesh out `crates/observability/src/noop.rs`

Replace the stub from task 02 with the final body. **Gated** `#[cfg(not(feature = "telemetry"))]` (the `mod noop;` declaration in `lib.rs` is itself gated, so the gate inside the file is belt-and-braces but documents intent and lets `cargo check --features telemetry` still parse the file if it is ever included unconditionally during refactor).

```rust
//! Noop fallback for the `cognee-observability` public API.
//!
//! Active when the `telemetry` cargo feature is **off** (the default).
//! Every public function below mirrors the signature of its counterpart in
//! [`crate::real`] but performs no work and pulls in no OTEL crates. See
//! [`docs/telemetry/01/08-noop-fallback.md`](../../docs/telemetry/01/08-noop-fallback.md)
//! for the rationale.

use tracing_subscriber::layer::Identity;

use crate::{BoxedTelemetryLayer, OtelSettings, TelemetryGuard, TelemetryInitError};

/// Build the noop bridge layer + guard. Always succeeds.
pub(crate) fn init(
    _settings: &OtelSettings,
) -> Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError> {
    let layer: BoxedTelemetryLayer = Box::new(Identity::new());
    Ok((layer, TelemetryGuard::noop()))
}

/// Mirror of `crate::real::is_tracing_enabled`. With `telemetry` off, the
/// answer is always `false` because nothing in the process can export
/// spans even if the user opted in via env. Callers that want to gate
/// log lines on the user's *intent* should read `OtelSettings` directly.
#[inline]
#[must_use]
pub fn is_tracing_enabled(_settings: &OtelSettings) -> bool {
    false
}

/// Pure-logic header parser, available regardless of feature state.
///
/// Splits the OTEL-standard `OTEL_EXPORTER_OTLP_HEADERS` form
/// (`"key1=value1,key2=value2"`) into a list of `(key, value)` pairs.
/// Whitespace around keys and values is trimmed. Empty entries are
/// skipped. An entry without `=` is ignored.
#[must_use]
pub fn parse_otlp_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (k, v) = entry.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_returns_ok_with_noop_guard() {
        let settings = OtelSettings::default();
        let (_layer, _guard) = init(&settings).expect("noop init never fails");
        // Dropping the guard must be a no-op; if Drop did anything bad
        // this test would crash.
    }

    #[test]
    fn is_tracing_enabled_is_always_false() {
        let mut settings = OtelSettings::default();
        settings.tracing_enabled = true;
        settings.exporter_otlp_endpoint = "http://localhost:4317".into();
        assert!(!is_tracing_enabled(&settings));
    }

    #[test]
    fn parse_otlp_headers_empty() {
        assert!(parse_otlp_headers("").is_empty());
    }

    #[test]
    fn parse_otlp_headers_single() {
        let parsed = parse_otlp_headers("authorization=Bearer abc");
        assert_eq!(
            parsed,
            vec![("authorization".to_string(), "Bearer abc".to_string())]
        );
    }

    #[test]
    fn parse_otlp_headers_multi_with_whitespace() {
        let parsed = parse_otlp_headers(" a = 1 , b=2 ,, c =3");
        assert_eq!(
            parsed,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn parse_otlp_headers_skips_malformed() {
        let parsed = parse_otlp_headers("good=ok,bad,also=fine");
        assert_eq!(
            parsed,
            vec![
                ("good".to_string(), "ok".to_string()),
                ("also".to_string(), "fine".to_string()),
            ]
        );
    }
}
```

### 4.4 Flesh out `TelemetryGuard` Drop semantics

In `crates/observability/src/guard.rs` keep `TelemetryGuard::noop()` as the cheap constructor. The `Drop` impl is shared between feature states (see [task 04](./04-implement-init-otel-and-guard.md) for the real-path body); under `not(feature = "telemetry")` the struct has no fields beyond `_private: ()` so `Drop` is automatically empty. To document this:

```rust
// crates/observability/src/guard.rs (final form)

/// RAII handle returned by [`crate::init_telemetry`].
///
/// Holding the guard for the lifetime of `main()` (CLI) or for as long as
/// `AppState` is alive (HTTP server) ensures the final batch of spans is
/// exported before the process exits. With the `telemetry` feature off,
/// the guard is zero-sized and its `Drop` runs no code — it exists only
/// so call sites can be uniform.
#[must_use = "TelemetryGuard must be held for the lifetime of the process to flush spans on shutdown"]
pub struct TelemetryGuard {
    #[cfg(feature = "telemetry")]
    pub(crate) provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(not(feature = "telemetry"))]
    _private: (),
}

impl TelemetryGuard {
    /// Construct a noop guard. Used by the off-path and by tests.
    #[inline]
    pub(crate) fn noop() -> Self {
        Self {
            #[cfg(feature = "telemetry")]
            provider: None,
            #[cfg(not(feature = "telemetry"))]
            _private: (),
        }
    }
}

#[cfg(not(feature = "telemetry"))]
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // intentionally empty — noop guard
    }
}

// The `#[cfg(feature = "telemetry")] impl Drop` lives in `real.rs`
// alongside the provider field; see task 04.
```

This makes the noop `Drop` explicit (vs. relying on the absence of an impl) and ensures the file produces an `impl Drop` symbol in both feature states for tooling consistency.

### 4.5 Wire the cfg-gated module switching in `lib.rs`

The final shape (after task 02's scaffold + this task's renames + task 04's signature) is:

```rust
// crates/observability/src/lib.rs (final glue)

#![deny(missing_docs)]

mod error;
mod guard;
pub mod settings;

#[cfg(feature = "telemetry")]
mod real;

#[cfg(not(feature = "telemetry"))]
mod noop;

pub use error::TelemetryInitError;
pub use guard::TelemetryGuard;
pub use settings::OtelSettings;

// `is_tracing_enabled` and `parse_otlp_headers` are exposed from whichever
// arm is active. The signatures match exactly so external callers cannot
// observe a difference.
#[cfg(feature = "telemetry")]
pub use real::{is_tracing_enabled, parse_otlp_headers};

#[cfg(not(feature = "telemetry"))]
pub use noop::{is_tracing_enabled, parse_otlp_headers};

// (BoxedTelemetryLayer + init_telemetry as defined in §4.1 / §4.2.)
```

`init_telemetry` itself stays in `lib.rs` and dispatches into the active module — a single function whose body is `#[cfg]`-forked, so rustdoc renders one entry regardless of feature state.

### 4.6 Document the cross-arm contract in module-level rustdoc

Add a short paragraph at the top of `lib.rs` (inside the existing `//!` block from task 02):

> **Feature-state contract:** every `pub` item below has the same signature
> regardless of whether `telemetry` is enabled. With the feature on, real
> OTEL machinery is built; off, the same functions return zero-cost noops.
> Call sites can therefore depend on this crate unconditionally and let
> the cargo feature decide at link time whether spans actually leave the
> process. See
> [docs/telemetry/01/08-noop-fallback.md](../../docs/telemetry/01/08-noop-fallback.md)
> for the contract rationale.

### 4.7 Verify both paths compile

```bash
# Default features — no OTEL deps.
cargo check -p cognee-observability

# Telemetry on — full OTEL stack.
cargo check -p cognee-observability --features telemetry

# Make sure all dependent crates still compile under both shapes.
cargo check --workspace
cargo check --workspace --features cognee-lib/telemetry
```

Both `cargo check -p cognee-observability` runs must succeed and produce **identical public-item rustdoc** modulo the body of `init_telemetry`. Confirm via:

```bash
cargo doc -p cognee-observability --no-deps
cargo doc -p cognee-observability --no-deps --features telemetry
diff -r target/doc/cognee_observability target/doc/cognee_observability  # before/after run, structural sanity
```

(The `diff` is illustrative — in practice run each `cargo doc` against a fresh `target/`, copy the HTML aside, then compare. The public-item index pages should differ only in feature badges, not in items listed.)

## 5. Resulting code

### 5.1 `crates/observability/src/noop.rs` — full final contents

(See §4.3 above for the verbatim file. No additional source is required.)

### 5.2 `crates/observability/src/lib.rs` — cfg-switching glue (excerpt)

```rust
//! OpenTelemetry SDK bring-up and `tracing` bridge for cognee.
//!
//! ## Feature flags
//!
//! - `telemetry` (off by default) — pulls in the OTEL Rust SDK and
//!   builds a real `SdkTracerProvider`. With the feature off, every
//!   public function below is a zero-cost noop with the same signature.
//!
//! **Feature-state contract:** every `pub` item has the same signature
//! regardless of whether `telemetry` is enabled. See
//! [docs/telemetry/01/08-noop-fallback.md](../../docs/telemetry/01/08-noop-fallback.md).

#![deny(missing_docs)]

use tracing_subscriber::Registry;

mod error;
mod guard;
pub mod settings;

#[cfg(feature = "telemetry")]
mod real;
#[cfg(not(feature = "telemetry"))]
mod noop;

pub use error::TelemetryInitError;
pub use guard::TelemetryGuard;
pub use settings::OtelSettings;

#[cfg(feature = "telemetry")]
pub use real::{is_tracing_enabled, parse_otlp_headers};
#[cfg(not(feature = "telemetry"))]
pub use noop::{is_tracing_enabled, parse_otlp_headers};

/// Boxed `tracing-subscriber` layer returned by [`init_telemetry`].
pub type BoxedTelemetryLayer =
    Box<dyn tracing_subscriber::Layer<Registry> + Send + Sync + 'static>;

/// Initialize telemetry. With `telemetry` off, returns an identity layer
/// and a noop guard. With it on, builds a real `SdkTracerProvider`,
/// installs it globally, and returns the bridge layer + RAII flush guard.
pub fn init_telemetry(
    settings: &OtelSettings,
) -> Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError> {
    #[cfg(feature = "telemetry")]
    {
        real::init(settings)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::init(settings)
    }
}
```

### 5.3 `error.rs` adjustment

Rename `OtelInitError` → `TelemetryInitError`, kept `#[non_exhaustive]`. The `NotImplemented` placeholder from task 02 stays until task 04 fills in real variants; the noop path never returns an error variant.

```rust
//! Errors surfaced by [`crate::init_telemetry`].

use thiserror::Error;

/// Errors returned during telemetry initialization.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TelemetryInitError {
    /// Placeholder until task 04 lands real variants.
    #[error("telemetry initialization not yet implemented")]
    NotImplemented,
}
```

(The noop path uses `Result<..., TelemetryInitError>` only to match the real signature; it always returns `Ok(...)`.)

## 6. Verification

Run from the repo root:

```bash
# 1. Default features — feature OFF — no OTEL deps in the resolved graph.
cargo check -p cognee-observability
cargo tree -p cognee-observability | grep -E 'opentelemetry|tracing-opentelemetry' \
    && echo "UNEXPECTED: OTEL deps with feature off" \
    || echo "OK: no OTEL deps without feature"

# 2. Telemetry ON — OTEL deps must resolve and the same surface compile.
cargo check -p cognee-observability --features telemetry

# 3. Unit tests for the noop arm (header parser, init smoke, drop smoke).
cargo test -p cognee-observability

# 4. Whole workspace with feature OFF — proves CLI / HTTP server compile
#    against the noop API.
cargo check --workspace --all-targets

# 5. CLI binary smoke (default features, telemetry off) — must run a
#    no-op command without panic.
cargo run --bin cognee -- --help

# 6. HTTP server binary smoke (default features, telemetry off) — must
#    start, accept a /healthz, and shut down cleanly. Drop of
#    AppState.guard must not block.
cargo run --bin cognee-http-server -- --help

# 7. Rustdoc parity — render both feature states and confirm the public
#    API listing is the same set of items.
cargo doc -p cognee-observability --no-deps
cargo doc -p cognee-observability --no-deps --features telemetry

# 8. Project-wide gate.
scripts/check_all.sh
```

Expected:

- `cargo check -p cognee-observability` (no features) succeeds and emits **zero** OTEL deps in `cargo tree`.
- `cargo test -p cognee-observability` passes the five `noop::tests` cases (init, is-enabled, three header-parsing cases).
- `cargo check --workspace --all-targets` succeeds — this is the strongest signal that callers in `cognee-cli` and `cognee-http-server` did not regress.
- Both `cargo doc` runs produce HTML that lists the same public items: `init_telemetry`, `BoxedTelemetryLayer`, `TelemetryGuard`, `TelemetryInitError`, `OtelSettings`, `is_tracing_enabled`, `parse_otlp_headers`.
- `scripts/check_all.sh` exits 0 (fmt + check + clippy + capi/python/js binding checks per the [project guide](../../../.claude/CLAUDE.md#build--development)).

## 7. Files modified

| File | Change |
|---|---|
| [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs) | Define `BoxedTelemetryLayer` type alias; finalize `init_telemetry` signature; cfg-gate `pub use` of `is_tracing_enabled` / `parse_otlp_headers`; document feature-state contract. |
| [`crates/observability/src/noop.rs`](../../../crates/observability/src/noop.rs) | Replace task-02 stub with the full noop body — `init`, `is_tracing_enabled`, `parse_otlp_headers`, plus `#[cfg(test)]` unit tests. |
| [`crates/observability/src/guard.rs`](../../../crates/observability/src/guard.rs) | Add `#[cfg(not(feature = "telemetry"))] impl Drop` (empty body); split fields by feature gate. |
| [`crates/observability/src/error.rs`](../../../crates/observability/src/error.rs) | Rename `OtelInitError` → `TelemetryInitError` ([decision 10](../01-otel-otlp-export.md#design-decisions-locked)). |

No source outside `crates/observability/` is changed in this task. The dependent refactors (CLI, HTTP server, `cognee-lib` re-exports) live in [tasks 05](./05-cognee-lib-reexports.md), [06](./06-refactor-cli-subscriber.md), and [07](./07-refactor-http-server-subscriber.md).

## 8. Risks

1. **Type signature drift between `real` and `noop`.** If task 04 evolves `init_telemetry` to return e.g. `Result<(BoxedTelemetryLayer, TelemetryGuard, ResourceHandle), TelemetryInitError>` but the noop arm still returns the 2-tuple, `cargo check --no-default-features` breaks at every call site. **Mitigation:** the function signature lives in `lib.rs` (single source of truth) and the two `mod` arms must implement `pub(crate) fn init(&OtelSettings) -> Result<(BoxedTelemetryLayer, TelemetryGuard), TelemetryInitError>` exactly. Add a CI lane that runs `cargo check --workspace --all-targets` (default features) — already covered by [parent doc action item 12](../01-otel-otlp-export.md#action-items).

2. **`tracing_subscriber::layer::Identity` trait bounds.** The plan relies on `Identity: Layer<Registry> + Send + Sync + 'static`. This is true today (verified against `tracing-subscriber` 0.3 rustdoc), but a future major-version bump could change it. **Mitigation:** if `Identity` ever fails to satisfy the bounds, replace with a 5-line custom layer:

   ```rust
   pub(crate) struct NoopLayer;
   impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for NoopLayer {}
   ```

   The Layer trait's default method bodies are all `()`, so the impl block is genuinely empty.

3. **`Box<dyn Layer<Registry>>` vs subscriber composition with `EnvFilter`.** `tracing-subscriber` uses an `S: Subscriber` generic on `Layer`. If a future call site writes `Registry::default().with(filter).with(layer)` where `filter` produces a `Filtered<L, F, S>` such that `S != Registry`, the `Box<dyn Layer<Registry>>` may not compose. **Mitigation:** [task 05](./05-cognee-lib-reexports.md) defines the composition order canonically; the boxed layer always sits at the same position in the chain. If a real call site needs a non-`Registry` `S`, lift the alias to be generic (`type BoxedTelemetryLayer<S> = Box<dyn Layer<S> + ...>`) — but resist that until needed because it complicates the noop side.

4. **Future cross-arm symbol drift.** If a later task adds an OTEL-only symbol (e.g. `set_resource_attributes(...)`) and forgets to add a noop counterpart, callers can only use it under `--features telemetry` — defeating the uniform-API goal. **Mitigation:** document in `crates/observability/src/lib.rs` (per §4.6) that **every** `pub` item must have a counterpart in both arms; reviewers reject PRs that violate this.

5. **`#![deny(missing_docs)]` + cfg-gated re-exports.** Doc-comments live on the items in `real.rs` and `noop.rs`, not on the `pub use` lines. If only one arm documents an item, `cargo doc -p cognee-observability` fails on the other. **Mitigation:** copy doc-comments verbatim across arms (header parser is a good example). A small `#[doc = include_str!("../docs/parse_otlp_headers.md")]` indirection could DRY this up, but is overkill for two functions.

6. **`tracing-subscriber` already in default deps but only used here.** If a future workspace-wide cleanup proposes making `tracing-subscriber` optional in `cognee-observability`, the noop path breaks (it needs `Identity`). **Mitigation:** the [task 02 manifest](./02-observability-crate-scaffold.md#42-create-cratesobservabilitycargotoml) declares `tracing-subscriber.workspace = true` (always-on); this task does not change that, and a comment in `Cargo.toml` should note that the noop arm requires `Identity`.

## 9. References

- Parent gap doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md), in particular the
  ["When is OTEL enabled?"](../01-otel-otlp-export.md#when-is-otel-enabled) subsection (which
  states the noop layer is `tracing_subscriber::layer::Identity`),
  [Action items](../01-otel-otlp-export.md#action-items) #8 (this task) and #12 (CI lane covering
  the no-deps build), and [Design decisions table](../01-otel-otlp-export.md#design-decisions-locked)
  decisions 1, 6, 10.
- Sibling sub-docs:
  - [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md) — pre-condition
    (crate skeleton, `OtelSettings`, initial `noop.rs` stub).
  - [`04-implement-init-otel-and-guard.md`](./04-implement-init-otel-and-guard.md) — pre-condition
    (`BoxedTelemetryLayer`, `TelemetryInitError` variants, real `init_telemetry`).
  - [`05-cognee-lib-reexports.md`](./05-cognee-lib-reexports.md) — first downstream consumer of
    the uniform API.
  - [`06-refactor-cli-subscriber.md`](./06-refactor-cli-subscriber.md),
    [`07-refactor-http-server-subscriber.md`](./07-refactor-http-server-subscriber.md) — both
    rely on `with(layer)` compiling regardless of feature state.
- External:
  - [`tracing_subscriber::layer::Identity`](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/layer/struct.Identity.html)
    — canonical noop layer.
  - [`tracing_subscriber::Layer`](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/layer/trait.Layer.html)
    — trait whose default methods are no-ops.
  - [`tracing_appender::non_blocking::WorkerGuard`](https://docs.rs/tracing-appender/latest/tracing_appender/non_blocking/struct.WorkerGuard.html)
    — precedent for "always return an RAII guard, even when its drop is cheap".
- Project conventions: [`../../../.claude/CLAUDE.md`](../../../.claude/CLAUDE.md), specifically the
  [Coding conventions](../../../.claude/CLAUDE.md#coding-conventions) section (no `unwrap()` in
  non-test code; `expect("...")` only with a justifying message).
