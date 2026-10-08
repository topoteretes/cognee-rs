//! JS function → cognee-core `Task` bridging.
//!
//! Every task built here is async on the Rust side: the JS callback runs on
//! the Node event loop (reached through a Neon [`Channel`]) and the pipeline
//! awaits its result off-thread. A callback may return a plain value or a
//! Promise; either way the result is routed through `Promise.resolve(..)` so
//! both cases share one settle path.
//!
//! Callbacks receive `(input, ctx)`, where `ctx` is a boxed
//! [`NeonTaskContext`] that the TS layer wraps in its `TaskContext` class.
//!
//! Failure handling — every one of these becomes a `TaskError` carrying the
//! JS message, never a silent empty result or an escaped exception:
//! - a synchronous throw (caught with `try_catch`, so no exception is left
//!   pending on the context);
//! - a rejected Promise (an `Error`'s `message` is kept, not just strings);
//! - a stream callback that throws or rejects mid-stream, or yields an
//!   unsupported value (surfaced as an `Err` stream item, which fails the
//!   producing task — see [`cognee_core::ValueStream`]).

use std::sync::{Arc, Mutex};

use futures::stream::StreamExt;
use neon::prelude::*;
use neon::types::buffer::TypedArray;
use tokio::sync::oneshot;

use cognee_core::TaskContext as CogneeTaskContext;
use cognee_core::{Task, TaskError, Value, ValueStream};

use crate::task_context::NeonTaskContext;
use crate::value::{JsObjectHolder, value_to_js};

/// Wrapper around `Task` stored in `JsBox`.
pub struct NeonTask {
    pub inner: Task,
}

impl Finalize for NeonTask {}

// ── Value conversion ────────────────────────────────────────────────────────

/// Convert a value produced by a JS callback into an owned pipeline value.
fn js_to_boxed<'cx>(
    cx: &mut impl Context<'cx>,
    handle: Handle<'cx, JsValue>,
) -> Result<Box<dyn Value>, TaskError> {
    if let Ok(n) = handle.downcast::<JsNumber, _>(cx) {
        return Ok(Box::new(n.value(cx)));
    }
    if let Ok(b) = handle.downcast::<JsBoolean, _>(cx) {
        return Ok(Box::new(b.value(cx)));
    }
    if let Ok(s) = handle.downcast::<JsString, _>(cx) {
        return Ok(Box::new(s.value(cx)));
    }
    if let Ok(buf) = handle.downcast::<JsBuffer, _>(cx) {
        return Ok(Box::new(buf.as_slice(cx).to_vec()));
    }
    if handle.is_a::<JsNull, _>(cx) || handle.is_a::<JsUndefined, _>(cx) {
        return Err("task returned null/undefined".into());
    }
    if let Ok(obj) = handle.downcast::<JsObject, _>(cx) {
        return Ok(Box::new(JsObjectHolder::new(cx, obj)));
    }
    Err("unsupported value type returned from JS task".into())
}

/// A pipeline value copied out of a `&[Box<dyn Value>]` batch so it can be
/// moved to the JS thread (the slice itself is only borrowed for the duration
/// of the batch closure, while the callback runs later).
enum BatchItem {
    F64(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
    Object(JsObjectHolder),
}

impl BatchItem {
    fn from_dyn(val: &dyn Value) -> Result<Self, TaskError> {
        let any = val.as_any();
        if let Some(&v) = any.downcast_ref::<f64>() {
            Ok(BatchItem::F64(v))
        } else if let Some(&v) = any.downcast_ref::<i32>() {
            // Common from Rust-side tasks (`.map(Number)`-style iterators).
            Ok(BatchItem::F64(v as f64))
        } else if let Some(&v) = any.downcast_ref::<bool>() {
            Ok(BatchItem::Bool(v))
        } else if let Some(v) = any.downcast_ref::<String>() {
            Ok(BatchItem::Str(v.clone()))
        } else if let Some(v) = any.downcast_ref::<Vec<u8>>() {
            Ok(BatchItem::Bytes(v.clone()))
        } else if let Some(v) = any.downcast_ref::<JsObjectHolder>() {
            Ok(BatchItem::Object(v.share()))
        } else {
            Err("batch item is a native value with no JS representation".into())
        }
    }

    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> JsResult<'cx, JsValue> {
        Ok(match self {
            BatchItem::F64(v) => cx.number(*v).upcast(),
            BatchItem::Bool(v) => cx.boolean(*v).upcast(),
            BatchItem::Str(v) => cx.string(v).upcast(),
            BatchItem::Bytes(v) => {
                let mut buf = cx.buffer(v.len())?;
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
            BatchItem::Object(v) => v.to_inner(cx).upcast(),
        })
    }
}

fn batch_items(items: &[Box<dyn Value>]) -> Result<Vec<BatchItem>, TaskError> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            BatchItem::from_dyn(item.as_ref())
                .map_err(|e| -> TaskError { format!("batch item {i}: {e}").into() })
        })
        .collect()
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Human-readable message for a thrown or rejected JS value.
///
/// Strings pass through; `Error`-like objects yield their `message`, prefixed
/// with `name` unless it is the plain `"Error"`; anything else goes through
/// `String(value)`.
fn js_error_message<'cx>(cx: &mut impl Context<'cx>, err: Handle<'cx, JsValue>) -> String {
    if let Ok(s) = err.downcast::<JsString, _>(cx) {
        return s.value(cx);
    }
    if let Ok(obj) = err.downcast::<JsObject, _>(cx)
        && let Ok(msg) = obj.get_value(cx, "message")
        && let Ok(msg) = msg.downcast::<JsString, _>(cx)
    {
        let msg = msg.value(cx);
        let name = obj
            .get_value(cx, "name")
            .ok()
            .and_then(|n| n.downcast::<JsString, _>(cx).ok())
            .map(|n| n.value(cx));
        return match name {
            Some(name) if !name.is_empty() && name != "Error" => format!("{name}: {msg}"),
            _ => msg,
        };
    }
    // `String(value)` can itself throw (a Symbol, a hostile `toString`), so
    // it gets its own try_catch rather than leaving an exception pending.
    match cx.try_catch(|cx| <JsValue as neon::types::Value>::to_string(&err, cx)) {
        Ok(s) => s.value(cx),
        Err(_) => "JS task failed with a value that cannot be converted to a string".into(),
    }
}

fn into_task_error<'cx>(cx: &mut impl Context<'cx>, err: Handle<'cx, JsValue>) -> TaskError {
    js_error_message(cx, err).into()
}

// ── JS-thread plumbing ──────────────────────────────────────────────────────

/// Converts a settled JS value into the Rust type a task waits for.
type Convert<T> = for<'a> fn(&mut FunctionContext<'a>, Handle<'a, JsValue>) -> Result<T, TaskError>;

type ResultSlot<T> = Arc<Mutex<Option<oneshot::Sender<Result<T, TaskError>>>>>;

fn fill<T>(slot: &ResultSlot<T>, result: Result<T, TaskError>) {
    // lock poison is unrecoverable
    if let Some(tx) = slot.lock().unwrap().take() {
        let _ = tx.send(result);
    }
}

/// Settle `value` — a plain value or a thenable — into `slot`, converting the
/// fulfilment with `convert` and a rejection into its message.
fn settle<'a, T: Send + 'static>(
    cx: &mut TaskContext<'a>,
    value: Handle<'a, JsValue>,
    slot: ResultSlot<T>,
    convert: Convert<T>,
) {
    let wired = cx.try_catch(|cx| {
        // The constructor is a function, which a `JsObject` downcast rejects.
        let promise_ctor: Handle<JsFunction> = cx.global("Promise")?;
        let resolve: Handle<JsFunction> = promise_ctor.get(cx, "resolve")?;
        let promise: Handle<JsObject> = resolve
            .call_with(cx)
            .this(promise_ctor)
            .arg(value)
            .apply(cx)?;

        let on_fulfilled_slot = Arc::clone(&slot);
        let on_fulfilled = JsFunction::new(cx, move |mut cx| {
            let v = cx.argument::<JsValue>(0)?;
            let result = convert(&mut cx, v);
            fill(&on_fulfilled_slot, result);
            Ok(cx.undefined())
        })?;
        let on_rejected_slot = Arc::clone(&slot);
        let on_rejected = JsFunction::new(cx, move |mut cx| {
            let e = cx.argument::<JsValue>(0)?;
            let err = into_task_error(&mut cx, e);
            fill(&on_rejected_slot, Err(err));
            Ok(cx.undefined())
        })?;

        let then: Handle<JsFunction> = promise.get(cx, "then")?;
        then.call_with(cx)
            .this(promise)
            .arg(on_fulfilled)
            .arg(on_rejected)
            .exec(cx)
    });
    if let Err(e) = wired {
        let err = into_task_error(cx, e);
        fill(&slot, Err(err));
    }
}

/// Run `call` on the JS thread and wait for the value it produces to settle.
///
/// `call` returns the handle to settle (a value or a Promise), or an error if
/// it could not even produce one (e.g. the callback threw synchronously).
async fn on_js_thread<T, F>(channel: &Channel, call: F, convert: Convert<T>) -> Result<T, TaskError>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&mut TaskContext<'a>) -> Result<Handle<'a, JsValue>, TaskError>
        + Send
        + 'static,
{
    let (tx, rx) = oneshot::channel();
    let slot: ResultSlot<T> = Arc::new(Mutex::new(Some(tx)));
    channel.send(move |mut cx| {
        match call(&mut cx) {
            Ok(value) => settle(&mut cx, value, slot, convert),
            Err(e) => fill(&slot, Err(e)),
        }
        Ok(())
    });
    rx.await
        .map_err(|_| -> TaskError { "JS callback channel dropped".into() })?
}

/// Invoke the user callback as `f(arg, ctx)`, catching a synchronous throw.
fn call_callback<'a>(
    cx: &mut TaskContext<'a>,
    f: &Root<JsFunction>,
    arg: Handle<'a, JsValue>,
    ctx: &Arc<CogneeTaskContext>,
) -> Result<Handle<'a, JsValue>, TaskError> {
    let f = f.to_inner(cx);
    let js_ctx = cx.boxed(NeonTaskContext {
        inner: Arc::clone(ctx),
    });
    match cx.try_catch(|cx| f.call_with(cx).arg(arg).arg(js_ctx).apply::<JsValue, _>(cx)) {
        Ok(v) => Ok(v),
        Err(e) => Err(into_task_error(cx, e)),
    }
}

/// Run a conversion that may throw (e.g. a Buffer allocation) without leaving
/// an exception pending on the context.
fn catching<'a, V>(
    cx: &mut TaskContext<'a>,
    f: impl FnOnce(&mut TaskContext<'a>) -> NeonResult<V>,
) -> Result<V, TaskError> {
    match cx.try_catch(f) {
        Ok(v) => Ok(v),
        Err(e) => Err(into_task_error(cx, e)),
    }
}

fn batch_array<'a>(
    cx: &mut TaskContext<'a>,
    items: &[BatchItem],
) -> Result<Handle<'a, JsValue>, TaskError> {
    catching(cx, |cx| {
        let arr = JsArray::new(cx, items.len());
        for (i, item) in items.iter().enumerate() {
            let v = item.to_js(cx)?;
            arr.set(cx, i as u32, v)?;
        }
        Ok(arr.upcast())
    })
}

/// The rooted callback plus the channel used to reach the JS thread.
#[derive(Clone)]
struct Callback {
    f: Arc<Root<JsFunction>>,
    channel: Channel,
}

impl Callback {
    fn from_argument(cx: &mut FunctionContext) -> NeonResult<Self> {
        let f = cx.argument::<JsFunction>(0)?.root(cx);
        Ok(Self {
            f: Arc::new(f),
            channel: cx.channel(),
        })
    }
}

// ── Converters ──────────────────────────────────────────────────────────────

fn to_single<'a>(
    cx: &mut FunctionContext<'a>,
    v: Handle<'a, JsValue>,
) -> Result<Arc<dyn Value>, TaskError> {
    js_to_boxed(cx, v).map(Arc::from)
}

fn to_iterator<'a>(
    cx: &mut FunctionContext<'a>,
    v: Handle<'a, JsValue>,
) -> Result<Arc<Root<JsObject>>, TaskError> {
    let obj = v
        .downcast::<JsObject, _>(cx)
        .map_err(|_| -> TaskError { "stream task must return an async iterator".into() })?;
    let next = obj.get_value(cx, "next").ok();
    if !next.is_some_and(|n| n.is_a::<JsFunction, _>(cx)) {
        return Err("stream task must return an async iterator (no next() method)".into());
    }
    Ok(Arc::new(obj.root(cx)))
}

/// One `IteratorResult` (`{ done, value }`) → `None` when done, else the item.
fn to_step<'a>(
    cx: &mut FunctionContext<'a>,
    v: Handle<'a, JsValue>,
) -> Result<Option<Box<dyn Value>>, TaskError> {
    let step = v
        .downcast::<JsObject, _>(cx)
        .map_err(|_| -> TaskError { "async iterator next() must resolve to an object".into() })?;
    let done = step
        .get_value(cx, "done")
        .ok()
        .and_then(|d| d.downcast::<JsBoolean, _>(cx).ok())
        .is_some_and(|d| d.value(cx));
    if done {
        return Ok(None);
    }
    let value = step
        .get_value(cx, "value")
        .map_err(|_| -> TaskError { "could not read IteratorResult.value".into() })?;
    js_to_boxed(cx, value).map(Some)
}

// ── Streams ─────────────────────────────────────────────────────────────────

/// Produces the async iterator a stream task pulls from.
type Start =
    Box<dyn for<'a> FnOnce(&mut TaskContext<'a>) -> Result<Handle<'a, JsValue>, TaskError> + Send>;

fn start_with<F>(f: F) -> Start
where
    F: for<'a> FnOnce(&mut TaskContext<'a>) -> Result<Handle<'a, JsValue>, TaskError>
        + Send
        + 'static,
{
    Box::new(f)
}

enum StreamState {
    NotStarted(Channel, Start),
    Pulling(Channel, Arc<Root<JsObject>>),
    Done,
}

/// Pull one item from a JS async iterator.
async fn pull(
    channel: &Channel,
    iter: &Arc<Root<JsObject>>,
) -> Result<Option<Box<dyn Value>>, TaskError> {
    let iter = Arc::clone(iter);
    on_js_thread(
        channel,
        move |cx| {
            let it = iter.to_inner(cx);
            catching(cx, |cx| {
                let next: Handle<JsFunction> = it.get(cx, "next")?;
                next.call_with(cx).this(it).apply::<JsValue, _>(cx)
            })
        },
        to_step,
    )
    .await
}

/// A [`ValueStream`] that obtains a JS async iterator via `start` and pulls
/// it one item per round-trip to the JS thread, so the producer advances only
/// as fast as the pipeline consumes. The first failure is yielded as an `Err`
/// item and ends the stream.
fn js_stream(channel: Channel, start: Start) -> ValueStream {
    let stream = futures::stream::unfold(
        StreamState::NotStarted(channel, start),
        |state| async move {
            let (channel, iter) = match state {
                StreamState::Done => return None,
                StreamState::NotStarted(channel, start) => {
                    match on_js_thread(&channel, start, to_iterator).await {
                        Ok(iter) => (channel, iter),
                        Err(e) => return Some((Err(e), StreamState::Done)),
                    }
                }
                StreamState::Pulling(channel, iter) => (channel, iter),
            };
            match pull(&channel, &iter).await {
                Ok(Some(item)) => Some((Ok(item), StreamState::Pulling(channel, iter))),
                Ok(None) => None,
                Err(e) => Some((Err(e), StreamState::Done)),
            }
        },
    );
    stream.boxed()
}

// ── Task constructors ───────────────────────────────────────────────────────

/// Create a `Task::Async` from a JS function.
///
/// JS signature: `(value, ctx) => value | Promise<value>`
pub fn create_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let callback = Callback::from_argument(&mut cx)?;

    let task = Task::async_fn(move |input: Arc<dyn Value>, ctx: Arc<CogneeTaskContext>| {
        let callback = callback.clone();
        Box::pin(async move {
            let Callback { f, channel } = callback;
            on_js_thread(
                &channel,
                move |cx| {
                    let arg = catching(cx, |cx| value_to_js(cx, input.as_ref()))?;
                    call_callback(cx, &f, arg, &ctx)
                },
                to_single,
            )
            .await
        })
    });

    Ok(cx.boxed(NeonTask { inner: task }))
}

/// Create a `Task::AsyncStream` from a JS function returning an async iterator.
///
/// JS signature: `(value, ctx) => AsyncIterator<value>`. The TS layer turns
/// arrays, iterables and async iterables into one async iterator, so this
/// side only ever pulls `next()`.
pub fn create_iter_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let callback = Callback::from_argument(&mut cx)?;

    let task = Task::async_stream(move |input: Arc<dyn Value>, ctx: Arc<CogneeTaskContext>| {
        let Callback { f, channel } = callback.clone();
        let start = start_with(move |cx| {
            let arg = catching(cx, |cx| value_to_js(cx, input.as_ref()))?;
            call_callback(cx, &f, arg, &ctx)
        });
        Ok(js_stream(channel, start))
    });

    Ok(cx.boxed(NeonTask { inner: task }))
}

/// Create a `Task::AsyncBatch` from a JS function.
///
/// JS signature: `(values[], ctx) => value | Promise<value>`
pub fn create_batch_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let callback = Callback::from_argument(&mut cx)?;

    let task = Task::async_batch(
        move |items: &[Box<dyn Value>], ctx: Arc<CogneeTaskContext>| {
            let Callback { f, channel } = callback.clone();
            // Copied now: the slice is only borrowed for this call.
            let items = batch_items(items);
            Box::pin(async move {
                let items = items?;
                on_js_thread(
                    &channel,
                    move |cx| {
                        let arg = batch_array(cx, &items)?;
                        call_callback(cx, &f, arg, &ctx)
                    },
                    to_single,
                )
                .await
            })
        },
    );

    Ok(cx.boxed(NeonTask { inner: task }))
}

/// Create a `Task::AsyncStreamBatch` from a JS function.
///
/// JS signature: `(values[], ctx) => AsyncIterator<value>` (normalised by the
/// TS layer, as for [`create_iter_task`]).
pub fn create_iter_batch_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let callback = Callback::from_argument(&mut cx)?;

    let task = Task::async_stream_batch(
        move |items: &[Box<dyn Value>], ctx: Arc<CogneeTaskContext>| {
            let Callback { f, channel } = callback.clone();
            let items = batch_items(items)?;
            let start = start_with(move |cx| {
                let arg = batch_array(cx, &items)?;
                call_callback(cx, &f, arg, &ctx)
            });
            Ok(js_stream(channel, start))
        },
    );

    Ok(cx.boxed(NeonTask { inner: task }))
}
