# Task 02-02 — Scaffold the `cognee-telemetry` workspace crate

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**: [Task 02-01 — Workspace dependencies](01-workspace-deps.md)
**Blocks**:
- [Task 02-03 — Identity derivation](03-id-derivation.md)
- [Task 02-04 — Payload + sanitize](04-payload-and-sanitize.md)
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md)
- [Task 02-06 — Public API + noop fallback](06-public-api-and-noop.md)
- All later tasks transitively.

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Create a brand-new workspace crate, **`cognee-telemetry`**, which will
host the `send_telemetry` HTTP client, the three identity helpers
(`anonymous_id`, `persistent_id`, `api_key_tracking_id`), the
`TelemetryPayload` serde struct, the URL-sanitizer, and the env-driven
opt-out logic. This task is **scaffold only**:

- Crate manifest with the `telemetry` cargo feature wired to optional
  HTTP/crypto dependencies.
- `lib.rs` skeleton with module declarations and public-API stubs
  (`send_telemetry` function signature + a noop body) so dependent
  tasks can land their pieces independently.
- A clean `#[cfg(feature = "telemetry")] mod real;` /
  `#[cfg(not)] mod noop;` split — empty modules for now, real bodies
  arrive in tasks 03-05.
- Workspace registration so `cargo metadata` and `cargo check` see
  the crate.

Implementation of identity derivation, payload schema, and HTTP
dispatch is **out of scope** here — see tasks 02-03, 02-04, 02-05.
The noop fallback body is fleshed out alongside the public API in
[task 02-06](06-public-api-and-noop.md). This task lays the foundation
all of them depend on.

## 2. Rationale — why a new crate, not a module inside `cognee-utils`

The original draft of the parent doc suggested
`cognee_utils::telemetry`. **Decision 6** in the locked decisions
table overrode this in favour of a sibling crate. Reasoning, expanded:

1. **Dependency hygiene.** `cognee-telemetry` will pull `pbkdf2`,
   `hmac`, `hex`, `once_cell`, `reqwest`, `serde_json`, `chrono`,
   `dirs` — a meaningful slice of the workspace dep graph. Today
   `cognee-utils` is a tiny crate (`tokio`, `log`, `rand`, `uuid`)
   that *every* other crate depends on for `id_generation`,
   `retry_with_backoff`, and `tracing_keys`. Inflating its dep set
   would mass-recompile the workspace whenever a telemetry-related
   crate updates.
2. **Optional-feature scoping.** With the new crate, a downstream
   consumer can `cargo build --no-default-features` on `cognee-lib`
   and exclude the entire telemetry stack. If the code lived in
   `cognee-utils`, the `pbkdf2`/`hmac` deps would surface in every
   consumer's `Cargo.lock` even when unused (Cargo still resolves
   optional deps for feature unification across the graph).
3. **Convention.** Gap 01 created the sibling crate
   `cognee-observability` for the same reason
   (see [`docs/telemetry/01/02-observability-crate-scaffold.md`](../01/02-observability-crate-scaffold.md)).
   The workspace already ships per-concern crates (`cognee-session`,
   `cognee-ontology`, `cognee-delete`); a sibling crate matches the
   pattern in the [project guide](../../../.claude/CLAUDE.md#rust-workspace-structure).
4. **Reusability across binaries.** Both `cognee-cli` (via
   `cognee-lib`) and `cognee-http-server` (which links some leaf
   crates directly) need to fire telemetry. A sibling crate is
   depended on directly by either, without going through the
   umbrella.
5. **Test isolation.** Integration tests for the HTTP client
   (task 02-09) live inside this crate and don't need to drag the
   rest of `cognee-lib`'s features (sqlite/qdrant/onnx/etc.) into a
   simple HTTP-mock test.

## 3. Pre-conditions

- [Task 02-01](01-workspace-deps.md) is **merged**:
  `[workspace.dependencies]` in [`Cargo.toml`](../../../Cargo.toml)
  defines `pbkdf2`, `hmac`, `hex`, `once_cell` (in addition to the
  pre-existing `reqwest`, `sha2`, `serde_json`, `dirs`, `chrono`,
  `tracing`, `uuid`).
- A clean `cargo check --workspace` on `main`.

If the workspace deps are not yet present, the manifest below will
fail to resolve `pbkdf2 = { workspace = true, optional = true }`
lines. Land task 01 first.

## 4. Step-by-step

### 4.1 Create the directory

```bash
mkdir -p crates/telemetry/src
```

Naming: directory is `crates/telemetry/`, package name is
`cognee-telemetry`. Mirrors `crates/observability/` →
`cognee-observability`, `crates/utils/` → `cognee-utils`.

### 4.2 Create `crates/telemetry/Cargo.toml`

Full contents (final form for this task — real impl deps land in
tasks 03-05):

```toml
[package]
name = "cognee-telemetry"
version.workspace = true
edition.workspace = true

[features]
default = []

# Pulls in the HTTP client, PBKDF2 / HMAC / hex stack, and the home-dir
# resolver. When disabled, the public API still compiles but
# `send_telemetry` becomes a noop and the identity helpers return
# empty strings. See task 02-06 for the noop body.
telemetry = [
    "dep:reqwest",
    "dep:serde",
    "dep:serde_json",
    "dep:sha2",
    "dep:hmac",
    "dep:pbkdf2",
    "dep:hex",
    "dep:once_cell",
    "dep:dirs",
    "dep:chrono",
]

[dependencies]
# Always-on. The crate exposes a public function and an error type
# regardless of feature state, so these unconditional deps cover the
# noop path.
thiserror.workspace = true
tracing.workspace = true
tokio = { workspace = true }
uuid = { workspace = true }

# Feature-gated. All four feature-gated stacks are introduced here so
# downstream tasks (03-05) only need to write code, not edit Cargo.toml.
reqwest      = { workspace = true, optional = true }
serde        = { workspace = true, optional = true }
serde_json   = { workspace = true, optional = true }
sha2         = { workspace = true, optional = true }
hmac         = { workspace = true, optional = true }
pbkdf2       = { workspace = true, optional = true }
hex          = { workspace = true, optional = true }
once_cell    = { workspace = true, optional = true }
dirs         = { workspace = true, optional = true }
chrono       = { workspace = true, optional = true }

[dev-dependencies]
mockito = "1"
tempfile.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util"] }
```

Notes:

- `thiserror` and `tracing` are **always on** because the public
  surface returns a `TelemetryError` enum and emits debug/warn logs
  even when the HTTP path is compiled out.
- `uuid` is always on because the noop branch still constructs (or
  reads) a stable identity string in some paths (e.g. when only the
  `anonymous_id` is requested without an HTTP send).
- `tokio = { workspace = true }` is always on so that the public
  `send_telemetry()` function signature compiles in both branches
  (the noop branch returns immediately; the real branch dispatches
  on `tokio::spawn`).
- `mockito` is the only HTTP-mock dev-dep we use (per decision 10).
  It is not added to `[workspace.dependencies]` because only this
  crate (and `cognee-cli`, which already declares it locally) needs
  it.

### 4.3 Register the crate in the workspace

Edit [`Cargo.toml`](../../../Cargo.toml) `[workspace] members` block
(currently lines 7-36). Insert `crates/telemetry` alphabetically —
between `crates/storage` (line 10) and `crates/test-utils` (line 27).
The current `members` order is not strictly alphabetical (it groups
related crates), but inserting near the existing `crates/observability`
(line 31) is acceptable. Concretely, add the new entry on a new line
between lines 31 and 32:

```toml
    "crates/observability",
    "crates/telemetry",
```

### 4.4 Create `crates/telemetry/src/lib.rs` skeleton

```rust
//! Cognee product-analytics client (`send_telemetry`).
//!
//! This crate ports Python's `cognee.shared.utils.send_telemetry` to
//! Rust. It implements:
//!
//! - Three-layer identity (`anonymous_id`, `persistent_id`,
//!   `api_key_tracking_id`).
//! - Recursive URL-sanitization of caller-supplied properties.
//! - Fire-and-forget HTTP POST to the Cognee proxy
//!   (`https://test.prometh.ai`).
//! - Env-var opt-out (`TELEMETRY_DISABLED`, `ENV in {test,dev}`).
//!
//! The full public surface and noop fallback are wired up in
//! `docs/telemetry/02/06-public-api-and-noop.md`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "telemetry")]
mod real;

#[cfg(not(feature = "telemetry"))]
mod noop;

/// Modules that are always compiled (their bodies vary by feature
/// state). Each has a `#[cfg]` split internally — see the per-task
/// sub-docs for details.
pub mod ids {
    //! Identity-layer helpers. Implementations land in
    //! `docs/telemetry/02/03-id-derivation.md`.
}
pub mod sanitize {
    //! URL-sanitisation. Implementation lands in
    //! `docs/telemetry/02/04-payload-and-sanitize.md`.
}
pub mod payload {
    //! `TelemetryPayload` serde struct. Implementation lands in
    //! `docs/telemetry/02/04-payload-and-sanitize.md`.
}
pub mod env {
    //! Env-var parsing and opt-out checks. Implementation lands in
    //! `docs/telemetry/02/05-client-dispatch-and-optout.md`.
}

/// Errors returned by the telemetry surface.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// Returned when the dispatcher is called from a non-async
    /// context AND the runtime fallback fails to bootstrap.
    #[error("could not acquire a tokio runtime to dispatch event")]
    NoRuntime,
}

/// Reference type for the `user_id` field — accepts a `Uuid`, a
/// string slice (e.g. `"sdk"` for SDK-internal callers), or `None`
/// to skip the field entirely.
#[derive(Debug)]
pub enum UserIdRef<'a> {
    /// A real cognee `User.id`.
    Uuid(Uuid),
    /// A symbolic identifier (e.g. `"sdk"`, `"anonymous"`).
    Symbolic(&'a str),
    /// No user attached.
    None,
}

impl From<Uuid> for UserIdRef<'_> {
    fn from(u: Uuid) -> Self {
        UserIdRef::Uuid(u)
    }
}
impl<'a> From<&'a str> for UserIdRef<'a> {
    fn from(s: &'a str) -> Self {
        UserIdRef::Symbolic(s)
    }
}
impl From<Option<Uuid>> for UserIdRef<'_> {
    fn from(o: Option<Uuid>) -> Self {
        match o {
            Some(u) => UserIdRef::Uuid(u),
            None => UserIdRef::None,
        }
    }
}

/// Fire-and-forget product-analytics event.
///
/// Mirrors Python `cognee.shared.utils.send_telemetry`. Returns
/// immediately; the HTTP POST is dispatched on a detached tokio task
/// with a 5-second (configurable) total timeout. Errors are swallowed
/// at debug level. See task 02-05 for the dispatch semantics and
/// runtime-fallback behaviour.
pub fn send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<Value>,
) {
    let _ = (event_name, user_id.into(), additional_properties);
    #[cfg(feature = "telemetry")]
    real::send_telemetry_impl(event_name, _user_id_unused(), additional_properties);
    // Noop branch falls through — `noop::send_telemetry_impl` is a
    // free function call only when `feature = "telemetry"` is off.
    #[cfg(not(feature = "telemetry"))]
    noop::send_telemetry_impl(event_name);
}

// Placeholder: keep the compiler happy until task 02-05 fills the
// real dispatcher in. Task 02-06 deletes this and wires the real
// signature.
#[cfg(feature = "telemetry")]
fn _user_id_unused<'a>() -> UserIdRef<'a> {
    UserIdRef::None
}
```

The skeleton compiles in both feature states. Subsequent tasks
replace the placeholder bodies:

- [Task 02-03](03-id-derivation.md) fills `ids::*`.
- [Task 02-04](04-payload-and-sanitize.md) fills `sanitize::*` and
  `payload::*`.
- [Task 02-05](05-client-dispatch-and-optout.md) fills `env::*` and
  the `real::send_telemetry_impl` body.
- [Task 02-06](06-public-api-and-noop.md) finalises the public
  surface (replaces the placeholder `_user_id_unused()` and wires the
  real noop body).

### 4.5 Create `crates/telemetry/src/real.rs` and `noop.rs` stubs

`crates/telemetry/src/real.rs`:

```rust
//! Real (`feature = "telemetry"`) implementation of `send_telemetry`.
//! Body lands in `docs/telemetry/02/05-client-dispatch-and-optout.md`.

use crate::UserIdRef;
use serde_json::Value;

pub(crate) fn send_telemetry_impl(
    _event_name: &str,
    _user_id: UserIdRef<'_>,
    _additional_properties: Option<Value>,
) {
    // Stub — replaced in task 02-05.
}
```

`crates/telemetry/src/noop.rs`:

```rust
//! Noop (`feature = "telemetry"` off) implementation of
//! `send_telemetry`. Body lands in
//! `docs/telemetry/02/06-public-api-and-noop.md`.

pub(crate) fn send_telemetry_impl(_event_name: &str) {
    // No-op. Compiled when the `telemetry` feature is disabled.
}
```

### 4.6 Verify compilation

```bash
cargo check -p cognee-telemetry
cargo check -p cognee-telemetry --features telemetry
cargo check --workspace --all-targets
```

All three must pass.

## 5. Verification

```bash
# 1. The crate is recognised by the workspace.
cargo metadata --format-version 1 --no-deps \
  | jq '.packages[] | select(.name == "cognee-telemetry") | .name'
# Expected: "cognee-telemetry"

# 2. Both feature states compile.
cargo check -p cognee-telemetry
cargo check -p cognee-telemetry --features telemetry

# 3. No new clippy warnings.
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings
cargo clippy -p cognee-telemetry -- -D warnings

# 4. The full workspace still builds.
cargo check --workspace --all-targets

# 5. Doc generation works (catches missing_docs).
cargo doc -p cognee-telemetry --no-deps --features telemetry
```

## 6. Files modified

- [`Cargo.toml`](../../../Cargo.toml) — one line added under
  `[workspace] members`.
- `crates/telemetry/Cargo.toml` — new file.
- `crates/telemetry/src/lib.rs` — new file.
- `crates/telemetry/src/real.rs` — new file (stub).
- `crates/telemetry/src/noop.rs` — new file (stub).

No edits to `cognee-lib`, `cognee-cli`, or any other existing crate
in this task — those land in [task 02-06](06-public-api-and-noop.md)
when the public surface is final.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Workspace `members` ordering is non-alphabetical | N/A — current order is grouped, not strict | Insert near `crates/observability` to keep telemetry-related crates together. |
| `missing_docs` lint trips on stub modules | Possible | Each empty module has a doc comment in the skeleton above. |
| Cargo unification pulls `reqwest` into the dep graph even with feature off | The `optional = true` flag means cargo only resolves it when a member explicitly enables `telemetry` | Verify with `cargo tree -p cognee-telemetry --no-default-features` — `reqwest` should not appear. |
| Future renames / module reshuffling in tasks 03-05 invalidate this skeleton | Likely — that's the *point* | Each follow-up task rewrites its module from scratch. The skeleton exists only to make the crate compile so other tasks can land independently. |

## 8. Out of scope

- Real implementation of any helper (covered by tasks 02-03, 02-04,
  02-05).
- Public API freeze (covered by [task 02-06](06-public-api-and-noop.md)).
- Wiring into `cognee-lib` defaults (covered by [task 02-06](06-public-api-and-noop.md)).
- Tests beyond a `cargo check` smoke test (covered by [task 02-08](08-unit-tests.md)).
