# T01 — Rust shim crate + Maven project skeleton, `NativeLibLoader`, version handshake

## Objective

After this task the `java/` directory exists with a compilable standalone Rust
cdylib (`cognee-java-jni`, library name `cognee_java`) that exports a single
`Native.version()` JNI function, and a single-module Maven project
(`ai.cognee:cognee`) whose `ai.cognee.internal.{Native,NativeLibLoader}` classes
load the native library (dev-path via `COGNEE_JAVA_LIB_PATH`) and assert a
native↔jar **version handshake** at class-load time. A JUnit test proves the
library loads and `Native.version()` equals the Maven project version. No ops,
no handle, no async yet.

## Dependencies & preconditions

- None (first task). Verify the starting repo state:
  - `test ! -e java` (the `java/` directory does not yet exist). If it exists,
    STOP and record in the Deviations log.
  - `ls ts/cognee-ts-neon/Cargo.toml` succeeds (the neon crate is the blueprint).
  - `grep -m1 '^version' Cargo.toml` prints `version = "0.1.3"` (the workspace
    version the jar must track; if different, use that value everywhere below).

## Context for this task

**Standalone-crate rule (design §7, mirrors neon).** The shim crate is **not**
a workspace member. Its `Cargo.toml` carries an empty `[workspace]` table so it
resolves as its own workspace, exactly like `ts/cognee-ts-neon/Cargo.toml`
(which has `[workspace]` on its own line). Root `cargo check --all-targets` does
**not** cover it — `java/scripts/check.sh` (T02) runs `cargo` for it explicitly.

**Edition 2024 requires `#[unsafe(no_mangle)]`** (not bare `#[no_mangle]`) on
every exported symbol. The workspace and the neon crate are both edition 2024.

**JNI name mangling (decision #5).** Every native method declared in the Java
class `ai.cognee.internal.Native` is exported from Rust as
`Java_ai_cognee_internal_Native_<methodName>`. Because no native method name
contains an underscore (all camelCase) and none are overloaded, no `_1` escaping
or signature suffix is needed. `extern "system"` + `#[unsafe(no_mangle)]`.

**Panic safety (design §5/§10).** A Rust panic crossing the JNI boundary is UB.
Every exported function body runs inside `std::panic::catch_unwind`. This task
defines the three guard helpers (`guard_void`, `guard_jlong`, `guard_jstring`)
used by every later task; on panic they throw a `java.lang.RuntimeException`
(the superclass of the `CogneeException` added in T04) carrying a
`[cognee-java panic]` marker and return the type's null/zero sentinel.

**`jni` crate version = `0.21` (see README §2).** Do not use 0.22.x.

**Version handshake (design §7/§10).** The jar embeds its Maven version in a
filtered resource `ai/cognee/version.properties`; `Native.version()` returns the
Rust crate version (`env!("CARGO_PKG_VERSION")`). At class-load the two must be
equal or class init fails fast — this prevents the classic jar↔cdylib skew bug.

## Steps

### 1. Create `java/cognee-java-jni/Cargo.toml`

Mirror the neon crate's feature table and dependency wiring. Verbatim:

```toml
[package]
name = "cognee-java-jni"
version = "0.1.3"
edition = "2024"
rust-version = "1.91"
license = "MIT OR Apache-2.0"
description = "JNI (jni-rs) native binding for the cognee Rust SDK (Java)."
repository = "https://github.com/topoteretes/cognee-rs"
homepage = "https://www.cognee.ai"
publish = false

# Standalone crate — not part of the parent workspace (mirrors cognee-ts-neon).
[workspace]

[lib]
name = "cognee_java"
crate-type = ["cdylib"]

[features]
default = [
    "visualization",
    "ladybug",
    "onnx",
    "hf-tokenizer",
    "tiktoken",
    "sqlite",
    "testing",
    "html-loader",
    "telemetry",
]
telemetry     = ["cognee-lib/telemetry"]
visualization = ["cognee-lib/visualization", "cognee-bindings-common/visualization"]
ladybug       = ["cognee-lib/ladybug",       "cognee-bindings-common/ladybug"]
onnx          = ["cognee-lib/onnx",          "cognee-bindings-common/onnx"]
hf-tokenizer  = ["cognee-lib/hf-tokenizer",  "cognee-bindings-common/hf-tokenizer"]
tiktoken      = ["cognee-lib/tiktoken",      "cognee-bindings-common/tiktoken"]
sqlite        = ["cognee-lib/sqlite",        "cognee-bindings-common/sqlite"]
testing       = ["cognee-lib/testing",       "cognee-bindings-common/testing"]
html-loader   = ["cognee-lib/html-loader"]

[dependencies]
cognee-bindings-common = { path = "../../crates/bindings-common", default-features = false }
cognee-lib = { path = "../../crates/lib", default-features = false }
cognee-logging = { path = "../../crates/logging" }
cognee-observability = { path = "../../crates/observability", features = ["telemetry"] }
cognee-telemetry     = { path = "../../crates/telemetry",     features = ["telemetry"] }
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

jni = "0.21"
async-trait = "0.1.89"
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time"] }
uuid = { version = "1.21", features = ["v4", "v5", "serde"] }
thiserror = "2.0"
serde_json = "1"
base64 = "0.22"
```

> Note: some deps (`cognee-observability`, `cognee-telemetry`, `cognee-logging`)
> are consumed only from T11's `logging.rs`/`telemetry.rs`. Declaring them now
> keeps `Cargo.toml` stable across tasks; an unused path-dep is not a compile
> error. If `cargo build` fails for a missing/renamed engine feature, open
> `ts/cognee-ts-neon/Cargo.toml` and reconcile the feature name — the neon table
> is the ground truth.

### 2. Create `java/cognee-java-jni/src/runtime.rs`

Port neon's `runtime.rs` — a process-wide multi-thread tokio runtime, lazily
built, race-safe via `OnceLock`. No neon types.

```rust
//! Process-wide tokio runtime for the Java binding (mirrors cognee-ts-neon).

use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Return the global runtime, building it on first use. Race-safe: a lost
/// `set` race drops the loser and returns the winner.
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    if let Some(rt) = RUNTIME.get() {
        return rt;
    }
    let candidate = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("cognee-java: failed to build the tokio runtime");
    let _ = RUNTIME.set(candidate);
    RUNTIME
        .get()
        .expect("runtime is set: either by this call or a concurrent initializer")
}
```

### 3. Create `java/cognee-java-jni/src/lib.rs`

Declare the module tree (most modules are added in later tasks — declare only
what exists now), cache the `JavaVM` in `JNI_OnLoad`, define the three panic
guards, and export `Native.version()`.

```rust
//! JNI (jni-rs) bindings for the cognee Rust SDK.
//!
//! Layer L1 of the Java binding: one exported `Java_ai_cognee_internal_Native_*`
//! function per `cognee-bindings-common` op. Structured data crosses the
//! boundary as JSON strings; the idiomatic Java layer (L3) owns all typing.

// This glue crate is wired up incrementally (one op group per task) and many
// helpers are reachable only through JNI-exported `extern` functions, which the
// dead-code lint cannot see as callers. Allow dead code crate-wide so each task
// stays green under `clippy -D warnings` while the surface is still growing.
#![allow(dead_code)]

mod runtime;
// Added in later tasks: mod errors; mod handle; mod config; mod sdk_ops; ...

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
    let _ = JAVA_VM.set(vm);
    JNI_VERSION_1_8
}

> **SAFETY (later tasks add non-trivial code here — keep it guarded):** T11 wires
> `install_default_subscriber()` + `arm_analytics()` into `JNI_OnLoad`. That body
> runs during `System.load`, where a panic unwinding into the JVM is UB, so the
> non-trivial calls **must** be wrapped in `std::panic::catch_unwind` and the
> function must still return `JNI_VERSION_1_8` regardless. The `JAVA_VM.set` above
> is infallible and needs no guard.

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
```

> If jni 0.21 rejects `JavaVM` as the `JNI_OnLoad` parameter type on this
> toolchain, use `vm: *mut jni::sys::JavaVM` and
> `JavaVM::from_raw(vm).expect("valid JavaVM from JNI_OnLoad")` inside an
> `unsafe` block; record the change in the Deviations log. The by-value
> `JavaVM` form is correct for jni 0.21.x.

### 4. Create `java/pom.xml`

Single-module Maven project. Java 17 release (the public API uses `record`
types, which require Java 16+), Jackson runtime dep, JUnit 5 test dep, resource
filtering for `version.properties`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 http://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>

  <groupId>ai.cognee</groupId>
  <artifactId>cognee</artifactId>
  <version>0.1.3</version>
  <packaging>jar</packaging>

  <name>cognee</name>
  <description>Java SDK for cognee — an AI memory pipeline over a Rust core (JNI).</description>
  <url>https://www.cognee.ai</url>

  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
    <jackson.version>2.17.2</jackson.version>
    <junit.version>5.10.2</junit.version>
  </properties>

  <dependencies>
    <dependency>
      <groupId>com.fasterxml.jackson.core</groupId>
      <artifactId>jackson-databind</artifactId>
      <version>${jackson.version}</version>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>${junit.version}</version>
      <scope>test</scope>
    </dependency>
  </dependencies>

  <build>
    <resources>
      <resource>
        <directory>src/main/resources</directory>
        <filtering>true</filtering>
      </resource>
    </resources>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
        <!-- -Xcheck:jni is added in T05 to audit global-ref hygiene. -->
      </plugin>
    </plugins>
  </build>
</project>
```

> Pin the exact Jackson/JUnit/surefire versions above only if they resolve
> offline in CI. If the CI Maven cache lacks them, the executor may bump to the
> nearest available patch and record it in the Deviations log.

### 5. Create `java/src/main/resources/ai/cognee/version.properties`

```
version=${project.version}
```

### 6. Create `java/src/main/java/ai/cognee/internal/NativeLibLoader.java`

Resolve platform → classifier resource → extract to a temp file → `System.load`,
with a `COGNEE_JAVA_LIB_PATH` dev override, plus `jarVersion()` reading the
filtered resource. Load exactly once (synchronized static holder).

```java
package ai.cognee.internal;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;
import java.util.Properties;

/** Loads the {@code cognee_java} native library and exposes the jar version. */
final class NativeLibLoader {
    private static volatile boolean loaded = false;

    private NativeLibLoader() {}

    /** Load the native library exactly once. */
    static synchronized void load() {
        if (loaded) {
            return;
        }
        String override = System.getenv("COGNEE_JAVA_LIB_PATH");
        if (override != null && !override.isEmpty()) {
            System.load(override);
        } else {
            extractAndLoadFromJar();
        }
        loaded = true;
    }

    /** The jar's Maven version, read from the filtered classpath resource. */
    static String jarVersion() {
        try (InputStream in =
                NativeLibLoader.class.getResourceAsStream("/ai/cognee/version.properties")) {
            if (in == null) {
                throw new IllegalStateException("version.properties missing from jar");
            }
            Properties p = new Properties();
            p.load(in);
            String v = p.getProperty("version");
            if (v == null || v.isEmpty() || v.contains("${")) {
                throw new IllegalStateException("version.properties not filtered: " + v);
            }
            return v;
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    private static void extractAndLoadFromJar() {
        String classifier = platformClassifier();
        String libFile = libFileName();
        String resource = "/native/" + classifier + "/" + libFile;
        try (InputStream in = NativeLibLoader.class.getResourceAsStream(resource)) {
            if (in == null) {
                throw new UnsatisfiedLinkError(
                        "no bundled native library for platform '" + classifier
                                + "' (resource " + resource + "). Set COGNEE_JAVA_LIB_PATH"
                                + " to a locally built cdylib for development.");
            }
            Path tmp = Files.createTempFile("cognee_java", suffix());
            tmp.toFile().deleteOnExit();
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            System.load(tmp.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    /** OpenDAL/RocksDB-style classifier: {os}-{arch}. */
    private static String platformClassifier() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        String arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        boolean aarch64 = arch.contains("aarch64") || arch.contains("arm64");
        if (os.contains("linux")) {
            return aarch64 ? "linux-aarch_64" : "linux-x86_64";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return "osx-aarch_64";
        }
        if (os.contains("win")) {
            return "windows-x86_64";
        }
        throw new UnsatisfiedLinkError("unsupported platform: os=" + os + " arch=" + arch);
    }

    private static String libFileName() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (os.contains("win")) {
            return "cognee_java.dll";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return "libcognee_java.dylib";
        }
        return "libcognee_java.so";
    }

    private static String suffix() {
        String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        if (os.contains("win")) {
            return ".dll";
        }
        if (os.contains("mac") || os.contains("darwin")) {
            return ".dylib";
        }
        return ".so";
    }
}
```

### 7. Create `java/src/main/java/ai/cognee/internal/Native.java`

The 1:1 mirror of the L1 exports. In this task it holds only `version()`. Its
static initializer loads the library and performs the version handshake.

```java
package ai.cognee.internal;

/**
 * Package-private 1:1 mirror of the Rust {@code Java_ai_cognee_internal_Native_*}
 * exports. Not part of the public API; excluded from published Javadoc.
 */
public final class Native {
    static {
        NativeLibLoader.load();
        String jar = NativeLibLoader.jarVersion();
        String nat = version();
        if (!jar.equals(nat)) {
            throw new IllegalStateException(
                    "cognee native/jar version skew: jar=" + jar + " native=" + nat
                            + " — the bundled native library does not match this jar.");
        }
    }

    private Native() {}

    /** The Rust crate version (from {@code CARGO_PKG_VERSION}). */
    static native String version();
}
```

### 8. Create `java/src/test/java/ai/cognee/internal/NativeLoadTest.java`

```java
package ai.cognee.internal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class NativeLoadTest {
    @Test
    void libraryLoadsAndVersionsMatch() {
        // Class-load of Native runs the handshake; reaching version() means it passed.
        String v = Native.version();
        assertNotNull(v);
        assertEquals(NativeLibLoader.jarVersion(), v);
    }
}
```

`Native.version()` is package-private, so the test lives in the same package
(`ai.cognee.internal`).

## Verification

Run from the repo root:

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml`
   → produces `java/cognee-java-jni/target/debug/libcognee_java.so`.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `cargo fmt --manifest-path java/cognee-java-jni/Cargo.toml -- --check` → clean.
4. Build + test the Java side against the just-built cdylib:
   ```bash
   COGNEE_JAVA_LIB_PATH="$PWD/java/cognee-java-jni/target/debug/libcognee_java.so" \
     mvn -q -f java/pom.xml test
   ```
   → `NativeLoadTest.libraryLoadsAndVersionsMatch` passes.
5. `scripts/check_all.sh` still passes (it does not yet run the Java stage — that
   is wired in T02).

## Out of scope

- `java/scripts/check.sh` and `check_all.sh`/CI wiring → **T02**.
- Handle new/destroy, the `Cognee` class, `close()` → **T03**.
- Any config, error, or op native methods → **T04+**.
- The async up-call machinery and the `errors`/`handle`/`config`/`sdk_*` Rust
  modules → **T04/T05+** (do not create empty module files now; declare them in
  `lib.rs` only when the task that fills them runs).
- `module-info.java` (JPMS): **not in v1** — the binding targets the classpath.
  If a consumer later needs JPMS, add it post-v1.
- The jar-extraction native path is implemented but not exercised until the
  classifier jars exist (**T13**); tests use `COGNEE_JAVA_LIB_PATH`.
