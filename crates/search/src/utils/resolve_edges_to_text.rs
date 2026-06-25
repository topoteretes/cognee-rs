use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::types::SearchContext;

/// English stop words matching Python's `DEFAULT_STOP_WORDS` from
/// `cognee/modules/retrieval/utils/stop_words.py`.
static DEFAULT_STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "a", "an", "the", "and", "or", "but", "is", "are", "was", "were", "in", "on", "at", "to",
        "for", "with", "by", "about", "of", "from", "as", "that", "this", "these", "those", "it",
        "its", "them", "they", "their", "he", "she", "his", "her", "him", "we", "our", "you",
        "your", "not", "be", "been", "being", "have", "has", "had", "do", "does", "did", "can",
        "could", "will", "would", "shall", "should", "may", "might", "must", "when", "where",
        "which", "who", "whom", "whose", "why", "how",
    ]
    .into_iter()
    .collect()
});

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

    // Sort by frequency descending. Use a stable sort so equal-frequency words
    // remain in a deterministic order.
    let mut word_counts: Vec<(String, usize)> = counts.into_iter().collect();
    word_counts.sort_by_key(|w| std::cmp::Reverse(w.1));

    word_counts
        .into_iter()
        .take(top_n)
        .map(|(word, _)| word)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Creates a title by combining the first `FIRST_N_WORDS` words with the
/// `TOP_N_WORDS` most frequent non-stop-words.
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
/// - If `{prefix}_text` exists and is non-empty: name = create_title_from_text(text), content = text
/// - Else: name = `{prefix}_name` (fallback "Unnamed Node"), content = `{prefix}_description` (fallback name)
fn build_node_info(payload: &serde_json::Value, prefix: &str) -> NodeInfo {
    let text_key = format!("{prefix}_text");
    let name_key = format!("{prefix}_name");
    let desc_key = format!("{prefix}_description");

    if let Some(text) = payload.get(&text_key).and_then(|v| v.as_str())
        && !text.is_empty()
    {
        return NodeInfo {
            name: create_title_from_text(text),
            content: text.to_string(),
        };
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
        assert!(title.contains('['));
        assert!(title.contains(']'));
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
        // First 7 words: "Alice is a software engineer who works"
        assert!(output.contains("Alice is a software engineer who works..."));
        assert!(output.contains('['));
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
        let nodes_section = output
            .split("\n\nConnections:")
            .next()
            .expect("output must contain Connections section");
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
