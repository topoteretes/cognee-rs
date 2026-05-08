# Task 05-08 — Full DataPoint dump in vector-store payloads

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**:
- [Task 05-01 — `source_content_hash` field](01-source-content-hash-field.md) (the helper serialises the field).

**Blocks**:
- [Task 05-10 — Tests](10-tests.md) (cross-SDK parity test reads vector payloads).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #5 — full DataPoint dump (Python parity), not a 5-key minimal patch.

---

## 1. Goal

Bring the vector-store payload shape into parity with Python's
LanceDB / Qdrant payloads. Today, Rust constructs payloads field-by-field
via `with_metadata("type", …) / with_metadata("name", …)` calls — only
context-specific keys make it in. Python serialises the entire pydantic
DataPoint into the payload, which means consumers see all five
`source_*` fields, `created_at`, `metadata`, `belongs_to_set`,
`feedback_weight`, `version`, `topological_rank`, etc.

This task adds a single helper:

```rust
// crates/models/src/data_point.rs

impl DataPoint {
    /// Canonical vector-store payload keys for this DataPoint.
    ///
    /// Mirrors Python's `DataPoint.model_dump()` payload shape: every
    /// pydantic-equivalent field flows into the metadata map. Keys
    /// with `None` values are omitted (consistent with the
    /// `skip_serializing_if` annotations on the struct).
    ///
    /// Used by the cognify pipeline when constructing `VectorPoint`
    /// payloads to ensure the Rust shape is byte-comparable to
    /// Python's for the cross-SDK parity tests.
    pub fn vector_metadata(&self) -> HashMap<String, serde_json::Value> {
        // Round-trip via serde so the field set stays in lock-step
        // with the struct definition. If a future field is added with
        // `skip_serializing_if = "Option::is_none"`, it's reflected here
        // automatically.
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) => map.into_iter().collect(),
            _ => HashMap::new(),
        }
    }
}
```

Apply at every `VectorPoint::new(...)` site in the cognify and memify
pipelines that originates from a DataPoint. Existing per-call
`with_metadata("field", …)` / `with_metadata("dataset_id", …)` /
`with_metadata("document_id", …)` calls **stay** — they add
context-specific keys (`field`, `dataset_id`, `document_id`,
`chunk_index`, `entity_type`, `text`) that are not on the DataPoint
itself.

The merge order is "DataPoint dump first, then context overrides":
context keys with the same name as a DP field win. In practice no DP
field overlaps with the context keys (verified by §4.6).

## 2. Rationale

- **Python parity** — Python's `DataPoint(**model_dump())` ends up in
  LanceDB's metadata column verbatim. The cross-SDK parity test in
  [05-10](10-tests.md) compares JSON dumps; mismatched shapes fail.
- **Free with serde** — using `serde_json::to_value` keeps the helper
  in lock-step with the struct definition. Adding a new DataPoint
  field automatically flows through the helper.
- **`skip_serializing_if` handles absence** — the existing field
  attributes ensure `None` values do not produce empty keys, so
  payloads stay compact for unstamped legacy nodes.

## 3. Pre-conditions

- [Task 05-01](01-source-content-hash-field.md) is committed.
- Clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Add the `vector_metadata()` method

Append to
[`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs)
inside the existing `impl DataPoint` block (near `to_json` /
`get_embeddable_data`):

```rust
/// Canonical vector-store payload keys for this DataPoint.
/// Mirrors Python's `DataPoint.model_dump()` shape.
pub fn vector_metadata(&self) -> HashMap<String, serde_json::Value> {
    match serde_json::to_value(self) {
        Ok(serde_json::Value::Object(map)) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}
```

`HashMap` is already in scope at the top of the file.

### 4.2 Add a unit test

Append to the inline `tests` module:

```rust
#[test]
fn vector_metadata_includes_all_set_source_fields() {
    let mut dp = DataPoint::new("Entity", None);
    dp.source_pipeline = Some("cognify_pipeline".into());
    dp.source_task = Some("classify_documents".into());
    dp.source_user = Some("alice@example.com".into());
    dp.source_node_set = Some("entity_nodes".into());
    dp.source_content_hash = Some("md5:abcdef".into());

    let m = dp.vector_metadata();
    assert_eq!(m.get("source_pipeline").unwrap(), &json!("cognify_pipeline"));
    assert_eq!(m.get("source_task").unwrap(), &json!("classify_documents"));
    assert_eq!(m.get("source_user").unwrap(), &json!("alice@example.com"));
    assert_eq!(m.get("source_node_set").unwrap(), &json!("entity_nodes"));
    assert_eq!(m.get("source_content_hash").unwrap(), &json!("md5:abcdef"));
    assert_eq!(m.get("type").unwrap(), &json!("Entity"));
    assert_eq!(m.get("version").unwrap(), &json!(1));
    assert!(m.contains_key("created_at"));
    assert!(m.contains_key("updated_at"));
}

#[test]
fn vector_metadata_omits_none_source_fields() {
    let dp = DataPoint::new("Entity", None);
    let m = dp.vector_metadata();
    assert!(!m.contains_key("source_pipeline"));
    assert!(!m.contains_key("source_task"));
    assert!(!m.contains_key("source_user"));
    assert!(!m.contains_key("source_node_set"));
    assert!(!m.contains_key("source_content_hash"));
}
```

### 4.3 Replace per-call `with_metadata` blocks in cognify

[`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) has
six `VectorPoint::new(...)` sites for the six standard collections,
plus two temporal sites:

| Collection | DP source | Site (approx) |
|---|---|---|
| `DocumentChunk` / `text` | `chunk.base` | line 2304 |
| `Entity` / `name` | `entity.entity.base` | line 2354 |
| `EntityType` / `name` | `et.base` | line 2387+ |
| `TextSummary` / `text` | `summary.base` | search for `"TextSummary"` |
| `EdgeType` / `relationship_name` | `edge_type.base` | search for `"EdgeType"` |
| `Triplet` / `text` | (no `base` — see §4.4) | search for `"Triplet"` |
| Temporal `Event` / `name` | `event.base` | line 1231 (temporal pipeline) |

For each site, replace the construction:

```rust
// Before
let mut point = VectorPoint::new(chunk.base.id, vector)
    .with_metadata("type", json!("DocumentChunk"))
    .with_metadata("field", json!("text"))
    .with_metadata("text", json!(chunk.text.clone()))
    .with_metadata("dataset_id", json!(dataset_id.to_string()))
    .with_metadata("document_id", json!(chunk.document_id.to_string()))
    .with_metadata("chunk_index", json!(chunk.chunk_index))
    .with_metadata("belongs_to_set", json!(chunk.base.belongs_to_set));
if let Some(uid) = user_id {
    point = point.with_metadata("user_id", json!(uid.to_string()));
}
if let Some(tid) = tenant_id {
    point = point.with_metadata("tenant_id", json!(tid.to_string()));
}
```

with:

```rust
// After
let mut point = VectorPoint::new(chunk.base.id, vector);

// 1. Full DataPoint dump (matches Python parity).
for (k, v) in chunk.base.vector_metadata() {
    point = point.with_metadata(k, v);
}

// 2. Context-specific keys not on the DataPoint.
point = point
    .with_metadata("field", json!("text"))
    .with_metadata("text", json!(chunk.text.clone()))
    .with_metadata("dataset_id", json!(dataset_id.to_string()))
    .with_metadata("document_id", json!(chunk.document_id.to_string()))
    .with_metadata("chunk_index", json!(chunk.chunk_index));
if let Some(uid) = user_id {
    point = point.with_metadata("user_id", json!(uid.to_string()));
}
if let Some(tid) = tenant_id {
    point = point.with_metadata("tenant_id", json!(tid.to_string()));
}
```

Notes:

- The explicit `with_metadata("type", json!("DocumentChunk"))` line
  is **dropped** because the DP dump already carries it (the
  `data_type` field serialises as `"type"` due to the
  `#[serde(rename = "type")]` attribute on `DataPoint::data_type`).
  Verify this in §4.5.
- `belongs_to_set` is also on the DP dump, so the explicit
  `with_metadata("belongs_to_set", …)` line is dropped.
- `field` / `text` / `dataset_id` / `document_id` / `chunk_index` /
  `user_id` / `tenant_id` are **context** keys; they are not on
  `DataPoint` and stay as explicit `with_metadata` calls.

Apply the same pattern to the other six sites (Entity, EntityType,
TextSummary, EdgeType, Triplet, Event).

### 4.4 Triplet vector payload — special case

`Triplet` does not embed a `DataPoint` (per
[task 05-04 §4.4](04-has-datapoint-impls.md#44-no-impl-triplet--document-the-skip)).
It has its own `id: Uuid` and seven flat fields (`source_entity_id`,
`target_entity_id`, `relationship_name`, `text`, etc.).

For triplets, **do not** call `vector_metadata()` (there is no DP).
Instead, keep the existing per-field `with_metadata` calls — which
already mirror the Python TripletDataPoint shape — and add the four
`source_*` keys via the originating edge:

```rust
// Triplets land in cognify's add_data_points by walking edges, where
// the originating EdgeType's DataPoint provenance is in scope. Stamp
// the triplet payload with the EdgeType's source_* values for cross-
// SDK parity:
for (k, v) in edge_type.base.vector_metadata() {
    if matches!(k.as_str(), "source_pipeline" | "source_task"
        | "source_user" | "source_node_set" | "source_content_hash")
    {
        point = point.with_metadata(k, v);
    }
}
```

This is a narrower override than the full dump because Triplet's own
fields would otherwise collide with the EdgeType's. Sub-agent A flags
this as a §4.4 special-case in the review.

### 4.5 Verify that `data_type` round-trips as `"type"` in `vector_metadata`

The `DataPoint::data_type` field has `#[serde(rename = "type")]`. In the
serde-JSON round-trip, `vector_metadata()` will produce a `"type"` key.
Confirm in the unit test from §4.2:

```rust
assert_eq!(m.get("type").unwrap(), &json!("Entity"));
```

If the assertion fails, the rename annotation is missing or there's a
serde feature flag interaction; investigate before proceeding.

### 4.6 Verify no key collisions

Run a grep over the existing `with_metadata("…")` calls in cognify and
confirm none of them name a field that also appears on `DataPoint`:

```bash
grep -E 'with_metadata\("(id|created_at|updated_at|ontology_valid|version|topological_rank|metadata|type|belongs_to_set|source_pipeline|source_task|source_node_set|source_user|source_content_hash|feedback_weight)"' \
  crates/cognify/src/tasks.rs
```

Hits indicate a context override that the Python parity tests will
need to accommodate — flag any unexpected match for the user.

### 4.7 Update the `points` build for memify

[`crates/cognify/src/memify.rs`](../../crates/cognify/src/memify.rs)
(or wherever the memify pipeline indexes triplets) has its own vector
construction. Apply the §4.4 special-case there too.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. New unit tests pass.
cargo test -p cognee-models vector_metadata

# 3. Existing cognify tests still pass — payload assertions may need
#    updating if they hard-code the old shape.
cargo test -p cognee-cognify

# 4. Cross-SDK parity test (lands in 05-10) compares Python and
#    Rust payloads byte-for-byte after this change.

# 5. Clippy.
cargo clippy --all-targets -- -D warnings

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs)
  — `vector_metadata()` method + two unit tests.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) —
  six `VectorPoint::new(…)` sites + two temporal sites updated to the
  "full dump + context extras" pattern. Triplet site applies the
  §4.4 special-case.
- [`crates/cognify/src/memify.rs`](../../crates/cognify/src/memify.rs)
  (or its triplet-indexing site) — same update.
- (Conditional) any cognify integration test under
  [`crates/cognify/tests/`](../../crates/cognify/tests/) that asserts
  the old payload shape — update assertions.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| The `vector_metadata` round-trip allocates a `serde_json::Map` per call, increasing per-point cost | Low — one allocation per chunk/entity/etc., negligible vs. embedding cost | Acceptable. If hot, switch to a manual field-by-field constructor; not now. |
| A consumer (search retriever, visualisation) relied on a key the old payload **lacked** and is confused by its presence | Very low — adding keys is additive; consumers ignore unknown keys | Cross-SDK parity test catches mismatches at boundary. |
| Existing search retriever payloads cached on disk (graph-store node JSON, vector-store metadata) become inconsistent over time | None — payloads are written fresh each cognify run | n/a |
| `version`, `feedback_weight`, `topological_rank` defaults differ between Python and Rust | Medium | Documented in [task 05-10](10-tests.md). Parity test asserts on **set** values, not defaults. |

## 8. Out of scope

- Adding metadata fields beyond what serde produces from `DataPoint`.
  The "five `source_*` keys" minimum is locked decision 5; we ship
  the full Python-equivalent dump and stop there.
- Backfilling `source_*` keys on legacy vector entries. Reindexing is
  the only path; not in scope.
- Migrating `Triplet` to embed a `DataPoint`. Separate refactor.
