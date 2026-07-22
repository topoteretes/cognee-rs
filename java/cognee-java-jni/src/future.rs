//! Async up-call machinery: run an op on the tokio runtime and settle a Java
//! `CompletableFuture` from a worker thread. See design §6.

use std::future::Future;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject};
use jni::{JNIEnv, JavaVM};

use cognee_bindings_common::SdkError;

use crate::errors::{throw_cognee_exception, throw_sdk_error};
use crate::java_vm;
use crate::runtime::runtime;

/// `ai.cognee.CogneeException` class, resolved once on an app-classloader thread
/// and pinned, so worker threads can construct it without `find_class`.
static EXCEPTION_CLASS: OnceLock<GlobalRef> = OnceLock::new();

/// Warm the exception-class cache on the *current* (initiating) JNI thread.
/// Returns false only if the class cannot be resolved.
fn ensure_exception_class(env: &mut JNIEnv) -> bool {
    if EXCEPTION_CLASS.get().is_some() {
        return true;
    }
    let Ok(class) = env.find_class("ai/cognee/CogneeException") else {
        return false;
    };
    let Ok(global) = env.new_global_ref(&class) else {
        return false;
    };
    let _ = EXCEPTION_CLASS.set(global);
    true
}

/// Spawn `fut` on the runtime and settle `future` (a `CompletableFuture`) from a
/// tokio worker thread. Must be called from a Java thread (the initiating op
/// call) so the exception-class cache is warmed with the app class loader.
pub(crate) fn spawn_future<F>(env: &mut JNIEnv, future: &JObject, fut: F)
where
    F: Future<Output = Result<serde_json::Value, SdkError>> + Send + 'static,
{
    if !ensure_exception_class(env) {
        throw_cognee_exception(
            env,
            "RUNTIME_ERROR",
            "could not resolve ai.cognee.CogneeException",
        );
        return;
    }
    let global = match env.new_global_ref(future) {
        Ok(g) => g,
        Err(_) => {
            throw_cognee_exception(
                env,
                "RUNTIME_ERROR",
                "could not create a global ref for the future",
            );
            return;
        }
    };
    let vm: &'static JavaVM = java_vm();

    // Build (or fetch) the runtime up front so a build failure is thrown
    // synchronously to the initiating thread rather than lost inside a task.
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            throw_sdk_error(env, e);
            return;
        }
    };

    rt.spawn(async move {
        // Inner spawn isolates op-body panics into a JoinError.
        let outcome = rt.spawn(fut).await;

        // Daemon attach: pooled worker threads stay attached and never block
        // JVM shutdown. Re-attaching an already-attached thread is a no-op.
        let mut env = match vm.attach_current_thread_as_daemon() {
            Ok(e) => e,
            Err(_) => {
                drop(global); // still release the ref
                return;
            }
        };

        // Settle inside a local frame so every JNI local ref created per
        // completion (`new_string`, the class local-ref, the exception object)
        // is freed when the frame pops. Daemon worker threads never detach, so
        // without this these locals would accumulate unbounded (fatal on ART).
        let framed = env.with_local_frame::<_, (), jni::errors::Error>(16, |env| {
            let settled = match outcome {
                Ok(Ok(value)) => complete_ok(env, global.as_obj(), &value.to_string()),
                Ok(Err(sdk)) => complete_err(env, global.as_obj(), sdk.code(), &sdk.to_string()),
                Err(join_err) => {
                    let msg = if join_err.is_panic() {
                        "native task panicked"
                    } else {
                        "native task was cancelled"
                    };
                    complete_err(env, global.as_obj(), "RUNTIME_ERROR", msg)
                }
            };

            // If the up-call itself failed the future is still unsettled, so a
            // Java `.join()` would hang forever. Best-effort fallback: clear any
            // pending exception, then try `completeExceptionally` with a plain
            // message. If that also fails, clear and log — never panic.
            if settled.is_err() {
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_clear();
                }
                let fallback = complete_err(
                    env,
                    global.as_obj(),
                    "RUNTIME_ERROR",
                    "cognee: failed to settle the future",
                );
                if fallback.is_err() {
                    if env.exception_check().unwrap_or(false) {
                        let _ = env.exception_clear();
                    }
                    eprintln!(
                        "[cognee-java] failed to settle CompletableFuture: both \
                         the result up-call and the fallback up-call failed"
                    );
                }
            }
            Ok(())
        });

        // `with_local_frame` only returns Err when `push_local_frame` itself
        // fails — in which case the settle closure never ran and the future is
        // still unsettled, so a Java `.join()` would hang forever. Settle it
        // directly without a frame (a handful of leaked locals on this rare
        // error path is far better than a permanent hang), clearing any pending
        // exception before and after so a pooled worker thread never carries one.
        if framed.is_err() {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            let _ = complete_err(
                &mut env,
                global.as_obj(),
                "RUNTIME_ERROR",
                "cognee: could not allocate a JNI local frame to settle the future",
            );
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
        }

        drop(global); // release the global ref on EVERY path
    });
}

/// `future.complete(jsonString)`.
fn complete_ok(env: &mut JNIEnv, future: &JObject, json: &str) -> jni::errors::Result<()> {
    let s = env.new_string(json)?;
    env.call_method(future, "complete", "(Ljava/lang/Object;)Z", &[(&s).into()])?;
    Ok(())
}

/// `future.completeExceptionally(new CogneeException(code, message))`.
fn complete_err(
    env: &mut JNIEnv,
    future: &JObject,
    code: &str,
    message: &str,
) -> jni::errors::Result<()> {
    let code_j = env.new_string(code)?;
    let msg_j = env.new_string(message)?;

    // Construct via the cached class OBJECT (no find_class on this worker thread).
    let class_global = EXCEPTION_CLASS
        .get()
        .expect("exception class cached on the initiating thread before any op ran");
    let class_local = env.new_local_ref(class_global)?;
    let class = JClass::from(class_local);
    let exc = env.new_object(
        &class,
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[(&code_j).into(), (&msg_j).into()],
    )?;

    env.call_method(
        future,
        "completeExceptionally",
        "(Ljava/lang/Throwable;)Z",
        &[(&exc).into()],
    )?;
    Ok(())
}
