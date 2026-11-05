#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The `provenance_edge_evidence` sidecar: schema, idempotent capture, the
//! active-support lookup and the delete sweeps. In-memory SQLite.
#![cfg(feature = "sqlite")]

use chrono::{Duration, Utc};
use cognee_database::ops::data::{create_data, delete_data};
use cognee_database::ops::datasets::{create_dataset, delete_dataset};
use cognee_database::ops::edge_evidence::{
    delete_edge_evidence, get_edge_evidence_records, insert_edge_evidence,
};
use cognee_database::ops::pipeline_runs::create_pipeline_run;
use cognee_database::{
    DatabaseConnection, EdgeEvidence, PipelineRun, PipelineRunStatus, connect, initialize,
};
use cognee_models::{Data, Dataset};
use sea_orm::{ConnectionTrait, Statement};
use uuid::Uuid;

struct Seed {
    db: DatabaseConnection,
    user: Uuid,
    dataset: Uuid,
}

async fn seed() -> Seed {
    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(&db, Dataset::new("evidence".into(), user, None, dataset))
        .await
        .unwrap();
    Seed { db, user, dataset }
}

async fn seed_data(seed: &Seed, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let data = Data::builder(
        id,
        name,
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        format!("hash-{id}"),
        seed.user,
    )
    .dataset_id(seed.dataset)
    .build();
    create_data(&seed.db, data).await.unwrap();
    id
}

async fn run(seed: &Seed, status: PipelineRunStatus) -> Uuid {
    let run_id = Uuid::new_v4();
    create_pipeline_run(
        &seed.db,
        PipelineRun {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            status,
            pipeline_run_id: run_id,
            pipeline_name: "cognify_pipeline".into(),
            pipeline_id: Uuid::new_v4(),
            dataset_id: Some(seed.dataset),
            run_info: None,
            user_id: None,
        },
    )
    .await
    .unwrap();
    run_id
}

fn row(
    seed: &Seed,
    data_id: Uuid,
    chunk_id: Uuid,
    edge_id: Uuid,
    run: Option<Uuid>,
    age_secs: i64,
) -> EdgeEvidence {
    EdgeEvidence {
        id: Uuid::new_v4(),
        tenant_id: None,
        user_id: seed.user,
        dataset_id: seed.dataset,
        data_id,
        pipeline_run_id: run,
        chunk_id,
        chunk_index: Some(0),
        edge_id,
        source_node_id: Uuid::new_v4(),
        destination_node_id: Uuid::new_v4(),
        relationship_name: "works_with".into(),
        evidence_kind: "extracted".into(),
        source_task: None,
        confidence: None,
        created_at: Utc::now() - Duration::seconds(age_secs),
    }
}

async fn count(db: &DatabaseConnection) -> i64 {
    db.query_one(Statement::from_string(
        db.get_database_backend(),
        "SELECT COUNT(*) AS cnt FROM provenance_edge_evidence",
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "cnt")
    .unwrap()
}

#[tokio::test]
async fn migration_creates_the_table_with_pythons_index_names() {
    let seed = seed().await;
    let rows = seed
        .db
        .query_all(Statement::from_string(
            seed.db.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type='index' \
             AND tbl_name='provenance_edge_evidence'",
        ))
        .await
        .unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect();
    for expected in [
        "ix_prov_evidence_edge",
        "ix_prov_evidence_source",
        "ix_prov_evidence_run",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
}

#[tokio::test]
async fn insert_is_idempotent_by_row_id() {
    let seed = seed().await;
    let data = seed_data(&seed, "a.txt").await;
    let one = row(&seed, data, Uuid::new_v4(), Uuid::new_v4(), None, 0);

    insert_edge_evidence(&seed.db, std::slice::from_ref(&one))
        .await
        .unwrap();
    insert_edge_evidence(&seed.db, &[one.clone(), one])
        .await
        .unwrap();

    assert_eq!(count(&seed.db).await, 1);
}

#[tokio::test]
async fn lookup_returns_active_supports_ranked_by_first_sighting() {
    let seed = seed().await;
    let data = seed_data(&seed, "report.txt").await;
    let edge = Uuid::new_v4();
    let (older, newer, failed_run_chunk, running_chunk) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let completed = run(&seed, PipelineRunStatus::Completed).await;
    let errored = run(&seed, PipelineRunStatus::Errored).await;
    let started = run(&seed, PipelineRunStatus::Started).await;

    let mut older_row = row(&seed, data, older, edge, Some(completed), 100);
    older_row.chunk_index = Some(2);
    let rows = vec![
        row(&seed, data, newer, edge, Some(completed), 10),
        older_row,
        // The same support seen again later keeps its first sighting.
        row(&seed, data, older, edge, None, 1),
        row(&seed, data, failed_run_chunk, edge, Some(errored), 200),
        row(&seed, data, running_chunk, edge, Some(started), 200),
    ];
    insert_edge_evidence(&seed.db, &rows).await.unwrap();

    let records = get_edge_evidence_records(&seed.db, seed.dataset, &[edge, edge], 5, 50)
        .await
        .unwrap();

    // `older` appears under two chunk_index values only if they differ; here
    // chunk_index 2 and 0 are two supports of the same chunk.
    let chunks: Vec<Uuid> = records.iter().map(|r| r.chunk_id).collect();
    assert!(!chunks.contains(&failed_run_chunk));
    assert!(!chunks.contains(&running_chunk));
    assert_eq!(chunks.first(), Some(&older));
    assert!(chunks.contains(&newer));
    assert!(
        records
            .iter()
            .all(|r| r.edge_id == edge && r.data_id == data)
    );
    assert!(
        records
            .iter()
            .all(|r| r.document_name.as_deref() == Some("report.txt"))
    );
}

#[tokio::test]
async fn lookup_limits_per_edge_and_in_total_and_skips_missing_documents() {
    let seed = seed().await;
    let data = seed_data(&seed, "doc.txt").await;
    let gone = Uuid::new_v4(); // no Data row
    let (edge_a, edge_b) = (Uuid::new_v4(), Uuid::new_v4());
    let mut rows = Vec::new();
    for age in 0..3 {
        rows.push(row(&seed, data, Uuid::new_v4(), edge_a, None, 30 - age));
        rows.push(row(&seed, data, Uuid::new_v4(), edge_b, None, 30 - age));
    }
    rows.push(row(&seed, gone, Uuid::new_v4(), edge_a, None, 99));
    insert_edge_evidence(&seed.db, &rows).await.unwrap();

    // The orphaned support has the oldest sighting of edge A, so it takes A's
    // first rank and is dropped by the Data join afterwards: Python ranks
    // before joining, so A keeps one of its two slots.
    let per_edge = get_edge_evidence_records(&seed.db, seed.dataset, &[edge_a, edge_b], 2, 50)
        .await
        .unwrap();
    assert_eq!(per_edge.iter().filter(|r| r.edge_id == edge_a).count(), 1);
    assert_eq!(per_edge.iter().filter(|r| r.edge_id == edge_b).count(), 2);
    // Edge-major order.
    let edges: Vec<Uuid> = per_edge.iter().map(|r| r.edge_id).collect();
    let mut sorted = edges.clone();
    sorted.sort_by_key(|id| id.simple().to_string());
    assert_eq!(edges, sorted);

    // With room for every support the orphan is simply absent.
    let all = get_edge_evidence_records(&seed.db, seed.dataset, &[edge_a], 5, 50)
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    let total = get_edge_evidence_records(&seed.db, seed.dataset, &[edge_a, edge_b], 5, 4)
        .await
        .unwrap();
    assert_eq!(total.len(), 4);

    // Other datasets and empty requests see nothing.
    assert!(
        get_edge_evidence_records(&seed.db, Uuid::new_v4(), &[edge_a], 5, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        get_edge_evidence_records(&seed.db, seed.dataset, &[], 5, 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn delete_sweeps_by_data_and_by_dataset() {
    let seed = seed().await;
    let (a, b) = (seed_data(&seed, "a").await, seed_data(&seed, "b").await);
    let edge = Uuid::new_v4();
    insert_edge_evidence(
        &seed.db,
        &[
            row(&seed, a, Uuid::new_v4(), edge, None, 0),
            row(&seed, b, Uuid::new_v4(), edge, None, 0),
        ],
    )
    .await
    .unwrap();

    assert_eq!(
        delete_edge_evidence(&seed.db, seed.dataset, Some(a))
            .await
            .unwrap(),
        1
    );
    assert_eq!(count(&seed.db).await, 1);
    assert_eq!(
        delete_edge_evidence(&seed.db, seed.dataset, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(count(&seed.db).await, 0);
}

#[tokio::test]
async fn deleting_a_data_row_or_a_dataset_removes_its_evidence() {
    let seed = seed().await;
    let (a, b) = (seed_data(&seed, "a").await, seed_data(&seed, "b").await);
    let edge = Uuid::new_v4();
    insert_edge_evidence(
        &seed.db,
        &[
            row(&seed, a, Uuid::new_v4(), edge, None, 0),
            row(&seed, b, Uuid::new_v4(), edge, None, 0),
        ],
    )
    .await
    .unwrap();

    delete_data(&seed.db, a).await.unwrap();
    assert_eq!(count(&seed.db).await, 1);

    delete_dataset(&seed.db, seed.dataset).await.unwrap();
    assert_eq!(count(&seed.db).await, 0);
}

/// Python migrated the shared SQLite file first: its alembic revision
/// `f3a7b9c1d2e4` created the table with SQLAlchemy's DDL (`Uuid` is CHAR(32)
/// hex, DATETIME, FLOAT) and Python wrote a row in SQLAlchemy's DATETIME
/// format. The Rust migration skips the existing table, Rust writes beside the
/// Python row, and the lookup reads both.
#[tokio::test]
async fn a_python_migrated_sqlite_table_is_shared() {
    let db = connect("sqlite::memory:").await.unwrap();
    for ddl in [
        "CREATE TABLE provenance_edge_evidence (
            id CHAR(32) NOT NULL, tenant_id CHAR(32), user_id CHAR(32) NOT NULL,
            dataset_id CHAR(32) NOT NULL, data_id CHAR(32) NOT NULL,
            pipeline_run_id CHAR(32), chunk_id CHAR(32) NOT NULL, chunk_index INTEGER,
            edge_id CHAR(32) NOT NULL, source_node_id CHAR(32) NOT NULL,
            destination_node_id CHAR(32) NOT NULL, relationship_name TEXT NOT NULL,
            evidence_kind VARCHAR(32) DEFAULT 'extracted' NOT NULL,
            source_task VARCHAR(255), confidence FLOAT, created_at DATETIME NOT NULL,
            PRIMARY KEY (id))",
        "CREATE INDEX ix_prov_evidence_edge ON provenance_edge_evidence (dataset_id, edge_id)",
        "CREATE INDEX ix_prov_evidence_source ON provenance_edge_evidence \
            (dataset_id, data_id, chunk_id)",
        "CREATE INDEX ix_prov_evidence_run ON provenance_edge_evidence (pipeline_run_id)",
    ] {
        db.execute(Statement::from_string(db.get_database_backend(), ddl))
            .await
            .unwrap();
    }
    initialize(&db).await.unwrap();

    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    create_dataset(&db, Dataset::new("shared".into(), user, None, dataset))
        .await
        .unwrap();
    let seed = Seed { db, user, dataset };
    let data = seed_data(&seed, "shared.txt").await;
    let edge = Uuid::new_v4();
    let (python_chunk, rust_chunk) = (Uuid::new_v4(), Uuid::new_v4());

    // Python's row: hex ids, no run, SQLAlchemy's naive DATETIME text, older.
    seed.db
        .execute(Statement::from_string(
            seed.db.get_database_backend(),
            format!(
                "INSERT INTO provenance_edge_evidence (id, user_id, dataset_id, data_id, \
                 chunk_id, chunk_index, edge_id, source_node_id, destination_node_id, \
                 relationship_name, evidence_kind, created_at) VALUES ('{}', '{}', '{}', \
                 '{}', '{}', 0, '{}', '{}', '{}', 'works_with', 'extracted', \
                 '2020-01-01 00:00:00.000000')",
                Uuid::new_v4().simple(),
                user.simple(),
                dataset.simple(),
                data.simple(),
                python_chunk.simple(),
                edge.simple(),
                Uuid::new_v4().simple(),
                Uuid::new_v4().simple(),
            ),
        ))
        .await
        .unwrap();
    insert_edge_evidence(&seed.db, &[row(&seed, data, rust_chunk, edge, None, 0)])
        .await
        .unwrap();

    let records = get_edge_evidence_records(&seed.db, dataset, &[edge], 5, 50)
        .await
        .unwrap();
    let chunks: Vec<Uuid> = records.iter().map(|r| r.chunk_id).collect();
    // Python's older sighting ranks first.
    assert_eq!(chunks, vec![python_chunk, rust_chunk]);
}
