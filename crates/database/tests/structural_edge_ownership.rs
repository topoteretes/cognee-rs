#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The ledger queries behind a data-scoped delete's structural-edge cleanup:
//!
//! * `get_structural_edges_for_data` attributes a nil-`data_id` structural row
//!   to the item when either endpoint is exclusive to it;
//! * `get_edge_retrieval_texts_claimed_outside_data` answers which candidate
//!   `EdgeType` texts surviving rows still claim, reading only rows that could
//!   produce a candidate and ignoring orphaned structural rows;
//! * `delete_edges_by_ids` deletes exactly the named rows and counts them.
//!
//! Every case runs on in-memory SQLite and, when `TEST_POSTGRES_URL` is set,
//! on a throwaway Postgres database, because the queries are correlated
//! `EXISTS` subqueries plus backend-specific JSON and trim SQL.
#![cfg(feature = "sqlite")]

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    delete_edges_by_ids, get_edge_retrieval_texts_claimed_outside_data, get_edges_by_dataset,
    get_structural_edges_for_data, upsert_edges, upsert_nodes,
};
use cognee_database::{DatabaseConnection, GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

struct Fixture<'a> {
    db: &'a DatabaseConnection,
    user: Uuid,
    dataset: Uuid,
    other_dataset: Uuid,
    a: Uuid,
    b: Uuid,
}

impl<'a> Fixture<'a> {
    async fn new(db: &'a DatabaseConnection) -> Self {
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let other_dataset = Uuid::new_v4();
        for (id, name) in [(dataset, "d"), (other_dataset, "e")] {
            create_dataset(db, Dataset::new(format!("{name}-{id}"), user, None, id))
                .await
                .expect("dataset");
        }
        Self {
            db,
            user,
            dataset,
            other_dataset,
            a: Uuid::new_v4(),
            b: Uuid::new_v4(),
        }
    }

    /// One node row claiming `slug` for `data` in `dataset`.
    async fn claim(&self, dataset: Uuid, data: Uuid, slug: Uuid) {
        upsert_nodes(
            self.db,
            &[GraphNode {
                id: Uuid::new_v4(),
                slug,
                user_id: self.user,
                data_id: data,
                dataset_id: dataset,
                pipeline_run_id: None,
                label: None,
                node_type: "Entity".into(),
                indexed_fields: json!(["name"]),
                attributes: None,
                created_at: Utc::now(),
            }],
        )
        .await
        .expect("upsert node");
    }

    /// Insert one edge row and return it.
    async fn edge(
        &self,
        dataset: Uuid,
        data: Uuid,
        source: Uuid,
        target: Uuid,
        relationship_name: &str,
        edge_text: Option<&str>,
    ) -> GraphEdge {
        let row = GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: self.user,
            data_id: data,
            dataset_id: dataset,
            pipeline_run_id: None,
            source_node_id: source,
            destination_node_id: target,
            relationship_name: relationship_name.into(),
            label: None,
            attributes: edge_text.map(|text| json!({ "edge_text": text })),
            created_at: Utc::now(),
        };
        upsert_edges(self.db, std::slice::from_ref(&row))
            .await
            .expect("upsert edge");
        row
    }

    async fn structural_for_a(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = get_structural_edges_for_data(self.db, self.a, self.dataset)
            .await
            .expect("structural rows")
            .into_iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        ids
    }

    async fn claimed_outside_a(&self, candidates: &[&str]) -> Vec<String> {
        let candidates: Vec<String> = candidates.iter().map(|text| text.to_string()).collect();
        get_edge_retrieval_texts_claimed_outside_data(self.db, self.a, self.dataset, &candidates)
            .await
            .expect("claimed texts")
    }
}

/// Item A types `alice` as `person`, item B types her as `engineer`. `alice`
/// is shared, `person` is exclusive to A (the node-row shape a Python-written
/// ledger has, where every node row carries the ctx `data_id`). Deleting A
/// detach-deletes `person`, so `alice -is_a-> person` is gone with it and its
/// row must go too, although its source is shared.
async fn either_endpoint_attributes_the_row(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let (alice, person, engineer) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    f.claim(f.dataset, f.a, alice).await;
    f.claim(f.dataset, f.b, alice).await;
    f.claim(f.dataset, f.a, person).await;
    f.claim(f.dataset, f.b, engineer).await;
    let to_person = f
        .edge(
            f.dataset,
            Uuid::nil(),
            alice,
            person,
            "is_a",
            Some("alice is a person."),
        )
        .await;
    f.edge(
        f.dataset,
        Uuid::nil(),
        alice,
        engineer,
        "is_a",
        Some("alice is a engineer."),
    )
    .await;

    assert_eq!(f.structural_for_a().await, vec![to_person.id]);
    assert_eq!(
        f.claimed_outside_a(&["alice is a person.", "alice is a engineer."])
            .await,
        vec!["alice is a engineer.".to_string()],
        "the row whose destination A owns no longer claims its text"
    );
}

/// The Rust cognify shape: an `EntityType` node row has a nil `data_id`, so
/// it is never exclusive and the graph keeps `alice -is_a-> person` while
/// `alice` survives. The row stays, and so does its claim.
async fn shared_endpoints_keep_the_row(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let (alice, person, chunk_a, doc_a) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    f.claim(f.dataset, f.a, alice).await;
    f.claim(f.dataset, f.b, alice).await;
    f.claim(f.dataset, Uuid::nil(), person).await;
    f.claim(f.dataset, f.a, chunk_a).await;
    f.claim(f.dataset, f.a, doc_a).await;
    f.edge(
        f.dataset,
        Uuid::nil(),
        alice,
        person,
        "is_a",
        Some("alice is a person."),
    )
    .await;
    let part_of = f
        .edge(f.dataset, Uuid::nil(), chunk_a, doc_a, "is_part_of", None)
        .await;
    // The same structural shape in another dataset is never A's.
    f.claim(f.other_dataset, f.a, chunk_a).await;
    f.edge(
        f.other_dataset,
        Uuid::nil(),
        chunk_a,
        doc_a,
        "is_part_of",
        None,
    )
    .await;

    assert_eq!(f.structural_for_a().await, vec![part_of.id]);
    assert_eq!(
        f.claimed_outside_a(&["alice is a person.", "is_part_of"])
            .await,
        vec!["alice is a person.".to_string(), "is_part_of".to_string()],
        "the shared entity's row and the other dataset's row both still claim"
    );
}

/// A row left by an item deleted before structural rows were attributed: both
/// endpoints' node rows are gone, so the row can never be selected, and it
/// must not pin the shared `is_part_of` point. A live row of another item
/// whose destination never had a node row still claims it through its source.
async fn orphaned_structural_rows_claim_nothing(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let (chunk_a, doc_a) = (Uuid::new_v4(), Uuid::new_v4());
    f.claim(f.dataset, f.a, chunk_a).await;
    f.claim(f.dataset, f.a, doc_a).await;
    f.edge(f.dataset, Uuid::nil(), chunk_a, doc_a, "is_part_of", None)
        .await;
    // The orphan: no node row anywhere names either endpoint.
    let orphan = f
        .edge(
            f.dataset,
            Uuid::nil(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "is_part_of",
            None,
        )
        .await;

    assert!(
        !f.structural_for_a().await.contains(&orphan.id),
        "an orphan is no item's row"
    );
    assert!(
        f.claimed_outside_a(&["is_part_of"]).await.is_empty(),
        "an orphaned row does not keep the text alive"
    );

    // Another item's live chunk, pointing at a document that has no node row
    // (a ledger written before documents got one): one claimed endpoint is
    // enough.
    let chunk_b = Uuid::new_v4();
    f.claim(f.dataset, f.b, chunk_b).await;
    f.edge(
        f.dataset,
        Uuid::nil(),
        chunk_b,
        Uuid::new_v4(),
        "is_part_of",
        None,
    )
    .await;
    assert_eq!(
        f.claimed_outside_a(&["is_part_of"]).await,
        vec!["is_part_of".to_string()]
    );
}

/// The candidate filter matches rows exactly as `EdgeType::retrieval_text`
/// derives their text: the trimmed `edge_text` (Unicode whitespace included)
/// or, when that is missing or blank, the trimmed `relationship_name`. Rows
/// that cannot produce a candidate are not returned.
async fn candidate_filter_matches_the_retrieval_text(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let (s, t) = (Uuid::new_v4(), Uuid::new_v4());
    f.edge(f.dataset, f.b, s, t, "rel", Some("  padded text\n"))
        .await;
    f.edge(f.dataset, f.b, s, t, " knows\t", Some("   ")).await;
    f.edge(f.dataset, f.b, s, t, "employees", Some("works at"))
        .await;
    f.edge(f.dataset, f.b, s, t, "x", Some("\u{3000}wide\u{A0}"))
        .await;
    f.edge(f.dataset, f.b, s, t, "unrelated", None).await;
    // The deleted item's own row claims nothing.
    f.edge(f.dataset, f.a, s, t, "own text", None).await;

    assert_eq!(
        f.claimed_outside_a(&[
            "padded text",
            "knows",
            "works at",
            "employees",
            "wide",
            "own text",
            "rel",
        ])
        .await,
        vec![
            "knows".to_string(),
            "padded text".to_string(),
            "wide".to_string(),
            "works at".to_string(),
        ],
        "`employees` and `rel` are shadowed by their edge_text"
    );
    assert!(f.claimed_outside_a(&[]).await.is_empty());

    // A row of A in another dataset survives deleting A from this one.
    f.edge(f.other_dataset, f.a, s, t, "own text", None).await;
    assert_eq!(
        f.claimed_outside_a(&["own text"]).await,
        vec!["own text".to_string()]
    );
}

/// More candidates than one statement binds: every batch is consulted.
async fn candidates_are_batched(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let candidates: Vec<String> = (0..700).map(|i| format!("text {i:04}")).collect();
    for i in [3usize, 260, 699] {
        f.edge(
            f.dataset,
            f.b,
            Uuid::new_v4(),
            Uuid::new_v4(),
            "rel",
            Some(&candidates[i]),
        )
        .await;
    }
    let claimed = get_edge_retrieval_texts_claimed_outside_data(f.db, f.a, f.dataset, &candidates)
        .await
        .expect("claimed texts");
    assert_eq!(claimed, vec!["text 0003", "text 0260", "text 0699"]);
}

/// Deleting by id removes exactly the named rows, ignores ids that are gone,
/// and reports the real count, across batches.
async fn delete_by_ids_counts_what_went(db: &DatabaseConnection) {
    let f = Fixture::new(db).await;
    let mut ids = Vec::new();
    for _ in 0..520 {
        ids.push(
            f.edge(f.dataset, f.a, Uuid::new_v4(), Uuid::new_v4(), "rel", None)
                .await
                .id,
        );
    }
    let keep = f
        .edge(f.dataset, f.a, Uuid::new_v4(), Uuid::new_v4(), "rel", None)
        .await;
    let mut targets = ids.clone();
    targets.push(Uuid::new_v4());
    assert_eq!(delete_edges_by_ids(f.db, &targets).await.unwrap(), 520);
    assert_eq!(delete_edges_by_ids(f.db, &targets).await.unwrap(), 0);
    assert_eq!(delete_edges_by_ids(f.db, &[]).await.unwrap(), 0);
    let left = get_edges_by_dataset(f.db, f.dataset).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, keep.id);
}

/// Empty the ledger between cases: the claim queries deliberately look across
/// every dataset, so one case's rows would otherwise answer for the next.
async fn clear_ledger(db: &DatabaseConnection) {
    for table in ["edges", "nodes"] {
        db.execute_unprepared(&format!("DELETE FROM {table}"))
            .await
            .expect("clear ledger table");
    }
}

async fn run_all(db: &DatabaseConnection) {
    either_endpoint_attributes_the_row(db).await;
    clear_ledger(db).await;
    shared_endpoints_keep_the_row(db).await;
    clear_ledger(db).await;
    orphaned_structural_rows_claim_nothing(db).await;
    clear_ledger(db).await;
    candidate_filter_matches_the_retrieval_text(db).await;
    clear_ledger(db).await;
    candidates_are_batched(db).await;
    clear_ledger(db).await;
    delete_by_ids_counts_what_went(db).await;
}

#[tokio::test]
async fn structural_edge_ownership_sqlite() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    run_all(&db).await;
}

/// The same suite on Postgres. Skipped when `TEST_POSTGRES_URL` is unset; the
/// run gets its own throwaway database.
#[tokio::test]
async fn structural_edge_ownership_postgres() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping structural_edge_ownership_postgres");
        return;
    };
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("create temp Postgres database");
    let db = connect(tmp.url()).await.expect("connect to temp Postgres");
    initialize(&db).await.expect("migrate");
    run_all(&db).await;
    drop(db);
    tmp.cleanup().await;
}
