#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Ticket test 9 of SDK-1694 (stage O6): deleting a document removes the
//! entity types and `is_a` edges only it declared, through the real cognify
//! path and the real delete service.
//!
//! Python removes an EntityType with its last owning document
//! (`get_data_related_nodes`: a node row no other document's row names).
//! Since O6 Rust writes one ledger row per declaring document and no
//! nil-`data_id` row beside them, so the same rule applies; an `is_a` edge is
//! counted by its triple, so the edge goes with the last document that
//! declared that pair even when the entity and the type both stay.
//!
//! A database written before O6 holds nil rows instead. They are still
//! claims: its types stay until their dataset is deleted, as before.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;
use std::sync::Arc;

use cognee_database::ops::graph_storage::{upsert_edges, upsert_nodes};
use cognee_database::{DeleteDb, GraphEdge, GraphNode};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_graph::GraphDBTrait;
use cognee_llm::{LlmResult, Message, MessageRole};
use cognee_models::{EdgeType, Entity, EntityType};
use cognee_vector::VectorDB;
use rollback_harness::{Harness, extraction_config};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};
use uuid::Uuid;

/// Answers each chunk from its text: one node per `Name:Type` token (type
/// `Untyped` is the empty type), and one edge per `Source~relationship~Target`
/// token, so an entity's only edges are its chunk's `contains`, its `is_a`
/// edges and the edges named that way.
struct TypesLlm;

fn graph_from(text: &str) -> Value {
    let edges: Vec<Value> = text
        .split_whitespace()
        .filter_map(|token| {
            let mut parts = token.split('~');
            let (source, relationship, target) = (parts.next()?, parts.next()?, parts.next()?);
            Some(json!({"source_node_id": source.to_lowercase(),
                        "target_node_id": target.to_lowercase(),
                        "relationship_name": relationship}))
        })
        .collect();
    let nodes: Vec<Value> = text
        .split_whitespace()
        .filter_map(|token| token.split_once(':'))
        .filter(|(name, kind)| {
            [name, kind].iter().all(|part| {
                part.chars().next().is_some_and(char::is_uppercase)
                    && part.chars().all(char::is_alphabetic)
            })
        })
        .map(|(name, kind)| {
            let kind = if kind == "Untyped" { "" } else { kind };
            json!({"id": name.to_lowercase(), "name": name, "type": kind,
                   "description": format!("{name} as a {kind}.")})
        })
        .collect();
    json!({"nodes": nodes, "edges": edges})
}

#[async_trait::async_trait]
impl cognee_llm::Llm for TypesLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> LlmResult<cognee_llm::GenerationResponse> {
        unreachable!("the cognify pipeline only uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> LlmResult<Value> {
        let text: String = messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(graph_from(&text))
    }

    fn model(&self) -> &str {
        "mock-types"
    }
}

/// Two documents, cognified in one run.
async fn cognified(first: &str, second: &str) -> (Harness, Uuid, Uuid) {
    let mut harness = Harness::new().await;
    let a = harness.add_file(first).await;
    let b = harness.add_file(second).await;
    harness
        .run_over(&extraction_config(), &[a, b], Arc::new(TypesLlm))
        .await
        .expect("cognify succeeds");
    (harness, a, b)
}

async fn delete(harness: &Harness, data_id: Uuid, mode: DeleteMode) {
    DeleteService::new(
        Arc::clone(&harness.storage),
        harness.db.clone() as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>)
    .execute(&DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id: harness.dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        mode,
        memory_only: false,
    })
    .await
    .expect("delete succeeds");
}

fn type_id(name: &str) -> Uuid {
    EntityType::id_for(name)
}

/// The graph's `is_a` edges, by entity and type name.
async fn is_a_edges(harness: &Harness) -> BTreeSet<(Uuid, Uuid)> {
    let (_, edges) = harness.graph_db.get_graph_data().await.unwrap();
    edges
        .into_iter()
        .filter(|(_, _, relationship, _)| relationship == "is_a")
        .map(|(source, target, _, _)| {
            (
                Uuid::parse_str(&source).unwrap(),
                Uuid::parse_str(&target).unwrap(),
            )
        })
        .collect()
}

async fn has_node(harness: &Harness, id: Uuid) -> bool {
    harness.graph_db.has_node(&id.to_string()).await.unwrap()
}

fn has_point(harness: &Harness, data_type: &str, field: &str, id: Uuid) -> bool {
    harness
        .vector_db
        .get_payload(data_type, field, id)
        .is_some()
}

fn has_edge_type_point(harness: &Harness, text: &str) -> bool {
    has_point(
        harness,
        "EdgeType",
        "relationship_name",
        EdgeType::deterministic_id(text),
    )
}

/// The ledger rows naming the node `slug`, by `data_id`.
async fn node_owners(harness: &Harness, slug: Uuid) -> BTreeSet<Uuid> {
    harness
        .ledger_nodes()
        .await
        .into_iter()
        .filter(|row| row.slug == slug)
        .map(|row| row.data_id)
        .collect()
}

/// The ledger rows of the edge `source -is_a-> target`, by `data_id`.
async fn is_a_owners(harness: &Harness, source: Uuid, target: Uuid) -> BTreeSet<Uuid> {
    harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| {
            row.source_node_id == source
                && row.destination_node_id == target
                && row.relationship_name == "is_a"
        })
        .map(|row| row.data_id)
        .collect()
}

/// Ticket test 9: doc A types Apple as a company, doc B as a fruit (ticket
/// test 1, one chunk each). Deleting B keeps Apple and its company type and
/// removes the fruit type — its node, its vector point, its rows — and the
/// `is_a` edge to it with its `EdgeType` point, soft or hard. In hard mode the
/// degree-one sweep keeps `company`, which doc A still declares although only
/// Apple's `is_a` edge reaches it.
#[tokio::test]
async fn deleting_a_document_removes_the_type_only_it_declared() {
    for mode in [DeleteMode::Soft, DeleteMode::Hard] {
        let (harness, a, b) = cognified("Apple:Company", "Apple:Fruit").await;
        let apple = Entity::id_for("Apple");
        let (company, fruit) = (type_id("company"), type_id("fruit"));
        assert_eq!(
            is_a_edges(&harness).await,
            BTreeSet::from([(apple, company), (apple, fruit)]),
            "{mode:?}: Apple carries both documents' types"
        );
        assert_eq!(node_owners(&harness, company).await, BTreeSet::from([a]));
        assert_eq!(node_owners(&harness, fruit).await, BTreeSet::from([b]));
        assert_eq!(
            is_a_owners(&harness, apple, fruit).await,
            BTreeSet::from([b])
        );
        assert!(has_edge_type_point(&harness, "apple is a fruit."));

        delete(&harness, b, mode.clone()).await;

        assert!(has_node(&harness, apple).await, "{mode:?}: Apple stays");
        assert!(
            has_node(&harness, company).await,
            "{mode:?}: so does company"
        );
        assert!(has_point(&harness, "EntityType", "name", company));
        assert!(!has_node(&harness, fruit).await, "{mode:?}: fruit goes");
        assert!(!has_point(&harness, "EntityType", "name", fruit));
        assert!(node_owners(&harness, fruit).await.is_empty());
        assert_eq!(
            is_a_edges(&harness).await,
            BTreeSet::from([(apple, company)]),
            "{mode:?}"
        );
        assert!(is_a_owners(&harness, apple, fruit).await.is_empty());
        assert!(!has_edge_type_point(&harness, "apple is a fruit."));
        assert!(has_edge_type_point(&harness, "apple is a company."));

        // The last document takes everything with it.
        delete(&harness, a, mode.clone()).await;
        let (nodes, edges) = harness.graph_db.get_graph_data().await.unwrap();
        assert!(nodes.is_empty(), "{mode:?}: {nodes:?}");
        assert!(edges.is_empty(), "{mode:?}: {edges:?}");
        assert!(harness.ledger_nodes().await.is_empty());
        assert!(harness.ledger_edges().await.is_empty());
        assert_eq!(harness.vector_point_count().await, 0, "{mode:?}");
    }
}

/// The type stays when another document still declares it for another
/// entity, but Apple's `is_a` edge to it goes with the only document that
/// declared that pair, although both its ends stay — and so does the edge's
/// `EdgeType` point, while Pear's stays.
#[tokio::test]
async fn an_is_a_edge_goes_with_its_last_declaring_document() {
    for mode in [DeleteMode::Soft, DeleteMode::Hard] {
        let (harness, a, b) = cognified("Apple:Company Pear:Fruit", "Apple:Fruit").await;
        let (apple, pear) = (Entity::id_for("Apple"), Entity::id_for("Pear"));
        let (company, fruit) = (type_id("company"), type_id("fruit"));
        assert_eq!(node_owners(&harness, fruit).await, BTreeSet::from([a, b]));

        delete(&harness, b, mode.clone()).await;

        assert!(has_node(&harness, apple).await && has_node(&harness, fruit).await);
        assert_eq!(node_owners(&harness, fruit).await, BTreeSet::from([a]));
        assert_eq!(
            is_a_edges(&harness).await,
            BTreeSet::from([(apple, company), (pear, fruit)]),
            "{mode:?}: Apple is no longer a fruit"
        );
        assert!(!has_edge_type_point(&harness, "apple is a fruit."));
        assert!(has_edge_type_point(&harness, "pear is a fruit."));
        assert!(has_point(&harness, "EntityType", "name", fruit));
    }
}

/// Turn the ledger into the shape a database written before SDK-1694 O6
/// has: every EntityType and structural edge (`is_a`, `contains`,
/// `is_part_of`, `made_from`) owned by one nil-`data_id` row — instead of one
/// row per document (before O5a), or beside them (`keep_document_rows`: a
/// database written between O5a and O6, or a pre-O5a one whose documents
/// were cognified again).
async fn make_legacy(harness: &Harness, keep_document_rows: bool) {
    let structural = |row: &GraphEdge| {
        ["is_a", "is_part_of", "made_from", "contains"]
            .contains(&row.label.as_deref().unwrap_or(&row.relationship_name))
            && row.slug
                == Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    row.relationship_name
                        .to_lowercase()
                        .replace(' ', "_")
                        .replace('\'', "")
                        .as_bytes(),
                )
    };
    let types: Vec<GraphNode> = harness
        .ledger_nodes()
        .await
        .into_iter()
        .filter(|row| row.node_type == "EntityType")
        .collect();
    let edges: Vec<GraphEdge> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(structural)
        .collect();
    assert!(!types.is_empty() && !edges.is_empty());
    let removed: Vec<(&str, Uuid)> = if keep_document_rows {
        Vec::new()
    } else {
        types
            .iter()
            .map(|row| ("nodes", row.id))
            .chain(edges.iter().map(|row| ("edges", row.id)))
            .collect()
    };
    for (table, id) in removed {
        harness
            .db
            .execute_unprepared(&format!("DELETE FROM {table} WHERE id = '{}'", id.simple()))
            .await
            .unwrap();
    }
    let mut nil_types: Vec<GraphNode> = Vec::new();
    for row in types {
        if nil_types.iter().all(|nil| nil.slug != row.slug) {
            nil_types.push(GraphNode {
                id: Uuid::new_v4(),
                data_id: Uuid::nil(),
                ..row
            });
        }
    }
    let mut nil_edges: Vec<GraphEdge> = Vec::new();
    for row in edges {
        let key = (
            row.source_node_id,
            row.destination_node_id,
            row.relationship_name.clone(),
        );
        if nil_edges.iter().all(|nil| {
            (
                nil.source_node_id,
                nil.destination_node_id,
                nil.relationship_name.clone(),
            ) != key
        }) {
            nil_edges.push(GraphEdge {
                id: Uuid::new_v4(),
                data_id: Uuid::nil(),
                ..row
            });
        }
    }
    upsert_nodes(&harness.db, &nil_types).await.unwrap();
    upsert_edges(&harness.db, &nil_edges).await.unwrap();
}

/// A database written before O6 (a legacy-shaped ledger): its nil rows are
/// still claims. Deleting doc B keeps the fruit type it alone declared —
/// whether or not B's own row stands beside the nil row — while B's chunk
/// and its structural rows go as before (through the chunk's ownership).
#[tokio::test]
async fn a_legacy_ledger_keeps_its_types_as_claimed() {
    for keep_document_rows in [false, true] {
        let (harness, _a, b) = cognified("Apple:Company", "Apple:Fruit").await;
        let fruit = type_id("fruit");
        let chunk_b: Vec<Uuid> = harness
            .ledger_nodes()
            .await
            .into_iter()
            .filter(|row| row.data_id == b && row.node_type == "DocumentChunk")
            .map(|row| row.slug)
            .collect();
        assert_eq!(chunk_b.len(), 1);
        make_legacy(&harness, keep_document_rows).await;
        let mut before = BTreeSet::from([Uuid::nil()]);
        if keep_document_rows {
            before.insert(b);
        }
        assert_eq!(node_owners(&harness, fruit).await, before);

        delete(&harness, b, DeleteMode::Soft).await;

        assert!(
            has_node(&harness, fruit).await,
            "keep_document_rows {keep_document_rows}: the nil row keeps fruit"
        );
        assert!(has_point(&harness, "EntityType", "name", fruit));
        assert_eq!(
            node_owners(&harness, fruit).await,
            BTreeSet::from([Uuid::nil()])
        );
        assert!(!has_node(&harness, chunk_b[0]).await);
        let left: Vec<GraphEdge> = harness
            .ledger_edges()
            .await
            .into_iter()
            .filter(|row| row.source_node_id == chunk_b[0])
            .collect();
        assert!(
            left.is_empty(),
            "B's chunk's structural rows went: {left:?}"
        );
    }
}

/// Two documents, each cognified in a run of its own: A first, then B.
async fn cognified_in_two_runs(first: &str, second: &str) -> (Harness, Uuid, Uuid) {
    let mut harness = Harness::new().await;
    let a = harness.add_file(first).await;
    let b = harness.add_file(second).await;
    for data in [a, b] {
        harness
            .run_over(&extraction_config(), &[data], Arc::new(TypesLlm))
            .await
            .expect("cognify succeeds");
    }
    (harness, a, b)
}

/// The graph's edges named `relationship`, by endpoints.
async fn edges_named(harness: &Harness, relationship: &str) -> BTreeSet<(Uuid, Uuid)> {
    let (_, edges) = harness.graph_db.get_graph_data().await.unwrap();
    edges
        .into_iter()
        .filter(|(_, _, name, _)| name == relationship)
        .map(|(source, target, _, _)| {
            (
                Uuid::parse_str(&source).unwrap(),
                Uuid::parse_str(&target).unwrap(),
            )
        })
        .collect()
}

/// Review finding 1: a ledger Python wrote keeps one row per edge, its first
/// writer's (`upsert_edges` keys it without the `data_id`). Docs A and B both
/// declare Apple a company, but only A's row is left. Deleting A must not
/// take the edge B still declares, nor its `EdgeType` point: such a row says
/// nothing about who else holds the edge, so only a deleted endpoint removes
/// it.
#[tokio::test]
async fn a_first_writer_ledger_row_never_removes_an_edge_directly() {
    let (harness, a, _b) = cognified("Apple:Company", "Apple:Company Pear:Fruit").await;
    let (apple, company) = (Entity::id_for("Apple"), type_id("company"));
    let rows: Vec<GraphEdge> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| {
            row.source_node_id == apple
                && row.destination_node_id == company
                && row.relationship_name == "is_a"
        })
        .collect();
    assert_eq!(rows.len(), 2, "both documents' rows: {rows:?}");
    for row in &rows {
        harness
            .db
            .execute_unprepared(&format!(
                "DELETE FROM edges WHERE id = '{}'",
                row.id.simple()
            ))
            .await
            .unwrap();
    }
    // Python's row: A's, keyed on the edge alone and labelled with its name.
    let first = rows.iter().find(|row| row.data_id == a).unwrap().clone();
    let python_id = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!(
            "None{}{}{}is_a{}",
            first.user_id, first.dataset_id, apple, company
        )
        .as_bytes(),
    );
    upsert_edges(
        &harness.db,
        &[GraphEdge {
            id: python_id,
            label: Some("is_a".to_string()),
            ..first
        }],
    )
    .await
    .unwrap();

    delete(&harness, a, DeleteMode::Soft).await;

    assert!(
        is_a_edges(&harness).await.contains(&(apple, company)),
        "B's Apple keeps its company type"
    );
    assert!(has_edge_type_point(&harness, "apple is a company."));
}

/// Review finding 2: a run sweep removes the `is_a` edge its run's document
/// alone declared, with its `EdgeType` point, although Apple and `fruit` stay
/// (doc A, an earlier run, keeps both). Before, the point went and the edge
/// stayed, with no row.
#[tokio::test]
async fn a_run_sweep_removes_the_is_a_edge_its_document_alone_declared() {
    use cognee_delete::{RunSweeper, SweepScope};
    let (harness, _a, b) = cognified_in_two_runs("Apple:Company Pear:Fruit", "Apple:Fruit").await;
    let (apple, pear, fruit) = (
        Entity::id_for("Apple"),
        Entity::id_for("Pear"),
        type_id("fruit"),
    );
    assert!(is_a_edges(&harness).await.contains(&(apple, fruit)));
    let run_b = harness
        .ledger_edges()
        .await
        .into_iter()
        .find(|row| row.data_id == b)
        .and_then(|row| row.pipeline_run_id)
        .unwrap();

    RunSweeper::new(
        harness.db.clone(),
        Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>,
        Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>,
    )
    .sweep(&SweepScope::whole_run(run_b, harness.dataset_id))
    .await
    .unwrap();

    assert!(has_node(&harness, apple).await && has_node(&harness, fruit).await);
    assert_eq!(
        is_a_edges(&harness).await,
        BTreeSet::from([(apple, type_id("company")), (pear, fruit)])
    );
    assert!(!has_edge_type_point(&harness, "apple is a fruit."));
    assert!(has_edge_type_point(&harness, "pear is a fruit."));
}

/// Review finding 3: a semantic edge goes with the last document that
/// produced it, even when both its entities stay — as Python's
/// graph-provenance delete removes every edge whose last source ref goes —
/// in a data delete and in a run sweep. Doc B names Alice and Bob too, but
/// only doc A says Alice knows Bob.
#[tokio::test]
async fn a_semantic_edge_goes_with_its_last_producing_document() {
    use cognee_delete::{RunSweeper, SweepScope};
    let (alice, bob) = (Entity::id_for("Alice"), Entity::id_for("Bob"));
    for sweep in [false, true] {
        let (harness, _b, a) = cognified_in_two_runs(
            "Alice:Person Bob:Person",
            "Alice:Person Bob:Person Alice~knows~Bob",
        )
        .await;
        assert_eq!(
            edges_named(&harness, "knows").await,
            BTreeSet::from([(alice, bob)])
        );
        if sweep {
            let run_a = harness
                .ledger_edges()
                .await
                .into_iter()
                .find(|row| row.data_id == a)
                .and_then(|row| row.pipeline_run_id)
                .unwrap();
            RunSweeper::new(
                harness.db.clone(),
                Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>,
                Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>,
            )
            .sweep(&SweepScope::whole_run(run_a, harness.dataset_id))
            .await
            .unwrap();
        } else {
            delete(&harness, a, DeleteMode::Soft).await;
        }
        assert!(has_node(&harness, alice).await && has_node(&harness, bob).await);
        assert!(
            edges_named(&harness, "knows").await.is_empty(),
            "sweep {sweep}: the edge only A produced goes"
        );
        assert!(
            harness
                .ledger_edges()
                .await
                .iter()
                .all(|row| row.label.as_deref() != Some("knows")),
            "and so do its rows"
        );
    }
}

/// Review finding 4: a document whose only entity has no type of its own
/// (Apple, typed by doc A) does not claim A's type. Graph building records
/// its producer set, so the set's not naming `company` means another
/// document declared it — whatever else the set holds. Before, a set with no
/// type in it counted as hand-built, and doc B got rows for `company` and
/// `apple is_a company`, which kept both after A's delete.
#[tokio::test]
async fn a_typeless_document_does_not_claim_another_documents_type() {
    let (harness, a, b) = cognified("Apple:Company", "Apple:Untyped").await;
    let (apple, company) = (Entity::id_for("Apple"), type_id("company"));
    assert_eq!(node_owners(&harness, company).await, BTreeSet::from([a]));
    assert_eq!(
        is_a_owners(&harness, apple, company).await,
        BTreeSet::from([a])
    );
    assert!(node_owners(&harness, apple).await.contains(&b));

    delete(&harness, a, DeleteMode::Soft).await;

    assert!(has_node(&harness, apple).await, "B keeps Apple");
    assert!(!has_node(&harness, company).await, "company goes with A");
    assert!(is_a_edges(&harness).await.is_empty());
}

/// Review finding 5: doc A's save fails after the run merged its graph, so
/// doc B's group carries A's `company` type to the graph store. Under
/// `failed_items` A is swept; no graph node may be left that no ledger row
/// names (before O6 the type's nil row named it), so `company` either goes or
/// is held by a row of B, which wrote it, and goes with B.
#[tokio::test]
async fn a_type_a_failed_document_declared_never_outlives_every_row() {
    use cognee_cognify::RollbackScope;
    for before_its_rows in [true, false] {
        let mut harness = Harness::new().await;
        let a = harness.add_file("Apple:Company").await;
        let b = harness.add_file("Apple:Fruit").await;
        if before_its_rows {
            // A's ledger write fails, so its save stops before any row or graph
            // node is written, and B's group is the first to write `company`.
            harness
                .db
                .execute_unprepared(&format!(
                    "CREATE TRIGGER fail_a BEFORE INSERT ON nodes \
                 WHEN NEW.data_id = '{}' BEGIN SELECT RAISE(ABORT, 'ledger down for A'); END",
                    a.simple()
                ))
                .await
                .unwrap();
        } else {
            // A's save fails after its rows and graph nodes, while indexing.
            harness
                .vector_db
                .set_index_error_for_point(a, "the vector store is down for document A");
        }
        let config = extraction_config().with_rollback_scope(RollbackScope::FailedItems);
        let outcome = harness.run_over(&config, &[a, b], Arc::new(TypesLlm)).await;
        assert!(outcome.is_err(), "A's failure errors the run");
        let company = type_id("company");
        let owners = node_owners(&harness, company).await;
        println!(
            "company in graph {} owners {owners:?} (a={a}, b={b})",
            has_node(&harness, company).await
        );
        assert!(
            !has_node(&harness, company).await || owners.iter().any(|data| !data.is_nil()),
            "company is in the graph with no row: {owners:?}"
        );

        delete(&harness, b, DeleteMode::Soft).await;
        let (nodes, _) = harness.graph_db.get_graph_data().await.unwrap();
        assert!(
            nodes.is_empty(),
            "before_its_rows {before_its_rows}: nothing outlives both documents: {nodes:?}"
        );
    }
}
