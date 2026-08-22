//! Durable SQLite ingestion ledger for crash-safe spool processing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::layout::{LayoutError, StateLayout};

const MAX_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
static JITTER_COUNTER: AtomicU64 = AtomicU64::new(1);

pub trait LedgerRuntime: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn retry_jitter(&self, attempt: u32, base: Duration) -> Duration;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLedgerRuntime;

impl LedgerRuntime for SystemLedgerRuntime {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn retry_jitter(&self, attempt: u32, base: Duration) -> Duration {
        let ceiling = u64::try_from(base.as_millis() / 2).unwrap_or(u64::MAX);
        if ceiling == 0 {
            return Duration::ZERO;
        }
        let counter = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64;
        Duration::from_millis((now ^ counter ^ u64::from(attempt)) % (ceiling + 1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IngestionState {
    Applying,
    Committed,
    Retry,
    Failed,
}

impl IngestionState {
    fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "applying" => Ok(Self::Applying),
            "committed" => Ok(Self::Committed),
            "retry" => Ok(Self::Retry),
            "failed" => Ok(Self::Failed),
            _ => Err(LedgerError::InvalidState),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub event_id: String,
    pub dataset: String,
    pub dataset_generation: u64,
    pub state: IngestionState,
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub applied_entry_id: Option<String>,
    pub last_error_class: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("ingestion ledger SQLite operation failed")]
    Sqlite(#[source] rusqlite::Error),
    #[error("private state layout failed")]
    Layout(#[source] LayoutError),
    #[error("ingestion ledger file operation failed")]
    Io(#[source] io::Error),
    #[error("ingestion event does not exist")]
    EventNotFound,
    #[error("ingestion event identity conflicts with its existing row")]
    EventConflict,
    #[error("ingestion ledger state is invalid")]
    InvalidState,
    #[error("ingestion ledger timestamp is invalid")]
    InvalidTimestamp,
    #[error("ingestion ledger integer overflowed")]
    Overflow,
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<LayoutError> for LedgerError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<io::Error> for LedgerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct Ledger {
    path: PathBuf,
    connection: Connection,
    runtime: Arc<dyn LedgerRuntime>,
}

impl Ledger {
    pub fn open(layout: StateLayout) -> Result<Self, LedgerError> {
        Self::with_runtime(layout, Arc::new(SystemLedgerRuntime))
    }

    pub fn with_runtime(
        layout: StateLayout,
        runtime: Arc<dyn LedgerRuntime>,
    ) -> Result<Self, LedgerError> {
        layout.ensure_private()?;
        let path = layout.ledger.join("ingestion.sqlite3");
        let connection = Connection::open(&path)?;
        set_private_mode(&path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS ingestion_events (
                event_id TEXT PRIMARY KEY,
                dataset TEXT NOT NULL,
                dataset_generation INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('applying','committed','retry','failed')),
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT,
                applied_entry_id TEXT,
                last_error_class TEXT,
                updated_at TEXT NOT NULL
            );",
        )?;
        Ok(Self {
            path,
            connection,
            runtime,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn begin(
        &mut self,
        event_id: &str,
        dataset: &str,
        dataset_generation: u64,
    ) -> Result<LedgerEntry, LedgerError> {
        let generation = i64::try_from(dataset_generation).map_err(|_| LedgerError::Overflow)?;
        let updated_at = self.runtime.now().to_rfc3339();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO ingestion_events (
                event_id, dataset, dataset_generation, state, attempts, updated_at
             ) VALUES (?1, ?2, ?3, 'applying', 0, ?4)",
            params![event_id, dataset, generation, updated_at],
        )?;
        let raw = query_state(&transaction, event_id)?.ok_or(LedgerError::EventNotFound)?;
        if raw.dataset != dataset || raw.dataset_generation != generation {
            return Err(LedgerError::EventConflict);
        }
        transaction.commit()?;
        LedgerEntry::try_from(raw)
    }

    pub fn mark_committed(
        &mut self,
        event_id: &str,
        applied_entry_id: Option<&str>,
    ) -> Result<LedgerEntry, LedgerError> {
        let updated = self.connection.execute(
            "UPDATE ingestion_events
             SET state = 'committed', next_attempt_at = NULL,
                 applied_entry_id = ?2, last_error_class = NULL, updated_at = ?3
             WHERE event_id = ?1",
            params![event_id, applied_entry_id, self.runtime.now().to_rfc3339()],
        )?;
        if updated == 0 {
            return Err(LedgerError::EventNotFound);
        }
        self.state(event_id)?.ok_or(LedgerError::EventNotFound)
    }

    pub fn record_retry(
        &mut self,
        event_id: &str,
        error_class: &str,
    ) -> Result<LedgerEntry, LedgerError> {
        let current = self.state(event_id)?.ok_or(LedgerError::EventNotFound)?;
        let attempts = current
            .attempts
            .checked_add(1)
            .ok_or(LedgerError::Overflow)?;
        let base = retry_base(attempts);
        let jitter = self.runtime.retry_jitter(attempts, base);
        let delay = base.saturating_add(jitter).min(MAX_RETRY_DELAY);
        let chrono_delay = chrono::Duration::from_std(delay).map_err(|_| LedgerError::Overflow)?;
        let now = self.runtime.now();
        let next_attempt_at = now
            .checked_add_signed(chrono_delay)
            .ok_or(LedgerError::Overflow)?;
        self.connection.execute(
            "UPDATE ingestion_events
             SET state = 'retry', attempts = ?2, next_attempt_at = ?3,
                 last_error_class = ?4, updated_at = ?5
             WHERE event_id = ?1",
            params![
                event_id,
                i64::from(attempts),
                next_attempt_at.to_rfc3339(),
                sanitize_error_class(error_class),
                now.to_rfc3339()
            ],
        )?;
        self.state(event_id)?.ok_or(LedgerError::EventNotFound)
    }

    pub fn mark_failed(
        &mut self,
        event_id: &str,
        error_class: &str,
    ) -> Result<LedgerEntry, LedgerError> {
        let updated = self.connection.execute(
            "UPDATE ingestion_events
             SET state = 'failed', next_attempt_at = NULL,
                 last_error_class = ?2, updated_at = ?3
             WHERE event_id = ?1",
            params![
                event_id,
                sanitize_error_class(error_class),
                self.runtime.now().to_rfc3339()
            ],
        )?;
        if updated == 0 {
            return Err(LedgerError::EventNotFound);
        }
        self.state(event_id)?.ok_or(LedgerError::EventNotFound)
    }

    pub fn state(&self, event_id: &str) -> Result<Option<LedgerEntry>, LedgerError> {
        query_state(&self.connection, event_id)?
            .map(LedgerEntry::try_from)
            .transpose()
    }
}

struct RawEntry {
    event_id: String,
    dataset: String,
    dataset_generation: i64,
    state: String,
    attempts: i64,
    next_attempt_at: Option<String>,
    applied_entry_id: Option<String>,
    last_error_class: Option<String>,
    updated_at: String,
}

impl TryFrom<RawEntry> for LedgerEntry {
    type Error = LedgerError;

    fn try_from(raw: RawEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            event_id: raw.event_id,
            dataset: raw.dataset,
            dataset_generation: u64::try_from(raw.dataset_generation)
                .map_err(|_| LedgerError::Overflow)?,
            state: IngestionState::parse(&raw.state)?,
            attempts: u32::try_from(raw.attempts).map_err(|_| LedgerError::Overflow)?,
            next_attempt_at: raw
                .next_attempt_at
                .as_deref()
                .map(parse_timestamp)
                .transpose()?,
            applied_entry_id: raw.applied_entry_id,
            last_error_class: raw.last_error_class,
            updated_at: parse_timestamp(&raw.updated_at)?,
        })
    }
}

fn query_state(
    connection: &Connection,
    event_id: &str,
) -> Result<Option<RawEntry>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT event_id, dataset, dataset_generation, state, attempts,
                    next_attempt_at, applied_entry_id, last_error_class, updated_at
             FROM ingestion_events WHERE event_id = ?1",
            [event_id],
            |row| {
                Ok(RawEntry {
                    event_id: row.get(0)?,
                    dataset: row.get(1)?,
                    dataset_generation: row.get(2)?,
                    state: row.get(3)?,
                    attempts: row.get(4)?,
                    next_attempt_at: row.get(5)?,
                    applied_entry_id: row.get(6)?,
                    last_error_class: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .optional()
}

fn retry_base(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(31);
    Duration::from_secs(1u64 << shift).min(MAX_RETRY_DELAY)
}

fn sanitize_error_class(value: &str) -> String {
    let token = value
        .split(|character: char| character == ':' || character.is_whitespace())
        .next()
        .unwrap_or_default();
    let sanitized: String = token
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, LedgerError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| LedgerError::InvalidTimestamp)
}

fn set_private_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
