//! RDFLib-compatible ontology resolver using sophia.
//!
//! Main implementation of the OntologyResolver trait providing fuzzy
//! entity matching and BFS-based subgraph extraction.

use log::{debug, info};
use sophia_api::graph::Graph;
use sophia_api::ns::{owl, rdf, rdfs};
use sophia_api::term::Term;
use sophia_api::triple::Triple;
use sophia_inmem::graph::FastGraph;
use std::collections::{HashSet, VecDeque};

use crate::builder::build_lookup;
use crate::error::{OntologyError, OntologyResult};
use crate::loader::{OntologyFileInput, load_ontology_files};
use crate::matching::{FuzzyMatchingStrategy, MatchingStrategy};
use crate::models::{AttachedOntologyNode, NodeCategory, OntologyLookup, uri_to_key};
use crate::traits::{OntologyEdge, OntologyResolver, OntologySubgraph};

/// RDFLib-compatible ontology resolver.
///
/// Loads RDF/OWL ontology files, builds lookup indexes, and provides
/// fuzzy matching + subgraph extraction for entity enrichment.
///
/// Matches Python's `RDFLibOntologyResolver` behavior.
///
/// # Example
///
/// ```ignore
/// use cognee_ontology::RdfLibOntologyResolver;
/// use std::path::PathBuf;
///
/// let resolver = RdfLibOntologyResolver::new(vec![PathBuf::from("ontology.ttl")])?;
///
/// if resolver.is_loaded() {
///     // Find matching entity
///     if let Some(name) = resolver.find_closest_match("car", "classes")? {
///         // Extract subgraph
///         let (nodes, edges, root) = resolver.get_subgraph(&name, "classes", true)?;
///         println!("Found {} nodes and {} edges", nodes.len(), edges.len());
///     }
/// }
/// ```
pub struct RdfLibOntologyResolver {
    /// RDF graph (None if no files loaded successfully)
    graph: Option<FastGraph>,
    /// Lookup index for fast entity matching
    lookup: OntologyLookup,
    /// Fuzzy matching strategy
    matching_strategy: Box<dyn MatchingStrategy>,
}

impl RdfLibOntologyResolver {
    /// Create resolver from file paths.
    ///
    /// # Arguments
    ///
    /// * `paths` - Vector of ontology file paths (.ttl, .rdf, .owl, etc.)
    ///
    /// # Returns
    ///
    /// Resolver with loaded ontology, or resolver with `is_loaded() == false`
    /// if all files failed to load (matches Python's permissive error handling).
    pub fn new<P: Into<OntologyFileInput>>(input: P) -> OntologyResult<Self> {
        Self::with_strategy(input, Box::new(FuzzyMatchingStrategy::default()))
    }

    /// Create resolver with custom matching strategy.
    pub fn with_strategy<P: Into<OntologyFileInput>>(
        input: P,
        matching_strategy: Box<dyn MatchingStrategy>,
    ) -> OntologyResult<Self> {
        let graph = load_ontology_files(input.into())?;

        let lookup = if let Some(ref g) = graph {
            build_lookup(g)?
        } else {
            OntologyLookup::new()
        };

        Ok(Self {
            graph,
            lookup,
            matching_strategy,
        })
    }

    /// Get reference to underlying RDF graph (if loaded).
    pub fn graph(&self) -> Option<&FastGraph> {
        self.graph.as_ref()
    }

    /// Get number of classes in ontology.
    pub fn class_count(&self) -> usize {
        self.lookup.classes.len()
    }

    /// Get number of individuals in ontology.
    pub fn individual_count(&self) -> usize {
        self.lookup.individuals.len()
    }
}

impl OntologyResolver for RdfLibOntologyResolver {
    fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>> {
        if self.graph.is_none() {
            return Ok(None);
        }

        let node_category = category.parse::<NodeCategory>().map_err(|_| {
            OntologyError::MatchingError(format!(
                "Invalid category '{}'. Must be 'classes' or 'individuals'",
                category
            ))
        })?;

        let candidates = self.lookup.get_candidates(node_category);
        Ok(self.matching_strategy.find_match(name, &candidates))
    }

    fn get_subgraph(
        &self,
        node_name: &str,
        node_type: &str,
        directed: bool,
    ) -> OntologyResult<OntologySubgraph> {
        let graph = match &self.graph {
            Some(g) => g,
            None => return Ok((vec![], vec![], None)),
        };

        // Find matching entity
        let matched_name = match self.find_closest_match(node_name, node_type)? {
            Some(name) => name,
            None => {
                debug!(
                    "No match found for '{}' in category '{}'",
                    node_name, node_type
                );
                return Ok((vec![], vec![], None));
            }
        };

        // Get category and lookup URI
        let category = node_type.parse::<NodeCategory>().map_err(|_| {
            OntologyError::MatchingError(format!("Invalid node type '{}'", node_type))
        })?;

        let start_uri = self
            .lookup
            .get_uri(&matched_name, category)
            .ok_or_else(|| {
                OntologyError::MatchingError(format!("URI not found for '{}'", matched_name))
            })?;

        info!(
            "Extracting subgraph from '{}' (matched: {}, URI: {})",
            node_name, matched_name, start_uri
        );

        // BFS traversal
        let (nodes, edges) = bfs_extract_subgraph(graph, start_uri, directed)?;

        let root_node = AttachedOntologyNode::new(start_uri.to_string(), category);

        Ok((nodes, edges, Some(root_node)))
    }

    fn is_loaded(&self) -> bool {
        self.graph.is_some()
    }
}

/// Perform BFS traversal to extract subgraph.
///
/// Extracts all reachable nodes and edges from starting URI.
/// Handles RDF.type, RDFS.subClassOf, and OWL object properties.
fn bfs_extract_subgraph(
    graph: &FastGraph,
    start_uri: &str,
    directed: bool,
) -> OntologyResult<(Vec<AttachedOntologyNode>, Vec<OntologyEdge>)> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let rdf_type = rdf::type_
        .iri()
        .expect("rdf:type is a compile-time constant IRI")
        .to_string();
    let rdfs_subclass = rdfs::subClassOf
        .iri()
        .expect("rdfs:subClassOf is a compile-time constant IRI")
        .to_string();

    // Start BFS from start_uri
    queue.push_back(start_uri.to_string());
    visited.insert(start_uri.to_string());

    while let Some(current_uri) = queue.pop_front() {
        // Extract outgoing edges: current --predicate--> target
        for triple_result in graph.triples() {
            let triple = triple_result
                .map_err(|e| OntologyError::MatchingError(format!("BFS traversal error: {}", e)))?;

            let Some(source_iri) = triple.s().iri() else {
                continue;
            };
            if source_iri.as_str() != current_uri {
                continue;
            }

            let Some(pred_iri) = triple.p().iri() else {
                continue;
            };
            let Some(target_iri) = triple.o().iri() else {
                continue;
            };

            let pred_str = pred_iri.to_string();
            let target_uri = target_iri.to_string();
            let current_name = uri_to_key(&current_uri);
            let target_name = uri_to_key(&target_uri);

            // Process edge based on predicate type
            if pred_str == rdf_type || pred_str == rdfs_subclass {
                edges.push((current_name, "is_a".to_string(), target_name));

                // Add target to queue if not visited
                if visited.insert(target_uri.clone()) {
                    queue.push_back(target_uri.clone());
                    // Determine category (assume Classes for hierarchical relationships)
                    nodes.push(AttachedOntologyNode::new(target_uri, NodeCategory::Classes));
                }
            }
            // Handle OWL object properties
            else if is_object_property(graph, &pred_str)? {
                let relationship = uri_to_key(&pred_str);

                edges.push((current_name, relationship, target_name));

                // Add target to queue if not visited
                if visited.insert(target_uri.clone()) {
                    queue.push_back(target_uri.clone());
                    nodes.push(AttachedOntologyNode::new(
                        target_uri,
                        NodeCategory::Individuals,
                    ));
                }
            }
        }

        // If undirected, also extract incoming edges: source --predicate--> current
        if !directed {
            for triple_result in graph.triples() {
                let triple = triple_result.map_err(|e| {
                    OntologyError::MatchingError(format!("BFS inverse traversal error: {}", e))
                })?;

                let Some(src_iri) = triple.s().iri() else {
                    continue;
                };
                let Some(pred_iri) = triple.p().iri() else {
                    continue;
                };
                let Some(obj_iri) = triple.o().iri() else {
                    continue;
                };
                if obj_iri.as_str() != current_uri {
                    continue;
                }

                let src_uri = src_iri.to_string();
                let pred_str = pred_iri.to_string();

                // Only add inverse edges for object properties (not hierarchical)
                if is_object_property(graph, &pred_str)? {
                    let source_name = uri_to_key(&src_uri);
                    let current_name = uri_to_key(&current_uri);
                    let relationship = uri_to_key(&pred_str);

                    edges.push((source_name, relationship, current_name));

                    // Add source to queue if not visited
                    if visited.insert(src_uri.clone()) {
                        queue.push_back(src_uri.clone());
                        nodes.push(AttachedOntologyNode::new(
                            src_uri,
                            NodeCategory::Individuals,
                        ));
                    }
                }
            }
        }
    }

    Ok((nodes, edges))
}

/// Check if a predicate is an OWL ObjectProperty.
fn is_object_property(graph: &FastGraph, predicate_uri: &str) -> OntologyResult<bool> {
    let rdf_type = rdf::type_
        .iri()
        .expect("rdf:type is a compile-time constant IRI")
        .to_string();
    let owl_obj_prop = owl::ObjectProperty
        .iri()
        .expect("owl:ObjectProperty is a compile-time constant IRI")
        .to_string();

    // Query: <predicate> rdf:type owl:ObjectProperty
    for triple_result in graph.triples() {
        let triple = triple_result.map_err(|e| {
            OntologyError::MatchingError(format!("ObjectProperty check error: {}", e))
        })?;

        let subject = triple.s().iri().map(|iri| iri.to_string());
        let predicate = triple.p().iri().map(|iri| iri.to_string());
        let object = triple.o().iri().map(|iri| iri.to_string());

        if subject.as_deref() == Some(predicate_uri)
            && predicate.as_deref() == Some(rdf_type.as_str())
            && object.as_deref() == Some(owl_obj_prop.as_str())
        {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_resolver() -> RdfLibOntologyResolver {
        let ttl = r#"
            @prefix ex: <http://example.org#> .
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
            @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

            ex:Vehicle rdf:type owl:Class .
            ex:Car rdf:type owl:Class ;
                   rdfs:subClassOf ex:Vehicle .

            ex:MyCar rdf:type ex:Car .

            ex:hasPart rdf:type owl:ObjectProperty .
            ex:MyCar ex:hasPart ex:Engine .
        "#;

        let reader: Box<dyn std::io::Read> = Box::new(ttl.as_bytes());
        RdfLibOntologyResolver::new(OntologyFileInput::Reader(reader)).unwrap()
    }

    #[test]
    fn test_resolver_creation() {
        let resolver = create_test_resolver();
        assert!(resolver.is_loaded());
        assert!(resolver.class_count() >= 2); // Vehicle, Car
    }

    #[test]
    fn test_find_exact_match() {
        let resolver = create_test_resolver();
        let result = resolver.find_closest_match("car", "classes").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_find_fuzzy_match() {
        let resolver = create_test_resolver();
        // "veicle" should match "vehicle" with default cutoff
        let result = resolver.find_closest_match("veicle", "classes").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_get_subgraph() {
        let resolver = create_test_resolver();
        let (_nodes, edges, root) = resolver.get_subgraph("car", "classes", true).unwrap();

        assert!(root.is_some());
        assert!(!edges.is_empty());

        // Should have "is_a" edge from Car to Vehicle
        assert!(edges.iter().any(|(_s, r, _t)| r == "is_a"));
    }

    #[test]
    fn test_invalid_category() {
        let resolver = create_test_resolver();
        let result = resolver.find_closest_match("car", "invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_no_match_found() {
        let resolver = create_test_resolver();
        let result = resolver.find_closest_match("xyz", "classes").unwrap();
        assert_eq!(result, None);
    }
}
