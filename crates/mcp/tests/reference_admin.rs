#![cfg(feature = "runtime")]

use std::io::{self, Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_mcp::reference::{
    CognificationWaiter, CommitStatus, PublishSpawner, ReferenceConfig, ReferenceError,
    ReferenceLayout, ReferenceLimits, ReferenceRememberArgs, RememberReceipt, prepare_documents,
    run_reference_remember_with,
};

fn config(root: std::path::PathBuf, limits: ReferenceLimits) -> ReferenceConfig {
    ReferenceConfig {
        layout: ReferenceLayout::under(root),
        dataset: "fleet_reference",
        limits,
    }
}

fn stdin_args() -> ReferenceRememberArgs {
    ReferenceRememberArgs {
        files: Vec::new(),
        source_id: None,
        label: None,
        wait_cognified: false,
        wait_seconds: 1,
    }
}

struct PanicReader;

impl Read for PanicReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        panic!("stdin must not be read when files are present")
    }
}

#[test]
fn file_inputs_are_read_in_argument_order_without_reading_stdin() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let first = temporary.path().join("first.md");
    let second = temporary.path().join("second.txt");
    std::fs::write(&first, "# First\n").expect("first file");
    std::fs::write(&second, "Second\n").expect("second file");
    let arguments = ReferenceRememberArgs {
        files: vec![first, second],
        ..stdin_args()
    };

    let documents = prepare_documents(&arguments, &mut PanicReader, &ReferenceLimits::default())
        .expect("prepared files");

    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].source_label, "first.md");
    assert_eq!(documents[0].content_type, "text/markdown");
    assert_eq!(documents[1].source_label, "second.txt");
    assert_eq!(documents[1].content_type, "text/plain");
}

#[test]
fn input_selection_rejects_directories_invalid_utf8_and_batch_limits() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let limits = ReferenceLimits {
        max_input_bytes: 8,
        max_batch_bytes: 10,
        max_batch_files: 1,
        ..ReferenceLimits::default()
    };
    let directory_args = ReferenceRememberArgs {
        files: vec![temporary.path().to_path_buf()],
        ..stdin_args()
    };
    assert!(matches!(
        prepare_documents(&directory_args, &mut Cursor::new(Vec::new()), &limits),
        Err(ReferenceError::InvalidInput)
    ));

    let invalid = temporary.path().join("invalid.txt");
    std::fs::write(&invalid, [0xff]).expect("invalid UTF-8 fixture");
    let invalid_args = ReferenceRememberArgs {
        files: vec![invalid],
        ..stdin_args()
    };
    assert!(matches!(
        prepare_documents(&invalid_args, &mut Cursor::new(Vec::new()), &limits),
        Err(ReferenceError::InvalidInput)
    ));

    let first = temporary.path().join("one.txt");
    let second = temporary.path().join("two.txt");
    std::fs::write(&first, "123456").expect("first batch file");
    std::fs::write(&second, "123456").expect("second batch file");
    let too_many = ReferenceRememberArgs {
        files: vec![first.clone(), second.clone()],
        ..stdin_args()
    };
    assert!(matches!(
        prepare_documents(&too_many, &mut PanicReader, &limits),
        Err(ReferenceError::TooManyFiles)
    ));

    let aggregate_limits = ReferenceLimits {
        max_batch_files: 2,
        ..limits
    };
    assert!(matches!(
        prepare_documents(&too_many, &mut PanicReader, &aggregate_limits),
        Err(ReferenceError::BatchTooLarge)
    ));
}

#[derive(Default)]
struct RecordingSpawner {
    calls: AtomicUsize,
}

impl PublishSpawner for RecordingSpawner {
    fn spawn(&self) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct FixedWaiter(bool);

impl CognificationWaiter for FixedWaiter {
    fn wait(
        &self,
        _layout: &ReferenceLayout,
        _receipt: &cognee_mcp::reference::CommitReceipt,
        _timeout: std::time::Duration,
    ) -> Result<bool, ReferenceError> {
        Ok(self.0)
    }
}

#[test]
fn durable_remember_spawns_publish_and_reports_exact_commit_state() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    let spawner = RecordingSpawner::default();

    let receipt = run_reference_remember_with(
        &config,
        &stdin_args(),
        &mut Cursor::new("durable fleet fact"),
        &spawner,
        &FixedWaiter(false),
    )
    .expect("remember reference");

    assert_eq!(receipt.status, CommitStatus::Durable);
    assert_eq!(receipt.highest_committed_sequence, 1);
    assert_eq!(receipt.records.len(), 1);
    assert!(!receipt.cognified);
    assert!(!receipt.wait_timed_out);
    assert!(receipt.publisher_started);
    assert_eq!(spawner.calls.load(Ordering::SeqCst), 1);
    assert!(serde_json::to_string(&receipt).is_ok());
}

#[test]
fn unchanged_remember_is_idempotent_and_does_not_spawn_another_publisher() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    let spawner = RecordingSpawner::default();
    let arguments = stdin_args();
    for _ in 0..2 {
        let receipt = run_reference_remember_with(
            &config,
            &arguments,
            &mut Cursor::new("same fleet fact"),
            &spawner,
            &FixedWaiter(false),
        )
        .expect("remember reference");
        if spawner.calls.load(Ordering::SeqCst) == 2 {
            panic!("unchanged replay spawned a second publisher: {receipt:?}");
        }
    }
    assert_eq!(spawner.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn wait_timeout_keeps_the_durable_receipt_and_marks_it_non_cognified() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    let arguments = ReferenceRememberArgs {
        wait_cognified: true,
        ..stdin_args()
    };

    let receipt: RememberReceipt = run_reference_remember_with(
        &config,
        &arguments,
        &mut Cursor::new("waited fleet fact"),
        &RecordingSpawner::default(),
        &FixedWaiter(false),
    )
    .expect("durable remember with timed-out wait");

    assert_eq!(receipt.status, CommitStatus::Durable);
    assert!(!receipt.cognified);
    assert!(receipt.wait_timed_out);
}

#[test]
fn doctor_reports_the_committed_head_without_mutating_the_tree() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    run_reference_remember_with(
        &config,
        &stdin_args(),
        &mut Cursor::new("diagnostic fleet fact"),
        &RecordingSpawner::default(),
        &FixedWaiter(false),
    )
    .expect("seed reference");
    let before = directory_inventory(&config.layout.root);

    let report =
        cognee_mcp::reference::run_reference_doctor(&config).expect("reference diagnostics");

    assert_eq!(report.highest_committed_sequence, 1);
    assert_eq!(report.committed_records, 1);
    assert_eq!(report.orphan_records, 0);
    assert_eq!(directory_inventory(&config.layout.root), before);
}

#[test]
fn doctor_rejects_a_mismatched_reference_schema() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    run_reference_remember_with(
        &config,
        &stdin_args(),
        &mut Cursor::new("diagnostic fleet fact"),
        &RecordingSpawner::default(),
        &FixedWaiter(false),
    )
    .expect("seed reference");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &config.layout.schema,
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("make schema writable");
    }
    std::fs::write(
        &config.layout.schema,
        br#"{"schema_version":1,"dataset":"wrong_dataset"}"#,
    )
    .expect("replace schema");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &config.layout.schema,
            std::fs::Permissions::from_mode(0o444),
        )
        .expect("restore schema mode");
    }

    assert!(matches!(
        cognee_mcp::reference::run_reference_doctor(&config),
        Err(ReferenceError::CorruptRecord)
    ));
}

#[test]
fn doctor_rejects_a_backlog_above_the_configured_bound() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let seeded_config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    for fact in ["first pending fact", "second pending fact"] {
        run_reference_remember_with(
            &seeded_config,
            &stdin_args(),
            &mut Cursor::new(fact),
            &RecordingSpawner::default(),
            &FixedWaiter(false),
        )
        .expect("seed pending reference");
    }
    let constrained = config(
        seeded_config.layout.root.clone(),
        ReferenceLimits {
            max_pending_events: 1,
            ..ReferenceLimits::default()
        },
    );

    assert!(matches!(
        cognee_mcp::reference::run_reference_doctor(&constrained),
        Err(ReferenceError::BacklogLimit)
    ));
}

#[test]
#[cfg(unix)]
fn doctor_rejects_writable_public_event_files() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(
        temporary.path().join("reference"),
        ReferenceLimits::default(),
    );
    run_reference_remember_with(
        &config,
        &stdin_args(),
        &mut Cursor::new("diagnostic fleet fact"),
        &RecordingSpawner::default(),
        &FixedWaiter(false),
    )
    .expect("seed reference");
    let event = cognee_mcp::reference::DeltaStore::new(config.layout.clone(), config.limits)
        .event_path(1)
        .expect("event path");
    std::fs::set_permissions(&event, std::fs::Permissions::from_mode(0o644))
        .expect("make event writable");

    assert!(matches!(
        cognee_mcp::reference::run_reference_doctor(&config),
        Err(ReferenceError::CorruptRecord)
    ));
}

fn directory_inventory(root: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(directory).expect("inventory directory");
        for entry in entries {
            let entry = entry.expect("inventory entry");
            let path = entry.path();
            let metadata = entry.metadata().expect("inventory metadata");
            result.push((
                path.strip_prefix(root)
                    .expect("relative inventory")
                    .to_owned(),
                metadata.len(),
            ));
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    result.sort();
    result
}
