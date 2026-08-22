#![cfg(feature = "runtime")]

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_mcp::engine::{
    ApplyPlan, ApplyReceipt, EngineFactory, ForgetReceipt, ForgetTarget, ImproveReceipt,
    MemoryEngine, RecallRequest, RecallResponse, plan_event_application,
};
use cognee_mcp::event::{CaptureMetadata, EventEnvelope, EventKind};
use cognee_mcp::generation::GenerationStore;
use cognee_mcp::layout::StateLayout;
use cognee_mcp::lease::EngineLease;
use cognee_mcp::ledger::{IngestionState, Ledger};
use cognee_mcp::limits::ResourceLimits;
use cognee_mcp::spool::{Priority, Spool};
use cognee_mcp::worker::{
    BudgetUsage, DrainBudget, FaultPoint, TokenEstimator, Worker, WorkerRuntime,
};
use tempfile::tempdir;

#[derive(Default)]
struct EngineState {
    calls: Mutex<Vec<&'static str>>,
}

struct InspectingFactory {
    layout: StateLayout,
    event_id: String,
    state: Arc<EngineState>,
}

#[async_trait]
impl EngineFactory for InspectingFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        assert!(self.layout.locks.join("engine/owner.json").is_file());
        assert_eq!(spool_depths(&self.layout), (0, 1));
        assert_eq!(
            ledger_state(&self.layout, &self.event_id),
            IngestionState::Applying
        );
        self.state.calls.lock().expect("calls lock").push("open");
        Ok(Box::new(InspectingEngine {
            layout: self.layout.clone(),
            event_id: self.event_id.clone(),
            state: self.state.clone(),
        }))
    }
}

struct InspectingEngine {
    layout: StateLayout,
    event_id: String,
    state: Arc<EngineState>,
}

#[async_trait]
impl MemoryEngine for InspectingEngine {
    async fn contains_event(
        &mut self,
        dataset: &str,
        event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        assert_eq!(dataset, "agent_sessions");
        assert_eq!(event_id, self.event_id);
        assert_eq!(
            ledger_state(&self.layout, event_id),
            IngestionState::Applying
        );
        assert_eq!(spool_depths(&self.layout), (0, 1));
        self.state
            .calls
            .lock()
            .expect("calls lock")
            .push("contains");
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        assert_eq!(event.event_id, self.event_id);
        assert_eq!(
            ledger_state(&self.layout, &event.event_id),
            IngestionState::Applying
        );
        assert_eq!(spool_depths(&self.layout), (0, 1));
        self.state.calls.lock().expect("calls lock").push("apply");
        Ok(ApplyReceipt::new(Some("entry-1".to_owned())))
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("one event must not trigger an improve checkpoint")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        assert_eq!(
            ledger_state(&self.layout, &self.event_id),
            IngestionState::Committed
        );
        assert_eq!(spool_depths(&self.layout), (0, 0));
        assert!(self.layout.locks.join("engine/owner.json").is_file());
        self.state.calls.lock().expect("calls lock").push("close");
    }
}

#[tokio::test]
async fn commits_ledger_before_removing_processing_and_closes_before_releasing_lease() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let spool = Spool::new(layout.clone(), limits.clone());
    let event = event("a", EventKind::AfterAgent);
    spool
        .enqueue(&event, Priority::Normal)
        .expect("enqueue event");

    let state = Arc::new(EngineState::default());
    let factory = Arc::new(InspectingFactory {
        layout: layout.clone(),
        event_id: event.event_id.clone(),
        state: state.clone(),
    });
    let lease = EngineLease::new(
        layout.clone(),
        std::time::Duration::from_secs(u64::from(limits.lease_stale_seconds)),
    );
    let ledger = Ledger::open(layout.clone()).expect("open ledger");
    let mut worker = Worker::new(
        layout.clone(),
        spool,
        lease,
        ledger,
        factory,
        limits.clone(),
    );

    let report = worker.drain(DrainBudget::from_limits(&limits)).await;

    assert_eq!(report.committed, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(
        state.calls.lock().expect("calls lock").as_slice(),
        ["open", "contains", "apply", "close"]
    );
    assert!(!layout.locks.join("engine").exists());
}

struct CrashRuntime {
    fault: Mutex<Option<FaultPoint>>,
}

impl CrashRuntime {
    fn once(point: FaultPoint) -> Self {
        Self {
            fault: Mutex::new(Some(point)),
        }
    }
}

impl WorkerRuntime for CrashRuntime {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-19T20:00:01Z")
            .expect("fixture time")
            .with_timezone(&chrono::Utc)
    }

    fn check_fault(&self, point: FaultPoint) -> Result<(), cognee_mcp::error::AgentError> {
        let mut fault = self.fault.lock().expect("fault lock");
        if fault.as_ref() == Some(&point) {
            fault.take();
            return Err(cognee_mcp::error::AgentError::InjectedFault(point));
        }
        Ok(())
    }
}

#[derive(Default)]
struct DurableEngineState {
    applied: Mutex<HashSet<String>>,
    apply_calls: Mutex<usize>,
    contains_calls: Mutex<usize>,
    close_calls: Mutex<usize>,
}

struct DurableFactory {
    state: Arc<DurableEngineState>,
}

#[async_trait]
impl EngineFactory for DurableFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(DurableEngine {
            state: self.state.clone(),
        }))
    }
}

struct DurableEngine {
    state: Arc<DurableEngineState>,
}

#[derive(Default)]
struct CheckpointEngineState {
    improve_calls: Mutex<Vec<(String, Vec<String>)>>,
}

struct CheckpointFactory {
    state: Arc<CheckpointEngineState>,
}

#[async_trait]
impl EngineFactory for CheckpointFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(CheckpointEngine {
            state: self.state.clone(),
        }))
    }
}

struct CheckpointEngine {
    state: Arc<CheckpointEngineState>,
}

#[async_trait]
impl MemoryEngine for CheckpointEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        Ok(ApplyReceipt::new(Some(event.event_id.clone())))
    }

    async fn improve(
        &mut self,
        dataset: &str,
        session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        self.state
            .improve_calls
            .lock()
            .expect("improve calls lock")
            .push((dataset.to_owned(), session_ids.to_vec()));
        Ok(ImproveReceipt {
            sessions_persisted: session_ids.len(),
        })
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        Ok(RecallResponse::default())
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {}
}

#[tokio::test]
async fn completed_turn_checkpoint_survives_workers_and_improves_once_at_threshold() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let mut limits = ResourceLimits::default();
    limits.max_events_per_drain = 1;
    limits.improve_every = 2;
    let spool = Spool::new(layout.clone(), limits.clone());
    let mut first_turn = event_with_payload(
        "6",
        EventKind::AfterAgent,
        serde_json::json!({"prompt": "first", "prompt_response": "one"}),
    );
    first_turn.session_id = "session-a".to_owned();
    let mut second_turn = event_with_payload(
        "7",
        EventKind::AfterAgent,
        serde_json::json!({"prompt": "second", "prompt_response": "two"}),
    );
    second_turn.session_id = "session-b".to_owned();
    spool
        .enqueue(&first_turn, Priority::Normal)
        .expect("enqueue first turn");
    spool
        .enqueue(&second_turn, Priority::Normal)
        .expect("enqueue second turn");
    let state = Arc::new(CheckpointEngineState::default());
    let factory = Arc::new(CheckpointFactory {
        state: state.clone(),
    });

    let first_report = worker_for(&layout, &limits, factory.clone())
        .drain(DrainBudget::from_limits(&limits))
        .await;
    assert_eq!(first_report.committed, 1);
    assert_eq!(first_report.improved, 0);
    assert!(
        state
            .improve_calls
            .lock()
            .expect("improve calls lock")
            .is_empty()
    );

    let second_report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;
    assert_eq!(second_report.committed, 1);
    assert_eq!(second_report.improved, 1);
    assert_eq!(
        state
            .improve_calls
            .lock()
            .expect("improve calls lock")
            .as_slice(),
        [(
            "agent_sessions".to_owned(),
            vec!["session-a".to_owned(), "session-b".to_owned()]
        )]
    );
}

#[derive(Default)]
struct RetryState {
    apply_calls: Mutex<usize>,
}

struct RetryFactory {
    state: Arc<RetryState>,
}

#[async_trait]
impl EngineFactory for RetryFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(RetryEngine {
            state: self.state.clone(),
        }))
    }
}

struct RetryEngine {
    state: Arc<RetryState>,
}

#[async_trait]
impl MemoryEngine for RetryEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        _event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        *self.state.apply_calls.lock().expect("apply calls lock") += 1;
        Err(cognee_mcp::error::AgentError::Retryable("proxy_429"))
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {}
}

#[tokio::test]
async fn retryable_proxy_error_backs_off_without_an_immediate_second_call() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let event = event_with_payload(
        "8",
        EventKind::BeforeAgent,
        serde_json::json!({"prompt": "retry later"}),
    );
    Spool::new(layout.clone(), limits.clone())
        .enqueue(&event, Priority::Normal)
        .expect("enqueue retry event");
    let state = Arc::new(RetryState::default());
    let factory = Arc::new(RetryFactory {
        state: state.clone(),
    });

    let first = worker_for(&layout, &limits, factory.clone())
        .drain(DrainBudget::from_limits(&limits))
        .await;
    assert_eq!(first.requeued, 1);
    assert_eq!(first.quarantined, 0);
    assert_eq!(first.last_error_class.as_deref(), Some("proxy_429"));
    assert_eq!(spool_depths(&layout), (1, 0));
    let retry = Ledger::open(layout.clone())
        .expect("open retry ledger")
        .state(&event.event_id)
        .expect("read retry state")
        .expect("retry row");
    assert_eq!(retry.state, IngestionState::Retry);
    assert_eq!(retry.attempts, 1);
    assert!(retry.next_attempt_at.is_some());

    let immediate = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;
    assert_eq!(immediate.selected, 0);
    assert_eq!(*state.apply_calls.lock().expect("apply calls lock"), 1);
    assert_eq!(spool_depths(&layout), (1, 0));
}

#[derive(Default)]
struct PoisonState {
    apply_calls: Mutex<Vec<String>>,
}

struct PoisonFactory {
    poison_event_id: String,
    state: Arc<PoisonState>,
}

#[async_trait]
impl EngineFactory for PoisonFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(PoisonEngine {
            poison_event_id: self.poison_event_id.clone(),
            state: self.state.clone(),
        }))
    }
}

struct PoisonEngine {
    poison_event_id: String,
    state: Arc<PoisonState>,
}

#[async_trait]
impl MemoryEngine for PoisonEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        self.state
            .apply_calls
            .lock()
            .expect("apply calls lock")
            .push(event.event_id.clone());
        if event.event_id == self.poison_event_id {
            Err(cognee_mcp::error::AgentError::Retryable("upstream_5xx"))
        } else {
            Ok(ApplyReceipt::new(Some(event.event_id.clone())))
        }
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {}
}

#[tokio::test]
async fn exhausted_poison_event_is_quarantined_without_blocking_its_peer() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let mut limits = ResourceLimits::default();
    limits.max_attempts = 1;
    let poison = event_with_payload(
        "9",
        EventKind::BeforeAgent,
        serde_json::json!({"prompt": "poison"}),
    );
    let healthy = event_with_payload(
        "a",
        EventKind::BeforeAgent,
        serde_json::json!({"prompt": "healthy"}),
    );
    let spool = Spool::new(layout.clone(), limits.clone());
    spool
        .enqueue(&poison, Priority::Normal)
        .expect("enqueue poison");
    spool
        .enqueue(&healthy, Priority::Normal)
        .expect("enqueue healthy");
    let state = Arc::new(PoisonState::default());
    let factory = Arc::new(PoisonFactory {
        poison_event_id: poison.event_id.clone(),
        state: state.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.quarantined, 1);
    assert_eq!(report.committed, 1);
    assert_eq!(
        state
            .apply_calls
            .lock()
            .expect("apply calls lock")
            .as_slice(),
        [poison.event_id, healthy.event_id]
    );
    let depths = Spool::new(layout, limits).depths().expect("spool depths");
    assert_eq!(
        (depths.pending, depths.processing, depths.failed),
        (0, 0, 1)
    );
}

struct CancellationFactory {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    close_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl EngineFactory for CancellationFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(CancellationEngine {
            entered: self.entered.lock().expect("entered lock").take(),
            close_calls: self.close_calls.clone(),
        }))
    }
}

struct CancellationEngine {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    close_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl MemoryEngine for CancellationEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        _event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
        std::future::pending().await
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        self.close_calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_the_drain_future_closes_engine_before_releasing_lease() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    Spool::new(layout.clone(), limits.clone())
        .enqueue(
            &event_with_payload(
                "b",
                EventKind::BeforeAgent,
                serde_json::json!({"prompt": "cancel me"}),
            ),
            Priority::Normal,
        )
        .expect("enqueue cancellation event");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let close_calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(CancellationFactory {
        entered: Mutex::new(Some(entered_tx)),
        close_calls: close_calls.clone(),
    });
    let mut worker = worker_for(&layout, &limits, factory);
    let task = tokio::spawn(async move { worker.drain(DrainBudget::from_limits(&limits)).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
        .await
        .expect("engine entered before timeout")
        .expect("engine entry signal");

    task.abort();
    assert!(
        task.await
            .expect_err("drain must be cancelled")
            .is_cancelled()
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while close_calls.load(Ordering::SeqCst) != 1 || layout.locks.join("engine").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation cleanup");

    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(spool_depths(&layout), (0, 1));
}

struct NeverOpenFactory {
    opens: Arc<AtomicUsize>,
}

#[async_trait]
impl EngineFactory for NeverOpenFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        panic!("a precommitted event must not open an engine")
    }
}

#[tokio::test]
async fn precommitted_ledger_row_removes_spool_file_without_opening_engine() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let event = event_with_payload(
        "c",
        EventKind::BeforeAgent,
        serde_json::json!({"prompt": "already committed"}),
    );
    Spool::new(layout.clone(), limits.clone())
        .enqueue(&event, Priority::Normal)
        .expect("enqueue committed event");
    let mut ledger = Ledger::open(layout.clone()).expect("open seed ledger");
    ledger
        .begin(&event.event_id, &event.dataset, event.dataset_generation)
        .expect("seed applying");
    ledger
        .mark_committed(&event.event_id, Some("existing-entry"))
        .expect("seed committed");
    drop(ledger);
    let opens = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(NeverOpenFactory {
        opens: opens.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.already_committed, 1);
    assert_eq!(report.committed, 0);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert_eq!(spool_depths(&layout), (0, 0));
}

#[tokio::test]
async fn precompress_forces_one_checkpoint_before_the_turn_threshold() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    Spool::new(layout.clone(), limits.clone())
        .enqueue(
            &event_with_payload(
                "d",
                EventKind::PreCompress,
                serde_json::json!({"trigger": "token_limit"}),
            ),
            Priority::High,
        )
        .expect("enqueue precompress");
    let state = Arc::new(CheckpointEngineState::default());
    let factory = Arc::new(CheckpointFactory {
        state: state.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.committed, 1);
    assert_eq!(report.improved, 1);
    assert_eq!(
        state
            .improve_calls
            .lock()
            .expect("improve calls lock")
            .as_slice(),
        [("agent_sessions".to_owned(), vec!["session-1".to_owned()])]
    );
}

struct NonceLossFactory {
    layout: StateLayout,
    apply_calls: Arc<AtomicUsize>,
    close_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl EngineFactory for NonceLossFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(NonceLossEngine {
            layout: self.layout.clone(),
            apply_calls: self.apply_calls.clone(),
            close_calls: self.close_calls.clone(),
        }))
    }
}

struct NonceLossEngine {
    layout: StateLayout,
    apply_calls: Arc<AtomicUsize>,
    close_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl MemoryEngine for NonceLossEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        let owner_path = self.layout.locks.join("engine/owner.json");
        let mut owner: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&owner_path).expect("owner bytes"))
                .expect("owner json");
        owner["nonce"] = serde_json::Value::String("replacement-nonce".to_owned());
        std::fs::write(
            owner_path,
            serde_json::to_vec(&owner).expect("tampered owner json"),
        )
        .expect("tamper nonce");
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        _event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        self.apply_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ApplyReceipt::default())
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        self.close_calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn lost_nonce_after_external_call_blocks_apply_and_keeps_processing() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    Spool::new(layout.clone(), limits.clone())
        .enqueue(
            &event_with_payload(
                "e",
                EventKind::BeforeAgent,
                serde_json::json!({"prompt": "fence this"}),
            ),
            Priority::Normal,
        )
        .expect("enqueue fenced event");
    let apply_calls = Arc::new(AtomicUsize::new(0));
    let close_calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(NonceLossFactory {
        layout: layout.clone(),
        apply_calls: apply_calls.clone(),
        close_calls: close_calls.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.last_error_class.as_deref(), Some("lease_lost"));
    assert_eq!(apply_calls.load(Ordering::SeqCst), 0);
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(spool_depths(&layout), (0, 1));
}

#[async_trait]
impl MemoryEngine for DurableEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        *self.state.contains_calls.lock().expect("contains lock") += 1;
        Ok(self
            .state
            .applied
            .lock()
            .expect("applied lock")
            .contains(event_id))
    }

    async fn apply_event(
        &mut self,
        event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        *self.state.apply_calls.lock().expect("apply calls lock") += 1;
        self.state
            .applied
            .lock()
            .expect("applied lock")
            .insert(event.external_event_id());
        Ok(ApplyReceipt::new(Some("durable-entry".to_owned())))
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        *self.state.close_calls.lock().expect("close calls lock") += 1;
    }
}

#[tokio::test]
async fn crash_after_apply_restarts_through_contains_without_duplicate_apply() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let event = event("b", EventKind::BeforeAgent);
    Spool::new(layout.clone(), limits.clone())
        .enqueue(&event, Priority::Normal)
        .expect("enqueue event");
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(DurableFactory {
        state: engine_state.clone(),
    });

    let mut first = worker_for(&layout, &limits, factory.clone()).with_runtime(Arc::new(
        CrashRuntime::once(FaultPoint::AfterApplyBeforeLedgerCommit),
    ));
    let interrupted = first.drain(DrainBudget::from_limits(&limits)).await;
    assert_eq!(interrupted.committed, 0);
    assert_eq!(
        interrupted.last_error_class.as_deref(),
        Some("injected_fault")
    );
    assert_eq!(spool_depths(&layout), (0, 1));
    assert_eq!(
        ledger_state(&layout, &event.event_id),
        IngestionState::Applying
    );

    let mut restarted = worker_for(&layout, &limits, factory);
    let completed = restarted.drain(DrainBudget::from_limits(&limits)).await;

    assert_eq!(completed.committed, 1);
    assert_eq!(
        *engine_state.apply_calls.lock().expect("apply calls lock"),
        1
    );
    assert_eq!(
        *engine_state
            .contains_calls
            .lock()
            .expect("contains calls lock"),
        2
    );
    assert_eq!(
        *engine_state.close_calls.lock().expect("close calls lock"),
        2
    );
    assert_eq!(spool_depths(&layout), (0, 0));
    assert_eq!(
        ledger_state(&layout, &event.event_id),
        IngestionState::Committed
    );
}

#[tokio::test]
async fn late_event_from_a_superseded_generation_is_quarantined_without_engine_calls() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let spool = Spool::new(layout.clone(), limits.clone());
    GenerationStore::new(layout.clone())
        .advance_and_quarantine("agent_sessions", &spool)
        .expect("advance generation");
    let stale = event("9", EventKind::BeforeAgent);
    spool
        .enqueue(&stale, Priority::Normal)
        .expect("enqueue late stale event");
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(DurableFactory {
        state: engine_state.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.committed, 0);
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(*engine_state.apply_calls.lock().expect("apply calls"), 0);
    assert_eq!(
        *engine_state.contains_calls.lock().expect("contains calls"),
        0
    );
    assert_eq!(*engine_state.close_calls.lock().expect("close calls"), 0);
    assert!(
        Ledger::open(layout.clone())
            .expect("inspect ledger")
            .state(&stale.event_id)
            .expect("read ledger")
            .is_none()
    );
    let superseded = layout.spool_failed.join("superseded/generation-0");
    let quarantined = std::fs::read_dir(superseded)
        .expect("superseded directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("superseded entries");
    assert_eq!(quarantined.len(), 1);
}

struct GenerationAdvancingFactory {
    layout: StateLayout,
    limits: ResourceLimits,
    state: Arc<DurableEngineState>,
    point: GenerationAdvancePoint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GenerationAdvancePoint {
    Open,
    Apply,
}

#[async_trait]
impl EngineFactory for GenerationAdvancingFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        if self.point == GenerationAdvancePoint::Open {
            advance_generation(&self.layout, &self.limits);
        }
        Ok(Box::new(GenerationAdvancingEngine {
            layout: self.layout.clone(),
            limits: self.limits.clone(),
            state: self.state.clone(),
            point: self.point,
        }))
    }
}

struct GenerationAdvancingEngine {
    layout: StateLayout,
    limits: ResourceLimits,
    state: Arc<DurableEngineState>,
    point: GenerationAdvancePoint,
}

#[async_trait]
impl MemoryEngine for GenerationAdvancingEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        *self.state.contains_calls.lock().expect("contains calls") += 1;
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        _event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        *self.state.apply_calls.lock().expect("apply calls") += 1;
        if self.point == GenerationAdvancePoint::Apply {
            advance_generation(&self.layout, &self.limits);
        }
        Ok(ApplyReceipt::new(Some("generation-race-entry".to_owned())))
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        *self.state.close_calls.lock().expect("close calls") += 1;
    }
}

fn advance_generation(layout: &StateLayout, limits: &ResourceLimits) {
    let spool = Spool::new(layout.clone(), limits.clone());
    GenerationStore::new(layout.clone())
        .advance_and_quarantine("agent_sessions", &spool)
        .expect("advance generation during engine operation");
}

#[tokio::test]
async fn generation_change_after_engine_open_blocks_apply_and_commit() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let stale = event("8", EventKind::BeforeAgent);
    Spool::new(layout.clone(), limits.clone())
        .enqueue(&stale, Priority::Normal)
        .expect("enqueue event");
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(GenerationAdvancingFactory {
        layout: layout.clone(),
        limits: limits.clone(),
        state: engine_state.clone(),
        point: GenerationAdvancePoint::Open,
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.committed, 0);
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(*engine_state.apply_calls.lock().expect("apply calls"), 0);
    assert_eq!(
        *engine_state.contains_calls.lock().expect("contains calls"),
        1
    );
    assert_ne!(
        ledger_state(&layout, &stale.event_id),
        IngestionState::Committed
    );
    assert_eq!(
        std::fs::read_dir(layout.spool_failed.join("superseded/generation-0"))
            .expect("superseded directory")
            .count(),
        1
    );
}

#[tokio::test]
async fn generation_change_after_apply_stops_before_a_second_dataset_event() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let stale = event("5", EventKind::BeforeAgent);
    let mut second = event("6", EventKind::BeforeAgent);
    second.dataset = "project_notes".to_owned();
    let spool = Spool::new(layout.clone(), limits.clone());
    spool
        .enqueue(&stale, Priority::Normal)
        .expect("enqueue event");
    spool
        .enqueue(&second, Priority::Normal)
        .expect("enqueue second dataset event");
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(GenerationAdvancingFactory {
        layout: layout.clone(),
        limits: limits.clone(),
        state: engine_state.clone(),
        point: GenerationAdvancePoint::Apply,
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.committed, 0);
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(*engine_state.apply_calls.lock().expect("apply calls"), 1);
    assert_ne!(
        ledger_state(&layout, &stale.event_id),
        IngestionState::Committed
    );
    assert_eq!(
        std::fs::read_dir(layout.spool_failed.join("superseded/generation-0"))
            .expect("superseded directory")
            .count(),
        1
    );
    assert_eq!(spool_depths(&layout), (1, 0));
}

#[tokio::test]
async fn repeated_session_end_callbacks_apply_one_external_effect() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let mut first = event_with_payload(
        "1",
        EventKind::SessionEnd,
        serde_json::json!({"reason": "exit"}),
    );
    first.timestamp = "2026-08-20T04:26:26.588Z".to_owned();
    let mut second = event_with_payload(
        "2",
        EventKind::SessionEnd,
        serde_json::json!({"reason": "exit"}),
    );
    second.timestamp = "2026-08-20T04:26:26.953Z".to_owned();
    second.cwd = "/same-workspace-via-a-different-spelling".to_owned();
    let mut different_reason = event_with_payload(
        "3",
        EventKind::SessionEnd,
        serde_json::json!({"reason": "logout"}),
    );
    different_reason.timestamp = "2026-08-20T04:26:27.383Z".to_owned();
    different_reason.payload_hash = "e".repeat(64);
    let spool = Spool::new(layout.clone(), limits.clone());
    spool
        .enqueue(&first, Priority::Normal)
        .expect("enqueue first terminal callback");
    spool
        .enqueue(&second, Priority::Normal)
        .expect("enqueue repeated terminal callback");
    spool
        .enqueue(&different_reason, Priority::Normal)
        .expect("enqueue distinct terminal callback");
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(DurableFactory {
        state: engine_state.clone(),
    });

    let report = worker_for(&layout, &limits, factory)
        .drain(DrainBudget::from_limits(&limits))
        .await;

    assert_eq!(report.committed, 3);
    assert_eq!(
        *engine_state.apply_calls.lock().expect("apply calls lock"),
        2
    );
    assert_eq!(engine_state.applied.lock().expect("applied lock").len(), 2);
    assert_eq!(spool_depths(&layout), (0, 0));
    assert_eq!(
        ledger_state(&layout, &first.event_id),
        IngestionState::Committed
    );
    assert_eq!(
        ledger_state(&layout, &second.event_id),
        IngestionState::Committed
    );
    assert_eq!(
        ledger_state(&layout, &different_reason.event_id),
        IngestionState::Committed
    );
}

struct FixedUsagePerEvent(BudgetUsage);

impl TokenEstimator for FixedUsagePerEvent {
    fn estimate_event(&self, _event: &EventEnvelope) -> BudgetUsage {
        self.0
    }

    fn estimate_improve(&self, _session_ids: &[String]) -> BudgetUsage {
        BudgetUsage::default()
    }
}

#[tokio::test]
async fn stops_before_starting_an_event_that_would_exceed_each_call_or_token_budget() {
    let cases = [
        (
            "llm calls",
            BudgetUsage {
                llm_calls: 1,
                input_tokens: 0,
                output_tokens: 0,
            },
            1,
            u32::MAX,
            u32::MAX,
        ),
        (
            "input tokens",
            BudgetUsage {
                llm_calls: 0,
                input_tokens: 6,
                output_tokens: 0,
            },
            u32::MAX,
            10,
            u32::MAX,
        ),
        (
            "output tokens",
            BudgetUsage {
                llm_calls: 0,
                input_tokens: 0,
                output_tokens: 6,
            },
            u32::MAX,
            u32::MAX,
            10,
        ),
    ];

    for (name, usage, max_llm_calls, max_input_tokens, max_output_tokens) in cases {
        let temporary = tempdir().expect("temporary root");
        let layout = StateLayout::under(temporary.path().join("cognee"));
        let limits = ResourceLimits::default();
        let spool = Spool::new(layout.clone(), limits.clone());
        spool
            .enqueue(
                &event_with_payload(
                    "1",
                    EventKind::BeforeAgent,
                    serde_json::json!({"prompt": "first"}),
                ),
                Priority::Normal,
            )
            .expect("enqueue first");
        spool
            .enqueue(
                &event_with_payload(
                    "2",
                    EventKind::BeforeAgent,
                    serde_json::json!({"prompt": "second"}),
                ),
                Priority::Normal,
            )
            .expect("enqueue second");
        let engine_state = Arc::new(DurableEngineState::default());
        let factory = Arc::new(DurableFactory {
            state: engine_state.clone(),
        });
        let mut worker = worker_for(&layout, &limits, factory)
            .with_token_estimator(Arc::new(FixedUsagePerEvent(usage)));
        let mut budget = DrainBudget::from_limits(&limits);
        budget.max_llm_calls = max_llm_calls;
        budget.max_input_tokens = max_input_tokens;
        budget.max_output_tokens = max_output_tokens;

        let report = worker.drain(budget).await;

        assert!(report.budget_exhausted, "{name}");
        assert_eq!(report.committed, 1, "{name}");
        assert_eq!(report.usage, usage, "{name}");
        assert_eq!(
            *engine_state.apply_calls.lock().expect("apply calls lock"),
            1,
            "{name}"
        );
        assert_eq!(spool_depths(&layout), (1, 0), "{name}");
    }
}

#[tokio::test]
async fn selects_no_more_events_than_the_drain_budget_allows() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    let spool = Spool::new(layout.clone(), limits.clone());
    for (id, prompt) in [("3", "first"), ("4", "second")] {
        spool
            .enqueue(
                &event_with_payload(
                    id,
                    EventKind::BeforeAgent,
                    serde_json::json!({"prompt": prompt}),
                ),
                Priority::Normal,
            )
            .expect("enqueue bounded event");
    }
    let engine_state = Arc::new(DurableEngineState::default());
    let factory = Arc::new(DurableFactory {
        state: engine_state.clone(),
    });
    let mut worker = worker_for(&layout, &limits, factory);
    let mut budget = DrainBudget::from_limits(&limits);
    budget.max_events = 1;

    let report = worker.drain(budget).await;

    assert_eq!(report.selected, 1);
    assert_eq!(report.committed, 1);
    assert_eq!(
        *engine_state.apply_calls.lock().expect("apply calls lock"),
        1
    );
    assert_eq!(spool_depths(&layout), (1, 0));
}

struct SlowFactory {
    close_calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl EngineFactory for SlowFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, cognee_mcp::error::AgentError> {
        Ok(Box::new(SlowEngine {
            close_calls: self.close_calls.clone(),
        }))
    }
}

struct SlowEngine {
    close_calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl MemoryEngine for SlowEngine {
    async fn contains_event(
        &mut self,
        _dataset: &str,
        _event_id: &str,
    ) -> Result<bool, cognee_mcp::error::AgentError> {
        Ok(false)
    }

    async fn apply_event(
        &mut self,
        _event: &EventEnvelope,
    ) -> Result<ApplyReceipt, cognee_mcp::error::AgentError> {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(ApplyReceipt::new(Some("too-late".to_owned())))
    }

    async fn improve(
        &mut self,
        _dataset: &str,
        _session_ids: &[String],
    ) -> Result<ImproveReceipt, cognee_mcp::error::AgentError> {
        panic!("checkpoint is not due")
    }

    async fn recall(
        &mut self,
        _request: RecallRequest,
    ) -> Result<RecallResponse, cognee_mcp::error::AgentError> {
        panic!("recall is not part of a drain")
    }

    async fn forget(
        &mut self,
        _target: ForgetTarget,
    ) -> Result<ForgetReceipt, cognee_mcp::error::AgentError> {
        panic!("forget is not part of a drain")
    }

    async fn close(self: Box<Self>) {
        *self.close_calls.lock().expect("close calls lock") += 1;
    }
}

#[tokio::test]
async fn cancels_a_slow_engine_call_and_still_closes_and_releases() {
    let temporary = tempdir().expect("temporary root");
    let layout = StateLayout::under(temporary.path().join("cognee"));
    let limits = ResourceLimits::default();
    Spool::new(layout.clone(), limits.clone())
        .enqueue(
            &event_with_payload(
                "3",
                EventKind::BeforeAgent,
                serde_json::json!({"prompt": "do not hang"}),
            ),
            Priority::Normal,
        )
        .expect("enqueue slow event");
    let close_calls = Arc::new(Mutex::new(0));
    let factory = Arc::new(SlowFactory {
        close_calls: close_calls.clone(),
    });
    let mut worker = worker_for(&layout, &limits, factory);
    let mut budget = DrainBudget::from_limits(&limits);
    budget.max_duration = std::time::Duration::from_millis(200);
    let started = std::time::Instant::now();

    let report = worker.drain(budget).await;

    assert!(started.elapsed() < std::time::Duration::from_millis(600));
    assert!(report.budget_exhausted);
    assert_eq!(report.committed, 0);
    assert_eq!(report.last_error_class.as_deref(), Some("timeout"));
    assert_eq!(*close_calls.lock().expect("close calls lock"), 1);
    assert_eq!(spool_depths(&layout), (1, 0));
    assert!(!layout.locks.join("engine").exists());
}

#[test]
fn maps_every_event_to_the_exact_binding_payload_and_external_event_key() {
    let cases = [
        (
            EventKind::SessionStart,
            serde_json::json!({"source": "startup"}),
            serde_json::json!({
                "type": "trace",
                "originFunction": "apex.session_start",
                "status": "success",
                "methodParams": {"source": "startup"},
                "generateFeedbackWithLlm": false
            }),
        ),
        (
            EventKind::BeforeAgent,
            serde_json::json!({"prompt": "Fix the fleet hook"}),
            serde_json::json!({
                "type": "trace",
                "originFunction": "apex.before_agent",
                "status": "success",
                "methodParams": {"prompt": "Fix the fleet hook"},
                "generateFeedbackWithLlm": false
            }),
        ),
        (
            EventKind::AfterTool,
            serde_json::json!({
                "tool_name": "read_file",
                "tool_input": {"path": "/tmp/proof"},
                "tool_response": {"text": "evidence"}
            }),
            serde_json::json!({
                "type": "trace",
                "originFunction": "read_file",
                "status": "success",
                "methodParams": {"path": "/tmp/proof"},
                "methodReturnValue": {"text": "evidence"},
                "generateFeedbackWithLlm": false
            }),
        ),
        (
            EventKind::AfterAgent,
            serde_json::json!({
                "prompt": "What changed?",
                "prompt_response": "The worker commits exactly once."
            }),
            serde_json::json!({
                "type": "qa",
                "question": "What changed?",
                "answer": "The worker commits exactly once.",
                "context": ""
            }),
        ),
        (
            EventKind::PreCompress,
            serde_json::json!({"trigger": "token_limit"}),
            serde_json::json!({
                "type": "trace",
                "originFunction": "apex.pre_compress",
                "status": "success",
                "methodParams": {"trigger": "token_limit"},
                "generateFeedbackWithLlm": false
            }),
        ),
        (
            EventKind::SessionEnd,
            serde_json::json!({"reason": "complete"}),
            serde_json::json!({
                "type": "trace",
                "originFunction": "apex.session_end",
                "status": "success",
                "methodParams": {"reason": "complete"},
                "generateFeedbackWithLlm": false
            }),
        ),
    ];

    for (kind, payload, expected_entry) in cases {
        let event = event_with_payload("c", kind, payload);
        let plan = plan_event_application(&event).expect("event plan");
        let expected_external_event_id = match kind {
            EventKind::SessionEnd => {
                "cee436769a8e8c07fc24a3466f837b3ace7a5b0ae61b3b7316a74bd27543bf56".to_owned()
            }
            _ => "c".repeat(64),
        };
        assert_eq!(
            plan,
            ApplyPlan::SessionEntry {
                dataset: "agent_sessions".to_owned(),
                session_id: "session-1".to_owned(),
                entry: expected_entry,
                options: serde_json::json!({"externalEventId": expected_external_event_id}),
            },
            "{kind:?}"
        );
    }

    let permanent = event_with_payload(
        "d",
        EventKind::McpRemember,
        serde_json::json!({"data": "APEX uses the official hook shape"}),
    );
    assert_eq!(
        plan_event_application(&permanent).expect("permanent plan"),
        ApplyPlan::Remember {
            dataset: "agent_sessions".to_owned(),
            input: serde_json::json!({
                "type": "text",
                "text": "APEX uses the official hook shape"
            }),
            options: serde_json::json!({
                "externalEventId": "d".repeat(64),
                "selfImprovement": false
            }),
        }
    );

    let session = event_with_payload(
        "e",
        EventKind::McpRemember,
        serde_json::json!({
            "data": "Keep this in the active session",
            "session_id": "explicit-session",
            "self_improvement": true
        }),
    );
    assert_eq!(
        plan_event_application(&session).expect("session plan"),
        ApplyPlan::Remember {
            dataset: "agent_sessions".to_owned(),
            input: serde_json::json!({
                "type": "text",
                "text": "Keep this in the active session"
            }),
            options: serde_json::json!({
                "externalEventId": "e".repeat(64),
                "sessionId": "explicit-session",
                "selfImprovement": true
            }),
        }
    );
}

#[test]
fn repeated_session_end_plans_use_one_external_event_key() {
    let first = event_with_payload(
        "1",
        EventKind::SessionEnd,
        serde_json::json!({"reason": "exit"}),
    );
    let second = event_with_payload(
        "2",
        EventKind::SessionEnd,
        serde_json::json!({"reason": "exit"}),
    );

    let ApplyPlan::SessionEntry {
        options: first_options,
        ..
    } = plan_event_application(&first).expect("first terminal plan")
    else {
        panic!("session end must map to a session entry")
    };
    let ApplyPlan::SessionEntry {
        options: second_options,
        ..
    } = plan_event_application(&second).expect("second terminal plan")
    else {
        panic!("session end must map to a session entry")
    };

    assert_eq!(
        first_options["externalEventId"],
        second_options["externalEventId"]
    );
    assert_ne!(first_options["externalEventId"], first.event_id);
    assert_ne!(second_options["externalEventId"], second.event_id);
}

fn event(id_digit: &str, kind: EventKind) -> EventEnvelope {
    event_with_payload(
        id_digit,
        kind,
        serde_json::json!({
            "prompt": "What changed?",
            "prompt_response": "The worker now commits in order."
        }),
    )
}

fn event_with_payload(
    id_digit: &str,
    kind: EventKind,
    payload: serde_json::Value,
) -> EventEnvelope {
    EventEnvelope {
        schema_version: 1,
        event_id: id_digit.repeat(64),
        engineer: "alice".to_owned(),
        host: "host-a".to_owned(),
        session_id: "session-1".to_owned(),
        event: kind,
        timestamp: "2026-08-19T20:00:00Z".to_owned(),
        cwd: "/x/eng/project".to_owned(),
        dataset: "agent_sessions".to_owned(),
        dataset_generation: 0,
        payload_hash: "f".repeat(64),
        payload,
        capture: CaptureMetadata {
            original_bytes: 64,
            retained_bytes: 64,
            redaction_count: 0,
            truncation_count: 0,
            prompt_truncated: false,
            response_truncated: false,
            tool_input_truncated: false,
            tool_response_truncated: false,
            capture_degraded: false,
        },
    }
}

fn ledger_state(layout: &StateLayout, event_id: &str) -> IngestionState {
    Ledger::open(layout.clone())
        .expect("inspect ledger")
        .state(event_id)
        .expect("read ledger")
        .expect("ledger event")
        .state
}

fn spool_depths(layout: &StateLayout) -> (usize, usize) {
    let depths = Spool::new(layout.clone(), ResourceLimits::default())
        .depths()
        .expect("spool depths");
    (depths.pending, depths.processing)
}

fn worker_for(
    layout: &StateLayout,
    limits: &ResourceLimits,
    factory: Arc<dyn EngineFactory>,
) -> Worker {
    Worker::new(
        layout.clone(),
        Spool::new(layout.clone(), limits.clone()),
        EngineLease::new(
            layout.clone(),
            std::time::Duration::from_secs(u64::from(limits.lease_stale_seconds)),
        ),
        Ledger::open(layout.clone()).expect("open worker ledger"),
        factory,
        limits.clone(),
    )
}
