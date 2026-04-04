//! Ontology integration for entity validation and enrichment.
//!
//! This crate provides RDF/OWL ontology support for the Cognee knowledge graph pipeline.
//! It offers a trait-based abstraction for ontology operations with two implementations:
//! - [`NoOpOntologyResolver`] - Default (no enrichment, matches Python's ontology_file=None)
//! - [`RdfLibOntologyResolver`] - Full RDF/OWL support using sophia (matches Python's rdflib)
//!
//! # Architecture
//!
//! The crate is built around the [`OntologyResolver`] trait providing:
//! - **Fuzzy entity matching**: Find ontology entities that match LLM-extracted names
//! - **Subgraph extraction**: BFS traversal to extract hierarchical relationships
//! - **Format support**: Turtle, RDF/XML, N-Triples, JSON-LD
//!
//! # Default Behavior (No-Op)
//!
//! The [`NoOpOntologyResolver`] is the default and does nothing:
//! - `find_closest_match()` returns `None`
//! - `get_subgraph()` returns empty vectors
//! - `is_loaded()` returns `false`
//!
//! This matches Python's `RDFLibOntologyResolver(ontology_file=None)`.
//!
//! # Full RDF/OWL Support
//!
//! The [`RdfLibOntologyResolver`] provides complete ontology integration:
//!
//! ```ignore
//! use cognee_ontology::{RdfLibOntologyResolver, OntologyResolver};
//!
//! // Load ontology from file(s)
//! let resolver = RdfLibOntologyResolver::new(vec!["ontology.ttl".into()])?;
//!
//! if resolver.is_loaded() {
//!     // Find matching entity (fuzzy matching with 0.8 threshold)
//!     if let Some(matched) = resolver.find_closest_match("car", "classes")? {
//!         println!("Matched: {}", matched);
//!
//!         // Extract subgraph (BFS from matched entity)
//!         let (nodes, edges, root) = resolver.get_subgraph(&matched, "classes", true)?;
//!
//!         for (source, rel, target) in edges {
//!             println!("{} --[{}]-> {}", source, rel, target);
//!         }
//!     }
//! }
//! ```
//!
//! # Supported Formats
//!
//! - **Turtle** (.ttl) - Recommended, human-readable
//! - **RDF/XML** (.rdf, .owl, .xml) - Most common for OWL ontologies
//! - **N-Triples** (.nt) - Simple line-based format
//! - **JSON-LD** (.jsonld) - JSON-based RDF
//!
//! # Integration with Cognify Pipeline
//!
//! ```ignore
//! use cognee_cognify::{cognify, CognifyConfig};
//! use cognee_ontology::RdfLibOntologyResolver;
//!
//! let ontology = RdfLibOntologyResolver::new(vec!["schema.ttl".into()])?;
//! // Pass the resolver to graph extraction tasks; the pipeline is built via build_cognify_pipeline()
//! let config = CognifyConfig::default();
//! let result = cognify(data_items, dataset_id, llm, storage, graph_db, vector_db, embedding_engine, &config).await?;
//! ```

pub mod builder;
pub mod error;
pub mod loader;
pub mod matching;
pub mod models;
pub mod noop;
pub mod rdflib;
pub mod traits;

pub use error::{OntologyError, OntologyResult};
pub use loader::OntologyFileInput;
pub use matching::{FuzzyMatchingStrategy, MatchingStrategy};
pub use models::{AttachedOntologyNode, NodeCategory, OntologyLookup, uri_to_key};
pub use noop::NoOpOntologyResolver;
pub use rdflib::RdfLibOntologyResolver;
pub use traits::OntologyResolver;
