//! JNI (jni-rs) bindings for the cognee Rust SDK.
//!
//! Layer L1 of the Java binding: one exported `Java_ai_cognee_internal_Native_*`
//! function per `cognee-bindings-common` op. Structured data crosses the
//! boundary as JSON strings; the idiomatic Java layer (L3) owns all typing.

mod args;
mod config;
mod errors;
mod future;
mod handle;
mod runtime;
mod sdk_admin;
mod sdk_data;
mod sdk_datasets;
mod sdk_lifecycle;
mod sdk_memory;
mod sdk_ops;
mod sdk_retrieval;
mod sdk_sessions;
mod sdk_static;
mod sdk_visualization;

use std::ffi::c_void;
use std::sync::OnceLock;

use jni::objects::JClass;
use jni::sys::{JNI_VERSION_1_8, jint, jstring};
use jni::{JNIEnv, JavaVM};

/// The process JavaVM, cached at load so tokio worker threads can attach.
static JAVA_VM: OnceLock<JavaVM> = OnceLock::new();

/// Cached `JavaVM`. Panics only if called before `JNI_OnLoad` ran, which the
/// JVM guarantees happens before any native method of this library is invoked.
pub(crate) fn java_vm() -> &'static JavaVM {
    JAVA_VM
        .get()
        .expect("JNI_OnLoad ran before any native method could be called")
}

/// Called by the JVM when `System.load`/`System.loadLibrary` maps this library.
/// Caches the `JavaVM` and declares the supported JNI version.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
    let _ = JAVA_VM.set(vm); // infallible; needs no panic guard
    // Parity with neon's `#[neon::main]`: install the default stderr subscriber
    // before any native method runs (honours `COGNEE_BINDING_SUPPRESS_LOGS`),
    // and arm product analytics so the `COGNEE_HOST_SDK` opt-out is authoritative
    // for any binding-hosted `send_telemetry` call.
    //
    // This runs during `System.load`; a panic unwinding into the JVM here is UB,
    // so guard the non-trivial body and always return a supported JNI version.
    let _ = std::panic::catch_unwind(|| {
        sdk_static::install_default_subscriber();
        let _ = sdk_static::arm_analytics();
    });
    JNI_VERSION_1_8
}

// ---------------------------------------------------------------------------
// Panic guards — every exported fn body runs inside one of these.
// A panic is converted to a thrown java.lang.RuntimeException (superclass of
// CogneeException) and the type's null/zero sentinel is returned, so no panic
// crosses the FFI boundary (UB).
// ---------------------------------------------------------------------------

const PANIC_MSG: &str = "[cognee-java panic] a native panic was caught at the JNI boundary";

pub(crate) fn guard_void<'l>(env: &mut JNIEnv<'l>, f: impl FnOnce(&mut JNIEnv<'l>)) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(env))).is_err() {
        let _ = env.throw_new("java/lang/RuntimeException", PANIC_MSG);
    }
}

pub(crate) fn guard_jlong<'l>(
    env: &mut JNIEnv<'l>,
    f: impl FnOnce(&mut JNIEnv<'l>) -> jni::sys::jlong,
) -> jni::sys::jlong {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(env))) {
        Ok(v) => v,
        Err(_) => {
            let _ = env.throw_new("java/lang/RuntimeException", PANIC_MSG);
            0
        }
    }
}

pub(crate) fn guard_jstring<'l>(
    env: &mut JNIEnv<'l>,
    f: impl FnOnce(&mut JNIEnv<'l>) -> jstring,
) -> jstring {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(env))) {
        Ok(v) => v,
        Err(_) => {
            let _ = env.throw_new("java/lang/RuntimeException", PANIC_MSG);
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Native.version() -> String
// ---------------------------------------------------------------------------

/// `ai.cognee.internal.Native.version()` — the Rust crate version string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_version<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    guard_jstring(&mut env, |env| {
        match env.new_string(env!("CARGO_PKG_VERSION")) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    })
}
