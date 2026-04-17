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

        // Format matches existing Rust create_triplets_from_graph():
        // "{source_text} -\u{203a} {relationship_text}-\u{203a}{target_text}"
        let text = format!("{source_text} -\u{203a} {relationship_text}-\u{203a}{target_text}");

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

/// Build embeddable text from a graph node's properties.
///
/// Uses "name" and "description" fields, matching existing
/// create_triplets_from_graph() in triplet_creation.rs.
///
/// Format: "Name: Description" or just "Name" if description is empty.
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

        add_node(&db, src_id, "Alice", "Software engineer").await;
        add_node(&db, tgt_id, "TechCorp", "A tech company").await;
        add_edge(&db, src_id, tgt_id, "works_at").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        let t = &triplets[0];
        assert_eq!(t.source_entity_id, src_id);
        assert_eq!(t.target_entity_id, tgt_id);
        assert_eq!(t.relationship_name, "works_at");
        // Format: "Name: Description -› relationship-›Name: Description"
        assert!(t.text.contains("Alice: Software engineer"));
        assert!(t.text.contains("works_at"));
        assert!(t.text.contains("TechCorp: A tech company"));
        assert!(t.text.contains("-\u{203a}"));
    }

    #[tokio::test]
    async fn test_extract_node_without_description() {
        let db = MockGraphDB::new();
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();

        add_node(&db, src_id, "Alice", "").await;
        add_node(&db, tgt_id, "Bob", "").await;
        add_edge(&db, src_id, tgt_id, "knows").await;

        let config = MemifyConfig::default();
        let triplets = extract_triplets_from_graph_db(&db, &config).await.unwrap();

        assert_eq!(triplets.len(), 1);
        let text = &triplets[0].text;
        // Should be just the name, no colon
        assert!(text.contains("Alice"));
        assert!(text.contains("Bob"));
        assert!(!text.contains(": "), "no colon when description is empty");
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
}
