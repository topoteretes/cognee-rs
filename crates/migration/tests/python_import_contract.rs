//! The on-disk contract Python cognee's COGX importer relies on.
//!
//! The cross-SDK roundtrip in `e2e-cross-sdk/harness/test_cogx_roundtrip.py`
//! proves the two SDKs actually interoperate, but it needs an LLM to produce a
//! graph worth exporting and so only runs on key-gated CI lanes. This suite
//! pins the same contract against a hand-built graph shaped like real cognify
//! output, deterministically, on every `cargo test`.
//!
//! Each assertion below maps to a specific line of Python behaviour, cited in
//! the test, because most of these fail *silently* on the Python side — a
//! wrong `source_system` or a missing raw node produces a successful import
//! with quietly wrong data, not an error.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use cognee_graph::{EdgeData, GraphNode, NodeData};
use cognee_migration::{ExportOptions, write_cogx};
use serde_json::{Value, json};

const DOC_ID: &str = "0198c1e0-0000-7000-8000-00000000d0c0";
const CHUNK_ID: &str = "0198c1e0-0000-7000-8000-00000000c8c8";
const ALICE_ID: &str = "0198c1e0-0000-7000-8000-0000000a11ce";
const BOB_ID: &str = "0198c1e0-0000-7000-8000-00000000b0b0";
const PERSON_TYPE_ID: &str = "0198c1e0-0000-7000-8000-000000009e50";
const NODE_SET_ID: &str = "0198c1e0-0000-7000-8000-0000000c0115";

fn node(id: &str, pairs: &[(&'static str, Value)]) -> GraphNode {
    let mut data = NodeData::new();
    data.insert(Cow::Borrowed("id"), json!(id));
    for (key, value) in pairs {
        data.insert(Cow::Borrowed(*key), value.clone());
    }
    (id.to_string(), data)
}

fn edge(source: &str, target: &str, relationship: &str) -> EdgeData {
    (
        source.to_string(),
        target.to_string(),
        relationship.to_string(),
        HashMap::new(),
    )
}

/// A graph shaped like real cognify output: a document, a chunk, two entities,
/// an entity type, and a node set, wired together.
fn cognify_shaped_graph() -> (Vec<GraphNode>, Vec<EdgeData>) {
    let nodes = vec![
        node(
            DOC_ID,
            &[
                ("type", json!("TextDocument")),
                ("name", json!("nlp.txt")),
                ("created_at", json!(1_768_164_683_000_i64)),
            ],
        ),
        node(
            CHUNK_ID,
            &[
                ("type", json!("DocumentChunk")),
                ("text", json!("Alice knows Bob.")),
                ("chunk_index", json!(0)),
                ("created_at", json!(1_768_164_683_000_i64)),
                ("metadata", json!({"index_fields": ["text"]})),
            ],
        ),
        node(
            ALICE_ID,
            &[
                ("type", json!("Entity")),
                ("name", json!("Alice")),
                ("description", json!("A person named Alice")),
                ("created_at", json!(1_768_164_683_000_i64)),
            ],
        ),
        node(
            BOB_ID,
            &[
                ("type", json!("Entity")),
                ("name", json!("Bob")),
                ("description", json!("A person named Bob")),
            ],
        ),
        node(
            PERSON_TYPE_ID,
            &[
                ("type", json!("EntityType")),
                ("name", json!("Person")),
                ("description", json!("Person")),
            ],
        ),
        node(
            NODE_SET_ID,
            &[("type", json!("NodeSet")), ("name", json!("e2e_test"))],
        ),
    ];

    // One edge carries the temporal/annotation properties COGX can represent,
    // so the golden exercises them end to end on the Python side too.
    let mut knows_properties = HashMap::new();
    knows_properties.insert(Cow::Borrowed("edge_text"), json!("Alice knows Bob"));
    knows_properties.insert(Cow::Borrowed("valid_at"), json!(1_768_164_683_000_i64));
    knows_properties.insert(Cow::Borrowed("confidence"), json!(0.87));

    let edges = vec![
        edge(CHUNK_ID, DOC_ID, "is_part_of"),
        edge(CHUNK_ID, ALICE_ID, "contains"),
        edge(CHUNK_ID, BOB_ID, "contains"),
        edge(ALICE_ID, PERSON_TYPE_ID, "is_a"),
        edge(BOB_ID, PERSON_TYPE_ID, "is_a"),
        (
            ALICE_ID.to_string(),
            BOB_ID.to_string(),
            "knows".to_string(),
            knows_properties,
        ),
        edge(ALICE_ID, NODE_SET_ID, "belongs_to_set"),
    ];

    (nodes, edges)
}

fn read_jsonl(archive: &Path, name: &str) -> Vec<Value> {
    let path = archive.join(name);
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

fn export_fixture(dir: &Path) -> std::path::PathBuf {
    let (nodes, edges) = cognify_shaped_graph();
    let archive = dir.join("archive");
    write_cogx(
        &nodes,
        &edges,
        &archive,
        &ExportOptions {
            embedding_model: Some("openai/text-embedding-3-small".to_string()),
            dataset_name: Some("e2e_test".to_string()),
        },
    )
    .unwrap();
    archive
}

#[test]
fn archive_uses_the_file_names_pythons_reader_looks_for() {
    // `read_archive` iterates a fixed RECORD_FILES map plus nodes.jsonl; a file
    // under any other name is simply never read.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    for name in [
        "manifest.json",
        "entities.jsonl",
        "facts.jsonl",
        "nodes.jsonl",
    ] {
        assert!(
            archive.join(name).exists(),
            "{name} missing from the archive"
        );
    }
    assert!(
        archive.join("documents.jsonl").exists(),
        "documents.jsonl missing — the DocumentChunk produced no typed record"
    );
}

#[test]
fn manifest_carries_the_fields_the_importer_branches_on() {
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(archive.join("manifest.json")).unwrap())
            .unwrap();

    // loader.py: `preserve_source_ids = source.source_system == "cognee"`,
    // where the value comes from this field via COGXArchiveSource.
    assert_eq!(manifest["source_system"], json!("cognee"));
    // cogx.py `validate_cogx_version` rejects a newer *major* version.
    assert_eq!(manifest["cogx_version"], json!("0.1"));
    // None = "unknown", which leaves the target's own Alembic stamp alone.
    assert!(manifest.get("migration_revision").is_none());
    assert!(manifest["exported_at"].is_string());
    assert_eq!(manifest["counts"]["entity"], json!(2));
    assert_eq!(manifest["counts"]["document"], json!(1));
    assert_eq!(manifest["counts"]["fact"], json!(7));
    // Every node, typed or not, is also written verbatim — the typed records
    // have no slot for most properties, so the raw copy is what preserves them.
    assert_eq!(manifest["counts"]["raw_node"], json!(6));
}

#[test]
fn every_record_line_is_a_standalone_json_object_with_its_kind() {
    // read_archive parses one record per line with a discriminated TypeAdapter;
    // a pretty-printed or untagged record breaks the whole file.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    for (file, kind) in [
        ("entities.jsonl", "entity"),
        ("documents.jsonl", "document"),
        ("facts.jsonl", "fact"),
    ] {
        let records = read_jsonl(&archive, file);
        assert!(!records.is_empty(), "{file} is empty");
        for record in records {
            assert_eq!(record["kind"], json!(kind), "wrong kind in {file}");
            assert_eq!(record["external_system"], json!("cognee"));
            assert!(
                record["external_id"].is_string(),
                "{file} record has no external_id"
            );
        }
    }
}

#[test]
fn entity_external_ids_are_the_source_node_uuids() {
    // `_register_entity` keeps external_id as the node id only when it parses
    // as a UUID; anything else silently falls back to Entity.id_for(name).
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let ids: HashSet<String> = read_jsonl(&archive, "entities.jsonl")
        .iter()
        .map(|record| record["external_id"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        ids,
        HashSet::from([ALICE_ID.to_string(), BOB_ID.to_string()])
    );
    for id in &ids {
        assert_eq!(id.len(), 36, "external_id {id} is not a hyphenated UUID");
        assert_eq!(id.matches('-').count(), 4);
    }
}

#[test]
fn no_fact_endpoint_dangles() {
    // `_build_graph_batches` skips a fact whose UUID refs resolve to nothing
    // and only logs a warning, so a dangling ref costs an edge, not a failure.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let mut resolvable: HashSet<String> = read_jsonl(&archive, "entities.jsonl")
        .iter()
        .map(|record| record["external_id"].as_str().unwrap().to_string())
        .collect();
    resolvable.extend(
        read_jsonl(&archive, "nodes.jsonl")
            .iter()
            .map(|record| record["id"].as_str().unwrap().to_string()),
    );

    let facts = read_jsonl(&archive, "facts.jsonl");
    assert_eq!(facts.len(), 7);
    for fact in facts {
        for key in ["subject_ref", "object_ref"] {
            let reference = fact[key].as_str().unwrap();
            assert!(
                resolvable.contains(reference),
                "fact {key}={reference} is not an exported node; \
                 Python would drop this edge"
            );
        }
    }
}

#[test]
fn document_chunks_are_written_both_typed_and_raw() {
    // The raw node preserves topology (chunk -contains-> entity resolves); the
    // typed document carries the text for hybrid / re-derive imports.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let documents = read_jsonl(&archive, "documents.jsonl");
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["external_id"], json!(CHUNK_ID));
    assert_eq!(documents[0]["content"], json!("Alice knows Bob."));

    let raw_ids: HashSet<String> = read_jsonl(&archive, "nodes.jsonl")
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        raw_ids.contains(CHUNK_ID),
        "chunk has no raw node; facts pointing at it would be skipped"
    );
}

#[test]
fn typed_nodes_are_also_written_verbatim_so_their_properties_survive() {
    // Regression: entities used to be written ONLY as typed records.
    // CogxEntity has slots for name/description/timestamps and nothing else,
    // and Python's _register_entity builds Entity(id, name, description, is_a),
    // so against cognee 1.5.3 such an entity came back with ontology_uri=None,
    // version=1, topological_rank=0 and created_at reset to import time.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let raw_ids: HashSet<String> = read_jsonl(&archive, "nodes.jsonl")
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_string())
        .collect();

    for (id, label) in [
        (ALICE_ID, "Entity Alice"),
        (BOB_ID, "Entity Bob"),
        (CHUNK_ID, "DocumentChunk"),
        (DOC_ID, "TextDocument"),
        (PERSON_TYPE_ID, "EntityType"),
        (NODE_SET_ID, "NodeSet"),
    ] {
        assert!(
            raw_ids.contains(id),
            "{label} has no raw node, so every property beyond the typed \
             record's fields is lost on import"
        );
    }

    // And the raw copy really does carry the extras.
    let alice = read_jsonl(&archive, "nodes.jsonl")
        .into_iter()
        .find(|record| record["id"] == json!(ALICE_ID))
        .unwrap();
    assert_eq!(alice["created_at"], json!(1_768_164_683_000_i64));
    assert_eq!(alice["type"], json!("Entity"));
}

#[test]
fn unmapped_node_types_keep_every_property() {
    // rehydrate_node feeds these straight back into a DataPoint, so anything
    // dropped here is lost from the imported graph.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let raw = read_jsonl(&archive, "nodes.jsonl");
    let doc = raw
        .iter()
        .find(|record| record["id"] == json!(DOC_ID))
        .expect("TextDocument was not persisted as a raw node");

    assert_eq!(doc["type"], json!("TextDocument"));
    assert_eq!(doc["name"], json!("nlp.txt"));
    // Epoch-ms passes through verbatim: DataPoint.created_at is an int field,
    // and Python's parse_timestamp scales it correctly when it needs a datetime.
    assert_eq!(doc["created_at"], json!(1_768_164_683_000_i64));

    let chunk = raw
        .iter()
        .find(|record| record["id"] == json!(CHUNK_ID))
        .unwrap();
    assert_eq!(chunk["metadata"]["index_fields"], json!(["text"]));
}

#[test]
fn timestamps_are_emitted_as_offset_qualified_iso_strings() {
    // Python reads an offset-less timestamp as UTC; being explicit removes the
    // ambiguity entirely and keeps archives diffable across SDKs.
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());

    let alice = read_jsonl(&archive, "entities.jsonl")
        .into_iter()
        .find(|record| record["name"] == json!("Alice"))
        .unwrap();

    let created = alice["created_at"].as_str().unwrap();
    assert!(
        created.ends_with('Z') || created.contains('+'),
        "created_at {created} carries no UTC offset"
    );
    assert!(created.starts_with("2026-01-11T"), "{created}");
}

#[test]
fn reexport_into_the_same_directory_replaces_rather_than_appends() {
    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());
    let first = read_jsonl(&archive, "facts.jsonl").len();

    let archive = export_fixture(dir.path());
    let second = read_jsonl(&archive, "facts.jsonl").len();

    assert_eq!(first, second, "re-export duplicated records");
}

/// The committed archive the no-LLM cross-SDK test imports with Python.
///
/// `e2e-cross-sdk/harness/test_cogx_import_contract.py` runs Python cognee's
/// real migration loader over this directory, which is what lets the Rust →
/// Python contract be gated on every PR instead of only on the key-gated
/// lanes. Keeping it in the tree means it can drift from the writer, so this
/// test regenerates and compares it.
#[test]
fn committed_golden_archive_still_matches_what_the_writer_produces() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../e2e-cross-sdk/harness/golden/cogx_archive");

    let dir = tempfile::tempdir().unwrap();
    let fresh = export_fixture(dir.path());

    // `exported_at` is a wall-clock stamp, so it can never match; every other
    // byte must.
    let comparable = |archive: &Path, name: &str| -> String {
        let path = archive.join(name);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        if name != "manifest.json" {
            return body;
        }
        let mut manifest: Value = serde_json::from_str(&body).unwrap();
        manifest
            .as_object_mut()
            .unwrap()
            .insert("exported_at".to_string(), json!("<stamped at export>"));
        serde_json::to_string_pretty(&manifest).unwrap()
    };

    let files = [
        "manifest.json",
        "entities.jsonl",
        "documents.jsonl",
        "facts.jsonl",
        "nodes.jsonl",
    ];

    if std::env::var("COGX_REGENERATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(&golden).unwrap();
        for name in files {
            std::fs::copy(fresh.join(name), golden.join(name)).unwrap();
        }
        return;
    }

    assert!(
        golden.join("manifest.json").exists(),
        "golden archive missing at {}. Regenerate with:\n  \
         COGX_REGENERATE_GOLDEN=1 cargo test -p cognee-migration \
         --test python_import_contract",
        golden.display()
    );

    for name in files {
        assert_eq!(
            comparable(&fresh, name),
            comparable(&golden, name),
            "golden archive is stale for {name}. If the writer change is \
             intended, regenerate with:\n  COGX_REGENERATE_GOLDEN=1 cargo test \
             -p cognee-migration --test python_import_contract"
        );
    }
}

#[cfg(feature = "archive")]
#[test]
fn packed_tarball_puts_the_manifest_where_find_archive_root_looks() {
    use flate2::read::GzDecoder;

    let dir = tempfile::tempdir().unwrap();
    let archive = export_fixture(dir.path());
    let tar_path =
        cognee_migration::pack_archive(&archive, dir.path().join("e2e_test.cogx.tar.gz")).unwrap();

    let names: Vec<String> =
        tar::Archive::new(GzDecoder::new(std::fs::File::open(&tar_path).unwrap()))
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect();

    // find_archive_root accepts manifest.json at the root or one level down.
    assert!(
        names.contains(&"manifest.json".to_string()),
        "manifest is not at the tarball root: {names:?}"
    );
    for name in ["entities.jsonl", "facts.jsonl", "nodes.jsonl"] {
        assert!(
            names.contains(&name.to_string()),
            "{name} missing: {names:?}"
        );
    }
}
