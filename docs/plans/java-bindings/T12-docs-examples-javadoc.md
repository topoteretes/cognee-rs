# T12 — Docs + examples + Javadoc + README

## Objective

After this task the Java binding is documented as a first-class citizen: the
repo docs list it alongside Python/TS/C, a `java/README.md` and a runnable
example exist, and `mvn javadoc:javadoc` builds the public API docs cleanly.

## Dependencies & preconditions

- **T06–T11 done** (the full op surface exists to document). Verify
  `bash java/scripts/check.sh` passes.
- Read the current state of each doc to be edited (structures confirmed against
  the repo):
  - `docs/architecture.md` — the workspace-tree fenced block (binding dirs
    `capi/`, `ts/`, `python/` sit at repo root after the `crates/` block, before
    `examples/`) and the crate-breakdown line for `cognee-bindings-common`
    ("Shared SDK facade for the Neon JS and C-API bindings") and the Key
    dependencies table row `| pyo3 / neon | Python / JavaScript bindings |`.
  - `docs/tools/bindings.md` — the bindings table (columns **Binding | README |
    Entry type | Async model**) and the config-surface table (columns **Binding |
    Setter surface**).
  - `docs/tools/README.md` — the `## Interfaces` bullet for `bindings.md`
    ("Python / C / JavaScript SDKs …").
  - root `README.md` — the `## Language Bindings` table (columns **Binding |
    Install | README | Primary API**).
  - `.claude/CLAUDE.md` — the `scripts/check_all.sh` stage description
    ("Runs in order: … → C API check → Python binding check → TS binding check").

## Steps

### 1. `docs/architecture.md`

- **Workspace tree:** add a `java/` line after the `python/` line and before
  `examples/`:
  ```
  ├── java/                       # Java/JVM bindings (JNI via the jni crate)
  ```
- **Crate breakdown:** update the `cognee-bindings-common` description to read
  "Shared SDK facade for the Neon JS, C-API, **and Java (JNI)** bindings".
- **Key dependencies table:** update the bindings row to include `jni`, e.g.
  `| pyo3 / neon / jni | Python / JavaScript / Java bindings |`.

### 2. `docs/tools/bindings.md`

- Change the title `# Language bindings (Python / C / JavaScript)` to
  `# Language bindings (Python / C / JavaScript / Java)`.
- Add a **Java** row to the bindings table:
  ```
  | **Java** (JNI/jni-rs) | [java/README.md](../../java/README.md) | `Cognee` (`import ai.cognee.Cognee`) | `CompletableFuture<T>` |
  ```
- Add a **Java** row to the config-surface table (matching C/Python — generic +
  bulk + get):
  ```
  | **Java** | `config().set` / `config().setStr`, the 4 bulk setters, `config().get` | snake_case keys |
  ```
  (Note in the casing column that Java uses snake_case keys, like Python/C.)

### 3. `docs/tools/README.md`

Update the `bindings.md` bullet under `## Interfaces` to mention Java:
"**[bindings.md](bindings.md)** — Python / C / JavaScript / Java SDKs (shared
`bindings-common`) + config-setter ergonomics."

### 4. root `README.md`

Add a **Java** row to the `## Language Bindings` table:

```
| **Java** (JNI) | build from source (`mvn -f java/pom.xml install`) — not yet on Maven Central | [java/README.md](java/README.md) | `import ai.cognee.Cognee;` |
```

If the intro paragraph enumerates the bindings, add Java there too.

### 5. `.claude/CLAUDE.md`

In the `scripts/check_all.sh` description, append the Java stage to the ordered
list: "… → C API check (`capi/scripts/check.sh`) → Python binding check
(`python/scripts/check.sh`) → TS binding check (`ts/scripts/check.sh`) →
**Java binding check (`java/scripts/check.sh`)**." Also add `java/` to any
binding enumeration in that file.

### 6. Create `java/README.md`

Cover: what it is (JNI over `cognee-bindings-common`), requirements (JDK 17+,
Maven), install/build (`mvn -f java/pom.xml install`, or dev via
`COGNEE_JAVA_LIB_PATH`), a quickstart, the op surface (link the Javadoc), the
error model (`CogneeException.code()`), the config surface (A3.1), telemetry
opt-out (`COGNEE_HOST_SDK`/`TELEMETRY_DISABLED`), and the three-layer
architecture (one paragraph, link the design doc). Include a runnable snippet:

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

### 7. Create a runnable example `java/examples/`

Add `java/examples/Quickstart.java` (or a small example Maven profile/module).
Keep it credential-gated (print a SKIP message and exit 0 when
`OPENAI_URL`/`OPENAI_TOKEN` are absent), mirroring the TS/Python example
skip-guards so `java/scripts/check.sh` can optionally run it. Wire it as a
manually-runnable class documented in `java/README.md` (do not add it to the
default surefire test run — examples are opt-in, matching the TS example pattern).

### 8. Javadoc packaging

Ensure the public API (`ai.cognee.*`) carries class/method Javadoc and the
internal package (`ai.cognee.internal.*`) is excluded from the published docs.
Add the javadoc plugin config to `java/pom.xml`:

```xml
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-javadoc-plugin</artifactId>
        <version>3.6.3</version>
        <configuration>
          <excludePackageNames>ai.cognee.internal</excludePackageNames>
          <quiet>true</quiet>
          <doclint>none</doclint>
        </configuration>
      </plugin>
```

Add concise Javadoc to any public class/method still missing it (constructors,
each op, each `*Options` builder, `SearchType`, `CogneeException`).

## Verification

1. `mvn -q -f java/pom.xml javadoc:javadoc` → builds without errors.
2. `bash java/scripts/check.sh` → still green (docs/examples do not break it).
3. `scripts/check_all.sh` → green.
4. Spot-check the edited docs render (no broken tables) and every new link
   resolves (`java/README.md`, Javadoc path).

## Out of scope

- Prebuild classifier-jar workflow → **T13**.
- Maven Central publishing → **T14** (blocked).
- API changes: this is docs-only + example + Javadoc; do not alter op signatures.
