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

/// The longest an extracted **identifier** may be, in characters.
///
/// # What this defends against
///
/// Non-constrained structured output lets the model emit whatever tokens it
/// likes (see [`deserialize_edges_lenient`] for why that is also true of
/// Python). Its observed degenerate mode is a free-association loop *inside a
/// single identifier*: the payload captured during the Bedrock runaway
/// investigation (PR #210) spent **22,305 characters** — 94% of the whole answer
/// — on one `Edge.target_node_id`. That payload was valid JSON with
/// `stop_reason: end_turn`, so nothing downstream would have questioned it; only
/// a correction the model happened to append kept the graph from taking the id.
///
/// `maxLength` in the JSON schema is **not** the defence. PR #210 MEASURED it as
/// advisory rather than enforced: a `maxLength` of 5 on `Node.id` came back with
/// ids of 13 and 14 characters and no error. The bound has to be checked on the
/// way in, which is what this constant and [`check_identifier`] do.
///
/// # Why 1024
///
/// MEASURED, not guessed. Every value of every key in
/// [`BOUNDED_IDENTIFIER_KEYS`] across all 12 committed cassettes — real recorded
/// extractions over real documents, War and Peace included — was scanned. The
/// longest of each:
///
/// | key | longest observed |
/// |---|---|
/// | `id` | 43 |
/// | `name` | 43 |
/// | `type` | 20 |
/// | `source_node_id` | 37 |
/// | `target_node_id` | 43 |
/// | `relationship_name` | 39 |
/// | *(`description`, for contrast — NOT bounded)* | **798** |
///
/// So 1024 sits roughly 24x above anything a model has actually produced in a
/// bounded field, and 22x below the captured runaway — about as far from both
/// ends as a single number can be. The `description` row is why prose is
/// excluded rather than merely unexamined: the same 1024 clears real prose by
/// only 1.3x, which is not a bound, it is a tripwire.
///
/// The asymmetry of the two errors is what argues for leaving that much room.
/// Accepting an oversized id corrupts a graph; rejecting a legitimate one fails
/// the chunk — and under the shipped defaults a failed chunk fails the whole run
/// (see `tests/oversized_identifier_retry.rs` for the verified chain). A false
/// rejection is therefore much the more expensive mistake, so the bound is set
/// where a legitimate identifier cannot plausibly reach it rather than snugly
/// around observed practice.
///
/// It is deliberately not configurable: a knob here would be a knob for
/// tolerating corrupt extractions.
///
/// # This is Rust-only hardening, not parity
///
/// VERIFIED: Python has no identifier validation either — nothing in
/// `cognee/shared/data_models.py` or `cognee/modules/graph/` bounds id length,
/// so Python would have accepted the same 22k id. Do not "correct" this toward
/// Python; the divergence is the point.
const MAX_IDENTIFIER_CHARS: usize = 1024;

/// Reject an identifier too long to be one.
///
/// Counted in `char`s, not bytes, so the bound means the same thing for a name
/// written in Latin script and one written in Han — a byte bound would reject a
/// legitimate CJK name at a third of the length it allows an ASCII one.
///
/// The returned message is not just a log line: it becomes the corrective
/// instruction the adapter re-asks with (`structured_output_impl` threads the
/// validation error into the next attempt), so it names the field, the observed
/// length and the remedy. It states the length rather than echoing the value,
/// which is what keeps the re-ask prompt small — re-sending 22k junk characters
/// to the model would be the one thing less useful than not retrying.
///
/// That is a property of *this* message only, and deliberately not a claim about
/// the error a caller finally sees: when the ladder is exhausted,
/// `OpenAIAdapter::structured_output_impl` appends `". Raw: {raw}"` — the whole
/// payload — to the `DeserializationError` it surfaces. So the oversized value
/// does reach the log and the `StageFailure` by that route. That is the
/// adapter's existing behaviour for every validation miss, not something this
/// check introduces.
fn check_identifier(field: &str, value: &str) -> Result<(), String> {
    let len = value.chars().count();
    if len <= MAX_IDENTIFIER_CHARS {
        return Ok(());
    }
    Err(format!(
        "`{field}` is {len} characters long, over the {MAX_IDENTIFIER_CHARS}-character limit \
         for an identifier. Identifiers are short entity names and snake_case relationship \
         labels; put prose in `description` instead and re-extract."
    ))
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
    //
    // NOT part of the doc comment above, for the same reason the note on `edges`
    // is a `//`: `schemars` copies `///` into the model-facing schema, and the
    // model has no business knowing about a private Rust bound. Deserialized
    // through `deserialize_nodes_checked`, which rejects an oversized `Node.id`
    // — see [`MAX_IDENTIFIER_CHARS`].
    #[serde(deserialize_with = "deserialize_nodes_checked")]
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
    // It would also INVALIDATE EVERY COMMITTED STRUCTURED-OUTPUT CASSETTE —
    // that is, every recorded call that sends a schema; plain chat entries hash
    // messages only and are unaffected. `cassette::input_hash`
    // hashes the canonicalized schema alongside the messages, and while
    // `canonicalize` sorts object keys — so property *order* does not matter —
    // it serialises every value, `description` strings included. Editing or
    // adding a `///` on any type reachable from this schema therefore changes
    // the hash of every recorded call, and the replay lane
    // (`COGNEE_TEST_REPLAY=1`, which is how CI runs) fails on a cassette miss.
    // Re-record with `COGNEE_RECORD_LLM=1` when that is genuinely intended.
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

/// Every JSON key in an extracted graph that is bounded by
/// [`MAX_IDENTIFIER_CHARS`], as the model spells it on the wire.
///
/// Equivalently: every string field of [`Node`] and [`Edge`] **except**
/// `description`. That exception is the whole of the distinction, and it is
/// measured rather than asserted — across all 12 committed cassettes the longest
/// value of every key below is **43 characters**, while `description` reaches
/// **798**. A bound that comfortably clears the identifiers by 24x clears a
/// description by only 1.3x, so prose needs its own number, if it needs one at
/// all, and that is a separate decision.
///
/// `name` and `type` are in this list on evidence, not on the strength of the
/// word "identifier". `Node.id` is only the UUID5 seed and the
/// `original_node_id` metadata (`Entity::from_node`); it is `name` that becomes
/// the *stored* graph node name, the text handed to the embedder
/// (`Entity::get_embeddable_text` returns `self.name`) and the key of the
/// by-name alias map endpoint resolution falls back to. A runaway landing in
/// `name` is therefore the more damaging case, not the more innocent one.
/// `type` seeds `EntityType::from_node_type`.
const BOUNDED_IDENTIFIER_KEYS: [&str; 6] = [
    "id",
    "name",
    "type",
    "source_node_id",
    "target_node_id",
    "relationship_name",
];

/// Reject an oversized identifier anywhere in a raw extracted object.
///
/// Works on the [`serde_json::Value`] rather than on a parsed [`Node`] or
/// [`Edge`] so that the bound holds for entries that do **not** parse as either
/// — see the call site in [`deserialize_edges_lenient`], where an entry the
/// lenient path is about to drop must still be able to fail the payload.
///
/// `context` names the array for the error message (`nodes` / `edges`); the key
/// itself comes from the payload, so the model is told the field it actually
/// wrote rather than a Rust field name it has never seen.
fn check_raw_identifiers(context: &str, entry: &serde_json::Value) -> Result<(), String> {
    let Some(object) = entry.as_object() else {
        return Ok(());
    };
    for key in BOUNDED_IDENTIFIER_KEYS {
        if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
            check_identifier(&format!("{context}[].{key}"), value)?;
        }
    }
    Ok(())
}

/// Deserialize `nodes`, rejecting the whole payload if any identifier is
/// oversized.
///
/// There is no leniency here and there is none for an oversized identifier in
/// `edges` either — see [`deserialize_edges_lenient`] for why an oversized
/// identifier is categorically different from a stray entry.
fn deserialize_nodes_checked<'de, D>(deserializer: D) -> Result<Vec<Node>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    // Checked on the raw values, before typed deserialization, for the same
    // reason `edges` is: a node whose `id` is fine but whose `type` is missing
    // would otherwise fail with the less useful of its two problems.
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    for entry in &raw {
        check_raw_identifiers("nodes", entry).map_err(D::Error::custom)?;
    }
    serde_json::from_value(serde_json::Value::Array(raw)).map_err(D::Error::custom)
}

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
        // An oversized identifier is NOT a droppable entry, and must not be
        // allowed to fall into the tolerance below. A stray entry is a blemish on
        // an otherwise good answer — the model repairing its own omission. A
        // 22k-character id is the opposite: it is the free-association loop
        // itself, and in the captured payload it consumed 94% of the output
        // budget, so whatever else the answer contains was written under a
        // budget the runaway had already eaten. Keeping the "good" 49 edges of
        // such a response means building a graph out of a generation we have no
        // reason to trust. Fail here instead, and let the ladder re-ask.
        //
        // Checked BEFORE the `Edge` parse, and on the raw value, so the rule
        // holds for an entry that is about to be dropped as well as for one that
        // parses. Those two cases are not exotic and they overlap: the stray
        // object documented above is node-shaped — `source_node_id` plus `name`,
        // `type`, `description`, and no `target_node_id` — so a runaway landing
        // in the `source_node_id` of a *stray* entry is exactly the shape that
        // would otherwise be counted as merely malformed, stay under the 25%
        // tolerance, and be accepted.
        check_raw_identifiers("edges", &entry).map_err(D::Error::custom)?;

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

    /// The captured degenerate payload, reduced to its essential shape: valid
    /// JSON, `stop_reason: end_turn`, one `Edge.target_node_id` of 22,305
    /// characters. Before this check the graph would have taken the id.
    #[test]
    fn the_captured_22k_target_node_id_is_rejected() {
        let runaway = "a".repeat(22_305);
        let raw = serde_json::json!({
            "nodes": [
                {"id": "Alice", "name": "Alice", "type": "PERSON", "description": "A girl."}
            ],
            "edges": [
                {"source_node_id": "Alice", "target_node_id": runaway,
                 "relationship_name": "relates_to", "description": "d"}
            ]
        });

        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("a 22,305-character node id must not be accepted");
        let msg = err.to_string();
        assert!(msg.contains("edges[].target_node_id"), "{msg}");
        assert!(
            msg.contains("22305"),
            "the message must state the length: {msg}"
        );
        assert!(
            !msg.contains(&runaway),
            "the message must not echo the 22k value it is complaining about"
        );
    }

    /// The load-bearing interaction with [`deserialize_edges_lenient`].
    ///
    /// One oversized id among 49 good edges is 2% of the array — comfortably
    /// inside [`MAX_DROPPABLE_EDGE_FRACTION`]. If the bound were expressed as an
    /// ordinary per-entry deserialization failure it would be silently *dropped*
    /// here and the remaining 49 edges kept, which is exactly the drop-and-count
    /// handling SDK-629 considered and rejected. It must hard-fail instead.
    #[test]
    fn an_oversized_id_is_not_absorbed_by_the_edge_drop_tolerance() {
        let mut edges: Vec<serde_json::Value> = (0..49)
            .map(|i| {
                serde_json::json!({
                    "source_node_id": format!("n{i}"),
                    "target_node_id": format!("n{}", i + 1),
                    "relationship_name": "r"
                })
            })
            .collect();
        edges.push(serde_json::json!({
            "source_node_id": "n0",
            "target_node_id": "a".repeat(22_305),
            "relationship_name": "r"
        }));
        let total = edges.len();

        let raw = serde_json::json!({ "nodes": [], "edges": edges });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("1 oversized id in 50 is only 2% of the array, but it must still be fatal");
        assert!(err.to_string().contains("edges[].target_node_id"), "{err}");
        assert_eq!(total, 50, "fixture sanity: the drop tolerance is 25%");
    }

    /// An oversized `Node.id` is fatal too — `nodes` has no leniency at all.
    #[test]
    fn an_oversized_node_id_is_rejected() {
        let raw = serde_json::json!({
            "nodes": [
                {"id": "Alice", "name": "Alice", "type": "PERSON", "description": "d"},
                {"id": "b".repeat(MAX_IDENTIFIER_CHARS + 1),
                 "name": "Runaway", "type": "PERSON", "description": "d"}
            ],
            "edges": []
        });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("an oversized Node.id must reach the retry loop");
        assert!(err.to_string().contains("nodes[].id"), "{err}");
    }

    /// `relationship_name` is an identifier as much as the endpoints are — it is
    /// half of the `{source}_{target}_{relationship}` edge key.
    #[test]
    fn an_oversized_relationship_name_is_rejected() {
        let raw = serde_json::json!({
            "nodes": [],
            "edges": [{
                "source_node_id": "a",
                "target_node_id": "b",
                "relationship_name": "r".repeat(MAX_IDENTIFIER_CHARS + 1)
            }]
        });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("an oversized relationship_name must reach the retry loop");
        assert!(
            err.to_string().contains("edges[].relationship_name"),
            "{err}"
        );
    }

    /// The bound is inclusive, and it is the *only* thing that rejects: exactly
    /// `MAX_IDENTIFIER_CHARS` passes, one more fails. Without this a later
    /// `<` / `<=` slip would move the limit by one and nothing would notice.
    #[test]
    fn the_identifier_bound_is_inclusive() {
        let at_limit = "a".repeat(MAX_IDENTIFIER_CHARS);
        let over_limit = "a".repeat(MAX_IDENTIFIER_CHARS + 1);

        let build = |id: &str| {
            serde_json::json!({
                "nodes": [{"id": id, "name": "N", "type": "T", "description": "d"}],
                "edges": []
            })
        };

        assert!(
            serde_json::from_value::<KnowledgeGraph>(build(&at_limit)).is_ok(),
            "exactly {MAX_IDENTIFIER_CHARS} characters must be accepted"
        );
        assert!(
            serde_json::from_value::<KnowledgeGraph>(build(&over_limit)).is_err(),
            "{} characters must be rejected",
            MAX_IDENTIFIER_CHARS + 1
        );
    }

    /// The bound counts characters, not bytes, so a name in a non-Latin script
    /// gets the same allowance as one in ASCII. A 3-byte-per-char string of
    /// exactly `MAX_IDENTIFIER_CHARS` chars is ~3 KiB and must still pass.
    #[test]
    fn the_bound_counts_characters_not_bytes() {
        let han = "字".repeat(MAX_IDENTIFIER_CHARS);
        assert!(han.len() > MAX_IDENTIFIER_CHARS, "fixture is multi-byte");

        let raw = serde_json::json!({
            "nodes": [{"id": han, "name": "N", "type": "T", "description": "d"}],
            "edges": []
        });
        assert!(
            serde_json::from_value::<KnowledgeGraph>(raw).is_ok(),
            "a byte-counted bound would reject this at a third of the allowance"
        );
    }

    /// Ordinary identifiers are untouched — the check must not become a cost on
    /// the normal path or a source of false rejections.
    #[test]
    fn realistic_identifiers_are_unaffected() {
        let raw = serde_json::json!({
            "nodes": [
                {"id": "Albert Einstein", "name": "Albert Einstein",
                 "type": "PERSON", "description": "A physicist."},
                {"id": "Institute for Advanced Study (Princeton, New Jersey)",
                 "name": "IAS", "type": "ORGANIZATION", "description": "A research institute."}
            ],
            "edges": [{
                "source_node_id": "Albert Einstein",
                "target_node_id": "Institute for Advanced Study (Princeton, New Jersey)",
                "relationship_name": "worked_at"
            }]
        });
        let graph: KnowledgeGraph = serde_json::from_value(raw).unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);
    }

    /// `description` is prose, not an identifier, and is deliberately NOT bound
    /// — on both `Node` and `Edge`, the only two fields left out.
    ///
    /// Pinned because it is the obvious next thing to "fix", and because doing so
    /// at *this* bound would be actively wrong: the longest `description` in the
    /// committed cassettes is 798 characters, so 1024 clears real prose by 1.3x
    /// where it clears a real identifier by 24x. Bounding free text needs its own
    /// number. A reviewer who wants it should have to delete this test.
    #[test]
    fn description_is_deliberately_unbounded() {
        let long_prose = "word ".repeat(MAX_IDENTIFIER_CHARS);
        let raw = serde_json::json!({
            "nodes": [{"id": "a", "name": "A", "type": "T", "description": long_prose}],
            "edges": [{
                "source_node_id": "a", "target_node_id": "a",
                "relationship_name": "r", "description": "word ".repeat(MAX_IDENTIFIER_CHARS)
            }]
        });
        assert!(
            serde_json::from_value::<KnowledgeGraph>(raw).is_ok(),
            "only identifiers are bounded; widening to free text is a separate decision"
        );
    }

    /// `Node.name` is bounded, and this is the case that matters most.
    ///
    /// `Node.id` is only the UUID5 seed and `original_node_id` metadata
    /// (`Entity::from_node`). It is `name` that becomes the stored graph node
    /// name, the text handed to the embedder (`Entity::get_embeddable_text`) and
    /// the by-name alias key endpoint resolution falls back to — so a runaway
    /// landing there is the *more* damaging case. Bounding `id` alone would have
    /// been one field short of where the corruption lands.
    #[test]
    fn an_oversized_node_name_is_rejected() {
        let raw = serde_json::json!({
            "nodes": [{
                "id": "Alice",
                "name": "a".repeat(22_305),
                "type": "PERSON",
                "description": "A girl."
            }],
            "edges": []
        });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("a 22k node name would be stored and embedded verbatim");
        assert!(err.to_string().contains("nodes[].name"), "{err}");
    }

    /// `Node.type` is bounded too — it seeds `EntityType::from_node_type`.
    #[test]
    fn an_oversized_node_type_is_rejected() {
        let raw = serde_json::json!({
            "nodes": [{
                "id": "Alice", "name": "Alice",
                "type": "T".repeat(MAX_IDENTIFIER_CHARS + 1),
                "description": "A girl."
            }],
            "edges": []
        });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("an oversized node type must reach the retry loop");
        assert!(err.to_string().contains("nodes[].type"), "{err}");
    }

    /// The gap the raw-value check closes: a runaway inside an entry that is
    /// *also* malformed as an [`Edge`].
    ///
    /// This is not a contrived shape — it is the documented stray entry, a
    /// node-shaped object in `edges` with no `target_node_id`. Checking only
    /// after a successful `Edge` parse would count this as merely "malformed",
    /// leave it under the 25% tolerance, and accept the payload: the exact
    /// drop-and-count outcome the hard failure exists to prevent, reached by the
    /// one path that skips it.
    #[test]
    fn an_oversized_id_in_a_droppable_entry_is_still_fatal() {
        let mut edges: Vec<serde_json::Value> = (0..49)
            .map(|i| {
                serde_json::json!({
                    "source_node_id": format!("n{i}"),
                    "target_node_id": format!("n{}", i + 1),
                    "relationship_name": "r"
                })
            })
            .collect();
        // A stray node-shaped object — no `target_node_id`, no
        // `relationship_name`, so it does NOT deserialize as an `Edge` — whose
        // `source_node_id` is the runaway.
        edges.push(serde_json::json!({
            "source_node_id": "a".repeat(22_305),
            "name": "Alice's Sister",
            "type": "PERSON",
            "description": "The stray."
        }));

        let raw = serde_json::json!({ "nodes": [], "edges": edges });
        let err = serde_json::from_value::<KnowledgeGraph>(raw)
            .expect_err("a runaway must be fatal even in an entry the lenient path would drop");
        assert!(err.to_string().contains("edges[].source_node_id"), "{err}");
    }

    /// The converse: closing that gap must not have made the lenient path strict.
    /// A stray entry with ordinary-length values is still dropped, not fatal.
    #[test]
    fn a_stray_entry_with_sane_values_is_still_merely_dropped() {
        let raw = serde_json::json!({
            "nodes": [],
            "edges": [
                {"source_node_id": "a", "target_node_id": "b", "relationship_name": "r"},
                {"source_node_id": "b", "target_node_id": "c", "relationship_name": "r"},
                {"source_node_id": "c", "target_node_id": "d", "relationship_name": "r"},
                {"source_node_id": "Sister", "name": "Alice's Sister",
                 "type": "PERSON", "description": "The stray."}
            ]
        });
        let graph: KnowledgeGraph = serde_json::from_value(raw)
            .expect("the raw identifier check must not have made the drop path strict");
        assert_eq!(graph.edge_count(), 3);
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
            // Same rule for the identifier bound (SDK-629). Telling the model
            // about it would also be useless: PR #210 MEASURED schema-declared
            // `maxLength` as advisory, which is why the bound is enforced on the
            // way in rather than advertised.
            "deserialize_nodes_checked",
            "MAX_IDENTIFIER_CHARS",
            "characters long",
        ] {
            assert!(
                !rendered.contains(leak),
                "the model-facing schema leaks the internal `{leak}`; keep the \
                 lenient-deserialization note on a `//` comment, not a `///` one"
            );
        }

        // The identifier check must not have perturbed the schema at all: a
        // changed schema rehashes every committed structured-output cassette
        // (`cassette::input_hash` canonicalizes the schema alongside the
        // messages) and the `COGNEE_TEST_REPLAY=1` lane then fails on a miss.
        // `deserialize_with` is invisible to `schemars` — `edges` has carried one
        // since the lenient deserializer landed — and this pins that it stays so.
        assert!(
            !rendered.contains("maxLength"),
            "the bound is enforced in Rust, not declared in the schema: adding \
             `maxLength` would invalidate every committed cassette and PR #210 \
             measured it as advisory anyway. Schema: {rendered}"
        );
        for required in ["nodes", "edges"] {
            assert!(
                schema["required"]
                    .as_array()
                    .expect("`required` is an array")
                    .iter()
                    .any(|v| v.as_str() == Some(required)),
                "`{required}` must stay in the schema's `required` array — a \
                 `deserialize_with` must not have made it look optional"
            );
        }
    }

    /// The schema must list properties in DECLARATION order, not alphabetically.
    ///
    /// This is the single highest-impact finding of the Bedrock runaway
    /// investigation, and it is invisible in code review — the schema is
    /// semantically identical either way, so only this test can hold it.
    ///
    /// `schemars` keys its `properties` map on [`schemars::Map`], which is a
    /// `BTreeMap` (alphabetical) unless the `preserve_order` feature is on, in
    /// which case it is an `IndexMap` (declaration order). The workspace
    /// `Cargo.toml` turns that feature on **for this reason and no other**.
    ///
    /// Why it matters. Alphabetical order puts `Edge.target_node_id` LAST, after
    /// the free-text `description`, and `Node.description` FIRST. Declaration
    /// order (which is also Pydantic's, hence Python's) writes the two endpoint
    /// ids adjacently and defers free text to the end. Measured on Bedrock
    /// Sonnet 4.5, 250 calls, one runaway-prone chunk held fixed:
    ///
    /// | properties order | truncated at an 8192 cap | max output |
    /// |---|---|---|
    /// | alphabetical (schemars default) | 13/68 = 19% | 8192 (censored) |
    /// | declaration (this) | **0/58 = 0%** | 2732 |
    ///
    /// Fisher exact two-sided p = 0.0002 pooled; p = 0.0057 for the baseline
    /// against exactly the shape this test pins. The degenerate mode it removes
    /// is a free-association loop inside `Edge.target_node_id` — one captured
    /// payload spent 22,305 chars (94% of the answer) on a single node id.
    ///
    /// If a future schemars bump drops `preserve_order`, or someone reorders the
    /// fields, this test is the only thing that will notice.
    #[test]
    fn schema_properties_are_in_declaration_order() {
        let schema = cognee_llm::schema::generate_json_schema::<KnowledgeGraph>();

        fn keys(v: &serde_json::Value) -> Vec<&str> {
            v.as_object()
                .expect("every `properties` value is a JSON object")
                .keys()
                .map(String::as_str)
                .collect()
        }

        assert_eq!(
            keys(&schema["definitions"]["Node"]["properties"]),
            ["id", "name", "type", "description"],
            "Node properties must be in declaration order — see this test's docs"
        );
        assert_eq!(
            keys(&schema["definitions"]["Edge"]["properties"]),
            [
                "source_node_id",
                "target_node_id",
                "relationship_name",
                "description"
            ],
            "Edge properties must be in declaration order: the two endpoint ids \
             adjacent, free text last — see this test's docs"
        );
        assert_eq!(
            keys(&schema["properties"]),
            ["nodes", "edges"],
            "nodes must precede edges: the model cannot reference a node id in an \
             edge it has not declared yet"
        );
    }
}
