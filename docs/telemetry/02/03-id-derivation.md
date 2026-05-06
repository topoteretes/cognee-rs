# Task 02-03 — Identity derivation (`anonymous_id`, `persistent_id`, `api_key_tracking_id`)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 02-01 — Workspace deps](01-workspace-deps.md) — `pbkdf2`, `hmac`, `hex`, `once_cell`.
- [Task 02-02 — Crate scaffold](02-telemetry-crate-scaffold.md) — `crates/telemetry/src/ids.rs` placeholder module.

**Blocks**:
- [Task 02-04 — Payload + sanitize](04-payload-and-sanitize.md) (the
  payload struct embeds the three identity strings).
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md)
  (the dispatcher reads identity at emission time).
- [Task 02-08 — Unit tests](08-unit-tests.md) (the byte-parity test
  against Python's PBKDF2 fixture lives here).
- [Task 02-10 — Cross-SDK parity](10-cross-sdk-parity.md) (asserts
  Python and Rust derive identical IDs from the same `LLM_API_KEY`).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Implement the **three identity layers** in
`crates/telemetry/src/ids.rs` with byte-for-byte parity to Python's
[`cognee/shared/utils.py`](file:///tmp/cognee-python/cognee/shared/utils.py)
helpers:

| Helper | Source | Stability | Python ref |
|---|---|---|---|
| `get_anonymous_id() -> String` | `<project_root>/.anon_id` (uuid4 created on first call) **or** `TRACKING_ID` env override | Resets on git re-clone / pip reinstall | utils.py:49-73 |
| `get_persistent_id() -> String` | `~/.cognee/.persistent_id` (uuid4, seeded from anon if present) | Survives `forget(everything=True)`, virtualenv recreation | utils.py:76-104 |
| `get_api_key_tracking_id() -> String` | PBKDF2-HMAC-SHA256(`LLM_API_KEY`, salt, 100 000 iter, 16 bytes) → `"ak_" + hex` | Stable across machines for the same API key | utils.py:139-168 |

Decision 11 in the locked decisions table: `LLM_API_KEY` is read at
**event-emission time**, not startup. This applies to
`api_key_tracking_id` only — `anonymous_id` and `persistent_id` are
cached in `Lazy<...>` and only re-read after a process restart.

This task does **not** touch the payload struct, the HTTP client, or
the dispatcher — those are tasks 02-04 and 02-05.

## 2. Rationale — what is load-bearing and why

### PBKDF2 byte-parity

If the Python and Rust SDKs ever produce different
`api_key_tracking_id` values for the same key, **a single user
double-counts on the proxy**. That is the failure mode the entire
`send_telemetry` initiative exists to avoid. The five constants below
are therefore frozen and a fixture-based test
([task 02-08](08-unit-tests.md)) asserts every change.

| Constant | Value | Source |
|---|---|---|
| Algorithm | PBKDF2-HMAC-SHA256 | Python `hashlib.pbkdf2_hmac("sha256", ...)` |
| Iterations | `100_000` | utils.py:24 |
| Output length (`dklen`) | `16` bytes | utils.py:24 |
| Default salt | `b"cognee.telemetry.api-key-tracking.v1"` (38 bytes UTF-8) | utils.py:25-27 |
| Output prefix | literal `"ak_"` | utils.py:163 |
| Hex case | lowercase | Python `derived.hex()` default |

### File location parity

| File | Python | Rust |
|---|---|---|
| `.anon_id` | `<project_root>/.anon_id` resolved as `pathlib.Path(__file__).parent.parent.parent.resolve()` (= cognee package root) | `<project_root>/.anon_id` resolved by walking up from `std::env::current_dir()` until a `Cargo.toml` is found, falling back to `current_dir()` |
| `.persistent_id` | `pathlib.Path.home() / ".cognee" / ".persistent_id"` | `dirs::home_dir() / ".cognee" / ".persistent_id"` |

The `.anon_id` location semantics differ slightly because Rust has no
`__file__`. The Python anchor walks up three levels from
`utils.py` (which lives at `cognee/shared/utils.py`) — i.e. the
cognee package root, which on a `pip install` lands inside
`site-packages/cognee/`. This is irrelevant for cross-SDK parity:
**`anonymous_id` is intentionally project-local** and not expected
to match between SDKs (only `persistent_id` and `api_key_tracking_id`
are). See decision 11 + the parity table in
[`02-send-telemetry-analytics.md` § "Cross-SDK identity parity"](../02-send-telemetry-analytics.md#cross-sdk-identity-parity).

### TRACKING_ID env override

Python only honours `TRACKING_ID` for `anonymous_id`, not for
`persistent_id` or `api_key_tracking_id`. We mirror that exactly.

## 3. Pre-conditions

- [Task 02-01](01-workspace-deps.md) merged — `pbkdf2`, `hmac`, `hex`,
  `once_cell` in `[workspace.dependencies]`.
- [Task 02-02](02-telemetry-crate-scaffold.md) merged — empty
  `crates/telemetry/src/ids.rs` placeholder exists.
- A clean `cargo check --workspace` on `main`.

## 4. Step-by-step

### 4.1 Pull deps into the crate manifest

[`crates/telemetry/Cargo.toml`](../../../crates/telemetry/Cargo.toml)
already declares `pbkdf2`, `hmac`, `hex`, `once_cell`, `dirs` as
optional under the `telemetry` feature (from
[task 02-02](02-telemetry-crate-scaffold.md)). No further edit needed.

### 4.2 Write `crates/telemetry/src/ids.rs`

Replace the empty placeholder with:

```rust
//! Identity-layer helpers for `send_telemetry`.
//!
//! Three layers, each used as a key in the proxy payload:
//!
//! - [`get_anonymous_id`]: project-local uuid4, file-backed at
//!   `<project_root>/.anon_id`. Honours `TRACKING_ID` env override.
//! - [`get_persistent_id`]: machine-local uuid4, file-backed at
//!   `~/.cognee/.persistent_id`. Survives `forget(everything=True)`.
//! - [`get_api_key_tracking_id`]: deterministic PBKDF2-HMAC-SHA256
//!   hash of `LLM_API_KEY` with a configurable salt. Stable across
//!   machines for the same key.

#[cfg(feature = "telemetry")]
mod inner {
    use hmac::Hmac;
    use once_cell::sync::Lazy;
    use pbkdf2::pbkdf2;
    use sha2::Sha256;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use uuid::Uuid;

    const DEFAULT_SALT: &[u8] = b"cognee.telemetry.api-key-tracking.v1";
    const ITERATIONS: u32 = 100_000;
    const DKLEN: usize = 16;

    /// Cached anonymous id. Set on first call; re-reading the file on
    /// every event would be cheap but pointless — the file content is
    /// process-stable.
    static ANON_ID: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

    /// Cached persistent id. Same caching rationale as above.
    static PERSISTENT_ID: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

    /// Project-local anonymous identifier.
    pub fn get_anonymous_id() -> String {
        if let Ok(v) = std::env::var("TRACKING_ID") {
            if !v.is_empty() {
                return v;
            }
        }
        // lock poison is unrecoverable
        let mut cached = ANON_ID.lock().unwrap();
        if let Some(v) = cached.as_ref() {
            return v.clone();
        }
        let computed = compute_anon_id();
        *cached = Some(computed.clone());
        computed
    }

    fn compute_anon_id() -> String {
        let dir = match find_project_root() {
            Some(p) => p,
            None => match std::env::current_dir() {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        "could not resolve project root for .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
            },
        };
        let path = dir.join(".anon_id");
        match std::fs::read_to_string(&path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                let new_id = Uuid::new_v4().to_string();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        path = %dir.display(),
                        "could not create dir for .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
                if let Err(e) = std::fs::write(&path, &new_id) {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        path = %path.display(),
                        "could not write .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
                new_id
            }
        }
    }

    /// Walk up from `current_dir` looking for a `Cargo.toml`. Returns
    /// the first ancestor that contains one, or `None` if none is
    /// found before reaching the filesystem root.
    fn find_project_root() -> Option<PathBuf> {
        let start = std::env::current_dir().ok()?;
        let mut here: &Path = &start;
        loop {
            if here.join("Cargo.toml").is_file() {
                return Some(here.to_path_buf());
            }
            here = here.parent()?;
        }
    }

    /// Machine-local persistent identifier.
    pub fn get_persistent_id() -> String {
        // lock poison is unrecoverable
        let mut cached = PERSISTENT_ID.lock().unwrap();
        if let Some(v) = cached.as_ref() {
            return v.clone();
        }
        let computed = compute_persistent_id();
        *cached = Some(computed.clone());
        computed
    }

    fn compute_persistent_id() -> String {
        let dir = match dirs::home_dir() {
            Some(p) => p.join(".cognee"),
            None => {
                tracing::debug!(
                    target: "cognee.telemetry",
                    "no home directory; falling back to anonymous id"
                );
                return get_anonymous_id();
            }
        };
        let path = dir.join(".persistent_id");
        if let Ok(s) = std::fs::read_to_string(&path) {
            return s.trim().to_string();
        }
        // Seed from anonymous id if available.
        let mut new_id = get_anonymous_id();
        if new_id == "unknown-anonymous-id" {
            new_id = Uuid::new_v4().to_string();
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                path = %dir.display(),
                "could not create ~/.cognee for persistent id"
            );
            return get_anonymous_id();
        }
        if let Err(e) = std::fs::write(&path, &new_id) {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                path = %path.display(),
                "could not write persistent id"
            );
            return get_anonymous_id();
        }
        new_id
    }

    /// PBKDF2-HMAC-SHA256 hash of `LLM_API_KEY` with a configurable
    /// salt. Returns `"ak_<32-hex-chars>"` for non-empty keys, empty
    /// string otherwise.
    ///
    /// Read at every call (decision 11) — no caching, because tests
    /// and consumers may set `LLM_API_KEY` in-process at runtime.
    pub fn get_api_key_tracking_id() -> String {
        let key = std::env::var("LLM_API_KEY").unwrap_or_default();
        if key.is_empty() {
            return String::new();
        }
        let salt: Vec<u8> = std::env::var("TELEMETRY_API_KEY_TRACKING_SALT")
            .map(|s| s.into_bytes())
            .unwrap_or_else(|_| DEFAULT_SALT.to_vec());
        let mut out = [0u8; DKLEN];
        // PBKDF2 with dklen ≤ HMAC-SHA256 output (32) cannot fail.
        pbkdf2::<Hmac<Sha256>>(key.as_bytes(), &salt, ITERATIONS, &mut out)
            .expect("dklen 16 ≤ Sha256 output 32 — invariant holds");
        format!("ak_{}", hex::encode(out))
    }

    #[cfg(test)]
    mod tests {
        // Real test bodies live in the crate-level tests/ directory and
        // in `08-unit-tests.md`. This stub keeps the module compiling
        // when run in isolation.
    }
}

#[cfg(not(feature = "telemetry"))]
mod inner {
    pub fn get_anonymous_id() -> String { String::new() }
    pub fn get_persistent_id() -> String { String::new() }
    pub fn get_api_key_tracking_id() -> String { String::new() }
}

pub use inner::{get_anonymous_id, get_api_key_tracking_id, get_persistent_id};
```

### 4.3 Re-export from `lib.rs`

In `crates/telemetry/src/lib.rs`, replace the `pub mod ids { ... }`
placeholder with:

```rust
pub mod ids;
```

The `pub use inner::*;` at the bottom of `ids.rs` ensures
`cognee_telemetry::ids::get_persistent_id()` is callable.

### 4.4 Add a workspace-level smoke test

Inline `#[cfg(test)]` block at the bottom of `ids.rs` (full bodies
live in [task 02-08](08-unit-tests.md)):

```rust
#[cfg(all(test, feature = "telemetry"))]
mod smoke {
    use super::*;

    // Note: workspace uses Rust edition 2024, where `std::env::set_var`
    // and `std::env::remove_var` are `unsafe` (concurrent env mutation
    // is process-wide UB). The full byte-parity matrix in task 02-08
    // adds `serial_test::serial` so the unsafe is sound; these smoke
    // tests follow the same pattern.

    #[test]
    fn empty_llm_api_key_produces_empty_tracking_id() {
        // SAFETY: no other thread mutates env in this single-test block;
        //   full ordering across the suite is enforced in 02-08 via serial_test.
        unsafe {
            std::env::remove_var("LLM_API_KEY");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
        assert_eq!(get_api_key_tracking_id(), "");
    }

    #[test]
    fn tracking_id_format() {
        // SAFETY: see sibling test.
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
        let id = get_api_key_tracking_id();
        assert!(id.starts_with("ak_"));
        assert_eq!(id.len(), 3 + 32);
        assert!(
            id[3..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex suffix, got {id:?}"
        );
    }
}
```

These smoke tests are intentionally minimal here so the file
compiles and passes basic invariants; the full byte-parity matrix is
in [task 02-08](08-unit-tests.md).

### 4.5 Verify

```bash
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry  # noop branch
cargo test -p cognee-telemetry --features telemetry --lib
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings
```

All four must pass.

## 5. Verification

```bash
# 1. Both feature states compile.
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry

# 2. Smoke tests pass.
cargo test -p cognee-telemetry --features telemetry --lib ids::

# 3. No clippy warnings.
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings

# 4. Hand-verify hex output is lowercase.
LLM_API_KEY=sk-test cargo run --example print_tracking_id --features telemetry
# Expected: prefix "ak_" followed by 32 lowercase hex chars.
# (You'll need to add a tiny example in `examples/print_tracking_id.rs`
#  if one doesn't exist; it's a 5-line throwaway and need not be
#  committed.)

# 5. Cross-platform home dir resolution.
HOME=/tmp/cognee-test cargo test -p cognee-telemetry --features telemetry --lib
# Expected: persistent_id file lands in /tmp/cognee-test/.cognee/.persistent_id.
```

The byte-parity test against a Python-generated fixture is in
[task 02-08](08-unit-tests.md); this task only needs to pass the
format-and-presence checks above.

## 6. Files modified

- `crates/telemetry/src/ids.rs` — full implementation (replaces the
  empty placeholder from [task 02-02](02-telemetry-crate-scaffold.md)).
- `crates/telemetry/src/lib.rs` — change `pub mod ids { ... }` to
  `pub mod ids;` so the new file is included.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `pbkdf2` 0.13 breaks the call signature | None today; pinned to `0.12` in workspace deps | A future bump is gated by the byte-parity test in [task 02-08](08-unit-tests.md). |
| `dirs::home_dir()` returns `None` on Android | Possible (per the Android memory note about `dirs::config_dir()` being read-only on adb shell) | Fallback to `get_anonymous_id()` is exercised. The Android `android-default` feature **excludes** `telemetry` (decision 1), so this is mostly defensive — but tests in [task 02-08](08-unit-tests.md) cover the fallback path. |
| `LLM_API_KEY` read order race vs caller setting it | Caller runs in user-code; no race | Decision 11 explicitly defers the read to event-emission time. Documented in the rustdoc on `get_api_key_tracking_id`. |
| Salt env var modified mid-process produces inconsistent tracking ids | Possible | Acceptable — Python has the same property (env vars are read on every call). Document in [task 02-11](11-user-docs.md). |
| File-system errors (read-only, full disk) panic | None — every `?`-able operation is matched and a debug log is emitted | The fallback path returns `"unknown-anonymous-id"` and the proxy treats it as a sentinel. |
| `.cognee` directory permissions reveal Cognee usage to other users | Same as Python | Document in [task 02-11](11-user-docs.md). Permissions are inherited from `home_dir()` defaults (typically `0700` or `0755`). |
| Lock poisoning on `Mutex` panics | None — `lock().unwrap()` is the documented allowed pattern (see project CLAUDE.md "no `unwrap` rule"); poisoning means the prior holder panicked, no recovery available | Comment `// lock poison is unrecoverable` is included per convention. |

## 8. Out of scope

- Payload struct / URL sanitization (covered by [task 02-04](04-payload-and-sanitize.md)).
- HTTP client and dispatch (covered by [task 02-05](05-client-dispatch-and-optout.md)).
- Public API freeze (covered by [task 02-06](06-public-api-and-noop.md)).
- The PBKDF2 byte-parity fixture test (covered by [task 02-08](08-unit-tests.md)).
- Cross-SDK persistent-id parity test (covered by [task 02-10](10-cross-sdk-parity.md)).
