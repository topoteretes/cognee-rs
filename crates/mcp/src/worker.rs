//! Bounded, transient drain worker for durable memory events.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic_fs::{ReplaceMode, SystemSyncOps, write_atomic};
use crate::context::ContextCache;
use crate::engine::{EngineFactory, MemoryEngine, RecallRequest};
use crate::error::AgentError;
use crate::event::{EventEnvelope, EventKind, canonical_json};
use crate::generation::GenerationStore;
use crate::layout::StateLayout;
use crate::lease::{EngineLease, LeaseGuard};
use crate::ledger::{IngestionState, Ledger};
use crate::limits::ResourceLimits;
use crate::scheduler::{ScheduledEvent, select_batch};
use crate::spool::{ClaimedEvent, FailureDisposition, Spool};

const CONTEXT_RECALL_QUERY: &str = "stable preferences decisions constraints and project facts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    AfterApplyBeforeLedgerCommit,
    DuringImproveEntity(u32),
}

pub trait WorkerRuntime: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn check_fault(&self, point: FaultPoint) -> Result<(), AgentError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWorkerRuntime;

impl WorkerRuntime for SystemWorkerRuntime {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn check_fault(&self, _point: FaultPoint) -> Result<(), AgentError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    pub llm_calls: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl BudgetUsage {
    fn saturating_add(self, other: Self) -> Self {
        Self {
            llm_calls: self.llm_calls.saturating_add(other.llm_calls),
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
        }
    }
}

pub trait TokenEstimator: Send + Sync {
    fn estimate_event(&self, event: &EventEnvelope) -> BudgetUsage;
    fn estimate_improve(&self, session_ids: &[String]) -> BudgetUsage;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConservativeTokenEstimator;

impl TokenEstimator for ConservativeTokenEstimator {
    fn estimate_event(&self, event: &EventEnvelope) -> BudgetUsage {
        if event.event != EventKind::McpRemember {
            return BudgetUsage::default();
        }
        BudgetUsage {
            llm_calls: 1,
            input_tokens: estimate_tokens(&canonical_json(&event.payload)),
            output_tokens: 1_024,
        }
    }

    fn estimate_improve(&self, session_ids: &[String]) -> BudgetUsage {
        let sessions = u32::try_from(session_ids.len()).unwrap_or(u32::MAX);
        BudgetUsage {
            llm_calls: sessions,
            input_tokens: sessions.saturating_mul(6_000),
            output_tokens: sessions.saturating_mul(1_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainBudget {
    pub max_events: usize,
    pub max_duration: std::time::Duration,
    pub max_llm_calls: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
}

impl DrainBudget {
    pub fn from_limits(limits: &ResourceLimits) -> Self {
        Self {
            max_events: usize::try_from(limits.max_events_per_drain).unwrap_or(usize::MAX),
            max_duration: std::time::Duration::from_secs(u64::from(limits.drain_timeout_seconds)),
            max_llm_calls: limits.max_llm_calls,
            max_input_tokens: limits.max_input_tokens,
            max_output_tokens: limits.max_output_tokens,
        }
    }

    fn allows(self, usage: BudgetUsage) -> bool {
        usage.llm_calls <= self.max_llm_calls
            && usage.input_tokens <= self.max_input_tokens
            && usage.output_tokens <= self.max_output_tokens
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub lease_acquired: bool,
    pub selected: usize,
    pub committed: usize,
    pub improved: usize,
    pub already_committed: usize,
    pub recovered: usize,
    pub requeued: usize,
    pub quarantined: usize,
    pub failed: usize,
    pub budget_exhausted: bool,
    pub usage: BudgetUsage,
    pub last_error_class: Option<String>,
}

pub struct Worker {
    layout: StateLayout,
    spool: Spool,
    lease: EngineLease,
    ledger: Ledger,
    generations: GenerationStore,
    engine_factory: Arc<dyn EngineFactory>,
    runtime: Arc<dyn WorkerRuntime>,
    token_estimator: Arc<dyn TokenEstimator>,
    context_cache: ContextCache,
    limits: ResourceLimits,
}

struct DrainResources {
    lease: Option<LeaseGuard>,
    engine: Option<Box<dyn MemoryEngine>>,
}

impl DrainResources {
    fn new(lease: LeaseGuard) -> Self {
        Self {
            lease: Some(lease),
            engine: None,
        }
    }

    fn verify_lease(&self) -> Result<(), AgentError> {
        self.lease
            .as_ref()
            .ok_or(AgentError::Engine("drain_lease_missing"))?
            .verify()
            .map_err(AgentError::from)
    }

    async fn close(mut self) -> Result<(), AgentError> {
        if let Some(engine) = self.engine.take() {
            engine.close().await;
        }
        if let Some(lease) = self.lease.take() {
            lease.release()?;
        }
        Ok(())
    }
}

impl Drop for DrainResources {
    fn drop(&mut self) {
        let engine = self.engine.take();
        let lease = self.lease.take();
        if engine.is_none() {
            if let Some(lease) = lease {
                let _ = lease.release();
            }
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let _cleanup = runtime.spawn(async move {
                    if let Some(engine) = engine {
                        engine.close().await;
                    }
                    if let Some(lease) = lease {
                        let _ = lease.release();
                    }
                });
            }
            Err(_) => {
                drop(engine);
                if let Some(lease) = lease {
                    let _ = lease.release();
                }
            }
        }
    }
}

impl Worker {
    pub fn new(
        layout: StateLayout,
        spool: Spool,
        lease: EngineLease,
        ledger: Ledger,
        engine_factory: Arc<dyn EngineFactory>,
        limits: ResourceLimits,
    ) -> Self {
        let context_cache = ContextCache::new(layout.clone());
        let generations = GenerationStore::new(layout.clone());
        Self {
            layout,
            spool,
            lease,
            ledger,
            generations,
            engine_factory,
            runtime: Arc::new(SystemWorkerRuntime),
            token_estimator: Arc::new(ConservativeTokenEstimator),
            context_cache,
            limits,
        }
    }

    pub fn with_runtime(mut self, runtime: Arc<dyn WorkerRuntime>) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_token_estimator(mut self, token_estimator: Arc<dyn TokenEstimator>) -> Self {
        self.token_estimator = token_estimator;
        self
    }

    pub fn with_context_cache(mut self, context_cache: ContextCache) -> Self {
        self.context_cache = context_cache;
        self
    }

    pub async fn drain(&mut self, budget: DrainBudget) -> DrainReport {
        let mut report = DrainReport::default();
        let deadline = tokio::time::Instant::now() + budget.max_duration;
        let guard = match self.lease.try_acquire("drain") {
            Ok(Some(guard)) => {
                report.lease_acquired = true;
                guard
            }
            Ok(None) => return report,
            Err(error) => {
                record_error(&mut report, &AgentError::from(error));
                return report;
            }
        };
        let mut resources = DrainResources::new(guard);

        if let Err(error) = self.reconcile_committed_checkpoints() {
            record_error(&mut report, &error);
            let _ = resources.close().await;
            return report;
        }

        let recovery = self.spool.recover_processing(|event_id| {
            self.ledger
                .state(event_id)
                .map(|entry| entry.is_some_and(|entry| entry.state == IngestionState::Committed))
                .map_err(|error| crate::spool::SpoolError::Io(io::Error::other(error)))
        });
        match recovery {
            Ok(recovery) => {
                report.recovered = recovery.requeued + recovery.committed_removed;
                report.already_committed += recovery.committed_removed;
            }
            Err(error) => {
                record_error(&mut report, &AgentError::from(error));
                let _ = resources.close().await;
                return report;
            }
        }

        let files = match self.spool.pending_files() {
            Ok(files) => files,
            Err(error) => {
                record_error(&mut report, &AgentError::from(error));
                let _ = resources.close().await;
                return report;
            }
        };
        let mut candidates = Vec::with_capacity(files.len());
        for file in files {
            let record = match read_spool_record(&file.path) {
                Ok(record) => record,
                Err(error) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::from(error));
                    let _ = resources.close().await;
                    return report;
                }
            };
            let not_before = match record.not_before.as_deref() {
                Some(value) => match DateTime::parse_from_rfc3339(value) {
                    Ok(timestamp) => Some(timestamp.with_timezone(&Utc)),
                    Err(_) => {
                        report.failed += 1;
                        record_error(
                            &mut report,
                            &AgentError::from(crate::spool::SpoolError::InvalidTimestamp),
                        );
                        let _ = resources.close().await;
                        return report;
                    }
                },
                None => None,
            };
            candidates.push(ScheduledEvent { file, not_before });
        }
        let selected = select_batch(candidates, self.runtime.now(), budget.max_events);
        report.selected = selected.len();

        for file in selected {
            if tokio::time::Instant::now() >= deadline {
                report.budget_exhausted = true;
                record_error(&mut report, &AgentError::Timeout("drain"));
                break;
            }
            let event_estimate = match read_pending_event(&file) {
                Ok(event) => self.token_estimator.estimate_event(&event),
                Err(error) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::from(error));
                    break;
                }
            };
            let projected_usage = report.usage.saturating_add(event_estimate);
            if !budget.allows(projected_usage) {
                report.budget_exhausted = true;
                break;
            }
            report.usage = projected_usage;
            let claimed = match self.spool.claim(&file) {
                Ok(claimed) => claimed,
                Err(error) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::from(error));
                    break;
                }
            };
            let event = claimed.record.envelope.clone();
            let current_generation = match self.generations.current(&event.dataset) {
                Ok(generation) => generation,
                Err(_) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::Checkpoint("dataset_generation"));
                    break;
                }
            };
            if event.dataset_generation != current_generation {
                match self.spool.quarantine_claimed_superseded(claimed) {
                    Ok(()) => {
                        report.quarantined += 1;
                        continue;
                    }
                    Err(error) => {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::from(error));
                        break;
                    }
                }
            }
            let ledger_entry =
                match self
                    .ledger
                    .begin(&event.event_id, &event.dataset, event.dataset_generation)
                {
                    Ok(entry) => entry,
                    Err(error) => {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::from(error));
                        break;
                    }
                };

            if ledger_entry.state == IngestionState::Committed {
                match self.spool.commit(claimed) {
                    Ok(()) => {
                        report.already_committed += 1;
                        continue;
                    }
                    Err(error) => {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::from(error));
                        break;
                    }
                }
            }

            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            if resources.engine.is_none() {
                match before_deadline(
                    operation_deadline(deadline, self.limits.embedding_timeout_seconds),
                    "engine_open",
                    self.engine_factory.open(),
                )
                .await
                {
                    Ok(opened) => resources.engine = Some(opened),
                    Err(error) => {
                        if error.retry_class().is_some() {
                            match self.retry_claimed(claimed, &error, &mut report) {
                                Ok(FailureDisposition::Quarantined(_)) => continue,
                                Ok(FailureDisposition::Requeued(_)) => {}
                                Err(control_error) => {
                                    report.failed += 1;
                                    record_error(&mut report, &control_error);
                                }
                            }
                        } else {
                            report.failed += 1;
                            record_error(&mut report, &error);
                        }
                        report.budget_exhausted |= matches!(error, AgentError::Timeout(_));
                        break;
                    }
                }
            }
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let contains_result = {
                let Some(opened) = resources.engine.as_mut() else {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::Engine("engine_missing"));
                    break;
                };
                before_deadline(
                    operation_deadline(deadline, self.limits.embedding_timeout_seconds),
                    "contains_event",
                    opened.contains_event_for(&event),
                )
                .await
            };
            let contains = match contains_result {
                Ok(contains) => contains,
                Err(error) => {
                    if error.retry_class().is_some() {
                        match self.retry_claimed(claimed, &error, &mut report) {
                            Ok(FailureDisposition::Quarantined(_)) => continue,
                            Ok(FailureDisposition::Requeued(_)) => {}
                            Err(control_error) => {
                                report.failed += 1;
                                record_error(&mut report, &control_error);
                            }
                        }
                    } else {
                        report.failed += 1;
                        record_error(&mut report, &error);
                    }
                    report.budget_exhausted |= matches!(error, AgentError::Timeout(_));
                    break;
                }
            };
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let current_generation = match self.generations.current(&event.dataset) {
                Ok(generation) => generation,
                Err(_) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::Checkpoint("dataset_generation"));
                    break;
                }
            };
            if event.dataset_generation != current_generation {
                match self.spool.quarantine_claimed_superseded(claimed) {
                    Ok(()) => {
                        report.quarantined += 1;
                        continue;
                    }
                    Err(error) => {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::from(error));
                        break;
                    }
                }
            }
            let applied_entry_id = if contains {
                None
            } else {
                let apply_result = {
                    let Some(opened) = resources.engine.as_mut() else {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::Engine("engine_missing"));
                        break;
                    };
                    before_deadline(
                        operation_deadline(
                            deadline,
                            self.limits
                                .llm_timeout_seconds
                                .min(self.limits.embedding_timeout_seconds),
                        ),
                        "apply_event",
                        opened.apply_event(&event),
                    )
                    .await
                };
                match apply_result {
                    Ok(receipt) => {
                        if let Err(error) = self
                            .runtime
                            .check_fault(FaultPoint::AfterApplyBeforeLedgerCommit)
                        {
                            report.failed += 1;
                            record_error(&mut report, &error);
                            break;
                        }
                        receipt.entry_id
                    }
                    Err(error) => {
                        if error.retry_class().is_some() {
                            match self.retry_claimed(claimed, &error, &mut report) {
                                Ok(FailureDisposition::Quarantined(_)) => continue,
                                Ok(FailureDisposition::Requeued(_)) => {}
                                Err(control_error) => {
                                    report.failed += 1;
                                    record_error(&mut report, &control_error);
                                }
                            }
                        } else {
                            report.failed += 1;
                            record_error(&mut report, &error);
                        }
                        report.budget_exhausted |= matches!(error, AgentError::Timeout(_));
                        break;
                    }
                }
            };
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let current_generation = match self.generations.current(&event.dataset) {
                Ok(generation) => generation,
                Err(_) => {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::Checkpoint("dataset_generation"));
                    break;
                }
            };
            if event.dataset_generation != current_generation {
                match self.spool.quarantine_claimed_superseded(claimed) {
                    Ok(()) => {
                        report.quarantined += 1;
                        break;
                    }
                    Err(error) => {
                        report.failed += 1;
                        record_error(&mut report, &AgentError::from(error));
                        break;
                    }
                }
            }
            if let Err(error) = self
                .ledger
                .mark_committed(&event.event_id, applied_entry_id.as_deref())
            {
                report.failed += 1;
                record_error(&mut report, &AgentError::from(error));
                break;
            }
            if let Err(error) = record_checkpoint(&self.layout, &event) {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            if let Err(error) = self.spool.commit(claimed) {
                report.failed += 1;
                record_error(&mut report, &AgentError::from(error));
                break;
            }
            report.committed += 1;
        }

        let due = match due_checkpoints(&self.layout, self.limits.improve_every) {
            Ok(due) => due,
            Err(error) => {
                report.failed += 1;
                record_error(&mut report, &error);
                Vec::new()
            }
        };
        for (dataset, session_ids) in due {
            let estimate = self.token_estimator.estimate_improve(&session_ids);
            let projected_usage = report.usage.saturating_add(estimate);
            if !budget.allows(projected_usage) || tokio::time::Instant::now() >= deadline {
                report.budget_exhausted = true;
                break;
            }
            report.usage = projected_usage;
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            if resources.engine.is_none() {
                match before_deadline(
                    operation_deadline(deadline, self.limits.embedding_timeout_seconds),
                    "engine_open",
                    self.engine_factory.open(),
                )
                .await
                {
                    Ok(opened) => resources.engine = Some(opened),
                    Err(error) => {
                        report.failed += 1;
                        report.budget_exhausted |= matches!(error, AgentError::Timeout(_));
                        record_error(&mut report, &error);
                        break;
                    }
                }
            }
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let Some(opened) = resources.engine.as_mut() else {
                report.failed += 1;
                record_error(&mut report, &AgentError::Engine("engine_missing"));
                break;
            };
            match before_deadline(
                operation_deadline(
                    deadline,
                    self.limits
                        .llm_timeout_seconds
                        .min(self.limits.embedding_timeout_seconds),
                ),
                "improve",
                opened.improve(&dataset, &session_ids),
            )
            .await
            {
                Ok(_) => {}
                Err(error) => {
                    report.failed += 1;
                    report.budget_exhausted |= matches!(error, AgentError::Timeout(_));
                    record_error(&mut report, &error);
                    break;
                }
            }
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let mut context_fence_error = None;
            for session_id in &session_ids {
                if let Err(error) = resources.verify_lease() {
                    context_fence_error = Some(error);
                    break;
                }
                let recall = {
                    let Some(opened) = resources.engine.as_mut() else {
                        break;
                    };
                    before_deadline(
                        operation_deadline(deadline, self.limits.embedding_timeout_seconds),
                        "context_recall",
                        opened.recall(RecallRequest {
                            query: CONTEXT_RECALL_QUERY.to_owned(),
                            dataset: dataset.clone(),
                            session_id: Some(session_id.clone()),
                            top_k: 3,
                            search_type: Some("CHUNKS".to_owned()),
                            auto_route: false,
                        }),
                    )
                    .await
                };
                if let Err(error) = resources.verify_lease() {
                    context_fence_error = Some(error);
                    break;
                }
                if let Ok(response) = recall {
                    let memory = response
                        .items
                        .into_iter()
                        .map(|item| item.content)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = self.context_cache.write(session_id, &memory);
                }
            }
            if let Some(error) = context_fence_error {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            let bootstrap_recall = {
                let Some(opened) = resources.engine.as_mut() else {
                    report.failed += 1;
                    record_error(&mut report, &AgentError::Engine("engine_missing"));
                    break;
                };
                before_deadline(
                    operation_deadline(deadline, self.limits.embedding_timeout_seconds),
                    "bootstrap_recall",
                    opened.recall(RecallRequest {
                        query: CONTEXT_RECALL_QUERY.to_owned(),
                        dataset: dataset.clone(),
                        session_id: None,
                        top_k: 3,
                        search_type: Some("CHUNKS".to_owned()),
                        auto_route: false,
                    }),
                )
                .await
            };
            if let Err(error) = resources.verify_lease() {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            if let Ok(response) = bootstrap_recall {
                let memory = response
                    .items
                    .into_iter()
                    .map(|item| item.content)
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = self.context_cache.write_bootstrap(&dataset, &memory);
            }
            if let Err(error) = complete_checkpoint(&self.layout, &dataset) {
                report.failed += 1;
                record_error(&mut report, &error);
                break;
            }
            report.improved += 1;
        }

        if let Err(error) = resources.close().await {
            report.failed += 1;
            record_error(&mut report, &error);
        }
        report
    }

    fn reconcile_committed_checkpoints(&self) -> Result<(), AgentError> {
        self.layout
            .ensure_private()
            .map_err(|_| AgentError::Checkpoint("layout"))?;
        let entries = std::fs::read_dir(&self.layout.spool_processing)
            .map_err(|_| AgentError::Checkpoint("read_processing"))?;
        for entry in entries {
            let entry = entry.map_err(|_| AgentError::Checkpoint("read_processing"))?;
            if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                continue;
            }
            if !entry
                .file_type()
                .map_err(|_| AgentError::Checkpoint("read_processing"))?
                .is_file()
            {
                continue;
            }
            let record = read_spool_record(&entry.path()).map_err(AgentError::from)?;
            let committed = self
                .ledger
                .state(&record.envelope.event_id)?
                .is_some_and(|row| row.state == IngestionState::Committed);
            if committed {
                record_checkpoint(&self.layout, &record.envelope)?;
            }
        }
        Ok(())
    }

    fn retry_claimed(
        &mut self,
        claimed: ClaimedEvent,
        error: &AgentError,
        report: &mut DrainReport,
    ) -> Result<FailureDisposition, AgentError> {
        let class = error.retry_class().unwrap_or("engine");
        let retry = self
            .ledger
            .record_retry(&claimed.record.envelope.event_id, class)?;
        let not_before = retry
            .next_attempt_at
            .map(|timestamp| timestamp.to_rfc3339());
        let disposition = self.spool.fail(claimed, class, not_before)?;
        match disposition {
            FailureDisposition::Requeued(_) => report.requeued += 1,
            FailureDisposition::Quarantined(_) => {
                self.ledger.mark_failed(&retry.event_id, class)?;
                report.quarantined += 1;
                report.failed += 1;
            }
        }
        record_error(report, error);
        Ok(disposition)
    }
}

fn record_error(report: &mut DrainReport, error: &AgentError) {
    report.last_error_class = Some(error.class().to_owned());
}

fn read_pending_event(
    file: &crate::spool::SpoolFile,
) -> Result<EventEnvelope, crate::spool::SpoolError> {
    let record = read_spool_record(&file.path)?;
    if record.envelope.event_id != file.event_id {
        return Err(crate::spool::SpoolError::IdentityMismatch);
    }
    Ok(record.envelope)
}

fn read_spool_record(
    path: &std::path::Path,
) -> Result<crate::spool::SpoolRecord, crate::spool::SpoolError> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > crate::spool::MAX_EVENT_FILE_BYTES {
        return Err(crate::spool::SpoolError::EventTooLarge);
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn estimate_tokens(value: &str) -> u32 {
    let bytes = u32::try_from(value.len()).unwrap_or(u32::MAX);
    bytes.saturating_add(3) / 4
}

async fn before_deadline<T>(
    deadline: tokio::time::Instant,
    operation: &'static str,
    future: impl std::future::Future<Output = Result<T, AgentError>>,
) -> Result<T, AgentError> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(AgentError::Timeout(operation));
    }
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| AgentError::Timeout(operation))?
}

fn operation_deadline(
    drain_deadline: tokio::time::Instant,
    timeout_seconds: u32,
) -> tokio::time::Instant {
    let operation_deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(u64::from(timeout_seconds));
    drain_deadline.min(operation_deadline)
}

const CHECKPOINT_STATE_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CheckpointState {
    #[serde(default)]
    datasets: BTreeMap<String, DatasetCheckpoint>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DatasetCheckpoint {
    #[serde(default)]
    completed_turn_event_ids: BTreeSet<String>,
    #[serde(default)]
    forced_event_ids: BTreeSet<String>,
    #[serde(default)]
    session_ids: BTreeSet<String>,
}

fn record_checkpoint(layout: &StateLayout, event: &EventEnvelope) -> Result<(), AgentError> {
    if !matches!(event.event, EventKind::AfterAgent | EventKind::PreCompress) {
        return Ok(());
    }
    let mut state = load_checkpoint_state(layout)?;
    let checkpoint = state.datasets.entry(event.dataset.clone()).or_default();
    let inserted = match event.event {
        EventKind::AfterAgent => checkpoint
            .completed_turn_event_ids
            .insert(event.event_id.clone()),
        EventKind::PreCompress => checkpoint.forced_event_ids.insert(event.event_id.clone()),
        _ => false,
    };
    if inserted {
        checkpoint.session_ids.insert(event.session_id.clone());
        store_checkpoint_state(layout, &state)?;
    }
    Ok(())
}

fn due_checkpoints(
    layout: &StateLayout,
    improve_every: u32,
) -> Result<Vec<(String, Vec<String>)>, AgentError> {
    let threshold = usize::try_from(improve_every).unwrap_or(usize::MAX);
    let state = load_checkpoint_state(layout)?;
    Ok(state
        .datasets
        .into_iter()
        .filter(|(_, checkpoint)| {
            !checkpoint.forced_event_ids.is_empty()
                || checkpoint.completed_turn_event_ids.len() >= threshold
        })
        .map(|(dataset, checkpoint)| (dataset, checkpoint.session_ids.into_iter().collect()))
        .collect())
}

fn complete_checkpoint(layout: &StateLayout, dataset: &str) -> Result<(), AgentError> {
    let mut state = load_checkpoint_state(layout)?;
    state.datasets.remove(dataset);
    store_checkpoint_state(layout, &state)
}

fn load_checkpoint_state(layout: &StateLayout) -> Result<CheckpointState, AgentError> {
    layout
        .ensure_private()
        .map_err(|_| AgentError::Checkpoint("layout"))?;
    let path = layout.status.join("improve-checkpoints.json");
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CheckpointState::default());
        }
        Err(_) => return Err(AgentError::Checkpoint("read")),
    };
    if metadata.len() > CHECKPOINT_STATE_MAX_BYTES {
        return Err(AgentError::Checkpoint("too_large"));
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|_| AgentError::Checkpoint("read"))?)
        .map_err(|_| AgentError::Checkpoint("json"))
}

fn store_checkpoint_state(layout: &StateLayout, state: &CheckpointState) -> Result<(), AgentError> {
    let bytes = serde_json::to_vec(state).map_err(|_| AgentError::Checkpoint("json"))?;
    if bytes.len() as u64 > CHECKPOINT_STATE_MAX_BYTES {
        return Err(AgentError::Checkpoint("too_large"));
    }
    write_atomic(
        &layout.status.join("improve-checkpoints.json"),
        &bytes,
        ReplaceMode::Replace,
        &SystemSyncOps,
    )
    .map_err(|_| AgentError::Checkpoint("write"))?;
    Ok(())
}
