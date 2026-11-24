//! Shared scenario of the nodes-before-edges regression tests: a cognify run
//! whose every ledger edge must exist in a real graph backend.
//!
//! A URL document (its chunks are `SOURCED_FROM` its WebPage) whose entities
//! resolve against an ontology with a class hierarchy (the type→type `is_a`
//! edges `Algorithm → Technology` and `Organisation → LegalEntity`). Before the
//! persistence layer wrote every node first, those edges went to the graph
//! before their endpoints: Ladybug dropped them silently and Postgres failed
//! the run on `graph_edge_source_id_fkey`.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_chunking::{TokenCountMode, TokenCounterKind};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::graph_storage::get_edges_by_dataset;
use cognee_database::{SeaOrmPipelineRunRepository, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, LlmError, LlmResult};
use cognee_models::{Data, Dataset};
use cognee_ontology::{OntologyFileInput, RdfLibOntologyResolver};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use serde_json::{Value, json};
use uuid::Uuid;

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

const TEXT: &str = "TechCorp ranks its search results with DeepSort.";
const URL: &str = "https://example.com/techcorp/deepsort";

/// Answers extraction with the TechCorp / DeepSort graph and summarization
/// with a fixed summary.
struct CannedLlm;

#[async_trait]
impl Llm for CannedLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported("structured only".to_string()))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let properties = json_schema.get("properties");
        if properties.and_then(|p| p.get("summary")).is_some() {
            return Ok(json!({"summary": "TechCorp uses DeepSort.", "description": "d"}));
        }
        Ok(json!({
            "nodes": [
                {"id": "techcorp", "name": "TechCorp", "type": "Organisation",
                 "description": "A company."},
                {"id": "deepsort", "name": "DeepSort", "type": "Algorithm",
                 "description": "A ranking algorithm."},
            ],
            "edges": [
                {"source_node_id": "techcorp", "target_node_id": "deepsort",
                 "relationship_name": "uses"},
            ],
        }))
    }

    fn model(&self) -> &str {
        "canned"
    }
}

/// Cognify the URL document into `graph_db`, then assert every edge the
/// ledger names is in the graph — the `SOURCED_FROM` edge and the ontology's
/// type→type `is_a` edges above all.
pub async fn every_ledger_edge_reaches_the_graph(graph_db: Arc<dyn GraphDBTrait>) {
    let dataset_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let conn = connect("sqlite::memory:").await.expect("connect sqlite");
    initialize(&conn).await.expect("initialize");
    create_dataset(
        &conn,
        Dataset::new("nodes-before-edges".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("seed dataset");
    let db = Arc::new(conn);

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let data_id = Uuid::new_v4();
    let location = storage
        .store(TEXT.as_bytes(), "techcorp.txt")
        .await
        .expect("store");
    let item = Data::builder(
        data_id,
        "techcorp.txt",
        location,
        "techcorp.txt",
        "txt",
        "text/plain",
        format!("hash-{data_id}"),
        owner_id,
    )
    .external_metadata(json!({"_cognee": {"source_uri": URL}}).to_string())
    .build();
    create_data(&db, item.clone()).await.expect("data row");
    attach_data_to_dataset(&db, dataset_id, data_id)
        .await
        .expect("attach");

    let reader: Box<dyn std::io::Read> = Box::new(ONTOLOGY.as_bytes());
    let ontology =
        RdfLibOntologyResolver::new(OntologyFileInput::Reader(reader)).expect("ontology loads");

    cognify(
        vec![item],
        dataset_id,
        Some(owner_id),
        None,
        None,
        Arc::new(CannedLlm),
        storage,
        Arc::clone(&graph_db),
        Arc::new(MockVectorDB::new()),
        Arc::new(MockEmbeddingEngine::new(8)),
        Arc::clone(&db) as Arc<dyn cognee_database::RelationalDb>,
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool")),
        Arc::new(ontology),
        // Pinned, so the chunking does not depend on whether the build has
        // `cognee-chunking/tiktoken` (see `persist_equivalence.rs`).
        &CognifyConfig::default()
            .with_chunk_size(1500)
            .with_token_counter(TokenCounterKind::Word)
            .with_token_count_mode(TokenCountMode::Span),
    )
    .await
    .expect("cognify must store a URL document resolved against an ontology");

    let (nodes, edges) = graph_db.get_graph_data().await.expect("graph data");
    let stored: BTreeSet<(String, String)> = edges
        .iter()
        .map(|(source, target, _, _)| (source.clone(), target.clone()))
        .collect();
    let ledger = get_edges_by_dataset(&db, dataset_id)
        .await
        .expect("ledger edges");
    assert!(!ledger.is_empty());
    let missing: Vec<String> = ledger
        .iter()
        .filter(|row| {
            !stored.contains(&(
                row.source_node_id.to_string(),
                row.destination_node_id.to_string(),
            ))
        })
        .map(|row| {
            format!(
                "{} -{}-> {}",
                row.source_node_id, row.relationship_name, row.destination_node_id
            )
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{} of {} ledger edges are not in the graph:\n{}",
        missing.len(),
        ledger.len(),
        missing.join("\n")
    );

    assert!(
        edges.iter().any(|(_, _, rel, _)| rel == "SOURCED_FROM"),
        "the chunk is SOURCED_FROM its WebPage"
    );
    let entity_types: BTreeSet<&str> = nodes
        .iter()
        .filter(|(_, props)| props.get("type").and_then(Value::as_str) == Some("EntityType"))
        .map(|(id, _)| id.as_str())
        .collect();
    let type_to_type = edges
        .iter()
        .filter(|(source, target, rel, _)| {
            rel == "is_a"
                && entity_types.contains(source.as_str())
                && entity_types.contains(target.as_str())
        })
        .count();
    assert!(
        type_to_type >= 2,
        "the ontology's type→type is_a edges are stored (found {type_to_type})"
    );
}
