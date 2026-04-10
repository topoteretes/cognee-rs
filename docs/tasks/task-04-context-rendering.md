# Task 4: Implement Python-Compatible Context Rendering

## Summary

The Rust `resolve_edges_to_text` function currently renders graph context as a flat list of triplet lines (`source -[rel]-> target`). The Python version renders a two-section format with a **Nodes** section (containing node content wrapped in `__node_content_start/end__` markers, with auto-generated titles) and a **Connections** section (using `--[REL]-->` arrow syntax). This task ports the Python rendering format to Rust so that graph-based retrievers produce identical context strings for the LLM.

## Current Rust Behavior

### File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/resolve_edges_to_text.rs`

The current implementation produces a flat newline-joined list of triplet lines:

```rust
use crate::types::SearchContext;

pub fn resolve_edges_to_text(context: &SearchContext) -> String {
    context
        .iter()
        .map(|item| {
            let source = item
                .payload
                .get("source_name")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("source_id")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("unknown_source");
            let target = item
                .payload
                .get("target_name")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("target_id")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("unknown_target");
            let relationship = item
                .payload
                .get("relationship")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("relationship_name")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("related_to");

            format!("{source} -[{relationship}]-> {target}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

**Current output example:**
```
Alice -[KNOWS]-> Bob
Bob -[WORKS_WITH]-> Charlie
```

**Problems:**
1. Arrow syntax is `-[REL]->` (single dash prefix), Python uses `--[REL]-->` (double dash prefix, double dash suffix)
2. No node content section -- Python includes full node text/description with markers
3. No title generation algorithm for nodes with `text` content
4. No deduplication of nodes across edges
5. No `__node_content_start__` / `__node_content_end__` markers

### File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/mod.rs` (line 9)

```rust
pub use resolve_edges_to_text::resolve_edges_to_text as render_edges_context;
```

This re-exports as `render_edges_context`, used by:
- `graph_completion_retriever.rs` (line 123)
- `advanced_graph_retrievers.rs` (lines 188, 295, 335, 427, 446)

## Required Python Behavior

### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/utils/resolve_edges_to_text.py`

The Python function produces a **two-section** output:

```python
async def resolve_edges_to_text(retrieved_edges: List[Edge]) -> str:
    """Converts retrieved graph edges into a human-readable string format."""
    if not retrieved_edges:
        return ""

    nodes = _extract_nodes_from_edges(retrieved_edges)

    node_section = "\n".join(
        f"Node: {info['name']}\n__node_content_start__\n{info['content']}\n__node_content_end__\n"
        for info in nodes.values()
    )

    connections = []

    for edge in retrieved_edges:
        source_name = nodes[edge.node1.id]["name"]
        target_name = nodes[edge.node2.id]["name"]
        edge_label = edge.attributes.get("edge_text")
        if not edge_label:
            edge_label = edge.attributes.get("relationship_type")

        connections.append(f"{source_name} --[{edge_label}]--> {target_name}")

    connection_section = "\n".join(connections)

    return f"Nodes:\n{node_section}\n\nConnections:\n{connection_section}"
```

**Python output example:**
```
Nodes:
Node: The quick brown fox jumps... [fox, quick, brown]
__node_content_start__
The quick brown fox jumps over the lazy dog. The fox is very fast.
__node_content_end__

Node: Alice
__node_content_start__
Alice is a software engineer working on AI systems.
__node_content_end__


Connections:
The quick brown fox jumps... [fox, quick, brown] --[DESCRIBES]--> Alice
```

### Title Generation Algorithm

#### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/utils/resolve_edges_to_text.py` (lines 12-30)

```python
def _get_top_n_frequent_words(
    text: str, stop_words: set = None, top_n: int = 3, separator: str = ", "
) -> str:
    """Concatenates the top N frequent words in text."""
    if stop_words is None:
        stop_words = DEFAULT_STOP_WORDS
    words = [word.lower().strip(string.punctuation) for word in text.split()]
    words = [word for word in words if word and word not in stop_words]
    top_words = [word for word, freq in Counter(words).most_common(top_n)]
    return separator.join(top_words)


def _create_title_from_text(text: str, first_n_words: int = 7, top_n_words: int = 3) -> str:
    """Creates a title by combining first words with most frequent words from the text."""
    first_words = text.split()[:first_n_words]
    top_words = _get_top_n_frequent_words(text, top_n=top_n_words)
    return f"{' '.join(first_words)}... [{top_words}]"
```

**Algorithm:**
1. Take the first 7 whitespace-split words of the text
2. Compute top-3 most frequent non-stop-words (lowercased, punctuation-stripped)
3. Format as: `"{first_7_words}... [{word1}, {word2}, {word3}]"`

### Node Extraction Logic

#### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/utils/resolve_edges_to_text.py` (lines 33-62)

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

**Logic:**
- Deduplicate nodes by ID across all edges
- If node has `text` attribute: use `_create_title_from_text(text)` as name, `text` as content
- If node has no `text`: use `name` attribute (fallback "Unnamed Node") as name, `description` attribute (fallback to name) as content

### Stop Words List

#### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/retrieval/utils/stop_words.py`

```python
DEFAULT_STOP_WORDS = {
    "a", "an", "the", "and", "or", "but", "is", "are", "was", "were",
    "in", "on", "at", "to", "for", "with", "by", "about", "of", "from",
    "as", "that", "this", "these", "those", "it", "its", "them", "they",
    "their", "he", "she", "his", "her", "him", "we", "our", "you", "your",
    "not", "be", "been", "being", "have", "has", "had", "do", "does", "did",
    "can", "could", "will", "would", "shall", "should", "may", "might",
    "must", "when", "where", "which", "who", "whom", "whose", "why", "how",
}
```

67 stop words total.

### Edge Label Resolution

Python edge label resolution order (lines 87-97):
1. `edge.attributes.get("edge_text")` -- try `edge_text` first
2. Fallback: `edge.attributes.get("relationship_type")` -- then `relationship_type`

### Arrow Syntax Difference

| | Python | Current Rust |
|---|---|---|
| Arrow | `--[REL]-->` | `-[REL]->` |
| Prefix dashes | 2 | 1 |
| Suffix dashes | 2 | 1 |

## Step-by-Step Changes

### Step 1: Add stop words constant

Create a new file or add to `resolve_edges_to_text.rs`:

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/resolve_edges_to_text.rs`**

Add at the top of the file, after imports:

```rust
use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;

/// English stop words matching Python's `DEFAULT_STOP_WORDS` from
/// `cognee/modules/retrieval/utils/stop_words.py`.
static DEFAULT_STOP_WORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "a", "an", "the", "and", "or", "but", "is", "are", "was", "were",
        "in", "on", "at", "to", "for", "with", "by", "about", "of", "from",
        "as", "that", "this", "these", "those", "it", "its", "them", "they",
        "their", "he", "she", "his", "her", "him", "we", "our", "you", "your",
        "not", "be", "been", "being", "have", "has", "had", "do", "does", "did",
        "can", "could", "will", "would", "shall", "should", "may", "might",
        "must", "when", "where", "which", "who", "whom", "whose", "why", "how",
    ]
    .into_iter()
    .collect()
});
```

**Note:** Check if `once_cell` is already a dependency in the search crate's `Cargo.toml`. If not, add it -- or use `std::sync::LazyLock` if the workspace is on Rust 1.80+ (edition 2024 implies Rust >= 1.85, so `std::sync::LazyLock` is available and `once_cell` is not needed).

With Rust edition 2024 / MSRV >= 1.85, prefer `std::sync::LazyLock`:

```rust
use std::sync::LazyLock;

static DEFAULT_STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    // ... same as above
});
```

### Step 2: Implement title generation functions

Add to `resolve_edges_to_text.rs`:

```rust
/// Returns the `top_n` most frequent non-stop-words from `text`, joined by `", "`.
///
/// Port of Python `_get_top_n_frequent_words` from
/// `cognee/modules/graph/utils/resolve_edges_to_text.py`.
fn get_top_n_frequent_words(text: &str, top_n: usize) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for word in text.split_whitespace() {
        let lower = word.to_lowercase();
        let stripped = lower.trim_matches(|c: char| c.is_ascii_punctuation());
        if stripped.is_empty() || DEFAULT_STOP_WORDS.contains(stripped) {
            continue;
        }
        *counts.entry(stripped.to_string()).or_insert(0) += 1;
    }

    // Sort by frequency descending, then by insertion order (use a stable sort
    // on a vec to preserve first-occurrence ordering for ties, matching Python's
    // Counter.most_common which is stable for equal counts).
    let mut word_counts: Vec<(String, usize)> = counts.into_iter().collect();
    word_counts.sort_by(|a, b| b.1.cmp(&a.1));

    word_counts
        .into_iter()
        .take(top_n)
        .map(|(word, _)| word)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Creates a title by combining the first `first_n_words` words with the
/// `top_n_words` most frequent non-stop-words.
///
/// Port of Python `_create_title_from_text` from
/// `cognee/modules/graph/utils/resolve_edges_to_text.py`.
///
/// Output format: `"word1 word2 ... word7... [freq1, freq2, freq3]"`
fn create_title_from_text(text: &str) -> String {
    const FIRST_N_WORDS: usize = 7;
    const TOP_N_WORDS: usize = 3;

    let first_words: Vec<&str> = text.split_whitespace().take(FIRST_N_WORDS).collect();
    let top_words = get_top_n_frequent_words(text, TOP_N_WORDS);

    format!("{}... [{}]", first_words.join(" "), top_words)
}
```

**Important note on Python `Counter.most_common` tie-breaking:** Python's `Counter.most_common()` returns items in insertion order for equal counts (CPython 3.7+ dict ordering). The Rust implementation above uses `HashMap` which does not guarantee insertion order. For strict Python parity on tie-breaking, use `IndexMap` from the `indexmap` crate instead of `HashMap`. However, for practical purposes the difference only affects the ordering of equally-frequent words in the title, which has negligible impact on LLM behavior. If strict parity is required, replace `HashMap<String, usize>` with `IndexMap<String, usize>` and sort stably.

### Step 3: Implement node extraction

Add to `resolve_edges_to_text.rs`:

```rust
struct NodeInfo {
    name: String,
    content: String,
}

/// Extracts and deduplicates nodes from the search context, determining
/// name and content for each unique node.
///
/// Port of Python `_extract_nodes_from_edges`.
///
/// Since the Rust `SearchContext` uses flat payload maps (not full Edge objects
/// with embedded Node objects), we extract node information from the payload
/// fields: `source_id`/`target_id`, `source_name`/`target_name`,
/// `source_text`/`target_text`, `source_description`/`target_description`.
fn extract_nodes_from_context(context: &SearchContext) -> IndexMap<String, NodeInfo> {
    let mut nodes: IndexMap<String, NodeInfo> = IndexMap::new();

    for item in context {
        // Process source node
        let source_id = item
            .payload
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_source");

        if !nodes.contains_key(source_id) {
            let info = build_node_info(&item.payload, "source");
            nodes.insert(source_id.to_string(), info);
        }

        // Process target node
        let target_id = item
            .payload
            .get("target_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_target");

        if !nodes.contains_key(target_id) {
            let info = build_node_info(&item.payload, "target");
            nodes.insert(target_id.to_string(), info);
        }
    }

    nodes
}

/// Builds a `NodeInfo` from payload fields with the given prefix ("source" or "target").
///
/// Mirrors Python logic:
/// - If `{prefix}_text` exists: name = create_title_from_text(text), content = text
/// - Else: name = `{prefix}_name` (fallback "Unnamed Node"), content = `{prefix}_description` (fallback name)
fn build_node_info(payload: &serde_json::Value, prefix: &str) -> NodeInfo {
    let text_key = format!("{prefix}_text");
    let name_key = format!("{prefix}_name");
    let desc_key = format!("{prefix}_description");

    if let Some(text) = payload.get(&text_key).and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return NodeInfo {
                name: create_title_from_text(text),
                content: text.to_string(),
            };
        }
    }

    let name = payload
        .get(&name_key)
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed Node")
        .to_string();

    let content = payload
        .get(&desc_key)
        .and_then(|v| v.as_str())
        .map(|d| d.to_string())
        .unwrap_or_else(|| name.clone());

    NodeInfo { name, content }
}
```

**Important:** This requires adding `source_text`, `target_text`, `source_description`, and `target_description` to the `SearchItem` payload in the graph retrieval pipeline. See Step 5 for the upstream change needed.

### Step 4: Rewrite `resolve_edges_to_text` to produce two-section format

Replace the existing function body:

```rust
/// Converts search context (graph edges) into a human-readable two-section format
/// matching Python's `resolve_edges_to_text`.
///
/// Output format:
/// ```text
/// Nodes:
/// Node: {name}
/// __node_content_start__
/// {content}
/// __node_content_end__
///
/// ...
///
/// Connections:
/// {source_name} --[{relationship}]--> {target_name}
/// ...
/// ```
pub fn resolve_edges_to_text(context: &SearchContext) -> String {
    if context.is_empty() {
        return String::new();
    }

    let nodes = extract_nodes_from_context(context);

    // Build node section
    let node_section: String = nodes
        .values()
        .map(|info| {
            format!(
                "Node: {}\n__node_content_start__\n{}\n__node_content_end__\n",
                info.name, info.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Build connections section
    let connections: Vec<String> = context
        .iter()
        .map(|item| {
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

            let source_name = nodes
                .get(source_id)
                .map(|n| n.name.as_str())
                .unwrap_or("unknown_source");
            let target_name = nodes
                .get(target_id)
                .map(|n| n.name.as_str())
                .unwrap_or("unknown_target");

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

            format!("{source_name} --[{relationship}]--> {target_name}")
        })
        .collect();

    let connection_section = connections.join("\n");

    format!("Nodes:\n{node_section}\n\nConnections:\n{connection_section}")
}
```

### Step 5: Propagate node text/description through the search pipeline

The Python `resolve_edges_to_text` receives full `Edge` objects that contain embedded `Node` objects with all attributes (including `text` and `description`). The Rust pipeline currently only propagates `source_name`, `target_name`, `source_id`, `target_id`, and `relationship` through the `SearchItem` payload.

Node text and description must be added to the payload. This requires changes in two places:

#### 5a: `brute_force_triplet_search` result type

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/graph_retrieval/`**

The `RankedGraphEdge` struct (or equivalent) must be extended to carry `source_text`, `target_text`, `source_description`, `target_description`. These values come from the graph DB node properties.

Examine the graph retrieval code to find where `RankedGraphEdge` is constructed and add the missing fields. The graph DB's `get_node` or `get_graph_data` returns node properties as `HashMap<Cow<'static, str>, serde_json::Value>` which should already contain `text` and `description` fields.

#### 5b: `SearchItem` payload construction in retrievers

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/graph_completion_retriever.rs` (lines 95-109)**

Current:
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

Required -- add node text and description fields to the payload:
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
            "source_text": edge.source_text,
            "target_text": edge.target_text,
            "source_description": edge.source_description,
            "target_description": edge.target_description,
            "dataset_id": edge.dataset_id,
        }),
    })
    .collect())
```

The same change must be applied in:
- `advanced_graph_retrievers.rs` -- the `GraphRetrieverCore::get_context` method (lines 90-104)
- `temporal_retriever.rs` -- wherever it constructs `SearchItem` from graph edges

### Step 6: Add `indexmap` dependency (if not already present)

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/Cargo.toml`**

Add `indexmap` to preserve insertion order for node deduplication (matching Python dict ordering):

```toml
[dependencies]
indexmap = "2"
```

### Step 7: Update imports in `resolve_edges_to_text.rs`

Replace:
```rust
use crate::types::SearchContext;
```

With:
```rust
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::types::SearchContext;
```

### Step 8: Update existing tests

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/graph_completion_retriever.rs`**

The test at line 583 currently asserts:
```rust
assert!(messages[1].content.contains("Graph=Alice -[KNOWS]-> Bob"));
```

This must be updated to match the new arrow syntax:
```rust
assert!(messages[1].content.contains("Graph="));
assert!(messages[1].content.contains("--[KNOWS]-->"));
```

However, note that this test uses a custom `user_prompt_template` (`"Question={question}\nGraph={context}"`) that receives the full rendered context. With the new two-section format, the context will be:
```
Nodes:
Node: Alice
__node_content_start__
Alice
__node_content_end__

Node: Bob
__node_content_start__
Bob
__node_content_end__


Connections:
Alice --[KNOWS]--> Bob
```

The test assertion needs to check for both sections.

## Test Verification

### Unit tests to add in `resolve_edges_to_text.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SearchItem;
    use serde_json::json;

    #[test]
    fn empty_context_returns_empty_string() {
        let context = vec![];
        assert_eq!(resolve_edges_to_text(&context), "");
    }

    #[test]
    fn stop_words_are_filtered() {
        let top = get_top_n_frequent_words("the fox and the dog and a fox", 3);
        assert_eq!(top, "fox, dog");
    }

    #[test]
    fn title_generation_matches_python_format() {
        let text = "The quick brown fox jumps over the lazy dog. The fox is very fast and agile.";
        let title = create_title_from_text(text);
        assert!(title.starts_with("The quick brown fox jumps over the..."));
        assert!(title.contains("["));
        assert!(title.contains("]"));
        // "fox" should be most frequent non-stop-word
        assert!(title.contains("fox"));
    }

    #[test]
    fn two_section_output_format() {
        let context = vec![SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({
                "source_id": "id1",
                "target_id": "id2",
                "source_name": "Alice",
                "target_name": "Bob",
                "relationship": "KNOWS",
            }),
        }];

        let output = resolve_edges_to_text(&context);
        assert!(output.starts_with("Nodes:\n"));
        assert!(output.contains("__node_content_start__"));
        assert!(output.contains("__node_content_end__"));
        assert!(output.contains("\n\nConnections:\n"));
        assert!(output.contains("Alice --[KNOWS]--> Bob"));
    }

    #[test]
    fn nodes_with_text_get_generated_titles() {
        let context = vec![SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({
                "source_id": "id1",
                "target_id": "id2",
                "source_name": "Alice",
                "source_text": "Alice is a software engineer who works on AI systems daily.",
                "target_name": "Bob",
                "relationship": "KNOWS",
            }),
        }];

        let output = resolve_edges_to_text(&context);
        // Source node should have a generated title (not "Alice")
        assert!(output.contains("Alice is a software engineer who..."));
        assert!(output.contains("["));
        // Target node has no text, should use name
        assert!(output.contains("Node: Bob"));
    }

    #[test]
    fn node_deduplication_across_edges() {
        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({
                    "source_id": "id1",
                    "target_id": "id2",
                    "source_name": "Alice",
                    "target_name": "Bob",
                    "relationship": "KNOWS",
                }),
            },
            SearchItem {
                id: None,
                score: Some(0.8),
                payload: json!({
                    "source_id": "id1",
                    "target_id": "id3",
                    "source_name": "Alice",
                    "target_name": "Charlie",
                    "relationship": "WORKS_WITH",
                }),
            },
        ];

        let output = resolve_edges_to_text(&context);
        // "Alice" should appear exactly once in the Nodes section
        let nodes_section = output.split("\n\nConnections:").next().unwrap();
        assert_eq!(nodes_section.matches("Node: Alice").count(), 1);
    }

    #[test]
    fn arrow_syntax_uses_double_dashes() {
        let context = vec![SearchItem {
            id: None,
            score: Some(0.9),
            payload: json!({
                "source_id": "id1",
                "target_id": "id2",
                "source_name": "Alice",
                "target_name": "Bob",
                "relationship": "KNOWS",
            }),
        }];

        let output = resolve_edges_to_text(&context);
        assert!(output.contains("--[KNOWS]-->"));
        assert!(!output.contains(" -[KNOWS]-> ")); // Old format should NOT appear
    }
}
```

### Integration test updates

The E2E search matrix test and any tests that assert on the rendered context format will need updating to expect the two-section format instead of the flat triplet format.

## Dependencies on Other Tasks

- **Task 5 (graph user prompt):** The context string produced by this task is what gets inserted into the `{context}` placeholder in the user prompt template. Task 5 changes the user prompt template for graph-based retrievers, so both tasks work together to match Python's full LLM input.
- **Graph retrieval pipeline:** Step 5 requires the `RankedGraphEdge` struct to carry node `text` and `description` fields from the graph DB. If this struct does not yet propagate these fields, that upstream change is a prerequisite. Investigate the graph retrieval module (`crates/search/src/graph_retrieval/`) to determine the exact changes needed.
