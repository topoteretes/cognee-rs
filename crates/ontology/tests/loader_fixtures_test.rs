use std::path::PathBuf;

use cognee_ontology::{OntologyFileInput, loader::load_ontology_files};
use sophia_api::graph::Graph;

fn fixture_path(file_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(file_name)
}

#[test]
fn loads_real_turtle_fixture() {
    let input = OntologyFileInput::from(fixture_path("w3c_turtle_subm_01.ttl"));
    let graph = load_ontology_files(input).expect("fixture should load");

    let graph = graph.expect("expected parsed turtle graph");
    assert!(graph.triples().count() > 0);
}

#[test]
fn loads_real_jsonld_fixture() {
    let input = OntologyFileInput::from(fixture_path("jsonld_expand_0002_in.jsonld"));
    let graph = load_ontology_files(input).expect("fixture should load");

    let graph = graph.expect("expected parsed json-ld graph");
    assert!(graph.triples().count() > 0);
}

#[test]
fn loads_real_rdfxml_fixture() {
    let input = OntologyFileInput::from(fixture_path("sophia_file5.rdf"));
    let graph = load_ontology_files(input).expect("fixture should load");

    let graph = graph.expect("expected parsed rdf/xml graph");
    assert!(graph.triples().count() > 0);
}

#[test]
fn merges_multiple_real_fixtures() {
    let input = OntologyFileInput::from(vec![
        fixture_path("w3c_turtle_subm_01.ttl"),
        fixture_path("jsonld_expand_0002_in.jsonld"),
        fixture_path("sophia_file5.rdf"),
    ]);

    let graph = load_ontology_files(input).expect("fixtures should load");
    let graph = graph.expect("expected merged graph");

    assert!(graph.triples().count() > 0);
}
