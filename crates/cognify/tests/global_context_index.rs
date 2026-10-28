#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The global context index against real stores: the Python-generated
//! `from_entities` fixture, the change-only write path, and the LLM method
//! with a scripted LLM (ports of Python's `test_build_document_index.py` write
//! test and the `update_global_context_index` cases of
//! `test_global_context_index.py`).
#![cfg(feature = "testing")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_cognify::global_context_index::content::dataset_content_from_rows;
use cognee_cognify::global_context_index::{
    GlobalContextIndexMethod, IndexStore, LlmIndexOptions, build_context_summaries,
    build_global_context_index, create_root_summary_id,
};
use cognee_database::ops::{data as data_ops, datasets as ds_ops};
use cognee_database::{DatabaseConnection, DeleteDb, connect, initialize};
use cognee_embedding::{EmbeddingEngine, EmbeddingResult};
use cognee_graph::{EdgeData, GraphDBTrait, MockGraphDB};
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message};
use cognee_models::{Data, Dataset, GlobalContextSummary};
use cognee_vector::{BruteForceVectorDB, VectorDB, VectorPoint};
use serde_json::{Value, json};
use uuid::Uuid;

// ── Stores ────────────────────────────────────────────────────────────────

/// Embeds a text by the topic words it contains, so placement is predictable.
struct TopicEmbedding;

const TOPICS: [&str; 6] = ["cats", "cars", "rain", "jazz", "chess", "bread"];

#[async_trait]
impl EmbeddingEngine for TopicEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v: Vec<f32> = TOPICS
                    .iter()
                    .map(|t| text.matches(t).count() as f32)
                    .collect();
                if v.iter().all(|x| *x == 0.0) {
                    v = vec![1.0; TOPICS.len()];
                }
                v
            })
            .collect())
    }
    fn dimension(&self) -> usize {
        TOPICS.len()
    }
    fn batch_size(&self) -> usize {
        8
    }
    fn max_sequence_length(&self) -> usize {
        512
    }
}

/// Answers every summary prompt with `kind[child texts joined by |]` and
/// records the calls.
#[derive(Default)]
struct ScriptedLlm {
    calls: Mutex<Vec<(String, String)>>,
}

impl ScriptedLlm {
    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        unreachable!("the index uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let system = &messages[0].content;
        let input = &messages[1].content;
        let kind = if system.starts_with("Compress the input summaries") {
            "bucket"
        } else {
            "root"
        };
        self.calls
            .lock()
            .unwrap()
            .push((kind.to_string(), input.clone()));
        let children: Vec<&str> = input
            .split("\n\n")
            .filter_map(|block| block.split_once('\n').map(|(_, text)| text))
            .collect();
        Ok(json!({"summary": format!("{kind}[{}]", children.join("|"))}))
    }

    fn model(&self) -> &str {
        "scripted"
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    db: DatabaseConnection,
    graph: MockGraphDB,
    vector: BruteForceVectorDB,
    embedding: TopicEmbedding,
    dataset_id: Uuid,
    owner: Uuid,
}

impl Harness {
    async fn new() -> Self {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("cognee.db");
        std::fs::File::create(&path).unwrap();
        let db = connect(&format!("sqlite://{}", path.display()))
            .await
            .unwrap();
        initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        ds_ops::create_dataset(&db, Dataset::new("ds".into(), owner, None, dataset_id))
            .await
            .unwrap();
        Self {
            _temp: temp,
            db,
            graph: MockGraphDB::new(),
            vector: BruteForceVectorDB::new(),
            embedding: TopicEmbedding,
            dataset_id,
            owner,
        }
    }

    fn store(&self) -> IndexStore<'_> {
        IndexStore {
            graph_db: &self.graph,
            vector_db: &self.vector,
            embedding_engine: &self.embedding,
            db: &self.db,
            dataset_id: self.dataset_id,
            owner_id: self.owner,
            tenant_id: None,
        }
    }

    async fn add_document(&self, name: &str) -> String {
        let id = Uuid::new_v4();
        let data = Data::builder(
            id,
            name,
            format!("file:///tmp/{name}"),
            name,
            "txt",
            "text/plain",
            id.simple().to_string(),
            self.owner,
        )
        .build();
        data_ops::create_data(&self.db, data).await.unwrap();
        ds_ops::attach_data_to_dataset(&self.db, self.dataset_id, id)
            .await
            .unwrap();
        id.to_string()
    }

    async fn node(&self, value: Value) {
        self.graph.add_node_raw(value).await.unwrap();
    }

    async fn edge(&self, source: &str, target: &str, relationship: &str, props: Value) {
        let props = props
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone().into(), v.clone()))
            .collect();
        self.graph
            .add_edge(source, target, relationship, Some(props))
            .await
            .unwrap();
    }

    async fn chunk_with_entities(&self, chunk: &str, document: &str, entities: &[(&str, &str)]) {
        self.node(json!({"id": chunk, "type": "DocumentChunk", "document_id": document}))
            .await;
        for (entity, type_name) in entities {
            self.node(json!({"id": entity, "type": "Entity", "name": entity}))
                .await;
            let type_id = format!("type-{type_name}");
            self.node(json!({"id": type_id, "type": "EntityType", "name": type_name}))
                .await;
            self.edge(chunk, entity, "contains", json!({})).await;
            self.edge(entity, &type_id, "is_a", json!({})).await;
        }
    }

    async fn index_nodes(&self) -> Vec<(String, HashMap<String, Value>)> {
        let (nodes, _) = self.graph.get_graph_data().await.unwrap();
        nodes
            .into_iter()
            .filter(|(_, p)| p.get("type") == Some(&json!(GlobalContextSummary::TYPE)))
            .map(|(id, p)| {
                (
                    id,
                    p.into_iter().map(|(k, v)| (k.into_owned(), v)).collect(),
                )
            })
            .collect()
    }

    async fn edges(&self, relationship: &str) -> Vec<(String, String)> {
        let (_, edges) = self.graph.get_graph_data().await.unwrap();
        let mut pairs: Vec<(String, String)> = edges
            .into_iter()
            .filter(|(_, _, rel, _): &EdgeData| rel == relationship)
            .map(|(s, t, _, _)| (s, t))
            .collect();
        pairs.sort();
        pairs
    }

    /// A text summary in the graph and in `TextSummary_text`.
    async fn text_summary(&self, text: &str) -> String {
        let id = Uuid::new_v4();
        self.node(json!({"id": id.to_string(), "type": "TextSummary", "text": text}))
            .await;
        if !self
            .vector
            .has_collection("TextSummary", "text")
            .await
            .unwrap()
        {
            self.vector
                .create_collection("TextSummary", "text", TOPICS.len())
                .await
                .unwrap();
        }
        let vector = self.embedding.embed(&[text]).await.unwrap().remove(0);
        self.vector
            .index_points("TextSummary", "text", &[VectorPoint::new(id, vector)])
            .await
            .unwrap();
        id.to_string()
    }
}

fn llm_method(options: LlmIndexOptions) -> GlobalContextIndexMethod {
    GlobalContextIndexMethod::Llm(options)
}

fn no_llm() -> Arc<dyn Llm> {
    Arc::new(ScriptedLlm::default())
}

// ── from_entities ─────────────────────────────────────────────────────────

/// The fixture is the output of Python's own `_dataset_content_from_rows` and
/// `build_context_summaries` (origin/dev) on a synthetic graph; see
/// `fixtures/global_context_index/gen_from_entities.py`. Ids and texts must
/// match byte for byte.
#[test]
fn from_entities_matches_the_python_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/global_context_index/from_entities.json"
    ))
    .unwrap();
    let to_props = |v: &Value| {
        v.as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone().into(), v.clone()))
            .collect()
    };
    let nodes: Vec<_> = fixture["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n[0].as_str().unwrap().to_string(), to_props(&n[1])))
        .collect();
    let edges: Vec<EdgeData> = fixture["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e[0].as_str().unwrap().to_string(),
                e[1].as_str().unwrap().to_string(),
                e[2].as_str().unwrap().to_string(),
                to_props(&e[3]),
            )
        })
        .collect();
    let documents: HashMap<String, Option<String>> = fixture["documents"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, name)| {
            let name = name
                .as_str()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (id.clone(), (!name.is_empty()).then_some(name))
        })
        .collect();

    let content = dataset_content_from_rows(
        &nodes,
        &edges,
        fixture["dataset_id"].as_str().unwrap(),
        &documents,
    );
    let actual: Vec<Value> = build_context_summaries(&content)
        .into_iter()
        .map(|s| {
            json!({"id": s.base.id.to_string(), "text": s.text, "level": s.level,
                   "is_root": s.is_root, "document_id": s.document_id})
        })
        .collect();
    assert_eq!(Value::Array(actual), fixture["expected"]);
}

#[tokio::test]
async fn from_entities_writes_only_changed_nodes_and_deletes_the_rest() {
    let h = Harness::new().await;
    let d1 = h.add_document("alpha.md").await;
    let d2 = h.add_document("beta.md").await;
    h.chunk_with_entities("c1", &d1, &[("Alice", "Person"), ("Acme", "Organization")])
        .await;
    h.chunk_with_entities("c2", &d2, &[("Bob", "Person")]).await;
    h.edge(
        "Alice",
        "Acme",
        "works_at",
        json!({"edge_text": "Alice works at Acme"}),
    )
    .await;
    // An index node a previous run left for a document that is gone.
    let stale = Uuid::new_v4().to_string();
    h.node(json!({"id": stale, "type": "GlobalContextSummary", "dataset_id": h.dataset_id.to_string(), "text": "orphan", "level": 0}))
        .await;

    let method = GlobalContextIndexMethod::default();
    let written = build_global_context_index(&h.store(), &no_llm(), &method)
        .await
        .unwrap();

    let texts: Vec<&str> = written.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "alpha.md\nOrganization: Acme\nPerson: Alice\nAlice works at Acme",
            "beta.md\nPerson: Bob",
            "Topics: Person (2), Organization (1)\nEntities: Acme, Alice, Bob\nDocuments: alpha.md, beta.md",
        ]
    );
    let index = h.index_nodes().await;
    assert_eq!(index.len(), 3, "the stale node is deleted");
    assert!(index.iter().all(|(id, _)| *id != stale));

    let root = create_root_summary_id(&h.dataset_id.to_string()).to_string();
    let bucket = |doc: &str| {
        written
            .iter()
            .find(|s| s.document_id.as_deref() == Some(doc))
            .unwrap()
            .base
            .id
            .to_string()
    };
    let mut expected_in = vec![(bucket(&d1), root.clone()), (bucket(&d2), root.clone())];
    expected_in.sort();
    assert_eq!(h.edges("summarized_in").await, expected_in);
    let mut expected_doc = vec![(bucket(&d1), d1.clone()), (bucket(&d2), d2.clone())];
    expected_doc.sort();
    assert_eq!(h.edges("summarizes").await, expected_doc);

    // Embedded with the full payload the read side keys on.
    assert_eq!(
        h.vector
            .collection_size(GlobalContextSummary::TYPE, "text")
            .await
            .unwrap(),
        3
    );
    let root_point = h
        .vector
        .retrieve(
            GlobalContextSummary::TYPE,
            "text",
            &[Uuid::parse_str(&root).unwrap()],
        )
        .await
        .unwrap();
    assert_eq!(root_point[0].metadata["is_root"], json!(true));
    assert_eq!(root_point[0].metadata["text"], json!(texts[2]));

    // Ownership rows name the dataset, so deleting it removes the index.
    let ledger = h.db.get_nodes_by_dataset(h.dataset_id).await.unwrap();
    assert_eq!(
        ledger
            .iter()
            .filter(|n| n.node_type == GlobalContextSummary::TYPE)
            .count(),
        3
    );

    // A re-run over unchanged data writes nothing.
    let rerun = build_global_context_index(&h.store(), &no_llm(), &method)
        .await
        .unwrap();
    assert!(rerun.is_empty());

    // A write to beta.md rewrites its bucket and the root, nothing else.
    h.chunk_with_entities("c3", &d2, &[("Cara", "Person")])
        .await;
    let after_write = build_global_context_index(&h.store(), &no_llm(), &method)
        .await
        .unwrap();
    let ids: Vec<String> = after_write.iter().map(|s| s.base.id.to_string()).collect();
    assert_eq!(ids, [bucket(&d2), root.clone()]);
}

#[tokio::test]
async fn from_entities_without_link_documents_writes_no_summarizes_edges() {
    let h = Harness::new().await;
    let d1 = h.add_document("alpha.md").await;
    h.chunk_with_entities("c1", &d1, &[("Alice", "Person")])
        .await;

    let method = GlobalContextIndexMethod::FromEntities {
        link_documents: false,
    };
    build_global_context_index(&h.store(), &no_llm(), &method)
        .await
        .unwrap();

    assert_eq!(h.edges("summarized_in").await.len(), 1);
    assert!(h.edges("summarizes").await.is_empty());
}

#[tokio::test]
async fn from_entities_ignores_chunks_of_documents_outside_the_dataset() {
    let h = Harness::new().await;
    h.chunk_with_entities("c1", "not-in-dataset", &[("Alice", "Person")])
        .await;

    let written = build_global_context_index(&h.store(), &no_llm(), &Default::default())
        .await
        .unwrap();

    assert!(written.is_empty());
    assert!(h.index_nodes().await.is_empty());
}

// ── LLM method ────────────────────────────────────────────────────────────

#[tokio::test]
async fn llm_first_build_creates_one_bucket_per_distant_summary_and_a_root() {
    let h = Harness::new().await;
    let cats = h.text_summary("cats purr").await;
    let cars = h.text_summary("cars roar").await;
    let rain = h.text_summary("rain falls").await;
    let llm = Arc::new(ScriptedLlm::default());
    let llm_dyn: Arc<dyn Llm> = llm.clone();

    let written = build_global_context_index(&h.store(), &llm_dyn, &llm_method(Default::default()))
        .await
        .unwrap();

    let buckets: Vec<&GlobalContextSummary> = written.iter().filter(|s| !s.is_root).collect();
    assert_eq!(buckets.len(), 3);
    assert!(buckets.iter().all(|b| b.level == 0));
    let root = written.last().unwrap();
    assert!(root.is_root);
    assert_eq!(root.level, 1);
    assert_eq!(
        root.base.id,
        create_root_summary_id(&h.dataset_id.to_string())
    );
    assert_eq!(
        llm.calls()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        ["bucket", "bucket", "bucket", "root"]
    );
    // Each bucket summarizes its one child, verbatim from the prompt input.
    assert!(
        llm.calls()
            .iter()
            .any(|(_, input)| input == "Input 1:\ncats purr")
    );

    let in_edges = h.edges("summarized_in").await;
    assert_eq!(
        in_edges.len(),
        6,
        "three summary→bucket and three bucket→root"
    );
    for summary in [&cats, &cars, &rain] {
        assert!(in_edges.iter().any(|(s, _)| s == summary));
    }
    assert_eq!(
        in_edges
            .iter()
            .filter(|(_, t)| *t == root.base.id.to_string())
            .count(),
        3
    );
}

#[tokio::test]
async fn llm_incremental_update_only_regenerates_the_touched_bucket_and_the_root() {
    let h = Harness::new().await;
    let cats = h.text_summary("cats purr").await;
    h.text_summary("cars roar").await;
    build_global_context_index(&h.store(), &no_llm(), &llm_method(Default::default()))
        .await
        .unwrap();

    let new = h.text_summary("cats nap").await;
    let llm = Arc::new(ScriptedLlm::default());
    let llm_dyn: Arc<dyn Llm> = llm.clone();
    let written = build_global_context_index(&h.store(), &llm_dyn, &llm_method(Default::default()))
        .await
        .unwrap();

    assert_eq!(written.len(), 2, "the cats bucket and the root");
    // Children are summarized in id order.
    let mut children = [(cats, "cats purr"), (new, "cats nap")];
    children.sort();
    assert_eq!(
        written[0].text,
        format!("bucket[{}|{}]", children[0].1, children[1].1)
    );
    assert!(written[1].is_root);
    assert_eq!(
        llm.calls()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        ["bucket", "root"]
    );
    // A second run with nothing new places nothing.
    let again = build_global_context_index(&h.store(), &no_llm(), &llm_method(Default::default()))
        .await
        .unwrap();
    assert!(again.is_empty());
}

#[tokio::test]
async fn llm_builds_upper_levels_when_a_level_overflows() {
    let h = Harness::new().await;
    for topic in TOPICS.iter().take(5) {
        h.text_summary(&format!("{topic} story")).await;
    }
    let options = LlmIndexOptions {
        max_bucket_size: 2,
        ..Default::default()
    };

    let written = build_global_context_index(&h.store(), &no_llm(), &llm_method(options))
        .await
        .unwrap();

    let count_at = |level: i64| {
        written
            .iter()
            .filter(|s| !s.is_root && s.level == level)
            .count()
    };
    assert_eq!(count_at(0), 5);
    assert_eq!(count_at(1), 3);
    assert_eq!(count_at(2), 2);
    let root = written.last().unwrap();
    assert!(root.is_root);
    assert_eq!(root.level, 3);
}

#[tokio::test]
async fn llm_rebuild_with_no_summaries_deletes_the_old_index() {
    let h = Harness::new().await;
    let summary = h.text_summary("cats purr").await;
    build_global_context_index(&h.store(), &no_llm(), &llm_method(Default::default()))
        .await
        .unwrap();
    assert_eq!(h.index_nodes().await.len(), 2);

    h.graph.delete_nodes(&[summary]).await.unwrap();
    let options = LlmIndexOptions {
        rebuild: true,
        ..Default::default()
    };
    let written = build_global_context_index(&h.store(), &no_llm(), &llm_method(options))
        .await
        .unwrap();

    assert!(written.is_empty());
    assert!(h.index_nodes().await.is_empty());
    assert_eq!(
        h.vector
            .collection_size(GlobalContextSummary::TYPE, "text")
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn llm_rejects_invalid_options_before_any_io() {
    let h = Harness::new().await;
    let options = LlmIndexOptions {
        max_bucket_size: 1,
        ..Default::default()
    };
    let err = build_global_context_index(&h.store(), &no_llm(), &llm_method(options))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("max_bucket_size"));
    assert!(h.graph.get_call_log().is_empty());
}
