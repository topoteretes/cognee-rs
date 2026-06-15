//! Ontology lookup index builder.
//!
//! Extracts classes and individuals from RDF graph and builds
//! normalized name → URI lookup tables for fast matching.

use std::collections::{HashMap, HashSet};

use sophia_api::graph::Graph;
use sophia_api::ns::{owl, rdf};
use sophia_api::term::Term;
use sophia_api::triple::Triple;
use sophia_inmem::graph::FastGraph;
use tracing::info;

use crate::error::{OntologyError, OntologyResult};
use crate::models::{OntologyLookup, uri_to_key};

/// Build lookup index from RDF graph.
///
/// Extracts OWL classes and individuals, normalizes their URIs to
/// lookup keys, and builds HashMap indexes for fast fuzzy matching.
///
/// Matches Python's `RDFLibOntologyResolver.build_lookup()` method.
///
/// # Algorithm
///
/// 1. Extract classes: Find all subjects with `rdf:type owl:Class`
/// 2. Extract individuals: For each class, find subjects with `rdf:type <class_uri>`
/// 3. Normalize URIs using `uri_to_key()` (lowercase, replace spaces)
/// 4. Build HashMap: normalized_name → full_uri
///
/// # Arguments
///
/// * `graph` - RDF graph loaded from ontology files
///
/// # Returns
///
/// `OntologyLookup` with populated `classes` and `individuals` maps.
///
/// # Example
///
/// ```ignore
/// use cognee_ontology::builder::build_lookup;
/// use sophia::inmem::graph::FastGraph;
///
/// let graph: FastGraph = /* ... load ontology ... */;
/// let lookup = build_lookup(&graph)?;
///
/// println!("Found {} classes", lookup.classes.len());
/// println!("Found {} individuals", lookup.individuals.len());
/// ```
pub fn build_lookup(graph: &FastGraph) -> OntologyResult<OntologyLookup> {
    let mut lookup = OntologyLookup::new();

    // Extract OWL classes
    let class_count = extract_classes(graph, &mut lookup)?;
    info!("Extracted {} OWL classes from ontology", class_count);

    // Extract individuals (instances of classes)
    // Clone classes HashMap to avoid borrow checker issues
    let classes_clone = lookup.classes.clone();
    let individual_count = extract_individuals(graph, &classes_clone, &mut lookup)?;
    info!("Extracted {} individuals from ontology", individual_count);

    Ok(lookup)
}

/// Extract OWL classes from graph.
///
/// Finds all subjects with `rdf:type owl:Class`.
fn extract_classes(graph: &FastGraph, lookup: &mut OntologyLookup) -> OntologyResult<usize> {
    let mut count = 0;

    let rdf_type = rdf::type_
        .iri()
        .expect("rdf:type is a compile-time constant IRI")
        .to_string();
    let owl_class = owl::Class
        .iri()
        .expect("owl:Class is a compile-time constant IRI")
        .to_string();

    // Query: ?s rdf:type owl:Class
    for triple_result in graph.triples() {
        let triple = triple_result.map_err(|e| {
            OntologyError::MatchingError(format!("Failed to extract classes: {}", e))
        })?;

        let predicate = triple.p().iri().map(|iri| iri.to_string());
        let object = triple.o().iri().map(|iri| iri.to_string());

        if predicate.as_deref() == Some(rdf_type.as_str())
            && object.as_deref() == Some(owl_class.as_str())
            && let Some(uri) = triple.s().iri()
        {
            let uri_str = uri.to_string();
            let key = uri_to_key(&uri_str);
            lookup.classes.insert(key, uri_str);
            count += 1;
        }
    }

    Ok(count)
}

/// Extract individuals (class instances) from graph.
///
/// For each known class, finds subjects with `rdf:type <class_uri>`.
fn extract_individuals(
    graph: &FastGraph,
    classes: &HashMap<String, String>,
    lookup: &mut OntologyLookup,
) -> OntologyResult<usize> {
    let mut count = 0;

    let rdf_type = rdf::type_
        .iri()
        .expect("rdf:type is a compile-time constant IRI")
        .to_string();
    let class_uris: HashSet<_> = classes.values().map(|uri| uri.as_str()).collect();

    // Query: ?s rdf:type <class_uri>
    for triple_result in graph.triples() {
        let triple = triple_result.map_err(|e| {
            OntologyError::MatchingError(format!("Failed to extract individuals: {}", e))
        })?;

        let predicate = triple.p().iri().map(|iri| iri.to_string());
        let object = triple.o().iri().map(|iri| iri.to_string());

        if predicate.as_deref() == Some(rdf_type.as_str())
            && object
                .as_deref()
                .is_some_and(|obj| class_uris.contains(obj))
            && let Some(uri) = triple.s().iri()
        {
            let uri_str = uri.to_string();
            let key = uri_to_key(&uri_str);
            // Only add if not already a class (classes can also be individuals)
            if !classes.contains_key(&key) {
                lookup.individuals.insert(key, uri_str);
                count += 1;
            }
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sophia_api::source::TripleSource;
    use sophia_turtle::parser::turtle;

    fn parse_test_ontology() -> FastGraph {
        let ttl = r#"
            @prefix ex: <http://example.org#> .
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

            ex:Vehicle rdf:type owl:Class .
            ex:Car rdf:type owl:Class .
            ex:Truck rdf:type owl:Class .

            ex:MyCar rdf:type ex:Car .
            ex:ToyotaCamry rdf:type ex:Car .
            ex:F150 rdf:type ex:Truck .
        "#;

        turtle::parse_str(ttl).collect_triples().unwrap()
    }

    #[test]
    fn test_build_lookup() {
        let graph = parse_test_ontology();
        let lookup = build_lookup(&graph).unwrap();

        // Should find 3 classes
        assert_eq!(lookup.classes.len(), 3);
        assert!(lookup.classes.contains_key("vehicle"));
        assert!(lookup.classes.contains_key("car"));
        assert!(lookup.classes.contains_key("truck"));

        // Should find 3 individuals
        assert_eq!(lookup.individuals.len(), 3);
        assert!(lookup.individuals.contains_key("mycar"));
        assert!(lookup.individuals.contains_key("toyotacamry"));
        assert!(lookup.individuals.contains_key("f150"));
    }

    #[test]
    fn test_extract_classes() {
        let graph = parse_test_ontology();
        let mut lookup = OntologyLookup::new();

        let count = extract_classes(&graph, &mut lookup).unwrap();

        assert_eq!(count, 3);
        assert_eq!(lookup.classes.len(), 3);
        assert!(lookup.classes.contains_key("car"));
        assert!(lookup.classes.get("car").unwrap().contains("Car"));
    }

    #[test]
    fn test_extract_individuals() {
        let graph = parse_test_ontology();
        let mut lookup = OntologyLookup::new();

        // First extract classes
        extract_classes(&graph, &mut lookup).unwrap();

        // Then extract individuals (clone to avoid borrow checker issues)
        let classes_clone = lookup.classes.clone();
        let count = extract_individuals(&graph, &classes_clone, &mut lookup).unwrap();

        assert_eq!(count, 3);
        assert!(lookup.individuals.contains_key("mycar"));
    }

    #[test]
    fn test_empty_graph() {
        let graph = FastGraph::new();
        let lookup = build_lookup(&graph).unwrap();

        assert_eq!(lookup.classes.len(), 0);
        assert_eq!(lookup.individuals.len(), 0);
    }

    #[test]
    fn test_uri_normalization() {
        let graph = parse_test_ontology();
        let lookup = build_lookup(&graph).unwrap();

        // URI should be normalized to lowercase key
        assert!(lookup.classes.contains_key("vehicle"));
        assert!(lookup.classes.contains_key("car"));

        // But original URI should be preserved
        assert!(lookup.classes.get("car").unwrap().contains("#Car"));
    }
}
