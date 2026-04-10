# Task 6: Expand Node Property Projection

## Summary

The Python `brute_force_triplet_search` projects five node properties from the graph: `["id", "description", "name", "type", "text"]`. The Rust implementation currently extracts only `name` from graph nodes. This limits the expressiveness of search context: when a node has a `description`, `type`, or `text` attribute, that information is lost and never surfaces in `RankedGraphEdge`, the search context payload, or the rendered text context sent to the LLM.

This task expands the Rust `brute_force_triplet_search` to extract all five properties and propagate them through `RankedGraphEdge` into the search context payload and the `render_edges_context` (aka `resolve_edges_to_text`) text rendering function.

---

## Current Rust Behavior

### `RankedGraphEdge` struct

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, lines 37-48

```rust
#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
}
```

Only `source_name` and `target_name` are carried. No `description`, `type`, or `text`.

### Node property extraction

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, lines 128-138

```rust
let node_names: HashMap<String, String> = graph_nodes
    .into_iter()
    .map(|(node_id, properties)| {
        let name = properties
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(node_id.as_str())
            .to_string();
        (node_id, name)
    })
    .collect();
```

Only `name` is extracted. `description`, `type`, `text`, and `id` are discarded.

### Edge construction

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, lines 150-158

```rust
let source_name = node_names
    .get(&source_id)
    .cloned()
    .unwrap_or(source_id.clone());
let target_name = node_names
    .get(&target_id)
    .cloned()
    .unwrap_or(target_id.clone());
```

Only name is looked up.

### Context payload (GraphCompletionRetriever)

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`, lines 95-108

```rust
Ok(ranked_edges
    .into_iter()
    .map(|edge| SearchItem {
        id: None,
        score: Some(edge.score),
        payload: json!({
            "source_id": edge.source_id,
            "target_id": edge.target_id,
            "relationship": edge.relationship_name,
            "source_name": edge.source_name,
            "target_name": edge.target_name,
            "dataset_id": edge.dataset_id,
        }),
    })
    .collect())
```

No `description`, `type`, or `text` in payload.

### Context payload (advanced graph retrievers)

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, lines 90-104

```rust
Ok(ranked_edges
    .into_iter()
    .map(|edge| SearchItem {
        id: None,
        score: Some(edge.score),
        payload: json!({
            "source_id": edge.source_id,
            "target_id": edge.target_id,
            "relationship": edge.relationship_name,
            "source_name": edge.source_name,
            "target_name": edge.target_name,
        }),
    })
    .collect())
```

Same -- no extra properties.

### Context payload (temporal retriever)

**File:** `crates/search/src/retrievers/temporal_retriever.rs`, lines 167-182 (inside `ranked_edges_to_context`)

```rust
ranked_edges
    .into_iter()
    .map(|edge| SearchItem {
        id: None,
        score: Some(edge.score),
        payload: json!({
            "source_id": edge.source_id,
            "target_id": edge.target_id,
            "relationship": edge.relationship_name,
            "source_name": edge.source_name,
            "target_name": edge.target_name,
        }),
    })
    .collect()
```

### `resolve_edges_to_text` (renders context for LLM)

**File:** `crates/search/src/utils/resolve_edges_to_text.rs`, lines 3-42

Currently renders only: `"{source_name} -[{relationship}]-> {target_name}"` with a `"\n"` join.

---

## Required Python Behavior

### Node property projection

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py`, lines 60-61

```python
if properties_to_project is None:
    properties_to_project = ["id", "description", "name", "type", "text"]
```

All five properties are projected from graph nodes by default.

### `resolve_edges_to_text` node extraction

**File:** `/tmp/cognee-python/cognee/modules/graph/utils/resolve_edges_to_text.py`, lines 33-62

```python
def _extract_nodes_from_edges(retrieved_edges: List[Edge]) -> dict:
    nodes = {}
    for edge in retrieved_edges:
        for node in (edge.node1, edge.node2):
            if node.id in nodes:
                continue
            text = node.attributes.get("text")
            if text:
                name = _create_title_from_text(text)
                content = text
            else:
                name = node.attributes.get("name", "Unnamed Node")
                content = node.attributes.get("description", name)
            nodes[node.id] = {"node": node, "name": name, "content": content}
    return nodes
```

Key behavior:
- If a node has `text`, it becomes the content and the name is derived from the text.
- If not, `name` is the display name and `description` (falling back to `name`) is the content.

### `resolve_edges_to_text` output format

**File:** `/tmp/cognee-python/cognee/modules/graph/utils/resolve_edges_to_text.py`, lines 65-110

```python
async def resolve_edges_to_text(retrieved_edges: List[Edge]) -> str:
    nodes = _extract_nodes_from_edges(retrieved_edges)
    node_section = "\n".join(
        f"Node: {info['name']}\n__node_content_start__\n{info['content']}\n__node_content_end__\n"
        for info in nodes.values()
    )
    connections = []
    for edge in retrieved_edges:
        source_name = nodes[edge.node1.id]["name"]
        target_name = nodes[edge.node2.id]["name"]
        edge_label = edge.attributes.get("edge_text") or edge.attributes.get("relationship_type")
        connections.append(f"{source_name} --[{edge_label}]--> {target_name}")
    connection_section = "\n".join(connections)
    return f"Nodes:\n{node_section}\n\nConnections:\n{connection_section}"
```

The output includes both a **Nodes** section (with content) and a **Connections** section.

### `format_triplets` output (alternative for debugging)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py`, lines 29-46

```python
def format_triplets(edges):
    for edge in edges:
        node1_info = {key: value for key, value in node1_attributes.items() if value is not None}
        node2_info = {key: value for key, value in node2_attributes.items() if value is not None}
        edge_info = {key: value for key, value in edge_attributes.items() if value is not None}
        triplet = f"Node1: {node1_info}\nEdge: {edge_info}\nNode2: {node2_info}\n\n\n"
```

All node attributes (id, description, name, type, text) are included.

---

## Step-by-Step Changes

### Step 1: Define a `NodeProperties` struct for extracted node data

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

Add a new struct after `RankedGraphEdge`:

```rust
#[derive(Debug, Clone, Default)]
struct NodeProperties {
    name: String,
    description: Option<String>,
    node_type: Option<String>,
    text: Option<String>,
}
```

### Step 2: Expand `RankedGraphEdge` to carry additional node properties

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (lines 37-48):**
```rust
#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
}
```

**Target:**
```rust
#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    pub source_description: Option<String>,
    pub target_description: Option<String>,
    pub source_type: Option<String>,
    pub target_type: Option<String>,
    pub source_text: Option<String>,
    pub target_text: Option<String>,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
}
```

### Step 3: Extract all five properties from graph nodes

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (lines 128-138):**
```rust
let node_names: HashMap<String, String> = graph_nodes
    .into_iter()
    .map(|(node_id, properties)| {
        let name = properties
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(node_id.as_str())
            .to_string();
        (node_id, name)
    })
    .collect();
```

**Target:**
```rust
let node_props: HashMap<String, NodeProperties> = graph_nodes
    .into_iter()
    .map(|(node_id, properties)| {
        let name = properties
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(node_id.as_str())
            .to_string();
        let description = properties
            .get("description")
            .and_then(|value| value.as_str())
            .map(ToString::to_string);
        let node_type = properties
            .get("type")
            .and_then(|value| value.as_str())
            .map(ToString::to_string);
        let text = properties
            .get("text")
            .and_then(|value| value.as_str())
            .map(ToString::to_string);
        (
            node_id,
            NodeProperties {
                name,
                description,
                node_type,
                text,
            },
        )
    })
    .collect();
```

### Step 4: Update edge construction to use `NodeProperties`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (lines 150-173):**
```rust
let source_name = node_names
    .get(&source_id)
    .cloned()
    .unwrap_or(source_id.clone());
let target_name = node_names
    .get(&target_id)
    .cloned()
    .unwrap_or(target_id.clone());

let dataset_id = node_dataset_ids
    .get(&source_id)
    .or_else(|| node_dataset_ids.get(&target_id))
    .cloned();

Some(RankedGraphEdge {
    source_id,
    target_id,
    relationship_name,
    score: rank_edge_score(source_score, target_score),
    source_name,
    target_name,
    dataset_id,
})
```

**Target:**
```rust
let source_props = node_props.get(&source_id);
let target_props = node_props.get(&target_id);

let source_name = source_props
    .map(|p| p.name.clone())
    .unwrap_or_else(|| source_id.clone());
let target_name = target_props
    .map(|p| p.name.clone())
    .unwrap_or_else(|| target_id.clone());

let dataset_id = node_dataset_ids
    .get(&source_id)
    .or_else(|| node_dataset_ids.get(&target_id))
    .cloned();

Some(RankedGraphEdge {
    source_id,
    target_id,
    relationship_name,
    score: rank_edge_score(source_score, target_score),
    source_name,
    target_name,
    source_description: source_props.and_then(|p| p.description.clone()),
    target_description: target_props.and_then(|p| p.description.clone()),
    source_type: source_props.and_then(|p| p.node_type.clone()),
    target_type: target_props.and_then(|p| p.node_type.clone()),
    source_text: source_props.and_then(|p| p.text.clone()),
    target_text: target_props.and_then(|p| p.text.clone()),
    dataset_id,
})
```

### Step 5: Propagate new fields into the search context payload (GraphCompletionRetriever)

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

**Current (lines 100-107):**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
    "dataset_id": edge.dataset_id,
}),
```

**Target:**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
    "source_description": edge.source_description,
    "target_description": edge.target_description,
    "source_type": edge.source_type,
    "target_type": edge.target_type,
    "source_text": edge.source_text,
    "target_text": edge.target_text,
    "dataset_id": edge.dataset_id,
}),
```

### Step 6: Propagate new fields into the search context payload (advanced graph retrievers)

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

In `GraphRetrieverCore::get_context()` (lines 96-101):

**Current:**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
}),
```

**Target:**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
    "source_description": edge.source_description,
    "target_description": edge.target_description,
    "source_type": edge.source_type,
    "target_type": edge.target_type,
    "source_text": edge.source_text,
    "target_text": edge.target_text,
}),
```

### Step 7: Propagate new fields into the search context payload (temporal retriever)

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

In `ranked_edges_to_context()` (lines 170-180):

**Current:**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
}),
```

**Target:**
```rust
payload: json!({
    "source_id": edge.source_id,
    "target_id": edge.target_id,
    "relationship": edge.relationship_name,
    "source_name": edge.source_name,
    "target_name": edge.target_name,
    "source_description": edge.source_description,
    "target_description": edge.target_description,
    "source_type": edge.source_type,
    "target_type": edge.target_type,
    "source_text": edge.source_text,
    "target_text": edge.target_text,
}),
```

### Step 8: Enrich `resolve_edges_to_text` to include node content

**File:** `crates/search/src/utils/resolve_edges_to_text.rs`

This is the most significant change. The current function renders only connection lines. The Python version renders a **Nodes** section with content and a **Connections** section.

**Current (full file):**
```rust
use crate::types::SearchContext;

pub fn resolve_edges_to_text(context: &SearchContext) -> String {
    context
        .iter()
        .map(|item| {
            let source = item
                .payload.get("source_name").and_then(|value| value.as_str())
                .or_else(|| item.payload.get("source_id").and_then(|value| value.as_str()))
                .unwrap_or("unknown_source");
            let target = item
                .payload.get("target_name").and_then(|value| value.as_str())
                .or_else(|| item.payload.get("target_id").and_then(|value| value.as_str()))
                .unwrap_or("unknown_target");
            let relationship = item
                .payload.get("relationship").and_then(|value| value.as_str())
                .or_else(|| item.payload.get("relationship_name").and_then(|value| value.as_str()))
                .unwrap_or("related_to");

            format!("{source} -[{relationship}]-> {target}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

**Target:**
```rust
use std::collections::HashMap;

use crate::types::SearchContext;

/// Determines the display name and content for a node from its payload fields.
///
/// Follows the Python logic:
/// - If `text` is present, content = text, name = first 7 words + top frequent words.
/// - Otherwise, name = `name` (or "Unnamed Node"), content = `description` (or name).
fn resolve_node_display(
    name: Option<&str>,
    description: Option<&str>,
    text: Option<&str>,
) -> (String, String) {
    if let Some(text_value) = text {
        let display_name = create_title_from_text(text_value);
        (display_name, text_value.to_string())
    } else {
        let display_name = name.unwrap_or("Unnamed Node").to_string();
        let content = description.unwrap_or(display_name.as_str()).to_string();
        (display_name, content)
    }
}

/// Creates a title from text: first N words + "[top frequent words]".
/// Simplified port of Python's `_create_title_from_text`.
fn create_title_from_text(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let first_words: String = words.iter().take(7).copied().collect::<Vec<_>>().join(" ");

    // Count word frequencies (lowercased, stripped of punctuation)
    let mut freq = HashMap::<String, usize>::new();
    for word in &words {
        let cleaned: String = word
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        if !cleaned.is_empty() {
            *freq.entry(cleaned).or_insert(0) += 1;
        }
    }
    let mut freq_vec: Vec<_> = freq.into_iter().collect();
    freq_vec.sort_by(|a, b| b.1.cmp(&a.1));
    let top_words: String = freq_vec
        .iter()
        .take(3)
        .map(|(w, _)| w.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    if words.len() > 7 {
        format!("{first_words}... [{top_words}]")
    } else {
        format!("{first_words} [{top_words}]")
    }
}

pub fn resolve_edges_to_text(context: &SearchContext) -> String {
    // Collect unique nodes by source/target id
    let mut nodes: HashMap<String, (String, String)> = HashMap::new(); // id -> (display_name, content)
    let mut connections = Vec::new();

    for item in context {
        let source_id = item
            .payload
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_source");
        let target_id = item
            .payload
            .get("target_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_target");

        // Extract source node properties
        if !nodes.contains_key(source_id) {
            let (display_name, content) = resolve_node_display(
                item.payload
                    .get("source_name")
                    .and_then(|v| v.as_str()),
                item.payload
                    .get("source_description")
                    .and_then(|v| v.as_str()),
                item.payload
                    .get("source_text")
                    .and_then(|v| v.as_str()),
            );
            nodes.insert(source_id.to_string(), (display_name, content));
        }

        // Extract target node properties
        if !nodes.contains_key(target_id) {
            let (display_name, content) = resolve_node_display(
                item.payload
                    .get("target_name")
                    .and_then(|v| v.as_str()),
                item.payload
                    .get("target_description")
                    .and_then(|v| v.as_str()),
                item.payload
                    .get("target_text")
                    .and_then(|v| v.as_str()),
            );
            nodes.insert(target_id.to_string(), (display_name, content));
        }

        let source_display = &nodes[source_id].0;
        let target_display = &nodes[target_id].0;
        let relationship = item
            .payload
            .get("relationship")
            .and_then(|v| v.as_str())
            .or_else(|| {
                item.payload
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("related_to");

        connections.push(format!(
            "{source_display} --[{relationship}]--> {target_display}"
        ));
    }

    let node_section: String = nodes
        .values()
        .map(|(name, content)| {
            format!(
                "Node: {name}\n__node_content_start__\n{content}\n__node_content_end__"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let connection_section = connections.join("\n");

    format!("Nodes:\n{node_section}\n\nConnections:\n{connection_section}")
}
```

**Note:** The Python `resolve_edges_to_text` output format is:
```
Nodes:
Node: {name}
__node_content_start__
{content}
__node_content_end__

Connections:
{source} --[{relationship}]--> {target}
```

The Rust arrow format changes from `-[...]->` to `--[...]-->` to match Python.

### Step 9: Update tests in `graph_completion_retriever.rs`

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

The test at line 583 asserts:
```rust
assert!(messages[1].content.contains("Graph=Alice -[KNOWS]-> Bob"));
```

After the `resolve_edges_to_text` format change, the rendered text will use `--[...]-->` and include a Nodes section. Update to:
```rust
assert!(messages[1].content.contains("Alice --[KNOWS]--> Bob"));
```

The test at line 525-526 asserts payload fields:
```rust
assert_eq!(context[0].payload["source_name"], "Alice");
assert_eq!(context[0].payload["target_name"], "Bob");
```

These still pass. Optionally add assertions for new fields:
```rust
// New fields should be present (possibly null if test graph nodes lack them)
assert!(context[0].payload.get("source_description").is_some());
assert!(context[0].payload.get("target_description").is_some());
```

### Step 10: Update the test helper `node()` in `graph_completion_retriever.rs` tests

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`, line 455-458

**Current:**
```rust
fn node(id: &str, name: &str) -> GraphNode {
    let mut props = HashMap::new();
    props.insert(Cow::Borrowed("name"), json!(name));
    (id.to_string(), props)
}
```

**Target (add description for richer testing):**
```rust
fn node(id: &str, name: &str) -> GraphNode {
    let mut props = HashMap::new();
    props.insert(Cow::Borrowed("name"), json!(name));
    props.insert(Cow::Borrowed("description"), json!(format!("Description of {name}")));
    props.insert(Cow::Borrowed("type"), json!("Entity"));
    (id.to_string(), props)
}
```

---

## Test Verification

1. Run `cargo check --all-targets` to verify compilation after struct changes.
2. Run `cargo test -p cognee-search` to verify all tests pass with updated format.
3. Run `scripts/check_all.sh` for full CI validation.
4. Verify the E2E search test still passes (the output format change may require updating expected strings in integration tests).

**Suggested new test for `resolve_edges_to_text`:**

```rust
#[test]
fn renders_nodes_and_connections_sections() {
    let context = vec![
        SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({
                "source_id": "1",
                "target_id": "2",
                "source_name": "Alice",
                "target_name": "Bob",
                "source_description": "A person named Alice",
                "target_description": "A person named Bob",
                "relationship": "KNOWS",
            }),
        },
    ];

    let text = resolve_edges_to_text(&context);

    assert!(text.starts_with("Nodes:"));
    assert!(text.contains("Node: Alice"));
    assert!(text.contains("A person named Alice"));
    assert!(text.contains("Node: Bob"));
    assert!(text.contains("A person named Bob"));
    assert!(text.contains("Connections:"));
    assert!(text.contains("Alice --[KNOWS]--> Bob"));
}

#[test]
fn uses_text_as_content_when_present() {
    let context = vec![
        SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({
                "source_id": "1",
                "target_id": "2",
                "source_name": "Doc1",
                "target_name": "Entity1",
                "source_text": "This is the full text content of the document chunk.",
                "relationship": "MENTIONS",
            }),
        },
    ];

    let text = resolve_edges_to_text(&context);

    // When text is present, content should be the text value
    assert!(text.contains("This is the full text content of the document chunk."));
}
```

---

## Dependencies on Other Tasks

- **None as a prerequisite.** This task is self-contained.
- **Downstream impact:** Tasks that consume `RankedGraphEdge` or the output of `resolve_edges_to_text` will benefit from the richer data. Any task that parses the rendered text context (e.g., the LLM prompt) should be aware of the new `Nodes:` / `Connections:` format.
- **Task 3 (Fix Default Values):** Can be done independently and in any order. No conflicts.
- If the `resolve_edges_to_text` format change breaks existing E2E tests, those tests should be updated to match the new Python-compatible format.
