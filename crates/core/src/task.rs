use std::any::Any;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::stream::{BoxStream, Stream, StreamExt};

use crate::task_context::TaskContext;
/// Type-erased value passed between pipeline tasks.
///
/// Automatically implemented for every `T: Any + Send + Sync + 'static`.
/// Use `value.as_any().downcast_ref::<T>()` for a borrowed `&T`, or
/// [`downcast_value`] to recover an owned `Box<T>`.
///
/// Both `Send` and `Sync` are required because the executor shares values
/// via `Arc<dyn Value>` across retry attempts and fan-out.
pub trait Value: Any + Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any + Send + Sync + 'static> Value for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// Attempt to downcast a `Box<dyn Value>` to a concrete type.
///
/// Returns `Ok(Box<T>)` on success and gives the original box back as
/// `Err(Box<dyn Value>)` on type mismatch.
pub fn downcast_value<T: Any>(value: Box<dyn Value>) -> Result<Box<T>, Box<dyn Value>> {
    if value.as_any().is::<T>() {
        // SAFETY: confirmed with is::<T>() above.
        Ok(value
            .into_any()
            .downcast::<T>()
            .expect("downcast can't fail after is::<T>() check"))
    } else {
        Err(value)
    }
}

/// A value wrapper that carries arbitrary string metadata alongside the inner
/// value.
///
/// Use this to attach provenance information (e.g. `node_set`) to pipeline
/// outputs so the executor can forward it to
/// [`ExecStatusManager::stamp_provenance`](crate::exec_status::ExecStatusManager::stamp_provenance).
///
/// ```rust,ignore
/// let output = Tagged::new(my_chunk)
///     .with_meta("node_set", "entity_nodes");
/// Ok(Arc::new(output) as Arc<dyn Value>)
/// ```
pub struct Tagged<T: Value> {
    inner: T,
    metadata: std::collections::HashMap<String, String>,
}

impl<T: Value> Tagged<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            metadata: std::collections::HashMap::new(),
        }
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }

    pub fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }
}

// Tagged<T>: Any + Send + Sync + 'static  →  blanket impl covers it
// (provided T: Value, which implies T: Any + Send + Sync + 'static,
// and HashMap<String, String> is Send + Sync + 'static).

/// Try to extract a `node_set` metadata value from an `Arc<dyn Value>`.
///
/// Checks whether the value is a [`TaggedMeta`] and extracts its `node_set`.
/// Returns `None` if the value is not a `TaggedMeta` or the field is absent.
///
/// The generic [`Tagged<T>`] is a user-facing wrapper; provenance extraction
/// in the executor uses `TaggedMeta` which is type-erased.
pub fn extract_node_set(value: &dyn Value) -> Option<&str> {
    value
        .as_any()
        .downcast_ref::<TaggedMeta>()
        .and_then(|m| m.node_set.as_deref())
}

/// Lightweight metadata carrier that tasks can attach to any `Arc<dyn Value>`
/// when they need to propagate `node_set` without wrapping in `Tagged<T>`.
///
/// The executor checks for this type when stamping provenance.
pub struct TaggedMeta {
    /// The wrapped value (type-erased).
    pub value: Arc<dyn Value>,
    /// Node set label for provenance stamping.
    pub node_set: Option<String>,
}

impl TaggedMeta {
    pub fn new(value: Arc<dyn Value>) -> Self {
        Self {
            value,
            node_set: None,
        }
    }

    pub fn with_node_set(mut self, node_set: impl Into<String>) -> Self {
        self.node_set = Some(node_set.into());
        self
    }
}
/// Boxed error returned by a failing task.
pub type TaskError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Boxed, type-erased iterator yielded by `SyncIter` tasks.
pub type ValueIter = Box<dyn Iterator<Item = Box<dyn Value>> + Send + 'static>;

/// Boxed, type-erased async stream yielded by `AsyncStream` tasks.
pub type ValueStream = BoxStream<'static, Box<dyn Value>>;

//
// Single-value flavours — calling convention:
//   fn(input: Arc<dyn Value>, ctx: Arc<TaskContext>) -> <output>
//
// `Arc<dyn Value>` as input enables two things:
//   • Retry:   the executor holds one Arc, clones it O(1) for each attempt.
//   • Fan-out: the same Arc can be given to multiple downstream calls cheaply.
//
// Batch flavours — calling convention:
//   fn(items: &[Box<dyn Value>], ctx: Arc<TaskContext>) -> <output>
//
// `&[Box<dyn Value>]` delivers a whole accumulated batch at once; the executor
// decides the slice boundary based on the next task's configured batch_size.
//
// `Arc<dyn Fn(...)>` (not Box<dyn FnOnce>) means:
//   • The same task object is callable multiple times without being consumed.

/// Sync task: one value in → one value out.
pub type SyncFn = Arc<
    dyn Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<Arc<dyn Value>, TaskError> + Send + Sync,
>;

/// Async task: one value in → one value out (via a future).
pub type AsyncFn = Arc<
    dyn Fn(
            Arc<dyn Value>,
            Arc<TaskContext>,
        ) -> BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
        + Send
        + Sync,
>;

/// Sync task: one value in → lazy iterator of values out.
pub type SyncIterFn =
    Arc<dyn Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<ValueIter, TaskError> + Send + Sync>;

/// Async task: one value in → async stream of values out.
pub type AsyncStreamFn =
    Arc<dyn Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<ValueStream, TaskError> + Send + Sync>;

/// Sync batch task: slice of values in → one value out.
pub type SyncBatchFn = Arc<
    dyn for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<Arc<dyn Value>, TaskError>
        + Send
        + Sync,
>;

/// Async batch task: slice of values in → one value out (via a future).
pub type AsyncBatchFn = Arc<
    dyn for<'a> Fn(
            &'a [Box<dyn Value>],
            Arc<TaskContext>,
        ) -> BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
        + Send
        + Sync,
>;

/// Sync batch task: slice of values in → lazy iterator of values out.
pub type SyncIterBatchFn = Arc<
    dyn for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<ValueIter, TaskError>
        + Send
        + Sync,
>;

/// Async batch task: slice of values in → async stream of values out.
pub type AsyncStreamBatchFn = Arc<
    dyn for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<ValueStream, TaskError>
        + Send
        + Sync,
>;
/// A single reusable unit of work in a cognee pipeline.
///
/// | Variant | Execution | Input | Output |
/// |---------|-----------|-------|--------|
/// | [`Task::Sync`] | blocking | single value | single value |
/// | [`Task::Async`] | non-blocking | single value | single value |
/// | [`Task::SyncIter`] | blocking | single value | lazy iterator |
/// | [`Task::AsyncStream`] | non-blocking | single value | async stream |
/// | [`Task::SyncBatch`] | blocking | slice of values | single value |
/// | [`Task::AsyncBatch`] | non-blocking | slice of values | single value |
/// | [`Task::SyncIterBatch`] | blocking | slice of values | lazy iterator |
/// | [`Task::AsyncStreamBatch`] | non-blocking | slice of values | async stream |
///
/// Single-value variants are called once per item. Batch variants receive a
/// `&[Box<dyn Value>]` slice of items accumulated up to the task's `batch_size`.
/// The pipeline executor detects which kind the next task is and routes
/// accordingly.
pub enum Task {
    Sync(SyncFn),
    Async(AsyncFn),
    SyncIter(SyncIterFn),
    AsyncStream(AsyncStreamFn),
    SyncBatch(SyncBatchFn),
    AsyncBatch(AsyncBatchFn),
    SyncIterBatch(SyncIterBatchFn),
    AsyncStreamBatch(AsyncStreamBatchFn),
}

impl Task {
    /// Returns `true` if this task accepts a batch slice rather than a single value.
    pub fn is_batch(&self) -> bool {
        matches!(
            self,
            Task::SyncBatch(_)
                | Task::AsyncBatch(_)
                | Task::SyncIterBatch(_)
                | Task::AsyncStreamBatch(_)
        )
    }
}

impl Task {
    // ── Raw constructors (type-erased Arc<dyn Value> in/out) ──────────────────

    /// Create a [`Task::Sync`] from a raw closure.
    pub fn sync<F>(f: F) -> Self
    where
        F: Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<Arc<dyn Value>, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::Sync(Arc::new(f))
    }

    /// Create a [`Task::Async`] from a raw closure returning a [`BoxFuture`].
    pub fn async_fn<F>(f: F) -> Self
    where
        F: Fn(
                Arc<dyn Value>,
                Arc<TaskContext>,
            ) -> BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        Task::Async(Arc::new(f))
    }

    /// Create a [`Task::SyncIter`] from a raw closure returning a [`ValueIter`].
    pub fn sync_iter<F>(f: F) -> Self
    where
        F: Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<ValueIter, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::SyncIter(Arc::new(f))
    }

    /// Create a [`Task::AsyncStream`] from a raw closure returning a
    /// [`ValueStream`].
    pub fn async_stream<F>(f: F) -> Self
    where
        F: Fn(Arc<dyn Value>, Arc<TaskContext>) -> Result<ValueStream, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::AsyncStream(Arc::new(f))
    }

    // ── Raw batch constructors (type-erased &[Box<dyn Value>] in) ─────────────

    /// Create a [`Task::SyncBatch`] from a raw closure.
    pub fn sync_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<Arc<dyn Value>, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::SyncBatch(Arc::new(f))
    }

    /// Create a [`Task::AsyncBatch`] from a raw closure returning a [`BoxFuture`].
    pub fn async_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(
                &'a [Box<dyn Value>],
                Arc<TaskContext>,
            ) -> BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        Task::AsyncBatch(Arc::new(f))
    }

    /// Create a [`Task::SyncIterBatch`] from a raw closure returning a [`ValueIter`].
    pub fn sync_iter_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<ValueIter, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::SyncIterBatch(Arc::new(f))
    }

    /// Create a [`Task::AsyncStreamBatch`] from a raw closure returning a [`ValueStream`].
    pub fn async_stream_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [Box<dyn Value>], Arc<TaskContext>) -> Result<ValueStream, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::AsyncStreamBatch(Arc::new(f))
    }

    // ── Typed constructors ────────────────────────────────────────────────────
    //
    // These accept closures over concrete `&I` / `Box<O>` types and generate all
    // downcast / coercion boilerplate automatically.
    //
    // The input is presented as `&I` — a borrowed view obtained via
    // `downcast_ref` on the shared `Arc<dyn Value>`.  The `Arc` stays alive
    // for the duration of the call, so the reference is valid.
    //
    // For async variants the closure must return `BoxFuture<'static, ...>`,
    // which means the future may NOT borrow `&I`.  Any data needed inside the
    // async block must be owned (copied/cloned) before `Box::pin(async move {
    // ... })`.
    //
    // The concrete output type `O: Sized` allows the wrapper to convert
    // `Box<O>` → `Arc<dyn Value>` via `Arc::new(*box_o)` at zero extra cost.

    /// Create a [`Task::Sync`] from a typed closure.
    ///
    /// ```rust,ignore
    /// Task::sync_typed(|input: &MyInput, ctx| {
    ///     Ok(Box::new(process(input)))
    /// })
    /// ```
    pub fn sync_typed<I, O, F>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: Fn(&I, Arc<TaskContext>) -> Result<Box<O>, TaskError> + Send + Sync + 'static,
    {
        Task::Sync(Arc::new(move |input: Arc<dyn Value>, ctx| {
            let typed = Self::borrow_input::<I>(&input);
            f(typed, ctx).map(|v| Arc::new(*v) as Arc<dyn Value>)
        }))
    }

    /// Create a [`Task::Async`] from a typed closure returning a `'static`
    /// future.
    ///
    /// Data needed inside the async block must be copied/cloned before it:
    ///
    /// ```rust,ignore
    /// Task::async_fn_typed(|input: &MyInput, ctx| {
    ///     let id = input.id;  // copy before async block
    ///     Box::pin(async move {
    ///         Ok(Box::new(fetch(id).await?))
    ///     })
    /// })
    /// ```
    pub fn async_fn_typed<I, O, F>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: Fn(&I, Arc<TaskContext>) -> BoxFuture<'static, Result<Box<O>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        Task::Async(Arc::new(move |input: Arc<dyn Value>, ctx| {
            let typed = Self::borrow_input::<I>(&input);
            // `f(typed, ctx)` produces a 'static future and must not borrow
            // from `typed` (ensured by the BoxFuture<'static> bound).
            let fut = f(typed, ctx);
            Box::pin(async move { fut.await.map(|v| Arc::new(*v) as Arc<dyn Value>) })
        }))
    }

    /// Create a [`Task::SyncIter`] from a typed closure returning a concrete
    /// iterator.  The iterator must be `'static` (may not borrow the input).
    ///
    /// ```rust,ignore
    /// Task::sync_iter_typed(|input: &Document, ctx| {
    ///     let chunks = split(input.text.clone());
    ///     Ok(chunks.into_iter().map(Box::new))
    /// })
    /// ```
    pub fn sync_iter_typed<I, O, F, Iter>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: Fn(&I, Arc<TaskContext>) -> Result<Iter, TaskError> + Send + Sync + 'static,
        Iter: Iterator<Item = Box<O>> + Send + 'static,
    {
        Task::SyncIter(Arc::new(move |input: Arc<dyn Value>, ctx| {
            let typed = Self::borrow_input::<I>(&input);
            f(typed, ctx).map(|iter| Box::new(iter.map(|v| v as Box<dyn Value>)) as ValueIter)
        }))
    }

    /// Create a [`Task::AsyncStream`] from a typed closure returning a concrete
    /// stream.  The stream must be `'static`.
    ///
    /// ```rust,ignore
    /// Task::async_stream_typed(|input: &DatasetId, ctx| {
    ///     let id = *input;
    ///     Ok(stream_chunks(id))
    /// })
    /// ```
    pub fn async_stream_typed<I, O, F, S>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: Fn(&I, Arc<TaskContext>) -> Result<S, TaskError> + Send + Sync + 'static,
        S: Stream<Item = Box<O>> + Send + 'static,
    {
        Task::AsyncStream(Arc::new(move |input: Arc<dyn Value>, ctx| {
            let typed = Self::borrow_input::<I>(&input);
            f(typed, ctx).map(|s| Box::pin(s.map(|v| v as Box<dyn Value>)) as ValueStream)
        }))
    }

    // ── Typed batch constructors ──────────────────────────────────────────────
    //
    // Same ergonomics as the single-value typed constructors, but the closure
    // receives `&[&I]` — a slice of borrow-downcast references.  Any data
    // needed inside an async block must be owned (copied/cloned) before
    // `Box::pin(async move { ... })`.

    /// Create a [`Task::SyncBatch`] from a typed closure receiving `&[&I]`.
    ///
    /// ```rust,ignore
    /// Task::sync_batch_typed(|chunks: &[&DocumentChunk], ctx| {
    ///     Ok(Box::new(embed_all(chunks)))
    /// })
    /// ```
    pub fn sync_batch_typed<I, O, F>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<Box<O>, TaskError>
            + Send
            + Sync
            + 'static,
    {
        Task::SyncBatch(Arc::new(move |items: &[Box<dyn Value>], ctx| {
            let typed: Vec<&I> = items.iter().map(|v| Self::borrow_item::<I>(v)).collect();
            f(&typed, ctx).map(|v| Arc::new(*v) as Arc<dyn Value>)
        }))
    }

    /// Create a [`Task::AsyncBatch`] from a typed closure returning a `'static` future.
    ///
    /// Data needed inside the async block must be copied/cloned before it:
    ///
    /// ```rust,ignore
    /// Task::async_batch_typed(|chunks: &[&DocumentChunk], ctx| {
    ///     let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    ///     Box::pin(async move {
    ///         Ok(Box::new(embed_batch(texts).await?))
    ///     })
    /// })
    /// ```
    pub fn async_batch_typed<I, O, F>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: for<'a> Fn(
                &'a [&'a I],
                Arc<TaskContext>,
            ) -> BoxFuture<'static, Result<Box<O>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        Task::AsyncBatch(Arc::new(move |items: &[Box<dyn Value>], ctx| {
            let typed: Vec<&I> = items.iter().map(|v| Self::borrow_item::<I>(v)).collect();
            let fut = f(&typed, ctx);
            Box::pin(async move { fut.await.map(|v| Arc::new(*v) as Arc<dyn Value>) })
        }))
    }

    /// Create a [`Task::SyncIterBatch`] from a typed closure returning a concrete iterator.
    pub fn sync_iter_batch_typed<I, O, F, Iter>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<Iter, TaskError>
            + Send
            + Sync
            + 'static,
        Iter: Iterator<Item = Box<O>> + Send + 'static,
    {
        Task::SyncIterBatch(Arc::new(move |items: &[Box<dyn Value>], ctx| {
            let typed: Vec<&I> = items.iter().map(|v| Self::borrow_item::<I>(v)).collect();
            f(&typed, ctx).map(|iter| Box::new(iter.map(|v| v as Box<dyn Value>)) as ValueIter)
        }))
    }

    /// Create a [`Task::AsyncStreamBatch`] from a typed closure returning a concrete stream.
    pub fn async_stream_batch_typed<I, O, F, S>(f: F) -> Self
    where
        I: Value,
        O: Value,
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<S, TaskError>
            + Send
            + Sync
            + 'static,
        S: Stream<Item = Box<O>> + Send + 'static,
    {
        Task::AsyncStreamBatch(Arc::new(move |items: &[Box<dyn Value>], ctx| {
            let typed: Vec<&I> = items.iter().map(|v| Self::borrow_item::<I>(v)).collect();
            f(&typed, ctx).map(|s| Box::pin(s.map(|v| v as Box<dyn Value>)) as ValueStream)
        }))
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Borrow-downcast the input to `&I`.
    ///
    /// Panics on type mismatch — a mismatch means the pipeline was assembled
    /// with incompatible task types (programming error).
    fn borrow_input<I: Value>(input: &Arc<dyn Value>) -> &I {
        let type_name = std::any::type_name::<I>();
        // Explicit deref through Arc to reach the inner `dyn Value`, then call
        // `as_any` via vtable dispatch. Without this, method resolution finds
        // `<Arc<dyn Value> as Value>::as_any()` (via the blanket impl) instead
        // of dispatching through the trait object.
        (**input)
            .as_any()
            .downcast_ref::<I>()
            .unwrap_or_else(|| panic!("Task input type mismatch: expected {type_name}"))
    }

    /// Borrow-downcast a `Box<dyn Value>` item to `&I`.
    ///
    /// Used inside typed batch constructors to downcast each slice element.
    fn borrow_item<I: Value>(item: &dyn Value) -> &I {
        let type_name = std::any::type_name::<I>();
        item.as_any()
            .downcast_ref::<I>()
            .unwrap_or_else(|| panic!("Batch item type mismatch: expected {type_name}"))
    }

    /// Call this task with a single input value.
    ///
    /// Panics if called on a batch variant — use [`Task::call_batch`] for those.
    /// The task is `Fn`, so `&self` suffices — the same task object handles
    /// every input in a fan-out scenario and every retry attempt.
    pub fn call(&self, input: Arc<dyn Value>, ctx: Arc<TaskContext>) -> TaskCall {
        match self {
            Task::Sync(f) => TaskCall::Sync(f(input, ctx)),
            Task::Async(f) => TaskCall::Async(f(input, ctx)),
            Task::SyncIter(f) => TaskCall::SyncIter(f(input, ctx)),
            Task::AsyncStream(f) => TaskCall::AsyncStream(f(input, ctx)),
            Task::SyncBatch(_)
            | Task::AsyncBatch(_)
            | Task::SyncIterBatch(_)
            | Task::AsyncStreamBatch(_) => {
                panic!("call() used on a batch task variant — use call_batch() instead")
            }
        }
    }

    /// Build a task that runs multiple sub-tasks concurrently on the same input.
    ///
    /// Semantics (matching Python `run_tasks_parallel`):
    /// - Each sub-task receives `Arc::clone(&input)` and the shared context.
    /// - All sub-tasks run concurrently via `futures::future::join_all`.
    /// - If any sub-task fails, the whole parallel task fails with that error.
    /// - On success, returns the result of the **last** sub-task (by position).
    ///
    /// Only single-value (`Sync` / `Async`) sub-tasks are supported. Iter/stream
    /// sub-tasks inside a parallel group don't have well-defined "last result"
    /// semantics and will panic at call time.
    pub fn parallel(tasks: Vec<Task>) -> Self {
        let tasks = Arc::new(tasks);
        Task::Async(Arc::new(move |input, ctx| {
            let tasks = Arc::clone(&tasks);
            Box::pin(async move {
                if tasks.is_empty() {
                    return Ok(input);
                }

                let futs: Vec<_> = tasks
                    .iter()
                    .map(|t| {
                        let call = t.call(Arc::clone(&input), Arc::clone(&ctx));
                        async move {
                            match call {
                                TaskCall::Sync(result) => result,
                                TaskCall::Async(fut) => fut.await,
                                TaskCall::SyncIter(_) | TaskCall::AsyncStream(_) => {
                                    Err("iter/stream tasks are not supported inside Task::parallel"
                                        .into())
                                }
                            }
                        }
                    })
                    .collect();

                let results = futures::future::join_all(futs).await;

                // Collect: if any failed, return the first error.
                // Otherwise return the last successful result.
                let mut last_ok: Option<Arc<dyn Value>> = None;
                for r in results {
                    match r {
                        Err(e) => return Err(e),
                        Ok(v) => last_ok = Some(v),
                    }
                }

                Ok(last_ok.expect("non-empty tasks guaranteed above"))
            })
        }))
    }

    /// Call this batch task with a slice of accumulated values.
    ///
    /// Panics if called on a single-value variant — use [`Task::call`] for those.
    pub fn call_batch(&self, items: &[Box<dyn Value>], ctx: Arc<TaskContext>) -> TaskCall {
        match self {
            Task::SyncBatch(f) => TaskCall::Sync(f(items, ctx)),
            Task::AsyncBatch(f) => TaskCall::Async(f(items, ctx)),
            Task::SyncIterBatch(f) => TaskCall::SyncIter(f(items, ctx)),
            Task::AsyncStreamBatch(f) => TaskCall::AsyncStream(f(items, ctx)),
            Task::Sync(_) | Task::Async(_) | Task::SyncIter(_) | Task::AsyncStream(_) => {
                panic!("call_batch() used on a single-value task variant — use call() instead")
            }
        }
    }
}
/// A [`Task`] bundled with optional per-task configuration.
///
/// Use [`TaskInfo::new`] to wrap a task and then chain `.with_name` /
/// `.with_batch_size` to override the pipeline-level defaults:
///
/// ```rust,ignore
/// TaskInfo::new(my_task)
///     .with_name("embed-chunks")
///     .with_batch_size(16)
/// ```
pub struct TaskInfo {
    pub task: Task,
    /// Human-readable label used in watcher events and status logs.
    pub name: Option<String>,
    /// Overrides the pipeline-level `batch_size` for this task.
    /// `None` → inherit `pipeline.batch_size`.
    pub batch_size: Option<usize>,
    /// Template for a human-readable result summary recorded as a tracing span
    /// attribute when the `telemetry` feature is enabled.
    ///
    /// Use `{n}` as a placeholder for the result count.
    /// E.g. `"Classified {n} document(s)"`.
    pub summary_template: Option<String>,
    /// Relative weight for progress allocation. The executor normalizes weights
    /// across all tasks to determine what fraction of overall progress each
    /// task owns. Default: 1.
    pub weight: u32,
}

impl TaskInfo {
    pub fn new(task: Task) -> Self {
        Self {
            task,
            name: None,
            batch_size: None,
            summary_template: None,
            weight: 1,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        assert!(size > 0, "batch_size must be > 0");
        self.batch_size = Some(size);
        self
    }

    /// Set a summary template for telemetry.
    ///
    /// `{n}` is replaced with the result count at runtime.
    /// E.g. `"Classified {n} document(s)"`.
    pub fn with_summary(mut self, template: impl Into<String>) -> Self {
        self.summary_template = Some(template.into());
        self
    }

    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    /// Build a parallel task from multiple `TaskInfo`s.
    ///
    /// Extracts the inner [`Task`]s, delegates to [`Task::parallel`], and
    /// auto-generates a name like `"parallel([name1, name2, …])"`.
    pub fn parallel(infos: Vec<TaskInfo>) -> Self {
        let names: Vec<String> = infos
            .iter()
            .enumerate()
            .map(|(i, ti)| ti.name.clone().unwrap_or_else(|| format!("task_{i}")))
            .collect();

        let tasks: Vec<Task> = infos.into_iter().map(|ti| ti.task).collect();

        TaskInfo {
            task: Task::parallel(tasks),
            name: Some(format!("parallel([{}])", names.join(", "))),
            batch_size: None,
            summary_template: None,
            weight: 1,
        }
    }
}

impl From<Task> for TaskInfo {
    fn from(task: Task) -> Self {
        TaskInfo::new(task)
    }
}

/// Typed sync single-value fn: `&I → Result<Box<O>, TaskError>`.
type TypedSyncFn<I, O> = dyn Fn(&I, Arc<TaskContext>) -> Result<Box<O>, TaskError> + Send + Sync;
/// Typed async single-value fn: `&I → BoxFuture<Result<Box<O>, TaskError>>`.
type TypedAsyncFn<I, O> =
    dyn Fn(&I, Arc<TaskContext>) -> BoxFuture<'static, Result<Box<O>, TaskError>> + Send + Sync;
/// Typed sync iterator fn: `&I → Result<Box<dyn Iterator<Item=Box<O>>>, TaskError>`.
type TypedSyncIterFn<I, O> = dyn Fn(&I, Arc<TaskContext>) -> Result<Box<dyn Iterator<Item = Box<O>> + Send + 'static>, TaskError>
    + Send
    + Sync;
/// Typed async stream fn: `&I → Result<BoxStream<Box<O>>, TaskError>`.
type TypedAsyncStreamFn<I, O> =
    dyn Fn(&I, Arc<TaskContext>) -> Result<BoxStream<'static, Box<O>>, TaskError> + Send + Sync;
/// Typed sync batch fn: `&[&I] → Result<Box<O>, TaskError>`.
type TypedSyncBatchFn<I, O> =
    dyn for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<Box<O>, TaskError> + Send + Sync;
/// Typed async batch fn: `&[&I] → BoxFuture<Result<Box<O>, TaskError>>`.
type TypedAsyncBatchFn<I, O> = dyn for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> BoxFuture<'static, Result<Box<O>, TaskError>>
    + Send
    + Sync;
/// Typed sync batch iterator fn: `&[&I] → Result<Box<dyn Iterator<Item=Box<O>>>, TaskError>`.
type TypedSyncIterBatchFn<I, O> = dyn for<'a> Fn(
        &'a [&'a I],
        Arc<TaskContext>,
    ) -> Result<Box<dyn Iterator<Item = Box<O>> + Send + 'static>, TaskError>
    + Send
    + Sync;
/// Typed async batch stream fn: `&[&I] → Result<BoxStream<Box<O>>, TaskError>`.
type TypedAsyncStreamBatchFn<I, O> = dyn for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<BoxStream<'static, Box<O>>, TaskError>
    + Send
    + Sync;

/// A typed pipeline task whose input and output types are tracked at the type level.
///
/// Unlike [`Task`], which erases all types to [`Value`] trait objects, `TypedTask<I, O>`
/// carries the concrete input type `I` and output type `O` in its variant signatures.
/// This allows [`PipelineBuilder`](crate::pipeline::PipelineBuilder) to enforce at
/// compile time that the output type of each task matches the input type of the next.
///
/// Type erasure occurs only when the task is converted to [`Task`] or [`TaskInfo`] via
/// the [`From`] impls, which delegate to the corresponding [`Task::sync_typed`] /
/// [`Task::async_fn_typed`] / … constructors.
///
/// # Constructors
///
/// | Method | Task variant |
/// |---|---|
/// | [`sync`](TypedTask::sync) | `Sync` — blocking, `&I → Box<O>` |
/// | [`async_fn`](TypedTask::async_fn) | `Async` — non-blocking, `&I → Box<O>` |
/// | [`sync_iter`](TypedTask::sync_iter) | `SyncIter` — blocking, `&I → Iterator<Box<O>>` |
/// | [`async_stream`](TypedTask::async_stream) | `AsyncStream` — non-blocking, `&I → Stream<Box<O>>` |
/// | [`sync_batch`](TypedTask::sync_batch) | `SyncBatch` — blocking, `&[&I] → Box<O>` |
/// | [`async_batch`](TypedTask::async_batch) | `AsyncBatch` — non-blocking, `&[&I] → Box<O>` |
/// | [`sync_iter_batch`](TypedTask::sync_iter_batch) | `SyncIterBatch` — blocking, `&[&I] → Iterator<Box<O>>` |
/// | [`async_stream_batch`](TypedTask::async_stream_batch) | `AsyncStreamBatch` — non-blocking, `&[&I] → Stream<Box<O>>` |
pub enum TypedTask<I: Value, O: Value> {
    /// Blocking single-value task: `&I → Result<Box<O>, TaskError>`.
    Sync(Arc<TypedSyncFn<I, O>>),
    /// Non-blocking single-value task: `&I → BoxFuture<Result<Box<O>, TaskError>>`.
    Async(Arc<TypedAsyncFn<I, O>>),
    /// Blocking iterator task: `&I → Result<Box<dyn Iterator<Item=Box<O>>>, TaskError>`.
    SyncIter(Arc<TypedSyncIterFn<I, O>>),
    /// Non-blocking stream task: `&I → Result<BoxStream<Box<O>>, TaskError>`.
    AsyncStream(Arc<TypedAsyncStreamFn<I, O>>),
    /// Blocking batch task: `&[&I] → Result<Box<O>, TaskError>`.
    SyncBatch(Arc<TypedSyncBatchFn<I, O>>),
    /// Non-blocking batch task: `&[&I] → BoxFuture<Result<Box<O>, TaskError>>`.
    AsyncBatch(Arc<TypedAsyncBatchFn<I, O>>),
    /// Blocking batch iterator task: `&[&I] → Result<Box<dyn Iterator<Item=Box<O>>>, TaskError>`.
    SyncIterBatch(Arc<TypedSyncIterBatchFn<I, O>>),
    /// Non-blocking batch stream task: `&[&I] → Result<BoxStream<Box<O>>, TaskError>`.
    AsyncStreamBatch(Arc<TypedAsyncStreamBatchFn<I, O>>),
}

impl<I: Value, O: Value> TypedTask<I, O> {
    /// Create a [`TypedTask::Sync`] from a typed closure `&I → Result<Box<O>, TaskError>`.
    pub fn sync<F>(f: F) -> Self
    where
        F: Fn(&I, Arc<TaskContext>) -> Result<Box<O>, TaskError> + Send + Sync + 'static,
    {
        TypedTask::Sync(Arc::new(f))
    }

    /// Create a [`TypedTask::Async`] from a typed closure returning a `'static` future.
    ///
    /// Any data needed inside the async block must be owned (copied/cloned) before
    /// `Box::pin(async move { ... })`.
    pub fn async_fn<F>(f: F) -> Self
    where
        F: Fn(&I, Arc<TaskContext>) -> BoxFuture<'static, Result<Box<O>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        TypedTask::Async(Arc::new(f))
    }

    /// Create a [`TypedTask::SyncIter`] from a typed closure returning a concrete iterator.
    ///
    /// The iterator is boxed into `Box<dyn Iterator<Item=Box<O>>>` at construction time.
    pub fn sync_iter<F, Iter>(f: F) -> Self
    where
        F: Fn(&I, Arc<TaskContext>) -> Result<Iter, TaskError> + Send + Sync + 'static,
        Iter: Iterator<Item = Box<O>> + Send + 'static,
    {
        TypedTask::SyncIter(Arc::new(move |i, ctx| {
            f(i, ctx)
                .map(|iter| Box::new(iter) as Box<dyn Iterator<Item = Box<O>> + Send + 'static>)
        }))
    }

    /// Create a [`TypedTask::AsyncStream`] from a typed closure returning a concrete stream.
    ///
    /// The stream is pinned into a `BoxStream` at construction time.
    pub fn async_stream<F, S>(f: F) -> Self
    where
        F: Fn(&I, Arc<TaskContext>) -> Result<S, TaskError> + Send + Sync + 'static,
        S: Stream<Item = Box<O>> + Send + 'static,
    {
        TypedTask::AsyncStream(Arc::new(move |i, ctx| {
            f(i, ctx).map(|s| Box::pin(s) as BoxStream<'static, Box<O>>)
        }))
    }

    /// Create a [`TypedTask::SyncBatch`] from a typed closure `&[&I] → Result<Box<O>, TaskError>`.
    pub fn sync_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<Box<O>, TaskError>
            + Send
            + Sync
            + 'static,
    {
        TypedTask::SyncBatch(Arc::new(f))
    }

    /// Create a [`TypedTask::AsyncBatch`] from a typed closure returning a `'static` future.
    pub fn async_batch<F>(f: F) -> Self
    where
        F: for<'a> Fn(
                &'a [&'a I],
                Arc<TaskContext>,
            ) -> BoxFuture<'static, Result<Box<O>, TaskError>>
            + Send
            + Sync
            + 'static,
    {
        TypedTask::AsyncBatch(Arc::new(f))
    }

    /// Create a [`TypedTask::SyncIterBatch`] from a typed closure returning a concrete iterator.
    pub fn sync_iter_batch<F, Iter>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<Iter, TaskError>
            + Send
            + Sync
            + 'static,
        Iter: Iterator<Item = Box<O>> + Send + 'static,
    {
        TypedTask::SyncIterBatch(Arc::new(move |items, ctx| {
            f(items, ctx)
                .map(|iter| Box::new(iter) as Box<dyn Iterator<Item = Box<O>> + Send + 'static>)
        }))
    }

    /// Create a [`TypedTask::AsyncStreamBatch`] from a typed closure returning a concrete stream.
    pub fn async_stream_batch<F, S>(f: F) -> Self
    where
        F: for<'a> Fn(&'a [&'a I], Arc<TaskContext>) -> Result<S, TaskError>
            + Send
            + Sync
            + 'static,
        S: Stream<Item = Box<O>> + Send + 'static,
    {
        TypedTask::AsyncStreamBatch(Arc::new(move |items, ctx| {
            f(items, ctx).map(|s| Box::pin(s) as BoxStream<'static, Box<O>>)
        }))
    }
}

impl<I: Value, O: Value> From<TypedTask<I, O>> for Task {
    /// Erase `I` and `O`, producing the type-erased [`Task`].
    ///
    /// Delegates to the corresponding [`Task::sync_typed`] / [`Task::async_fn_typed`] / …
    /// constructor, reusing their downcast logic.
    fn from(typed: TypedTask<I, O>) -> Self {
        match typed {
            TypedTask::Sync(f) => Task::sync_typed(move |i: &I, ctx| f(i, ctx)),
            TypedTask::Async(f) => Task::async_fn_typed(move |i: &I, ctx| f(i, ctx)),
            TypedTask::SyncIter(f) => Task::sync_iter_typed(move |i: &I, ctx| f(i, ctx)),
            TypedTask::AsyncStream(f) => Task::async_stream_typed(move |i: &I, ctx| f(i, ctx)),
            TypedTask::SyncBatch(f) => {
                Task::sync_batch_typed(move |items: &[&I], ctx| f(items, ctx))
            }
            TypedTask::AsyncBatch(f) => {
                Task::async_batch_typed(move |items: &[&I], ctx| f(items, ctx))
            }
            TypedTask::SyncIterBatch(f) => {
                Task::sync_iter_batch_typed(move |items: &[&I], ctx| f(items, ctx))
            }
            TypedTask::AsyncStreamBatch(f) => {
                Task::async_stream_batch_typed(move |items: &[&I], ctx| f(items, ctx))
            }
        }
    }
}

impl<I: Value, O: Value> From<TypedTask<I, O>> for TaskInfo {
    fn from(t: TypedTask<I, O>) -> TaskInfo {
        TaskInfo::new(Task::from(t))
    }
}

/// The pending (or already-resolved) output of [`Task::call`].
pub enum TaskCall {
    /// Already-computed single value (or an error).
    Sync(Result<Arc<dyn Value>, TaskError>),

    /// Future resolving to a single value (or an error).
    Async(BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>),

    /// Lazy iterator of values (or a setup error).
    SyncIter(Result<ValueIter, TaskError>),

    /// Async stream of values (or a setup error).
    AsyncStream(Result<ValueStream, TaskError>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    use crate::cancellation::cancellation_pair;
    use crate::exec_status::NoopExecStatusManager;
    use crate::progress::ProgressToken;
    use crate::task_context::TaskContext;
    use crate::thread_pool::CpuPool;

    // ── Minimal stub for CpuPool (no mock crate needed) ─────────────────────

    struct StubPool;
    impl CpuPool for StubPool {
        fn spawn_raw(
            &self,
            _task: Box<dyn FnOnce() + Send + 'static>,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::error::CoreError>> + Send + 'static>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    async fn stub_ctx() -> Arc<TaskContext> {
        let db = cognee_database::connect("sqlite::memory:").await.unwrap();
        cognee_database::initialize(&db).await.unwrap();
        let (_handle, token) = cancellation_pair();
        Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(db),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: ProgressToken::new(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
        })
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn parallel_runs_sync_tasks_concurrently() {
        // Two sync tasks: one doubles, one triples. Last result (triple) wins.
        let double = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 2)));
        let triple = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 3)));

        let par = Task::parallel(vec![double, triple]);
        let input: Arc<dyn Value> = Arc::new(5_i32);
        let ctx = stub_ctx().await;

        let call = par.call(input, ctx);
        let result = match call {
            TaskCall::Async(fut) => fut.await.unwrap(),
            _ => panic!("parallel should produce Async variant"),
        };

        // Last task (triple) result: 5 * 3 = 15
        assert_eq!(*(*result).as_any().downcast_ref::<i32>().unwrap(), 15);
    }

    #[tokio::test]
    async fn parallel_runs_async_tasks() {
        let add_ten = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x + 10;
            Box::pin(async move { Ok(Box::new(v)) })
        });
        let add_twenty = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x + 20;
            Box::pin(async move { Ok(Box::new(v)) })
        });

        let par = Task::parallel(vec![add_ten, add_twenty]);
        let input: Arc<dyn Value> = Arc::new(100_i32);
        let ctx = stub_ctx().await;

        let result = match par.call(input, ctx) {
            TaskCall::Async(fut) => fut.await.unwrap(),
            _ => panic!("expected Async"),
        };

        // Last task: 100 + 20 = 120
        assert_eq!(*(*result).as_any().downcast_ref::<i32>().unwrap(), 120);
    }

    #[tokio::test]
    async fn parallel_propagates_first_error() {
        let ok_task = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x)));
        let err_task = Task::Sync(Arc::new(|_input, _ctx| Err("boom".into())));

        let par = Task::parallel(vec![ok_task, err_task]);
        let input: Arc<dyn Value> = Arc::new(42_i32);
        let ctx = stub_ctx().await;

        let result = match par.call(input, ctx) {
            TaskCall::Async(fut) => fut.await,
            _ => panic!("expected Async"),
        };

        let err = result.err().expect("should be an error");
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn parallel_empty_returns_input() {
        let par = Task::parallel(vec![]);
        let input: Arc<dyn Value> = Arc::new(99_i32);
        let ctx = stub_ctx().await;

        let result = match par.call(Arc::clone(&input), ctx) {
            TaskCall::Async(fut) => fut.await.unwrap(),
            _ => panic!("expected Async"),
        };

        assert_eq!(*(*result).as_any().downcast_ref::<i32>().unwrap(), 99);
    }

    #[tokio::test]
    async fn test_typed_task_panics_on_type_mismatch() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let task = Task::sync_typed(|_x: &String, _ctx| Ok(Box::new("ok".to_string())));
        let input: Arc<dyn Value> = Arc::new(42_i32); // wrong type
        let ctx = stub_ctx().await;

        let result = catch_unwind(AssertUnwindSafe(|| task.call(input, ctx)));

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("should have panicked on type mismatch"),
        };
        let msg = err
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| err.downcast_ref::<&str>().copied())
            .expect("panic payload should be a string");
        assert!(
            msg.contains("type mismatch"),
            "expected 'type mismatch' in panic message, got: {msg}"
        );
    }

    #[test]
    fn test_taskinfo_weight_default() {
        let info = TaskInfo::new(Task::sync_typed(|_: &i32, _| Ok(Box::new(0_i32))));
        assert_eq!(info.weight, 1);
    }

    #[test]
    fn test_taskinfo_with_weight() {
        let info = TaskInfo::new(Task::sync_typed(|_: &i32, _| Ok(Box::new(0_i32)))).with_weight(5);
        assert_eq!(info.weight, 5);
    }

    #[test]
    fn task_info_parallel_generates_name() {
        let t1 =
            TaskInfo::new(Task::sync_typed(|_: &i32, _| Ok(Box::new(0_i32)))).with_name("classify");
        let t2 =
            TaskInfo::new(Task::sync_typed(|_: &i32, _| Ok(Box::new(0_i32)))).with_name("embed");
        let t3 = TaskInfo::new(Task::sync_typed(|_: &i32, _| Ok(Box::new(0_i32))));

        let par = TaskInfo::parallel(vec![t1, t2, t3]);
        assert_eq!(
            par.name.as_deref(),
            Some("parallel([classify, embed, task_2])")
        );
    }
}
