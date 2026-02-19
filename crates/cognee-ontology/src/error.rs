//! Error types for ontology operations.

use thiserror::Error;

/// Errors that can occur during ontology operations.
#[derive(Error, Debug)]
pub enum OntologyError {
    /// Ontology file not found at the specified path.
    #[error("Ontology file not found: {0}")]
    FileNotFound(String),

    /// Error parsing ontology file (RDF/OWL).
    #[error("Ontology parsing error: {0}")]
    ParseError(String),

    /// Error matching entities against ontology.
    #[error("Entity matching error: {0}")]
    MatchingError(String),
}

/// Result type for ontology operations.
pub type OntologyResult<T> = Result<T, OntologyError>;
