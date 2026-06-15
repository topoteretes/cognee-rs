//! RDF/OWL ontology file loading using sophia.
//!
//! Provides format auto-detection and multi-file merging with
//! permissive error handling (logs warnings, continues with valid files).

use sophia_api::graph::{Graph, MutableGraph};
use sophia_api::parser::TripleParser;
use sophia_api::prelude::{Quad, QuadParser, QuadSource};
use sophia_api::source::TripleSource;
use sophia_inmem::graph::FastGraph;
use sophia_jsonld::JsonLdParser;
use sophia_turtle::parser::turtle;
use sophia_xml::parser::RdfXmlParser;
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::error::{OntologyError, OntologyResult};

/// Input sources for ontology files.
///
/// Supports single/multiple file paths and file-like readers.
pub enum OntologyFileInput {
    /// Single file path
    Path(PathBuf),
    /// Multiple file paths
    Paths(Vec<PathBuf>),
    /// Single reader (e.g., in-memory buffer)
    Reader(Box<dyn Read>),
    /// Multiple readers
    Readers(Vec<Box<dyn Read>>),
}

impl From<PathBuf> for OntologyFileInput {
    fn from(path: PathBuf) -> Self {
        OntologyFileInput::Path(path)
    }
}

impl From<Vec<PathBuf>> for OntologyFileInput {
    fn from(paths: Vec<PathBuf>) -> Self {
        OntologyFileInput::Paths(paths)
    }
}

impl<'a> From<&'a str> for OntologyFileInput {
    fn from(path: &'a str) -> Self {
        OntologyFileInput::Path(PathBuf::from(path))
    }
}

impl From<Vec<&str>> for OntologyFileInput {
    fn from(paths: Vec<&str>) -> Self {
        OntologyFileInput::Paths(paths.into_iter().map(PathBuf::from).collect())
    }
}

/// Detect RDF format from file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RdfFormat {
    Turtle,   // .ttl
    RdfXml,   // .rdf, .owl, .xml
    NTriples, // .nt
    JsonLd,   // .jsonld
}

impl RdfFormat {
    /// Detect format from file extension.
    fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| match ext.to_lowercase().as_str() {
                "ttl" => Some(RdfFormat::Turtle),
                "rdf" | "owl" | "xml" => Some(RdfFormat::RdfXml),
                "nt" => Some(RdfFormat::NTriples),
                "jsonld" => Some(RdfFormat::JsonLd),
                _ => None,
            })
    }
}

/// Load ontology files and merge into a single graph.
///
/// Matches Python's permissive error handling: logs warnings for missing
/// files but continues with valid files. Returns `None` if all files fail.
pub fn load_ontology_files(input: OntologyFileInput) -> OntologyResult<Option<FastGraph>> {
    match input {
        OntologyFileInput::Path(path) => load_single_path(&path),
        OntologyFileInput::Paths(paths) => load_multiple_paths(&paths),
        OntologyFileInput::Reader(reader) => load_single_reader(reader),
        OntologyFileInput::Readers(readers) => load_multiple_readers(readers),
    }
}

fn load_single_path(path: &Path) -> OntologyResult<Option<FastGraph>> {
    if !path.exists() {
        warn!(
            "Ontology file '{}' not found. Skipping this file.",
            path.display()
        );
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        OntologyError::FileNotFound(format!("Failed to read file '{}': {}", path.display(), e))
    })?;

    let format = RdfFormat::from_path(path).ok_or_else(|| {
        OntologyError::ParseError(format!(
            "Unknown RDF format for file '{}'. Supported: .ttl, .rdf, .owl, .nt, .jsonld",
            path.display()
        ))
    })?;

    let parse_result = match format {
        RdfFormat::Turtle => parse_turtle_with_path_base(path, &content),
        _ => parse_rdf(&content, format),
    };

    match parse_result {
        Ok(graph) => {
            info!("Ontology loaded successfully from file: {}", path.display());
            Ok(Some(graph))
        }
        Err(e) => {
            warn!("Failed to parse ontology file '{}': {}", path.display(), e);
            Ok(None)
        }
    }
}

fn parse_turtle_with_path_base(path: &Path, content: &str) -> OntologyResult<FastGraph> {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let base_iri = format!("file://{}", absolute.to_string_lossy());
    let content_with_base = format!("@base <{}> .\n{}", base_iri, content);

    parse_rdf(&content_with_base, RdfFormat::Turtle)
}

fn load_multiple_paths(paths: &[PathBuf]) -> OntologyResult<Option<FastGraph>> {
    if paths.is_empty() {
        info!("No ontology file provided. No owl ontology will be attached to the graph.");
        return Ok(None);
    }

    let mut merged_graph = FastGraph::new();
    let mut loaded_count = 0;

    for path in paths {
        match load_single_path(path) {
            Ok(Some(graph)) => {
                merged_graph.insert_all(graph.triples()).map_err(|e| {
                    OntologyError::ParseError(format!(
                        "Failed to merge graph from '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                loaded_count += 1;
            }
            Ok(None) => {}
            Err(e) => warn!(
                "Failed to process ontology file '{}': {}",
                path.display(),
                e
            ),
        }
    }

    if loaded_count == 0 {
        info!("No valid ontology files found. No owl ontology will be attached to the graph.");
        Ok(None)
    } else {
        info!("Total ontology files loaded: {}", loaded_count);
        Ok(Some(merged_graph))
    }
}

fn load_single_reader(mut reader: Box<dyn Read>) -> OntologyResult<Option<FastGraph>> {
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .map_err(|e| OntologyError::FileNotFound(format!("Failed to read from reader: {}", e)))?;

    // Prefer RDF/XML (Python parity), but permissively fall back for other valid RDF payloads.
    let parse_attempts = [
        RdfFormat::RdfXml,
        RdfFormat::Turtle,
        RdfFormat::JsonLd,
        RdfFormat::NTriples,
    ];

    let mut last_error: Option<OntologyError> = None;
    let mut parsed_graph: Option<FastGraph> = None;

    for format in parse_attempts {
        match parse_rdf(&content, format) {
            Ok(graph) => {
                parsed_graph = Some(graph);
                break;
            }
            Err(e) => last_error = Some(e),
        }
    }

    match parsed_graph {
        Some(graph) => {
            info!("Ontology loaded successfully from reader");
            Ok(Some(graph))
        }
        None => {
            let err_message = last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Unknown parse error".to_string());
            warn!("Failed to parse ontology from reader: {}", err_message);
            Ok(None)
        }
    }
}

fn load_multiple_readers(readers: Vec<Box<dyn Read>>) -> OntologyResult<Option<FastGraph>> {
    if readers.is_empty() {
        info!("No ontology file provided. No owl ontology will be attached to the graph.");
        return Ok(None);
    }

    let mut merged_graph = FastGraph::new();
    let mut loaded_count = 0;

    for reader in readers {
        if let Some(graph) = load_single_reader(reader)? {
            merged_graph.insert_all(graph.triples()).map_err(|e| {
                OntologyError::ParseError(format!("Failed to merge graph from reader: {}", e))
            })?;
            loaded_count += 1;
        }
    }

    if loaded_count == 0 {
        info!("No valid ontology readers found. No owl ontology will be attached to the graph.");
        Ok(None)
    } else {
        info!("Total ontology readers loaded: {}", loaded_count);
        Ok(Some(merged_graph))
    }
}

/// Parse RDF content with specified format.
fn parse_rdf(content: &str, format: RdfFormat) -> OntologyResult<FastGraph> {
    match format {
        RdfFormat::Turtle | RdfFormat::NTriples => turtle::parse_str(content)
            .collect_triples()
            .map_err(|e| OntologyError::ParseError(format!("Turtle/N-Triples parse error: {}", e))),
        RdfFormat::RdfXml => RdfXmlParser::default()
            .parse_str(content)
            .collect_triples()
            .map_err(|e| OntologyError::ParseError(format!("RDF/XML parse error: {}", e))),
        RdfFormat::JsonLd => JsonLdParser::new()
            .parse_str(content)
            .filter_quads(|q| q.g().is_none())
            .map_quads(Quad::into_triple)
            .collect_triples()
            .map_err(|e| OntologyError::ParseError(format!("JSON-LD parse error: {}", e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_detection_turtle() {
        assert_eq!(
            RdfFormat::from_path(Path::new("ontology.ttl")),
            Some(RdfFormat::Turtle)
        );
    }

    #[test]
    fn test_format_detection_rdfxml() {
        assert_eq!(
            RdfFormat::from_path(Path::new("ontology.rdf")),
            Some(RdfFormat::RdfXml)
        );
        assert_eq!(
            RdfFormat::from_path(Path::new("ontology.owl")),
            Some(RdfFormat::RdfXml)
        );
    }

    #[test]
    fn test_format_detection_unknown() {
        assert_eq!(RdfFormat::from_path(Path::new("ontology.txt")), None);
    }

    #[test]
    fn test_load_missing_file() {
        let result = load_single_path(Path::new("nonexistent.ttl")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_simple_turtle() {
        let ttl = r#"
            @prefix ex: <http://example.org#> .
            ex:Car a ex:Vehicle .
        "#;

        let graph = parse_rdf(ttl, RdfFormat::Turtle).unwrap();
        assert!(graph.triples().count() > 0);
    }

    #[test]
    fn test_parse_simple_rdfxml() {
        let rdfxml = r#"<?xml version="1.0"?>
            <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
                     xmlns:ex="http://example.org#">
              <rdf:Description rdf:about="http://example.org#Car">
                <rdf:type rdf:resource="http://example.org#Vehicle"/>
              </rdf:Description>
            </rdf:RDF>"#;

        let graph = parse_rdf(rdfxml, RdfFormat::RdfXml).unwrap();
        assert!(graph.triples().count() > 0);
    }

    #[test]
    fn test_parse_simple_jsonld() {
        let jsonld = r#"{
            "@context": {
                "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
                "ex": "http://example.org#",
                "type": {"@id": "rdf:type", "@type": "@id"}
            },
            "@id": "ex:Car",
            "type": "ex:Vehicle"
        }"#;

        let graph = parse_rdf(jsonld, RdfFormat::JsonLd).unwrap();
        assert!(graph.triples().count() > 0);
    }

    #[test]
    fn test_parse_invalid_turtle() {
        let ttl = "invalid turtle syntax !!!";
        let result = parse_rdf(ttl, RdfFormat::Turtle);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_rdfxml() {
        let rdfxml = "<rdf:RDF><rdf:Description></rdf:RDF>";
        let result = parse_rdf(rdfxml, RdfFormat::RdfXml);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_jsonld() {
        let jsonld = "{invalid json-ld}";
        let result = parse_rdf(jsonld, RdfFormat::JsonLd);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_turtle() {
        let ttl = "";
        let graph = parse_rdf(ttl, RdfFormat::Turtle).unwrap();
        assert_eq!(graph.triples().count(), 0);
    }
}
