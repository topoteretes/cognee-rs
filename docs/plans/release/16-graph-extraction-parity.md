# 16 — Graph extraction parity (Edge.description + Document nodes/collections)

> Wave 3 · P1 should-fix · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: [15](15-vector-collection-parity.md) · Source: audit
> [B2.5](../cleanup-and-parity-audit.md#b2-cognify),
> [B2.6](../cleanup-and-parity-audit.md#b2-cognify)

Back to the [master index](00-INDEX.md). Builds on task
[15](15-vector-collection-parity.md): reuse the "drive indexing off `index_fields`
rather than a hardcoded list" idea introduced there.

## Goal

Bring Rust cognify's graph-extraction output structurally in line with Python by
(1) adding the `description` field to the extraction `Edge` model and threading it
through to the stored graph edge property `edge_text` and into the edge-type /
triplet embedding inputs, and (2) storing `Document` nodes in the graph and
indexing them into `TextDocument_name` (and the other `*Document_name`
collections), ideally by driving indexing off each DataPoint's `index_fields`
rather than the current hardcoded 6-collection list. After this task, a Python
search against `TextDocument_name` finds the same documents on a Rust store, and
edges carry concrete fact text.

## Background & why

### (1) `Edge` schema missing `description` — audit B2.5

Python's `KnowledgeGraph.Edge` has a `description` field — a "concrete
one-sentence fact expressed by this edge, using endpoint names". During graph
expansion it becomes the edge property `edge_text`
(`expand_with_nodes_and_edges.py:294,307`), which then feeds:

- **EdgeType embeddings** — `index_graph_edges.py:33-53` builds `EdgeType`
  datapoints from `edge_text` (falling back to `relationship_name`), and embeds
  `relationship_name` (which is set to the edge text).
- **Triplet embeddings** — memify reads `edge_text` for the relationship segment
  (`get_triplet_datapoints.py:90-96`).

Rust's `Edge` (`crates/cognify/src/fact_extraction/models.rs:76-86`) has only
`source_node_id`, `target_node_id`, `relationship_name` — **no `description`**. So
Rust edges carry no fact text; `edge_text` is empty and edge-type / triplet
embeddings degrade to bare relationship names. This changes the LLM's structured
output (the prompt no longer asks for a description — see task
[13](13-prompt-parity-sync.md) for the prompt half) and the stored edge payload.

> Note: the Rust memify code **already reads** `edge_text` from edge props
> (`extract_triplets.rs:134-145`) and graph-edge writing already accepts an
> `edge_text` property — it's just never populated because extraction drops it.

### (2) Missing `TextDocument_name` collection + Document graph nodes — audit B2.6

Python stores each classified `Document` as a graph node and indexes it by its
`index_fields=["name"]`, producing collections named by the **concrete subclass**:
`TextDocument_name`, `PdfDocument_name`, `CsvDocument_name`, `ImageDocument_name`,
`AudioDocument_name`, `UnstructuredDocument_name`, `DltRowDocument_name`. Documents
are linked to their chunks via the `is_part_of` edge (chunk → document).

Rust:
- **Never stores Document nodes** in the graph (`add_data_points`, `tasks.rs:937-975`
  stores chunks, summaries, entity-types, but not documents).
- **Hardcodes 6 `index_points` calls** in `index_data_points` (`tasks.rs:2562-2947`):
  `DocumentChunk_text`, `Entity_name`, `EntityType_name`, `TextSummary_text`,
  `Triplet_text`, `EdgeType_relationship_name` — no `*Document_name`.

So a Python search against `TextDocument_name` finds **nothing** on a Rust store,
and the graph is missing Document nodes (the `is_part_of` edge points at a node
that doesn't exist as a stored Document).

> **CRITICAL — preserve the collection-name format.** `{Type}_{field}`
> (`crates/vector/src/qdrant_adapter.rs:111`) already matches Python. Adding new
> collections (`TextDocument_name`, …) is **additive** and must use the exact
> Python subclass class-name (`TextDocument`, not `Document` or `text`). Adding
> collections does not break existing cross-SDK reads; mis-naming them does.

## Prerequisites

```bash
git checkout -b task/16-graph-extraction-parity
```

Land task [15](15-vector-collection-parity.md) first (dependency). Read both sides:

| Side | File | What to look at |
|---|---|---|
| Rust | `crates/cognify/src/fact_extraction/models.rs` | `Edge` struct (76-86), tests (165-173) |
| Rust | `crates/cognify/src/tasks.rs` | `add_data_points` graph-node storage (937-975), edge-prop writer (~1520-1545, `relationship_name`), `index_data_points` 6 hardcoded blocks (2562-2947) |
| Rust | `crates/cognify/src/graph_extraction/extractable.rs` | `GraphExtractable` impls; `Document` is absent |
| Rust | `crates/cognify/src/memify/extract_triplets.rs` | `extract_relationship_text` (134-145) already reads `edge_text` |
| Python | `cognee/shared/data_models.py` | `Edge.description` (62-71) |
| Python | `cognee/modules/graph/utils/expand_with_nodes_and_edges.py` | `_process_graph_edges` (281-311): `edge_text = _strip_nonblank_text(edge.description)` (294), stored in props (307) |
| Python | `cognee/tasks/storage/index_graph_edges.py` | `_get_edge_text` / `create_edge_type_datapoints` (33-53) |
| Python | `cognee/tasks/storage/index_data_points.py` | `index_data_points` (10-70): iterates `metadata["index_fields"]` |
| Python | `cognee/modules/data/processing/document_types/Document.py` | `index_fields=["name"]` (11); subclasses `TextDocument`, `PdfDocument`, … each inherit it |

## Files to change

| Path | Change |
|---|---|
| `crates/cognify/src/fact_extraction/models.rs` | Add `description: Option<String>` to `Edge` with the Python field doc; update `JsonSchema`/tests |
| `crates/cognify/src/fact_extraction/extractor.rs` | (prompt half is task 13) ensure parsed `description` survives into the expanded edge |
| `crates/cognify/src/tasks.rs` | Populate `edge_text` edge property from `Edge.description`; store `Document` graph nodes in `add_data_points`; index `*Document_name` collections (prefer `index_fields`-driven loop over hardcoded blocks) |
| `crates/cognify/src/graph_extraction/extractable.rs` | Add a `GraphExtractable` impl for `Document` (or include documents in the node set passed to `add_nodes`) |

## Python reference

| Behavior | Python file:line |
|---|---|
| `Edge.description` field + its doc string | `cognee/shared/data_models.py:62-71` |
| `edge_text = _strip_nonblank_text(edge.description)` | `cognee/modules/graph/utils/expand_with_nodes_and_edges.py:294` |
| `edge_text` written into edge props | same, 307 |
| EdgeType text from `edge_text` (fallback rel name) | `cognee/tasks/storage/index_graph_edges.py:33-53` |
| Index driven by `metadata["index_fields"]` | `cognee/tasks/storage/index_data_points.py:39-52` |
| `Document.index_fields=["name"]`; subclasses inherit | `cognee/modules/data/processing/document_types/Document.py:11` + `TextDocument.py` etc. |
| Collection name = `{type_name}_{field_name}` | `index_data_points.py:46-47` (`create_vector_index(type_name, field_name)`) |

## Implementation steps

### Part A — `Edge.description` → `edge_text` (B2.5)

1. **Add the field** to `Edge` in `fact_extraction/models.rs`. Current:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
   pub struct Edge {
       pub source_node_id: String,
       pub target_node_id: String,
       pub relationship_name: String,
   }
   ```

   New:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
   pub struct Edge {
       pub source_node_id: String,
       pub target_node_id: String,
       pub relationship_name: String,
       /// Concrete one-sentence fact expressed by this edge, using endpoint names.
       /// Mirrors Python `KnowledgeGraph.Edge.description` (data_models.py:68-71).
       /// Becomes the `edge_text` graph-edge property, feeding EdgeType + Triplet
       /// embeddings. Optional because older/custom outputs may omit it.
       #[serde(default)]
       pub description: Option<String>,
   }
   ```

   Update the existing `test_edge_creation` / `test_knowledge_graph` constructors
   (they build `Edge { … }` literally) to add `description: None`.

   > The prompt that instructs the LLM to emit `description` is owned by task
   > [13](13-prompt-parity-sync.md). This task makes the *schema* accept it and
   > *thread it through*; if the prompt isn't synced yet, `description` will be
   > `None` and behavior is unchanged — safe to land independently.

2. **Thread `description` into the stored edge property.** Find where Rust builds
   graph edges from extracted `Edge`s and sets `relationship_name`
   (`tasks.rs`, grep `rg 'Cow::Borrowed\("relationship_name"\)' crates/cognify/src/tasks.rs`
   — first hit ~1535). Where the edge property map is constructed, add an
   `edge_text` entry, mirroring Python's `_process_graph_edges` (which writes
   `relationship_name`, `source_node_id`, `target_node_id`, `ontology_valid`,
   `edge_text`). Use Python's `_strip_nonblank_text` semantics — store the trimmed
   description, or empty string when absent/blank:

   ```rust
   let edge_text = edge
       .description
       .as_deref()
       .map(str::trim)
       .filter(|s| !s.is_empty())
       .unwrap_or("")
       .to_string();
   // ... in the property HashMap:
   props.insert(Cow::Borrowed("edge_text"), json!(edge_text));
   ```

   Confirm the exact construction site and the `props`/`HashMap` variable name by
   reading the surrounding code (the edge expansion that produces
   `GraphEdgePair` / the props written by `add_edges`). The downstream EdgeType and
   memify code already consume `edge_text`, so no change is needed there.

3. **Verify EdgeType text uses `edge_text` when present.** Read how Rust builds
   `EdgeType` relationship text (`tasks.rs` ~977-1000 collects relationship names).
   Python builds EdgeType text from `edge_text` (falling back to
   `relationship_name`) — `index_graph_edges.py:33-53`. If Rust currently keys
   EdgeType solely on `relationship_name`, decide whether to match Python's
   `edge_text`-first behavior. **Be careful:** this changes the `EdgeType` node IDs
   and `EdgeType_relationship_name` embedding inputs (Python uses
   `generate_edge_id(edge_id=text)`), which affects cross-SDK parity. If matching,
   mirror Python exactly; if deferring, document it as a known gap in the
   [index](00-INDEX.md) and keep `relationship_name` keying. Prefer mirroring,
   since the audit's B2.5 explicitly calls this out.

### Part B — Document nodes + `*Document_name` collections (B2.6)

4. **Store Document nodes in the graph.** In `add_data_points`
   (`tasks.rs:937-975`), after storing chunks/summaries/entity-types, add the
   Documents from `input.documents` as graph nodes:

   ```rust
   // Store Documents as graph nodes (Python stores classified Documents and
   // links chunks via is_part_of). Mirrors add_data_points in Python.
   if !input.documents.is_empty() {
       let doc_refs: Vec<&Document> = input.documents.iter().collect();
       graph_db.add_nodes(&doc_refs).await.map_err(CognifyError::from)?;
       info!("Stored {} documents as graph nodes", doc_refs.len());
   }
   ```

   This requires `Document` to be addable via `add_nodes`. Check the `add_nodes`
   bound (grep `rg "fn add_nodes" crates/graph/src`). If it needs a
   `GraphExtractable`/DataPoint-like trait, add an impl for `Document` in
   `graph_extraction/extractable.rs`. `Document` has **no outgoing DataPoint
   relationships** of its own (the `is_part_of` edge originates from the chunk and
   already exists), so its `relationships()` returns `Vec::new()`:

   ```rust
   impl GraphExtractable for Document {
       fn data_point_id(&self) -> Uuid { self.base.id }
       fn data_point_type(&self) -> &str { &self.base.data_type }
       fn relationships(&self) -> Vec<Relationship> { Vec::new() }
   }
   ```

   Confirm `Document.base.data_type` carries the concrete subclass name Python
   uses (`TextDocument`, `PdfDocument`, …). Read how `classify_documents` sets the
   document/data_type discriminator (`tasks.rs:157-178` + `document_classifier`).
   The graph node `type` property and the vector collection name **must** be the
   concrete Python class name for cross-SDK reads.

5. **Index `*Document_name` collections — prefer the `index_fields`-driven loop.**
   The current `index_data_points` (`tasks.rs:2562-2947`) has 6 copy-pasted blocks.
   Rather than adding a 7th hardcoded block, introduce a small generic helper that,
   given a slice of items each exposing `(type_name, field_name, point_id,
   embeddable_text, metadata)`, embeds and indexes them — then call it for every
   `(type, field)` pair derived from `index_fields`. This mirrors Python's
   `index_data_points` (`index_data_points.py:30-68`).

   Minimum viable version (additive, lower risk): add one block for Documents,
   keyed by the concrete document type:

   ```rust
   // Index Documents by name into {ConcreteType}_name (e.g. TextDocument_name).
   // Python indexes every Document subclass via its index_fields=["name"].
   if !input.documents.is_empty() {
       // Group by concrete type so collection names match Python class names.
       use std::collections::HashMap as Map;
       let mut by_type: Map<&str, Vec<&Document>> = Map::new();
       for d in &input.documents {
           by_type.entry(d.base.data_type.as_str()).or_default().push(d);
       }
       for (type_name, docs) in by_type {
           if !vector_db.has_collection(type_name, "name").await
               .map_err(|e| CognifyError::VectorDBError(e.to_string()))? {
               vector_db.create_collection(type_name, "name", dimension).await
                   .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
           }
           let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
           let vectors = engine.embed(&names).await
               .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;
           let points: Vec<VectorPoint> = docs.iter().zip(vectors).map(|(d, v)| {
               let mut p = VectorPoint::new(d.base.id, v);
               for (k, val) in d.base.vector_metadata() { p = p.with_metadata(k, val); }
               p.with_metadata("field", json!("name"))
                .with_metadata("name", json!(d.name.clone()))
                .with_metadata("dataset_id", json!(dataset_id.to_string()))
           }).collect();
           vector_db.index_points(type_name, "name", &points).await
               .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
           stats.record(type_name, "name", docs.len());
       }
   }
   ```

   Adjust the exact `Document` field accessor (`d.name`) and `vector_metadata()`
   call to match the real `Document` model (read `crates/models/src/document.rs`).
   Confirm `add_data_points` actually has `input.documents` available
   (`SummarizedData` carries `documents` — `tasks.rs:128`).

   > **The fully `index_fields`-driven refactor** (replacing all 6 blocks) is the
   > parity-correct end state and matches the audit recommendation. If time-boxed,
   > do the additive Document block now and file the refactor under task
   > [25](25-deferred-refactors.md).

## Verification

```bash
cargo check -p cognee-cognify --all-targets
cargo test -p cognee-cognify --features testing
# Edge schema JSON shape:
cargo test -p cognee-cognify edge --features testing
# End-to-end (needs OpenAI + embed model): structural cognify
bash scripts/run_tests_with_openai.sh cognify
scripts/check_all.sh
```

### Tests to add

- `models.rs`: serialize an `Edge` with `description: Some("Alice founded Acme")`
  and assert the JSON contains `"description":"Alice founded Acme"`; deserialize
  JSON **without** `description` and assert it defaults to `None` (back-compat).
- `tasks.rs` (mock graph + mock vector, `testing` feature): run `add_data_points`
  with one `TextDocument` and assert (a) the graph has a node with that document's
  ID and `type == "TextDocument"`, and (b) `MockVectorDB` has a `TextDocument_name`
  collection containing 1 point.
- `tasks.rs`: build an edge with `description = Some("…")` and assert the written
  edge property map contains `edge_text == "…"` (trimmed).

### Expected outcomes

- `Edge` round-trips with and without `description`.
- After cognify, a `TextDocument_name` collection exists and contains the document.
- The stored `is_part_of` target (Document) now exists as a graph node.

## Acceptance criteria

- [ ] `Edge` has `description: Option<String>` with `#[serde(default)]`; existing tests updated.
- [ ] Extracted `description` is written to the `edge_text` edge property (trimmed; empty when absent).
- [ ] `Document`s are stored as graph nodes with the concrete subclass `type`.
- [ ] `*Document_name` collections (`TextDocument_name`, …) are created and populated using the concrete Python class name.
- [ ] Collection-name format `{Type}_{field}` unchanged.
- [ ] New tests cover Edge schema back-compat, Document node storage, `TextDocument_name`, and `edge_text` population.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **DO NOT** rename collections or change the `{Type}_{field}` format. New
  collections must use the exact Python **concrete subclass** name
  (`TextDocument`, `PdfDocument`, `CsvDocument`, `ImageDocument`, `AudioDocument`,
  `UnstructuredDocument`, `DltRowDocument`) — NOT `Document` and NOT the lowercase
  `document_type`. A Python search hits `TextDocument_name`; a Rust-written
  `Document_name` or `text_name` is invisible to it.
- **`edge_text` is an embedding input** (via EdgeType + Triplet). Match Python's
  trimming (`_strip_nonblank_text`: trim, treat blank as empty) exactly, or Rust
  and Python edge-type/triplet vectors diverge.
- **EdgeType ID/text change is cross-SDK-sensitive.** If you make EdgeType use
  `edge_text` (step 3), mirror Python's `generate_edge_id(edge_id=text)` precisely;
  otherwise leave EdgeType keyed on `relationship_name` and record the gap.
- **`description` is additive and back-compatible** — `#[serde(default)]` means old
  JSON (no `description`) still deserializes. Without the prompt sync (task 13) the
  LLM won't emit it and behavior is unchanged; that's intentional, not a bug.
- **Determinism:** adding Document nodes/collections does not change chunk IDs,
  content hashes, or existing collection contents — it is purely additive. Do not
  touch chunk/entity ID derivation.
- Verify `Document` exposes `vector_metadata()` / `base.id` / `name` exactly as
  used; read `crates/models/src/document.rs` before pasting the snippet.

## Rollback

Revert the branch. All changes are additive (one optional struct field with a
serde default, extra graph nodes, extra vector collections, one extra edge
property). Existing stores remain readable; existing collections are untouched.
Old Rust stores simply lack the new Document collections until re-cognified.
