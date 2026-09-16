#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! SDK-636: `add` must not ingest into a dataset a concurrent create is about
//! to roll back.
//!
//! A dataset row and its ACL rows cannot be written in one transaction, so
//! `POST /v1/datasets` writes the row, grants second, and compensates a failed
//! grant by deleting the row again. Between the insert and the grant the row is
//! visible to everyone — including `add`, which resolves the dataset by name
//! and happily attaches data to whatever it finds. When the create then rolls
//! back, that data is left pointing at a dataset that no longer exists, and the
//! caller was told the ingest succeeded.
//!
//! [`AddPipeline::with_dataset_locks`] closes it: the ingest path takes the
//! same per-identity lock the create handler holds, so it either finds a
//! committed dataset or finds nothing and creates its own.
//!
//! The create side of the same window — two `POST /v1/datasets` racing — is
//! covered in `crates/http-server/tests/test_dataset_create_acl.rs`.

use std::sync::Arc;
use std::time::Duration;

use cognee_core::RayonThreadPool;
use cognee_database::{DeleteDb, IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::{AddPipeline, DatasetLocks, generate_dataset_id};
use cognee_models::{DataInput, Dataset};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use tokio::sync::Notify;
use uuid::Uuid;

/// How long the simulated create holds its half-finished row before rolling it
/// back. Long enough for an *unsynchronised* `add` to resolve that row and
/// attach to it — which is what makes the without-the-lock failure
/// deterministic rather than a race the test might lose.
const WINDOW: Duration = Duration::from_millis(400);

const DATASET: &str = "contended_ds";

async fn make_pipeline(dir: &TempDir) -> (AddPipeline, Arc<cognee_database::DatabaseConnection>) {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file should be created");
    let db = connect(&format!("sqlite://{}", db_path.display()))
        .await
        .expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);

    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let pipeline = AddPipeline::new(
        storage as Arc<dyn StorageTrait>,
        Arc::clone(&db) as Arc<dyn IngestDb>,
    )
    .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
    .with_graph_db(Arc::new(MockGraphDB::new()))
    .with_vector_db(Arc::new(MockVectorDB::new()))
    .with_database(Arc::clone(&db));

    (pipeline, db)
}

/// Stand in for `create_new_dataset`'s failing path: take the identity lock,
/// write the row, sit in the grant for `WINDOW`, then roll the row back.
///
/// Uses the same raw row delete the handler's compensation used to, because the
/// point being tested is the *visibility* of the row during the window, not how
/// thoroughly it is swept afterwards.
async fn simulated_failing_create(
    db: Arc<cognee_database::DatabaseConnection>,
    locks: Arc<DatasetLocks>,
    owner: Uuid,
    row_written: Arc<Notify>,
) {
    let id = generate_dataset_id(DATASET, owner, None);
    let _guard = locks.lock(id).await;

    db.create_dataset(Dataset::new(DATASET.to_string(), owner, None, id))
        .await
        .expect("insert the dataset row");
    row_written.notify_one();

    // The grant is in flight here. The row exists and is not usable.
    tokio::time::sleep(WINDOW).await;

    DeleteDb::delete_dataset(&*db, id)
        .await
        .expect("roll the row back");
}

/// Scenario: `add` for a dataset name arrives while a create for that same name
/// sits between its insert and its grant, and that create then rolls back.
/// Expected: the ingest's data is attached to a dataset that still exists.
/// Unsynchronised, `add` resolves the doomed row, attaches to it, reports
/// success, and the rollback deletes the dataset out from under it — the
/// caller's data is then linked to an id with no row behind it.
/// Verification: run both concurrently with a shared [`DatasetLocks`], then
/// assert the dataset resolves by name and the returned `Data` id is reachable.
#[tokio::test(flavor = "multi_thread")]
async fn add_does_not_ingest_into_a_dataset_that_is_being_rolled_back() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, db) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    let locks = Arc::new(DatasetLocks::new());
    let pipeline = pipeline.with_dataset_locks(Arc::clone(&locks));

    let row_written = Arc::new(Notify::new());
    let creator = tokio::spawn(simulated_failing_create(
        Arc::clone(&db),
        Arc::clone(&locks),
        owner,
        Arc::clone(&row_written),
    ));

    // Start the ingest only once the doomed row is visible — otherwise `add`
    // may create the dataset itself before the window ever opens, and the test
    // would pass without exercising anything.
    row_written.notified().await;

    let ingested = pipeline
        .add(
            vec![DataInput::Text("a document worth keeping".to_string())],
            DATASET,
            owner,
            None,
        )
        .await
        .expect("add reports success");
    creator.await.expect("simulated create");

    let dataset = IngestDb::get_dataset_by_name(&*db, DATASET, owner, None)
        .await
        .expect("lookup");
    assert!(
        dataset.is_some(),
        "add reported success but its dataset is gone — it ingested into a row a \
         concurrent create was about to roll back"
    );

    let data_id = ingested.first().expect("one data item").id;
    assert!(
        IngestDb::get_data(&*db, data_id)
            .await
            .expect("lookup")
            .is_some(),
        "the ingested data row must survive alongside its dataset"
    );
}

/// Scenario: the same interleaving, but the caller wired no locks — the CLI and
/// library-facade default.
/// Expected: behaviour is unchanged from before SDK-636, i.e. the ingest does
/// attach to the doomed row and loses its dataset. Pinned deliberately: the
/// lock is opt-in, and a test that only ever ran the wired path would not
/// notice if the unwired one silently started blocking.
/// Verification: same interleaving with `with_dataset_locks` omitted; assert
/// the dataset is gone, which is exactly what the wired case above forbids.
#[tokio::test(flavor = "multi_thread")]
async fn without_locks_the_window_is_still_open() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, db) = make_pipeline(&dir).await;
    let owner = Uuid::new_v4();

    // The creator still takes a lock; the pipeline just does not share it.
    let locks = Arc::new(DatasetLocks::new());
    let row_written = Arc::new(Notify::new());
    let creator = tokio::spawn(simulated_failing_create(
        Arc::clone(&db),
        locks,
        owner,
        Arc::clone(&row_written),
    ));

    row_written.notified().await;
    pipeline
        .add(
            vec![DataInput::Text("a document worth keeping".to_string())],
            DATASET,
            owner,
            None,
        )
        .await
        .expect("add reports success");
    creator.await.expect("simulated create");

    assert!(
        IngestDb::get_dataset_by_name(&*db, DATASET, owner, None)
            .await
            .expect("lookup")
            .is_none(),
        "precondition for the test above: unsynchronised, the ingest attaches to the \
         doomed row and the rollback takes the dataset with it"
    );
}
