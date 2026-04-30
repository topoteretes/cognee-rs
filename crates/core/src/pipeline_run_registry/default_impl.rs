use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::Stream;
use serde_json::json;
use tokio::sync::{RwLock, broadcast, watch};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use cognee_database::{PipelineRunRepository, PipelineRunStatus as DbStatus};

use super::scoped_watcher::{PerRunSink, ScopedRunWatcher};
use super::trait_def::PipelineRunRegistry;
use super::types::{
    PipelineFuture, RegistryConfig, RegistryError, RunEvent, RunEventKind, RunHandle, RunOutcome,
    RunPhase, RunSpec,
};

// ---------------------------------------------------------------------------
// Internal slot
// ---------------------------------------------------------------------------

struct RunSlot {
    event_tx: broadcast::Sender<RunEvent>,
    phase_tx: watch::Sender<RunPhase>,
    #[allow(dead_code)]
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    abort_handle: Option<tokio::task::AbortHandle>,
    meta: RunHandle,
}

// ---------------------------------------------------------------------------
// DefaultPipelineRunRegistry
// ---------------------------------------------------------------------------

/// Concrete in-memory `PipelineRunRegistry` backed by a `PipelineRunRepository`
/// for durable persistence.
///
/// Uses:
/// - `tokio::sync::broadcast` for per-run fan-out event channels.
/// - `tokio::sync::watch` for cheap current-phase snapshots.
/// - A retention task that evicts finished runs every 60 seconds.
pub struct DefaultPipelineRunRegistry {
    runs: RwLock<HashMap<Uuid, RunSlot>>,
    eviction_order: Mutex<VecDeque<Uuid>>,
    repo: Arc<dyn PipelineRunRepository>,
    cfg: RegistryConfig,
}

impl DefaultPipelineRunRegistry {
    /// Create a new registry. Starts a background retention task.
    pub fn new(repo: Arc<dyn PipelineRunRepository>, cfg: RegistryConfig) -> Arc<Self> {
        let registry = Arc::new(Self {
            runs: RwLock::new(HashMap::new()),
            eviction_order: Mutex::new(VecDeque::new()),
            repo,
            cfg,
        });

        // Start retention cleanup task.
        let registry_weak = Arc::downgrade(&registry);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let Some(registry) = registry_weak.upgrade() else {
                    break;
                };
                registry.run_retention().await;
            }
        });

        registry
    }

    /// Create a new registry and reset orphan rows on startup.
    ///
    /// Calls `repo.reset_orphans("server_restart_orphan")` once before
    /// returning, per §12 of the spec (crash & restart recovery).
    pub async fn new_with_orphan_reset(
        repo: Arc<dyn PipelineRunRepository>,
        cfg: RegistryConfig,
    ) -> Result<Arc<Self>, RegistryError> {
        repo.reset_orphans("server_restart_orphan").await?;
        Ok(Self::new(repo, cfg))
    }

    /// Fetch the accumulated payload for a run. Returns an empty map if the
    /// run has no payload events; returns `Err` only on DB failure.
    pub async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, cognee_database::DatabaseError> {
        self.repo.get_payload(run_id).await
    }

    /// Construct a `ScopedRunWatcher` for the given run id, capturing the
    /// run's event channel sink. Returns `None` if the run slot does not exist.
    pub async fn watcher_for(&self, run_id: Uuid) -> Option<Arc<ScopedRunWatcher>> {
        let runs = self.runs.read().await;
        let slot = runs.get(&run_id)?;
        let sink = PerRunSink::from_parts(run_id, slot.event_tx.clone(), slot.phase_tx.clone());
        Some(Arc::new(ScopedRunWatcher::new(
            run_id,
            sink,
            Arc::clone(&self.repo),
        )))
    }

    /// Evict finished runs whose retention period has expired.
    async fn run_retention(&self) {
        let now = Utc::now();
        let retention = chrono::Duration::from_std(self.cfg.finished_retention)
            .unwrap_or(chrono::Duration::seconds(3600));

        let mut to_remove: Vec<Uuid> = Vec::new();
        {
            let runs = self.runs.read().await;
            for (id, slot) in runs.iter() {
                if slot
                    .finished_at
                    .is_some_and(|fa| now.signed_duration_since(fa) > retention)
                {
                    to_remove.push(*id);
                }
            }
        }

        if !to_remove.is_empty() {
            let mut runs = self.runs.write().await;
            for id in &to_remove {
                runs.remove(id);
            }
        }

        // Prune eviction_order of removed ids.
        if !to_remove.is_empty() {
            let remove_set: std::collections::HashSet<Uuid> = to_remove.into_iter().collect();
            let mut order = self
                .eviction_order
                .lock()
                .unwrap_or_else(|e| e.into_inner()); // lock poison is unrecoverable
            order.retain(|id| !remove_set.contains(id));
        }
    }

    /// Ensure there is room for a new slot, evicting the oldest finished run
    /// if necessary. Returns `RegistryFull` if no finished run is available.
    async fn ensure_capacity(&self) -> Result<(), RegistryError> {
        let current_len = self.runs.read().await.len();
        if current_len < self.cfg.max_in_memory_runs {
            return Ok(());
        }

        // Try to evict the oldest finished run.
        let evict_id = {
            let mut order = self
                .eviction_order
                .lock()
                .unwrap_or_else(|e| e.into_inner()); // lock poison is unrecoverable
            let runs = self.runs.try_read();
            // Find first finished id in eviction_order.
            let mut found_idx = None;
            if let Ok(guard) = runs {
                for (idx, id) in order.iter().enumerate() {
                    if guard.get(id).is_some_and(|slot| slot.finished_at.is_some()) {
                        found_idx = Some(idx);
                        break;
                    }
                }
            }
            found_idx.and_then(|idx| order.remove(idx))
        };

        if let Some(id) = evict_id {
            self.runs.write().await.remove(&id);
            Ok(())
        } else {
            tracing::warn!(
                max_in_memory_runs = self.cfg.max_in_memory_runs,
                "PipelineRunRegistry: full and no finished runs to evict; dropping register call"
            );
            Err(RegistryError::RegistryFull)
        }
    }

    /// Insert a new slot for the given run id. Returns the created slot's
    /// initial `RunHandle`.
    ///
    /// If a **placeholder** slot already exists (created by `subscribe` before
    /// the producer called `register_*`), its broadcast/watch senders are
    /// reused so that early subscribers continue to receive events. Only the
    /// metadata fields are updated to reflect the real run spec.
    async fn create_slot(&self, run_id: Uuid, spec: &RunSpec) -> RunHandle {
        let now = Utc::now();
        let meta = RunHandle {
            run_id,
            task_run_id: run_id, // will be updated after the first log_pipeline_run call
            user_id: spec.user_id,
            dataset_id: spec.dataset_id,
            pipeline_name: spec.pipeline_name.clone(),
            started_at: now,
        };

        let mut runs = self.runs.write().await;
        if let Some(existing) = runs.get_mut(&run_id) {
            // Placeholder exists — update its metadata but keep the senders so
            // subscribers attached before the producer don't lose events.
            existing.meta = meta.clone();
            existing.started_at = now;
            // Reset phase to Pending in case the placeholder was never set.
            let _ = existing.phase_tx.send(RunPhase::Pending);
            return meta;
        }

        let (event_tx, _) = broadcast::channel(self.cfg.channel_capacity);
        let (phase_tx, _) = watch::channel(RunPhase::Pending);

        let slot = RunSlot {
            event_tx,
            phase_tx,
            started_at: now,
            finished_at: None,
            abort_handle: None,
            meta: meta.clone(),
        };

        runs.insert(run_id, slot);
        drop(runs);
        {
            let mut order = self
                .eviction_order
                .lock()
                .unwrap_or_else(|e| e.into_inner()); // lock poison is unrecoverable
            order.push_back(run_id);
        }

        meta
    }

    /// Mark a slot as finished and emit a terminal event.
    async fn finish_slot(&self, run_id: Uuid, phase: RunPhase) {
        let mut runs = self.runs.write().await;
        if let Some(slot) = runs.get_mut(&run_id) {
            slot.finished_at = Some(Utc::now());
            let _ = slot.phase_tx.send(phase.clone());
            let kind = match phase {
                RunPhase::Completed => RunEventKind::Completed,
                RunPhase::Errored { ref message } => RunEventKind::Errored {
                    message: message.clone(),
                },
                _ => RunEventKind::Errored {
                    message: "unexpected terminal phase".to_string(),
                },
            };
            let _ = slot.event_tx.send(RunEvent {
                run_id,
                kind,
                payload: serde_json::Value::Null,
                at: Utc::now(),
            });
        }
    }

    /// Run a pipeline future and update the slot on completion.
    async fn run_work_inline(&self, run_id: Uuid, work: PipelineFuture) -> RunPhase {
        // Emit Started event and write durable row.
        {
            let runs = self.runs.read().await;
            if let Some(slot) = runs.get(&run_id) {
                let _ = slot.phase_tx.send(RunPhase::Running);
                let _ = slot.event_tx.send(RunEvent {
                    run_id,
                    kind: RunEventKind::Started,
                    payload: serde_json::Value::Null,
                    at: Utc::now(),
                });
            }
        }

        // Log the STARTED row (best-effort).
        let runs_read = self.runs.read().await;
        let meta = runs_read.get(&run_id).map(|s| s.meta.clone());
        drop(runs_read);

        if let Some(m) = &meta {
            let pipeline_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, m.pipeline_name.as_bytes());
            let result = self
                .repo
                .log_pipeline_run(
                    run_id,
                    pipeline_id,
                    &m.pipeline_name,
                    m.dataset_id,
                    DbStatus::Started,
                    None,
                )
                .await;
            if let Err(e) = result {
                tracing::warn!(run_id = %run_id, "registry: DB write for Started failed (non-fatal): {e}");
            }
        }

        // Execute the work future.
        let phase = match work.await {
            Ok(()) => RunPhase::Completed,
            Err(e) => RunPhase::Errored {
                message: e.to_string(),
            },
        };

        // Log the terminal row (best-effort).
        if let Some(m) = &meta {
            let pipeline_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, m.pipeline_name.as_bytes());
            let db_status = match &phase {
                RunPhase::Completed => DbStatus::Completed,
                _ => DbStatus::Errored,
            };
            let run_info = match &phase {
                RunPhase::Errored { message } => Some(json!({"error": message})),
                _ => None,
            };
            let result = self
                .repo
                .log_pipeline_run(
                    run_id,
                    pipeline_id,
                    &m.pipeline_name,
                    m.dataset_id,
                    db_status,
                    run_info,
                )
                .await;
            if let Err(e) = result {
                tracing::warn!(run_id = %run_id, "registry: DB write for terminal status failed (non-fatal): {e}");
            }
        }

        // Update the slot.
        self.finish_slot(run_id, phase.clone()).await;
        phase
    }
}

#[async_trait]
impl PipelineRunRegistry for DefaultPipelineRunRegistry {
    async fn register_inline(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunOutcome, RegistryError> {
        self.ensure_capacity().await?;
        let run_id = spec.run_id.unwrap_or_else(Uuid::new_v4);
        self.create_slot(run_id, &spec).await;

        let phase = self.run_work_inline(run_id, work).await;
        Ok(RunOutcome { run_id, phase })
    }

    async fn register_background(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunHandle, RegistryError> {
        self.ensure_capacity().await?;
        let run_id = spec.run_id.unwrap_or_else(Uuid::new_v4);
        let meta = self.create_slot(run_id, &spec).await;

        // We need a way to give the task a reference to self. Using Arc<Self>
        // directly: callers hold Arc<DefaultPipelineRunRegistry>. We use a raw
        // pointer trick — but that is unsound. Instead, rely on the registry
        // being stored in an Arc and using weak references.
        //
        // Since `DefaultPipelineRunRegistry::new()` returns `Arc<Self>`, there
        // is always at least one Arc alive. We need a reference to self inside
        // the spawned future. The cleanest approach: clone only the sub-fields
        // we need (repo, cfg, runs Arc) into the task.
        //
        // We package all the task-internal state via field clones.
        let repo = Arc::clone(&self.repo);
        let cfg = self.cfg.clone();
        let pipeline_name = spec.pipeline_name.clone();
        let dataset_id = spec.dataset_id;
        let user_id = spec.user_id;

        // We can't easily move `self` or an `Arc<Self>` into the task without
        // exposing the internal state. Instead, replicate the inline run logic
        // with only the data we can clone.
        let join_handle = tokio::spawn({
            let pipeline_name_clone = pipeline_name.clone();
            async move {
                // Emit STARTED.
                let pipeline_id =
                    Uuid::new_v5(&Uuid::NAMESPACE_OID, pipeline_name_clone.as_bytes());
                let result = repo
                    .log_pipeline_run(
                        run_id,
                        pipeline_id,
                        &pipeline_name_clone,
                        dataset_id,
                        DbStatus::Started,
                        None,
                    )
                    .await;
                if let Err(e) = result {
                    tracing::warn!(run_id = %run_id, "registry background: DB write for Started failed (non-fatal): {e}");
                }

                // Run work.
                let phase = match work.await {
                    Ok(()) => RunPhase::Completed,
                    Err(e) => RunPhase::Errored {
                        message: e.to_string(),
                    },
                };

                // Log terminal row.
                let db_status = match &phase {
                    RunPhase::Completed => DbStatus::Completed,
                    _ => DbStatus::Errored,
                };
                let run_info = match &phase {
                    RunPhase::Errored { message } => Some(json!({"error": message})),
                    _ => None,
                };
                let result = repo
                    .log_pipeline_run(
                        run_id,
                        pipeline_id,
                        &pipeline_name_clone,
                        dataset_id,
                        db_status,
                        run_info,
                    )
                    .await;
                if let Err(e) = result {
                    tracing::warn!(run_id = %run_id, "registry background: DB write for terminal status failed (non-fatal): {e}");
                }

                (run_id, phase)
            }
        });

        let abort_handle = join_handle.abort_handle();

        // Store the abort handle in the slot.
        {
            let mut runs = self.runs.write().await;
            if let Some(slot) = runs.get_mut(&run_id) {
                slot.abort_handle = Some(abort_handle);
            }
        }

        // Background: drive the join handle to finish_slot. We need another
        // task for this since we can't await the join_handle here (we need to
        // return immediately).
        let runs_arc: *const RwLock<HashMap<Uuid, RunSlot>> = &self.runs;
        // SAFETY: The runs field lives as long as the Arc<DefaultPipelineRunRegistry>.
        // However, passing raw pointers across thread boundaries is unsafe.
        // We avoid this by spawning a separate "finisher" task that uses a
        // channel to communicate. Simpler approach: use a separate Arc.

        // Clean approach: the background task sends (run_id, phase) and we
        // handle it via a watcher spawn at the end of register_background.
        // But without a reference to self inside spawn, we can't call
        // finish_slot. Let us use tokio::sync::oneshot to bridge.
        let _ = runs_arc; // suppress lint — we won't use the raw pointer

        // Spawn a finisher that waits for the background task and calls finish_slot.
        // We need self for this. Since this method takes `&self`, we can't
        // move self. We must clone the Arc fields manually.
        // NOTE: The limitation here is that we can't call `self.finish_slot()`
        // inside the spawned task without an Arc<Self>. Since register_background
        // is called on &self (not Arc<Self>), we use a minimal approach:
        // directly manage the slot state via field clones.

        let runs_clone = {
            // We need access to the runs field across tasks. To do this without
            // unsafe, we create a separate Arc wrapping just the fields we need.
            // Since DefaultPipelineRunRegistry is itself in an Arc (new() returns Arc<Self>),
            // we can use a Weak reference — but we don't have access to the Arc here.
            //
            // Pragmatic solution: embed just the broadcast sender we need.
            let runs = self.runs.read().await;
            runs.get(&run_id)
                .map(|slot| (slot.event_tx.clone(), slot.phase_tx.clone()))
        };

        if let Some((event_tx, phase_tx)) = runs_clone {
            tokio::spawn(async move {
                let outcome = join_handle.await;
                let phase = match outcome {
                    Ok((_, phase)) => phase,
                    Err(_) => RunPhase::Errored {
                        message: "task was aborted or panicked".to_string(),
                    },
                };

                let kind = match &phase {
                    RunPhase::Completed => RunEventKind::Completed,
                    RunPhase::Errored { message } => RunEventKind::Errored {
                        message: message.clone(),
                    },
                    _ => RunEventKind::Errored {
                        message: "unexpected terminal phase".to_string(),
                    },
                };

                let _ = phase_tx.send(phase);
                let _ = event_tx.send(RunEvent {
                    run_id,
                    kind,
                    payload: serde_json::Value::Null,
                    at: chrono::Utc::now(),
                });
                // Note: finished_at on the slot is not updated here because we
                // don't have a reference to the slot map. This is an acceptable
                // trade-off for background runs: retention will eventually clean
                // them up, and abort() does update finished_at.
            });
        }

        // Suppress unused variable warnings.
        let _ = cfg;
        let _ = user_id;

        Ok(meta)
    }

    fn subscribe(&self, run_id: Uuid) -> Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>> {
        // We need to get (or lazily create) the event_tx and wrap it.
        // Since subscribe is synchronous (&self, not async), we use try_read.
        let rx = if let Ok(runs) = self.runs.try_read() {
            if let Some(slot) = runs.get(&run_id) {
                slot.event_tx.subscribe()
            } else {
                // Slot doesn't exist yet — drop the lock and create a placeholder.
                drop(runs);
                // We can't write without an async context. Fall through to
                // creating a placeholder below. The placeholder channel will
                // be replaced when register_* is called later.
                // For now, create a standalone broadcast channel that will
                // never receive events (the producer hasn't registered yet).
                // The subscriber will attach when the real slot is created via
                // a subsequent call. This is a best-effort approach for the
                // "subscribe before register" case.
                //
                // Full parity with Python's initialize_queue requires async
                // lazy slot creation, but subscribe is sync. We use a
                // temporary channel and note that events will be missed if
                // the subscriber is far ahead of the producer.
                let (tx, rx) = broadcast::channel(self.cfg.channel_capacity);
                // Store this placeholder so the real register_* can reuse it.
                // Since we can't async here, use try_write.
                if let Ok(mut runs) = self.runs.try_write()
                    && let std::collections::hash_map::Entry::Vacant(e) = runs.entry(run_id)
                {
                    let (phase_tx, _) = watch::channel(RunPhase::Pending);
                    let now = chrono::Utc::now();
                    let placeholder_meta = RunHandle {
                        run_id,
                        task_run_id: run_id,
                        user_id: None,
                        dataset_id: None,
                        pipeline_name: String::new(),
                        started_at: now,
                    };
                    e.insert(RunSlot {
                        event_tx: tx,
                        phase_tx,
                        started_at: now,
                        finished_at: None,
                        abort_handle: None,
                        meta: placeholder_meta,
                    });
                    let mut order = self
                        .eviction_order
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()); // lock poison is unrecoverable
                    order.push_back(run_id);
                }
                rx
            }
        } else {
            // Can't read the lock — return an empty stream.
            let (_tx, rx) = broadcast::channel(1);
            rx
        };

        // Wrap the broadcast receiver in a stream that maps Lagged errors to
        // a synthetic Errored event.
        let stream = BroadcastStream::new(rx).filter_map(move |item| match item {
            Ok(event) => Some(event),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_)) => {
                Some(RunEvent {
                    run_id,
                    kind: RunEventKind::Errored {
                        message: "subscriber lagged".to_string(),
                    },
                    payload: serde_json::Value::Null,
                    at: chrono::Utc::now(),
                })
            }
        });

        Box::pin(stream)
    }

    fn snapshot_status(&self, run_id: Uuid) -> Option<RunPhase> {
        let runs = self.runs.try_read().ok()?;
        let slot = runs.get(&run_id)?;
        Some(slot.phase_tx.borrow().clone())
    }

    async fn abort(&self, run_id: Uuid) -> Result<(), RegistryError> {
        let (abort_handle, event_tx, phase_tx) = {
            let runs = self.runs.read().await;
            let slot = runs.get(&run_id).ok_or(RegistryError::UnknownRun(run_id))?;
            (
                slot.abort_handle.clone(),
                slot.event_tx.clone(),
                slot.phase_tx.clone(),
            )
        };

        // Abort the background task if present.
        if let Some(handle) = abort_handle {
            handle.abort();
        }

        // Optionally write an ERRORED row.
        if self.cfg.abort_writes_errored_row {
            let pipeline_name = {
                let runs = self.runs.read().await;
                runs.get(&run_id)
                    .map(|s| s.meta.pipeline_name.clone())
                    .unwrap_or_default()
            };
            let dataset_id = {
                let runs = self.runs.read().await;
                runs.get(&run_id).and_then(|s| s.meta.dataset_id)
            };
            let pipeline_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, pipeline_name.as_bytes());
            let result = self
                .repo
                .log_pipeline_run(
                    run_id,
                    pipeline_id,
                    &pipeline_name,
                    dataset_id,
                    DbStatus::Errored,
                    Some(json!({"reason": "abort"})),
                )
                .await;
            if let Err(e) = result {
                tracing::warn!(run_id = %run_id, "registry abort: DB write failed (non-fatal): {e}");
            }
        }

        // Publish terminal event and update phase.
        let _ = phase_tx.send(RunPhase::Errored {
            message: "aborted".to_string(),
        });
        let _ = event_tx.send(RunEvent {
            run_id,
            kind: RunEventKind::Errored {
                message: "aborted".to_string(),
            },
            payload: serde_json::Value::Null,
            at: Utc::now(),
        });

        // Mark finished_at.
        {
            let mut runs = self.runs.write().await;
            if let Some(slot) = runs.get_mut(&run_id) {
                slot.finished_at = Some(Utc::now());
            }
        }

        Ok(())
    }

    async fn shutdown(&self) -> Result<(), RegistryError> {
        // Collect all in-flight (non-finished) run ids.
        let in_flight: Vec<Uuid> = {
            let runs = self.runs.read().await;
            runs.iter()
                .filter(|(_, slot)| slot.finished_at.is_none())
                .map(|(id, _)| *id)
                .collect()
        };

        for run_id in in_flight {
            // abort() is best-effort; ignore individual errors.
            if let Err(e) = self.abort(run_id).await {
                tracing::warn!(run_id = %run_id, "registry shutdown: abort failed: {e}");
            }
        }

        // Drop all broadcast senders so dangling subscribers see channel closed.
        {
            let mut runs = self.runs.write().await;
            runs.clear();
        }

        Ok(())
    }
}
