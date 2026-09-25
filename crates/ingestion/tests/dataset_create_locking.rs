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
use cognee_ingestion::{AddParams, AddPipeline, DatasetLocks, generate_dataset_id};
use cognee_models::{DataInput, Dataset};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use tokio::sync::Notify;
use uuid::Uuid;

/// How long the *locked* simulated create holds its half-finished row before
/// rolling it back.
///
/// Only used on the wired path, where the ingest is blocked on the lock for
/// exactly this long and the assertions do not depend on the duration — so a
/// slow runner cannot change the outcome, only the wait. The unwired path does
/// not use a clock at all; see [`Rollback`].
const LOCKED_WINDOW: Duration = Duration::from_millis(200);

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

/// When the simulated create stops sitting in its grant and rolls the row back.
enum Rollback {
    /// After a fixed wait. Used only where the ingest is *blocked on the lock*
    /// for that wait, so the duration decides how long the test takes and
    /// nothing else.
    After(Duration),
    /// When signalled. Used where the ingest is *not* blocked, so "has it
    /// reached the row yet?" is a real question — and one a wall clock answers
    /// wrong on a loaded runner. Waiting for the signal makes the unwired case
    /// deterministic instead of a race the test can lose.
    OnSignal(Arc<Notify>),
}

/// Stand in for `create_new_dataset`'s failing path: take the identity lock,
/// write the row, sit in the grant, then roll the row back.
///
/// Uses a raw row delete, because the point being tested is the *visibility* of
/// the row during the window, not how thoroughly it is swept afterwards.
async fn simulated_failing_create(
    db: Arc<cognee_database::DatabaseConnection>,
    locks: Arc<DatasetLocks>,
    owner: Uuid,
    row_written: Arc<Notify>,
    rollback: Rollback,
) {
    let id = generate_dataset_id(DATASET, owner, None);
    let _guard = locks.lock(id).await;

    db.create_dataset(Dataset::new(DATASET.to_string(), owner, None, id))
        .await
        .expect("insert the dataset row");
    row_written.notify_one();

    // The grant is in flight here. The row exists and is not usable.
    match rollback {
        Rollback::After(d) => tokio::time::sleep(d).await,
        Rollback::OnSignal(signal) => signal.notified().await,
    }

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
        Rollback::After(LOCKED_WINDOW),
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
    // No clock here: the creator rolls back only once the ingest has actually
    // finished attaching, so a slow runner cannot turn this into a pass.
    let ingest_done = Arc::new(Notify::new());
    let creator = tokio::spawn(simulated_failing_create(
        Arc::clone(&db),
        locks,
        owner,
        Arc::clone(&row_written),
        Rollback::OnSignal(Arc::clone(&ingest_done)),
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
    ingest_done.notify_one();
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

/// Scenario: the same interleaving, but the ingest targets the doomed dataset
/// by **id** (`AddParams::dataset_id`) rather than by name.
/// Expected: identical protection. The by-id path creates nothing and grants
/// nothing, so it has no insert-to-grant window of its own — but the window
/// being guarded is about attaching to a row someone else is rolling back, and
/// a half-created row is reachable by id: `uuid5(name, owner, tenant)` is
/// derivable from the name, so no listing is needed to observe it. (Before
/// SDK-637 the listing handed it over as well — `GET /v1/datasets` read through
/// to ownership when the ACL returned nothing — which it now does only with no
/// `AclDb` wired.) Skipping the lock here would walk straight into the case the
/// by-name lock exists to prevent.
/// Verification: resolve by id against the doomed row with locks wired, then
/// assert the ingest's dataset and data both survive.
#[tokio::test(flavor = "multi_thread")]
async fn add_by_id_is_locked_too() {
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
        Rollback::After(LOCKED_WINDOW),
    ));

    row_written.notified().await;

    // The id a client can derive, or read out of `GET /v1/datasets`, while the
    // row is still half-created.
    let doomed_id = generate_dataset_id(DATASET, owner, None);
    let params = AddParams {
        dataset_id: Some(doomed_id),
        ..AddParams::default()
    };

    let result = pipeline
        .add_with_params(
            vec![DataInput::Text("a document worth keeping".to_string())],
            DATASET,
            owner,
            None,
            &params,
        )
        .await;
    creator.await.expect("simulated create");

    // Blocked until the rollback finished, the id no longer resolves, so the
    // ingest fails loudly instead of silently writing into a deleted dataset.
    // Either outcome is acceptable — what is not is "reported success, and the
    // data is attached to a dataset that is gone".
    //
    // The failure must be *that* failure, though. Accepting any `Err` would let
    // an unrelated pipeline breakage stand in for the lock working, and the
    // test would keep passing after the protection was removed.
    if let Err(ref e) = result {
        let msg = e.to_string();
        assert!(
            msg.contains(&doomed_id.to_string()) && msg.contains("not found"),
            "the only acceptable error is the rolled-back id failing to resolve, got: {msg}"
        );
    }
    if let Ok(ingested) = result {
        assert!(
            IngestDb::get_dataset_by_name(&*db, DATASET, owner, None)
                .await
                .expect("lookup")
                .is_some(),
            "add reported success but its dataset is gone — the by-id path ingested \
             into a row a concurrent create was about to roll back"
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
}
