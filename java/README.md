# cognee (Java)

Java/JVM bindings for the [cognee](https://github.com/topoteretes/cognee-rs)
AI-memory SDK. A thin JNI layer over the shared Rust
[`cognee-bindings-common`](../crates/bindings-common/) facade — the same op
bodies and stable error codes that back the Python, C, and JavaScript SDKs, so
the surfaces line up 1:1.

Cognee transforms raw text, files, and URLs into a persistent, queryable
knowledge graph. The high-level flow is **remember** (ingest + extract in one
call) → **recall** (source-aware retrieval); these wrap the lower-level
**add** → **cognify** → **search** stages, which remain available for finer
control.

## Requirements

- JDK 17+ (the artifact is compiled for release 17).
- Maven or Gradle.
- No Rust toolchain needed to *use* the library — the native code ships prebuilt
  in the per-platform classifier jars. A Rust toolchain is only needed to build
  from source (see "Development builds").

## Install

[![Maven Central](https://img.shields.io/maven-central/v/io.github.topoteretes/cognee?label=Maven%20Central)](https://central.sonatype.com/artifact/io.github.topoteretes/cognee)

Available on [Maven Central](https://central.sonatype.com/artifact/io.github.topoteretes/cognee).
Just add the dependency — the matching per-platform native library is pulled in
automatically and loaded at runtime, so there is nothing else to build or
configure. Use the **latest version shown on the badge above**.

**Maven:**

```xml
<dependency>
  <groupId>io.github.topoteretes</groupId>
  <artifactId>cognee</artifactId>
  <version>LATEST</version> <!-- replace with the version shown on the badge above -->
</dependency>
```

**Gradle:**

```kotlin
implementation("io.github.topoteretes:cognee:LATEST") // use the version from the badge
```

At runtime, `NativeLibLoader` extracts and loads the classifier native library
matching your OS/architecture — one of `linux-x86_64`, `linux-aarch_64`,
`osx-aarch_64`, `windows-x86_64` — so no `COGNEE_JAVA_LIB_PATH` is needed.

### Development builds (locally built native library)

For iterating on the bindings without repackaging the classifier jar, build the
native cdylib and point the loader at it with `COGNEE_JAVA_LIB_PATH`:

```bash
cargo build --manifest-path java/cognee-java-jni/Cargo.toml
export COGNEE_JAVA_LIB_PATH="$(pwd)/java/cognee-java-jni/target/debug/libcognee_java.so"   # .dylib on macOS, .dll on Windows
```

When `COGNEE_JAVA_LIB_PATH` is set, `NativeLibLoader` loads that file directly
instead of extracting the bundled per-platform library from the jar. This is the
same path `java/scripts/check.sh` uses.

## Quick start

```java
import ai.cognee.*;
import java.util.List;
import java.util.Map;

try (Cognee cognee = new Cognee(Map.of("data_root_directory", "./data"))) {
    cognee.config().setLlmConfig(Map.of("llm_provider", "openai", "llm_api_key", System.getenv("OPENAI_TOKEN"), "llm_endpoint", System.getenv("OPENAI_URL")));
    cognee.warm().join();
    cognee.add(List.of(DataInput.text("Ada Lovelace wrote the first algorithm.")), "history").join();
    cognee.cognify("history").join();
    SearchResponse hits = cognee.search("Who wrote the first algorithm?",
            new SearchOptions().searchType(SearchType.GRAPH_COMPLETION)).join();
    System.out.println(hits.raw());
}
```

`Cognee` is `AutoCloseable` — use try-with-resources so the native handle is
released promptly (a `Cleaner` is a leak backstop, but `close()` is the primary
path). Settings keys are the canonical snake_case `Settings` field names; absent
keys fall back to environment variables and then compiled-in defaults.

## Operation surface

Every async op returns a `CompletableFuture<T>`. The generated
[Javadoc](#javadoc) (`mvn -f java/pom.xml javadoc:javadoc`, output under
`java/target/reports/apidocs/`) is the canonical per-method reference. In brief:

| Area | Methods |
|---|---|
| Lifecycle | `warm()`, `ownerId()`, `close()` |
| Ingest / extract | `add(...)`, `cognify(...)`, `addAndCognify(...)` |
| Retrieval | `search(query[, SearchOptions])`, `recall(query[, RecallOptions])` |
| Memory | `remember(...)`, `rememberEntry(...)`, `memify([MemifyOptions])`, `improve(ImproveOptions)` |
| Data lifecycle | `forget(ForgetTarget[, tenant])`, `update(...)`, `pruneData()`, `pruneSystem([PruneSystemOptions])` |
| Visualization | `visualize([VisualizeOptions])`, `visualizeToFile(VisualizeOptions)` |
| Sub-surfaces | `datasets()`, `sessions()`, `users()`, `notebooks()`, `config()` |
| Module statics | `Cognee.setupLogging()`, `Cognee.initOtlp()`, `Cognee.initTelemetry()`, `Cognee.version()` |

Inputs are built with the `DataInput` factories: `DataInput.text(String)`,
`DataInput.file(String path)`, `DataInput.url(String)`, and
`DataInput.binary(byte[] bytes, String name)`. Per-op options use fluent
builders (`SearchOptions`, `CognifyOptions`, `RecallOptions`, …); pass `null`
(or use the no-option overloads) to accept defaults. All 15 `SearchType` values
are supported (SCREAMING_SNAKE_CASE, e.g. `GRAPH_COMPLETION`, `SUMMARIES`,
`CHUNKS`, `RAG_COMPLETION`, `TRIPLET_COMPLETION`, `CYPHER`, …).

Structured results whose payloads are open-ended (`SearchResponse`,
`RecallResult`, `ForgetResult`, …) expose the parsed Jackson tree via `raw()`
plus typed accessors for the stable top-level fields.

## Error model

Every failure surfaces as an unchecked `CogneeException` carrying a stable,
machine-readable `code()` string shared with the other bindings (JS `e.code`,
C `CgErrorCode`). **Branch on the code, not the message.** Because ops return
`CompletableFuture`, a `CogneeException` arrives wrapped in a
`CompletionException` from `.join()`/`.get()`:

```java
try {
    cognee.cognify("history").join();
} catch (java.util.concurrent.CompletionException e) {
    if (e.getCause() instanceof CogneeException ce) {
        System.err.println("cognify failed: " + ce.code() + " — " + ce.getMessage());
    }
}
```

Codes: op failures use the `SdkError` set — `COMPONENT_ERROR`,
`SERVICE_BUILD_ERROR`, `USER_BOOTSTRAP_ERROR`, `RUNTIME_ERROR`,
`VALIDATION_ERROR`, `UNSUPPORTED`, `FEATURE_NOT_BUILT`; synchronous config
setters use the `ConfigError` set — `UNKNOWN_CONFIG_KEY`,
`CONFIG_TYPE_MISMATCH` — and throw `CogneeException` directly (not wrapped).

## Configuration

`cognee.config()` is the synchronous configuration surface (design decision
A3.1): the generic `set(key, value)` / `setStr(key, value)`, the four bulk
setters, and `get()`. Keys are the canonical snake_case `Settings` field names
(same as Python/C). See [docs/configuration.md](../docs/configuration.md) for
the full key reference.

```java
CogneeConfig cfg = cognee.config();

// Generic key-value setter (reaches every key):
cfg.set("llm_model", "gpt-4o-mini");
cfg.setStr("data_root_directory", "./data");

// Bulk setters — one per subsystem (throw on unknown key or type mismatch):
cfg.setLlmConfig(Map.of("llm_provider", "openai", "llm_model", "gpt-4o-mini",
        "llm_api_key", System.getenv("OPENAI_TOKEN"), "llm_endpoint", System.getenv("OPENAI_URL")));
cfg.setEmbeddingConfig(Map.of("embedding_provider", "openai", "embedding_model", "text-embedding-3-small"));
cfg.setVectorDbConfig(Map.of("vector_db_provider", "brute-force"));
cfg.setGraphDbConfig(Map.of("graph_db_provider", "kuzu"));

// Read back the current config (secret fields blanked):
Map<String, Object> current = cfg.get();
```

## Telemetry opt-out

Product analytics follow the shared opt-out policy and are **off unless armed**
by calling `Cognee.initTelemetry()` (which returns whether analytics are
effective for this process). Even when armed, emission is suppressed when:

- `COGNEE_HOST_SDK` is set — signals the host is an embedding SDK, so the
  binding must not emit its own analytics; or
- `TELEMETRY_DISABLED` (or `ENV`) requests the standard opt-out.

`Cognee.setupLogging()` and `Cognee.initOtlp()` (OTLP trace export) are likewise
opt-in and read their configuration from environment variables. See
[docs/observability/send_telemetry.md](../docs/observability/send_telemetry.md).

## Architecture

The binding is three layers with hard boundaries (full design:
[docs/design/java-bindings.md](../docs/design/java-bindings.md)): **L3** the
public, idiomatic Java SDK (`ai.cognee.*` — `CompletableFuture` async,
`AutoCloseable`, builders, enums, Javadoc); **L2** a package-private JNI shim
(`ai.cognee.internal.*` — a 1:1 mirror of the native exports plus the library
loader, never part of the public API); and **L1** the Rust cdylib
(`java/cognee-java-jni/`, jni-rs glue over `cognee-bindings-common`, a shared
tokio runtime that completes the passed-in `CompletableFuture` via JNI upcalls).
Structured data crosses the boundary as JSON strings in both directions — L3
deserializes into typed Java objects with Jackson.

## Examples

A runnable, credential-gated example lives under
[`examples/`](examples/):

| Example | What it covers |
|---|---|
| [`Quickstart.java`](examples/Quickstart.java) | Full add → cognify → search pipeline |

It is **not** part of the default `mvn verify` test run (examples are opt-in).
Compile and run it against the built classes and native library — it prints a
`SKIP` message and exits 0 when `OPENAI_URL`/`OPENAI_TOKEN` are absent:

```bash
# Build the jar + native library first (see "Install / build" above), then:
export COGNEE_JAVA_LIB_PATH="$(pwd)/java/cognee-java-jni/target/debug/libcognee_java.so"
CP="java/target/classes:$(mvn -q -f java/pom.xml dependency:build-classpath -Dmdep.outputFile=/dev/stdout 2>/dev/null | tail -1)"
javac -cp "$CP" -d "$TMPDIR/cognee-examples" java/examples/Quickstart.java
java  -cp "$CP:$TMPDIR/cognee-examples" Quickstart
```

## Javadoc

```bash
mvn -q -f java/pom.xml javadoc:javadoc
```

The public API (`ai.cognee.*`) carries class/method Javadoc; the internal shim
(`ai.cognee.internal.*`) is excluded from the generated docs. Output lands under
`java/target/reports/apidocs/`.

## Environment variables

| Variable | Purpose |
|---|---|
| `OPENAI_URL` | LLM API base URL (OpenAI-compatible endpoint). |
| `OPENAI_TOKEN` | LLM API key. |
| `OPENAI_MODEL` | LLM model name (default: `gpt-4o-mini`). |
| `COGNEE_JAVA_LIB_PATH` | Load this cdylib directly instead of the bundled per-platform library (dev). |
| `COGNEE_HOST_SDK` | Suppress binding-armed analytics when the host is an embedding SDK. |
| `TELEMETRY_DISABLED`, `ENV` | Standard analytics opt-outs for `initTelemetry()`. |
| `RUST_LOG`, `LOG_LEVEL` | `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `setupLogging()`. |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_*` | Consumed by `initOtlp()`. |

## References

- Binding overview: [docs/tools/bindings.md](../docs/tools/bindings.md)
- Design: [docs/design/java-bindings.md](../docs/design/java-bindings.md)
- Operations: [docs/operations.md](../docs/operations.md)
- Configuration: [docs/configuration.md](../docs/configuration.md)
- Python bindings: [python/README.md](../python/README.md)
- C API bindings: [capi/README.md](../capi/README.md)
- JavaScript/TS bindings: [ts/README.md](../ts/README.md)
