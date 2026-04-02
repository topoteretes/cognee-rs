use std::sync::Arc;

use futures::stream::StreamExt;
use neon::prelude::*;
use neon::types::buffer::TypedArray;

use cognee_core::TaskContext as CogneeTaskContext;
use cognee_core::{Task, TaskError, Value, ValueStream};

use crate::value::{JsObjectHolder, value_to_js};

/// Wrapper around `Task` stored in `JsBox`.
pub struct NeonTask {
    pub inner: Task,
}

impl Finalize for NeonTask {}

/// Check whether a JS value is a thenable (has a `.then` method that is a function).
fn is_thenable<'cx>(cx: &mut impl Context<'cx>, val: &Handle<'cx, JsValue>) -> bool {
    if let Ok(obj) = val.downcast::<JsObject, _>(cx)
        && let Ok(then) = obj.get::<JsValue, _, _>(cx, "then")
    {
        return then.is_a::<JsFunction, _>(cx);
    }
    false
}

/// Convert a JS value back to `Result<Arc<dyn Value>, TaskError>`.
fn js_result_to_value<'cx>(
    cx: &mut impl Context<'cx>,
    handle: Handle<'cx, JsValue>,
) -> Result<Arc<dyn Value>, TaskError> {
    if let Ok(n) = handle.downcast::<JsNumber, _>(cx) {
        return Ok(Arc::new(n.value(cx)));
    }
    if let Ok(b) = handle.downcast::<JsBoolean, _>(cx) {
        return Ok(Arc::new(b.value(cx)));
    }
    if let Ok(s) = handle.downcast::<JsString, _>(cx) {
        return Ok(Arc::new(s.value(cx)));
    }
    if let Ok(buf) = handle.downcast::<JsBuffer, _>(cx) {
        let bytes: Vec<u8> = buf.as_slice(cx).to_vec();
        return Ok(Arc::new(bytes));
    }
    if handle.is_a::<JsNull, _>(cx) || handle.is_a::<JsUndefined, _>(cx) {
        return Err("task returned null/undefined".into());
    }
    if let Ok(obj) = handle.downcast::<JsObject, _>(cx) {
        let root = obj.root(cx);
        return Ok(Arc::new(JsObjectHolder { root }));
    }
    Err("unsupported return type from JS task".into())
}

/// Handle a sync or thenable result, sending through the oneshot channel.
fn settle_js_result<'a>(
    cx: &mut TaskContext<'a>,
    result: Handle<'a, JsValue>,
    tx: tokio::sync::oneshot::Sender<Result<Arc<dyn Value>, TaskError>>,
) {
    if !is_thenable(cx, &result) {
        let val = js_result_to_value(cx, result);
        let _ = tx.send(val);
        return;
    }

    // Promise path
    let Ok(promise) = result.downcast::<JsObject, _>(cx) else {
        let _ = tx.send(Err("thenable is not an object".into()));
        return;
    };

    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));

    let tx_then = Arc::clone(&tx);
    let Ok(then_fn) = JsFunction::new(cx, move |mut cx| {
        let resolved = cx.argument::<JsValue>(0)?;
        let val = js_result_to_value(&mut cx, resolved);
        if let Some(tx) = tx_then.lock().unwrap().take() {
            let _ = tx.send(val);
        }
        Ok(cx.undefined())
    }) else {
        return;
    };

    let tx_catch = Arc::clone(&tx);
    let Ok(catch_fn) = JsFunction::new(cx, move |mut cx| {
        let err = cx.argument::<JsValue>(0)?;
        let msg = if let Ok(s) = err.downcast::<JsString, _>(&mut cx) {
            s.value(&mut cx)
        } else {
            "JS task rejected".to_string()
        };
        if let Some(tx) = tx_catch.lock().unwrap().take() {
            let _ = tx.send(Err(msg.into()));
        }
        Ok(cx.undefined())
    }) else {
        return;
    };

    let Ok(then_method) = promise.get::<JsFunction, _, _>(cx, "then") else {
        return;
    };
    let Ok(chained) = then_method.call(cx, promise, [then_fn.upcast()]) else {
        return;
    };
    let Ok(chained_obj) = chained.downcast::<JsObject, _>(cx) else {
        return;
    };
    let Ok(catch_method) = chained_obj.get::<JsFunction, _, _>(cx, "catch") else {
        return;
    };
    let _ = catch_method.call(cx, chained_obj, [catch_fn.upcast()]);
}

// ── Task constructors ───────────────────────────────────────────────────────

/// Create a `Task::Async` from a JS function.
///
/// JS signature: `(value) => value | Promise<value>`
pub fn create_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let js_fn = cx.argument::<JsFunction>(0)?.root(&mut cx);
    let channel = cx.channel();
    let js_fn = Arc::new(js_fn);

    let task = Task::async_fn(move |input: Arc<dyn Value>, _ctx: Arc<CogneeTaskContext>| {
        let js_fn = Arc::clone(&js_fn);
        let channel = channel.clone();

        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();

            channel.send(move |mut cx| {
                let js_fn_inner = js_fn.to_inner(&mut cx);
                let js_input = value_to_js(&mut cx, input.as_ref())
                    .unwrap_or_else(|_| cx.undefined().upcast());

                match js_fn_inner
                    .call_with(&cx)
                    .arg(js_input)
                    .apply::<JsValue, _>(&mut cx)
                {
                    Ok(val) => settle_js_result(&mut cx, val, tx),
                    Err(_) => {
                        let _ = tx.send(Err("JS task threw an error".into()));
                    }
                }
                Ok(())
            });

            rx.await
                .map_err(|_| -> TaskError { "JS callback channel dropped".into() })?
        })
    });

    Ok(cx.boxed(NeonTask { inner: task }))
}

/// Create a task from a JS function that returns an array (fan-out).
///
/// JS signature: `(value) => value[] | Promise<value[]>`
///
/// Uses `Task::AsyncStream` so the pipeline executor fans out each element
/// to subsequent tasks individually.
pub fn create_iter_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let js_fn = cx.argument::<JsFunction>(0)?.root(&mut cx);
    let channel = cx.channel();
    let js_fn = Arc::new(js_fn);

    let task = Task::async_stream(move |input: Arc<dyn Value>, _ctx: Arc<CogneeTaskContext>| {
        let js_fn = Arc::clone(&js_fn);
        let channel = channel.clone();

        // We return a stream that first awaits the JS call, then yields each item.
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Vec<Box<dyn Value>>, TaskError>>();

        channel.send(move |mut cx| {
            let js_fn_inner = js_fn.to_inner(&mut cx);
            let js_input =
                value_to_js(&mut cx, input.as_ref()).unwrap_or_else(|_| cx.undefined().upcast());

            match js_fn_inner
                .call_with(&cx)
                .arg(js_input)
                .apply::<JsValue, _>(&mut cx)
            {
                Ok(val) => {
                    if is_thenable(&mut cx, &val) {
                        settle_array_promise(&mut cx, val, tx);
                    } else {
                        let result = convert_array_boxed(&mut cx, val);
                        let _ = tx.send(result);
                    }
                }
                Err(_) => {
                    let _ = tx.send(Err("JS iter task threw an error".into()));
                }
            }
            Ok(())
        });

        // Create a stream: await the oneshot, then yield each item.
        let stream: ValueStream = Box::pin(
            futures::stream::once(async move {
                rx.await
                    .unwrap_or_else(|_| Err("JS callback channel dropped".into()))
            })
            .flat_map(|result| {
                match result {
                    Ok(items) => futures::stream::iter(items).left_stream(),
                    Err(_) => {
                        // On error, yield nothing. The error was already logged.
                        futures::stream::empty().right_stream()
                    }
                }
            }),
        );

        Ok(stream)
    });

    Ok(cx.boxed(NeonTask { inner: task }))
}

/// Attach .then/.catch to a Promise that should resolve to an array.
fn settle_array_promise(
    cx: &mut TaskContext,
    val: Handle<JsValue>,
    tx: tokio::sync::oneshot::Sender<Result<Vec<Box<dyn Value>>, TaskError>>,
) {
    let Ok(promise) = val.downcast::<JsObject, _>(cx) else {
        let _ = tx.send(Err("thenable is not an object".into()));
        return;
    };

    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));

    let tx_then = Arc::clone(&tx);
    let Ok(then_fn) = JsFunction::new(cx, move |mut cx| {
        let resolved = cx.argument::<JsValue>(0)?;
        let result = convert_array_boxed(&mut cx, resolved);
        if let Some(tx) = tx_then.lock().unwrap().take() {
            let _ = tx.send(result);
        }
        Ok(cx.undefined())
    }) else {
        return;
    };

    let tx_catch = Arc::clone(&tx);
    let Ok(catch_fn) = JsFunction::new(cx, move |mut cx| {
        let err = cx.argument::<JsValue>(0)?;
        let msg = if let Ok(s) = err.downcast::<JsString, _>(&mut cx) {
            s.value(&mut cx)
        } else {
            "iter task rejected".to_string()
        };
        if let Some(tx) = tx_catch.lock().unwrap().take() {
            let _ = tx.send(Err(msg.into()));
        }
        Ok(cx.undefined())
    }) else {
        return;
    };

    let Ok(then_method) = promise.get::<JsFunction, _, _>(cx, "then") else {
        return;
    };
    let Ok(chained) = then_method.call(cx, promise, [then_fn.upcast()]) else {
        return;
    };
    let Ok(chained_obj) = chained.downcast::<JsObject, _>(cx) else {
        return;
    };
    let Ok(catch_method) = chained_obj.get::<JsFunction, _, _>(cx, "catch") else {
        return;
    };
    let _ = catch_method.call(cx, chained_obj, [catch_fn.upcast()]);
}

/// Convert a JS array to a `Vec<Box<dyn Value>>` for use as stream items.
fn convert_array_boxed<'cx>(
    cx: &mut impl Context<'cx>,
    val: Handle<'cx, JsValue>,
) -> Result<Vec<Box<dyn Value>>, TaskError> {
    let arr = val
        .downcast::<JsArray, _>(cx)
        .map_err(|_| -> TaskError { "iter task must return an array".into() })?;
    let len = arr.len(cx);
    let mut items: Vec<Box<dyn Value>> = Vec::with_capacity(len as usize);
    for i in 0..len {
        let item: Handle<JsValue> = arr
            .get(cx, i)
            .map_err(|_| -> TaskError { format!("failed to read array element {i}").into() })?;
        // Convert JS value directly to a concrete Box<T>.
        let boxed: Box<dyn Value> = if let Ok(n) = item.downcast::<JsNumber, _>(cx) {
            Box::new(n.value(cx))
        } else if let Ok(b) = item.downcast::<JsBoolean, _>(cx) {
            Box::new(b.value(cx))
        } else if let Ok(s) = item.downcast::<JsString, _>(cx) {
            Box::new(s.value(cx))
        } else if let Ok(buf) = item.downcast::<JsBuffer, _>(cx) {
            Box::new(buf.as_slice(cx).to_vec())
        } else if let Ok(obj) = item.downcast::<JsObject, _>(cx) {
            let root = obj.root(cx);
            Box::new(JsObjectHolder { root })
        } else {
            return Err("unsupported type in iter array".into());
        };
        items.push(boxed);
    }
    Ok(items)
}

/// Serializable representation of a Value for sending across threads.
/// Used by batch tasks to clone `&[Box<dyn Value>]` items into owned data.
enum OwnedValue {
    F64(f64),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
}

impl OwnedValue {
    fn from_dyn(val: &dyn Value) -> Self {
        let any = val.as_any();
        if let Some(&v) = any.downcast_ref::<f64>() {
            OwnedValue::F64(v)
        } else if let Some(&v) = any.downcast_ref::<bool>() {
            OwnedValue::Bool(v)
        } else if let Some(v) = any.downcast_ref::<String>() {
            OwnedValue::Str(v.clone())
        } else if let Some(v) = any.downcast_ref::<Vec<u8>>() {
            OwnedValue::Bytes(v.clone())
        } else {
            // Try i32 (common from iterator .map(Number))
            if let Some(&v) = any.downcast_ref::<i32>() {
                OwnedValue::F64(v as f64)
            } else {
                OwnedValue::Str(format!("<opaque {:p}>", val))
            }
        }
    }

    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> Handle<'cx, JsValue> {
        match self {
            OwnedValue::F64(v) => cx.number(*v).upcast(),
            OwnedValue::Bool(v) => cx.boolean(*v).upcast(),
            OwnedValue::Str(v) => cx.string(v).upcast(),
            OwnedValue::Bytes(v) => {
                let mut buf = cx.buffer(v.len()).unwrap();
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
        }
    }
}

/// Create a `Task::AsyncBatch` from a JS function.
///
/// JS signature: `(values[]) => value | Promise<value>`
pub fn create_batch_task(mut cx: FunctionContext) -> JsResult<JsBox<NeonTask>> {
    let js_fn = cx.argument::<JsFunction>(0)?.root(&mut cx);
    let channel = cx.channel();
    let js_fn = Arc::new(js_fn);

    let task = Task::async_batch(
        move |items: &[Box<dyn Value>], _ctx: Arc<CogneeTaskContext>| {
            let js_fn = Arc::clone(&js_fn);
            let channel = channel.clone();

            // Serialize items into owned data that can cross thread boundaries.
            let owned: Vec<OwnedValue> = items
                .iter()
                .map(|item| OwnedValue::from_dyn(item.as_ref()))
                .collect();

            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();

                channel.send(move |mut cx| {
                    let js_fn_inner = js_fn.to_inner(&mut cx);

                    let arr = JsArray::new(&mut cx, owned.len());
                    for (i, item) in owned.iter().enumerate() {
                        let js_val = item.to_js(&mut cx);
                        let _ = arr.set(&mut cx, i as u32, js_val);
                    }

                    match js_fn_inner
                        .call_with(&cx)
                        .arg(arr)
                        .apply::<JsValue, _>(&mut cx)
                    {
                        Ok(val) => settle_js_result(&mut cx, val, tx),
                        Err(_) => {
                            let _ = tx.send(Err("JS batch task threw an error".into()));
                        }
                    }
                    Ok(())
                });

                rx.await
                    .map_err(|_| -> TaskError { "JS callback channel dropped".into() })?
            })
        },
    );

    Ok(cx.boxed(NeonTask { inner: task }))
}
