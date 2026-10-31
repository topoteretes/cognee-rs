#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for adding image and audio files via `AddPipeline`.
//!
//! As of task 17 ("Run loaders at ADD + correct `raw_content_hash`"), the
//! document loader runs at **ingest time** and the stored artifact is the
//! extracted text (Python parity, `ingest_data.py:103`). The image and audio
//! loaders extract via vision / Whisper and therefore need an LLM /
//! transcriber handle, so they are NOT part of
//! `LoaderRegistry::default_registry()`.
//!
//! Audio: with `AddPipeline::with_transcriber` the audio loader runs at add
//! and the transcript is stored, as Python does. Without one, and for images
//! (no LLM handle on the add path yet), adding surfaces a clear error rather
//! than silently storing raw bytes with media metadata (the old, buggy
//! behaviour). These tests assert both contracts.
//!
//! `content_hash` / `data_id` (derived from the raw bytes) are unaffected and
//! still match Python; only the stored artifact + extraction differ.

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
#[cfg(feature = "audio-loader")]
use cognee_ingestion::HashAlgorithm;
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
    .with_database(db.clone());
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

// ---------------------------------------------------------------------------
// Audio add with a transcriber: the transcript is stored as text
// ---------------------------------------------------------------------------

#[cfg(feature = "audio-loader")]
fn transcriber(text: &str) -> Arc<dyn cognee_llm::Transcriber> {
    Arc::new(cognee_test_utils::MockTranscriber::new(
        "mock-whisper",
        vec![cognee_llm::TranscriptionOutput {
            text: text.to_string(),
            language: Some("english".to_string()),
            duration: Some(1.5),
        }],
    ))
}

#[cfg(feature = "audio-loader")]
async fn impl_add_audio_stores_transcript(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, storage) = make_pipeline(&dir, db_url).await;
    let pipeline = pipeline.with_transcriber(transcriber("Hello from the meeting."));
    let owner = Uuid::new_v4();
    let audio = b"ID3\x04\x00\x00\x00fake-mp3-payload".to_vec();

    let data = pipeline
        .add(
            vec![DataInput::Binary {
                data: audio.clone(),
                name: "speech.mp3".to_string(),
            }],
            "audio",
            owner,
            None,
        )
        .await
        .expect("audio add with a transcriber must succeed");

    assert_eq!(data.len(), 1);
    let data = &data[0];
    assert_eq!(data.loader_engine.as_deref(), Some("audio_loader"));
    assert_eq!(data.extension, "txt");
    assert_eq!(data.mime_type, "text/plain");
    assert_eq!(data.original_extension.as_deref(), Some("mp3"));
    assert_eq!(
        data.content_hash,
        cognee_ingestion::ContentHasher::hash_content(&audio, HashAlgorithm::Md5),
        "content_hash stays the hash of the raw audio bytes"
    );
    assert_eq!(
        storage.retrieve(&data.raw_data_location).await.unwrap(),
        b"Hello from the meeting."
    );
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn add_audio_stores_transcript_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_add_audio_stores_transcript(&url).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn add_audio_stores_transcript_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_add_audio_stores_transcript(&url).await;
}

/// Re-adding the same audio reuses the stored transcript: no second (paid)
/// transcription, whose text could differ, and no second stored file.
#[cfg(feature = "audio-loader")]
async fn impl_re_adding_audio_transcribes_it_once(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, storage) = make_pipeline(&dir, db_url).await;
    let whisper = Arc::new(cognee_test_utils::MockTranscriber::new(
        "mock-whisper",
        ["first take", "second take"]
            .into_iter()
            .map(|text| cognee_llm::TranscriptionOutput {
                text: text.to_string(),
                language: None,
                duration: None,
            })
            .collect(),
    ));
    let pipeline = pipeline.with_transcriber(whisper.clone());
    let owner = Uuid::new_v4();
    let add = || {
        pipeline.add(
            vec![DataInput::Binary {
                data: b"ID3\x04\x00\x00\x00fake-mp3-payload".to_vec(),
                name: "speech.mp3".to_string(),
            }],
            "audio",
            owner,
            None,
        )
    };

    let first = add().await.expect("first add").remove(0);
    let again = add().await.expect("re-add").remove(0);

    assert_eq!(again.id, first.id);
    assert_eq!(again.raw_data_location, first.raw_data_location);
    assert_eq!(again.raw_content_hash, first.raw_content_hash);
    assert_eq!(
        storage.retrieve(&again.raw_data_location).await.unwrap(),
        b"first take"
    );
    assert_eq!(whisper.calls(), 1, "the re-add must not transcribe again");
    let stored_files = walkdir(&dir.path().join("storage"));
    assert_eq!(stored_files.len(), 1, "stored files: {stored_files:?}");
}

#[cfg(feature = "audio-loader")]
fn queued(texts: &[&str]) -> Arc<cognee_test_utils::MockTranscriber> {
    Arc::new(cognee_test_utils::MockTranscriber::new(
        "mock-whisper",
        texts
            .iter()
            .map(|text| cognee_llm::TranscriptionOutput {
                text: text.to_string(),
                language: None,
                duration: None,
            })
            .collect(),
    ))
}

#[cfg(feature = "audio-loader")]
fn speech() -> Vec<DataInput> {
    vec![DataInput::Binary {
        data: b"ID3\x04\x00\x00\x00fake-mp3-payload".to_vec(),
        name: "speech.mp3".to_string(),
    }]
}

#[cfg(feature = "audio-loader")]
fn stored_path(location: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(location.strip_prefix("file://").unwrap_or(location))
}

/// Audio transcribed before is reused even when no transcriber is attached now,
/// as when the first add had an API key and a later one does not.
#[cfg(feature = "audio-loader")]
async fn impl_re_adding_audio_without_a_transcriber_reuses_it(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, storage) = make_pipeline(&dir, db_url).await;
    let whisper = queued(&["Hello from the meeting."]);
    let owner = Uuid::new_v4();
    let first = pipeline
        .with_transcriber(whisper.clone())
        .add(speech(), "audio", owner, None)
        .await
        .expect("first add")
        .remove(0);

    let (keyless, _db, _storage) = make_pipeline(&dir, db_url).await;
    let again = keyless
        .add(speech(), "other", owner, None)
        .await
        .expect("a re-add needs no transcriber")
        .remove(0);

    assert_eq!(again.id, first.id);
    assert_eq!(again.raw_data_location, first.raw_data_location);
    assert_eq!(
        storage.retrieve(&again.raw_data_location).await.unwrap(),
        b"Hello from the meeting."
    );
    assert_eq!(whisper.calls(), 1);
}

/// When the stored transcript is gone, a re-add transcribes once more and points
/// the row at the new file; the next re-add reuses it.
#[cfg(feature = "audio-loader")]
async fn impl_re_adding_audio_whose_text_is_gone_repairs_the_row(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, db, storage) = make_pipeline(&dir, db_url).await;
    let whisper = queued(&["first take", "second take", "third take"]);
    let pipeline = pipeline.with_transcriber(whisper.clone());
    let owner = Uuid::new_v4();
    let first = pipeline
        .add(speech(), "audio", owner, None)
        .await
        .expect("first add")
        .remove(0);
    std::fs::remove_file(stored_path(&first.raw_data_location)).expect("remove the transcript");

    let repaired = pipeline
        .add(speech(), "audio", owner, None)
        .await
        .expect("re-add")
        .remove(0);
    assert_eq!(whisper.calls(), 2);
    assert_ne!(repaired.raw_data_location, first.raw_data_location);
    assert_eq!(
        storage.retrieve(&repaired.raw_data_location).await.unwrap(),
        b"second take"
    );
    let row = IngestDb::get_data(&*db, first.id)
        .await
        .unwrap()
        .expect("the row");
    assert_eq!(row.raw_data_location, repaired.raw_data_location);
    assert_eq!(
        row.raw_content_hash.as_deref(),
        Some(
            cognee_ingestion::ContentHasher::hash_content(b"second take", HashAlgorithm::Md5)
                .as_str()
        )
    );
    assert_eq!(row.extension, "txt");
    assert_eq!(row.loader_engine.as_deref(), Some("audio_loader"));

    pipeline
        .add(speech(), "audio", owner, None)
        .await
        .expect("third add");
    assert_eq!(whisper.calls(), 2, "the repaired row is reused");
    let stored_files = walkdir(&dir.path().join("storage"));
    assert_eq!(stored_files.len(), 1, "stored files: {stored_files:?}");
}

/// With the transcript gone and no transcriber there is nothing to reuse or
/// repair, so the re-add fails as a first add would.
#[cfg(feature = "audio-loader")]
async fn impl_re_adding_lost_audio_without_a_transcriber_fails(db_url: &str) {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _db, _storage) = make_pipeline(&dir, db_url).await;
    let owner = Uuid::new_v4();
    let first = pipeline
        .with_transcriber(queued(&["take"]))
        .add(speech(), "audio", owner, None)
        .await
        .expect("first add")
        .remove(0);
    std::fs::remove_file(stored_path(&first.raw_data_location)).expect("remove the transcript");

    let (keyless, _db, _storage) = make_pipeline(&dir, db_url).await;
    let error = keyless
        .add(speech(), "audio", owner, None)
        .await
        .expect_err("nothing to reuse");
    assert!(
        error
            .to_string()
            .contains("Unsupported document type at ingest: audio"),
        "unexpected error: {error}"
    );
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_without_a_transcriber_reuses_it_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    impl_re_adding_audio_without_a_transcriber_reuses_it(&sqlite_db_url(&dir)).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_without_a_transcriber_reuses_it_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_re_adding_audio_without_a_transcriber_reuses_it(&url).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_whose_text_is_gone_repairs_the_row_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    impl_re_adding_audio_whose_text_is_gone_repairs_the_row(&sqlite_db_url(&dir)).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_whose_text_is_gone_repairs_the_row_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_re_adding_audio_whose_text_is_gone_repairs_the_row(&url).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_lost_audio_without_a_transcriber_fails_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    impl_re_adding_lost_audio_without_a_transcriber_fails(&sqlite_db_url(&dir)).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_lost_audio_without_a_transcriber_fails_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_re_adding_lost_audio_without_a_transcriber_fails(&url).await;
}

#[cfg(feature = "audio-loader")]
fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root).expect("read storage dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            files.extend(walkdir(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_transcribes_it_once_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    impl_re_adding_audio_transcribes_it_once(&url).await;
}

#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn re_adding_audio_transcribes_it_once_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_re_adding_audio_transcribes_it_once(&url).await;
}

/// `aac` is classified as audio but Whisper does not accept it, so the
/// loader refuses it before any transcription call.
#[cfg(feature = "audio-loader")]
#[tokio::test]
async fn add_audio_rejects_format_whisper_does_not_accept() {
    let dir = TempDir::new().expect("tempdir");
    let url = sqlite_db_url(&dir);
    let (pipeline, _db, _storage) = make_pipeline(&dir, &url).await;
    let pipeline = pipeline.with_transcriber(transcriber("never returned"));

    let error = pipeline
        .add(
            vec![DataInput::Binary {
                data: b"fake-aac-payload".to_vec(),
                name: "speech.aac".to_string(),
            }],
            "audio",
            Uuid::new_v4(),
            None,
        )
        .await
        .expect_err("aac must be refused");

    assert!(
        error.to_string().contains("aac"),
        "unexpected error: {error}"
    );
}
