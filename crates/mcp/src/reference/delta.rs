use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::record::{REFERENCE_SCHEMA_VERSION, ReferenceRecord, hash_fields, sha256_bytes};
use super::{
    PreparedDocument, REFERENCE_DATASET, ReferenceError, ReferenceLayout, ReferenceLimits,
};
use crate::atomic_fs::{
    AtomicWriteOutcome, ReplaceMode, SyncOps, SystemSyncOps, write_atomic,
    write_atomic_with_permissions,
};

const JSON_FILE_LIMIT: u64 = 16 * 1024 * 1024;
const WRITER_WAIT: Duration = Duration::from_secs(5);
const WRITER_POLL: Duration = Duration::from_millis(10);
static BATCH_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReferenceSchema {
    schema_version: u32,
    dataset: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeltaHeadPayload {
    schema_version: u32,
    highest_committed_sequence: u64,
    batch_id: Option<String>,
    event_count: usize,
    aggregate_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaHead {
    pub schema_version: u32,
    pub highest_committed_sequence: u64,
    pub batch_id: Option<String>,
    pub event_count: usize,
    pub aggregate_bytes: u64,
    pub payload_sha256: String,
}

impl DeltaHead {
    fn new(
        highest_committed_sequence: u64,
        batch_id: Option<String>,
        event_count: usize,
        aggregate_bytes: u64,
    ) -> Result<Self, ReferenceError> {
        let payload = DeltaHeadPayload {
            schema_version: REFERENCE_SCHEMA_VERSION,
            highest_committed_sequence,
            batch_id,
            event_count,
            aggregate_bytes,
        };
        let payload_sha256 =
            sha256_bytes(&serde_json::to_vec(&payload).map_err(|_| ReferenceError::CorruptRecord)?);
        Ok(Self {
            schema_version: payload.schema_version,
            highest_committed_sequence: payload.highest_committed_sequence,
            batch_id: payload.batch_id,
            event_count: payload.event_count,
            aggregate_bytes: payload.aggregate_bytes,
            payload_sha256,
        })
    }

    pub fn verify_hash(&self) -> bool {
        Self::new(
            self.highest_committed_sequence,
            self.batch_id.clone(),
            self.event_count,
            self.aggregate_bytes,
        )
        .is_ok_and(|expected| {
            self.schema_version == REFERENCE_SCHEMA_VERSION
                && expected.payload_sha256 == self.payload_sha256
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaSnapshot {
    pub head: DeltaHead,
    pub records: Vec<ReferenceRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    Durable,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitReceipt {
    pub status: CommitStatus,
    pub batch_id: Option<String>,
    pub first_sequence: Option<u64>,
    pub highest_committed_sequence: u64,
    pub records: Vec<ReferenceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedEvent {
    sequence: u64,
    file_name: String,
    file_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedBatch {
    schema_version: u32,
    batch_id: String,
    previous_head: u64,
    head: DeltaHead,
    events: Vec<StagedEvent>,
}

#[derive(Clone)]
pub struct DeltaStore {
    layout: ReferenceLayout,
    limits: ReferenceLimits,
    sync: Arc<dyn SyncOps>,
}

impl DeltaStore {
    pub fn new(layout: ReferenceLayout, limits: ReferenceLimits) -> Self {
        Self::with_sync(layout, limits, Arc::new(SystemSyncOps))
    }

    pub fn with_sync(
        layout: ReferenceLayout,
        limits: ReferenceLimits,
        sync: Arc<dyn SyncOps>,
    ) -> Self {
        Self {
            layout,
            limits,
            sync,
        }
    }

    pub fn limits(&self) -> &ReferenceLimits {
        &self.limits
    }

    pub(crate) fn validate_schema(&self) -> Result<(), ReferenceError> {
        let schema: ReferenceSchema = read_json(&self.layout.schema)?;
        if schema.schema_version != REFERENCE_SCHEMA_VERSION || schema.dataset != REFERENCE_DATASET
        {
            return Err(ReferenceError::CorruptRecord);
        }
        Ok(())
    }

    pub(crate) fn validate_diagnostic_snapshot(
        &self,
        snapshot: &DeltaSnapshot,
    ) -> Result<(), ReferenceError> {
        enforce_backlog(
            &self.layout,
            &snapshot.records,
            &[],
            snapshot.head.highest_committed_sequence,
            &self.limits,
        )?;
        if snapshot.head.highest_committed_sequence == 0 {
            if snapshot.head.batch_id.is_some()
                || snapshot.head.event_count != 0
                || snapshot.head.aggregate_bytes != 0
                || !snapshot.records.is_empty()
            {
                return Err(ReferenceError::CorruptRecord);
            }
            return Ok(());
        }
        let batch_id = snapshot
            .head
            .batch_id
            .as_deref()
            .ok_or(ReferenceError::CorruptRecord)?;
        let event_count =
            u64::try_from(snapshot.head.event_count).map_err(|_| ReferenceError::CorruptRecord)?;
        if event_count == 0 || event_count > snapshot.head.highest_committed_sequence {
            return Err(ReferenceError::CorruptRecord);
        }
        let first_sequence = snapshot
            .head
            .highest_committed_sequence
            .checked_sub(event_count - 1)
            .ok_or(ReferenceError::CorruptRecord)?;
        let last_batch = snapshot
            .records
            .iter()
            .filter(|record| record.sequence >= first_sequence)
            .collect::<Vec<_>>();
        if last_batch.len() != snapshot.head.event_count
            || last_batch.iter().any(|record| record.batch_id != batch_id)
        {
            return Err(ReferenceError::CorruptRecord);
        }
        let aggregate_bytes = last_batch.iter().try_fold(0_u64, |total, record| {
            total
                .checked_add(u64::try_from(record.normalized_bytes).unwrap_or(u64::MAX))
                .ok_or(ReferenceError::CorruptRecord)
        })?;
        if aggregate_bytes != snapshot.head.aggregate_bytes {
            return Err(ReferenceError::CorruptRecord);
        }
        Ok(())
    }

    pub fn initialize(&self) -> Result<(), ReferenceError> {
        self.layout.ensure_admin_tree()?;
        let schema = ReferenceSchema {
            schema_version: REFERENCE_SCHEMA_VERSION,
            dataset: REFERENCE_DATASET.to_owned(),
        };
        install_public_json(
            &self.layout.schema,
            &schema,
            ReplaceMode::NoReplace,
            self.sync.as_ref(),
        )?;
        if read_json::<ReferenceSchema>(&self.layout.schema)? != schema {
            return Err(ReferenceError::CorruptRecord);
        }
        if !self.layout.delta_head.exists() {
            install_public_json(
                &self.layout.delta_head,
                &DeltaHead::new(0, None, 0, 0)?,
                ReplaceMode::NoReplace,
                self.sync.as_ref(),
            )?;
        }
        self.read_head().map(|_| ())
    }

    pub fn commit_batch(
        &self,
        documents: &[PreparedDocument],
    ) -> Result<CommitReceipt, ReferenceError> {
        validate_batch(documents, &self.limits)?;
        self.initialize()?;
        let _lock = WriterLock::acquire(&self.layout, Arc::clone(&self.sync))?;
        let old_head = self.read_head()?;
        let existing = self.snapshot_range(0, old_head.highest_committed_sequence)?;
        let mut latest = latest_by_source(&existing);
        let mut new_records = Vec::new();
        let mut receipt_records = Vec::with_capacity(documents.len());
        let batch_id = next_batch_id();
        let committed_at = Utc::now().to_rfc3339();
        let mut next_sequence = old_head
            .highest_committed_sequence
            .checked_add(1)
            .ok_or(ReferenceError::SequenceOverflow)?;

        for document in documents {
            if let Some(previous) = latest.get(&document.source_id)
                && previous.content_sha256 == document.content_sha256
            {
                receipt_records.push(previous.clone());
                continue;
            }
            let previous = latest.get(&document.source_id);
            let revision = previous.map_or(Ok(1), |record| {
                record
                    .revision
                    .checked_add(1)
                    .ok_or(ReferenceError::SequenceOverflow)
            })?;
            let record = ReferenceRecord::from_prepared(
                document,
                next_sequence,
                batch_id.clone(),
                revision,
                previous.map(|record| record.event_id.clone()),
                committed_at.clone(),
            );
            next_sequence = next_sequence
                .checked_add(1)
                .ok_or(ReferenceError::SequenceOverflow)?;
            latest.insert(document.source_id.clone(), record.clone());
            new_records.push(record);
        }

        if new_records.is_empty() {
            return Ok(CommitReceipt {
                status: CommitStatus::Unchanged,
                batch_id: None,
                first_sequence: None,
                highest_committed_sequence: old_head.highest_committed_sequence,
                records: receipt_records,
            });
        }
        enforce_backlog(
            &self.layout,
            &existing,
            &new_records,
            old_head.highest_committed_sequence,
            &self.limits,
        )?;
        let new_head = DeltaHead::new(
            new_records
                .last()
                .ok_or(ReferenceError::CorruptRecord)?
                .sequence,
            Some(batch_id.clone()),
            new_records.len(),
            new_records.iter().try_fold(0_u64, |total, record| {
                total
                    .checked_add(u64::try_from(record.normalized_bytes).unwrap_or(u64::MAX))
                    .ok_or(ReferenceError::SequenceOverflow)
            })?,
        )?;
        let stage = self.stage_batch(&old_head, &new_head, &new_records)?;
        self.install_staged_events(&stage)?;
        install_public_json(
            &self.layout.delta_head,
            &new_head,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        )?;
        cleanup_stage(&stage, self.sync.as_ref())?;
        receipt_records.extend(new_records.clone());
        receipt_records.sort_by_key(|record| record.sequence);
        Ok(CommitReceipt {
            status: CommitStatus::Durable,
            batch_id: Some(batch_id),
            first_sequence: new_records.first().map(|record| record.sequence),
            highest_committed_sequence: new_head.highest_committed_sequence,
            records: receipt_records,
        })
    }

    pub fn snapshot_after(&self, included_through: u64) -> Result<DeltaSnapshot, ReferenceError> {
        let head = self.read_head()?;
        if included_through > head.highest_committed_sequence {
            return Err(ReferenceError::CorruptRecord);
        }
        let records = self.snapshot_range(included_through, head.highest_committed_sequence)?;
        Ok(DeltaSnapshot { head, records })
    }

    pub fn event_path(&self, sequence: u64) -> Option<PathBuf> {
        find_event_path(&self.layout.delta_events, sequence)
            .ok()
            .flatten()
    }

    pub fn adopt_orphans(&self) -> Result<DeltaHead, ReferenceError> {
        self.initialize()?;
        let _lock = WriterLock::acquire(&self.layout, Arc::clone(&self.sync))?;
        let mut head = self.read_head()?;
        let mut stages = fs::read_dir(&self.layout.staging)?.collect::<Result<Vec<_>, _>>()?;
        stages.sort_by_key(fs::DirEntry::file_name);
        for stage_entry in stages {
            let stage_path = stage_entry.path();
            if !stage_entry.file_type()?.is_dir()
                || !stage_entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("delta-")
            {
                continue;
            }
            let batch = match read_json::<StagedBatch>(&stage_path.join("batch.json")) {
                Ok(batch) => batch,
                Err(_) => {
                    quarantine_stage(
                        &self.layout,
                        &stage_path,
                        None,
                        head.highest_committed_sequence,
                        self.sync.as_ref(),
                    )?;
                    return Err(ReferenceError::CorruptRecord);
                }
            };
            if batch.head.highest_committed_sequence <= head.highest_committed_sequence {
                cleanup_stage(&stage_path, self.sync.as_ref())?;
                continue;
            }
            if batch.previous_head != head.highest_committed_sequence
                || !batch.head.verify_hash()
                || !verify_staged_batch(&self.layout, &stage_path, &batch)?
            {
                quarantine_stage(
                    &self.layout,
                    &stage_path,
                    Some(&batch),
                    head.highest_committed_sequence,
                    self.sync.as_ref(),
                )?;
                return Err(ReferenceError::CorruptRecord);
            }
            install_batch_events(&self.layout, &stage_path, &batch, self.sync.as_ref())?;
            install_public_json(
                &self.layout.delta_head,
                &batch.head,
                ReplaceMode::Replace,
                self.sync.as_ref(),
            )?;
            head = batch.head;
            cleanup_stage(&stage_path, self.sync.as_ref())?;
        }
        Ok(head)
    }

    fn read_head(&self) -> Result<DeltaHead, ReferenceError> {
        let head: DeltaHead = read_json(&self.layout.delta_head)?;
        if !head.verify_hash() {
            return Err(ReferenceError::CorruptRecord);
        }
        Ok(head)
    }

    fn snapshot_range(
        &self,
        included_through: u64,
        committed_head: u64,
    ) -> Result<Vec<ReferenceRecord>, ReferenceError> {
        let mut records = Vec::new();
        for sequence in included_through.saturating_add(1)..=committed_head {
            let path = find_event_path(&self.layout.delta_events, sequence)?
                .ok_or(ReferenceError::CorruptRecord)?;
            let record: ReferenceRecord = read_json(&path)?;
            let expected_file_name = event_file_name(&record);
            if record.sequence != sequence
                || path.file_name().and_then(|name| name.to_str())
                    != Some(expected_file_name.as_str())
                || !record.verify()
            {
                return Err(ReferenceError::CorruptRecord);
            }
            records.push(record);
        }
        Ok(records)
    }

    fn stage_batch(
        &self,
        old_head: &DeltaHead,
        new_head: &DeltaHead,
        records: &[ReferenceRecord],
    ) -> Result<PathBuf, ReferenceError> {
        let batch_id = new_head
            .batch_id
            .as_deref()
            .ok_or(ReferenceError::CorruptRecord)?;
        let stage = self.layout.staging.join(format!("delta-{batch_id}"));
        fs::create_dir(&stage)?;
        set_mode(&stage, 0o700)?;
        let mut events = Vec::with_capacity(records.len());
        for record in records {
            let bytes = serde_json::to_vec(record).map_err(|_| ReferenceError::CorruptRecord)?;
            let file_name = event_file_name(record);
            let path = stage.join(&file_name);
            write_atomic(&path, &bytes, ReplaceMode::NoReplace, self.sync.as_ref())?;
            set_mode(&path, 0o444)?;
            events.push(StagedEvent {
                sequence: record.sequence,
                file_name,
                file_sha256: sha256_bytes(&bytes),
            });
        }
        let batch = StagedBatch {
            schema_version: REFERENCE_SCHEMA_VERSION,
            batch_id: batch_id.to_owned(),
            previous_head: old_head.highest_committed_sequence,
            head: new_head.clone(),
            events,
        };
        write_atomic(
            &stage.join("batch.json"),
            &serde_json::to_vec(&batch).map_err(|_| ReferenceError::CorruptRecord)?,
            ReplaceMode::NoReplace,
            self.sync.as_ref(),
        )?;
        self.sync.sync_directory(&stage)?;
        Ok(stage)
    }

    fn install_staged_events(&self, stage: &Path) -> Result<(), ReferenceError> {
        let batch: StagedBatch = read_json(&stage.join("batch.json"))?;
        install_batch_events(&self.layout, stage, &batch, self.sync.as_ref())
    }
}

fn validate_batch(
    documents: &[PreparedDocument],
    limits: &ReferenceLimits,
) -> Result<(), ReferenceError> {
    if documents.is_empty() {
        return Err(ReferenceError::InvalidInput);
    }
    if documents.len() > limits.max_batch_files {
        return Err(ReferenceError::TooManyFiles);
    }
    for document in documents {
        document.validate(limits)?;
    }
    let total = documents.iter().try_fold(0_usize, |total, document| {
        total
            .checked_add(document.normalized_bytes)
            .ok_or(ReferenceError::BatchTooLarge)
    })?;
    if total > limits.max_batch_bytes {
        return Err(ReferenceError::BatchTooLarge);
    }
    Ok(())
}

fn latest_by_source(records: &[ReferenceRecord]) -> BTreeMap<String, ReferenceRecord> {
    let mut latest = BTreeMap::new();
    for record in records {
        latest.insert(record.source_id.clone(), record.clone());
    }
    latest
}

fn included_through(layout: &ReferenceLayout) -> Result<u64, ReferenceError> {
    let bytes = match fs::read(&layout.current) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(ReferenceError::Io(error)),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| ReferenceError::CorruptRecord)?;
    value
        .get("included_through")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ReferenceError::CorruptRecord)
}

fn enforce_backlog(
    layout: &ReferenceLayout,
    existing: &[ReferenceRecord],
    additions: &[ReferenceRecord],
    old_head: u64,
    limits: &ReferenceLimits,
) -> Result<(), ReferenceError> {
    let included = included_through(layout)?;
    if included > old_head {
        return Err(ReferenceError::CorruptRecord);
    }
    let pending = existing
        .iter()
        .filter(|record| record.sequence > included)
        .chain(additions.iter());
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    for record in pending {
        count = count
            .checked_add(1)
            .ok_or(ReferenceError::SequenceOverflow)?;
        bytes = bytes
            .checked_add(u64::try_from(record.normalized_bytes).unwrap_or(u64::MAX))
            .ok_or(ReferenceError::SequenceOverflow)?;
    }
    if count > limits.max_pending_events || bytes > limits.max_pending_bytes {
        return Err(ReferenceError::BacklogLimit);
    }
    Ok(())
}

fn install_public_json<T: Serialize>(
    path: &Path,
    value: &T,
    replace: ReplaceMode,
    sync: &dyn SyncOps,
) -> Result<AtomicWriteOutcome, ReferenceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ReferenceError::CorruptRecord)?;
    write_atomic_with_permissions(path, &bytes, replace, 0o755, 0o444, sync)
        .map_err(ReferenceError::Atomic)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ReferenceError> {
    let metadata = fs::symlink_metadata(path).map_err(ReferenceError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > JSON_FILE_LIMIT
    {
        return Err(ReferenceError::CorruptRecord);
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|_| ReferenceError::CorruptRecord)
}

fn event_file_name(record: &ReferenceRecord) -> String {
    let digest = record
        .event_id
        .strip_prefix("sha256:")
        .unwrap_or(&record.event_id);
    format!("{:020}-{digest}.json", record.sequence)
}

fn find_event_path(directory: &Path, sequence: u64) -> Result<Option<PathBuf>, ReferenceError> {
    let prefix = format!("{sequence:020}-");
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            matches.push(entry.path());
        }
    }
    if matches.len() > 1 {
        return Err(ReferenceError::CorruptRecord);
    }
    Ok(matches.pop())
}

fn install_batch_events(
    layout: &ReferenceLayout,
    stage: &Path,
    batch: &StagedBatch,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    for event in &batch.events {
        if !is_safe_event_file_name(&event.file_name) {
            return Err(ReferenceError::CorruptRecord);
        }
        let staged = stage.join(&event.file_name);
        let destination = layout.delta_events.join(&event.file_name);
        if destination.exists() {
            let metadata = fs::symlink_metadata(&destination)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ReferenceError::CorruptRecord);
            }
            if sha256_bytes(&fs::read(&destination)?) != event.file_sha256 {
                return Err(ReferenceError::CorruptRecord);
            }
            continue;
        }
        if sha256_bytes(&fs::read(&staged)?) != event.file_sha256 {
            return Err(ReferenceError::CorruptRecord);
        }
        sync.before_rename(&staged, &destination)?;
        fs::rename(&staged, &destination)?;
        sync.sync_directory(&layout.delta_events)?;
    }
    Ok(())
}

fn verify_staged_batch(
    layout: &ReferenceLayout,
    stage: &Path,
    batch: &StagedBatch,
) -> Result<bool, ReferenceError> {
    if batch.schema_version != REFERENCE_SCHEMA_VERSION
        || batch.events.len() != batch.head.event_count
        || batch.head.batch_id.as_deref() != Some(batch.batch_id.as_str())
    {
        return Ok(false);
    }
    let mut expected = batch.previous_head.saturating_add(1);
    let mut aggregate_bytes = 0_u64;
    for event in &batch.events {
        if !is_safe_event_file_name(&event.file_name) {
            return Ok(false);
        }
        if event.sequence != expected {
            return Ok(false);
        }
        expected = expected
            .checked_add(1)
            .ok_or(ReferenceError::SequenceOverflow)?;
        let public = layout.delta_events.join(&event.file_name);
        let staged = stage.join(&event.file_name);
        let selected = if public.exists() { &public } else { &staged };
        let bytes = match fs::read(selected) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(false),
        };
        if sha256_bytes(&bytes) != event.file_sha256 {
            return Ok(false);
        }
        let record: ReferenceRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(_) => return Ok(false),
        };
        if record.sequence != event.sequence
            || record.batch_id != batch.batch_id
            || event.file_name != event_file_name(&record)
            || !record.verify()
        {
            return Ok(false);
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(u64::try_from(record.normalized_bytes).unwrap_or(u64::MAX))
            .ok_or(ReferenceError::SequenceOverflow)?;
    }
    Ok(aggregate_bytes == batch.head.aggregate_bytes
        && batch
            .events
            .last()
            .is_some_and(|event| event.sequence == batch.head.highest_committed_sequence))
}

fn cleanup_stage(stage: &Path, sync: &dyn SyncOps) -> Result<(), ReferenceError> {
    match fs::remove_dir_all(stage) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ReferenceError::Io(error)),
    }
    if let Some(parent) = stage.parent() {
        sync.sync_directory(parent)?;
    }
    Ok(())
}

fn quarantine_stage(
    layout: &ReferenceLayout,
    stage: &Path,
    batch: Option<&StagedBatch>,
    committed_head: u64,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    if let Some(batch) = batch {
        for event in &batch.events {
            if event.sequence <= committed_head || !is_safe_event_file_name(&event.file_name) {
                continue;
            }
            let public = layout.delta_events.join(&event.file_name);
            let Ok(metadata) = fs::symlink_metadata(&public) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let Ok(bytes) = fs::read(&public) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ReferenceRecord>(&bytes) else {
                continue;
            };
            if sha256_bytes(&bytes) != event.file_sha256
                || record.sequence != event.sequence
                || record.batch_id != batch.batch_id
                || event.file_name != event_file_name(&record)
                || !record.verify()
            {
                continue;
            }
            let quarantined = stage.join(format!("orphan-{}", event.file_name));
            if quarantined.exists() {
                return Err(ReferenceError::CorruptRecord);
            }
            fs::rename(public, quarantined)?;
        }
        sync.sync_directory(&layout.delta_events)?;
    }
    let quarantine = layout
        .staging
        .join(format!("quarantine-{}", next_batch_id()));
    fs::rename(stage, quarantine)?;
    sync.sync_directory(&layout.staging)?;
    Ok(())
}

fn is_safe_event_file_name(file_name: &str) -> bool {
    let path = Path::new(file_name);
    path.parent()
        .is_some_and(|parent| parent.as_os_str().is_empty())
        && path.file_name().is_some_and(|name| name == file_name)
}

fn next_batch_id() -> String {
    let nonce = BATCH_NONCE.fetch_add(1, Ordering::Relaxed).to_be_bytes();
    let timestamp = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .to_be_bytes();
    let pid = std::process::id().to_be_bytes();
    hash_fields(&[&pid, &timestamp, &nonce])
        .trim_start_matches("sha256:")
        .to_owned()
}

#[derive(Debug, Serialize, Deserialize)]
struct WriterOwner {
    nonce: String,
    pid: u32,
    started_at: String,
}

struct WriterLock {
    path: PathBuf,
    nonce: String,
    sync: Arc<dyn SyncOps>,
}

impl WriterLock {
    fn acquire(layout: &ReferenceLayout, sync: Arc<dyn SyncOps>) -> Result<Self, ReferenceError> {
        let deadline = Instant::now() + WRITER_WAIT;
        loop {
            match fs::create_dir(&layout.delta_lock) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(ReferenceError::WriterBusy);
                    }
                    thread::sleep(WRITER_POLL);
                }
                Err(error) => return Err(ReferenceError::Io(error)),
            }
        }
        if let Err(error) = set_mode(&layout.delta_lock, 0o700) {
            cleanup_failed_lock(&layout.delta_lock, sync.as_ref());
            return Err(error);
        }
        if let Some(parent) = layout.delta_lock.parent()
            && let Err(error) = sync.sync_directory(parent)
        {
            cleanup_failed_lock(&layout.delta_lock, sync.as_ref());
            return Err(ReferenceError::Io(error));
        }
        let nonce = next_batch_id();
        let owner = WriterOwner {
            nonce: nonce.clone(),
            pid: std::process::id(),
            started_at: Utc::now().to_rfc3339(),
        };
        if let Err(error) = write_atomic(
            &layout.delta_lock.join("owner.json"),
            &serde_json::to_vec(&owner).map_err(|_| ReferenceError::CorruptRecord)?,
            ReplaceMode::NoReplace,
            sync.as_ref(),
        ) {
            cleanup_failed_lock(&layout.delta_lock, sync.as_ref());
            return Err(ReferenceError::Atomic(error));
        }
        Ok(Self {
            path: layout.delta_lock.clone(),
            nonce,
            sync,
        })
    }
}

fn cleanup_failed_lock(path: &Path, sync: &dyn SyncOps) {
    let _ = fs::remove_dir_all(path);
    if let Some(parent) = path.parent() {
        let _ = sync.sync_directory(parent);
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let owner_path = self.path.join("owner.json");
        let owned =
            read_json::<WriterOwner>(&owner_path).is_ok_and(|owner| owner.nonce == self.nonce);
        if !owned {
            return;
        }
        let _ = fs::remove_file(owner_path);
        let _ = fs::remove_dir(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = self.sync.sync_directory(parent);
        }
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), ReferenceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}
