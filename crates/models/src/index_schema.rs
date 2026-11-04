//! Vector payloads in the shape of Python's `IndexSchema`.
//!
//! Python writes every vector row through one schema, `IndexSchema(DataPoint)`
//! (`cognee/infrastructure/databases/vector/lancedb/LanceDBAdapter.py:74-103`,
//! the pgvector and Turso adapters carry a copy), built in
//! `index_data_points` (`LanceDBAdapter.py:1684-1706`):
//!
//! * `id`, `text` (the indexed field's value), and the reference scalars
//!   `document_id`, `document_name`, `chunk_index`, `source_chunk_id`,
//!   `importance_weight`, `external_metadata` copied from the source point
//!   (`None` when it has no such field), plus `belongs_to_set`;
//! * every other `DataPoint` base field at its **IndexSchema default**, not the
//!   source point's value: `type = "IndexSchema"`, `version = 1`,
//!   `topological_rank = 0`, fresh `created_at` / `updated_at`,
//!   `ontology_valid = false`, `ontology_uri = None`, `source_* = None`,
//!   `feedback_weight = 0.5`;
//! * `metadata = {"index_fields": ["text"]}`. Whether it is stored depends on
//!   the adapter: Python's LanceDB adapter excludes it from the table schema
//!   (`_build_data_point_schema(exclude_fields=["metadata"])`), pgvector and
//!   Turso store it. [`index_schema_payload`] always emits it; the LanceDB
//!   adapter drops it on write.
//!
//! Keys whose value is `None` are written as JSON `null`, not omitted: Python's
//! stored row carries every schema field (see the golden fixtures under
//! `crates/cognify/tests/fixtures/index_schema/`, dumped from a real Python run).
//!
//! # Rust-only keys
//!
//! [`RUST_ONLY_PAYLOAD_KEYS`] lists the keys Rust adds on top of the schema:
//!
//! * `dataset_id`: the dataset the point is being indexed for. Dataset-scoped
//!   search filters on it (`cognee_vector::DATASET_ID_KEY`); the vector
//!   adapters fold it into a `dataset_ids` union when a content-addressed point
//!   is re-indexed for a second dataset. Python scopes by per-dataset stores
//!   instead and has no such key.
//!
//! Nothing else is added. In particular the indexed point's own fields (an
//! entity's description, a chunk's `chunker_id`, a custom model's properties)
//! never reach the payload, as in Python.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::DataPoint;

/// `type` of every payload [`index_schema_payload`] builds.
pub const INDEX_SCHEMA_TYPE: &str = "IndexSchema";

/// Key of the `{"index_fields": ["text"]}` map. Stored by pgvector, dropped by
/// LanceDB (see the module docs).
pub const INDEX_SCHEMA_METADATA_KEY: &str = "metadata";

/// The keys of Python's stored `IndexSchema` row, `metadata` included.
pub const INDEX_SCHEMA_KEYS: &[&str] = &[
    "id",
    "text",
    "document_id",
    "document_name",
    "chunk_index",
    "source_chunk_id",
    "importance_weight",
    "external_metadata",
    "belongs_to_set",
    "type",
    "version",
    "topological_rank",
    "created_at",
    "updated_at",
    "ontology_valid",
    "ontology_uri",
    "source_pipeline",
    "source_task",
    "source_node_set",
    "source_user",
    "source_content_hash",
    "feedback_weight",
    INDEX_SCHEMA_METADATA_KEY,
];

/// The keys Rust adds to the schema, each needed by a reader (module docs).
pub const RUST_ONLY_PAYLOAD_KEYS: &[&str] = &["dataset_id"];

/// The reference scalars an index site copies from its source point.
///
/// Python reads each with `getattr(data_point, name, None)`, so a type without
/// the field stores `null`; a field left `None` here does the same.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexRefs<'a> {
    /// Source document of a chunk (`DocumentChunk.document_id`).
    pub document_id: Option<Uuid>,
    /// Display name of a chunk's source document (`DocumentChunk.document_name`).
    pub document_name: Option<&'a str>,
    /// Position of a chunk in its document.
    pub chunk_index: Option<usize>,
    /// Chunk a summary was made from (`TextSummary.source_chunk_id`).
    pub source_chunk_id: Option<Uuid>,
    /// External metadata as JSON text (`Document.external_metadata`, and the
    /// copy Python's chunks carry).
    pub external_metadata: Option<&'a str>,
}

/// The vector payload Python's `IndexSchema` stores for `source`, indexed under
/// `text`, plus the [`RUST_ONLY_PAYLOAD_KEYS`].
///
/// `id`, `importance_weight` and `belongs_to_set` come from `source`; the other
/// base fields take their IndexSchema defaults (module docs). `belongs_to_set`
/// holds node-set names, as Python's graph model flattens them
/// (`get_graph_from_model._belongs_to_set_names`): a string entry is kept, an
/// object entry contributes its `name`, anything else is dropped. Rust's
/// `DataPoint::new(_, Some(dataset))` also records the dataset id there; that
/// entry is not a node set, Python has no such entry, and the payload carries
/// the dataset in `dataset_id`, so the entry equal to `dataset_id` is dropped.
///
/// `dataset_id` is written when given; a dataset-less write (the whole-graph
/// edge re-index) has none.
pub fn index_schema_payload(
    source: &DataPoint,
    text: String,
    refs: &IndexRefs<'_>,
    dataset_id: Option<Uuid>,
) -> HashMap<String, Value> {
    let now = Utc::now().timestamp_millis();
    let optional_str = |value: Option<&str>| value.map_or(Value::Null, |s| json!(s));
    let optional_uuid = |value: Option<Uuid>| value.map_or(Value::Null, |id| json!(id.to_string()));

    let mut payload = HashMap::with_capacity(INDEX_SCHEMA_KEYS.len() + 1);
    payload.insert("id".to_string(), json!(source.id.to_string()));
    payload.insert("text".to_string(), Value::String(text));
    payload.insert("document_id".to_string(), optional_uuid(refs.document_id));
    payload.insert(
        "document_name".to_string(),
        optional_str(refs.document_name),
    );
    payload.insert(
        "chunk_index".to_string(),
        refs.chunk_index.map_or(Value::Null, |i| json!(i)),
    );
    payload.insert(
        "source_chunk_id".to_string(),
        optional_uuid(refs.source_chunk_id),
    );
    payload.insert(
        "importance_weight".to_string(),
        source.importance_weight.map_or(Value::Null, |w| json!(w)),
    );
    payload.insert(
        "external_metadata".to_string(),
        optional_str(refs.external_metadata),
    );
    payload.insert(
        "belongs_to_set".to_string(),
        Value::Array(belongs_to_set_names(
            source.belongs_to_set.as_deref(),
            dataset_id,
        )),
    );
    payload.insert("type".to_string(), json!(INDEX_SCHEMA_TYPE));
    payload.insert("version".to_string(), json!(1));
    payload.insert("topological_rank".to_string(), json!(0));
    payload.insert("created_at".to_string(), json!(now));
    payload.insert("updated_at".to_string(), json!(now));
    payload.insert("ontology_valid".to_string(), json!(false));
    for key in [
        "ontology_uri",
        "source_pipeline",
        "source_task",
        "source_node_set",
        "source_user",
        "source_content_hash",
    ] {
        payload.insert(key.to_string(), Value::Null);
    }
    payload.insert("feedback_weight".to_string(), json!(0.5));
    payload.insert(
        INDEX_SCHEMA_METADATA_KEY.to_string(),
        json!({"index_fields": ["text"]}),
    );
    if let Some(dataset_id) = dataset_id {
        payload.insert("dataset_id".to_string(), json!(dataset_id.to_string()));
    }
    payload
}

/// Node-set names of a `belongs_to_set` list (see [`index_schema_payload`]).
fn belongs_to_set_names(entries: Option<&[Value]>, dataset_id: Option<Uuid>) -> Vec<Value> {
    let dataset = dataset_id.map(|id| id.to_string());
    entries
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| match entry {
            Value::String(name) if Some(name) == dataset.as_ref() => None,
            Value::String(_) => Some(entry.clone()),
            Value::Object(object) => object
                .get("name")
                .and_then(Value::as_str)
                .map(|name| json!(name)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn source() -> DataPoint {
        let mut dp = DataPoint::new("DocumentChunk", None);
        dp.source_pipeline = Some("cognify_pipeline".into());
        dp.source_task = Some("extract_chunks_from_documents".into());
        dp.source_user = Some("alice@example.com".into());
        dp.source_content_hash = Some("md5:abc".into());
        dp.topological_rank = Some(2);
        dp.version = 3;
        dp.ontology_valid = true;
        dp.feedback_weight = 0.9;
        dp.importance_weight = Some(0.7);
        dp.set_metadata("index_fields", json!(["text"]));
        dp
    }

    #[test]
    fn emits_exactly_the_schema_keys_plus_dataset_id() {
        let dataset = Uuid::new_v4();
        let payload =
            index_schema_payload(&source(), "t".into(), &IndexRefs::default(), Some(dataset));
        let keys: BTreeSet<&str> = payload.keys().map(String::as_str).collect();
        let expected: BTreeSet<&str> = INDEX_SCHEMA_KEYS
            .iter()
            .chain(RUST_ONLY_PAYLOAD_KEYS)
            .copied()
            .collect();
        assert_eq!(keys, expected);
        assert_eq!(payload["dataset_id"], json!(dataset.to_string()));
    }

    #[test]
    fn no_dataset_means_no_dataset_key() {
        let payload = index_schema_payload(&source(), "t".into(), &IndexRefs::default(), None);
        assert!(!payload.contains_key("dataset_id"));
    }

    #[test]
    fn base_fields_take_index_schema_defaults_not_the_source_values() {
        let payload = index_schema_payload(&source(), "t".into(), &IndexRefs::default(), None);
        assert_eq!(payload["type"], json!("IndexSchema"));
        assert_eq!(payload["version"], json!(1));
        assert_eq!(payload["topological_rank"], json!(0));
        assert_eq!(payload["ontology_valid"], json!(false));
        assert_eq!(payload["feedback_weight"], json!(0.5));
        for key in [
            "ontology_uri",
            "source_pipeline",
            "source_task",
            "source_node_set",
            "source_user",
            "source_content_hash",
        ] {
            assert_eq!(payload[key], Value::Null, "{key}");
        }
        assert_eq!(payload["metadata"], json!({"index_fields": ["text"]}));
    }

    #[test]
    fn copies_id_text_importance_weight_and_refs() {
        let dp = source();
        let doc = Uuid::new_v4();
        let chunk = Uuid::new_v4();
        let refs = IndexRefs {
            document_id: Some(doc),
            document_name: Some("alice.txt"),
            chunk_index: Some(4),
            source_chunk_id: Some(chunk),
            external_metadata: Some(r#"{"k": "v"}"#),
        };
        let payload = index_schema_payload(&dp, "hello".into(), &refs, None);
        assert_eq!(payload["id"], json!(dp.id.to_string()));
        assert_eq!(payload["text"], json!("hello"));
        assert_eq!(payload["importance_weight"], json!(0.7));
        assert_eq!(payload["document_id"], json!(doc.to_string()));
        assert_eq!(payload["document_name"], json!("alice.txt"));
        assert_eq!(payload["chunk_index"], json!(4));
        assert_eq!(payload["source_chunk_id"], json!(chunk.to_string()));
        assert_eq!(payload["external_metadata"], json!(r#"{"k": "v"}"#));
    }

    #[test]
    fn absent_refs_are_null_not_missing() {
        let payload = index_schema_payload(&source(), "t".into(), &IndexRefs::default(), None);
        for key in [
            "document_id",
            "document_name",
            "chunk_index",
            "source_chunk_id",
            "external_metadata",
        ] {
            assert_eq!(payload.get(key), Some(&Value::Null), "{key}");
        }
    }

    #[test]
    fn belongs_to_set_holds_node_set_names() {
        let mut dp = source();
        dp.belongs_to_set = Some(vec![
            json!("plain"),
            json!({"id": "1", "name": "alpha", "type": "NodeSet"}),
            json!({"id": "2", "type": "NodeSet"}),
            json!(7),
        ]);
        let payload = index_schema_payload(&dp, "t".into(), &IndexRefs::default(), None);
        assert_eq!(payload["belongs_to_set"], json!(["plain", "alpha"]));

        dp.belongs_to_set = None;
        let payload = index_schema_payload(&dp, "t".into(), &IndexRefs::default(), None);
        assert_eq!(payload["belongs_to_set"], json!([]));
    }

    #[test]
    fn the_indexed_dataset_is_not_a_node_set() {
        let dataset = Uuid::new_v4();
        let mut dp = DataPoint::new("EdgeType", Some(dataset));
        dp.belongs_to_set
            .get_or_insert_with(Vec::new)
            .push(json!({"name": "alpha"}));
        let payload = index_schema_payload(&dp, "t".into(), &IndexRefs::default(), Some(dataset));
        assert_eq!(payload["belongs_to_set"], json!(["alpha"]));
        assert_eq!(payload["dataset_id"], json!(dataset.to_string()));
    }
}
