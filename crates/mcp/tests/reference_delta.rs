use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cognee_mcp::atomic_fs::SyncOps;
use cognee_mcp::reference::{
    CommitStatus, DeltaStore, PreparedDocument, ReferenceError, ReferenceLayout, ReferenceLimits,
    Source, SourceKind,
};

fn stdin_document(content: &str, limits: &ReferenceLimits) -> PreparedDocument {
    PreparedDocument::from_bytes(Source::Stdin, content.as_bytes(), None, None, limits)
        .expect("stdin document")
}

fn store_with_limits(limits: ReferenceLimits) -> (tempfile::TempDir, DeltaStore) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let layout = ReferenceLayout::under(temporary.path().join("reference"));
    let store = DeltaStore::new(layout, limits);
    store.initialize().expect("initialize delta");
    (temporary, store)
}

#[test]
fn documents_normalize_utf8_markdown_and_redact_before_hashing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("standard.md");
    std::fs::write(&path, b"fixture").expect("source fixture");
    let secret = concat!("sk-", "fixture0123456789abcdef");
    let bytes = format!("\u{feff}# Rule\r\ntask-specific evidence\rcredential {secret}\r");

    let document = PreparedDocument::from_bytes(
        Source::File(path.clone()),
        bytes.as_bytes(),
        None,
        None,
        &ReferenceLimits::default(),
    )
    .expect("prepared document");

    assert_eq!(document.source_kind, SourceKind::File);
    assert_eq!(document.source_label, "standard.md");
    assert_eq!(document.content_type, "text/markdown");
    assert_eq!(
        document.content,
        "# Rule\ntask-specific evidence\ncredential [REDACTED]\n"
    );
    assert_eq!(document.redaction_count, 1);
    assert_eq!(document.normalized_bytes, document.content.len());
    assert!(document.source_id.starts_with("sha256:"));
    assert!(document.content_sha256.starts_with("sha256:"));
    let serialized = serde_json::to_string(&document).expect("serialize document");
    assert!(!serialized.contains(path.to_str().expect("UTF-8 path")));
    assert!(!serialized.contains(secret));
}

#[test]
fn strict_utf8_empty_and_exact_input_limits_are_enforced() {
    let limits = ReferenceLimits {
        max_input_bytes: 4,
        ..ReferenceLimits::default()
    };

    assert!(matches!(
        PreparedDocument::from_bytes(Source::Stdin, b"", None, None, &limits),
        Err(ReferenceError::InvalidInput)
    ));
    assert!(matches!(
        PreparedDocument::from_bytes(Source::Stdin, &[0xff], None, None, &limits),
        Err(ReferenceError::InvalidInput)
    ));
    assert!(PreparedDocument::from_bytes(Source::Stdin, b"four", None, None, &limits).is_ok());
    assert!(matches!(
        PreparedDocument::from_bytes(Source::Stdin, b"five!", None, None, &limits),
        Err(ReferenceError::InputTooLarge)
    ));
}

#[test]
fn blank_logical_stdin_identity_is_rejected() {
    assert!(matches!(
        PreparedDocument::from_bytes(
            Source::Stdin,
            b"fleet fact",
            Some(" \t "),
            None,
            &ReferenceLimits::default(),
        ),
        Err(ReferenceError::InvalidInput)
    ));
}

#[test]
fn forged_prepared_document_is_rejected_before_lock_or_sequence_allocation() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    let mut document = stdin_document("fleet fact", store.limits());
    document.content_sha256 = "sha256:forged".to_owned();

    assert!(matches!(
        store.commit_batch(&[document]),
        Err(ReferenceError::InvalidInput)
    ));
    assert_eq!(
        store
            .snapshot_after(0)
            .expect("unchanged snapshot")
            .head
            .highest_committed_sequence,
        0
    );
}

#[test]
fn source_identity_is_stable_for_files_and_logical_stdin_updates() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let first_path = temporary.path().join("first.txt");
    let second_path = temporary.path().join("second.txt");
    std::fs::write(&first_path, b"one").expect("first source");
    std::fs::write(&second_path, b"one").expect("second source");
    let limits = ReferenceLimits::default();

    let first_v1 = PreparedDocument::from_bytes(
        Source::File(first_path.clone()),
        b"one",
        None,
        None,
        &limits,
    )
    .expect("first revision");
    let first_v2 =
        PreparedDocument::from_bytes(Source::File(first_path), b"two", None, None, &limits)
            .expect("second revision");
    let second =
        PreparedDocument::from_bytes(Source::File(second_path), b"one", None, None, &limits)
            .expect("other source");
    assert_eq!(first_v1.source_id, first_v2.source_id);
    assert_ne!(first_v1.source_id, second.source_id);

    let anonymous_v1 = stdin_document("one", &limits);
    let anonymous_v2 = stdin_document("two", &limits);
    assert_ne!(anonymous_v1.source_id, anonymous_v2.source_id);
    let logical_v1 = PreparedDocument::from_bytes(
        Source::Stdin,
        b"one",
        Some("fleet-standard"),
        Some("Standard"),
        &limits,
    )
    .expect("logical stdin");
    let logical_v2 = PreparedDocument::from_bytes(
        Source::Stdin,
        b"two",
        Some("fleet-standard"),
        Some("Standard"),
        &limits,
    )
    .expect("logical stdin update");
    assert_eq!(logical_v1.source_id, logical_v2.source_id);
    let serialized = serde_json::to_string(&logical_v1).expect("serialize logical source");
    assert!(!serialized.contains("fleet-standard"));
}

#[test]
fn a_committed_batch_becomes_visible_at_one_head() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    let documents = vec![
        stdin_document("first reference", store.limits()),
        PreparedDocument::from_bytes(
            Source::Stdin,
            b"second reference",
            Some("second"),
            Some("Second"),
            store.limits(),
        )
        .expect("second document"),
    ];

    let receipt = store.commit_batch(&documents).expect("commit batch");
    let snapshot = store.snapshot_after(0).expect("delta snapshot");

    assert_eq!(receipt.status, CommitStatus::Durable);
    assert_eq!(receipt.first_sequence, Some(1));
    assert_eq!(receipt.highest_committed_sequence, 2);
    assert_eq!(snapshot.head.highest_committed_sequence, 2);
    assert_eq!(snapshot.records.len(), 2);
    assert_eq!(snapshot.records[0].sequence, 1);
    assert_eq!(snapshot.records[1].sequence, 2);
    assert!(snapshot.head.verify_hash());
}

#[test]
fn unchanged_replay_allocates_no_sequence_and_changed_content_supersedes() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    let first = PreparedDocument::from_bytes(
        Source::Stdin,
        b"version one",
        Some("standard"),
        Some("Standard"),
        store.limits(),
    )
    .expect("first document");
    let first_receipt = store.commit_batch(&[first.clone()]).expect("first commit");
    let replay = store.commit_batch(&[first]).expect("unchanged replay");
    assert_eq!(replay.status, CommitStatus::Unchanged);
    assert_eq!(replay.first_sequence, None);
    assert_eq!(replay.highest_committed_sequence, 1);

    let changed = PreparedDocument::from_bytes(
        Source::Stdin,
        b"version two",
        Some("standard"),
        Some("Standard"),
        store.limits(),
    )
    .expect("changed document");
    let changed_receipt = store.commit_batch(&[changed]).expect("changed commit");
    let snapshot = store.snapshot_after(0).expect("delta snapshot");
    assert_eq!(changed_receipt.status, CommitStatus::Durable);
    assert_eq!(snapshot.records[1].revision, 2);
    assert_eq!(
        snapshot.records[1].supersedes_event_id.as_deref(),
        Some(first_receipt.records[0].event_id.as_str())
    );
}

#[derive(Default)]
struct HeadRenameFault {
    armed: AtomicBool,
}

impl SyncOps for HeadRenameFault {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn before_rename(&self, _temporary: &Path, destination: &Path) -> io::Result<()> {
        if self.armed.load(Ordering::SeqCst)
            && destination
                .file_name()
                .is_some_and(|name| name == "head.json")
        {
            return Err(io::Error::other("injected head fault"));
        }
        Ok(())
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

#[test]
fn records_renamed_before_a_failed_head_replace_remain_invisible_then_recover() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let layout = ReferenceLayout::under(temporary.path().join("reference"));
    let fault = Arc::new(HeadRenameFault::default());
    let store = DeltaStore::with_sync(layout.clone(), ReferenceLimits::default(), fault.clone());
    store.initialize().expect("initialize delta");
    fault.armed.store(true, Ordering::SeqCst);

    let error = store
        .commit_batch(&[stdin_document("orphan candidate", store.limits())])
        .expect_err("head replacement must fail");
    assert!(matches!(
        error,
        ReferenceError::Io(_) | ReferenceError::Atomic(_)
    ));
    fault.armed.store(false, Ordering::SeqCst);
    assert!(
        store
            .snapshot_after(0)
            .expect("old head snapshot")
            .records
            .is_empty()
    );
    assert_eq!(
        std::fs::read_dir(&layout.delta_events)
            .expect("delta events")
            .count(),
        1
    );

    let recovered = store.adopt_orphans().expect("adopt complete orphan batch");
    assert_eq!(recovered.highest_committed_sequence, 1);
    assert_eq!(
        store
            .snapshot_after(0)
            .expect("recovered snapshot")
            .records
            .len(),
        1
    );
}

#[test]
fn invalid_orphan_manifest_cannot_quarantine_a_committed_event() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let layout = ReferenceLayout::under(temporary.path().join("reference"));
    let fault = Arc::new(HeadRenameFault::default());
    let store = DeltaStore::with_sync(layout.clone(), ReferenceLimits::default(), fault.clone());
    store.initialize().expect("initialize delta");
    store
        .commit_batch(&[stdin_document("committed", store.limits())])
        .expect("first commit");
    let committed_name = store
        .event_path(1)
        .expect("committed event")
        .file_name()
        .expect("event file name")
        .to_string_lossy()
        .into_owned();

    fault.armed.store(true, Ordering::SeqCst);
    store
        .commit_batch(&[stdin_document("orphan", store.limits())])
        .expect_err("head replacement must fail");
    fault.armed.store(false, Ordering::SeqCst);

    let stage = std::fs::read_dir(&layout.staging)
        .expect("staging directory")
        .map(|entry| entry.expect("staging entry"))
        .find(|entry| entry.file_name().to_string_lossy().starts_with("delta-"))
        .expect("orphan stage")
        .path();
    let batch_path = stage.join("batch.json");
    let mut batch: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&batch_path).expect("read orphan manifest"))
            .expect("parse orphan manifest");
    batch["events"][0]["file_name"] = serde_json::Value::String(committed_name);
    std::fs::write(
        &batch_path,
        serde_json::to_vec(&batch).expect("serialize corrupt manifest"),
    )
    .expect("write corrupt manifest");

    assert!(matches!(
        store.adopt_orphans(),
        Err(ReferenceError::CorruptRecord)
    ));
    let snapshot = store
        .snapshot_after(0)
        .expect("committed history must survive quarantine");
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].content, "committed");
}

struct FailNextFileSync {
    armed: AtomicBool,
}

impl SyncOps for FailNextFileSync {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected owner sync fault"));
        }
        file.sync_all()
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

#[test]
fn writer_lock_setup_failure_does_not_strand_the_lock() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let layout = ReferenceLayout::under(temporary.path().join("reference"));
    let normal = DeltaStore::new(layout.clone(), ReferenceLimits::default());
    normal.initialize().expect("initialize delta");
    let fault = Arc::new(FailNextFileSync {
        armed: AtomicBool::new(true),
    });
    let faulty = DeltaStore::with_sync(layout.clone(), ReferenceLimits::default(), fault);

    assert!(
        faulty
            .commit_batch(&[stdin_document("lock fault", faulty.limits())])
            .is_err()
    );
    assert!(
        !layout.delta_lock.exists(),
        "failed lock initialization must remove delta.lock"
    );
}

#[test]
fn a_missing_committed_sequence_is_rejected() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    store
        .commit_batch(&[
            stdin_document("first", store.limits()),
            stdin_document("second", store.limits()),
        ])
        .expect("commit batch");
    let second_path = store.event_path(2).expect("second event path");
    std::fs::remove_file(second_path).expect("remove committed event");

    assert!(matches!(
        store.snapshot_after(0),
        Err(ReferenceError::CorruptRecord)
    ));
}

#[test]
fn an_event_stored_under_the_wrong_identity_is_rejected() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    store
        .commit_batch(&[stdin_document("committed event", store.limits())])
        .expect("commit event");
    let original = store.event_path(1).expect("event path");
    let wrong_name = original.parent().expect("event directory").join(format!(
        "{:020}-{}.json",
        1,
        "0".repeat(64)
    ));
    std::fs::rename(original, wrong_name).expect("rename event to wrong identity");

    assert!(matches!(
        store.snapshot_after(0),
        Err(ReferenceError::CorruptRecord)
    ));
}

#[test]
fn pending_event_limit_rejects_before_allocating_a_sequence() {
    let limits = ReferenceLimits {
        max_pending_events: 1,
        ..ReferenceLimits::default()
    };
    let (_temporary, store) = store_with_limits(limits);
    store
        .commit_batch(&[stdin_document("first", store.limits())])
        .expect("first commit");

    assert!(matches!(
        store.commit_batch(&[stdin_document("second", store.limits())]),
        Err(ReferenceError::BacklogLimit)
    ));
    assert_eq!(
        store
            .snapshot_after(0)
            .expect("unchanged snapshot")
            .head
            .highest_committed_sequence,
        1
    );
    assert!(store.event_path(2).is_none());
}

#[test]
fn concurrent_writers_allocate_distinct_contiguous_sequences() {
    let (_temporary, store) = store_with_limits(ReferenceLimits::default());
    let first_store = store.clone();
    let second_store = store.clone();
    let first = std::thread::spawn(move || {
        first_store.commit_batch(&[stdin_document("first", first_store.limits())])
    });
    let second = std::thread::spawn(move || {
        second_store.commit_batch(&[stdin_document("second", second_store.limits())])
    });

    first
        .join()
        .expect("first writer thread")
        .expect("first writer");
    second
        .join()
        .expect("second writer thread")
        .expect("second writer");
    let snapshot = store.snapshot_after(0).expect("delta snapshot");
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}
