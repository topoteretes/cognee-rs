# Task 01-05: Re-export observability API from `cognee-lib`

## Status

Not started.

## Owner / dependencies

- **Depends on**:
  - [Task 01-02 — `cognee-observability` crate scaffold](02-cognee-observability-crate.md)
    creates the crate and its public surface (including the noop module
    skeleton).
  - [Task 01-03 — `telemetry` cargo feature wiring](03-telemetry-feature-wiring.md)
    adds `cognee-observability` as an optional dependency of
    `cognee-lib` behind `telemetry`.
  - [Task 01-04 — `init_telemetry` implementation](04-init-telemetry.md)
    finalises the names, signatures, and noop fallbacks of the public
    items being re-exported here (`init_telemetry`, `TelemetryGuard`,
    `TelemetryInitError`, `is_tracing_enabled`, `parse_otlp_headers`).
- **Blocks**:
  - [Task 01-06 — CLI subscriber refactor](06-cli-subscriber-refactor.md)
    will call `cognee_lib::observability::init_telemetry` rather than
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

A thin re-export module also gives us a single place to:

- gate the symbols on the `telemetry` feature (with a noop fallback so
  call sites compile unconditionally — see
  [task 01-08](08-noop-fallback.md));
- swap in additional helpers later (e.g. metrics, log export) without
  re-stamping the call sites.

This task introduces no new logic; it is purely a re-export shim.

## Pre-conditions

- [Task 01-02](02-cognee-observability-crate.md) has landed:
  the workspace member `crates/observability/` exists and exports
  `init_telemetry`, `TelemetryGuard`, `TelemetryInitError`,
  `is_tracing_enabled`, and `parse_otlp_headers` from its `lib.rs`.
- Per the contract noted in step 2 below and locked by
  [task 01-08](08-noop-fallback.md), the `cognee-observability` crate
  exposes those names *unconditionally* — when its own `telemetry`
  feature is off, the symbols still exist as noop shapes
  (`TelemetryGuard::noop()`, `init_telemetry` returning that noop,
  etc.). This is what allows the re-exports below to drop the
  `#[cfg(...)]` guards.
- [Task 01-03](03-telemetry-feature-wiring.md) has landed:
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

  Without the `optional = true` declaration, `pub use
  cognee_observability::...` from `cognee-lib` would not compile when
  the dependency is absent.

  > **Note on noop visibility under `optional = true`.** Even though
  > `cognee-observability` exposes noop types unconditionally inside
  > *its own* crate, those types are only visible to `cognee-lib`
  > when the `cognee-observability` crate is actually pulled in. With
  > `optional = true`, that happens **only when the `cognee-lib`
  > `telemetry` feature is on** — `dep:cognee-observability` brings
  > the dependency into the build graph. To make
  > `cognee_lib::observability::*` names exist regardless of the
  > `cognee-lib` feature state (per this task's stated goal), we have
  > two options:
  >
  > 1. **Make `cognee-observability` a non-optional dependency** of
  >    `cognee-lib`. The `telemetry` feature then only toggles the
  >    *child* crate's `telemetry` feature
  >    (`["cognee-observability/telemetry"]`), and the noop surface is
  >    always linked. This is the cleanest match for the requirement
  >    that `cognee_lib::observability::*` always exists.
  > 2. **Keep `cognee-observability` optional** and place
  >    `pub mod observability;` itself behind
  >    `#[cfg(feature = "telemetry")]`. Embedders who never enable
  >    `telemetry` then will not see the module at all.
  >
  > The recommended choice is **(1)** because it preserves the
  > stated invariant that the module path is stable. Task 01-03 should
  > be amended (or this task should ship the amendment) to drop
  > `optional = true` from the `cognee-observability` line and reduce
  > the `telemetry` feature to forwarding only. If task 01-03 has
  > already shipped with `optional = true`, see Risks below for the
  > follow-up.

## Step-by-step

1. **Add the module declaration to `crates/lib/src/lib.rs`.**
   Insert `pub mod observability;` next to the other top-level module
   declarations (around the block that contains `pub mod session;`,
   `pub mod api;`, `pub mod component_manager;` — see
   [`lib.rs:130`–`136`](../../../crates/lib/src/lib.rs#L130)). Order
   alphabetically with the other `pub mod`s for readability.

2. **Create `crates/lib/src/observability.rs`** as a single-file
   module (no submodules — this is purely a re-export shim). Its
   complete contents:

   ```rust
   //! Observability surface for embedders.
   //!
   //! Re-exports the public API of [`cognee_observability`] so that
   //! consumers reach OTEL setup through the same `cognee_lib::<topic>`
   //! pattern used for `storage`, `vector`, `graph`, etc.
   //!
   //! When the `telemetry` feature is enabled on `cognee-lib`, these
   //! re-exports point at the real OTEL-backed implementation
   //! (`SdkTracerProvider`, OTLP exporter, batch processor). When the
   //! feature is off, `cognee_observability` still exposes the same
   //! names with noop bodies (see
   //! `docs/telemetry/01/08-noop-fallback.md`), so call sites compile
   //! and run unchanged with or without the feature.

   pub use cognee_observability::{
       TelemetryGuard, TelemetryInitError, init_telemetry, is_tracing_enabled,
       parse_otlp_headers,
   };
   ```

   The re-exports are **unconditional** — no `#[cfg(feature =
   "telemetry")]` — because (a) `cognee-observability` always defines
   the names (noop bodies when its own `telemetry` feature is off),
   and (b) per the pre-conditions note above, `cognee-observability`
   is a non-optional path dependency of `cognee-lib`.

3. **Decide whether to extend `prelude`.** `cognee-lib` already has a
   `pub mod prelude` block at
   [`lib.rs:147`–`179`](../../../crates/lib/src/lib.rs#L147) listing
   the symbols most commonly imported via `use cognee_lib::prelude::*`.
   The recommendation is to **leave the OTEL symbols out of the
   prelude** for the same reason `serve` / `serve_url` / `CloudClient`
   are gated behind `feature = "cloud"` there: telemetry setup is a
   one-shot `main()` concern and does not belong in everyday
   re-exports. Embedders can `use cognee_lib::observability::{
   init_telemetry, TelemetryGuard };` explicitly. (Keeping the
   namespace also preserves room for future
   `cognee_lib::observability::metrics`, `::logs`, etc.)

   If the user later requests prelude inclusion, append:

   ```rust
   pub use crate::observability::{init_telemetry, TelemetryGuard};
   ```

   inside the existing `pub mod prelude { ... }` — but do not do so as
   part of this task.

4. **Confirm `cargo check -p cognee-lib` succeeds** without the
   `telemetry` feature. The non-optional `cognee-observability`
   dependency will be linked, and its noop surface satisfies the
   `pub use` statements.

5. **Confirm `cargo check -p cognee-lib --features telemetry`
   succeeds.** With the feature on, the same paths now resolve to the
   real OTEL-backed types in `cognee-observability`.

## Resulting code

### `crates/lib/src/observability.rs` (new)

```rust
//! Observability surface for embedders.
//!
//! Re-exports the public API of [`cognee_observability`] so that
//! consumers reach OTEL setup through the same `cognee_lib::<topic>`
//! pattern used for `storage`, `vector`, `graph`, etc.

pub use cognee_observability::{
    TelemetryGuard, TelemetryInitError, init_telemetry, is_tracing_enabled,
    parse_otlp_headers,
};
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
pub mod observability;   // ← added by this task
```

The `pub mod prelude { ... }` block remains unchanged (see step 3).

## Verification

- [ ] `cargo check -p cognee-lib` (no features beyond defaults).
- [ ] `cargo check -p cognee-lib --no-default-features` — confirms
      the noop path compiles when no other feature is on.
- [ ] `cargo check -p cognee-lib --features telemetry`.
- [ ] `cargo check -p cognee-lib --all-features`.
- [ ] `cargo doc -p cognee-lib --features telemetry --no-deps` —
      open the generated rustdoc and confirm
      `cognee_lib::observability` is listed as a module on the crate
      landing page, with `init_telemetry`, `TelemetryGuard`,
      `TelemetryInitError`, `is_tracing_enabled`, and
      `parse_otlp_headers` visible inside it.
- [ ] `cargo doc -p cognee-lib --no-deps` (without `telemetry`) —
      same module, same names, but the docs reflect the noop bodies.
- [ ] `scripts/check_all.sh` — fmt + check + clippy + binding checks.

## Files modified

- [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs) — add
  `pub mod observability;` next to the other module declarations.
- **New**: `crates/lib/src/observability.rs` — single-file re-export
  module (full contents above).

If the pre-condition follow-up is required (see Risks below):

- [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml) — drop
  `optional = true` from the `cognee-observability` dependency line
  and update the `telemetry` feature definition accordingly.

## Risks

- **`pub use` of an `optional = true` dependency.** If task 01-03
  shipped `cognee-observability = { path = "../observability",
  optional = true }`, then the unconditional `pub use
  cognee_observability::...` here will fail to compile when
  `--features telemetry` is off. Mitigations, in order of preference:
  1. Drop `optional = true` and let the `telemetry` feature only
     forward to `cognee-observability/telemetry` (see pre-conditions
     note). Costs one extra small crate compile in default builds;
     the noop surface is `#![no_implementation]`-tier code with
     negligible build cost.
  2. Wrap the re-exports in `#[cfg(feature = "telemetry")]` and
     accept that `cognee_lib::observability::*` does not exist
     without the feature. Embedders that want a single call site
     across feature states would then need their own `cfg` shim.
  3. Inline a noop module directly in `crates/lib/src/observability.rs`
     under `#[cfg(not(feature = "telemetry"))]` mirroring the
     `cognee-observability` noop bodies. Duplicates the noop surface
     across two crates and risks drift; not recommended.
- **Double-imports for downstream embedders.** Anyone who today takes
  a direct dependency on `cognee-observability` *and* on
  `cognee-lib` will see two paths to the same types
  (`cognee_observability::TelemetryGuard` vs
  `cognee_lib::observability::TelemetryGuard`). They are the same
  type — re-exports do not duplicate — but the rustdoc listing and
  IDE auto-import suggestions may point users at either. The
  recommended convention (documented in the new module's rustdoc) is
  to prefer the `cognee_lib::observability` path inside applications
  that already depend on `cognee-lib`, mirroring the existing
  pattern for `cognee_lib::storage`, `cognee_lib::graph`, etc.
- **Stale prelude expectations.** If a future task adds
  `init_telemetry` to the prelude, any embedder using
  `cognee_lib::prelude::*` plus a direct `use
  cognee_observability::init_telemetry;` will see a name collision.
  Mitigated by the recommendation in step 3 to *not* prelude these
  symbols.
- **Surface drift between feature states.** If the noop bodies in
  `cognee-observability` ever lose a symbol that the real
  implementation has (or vice versa), `cognee-lib` will fail to
  compile in one feature lane. Mitigation: the verification matrix
  above runs both `--no-default-features` and `--features
  telemetry`, and CI lane parity (see
  [task 01-12](12-ci-and-docs.md)) catches drift early.

## Open / clarifying questions

- **Should the OTEL symbols also live at the crate root
  (`cognee_lib::TelemetryGuard`, `cognee_lib::init_telemetry`)?**
  Recommendation: **no**. Keep them namespaced under
  `cognee_lib::observability::*`. Rationale:
  - Crate-root re-exports in `lib.rs` are reserved for the most-used
    types (`AddPipeline`, `Settings`, `Data`, `Dataset`,
    `ComponentManager`); telemetry setup is a one-shot bootstrap
    concern.
  - A namespace gives the future
    `cognee_lib::observability::metrics`, `::logs`, `::probes`
    submodules a stable home.
  - Mirrors how `cognee_lib::cloud` and `cognee_lib::http` keep
    bootstrap-style APIs in their own modules instead of crate-root.
- **Should this task also cover the language bindings (`capi/`,
  `python/`, `js/`)?** No — those are scoped to
  [task 01-11 — bindings auto-init](11-bindings-auto-init.md) which
  builds on top of the surface this task exposes.
- **Should `cognee-cli` be made to import via `cognee_lib::observability`
  or directly from `cognee-observability`?** Per decision 6 in the
  design table, the CLI may take either dependency, but for
  consistency with the rest of `crates/cli/src/main.rs` (which uses
  `cognee_lib::*` throughout), [task 01-06](06-cli-subscriber-refactor.md)
  should route through `cognee_lib::observability`. Confirming this is
  a decision for that task, not this one.

## References

- [`01-otel-otlp-export.md` — design decisions table](../01-otel-otlp-export.md#design-decisions-locked)
  (decisions 6 and 10 lock the implementation crate name and the
  `TelemetryGuard` type name).
- [`01-otel-otlp-export.md` — Module placement](../01-otel-otlp-export.md#module-placement).
- [`01-otel-otlp-export.md` — Public API](../01-otel-otlp-export.md#public-api).
- [Task 01-02 — `cognee-observability` crate scaffold](02-cognee-observability-crate.md).
- [Task 01-03 — `telemetry` cargo feature wiring](03-telemetry-feature-wiring.md).
- [Task 01-04 — `init_telemetry` implementation](04-init-telemetry.md).
- [Task 01-06 — CLI subscriber refactor](06-cli-subscriber-refactor.md).
- [Task 01-07 — HTTP server subscriber refactor](07-http-server-subscriber-refactor.md).
- [Task 01-08 — noop fallback](08-noop-fallback.md).
- [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs) — existing
  module structure that this task extends.
- [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml) — feature
  declarations referenced in pre-conditions.
