#![cfg(feature = "runtime")]

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use cognee_mcp::atomic_fs::{SyncOps, SystemSyncOps};
use cognee_mcp::event::EventEnvelope;
use cognee_mcp::generation::GenerationStore;
use cognee_mcp::hook_input::HookInput;
use cognee_mcp::layout::StateLayout;
use cognee_mcp::limits::ResourceLimits;
use cognee_mcp::spool::{Priority, Spool, SpoolRecord};
use serde_json::json;
use tempfile::tempdir;

struct CrashBeforeRename;

impl SyncOps for CrashBeforeRename {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn before_rename(&self, _temporary: &Path, _destination: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "simulated crash before rename",
        ))
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

fn event(dataset: &str, marker: &str) -> EventEnvelope {
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
        0,
    )
}

#[test]
fn advancing_one_dataset_quarantines_only_its_old_generation() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let spool = Spool::new(layout.clone(), ResourceLimits::default());
    let generations = GenerationStore::new(layout.clone());
    let session_pending = event("agent_sessions", "pending");
    let session_processing = event("agent_sessions", "processing");
    let other = event("project_notes", "other");
    for event in [&session_pending, &session_processing, &other] {
        spool
            .enqueue(event, Priority::Normal)
            .expect("enqueue generation fixture");
    }
    let processing_file = spool
        .pending_files()
        .expect("pending files")
        .into_iter()
        .find(|file| file.event_id == session_processing.event_id)
        .expect("processing candidate");
    spool.claim(&processing_file).expect("processing claim");

    assert_eq!(
        generations.current("agent_sessions").expect("generation"),
        0
    );
    let report = generations
        .advance_and_quarantine("agent_sessions", &spool)
        .expect("advance generation");
    assert_eq!(report.previous, 0);
    assert_eq!(report.current, 1);
    assert_eq!(report.quarantined, 2);
    assert_eq!(
        generations.current("agent_sessions").expect("generation"),
        1
    );
    assert_eq!(generations.current("project_notes").expect("generation"), 0);

    let pending = spool.pending_files().expect("remaining pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].event_id, other.event_id);
    let superseded = layout.spool_failed.join("superseded").join("generation-0");
    let superseded_count = std::fs::read_dir(superseded)
        .expect("superseded directory")
        .filter_map(Result::ok)
        .count();
    assert_eq!(superseded_count, 2);
}

#[test]
fn superseded_quarantine_preserves_an_existing_same_name_record() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let spool = Spool::new(layout.clone(), ResourceLimits::default());
    let stale = event("agent_sessions", "collision");
    let queued = spool
        .enqueue(&stale, Priority::Normal)
        .expect("enqueue stale event");
    let superseded = layout.spool_failed.join("superseded/generation-0");
    std::fs::create_dir_all(&superseded).expect("superseded directory");
    let existing = superseded.join(queued.path.file_name().expect("event file name"));
    let prior_evidence = b"preexisting-superseded-evidence";
    std::fs::write(&existing, prior_evidence).expect("existing evidence");

    let report = GenerationStore::new(layout.clone())
        .advance_and_quarantine("agent_sessions", &spool)
        .expect("advance generation");

    assert_eq!(report.quarantined, 1);
    assert_eq!(
        std::fs::read(&existing).expect("preserved prior evidence"),
        prior_evidence
    );
    let entries = std::fs::read_dir(&superseded)
        .expect("superseded records")
        .collect::<Result<Vec<_>, _>>()
        .expect("superseded entries");
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| {
        serde_json::from_slice::<SpoolRecord>(
            &std::fs::read(entry.path()).expect("superseded bytes"),
        )
        .is_ok_and(|record| record.envelope.event_id == stale.event_id)
    }));
}

#[test]
fn crash_before_generation_pointer_rename_keeps_old_generation_authoritative() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    layout.ensure_private().expect("private layout");
    let spool = Spool::with_sync(
        layout.clone(),
        ResourceLimits::default(),
        Arc::new(SystemSyncOps),
    );
    let event = event("agent_sessions", "crash");
    spool
        .enqueue(&event, Priority::Normal)
        .expect("enqueue before crash");
    let generations = GenerationStore::with_sync(layout.clone(), Arc::new(CrashBeforeRename));

    assert!(
        generations
            .advance_and_quarantine("agent_sessions", &spool)
            .is_err()
    );
    let fresh_reader = GenerationStore::new(layout);
    assert_eq!(
        fresh_reader
            .current("agent_sessions")
            .expect("authoritative generation"),
        0
    );
    assert_eq!(spool.depths().expect("spool depths").pending, 1);
}
