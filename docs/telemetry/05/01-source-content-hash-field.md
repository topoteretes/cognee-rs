# Task 05-01 — Add `source_content_hash` field on `DataPoint`

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (`stamp_tree` writes the field).
- [Task 05-08 — Vector payload full DataPoint dump](08-vector-payload-full-dump.md) (the helper serializes the field).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: — (foundational; no decision applies directly).

---

## 1. Goal

Add the missing fifth provenance field on
[`cognee_models::DataPoint`](../../crates/models/src/data_point.rs) so that
the rest of the gap can write to and read from it:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub source_content_hash: Option<String>,
```

Initialise to `None` in both constructors (`DataPoint::new` and
`DataPoint::with_metadata`) and extend the inline test for default-state
assertions.

After this task, `serde` round-trips for legacy payloads (where the field
is absent) continue to deserialise cleanly because of the `#[serde(default)]`
attribute, and JSON serialisations omit the field when `None` because of
`skip_serializing_if`.

## 2. Rationale

- Python's [`DataPoint.py:55-62`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/engine/models/DataPoint.py#L55)
  declares all five `source_*` fields. Rust currently has only four
  (lines 67-79 of [`data_point.rs`](../../crates/models/src/data_point.rs#L67-L79)).
- Without the field, `cognee_core::provenance::stamp_tree` (gap task
  05-03) cannot propagate `Data.content_hash` from raw ingestion artefacts
  into the knowledge graph.
- Without it, the cross-SDK parity test (gap task 05-10) cannot assert
  that "every Rust DocumentChunk carries the same `source_content_hash`
  as the Python equivalent" — which is the only way to detect a future
  regression in lineage tracking.

## 3. Pre-conditions

- Clean `cargo check --all-targets` on `main`.
- No outstanding edits to
  [`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs).
- Verify the existing four `source_*` fields use
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so that
  the new field follows the same pattern.

## 4. Step-by-step

### 4.1 Add the field

Edit
[`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs)
inside `pub struct DataPoint`. After the existing `source_user` block
(line 78-79), add:

```rust
/// Content hash of the raw `Data` artefact that produced this DataPoint.
/// Propagates from upstream `Data.content_hash` through every task in
/// the cognify pipeline, enabling content-addressed lineage queries.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub source_content_hash: Option<String>,
```

### 4.2 Initialise in `new`

In `DataPoint::new` (currently lines 92-110), add `source_content_hash: None`
inside the struct literal — alphabetically near the other `source_*` fields
(insert between `source_user: None,` and `feedback_weight: 0.5,`):

```rust
source_pipeline: None,
source_task: None,
source_node_set: None,
source_user: None,
source_content_hash: None,            // NEW
feedback_weight: 0.5,
```

### 4.3 Initialise in `with_metadata`

Same change in `DataPoint::with_metadata` (currently lines 113-135) —
insert `source_content_hash: None,` in the same position.

### 4.4 Extend the default-state test

In the inline `tests` module (line 173 onwards), update
`test_data_point_creation` to also assert `dp.source_content_hash.is_none()`:

```rust
assert!(dp.source_user.is_none());
assert!(dp.source_content_hash.is_none());     // NEW
```

### 4.5 (Optional but recommended) Add a round-trip test

Append a new test that confirms serialisation order and absence-on-`None`
behaviour:

```rust
#[test]
fn source_content_hash_round_trips_when_set_and_omitted_when_none() {
    let mut dp = DataPoint::new("Entity", None);
    assert!(serde_json::to_string(&dp).unwrap().contains("source_content_hash") == false,
        "absent field must be skipped by serde");

    dp.source_content_hash = Some("md5:abcdef".to_string());
    let json = serde_json::to_string(&dp).unwrap();
    assert!(json.contains(r#""source_content_hash":"md5:abcdef""#));

    let parsed: DataPoint = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.source_content_hash.as_deref(), Some("md5:abcdef"));
}
```

This guards against a future struct-field reordering that would break
the cross-SDK parity test.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. The model crate's own tests still pass.
cargo test -p cognee-models data_point

# 3. The new round-trip test passes.
cargo test -p cognee-models source_content_hash_round_trips

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs)
  — add the `source_content_hash` field, two constructor lines, and one
  (optionally two) test assertion(s).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Existing serialised DataPoint payloads (graph-store node JSON, vector-store metadata) deserialise differently | Very low — `#[serde(default)]` ensures a missing field deserialises as `None`, matching the field's never-set state. | The optional round-trip test in §4.5 guards this. |
| Downstream code constructing `DataPoint` with `..Default::default()` style breaks | None — `DataPoint` does not derive `Default`; all construction goes through `new` / `with_metadata` / `Deserialize`. | n/a |
| Bincode-serialised payloads on disk break | None today — `cognee_models::DataPoint` is not used as a `bincode` payload anywhere in the workspace; both graph and vector backends round-trip via JSON. | If a future bincode user appears, that user must add a backward-compat strategy at their own layer; absorbing that complexity here would be premature. |

## 8. Out of scope

- Adding new constructor variants or `with_*` builder methods. Five
  optional fields use direct field-set syntax in callers; that is
  sufficient.
- Changing the type to `Option<ContentHash>` (a typed wrapper). The
  Rust DataPoint mirrors Python's `str | None`; introducing a typed
  wrapper would force every caller (including bincode/JSON consumers)
  to migrate.
- Backfilling `source_content_hash` on existing graph nodes / vector
  payloads. The field starts populating from the next cognify run; old
  rows stay `None` (which serde writes as absent).
