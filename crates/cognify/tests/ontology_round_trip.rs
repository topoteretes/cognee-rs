#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: ontology round-trip with real RDF resolver.
//!
//! Uses a real `RdfLibOntologyResolver` loaded from an inline Turtle ontology
//! (no LLM needed) to verify end-to-end ontology expansion including subgraph
//! injection (ancestor nodes, is_a edges).

use std::collections::HashSet;
use uuid::Uuid;

use cognee_cognify::fact_extraction::{Edge, KnowledgeGraph, Node};
use cognee_cognify::graph_integration::expand_with_nodes_and_edges;
use cognee_ontology::{OntologyFileInput, OntologyResolver, RdfLibOntologyResolver};

const TURTLE_ONTOLOGY: &str = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix : <http://test.cognee.ai/ontology#> .

:LegalEntity a owl:Class ;
    rdfs:label "LegalEntity" .

:Organisation a owl:Class ;
    rdfs:subClassOf :LegalEntity ;
    rdfs:label "Organisation" .

:Person a owl:Class ;
    rdfs:label "Person" .

:Technology a owl:Class ;
    rdfs:label "Technology" .

:Algorithm a owl:Class ;
    rdfs:subClassOf :Technology ;
    rdfs:label "Algorithm" .
"#;

fn build_resolver() -> RdfLibOntologyResolver {
    let reader: Box<dyn std::io::Read> = Box::new(TURTLE_ONTOLOGY.as_bytes());
    RdfLibOntologyResolver::new(OntologyFileInput::Reader(reader))
        .expect("ontology should load from reader")
}

#[tokio::test]
async fn test_ontology_round_trip_with_real_resolver() {
    let resolver = build_resolver();
    assert!(resolver.is_loaded(), "Resolver should be loaded");

    // Synthetic knowledge graph (mimics LLM output)
    let graph = KnowledgeGraph {
        nodes: vec![
            Node {
                id: "tc_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A tech company".to_string(),
            },
            Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "An engineer".to_string(),
            },
            Node {
                id: "algo_1".to_string(),
                name: "DeepSort".to_string(),
                node_type: "Algorithm".to_string(),
                description: "A sorting algorithm".to_string(),
            },
        ],
        edges: vec![Edge {
            source_node_id: "alice_1".to_string(),
            target_node_id: "tc_1".to_string(),
            relationship_name: "works_at".to_string(),
            description: None,
        }],
    };

    let chunk_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    let (nodes, edges) = expand_with_nodes_and_edges(
        vec![(chunk_id, graph)],
        dataset_id,
        &HashSet::new(),
        &resolver,
        None,
    )
    .await;

    // At least 3 LLM nodes + ontology-derived ancestor nodes
    assert!(
        nodes.len() >= 3,
        "Expected at least 3 nodes (3 LLM + ontology ancestors), got {}",
        nodes.len()
    );

    // TechCorp's type "Organization" should fuzzy-match ontology class "Organisation"
    let tc_node = nodes
        .iter()
        .find(|n| n.entity.name == "TechCorp")
        .expect("TechCorp entity should exist");
    assert!(
        tc_node.entity_type.is_ontology_valid(),
        "TechCorp's type should be ontology-valid (Organization ~= Organisation)"
    );

    // Alice's type "Person" should match ontology class "Person"
    let alice_node = nodes
        .iter()
        .find(|n| n.entity.name == "Alice")
        .expect("Alice entity should exist");
    assert!(
        alice_node.entity_type.is_ontology_valid(),
        "Alice's type (Person) should be ontology-valid"
    );

    // DeepSort's type "Algorithm" should match ontology class "Algorithm"
    let algo_node = nodes
        .iter()
        .find(|n| n.entity.name == "DeepSort")
        .expect("DeepSort entity should exist");
    assert!(
        algo_node.entity_type.is_ontology_valid(),
        "DeepSort's type (Algorithm) should be ontology-valid"
    );

    // Check for is_a edges from ontology hierarchy (Organisation→LegalEntity, Algorithm→Technology)
    let is_a_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.relationship_name == "is_a")
        .collect();
    assert!(
        !is_a_edges.is_empty(),
        "Expected at least one is_a edge from ontology hierarchy"
    );

    // The works_at edge should be preserved
    let works_at: Vec<_> = edges
        .iter()
        .filter(|e| e.relationship_name == "works_at")
        .collect();
    assert_eq!(works_at.len(), 1, "works_at edge should be preserved");
    assert_eq!(works_at[0].source_entity_id, alice_node.entity.base.id);
    assert_eq!(works_at[0].target_entity_id, tc_node.entity.base.id);
}

#[tokio::test]
async fn test_ontology_unmatched_type_not_validated() {
    let resolver = build_resolver();

    // "Gadget" does not exist in the ontology
    let graph = KnowledgeGraph {
        nodes: vec![Node {
            id: "gadget_1".to_string(),
            name: "Widget".to_string(),
            node_type: "Gadget".to_string(),
            description: "An unknown type".to_string(),
        }],
        edges: vec![],
    };

    let chunk_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    let (nodes, _edges) = expand_with_nodes_and_edges(
        vec![(chunk_id, graph)],
        dataset_id,
        &HashSet::new(),
        &resolver,
        None,
    )
    .await;

    assert_eq!(nodes.len(), 1);
    let gadget = &nodes[0];
    assert_eq!(gadget.entity.name, "Widget");
    assert_eq!(gadget.entity_type.name, "Gadget");
    assert!(
        !gadget.entity_type.is_ontology_valid(),
        "Gadget should NOT be ontology-valid (not in ontology)"
    );
}

#[tokio::test]
async fn test_ontology_noop_resolver_leaves_everything_unvalidated() {
    let resolver = cognee_ontology::NoOpOntologyResolver::new();

    let graph = KnowledgeGraph {
        nodes: vec![Node {
            id: "person_1".to_string(),
            name: "Bob".to_string(),
            node_type: "Person".to_string(),
            description: "A person".to_string(),
        }],
        edges: vec![],
    };

    let chunk_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    let (nodes, _edges) = expand_with_nodes_and_edges(
        vec![(chunk_id, graph)],
        dataset_id,
        &HashSet::new(),
        &resolver,
        None,
    )
    .await;

    assert_eq!(nodes.len(), 1);
    assert!(!nodes[0].entity_type.is_ontology_valid());
    assert!(!nodes[0].entity.base.ontology_valid);
}
