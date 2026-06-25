//! No-op ontology resolver.
//!
//! Mirrors Python's RDFLibOntologyResolver(ontology_file=None) behavior.

use crate::error::OntologyResult;
use crate::models::AttachedOntologyNode;
use crate::traits::OntologyResolver;

/// No-op ontology resolver (default behavior).
///
/// This resolver does nothing and matches Python's behavior when no ontology
/// file is provided. It serves as the default resolver and provides structural
/// parity with the Python implementation.
///
/// # Behavior
/// - `find_closest_match()` returns `Ok(None)` (no matches)
/// - `get_subgraph()` returns empty vectors (no subgraph)
/// - `is_loaded()` returns `false`
///
/// # Example
/// ```
/// use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
///
/// let resolver = NoOpOntologyResolver::new();
/// assert!(!resolver.is_loaded());
/// ```
#[derive(Debug, Default, Clone)]
pub struct NoOpOntologyResolver;

impl NoOpOntologyResolver {
    pub fn new() -> Self {
        Self
    }
}

impl OntologyResolver for NoOpOntologyResolver {
    fn find_closest_match(&self, _name: &str, _category: &str) -> OntologyResult<Option<String>> {
        Ok(None)
    }

    fn get_subgraph(
        &self,
        _node_name: &str,
        _node_type: &str,
        _directed: bool,
    ) -> OntologyResult<(
        Vec<AttachedOntologyNode>,
        Vec<(String, String, String)>,
        Option<AttachedOntologyNode>,
    )> {
        Ok((vec![], vec![], None))
    }

    fn is_loaded(&self) -> bool {
        false
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
    fn test_noop_resolver_creation() {
        let resolver = NoOpOntologyResolver::new();
        assert!(!resolver.is_loaded());
    }

    #[test]
    fn test_noop_resolver_default() {
        let resolver = NoOpOntologyResolver;
        assert!(!resolver.is_loaded());
    }

    #[test]
    fn test_find_closest_match_returns_none() {
        let resolver = NoOpOntologyResolver::new();
        let result = resolver.find_closest_match("car", "classes").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_subgraph_returns_empty() {
        let resolver = NoOpOntologyResolver::new();
        let (nodes, edges, root) = resolver.get_subgraph("car", "classes", true).unwrap();
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
        assert!(root.is_none());
    }
}
