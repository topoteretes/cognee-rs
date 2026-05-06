# Task 02 — Scaffold the `cognee-observability` workspace crate

**Status**: Implemented in commit c88df3d
**Owner:** _unassigned_
**Depends on:** [Task 01 — Add OTEL workspace dependencies](./01-workspace-otel-deps.md)
**Blocks:** [Task 03 — Wire `telemetry` feature on `cognee-lib`](./03-cognee-lib-feature-wiring.md), [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-init-telemetry-implementation.md), [Task 05 — Re-exports & subscriber composition helper](./05-cognee-lib-reexports.md), [Task 06 — Refactor CLI subscriber](./06-cli-subscriber-refactor.md), [Task 07 — Refactor HTTP server subscriber](./07-http-server-subscriber-refactor.md)
**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md)

---

## 1. Goal

Create a brand-new workspace crate, **`cognee-observability`**, which will host all OTEL bring-up code, the `TelemetryGuard` RAII handle, and the `init_telemetry(...)` entry point. This task is **scaffold only**:

- Crate manifest with the `telemetry` cargo feature wired to optional OTEL dependencies.
- `lib.rs` skeleton with public-API stubs (`TelemetryGuard`, `TelemetryInitError`, module declarations) so dependent tasks can land their pieces in parallel.
- A clean `#[cfg(feature = "telemetry")] mod real;` / `#[cfg(not)] mod noop;` split — empty modules for now, real bodies arrive in tasks 04 and 08.
- Workspace registration so `cargo metadata` and `cargo check` see the crate.

The actual exporter, processor, resource, and bridge layer construction is **out of scope here** — see [task 04](./04-init-telemetry-implementation.md). The noop fallback body is fleshed out in [task 08](./08-noop-fallback.md). This task lays the foundation both depend on.

## 2. Rationale — why a new crate, not a module inside `cognee-lib`

Decision 6 in the [parent doc's "Design decisions (locked)" table](../01-otel-otlp-export.md#design-decisions-locked) selected a new workspace crate over a module inside `cognee-lib`. The reasoning, expanded:

1. **Reusability across binaries.** `cognee-http-server` is a workspace member that does **not** depend on `cognee-lib` for its binary entry point — it links a few smaller cognee crates directly. Putting OTEL bring-up inside `cognee-lib` would either force `cognee-http-server` to take the whole umbrella as a dep (large), or force us to duplicate the subscriber composition. A sibling crate is depended on by both `cognee-cli` (via `cognee-lib`) and `cognee-http-server` directly.
2. **Dependency hygiene with feature off.** The OTEL crate set (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` with `tonic` + `reqwest`, `tracing-opentelemetry`) is heavy. With the `telemetry` feature gated at the leaf crate, a `cargo build -p cognee-lib --no-default-features` excludes them entirely. If the same gating lived inside `cognee-lib`, every `cognee-lib` consumer would see the optional deps in their `Cargo.lock` resolution graph (Cargo still resolves optional deps for feature unification).
3. **Per-concern crate split convention.** The workspace already has dedicated crates for narrow concerns (`cognee-utils`, `cognee-session`, `cognee-ontology`, `cognee-delete`, etc.). Observability is a comparable cross-cutting concern; a sibling crate matches the pattern in the [project guide](../../../.claude/CLAUDE.md#rust-workspace-structure).
4. **Testability.** Integration tests for OTEL exporters (task 10 of the parent doc) live inside this crate and don't need to drag in the rest of `cognee-lib`'s features (sqlite/qdrant/onnx/etc.) just to spin up an `init_telemetry` test.
5. **Future home for `SpanBufferLayer`.** If the in-memory ring buffer that powers `/api/v1/activity/spans` is later generalized out of `cognee-http-server`, this crate is its natural new home (see Open extensions, §10).

## 3. Pre-conditions

- [Task 01](./01-workspace-otel-deps.md) is **merged**: `[workspace.dependencies]` in the root [`Cargo.toml`](../../../Cargo.toml) defines `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, `tracing-opentelemetry` with the version pins from the parent doc.
- A clean `cargo check --workspace` on `main`.

If the workspace deps are not yet present, the manifest below will fail to resolve `opentelemetry = { workspace = true, optional = true }` lines. Land task 01 first.

## 4. Step-by-step

### 4.1 Create the directory

```bash
mkdir -p crates/observability/src
```

Naming: directory is `crates/observability/`, package name is `cognee-observability`. This mirrors `crates/core/` → `cognee-core`, `crates/utils/` → `cognee-utils`, `crates/http-server/` → `cognee-http-server`.

### 4.2 Create `crates/observability/Cargo.toml`

Full contents (final form for this task — no extra deps until task 04 needs them):

```toml
[package]
name = "cognee-observability"
version.workspace = true
edition.workspace = true

[features]
default = []

# Pulls in the OpenTelemetry SDK + OTLP exporter + tracing bridge.
# When disabled, the public API still compiles but `init_telemetry` returns a
# noop `TelemetryGuard` and an identity tracing layer. See task 08 for the
# noop body.
telemetry = [
    "dep:opentelemetry",
    "dep:opentelemetry_sdk",
    "dep:opentelemetry-otlp",
    "dep:opentelemetry-semantic-conventions",
    "dep:tracing-opentelemetry",
]

[dependencies]
# Always-on. The crate exposes a public error type and a `TelemetryGuard`
# regardless of feature state, so these are unconditional.
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

# Optional — pulled in only by the `telemetry` feature.
opentelemetry = { workspace = true, optional = true }
opentelemetry_sdk = { workspace = true, optional = true }
opentelemetry-otlp = { workspace = true, optional = true }
opentelemetry-semantic-conventions = { workspace = true, optional = true }
tracing-opentelemetry = { workspace = true, optional = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

Notes on this manifest:
- No `cognee-*` path dependencies. The `Settings` argument that `init_telemetry` will eventually accept is taken as a borrowed struct from `cognee-lib`. To avoid a circular `cognee-lib` ↔ `cognee-observability` dep, **task 04** will introduce a small input struct (`TelemetrySettings`) defined inside this crate and `cognee-lib` will convert its `Settings` into it. That keeps this crate at the bottom of the dep graph.
- `tracing-subscriber` is always-on because the public return type of `init_telemetry` is a boxed/identity `tracing_subscriber::Layer` regardless of feature state.
- `opentelemetry-otlp` features (`grpc-tonic`, `http-proto`, `reqwest-client`, `trace`) are configured at the workspace-deps layer in [task 01](./01-workspace-otel-deps.md), so we just write `{ workspace = true, optional = true }` here.
- Workspace lints inheritance (`lints.workspace = true`) is **not** used elsewhere in this repo (verified by grepping `lints.workspace` across `crates/*` and the root `Cargo.toml`); we omit it to match the existing convention.

### 4.3 Create `crates/observability/src/lib.rs`

Skeleton-only contents:

```rust
//! OpenTelemetry SDK bring-up and `tracing` bridge for cognee.
//!
//! This crate is the single home for OTEL configuration, OTLP exporter
//! construction, the `tracing-opentelemetry` bridge layer, and the RAII
//! [`TelemetryGuard`] that flushes pending spans on drop.
//!
//! ## Feature flags
//!
//! - `telemetry` (off by default) — pulls in `opentelemetry`,
//!   `opentelemetry_sdk`, `opentelemetry-otlp`,
//!   `opentelemetry-semantic-conventions`, and `tracing-opentelemetry`.
//!   When enabled, [`init_telemetry`] builds a real `SdkTracerProvider`,
//!   installs it globally, and returns a guard that flushes on drop.
//!   When disabled, [`init_telemetry`] still compiles but returns an identity
//!   tracing layer plus a noop guard, so embedders can call it
//!   unconditionally.
//!
//! See [`docs/telemetry/01-otel-otlp-export.md`](../../docs/telemetry/01-otel-otlp-export.md)
//! for the full design.

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
pub use settings::TelemetrySettings;

/// Initialize OpenTelemetry tracing for the current process.
///
/// This is a stub that will be implemented in
/// [task 04](../../docs/telemetry/01/04-init-telemetry-implementation.md)
/// (real path) and
/// [task 08](../../docs/telemetry/01/08-noop-fallback.md)
/// (noop path). Both arms return [`TelemetryGuard`] today.
pub fn init_telemetry(_settings: &TelemetrySettings) -> Result<TelemetryGuard, TelemetryInitError> {
    #[cfg(feature = "telemetry")]
    {
        real::init(_settings)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::init(_settings)
    }
}
```

And the small supporting files (also stubs):

`crates/observability/src/error.rs`:

```rust
//! Errors surfaced by [`crate::init_telemetry`].

use thiserror::Error;

/// Errors returned during OpenTelemetry SDK initialization.
///
/// Variants will be filled in by [task 04](../../docs/telemetry/01/04-init-telemetry-implementation.md).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TelemetryInitError {
    /// Placeholder so the enum is non-empty until task 04 lands real
    /// variants (exporter build failures, header parse errors, etc.).
    #[error("OTEL initialization not yet implemented")]
    NotImplemented,
}
```

`crates/observability/src/guard.rs`:

```rust
//! RAII handle returned by [`crate::init_telemetry`].

/// RAII handle that flushes and shuts down the global tracer provider on
/// drop.
///
/// Holding the guard for the lifetime of `main()` (CLI) or for as long as
/// `AppState` is alive (HTTP server) ensures the final batch of spans is
/// exported before the process exits.
///
/// The real `Drop` body lands in [task 04](../../docs/telemetry/01/04-init-telemetry-implementation.md);
/// today this is a noop placeholder that lets dependent crates compile.
#[must_use = "TelemetryGuard must be held for the lifetime of the process to flush spans on shutdown"]
pub struct TelemetryGuard {
    // Real fields land in task 04 — kept private so the public API is
    // stable.
    _private: (),
}

impl TelemetryGuard {
    /// Construct a noop guard. Used by the `not(feature = "telemetry")`
    /// branch and by tests.
    pub(crate) fn noop() -> Self {
        Self { _private: () }
    }
}
```

`crates/observability/src/settings.rs`:

```rust
//! Input struct for [`crate::init_telemetry`].
//!
//! Defined here (rather than re-using `cognee_lib::config::Settings`) so
//! that this crate sits at the bottom of the workspace dependency graph
//! and does not pull in `cognee-lib`. `cognee-lib` constructs an
//! `TelemetrySettings` from its own `Settings` in
//! [task 05](../../docs/telemetry/01/05-cognee-lib-reexports.md).

/// Subset of cognee settings required to initialize OpenTelemetry.
///
/// Field semantics are documented in the parent
/// [gap doc](../../docs/telemetry/01-otel-otlp-export.md#config-fields).
/// Keeping this struct minimal lets us evolve `cognee-lib::Settings`
/// without breaking the observability ABI.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TelemetrySettings {
    /// Mirrors `Settings.cognee_tracing_enabled`.
    pub tracing_enabled: bool,
    /// Mirrors `Settings.otel_service_name`.
    pub service_name: String,
    /// Mirrors `Settings.otel_exporter_otlp_endpoint`.
    pub exporter_otlp_endpoint: String,
    /// Mirrors `Settings.otel_exporter_otlp_headers`.
    pub exporter_otlp_headers: String,
}
```

`crates/observability/src/real.rs` (telemetry-on stub):

```rust
//! Real OTEL bring-up. Body lands in
//! [task 04](../../docs/telemetry/01/04-init-telemetry-implementation.md).

use crate::{TelemetryInitError, TelemetrySettings, TelemetryGuard};

pub(crate) fn init(_settings: &TelemetrySettings) -> Result<TelemetryGuard, TelemetryInitError> {
    // TODO(task 04): build SdkTracerProvider, install globally, return
    // guard that flushes on drop.
    Ok(TelemetryGuard::noop())
}
```

`crates/observability/src/noop.rs` (telemetry-off stub):

```rust
//! Noop fallback used when the `telemetry` feature is disabled. Final
//! body lands in
//! [task 08](../../docs/telemetry/01/08-noop-fallback.md).

use crate::{TelemetryInitError, TelemetrySettings, TelemetryGuard};

pub(crate) fn init(_settings: &TelemetrySettings) -> Result<TelemetryGuard, TelemetryInitError> {
    Ok(TelemetryGuard::noop())
}
```

### 4.4 Register the crate in the workspace

Edit the root [`Cargo.toml`](../../../Cargo.toml) `members = [...]` array. Insert `"crates/observability"` next to the other crate paths — alphabetical order is **not** consistently followed in the existing file, so place it after `"crates/http-server"` for readability:

```toml
[workspace]
members = [
    "examples",
    "crates/models",
    "crates/storage",
    # ... existing entries unchanged ...
    "crates/cloud",
    "crates/http-server",
    "crates/observability",   # <-- new

    # TODO: Move this out of the main workspace to avoid pulling in testing features and C FFI dependencies for all members
    "capi/cognee-capi",
    "python",
]
```

### 4.5 Verify both feature shapes compile

```bash
cargo check -p cognee-observability
cargo check -p cognee-observability --features telemetry
```

The first invocation must compile **without** any of the OTEL crates resolving — confirm with `cargo tree -p cognee-observability` showing only `thiserror`, `tracing`, `tracing-subscriber`. The second must pull in `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, `tracing-opentelemetry` (visible in `cargo tree`).

## 5. Resulting files (full contents)

### 5.1 `crates/observability/Cargo.toml`

```toml
[package]
name = "cognee-observability"
version.workspace = true
edition.workspace = true

[features]
default = []

telemetry = [
    "dep:opentelemetry",
    "dep:opentelemetry_sdk",
    "dep:opentelemetry-otlp",
    "dep:opentelemetry-semantic-conventions",
    "dep:tracing-opentelemetry",
]

[dependencies]
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

opentelemetry = { workspace = true, optional = true }
opentelemetry_sdk = { workspace = true, optional = true }
opentelemetry-otlp = { workspace = true, optional = true }
opentelemetry-semantic-conventions = { workspace = true, optional = true }
tracing-opentelemetry = { workspace = true, optional = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

### 5.2 `crates/observability/src/lib.rs`

```rust
//! OpenTelemetry SDK bring-up and `tracing` bridge for cognee.
//!
//! This crate is the single home for OTEL configuration, OTLP exporter
//! construction, the `tracing-opentelemetry` bridge layer, and the RAII
//! [`TelemetryGuard`] that flushes pending spans on drop.
//!
//! ## Feature flags
//!
//! - `telemetry` (off by default) — pulls in `opentelemetry`,
//!   `opentelemetry_sdk`, `opentelemetry-otlp`,
//!   `opentelemetry-semantic-conventions`, and `tracing-opentelemetry`.
//!   When enabled, [`init_telemetry`] builds a real `SdkTracerProvider`,
//!   installs it globally, and returns a guard that flushes on drop.
//!   When disabled, [`init_telemetry`] still compiles but returns an identity
//!   tracing layer plus a noop guard, so embedders can call it
//!   unconditionally.

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
pub use settings::TelemetrySettings;

/// Initialize OpenTelemetry tracing for the current process.
pub fn init_telemetry(_settings: &TelemetrySettings) -> Result<TelemetryGuard, TelemetryInitError> {
    #[cfg(feature = "telemetry")]
    {
        real::init(_settings)
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::init(_settings)
    }
}
```

### 5.3 Supporting source files

See §4.3 for the verbatim contents of `error.rs`, `guard.rs`, `settings.rs`, `real.rs`, and `noop.rs`. No additional source files are added in this task.

### 5.4 Root `Cargo.toml` diff

```diff
     "crates/cloud",
     "crates/http-server",
+    "crates/observability",

     # TODO: Move this out of the main workspace to avoid pulling in testing features and C FFI dependencies for all members
     "capi/cognee-capi",
```

## 6. Verification

Run from the repo root:

```bash
# Default features — no OTEL deps in the resolved graph.
cargo check -p cognee-observability

# Telemetry on — OTEL deps must resolve.
cargo check -p cognee-observability --features telemetry

# Whole workspace still healthy.
cargo check --all-targets

# Confirm package registration.
cargo metadata --format-version 1 \
  | jq '.packages[] | select(.name=="cognee-observability") | {name, version, manifest_path, features}'

# Confirm dep graph differences.
cargo tree -p cognee-observability                       | grep -E 'opentelemetry|tracing-opentelemetry' && echo "UNEXPECTED" || echo "OK: no OTEL deps without feature"
cargo tree -p cognee-observability --features telemetry  | grep -E 'opentelemetry|tracing-opentelemetry'

# Format & clippy clean.
cargo fmt --all -- --check
cargo clippy -p cognee-observability --all-targets -- -D warnings
cargo clippy -p cognee-observability --features telemetry --all-targets -- -D warnings
```

Expected:

- All `cargo check` invocations exit 0.
- `cargo metadata` prints a JSON object with `"name": "cognee-observability"`, `"version": "0.1.0"`, both feature lists (`default`, `telemetry`), and the manifest path under `crates/observability/`.
- The first `cargo tree` prints `OK: no OTEL deps without feature`.
- The second `cargo tree` lists at minimum `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, `tracing-opentelemetry`.
- `clippy` reports zero warnings under both feature shapes.

Finally, run the project-wide gate:

```bash
scripts/check_all.sh
```

This is required by the [project `CLAUDE.md`](../../../.claude/CLAUDE.md#build--development) and validates fmt + check + clippy + capi/python/js binding checks together.

## 7. Files modified

| File | Change |
|---|---|
| [`Cargo.toml`](../../../Cargo.toml) | Add `"crates/observability"` to `[workspace] members`. |
| `crates/observability/Cargo.toml` | **New.** Manifest with `telemetry` feature wiring optional OTEL deps. |
| `crates/observability/src/lib.rs` | **New.** Skeleton with `init_telemetry`, module declarations, public re-exports. |
| `crates/observability/src/error.rs` | **New.** `TelemetryInitError` enum stub. |
| `crates/observability/src/guard.rs` | **New.** `TelemetryGuard` struct with placeholder fields. |
| `crates/observability/src/settings.rs` | **New.** `TelemetrySettings` input struct. |
| `crates/observability/src/real.rs` | **New.** Telemetry-on stub (real body in task 04). |
| `crates/observability/src/noop.rs` | **New.** Telemetry-off stub (real body in task 08). |

No other crates change in this task. Cross-crate wiring lives in tasks 03 (cargo feature forwarding from `cognee-lib`) and 05 (re-exports + subscriber helper).

## 8. Risks

1. **Circular dependency with `cognee-lib`.** This crate must **not** depend on `cognee-lib` (which already depends transitively on most siblings). The mitigation is the dedicated `TelemetrySettings` struct in §4.3 — `cognee-lib` converts its bigger `Settings` into `TelemetrySettings` at the call site (task 05). Reviewers should fail the PR if any `cognee_lib::` path appears in `crates/observability/`.
2. **Feature unification across the workspace.** Cargo unifies features per package across the dep graph. If any crate later writes `cognee-observability = { ..., features = ["telemetry"] }` unconditionally, every workspace consumer ends up with OTEL deps. Tasks 03 and 07 are written to forward the flag through cargo features only; CI lane `cargo check -p cognee-lib --no-default-features` (task 12 of the parent doc) will catch regressions.
3. **Naming collision.** `cognee-observability` is a new name; double-check `crates.io` does not already publish it under our account before any future `cargo publish`. (Not relevant for the workspace path-dep build.)
4. **User enables `telemetry` but workspace dep missing.** If task 01 is not yet merged, `cargo check -p cognee-observability --features telemetry` fails with `error: failed to select a version for the requirement opentelemetry = ...`. Ensure the dependency PR (task 01) is merged before this one, or land them together.
5. **`#![deny(missing_docs)]`.** All public items must be documented. The skeleton in §4.3 satisfies this today; reviewers of tasks 04/08 must keep new `pub` items documented or they will break the build.

## 9. Open extensions (not in this task)

- **Hosting `SpanBufferLayer` here.** Today the in-memory ring buffer that backs `/api/v1/activity/spans` lives in [`crates/http-server/src/observability/`](../../../crates/http-server/src/observability/). If a future task generalizes it for the CLI / SDK use-case, this crate is the natural new home (it already owns the OTEL layer and would be at the same layer of the dep graph). Tracked under "Future Work" in the [root gap analysis](../gap-analysis.md#future-work--out-of-scope).
- **Metrics & logs.** Decision 12 puts metric and log exporters out of scope for this initiative. When they land, they slot into `cognee-observability/src/{metrics,logs}.rs` next to the existing `real`/`noop` modules and reuse the same `telemetry` feature flag.
- **Sampling configuration.** [Task 04](./04-init-telemetry-implementation.md) covers sampler wiring per decision 5; the public `TelemetrySettings` struct will gain `traces_sampler` / `traces_sampler_arg` fields then. This task intentionally keeps `TelemetrySettings` minimal so the surface to update later is small.

## Implementation notes

After the scaffold landed, the public identifiers were renamed to match the
broader telemetry naming used by sibling tasks 04–10:

- `init_otel` → `init_telemetry`
- `OtelInitError` → `TelemetryInitError`
- `OtelSettings` → `TelemetrySettings`

The renamed names are what shipped on `main`; this doc body has been
back-filled to reference them.

## 10. References

- Parent gap doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md), especially [Action items](../01-otel-otlp-export.md#action-items) #2 and the [Design decisions table](../01-otel-otlp-export.md#design-decisions-locked) (decisions 1, 6, 7, 10).
- Workspace layout reference: [`../../../.claude/CLAUDE.md`](../../../.claude/CLAUDE.md#rust-workspace-structure).
- Crate templates used as structural reference:
  - [`crates/core/Cargo.toml`](../../../crates/core/Cargo.toml) — `version.workspace = true` + leaf-feature pattern.
  - [`crates/utils/Cargo.toml`](../../../crates/utils/Cargo.toml) — minimal leaf crate manifest.
  - [`crates/utils/src/lib.rs`](../../../crates/utils/src/lib.rs) — minimal `lib.rs` shape.
  - [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml) — current `telemetry = []` placeholder feature, which task 03 will rewrite to forward into this new crate.
- Sibling tasks under [`./`](./):
  - [`01-workspace-otel-deps.md`](./01-workspace-otel-deps.md) — pre-condition.
  - [`03-cognee-lib-feature-wiring.md`](./03-cognee-lib-feature-wiring.md) — consumes this crate.
  - [`04-init-telemetry-implementation.md`](./04-init-telemetry-implementation.md) — fills the `real` body.
  - [`05-cognee-lib-reexports.md`](./05-cognee-lib-reexports.md) — re-exports through `cognee-lib`.
  - [`06-cli-subscriber-refactor.md`](./06-cli-subscriber-refactor.md), [`07-http-server-subscriber-refactor.md`](./07-http-server-subscriber-refactor.md) — call-site refactors.
  - [`08-noop-fallback.md`](./08-noop-fallback.md) — fills the `noop` body and unit tests.
