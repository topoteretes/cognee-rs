# CL-2 — Hoist `SECRET_FIELDS` redaction into `bindings-common`

- **Binding:** All (`capi/`, `js/`, `python/`) + `crates/bindings-common/`
- **Dimension:** Cleanliness
- **Priority:** P2
- **Status:** Not started

## Problem

The config-redaction secret-field list is **triplicated** — one copy per binding,
confirmed:

- [capi/cognee-capi/src/sdk_config.rs](../../capi/cognee-capi/src/sdk_config.rs)
- [js/cognee-neon/src/config.rs](../../js/cognee-neon/src/config.rs)
- [python/src/config.rs](../../python/src/config.rs)

Each blanks the same ~10 secret fields to `"***REDACTED***"` before returning
config to the host language. The code itself flags this as a known drift risk
("hoisting into bindings-common is tracked as a follow-up"). If a new secret
field is added to the config and only one or two copies are updated, a binding
will **leak a secret** through `config.get()`. This is a security-relevant
duplication, not merely stylistic.

The generic `cognee_utils::redact` was intentionally not reused because it
matches secret-*shaped* substrings, not the bare key/value config fields — so the
right fix is a shared, config-aware helper in `bindings-common`, where the three
bindings already share their op logic.

## Goal / definition of done

There is exactly **one** definition of the secret-field set and the
config-redaction logic, in `crates/bindings-common`, used by all three bindings.
Adding a secret field in one place protects every binding.

## Implementation plan

### Step 1 — Add the canonical list + helper to `bindings-common`

Create `crates/bindings-common/src/redact.rs` (and `pub mod redact;` in
[crates/bindings-common/src/lib.rs](../../crates/bindings-common/src/lib.rs)):

```rust
/// Config keys whose values must be redacted before crossing a binding boundary.
pub const SECRET_FIELDS: &[&str] = &[
    "llm_api_key",
    "embedding_api_key",
    // ... the full set currently duplicated in the three config modules ...
];

const REDACTED: &str = "***REDACTED***";

/// Redact secret values in a config JSON object in place.
pub fn redact_config_json(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        for key in SECRET_FIELDS {
            if let Some(v) = obj.get_mut(*key) {
                if !v.is_null() {
                    *v = serde_json::Value::String(REDACTED.to_string());
                }
            }
        }
        // recurse into nested config sub-objects (llm/embedding/vector/graph)
        for (_k, v) in obj.iter_mut() {
            if v.is_object() {
                redact_config_json(v);
            }
        }
    }
}
```

Reconcile the exact field list and nesting behavior against all three current
copies — take the **union** so no binding loses coverage, and verify the nesting
(some configs are flat `llm_api_key`, others nested under `llm.api_key`); handle
both shapes the existing copies handle.

### Step 2 — Unit-test the helper

In `bindings-common`, add tests asserting every `SECRET_FIELDS` entry is redacted
(flat and nested), null values are left null (not redacted to a string), and
non-secret fields pass through unchanged.

### Step 3 — Replace the three copies

In each binding's config module, delete the local `SECRET_FIELDS`/redaction code
and call `cognee_bindings_common::redact::redact_config_json(&mut json)` before
marshalling to the host language:

- [capi/cognee-capi/src/sdk_config.rs](../../capi/cognee-capi/src/sdk_config.rs)
- [js/cognee-neon/src/config.rs](../../js/cognee-neon/src/config.rs)
- [python/src/config.rs](../../python/src/config.rs)

Keep each binding's host-side serialization (`serde_to_py`, `JSON.parse`,
`CString`) — only the redaction logic moves.

### Step 4 — Grep-gate the dedup

```bash
grep -rln 'SECRET_FIELDS\|REDACTED' capi/ js/ python/ --include='*.rs'
```

Should return **no** matches outside `crates/bindings-common` after the change
(the constant and string literal now live only there).

## Verification

```bash
# bindings-common unit tests
cargo test -p cognee-bindings-common redact
# each binding still redacts via config.get()
cd capi && bash scripts/check.sh        # config example shows ***REDACTED***
cd js && npm test                       # config.test.ts asserts redaction
cd python && pytest tests -k config
# from repo root
scripts/check_all.sh
```

Add/confirm a test in each binding that sets an API key then reads config back
and asserts `"***REDACTED***"`, so the shared helper is exercised through each
boundary.

## Risks / notes

- The three current copies may differ slightly (field names, nested vs flat,
  whether null is redacted). Diff them carefully and take the safe union;
  document any intentional per-binding difference (there should be none).
- This is security-relevant: a missed field leaks a credential. The unit test in
  Step 2 is the durable guard against future drift.
