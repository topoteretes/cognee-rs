# Task 01-05: Re-export telemetry API from `cognee-lib`

## Status

Not started.

## Owner / dependencies

- **Depends on**:
  - [Task 01-02 — `cognee-observability` crate scaffold](02-observability-crate-scaffold.md)
    creates the crate and its public surface (including the noop module
    skeleton).
  - [Task 01-03 — `telemetry` cargo feature wiring](03-cognee-lib-feature-wiring.md)
    adds `cognee-observability` as an optional dependency of
    `cognee-lib` behind `telemetry`.
  - [Task 01-04 — `init_telemetry` implementation](04-init-telemetry-implementation.md)
    finalises the names, signatures, and noop fallbacks of the public
    items being re-exported here (`init_telemetry`, `TelemetryGuard`,
    `TelemetryInitError`, `SettingsView`, `BoxedTelemetryLayer`,
    `parse_otlp_headers`, `is_tracing_enabled`,
    `already_instrumented`).
- **Blocks**:
  - [Task 01-06 — CLI subscriber refactor](06-cli-subscriber-refactor.md)
    will call `cognee_lib::telemetry::init_telemetry` rather than
    depending on `cognee-observability` directly.
  - [Task 01-07 — HTTP server subscriber refactor](07-http-server-subscriber-refactor.md)
    likewise consumes the API through `cognee-lib`.
- **Owner**: TBD.

## Rationale

`cognee-lib` is the umbrella facade every embedder consumes — it
already re-exports the public surface of every other workspace crate
under a topical module name (see
[`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs)):

```rust
pub mod add      { pub use cognee_ingestion::{...}; }
pub mod cognify  { pub use cognee_cognify::*; }
pub mod search   { pub use cognee_search::*; }
pub mod storage  { pub use cognee_storage::*; }
pub mod database { pub use cognee_database::*; }
pub mod graph    { pub use cognee_graph::{...}; }
pub mod vector   { pub use cognee_vector::{...}; }
pub mod embedding{ pub use cognee_embedding::{...}; }
pub mod llm      { pub use cognee_llm::*; }
// ...etc
```

Per
[decision 6 of the design table](../01-otel-otlp-export.md#design-decisions-locked),
the implementation crate is `cognee-observability` (a sibling of
`cognee-core`), but the *typical* consumer (CLI, HTTP server,
embedder applications, language bindings) reaches it via `cognee-lib`.
Forcing those consumers to add a second `cognee-observability =
{ path = "../observability" }` dependency to their `Cargo.toml`
breaks the established pattern — every other backend (graph, vector,
storage, …) is reachable through `cognee_lib::<topic>::*` and the
prelude, and OTEL setup should follow suit.

The new module is named **`telemetry`** to match the cargo feature
flag of the same name (`cognee-lib`'s `telemetry` feature toggles
this module on). Keeping the names aligned avoids the cognitive
mismatch of "feature `telemetry` enables module `observability`".

A thin re-export module also gives us a single place to:

- gate the symbols on the `telemetry` feature (the entire module is
  feature-gated — see step 1 below);
- swap in additional helpers later (e.g. metrics, log export) without
  re-stamping the call sites;
- house the `impl SettingsView for Settings` adapter so embedders do
  not need to write the trait impl themselves.

This task introduces the re-export shim, the `SettingsView` adapter
impl for `cognee_lib::config::Settings`, and the feature gate on the
new module declaration.

## Pre-conditions

- [Task 01-02](02-observability-crate-scaffold.md) has landed:
  the workspace member `crates/observability/` exists and exports
  the full public surface from its `lib.rs`:
  - `init_telemetry`
  - `TelemetryGuard`
  - `TelemetryInitError`
  - `SettingsView`
  - `BoxedTelemetryLayer`
  - `parse_otlp_headers`
  - `is_tracing_enabled`
  - `already_instrumented`
- Per the contract noted in step 2 below and locked by
  [task 01-08](08-noop-fallback.md), the `cognee-observability` crate
  exposes those names *unconditionally* — when its own `telemetry`
  feature is off, the symbols still exist as noop shapes
  (`TelemetryGuard::noop()`, `init_telemetry` returning that noop,
  etc.). Inside the `cognee-observability` crate the names are
  visible regardless of its own feature state; what gates them at
  the `cognee-lib` level is the optional dependency declaration
  described next.
- [Task 01-03](03-cognee-lib-feature-wiring.md) has landed:
  `crates/lib/Cargo.toml` lists `cognee-observability` as an optional
  workspace path dependency, and the `telemetry` feature on
  `cognee-lib` enables both `cognee-observability` and its
  `telemetry` feature:

  ```toml
  [dependencies]
  cognee-observability = { path = "../observability", optional = true }

  [features]
  telemetry = ["dep:cognee-observability", "cognee-observability/telemetry"]
  ```

  The `optional = true` declaration is **kept** — per design
  decision 1, telemetry is off by default, and the android-default
  build profile must remain lean. Because the dependency is optional,
  the new `pub mod telemetry;` declaration in `crates/lib/src/lib.rs`
  is itself gated behind `#[cfg(feature = "telemetry")]`. Embedders
  who never enable `telemetry` will not see the module, and the
  `cognee-observability` crate will not be linked into their build
  graph.

## Step-by-step

1. **Add the feature-gated module declaration to
   `crates/lib/src/lib.rs`.** Insert the following next to the other
   top-level module declarations (around the block that contains
   `pub mod session;`, `pub mod api;`, `pub mod component_manager;`
   — see [`lib.rs:130`–`136`](../../../crates/lib/src/lib.rs#L130)):

   ```rust
   #[cfg(feature = "telemetry")]
   pub mod telemetry;
   ```

   Order alphabetically with the other `pub mod`s for readability.
   The `#[cfg(feature = "telemetry")]` gate is required because
   `cognee-observability` is an `optional = true` dependency — the
   crate is only present in the build graph when the feature is on,
   so the module's `pub use` statements would fail to resolve
   otherwise.

2. **Create `crates/lib/src/telemetry.rs`** as a single-file
   module (no submodules — this is purely a re-export shim plus the
   `SettingsView` adapter impl). Its complete contents:

   ```rust
   //! Telemetry surface for embedders.
   //!
   //! Re-exports the public API of [`cognee_observability`] so that
   //! consumers reach OTEL setup through the same `cognee_lib::<topic>`
   //! pattern used for `storage`, `vector`, `graph`, etc.
   //!
   //! This module is only compiled when the `telemetry` cargo feature
   //! is enabled on `cognee-lib`. With the feature off, the module
   //! does not exist and `cognee-observability` is not linked into
   //! the build graph.

   pub use cognee_observability::{
       BoxedTelemetryLayer, SettingsView, TelemetryGuard, TelemetryInitError,
       already_instrumented, init_telemetry, is_tracing_enabled, parse_otlp_headers,
   };

   use crate::config::Settings;

   /// Adapter impl that lets [`Settings`] satisfy the
   /// [`SettingsView`] contract `cognee-observability` reads.
   ///
   /// All eight accessors borrow from the underlying `Settings` —
   /// no allocation, no validation. The returned strings are the
   /// raw configured values; the OTEL SDK / `init_telemetry`
   /// performs validation and applies defaults.
   impl SettingsView for Settings {
       fn tracing_enabled(&self) -> bool {
           self.cognee_tracing_enabled
       }

       fn service_name(&self) -> &str {
           &self.otel_service_name
       }

       fn otlp_endpoint(&self) -> &str {
           &self.otel_exporter_otlp_endpoint
       }

       fn otlp_headers(&self) -> &str {
           &self.otel_exporter_otlp_headers
       }

       fn otlp_protocol(&self) -> &str {
           &self.otel_exporter_otlp_protocol
       }

       fn span_processor(&self) -> &str {
           &self.otel_span_processor
       }

       fn traces_sampler(&self) -> &str {
           &self.otel_traces_sampler
       }

       fn traces_sampler_arg(&self) -> &str {
           &self.otel_traces_sampler_arg
       }
   }
   ```

   The whole file is only compiled under
   `#[cfg(feature = "telemetry")]` (because the parent `pub mod
   telemetry;` is gated). The `impl SettingsView for Settings` block
   therefore inherits the same gate — it only exists when
   `cognee-observability` (and `SettingsView` itself) is in scope.

3. **Re-export list — full coverage.** The eight public items
   re-exported above mirror the entire public surface of
   `cognee-observability` (verify against
   [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs)):

   | Symbol | Purpose |
   |---|---|
   | `init_telemetry` | One-shot initializer that builds the OTEL pipeline (or noop layer) and returns a guard. |
   | `TelemetryGuard` | RAII handle whose `Drop` flushes pending spans. |
   | `TelemetryInitError` | Error returned by `init_telemetry`. |
   | `SettingsView` | Trait the initializer reads its config from. Implemented for `Settings` in this file. |
   | `BoxedTelemetryLayer` | Boxed `tracing-subscriber` layer returned by `init_telemetry` for callers that build their own subscriber stack. |
   | `parse_otlp_headers` | Parser for `OTEL_EXPORTER_OTLP_HEADERS` / `Settings.otel_exporter_otlp_headers` (key=value comma-separated). |
   | `is_tracing_enabled` | Helper that checks the global tracing dispatch state. |
   | `already_instrumented` | Helper that checks whether a global subscriber/provider is already installed. |

4. **`SettingsView for Settings` field mapping.** The adapter maps
   the eight trait methods to `Settings` fields (verified against
   [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs)):

   | Trait method | `Settings` field |
   |---|---|
   | `tracing_enabled() -> bool` | `cognee_tracing_enabled` |
   | `service_name() -> &str` | `otel_service_name` |
   | `otlp_endpoint() -> &str` | `otel_exporter_otlp_endpoint` |
   | `otlp_headers() -> &str` | `otel_exporter_otlp_headers` |
   | `otlp_protocol() -> &str` | `otel_exporter_otlp_protocol` |
   | `span_processor() -> &str` | `otel_span_processor` |
   | `traces_sampler() -> &str` | `otel_traces_sampler` |
   | `traces_sampler_arg() -> &str` | `otel_traces_sampler_arg` |

   All field names already exist on `Settings` (see the `// --
   Observability ------` block in `config.rs`); no schema changes
   are required by this task.

5. **Decide whether to extend `prelude`.** `cognee-lib` already has a
   `pub mod prelude` block at
   [`lib.rs:147`–`179`](../../../crates/lib/src/lib.rs#L147) listing
   the symbols most commonly imported via `use cognee_lib::prelude::*`.
   The recommendation is to **leave the OTEL symbols out of the
   prelude** for the same reason `serve` / `serve_url` / `CloudClient`
   are gated behind `feature = "cloud"` there: telemetry setup is a
   one-shot `main()` concern and does not belong in everyday
   re-exports. Embedders can `use cognee_lib::telemetry::{
   init_telemetry, TelemetryGuard };` explicitly. (Keeping the
   namespace also preserves room for future
   `cognee_lib::telemetry::metrics`, `::logs`, etc.)

   If the user later requests prelude inclusion, append:

   ```rust
   #[cfg(feature = "telemetry")]
   pub use crate::telemetry::{init_telemetry, TelemetryGuard};
   ```

   inside the existing `pub mod prelude { ... }` — but do not do so as
   part of this task.

6. **Confirm `cargo check -p cognee-lib` succeeds** without the
   `telemetry` feature. The optional `cognee-observability`
   dependency is absent from the build graph; the
   `#[cfg(feature = "telemetry")]` gate hides the entire
   `telemetry` module (and its `SettingsView` impl) so nothing
   references the missing crate.

7. **Confirm `cargo check -p cognee-lib --features telemetry`
   succeeds.** With the feature on, `cognee-observability` is linked
   in, the `telemetry` module compiles, and the
   `impl SettingsView for Settings` block makes
   `init_telemetry(&settings, ...)` valid at every call site.

## Resulting code

### `crates/lib/src/telemetry.rs` (new, feature-gated by parent module)

```rust
//! Telemetry surface for embedders.
//!
//! Re-exports the public API of [`cognee_observability`] so that
//! consumers reach OTEL setup through the same `cognee_lib::<topic>`
//! pattern used for `storage`, `vector`, `graph`, etc.

pub use cognee_observability::{
    BoxedTelemetryLayer, SettingsView, TelemetryGuard, TelemetryInitError,
    already_instrumented, init_telemetry, is_tracing_enabled, parse_otlp_headers,
};

use crate::config::Settings;

impl SettingsView for Settings {
    fn tracing_enabled(&self) -> bool { self.cognee_tracing_enabled }
    fn service_name(&self) -> &str { &self.otel_service_name }
    fn otlp_endpoint(&self) -> &str { &self.otel_exporter_otlp_endpoint }
    fn otlp_headers(&self) -> &str { &self.otel_exporter_otlp_headers }
    fn otlp_protocol(&self) -> &str { &self.otel_exporter_otlp_protocol }
    fn span_processor(&self) -> &str { &self.otel_span_processor }
    fn traces_sampler(&self) -> &str { &self.otel_traces_sampler }
    fn traces_sampler_arg(&self) -> &str { &self.otel_traces_sampler_arg }
}
```

### `crates/lib/src/lib.rs` (delta)

The relevant region around
[line 130](../../../crates/lib/src/lib.rs#L130) becomes:

```rust
pub mod session;

pub mod api;
pub mod component_manager;
pub mod config;
pub mod context;
pub mod error;

#[cfg(feature = "telemetry")]
pub mod telemetry;   // ← added by this task
```

The `pub mod prelude { ... }` block remains unchanged (see step 5).

### `crates/lib/Cargo.toml` (no change required by this task)

The dependency declaration from task 01-03 remains:

```toml
[dependencies]
cognee-observability = { path = "../observability", optional = true }

[features]
telemetry = ["dep:cognee-observability", "cognee-observability/telemetry"]
```

The `optional = true` flag is intentionally **not** dropped — it
preserves the off-by-default behaviour locked in design decision 1
and keeps the android-default build lean.

## Verification

- [ ] `cargo check -p cognee-lib` (no features beyond defaults) —
      confirms that with `telemetry` off, the module is hidden and
      `cognee-observability` is not pulled in.
- [ ] `cargo check -p cognee-lib --no-default-features` — same
      expectation; the optional dependency stays out of the graph.
- [ ] `cargo check -p cognee-lib --features telemetry` — confirms
      the `telemetry` module compiles, the eight re-exports resolve,
      and `impl SettingsView for Settings` type-checks.
- [ ] `cargo check -p cognee-lib --all-features`.
- [ ] `cargo doc -p cognee-lib --features telemetry --no-deps` —
      open the generated rustdoc and confirm
      `cognee_lib::telemetry` is listed as a module on the crate
      landing page, with `init_telemetry`, `TelemetryGuard`,
      `TelemetryInitError`, `SettingsView`, `BoxedTelemetryLayer`,
      `parse_otlp_headers`, `is_tracing_enabled`, and
      `already_instrumented` visible inside it.
- [ ] `cargo doc -p cognee-lib --no-deps` (without `telemetry`) —
      the `telemetry` module should be absent from the rustdoc.
- [ ] `scripts/check_all.sh` — fmt + check + clippy + binding checks.

## Files modified

- [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs) — add
  `#[cfg(feature = "telemetry")] pub mod telemetry;` next to the
  other module declarations.
- **New**: `crates/lib/src/telemetry.rs` — single-file module
  containing the eight re-exports plus `impl SettingsView for
  Settings` (full contents above). Compiled only under
  `#[cfg(feature = "telemetry")]` via the parent `pub mod` gate.

No `Cargo.toml` changes are introduced by this task; the dependency
declaration shipped in [task 01-03](03-cognee-lib-feature-wiring.md)
already satisfies the build requirements.

## Risks

- **Module visibility tied to the feature flag.** With the
  recommended `#[cfg(feature = "telemetry")]` gate on `pub mod
  telemetry;`, embedders who do not enable the feature will not see
  `cognee_lib::telemetry::*` at all. Application code that wants a
  single call site across feature states must therefore guard its
  own `use cognee_lib::telemetry::...;` lines with a matching
  `#[cfg(feature = "telemetry")]` (or a downstream feature that
  enables `cognee-lib/telemetry`). This is the deliberate trade-off
  for keeping the default build lean — see design decision 1 in
  the parent doc. The CLI and HTTP server (tasks 01-06 / 01-07)
  enable the feature unconditionally and therefore see the module
  unconditionally; bindings (task 01-11) follow the same pattern.
- **Double-imports for downstream embedders.** Anyone who today takes
  a direct dependency on `cognee-observability` *and* on
  `cognee-lib` (with `telemetry` on) will see two paths to the same
  types (`cognee_observability::TelemetryGuard` vs
  `cognee_lib::telemetry::TelemetryGuard`). They are the same type
  — re-exports do not duplicate — but the rustdoc listing and IDE
  auto-import suggestions may point users at either. The recommended
  convention (documented in the new module's rustdoc) is to prefer
  the `cognee_lib::telemetry` path inside applications that already
  depend on `cognee-lib`, mirroring the existing pattern for
  `cognee_lib::storage`, `cognee_lib::graph`, etc.
- **Stale prelude expectations.** If a future task adds
  `init_telemetry` to the prelude, any embedder using
  `cognee_lib::prelude::*` plus a direct `use
  cognee_observability::init_telemetry;` will see a name collision.
  Mitigated by the recommendation in step 5 to *not* prelude these
  symbols.
- **`SettingsView` field drift.** If the
  [`Settings`](../../../crates/lib/src/config.rs) schema renames any
  of the eight `otel_*` / `cognee_tracing_enabled` fields without
  updating the impl in `telemetry.rs`, the `--features telemetry`
  build will fail. Mitigation: the verification matrix above
  exercises that build lane, and CI lane parity (see
  [task 01-12](12-ci-updates.md)) catches drift early. The trait
  itself lives in `cognee-observability` so the trait shape is
  stable across crates.
- **Surface drift between feature states.** This task does not
  introduce a noop `cognee_lib::telemetry` for the
  feature-off case (decision 4 explicitly chose visibility-tied
  gating over a duplicated noop surface). Embedders that need a
  uniform call site must wrap their telemetry bootstrap in their
  own `#[cfg]` shim or always enable the feature.

## Open / clarifying questions

- **Should the OTEL symbols also live at the crate root
  (`cognee_lib::TelemetryGuard`, `cognee_lib::init_telemetry`)?**
  Recommendation: **no**. Keep them namespaced under
  `cognee_lib::telemetry::*`. Rationale:
  - Crate-root re-exports in `lib.rs` are reserved for the most-used
    types (`AddPipeline`, `Settings`, `Data`, `Dataset`,
    `ComponentManager`); telemetry setup is a one-shot bootstrap
    concern.
  - A namespace gives the future
    `cognee_lib::telemetry::metrics`, `::logs`, `::probes`
    submodules a stable home.
  - Mirrors how `cognee_lib::cloud` and `cognee_lib::http` keep
    bootstrap-style APIs in their own modules instead of crate-root.
- **Should this task also cover the language bindings (`capi/`,
  `python/`, `js/`)?** No — those are scoped to
  [task 01-11 — bindings auto-init](11-bindings-auto-init.md) which
  builds on top of the surface this task exposes.
- **Should `cognee-cli` be made to import via `cognee_lib::telemetry`
  or directly from `cognee-observability`?** Per decision 6 in the
  design table, the CLI may take either dependency, but for
  consistency with the rest of `crates/cli/src/main.rs` (which uses
  `cognee_lib::*` throughout), [task 01-06](06-cli-subscriber-refactor.md)
  should route through `cognee_lib::telemetry`. Confirming this is
  a decision for that task, not this one.

## References

- [`01-otel-otlp-export.md` — design decisions table](../01-otel-otlp-export.md#design-decisions-locked)
  (decisions 6 and 10 lock the implementation crate name and the
  `TelemetryGuard` type name).
- [`01-otel-otlp-export.md` — Module placement](../01-otel-otlp-export.md#module-placement).
- [`01-otel-otlp-export.md` — Public API](../01-otel-otlp-export.md#public-api).
- [Task 01-02 — `cognee-observability` crate scaffold](02-observability-crate-scaffold.md).
- [Task 01-03 — `telemetry` cargo feature wiring](03-cognee-lib-feature-wiring.md).
- [Task 01-04 — `init_telemetry` implementation](04-init-telemetry-implementation.md).
- [Task 01-06 — CLI subscriber refactor](06-cli-subscriber-refactor.md).
- [Task 01-07 — HTTP server subscriber refactor](07-http-server-subscriber-refactor.md).
- [Task 01-08 — noop fallback](08-noop-fallback.md).
- [Task 01-12 — CI updates](12-ci-updates.md).
- [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs) — existing
  module structure that this task extends.
- [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs) —
  `Settings` struct whose fields back the `SettingsView` impl.
- [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml) — feature
  declarations referenced in pre-conditions.
- [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs)
  — canonical list of the eight public items being re-exported.
