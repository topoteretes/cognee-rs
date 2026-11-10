#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Re-adding audio reuses the transcript an earlier add stored.
//!
//! The audio loader calls a paid model at `add`, and running it again can
//! change the text. A re-add of the same content into the same dataset finds
//! the row `persist_data` deduplicates against, and reuses its transcript when
//! the audio loader wrote it and it is still in storage. Otherwise the audio is
//! transcribed again and the re-add points the row at the new transcript.

use cognee_core::RayonThreadPool;
use cognee_database::{DatabaseConnection, IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::{AddPipeline, LoaderRegistry};
use cognee_llm::TranscriptionOutput;
use cognee_models::{Data, DataInput};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockTranscriber;
use cognee_vector::MockVectorDB;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const WAV_BYTES: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt fake-wav-payload";

struct Setup {
    dir: TempDir,
    db: Arc<DatabaseConnection>,
    storage: Arc<LocalStorage>,
}

impl Setup {
    async fn new(db_url: Option<String>) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let db_url = db_url.unwrap_or_else(|| {
            let path = dir.path().join("cognee.db");
            std::fs::File::create(&path).expect("sqlite db file");
            format!("sqlite://{}", path.display())
        });
        let db = connect(&db_url).await.expect("connect");
        initialize(&db).await.expect("initialize");
        let storage = Arc::new(LocalStorage::new(dir.path().join("storage")));
        storage.initialize().await.expect("storage.initialize");
        Self {
            dir,
            db: Arc::new(db),
            storage,
        }
    }

    /// An add pipeline with the default loaders, plus the audio loader on
    /// `transcriber` when there is one.
    fn pipeline(&self, transcriber: Option<Arc<MockTranscriber>>) -> AddPipeline {
        let mut registry = LoaderRegistry::default_registry();
        if let Some(transcriber) = transcriber {
            registry.register_audio_loader(transcriber);
        }
        AddPipeline::new(
            self.storage.clone() as Arc<dyn StorageTrait>,
            self.db.clone() as Arc<dyn IngestDb>,
        )
        .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
        .with_graph_db(Arc::new(MockGraphDB::new()))
        .with_vector_db(Arc::new(MockVectorDB::new()))
        .with_database(self.db.clone())
        .with_loader_registry(Arc::new(registry))
    }

    fn speech(&self) -> Vec<DataInput> {
        let path = self.dir.path().join("speech.wav");
        std::fs::write(&path, WAV_BYTES).expect("write fixture");
        vec![DataInput::FilePath(path.display().to_string())]
    }

    async fn add(&self, pipeline: &AddPipeline, owner: Uuid) -> Data {
        pipeline
            .add(self.speech(), "meetings", owner, None)
            .await
            .expect("add")
            .remove(0)
    }

    async fn row(&self, id: Uuid) -> Data {
        IngestDb::get_data(&*self.db, id)
            .await
            .unwrap()
            .expect("the row")
    }

    async fn text(&self, data: &Data) -> Vec<u8> {
        self.storage
            .retrieve(&data.raw_data_location)
            .await
            .unwrap()
    }

    /// Every file in storage: the transcripts, since a file input's raw bytes
    /// stay at their own path.
    fn stored_files(&self) -> Vec<PathBuf> {
        walk(&self.dir.path().join("storage"))
    }
}

fn whisper(texts: &[&str]) -> Arc<MockTranscriber> {
    Arc::new(MockTranscriber::new(
        "mock-whisper",
        texts
            .iter()
            .map(|text| TranscriptionOutput {
                text: (*text).to_string(),
                language: None,
                duration: None,
            })
            .collect(),
    ))
}

fn stored_path(location: &str) -> PathBuf {
    PathBuf::from(location.strip_prefix("file://").unwrap_or(location))
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root).expect("read storage dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// A re-add reuses the stored transcript: no second paid transcription, whose
/// text could differ, and no second stored file.
async fn impl_re_adding_audio_transcribes_it_once(db_url: Option<String>) {
    let setup = Setup::new(db_url).await;
    let transcriber = whisper(&["first take", "second take"]);
    let pipeline = setup.pipeline(Some(transcriber.clone()));
    let owner = Uuid::new_v4();

    let first = setup.add(&pipeline, owner).await;
    let again = setup.add(&pipeline, owner).await;

    assert_eq!(again.id, first.id);
    assert_eq!(again.raw_data_location, first.raw_data_location);
    assert_eq!(again.raw_content_hash, first.raw_content_hash);
    assert_eq!(setup.text(&again).await, b"first take");
    assert_eq!(
        transcriber.calls(),
        1,
        "the re-add must not transcribe again"
    );
    assert_eq!(setup.stored_files().len(), 1, "{:?}", setup.stored_files());
}

/// Audio transcribed before is reused even when no transcriber is configured
/// now, as when the first add had an API key and a later one does not.
async fn impl_re_adding_audio_without_a_transcriber_reuses_it(db_url: Option<String>) {
    let setup = Setup::new(db_url).await;
    let transcriber = whisper(&["Hello from the meeting."]);
    let owner = Uuid::new_v4();
    let first = setup
        .add(&setup.pipeline(Some(transcriber.clone())), owner)
        .await;

    let again = setup.add(&setup.pipeline(None), owner).await;

    assert_eq!(again.id, first.id);
    assert_eq!(again.raw_data_location, first.raw_data_location);
    assert_eq!(setup.text(&again).await, b"Hello from the meeting.");
    assert_eq!(transcriber.calls(), 1);
}

/// When the stored transcript is gone, a re-add transcribes once more and
/// points the row at the new file; the next re-add reuses it.
async fn impl_re_adding_audio_whose_text_is_gone_repairs_the_row(db_url: Option<String>) {
    let setup = Setup::new(db_url).await;
    let transcriber = whisper(&["first take", "second take", "third take"]);
    let pipeline = setup.pipeline(Some(transcriber.clone()));
    let owner = Uuid::new_v4();
    let first = setup.add(&pipeline, owner).await;
    std::fs::remove_file(stored_path(&first.raw_data_location)).expect("remove the transcript");

    let repaired = setup.add(&pipeline, owner).await;
    assert_eq!(transcriber.calls(), 2);
    assert_ne!(repaired.raw_data_location, first.raw_data_location);
    assert_eq!(setup.text(&repaired).await, b"second take");
    let row = setup.row(first.id).await;
    assert_eq!(row.raw_data_location, repaired.raw_data_location);
    assert_eq!(row.extension, "txt");
    assert_eq!(row.loader_engine.as_deref(), Some("audio_loader"));

    setup.add(&pipeline, owner).await;
    assert_eq!(transcriber.calls(), 2, "the repaired row is reused");
}

/// A row whose stored file is not a transcript (the raw audio a URL add
/// stores) is transcribed again, and the row then holds the transcript.
async fn impl_re_adding_audio_stored_raw_transcribes_it(db_url: Option<String>) {
    let setup = Setup::new(db_url).await;
    let transcriber = whisper(&["first take", "second take", "third take"]);
    let pipeline = setup.pipeline(Some(transcriber.clone()));
    let owner = Uuid::new_v4();
    let first = setup.add(&pipeline, owner).await;

    // Turn the row into what the URL path stores: the raw wav bytes.
    let raw_location = setup.storage.store(WAV_BYTES, "speech.wav").await.unwrap();
    let mut row = setup.row(first.id).await;
    row.raw_data_location = format!(
        "file://{}",
        setup.storage.get_full_path(&raw_location).display()
    );
    row.extension = "wav".to_string();
    row.mime_type = "audio/wav".to_string();
    IngestDb::update_data(&*setup.db, row).await.unwrap();

    let repaired = setup.add(&pipeline, owner).await;
    assert_eq!(
        transcriber.calls(),
        2,
        "raw audio is not a transcript to reuse"
    );
    assert_eq!(repaired.extension, "txt");
    assert_eq!(repaired.mime_type, "text/plain");
    assert_eq!(setup.text(&repaired).await, b"second take");

    setup.add(&pipeline, owner).await;
    assert_eq!(transcriber.calls(), 2, "the repaired row is reused");
}

/// The same audio in another dataset is another row, so it is transcribed for
/// that dataset too.
async fn impl_another_dataset_transcribes_its_own_copy(db_url: Option<String>) {
    let setup = Setup::new(db_url).await;
    let transcriber = whisper(&["first take", "second take"]);
    let pipeline = setup.pipeline(Some(transcriber.clone()));
    let owner = Uuid::new_v4();
    let first = setup.add(&pipeline, owner).await;

    let other = pipeline
        .add(setup.speech(), "other", owner, None)
        .await
        .expect("add to another dataset")
        .remove(0);

    assert_ne!(other.id, first.id);
    assert_eq!(transcriber.calls(), 2);
}

macro_rules! sqlite_and_pg {
    ($impl_fn:ident, $sqlite:ident, $pg:ident) => {
        #[tokio::test]
        async fn $sqlite() {
            $impl_fn(None).await;
        }

        #[tokio::test]
        async fn $pg() {
            let Some(url) = cognee_test_utils::pg_test_url() else {
                return;
            };
            $impl_fn(Some(url)).await;
        }
    };
}

sqlite_and_pg!(
    impl_re_adding_audio_transcribes_it_once,
    re_adding_audio_transcribes_it_once_sqlite,
    re_adding_audio_transcribes_it_once_pg
);
sqlite_and_pg!(
    impl_re_adding_audio_without_a_transcriber_reuses_it,
    re_adding_audio_without_a_transcriber_reuses_it_sqlite,
    re_adding_audio_without_a_transcriber_reuses_it_pg
);
sqlite_and_pg!(
    impl_re_adding_audio_whose_text_is_gone_repairs_the_row,
    re_adding_audio_whose_text_is_gone_repairs_the_row_sqlite,
    re_adding_audio_whose_text_is_gone_repairs_the_row_pg
);
sqlite_and_pg!(
    impl_re_adding_audio_stored_raw_transcribes_it,
    re_adding_audio_stored_raw_transcribes_it_sqlite,
    re_adding_audio_stored_raw_transcribes_it_pg
);
sqlite_and_pg!(
    impl_another_dataset_transcribes_its_own_copy,
    another_dataset_transcribes_its_own_copy_sqlite,
    another_dataset_transcribes_its_own_copy_pg
);
