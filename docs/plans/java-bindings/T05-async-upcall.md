# T05 — Async up-call machinery + `warm()` + `ownerId()`

## Objective

After this task the binding can run an async op on the shared tokio runtime and
settle a Java `CompletableFuture` from a tokio worker thread — the one piece of
the design without an in-repo precedent. It ships with `warm()` and `ownerId()`
as the first two async ops. The machinery guarantees the global ref to the
future is dropped on **every** path (success, op error, panic), constructs the
exceptional-completion `CogneeException` in a way that is safe on ART/daemon
worker threads (cached class global ref, never `find_class` on the worker
thread), and is audited under `-Xcheck:jni`.

## Dependencies & preconditions

- **T04 done.** Verify `bash java/scripts/check.sh` passes and
  `ai.cognee.CogneeException(String,String)` + `crate::errors` +
  `crate::handle::handle_ref` + `crate::runtime::runtime` + `crate::java_vm()`
  all exist.
- Confirm `crate::java_vm()` returns `&'static jni::JavaVM` (added in T01).
- Confirm `HandleState::services()` / `owner_id()` signatures in
  `crates/bindings-common/src/handle.rs` (both `async`, return
  `Result<_, SdkError>`).

## Context for this task — the full threading model (design §6)

1. `Native.<op>(handle, …args…, CompletableFuture<String> future)` is invoked on
   a **Java thread**. The native method validates args, `Arc::clone`s the
   `HandleState`, creates a **JNI global ref** to the future, and
   `runtime().spawn`s the op. It returns immediately (`void`).
2. The op body runs to completion on the tokio runtime.
3. On completion, a tokio worker thread attaches to the JVM as a **daemon**
   (`attach_current_thread_as_daemon` — never blocks JVM shutdown) and calls
   `future.complete(jsonString)` or
   `future.completeExceptionally(new CogneeException(code, msg))` through the
   global ref, then **drops the global ref**.
4. **Class-loader safety (§6.4):** class *name* lookup (`find_class`) on a daemon
   worker thread resolves against the system class loader, which cannot see
   `ai.cognee.CogneeException`. Therefore the exception class is looked up
   **once, on the initiating Java thread** (which has the app class loader) and
   held as a **global ref**; the worker thread constructs the exception from
   that cached `JClass` object (never by name). `complete`/`completeExceptionally`
   are called via `env.call_method` on the future *instance*, which resolves the
   method from the object's own class (`GetObjectClass`) — this is **not** subject
   to the class-loader pitfall, so no cached method IDs are required. (Caching
   `jmethodID`s is a valid post-v1 micro-optimization; v1 uses safe calls for
   executor correctness.)
5. **Panic isolation:** the op future is run under an **inner** `runtime().spawn`,
   whose `JoinHandle` converts any panic into a `JoinError` on `.await`. The outer
   task never panics; it always settles the future and drops the global ref. A
   panicked op completes the future exceptionally with code `RUNTIME_ERROR`.
6. **Uniform result contract:** every async native op completes the
   `CompletableFuture<String>` with the **JSON string** of the op's
   `serde_json::Value` (`value.to_string()`). Scalars are JSON-encoded too
   (`null` → `"null"`, a UUID → `"\"<uuid>\""`). L3 `thenApply`-deserializes with
   Jackson. This keeps L1 dumb and all typing in L3.
7. **Cancellation:** none in v1 (`CompletableFuture.cancel` only abandons the
   Java-side future; the native task runs to completion). Do not wire it.

> **SAFETY (do not re-use this boilerplate unguarded):** the settle block **must**
> run inside `env.with_local_frame(16, |env| { … })`. Daemon worker threads never
> detach, so the JNI locals created per completion (`new_string`, the class
> local-ref, the exception object) would otherwise accumulate unbounded — fatal
> on Android/ART. Additionally, if the `complete`/`completeExceptionally` up-call
> itself fails the future is never settled and Java `.join()` hangs forever, so
> add a best-effort fallback: clear any pending exception, retry
> `completeExceptionally` with a plain message, and log (never panic) if that also
> fails. (Implemented in `future.rs`; the snippet below predates both fixes.)

## Steps

### 1. Create `java/cognee-java-jni/src/future.rs`

```rust
//! Async up-call machinery: run an op on the tokio runtime and settle a Java
//! `CompletableFuture` from a worker thread. See design §6.

use std::future::Future;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject};
use jni::{JNIEnv, JavaVM};

use cognee_bindings_common::SdkError;

use crate::errors::throw_cognee_exception;
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
            throw_cognee_exception(env, "RUNTIME_ERROR", "could not create a global ref for the future");
            return;
        }
    };
    let vm: &'static JavaVM = java_vm();

    runtime().spawn(async move {
        // Inner spawn isolates op-body panics into a JoinError.
        let outcome = runtime().spawn(fut).await;

        // Daemon attach: pooled worker threads stay attached and never block
        // JVM shutdown. Re-attaching an already-attached thread is a no-op.
        let mut env = match vm.attach_current_thread_as_daemon() {
            Ok(e) => e,
            Err(_) => {
                drop(global); // still release the ref
                return;
            }
        };

        let settled = match outcome {
            Ok(Ok(value)) => complete_ok(&mut env, global.as_obj(), &value.to_string()),
            Ok(Err(sdk)) => complete_err(&mut env, global.as_obj(), sdk.code(), &sdk.to_string()),
            Err(join_err) => {
                let msg = if join_err.is_panic() {
                    "native task panicked"
                } else {
                    "native task was cancelled"
                };
                complete_err(&mut env, global.as_obj(), "RUNTIME_ERROR", msg)
            }
        };

        // Never leave a pending exception on a pooled worker thread.
        if settled.is_err() && env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }

        drop(global); // release the global ref on EVERY path
    });
}

/// `future.complete(jsonString)`.
fn complete_ok(env: &mut JNIEnv, future: &JObject, json: &str) -> jni::errors::Result<()> {
    let s = env.new_string(json)?;
    env.call_method(
        future,
        "complete",
        "(Ljava/lang/Object;)Z",
        &[(&s).into()],
    )?;
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
```

> jni 0.21 API notes: `JavaVM::attach_current_thread_as_daemon(&self) ->
> Result<JNIEnv<'_>>` returns a permanently-attached `JNIEnv` (no guard — daemon
> threads auto-detach at process exit). `JClass::from(JObject)` exists. `(&s).into()`
> / `(&exc).into()` build `JValue`s from `&JString` / `&JObject`. If any conversion
> is rejected, use `JValue::Object(&s)` (add `use jni::objects::JValue;`) and record
> it in the Deviations log.

### 2. Create `java/cognee-java-jni/src/sdk_lifecycle.rs`

The two first async ops. (Later op groups add their own `sdk_*.rs` files; these
two lifecycle ops live here.)

```rust
//! Async lifecycle ops: `warm`, `ownerId`.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject};
use jni::sys::jlong;

use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `warm(handle, future)` — force `services()` to build (async), surfacing
/// config/connection errors and resolving `owner_id`. Completes with `null`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_warm<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        // SAFETY: live handle (Java closed-guard); clone before moving into spawn.
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        spawn_future(env, &future, async move {
            state.services().await.map(|_| serde_json::Value::Null)
        });
    })
}

/// `ownerId(handle, future)` — resolve the email-derived owner id (warms lazily).
/// Completes with the UUID string (JSON-encoded).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_ownerId<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        // SAFETY: live handle (Java closed-guard).
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        spawn_future(env, &future, async move {
            state
                .owner_id()
                .await
                .map(|id| serde_json::Value::String(id.to_string()))
        });
    })
}
```

### 3. Extend `java/cognee-java-jni/src/lib.rs`

Add to the module list: `mod future;` and `mod sdk_lifecycle;`.

### 4. Extend `java/src/main/java/ai/cognee/internal/Native.java`

Add (and add the import `import java.util.concurrent.CompletableFuture;` at the
top of the file):

```java
    public static native void warm(long handle, CompletableFuture<String> future);

    public static native void ownerId(long handle, CompletableFuture<String> future);
```

### 5. Add `warm()` / `ownerId()` to `Cognee.java`

Add the import `import java.util.concurrent.CompletableFuture;` and the methods:

```java
    /** Force engine construction now (surfaces config/connection errors early). */
    public CompletableFuture<Void> warm() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.warm(handle(), f);
        return f.thenApply(s -> null);
    }

    /** The email-derived owner id (warms lazily if needed). */
    public CompletableFuture<String> ownerId() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.ownerId(handle(), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }
```

### 6. Enable `-Xcheck:jni` in the Maven surefire plugin

In `java/pom.xml`, give the `maven-surefire-plugin` a configuration so tests run
under JNI checking (surfaces global-ref / local-ref / pending-exception misuse;
`-Xcheck:jni` makes serious violations fatal, so the test JVM aborts and the
build fails if the up-call machinery misuses JNI):

```xml
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
        <configuration>
          <argLine>-Xcheck:jni</argLine>
        </configuration>
      </plugin>
```

### 7. Create `java/src/test/java/ai/cognee/CogneeAsyncTest.java`

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeAsyncTest {
    private Cognee handle(Path dir) {
        return new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
    }

    @Test
    void warmAndOwnerIdComplete(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            assertDoesNotThrow(() -> cognee.warm().join());
            String owner = cognee.ownerId().join();
            assertNotNull(owner);
            assertDoesNotThrow(() -> UUID.fromString(owner)); // valid UUID
        }
    }

    @Test
    void repeatedWarmIsStableUnderXcheckJni(@TempDir Path dir) {
        // Runs many completions so a global-/local-ref leak would trip -Xcheck:jni.
        try (Cognee cognee = handle(dir)) {
            for (int i = 0; i < 50; i++) {
                cognee.warm().join();
            }
        }
    }

    @Test
    void exceptionalCompletionCarriesCogneeException(@TempDir Path dir) {
        // Point the LLM/embedding at nonsense so warm() fails, exercising the
        // exceptional-completion path (CogneeException via the cached class).
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString(),
                "vector_db_provider", "definitely-not-a-real-provider"))) {
            CompletionException ex =
                    assertThrows(CompletionException.class, () -> cognee.warm().join());
            org.junit.jupiter.api.Assertions.assertTrue(
                    ex.getCause() instanceof CogneeException,
                    "cause should be CogneeException, was: " + ex.getCause());
        }
    }
}
```

> The `exceptionalCompletionCarriesCogneeException` test assumes an invalid
> `vector_db_provider` makes `services()` fail during `warm()`. If that specific
> key does not error on this build, pick any setting that makes engine
> construction fail (e.g. an unresolvable `graph_database_provider`) and record
> the substitution in the Deviations log. The point is to exercise the
> `completeExceptionally` path — any guaranteed `SdkError` will do.

## Verification

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` → clean.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `bash java/scripts/check.sh` → `CogneeAsyncTest` passes **with no `-Xcheck:jni`
   FATAL/WARNING output** in the surefire logs (`grep -i 'xcheck\|jni warning\|FATAL'
   java/target/surefire-reports/*` finds nothing). If `-Xcheck:jni` aborts the
   JVM, the up-call misuses JNI — fix it before proceeding.
4. `scripts/check_all.sh` → green.

## Out of scope

- Any pipeline/retrieval/memory/data/dataset/session/admin ops → **T06–T10**
  (they reuse `spawn_future`; do not re-implement the machinery).
- Cancellation wiring → not in v1.
- Cached `jmethodID`s → post-v1 optimization; v1 uses safe `call_method`.
- Visualization's `String` result → **T11** wraps it as `Value::String` so it
  flows through the same uniform contract.
