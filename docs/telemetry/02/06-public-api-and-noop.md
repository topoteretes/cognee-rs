# Task 02-06 — Public API freeze, noop fallback, feature wiring

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 02-02 — Crate scaffold](02-telemetry-crate-scaffold.md)
- [Task 02-03 — Identity derivation](03-id-derivation.md)
- [Task 02-04 — Payload + sanitize](04-payload-and-sanitize.md)
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md)

**Blocks**:
- [Task 02-07 — Callsite migration](07-callsite-migration.md) (callers depend on the public surface frozen here).
- [Task 02-08 — Unit tests](08-unit-tests.md) (tests assert the noop fallback contract).
- [Task 02-09 — Integration tests](09-integration-tests.md), [Task 02-10 — Cross-SDK parity](10-cross-sdk-parity.md), [Task 02-11 — User docs](11-user-docs.md), [Task 02-12 — CI updates](12-ci-updates.md).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

This task is the **API-freeze gate**. Three deliverables:

1. **Final public surface** in `cognee_telemetry::` — replace the
   placeholder signature from [task 02-02](02-telemetry-crate-scaffold.md)
   with the production form, including a `try_send_telemetry` variant
   that returns `Result<(), TelemetryError>` for callers who want
   error visibility.
2. **Noop fallback body** in `crates/telemetry/src/noop.rs` — when
   the `telemetry` feature is off, every entry point compiles to a
   single `tracing::debug!` line and returns. The signatures match
   the real path so callers compile in both feature states.
3. **Feature wiring** through `cognee-lib` (decision 1: ON by
   default), `cognee-cli` (decision 1: ON by default), and
   `android-default` (decision 1: OFF). Add a re-export at
   `cognee_lib::telemetry::send_telemetry` so callers don't need to
   know about the `cognee-telemetry` crate by name.

Once this task lands, [task 02-07](07-callsite-migration.md) can
replace the placeholder in `forget.rs` and add the rest of the
catalogued call sites.

## 2. Rationale

### Why a fallible variant alongside the fire-and-forget one

Python only has the fire-and-forget form. Rust callers occasionally
want to surface errors (e.g. for tests that assert "this event
fired"). A `try_send_telemetry` returning `Result<JoinHandle<()>,
TelemetryError>` lets:

- Tests `await` the returned handle and assert dispatch happened.
- Library code that doesn't care continues to call the void-returning
  `send_telemetry`.

The noop fallback returns `Ok(())` for `try_send_telemetry` because
the contract is "we don't promise the request fired" — same as the
real path swallowing transport errors at debug level.

### Why the re-export from `cognee-lib`

Two reasons:

1. **Discoverability.** Most callers already depend on
   `cognee-lib`. Routing through `cognee_lib::telemetry` keeps the
   import path uniform with the rest of the API surface
   (`cognee_lib::api::*`, `cognee_lib::observability::*` from gap
   01).
2. **Feature gating.** `cognee-lib` is the right place to compose
   the `telemetry` cargo feature with the rest of cognee's feature
   matrix. Downstream crates that want telemetry off can do so via
   `cognee-lib --no-default-features` without knowing about the
   leaf crate.

### Default-on at the library level (decision 1)

Decision 1 in the locked decisions table:

> `telemetry` is ON by default in `cognee-lib` and `cognee-cli`,
> OFF in `android-default`.

This **inverts** the gap-01 stance (where `telemetry` was off by
default). The reasoning is:

- Python ships `send_telemetry` enabled-by-default with
  `TELEMETRY_DISABLED` as the kill switch. Cross-SDK parity demands
  the same behaviour.
- Operators who care about privacy still have a runtime toggle
  (`TELEMETRY_DISABLED=1`) AND a compile-time toggle
  (`--no-default-features`).
- Android binaries via `android-default` opt out at compile time
  because they ship to end-users who didn't choose to install the
  CLI.

The OTLP feature (gap 01) remains compile-time opt-in because OTLP
needs an explicit collector endpoint — there is no useful default.
`send_telemetry` has a hard-coded proxy URL, so opt-in is a higher
bar than opt-out.

## 3. Pre-conditions

- Tasks 02-02, 02-03, 02-04, 02-05 merged.
- `cargo check --workspace --all-targets --features telemetry` and
  `cargo check --workspace --all-targets --no-default-features` both
  pass.

## 4. Step-by-step

### 4.1 Finalise `crates/telemetry/src/lib.rs`

Replace the contents written in [task 02-02](02-telemetry-crate-scaffold.md)
with the production form:

```rust
//! Cognee product-analytics client (`send_telemetry`).
//!
//! Mirrors Python's `cognee.shared.utils.send_telemetry`.
//!
//! # Quick start
//!
//! ```no_run
//! use cognee_telemetry::send_telemetry;
//! use serde_json::json;
//!
//! send_telemetry(
//!     "cognee.forget",
//!     "user-id-string",
//!     Some(json!({ "endpoint": "POST /api/v1/forget" })),
//! );
//! ```
//!
//! # Opt-out
//!
//! At runtime: `TELEMETRY_DISABLED=1` (any non-empty value) or
//! `ENV=test` / `ENV=dev`.
//!
//! At compile time: build `cognee-lib` (or any consumer) with
//! `--no-default-features`. The function still exists but becomes a
//! noop.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "telemetry")]
mod client;
#[cfg(feature = "telemetry")]
mod real;
#[cfg(not(feature = "telemetry"))]
mod noop;

pub mod env;
pub mod ids;
pub mod payload;
pub mod sanitize;

/// Errors returned by [`try_send_telemetry`].
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The dispatcher could not acquire a tokio runtime and the
    /// fallback runtime build failed. Practically unreachable.
    #[error("could not bootstrap a tokio runtime to dispatch event")]
    NoRuntime,
}

/// Reference type for the `user_id` field. Accepts a `Uuid`, a
/// `&str`, or `Option<Uuid>`.
#[derive(Debug, Clone)]
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
impl<'a> From<&'a String> for UserIdRef<'a> {
    fn from(s: &'a String) -> Self {
        UserIdRef::Symbolic(s.as_str())
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
/// Returns immediately; the HTTP POST is dispatched on a detached
/// tokio task with a 5-second (configurable) total timeout. Errors
/// are swallowed at debug level. When called outside a tokio runtime,
/// falls back to a one-shot single-thread runtime (decision 5 — see
/// [`docs/telemetry/02-send-telemetry-analytics.md`]).
///
/// No-op when:
/// - the `telemetry` cargo feature is disabled at compile time,
/// - `TELEMETRY_DISABLED` is set to a non-empty value at runtime,
/// - `ENV` is `"test"` or `"dev"`.
pub fn send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<Value>,
) {
    let _ = try_send_telemetry(event_name, user_id, additional_properties);
}

/// Same as [`send_telemetry`] but returns `Result<(), TelemetryError>`
/// for callers that want to know whether dispatch was attempted.
///
/// The `Ok(())` return does **not** mean the proxy received the
/// payload — it means the dispatch was scheduled. Transport failures
/// are still swallowed at debug level (mirrors Python's
/// fire-and-forget semantics).
pub fn try_send_telemetry<'a>(
    event_name: &str,
    user_id: impl Into<UserIdRef<'a>>,
    additional_properties: Option<Value>,
) -> Result<(), TelemetryError> {
    let user_id = user_id.into();
    #[cfg(feature = "telemetry")]
    {
        real::send_telemetry_impl(event_name, user_id, additional_properties);
    }
    #[cfg(not(feature = "telemetry"))]
    {
        noop::send_telemetry_impl(event_name, user_id, additional_properties);
    }
    Ok(())
}
```

Key changes vs the [task 02-02](02-telemetry-crate-scaffold.md) skeleton:

- Public functions are `send_telemetry` and `try_send_telemetry`.
- The placeholder `_user_id_unused()` is gone.
- All five sub-modules are `pub mod ...;` (no inline placeholders).
- The `client` module is private (`mod client;` without `pub`).

### 4.2 Replace `crates/telemetry/src/noop.rs` body

```rust
//! Noop (`feature = "telemetry"` off) implementation of
//! `send_telemetry`. Compiled when the `telemetry` cargo feature is
//! disabled.
//!
//! Every entry point becomes a no-op that emits a single
//! `tracing::debug!` line. Identity helpers in [`crate::ids`] return
//! empty strings.

use serde_json::Value;

use crate::UserIdRef;

pub(crate) fn send_telemetry_impl(
    event_name: &str,
    _user_id: UserIdRef<'_>,
    _additional_properties: Option<Value>,
) {
    tracing::debug!(
        target: "cognee.telemetry",
        event = event_name,
        "send_telemetry called but telemetry feature disabled at compile time"
    );
}
```

### 4.3 Wire `cognee-lib` to expose the public surface

#### 4.3.1 Add the dep

In [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml)
`[dependencies]` (currently lines 87-116):

```toml
cognee-telemetry = { path = "../telemetry", optional = true }
```

#### 4.3.2 Update the `telemetry` feature

The existing `telemetry` feature (lines 45-49 per gap 01) currently
gates `cognee-observability`. Extend it to **also** gate the new
`cognee-telemetry` crate:

```toml
telemetry = [
    "dep:cognee-observability",
    "cognee-observability/telemetry",
    "cognee-core/telemetry",
    "dep:cognee-telemetry",
    "cognee-telemetry/telemetry",
]
```

#### 4.3.3 Add `telemetry` to `default`

Per decision 1: `telemetry` is ON by default for `cognee-lib`. Edit
the `default = [...]` list (currently lines 7-22):

```toml
default = [
    "onnx",
    "ladybug",
    # ... existing entries ...
    "cloud",
    "telemetry",      # decision 1: ON by default. Disable with
                      # --no-default-features for compile-time opt-out.
]
```

#### 4.3.4 Confirm `android-default` does NOT include telemetry

Edit the `android-default = [...]` list (currently lines 65-78). The
list must **not** include `"telemetry"`. If it currently does (it
shouldn't from gap 01 since gap 01 left it off), remove it.

#### 4.3.5 Add the re-export

In [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs), the
existing `#[cfg(feature = "telemetry")] pub mod telemetry;` line
(160-161 per the explore report) currently re-exports the gap-01
observability surface. Replace with a more targeted module:

```rust
/// Product-analytics client. Re-exports of
/// [`cognee_telemetry`](../cognee_telemetry/) for embedders.
#[cfg(feature = "telemetry")]
pub mod telemetry {
    pub use cognee_telemetry::{
        env, ids, payload, sanitize, send_telemetry, try_send_telemetry, TelemetryError,
        UserIdRef,
    };
}

/// Noop stub when the feature is off — the type signatures still
/// resolve so callers compile in both states.
#[cfg(not(feature = "telemetry"))]
pub mod telemetry {
    use serde_json::Value;

    pub fn send_telemetry(_event_name: &str, _user_id: &str, _additional: Option<Value>) {}
    pub fn try_send_telemetry(
        _event_name: &str,
        _user_id: &str,
        _additional: Option<Value>,
    ) -> Result<(), std::convert::Infallible> {
        Ok(())
    }
}
```

The noop sibling exposes a narrower signature on purpose: when the
real `cognee-telemetry` crate isn't compiled, `UserIdRef` doesn't
exist either, so callers must pass a `&str`. This is documented in
the rustdoc.

If sub-tasks rely on the full noop surface (e.g. `ids::*` returning
empty strings), enrich the noop module accordingly. The minimal
form above is sufficient for the [task 02-07](07-callsite-migration.md)
call sites.

#### 4.3.6 Add to the prelude (optional)

If [`crates/lib/src/prelude.rs`](../../../crates/lib/src/prelude.rs)
or whatever serves as the prelude exists (per the explore report,
the prelude is in `lib.rs` lines 172-204), add:

```rust
#[cfg(feature = "telemetry")]
pub use crate::telemetry::send_telemetry;
```

Decide based on whether other top-level helpers are exposed via the
prelude. Document the choice in the task review (sub-agent A).

### 4.4 Wire `cognee-cli`

In [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml):

#### 4.4.1 Update the `default` feature list (decision 1)

Add `"telemetry"` to the `default` list (currently lines 12-27):

```toml
default = [
    # ... existing entries ...
    "telemetry",
]
```

#### 4.4.2 Update the `telemetry` feature delegation

The existing `telemetry = ["cognee-lib/telemetry"]` (line 38 per the
explore report) is correct. Verify it pulls the new dep transitively:

```bash
cargo tree -p cognee-cli --features telemetry --depth 3 \
  | grep cognee-telemetry
# Expected: cognee-telemetry v0.1.0 (path)
```

### 4.5 Wire `cognee-http-server`

If [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)
has a `telemetry` feature (gap 01 added it), extend the same way as
`cognee-cli`:

```toml
default = [
    # ... existing entries ...
    "telemetry",
]
telemetry = ["cognee-lib/telemetry"]
```

`cognee-http-server`'s default-on inclusion is consistent with
decision 1 — the server is a long-running process; opt-out is via
runtime env or `--no-default-features`.

### 4.6 Verify

```bash
# 1. All feature combinations build.
cargo check --workspace --all-targets
cargo check --workspace --all-targets --features telemetry
cargo check --workspace --all-targets --no-default-features
cargo check -p cognee-lib --no-default-features --features sqlite

# 2. The new public surface is reachable through cognee-lib.
cargo doc -p cognee-lib --no-deps --features telemetry --open
# (Visually confirm `cognee_lib::telemetry::send_telemetry` is in the
#  generated docs.)

# 3. Clippy clean.
cargo clippy --workspace --all-targets --features telemetry -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings

# 4. Existing tests pass.
cargo test -p cognee-lib --features telemetry
cargo test -p cognee-cli --features telemetry
```

## 5. Verification

The `scripts/check_all.sh` run inside sub-agent C is the gate. It
runs `cargo fmt --check`, `cargo check --all-targets`,
`cargo clippy -- -D warnings`, and the binding checks.

Additionally, eyeball:

- `cargo tree -p cognee-cli --features telemetry | grep cognee-telemetry`
  — must show one resolved version.
- `cargo tree -p cognee-cli --no-default-features | grep cognee-telemetry`
  — must show **nothing** (the dep should be excluded).

## 6. Files modified

- `crates/telemetry/src/lib.rs` — final public-surface form.
- `crates/telemetry/src/noop.rs` — full body (replaces stub from
  [task 02-02](02-telemetry-crate-scaffold.md)).
- `crates/lib/Cargo.toml` — extend `telemetry` feature, add
  `cognee-telemetry` dep, add `telemetry` to `default`, confirm
  `android-default` excludes it.
- `crates/lib/src/lib.rs` — replace existing `telemetry` module with
  the re-exports above.
- `crates/cli/Cargo.toml` — add `telemetry` to `default`.
- `crates/http-server/Cargo.toml` — add `telemetry` to `default`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Adding `telemetry` to `cognee-lib/default` breaks downstream consumers that built with default features but didn't have `cognee-telemetry`'s deps cached | One-time CI cache miss; mitigated by [task 02-12](12-ci-updates.md) | Document the change in the changelog. CI lanes already exercise both feature states (gap 01 added the off-default lane). |
| The two `telemetry` modules in `cognee-lib` (gap-01 observability re-exports vs gap-02 send_telemetry re-exports) collide on the same name | Real risk — both gap-01 and this task want `cognee_lib::telemetry::*` | Resolve by consolidating into one module: `pub use cognee_observability::*; pub use cognee_telemetry::*;` Document any name collisions discovered at implementation time and rename on the gap-02 side if needed. |
| Noop fallback signature drift from real signature breaks call sites under `--no-default-features` | Possible — the noop module exposes a narrower API | The noop signature accepts `&str` for `user_id`, which is sufficient for every cataloged call site in [task 02-07](07-callsite-migration.md). Document the constraint in the rustdoc. |
| `cognee-http-server` already has `telemetry` in `default` from gap 01 — adding it again is a no-op edit | Low risk; explore report confirms gap-01 left it off-default | Confirm with `git blame` before editing. |
| `try_send_telemetry` returning `Ok(())` without checking dispatch lies to callers | Documented contract; tests use `JoinHandle` indirectly via mockito assertions | The `Result` return is for future use (e.g. when the runtime fallback fails). Keeping it now means we don't break callers later when we tighten the contract. |

## 8. Out of scope

- Replacing `forget.rs` placeholder + porting other call sites (covered by [task 02-07](07-callsite-migration.md)).
- Unit tests on the public surface (covered by [task 02-08](08-unit-tests.md)).
- Mockito integration tests (covered by [task 02-09](09-integration-tests.md)).
- Cross-SDK parity (covered by [task 02-10](10-cross-sdk-parity.md)).
- User docs for the public API (covered by [task 02-11](11-user-docs.md)).
- CI lanes (covered by [task 02-12](12-ci-updates.md)).
