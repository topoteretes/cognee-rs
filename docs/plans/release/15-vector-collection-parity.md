# 15 — Vector collection parity

> Wave 3 · P1 should-fix · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: — · Source: audit [B3.2](../cleanup-and-parity-audit.md#b3-search),
> [B4.1](../cleanup-and-parity-audit.md#b4-memory-recall--remember--improve--memify)

Back to the [master index](00-INDEX.md). Downstream: task 16 ([graph extraction
parity](16-graph-extraction-parity.md)) depends on this one and reuses the
"drive off `index_fields`" idea introduced here.

## Goal

Make the Rust default graph-completion search consider **every indexed vector
collection** — most importantly `Triplet_text`, which `memify` populates — instead
of a hardcoded list of 5 collections, and make `memify`'s triplet node-text use
the same `index_fields`-derived text Python uses (e.g. `Entity → "name"` only, not
`"name: description"`). After this task, a graph search run on a Rust store after
`memify` uses triplet vectors exactly like Python does, and triplet embedding
inputs are byte-identical across SDKs.

## Background & why

Two related divergences in how Rust treats DataPoint vector collections.

### (1) Brute-force search omits `Triplet_text` (and others) — audit B3.2

Python's `GraphCompletionRetriever` enumerates **all** `DataPoint` subclass vector
collections at query time by reading each subclass's
`metadata["index_fields"]`, producing collection names `{ClassName}_{field}`
(e.g. `Triplet_text`, `Entity_name`, `TextSummary_text`, `EntityType_name`,
`DocumentChunk_text`, `EdgeType_relationship_name`). It then vector-searches each
collection for the query and maps matched point IDs onto graph nodes.

Rust hardcodes **5** collections and *explicitly excludes* `Triplet_text`. So
after `memify` has indexed triplet vectors, Python's graph search benefits from
them but Rust never does — different ranked context → different LLM answer.

`Triplet` declares `index_fields=["text"]`
(`/tmp/cognee-python/cognee/modules/engine/models/Triplet.py:9`).

### (2) memify triplet node-text differs — audit B4.1

When `memify` builds the embeddable `Triplet.text` for each edge, Python derives
each endpoint's text **from that node type's `index_fields`** (so an `Entity`
contributes only its `name`, e.g. `"Alice"`). Rust always concatenates
`name + ": " + description` (e.g. `"Alice: engineer"`). Same edge → different
embedding input → triplet vectors are **not** cross-SDK comparable, even though
the collection name (`Triplet_text`) and separator (`-›`, `\u{203a}`) already
match.

> **Sacred parity (do not touch):** the collection-name format `{Type}_{field}`
> (`crates/vector/src/qdrant_adapter.rs:111` → `format!("{}_{}", data_type,
> field_name)`), the `Triplet_text` collection name, and the `-›` triplet
> separator (`crates/cognify/src/memify/extract_triplets.rs:73`) are already
> byte-compatible with Python. Only *which* collections are searched and *what
> text* is embedded change.

## Prerequisites

```bash
git checkout -b task/15-vector-collection-parity
```

Read first (both sides):

| Side | File | What to look at |
|---|---|---|
| Rust | `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` | `DEFAULT_TRIPLET_DISTANCE_PENALTY` (~16), `SEARCH_COLLECTIONS` (~24-30), the `for (data_type, field_name) in SEARCH_COLLECTIONS` loop (~133) |
| Rust | `crates/cognify/src/memify/extract_triplets.rs` | `build_node_text` (~117-128), `extract_triplets_from_graph_db` text build (~59-73) |
| Rust | `crates/vector/src/qdrant_adapter.rs` | `collection_name` (~108-113), `has_collection` (~240) |
| Python | `cognee/modules/retrieval/graph_completion_retriever.py` | `_get_vector_index_collections` (88-99), `get_retrieved_objects` (101-136), `get_triplets` (154+, line 172 `collections = self._get_vector_index_collections()`) |
| Python | `cognee/modules/engine/models/Triplet.py` | `metadata = {"index_fields": ["text"]}` (line 9) |
| Python | `cognee/tasks/memify/get_triplet_datapoints.py` | `_build_datapoint_type_index_mapping` (13-41), `_extract_embeddable_text` (44-69), `_process_single_triplet` (99-166) |

## Files to change

| Path | Change |
|---|---|
| `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` | Replace the hardcoded `SEARCH_COLLECTIONS` with a list that includes `("Triplet", "text")` (minimum), and prefer dynamic enumeration of existing collections; fix the stale doc comment |
| `crates/cognify/src/memify/extract_triplets.rs` | Replace `build_node_text` (name+description concat) with `index_fields`-driven text (Entity/EntityType/etc → `name`; DocumentChunk/TextSummary/Triplet → `text`) |
| `crates/vector/src/lib.rs` (or wherever `VectorDB` trait lives) | (If choosing dynamic enumeration) add a `list_collections()` method to the `VectorDB` trait + impls (`QdrantAdapter`, `MockVectorDB`) |

## Python reference

| Behavior | Python file:line |
|---|---|
| Enumerate all DataPoint index collections | `cognee/modules/retrieval/graph_completion_retriever.py:88-99` |
| Collection name = `f"{subclass.__name__}_{field_name}"` | same, line 98 |
| Triplet has `index_fields=["text"]` | `cognee/modules/engine/models/Triplet.py:9` |
| Build type→index_fields map for memify | `cognee/tasks/memify/get_triplet_datapoints.py:13-41` |
| Node text = join of `index_fields` values (Entity → name only) | `get_triplet_datapoints.py:44-69, 148-149` |
| Triplet text format `f"{start}-›{rel}-›{end}".strip()` | `get_triplet_datapoints.py:157` |

`_extract_embeddable_text` (verbatim behavior to replicate):

```python
def _extract_embeddable_text(node_or_edge, index_fields):
    if not node_or_edge or not index_fields:
        return ""
    embeddable_values = []
    for field_name in index_fields:
        field_value = node_or_edge.get(field_name)
        if field_value is not None:
            field_value = str(field_value).strip()
            if field_value:
                embeddable_values.append(field_value)
    return " ".join(embeddable_values) if embeddable_values else ""
```

The type→index_fields map for the node types Rust produces in cognify:

| Node type | `index_fields` |
|---|---|
| `Entity` | `["name"]` |
| `EntityType` | `["name"]` |
| `DocumentChunk` | `["text"]` |
| `TextSummary` | `["text"]` |
| `Triplet` | `["text"]` |
| `TextDocument` (and Document subtypes) | `["name"]` |

## Implementation steps

### Part A — search enumerates `Triplet_text` (B3.2)

1. **Add `list_collections` to the `VectorDB` trait** (preferred, fully dynamic).
   Open the `VectorDB` trait definition (grep: `rg "trait VectorDB" crates/vector/src`).
   Add:

   ```rust
   /// Return all existing collection names as (data_type, field_name) pairs.
   /// Names follow the `{data_type}_{field_name}` convention. Cross-SDK callers
   /// must rely on the same split logic Python uses (`ClassName_field`).
   async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>>;
   ```

   - In `QdrantAdapter`: the adapter already discovers shard directories by name
     (`qdrant_adapter.rs:87` reads `path.file_name()`). Implement `list_collections`
     by listing the shard map / `data_dir` entries and splitting each name on the
     **first** `_` is WRONG — types like `relationship_name` contain `_`. Instead,
     split on the **last** `_`? No — `EdgeType_relationship_name` must split into
     `("EdgeType","relationship_name")`, and `DocumentChunk_text` into
     `("DocumentChunk","text")`. The reliable rule: collections are created via
     `collection_name(data_type, field_name)` and the `data_type` is always a
     CamelCase class name with **no** underscore, while `field_name` may contain
     underscores. So split on the **first** `_`. Confirm there is no
     underscore-containing data_type in the codebase (there is not:
     `DocumentChunk`, `Entity`, `EntityType`, `TextSummary`, `Triplet`, `EdgeType`,
     `Event`, `TextDocument`). Document this invariant in a comment.
   - In `MockVectorDB`: track created collections and return them.

   > If adding a trait method is judged too invasive for a 0.5d task, fall back to
   > the **minimum fix** in step 2 (static list including `Triplet_text`). The
   > dynamic approach is the parity-correct end state and is preferred.

2. **Rewrite `SEARCH_COLLECTIONS` usage** in `brute_force_triplet_search.rs`.

   Current (lines ~18-30):

   ```rust
   /// Note: "Entity_description" and "Triplet_text" are intentionally excluded here
   /// because they don't match the default Python collection set used in brute_force_triplet_search.
   /// The "EdgeType_relationship_name" collection provides per-relationship-name distances.
   const SEARCH_COLLECTIONS: [(&str, &str); 5] = [
       ("Entity", "name"),
       ("TextSummary", "text"),
       ("EntityType", "name"), // matches Python default collection list
       ("DocumentChunk", "text"),
       ("EdgeType", "relationship_name"),
   ];
   ```

   Replace the **doc comment** (the "intentionally excluded" claim is false vs
   Python) and the search loop. If you implemented `list_collections` (step 1),
   build the collection set at runtime:

   ```rust
   // Python enumerates ALL DataPoint subclass index collections at query time
   // (graph_completion_retriever.py:88-99), including Triplet_text after memify.
   // We enumerate existing collections dynamically to match.
   let collections = vector_db.list_collections().await?;
   ```

   Then iterate `for (data_type, field_name) in &collections`. The existing branch
   that special-cases `EdgeType` / `relationship_name` (lines ~152-167) stays as-is
   — it keys edge distances by `relationship_name`. **All other collections**
   (now including `Triplet` / `text`) flow through the node-distance branch keyed
   by point ID. Confirm that triplet vector point IDs equal the graph node IDs they
   should match: Python's `Triplet` points are NOT graph nodes — their IDs are
   `generate_node_id(start+rel+end)`. So matching by point-ID against graph node
   IDs will not directly hit a node. **Mirror Python exactly:** Python adds these
   distances to the memory fragment by node id too; verify in
   `get_memory_fragment` how triplet collection hits are consumed. If the Rust
   scoring model cannot use Triplet hits the same way, document the limitation in
   the code and the [index](00-INDEX.md) status note rather than silently dropping
   them. (At minimum, including the collection in the search set removes the false
   "intentionally excluded" claim and matches Python's enumeration surface.)

   **Minimum fix (fallback):** keep the static array but add `("Triplet", "text")`
   and delete the false comment:

   ```rust
   /// Collections searched for candidate graph nodes and edge-type distances.
   /// Mirrors Python's enumeration of all DataPoint index collections
   /// (graph_completion_retriever.py:88-99), including Triplet_text after memify.
   const SEARCH_COLLECTIONS: [(&str, &str); 6] = [
       ("Entity", "name"),
       ("TextSummary", "text"),
       ("EntityType", "name"),
       ("DocumentChunk", "text"),
       ("EdgeType", "relationship_name"),
       ("Triplet", "text"),
   ];
   ```

   The existing per-collection `has_collection` guard (lines ~133-137) already
   skips collections that don't exist, so adding `Triplet_text` is safe when memify
   hasn't run.

### Part B — memify node-text uses `index_fields` (B4.1)

3. **Replace `build_node_text`** in `crates/cognify/src/memify/extract_triplets.rs`.

   Current (lines ~111-128):

   ```rust
   /// Build embeddable text from a graph node's properties.
   /// Uses "name" and "description" fields...
   fn build_node_text(node: &NodeData) -> String {
       let name = extract_string_prop(node, "name");
       let description = extract_string_prop(node, "description");
       if !description.is_empty() {
           format!("{name}: {description}")
       } else {
           name
       }
       .trim()
       .to_string()
   }
   ```

   Replace with an `index_fields`-driven version that mirrors Python's
   `_extract_embeddable_text`. Read the node's `type` property and look up its
   index fields:

   ```rust
   /// Map a DataPoint type name to its `index_fields`, mirroring Python's
   /// `_build_datapoint_type_index_mapping` (get_triplet_datapoints.py:13-41).
   /// Cross-SDK triplet vectors require the SAME embeddable text on both sides.
   fn index_fields_for_type(node_type: &str) -> &'static [&'static str] {
       match node_type {
           "Entity" | "EntityType" | "TextDocument" => &["name"],
           "DocumentChunk" | "TextSummary" | "Triplet" => &["text"],
           _ => &[], // unknown type → empty text (Python skips, see step note)
       }
   }

   /// Concatenate the node's `index_fields` values with a single space,
   /// trimming each, dropping empties. Mirrors `_extract_embeddable_text`.
   fn build_node_text(node: &NodeData) -> String {
       let node_type = extract_string_prop(node, "type");
       let fields = index_fields_for_type(&node_type);
       let values: Vec<String> = fields
           .iter()
           .filter_map(|f| {
               let v = extract_string_prop(node, f);
               if v.is_empty() { None } else { Some(v) }
           })
           .collect();
       values.join(" ")
   }
   ```

   - Confirm the property carrying the node type is `"type"` — the cognify graph
     writer stores it (grep: `rg '"type"' crates/cognify/src/tasks.rs`). Verify by
     reading how nodes are written; if it's a different key adjust `extract_string_prop`.
   - Python falls back to `""` for unknown types and the caller skips a triplet
     only when **all three** of start/rel/end text are empty
     (`get_triplet_datapoints.py:151-155`). The Rust caller already has the same
     guard (`extract_triplets.rs:63-66`) — leave it.

4. **Update the stale comment** above the triplet text build
   (`extract_triplets.rs:68-72`) to note that node text now comes from
   `index_fields`, not name+description.

## Verification

```bash
# Compile (search + cognify with testing mocks)
cargo check -p cognee-search -p cognee-cognify --all-targets

# Unit tests for both crates
cargo test -p cognee-search -p cognee-cognify --features testing

# Full gate before pushing
scripts/check_all.sh
```

### Tests to add

- In `crates/cognify/src/memify/extract_triplets.rs` `#[cfg(test)]` module
  (uses `MockGraphDB`): add a node with `type="Entity"`, `name="Alice"`,
  `description="engineer"`, and assert the produced `Triplet.text` contains
  `"Alice-›"` and **not** `"Alice: engineer"`. Add a `DocumentChunk` node with
  `type="DocumentChunk"`, `text="hello"` and assert its segment is `"hello"`.
- In `crates/search`, add a test (or extend an existing brute-force test) that
  creates a `MockVectorDB` with a `Triplet`/`text` collection and asserts the
  search code path queries it (e.g. via a spy/recorded-call mock, or assert
  `list_collections()` includes it). Expected: `Triplet_text` is present.

### Expected outcomes

- `cargo test` green; new memify test proves Entity text = name-only.
- `rg "intentionally excluded" crates/search` returns nothing (false comment gone).
- `rg '"Triplet", "text"' crates/search/src/graph_retrieval/brute_force_triplet_search.rs`
  (minimum fix) or a `list_collections()` call is present.

## Acceptance criteria

- [ ] Default graph search enumerates / includes `Triplet_text`.
- [ ] The false "intentionally excluded … matches Python's 3.5" style comment is removed/corrected.
- [ ] `memify` node text is built from `index_fields` (Entity → `name` only).
- [ ] Triplet `text` format (`-›` separator) and `Triplet_text` collection name unchanged.
- [ ] New tests cover Entity name-only text and `Triplet_text` enumeration.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **DO NOT** change the `{Type}_{field}` collection-name format
  (`qdrant_adapter.rs:111`) or the `-›` (`\u{203a}`) triplet separator — both are
  already cross-SDK-compatible. Changing either silently invalidates every stored
  vector.
- **Cross-SDK determinism:** memify triplet text is an **embedding input**.
  Changing it changes the stored vectors, so a Rust-written `Triplet_text`
  collection only matches a Python one if the text is identical. The `index_fields`
  rule is the whole point — do not "improve" it (no extra fields, no different
  join character; Python uses a single ASCII space).
- **Data-type split rule:** when implementing `list_collections`, split collection
  names on the **first** `_` (data_type has no underscore; field may, e.g.
  `relationship_name`). Add a comment recording this invariant.
- **Triplet point IDs are not graph node IDs** (`generate_node_id(start+rel+end)`).
  Verify how Python consumes Triplet collection hits before assuming Rust's node-ID
  keyed scoring will use them; if it cannot, document the limitation rather than
  fabricating a mapping.
- Keep the per-collection `has_collection` guard so stores without `Triplet_text`
  (no memify yet) still work.

## Rollback

Revert the branch. The changes are additive (extra collection in the search set,
different embedding text in memify). No schema or migration changes; existing
stores remain readable. Note that vectors written by the new memify text will
differ from old ones — re-run `memify` to regenerate if mixing.
