//! Integration tests for NoOpOntologyResolver.
//!
//! These tests verify that the no-op resolver correctly implements
//! the pass-through behavior expected when no ontology is loaded.

use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};

#[test]
fn test_noop_resolver_not_loaded() {
    let resolver = NoOpOntologyResolver::new();

    // No-op resolver should never report as loaded
    assert!(!resolver.is_loaded());
}

#[test]
fn test_find_closest_match_returns_none() {
    let resolver = NoOpOntologyResolver::new();

    // No-op resolver should return no match
    let matched = resolver.find_closest_match("TechCorp", "classes").unwrap();
    assert!(matched.is_none());
}

#[test]
fn test_get_subgraph_returns_empty() {
    let resolver = NoOpOntologyResolver::new();
    let (nodes, edges, root) = resolver.get_subgraph("TechCorp", "classes", true).unwrap();

    assert!(nodes.is_empty());
    assert!(edges.is_empty());
    assert!(root.is_none());
}

#[test]
fn test_multiple_find_match_calls() {
    let resolver = NoOpOntologyResolver::new();
    let entities = vec!["Alice", "Bob", "TechCorp"];

    // All should return None
    for entity in entities {
        let matched = resolver.find_closest_match(entity, "individuals").unwrap();
        assert!(matched.is_none());
    }
}

#[test]
fn test_multiple_get_subgraph_calls() {
    let resolver = NoOpOntologyResolver::new();

    let entities = vec!["Person", "Company", "Project"];

    // Each should return empty subgraph
    for entity in entities {
        let (nodes, edges, root) = resolver.get_subgraph(entity, "classes", true).unwrap();
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
        assert!(root.is_none());
    }
}

#[test]
fn test_noop_resolver_clone() {
    let resolver1 = NoOpOntologyResolver::new();
    let resolver2 = resolver1.clone();

    assert!(!resolver1.is_loaded());
    assert!(!resolver2.is_loaded());
}

#[test]
fn test_noop_resolver_debug() {
    let resolver = NoOpOntologyResolver::new();
    let debug_string = format!("{:?}", resolver);

    assert!(debug_string.contains("NoOpOntologyResolver"));
}

#[test]
fn test_noop_resolver_default() {
    let resolver = NoOpOntologyResolver;

    assert!(!resolver.is_loaded());
}
