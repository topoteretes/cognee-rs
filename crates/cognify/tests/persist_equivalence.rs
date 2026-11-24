#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "test code — panics are acceptable failures, and the snapshot \
              refresh switch is read from the process env"
)]
//! What a cognify run stores, pinned as sets (SDK-1694 O4a).
//!
//! O4a moved every graph, vector and ledger write of the run into one
//! persistence layer, and promised that the move changed nothing that is
//! stored. These tests hold it to that. Each runs a fixed scenario offline and
//! compares, **as sets**, what it left behind against a committed snapshot:
//!
//! - the graph nodes and their properties;
//! - the graph edges and their properties;
//! - the vector points of every collection, vectors and payloads included;
//! - the ownership-ledger node and edge rows;
//! - the edge-evidence rows.
//!
//! The snapshots under `tests/fixtures/persist_equivalence/` were recorded by
//! these same tests running against the code **before** the refactor
//! (`origin/main` at `faf94ea5`), so a pass means the refactored layer stores
//! exactly what the old scattered writes did. Wall-clock values (`created_at`,
//! `updated_at`, `captured_at`) are blanked, run ids are replaced by the run's
//! position, and the edge-evidence row id — a hash that includes the run id —
//! is dropped. Nothing else is normalised.
//!
//! Refresh a snapshot (only for an intended change of stored output) with
//! `COGNEE_UPDATE_PERSIST_SNAPSHOTS=1 cargo test -p cognee-cognify --test
//! persist_equivalence`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_chunking::{TokenCountMode, TokenCounterKind};
use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, LlmError, LlmResult, MessageRole};
use cognee_models::{Data, Dataset};
use cognee_ontology::{
    NoOpOntologyResolver, OntologyFileInput, OntologyResolver, RdfLibOntologyResolver,
};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockVectorDB};
use cognee_vector::VectorDB;
use sea_orm::{DatabaseBackend, FromQueryResult, JsonValue, Statement};
use serde_json::{Value, json};
use uuid::Uuid;

const DATASET_ID: Uuid = Uuid::from_u128(0x0a4a_0000_0000_0000_0000_0000_0000_0001);
const OWNER_ID: Uuid = Uuid::from_u128(0x0a4a_0000_0000_0000_0000_0000_0000_0002);

/// The ontology the default scenario resolves against: `DeepSort` is an
/// Algorithm, a Technology; `TechCorp` an Organisation, a LegalEntity.
const ONTOLOGY: &str = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix : <http://test.cognee.ai/ontology#> .

:LegalEntity a owl:Class ; rdfs:label "LegalEntity" .
:Organisation a owl:Class ; rdfs:subClassOf :LegalEntity ; rdfs:label "Organisation" .
:Technology a owl:Class ; rdfs:label "Technology" .
:Algorithm a owl:Class ; rdfs:subClassOf :Technology ; rdfs:label "Algorithm" .
:DeepSort a :Algorithm ; rdfs:label "DeepSort" .
:TechCorp a :Organisation ; rdfs:label "TechCorp" .
"#;

// ── The scenario's documents ────────────────────────────────────────────────
//
// Every sentence carries a marker the LLM double answers by, so the answer
// depends neither on where the chunker cuts nor on the order calls are
// scheduled in. At `CHUNK_SIZE` doc A is cut into several chunks, so at
// `chunks_per_batch = 1` into several blocks.

const CHUNK_SIZE: usize = 48;

/// Several chunks, so several blocks: `Apple` is a Company in the `MARKA1`
/// ones and an Organization in the `MARKA2` ones, which keeps both types.
const DOC_A: &str = "MARKA1 Alice works at Acme. MARKA1 DeepSort is an algorithm Acme \
uses to rank results, and Apple is a company that partners with Acme on research.\n\n\
MARKA2 Apple is an organization founded in 1867. MARKA2 Alice was born in 1867 as \
well, long before Acme existed, and grew up near the old harbour.";

/// `Apple` as a Fruit, the ontology-resolved `TechCorp`, and the `Alice works
/// at Acme` edge doc A also produces — one edge, two producers.
const DOC_B: &str = "MARKB1 TechCorp sells apples, and Alice works at Acme too.";

/// A URL document, so the WebPage / WebSite nodes are written.
const DOC_C: &str = "MARKC1 Bob knows Carol from the conference.";

/// The second run's document: it produces the `Alice works at Acme` edge the
/// first run already wrote, which is claimed rather than written again.
const DOC_D: &str = "MARKD1 Alice works at Acme, says Dave.";

fn node(id: &str, name: &str, kind: &str, description: &str) -> Value {
    json!({"id": id, "name": name, "type": kind, "description": description})
}

fn edge(source: &str, target: &str, relationship: &str) -> Value {
    json!({"source_node_id": source, "target_node_id": target, "relationship_name": relationship})
}

/// The graph the LLM double answers for the chunk carrying `marker`.
fn graph_for(marker: &str) -> Value {
    match marker {
        "MARKA1" => json!({
            "nodes": [
                node("alice", "Alice", "Person", "A researcher."),
                node("acme", "Acme", "Company", "A company."),
                node("deepsort", "DeepSort", "Algorithm", "A ranking algorithm."),
                node("apple", "Apple", "Company", "A technology company."),
            ],
            "edges": [
                edge("alice", "acme", "works_at"),
                edge("acme", "deepsort", "Uses Algorithm"),
                edge("apple", "acme", "partners_with"),
            ],
        }),
        "MARKA2" => json!({
            "nodes": [
                node("apple", "Apple", "Organization", "An old organization."),
                node("alice", "Alice", "Person", "Born in 1867."),
                node("1867", "1867", "Timestamp", "A year"),
            ],
            "edges": [
                edge("apple", "1867", "founded_in"),
                edge("alice", "1867", "born_in"),
            ],
        }),
        "MARKB1" => json!({
            "nodes": [
                node("techcorp", "TechCorp", "Organisation", "A retailer."),
                node("apple", "Apple", "Fruit", "A fruit."),
                node("alice", "Alice", "Person", "A researcher."),
                node("acme", "Acme", "Company", "A company."),
            ],
            "edges": [
                edge("techcorp", "apple", "sells"),
                edge("alice", "acme", "works_at"),
            ],
        }),
        "MARKC1" => json!({
            "nodes": [
                node("bob", "Bob", "Person", "A speaker."),
                node("carol", "Carol", "Person", "An organiser."),
            ],
            "edges": [edge("bob", "carol", "knows")],
        }),
        "MARKD1" => json!({
            "nodes": [
                node("alice", "Alice", "Person", "A researcher."),
                node("acme", "Acme", "Company", "A company."),
                node("dave", "Dave", "Person", "A colleague."),
            ],
            "edges": [
                edge("alice", "acme", "works_at"),
                edge("dave", "alice", "quotes"),
            ],
        }),
        other => panic!("no canned graph for marker {other}"),
    }
}

const MARKERS: [&str; 5] = ["MARKA1", "MARKA2", "MARKB1", "MARKC1", "MARKD1"];

/// Answers by the schema it is asked for and by the marker in the chunk text:
/// the marker's graph for extraction, a marker-specific summary for
/// summarization, and `custom` for a runtime graph schema.
struct MarkerLlm {
    custom: Option<Value>,
}

#[async_trait]
impl Llm for MarkerLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported(
            "MarkerLlm only serves structured output".to_string(),
        ))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let user: String = messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .map(|message| message.content.as_str())
            .collect();
        let marker = MARKERS
            .iter()
            .find(|marker| user.contains(*marker))
            .copied()
            .unwrap_or_else(|| panic!("no marker in the chunk: {user}"));
        let properties = json_schema.get("properties");
        if properties.and_then(|p| p.get("summary")).is_some() {
            return Ok(json!({
                "summary": format!("Summary of {marker}."),
                "description": format!("Description of {marker}."),
            }));
        }
        if properties.and_then(|p| p.get("nodes")).is_some() {
            return Ok(graph_for(marker));
        }
        self.custom
            .clone()
            .ok_or_else(|| LlmError::DeserializationError("no custom answer".to_string()))
    }

    fn model(&self) -> &str {
        "marker"
    }
}

/// The stores one scenario writes into, kept across its runs.
struct Stores {
    db: Arc<DatabaseConnection>,
    repo: Arc<dyn PipelineRunRepository>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
}

impl Stores {
    async fn new() -> Self {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("persist-equivalence".into(), OWNER_ID, None, DATASET_ID),
        )
        .await
        .expect("seed dataset");
        let db = Arc::new(conn);
        Self {
            repo: Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
            db,
            storage: Arc::new(MockStorage::new()),
            graph_db: Arc::new(MockGraphDB::new()),
            vector_db: Arc::new(MockVectorDB::new()),
        }
    }

    /// Store `text` as the data item `id`, persist its row and attach it.
    async fn add(&self, id: u128, name: &str, text: &str, url: Option<&str>) -> Data {
        let data_id = Uuid::from_u128(id);
        let location = self
            .storage
            .store(text.as_bytes(), &format!("{name}.txt"))
            .await
            .expect("store");
        let mut builder = Data::builder(
            data_id,
            name,
            location,
            name,
            "txt",
            "text/plain",
            format!("hash-{name}"),
            OWNER_ID,
        );
        if let Some(url) = url {
            builder =
                builder.external_metadata(json!({"_cognee": {"source_uri": url}}).to_string());
        }
        let item = builder.build();
        create_data(&self.db, item.clone()).await.expect("data row");
        attach_data_to_dataset(&self.db, DATASET_ID, data_id)
            .await
            .expect("attach");
        item
    }

    async fn cognify(
        &self,
        items: Vec<Data>,
        llm: Arc<dyn Llm>,
        ontology: Arc<dyn OntologyResolver>,
        config: &CognifyConfig,
    ) -> CognifyResult {
        cognify(
            items,
            DATASET_ID,
            Some(OWNER_ID),
            None,
            None,
            llm,
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>,
            Arc::clone(&self.vector_db) as Arc<dyn VectorDB>,
            Arc::new(MockEmbeddingEngine::new(8)),
            Arc::clone(&self.db) as Arc<dyn cognee_database::RelationalDb>,
            Arc::clone(&self.repo),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool")),
            ontology,
            config,
        )
        .await
        .expect("cognify completes")
    }

    /// Everything the scenario stored, normalised as the module doc says.
    async fn snapshot(&self, runs: &[Option<Uuid>]) -> Value {
        let labels: BTreeMap<String, String> = runs
            .iter()
            .enumerate()
            .filter_map(|(index, run)| run.map(|run| (index, run)))
            .flat_map(|(index, run)| {
                let label = format!("<run {}>", index + 1);
                [
                    (run.to_string(), label.clone()),
                    (run.simple().to_string(), label),
                ]
            })
            .collect();
        let normalise = |mut value: Value| {
            normalise(&mut value, &labels);
            sorted_keys(value)
        };

        let (nodes, edges) = self.graph_db.get_graph_data().await.expect("graph data");
        let graph_nodes: BTreeSet<String> = nodes
            .into_iter()
            .map(|(id, props)| {
                let props: serde_json::Map<String, Value> = props
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v))
                    .collect();
                normalise(json!({"id": id, "props": props})).to_string()
            })
            .collect();
        let graph_edges: BTreeSet<String> = edges
            .into_iter()
            .map(|(source, target, relationship, props)| {
                let props: serde_json::Map<String, Value> = props
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v))
                    .collect();
                normalise(json!([source, target, relationship, props])).to_string()
            })
            .collect();

        let vectors: BTreeMap<String, BTreeSet<String>> = self
            .vector_db
            .all_points()
            .into_iter()
            .map(|(collection, points)| {
                let points = points
                    .into_iter()
                    .map(|point| {
                        let metadata: serde_json::Map<String, Value> =
                            point.metadata.into_iter().collect();
                        normalise(json!({
                            "id": point.id,
                            "vector": point.vector,
                            "payload": metadata,
                        }))
                        .to_string()
                    })
                    .collect();
                (collection, points)
            })
            .collect();

        let rows = |table: &str, drop: &'static [&'static str]| {
            let db = Arc::clone(&self.db);
            let sql = format!("SELECT * FROM {table}");
            let normalise = &normalise;
            async move {
                let found = JsonValue::find_by_statement(Statement::from_string(
                    DatabaseBackend::Sqlite,
                    sql,
                ))
                .all(db.as_ref())
                .await
                .expect("read rows");
                found
                    .into_iter()
                    .map(|mut row| {
                        if let Some(object) = row.as_object_mut() {
                            for key in drop {
                                object.remove(*key);
                            }
                        }
                        normalise(row).to_string()
                    })
                    .collect::<BTreeSet<String>>()
            }
        };
        let ledger_nodes = rows("nodes", &[]).await;
        let ledger_edges = rows("edges", &[]).await;
        // The row id hashes the run id, which is random.
        let evidence = rows("provenance_edge_evidence", &["id"]).await;

        json!({
            "graph_nodes": graph_nodes,
            "graph_edges": graph_edges,
            "vectors": vectors,
            "ledger_nodes": ledger_nodes,
            "ledger_edges": ledger_edges,
            "edge_evidence": evidence,
        })
    }
}

/// `value` with every object's keys in sorted order, so two equal values print
/// the same: `serde_json` keeps insertion order in this workspace.
fn sorted_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let entries: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, entry)| (key, sorted_keys(entry)))
                .collect();
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted_keys).collect()),
        other => other,
    }
}

const VOLATILE_KEYS: [&str; 3] = ["created_at", "updated_at", "captured_at"];

/// Blank wall-clock values and replace run ids by the run's position, in
/// place, recursing into JSON carried as a string (ledger `attributes`).
fn normalise(value: &mut Value, run_labels: &BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if VOLATILE_KEYS.contains(&key.as_str()) && !entry.is_null() {
                    *entry = Value::String("<time>".to_string());
                } else {
                    normalise(entry, run_labels);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalise(item, run_labels);
            }
        }
        Value::String(text) => {
            if let Some(label) = run_labels.get(text.as_str()) {
                *value = Value::String(label.clone());
            } else if (text.starts_with('{') || text.starts_with('['))
                && let Ok(mut parsed) = serde_json::from_str::<Value>(text)
            {
                normalise(&mut parsed, run_labels);
                *value = json!({"<json>": parsed});
            }
        }
        _ => {}
    }
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/persist_equivalence")
        .join(format!("{name}.json"))
}

/// Compare `actual` with the committed snapshot `name`, set by set, or write
/// it when `COGNEE_UPDATE_PERSIST_SNAPSHOTS` is set.
fn assert_matches_snapshot(name: &str, actual: &Value) {
    let path = snapshot_path(name);
    if std::env::var("COGNEE_UPDATE_PERSIST_SNAPSHOTS").is_ok_and(|v| !v.is_empty()) {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        let pretty = serde_json::to_string_pretty(actual).expect("serialize snapshot");
        std::fs::write(&path, pretty + "\n").expect("write snapshot");
        return;
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read snapshot {}: {e}", path.display())),
    )
    .expect("parse snapshot");

    let as_set = |value: &Value| -> BTreeSet<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_string).unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut differences = Vec::new();
    for section in [
        "graph_nodes",
        "graph_edges",
        "ledger_nodes",
        "ledger_edges",
        "edge_evidence",
    ] {
        let want = as_set(&expected[section]);
        let got = as_set(&actual[section]);
        for missing in want.difference(&got) {
            differences.push(format!("{section}: missing {missing}"));
        }
        for extra in got.difference(&want) {
            differences.push(format!("{section}: unexpected {extra}"));
        }
    }
    let want_collections: BTreeSet<String> = expected["vectors"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    let got_collections: BTreeSet<String> = actual["vectors"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    for collection in want_collections.union(&got_collections) {
        let want = as_set(&expected["vectors"][collection]);
        let got = as_set(&actual["vectors"][collection]);
        for missing in want.difference(&got) {
            differences.push(format!("vectors {collection}: missing {missing}"));
        }
        for extra in got.difference(&want) {
            differences.push(format!("vectors {collection}: unexpected {extra}"));
        }
    }
    assert!(
        differences.is_empty(),
        "the stored sets differ from the snapshot {} ({} differences):\n{}",
        path.display(),
        differences.len(),
        differences.join("\n")
    );
}

fn ontology() -> Arc<dyn OntologyResolver> {
    let reader: Box<dyn std::io::Read> = Box::new(ONTOLOGY.as_bytes());
    Arc::new(
        RdfLibOntologyResolver::new(OntologyFileInput::Reader(reader)).expect("ontology loads"),
    )
}

/// The chunker's settings, pinned so the chunk boundaries — and with them
/// every chunk id, `chunk_size` and `contains` in the snapshots — do not depend
/// on the build. The default counter is TikToken, which degrades to word
/// counting unless `cognee-chunking/tiktoken` is compiled in, and that depends
/// on feature unification: `cargo test -p cognee-cognify` leaves it out, a
/// workspace build (CI) brings it in through `cognee`'s defaults.
fn pinned_chunking(config: CognifyConfig) -> CognifyConfig {
    config
        .with_chunk_size(CHUNK_SIZE)
        .with_token_counter(TokenCounterKind::Word)
        .with_token_count_mode(TokenCountMode::Span)
}

fn default_config() -> CognifyConfig {
    pinned_chunking(CognifyConfig::default()).with_chunks_per_batch(1)
}

/// The default path: several documents and blocks, an entity with several
/// types, timestamps, an ontology, a URL document, summaries, edge evidence —
/// then a second run whose document produces an edge the first run already
/// wrote, which it claims.
#[tokio::test]
async fn the_default_path_stores_the_pinned_sets() {
    let stores = Stores::new().await;
    let a = stores.add(0xa, "doc-a", DOC_A, None).await;
    let b = stores.add(0xb, "doc-b", DOC_B, None).await;
    let c = stores
        .add(0xc, "doc-c", DOC_C, Some("https://example.com/talks/bob"))
        .await;
    let llm: Arc<dyn Llm> = Arc::new(MarkerLlm { custom: None });

    let first = stores
        .cognify(
            vec![a, b, c],
            Arc::clone(&llm),
            ontology(),
            &default_config(),
        )
        .await;
    let chunks_of_a = first
        .chunks
        .iter()
        .filter(|chunk| chunk.document_id == Uuid::from_u128(0xa))
        .count();
    assert!(chunks_of_a >= 2, "doc A must span several blocks");

    let d = stores.add(0xd, "doc-d", DOC_D, None).await;
    let second = stores
        .cognify(vec![d], llm, ontology(), &default_config())
        .await;

    let snapshot = stores
        .snapshot(&[first.pipeline_run_id, second.pipeline_run_id])
        .await;
    assert_matches_snapshot("default_path", &snapshot);
}

/// A `Company` root with `Person` employees, for the runtime-schema path.
fn company_schema() -> Value {
    let person = json!({
        "title": "Person",
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "metadata": {"type": "object", "default": {
                "index_fields": ["name"], "identity_fields": ["name"]
            }}
        },
        "required": ["name"]
    });
    json!({
        "$defs": {"Person": person},
        "title": "Company",
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "employees": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
            "metadata": {"type": "object", "default": {
                "index_fields": ["name"], "identity_fields": ["name"]
            }}
        },
        "required": ["name", "employees"]
    })
}

/// The runtime-schema path: two documents, one of them over two blocks, whose
/// custom graphs share nodes, plus a URL document.
#[tokio::test]
async fn the_custom_schema_path_stores_the_pinned_sets() {
    let stores = Stores::new().await;
    let a = stores.add(0xa, "doc-a", DOC_A, None).await;
    let c = stores
        .add(0xc, "doc-c", DOC_C, Some("https://example.com/talks/bob"))
        .await;
    let llm: Arc<dyn Llm> = Arc::new(MarkerLlm {
        custom: Some(json!({"name": "Acme", "employees": [{"name": "Alice"}, {"name": "Bob"}]})),
    });
    let config = default_config().with_graph_schema(company_schema());

    let run = stores
        .cognify(
            vec![a, c],
            llm,
            Arc::new(NoOpOntologyResolver::new()),
            &config,
        )
        .await;

    let snapshot = stores.snapshot(&[run.pipeline_run_id]).await;
    assert_matches_snapshot("custom_schema_path", &snapshot);
}

/// One DLT row's `external_metadata`.
fn dlt_metadata(table: &str, key: &str, extra: Value) -> String {
    let mut metadata = json!({
        "source": "dlt",
        "table_name": table,
        "primary_key_value": key,
        "dlt_db_name": "shop",
        "schema_info": [{"name": "id"}],
        "foreign_keys": [],
        "column_values": {},
    });
    for (key, value) in extra.as_object().expect("an object") {
        metadata[key] = value.clone();
    }
    metadata.to_string()
}

/// The LLM-free graph-backend path, with DLT rows: a prose document, and two
/// rows linked by a foreign key whose schema nodes, column values and FK edges
/// the post-run DLT step writes.
#[tokio::test]
async fn the_backend_and_dlt_path_stores_the_pinned_sets() {
    let stores = Stores::new().await;
    let prose = stores
        .add(
            0x1,
            "prose",
            "Quarterly planning notes about the garden project.",
            None,
        )
        .await;
    let customer = Uuid::from_u128(0x2);
    let rows = [
        (
            0x2,
            "customers",
            "Table: customers\nRow Data:\n  id: 7\n  name: Acme",
            dlt_metadata("customers", "7", json!({"column_values": {"name": "Acme"}})),
        ),
        (
            0x3,
            "orders",
            "Table: orders\nRow Data:\n  id: 1\n  status: shipped",
            dlt_metadata(
                "orders",
                "1",
                json!({
                    "foreign_keys": [
                        {"column": "customer_id", "ref_table": "customers", "ref_column": "id"}
                    ],
                    "fk_references": [{
                        "column": "customer_id",
                        "target_table": "customers",
                        "target_data_id": customer.to_string(),
                        "relationship_name": "customer_id_references_customers",
                    }],
                    "column_values": {"status": "shipped"},
                }),
            ),
        ),
    ];
    let mut items = vec![prose];
    for (id, name, text, metadata) in rows {
        let data_id = Uuid::from_u128(id);
        let location = stores
            .storage
            .store(text.as_bytes(), &format!("{name}.txt"))
            .await
            .expect("store");
        let item = Data::builder(
            data_id,
            name,
            location,
            name,
            "txt",
            "text/plain",
            format!("hash-{name}"),
            OWNER_ID,
        )
        .external_metadata(metadata)
        .build();
        create_data(&stores.db, item.clone())
            .await
            .expect("data row");
        attach_data_to_dataset(&stores.db, DATASET_ID, data_id)
            .await
            .expect("attach");
        items.push(item);
    }
    let config = pinned_chunking(CognifyConfig::default())
        .with_summarization(false)
        .with_graph_backend(Arc::new(
            cognee_cognify::graph_backend::MockChunkGraphExtractor::new(),
        ));

    let run = stores
        .cognify(
            items,
            Arc::new(MarkerLlm { custom: None }),
            Arc::new(NoOpOntologyResolver::new()),
            &config,
        )
        .await;

    let snapshot = stores.snapshot(&[run.pipeline_run_id]).await;
    assert_matches_snapshot("backend_and_dlt_path", &snapshot);
}
