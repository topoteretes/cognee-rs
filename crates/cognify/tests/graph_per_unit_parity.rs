#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! SDK-1694 test 10: the graph cognify builds from a fixed set of extracted
//! chunk graphs matches what Python's `construct_data_points_and_edges` built
//! from the same input, one call per data item.
//!
//! The expected output was generated once from Python and committed; see
//! `tests/fixtures/graph_per_unit_parity/README.md`. Endpoints resolve only
//! within their own chunk here (`EndpointScope::Chunk`, i.e.
//! `cognify.cross_chunk_endpoints=false`), as in Python.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use cognee_cognify::{
    EndpointScope, KnowledgeGraph, expand_extracted_graphs_by_unit, extraction_units,
};
use cognee_models::DocumentChunk;
use cognee_ontology::NoOpOntologyResolver;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
struct Input {
    chunks: Vec<InputChunk>,
}

#[derive(Deserialize)]
struct InputChunk {
    data_id: Uuid,
    chunk_id: Uuid,
    graph: KnowledgeGraph,
}

#[derive(Deserialize)]
struct Expected {
    entity_ids: BTreeSet<Uuid>,
    entity_types: BTreeSet<(Uuid, Uuid)>,
    type_names: BTreeMap<Uuid, String>,
    timestamp_ids: BTreeSet<Uuid>,
    semantic_edges: BTreeSet<(Uuid, Uuid, String)>,
}

fn read<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_per_unit_parity")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[tokio::test]
async fn the_graph_built_per_data_item_matches_python() {
    let input: Input = read("input.json");
    let expected: Expected = read("expected.json");

    // The pipeline's chunks: positions within each data item decide the
    // block; every data item here is far below the default 2000.
    let mut index_in_document: HashMap<Uuid, usize> = HashMap::new();
    let chunks: Vec<DocumentChunk> = input
        .chunks
        .iter()
        .map(|chunk| {
            let index = index_in_document.entry(chunk.data_id).or_default();
            let document_chunk = DocumentChunk::new(
                chunk.chunk_id,
                String::new(),
                1,
                *index,
                "sentence_end".to_string(),
                chunk.data_id,
            );
            *index += 1;
            document_chunk
        })
        .collect();
    let units = extraction_units(
        &chunks,
        cognee_cognify::config::DEFAULT_CHUNKS_PER_BATCH,
        input
            .chunks
            .into_iter()
            .map(|chunk| (chunk.data_id, chunk.chunk_id, (chunk.chunk_id, chunk.graph))),
    );
    assert_eq!(units.len(), 3, "one unit per data item");

    let expansion = expand_extracted_graphs_by_unit(
        units,
        Uuid::new_v4(),
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
        std::sync::Arc::new(NoOpOntologyResolver::new()),
        None,
        None,
        EndpointScope::Chunk,
    )
    .await;

    let entity_ids: BTreeSet<Uuid> = expansion
        .nodes
        .iter()
        .map(|pair| pair.entity.base.id)
        .collect();
    assert_eq!(entity_ids, expected.entity_ids);

    let entity_types: BTreeSet<(Uuid, Uuid)> = expansion
        .nodes
        .iter()
        .flat_map(|pair| {
            let id = pair.entity.base.id;
            pair.entity.type_ids().map(move |type_id| (id, type_id))
        })
        .collect();
    assert_eq!(entity_types, expected.entity_types);

    let type_names: BTreeMap<Uuid, String> = expansion
        .nodes
        .iter()
        .flat_map(|pair| pair.entity_types())
        .map(|entity_type| (entity_type.base.id, entity_type.name.clone()))
        .collect();
    assert_eq!(type_names, expected.type_names);

    let timestamp_ids: BTreeSet<Uuid> = expansion
        .timestamps
        .iter()
        .map(|node| node.timestamp.base.id)
        .collect();
    assert_eq!(timestamp_ids, expected.timestamp_ids);

    let edges: BTreeSet<(Uuid, Uuid, String)> = expansion
        .edges
        .iter()
        .map(|edge| edge.dedup_key())
        .collect();
    // Chunk-local, id-only resolution is Python's rule, so the edge sets are
    // equal; the subset check first names what is missing, if anything.
    assert!(
        expected.semantic_edges.is_subset(&edges),
        "Python edges Rust lacks: {:?}",
        expected
            .semantic_edges
            .difference(&edges)
            .collect::<Vec<_>>()
    );
    assert_eq!(edges, expected.semantic_edges);

    // The fixture's one entity in every data item: one type per data item,
    // the first chunk's in the data item it was declared twice in.
    let apple = cognee_models::Entity::id_for("Apple");
    let apple_types: Vec<&str> = expansion
        .nodes
        .iter()
        .find(|pair| pair.entity.base.id == apple)
        .unwrap()
        .entity_types()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(apple_types, ["company", "fruit", "brand"]);
}
