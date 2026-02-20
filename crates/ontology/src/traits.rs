//! Ontology resolver trait for entity enrichment.
//!
//! Provides graph-based entity matching and subgraph extraction for
//! validating and enriching LLM-extracted entities with ontology knowledge.

use crate::error::OntologyResult;
use crate::models::AttachedOntologyNode;

pub type OntologyEdge = (String, String, String);
pub type OntologySubgraph = (
    Vec<AttachedOntologyNode>,
    Vec<OntologyEdge>,
    Option<AttachedOntologyNode>,
);

/// Ontology resolver for entity validation and graph enrichment.
///
/// This trait provides methods for fuzzy matching entity names against
/// ontology and extracting relationship subgraphs via BFS traversal.
///
/// Python reference: `BaseOntologyResolver` in `cognee/modules/ontology/`
///
/// # Synchronous API
///
/// Unlike the initial design, this trait uses synchronous methods to match
/// Python's `rdflib` behavior. Ontology loading is one-time (on initialization),
/// and graph queries are in-memory operations that don't benefit from async.
///
/// # Example
///
/// ```ignore
/// use cognee_ontology::{RdfLibOntologyResolver, OntologyResolver};
///
/// let resolver = RdfLibOntologyResolver::new(vec!["ontology.ttl".into()])?;
///
/// // Find closest matching entity
/// if let Some(matched_name) = resolver.find_closest_match("car", "classes")? {
///     println!("Matched: {}", matched_name);
///
///     // Extract subgraph from matched entity
///     let (nodes, edges, root) = resolver.get_subgraph(&matched_name, "classes", true)?;
///     for (source, rel, target) in edges {
///         println!("{} - {} -> {}", source, rel, target);
///     }
/// }
/// ```
pub trait OntologyResolver: Send + Sync {
    /// Find the closest matching entity name in the ontology.
    ///
    /// Uses fuzzy string matching (Jaro-Winkler) to find ontology entities
    /// that match the query string. Returns exact matches immediately, otherwise
    /// returns the best fuzzy match above the similarity threshold (0.8).
    ///
    /// Python reference: `RDFLibOntologyResolver.find_closest_match()`
    ///
    /// # Arguments
    ///
    /// * `name` - Entity name to match (e.g., "car", "vehicle")
    /// * `category` - Entity category: `"classes"` (types) or `"individuals"` (instances)
    ///
    /// # Returns
    ///
    /// * `Ok(Some(name))` - Matched entity name (may differ from query due to normalization)
    /// * `Ok(None)` - No match found above threshold
    /// * `Err(_)` - Invalid category or internal error
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Exact match
    /// assert_eq!(resolver.find_closest_match("Car", "classes")?, Some("Car".to_string()));
    ///
    /// // Fuzzy match (typo)
    /// assert_eq!(resolver.find_closest_match("veicle", "classes")?, Some("Vehicle".to_string()));
    ///
    /// // No match
    /// assert_eq!(resolver.find_closest_match("xyz", "classes")?, None);
    /// ```
    fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>>;

    /// Extract subgraph from ontology starting at a given entity (BFS traversal).
    ///
    /// Performs breadth-first search from the starting entity, extracting
    /// all reachable nodes and edges. Extracts hierarchical relationships
    /// (RDF.type, RDFS.subClassOf) and OWL object properties.
    ///
    /// Python reference: `RDFLibOntologyResolver.get_subgraph()`
    ///
    /// # Arguments
    ///
    /// * `node_name` - Starting entity name (will be fuzzy-matched first)
    /// * `node_type` - Entity category: `"classes"` or `"individuals"`
    /// * `directed` - If `false`, also extract inverse edges (source → current)
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// 1. `Vec<AttachedOntologyNode>` - All nodes discovered in BFS
    /// 2. `Vec<(String, String, String)>` - Edges as (source_name, relationship, target_name)
    /// 3. `Option<AttachedOntologyNode>` - Root node if match found, None otherwise
    ///
    /// # Edge Types
    ///
    /// - `"is_a"` - Hierarchical relationships (RDF.type, RDFS.subClassOf)
    /// - Property labels - OWL object properties (e.g., "hasPart", "locatedIn")
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (nodes, edges, root) = resolver.get_subgraph("Car", "classes", true)?;
    ///
    /// if let Some(root_node) = root {
    ///     println!("Root: {} ({})", root_node.name, root_node.uri);
    /// }
    ///
    /// for (source, rel, target) in edges {
    ///     println!("{} --[{}]-> {}", source, rel, target);
    /// }
    /// // Output might be: "Car --[is_a]-> Vehicle"
    /// ```
    fn get_subgraph(
        &self,
        node_name: &str,
        node_type: &str,
        directed: bool,
    ) -> OntologyResult<OntologySubgraph>;

    /// Check if ontology is loaded (has data).
    ///
    /// Returns `true` if an ontology file has been successfully loaded
    /// and is available for matching/enrichment. Returns `false` for
    /// the no-op resolver or when all file loads failed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if resolver.is_loaded() {
    ///     // Perform ontology-based validation
    ///     let matched = resolver.find_closest_match("car", "classes")?;
    /// } else {
    ///     // Skip ontology enrichment
    /// }
    /// ```
    fn is_loaded(&self) -> bool;
}
