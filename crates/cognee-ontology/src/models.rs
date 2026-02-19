//! Data structures for ontology entities and lookup indexing.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Category of ontology nodes - either classes (types) or individuals (instances).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeCategory {
    /// OWL classes representing entity types (e.g., "Car", "Vehicle")
    Classes,
    /// Individuals representing entity instances (e.g., "MyCar", "Toyota")
    Individuals,
}

impl NodeCategory {
    /// Convert category to string representation used in Python API.
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeCategory::Classes => "classes",
            NodeCategory::Individuals => "individuals",
        }
    }
}

impl FromStr for NodeCategory {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "classes" => Ok(NodeCategory::Classes),
            "individuals" => Ok(NodeCategory::Individuals),
            _ => Err("Invalid node category. Must be 'classes' or 'individuals'"),
        }
    }
}

impl fmt::Display for NodeCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// An ontology node attached to the knowledge graph.
///
/// Represents entities that were matched against the ontology during
/// graph enrichment. Corresponds to Python's `AttachedOntologyNode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedOntologyNode {
    /// Full URI of the ontology entity (e.g., "http://example.org#Car")
    pub uri: String,
    /// Local name extracted from URI (e.g., "Car")
    pub name: String,
    /// Category: class or individual
    pub category: NodeCategory,
}

impl AttachedOntologyNode {
    /// Create a new ontology node.
    pub fn new(uri: String, category: NodeCategory) -> Self {
        let name = uri_to_key(&uri);
        Self {
            uri,
            name,
            category,
        }
    }
}

/// Lookup index for fast entity matching.
///
/// Maps normalized entity names to their full URIs for both
/// classes (types) and individuals (instances).
#[derive(Debug, Clone, Default)]
pub struct OntologyLookup {
    /// Class name → URI mapping (e.g., "car" → "http://example.org#Car")
    pub classes: HashMap<String, String>,
    /// Individual name → URI mapping (e.g., "my_car" → "http://example.org#MyCar")
    pub individuals: HashMap<String, String>,
}

impl OntologyLookup {
    /// Create an empty lookup index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get candidates for fuzzy matching from specified category.
    pub fn get_candidates(&self, category: NodeCategory) -> Vec<&str> {
        match category {
            NodeCategory::Classes => self.classes.keys().map(|s| s.as_str()).collect(),
            NodeCategory::Individuals => self.individuals.keys().map(|s| s.as_str()).collect(),
        }
    }

    /// Lookup URI by normalized name and category.
    pub fn get_uri(&self, name: &str, category: NodeCategory) -> Option<&str> {
        match category {
            NodeCategory::Classes => self.classes.get(name).map(|s| s.as_str()),
            NodeCategory::Individuals => self.individuals.get(name).map(|s| s.as_str()),
        }
    }
}

/// Convert URI to normalized lookup key.
///
/// Matches Python's RDFLibOntologyResolver._uri_to_key():
/// - Split on '#' or '/' and take last segment
/// - Convert to lowercase
/// - Replace spaces with underscores
///
/// # Examples
///
/// ```
/// use cognee_ontology::models::uri_to_key;
///
/// assert_eq!(uri_to_key("http://example.org#Car"), "car");
/// assert_eq!(uri_to_key("http://example.org/Vehicle"), "vehicle");
/// assert_eq!(uri_to_key("http://example.org#My Car"), "my_car");
/// ```
pub fn uri_to_key(uri: &str) -> String {
    uri.rsplit(['#', '/'])
        .next()
        .unwrap_or(uri)
        .to_lowercase()
        .replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri_to_key_with_hash() {
        assert_eq!(uri_to_key("http://example.org#Car"), "car");
    }

    #[test]
    fn test_uri_to_key_with_slash() {
        assert_eq!(uri_to_key("http://example.org/Vehicle"), "vehicle");
    }

    #[test]
    fn test_uri_to_key_with_spaces() {
        assert_eq!(uri_to_key("http://example.org#My Car"), "my_car");
    }

    #[test]
    fn test_uri_to_key_mixed_case() {
        assert_eq!(uri_to_key("http://example.org#MyCar"), "mycar");
    }

    #[test]
    fn test_node_category_display() {
        assert_eq!(NodeCategory::Classes.to_string(), "classes");
        assert_eq!(NodeCategory::Individuals.to_string(), "individuals");
    }

    #[test]
    fn test_node_category_from_str() {
        assert_eq!(
            "classes".parse::<NodeCategory>().ok(),
            Some(NodeCategory::Classes)
        );
        assert_eq!(
            "individuals".parse::<NodeCategory>().ok(),
            Some(NodeCategory::Individuals)
        );
        assert!("invalid".parse::<NodeCategory>().is_err());
    }

    #[test]
    fn test_attached_ontology_node_creation() {
        let node =
            AttachedOntologyNode::new("http://example.org#Car".to_string(), NodeCategory::Classes);
        assert_eq!(node.uri, "http://example.org#Car");
        assert_eq!(node.name, "car");
        assert_eq!(node.category, NodeCategory::Classes);
    }

    #[test]
    fn test_ontology_lookup_get_candidates() {
        let mut lookup = OntologyLookup::new();
        lookup
            .classes
            .insert("car".to_string(), "http://example.org#Car".to_string());
        lookup
            .classes
            .insert("truck".to_string(), "http://example.org#Truck".to_string());

        let candidates = lookup.get_candidates(NodeCategory::Classes);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&"car"));
        assert!(candidates.contains(&"truck"));
    }

    #[test]
    fn test_ontology_lookup_get_uri() {
        let mut lookup = OntologyLookup::new();
        lookup
            .classes
            .insert("car".to_string(), "http://example.org#Car".to_string());

        assert_eq!(
            lookup.get_uri("car", NodeCategory::Classes),
            Some("http://example.org#Car")
        );
        assert_eq!(lookup.get_uri("truck", NodeCategory::Classes), None);
    }
}
