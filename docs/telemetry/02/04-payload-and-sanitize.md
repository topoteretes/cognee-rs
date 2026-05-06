# Task 02-04 — `TelemetryPayload` struct + URL sanitization

**Status**: implemented in commit 0c053b5 (note: collapsed nested if into a let-chain for clippy; sanitize_nested_properties takes a names: &[&str] slice rather than hard-coding "url" to mirror Python's parametrized helper; noop sanitize signature uses &mut () since serde_json is feature-gated).
**Owner**: _unassigned_
**Depends on**:
- [Task 02-02 — Crate scaffold](02-telemetry-crate-scaffold.md) — empty `payload`/`sanitize` modules.

(Does **not** depend on [task 02-03](03-id-derivation.md) — the
payload struct accepts identity strings as parameters; the wiring is
done in [task 02-05](05-client-dispatch-and-optout.md).)

**Blocks**:
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md) (the dispatcher serializes a `TelemetryPayload`).
- [Task 02-08 — Unit tests](08-unit-tests.md) (URL sanitization tests).
- [Task 02-09 — Integration tests](09-integration-tests.md) (asserts the wire schema matches Python).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Implement two strictly-scoped helpers:

1. **`TelemetryPayload`** (in `crates/telemetry/src/payload.rs`) — a
   `serde::Serialize` struct that produces the **exact** Python
   wire schema, including the backward-compat `api_key_hash` alias.
2. **`sanitize_nested_properties`** (in
   `crates/telemetry/src/sanitize.rs`) — a recursive walker that
   replaces caller-supplied string values under selected key names
   with `uuid5(NAMESPACE_OID, value)`. Mirrors Python's
   `_sanitize_nested_properties` (utils.py:107-124).

This task does **not** touch identity derivation (task 02-03), the
HTTP client (task 02-05), or the public dispatch API (task 02-06). It
is a pure data-modelling task; both helpers are deterministic and
fully unit-testable.

## 2. Rationale — wire-format parity is contractual

A user moving between SDKs MUST land in the same proxy bucket. That
implies the JSON body byte-for-byte matches the Python schema in:

- **Field names** (`anonymous_id`, `event_name`, `user_properties`,
  `properties`).
- **Field nesting** (`user_properties` and `properties` are sibling
  objects, both repeating the same identity tuple — Python does this
  intentionally for analytics dashboards that flatten only one of the
  two views).
- **Backward-compat alias** — `api_key_hash` mirrors the same value
  as `api_key_tracking_id` (utils.py:226). A renamed field would
  silently break analytics joins.
- **`time` field format** — `MM/DD/YYYY` (`current_time.strftime("%m/%d/%Y")`,
  utils.py:206).
- **`url`-key sanitization** — every nested object has its `url`
  string keys replaced with a uuid5 derivation **before** transport.
  Python only sanitizes the key `"url"` by default; we do the same.

### Why a struct, not a `serde_json::Value`

Python builds the payload as a `dict`. We could mirror that with a
hand-built `serde_json::json!({...})` per call site. We prefer a
strongly-typed struct because:

1. **Schema drift detection at compile time.** If Python changes a
   field name, the struct definition diverges visibly.
2. **`#[serde(rename = ...)]` handles `api_key_hash` aliasing
   cleanly** — without the struct, every call site would have to
   remember to insert both `api_key_tracking_id` and `api_key_hash`.
3. **Test-friendly.** Unit tests can assert `serde_json::to_value`
   roundtrips against a known-good Python fixture file without going
   through the full HTTP path.

### Why uuid5 for URL sanitization, not a plain hash

Python does `str(uuid5(NAMESPACE_OID, value))`. UUID v5 is
deterministic SHA-1 of `(namespace, name)` — the same value always
hashes to the same UUID, which lets dashboards group repeated URLs
without exposing them. We use the same `NAMESPACE_OID =
6ba7b812-9dad-11d1-80b4-00c04fd430c8` namespace as the cognee
codebase elsewhere
([`crates/utils/src/id_generation.rs`](../../../crates/utils/src/id_generation.rs)
already exposes `NAMESPACE_OID`); reusing it keeps Rust ↔ Python
parity and avoids creating a parallel namespace constant.

## 3. Pre-conditions

- [Task 02-02](02-telemetry-crate-scaffold.md) merged — empty
  `payload.rs` and `sanitize.rs` (currently inline in `lib.rs`).
- `serde`, `serde_json`, `chrono`, `uuid` available under the
  `telemetry` feature (declared in
  [task 02-02](02-telemetry-crate-scaffold.md)).
- A clean `cargo check --workspace` on `main`.

## 4. Step-by-step

### 4.1 Replace `lib.rs` placeholder modules with file-backed modules

In `crates/telemetry/src/lib.rs`, replace:

```rust
pub mod sanitize { /* ... */ }
pub mod payload { /* ... */ }
```

with:

```rust
pub mod sanitize;
pub mod payload;
```

### 4.2 Create `crates/telemetry/src/sanitize.rs`

```rust
//! Recursive URL-sanitization for caller-supplied properties.
//!
//! Mirrors Python `_sanitize_nested_properties` (utils.py:107-124).
//! For every nested string value whose key matches one of `names`,
//! the value is replaced with `uuid5(NAMESPACE_OID, value)`.

#[cfg(feature = "telemetry")]
use serde_json::Value;
#[cfg(feature = "telemetry")]
use uuid::Uuid;

/// SHA-1 OID namespace: `6ba7b812-9dad-11d1-80b4-00c04fd430c8`.
///
/// Same constant used elsewhere in the workspace
/// (`cognee_utils::id_generation::NAMESPACE_OID`) — reusing it keeps
/// uuid5 derivations consistent across the SDK.
#[cfg(feature = "telemetry")]
const NAMESPACE_OID: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1,
    0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Replace, in-place, every string value whose key is in `names`
/// with `uuid5(NAMESPACE_OID, value).to_string()`. Walks objects
/// and arrays recursively; leaves other scalar types untouched.
#[cfg(feature = "telemetry")]
pub fn sanitize_nested_properties(value: &mut Value, names: &[&str]) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if names.contains(&k.as_str()) {
                    if let Value::String(s) = v {
                        *v = Value::String(
                            Uuid::new_v5(&NAMESPACE_OID, s.as_bytes()).to_string(),
                        );
                        continue;
                    }
                }
                sanitize_nested_properties(v, names);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                sanitize_nested_properties(item, names);
            }
        }
        _ => {}
    }
}

/// Noop when the feature is off — caller-supplied properties are
/// dropped entirely on the dispatch path, so sanitization is moot.
#[cfg(not(feature = "telemetry"))]
pub fn sanitize_nested_properties(_value: &mut serde_json::Value, _names: &[&str]) {}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_top_level_url_string() {
        let mut v = json!({ "url": "https://example.com", "other": "x" });
        sanitize_nested_properties(&mut v, &["url"]);
        let url = v["url"].as_str().expect("url is string");
        assert_ne!(url, "https://example.com");
        assert!(Uuid::parse_str(url).is_ok(), "expected uuid5 string");
        assert_eq!(v["other"], "x");
    }

    #[test]
    fn descends_into_nested_objects_and_arrays() {
        let mut v = json!({
            "outer": {
                "inner": [
                    { "url": "https://a.example", "keep": "yes" },
                    { "url": "https://b.example" }
                ]
            }
        });
        sanitize_nested_properties(&mut v, &["url"]);
        let urls: Vec<&str> = v["outer"]["inner"]
            .as_array()
            .expect("array")
            .iter()
            .map(|i| i["url"].as_str().expect("url"))
            .collect();
        for u in &urls {
            assert!(Uuid::parse_str(u).is_ok(), "expected uuid5, got {u}");
        }
        assert_ne!(urls[0], urls[1], "different inputs → different uuid5");
    }

    #[test]
    fn non_string_url_value_is_left_alone() {
        // Defensive: if a caller mistakenly passes `url: 42`, do not
        // panic; just leave it alone (Python silently coerces, we
        // diverge slightly here for safety).
        let mut v = json!({ "url": 42 });
        sanitize_nested_properties(&mut v, &["url"]);
        assert_eq!(v["url"], 42);
    }
}
```

### 4.3 Create `crates/telemetry/src/payload.rs`

```rust
//! Strongly-typed serde model of the `send_telemetry` proxy payload.
//!
//! Field-for-field parity with Python's
//! `cognee.shared.utils.send_telemetry` (utils.py:176-228). Includes
//! the backward-compat `api_key_hash` alias (utils.py:226) which
//! carries the same value as `api_key_tracking_id`.

#[cfg(feature = "telemetry")]
use serde::Serialize;
#[cfg(feature = "telemetry")]
use serde_json::Value;

/// Top-level proxy payload, dispatched as the body of
/// `POST https://test.prometh.ai`.
#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct TelemetryPayload<'a> {
    /// Project-local uuid4 from `<project_root>/.anon_id`.
    pub anonymous_id: &'a str,
    /// Caller-supplied event name (e.g. `"cognee.forget"`).
    pub event_name: &'a str,
    /// Identity tuple repeated under the `user_properties` view.
    pub user_properties: UserProperties<'a>,
    /// Identity tuple plus `time` and the spread of caller-supplied
    /// `additional_properties` (after URL sanitization).
    pub properties: Properties<'a>,
}

#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct UserProperties<'a> {
    pub user_id: &'a str,
    pub persistent_id: &'a str,
    pub api_key_tracking_id: &'a str,
    /// Backward-compat alias of `api_key_tracking_id`. Same value.
    pub api_key_hash: &'a str,
}

#[cfg(feature = "telemetry")]
#[derive(Debug, Serialize)]
pub struct Properties<'a> {
    /// `MM/DD/YYYY` of the current date — Python's
    /// `current_time.strftime("%m/%d/%Y")`.
    pub time: String,
    pub user_id: &'a str,
    pub anonymous_id: &'a str,
    pub persistent_id: &'a str,
    pub api_key_tracking_id: &'a str,
    pub api_key_hash: &'a str,
    /// `sdk_runtime: "rust"` — added per locked decision 2 so the
    /// proxy can distinguish Rust vs Python events without losing
    /// cross-SDK identity grouping.
    pub sdk_runtime: &'static str,
    /// Cognee crate version — `env!("CARGO_PKG_VERSION")`.
    pub cognee_version: &'static str,
    /// Caller-supplied properties, already sanitized by
    /// `sanitize_nested_properties` (URL keys hashed). Flattened into
    /// the parent object on the wire — Python spreads the dict.
    #[serde(flatten)]
    pub additional: AdditionalProperties,
}

/// A `serde_json::Value::Object` flattened into `Properties`. Modelled
/// as a wrapper so the `#[serde(flatten)]` works correctly on a
/// `Value`.
#[cfg(feature = "telemetry")]
#[derive(Debug, Default, Serialize)]
#[serde(transparent)]
pub struct AdditionalProperties {
    inner: serde_json::Map<String, Value>,
}

#[cfg(feature = "telemetry")]
impl AdditionalProperties {
    /// Construct from a caller-provided `Value::Object`. Anything
    /// other than an object (e.g. `Value::Array`, `Value::String`)
    /// is dropped with a `tracing::debug` log and treated as empty —
    /// Python coerces silently, we diverge for safety since the
    /// payload contract requires a flat object.
    pub fn from_value(v: Option<Value>) -> Self {
        match v {
            Some(Value::Object(map)) => Self { inner: map },
            Some(other) => {
                tracing::debug!(
                    target: "cognee.telemetry",
                    actual_type = std::any::type_name_of_val(&other),
                    "additional_properties was not an object; dropping"
                );
                Self::default()
            }
            None => Self::default(),
        }
    }

    /// Mutable access for [`crate::sanitize::sanitize_nested_properties`].
    pub fn as_value_mut(&mut self) -> Value {
        // Take the inner map out, sanitize externally, put it back.
        // Caller is the dispatcher; this is a single-use API.
        Value::Object(std::mem::take(&mut self.inner))
    }

    /// Restore from a sanitized [`Value`].
    pub fn replace_with(&mut self, v: Value) {
        if let Value::Object(map) = v {
            self.inner = map;
        }
        // Defensive: if sanitization somehow returned a non-object,
        // leave self empty.
    }
}

/// Format the current date as `MM/DD/YYYY` to match Python's
/// `current_time.strftime("%m/%d/%Y")` (utils.py:206).
#[cfg(feature = "telemetry")]
pub fn format_time_field(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%m/%d/%Y").to_string()
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn time_field_format() {
        let when = chrono::DateTime::parse_from_rfc3339("2026-05-06T12:00:00Z")
            .expect("rfc3339 fixture")
            .with_timezone(&chrono::Utc);
        assert_eq!(format_time_field(when), "05/06/2026");
    }

    #[test]
    fn payload_roundtrips_to_python_compatible_json() {
        let mut additional = AdditionalProperties::from_value(Some(json!({
            "endpoint": "POST /api/v1/forget",
            "cognee_version": "0.1.0",
        })));
        let payload = TelemetryPayload {
            anonymous_id: "a-id",
            event_name: "cognee.forget",
            user_properties: UserProperties {
                user_id: "u-id",
                persistent_id: "p-id",
                api_key_tracking_id: "ak_deadbeefcafebabe0123456789abcdef",
                api_key_hash: "ak_deadbeefcafebabe0123456789abcdef",
            },
            properties: Properties {
                time: "05/06/2026".into(),
                user_id: "u-id",
                anonymous_id: "a-id",
                persistent_id: "p-id",
                api_key_tracking_id: "ak_deadbeefcafebabe0123456789abcdef",
                api_key_hash: "ak_deadbeefcafebabe0123456789abcdef",
                sdk_runtime: "rust",
                cognee_version: "0.1.0",
                additional,
            },
        };
        let v = serde_json::to_value(&payload).expect("serialize");
        // Spot-check the wire schema.
        assert_eq!(v["anonymous_id"], "a-id");
        assert_eq!(v["event_name"], "cognee.forget");
        assert_eq!(v["user_properties"]["api_key_hash"], v["user_properties"]["api_key_tracking_id"]);
        assert_eq!(v["properties"]["sdk_runtime"], "rust");
        assert_eq!(v["properties"]["time"], "05/06/2026");
        // additional_properties were flattened.
        assert_eq!(v["properties"]["endpoint"], "POST /api/v1/forget");
    }
}
```

### 4.4 Verify

```bash
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry  # noop branch
cargo test -p cognee-telemetry --features telemetry --lib payload:: sanitize::
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings
```

## 5. Verification

```bash
# 1. Both feature states compile.
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry

# 2. Inline tests pass.
cargo test -p cognee-telemetry --features telemetry --lib

# 3. JSON output matches Python schema.
# (Use the `payload_roundtrips_to_python_compatible_json` test as the
#  in-tree contract; cross-SDK byte parity is verified in task 02-10.)

# 4. No clippy warnings.
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings
```

## 6. Files modified

- `crates/telemetry/src/lib.rs` — change `pub mod sanitize { ... }`
  and `pub mod payload { ... }` to `pub mod sanitize;` and
  `pub mod payload;`.
- `crates/telemetry/src/sanitize.rs` — new file (replaces inline
  empty module).
- `crates/telemetry/src/payload.rs` — new file (replaces inline
  empty module).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `#[serde(flatten)]` on a `Value::Object` produces unexpected ordering | None — `serde_json::Map` preserves insertion order under the `preserve_order` feature; we don't enable it, so order is BTreeMap-stable. The proxy doesn't care about field order. | If a downstream test asserts byte-equality, switch to `preserve_order` workspace-wide. Document a follow-up if needed. |
| `chrono::Utc::now()` in tests | Tests use a fixed RFC-3339 fixture; production uses `Utc::now()`. | Production `format_time_field` is called by the dispatcher in [task 02-05](05-client-dispatch-and-optout.md), with `chrono::Utc::now()` injected at the call site so tests can pin time. |
| Caller passes `additional_properties` containing field names that collide with the static `Properties` fields (e.g. `time`, `user_id`) | Possible — Python silently lets the additional dict overwrite the structural fields | We diverge slightly: with `#[serde(flatten)]` and a flattened map, collisions produce duplicate keys in JSON, which most consumers will deduplicate inconsistently. Document in the rustdoc that callers MUST NOT use any of the reserved field names. Add a debug-mode assertion in [task 02-05](05-client-dispatch-and-optout.md) that warns on collision. |
| URL hashing collides for two different URLs | uuid5 collision probability is 1/2^122 — astronomically negligible | Same as Python; not a parity risk. |
| `sdk_runtime` field accidentally renamed | Low — defined as a `&'static str` literal | The integration test in [task 02-09](09-integration-tests.md) asserts `sdk_runtime == "rust"` against a captured payload. |

## 8. Out of scope

- Identity derivation (covered by [task 02-03](03-id-derivation.md)).
- HTTP client and dispatch (covered by [task 02-05](05-client-dispatch-and-optout.md)).
- Cross-SDK byte parity tests (covered by [task 02-10](10-cross-sdk-parity.md)).
- Public API freeze (covered by [task 02-06](06-public-api-and-noop.md)).
