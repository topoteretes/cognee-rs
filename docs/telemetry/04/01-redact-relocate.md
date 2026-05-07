# Task 04-01 — Move `redact()` from `cognee-http-server` to `cognee-utils`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 04-05 — Instrument `LadybugAdapter::execute_query`](05-ladybug-instrumentation.md) (calls `redact()` on every query string).
- [Task 04-08 — PG adapters](08-pg-adapters.md) (calls `redact()` on `pg_graph_adapter` Cypher).
- [Task 04-09 — SeaORM ops](09-seaorm-ops-instrumentation.md) (calls `redact()` if any op records query text).

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decision**: #7 — foundation cleanups split into two tasks; this is one of them.

---

## 1. Goal

Move the existing `pub fn redact(&str) -> Cow<'_, str>` (and its tests)
from
[`crates/http-server/src/observability/redaction.rs`](../../crates/http-server/src/observability/redaction.rs)
into a new module `cognee_utils::redact`, so that adapter crates
(`cognee-vector`, `cognee-graph`, `cognee-database`, `cognee-llm`) can
call it **without depending on `cognee-http-server`** (which would be
an architectural cycle: adapters live below the HTTP layer).

The JSON-walking `redact_attributes()` stays in
`cognee-http-server` because it's specific to the observability HTTP
API; it is rewritten to call the relocated `redact()`.

After this task:

| Item | Before | After |
|---|---|---|
| `pub fn redact(&str) -> Cow<'_, str>` | `cognee_http_server::observability::redaction::redact` | `cognee_utils::redact::redact` |
| `pub fn redact_attributes(&mut serde_json::Map)` | `cognee_http_server::observability::redaction::redact_attributes` | unchanged location, but body calls `cognee_utils::redact::redact` |
| Regex set + `OnceLock` | inside `crates/http-server/src/observability/redaction.rs` | inside `crates/utils/src/redact.rs` |
| `regex` direct dep | only on `cognee-http-server` | also on `cognee-utils` |

## 2. Rationale

- Adapter crates **must not** depend on `cognee-http-server`. The
  http-server already depends on adapters transitively (it wires up
  the search/cognify pipelines that use Qdrant/Ladybug); adding the
  reverse edge would be a cycle. The current location of `redact()`
  is therefore unreachable from adapter call sites that need it.
- `cognee-utils` is the existing home for cross-crate utilities
  (retry, ID generation, tracing key constants) and is already a
  transitive dep of every adapter crate, so the relocation does not
  introduce a new edge in the dep graph.
- The function is **already correct**: four-pattern regex set,
  six-character prefix-survives semantics, allocation-free `Cow`
  fast path, and existing unit tests cover OpenAI keys, generic
  `api_key=`, `Bearer …`, `password=`, and a multi-secret case.
  Relocation is mechanical — the body and tests don't change.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- `regex = "1"` is currently only a **direct dep** of `cognee-http-server`
  ([`crates/http-server/Cargo.toml:95`](../../crates/http-server/Cargo.toml#L95));
  it is **not** in the workspace `[workspace.dependencies]` table
  ([`Cargo.toml:51`](../../Cargo.toml#L51) onwards). This task promotes
  it to a workspace dep so both `cognee-utils` and `cognee-http-server`
  can pin to the same version via `workspace = true`.
- No outstanding edits to
  [`crates/utils/`](../../crates/utils/) or
  [`crates/http-server/src/observability/redaction.rs`](../../crates/http-server/src/observability/redaction.rs).

## 4. Step-by-step

### 4.1 Promote `regex` to a workspace dep, then add it to `cognee-utils`

The workspace `[workspace.dependencies]` table
([`Cargo.toml:51`](../../Cargo.toml#L51)) does **not** currently
declare `regex` — only `cognee-http-server` has it as a direct dep
(`regex = "1"` at
[`crates/http-server/Cargo.toml:95`](../../crates/http-server/Cargo.toml#L95)).
Promote it to the workspace so both crates pin to the same version.

1. Edit the root [`Cargo.toml`](../../Cargo.toml): add
   `regex = "1"` inside `[workspace.dependencies]` (alphabetical
   placement near `rand`/`rayon`).

2. Edit [`crates/http-server/Cargo.toml`](../../crates/http-server/Cargo.toml):
   change the `regex = "1"` direct pin (line 95) to
   `regex = { workspace = true }` so the http-server keeps using the
   same crate, just via the workspace table.

3. Edit [`crates/utils/Cargo.toml`](../../crates/utils/Cargo.toml):

   ```toml
   [dependencies]
   # ... existing deps ...

   # Secret redaction (cognee_utils::redact)
   regex = { workspace = true }
   ```

After these three edits, `cargo check -p cognee-utils -p cognee-http-server`
should still succeed; only the dep declaration site moved.

### 4.2 Create `crates/utils/src/redact.rs`

Move the `pub fn redact()` function plus the `patterns()` `OnceLock`
helper plus the relevant unit tests verbatim from
[`crates/http-server/src/observability/redaction.rs`](../../crates/http-server/src/observability/redaction.rs).
The new file should look exactly like the old one **minus**
`redact_attributes()`, `redact_value()`, and the
`nested_object_redacted_in_place` test (those stay in http-server).

Module-level doc comment to lead with:

```rust
//! Secret redaction helper, shared across cognee crates.
//!
//! Mirrors Python's `redact_secrets`
//! (`cognee/modules/observability/tracing.py`): four regex patterns
//! covering OpenAI-style keys, generic `api_key=`/`api-key=`,
//! `Bearer <token>`, and `password=`. On match we keep the first 6
//! characters of the value and replace the rest with
//! `***REDACTED***` so the original prefix remains visible for
//! debugging.
//!
//! The JSON-walking variant `redact_attributes` lives in
//! `cognee-http-server` because it is specific to the observability
//! HTTP API.
```

### 4.3 Re-export from `cognee-utils`

Edit [`crates/utils/src/lib.rs`](../../crates/utils/src/lib.rs):

```rust
pub mod id_generation;
pub mod redact;             // NEW
pub mod retry;
pub mod tracing_keys;

pub use id_generation::{NAMESPACE_OID, generate_edge_name, generate_node_id, generate_node_name};
pub use redact::redact;     // NEW — top-level convenience re-export
pub use retry::{RetryConfig, RetryDecision, retry_with_backoff};
```

The top-level re-export keeps adapter call sites short:
`use cognee_utils::redact;` then `redact(query)`.

### 4.4 Rewrite `crates/http-server/src/observability/redaction.rs`

Replace the body of the file with:

```rust
//! JSON-walking secret redaction for the observability HTTP API.
//!
//! The single-string `redact()` helper now lives in
//! `cognee_utils::redact` so adapter crates can reach it without
//! depending on the http-server. This module keeps only the JSON
//! object walker.

use std::borrow::Cow;

use cognee_utils::redact::redact;

/// Walk a JSON object and redact any string-leaf values in place.
///
/// Object *keys* are intentionally left alone (matches Python).
pub fn redact_attributes(attrs: &mut serde_json::Map<String, serde_json::Value>) {
    for (_k, v) in attrs.iter_mut() {
        redact_value(v);
    }
}

fn redact_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => {
            if let Cow::Owned(replaced) = redact(s) {
                *s = replaced;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (_k, vv) in map.iter_mut() {
                redact_value(vv);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_object_redacted_in_place() {
        let mut value = json!({
            "headers": {
                "Authorization": "Bearer eyJabc.def.ghi-very-long-jwt-1234567890",
                "X-Other": "fine"
            },
            "key_unchanged": "value"
        });
        if let serde_json::Value::Object(map) = &mut value {
            redact_attributes(map);
        }
        let auth = value["headers"]["Authorization"].as_str().unwrap_or("");
        assert!(auth.contains("***REDACTED***"));
        assert!(!auth.contains("ghi-very-long-jwt"));
        assert_eq!(value["headers"]["X-Other"], "fine");
        assert_eq!(value["key_unchanged"], "value");
    }
}
```

The `regex` direct dep on `cognee-http-server` can stay (it has other
users — middleware, span_buffer_layer, settings DTO redaction). Do
**not** remove it in this task; that is a separate cleanup if ever
needed.

### 4.5 Verify call sites

`crates/http-server/src/observability/span_buffer_layer.rs:19` already
imports `redact_attributes` from the same module — that import stays
valid because `redact_attributes` keeps its current path.

There are **no other call sites** of the relocated `redact()` today
(it was only consumed via `redact_attributes` and via the inline
unit tests). So no other crate needs editing in this task; the
adapter crates start using `cognee_utils::redact::redact` in
04-05 / 04-08 / 04-09.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. cognee-utils builds standalone (no http-server in scope).
cargo check -p cognee-utils

# 3. The relocated tests still pass in their new home.
cargo test -p cognee-utils redact::

# 4. The remaining JSON walker test still passes in http-server.
cargo test -p cognee-http-server -- redaction::

# 5. Clippy.
cargo clippy --all-targets -- -D warnings

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`Cargo.toml`](../../Cargo.toml) — promote `regex = "1"` into
  `[workspace.dependencies]`.
- [`crates/http-server/Cargo.toml`](../../crates/http-server/Cargo.toml)
  — switch the existing direct `regex = "1"` pin to
  `regex = { workspace = true }`.
- [`crates/utils/Cargo.toml`](../../crates/utils/Cargo.toml) — add
  `regex = { workspace = true }` direct dep.
- [`crates/utils/src/redact.rs`](../../crates/utils/src/redact.rs) —
  NEW. Contains `redact()`, `patterns()`, and the six unit tests
  (`redacts_openai_key`, `redacts_api_key_assignment`,
  `redacts_bearer_token`, `redacts_password_assignment`,
  `inert_string_passes_through_borrowed`,
  `multiple_secrets_in_one_string_all_redacted`,
  `pattern_module_loads_without_panic`).
- [`crates/utils/src/lib.rs`](../../crates/utils/src/lib.rs) —
  declare `pub mod redact;` and add `pub use redact::redact;`.
- [`crates/http-server/src/observability/redaction.rs`](../../crates/http-server/src/observability/redaction.rs)
  — strip `redact()`, `patterns()`, the corresponding tests, and the
  `regex` / `OnceLock` imports; keep `redact_attributes()`,
  `redact_value()`, and the `nested_object_redacted_in_place` test;
  add `use cognee_utils::redact::redact;`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Inline `cargo doc` examples reference `cognee_http_server::observability::redaction::redact` somewhere | Very low — `grep -rn "observability::redaction::redact" crates/` returned only the `redact_attributes` re-export at the time of writing. | Sub-agent A re-runs the grep before approving. |
| `regex` workspace dep version drift | None — both crates pull `workspace = true`, so they pin to the same version. | n/a |
| A future cleanup tries to drop the `regex` dep on `cognee-http-server` and breaks `redact_attributes` (which still uses `Cow` from std but no longer the `regex` crate directly) | Low. After this task, `regex` is no longer used in `redaction.rs`; it is still used by `middleware/tracing.rs` and `routers/settings.rs`. | Leave the http-server `regex` dep alone; document that it stays for unrelated users. |

## 8. Out of scope

- Removing the `regex` dep from `cognee-http-server` (still used by
  middleware/settings).
- Adding new redaction patterns. The four-pattern set is locked at
  Python parity.
- Refactoring `redact_attributes` to be more efficient (it walks the
  JSON twice in the worst case — once to clone, once to write back).
  Out-of-scope micro-optimisation.
