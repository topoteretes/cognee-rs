#![cfg(feature = "runtime")]

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use cognee_mcp::atomic_fs::SyncOps;
use cognee_mcp::event::EventEnvelope;
use cognee_mcp::hook_input::HookInput;
use cognee_mcp::layout::StateLayout;
use cognee_mcp::limits::ResourceLimits;
use cognee_mcp::spool::{FailureDisposition, MAX_EVENT_FILE_BYTES, Priority, Spool, SpoolRecord};
use serde_json::json;
use tempfile::tempdir;

#[derive(Default)]
struct RecordingSync {
    events: Mutex<Vec<String>>,
}

impl RecordingSync {
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("recording lock").clone()
    }
}

impl SyncOps for RecordingSync {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        self.events
            .lock()
            .expect("recording lock")
            .push("file-sync".to_owned());
        file.sync_all()
    }

    fn before_rename(&self, _temporary: &Path, _destination: &Path) -> io::Result<()> {
        self.events
            .lock()
            .expect("recording lock")
            .push("rename".to_owned());
        Ok(())
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        self.events
            .lock()
            .expect("recording lock")
            .push(format!("dir-sync:{}", directory.display()));
        File::open(directory)?.sync_all()
    }
}

struct ConcurrentInstallSync {
    before_install: Barrier,
}

impl ConcurrentInstallSync {
    fn new(writers: usize) -> Self {
        Self {
            before_install: Barrier::new(writers),
        }
    }
}

impl SyncOps for ConcurrentInstallSync {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn before_rename(&self, _temporary: &Path, _destination: &Path) -> io::Result<()> {
        self.before_install.wait();
        Ok(())
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

fn fixture_event(dataset: &str, generation: u64, marker: &str) -> EventEnvelope {
    let raw = serde_json::to_vec(&json!({
        "session_id": format!("session-{marker}"),
        "transcript_path": "/private/transcript.jsonl",
        "cwd": "/work/project",
        "hook_event_name": "AfterAgent",
        "timestamp": "2026-08-19T20:00:00.123456789Z",
        "prompt": format!("prompt-{marker}"),
        "prompt_response": format!("response-{marker}"),
        "stop_hook_active": false
    }))
    .expect("fixture JSON");
    EventEnvelope::from_hook(
        HookInput::parse(&raw).expect("official hook fixture"),
        "alice",
        "host-a",
        dataset,
        generation,
    )
}

fn fixture_spool() -> (tempfile::TempDir, StateLayout, Spool) {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let spool = Spool::new(layout.clone(), ResourceLimits::default());
    (root, layout, spool)
}

#[test]
fn enqueue_is_atomic_private_and_duplicate_safe() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let sync = Arc::new(RecordingSync::default());
    let spool = Spool::with_sync(layout.clone(), ResourceLimits::default(), sync.clone());
    let event = fixture_event("agent_sessions", 0, "atomic");

    let first = spool
        .enqueue(&event, Priority::Normal)
        .expect("first enqueue");
    let second = spool
        .enqueue(&event, Priority::Normal)
        .expect("duplicate enqueue");

    assert!(!first.duplicate);
    assert!(second.duplicate);
    assert_eq!(spool.depths().expect("depths").pending, 1);
    assert_eq!(
        fs::metadata(&first.path)
            .expect("event metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        fs::read_dir(&layout.spool_pending)
            .expect("pending dir")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-"))
    );

    let events = sync.events();
    let file_sync = events
        .iter()
        .position(|event| event == "file-sync")
        .expect("file sync recorded");
    let rename = events
        .iter()
        .position(|event| event == "rename")
        .expect("rename recorded");
    let directory_sync = events
        .iter()
        .position(|event| event.starts_with("dir-sync:"))
        .expect("directory sync recorded");
    assert!(file_sync < rename && rename < directory_sync);
}

#[test]
fn concurrent_duplicate_enqueue_has_exactly_one_atomic_winner() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let sync = Arc::new(ConcurrentInstallSync::new(2));
    let spool = Spool::with_sync(layout, ResourceLimits::default(), sync);
    let event = fixture_event("agent_sessions", 0, "concurrent-duplicate");

    let writers: Vec<_> = (0..2)
        .map(|_| {
            let spool = spool.clone();
            let event = event.clone();
            thread::spawn(move || {
                spool
                    .enqueue(&event, Priority::Normal)
                    .expect("concurrent enqueue")
            })
        })
        .collect();
    let outcomes: Vec<_> = writers
        .into_iter()
        .map(|writer| writer.join().expect("writer thread"))
        .collect();

    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.duplicate).count(),
        1
    );
    assert_eq!(
        outcomes.iter().filter(|outcome| !outcome.duplicate).count(),
        1
    );
    assert_eq!(spool.depths().expect("depths").pending, 1);
}

#[test]
fn processing_recovery_commit_and_five_attempt_quarantine_are_bounded() {
    let (_root, layout, spool) = fixture_spool();
    let first = fixture_event("agent_sessions", 0, "first");
    let second = fixture_event("agent_sessions", 0, "second");
    spool
        .enqueue(&first, Priority::Normal)
        .expect("enqueue first");
    spool
        .enqueue(&second, Priority::Normal)
        .expect("enqueue second");

    let first_file = spool
        .pending_files()
        .expect("pending files")
        .into_iter()
        .find(|file| file.event_id == first.event_id)
        .expect("first pending file");
    let _claimed = spool.claim(&first_file).expect("claim first");
    assert_eq!(spool.depths().expect("depths").processing, 1);

    let recovery = spool
        .recover_processing(|_| Ok(false))
        .expect("recover uncommitted processing");
    assert_eq!(recovery.requeued, 1);
    assert_eq!(spool.depths().expect("depths").processing, 0);

    let first_file = spool
        .pending_files()
        .expect("pending files")
        .into_iter()
        .find(|file| file.event_id == first.event_id)
        .expect("recovered first file");
    let claimed = spool.claim(&first_file).expect("reclaim first");
    spool.commit(claimed).expect("commit first");
    let pending = spool.pending_files().expect("pending after commit");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].event_id, second.event_id);

    for expected_attempt in 1..=5 {
        let file = spool
            .pending_files()
            .expect("pending retry")
            .into_iter()
            .find(|file| file.event_id == second.event_id)
            .expect("second retry file");
        let claimed = spool.claim(&file).expect("claim retry");
        let disposition = spool
            .fail(claimed, "proxy_429: bearer-secret", None)
            .expect("record failure");
        if expected_attempt < 5 {
            assert_eq!(disposition, FailureDisposition::Requeued(expected_attempt));
        } else {
            assert_eq!(disposition, FailureDisposition::Quarantined(5));
        }
    }

    let failed_files: Vec<_> = fs::read_dir(&layout.spool_failed)
        .expect("failed directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .collect();
    assert_eq!(failed_files.len(), 1);
    let bytes = fs::read(failed_files[0].path()).expect("failed event");
    let record: SpoolRecord = serde_json::from_slice(&bytes).expect("failed record JSON");
    assert_eq!(record.envelope.event_id, second.event_id);
    assert_eq!(record.attempts, 5);
    assert_eq!(record.last_error_class.as_deref(), Some("proxy_429"));
    assert!(!String::from_utf8_lossy(&bytes).contains("bearer-secret"));
}

#[test]
fn high_water_degrades_only_the_new_event_and_retains_identity() {
    let (_root, layout, spool) = fixture_spool();
    let existing = layout.spool_pending.join("existing.sparse");
    let file = File::create(&existing).expect("sparse fixture");
    file.set_len(ResourceLimits::default().spool_high_water_bytes + 1)
        .expect("sparse high-water fixture");
    let before = fs::metadata(&existing).expect("before metadata").len();
    let event = fixture_event("agent_sessions", 0, "high-water-sensitive-body");

    let outcome = spool
        .enqueue(&event, Priority::High)
        .expect("degraded enqueue");
    assert!(outcome.capture_degraded);
    assert_eq!(
        fs::metadata(&existing).expect("after metadata").len(),
        before
    );

    let record: SpoolRecord =
        serde_json::from_slice(&fs::read(&outcome.path).expect("degraded event bytes"))
            .expect("degraded event JSON");
    assert_eq!(record.envelope.event_id, event.event_id);
    assert_eq!(record.envelope.payload_hash, event.payload_hash);
    assert!(record.envelope.capture.capture_degraded);
    let payload = record.envelope.payload.to_string();
    assert!(payload.contains("OMITTED"));
    assert!(!payload.contains("high-water-sensitive-body"));
}

#[test]
fn recovery_quarantines_invalid_or_oversized_bytes_without_parsing_them() {
    let (_root, layout, spool) = fixture_spool();
    let malformed = fixture_event("agent_sessions", 0, "malformed");
    let oversized = fixture_event("agent_sessions", 0, "oversized");
    for event in [&malformed, &oversized] {
        spool
            .enqueue(event, Priority::Normal)
            .expect("enqueue invalid fixture");
        let pending = spool
            .pending_files()
            .expect("pending invalid fixture")
            .into_iter()
            .find(|file| file.event_id == event.event_id)
            .expect("invalid fixture path");
        let _claimed = spool.claim(&pending).expect("claim invalid fixture");
    }

    let mut processing: Vec<_> = fs::read_dir(&layout.spool_processing)
        .expect("processing directory")
        .filter_map(Result::ok)
        .collect();
    processing.sort_by_key(std::fs::DirEntry::file_name);
    fs::write(processing[0].path(), b"{not-json:fixture-sentinel}")
        .expect("write malformed fixture");
    File::options()
        .write(true)
        .open(processing[1].path())
        .expect("open oversized fixture")
        .set_len(MAX_EVENT_FILE_BYTES + 1)
        .expect("extend oversized fixture");

    let report = spool
        .recover_processing(|_| panic!("invalid events must not reach the ledger"))
        .expect("quarantine invalid processing");
    assert_eq!(report.invalid_quarantined, 2);
    assert_eq!(spool.depths().expect("depths").processing, 0);
    let invalid = layout.spool_failed.join("invalid");
    let quarantined: Vec<_> = fs::read_dir(&invalid)
        .expect("invalid quarantine")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(quarantined.len(), 2);
    assert!(quarantined.iter().any(|entry| {
        fs::read(entry.path()).is_ok_and(|bytes| bytes == b"{not-json:fixture-sentinel}")
    }));
    assert!(quarantined.iter().any(|entry| {
        fs::metadata(entry.path()).is_ok_and(|metadata| metadata.len() == MAX_EVENT_FILE_BYTES + 1)
    }));
    let status =
        fs::read_to_string(layout.status.join("spool-last-error.json")).expect("redacted status");
    assert!(!status.contains("fixture-sentinel"));
}
