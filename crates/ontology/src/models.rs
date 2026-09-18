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

/// One ontology term: an `owl:Class` or `owl:ObjectProperty` subject together
/// with its raw `rdfs:label` and `rdfs:comment`.
///
/// Values are returned **exactly as they appear in the graph** — not trimmed,
/// not lower-cased, not passed through [`uri_to_key`]. Normalisation is the
/// caller's job, because different callers normalise differently: `uri_to_key`
/// would turn `worksAt` into `worksat`, where a caller building snake_case
/// schema names needs `works_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OntologyTerm {
    /// Full IRI of the subject, e.g. `http://example.org#worksAt`.
    pub uri: String,
    /// Raw lexical form of the subject's `rdfs:label`, un-trimmed.
    ///
    /// `None` **only** when the subject carries no `rdfs:label` triple. An
    /// explicitly empty literal (`rdfs:label ""`) yields `Some(String::new())`
    /// — the distinction is load-bearing: a caller falls back to the URI's
    /// local name for `None`, but must **skip the term entirely** for
    /// `Some("")`, which is what Python does.
    ///
    /// When several `rdfs:label` triples exist, the first in graph index order
    /// is used. Python's `rdflib.Graph.value()` picks an arbitrary one; taking
    /// the first is deterministic and therefore strictly better-defined.
    ///
    /// # Deliberate divergence from Python: non-literal objects
    ///
    /// A malformed ontology may point `rdfs:label` at something other than a
    /// literal, e.g. `ex:Person rdfs:label ex:SomeIri .`. Python's
    /// `graph.value(subject, RDFS.label)` returns whatever node it finds and
    /// the caller `str()`s it, so the *full IRI* becomes the label. Here such
    /// an object has no lexical form and is **treated as absent**: it is
    /// skipped, a later literal label for the same subject may claim the slot,
    /// and if none exists the field is `None`, so the caller falls back to the
    /// local name.
    ///
    /// This is a choice, not an oversight. `rdfs:label` is defined to range
    /// over literals; surfacing a stray IRI as a human-readable name produces
    /// a nonsense entity type (`http_example_org_some_iri`) where the
    /// local-name fallback produces a usable one. The divergence is
    /// unreachable for any ontology that validates.
    pub label: Option<String>,
    /// Raw lexical form of the subject's `rdfs:comment`, un-trimmed.
    ///
    /// `None` **only** when the subject carries no `rdfs:comment` triple; a
    /// whitespace-only comment is returned verbatim, so callers that treat a
    /// blank description as absent must trim before testing. Multiple
    /// comments resolve like multiple labels (first in index order), and a
    /// non-literal object is treated as absent for the same reason and with
    /// the same divergence from Python — see [`OntologyTerm::label`].
    pub comment: Option<String>,
}

/// The `owl:Class` and `owl:ObjectProperty` terms of an ontology.
///
/// Both vectors are deduplicated by URI and sorted **ascending by full URI**,
/// matching Python's `sorted(set(graph.subjects(RDF.type, …)), key=str)`. That
/// order is part of the contract: a caller that folds terms into a
/// `name -> description` map under a "first non-empty description wins" rule
/// reproduces Python's result only if it consumes the vectors in this order.
///
/// The two vectors are independent pools — a subject typed both `owl:Class`
/// and `owl:ObjectProperty` appears in both.
///
/// # Stability
///
/// `#[non_exhaustive]`: `owl:DatatypeProperty` is the obvious next pool, and
/// `owl:AnnotationProperty` after it, so this struct is expected to grow. It is
/// returned from [`OntologyResolver::terms`](crate::OntologyResolver::terms),
/// which out-of-tree resolvers do implement, so construction outside this crate
/// is a supported use and the attribute is not free — [`Self::new`] exists to
/// pay for it. That is the cheaper trade: with `new`, adding a pool leaves
/// every existing caller compiling; with a bare struct literal, adding a pool
/// breaks all of them. Reading fields, `Default`, `Clone` and pattern matching
/// with `..` are unaffected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OntologyTerms {
    /// `owl:Class` subjects, URI-sorted.
    pub classes: Vec<OntologyTerm>,
    /// `owl:ObjectProperty` subjects, URI-sorted.
    pub object_properties: Vec<OntologyTerm>,
}

impl OntologyTerms {
    /// Build the two pools directly.
    ///
    /// The supported way to construct this type outside `cognee-ontology` —
    /// see the `Stability` note above. Both vectors are taken as given: this
    /// does **not** sort or deduplicate them, so an implementor of
    /// [`OntologyResolver::terms`](crate::OntologyResolver::terms) that builds
    /// its own pools owes the URI ordering documented above.
    pub fn new(classes: Vec<OntologyTerm>, object_properties: Vec<OntologyTerm>) -> Self {
        Self {
            classes,
            object_properties,
        }
    }

    /// `true` when the ontology yielded neither classes nor object properties.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.object_properties.is_empty()
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
