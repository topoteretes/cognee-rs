//! Integration tests for adding image and audio files via `AddPipeline`.
//!
//! As of task 17 ("Run loaders at ADD + correct `raw_content_hash`"), the
//! document loader runs at **ingest time** and the stored artifact is the
//! extracted text (Python parity, `ingest_data.py:103`). The image and audio
//! loaders extract via vision / Whisper and therefore require an LLM /
//! transcriber handle that `AddPipeline` does not currently carry, so they are
//! NOT part of `LoaderRegistry::default_registry()`.
//!
//! Consequently, adding an image/audio input now surfaces a clear error rather
//! than silently storing raw bytes with image/audio metadata (the old, buggy
//! behaviour). These tests assert that contract. Wiring an LLM-backed
//! image/audio loader into the ADD path (so the extracted text is stored, as
//! Python does) is a residual parity gap tracked separately.
//!
//! `content_hash` / `data_id` (derived from the raw bytes) are unaffected and
//! still match Python; only the stored artifact + extraction differ.

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

fn sqlite_db_url(dir: &TempDir) -> String {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file");
    format!("sqlite://{}", db_path.display())
}

async fn make_pipeline(
    dir: &TempDir,
    db_url: &str,
) -> (
    AddPipeline,
    Arc<cognee_database::DatabaseConnection>,
    Arc<LocalStorage>,
) {
    let db = connect(db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let db = Arc::new(db);
    let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");
    let pipeline = AddPipeline::new(
        storage.clone() as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn IngestDb>,
    )
    .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
    .with_graph_db(Arc::new(MockGraphDB::new()))
    .with_vector_db(Arc::new(MockVectorDB::new()))
    .with_database(Arc::clone(&db));
    (pipeline, db, storage)
}

// ---------------------------------------------------------------------------
// Image add — no image loader registered at ADD → clear error (no raw bytes)
// ---------------------------------------------------------------------------

async fn impl_add_image_errors_without_loader(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let result = pipeline
        .add(
            vec![DataInput::Binary {
                data: b"\x89PNG\r\n\x1a\nfake-png-payload".to_vec(),
                name: "photo.png".to_string(),
            }],
            "images",
            owner,
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "image add must error when no image loader is registered (must not store raw bytes)"
    );
}

#[tokio::test]
async fn add_image_errors_without_loader_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_image_errors_without_loader(&url).await;
}

#[tokio::test]
async fn add_image_errors_without_loader_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_image_errors_without_loader(&url).await;
}

// ---------------------------------------------------------------------------
// Audio add — no audio loader registered at ADD → clear error (no raw bytes)
// ---------------------------------------------------------------------------

async fn impl_add_audio_errors_without_loader(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let result = pipeline
        .add(
            vec![DataInput::Binary {
                data: b"ID3\x04\x00\x00\x00fake-mp3-payload".to_vec(),
                name: "speech.mp3".to_string(),
            }],
            "audio",
            owner,
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "audio add must error when no audio loader is registered (must not store raw bytes)"
    );
}

#[tokio::test]
async fn add_audio_errors_without_loader_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_audio_errors_without_loader(&url).await;
}

#[tokio::test]
async fn add_audio_errors_without_loader_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_audio_errors_without_loader(&url).await;
}
