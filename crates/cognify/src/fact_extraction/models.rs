//! Knowledge graph data models.
//!
//! Port of Python's cognee/shared/data_models.py
//! These models represent the extracted knowledge graph structure:
//! - Node: Entities and concepts in the graph
//! - Edge: Relationships between nodes
//! - KnowledgeGraph: Collection of nodes and edges

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Marker trait for types that can be used as graph extraction models.
///
/// Types implementing this trait can be extracted from text via LLM
/// structured output. The LLM generates JSON conforming to the type's
/// [`JsonSchema`], which is then deserialized into the concrete type.
///
/// The built-in [`KnowledgeGraph`] model implements this trait with
/// `is_default_knowledge_graph() == true`, which triggers additional
/// post-processing (entity/edge expansion, deduplication, graph DB storage).
/// Custom models return `false`, causing the extracted value to be stored
/// directly in [`DocumentChunk::contains`] as serialized JSON — mirroring
/// the Python branching at `extract_graph_from_data.py:99-103`.
///
/// # Required bounds
/// `Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static`
pub trait GraphModel:
    Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static
{
    /// Returns `true` if this is the built-in [`KnowledgeGraph`] model.
    ///
    /// Custom models should leave the default (`false`), which changes
    /// the processing flow: extracted data is stored as-is in chunk metadata
    /// instead of being expanded into graph nodes and edges.
    fn is_default_knowledge_graph() -> bool {
        false
    }
}

/// Node in a knowledge graph.
///
/// Represents an entity or concept extracted from text.
/// Nodes are akin to Wikipedia nodes - they represent distinct entities.
///
/// # Fields
/// * `id` - Unique identifier (human-readable, not an integer)
/// * `name` - Display name of the entity
/// * `node_type` - Type classification (e.g., "PERSON", "ORGANIZATION", "CONCEPT")
/// * `description` - Brief description of the entity
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// Unique identifier for the node (human-readable, e.g., "Albert Einstein")
    pub id: String,

    /// Display name of the entity
    pub name: String,

    /// Entity type (e.g., "PERSON", "ORGANIZATION", "CONCEPT")
    /// Use uppercase for consistency with Python
    #[serde(rename = "type")]
    pub node_type: String,

    /// Brief description of the entity (1-2 sentences)
    pub description: String,
}

/// Edge in a knowledge graph.
///
/// Represents a relationship between two nodes.
/// Edges are akin to Wikipedia links - they connect related concepts.
///
/// # Fields
/// * `source_node_id` - ID of the source node
/// * `target_node_id` - ID of the target node
/// * `relationship_name` - Type of relationship (use snake_case, e.g., "works_at")
/// * `description` - Concrete one-sentence fact expressed by this edge
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// ID of the source node
    pub source_node_id: String,

    /// ID of the target node
    pub target_node_id: String,

    /// Type of relationship (snake_case, e.g., "works_at", "founded", "located_in")
    pub relationship_name: String,

    /// Concrete one-sentence fact expressed by this edge, using endpoint names.
    /// Mirrors Python `KnowledgeGraph.Edge.description` (data_models.py:62-71).
    /// Becomes the `edge_text` graph-edge property, feeding EdgeType + Triplet
    /// embeddings. Optional because older/custom outputs may omit it.
    #[serde(default)]
    pub description: Option<String>,
}

/// Knowledge graph extracted from text.
///
/// Contains nodes (entities/concepts) and edges (relationships).
/// This is the primary output of fact extraction.
///
/// # Fields
/// * `nodes` - List of extracted entities and concepts
/// * `edges` - List of relationships between nodes
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeGraph {
    /// List of nodes (entities and concepts).
    ///
    /// Intentionally NOT `#[serde(default)]`: schemars honours serde `default`
    /// by emitting a JSON-schema `"default"` and dropping the field from the
    /// `required` array, which advertises the field as optional to the model.
    /// gpt-oss-120b (and other non-strict tool-callers) then omit it. Requiring
    /// `nodes`/`edges` here makes schemars list them in `required` (forcing the
    /// model to return both) and makes a payload missing either field fail typed
    /// deserialization, driving the adapter's corrective-retry loop instead of
    /// silently yielding a 0-edge graph. Mirrors what instructor does in Python,
    /// where its TOOLS-mode schema builder re-adds non-default fields to
    /// `required` (Python's `default_factory` emits no schema `"default"`).
    pub nodes: Vec<Node>,

    /// List of edges (relationships between nodes). NOT `#[serde(default)]` — see
    /// the note on `nodes`. An empty graph must send `{"nodes":[],"edges":[]}`.
    //
    // NOT a doc comment, deliberately. `schemars` copies every `///` on a field
    // straight into that property's JSON-schema `description`, and this struct's
    // schema is what `generate_json_schema` hands the model on every extraction
    // call — so a `///` here would spend prompt tokens telling the model about a
    // private Rust deserializer, and condition it on the existence of a
    // tolerance it should never aim at. `the_model_facing_schema_carries_no_
    // implementation_detail` below pins that.
    //
    // Deserialized through `deserialize_edges_lenient`, which drops a small
    // number of malformed entries instead of failing the whole graph. See that
    // function for why the model emits them.
    #[serde(deserialize_with = "deserialize_edges_lenient")]
    pub edges: Vec<Edge>,
}

/// The share of `edges` entries that may be dropped before the payload is
/// treated as broken rather than merely blemished.
///
/// Calibrated against measured failures on `gpt-4.1-mini` and Bedrock Sonnet 4.5:
/// the stray-node defect described in [`deserialize_edges_lenient`] contaminated
/// **1–2 entries out of 38–87**, i.e. 1.5%–4.8% of the array, with the worst
/// observed case 2 of 42. A quarter leaves a wide margin over that while still
/// letting a genuinely garbled array — a truncation, or a model that mixed up
/// the two arrays wholesale — fall through to the adapter's corrective-retry
/// loop, which is the right handling for those.
///
/// The comparison is `>`, so a payload sitting *exactly* on the fraction is
/// accepted. That is deliberate and it is what makes the rule sharp on short
/// arrays: 1 of 4 is 25% and survives, but **any** array of fewer than four
/// entries has zero tolerance, because one drop out of three is already 33%.
/// A one-edge payload whose single edge is malformed is rejected, which is the
/// right answer — there is nothing left to keep.
const MAX_DROPPABLE_EDGE_FRACTION: f64 = 0.25;

/// Deserialize `edges`, dropping individual entries that are not edges at all.
///
/// # The defect this absorbs
///
/// Non-constrained structured output (OpenAI `tools` mode, Bedrock Converse
/// `toolConfig`) lets the model emit whatever tokens it likes; only this typed
/// deserialization enforces the schema, and it does so after the fact. Because
/// the model writes `nodes` before `edges` and cannot revise an array it has
/// already emitted, a chunk that introduces a principal entity *late* in the text
/// puts it in a bind: while writing the edges it needs a node id it never
/// declared. Its observed repair is to append a **node-shaped object into the
/// `edges` array**, renaming `id` to `source_node_id`:
///
/// ```text
/// {"source_node_id": "Sister", "name": "Alice's Sister",
///  "type": "PERSON", "description": "Alice's sister, who wakes her from the dream."}
/// ```
///
/// That object has no `target_node_id` and no `relationship_name`, so strict
/// deserialization of the array fails and — before this function existed — the
/// entire chunk's extraction was discarded, one bad entry voiding 50 good edges,
/// then re-requested at full cost. On a nine-chunk document a single such chunk
/// is 11% of the run, which trips the 5% `chunk_failure_ratio_threshold` and
/// rolls back everything.
///
/// # What Python actually does (it is not constrained decoding)
///
/// It is tempting to say Python escapes this because it uses constrained
/// decoding. It does not. Verified against the installed cognee 1.5.4:
/// `cognee/infrastructure/llm/structured_output_framework/litellm_instructor/`
/// `llm/openai/adapter.py:107` takes the explicit-mode branch only when
/// `"gpt-5" in model` or an explicit `instructor_mode` was passed
/// (`llm_instructor_mode`, empty by default); otherwise `:115` builds the
/// client as `instructor.from_litellm(litellm.acompletion)` with no mode, and
/// every `from_litellm` signature in `instructor/core/client.py` defaults
/// `mode` to `instructor.Mode.TOOLS`. That is the *same*
/// unconstrained forced-tool-call path Rust uses, so Python's model is just as
/// free to emit the stray object.
///
/// Python survives it by **retrying**: instructor re-prompts with the validation
/// error, and cognee sets `MAX_RETRIES = 2`, against Rust's 5. Its Pydantic
/// `Edge` requires exactly the same three fields this one does.
///
/// So this is not parity convergence, and should not be described as such: it
/// makes Rust **more lenient than Python**, which pays for the same chunk two or
/// three times to get an array Rust now repairs in place. The strictness that is
/// preserved is the one that matters — a wholesale garbling still reaches the
/// retry loop.
///
/// # Behaviour
///
/// Entries that deserialize as an [`Edge`] are kept in order. Entries that do not
/// are dropped and counted. If the drops exceed
/// [`MAX_DROPPABLE_EDGE_FRACTION`] of the array the payload is rejected, so a
/// wholesale garbling still reaches the retry loop.
fn deserialize_edges_lenient<'de, D>(deserializer: D) -> Result<Vec<Edge>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let total = raw.len();

    let mut edges = Vec::with_capacity(total);
    let mut dropped: Vec<String> = Vec::new();
    for entry in raw {
        match serde_json::from_value::<Edge>(entry.clone()) {
            Ok(edge) => edges.push(edge),
            Err(err) => {
                // Keep a compact, non-PII-ish trace: the reason plus the keys the
                // object actually carried. The full entry can be large.
                let keys = entry
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_else(|| "<not an object>".to_string());
                dropped.push(format!("{err} (keys: {keys})"));
            }
        }
    }

    if dropped.is_empty() {
        return Ok(edges);
    }

    // `total` is non-zero here: `dropped` is only non-empty if we iterated.
    let fraction = dropped.len() as f64 / total as f64;
    if fraction > MAX_DROPPABLE_EDGE_FRACTION {
        return Err(D::Error::custom(format!(
            "{}/{total} edges were malformed ({:.0}% > {:.0}% tolerated); \
             treating the payload as broken. First: {}",
            dropped.len(),
            fraction * 100.0,
            MAX_DROPPABLE_EDGE_FRACTION * 100.0,
            dropped[0],
        )));
    }

    tracing::warn!(
        dropped = dropped.len(),
        total,
        kept = edges.len(),
        reasons = ?dropped,
        "dropped malformed entries from the extracted `edges` array; \
         keeping the rest of the graph",
    );
    Ok(edges)
}

impl KnowledgeGraph {
    /// Create a new empty knowledge graph.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Check if the graph is empty (no nodes or edges).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Get the number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get the number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

impl Default for KnowledgeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphModel for KnowledgeGraph {
    fn is_default_knowledge_graph() -> bool {
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialization() {
        let node = Node {
            id: "alice_johnson".to_string(),
            name: "Alice Johnson".to_string(),
            node_type: "PERSON".to_string(),
            description: "Software engineer at TechCorp".to_string(),
        };

        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"type\":\"PERSON\""));

        let deserialized: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.node_type, "PERSON");
    }

    #[test]
    fn test_edge_creation() {
        let edge = Edge {
            source_node_id: "alice_johnson".to_string(),
            target_node_id: "techcorp".to_string(),
            relationship_name: "works_at".to_string(),
            description: None,
        };

        assert_eq!(edge.relationship_name, "works_at");
    }

    #[test]
    fn test_edge_serializes_description() {
        let edge = Edge {
            source_node_id: "alice".to_string(),
            target_node_id: "acme".to_string(),
            relationship_name: "founded".to_string(),
            description: Some("Alice founded Acme".to_string()),
        };

        let json = serde_json::to_string(&edge).unwrap();
        assert!(json.contains("\"description\":\"Alice founded Acme\""));
    }

    #[test]
    fn test_edge_deserializes_without_description() {
        // Back-compat: JSON omitting `description` defaults to None.
        let json = r#"{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "founded"
        }"#;
        let edge: Edge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.relationship_name, "founded");
        assert_eq!(edge.description, None);
    }

    #[test]
    fn test_edge_deserializes_with_description() {
        let json = r#"{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "founded",
            "description": "Alice founded Acme"
        }"#;
        let edge: Edge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.description.as_deref(), Some("Alice founded Acme"));
    }

    #[test]
    fn test_knowledge_graph() {
        let mut graph = KnowledgeGraph::new();
        assert!(graph.is_empty());
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);

        graph.nodes.push(Node {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            node_type: "PERSON".to_string(),
            description: "A person".to_string(),
        });

        graph.edges.push(Edge {
            source_node_id: "alice".to_string(),
            target_node_id: "techcorp".to_string(),
            relationship_name: "works_at".to_string(),
            description: None,
        });

        assert!(!graph.is_empty());
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_knowledge_graph_is_default() {
        assert!(KnowledgeGraph::is_default_knowledge_graph());
    }

    /// A custom graph model for testing the `GraphModel` trait.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct CustomModel {
        items: Vec<String>,
    }

    impl GraphModel for CustomModel {}

    #[test]
    fn test_custom_model_is_not_default() {
        assert!(!CustomModel::is_default_knowledge_graph());
    }

    #[test]
    fn test_custom_model_roundtrip() {
        let model = CustomModel {
            items: vec!["a".to_string(), "b".to_string()],
        };
        let json = serde_json::to_string(&model).unwrap();
        let deserialized: CustomModel = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.items, vec!["a", "b"]);
    }

    /// The exact defect measured against `gpt-4.1-mini` and Bedrock Sonnet 4.5:
    /// a node-shaped object appended into `edges` because the model needed a node
    /// id it had not declared. Before the lenient deserializer this voided the
    /// whole chunk with "missing field `target_node_id`".
    #[test]
    fn stray_node_object_in_edges_is_dropped_not_fatal() {
        let raw = serde_json::json!({
            "nodes": [
                {"id": "Alice", "name": "Alice", "type": "PERSON", "description": "A girl."},
                {"id": "Queen", "name": "Queen of Hearts", "type": "PERSON", "description": "A queen."}
            ],
            "edges": [
                {"source_node_id": "Alice", "target_node_id": "Queen",
                 "relationship_name": "defies", "description": "Alice defies the Queen."},
                {"source_node_id": "Queen", "target_node_id": "Alice",
                 "relationship_name": "commands_attack", "description": "The Queen commands an attack."},
                {"source_node_id": "Alice", "target_node_id": "Queen",
                 "relationship_name": "answers", "description": "Alice answers the Queen."},
                {"source_node_id": "Alice", "target_node_id": "Queen",
                 "relationship_name": "faces", "description": "Alice faces the Queen."},
                // The stray: a Node wearing an Edge's first key.
                {"source_node_id": "Sister", "name": "Alice's Sister",
                 "type": "PERSON", "description": "Alice's sister, who wakes her."}
            ]
        });

        let graph: KnowledgeGraph =
            serde_json::from_value(raw).expect("stray entry must not be fatal");
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 4, "the four real edges survive");
        assert!(graph.edges.iter().all(|e| !e.target_node_id.is_empty()));
    }

    #[test]
    fn a_wholly_garbled_edges_array_still_fails() {
        let raw = serde_json::json!({
            "nodes": [],
            "edges": [
                {"source_node_id": "a", "name": "a", "type": "T", "description": "d"},
                {"source_node_id": "b", "name": "b", "type": "T", "description": "d"},
                {"source_node_id": "c", "target_node_id": "d", "relationship_name": "r"}
            ]
        });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("2/3 malformed is past the tolerance and must reach the retry loop");
        assert!(
            err.to_string().contains("treating the payload as broken"),
            "{err}"
        );
    }

    #[test]
    fn a_clean_edges_array_is_unaffected() {
        let raw = serde_json::json!({
            "nodes": [{"id": "a", "name": "A", "type": "T", "description": "d"}],
            "edges": [{"source_node_id": "a", "target_node_id": "b", "relationship_name": "r"}]
        });
        let graph: KnowledgeGraph = serde_json::from_value(raw).unwrap();
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.edges[0].description, None);
    }

    /// `edges` stays required: a payload omitting it must still drive the
    /// corrective-retry loop (the invariant the struct doc calls out).
    #[test]
    fn edges_remains_a_required_field() {
        let raw = serde_json::json!({ "nodes": [] });
        assert!(serde_json::from_value::<KnowledgeGraph>(raw).is_err());
    }

    /// One malformed entry out of four is *exactly* the tolerance, and the
    /// comparison is `>`, so it is accepted. Sitting on the boundary was
    /// untested; a future change of `>` to `>=` would silently make every
    /// four-entry payload with one stray fatal again.
    #[test]
    fn exactly_the_tolerated_fraction_is_accepted() {
        let raw = serde_json::json!({
            "nodes": [],
            "edges": [
                {"source_node_id": "a", "target_node_id": "b", "relationship_name": "r"},
                {"source_node_id": "b", "target_node_id": "c", "relationship_name": "r"},
                {"source_node_id": "c", "target_node_id": "d", "relationship_name": "r"},
                // 1 of 4 = 25.0%, which is not *greater than* 25%.
                {"source_node_id": "Sister", "name": "Alice's Sister",
                 "type": "PERSON", "description": "The stray."}
            ]
        });
        let graph: KnowledgeGraph = serde_json::from_value(raw)
            .expect("1/4 is exactly the tolerance and the comparison is strictly greater-than");
        assert_eq!(graph.edge_count(), 3);
    }

    /// One entry past the boundary — 2 of 4 — is rejected, so the accepted case
    /// above is a boundary and not an accident of the fixture.
    #[test]
    fn one_entry_past_the_boundary_is_rejected() {
        let raw = serde_json::json!({
            "nodes": [],
            "edges": [
                {"source_node_id": "a", "target_node_id": "b", "relationship_name": "r"},
                {"source_node_id": "b", "target_node_id": "c", "relationship_name": "r"},
                {"source_node_id": "x", "name": "X", "type": "PERSON", "description": "stray"},
                {"source_node_id": "y", "name": "Y", "type": "PERSON", "description": "stray"}
            ]
        });
        assert!(
            serde_json::from_value::<KnowledgeGraph>(raw).is_err(),
            "2/4 = 50% > 25%"
        );
    }

    /// The corollary of a 25% tolerance: an array of fewer than four entries has
    /// **zero** tolerance, because one drop out of three is already 33%. This is
    /// intended — on a three-edge payload there is too little left to trust —
    /// but it is worth pinning, because it is the case where "we tolerate a
    /// quarter" reads most misleadingly.
    #[test]
    fn arrays_shorter_than_four_entries_tolerate_nothing() {
        let stray = serde_json::json!({"source_node_id": "s", "name": "S", "type": "P", "description": "d"});
        let good = serde_json::json!({"source_node_id": "a", "target_node_id": "b", "relationship_name": "r"});

        for good_count in 0..3 {
            let mut edges: Vec<serde_json::Value> = vec![good.clone(); good_count];
            edges.push(stray.clone());
            let total = edges.len();
            let raw = serde_json::json!({ "nodes": [], "edges": edges });
            assert!(
                serde_json::from_value::<KnowledgeGraph>(raw).is_err(),
                "1 stray out of {total} is > 25% and must reach the retry loop"
            );
        }
    }

    /// The `edges` explanation must stay OFF the field's doc comment.
    ///
    /// `schemars` copies a field's `///` into that property's JSON-schema
    /// `description`, and this schema is sent to the model on every extraction
    /// call. Describing a private Rust deserializer there spends prompt tokens
    /// and, worse, tells the model a tolerance exists. The comment on the field
    /// is therefore a plain `//`; this test is what keeps it that way.
    #[test]
    fn the_model_facing_schema_carries_no_implementation_detail() {
        let schema = cognee_llm::schema::generate_json_schema::<KnowledgeGraph>();
        let rendered = schema.to_string();

        // Precondition: field docs really do reach the model, so the assertion
        // below is testing something. `nodes` keeps its `///` and its text is
        // in the schema.
        assert!(
            rendered.contains("serde(default)") || rendered.contains("schemars honours serde"),
            "precondition failed: field doc comments are expected to appear in the \
             generated schema, so this test would be vacuous. Schema: {rendered}"
        );

        for leak in [
            "deserialize_edges_lenient",
            "malformed",
            "MAX_DROPPABLE_EDGE_FRACTION",
        ] {
            assert!(
                !rendered.contains(leak),
                "the model-facing schema leaks the internal `{leak}`; keep the \
                 lenient-deserialization note on a `//` comment, not a `///` one"
            );
        }
    }
}
