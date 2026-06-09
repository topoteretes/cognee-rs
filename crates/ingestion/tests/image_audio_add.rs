//! Integration tests for adding image and audio files via `AddPipeline`.
//!
//! These tests verify that the `add` operation stores image and audio inputs
//! with the correct metadata fields (`extension`, `mime_type`, `loader_engine`,
//! `content_hash`) — the same fields the Python SDK produces.  LLM-based
//! content extraction (vision / Whisper) happens during `cognify`, not `add`,
//! so no mocked LLM is needed here.
//!
//! Each test is instantiated twice: SQLite (always runs) and PostgreSQL
//! (skipped when `DB_PROVIDER` is not configured in the environment).

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
// Image add — metadata
// ---------------------------------------------------------------------------

async fn impl_add_image_records_correct_metadata(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    // Minimal PNG header bytes — content doesn't need to be a valid image;
    // the add pipeline stores raw bytes and derives metadata from the name.
    let results = pipeline
        .add(
            vec![DataInput::Binary {
                data: b"\x89PNG\r\n\x1a\nfake-png-payload".to_vec(),
                name: "photo.png".to_string(),
            }],
            "images",
            owner,
            None,
        )
        .await
        .expect("add image");

    assert_eq!(results.len(), 1);
    let data = &results[0];
    assert_eq!(data.extension, "png");
    assert_eq!(data.mime_type, "image/png");
    assert_eq!(
        data.loader_engine.as_deref(),
        Some("image_loader"),
        "loader_engine must match Python SDK for cross-SDK DB compatibility"
    );
    assert!(!data.content_hash.is_empty(), "content_hash must be set");
    assert!(data.data_size > 0, "data_size must be positive");
}

#[tokio::test]
async fn add_image_records_correct_metadata_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_image_records_correct_metadata(&url).await;
}

#[tokio::test]
async fn add_image_records_correct_metadata_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_image_records_correct_metadata(&url).await;
}

// ---------------------------------------------------------------------------
// Audio add — metadata
// ---------------------------------------------------------------------------

async fn impl_add_audio_records_correct_metadata(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let results = pipeline
        .add(
            vec![DataInput::Binary {
                data: b"ID3\x04\x00\x00\x00fake-mp3-payload".to_vec(),
                name: "speech.mp3".to_string(),
            }],
            "audio",
            owner,
            None,
        )
        .await
        .expect("add audio");

    assert_eq!(results.len(), 1);
    let data = &results[0];
    assert_eq!(data.extension, "mp3");
    assert_eq!(data.mime_type, "audio/mpeg");
    assert_eq!(
        data.loader_engine.as_deref(),
        Some("audio_loader"),
        "loader_engine must match Python SDK for cross-SDK DB compatibility"
    );
    assert!(!data.content_hash.is_empty(), "content_hash must be set");
    assert!(data.data_size > 0, "data_size must be positive");
}

#[tokio::test]
async fn add_audio_records_correct_metadata_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_audio_records_correct_metadata(&url).await;
}

#[tokio::test]
async fn add_audio_records_correct_metadata_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_audio_records_correct_metadata(&url).await;
}

// ---------------------------------------------------------------------------
// Image deduplication — identical bytes, same owner → same data_id
// ---------------------------------------------------------------------------

async fn impl_add_image_deduplicates_identical_bytes(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let bytes = b"\x89PNG\r\n\x1a\nidentical-png-content".to_vec();

    let r1 = pipeline
        .add(
            vec![DataInput::Binary {
                data: bytes.clone(),
                name: "image_a.png".to_string(),
            }],
            "dataset_a",
            owner,
            None,
        )
        .await
        .expect("add image 1");

    let r2 = pipeline
        .add(
            vec![DataInput::Binary {
                data: bytes,
                name: "image_b.png".to_string(),
            }],
            "dataset_b",
            owner,
            None,
        )
        .await
        .expect("add image 2");

    assert_eq!(
        r1[0].content_hash, r2[0].content_hash,
        "identical bytes must produce identical content_hash"
    );
    assert_eq!(
        r1[0].id, r2[0].id,
        "identical bytes + same owner must produce identical data_id (content-addressed dedup)"
    );
}

#[tokio::test]
async fn add_image_deduplicates_identical_bytes_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_image_deduplicates_identical_bytes(&url).await;
}

#[tokio::test]
async fn add_image_deduplicates_identical_bytes_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_image_deduplicates_identical_bytes(&url).await;
}

// ---------------------------------------------------------------------------
// Audio deduplication — identical bytes, same owner → same data_id
// ---------------------------------------------------------------------------

async fn impl_add_audio_deduplicates_identical_bytes(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let bytes = b"RIFF\x24\x00\x00\x00WAVEfmt identical-wav-content".to_vec();

    let r1 = pipeline
        .add(
            vec![DataInput::Binary {
                data: bytes.clone(),
                name: "track_v1.wav".to_string(),
            }],
            "audio_ds1",
            owner,
            None,
        )
        .await
        .expect("add audio 1");

    let r2 = pipeline
        .add(
            vec![DataInput::Binary {
                data: bytes,
                name: "track_v2.wav".to_string(),
            }],
            "audio_ds2",
            owner,
            None,
        )
        .await
        .expect("add audio 2");

    assert_eq!(
        r1[0].id, r2[0].id,
        "identical audio bytes + same owner must produce identical data_id"
    );
}

#[tokio::test]
async fn add_audio_deduplicates_identical_bytes_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_audio_deduplicates_identical_bytes(&url).await;
}

#[tokio::test]
async fn add_audio_deduplicates_identical_bytes_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_audio_deduplicates_identical_bytes(&url).await;
}

// ---------------------------------------------------------------------------
// Mixed add — image + audio in a single call
// ---------------------------------------------------------------------------

async fn impl_add_image_and_audio_together(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();

    let results = pipeline
        .add(
            vec![
                DataInput::Binary {
                    data: b"\xff\xd8\xff\xe0fake-jpeg-bytes".to_vec(),
                    name: "photo.jpg".to_string(),
                },
                DataInput::Binary {
                    data: b"RIFF\x00\x00\x00\x00WAVEfake-wav-bytes".to_vec(),
                    name: "clip.wav".to_string(),
                },
            ],
            "mixed",
            owner,
            None,
        )
        .await
        .expect("add mixed image+audio");

    assert_eq!(results.len(), 2);

    let img = results
        .iter()
        .find(|d| d.extension == "jpg")
        .expect("jpeg result");
    let aud = results
        .iter()
        .find(|d| d.extension == "wav")
        .expect("wav result");

    assert_eq!(img.loader_engine.as_deref(), Some("image_loader"));
    assert_eq!(aud.loader_engine.as_deref(), Some("audio_loader"));

    assert_ne!(
        img.id, aud.id,
        "different content must produce different data_ids"
    );
}

#[tokio::test]
async fn add_image_and_audio_together_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_image_and_audio_together(&url).await;
}

#[tokio::test]
async fn add_image_and_audio_together_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_image_and_audio_together(&url).await;
}
