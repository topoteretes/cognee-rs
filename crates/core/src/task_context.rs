use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use cognee_database::DatabaseConnection;
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use uuid::Uuid;

use crate::{
    cancellation::{CancellationHandle, CancellationToken, cancellation_pair},
    error::CoreError,
    exec_status::{ExecStatusManager, NoopExecStatusManager},
    pipeline::PipelineWatcher,
    progress::ProgressToken,
    task::Value,
    thread_pool::CpuPool,
};
/// Identity of the running pipeline and the data item being processed.
///
/// Tasks that need attribution metadata (user, dataset, current data item)
/// read this from [`TaskContext::pipeline_ctx`].
#[derive(Clone)]
pub struct PipelineContext {
    /// Unique ID of this pipeline run (matches [`Pipeline::id`]).
    pub pipeline_id: Uuid,
    /// Human-readable pipeline name.
    pub pipeline_name: String,
    /// Owner / tenant executing the pipeline.
    pub user_id: Option<Uuid>,
    /// Tenant the pipeline run belongs to. `None` for single-user
    /// deployments — telemetry emitters substitute the literal
    /// `"Single User Tenant"` to match Python's behaviour.
    pub tenant_id: Option<Uuid>,
    /// Dataset being processed.
    pub dataset_id: Option<Uuid>,
    /// The data item currently being processed.
    /// Set per-item by the executor before calling a task.
    pub current_data: Option<Arc<dyn Value>>,
    /// Random per-invocation run id. Set by [`crate::pipeline::execute`] when
    /// it creates `PipelineRunInfo`. Used by tasks (via
    /// [`TaskContext::publish_payload_field`]) to attribute payload events.
    /// `None` when the task is not running inside `execute()`.
    pub run_id: Option<Uuid>,
    /// Email of the user running the pipeline, if known. Used by the
    /// provenance-stamping algorithm to populate
    /// `DataPoint.source_user`. Mirrors Python's `user.email`.
    /// Resolution priority is captured by [`PipelineContext::user_label`].
    pub user_email: Option<String>,
    /// DataPoints already stamped during this pipeline run, keyed on
    /// their UUID. Shared across all tasks via the per-run
    /// `PipelineContext` so a DataPoint that survives multiple tasks
    /// is stamped exactly once — with the **first** task's name.
    /// Mirrors Python's `PipelineContext._provenance_visited`.
    pub provenance_visited: Arc<Mutex<HashSet<Uuid>>>,
}

impl PipelineContext {
    /// Resolved label used as `DataPoint.source_user` by the
    /// provenance-stamping algorithm.
    ///
    /// Priority order (matches Python's `user.email or str(user.id)`,
    /// locked decision 4):
    ///
    /// 1. `user_email` if set.
    /// 2. Else `user_id.to_string()` if set.
    /// 3. Else `None` (the DP keeps its own value, or stays unstamped).
    pub fn user_label(&self) -> Option<String> {
        self.user_email
            .clone()
            .or_else(|| self.user_id.map(|id| id.to_string()))
    }
}
/// Runtime dependencies and control tokens for a single pipeline task.
///
/// Build via [`TaskContextBuilder`].
pub struct TaskContext {
    /// CPU-bound work executor (wraps a Rayon pool by default).
    pub thread_pool: Arc<dyn CpuPool>,
    /// Relational / metadata database connection.
    pub database: Arc<DatabaseConnection>,
    /// Graph database.
    pub graph_db: Arc<dyn GraphDBTrait>,
    /// Vector database.
    pub vector_db: Arc<dyn VectorDB>,
    /// Token the task checks to detect cancellation requests.
    pub cancellation: CancellationToken,
    /// Token the task uses to report progress.
    pub progress: ProgressToken,
    /// Pipeline run identity and current data item context.
    pub pipeline_ctx: Option<PipelineContext>,
    /// Per-item incremental status tracker (deduplication / resume).
    pub exec_status: Arc<dyn ExecStatusManager>,
    /// Optional pipeline watcher injected by the registry.
    ///
    /// When set, the pipeline executor routes lifecycle events here in addition
    /// to (or instead of) any watcher passed directly to `execute()`. Set by
    /// `PipelineRunRegistry` so library functions can publish events without
    /// knowing about the registry.
    pub pipeline_watcher: Option<Arc<dyn PipelineWatcher>>,
}

impl TaskContext {
    /// Convenience accessor for the pipeline context.
    ///
    /// Panics if the context was not set — only call this from tasks that are
    /// known to run inside a pipeline executor.
    pub fn pipeline(&self) -> &PipelineContext {
        self.pipeline_ctx
            .as_ref()
            .expect("PipelineContext not set — task is not running inside a pipeline executor")
    }

    /// Create a new `Arc<TaskContext>` with a different progress token.
    /// All other fields are shallow-cloned.
    pub fn with_progress(self: &Arc<Self>, progress: ProgressToken) -> Arc<Self> {
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&self.thread_pool),
            database: Arc::clone(&self.database),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            cancellation: self.cancellation.clone(),
            progress,
            pipeline_ctx: self.pipeline_ctx.clone(),
            exec_status: Arc::clone(&self.exec_status),
            pipeline_watcher: self.pipeline_watcher.clone(),
        })
    }

    /// Create a new `Arc<TaskContext>` with `current_data` set on the pipeline
    /// context. All `Arc` fields are shallow-cloned (cheap reference bumps).
    ///
    /// Returns the original `Arc` unchanged if no `pipeline_ctx` is present.
    pub fn with_current_data(self: &Arc<Self>, data: Arc<dyn Value>) -> Arc<Self> {
        let mut pipeline_ctx = match &self.pipeline_ctx {
            Some(ctx) => ctx.clone(),
            None => return Arc::clone(self),
        };
        pipeline_ctx.current_data = Some(data);
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&self.thread_pool),
            database: Arc::clone(&self.database),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            cancellation: self.cancellation.clone(),
            progress: self.progress.clone(),
            pipeline_ctx: Some(pipeline_ctx),
            exec_status: Arc::clone(&self.exec_status),
            pipeline_watcher: self.pipeline_watcher.clone(),
        })
    }

    /// Create a new `Arc<TaskContext>` with `user_email` set on the pipeline
    /// context. All `Arc` fields are shallow-cloned.
    ///
    /// Returns the original `Arc` unchanged if no `pipeline_ctx` is present.
    pub fn with_user_email(self: &Arc<Self>, email: String) -> Arc<Self> {
        let mut pipeline_ctx = match &self.pipeline_ctx {
            Some(ctx) => ctx.clone(),
            None => return Arc::clone(self),
        };
        pipeline_ctx.user_email = Some(email);
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&self.thread_pool),
            database: Arc::clone(&self.database),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            cancellation: self.cancellation.clone(),
            progress: self.progress.clone(),
            pipeline_ctx: Some(pipeline_ctx),
            exec_status: Arc::clone(&self.exec_status),
            pipeline_watcher: self.pipeline_watcher.clone(),
        })
    }

    /// Create a new `Arc<TaskContext>` with `run_id` set on the pipeline
    /// context. All other fields are shallow-cloned.
    ///
    /// Returns the original `Arc` unchanged if no `pipeline_ctx` is present.
    pub fn with_run_id(self: &Arc<Self>, run_id: Uuid) -> Arc<Self> {
        let mut pipeline_ctx = match &self.pipeline_ctx {
            Some(ctx) => ctx.clone(),
            None => return Arc::clone(self),
        };
        pipeline_ctx.run_id = Some(run_id);
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&self.thread_pool),
            database: Arc::clone(&self.database),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            cancellation: self.cancellation.clone(),
            progress: self.progress.clone(),
            pipeline_ctx: Some(pipeline_ctx),
            exec_status: Arc::clone(&self.exec_status),
            pipeline_watcher: self.pipeline_watcher.clone(),
        })
    }

    /// Publish a run-scoped payload field. Tasks running inside
    /// [`crate::pipeline::execute`] call this to attach metadata that downstream
    /// observers read via the registry's payload accumulator.
    ///
    /// Silently no-ops if no `pipeline_watcher` is attached or if
    /// `pipeline_ctx.run_id` was never set (i.e. the task is not running
    /// inside `execute()`).
    pub async fn publish_payload_field(&self, key: &str, value: serde_json::Value) {
        let Some(w) = self.pipeline_watcher.as_ref() else {
            return;
        };
        let Some(pctx) = self.pipeline_ctx.as_ref() else {
            return;
        };
        let Some(run_id) = pctx.run_id else {
            return;
        };
        w.on_payload_field(run_id, key, value).await;
    }
}
/// Fluent builder for [`TaskContext`].
///
/// ```rust,ignore
/// let (handle, ctx) = TaskContextBuilder::new()
///     .thread_pool(Arc::new(RayonThreadPool::with_default_threads()?))
///     .database(db)
///     .graph_db(graph)
///     .vector_db(vectors)
///     .progress(ProgressToken::new())
///     .build()?;
/// ```
#[derive(Default)]
pub struct TaskContextBuilder {
    thread_pool: Option<Arc<dyn CpuPool>>,
    database: Option<Arc<DatabaseConnection>>,
    graph_db: Option<Arc<dyn GraphDBTrait>>,
    vector_db: Option<Arc<dyn VectorDB>>,
    /// If set, the cancellation pair is created from an external handle.
    cancellation: Option<(CancellationHandle, CancellationToken)>,
    progress: Option<ProgressToken>,
    pipeline_ctx: Option<PipelineContext>,
    exec_status: Option<Arc<dyn ExecStatusManager>>,
    pipeline_watcher: Option<Arc<dyn PipelineWatcher>>,
}

impl TaskContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn thread_pool(mut self, pool: Arc<dyn CpuPool>) -> Self {
        self.thread_pool = Some(pool);
        self
    }

    pub fn database(mut self, db: Arc<DatabaseConnection>) -> Self {
        self.database = Some(db);
        self
    }

    pub fn graph_db(mut self, graph: Arc<dyn GraphDBTrait>) -> Self {
        self.graph_db = Some(graph);
        self
    }

    pub fn vector_db(mut self, vectors: Arc<dyn VectorDB>) -> Self {
        self.vector_db = Some(vectors);
        self
    }

    /// Set a pre-built progress token. Defaults to a fresh root token.
    pub fn progress(mut self, token: ProgressToken) -> Self {
        self.progress = Some(token);
        self
    }

    /// Set pipeline run identity context.
    pub fn pipeline_context(mut self, ctx: PipelineContext) -> Self {
        self.pipeline_ctx = Some(ctx);
        self
    }

    /// Set the per-item status manager for incremental deduplication.
    /// Defaults to [`NoopExecStatusManager`] if not set.
    pub fn exec_status(mut self, mgr: Arc<dyn ExecStatusManager>) -> Self {
        self.exec_status = Some(mgr);
        self
    }

    /// Inject a pipeline watcher into the context.
    ///
    /// When set, the registry's `ScopedRunWatcher` is stored here so that
    /// library functions can publish lifecycle events without needing to know
    /// about the registry. Defaults to `None` (no watcher).
    pub fn pipeline_watcher(mut self, w: Arc<dyn PipelineWatcher>) -> Self {
        self.pipeline_watcher = Some(w);
        self
    }

    /// Build the context. Returns `(CancellationHandle, TaskContext)` so the
    /// caller keeps the handle while the task receives the token.
    pub fn build(self) -> Result<(CancellationHandle, TaskContext), CoreError> {
        let thread_pool = self.thread_pool.ok_or(CoreError::MissingContextField {
            field: "thread_pool",
        })?;
        let database = self
            .database
            .ok_or(CoreError::MissingContextField { field: "database" })?;
        let graph_db = self
            .graph_db
            .ok_or(CoreError::MissingContextField { field: "graph_db" })?;
        let vector_db = self
            .vector_db
            .ok_or(CoreError::MissingContextField { field: "vector_db" })?;

        let (handle, token) = self.cancellation.unwrap_or_else(cancellation_pair);

        let ctx = TaskContext {
            thread_pool,
            database,
            graph_db,
            vector_db,
            cancellation: token,
            progress: self.progress.unwrap_or_default(),
            pipeline_ctx: self.pipeline_ctx,
            exec_status: self
                .exec_status
                .unwrap_or_else(|| Arc::new(NoopExecStatusManager)),
            pipeline_watcher: self.pipeline_watcher,
        };

        Ok((handle, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_label_prefers_email() {
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: Some("alice@example.com".into()),
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert_eq!(ctx.user_label().as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn user_label_falls_back_to_user_id() {
        let uid = Uuid::new_v4();
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: Some(uid),
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert_eq!(ctx.user_label(), Some(uid.to_string()));
    }

    #[test]
    fn user_label_is_none_when_neither_set() {
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: None,
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert!(ctx.user_label().is_none());
    }
}
