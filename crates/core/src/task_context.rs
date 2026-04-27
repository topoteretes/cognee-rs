use std::sync::Arc;

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
    /// Dataset being processed.
    pub dataset_id: Option<Uuid>,
    /// The data item currently being processed.
    /// Set per-item by the executor before calling a task.
    pub current_data: Option<Arc<dyn Value>>,
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
