use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, NodeData};
use cognee_models::Triplet;
use std::borrow::Cow;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

use super::config::MemifyConfig;
use super::error::MemifyError;

/// Extract triplets from an existing graph database.
///
/// Reads all nodes and edges (or a filtered subgraph) via GraphDBTrait
/// and constructs Triplet objects with embeddable text.
///
/// Rust equivalent of Python's get_triplet_datapoints()
/// (cognee/tasks/memify/get_triplet_datapoints.py:169).
pub async fn extract_triplets_from_graph_db(
    graph_db: &dyn GraphDBTrait,
    config: &MemifyConfig,
) -> Result<Vec<Triplet>, MemifyError> {
    // Step 1: Read graph data (full or filtered)
    let (nodes, edges) = read_graph_data(graph_db, config).await?;

    info!(
        node_count = nodes.len(),
        edge_count = edges.len(),
        "Read graph data for triplet extraction"
    );

    if edges.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Build node lookup: node_id -> NodeData
    let node_map: HashMap<&str, &NodeData> =
        nodes.iter().map(|(id, data)| (id.as_str(), data)).collect();

    // Step 3: Build triplets from edges
    let mut triplets = Vec::new();
    let mut skipped = 0usize;

    for (source_id, target_id, relationship_name, edge_props) in &edges {
        let source = match node_map.get(source_id.as_str()) {
            Some(data) => *data,
            None => {
                skipped += 1;
                continue;
            }
        };
        let target = match node_map.get(target_id.as_str()) {
            Some(data) => *data,
            None => {
                skipped += 1;
                continue;
            }
        };

        let source_text = build_node_text(source);
        let target_text = build_node_text(target);
        let relationship_text = extract_relationship_text(edge_props, relationship_name);

        if source_text.is_empty() && relationship_text.is_empty() && target_text.is_empty() {
            skipped += 1;
            continue;
        }

        // Format matches Python's canonical triplet text:
        // f"{start_node_text}-›{relationship_text}-›{end_node_text}".strip()
        // (get_triplet_datapoints.py:157).
        // Each endpoint's text is derived from its type's index_fields
        // (e.g. Entity → "name" only, not "name: description"), so that
        // cross-SDK embedding vectors are byte-identical.
        let text = format!("{source_text}-\u{203a}{relationship_text}-\u{203a}{target_text}");

        let source_uuid = parse_node_uuid(source_id)?;
        let target_uuid = parse_node_uuid(target_id)?;

        let triplet = Triplet::new(source_uuid, target_uuid, relationship_name.clone(), text)
            .with_names(
                extract_string_prop(source, "name"),
                extract_string_prop(target, "name"),
            );

        triplets.push(triplet);
    }

    if skipped > 0 {
        warn!(skipped, "Skipped edges (missing nodes or empty text)");
    }

    Ok(triplets)
}

/// Read graph data, applying filters from config if present.
async fn read_graph_data(
    graph_db: &dyn GraphDBTrait,
    config: &MemifyConfig,
) -> Result<(Vec<GraphNode>, Vec<EdgeData>), MemifyError> {
    match (&config.node_type_filter, &config.node_name_filter) {
        (Some(node_type), Some(node_names)) => graph_db
            .get_nodeset_subgraph(node_type, node_names, &config.node_name_filter_operator)
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string())),
        _ => graph_db
            .get_graph_data()
            .await
            .map_err(|e| MemifyError::GraphDBError(e.to_string())),
    }
}

/// Map a DataPoint type name to its `index_fields`, mirroring Python's
/// `_build_datapoint_type_index_mapping` (get_triplet_datapoints.py:13-41).
///
/// Cross-SDK triplet vectors are byte-comparable only when both sides embed
/// the same text. Python derives node text from `index_fields` (e.g. `Entity`
/// contributes only `name`; `DocumentChunk` contributes only `text`).
/// Unknown types return an empty slice, which produces an empty string, and
/// the caller's all-empty guard skips the triplet (mirroring Python:
/// get_triplet_datapoints.py:151-155).
fn index_fields_for_type(node_type: &str) -> &'static [&'static str] {
    match node_type {
        "Entity" | "EntityType" | "TextDocument" => &["name"],
        "DocumentChunk" | "TextSummary" | "Triplet" => &["text"],
        _ => &[],
    }
}

/// Build embeddable text from a graph node's properties using `index_fields`.
///
/// Mirrors Python's `_extract_embeddable_text` (get_triplet_datapoints.py:44-69):
/// reads the node's `type` property, looks up its index_fields, extracts and
/// trims each field value, drops empties, then joins with a single space.
///
/// Examples (Python-compatible):
///   Entity   {name="Alice", description="engineer"} → "Alice"
///   EntityType {name="Person"} → "Person"
///   DocumentChunk {text="hello world"} → "hello world"
///   unknown type → "" (caller skips if all three parts are empty)
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

/// Extract relationship text from edge properties.
///
/// Tries "edge_text" property first (matching Python's
/// _extract_relationship_text), falls back to relationship_name.
fn extract_relationship_text(
    edge_props: &HashMap<Cow<'static, str>, serde_json::Value>,
    relationship_name: &str,
) -> String {
    edge_props
        .get("edge_text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(relationship_name)
        .to_string()
}

/// Extract a string property from NodeData.
fn extract_string_prop(data: &NodeData, key: &str) -> String {
    data.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Parse a node ID string as UUID.
fn parse_node_uuid(id: &str) -> Result<Uuid, MemifyError> {
    Uuid::parse_str(id)
        .map_err(|e| MemifyError::GraphDBError(format!("Invalid node UUID '{id}': {e}")))
}

#[cfg(all(test, feature = "testing"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_graph::MockGraphDB;
    use serde_json::json;

    /// Helper: add a node with name and description to the mock graph.
    async fn add_node(db: &MockGraphDB, id: Uuid, name: &str, description: &str) {
        let mut node_json = serde_json::Map::new();
        node_json.insert("id".to_string(), json!(id.to_string()));
        node_json.insert("name".to_string(), json!(name));
        if !description.is_empty() {
            node_json.insert("description".to_string(), json!(description));
        }
        db.add_node_raw(serde_json::Value::Object(node_json))
            .await
            .unwrap();
    }

    /// Helper: add a typed node (with `type` property, needed for filter tests).
    async fn add_typed_node(
        db: &MockGraphDB,
        id: Uuid,
        name: &str,
        node_type: &str,
        description: &str,
    ) {
        let mut node_json = serde_json::Map::new();
        node_json.insert("id".to_string(), json!(id.to_string()));
        node_json.insert("name".to_string(), json!(name));
        node_json.insert("type".to_string(), json!(node_type));
        if !description.is_empty() {
            node_json.insert("description".to_string(), json!(description));
        }
        db.add_node_raw(serde_json::Value::Object(node_json))
            .await
            .unwrap();
    }

    /// Helper: add an edge between two nodes.
    async fn add_edge(db: &MockGraphDB, source: Uuid, target: Uuid, relationship: &str) {
        db.add_edge(&source.to_string(), &target.to_string(), relationship, None)
            .await
            .unwrap();
    }

    /// Seed a graph used by the filter tests.
    ///
    /// - 3 nodes with type=Entity: Alice, Bob, Carol
    /// - 1 node with type=Concept: Idea1
    /// - Edges:
    ///   Alice --knows--> Bob,
    ///   Bob   --knows--> Carol,
    ///   Alice --likes--> Idea1
    ///
    /// Returns (alice, bob, carol, idea1).
    async fn seed_filter_graph(db: &MockGraphDB) -> (Uuid, Uuid, Uuid, Uuid) {
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let carol = Uuid::new_v4();
        let idea1 = Uuid::new_v4();

        add_typed_node(db, alice, "Alice", "Entity", "Person A").await;
        add_typed_node(db, bob, "Bob", "Entity", "Person B").await;
        add_typed_node(db, carol, "Carol", "Entity", "Person C").await;
        add_typed_node(db, idea1, "Idea1", "Concept", "An idea").await;

        add_edge(db, alice, bob, "knows").await;
        add_edge(db, bob, carol, "knows").await;
        add_edge(db, alice, idea1, "likes").await;

        (alice, bob, carol, idea1)
    }

    #[tokio::test]
    async fn test_extract_empty_graph() {
        let db = MockGraphDB::new();
        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();
        assert!(triplets.is_empty());
    }

    #[tokio::test]
    async fn test_extract_basic_triplet() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();

        // Nodes have type="Entity" so index_fields=["name"] applies.
        // Description is ignored: Python's _extract_embeddable_text uses only name.
        add_typed_node(&db, src_id, "Alice", "Entity", "Software engineer").await;
        add_typed_node(&db, tgt_id, "TechCorp", "Entity", "A tech company").await;
        add_edge(&db, src_id, tgt_id, "works_at").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        let t = &triplets[0];
        assert_eq!(t.source_entity_id, src_id);
        assert_eq!(t.target_entity_id, tgt_id);
        assert_eq!(t.relationship_name, "works_at");
        // New Python-matching format: name only, no description.
        // Entity index_fields=["name"] → "Alice" not "Alice: Software engineer".
        assert!(t.text.contains("Alice"));
        assert!(
            !t.text.contains("Alice: Software engineer"),
            "description must NOT appear"
        );
        assert!(t.text.contains("works_at"));
        assert!(t.text.contains("TechCorp"));
        assert!(
            !t.text.contains("TechCorp: A tech company"),
            "description must NOT appear"
        );
        assert!(t.text.contains("-\u{203a}"));
    }

    #[tokio::test]
    async fn test_extract_node_without_description() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();

        // type="Entity" → index_fields=["name"] → name-only text, no colon.
        add_typed_node(&db, src_id, "Alice", "Entity", "").await;
        add_typed_node(&db, tgt_id, "Bob", "Entity", "").await;
        add_edge(&db, src_id, tgt_id, "knows").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        let text = &triplets[0].text;
        // Entity index_fields=["name"] → just the name, no colon.
        assert!(text.contains("Alice"));
        assert!(text.contains("Bob"));
        assert!(
            !text.contains(": "),
            "no colon when type=Entity (name-only index_fields)"
        );
    }

    #[tokio::test]
    async fn test_extract_skips_orphaned_edges() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let missing_id = Uuid::new_v4();

        add_node(&db, src_id, "Alice", "A person").await;
        // Edge references a node not in the graph
        add_edge(&db, src_id, missing_id, "knows").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();
        assert!(
            triplets.is_empty(),
            "should skip edges with missing target node"
        );
    }

    #[tokio::test]
    async fn test_extract_multiple_triplets() {
        let db = MockGraphDB::new();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();

        add_node(&db, id_a, "A", "Entity A").await;
        add_node(&db, id_b, "B", "Entity B").await;
        add_node(&db, id_c, "C", "Entity C").await;
        add_edge(&db, id_a, id_b, "r1").await;
        add_edge(&db, id_b, id_c, "r2").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();
        assert_eq!(triplets.len(), 2);
    }

    #[tokio::test]
    async fn test_extract_triplet_id_deterministic() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();

        add_node(&db, src_id, "X", "desc").await;
        add_node(&db, tgt_id, "Y", "desc").await;
        add_edge(&db, src_id, tgt_id, "rel").await;

        let config = MemifyConfig::default();
        let t1 = extract_triplets_from_graph_db(&db, &config).await.unwrap();
        let t2 = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(t1.len(), 1);
        assert_eq!(t2.len(), 1);
        assert_eq!(t1[0].id, t2[0].id, "same input should produce same ID");
    }

    /// With both type and name filters set, the subgraph code path must be
    /// invoked (not the full-graph default path).
    #[tokio::test]
    async fn test_extract_subgraph_path_is_invoked() {
        let db = MockGraphDB::new();
        let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&db).await;

        let config = MemifyConfig::default()
            .with_node_type_filter("Entity".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()]);

        let _ = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        let log = db.get_call_log();
        assert!(
            log.iter().any(|m| m == "get_nodeset_subgraph"),
            "expected get_nodeset_subgraph to be invoked, got call log: {log:?}"
        );
        assert!(
            !log.iter().any(|m| m == "get_graph_data"),
            "expected get_graph_data NOT to be invoked, got call log: {log:?}"
        );
    }

    /// With no filters, the default get_graph_data path must be invoked
    /// (not the subgraph path).
    #[tokio::test]
    async fn test_extract_default_path_is_invoked() {
        let db = MockGraphDB::new();
        let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&db).await;

        let config = MemifyConfig::default();

        let _ = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        let log = db.get_call_log();
        assert!(
            log.iter().any(|m| m == "get_graph_data"),
            "expected get_graph_data to be invoked, got call log: {log:?}"
        );
        assert!(
            !log.iter().any(|m| m == "get_nodeset_subgraph"),
            "expected get_nodeset_subgraph NOT to be invoked, got call log: {log:?}"
        );
    }

    /// OR semantics: primaries ∪ all neighbors of any primary.
    ///
    /// Seed: Alice-knows->Bob, Bob-knows->Carol, Alice-likes->Idea1.
    /// Filter type=Entity, names=[Alice, Bob], OR.
    ///
    /// Primaries = {Alice, Bob}.
    /// Neighbors of Alice or Bob = {Carol, Idea1, plus Alice/Bob themselves}.
    /// Included nodes = {Alice, Bob, Carol, Idea1}.
    /// Edges with both endpoints in included = all 3 → 3 triplets.
    #[tokio::test]
    async fn test_extract_with_node_type_and_names_or() {
        let db = MockGraphDB::new();
        let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&db).await;

        let config = MemifyConfig::default()
            .with_node_type_filter("Entity".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
            .with_node_name_filter_operator("OR".to_string());

        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(
            triplets.len(),
            3,
            "OR filter should include all 3 edges (Alice-knows-Bob, Bob-knows-Carol, Alice-likes-Idea1)"
        );

        // Every triplet must have at least one endpoint among the primaries,
        // because all neighbors in this seed are reached directly via an edge
        // incident to a primary.
        let relationships: std::collections::HashSet<&str> = triplets
            .iter()
            .map(|t| t.relationship_name.as_str())
            .collect();
        assert!(relationships.contains("knows"));
        assert!(relationships.contains("likes"));
    }

    /// AND semantics: primaries ∪ nodes that are neighbors of EVERY primary.
    ///
    /// Seed: Alice-knows->Bob, Bob-knows->Carol, Alice-likes->Idea1.
    /// Filter type=Entity, names=[Alice, Bob], AND.
    ///
    /// Primaries = {Alice, Bob}.
    /// AND-neighbors (connected to BOTH Alice and Bob):
    ///   - Carol: neighbor of Bob only → excluded
    ///   - Idea1: neighbor of Alice only → excluded
    /// Included nodes = {Alice, Bob}.
    /// Edges with both endpoints in included = {Alice-knows-Bob} → 1 triplet.
    #[tokio::test]
    async fn test_extract_with_node_type_and_names_and() {
        let db = MockGraphDB::new();
        let (alice, bob, _carol, _idea1) = seed_filter_graph(&db).await;

        let config = MemifyConfig::default()
            .with_node_type_filter("Entity".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
            .with_node_name_filter_operator("AND".to_string());

        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(
            triplets.len(),
            1,
            "AND filter should include only the Alice-knows-Bob edge"
        );
        let t = &triplets[0];
        assert_eq!(t.source_entity_id, alice);
        assert_eq!(t.target_entity_id, bob);
        assert_eq!(t.relationship_name, "knows");
    }

    /// A filter that matches nothing should return an empty triplet set
    /// without error.
    #[tokio::test]
    async fn test_extract_with_filter_empty_result() {
        let db = MockGraphDB::new();
        let (_alice, _bob, _carol, _idea1) = seed_filter_graph(&db).await;

        let config = MemifyConfig::default()
            .with_node_type_filter("NonexistentType".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()]);

        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert!(
            triplets.is_empty(),
            "filters referencing a nonexistent type should yield no triplets"
        );
    }

    /// Self-loop: a single node with an edge to itself produces exactly one
    /// triplet whose source and target UUIDs are equal. The extractor does
    /// not de-duplicate or reject self-loops.
    #[tokio::test]
    async fn test_extract_circular_self_loop() {
        let db = MockGraphDB::new();
        let node_id = Uuid::new_v4();

        add_node(&db, node_id, "Ouroboros", "A snake eating its tail").await;
        add_edge(&db, node_id, node_id, "relates_to").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1, "self-loops must produce one triplet");
        let t = &triplets[0];
        assert_eq!(
            t.source_entity_id, t.target_entity_id,
            "self-loop source and target IDs must be equal"
        );
        assert_eq!(t.source_entity_id, node_id);
        assert_eq!(t.relationship_name, "relates_to");
    }

    /// Helper: add a node populated from a raw JSON builder closure so tests
    /// can control exactly which property keys are present (e.g. description
    /// without name, or name without description).
    async fn add_node_with_props(db: &MockGraphDB, id: Uuid, props: serde_json::Value) {
        let mut node_json = match props {
            serde_json::Value::Object(m) => m,
            _ => panic!("props must be a JSON object"),
        };
        node_json.insert("id".to_string(), json!(id.to_string()));
        db.add_node_raw(serde_json::Value::Object(node_json))
            .await
            .unwrap();
    }

    /// Covers nodes whose `type` property is absent or unknown.
    ///
    /// With the index_fields-driven `build_node_text`, a node with no `type`
    /// gets `index_fields_for_type("") == &[]`, so its embeddable text is `""`.
    /// Python's `_extract_embeddable_text` returns `""` for unknown types too
    /// (get_triplet_datapoints.py:126-136: empty index_fields → empty text).
    ///
    /// Sub-cases:
    /// 1. Node A has `description` only, no `name`, no `type` → text = "".
    ///    Node B has `name`+`description`, no `type` → text = "".
    ///    relationship_text = "knows" (non-empty), so the triplet is NOT
    ///    skipped (not all three parts are empty).
    ///    Resulting text: "-›knows-›" (both node texts are empty).
    /// 2. Node C has `name`+`type="Entity"`, Node D has `name`+`type="Entity"`.
    ///    Entity index_fields=["name"] → text = name only (no colon, no description).
    #[tokio::test]
    async fn test_extract_node_missing_name_field() {
        // --- Sub-case 1: nodes with no `type` → unknown type → empty node text ---
        let db1 = MockGraphDB::new();
        let a_id = Uuid::new_v4();
        let b_id = Uuid::new_v4();

        // Neither node has a `type` property → index_fields = [] → text = "".
        add_node_with_props(&db1, a_id, json!({ "description": "Some description" })).await;
        add_node(&db1, b_id, "Bob", "A person").await;
        add_edge(&db1, a_id, b_id, "knows").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db1, &config).await.unwrap();

        assert_eq!(
            triplets.len(),
            1,
            "edge must NOT be skipped when relationship text is non-empty \
             even if both node texts are empty"
        );
        let t = &triplets[0];
        assert_eq!(t.source_entity_id, a_id);
        assert_eq!(t.target_entity_id, b_id);
        assert_eq!(t.relationship_name, "knows");
        // Both node texts are "" (unknown type), relationship_text = "knows".
        // Python-matching format: "-›knows-›"
        assert_eq!(
            t.text, "-\u{203a}knows-\u{203a}",
            "unknown-type nodes produce empty text → '-›rel-›' format"
        );

        // --- Sub-case 2: typed Entity nodes → index_fields=["name"] → name only ---
        let db2 = MockGraphDB::new();
        let c_id = Uuid::new_v4();
        let d_id = Uuid::new_v4();

        add_typed_node(&db2, c_id, "Carol", "Entity", "").await;
        add_typed_node(&db2, d_id, "Dave", "Entity", "").await;
        add_edge(&db2, c_id, d_id, "knows").await;

        let triplets2 = extract_triplets_from_graph_db(&db2, &config).await.unwrap();
        assert_eq!(triplets2.len(), 1);
        let t2 = &triplets2[0];
        // Entity index_fields=["name"] → bare name, no colon, no description.
        assert_eq!(
            t2.text, "Carol-\u{203a}knows-\u{203a}Dave",
            "Entity nodes must produce name-only text with no colon"
        );
        assert!(
            !t2.text.contains(": "),
            "Entity node text must not contain ': ', got: {text:?}",
            text = t2.text
        );
    }

    /// All three text components empty → edge is skipped (empty Vec returned).
    #[tokio::test]
    async fn test_extract_edge_all_empty_fields_skipped() {
        let db = MockGraphDB::new();
        let a_id = Uuid::new_v4();
        let b_id = Uuid::new_v4();

        // Nodes with no name and no description: text will be empty.
        add_node_with_props(&db, a_id, json!({})).await;
        add_node_with_props(&db, b_id, json!({})).await;
        // Edge with empty relationship_name AND empty edge_text.
        let mut props: HashMap<Cow<'static, str>, serde_json::Value> = HashMap::new();
        props.insert(Cow::Borrowed("edge_text"), json!(""));
        db.add_edge(&a_id.to_string(), &b_id.to_string(), "", Some(props))
            .await
            .unwrap();

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert!(
            triplets.is_empty(),
            "edge with all three text components empty must be skipped, got: {triplets:?}"
        );
    }

    /// Pins the exact triplet text format:
    ///   "{source_name}-\u{203a}{rel}-\u{203a}{target_name}"
    ///
    /// Matches Python's canonical form at get_triplet_datapoints.py:157.
    /// Entity nodes use index_fields=["name"], so description is excluded.
    /// Any future refactor that drifts from this format will change the
    /// embedding input string and break cross-SDK vector comparability.
    #[tokio::test]
    async fn test_extract_triplet_text_format() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();

        // type="Entity" → index_fields=["name"] → only "name" is embedded.
        add_typed_node(&db, src_id, "Alice", "Entity", "engineer").await;
        add_typed_node(&db, tgt_id, "TechCorp", "Entity", "tech").await;
        add_edge(&db, src_id, tgt_id, "works_at").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        // Python: _extract_embeddable_text(entity_node, ["name"]) → "Alice"
        // Python line 157: f"{start_node_text}-›{relationship_text}-›{end_node_text}".strip()
        assert_eq!(triplets[0].text, "Alice-\u{203a}works_at-\u{203a}TechCorp",);
    }

    /// Verifies `index_fields_for_type` returns the correct fields for each
    /// known type, matching Python's `_build_datapoint_type_index_mapping`
    /// (get_triplet_datapoints.py:13-41).
    #[test]
    fn test_index_fields_for_type() {
        // Entity and EntityType → ["name"]
        assert_eq!(index_fields_for_type("Entity"), &["name"]);
        assert_eq!(index_fields_for_type("EntityType"), &["name"]);
        assert_eq!(index_fields_for_type("TextDocument"), &["name"]);

        // DocumentChunk, TextSummary, Triplet → ["text"]
        assert_eq!(index_fields_for_type("DocumentChunk"), &["text"]);
        assert_eq!(index_fields_for_type("TextSummary"), &["text"]);
        assert_eq!(index_fields_for_type("Triplet"), &["text"]);

        // Unknown types → []
        assert_eq!(index_fields_for_type(""), &[] as &[&str]);
        assert_eq!(index_fields_for_type("UnknownType"), &[] as &[&str]);
    }

    /// Verifies that Entity node text is name-only (no description),
    /// and DocumentChunk node text uses the `text` field.
    /// Required by task 15 step 3 / B4.1 acceptance criterion.
    #[tokio::test]
    async fn test_index_fields_entity_name_only_documentchunk_text() {
        let db = MockGraphDB::new();
        let entity_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();

        // Entity: name="Alice", description="engineer" → text must be "Alice" only.
        let mut entity_json = serde_json::Map::new();
        entity_json.insert("id".to_string(), json!(entity_id.to_string()));
        entity_json.insert("type".to_string(), json!("Entity"));
        entity_json.insert("name".to_string(), json!("Alice"));
        entity_json.insert("description".to_string(), json!("engineer"));
        db.add_node_raw(serde_json::Value::Object(entity_json))
            .await
            .unwrap();

        // DocumentChunk: text="hello world" → text must be "hello world".
        let mut chunk_json = serde_json::Map::new();
        chunk_json.insert("id".to_string(), json!(chunk_id.to_string()));
        chunk_json.insert("type".to_string(), json!("DocumentChunk"));
        chunk_json.insert("text".to_string(), json!("hello world"));
        chunk_json.insert("name".to_string(), json!("irrelevant"));
        db.add_node_raw(serde_json::Value::Object(chunk_json))
            .await
            .unwrap();

        db.add_edge(
            &entity_id.to_string(),
            &chunk_id.to_string(),
            "contains",
            None,
        )
        .await
        .unwrap();

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        let text = &triplets[0].text;

        // Entity → name-only; DocumentChunk → text field only.
        assert!(
            text.starts_with("Alice-\u{203a}"),
            "Entity source must use name only, got: {text:?}"
        );
        assert!(
            !text.contains("Alice: engineer"),
            "Entity must NOT include description, got: {text:?}"
        );
        assert!(
            text.ends_with("-\u{203a}hello world"),
            "DocumentChunk target must use text field, got: {text:?}"
        );
    }
}
