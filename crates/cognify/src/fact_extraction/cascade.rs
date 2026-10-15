//! Cascade (multi-round) graph extraction.
//!
//! Port of Python's `cognee/tasks/graph/cascade_extract/` and the
//! `extract_graph_from_data` in `cognee/tasks/graph/extract_graph_from_data_v2.py`.
//! Instead of one structured-output call per chunk, the graph is built in three
//! stages, each run for `n_rounds` rounds that see what earlier rounds found:
//!
//! 1. **nodes** — candidate entity strings ([`extract_nodes`]);
//! 2. **nodes and relationship names** — refined nodes plus candidate relation
//!    names ([`extract_content_nodes_and_relationship_names`]);
//! 3. **edge triplets** — a [`KnowledgeGraph`] whose edges join the nodes found
//!    ([`extract_edge_triplets`]).
//!
//! That is `3 × n_rounds` LLM calls per chunk instead of one. Opt-in through
//! [`crate::CognifyConfig::with_extraction_strategy`]; in Python it is reachable
//! only from the eval framework (`get_cascade_graph_tasks`), never from
//! `cognify()`, and the default stays single-pass in both SDKs. The resulting
//! graphs go through the same expansion as single-pass ones.
//!
//! The prompts are vendored verbatim from Python and rendered the way Jinja
//! renders them there: list and set context values print as Python's `str()`
//! of the value (`['a', 'b']`, `set()`), and the template's trailing newline is
//! dropped.

use std::collections::HashSet;
use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::models::KnowledgeGraph;
use crate::error::CognifyError;

/// Python's default `n_rounds` for every cascade stage.
pub const DEFAULT_CASCADE_ROUNDS: usize = 2;

const NODES_SYSTEM: &str = include_str!("prompts/cascade/extract_graph_nodes_prompt_system.txt");
const NODES_INPUT: &str = include_str!("prompts/cascade/extract_graph_nodes_prompt_input.txt");
const RELATIONSHIPS_SYSTEM: &str =
    include_str!("prompts/cascade/extract_graph_relationship_names_prompt_system.txt");
const RELATIONSHIPS_INPUT: &str =
    include_str!("prompts/cascade/extract_graph_relationship_names_prompt_input.txt");
const TRIPLETS_SYSTEM: &str =
    include_str!("prompts/cascade/extract_graph_edge_triplets_prompt_system.txt");
const TRIPLETS_INPUT: &str =
    include_str!("prompts/cascade/extract_graph_edge_triplets_prompt_input.txt");

/// Python's `PotentialNodes`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct PotentialNodes {
    nodes: Vec<String>,
}

/// Python's `PotentialNodesAndRelationshipNames`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct PotentialNodesAndRelationshipNames {
    nodes: Vec<String>,
    relationship_names: Vec<String>,
}

/// Python's `repr()` of a string: single quotes unless the text contains a
/// single quote and no double quote, with backslashes, the chosen quote and
/// control whitespace escaped.
fn py_repr_str(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str()` of a list of strings: `['a', 'b']`.
fn py_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| py_repr_str(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// Python's `str()` of a set of strings, in insertion order: `{'a', 'b'}`, or
/// `set()` when empty. (Python's own order is hash-dependent.)
fn py_set(items: &[String]) -> String {
    if items.is_empty() {
        return "set()".to_string();
    }
    let inner: Vec<String> = items.iter().map(|s| py_repr_str(s)).collect();
    format!("{{{}}}", inner.join(", "))
}

/// Python's `str()` of a set of `(source, target, relationship)` tuples.
fn py_triplet_set(items: &[(String, String, String)]) -> String {
    if items.is_empty() {
        return "set()".to_string();
    }
    let inner: Vec<String> = items
        .iter()
        .map(|(s, t, r)| {
            format!(
                "({}, {}, {})",
                py_repr_str(s),
                py_repr_str(t),
                py_repr_str(r)
            )
        })
        .collect();
    format!("{{{}}}", inner.join(", "))
}

/// Render `{{ key }}` placeholders the way Python's Jinja `render_prompt` does
/// for these templates: an absent key renders empty, and the template's
/// trailing newline is dropped (`keep_trailing_newline=False`).
fn render(template: &str, context: &[(&str, String)]) -> String {
    let mut out = template.strip_suffix('\n').unwrap_or(template).to_string();
    let mut start = 0;
    while let Some(open) = out[start..].find("{{") {
        let open = start + open;
        let Some(close) = out[open..].find("}}") else {
            break;
        };
        let close = open + close;
        let key = out[open + 2..close].trim().to_string();
        let value = context
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        out.replace_range(open..close + 2, &value);
        start = open + value.len();
    }
    out
}

async fn ask<T: super::GraphModel>(
    llm: &Arc<dyn Llm>,
    input: &str,
    system: &str,
) -> Result<T, CognifyError> {
    llm.create_structured_output(
        input,
        system,
        Some(GenerationOptions {
            temperature: None,
            max_tokens: None,
            ..Default::default()
        }),
    )
    .await
    .map_err(|e| CognifyError::LlmError(e.to_string()))
}

impl super::GraphModel for PotentialNodes {}
impl super::GraphModel for PotentialNodesAndRelationshipNames {}

/// Candidate node strings found over `n_rounds` rounds, case-insensitively
/// deduplicated in first-seen order. Port of Python's `extract_nodes`.
pub async fn extract_nodes(
    llm: &Arc<dyn Llm>,
    text: &str,
    n_rounds: usize,
) -> Result<Vec<String>, CognifyError> {
    let mut all_nodes: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for round in 0..n_rounds {
        // Python's template reads `{{ previous_entities }}` but the code passes
        // `previous_nodes`, so the slot always renders empty there. Kept as is
        // so both SDKs send the same prompt; the earlier rounds still shape the
        // answer through deduplication below.
        let input = render(
            NODES_INPUT,
            &[
                ("previous_nodes", py_list(&all_nodes)),
                ("round_number", (round + 1).to_string()),
                ("total_rounds", n_rounds.to_string()),
                ("text", text.to_string()),
            ],
        );
        let response: PotentialNodes = ask(llm, &input, NODES_SYSTEM).await?;
        for node in response.nodes {
            if seen.insert(node.to_lowercase()) {
                all_nodes.push(node);
            }
        }
    }
    Ok(all_nodes)
}

/// Refined nodes and candidate relationship names over `n_rounds` rounds.
/// Port of Python's `extract_content_nodes_and_relationship_names`.
pub async fn extract_content_nodes_and_relationship_names(
    llm: &Arc<dyn Llm>,
    text: &str,
    existing_nodes: &[String],
    n_rounds: usize,
) -> Result<(Vec<String>, Vec<String>), CognifyError> {
    let mut all_nodes: Vec<String> = existing_nodes.to_vec();
    let mut all_relationship_names: Vec<String> = Vec::new();
    let mut node_set: HashSet<String> = all_nodes.iter().map(|n| n.to_lowercase()).collect();
    let mut relationship_set: HashSet<String> = HashSet::new();
    for round in 0..n_rounds {
        let input = render(
            RELATIONSHIPS_INPUT,
            &[
                ("text", text.to_string()),
                ("potential_nodes", py_list(existing_nodes)),
                ("previous_nodes", py_list(&all_nodes)),
                (
                    "previous_relationship_names",
                    py_list(&all_relationship_names),
                ),
                ("round_number", (round + 1).to_string()),
                ("total_rounds", n_rounds.to_string()),
            ],
        );
        let response: PotentialNodesAndRelationshipNames =
            ask(llm, &input, RELATIONSHIPS_SYSTEM).await?;
        for node in response.nodes {
            if node_set.insert(node.to_lowercase()) {
                all_nodes.push(node);
            }
        }
        for name in response.relationship_names {
            if relationship_set.insert(name.to_lowercase()) {
                all_relationship_names.push(name);
            }
        }
    }
    Ok((all_nodes, all_relationship_names))
}

/// The knowledge graph joining `nodes` with edges over `n_rounds` rounds.
/// Port of Python's `extract_edge_triplets`: a node is kept once per name, and
/// an edge only when it is new and both its endpoints are kept nodes' ids.
pub async fn extract_edge_triplets(
    llm: &Arc<dyn Llm>,
    text: &str,
    nodes: &[String],
    relationship_names: &[String],
    n_rounds: usize,
) -> Result<KnowledgeGraph, CognifyError> {
    let mut graph = KnowledgeGraph {
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let mut node_names: Vec<String> = Vec::new();
    let mut node_name_set: HashSet<String> = HashSet::new();
    let mut node_ids: HashSet<String> = HashSet::new();
    let mut triplets: Vec<(String, String, String)> = Vec::new();
    let mut triplet_set: HashSet<(String, String, String)> = HashSet::new();
    for round in 0..n_rounds {
        let input = render(
            TRIPLETS_INPUT,
            &[
                ("text", text.to_string()),
                ("potential_nodes", py_list(nodes)),
                ("potential_relationship_names", py_list(relationship_names)),
                ("previous_nodes", py_set(&node_names)),
                ("previous_edge_triplets", py_triplet_set(&triplets)),
                ("round_number", (round + 1).to_string()),
                ("total_rounds", n_rounds.to_string()),
            ],
        );
        let extracted: KnowledgeGraph = ask(llm, &input, TRIPLETS_SYSTEM).await?;
        for mut node in extracted.nodes {
            // A node with no name is named after its id *before* the
            // per-name dedup (Python `Node.__init__` does it on construction);
            // otherwise every unnamed node would collapse into the first.
            if node.name.is_empty() {
                node.name = node.id.clone();
            }
            if node_name_set.insert(node.name.clone()) {
                node_names.push(node.name.clone());
                node_ids.insert(node.id.clone());
                graph.nodes.push(node);
            }
        }
        for edge in extracted.edges {
            let key = (
                edge.source_node_id.clone(),
                edge.target_node_id.clone(),
                edge.relationship_name.clone(),
            );
            if triplet_set.contains(&key)
                || !(node_ids.contains(&edge.source_node_id)
                    && node_ids.contains(&edge.target_node_id))
            {
                continue;
            }
            triplet_set.insert(key.clone());
            triplets.push(key);
            graph.edges.push(edge);
        }
    }
    Ok(graph)
}

/// All three cascade stages for one chunk.
pub async fn cascade_extract(
    llm: &Arc<dyn Llm>,
    text: &str,
    n_rounds: usize,
) -> Result<KnowledgeGraph, CognifyError> {
    let nodes = extract_nodes(llm, text, n_rounds).await?;
    let (nodes, relationship_names) =
        extract_content_nodes_and_relationship_names(llm, text, &nodes, n_rounds).await?;
    extract_edge_triplets(llm, text, &nodes, &relationship_names, n_rounds).await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_llm::{GenerationResponse, LlmResult, Message, MessageRole};
    use std::sync::Mutex;

    #[test]
    fn python_reprs() {
        assert_eq!(py_list(&[]), "[]");
        assert_eq!(py_list(&["a".into(), "it's".into()]), "['a', \"it's\"]");
        assert_eq!(
            py_list(&["say \"hi\" it's".into()]),
            "['say \"hi\" it\\'s']"
        );
        assert_eq!(py_set(&[]), "set()");
        assert_eq!(py_set(&["x".into()]), "{'x'}");
        assert_eq!(
            py_triplet_set(&[("a".into(), "b".into(), "r".into())]),
            "{('a', 'b', 'r')}"
        );
        assert_eq!(py_repr_str("a\\b\n"), "'a\\\\b\\n'");
    }

    #[test]
    fn render_fills_placeholders_and_drops_the_trailing_newline() {
        let out = render(
            NODES_INPUT,
            &[
                ("round_number", "1".into()),
                ("total_rounds", "2".into()),
                ("text", "Alice met Bob.".into()),
            ],
        );
        assert!(out.contains("This is round 1 of 2."));
        assert!(out.contains("**Text:**\nAlice met Bob.\n"));
        // `previous_entities` is never passed (Python parity) and renders empty.
        assert!(out.ends_with("**Previously Extracted Entities:**\n"));
        assert!(!out.contains("{{"));
    }

    /// Answers each stage by recognising its system prompt, and records the
    /// user prompt of every call.
    struct StagedLlm(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl Llm for StagedLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            let system = messages
                .iter()
                .find(|m| matches!(m.role, MessageRole::System))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let user = messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, MessageRole::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            // lock poison is unrecoverable
            let round = self.0.lock().unwrap().len();
            self.0.lock().unwrap().push(user);
            Ok(if system.contains("node identification") {
                serde_json::json!({"nodes": ["Alice", "alice", "Bob"]})
            } else if system.contains("focusing on relationships") {
                serde_json::json!({"nodes": ["Acme"], "relationship_names": ["works_at", "Works_At"]})
            } else {
                serde_json::json!({
                    "nodes": [
                        {"id": "alice", "name": "Alice", "type": "Person", "description": ""},
                        {"id": "acme", "name": "Acme", "type": "Organization", "description": ""}
                    ],
                    "edges": [
                        {"source_node_id": "alice", "target_node_id": "acme", "relationship_name": "works_at"},
                        {"source_node_id": "alice", "target_node_id": "ghost", "relationship_name": "knows"},
                        {"source_node_id": "alice", "target_node_id": "acme", "relationship_name": format!("r{}", round % 2)}
                    ]
                })
            })
        }

        fn model(&self) -> &str {
            "staged"
        }
    }

    /// Answers the triplet stage with two unnamed nodes and an edge between them.
    struct UnnamedNodesLlm;

    #[async_trait::async_trait]
    impl Llm for UnnamedNodesLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            Ok(serde_json::json!({
                "nodes": [
                    {"id": "ada", "name": "", "type": "Person", "description": ""},
                    {"id": "engine", "name": "", "type": "Machine", "description": ""}
                ],
                "edges": [
                    {"source_node_id": "ada", "target_node_id": "engine", "relationship_name": "designed"}
                ]
            }))
        }

        fn model(&self) -> &str {
            "unnamed"
        }
    }

    #[tokio::test]
    async fn unnamed_nodes_are_named_after_their_id_before_the_dedup() {
        let llm: Arc<dyn Llm> = Arc::new(UnnamedNodesLlm);
        let graph = extract_edge_triplets(&llm, "text", &[], &[], 1)
            .await
            .unwrap();
        let names: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["ada", "engine"]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[tokio::test]
    async fn three_stages_of_n_rounds_build_one_graph() {
        let recorder = Arc::new(StagedLlm(Mutex::new(Vec::new())));
        let llm: Arc<dyn Llm> = recorder.clone();
        let graph = cascade_extract(&llm, "Alice works at Acme.", 2)
            .await
            .unwrap();

        let calls = recorder.0.lock().unwrap();
        assert_eq!(calls.len(), 6, "3 stages × 2 rounds");
        // Round two of the relationship stage sees round one's findings.
        assert!(
            calls[3].contains("['Alice', 'Bob', 'Acme']"),
            "{}",
            calls[3]
        );
        assert!(
            calls[3].contains("**Relationships Identified in Previous Rounds:**\n['works_at']")
        );
        // Round two of the triplet stage sees round one's nodes and edges.
        assert!(calls[5].contains("{'Alice', 'Acme'}"), "{}", calls[5]);
        assert!(calls[5].contains("('alice', 'acme', 'works_at')"));

        // Nodes once per name; an edge to an unknown node is dropped; a
        // repeated triplet is kept once.
        assert_eq!(graph.nodes.len(), 2);
        let rels: Vec<&str> = graph
            .edges
            .iter()
            .map(|e| e.relationship_name.as_str())
            .collect();
        assert_eq!(rels, vec!["works_at", "r0", "r1"]);
    }
}
