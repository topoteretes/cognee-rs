#![cfg(feature = "runtime")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use cognee_mcp::layout::StateLayout;
use cognee_mcp::ledger::{IngestionState, Ledger, LedgerError, LedgerRuntime};
use rusqlite::Connection;
use tempfile::tempdir;

struct FakeRuntime {
    now: Mutex<DateTime<Utc>>,
    jitter: Duration,
}

impl FakeRuntime {
    fn new(now: DateTime<Utc>, jitter: Duration) -> Self {
        Self {
            now: Mutex::new(now),
            jitter,
        }
    }
}

impl LedgerRuntime for FakeRuntime {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("clock lock")
    }

    fn retry_jitter(&self, _attempt: u32, _base: Duration) -> Duration {
        self.jitter
    }
}

fn instant() -> DateTime<Utc> {
    Utc.timestamp_opt(1_777_100_000, 0)
        .single()
        .expect("fixture timestamp")
}

#[test]
fn begin_is_idempotent_and_commit_survives_reopen_with_delete_journaling() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    let runtime = Arc::new(FakeRuntime::new(instant(), Duration::ZERO));
    let mut ledger = Ledger::with_runtime(layout.clone(), runtime.clone()).expect("open ledger");

    let first = ledger
        .begin("event-a", "agent_sessions", 7)
        .expect("first begin");
    let second = ledger
        .begin("event-a", "agent_sessions", 7)
        .expect("idempotent begin");
    assert_eq!(first, second);
    assert_eq!(first.state, IngestionState::Applying);
    assert!(matches!(
        ledger.begin("event-a", "other", 7),
        Err(LedgerError::EventConflict)
    ));

    ledger
        .mark_committed("event-a", Some("entry-123"))
        .expect("commit event");
    let database_path = ledger.path().to_path_buf();
    drop(ledger);

    let reopened = Ledger::with_runtime(layout, runtime).expect("reopen ledger");
    let committed = reopened
        .state("event-a")
        .expect("read state")
        .expect("committed row");
    assert_eq!(committed.state, IngestionState::Committed);
    assert_eq!(committed.applied_entry_id.as_deref(), Some("entry-123"));
    assert_eq!(
        fs::metadata(&database_path)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let connection = Connection::open(database_path).expect("inspection connection");
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .expect("synchronous mode");
    assert_eq!(journal.to_ascii_lowercase(), "delete");
    assert_eq!(synchronous, 2);
}

#[test]
fn retry_backoff_is_capped_jittered_and_stores_only_error_class() {
    let root = tempdir().expect("temp root");
    let layout = StateLayout::under(root.path().join("cognee"));
    let runtime = Arc::new(FakeRuntime::new(instant(), Duration::from_millis(250)));
    let mut ledger = Ledger::with_runtime(layout, runtime).expect("open ledger");
    ledger
        .begin("event-retry", "agent_sessions", 0)
        .expect("begin retry event");

    let first = ledger
        .record_retry("event-retry", "proxy_429: bearer-secret")
        .expect("first retry");
    assert_eq!(first.state, IngestionState::Retry);
    assert_eq!(first.attempts, 1);
    assert_eq!(first.last_error_class.as_deref(), Some("proxy_429"));
    let first_delay = first.next_attempt_at.expect("first retry time") - instant();
    assert_eq!(first_delay.num_milliseconds(), 1_250);

    let mut latest = first;
    for _ in 1..20 {
        latest = ledger
            .record_retry("event-retry", "upstream_5xx raw-payload")
            .expect("later retry");
    }
    let capped_delay = latest.next_attempt_at.expect("capped retry time") - instant();
    assert!(capped_delay <= chrono::Duration::minutes(5));
    assert_eq!(latest.last_error_class.as_deref(), Some("upstream_5xx"));

    let database_bytes = fs::read(ledger.path()).expect("database bytes");
    assert!(!String::from_utf8_lossy(&database_bytes).contains("bearer-secret"));
    assert!(!String::from_utf8_lossy(&database_bytes).contains("raw-payload"));

    let failed = ledger
        .mark_failed("event-retry", "schema_drift: private-detail")
        .expect("mark failed");
    assert_eq!(failed.state, IngestionState::Failed);
    assert_eq!(failed.last_error_class.as_deref(), Some("schema_drift"));
    assert!(failed.next_attempt_at.is_none());
}
