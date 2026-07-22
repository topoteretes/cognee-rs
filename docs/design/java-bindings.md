# Design: Java bindings for cognee-rust (JNI / jni-rs)

> **Audience note:** this document is written as input for an LLM that will
> produce a detailed implementation plan. It is deliberately explicit about
> repo-specific facts, file paths, invariants, and decision rationale. When a
> statement below conflicts with the current state of the repository, the
> repository wins — re-verify every referenced path/symbol before planning
> against it.

## 1. Goal

Add an official **Java SDK** (`cognee-java`) as the fourth language binding of
cognee-rust, alongside the existing three:

| Binding | Location | Native tech | Async model |
|---|---|---|---|
| Python | `python/` | PyO3 (maturin wheel) | native `async` |
| JavaScript/TS | `ts/` | Neon (Node cdylib) | Promise |
| C | `capi/` | cbindgen-style FFI | callbacks + `CgSdkWaiter` sync bridge |
| **Java (new)** | `java/` | **JNI via the `jni` crate (jni-rs)** | **`CompletableFuture<T>`** |

The chosen approach is **hand-written JNI glue over `cognee-bindings-common`**
(the "OpenDAL blueprint"), *not* FFM/Panama/jextract and *not* UniFFI. Decision
rationale (recorded so the planner does not re-litigate it):

- JNI works on **every JVM ≥ 8 and on Android/ART**; FFM (`java.lang.foreign`)
  requires Java ≥ 22 and does not exist on Android. The project's edge/Android
  ambition and enterprise Java-17/21 reality make JNI the only option serving
  both audiences.
- UniFFI is used nowhere in this repo; adopting it for one language would add a
  parallel binding architecture. The Java UniFFI generator
  (IronCoreLabs/uniffi-bindgen-java) is 0.x/unstable and FFM-based anyway.
- Production precedent for this exact architecture: Apache OpenDAL's Java
  binding (jni-rs + tokio + `CompletableFuture` + Maven classifier jars).

## 2. Architecture (three layers)

```
┌──────────────────────────────────────────────────────────────┐
│ L3: Java SDK (public, idiomatic)     java/src/main/java/...  │
│     ai.cognee.Cognee, CogneeException, SearchType, ...       │
│     CompletableFuture async, AutoCloseable, Javadoc          │
├──────────────────────────────────────────────────────────────┤
│ L2: JNI shim (package-private)                               │
│     ai.cognee.internal.Native — private static native decls  │
│     ai.cognee.internal.NativeLibLoader — classifier-jar load │
├──────────────────────────────────────────────────────────────┤
│ L1: Rust cdylib                      java/cognee-java-jni/   │
│     jni-rs glue; depends on cognee-bindings-common;          │
│     shared tokio runtime; completes futures via JNI upcalls  │
└──────────────────────────────────────────────────────────────┘
```

### Layer responsibilities (hard boundaries)

- **L1 (Rust)** is *dumb and narrow*: one exported JNI function per
  `bindings-common` op. It parses JNI strings/handles, calls the shared op
  body, and completes the passed-in `CompletableFuture` object from the tokio
  task via a global ref + `JavaVM::attach_current_thread`. **No business
  logic, no result shaping beyond what `bindings-common` already provides.**
- **L2 (Java, internal)** is a 1:1 mirror of L1 exports plus the native
  library loader. Never exposed in Javadoc/public API (`internal` package,
  not exported from `module-info.java`).
- **L3 (Java, public)** owns all ergonomics: futures, exceptions, enums,
  builders, overloads, JSON deserialization of structured results. Pure Java,
  testable with JUnit without rebuilding Rust.

### Structured data crosses the boundary as JSON strings

`bindings-common` op bodies already return `serde_json::Value`
(see `crates/bindings-common/src/ops/*.rs` — e.g. `pipeline::add`,
`pipeline::cognify`, `retrieval::search`, `datasets::list_datasets` all
produce JSON) and `crates/bindings-common/src/wire.rs` has neon-free JSON
helpers (`cognify_result_json`, `marshal_inputs`, `marshal_one`,
`marshal_bytes`). **The JNI shim passes JSON strings in both directions**;
L3 deserializes into typed Java records. Do **not** construct Java objects
field-by-field through JNI — that is the failure mode that makes JNI bindings
leaky and unmaintainable. Options/opts arguments likewise travel as JSON
strings (mirroring how the C API and the neon `opts` objects are marshalled).

For the JSON layer in Java, prefer **zero heavyweight dependencies**: either
(a) a small vendored/shaded parser, or (b) a single well-known dependency
(Jackson-core or Gson) — this is an open decision (see §9), default to
Jackson if undecided.

## 3. Repo integration points (verified facts)

- **Shared facade:** `crates/bindings-common/` — modules `error` (`SdkError`
  with stable `code()` strings), `handle` (`HandleState`: `ConfigManager` +
  lazy engines + cached `CogneeServices`, version-invalidated on config
  change), `services`, `ops/{admin,data,datasets,memory,pipeline,retrieval,
  sessions,visualization}.rs`, `wire`, `redact` (`redact_config_json` — use it
  for any config echo/logging). The Java shim must consume these op bodies
  exactly like `ts/cognee-ts-neon/src/sdk_*.rs` and the capi do.
- **Blueprint crate:** `ts/cognee-ts-neon/` is the closest structural model —
  a **standalone crate, not a workspace member** (`[workspace]` empty table in
  its Cargo.toml), `crate-type = ["cdylib"]`, files split by domain
  (`sdk_ops.rs`, `sdk_datasets.rs`, `sdk_retrieval.rs`, `sdk_memory.rs`,
  `sdk_sessions.rs`, `sdk_admin.rs`, `sdk_visualization.rs`, `config.rs`,
  `runtime.rs`, `errors.rs`, `logging.rs`, `telemetry_otlp.rs`,
  `telemetry_analytics.rs`). Mirror this file decomposition in the Java shim
  crate. Note the python crate IS a workspace member with test harness
  disabled (see comment in `python/Cargo.toml`) — for Java, follow the
  ts/capi pattern (standalone) unless the planner finds a strong reason
  otherwise.
- **Feature set:** copy the default feature list from
  `ts/cognee-ts-neon/Cargo.toml` (visualization, ladybug, onnx, hf-tokenizer,
  tiktoken, sqlite, testing, ...). The capi's "slim embedded" variant
  (`--no-default-features --features sqlite,testing`) is *not* a v1 Java
  concern.
- **Runtime:** all bindings share the pattern of one process-wide tokio
  runtime (see `ts/cognee-ts-neon/src/runtime.rs` and `cg_init` /
  `cg_init_with_threads` in `capi/include/cognee.h`). The Java shim needs the
  same: lazily-initialized global runtime; ops are `runtime.spawn(...)`;
  completion hops back to Java via a cached `JavaVM` pointer.
- **Config:** all bindings delegate to the same `ConfigManager` via
  `HandleState`. Per design decision A3.1 (documented in
  `docs/tools/bindings.md`): Java v1 ships the **generic `set(key, value)` /
  `setStr` + the 4 bulk setters** (`setLlmConfig`, `setEmbeddingConfig`,
  `setVectorDbConfig`, `setGraphDbConfig`) + `get()`. The ~40 granular typed
  setters (JS-style sugar) are an optional post-v1 mechanical addition
  (~0.5 d, per the "Unification path" note in bindings.md). Key names are the
  canonical `Settings` field names (`crates/lib/src/config.rs`,
  `docs/configuration.md`), snake_case at the boundary; L3 may expose
  camelCase Java methods that translate.
- **Errors:** `SdkError::code()` provides stable machine-readable codes shared
  by JS (`e.code`) and C (`CgErrorCode`). The JNI shim must surface
  `(code, message)` pairs; L3 maps them onto a `CogneeException` hierarchy
  (see §5). `CONFIG_TYPE_MISMATCH` must surface at the config call site,
  matching the other bindings.
- **Module-level helpers:** each binding exposes logging/telemetry setup
  (`cognee_setup_logging`, `cognee_init_otlp`, `cognee_init_telemetry` in
  capi; `logging.rs`/`telemetry_*.rs` in neon). Java needs equivalents as
  static methods (e.g. `Cognee.setupLogging()`), idempotent, env-var driven.
  The product-analytics arming must follow the same per-binding policy
  (decision 11 / `COGNEE_HOST_SDK` sentinel — see capi header comments) with
  a Java-specific host-SDK value.
- **Checks:** `scripts/check_all.sh` runs, in order: fmt → check → clippy →
  feature-variant checks → wasm drift guards → `capi/scripts/check.sh` →
  `python/scripts/check.sh` → `ts/scripts/check.sh`. A new
  `java/scripts/check.sh` must be added and wired into `check_all.sh` and CI
  (`ci.yml` has per-binding jobs; add `java-check`).
- **Prebuild CI:** `.github/workflows/ts-prebuild.yml` builds Neon cdylibs for
  a 4-target matrix (linux x64/arm64 gnu, darwin arm64, win32 x64 msvc; see
  `ts/platform-packages/`). A `java-prebuild.yml` should reuse this matrix to
  produce per-platform jars.
- **Cross-SDK parity:** `e2e-cross-sdk/` verifies Python↔Rust CLI parity.
  Java parity testing is out of scope for v1 (see §8 non-goals), but the SDK
  surface must remain 1:1 with the op set in `bindings-common` so it stays
  *possible*.

## 4. Public Java API surface (v1)

Package: `ai.cognee` (group id `ai.cognee`, artifact `cognee`). Java 11 as the
source/target floor (open question §9; do not go below 11 — `CompletableFuture`
ergonomics and `Cleaner` require 9+; 8 only if a concrete consumer demands it).

The surface mirrors the TS `Cognee` class 1:1 at the *operation* level
(`ts/src/cognee.ts` is the reference for naming and option shapes; Python
`python/cognee_py/__init__.pyi` is the secondary reference):

```java
try (Cognee cognee = new Cognee(Map.of("data_root_directory", "..."))) {
    cognee.config().set("llm_model", "gpt-4o-mini");
    cognee.config().setLlmConfig(Map.of("provider", "openai", "api_key", "..."));

    cognee.warm().join();
    AddResult added   = cognee.add(List.of("some text"), "my_dataset").join();
    CognifyResult res = cognee.cognify().join();
    List<SearchResult> hits =
        cognee.search(SearchType.GRAPH_COMPLETION, "What is ...?").join();
}
```

Operation groups (each maps to one `bindings-common/src/ops/*.rs` module and
one `sdk_*.rs` shim file):

| Java accessor | Ops (from `bindings-common`) |
|---|---|
| top-level | `warm`, `add`, `cognify`, `addAndCognify`, `search`, `recall`, `memify`, `remember`, `improve`, `forget`, `update`, `prune*` |
| `cognee.datasets()` | `list`, `listData`, `has`, `status`, `empty`, `deleteData`, `deleteAll` |
| `cognee.sessions()` | `get`, `addFeedback`, `deleteFeedback`, `getGraphContext`, `setGraphContext` |
| `cognee.notebooks()` | `list`, `create`, `update`, `delete` |
| `cognee.users()` | `getOrCreateDefault` |
| `cognee.admin()` | `resetPipelineRunStatus`, `resetDatasetPipelineRunStatus` |
| `cognee.config()` | `set`, `setStr`, bulk setters ×4, `get` |
| static | `setupLogging`, `initOtlp`, `initTelemetry`, `version` |

Conventions:

- Every async op returns `CompletableFuture<T>`; no separate blocking API
  (callers use `.join()` — matches the TS Promise decision).
- `T` is a typed record/POJO deserialized from the op's JSON (e.g.
  `SearchResult`, `CogneeDataset`, `DatasetStatus`), not raw JSON strings —
  but keep a `raw()` escape hatch if a payload is open-ended (e.g. notebook
  cells).
- Optional parameters: Java overloads for the common cases + an `Options`
  builder object serialized to the same JSON `opts` shape the other bindings
  send. Do not invent new option keys; reuse the exact keys from
  `ts/src/cognee.ts` / the neon `read_opts` sites.
- `Cognee implements AutoCloseable`; `close()` destroys the native handle
  (idempotent, subsequent op calls fail with `IllegalStateException` or a
  dedicated `HANDLE_CLOSED` error). Additionally register a
  `java.lang.ref.Cleaner` as a leak backstop; `close()` is the primary path.
- `SearchType` is a Java enum whose wire values are the exact strings accepted
  by `ops/retrieval.rs::parse_search_type`.
- Visualization ops (`visualize`, `visualizeToFile`) included, feature-gated
  identically to the other bindings.

## 5. Error model

- Rust side: every op resolves to `Result<serde_json::Value, SdkError>`. On
  `Err`, the shim completes the future **exceptionally** with a
  `CogneeException` constructed via JNI (`completeExceptionally` upcall),
  carrying `code` (stable string from `SdkError::code()`) and `message`
  (Display). Synchronous failures (bad JSON, null handle) throw immediately
  from the native method.
- Java side: `CogneeException extends RuntimeException` with
  `String code()`; consider thin subclasses only for the codes callers branch
  on (`CONFIG_TYPE_MISMATCH` → `CogneeConfigException`; validation → 
  `CogneeValidationException`). Keep the hierarchy minimal; the code string is
  the contract.
- **Panics must not cross the JNI boundary** (UB). Every exported JNI function
  body wraps in `std::panic::catch_unwind` (or the jni-rs equivalent pattern)
  and converts panics to a `RUNTIME`-coded exception. The neon/capi crates
  have equivalent guards — replicate, don't innovate.
- `unwrap()` remains forbidden (repo convention); JNI env calls that "cannot
  fail" need `expect("why")` with a real invariant justification.

## 6. Async / threading model (the one genuinely novel piece)

This is the only part of the binding without a direct in-repo precedent, so it
must be specified precisely:

1. `Native.<op>(handle, argsJson, future)` is called on a Java thread. The
   native method: validates args, clones an `Arc<HandleState>`, creates a JNI
   **global ref** to the `CompletableFuture`, and `runtime.spawn`s the op.
   The native method returns immediately (void).
2. The spawned task runs the `bindings-common` op body to completion on the
   tokio runtime.
3. On completion, the task calls `vm.attach_current_thread()` (the `JavaVM`
   was cached at `JNI_OnLoad` / first init) and invokes
   `future.complete(resultJsonString)` or
   `future.completeExceptionally(new CogneeException(code, msg))` through the
   global ref, then drops the global ref. Use
   `attach_current_thread_as_daemon` for runtime worker threads so they never
   block JVM shutdown.
4. Cache `jclass`/`jmethodID` lookups (for `CompletableFuture#complete`,
   `#completeExceptionally`, `CogneeException#<init>`) once at load time in
   `OnceLock` statics — classes must be looked up from a thread with the
   correct class loader (do it in `JNI_OnLoad` or the first native call, and
   hold global refs to the classes; this is also the Android-proofing move,
   since worker threads on ART get the system class loader otherwise).
5. Cancellation: v1 exposes none (matches capi v1 posture); do not wire
   `CompletableFuture.cancel` to anything — document that it only abandons the
   Java-side future. (TS has a cancellation module; parity here is post-v1,
   see §9.)
6. Runtime lifecycle: initialize lazily on first op; no explicit shutdown API
   in v1 (JVM exit tears the process down; daemon-attached threads don't
   block it). If the planner adds `Cognee.shutdown()`, it must be idempotent
   and mirror `cg_shutdown` semantics.

## 7. Packaging, build, and repo layout

```
java/
├── cognee-java-jni/          # standalone Rust crate (NOT workspace member)
│   ├── Cargo.toml            # cdylib "cognee_java", deps: jni, cognee-bindings-common, serde_json, tokio
│   └── src/                  # lib.rs, runtime.rs, errors.rs, config.rs, sdk_*.rs (mirror neon split)
├── sdk/                      # Maven/Gradle project (choose one build tool — see §9; default Maven)
│   ├── pom.xml
│   └── src/main/java/ai/cognee/{...}
│   └── src/test/java/...     # JUnit 5
├── examples/
├── scripts/check.sh          # build Rust cdylib + mvn verify (compile, test, javadoc)
└── README.md
```

- **Artifacts (Maven Central, OpenDAL/RocksDB pattern):**
  - `ai.cognee:cognee` — Java classes only, no native lib.
  - `ai.cognee:cognee` with platform **classifiers** (`linux-x86_64`,
    `linux-aarch_64`, `osx-aarch_64`, `windows-x86_64`) — each jar contains
    only `native/<platform>/libcognee_java.{so,dylib,dll}`.
  - Consumers use the os-detector Maven plugin or an explicit classifier.
  - Platform set = the existing `ts-prebuild.yml` matrix (4 targets;
    `ts/platform-packages/` names them darwin-arm64, linux-arm64-gnu,
    linux-x64-gnu, win32-x64-msvc). Android AAR is explicitly **post-v1**.
- **`NativeLibLoader`:** resolve platform → classifier resource path → extract
  to temp dir → `System.load`; support `COGNEE_JAVA_LIB_PATH` env override for
  development (load a locally built `target/{debug,release}` cdylib without
  jar packaging). Load exactly once (static holder idiom).
- **Version:** the jar version tracks the workspace/release version; the shim
  crate exposes `version()` returning the Rust crate version so L3 can assert
  native/Java version match at load time (fail fast on mismatch — this
  prevents the classic skew bug between jar and native lib).
- **CI:**
  - `ci.yml`: add `java-check` job — build cdylib (linux x64), run
    `java/scripts/check.sh` (needs a JDK via `actions/setup-java`,
    Temurin 17 on CI is fine even if the source floor is 11).
  - `java-prebuild.yml`: clone of `ts-prebuild.yml` matrix producing the
    classifier jars; release wiring follows `release-publish.yml` patterns
    (Maven Central publishing via Sonatype — needs new secrets; flag as an
    infra prerequisite, not a code task).
- **check_all.sh:** append a `Java binding check` section invoking
  `java/scripts/check.sh` after the TS check. The script must gracefully
  no-op with a clear message if no JDK/Maven is installed (developers without
  Java shouldn't be blocked — mirror how other scripts handle missing
  optional toolchains, and make CI the enforcing environment).

## 8. Testing strategy

- **Rust shim:** minimal — the crate is glue; correctness lives in
  `bindings-common` (already covered). A compile check + clippy in check.sh.
  (Note the python crate's Cargo.toml comment about why cdylib binding crates
  disable Rust test harnesses / stay out of the workspace — same logic.)
- **Java unit tests (no LLM):** JUnit 5. Cover: native load, handle
  lifecycle (`close()` idempotency, use-after-close), config set/get
  round-trip + `CONFIG_TYPE_MISMATCH` surfacing, error mapping, JSON
  deserialization of canned payloads, `add` + `datasets().list()` against
  sqlite in a temp dir (deterministic, no network — mirrors what
  `test_add_parity.py` proves possible without an LLM).
- **Integration tests (LLM-gated):** `warm → add → cognify → search`
  end-to-end, skipped unless `OPENAI_URL`/`OPENAI_TOKEN` are set — same
  convention as the Rust workspace tests (graceful skip, see CLAUDE.md test
  patterns).
- **Non-goals for v1:** Android build/AAR, cross-SDK e2e harness integration,
  granular config setters, cancellation, sync (non-future) API variants,
  Kotlin-idiomatic wrappers, GraalVM native-image metadata.

## 9. Open decisions the implementation plan must resolve (or default)

| # | Decision | Default if not overridden |
|---|---|---|
| 1 | Java source/target floor | 11 (bytecode 55); revisit 8 only on concrete demand |
| 2 | Build tool for `java/sdk` | Maven (better Central publishing story; Gradle acceptable) |
| 3 | JSON library | Jackson-core (shaded or plain dependency — planner picks based on jar-size stance) |
| 4 | Package/group naming | `ai.cognee` / `ai.cognee:cognee` — **verify with maintainers**, requires Central namespace ownership |
| 5 | JNI method registration | `RegisterNatives` in `JNI_OnLoad` (explicit, typo-proof) vs name-mangled exports — planner picks; jni-rs supports both |
| 6 | Handle representation | `long` (raw `Box`/`Arc` pointer) held in the Java object, standard pattern; document ownership + destroy in one place |
| 7 | Progress/watcher/pipeline surface | TS exposes pipeline/task/watcher extras; **exclude from Java v1**, ops-parity only |
| 8 | Sonatype/Maven Central credentials & namespace verification | infra prerequisite; cannot be done by code changes |

## 10. Risks & considerations checklist (for the planner)

- **JNI + panics = UB** → catch_unwind everywhere (§5).
- **Global-ref leaks** → every `CompletableFuture` global ref must be dropped
  on both success and failure paths; audit with `-Xcheck:jni` in CI test runs.
- **Class-loader pitfalls on worker threads** → cache class global refs at
  load (§6.4).
- **Version skew jar↔cdylib** → fail-fast version handshake (§7).
- **Windows path/encoding**: JNI strings are *modified* UTF-8 — use jni-rs
  `JNIEnv::get_string` (handles it) and never assume standard UTF-8 on the
  boundary; test non-ASCII dataset names.
- **Two GCs, one process**: tokio worker threads attached as daemons; never
  hold `JNIEnv` across `.await` (it is thread-local — re-attach after the
  await point; structure shim code so all JNI happens before spawn or after
  completion, nothing in between).
- **`cargo check --all-targets` does not cover this crate** (standalone) —
  `java/scripts/check.sh` must run `cargo check`/`clippy` for it explicitly,
  like `ts/scripts/check.sh` does for the neon crate.
- **Docs to update on completion** (repo rule: architecture.md is the single
  source of truth): `docs/architecture.md`, `docs/tools/bindings.md` (add the
  4th row + config-surface table entry), `docs/tools/README.md`, root
  `README.md`, `.claude/CLAUDE.md` check-suite description if check_all.sh
  gains a stage.

## 11. Suggested phasing (high level — the plan should decompose further)

1. **Skeleton + lifecycle:** `java/` layout, shim crate with `init`/handle
   new/destroy/version, `NativeLibLoader`, `Cognee` + `close()`, check.sh,
   CI job. Exit criterion: `new Cognee(...)` + `close()` round-trips on
   linux-x64 dev build.
2. **Config + errors:** `set`/`setStr`/bulk/get, `CogneeException` mapping,
   `CONFIG_TYPE_MISMATCH` test.
3. **Core ops:** `warm`, `add`, `cognify`, `search`, `addAndCognify` with the
   full async upcall machinery (§6) — this phase de-risks everything else.
4. **Remaining op groups:** datasets, sessions, memory (`memify`/`remember`/
   `improve`/`forget`/`update`), notebooks/users/admin, visualization,
   logging/telemetry statics.
5. **Typed results + polish:** POJOs/records for all payloads, Javadoc,
   examples, README.
6. **Packaging + release:** prebuild workflow, classifier jars, loader
   fallback chain, version handshake, (infra) Central publishing.
