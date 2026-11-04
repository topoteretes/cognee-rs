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
const JPEG_BYTES: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00fake-jpeg-payload";
const TIFF_BYTES: &[u8] = b"II*\x00\x08\x00\x00\x00fake-tiff-payload";
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
    // Python's `filetype` names a WAV `audio/x-wav`.
    assert_python_media_row(
        &storage,
        &rows[0],
        WAV_BYTES,
        TRANSCRIPT,
        "wav",
        "audio/x-wav",
        "audio_loader",
    )
    .await;
}

/// SDK-1039 P20: `infer` calls an M4A `audio/m4a`, which is not in
/// `audio_loader`'s MIME list; Python's `filetype` calls it `audio/mp4`, which
/// is. The sniffed type is translated, so an `.m4a` is transcribed and the row
/// says `m4a` / `audio/mp4`, as in Python.
#[tokio::test]
async fn add_m4a_uses_the_audio_loader_with_python_mime() {
    let mut m4a = b"\0\0\0\x20ftypM4A \0\0\0\0M4A mp42isom".to_vec();
    m4a.resize(64, 0);
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, storage) = make_pipeline(&dir, 1).await;
    let rows = pipeline
        .add(
            vec![file_input(&dir, "voice.m4a", &m4a)],
            "audio",
            Uuid::new_v4(),
            None,
        )
        .await
        .expect("m4a add succeeds with an audio loader");
    assert_python_media_row(
        &storage,
        &rows[0],
        &m4a,
        TRANSCRIPT,
        "m4a",
        "audio/mp4",
        "audio_loader",
    )
    .await;
}

/// Python's image loader also claims `jpeg`, `jpe` and `tiff`, so those are
/// described too rather than stored as a raw blob. The row records what the
/// bytes are (`filetype` extension and MIME type), not the file name's.
#[tokio::test]
async fn add_jpeg_and_tiff_use_the_image_loader() {
    for (name, bytes, ext, mime) in [
        ("photo.jpeg", JPEG_BYTES, "jpg", "image/jpeg"),
        ("photo.jpe", JPEG_BYTES, "jpg", "image/jpeg"),
        ("scan.tiff", TIFF_BYTES, "tif", "image/tiff"),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let (pipeline, storage) = make_pipeline(&dir, 1).await;
        let rows = pipeline
            .add(
                vec![file_input(&dir, name, bytes)],
                "images",
                Uuid::new_v4(),
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{name}: add failed: {e}"));
        assert_eq!(rows.len(), 1);
        assert_python_media_row(
            &storage,
            &rows[0],
            bytes,
            DESCRIPTION,
            ext,
            mime,
            "image_loader",
        )
        .await;
    }
}

/// SDK-1039 P20: Python routes by the sniffed type, so a PNG named `.jpg`
/// goes to the image loader and the row says `png` / `image/png`.
#[tokio::test]
async fn add_routes_images_by_content_not_by_name() {
    for name in ["image.jpg", "image.PNG"] {
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
}

/// An image whose name has no image extension is routed to the image loader
/// and then fails, as in Python, whose vision call takes the MIME type from
/// the name (`native_adapter.py:633-638`).
#[tokio::test]
async fn add_image_without_an_image_extension_fails_in_the_image_loader() {
    for name in ["image", "image.bin"] {
        let dir = TempDir::new().expect("tempdir");
        let (pipeline, _storage) = make_pipeline(&dir, 1).await;
        let err = pipeline
            .add(
                vec![file_input(&dir, name, PNG_BYTES)],
                "images",
                Uuid::new_v4(),
                None,
            )
            .await
            .expect_err("no image extension must fail");
        assert!(
            err.to_string().contains("Could not determine MIME type"),
            "{name}: unexpected error: {err}"
        );
    }
}

/// A PNG named `.txt` is read as text, which fails (Python:
/// `UnreadableFileContentError`).
#[tokio::test]
async fn add_png_named_txt_fails() {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _storage) = make_pipeline(&dir, 1).await;
    let result = pipeline
        .add(
            vec![file_input(&dir, "image.txt", PNG_BYTES)],
            "images",
            Uuid::new_v4(),
            None,
        )
        .await;
    assert!(result.is_err(), "a PNG named .txt must fail the add");
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
