#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `add` routes loaders by sniffed content type, as Python does
//! (SDK-1039 P20; `LoaderEngine.get_loader`, `guess_file_type.py`).
//!
//! These run with the default loaders (text only). The image cases, which need
//! an image loader, are in `image_audio_add_loaders.rs`.

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::{Data, DataInput};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nfake-png-payload";

async fn make_pipeline(dir: &TempDir) -> (AddPipeline, Arc<LocalStorage>) {
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
    .with_database(db);
    (pipeline, storage)
}

fn file_input(dir: &TempDir, name: &str, bytes: &[u8]) -> DataInput {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    DataInput::FilePath(path.display().to_string())
}

async fn add_one(name: &str, bytes: &[u8]) -> Result<Data, String> {
    let dir = TempDir::new().expect("tempdir");
    let (pipeline, _storage) = make_pipeline(&dir).await;
    let mut rows = pipeline
        .add(
            vec![file_input(&dir, name, bytes)],
            "routing",
            Uuid::new_v4(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.remove(0))
}

/// UTF-8 text named `.conf` goes to `text_loader` and the row says `txt`.
#[tokio::test]
async fn utf8_text_with_an_unknown_extension_is_text() {
    let row = add_one("notes.conf", b"key = value\n").await.unwrap();
    assert_eq!(row.loader_engine.as_deref(), Some("text_loader"));
    assert_eq!(row.original_extension.as_deref(), Some("txt"));
    assert_eq!(row.original_mime_type.as_deref(), Some("text/plain"));
    assert_eq!(row.extension, "txt");
    assert_eq!(row.mime_type, "text/plain");
}

/// No extension at all is the same.
#[tokio::test]
async fn utf8_text_without_an_extension_is_text() {
    let row = add_one("notes", b"hello\n").await.unwrap();
    assert_eq!(row.loader_engine.as_deref(), Some("text_loader"));
    assert_eq!(row.original_extension.as_deref(), Some("txt"));
}

/// `.md` and `.json` are not sniffed: the extension decides the type.
#[tokio::test]
async fn short_circuit_extensions_record_their_own_type() {
    let row = add_one("a.md", b"# title\n").await.unwrap();
    assert_eq!(row.original_extension.as_deref(), Some("md"));
    assert_eq!(row.original_mime_type.as_deref(), Some("text/markdown"));
    let row = add_one("a.json", b"{\"a\": 1}").await.unwrap();
    assert_eq!(row.original_extension.as_deref(), Some("json"));
    assert_eq!(row.original_mime_type.as_deref(), Some("application/json"));
    let row = add_one("a.yml", b"a: 1\n").await.unwrap();
    assert_eq!(row.original_extension.as_deref(), Some("yaml"));
    assert_eq!(row.original_mime_type.as_deref(), Some("application/yaml"));
}

/// D15: non-UTF-8 bytes named `.txt` fail the add (Python:
/// `UnreadableFileContentError`), instead of being stored as a blob.
#[tokio::test]
async fn non_utf8_bytes_named_txt_fail() {
    let err = add_one("image.txt", PNG_BYTES).await.unwrap_err();
    assert!(
        err.to_lowercase().contains("utf"),
        "unexpected error: {err}"
    );
}

/// Binary content the sniffer knows but no loader claims fails the add
/// (Python: "No loader found").
#[tokio::test]
async fn a_binary_no_loader_claims_fails() {
    let err = add_one("archive.zip", b"PK\x03\x04\x00\x00\x00\x00")
        .await
        .unwrap_err();
    assert!(err.contains("No loader found"), "unexpected error: {err}");
}

/// A PNG without an image loader fails, whatever its name.
#[tokio::test]
async fn a_png_without_an_image_loader_fails_by_content() {
    for name in ["image", "image.jpg"] {
        let err = add_one(name, PNG_BYTES).await.unwrap_err();
        assert!(err.contains("image"), "{name}: unexpected error: {err}");
    }
}

/// A `.csv` is not sniffed.
#[cfg(feature = "csv-loader")]
#[tokio::test]
async fn csv_is_routed_without_sniffing() {
    let row = add_one("a.csv", b"a,b\n1,2\n").await.unwrap();
    assert_eq!(row.loader_engine.as_deref(), Some("csv_loader"));
    assert_eq!(row.original_extension.as_deref(), Some("csv"));
    assert_eq!(row.original_mime_type.as_deref(), Some("text/csv"));
}
