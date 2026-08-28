//! Map a knowledge graph onto COGX records.
//!
//! A port of Python cognee's `_write_cogx` (`cognee/modules/migration/export.py`).
//! The mapping is deliberately identical, because the Python importer's
//! behaviour is keyed off the exact shapes produced here:
//!
//! * `Entity` nodes with a name become typed [`CogxEntity`] records. Their
//!   `external_id` is the node UUID, which Python keeps verbatim as the
//!   imported node id (see [`SOURCE_SYSTEM`]).
//! * `DocumentChunk` nodes with text become a [`CogxDocument`] **and** a raw
//!   node. Both are needed: the raw node preserves graph topology so facts
//!   pointing at the chunk resolve, while the document carries the text for
//!   `hybrid`/`re-derive` imports that re-cognify content.
//! * Everything else is persisted verbatim as a raw node.
//! * Every edge becomes a [`CogxFact`].

use std::path::{Path, PathBuf};

use cognee_graph::{EdgeData, GraphDBTrait, GraphNode};
use tracing::info;

use crate::cogx::{
    CogxArchiveWriter, CogxDocument, CogxEntity, CogxFact, CogxRecord, CogxScope, SOURCE_SYSTEM,
    parse_timestamp,
};
use crate::error::MigrationResult;

/// Knobs for a COGX export.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Embedding model recorded in the manifest. Advisory only — the archive
    /// carries no vectors, so the importing instance re-embeds.
    pub embedding_model: Option<String>,
    /// Dataset name, used only for the manifest's human-readable note.
    pub dataset_name: Option<String>,
}

/// What an export produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    /// Directory the archive was written to.
    pub destination: PathBuf,
    /// Graph nodes read.
    pub num_nodes: usize,
    /// Graph edges read.
    pub num_edges: usize,
    /// Typed entity records emitted.
    pub num_entities: usize,
    /// Typed document records emitted.
    pub num_documents: usize,
    /// Fact records emitted (one per edge).
    pub num_facts: usize,
    /// Nodes persisted verbatim.
    pub num_raw_nodes: usize,
}

/// Read a graph and write it as a COGX archive directory.
pub async fn export_graph(
    graph_db: &dyn GraphDBTrait,
    destination: impl AsRef<Path>,
    options: &ExportOptions,
) -> MigrationResult<ExportSummary> {
    let (nodes, edges) = graph_db.get_graph_data().await?;
    write_cogx(&nodes, &edges, destination.as_ref(), options)
}

/// Write nodes and edges as a COGX archive directory.
///
/// Pure: no database access, so the mapping is unit-testable on its own.
pub fn write_cogx(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    destination: &Path,
    options: &ExportOptions,
) -> MigrationResult<ExportSummary> {
    let mut writer = CogxArchiveWriter::new(destination)?;
    writer.set_embedding_model(options.embedding_model.clone());

    // The note must not claim a dataset scope the archive does not have.
    // `get_graph_data()` returns the whole store — cognee-rs has no
    // per-dataset graph partition — so an archive labelled "dataset X" would
    // in fact carry every other dataset's nodes too. On a tarball shipped to
    // another party that is a disclosure hazard, not just a stale label.
    writer.add_note(
        "Exported from cognee-rs. Contains the ENTIRE graph store, not a single \
         dataset: cognee-rs has no per-dataset graph partition."
            .to_string(),
    );
    if let Some(name) = &options.dataset_name {
        writer.add_note(format!("Requested dataset label: '{name}' (label only)."));
    }

    let mut summary = ExportSummary {
        destination: destination.to_path_buf(),
        num_nodes: nodes.len(),
        num_edges: edges.len(),
        num_entities: 0,
        num_documents: 0,
        num_facts: 0,
        num_raw_nodes: 0,
    };

    for (node_id, properties) in nodes {
        let node_type = properties.get("type").and_then(|value| value.as_str());
        let name = non_empty_str(properties.get("name"));
        let text = non_empty_str(properties.get("text"));

        match (node_type, name, text) {
            (Some("Entity"), Some(name), _) => {
                writer.write(&CogxRecord::Entity(CogxEntity {
                    kind: "entity".to_string(),
                    external_system: SOURCE_SYSTEM.to_string(),
                    external_id: node_id.clone(),
                    scope: CogxScope::default(),
                    created_at: parse_timestamp(properties.get("created_at")),
                    updated_at: parse_timestamp(properties.get("updated_at")),
                    metadata: serde_json::Map::new(),
                    name: name.to_string(),
                    entity_type: None,
                    description: non_empty_str(properties.get("description")).map(str::to_string),
                    aliases: Vec::new(),
                    attributes: serde_json::Map::new(),
                }))?;
                summary.num_entities += 1;
            }
            (Some("DocumentChunk"), _, Some(text)) => {
                writer.write(&CogxRecord::Document(CogxDocument {
                    kind: "document".to_string(),
                    external_system: SOURCE_SYSTEM.to_string(),
                    external_id: node_id.clone(),
                    scope: CogxScope::default(),
                    created_at: parse_timestamp(properties.get("created_at")),
                    updated_at: None,
                    metadata: serde_json::Map::new(),
                    content: text.to_string(),
                    title: None,
                    mime_type: None,
                }))?;
                summary.num_documents += 1;

                // Also persist the chunk verbatim. Preserve-mode restore
                // rehydrates it as a graph node, so facts referencing the
                // chunk (DocumentChunk -contains-> Entity) keep their
                // topology instead of dangling.
                writer.write_raw_node(&raw_node_value(node_id, properties))?;
                summary.num_raw_nodes += 1;
            }
            _ => {
                writer.write_raw_node(&raw_node_value(node_id, properties))?;
                summary.num_raw_nodes += 1;
            }
        }
    }

    for (source, target, relationship, properties) in edges {
        writer.write(&CogxRecord::Fact(CogxFact {
            kind: "fact".to_string(),
            external_system: SOURCE_SYSTEM.to_string(),
            external_id: format!("{source}:{relationship}:{target}"),
            scope: CogxScope::default(),
            created_at: None,
            updated_at: None,
            metadata: serde_json::Map::new(),
            subject_ref: source.clone(),
            predicate: relationship.clone(),
            object_ref: target.clone(),
            fact_text: non_empty_str(properties.get("edge_text")).map(str::to_string),
            valid_at: parse_timestamp(properties.get("valid_at")),
            invalid_at: parse_timestamp(properties.get("invalid_at")),
            confidence: properties.get("confidence").and_then(|v| v.as_f64()),
            provenance: Vec::new(),
        }))?;
        summary.num_facts += 1;
    }

    writer.finish()?;

    info!(
        nodes = summary.num_nodes,
        edges = summary.num_edges,
        entities = summary.num_entities,
        documents = summary.num_documents,
        raw_nodes = summary.num_raw_nodes,
        destination = %destination.display(),
        "Wrote COGX archive"
    );

    Ok(summary)
}

/// `{"id": node_id, ...properties}` — properties win on a clash, matching
/// Python's `{"id": str(node_id), **properties}`.
///
/// Properties are emitted in sorted key order. `NodeData` is a `HashMap`, so
/// its iteration order changes between runs, and the workspace enables
/// serde_json's `preserve_order` (via `cognee-database`), which makes
/// `serde_json::Map` an insertion-ordered `IndexMap` rather than a sorted
/// `BTreeMap` — together those would leak the hash order straight into the
/// file. JSON object order carries no meaning to the importer, but a
/// non-reproducible export cannot be diffed between runs or checked against a
/// golden archive.
fn raw_node_value(node_id: &str, properties: &cognee_graph::NodeData) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "id".to_string(),
        serde_json::Value::String(node_id.to_string()),
    );

    let mut keys: Vec<&str> = properties.keys().map(|key| key.as_ref()).collect();
    keys.sort_unstable();
    for key in keys {
        if let Some(value) = properties.get(key) {
            map.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(map)
}

/// A non-blank string property, or `None`.
///
/// Python tests these with plain truthiness (`properties.get("name")`), which
/// treats `""` as absent; a bare `is_some()` here would route an empty-named
/// Entity down the typed branch and emit a record Python then imports as a
/// nameless entity.
fn non_empty_str(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::cogx::{CogxManifest, MANIFEST_FILE, RAW_NODES_FILE};
    use serde_json::json;
    use std::borrow::Cow;
    use std::collections::HashMap;

    fn node(id: &str, pairs: &[(&'static str, serde_json::Value)]) -> GraphNode {
        let mut data = HashMap::new();
        data.insert(Cow::Borrowed("id"), json!(id));
        for (key, value) in pairs {
            data.insert(Cow::Borrowed(*key), value.clone());
        }
        (id.to_string(), data)
    }

    fn read_lines(dir: &Path, file: &str) -> Vec<serde_json::Value> {
        let path = dir.join(file);
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn entities_documents_and_unknown_nodes_route_to_the_right_records() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");

        let nodes = vec![
            node(
                "11111111-1111-5111-8111-111111111111",
                &[
                    ("type", json!("Entity")),
                    ("name", json!("Alice")),
                    ("description", json!("A person")),
                ],
            ),
            node(
                "22222222-2222-5222-8222-222222222222",
                &[("type", json!("DocumentChunk")), ("text", json!("hello"))],
            ),
            node(
                "33333333-3333-5333-8333-333333333333",
                &[("type", json!("TextDocument")), ("name", json!("doc.txt"))],
            ),
        ];
        let edges: Vec<EdgeData> = vec![(
            "22222222-2222-5222-8222-222222222222".to_string(),
            "11111111-1111-5111-8111-111111111111".to_string(),
            "contains".to_string(),
            HashMap::new(),
        )];

        let summary = write_cogx(&nodes, &edges, &out, &ExportOptions::default()).unwrap();

        assert_eq!(summary.num_entities, 1);
        assert_eq!(summary.num_documents, 1);
        // The DocumentChunk is written twice — typed *and* raw — plus the
        // unmapped TextDocument.
        assert_eq!(summary.num_raw_nodes, 2);
        assert_eq!(summary.num_facts, 1);

        let entities = read_lines(&out, "entities.jsonl");
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0]["name"], json!("Alice"));
        assert_eq!(
            entities[0]["external_id"],
            json!("11111111-1111-5111-8111-111111111111"),
            "entity external_id must be the node UUID so Python preserves it"
        );
        assert_eq!(entities[0]["external_system"], json!("cognee"));

        let documents = read_lines(&out, "documents.jsonl");
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0]["content"], json!("hello"));

        let raw = read_lines(&out, RAW_NODES_FILE);
        let raw_ids: Vec<_> = raw.iter().map(|node| node["id"].clone()).collect();
        assert!(raw_ids.contains(&json!("22222222-2222-5222-8222-222222222222")));
        assert!(raw_ids.contains(&json!("33333333-3333-5333-8333-333333333333")));

        let facts = read_lines(&out, "facts.jsonl");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["predicate"], json!("contains"));
        assert_eq!(
            facts[0]["external_id"],
            json!(
                "22222222-2222-5222-8222-222222222222:contains:11111111-1111-5111-8111-111111111111"
            )
        );
    }

    #[test]
    fn every_fact_endpoint_resolves_to_an_exported_node() {
        // Python's importer silently SKIPS a fact whose UUID endpoints are not
        // in the archive. Exporting every node is what keeps edges alive, so
        // assert the invariant directly.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");

        let nodes = vec![
            node("a", &[("type", json!("Entity")), ("name", json!("Alice"))]),
            node("b", &[("type", json!("Entity")), ("name", json!("Bob"))]),
        ];
        let edges: Vec<EdgeData> = vec![(
            "a".to_string(),
            "b".to_string(),
            "knows".to_string(),
            HashMap::new(),
        )];

        write_cogx(&nodes, &edges, &out, &ExportOptions::default()).unwrap();

        let exported: std::collections::HashSet<String> = read_lines(&out, "entities.jsonl")
            .iter()
            .chain(read_lines(&out, RAW_NODES_FILE).iter())
            .filter_map(|record| {
                record
                    .get("external_id")
                    .or_else(|| record.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        for fact in read_lines(&out, "facts.jsonl") {
            for key in ["subject_ref", "object_ref"] {
                let reference = fact[key].as_str().unwrap();
                assert!(
                    exported.contains(reference),
                    "fact {key}={reference} has no exported node; Python would drop this edge"
                );
            }
        }
    }

    #[test]
    fn an_entity_with_a_blank_name_falls_back_to_a_raw_node() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");
        let nodes = vec![node("a", &[("type", json!("Entity")), ("name", json!(""))])];

        let summary = write_cogx(&nodes, &[], &out, &ExportOptions::default()).unwrap();

        assert_eq!(summary.num_entities, 0);
        assert_eq!(summary.num_raw_nodes, 1);
    }

    #[test]
    fn raw_nodes_keep_every_property_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");
        let nodes = vec![node(
            "a",
            &[
                ("type", json!("NodeSet")),
                ("name", json!("group")),
                ("created_at", json!(1_768_164_683_000_i64)),
                ("metadata", json!({"index_fields": ["name"]})),
            ],
        )];

        write_cogx(&nodes, &[], &out, &ExportOptions::default()).unwrap();

        let raw = read_lines(&out, RAW_NODES_FILE);
        assert_eq!(raw[0]["type"], json!("NodeSet"));
        assert_eq!(raw[0]["created_at"], json!(1_768_164_683_000_i64));
        assert_eq!(raw[0]["metadata"]["index_fields"], json!(["name"]));
    }

    #[test]
    fn raw_node_property_order_is_stable_across_exports() {
        // Regression: NodeData is a HashMap and the workspace turns
        // serde_json::Map into an insertion-ordered IndexMap, so the hash
        // iteration order used to leak into nodes.jsonl and the same graph
        // exported twice produced different bytes.
        let nodes = vec![node(
            "a",
            &[
                ("type", json!("TextDocument")),
                ("name", json!("doc.txt")),
                ("created_at", json!(1_768_164_683_000_i64)),
                ("version", json!(3)),
                ("owner_id", json!("owner")),
                ("mime_type", json!("text/plain")),
            ],
        )];

        let mut rendered = std::collections::HashSet::new();
        for _ in 0..12 {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("archive");
            write_cogx(&nodes, &[], &out, &ExportOptions::default()).unwrap();
            rendered.insert(std::fs::read_to_string(out.join(RAW_NODES_FILE)).unwrap());
        }

        assert_eq!(
            rendered.len(),
            1,
            "the same graph produced {} different renderings of nodes.jsonl",
            rendered.len()
        );

        let only = rendered.into_iter().next().unwrap();
        assert!(
            only.starts_with(r#"{"id":"a","created_at":"#),
            "id must lead, then properties in sorted order: {only}"
        );
    }

    #[test]
    fn manifest_counts_match_the_records_written() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");
        let nodes = vec![
            node("a", &[("type", json!("Entity")), ("name", json!("Alice"))]),
            node(
                "b",
                &[("type", json!("DocumentChunk")), ("text", json!("t"))],
            ),
        ];
        let edges: Vec<EdgeData> = vec![(
            "b".to_string(),
            "a".to_string(),
            "contains".to_string(),
            HashMap::new(),
        )];

        write_cogx(
            &nodes,
            &edges,
            &out,
            &ExportOptions {
                embedding_model: Some("text-embedding-3-small".to_string()),
                dataset_name: Some("main_dataset".to_string()),
            },
        )
        .unwrap();

        let manifest: CogxManifest =
            serde_json::from_str(&std::fs::read_to_string(out.join(MANIFEST_FILE)).unwrap())
                .unwrap();

        assert_eq!(manifest.counts.get("entity"), Some(&1));
        assert_eq!(manifest.counts.get("document"), Some(&1));
        assert_eq!(manifest.counts.get("fact"), Some(&1));
        assert_eq!(manifest.counts.get("raw_node"), Some(&1));
        assert_eq!(
            manifest.embedding_model.as_deref(),
            Some("text-embedding-3-small")
        );
        // The first note must state the archive is store-wide; the dataset is
        // only ever a label, so it must not be presented as a scope.
        assert!(
            manifest.notes[0].contains("ENTIRE graph store"),
            "{:?}",
            manifest.notes
        );
        assert!(
            manifest
                .notes
                .iter()
                .any(|note| note.contains("main_dataset") && note.contains("label")),
            "the dataset must be recorded as a label, not a scope: {:?}",
            manifest.notes
        );
    }

    #[test]
    fn edge_temporal_validity_survives_onto_the_fact() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("archive");
        let mut props: HashMap<Cow<'static, str>, serde_json::Value> = HashMap::new();
        props.insert(Cow::Borrowed("edge_text"), json!("Alice knows Bob"));
        props.insert(
            Cow::Borrowed("valid_at"),
            json!("2026-01-11T20:51:23+00:00"),
        );
        props.insert(Cow::Borrowed("confidence"), json!(0.9));

        let edges: Vec<EdgeData> =
            vec![("a".to_string(), "b".to_string(), "knows".to_string(), props)];

        write_cogx(&[], &edges, &out, &ExportOptions::default()).unwrap();

        let facts = read_lines(&out, "facts.jsonl");
        assert_eq!(facts[0]["fact_text"], json!("Alice knows Bob"));
        assert_eq!(facts[0]["valid_at"], json!("2026-01-11T20:51:23Z"));
        assert_eq!(facts[0]["confidence"], json!(0.9));
        assert!(facts[0].get("invalid_at").is_none());
    }
}
