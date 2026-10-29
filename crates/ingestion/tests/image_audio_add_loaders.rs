#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `add` of image and audio files with the image and audio loaders registered
//! (SDK-1039 P15).
//!
//! Python runs its loaders at `add` (`cognee/tasks/ingestion/ingest_data.py`,
//! `loaders/store_derived_text.py`, `loaders/core/image_loader.py`), and
//! Python 1d3ed15 stores, for a PNG or a WAV:
//! - `raw_data_location`: the derived text `text_<md5(raw)>.txt`, with
//!   `extension` `txt` and `mime_type` `text/plain`;
//! - `original_data_location`: the raw file, with the original extension and
//!   MIME type;
//! - `content_hash` = MD5 of the raw bytes, `raw_content_hash` = MD5 of the
//!   stored text, `loader_engine` = `image_loader` / `audio_loader`.
//!
//! These tests pin the same contract on the Rust side. Without these loaders
//! the add fails instead (see `image_audio_add.rs`).

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::{AddPipeline, LoaderRegistry};
use cognee_llm::TranscriptionOutput;
use cognee_models::{Data, DataInput};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::{MockLlm, MockTranscriber};
use cognee_vector::MockVectorDB;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nfake-png-payload";
const WAV_BYTES: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt fake-wav-payload";
const DESCRIPTION: &str = "The image is a solid red square.";
const TRANSCRIPT: &str = "Hello from the recording.";

fn md5_hex(bytes: &[u8]) -> String {
    cognee_ingestion::ContentHasher::hash_content(bytes, cognee_ingestion::HashAlgorithm::Md5)
}

/// A registry with the default loaders plus image and audio loaders whose
/// mocks answer `responses` times.
fn media_registry(responses: usize) -> Arc<LoaderRegistry> {
    let mut registry = LoaderRegistry::default_registry();
    registry.register_image_loader(Arc::new(
        MockLlm::empty().with_vision_responses(vec![DESCRIPTION.to_string(); responses]),
    ));
    registry.register_audio_loader(Arc::new(MockTranscriber::new(
        "mock-whisper",
        vec![
            TranscriptionOutput {
                text: TRANSCRIPT.to_string(),
                language: None,
                duration: None,
            };
            responses
        ],
    )));
    Arc::new(registry)
}

async fn make_pipeline(dir: &TempDir, responses: usize) -> (AddPipeline, Arc<LocalStorage>) {
    let db_path = dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("sqlite db file");
    let db = connect(&format!("sqlite://{}", db_path.display()))
        .await
        .expect("connect");
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
    .with_database(db)
    .with_loader_registry(media_registry(responses));
    (pipeline, storage)
}

/// Write `bytes` to `<dir>/<name>` and return it as a file-path input.
fn file_input(dir: &TempDir, name: &str, bytes: &[u8]) -> DataInput {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    DataInput::FilePath(path.display().to_string())
}

/// Assert the Python 1d3ed15 `add` contract for one media file.
async fn assert_python_media_row(
    storage: &LocalStorage,
    data: &Data,
    raw: &[u8],
    text: &str,
    original_extension: &str,
    original_mime_type: &str,
    loader_engine: &str,
) {
    let content_hash = md5_hex(raw);
    assert_eq!(data.content_hash, content_hash, "content_hash = md5(raw)");
    assert_eq!(data.extension, "txt");
    assert_eq!(data.mime_type, "text/plain");
    assert_eq!(data.loader_engine.as_deref(), Some(loader_engine));
    assert_eq!(data.original_extension.as_deref(), Some(original_extension));
    assert_eq!(data.original_mime_type.as_deref(), Some(original_mime_type));
    assert!(
        data.raw_data_location
            .ends_with(&format!("text_{content_hash}.txt")),
        "raw_data_location {:?} must be the derived text_<md5(raw)>.txt",
        data.raw_data_location
    );

    let stored = storage
        .retrieve(&data.raw_data_location)
        .await
        .expect("stored derived text");
    assert_eq!(
        stored,
        text.as_bytes(),
        "the stored file is the derived text"
    );
    assert_eq!(
        data.raw_content_hash.as_deref(),
        Some(md5_hex(text.as_bytes()).as_str()),
        "raw_content_hash = md5(stored text)"
    );

    let original_path = data
        .original_data_location
        .strip_prefix("file://")
        .unwrap_or(&data.original_data_location);
    assert_eq!(
        std::fs::read(original_path).expect("original file"),
        raw,
        "original_data_location holds the raw file"
    );
}

#[tokio::test]
async fn add_image_stores_description_as_python_does() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, storage) = make_pipeline(&dir, 1).await;

    let rows = pipeline
        .add(
            vec![file_input(&dir, "photo.png", PNG_BYTES)],
            "images",
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("image add succeeds with an image loader");

    assert_eq!(rows.len(), 1);
    assert_python_media_row(
        &storage,
        &rows[0],
        PNG_BYTES,
        DESCRIPTION,
        "png",
        "image/png",
        "image_loader",
    )
    .await;
}

#[tokio::test]
async fn add_audio_stores_transcript_as_python_does() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, storage) = make_pipeline(&dir, 1).await;

    let rows = pipeline
        .add(
            vec![file_input(&dir, "speech.wav", WAV_BYTES)],
            "audio",
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("audio add succeeds with an audio loader");

    assert_eq!(rows.len(), 1);
    let original_mime = rows[0].original_mime_type.clone().unwrap_or_default();
    assert!(
        original_mime.starts_with("audio/"),
        "original_mime_type {original_mime:?} must be an audio type"
    );
    assert_python_media_row(
        &storage,
        &rows[0],
        WAV_BYTES,
        TRANSCRIPT,
        "wav",
        &original_mime,
        "audio_loader",
    )
    .await;
}

/// Python's image loader also claims `jpeg`, `jpe` and `tiff`, so those are
/// described too rather than stored as a raw blob.
#[tokio::test]
async fn add_jpeg_and_tiff_use_the_image_loader() {
    for (name, mime) in [
        ("photo.jpeg", "image/jpeg"),
        ("photo.jpe", "image/jpeg"),
        ("scan.tiff", "image/tiff"),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let (pipeline, storage) = make_pipeline(&dir, 1).await;
        let rows = pipeline
            .add(
                vec![file_input(&dir, name, PNG_BYTES)],
                "images",
                Uuid::new_v4(),
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{name}: add failed: {e}"));
        assert_eq!(rows.len(), 1);
        let ext = name.rsplit('.').next().expect("extension");
        assert_python_media_row(
            &storage,
            &rows[0],
            PNG_BYTES,
            DESCRIPTION,
            ext,
            mime,
            "image_loader",
        )
        .await;
    }
}

/// The same image under two names is one `data` row, as the lane's
/// `test_add_image_deduplication_within_rust` expects.
#[tokio::test]
async fn add_same_image_twice_dedups() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _storage) = make_pipeline(&dir, 2).await;
    let owner = Uuid::new_v4();

    let first = pipeline
        .add(
            vec![file_input(&dir, "photo_a.png", PNG_BYTES)],
            "dedup",
            owner,
            None,
        )
        .await
        .expect("first add");
    let second = pipeline
        .add(
            vec![file_input(&dir, "photo_b.png", PNG_BYTES)],
            "dedup",
            owner,
            None,
        )
        .await
        .expect("second add");

    assert_eq!(first[0].id, second[0].id, "the second add reuses the row");
}

/// The same audio file under two names is one `data` row.
#[tokio::test]
async fn add_same_audio_twice_dedups() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _storage) = make_pipeline(&dir, 2).await;
    let owner = Uuid::new_v4();

    let first = pipeline
        .add(
            vec![file_input(&dir, "track_a.wav", WAV_BYTES)],
            "dedup",
            owner,
            None,
        )
        .await
        .expect("first add");
    let second = pipeline
        .add(
            vec![file_input(&dir, "track_b.wav", WAV_BYTES)],
            "dedup",
            owner,
            None,
        )
        .await
        .expect("second add");

    assert_eq!(first[0].id, second[0].id, "the second add reuses the row");
}
