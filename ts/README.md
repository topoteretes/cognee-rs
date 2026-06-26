# cognee-ts

Node.js bindings for the [cognee-rs](https://github.com/topoteretes/cognee-rs)
AI-memory SDK, built with [Neon](https://neon-bindings.com/).

Cognee transforms raw text, files, and URLs into a persistent, queryable knowledge graph.
The high-level API is **remember** (ingest + extract in one call) → **recall** (source-aware
retrieval). These wrap the lower-level **add** → **cognify** → **search** stages, which remain
available when you need finer control.

## Installation

```bash
npm install @cognee/cognee-ts
```

## Quick start

```ts
import { init, Cognee } from '@cognee/cognee-ts';

// Boot the Rust async runtime (call once at process start).
init();

const c = new Cognee({
  llmModel:   "gpt-4o-mini",
  llmApiKey:  process.env.OPENAI_TOKEN,
});

// Warm up engines (builds embedding model, resolves default user).
await c.warm();

// Ingest content and extract a knowledge graph in one call.
await c.remember({ type: "text", text: "The quick brown fox jumps over the lazy dog." }, "demo");

// Recall an answer with source-aware routing.
const recall = await c.recall("What does the fox do?");
console.log(recall.searchResponse?.result?.data);
```

Fully-annotated runnable examples are available in the [`examples/`](examples/) directory.

| Example | npm script | What it covers |
|---|---|---|
| [`remember-recall.ts`](examples/remember-recall.ts) | `npm run example` | High-level remember → recall pipeline |
| [`add-cognify-search.ts`](examples/add-cognify-search.ts) | `npm run example:add-cognify-search` | Lower-level add → cognify → search pipeline |
| [`memify-recall.ts`](examples/memify-recall.ts) | `npm run example:memify` | Triplet embeddings (memify) + session recall |
| [`datasets.ts`](examples/datasets.ts) | `npm run example:datasets` | Dataset listing, status, deletion |
| [`sessions.ts`](examples/sessions.ts) | `npm run example:sessions` | QA history, feedback, graph-context snapshots |
| [`config.ts`](examples/config.ts) | `npm run example:config` | Programmatic config (LLM / embedding / DBs) |
| [`visualize.ts`](examples/visualize.ts) | `npm run example:visualize` | Render knowledge graph to HTML |
| [`pipeline-engine.ts`](examples/pipeline-engine.ts) | `npm run example:pipeline` | Low-level pipeline API (no credentials needed) |

All examples validate required env vars up front and exit 0 with a clear `SKIP`
message when they are absent, so they can be run in CI without secrets.

## Constructor

```ts
const c = new Cognee(settings?)
```

`settings` is an optional object (or JSON string) that overrides env-derived defaults.
Keys are the canonical Settings field names (`llmModel`, `embeddingProvider`,
`vectorDbProvider`, etc.). Absent keys keep their env-variable or compiled-in default.

## Config

Use `c.config` to change settings after construction. Granular setters are synchronous
and take effect immediately (the engines are lazily rebuilt on the next pipeline call).

```ts
c.config.setLlmModel("gpt-4o");
c.config.setLlmApiKey(process.env.OPENAI_TOKEN!);
c.config.setEmbeddingProvider("openai");
c.config.setEmbeddingModel("text-embedding-3-small");

// Bulk setters (throw on unknown key or type mismatch) — one per subsystem:
c.config.setLlmConfig({ model: "gpt-4o", temperature: 0.2 });
c.config.setEmbeddingConfig({ provider: "openai", model: "text-embedding-3-small" });
c.config.setVectorDbConfig({ provider: "brute-force" });
c.config.setGraphDbConfig({ provider: "kuzu" });

// Generic key-value setter:
c.config.set("llmModel", "gpt-4o-mini");

// Read back the current config (secret fields are redacted):
const cfg = c.config.get();
console.log(cfg);
```

## Pipeline operations

### add

Ingest one or more data items into a named dataset.

```ts
// Text
await c.add({ type: "text", text: "…" }, "my-dataset");

// File
await c.add({ type: "file", path: "/abs/path/to/doc.txt" }, "my-dataset");

// URL
await c.add({ type: "url", url: "https://example.com/article" }, "my-dataset");

// Binary (name is required for MIME detection)
await c.add({ type: "binary", bytes: buffer, name: "report.pdf" }, "my-dataset");

// Multiple items at once
await c.add([
  { type: "text", text: "First document" },
  { type: "file", path: "/abs/path/two.txt" },
], "my-dataset");
```

### cognify

Extract entities and relationships into the knowledge graph.

```ts
await c.cognify("my-dataset");

// With options
await c.cognify("my-dataset", {
  chunkSize: 512,
  summarization: true,
  triplet: true,       // also index triplet embeddings (enables TripletCompletion search)
});
```

### addAndCognify

Ingest and extract in a single call.

```ts
const { add, cognify } = await c.addAndCognify(
  { type: "text", text: "…" },
  "my-dataset"
);
```

## Search and recall

### search

Query the knowledge graph. Defaults to `GRAPH_COMPLETION`.

```ts
const result = await c.search("What is the capital of France?");

// With options
const result = await c.search("summarise recent events", {
  searchType: "SUMMARIES",
  topK: 5,
  datasets: ["news"],
});
```

All 15 search types are supported (SCREAMING_SNAKE_CASE):
`GRAPH_COMPLETION`, `SUMMARIES`, `CHUNKS`, `RAG_COMPLETION`, `TRIPLET_COMPLETION`,
`GRAPH_SUMMARY_COMPLETION`, `CYPHER`, `NATURAL_LANGUAGE`, `GRAPH_COMPLETION_COT`,
`GRAPH_COMPLETION_CONTEXT_EXTENSION`, `FEELING_LUCKY`, `FEEDBACK`, `TEMPORAL`,
`CODING_RULES`, `CHUNKS_LEXICAL`.

### recall

Session-first routing: checks session QA history before falling back to graph search.

```ts
const result = await c.recall("What did we discuss?", {
  sessionId: "session-uuid",
  scope: "auto",   // "graph" | "session" | "trace" | "graph_context" | "all"
});
```

## Memory operations

### remember

Composite add + cognify with an optional improvement pass.

```ts
await c.remember({ type: "text", text: "…" }, "my-dataset", {
  selfImprovement: true,   // run memify after cognify
  sessionId: "session-id", // session-only mode (no graph writes)
});
```

### memify

Index triplet embeddings from the existing knowledge graph.
Enables `TripletCompletion` search. Idempotent.

```ts
await c.memify();
```

### improve

Run the four-stage session-graph bridge pipeline.

```ts
await c.improve({
  datasetName: "my-dataset",
  sessionIds: ["session-uuid"],
});
```

### rememberEntry

Store a typed memory entry (`"qa"`, `"trace"`, or `"feedback"`) in a session.

```ts
const result = await c.rememberEntry(
  { type: "qa", question: "…", answer: "…" },
  "my-dataset",
  "session-uuid",
  { tenant: "tenant-id" }, // optional
);
```

## Datasets

```ts
const datasets   = await c.datasets.list();
const items      = await c.datasets.listData(datasetId);
const hasContent = await c.datasets.has(datasetId);
const statuses   = await c.datasets.status([id1, id2]);

await c.datasets.empty(datasetId);
await c.datasets.deleteData(datasetId, dataId);
await c.datasets.deleteAll();
```

## Sessions

```ts
const entries = await c.sessions.get("session-uuid", { lastN: 10 });

await c.sessions.addFeedback("session-uuid", "qa-uuid", "Great answer!", 5);
await c.sessions.deleteFeedback("session-uuid", "qa-uuid");

const ctx = await c.sessions.getGraphContext("session-uuid");
await c.sessions.setGraphContext("session-uuid", "new context");
```

## Notebooks

```ts
// List all notebooks for the current user.
const notebooks = await c.notebooks.list();

// Create a new notebook with optional cells and deletability flag.
const nb = await c.notebooks.create("My Notes", [], true);

// Partially update a notebook (name, cells, or both).
const updated = await c.notebooks.update(nb.id, { name: "Renamed Notes" });

// Delete a notebook — returns true if a row was removed.
const removed = await c.notebooks.delete(nb.id);
```

## Users and pipeline-run admin

```ts
// Resolve (or lazily create) the default user for this handle.
const user = await c.users.getOrCreateDefault();

// Unblock a dataset stuck in "running" state so it can be re-cognified.
await c.users.resetPipelineRunStatus(datasetId, "cognify_pipeline");

// Reset all pipeline-run statuses for a dataset at once.
await c.users.resetDatasetPipelineRunStatus(datasetId);
```

## Data lifecycle

```ts
// Forget a single item
await c.forget({ kind: "item", dataId: "uuid", dataset: { name: "my-dataset" } });

// Forget an entire dataset
await c.forget({ kind: "dataset", dataset: { name: "my-dataset" } });

// Forget everything
await c.forget({ kind: "all" });

// Replace a data item (delete → re-add → re-cognify)
await c.update("old-data-uuid", { type: "text", text: "updated content" }, "my-dataset");

// Remove all files from storage (metadata DB untouched)
await c.pruneData();

// Wipe graph, vector, metadata, and/or cache backends
await c.pruneSystem({ pruneGraph: true, pruneVector: true });
```

## Cloud: serve / disconnect

`serve` and `disconnect` are module-level functions (not instance methods) because
they operate on global cloud state.

```ts
import { serve, disconnect } from '@cognee/cognee-ts';

// Direct mode (no Auth0 flow; headless-friendly)
const { serviceUrl } = await serve({ url: "http://localhost:8000", apiKey: "key" });
console.log("Connected to", serviceUrl);

// Cloud mode (Auth0 device-code flow — requires a TTY)
await serve();

// Tear down
await disconnect();
await disconnect({ wipeCredentials: true }); // also removes the local credential cache
```

## Visualisation

```ts
// Get the HTML string
const html = await c.visualize();

// Write to a file (returns the absolute path)
const path = await c.visualizeToFile({ destinationPath: "/tmp/graph.html" });
```

Requires the `visualization` feature compiled into the native addon.

## Initialisation and observability

```ts
import {
  init,
  initWithThreads,
  shutdown,
  setupLogging,
  setupTelemetry,
  setupTelemetryAnalytics,
} from '@cognee/cognee-ts';

// Boot the Rust tokio runtime (required before any async op).
init();

// Alternatively boot with a fixed worker-thread count.
initWithThreads(4);

// Optional: add file logging (reads COGNEE_LOG_*, LOG_FILE_NAME, LOG_LEVEL).
setupLogging();

// Optional: enable OTLP trace export (reads OTEL_* env vars).
setupTelemetry();

// Optional: enable product-analytics emission (returns true if armed).
const armed = setupTelemetryAnalytics();

// Tear the runtime down (e.g. before process exit).
shutdown();
```

Each handle also exposes `await c.ownerId()`, returning the owner UUID used for
deterministic, per-tenant ID generation.

Set `COGNEE_BINDING_SUPPRESS_LOGS=1` before `require`ing the module to skip the
auto-installed stderr subscriber if your host manages the logging pipeline.

## Environment variables

| Variable | Purpose |
|---|---|
| `OPENAI_URL` | LLM API base URL (OpenAI-compatible endpoint). |
| `OPENAI_TOKEN` | LLM API key. |
| `OPENAI_MODEL` | LLM model name (default: `gpt-4o-mini`). |
| `EMBEDDING_PROVIDER` | Embedding provider: `openai`, `ollama`, `onnx`, `mock`. |
| `EMBEDDING_MODEL` | Embedding model name. |
| `EMBEDDING_DIMENSIONS` | Embedding vector dimensions. |
| `EMBEDDING_ENDPOINT` | Embedding API base URL (falls back to `OPENAI_URL`). |
| `EMBEDDING_API_KEY` | Embedding API key (falls back to `OPENAI_TOKEN`). |
| `MOCK_EMBEDDING` | Set `true` to use zero-vector mock embeddings (no model download). |
| `COGNEE_BINDING_SUPPRESS_LOGS` | Suppress the auto-installed stderr fmt subscriber. |
| `COGNEE_HOST_SDK` | Suppress binding-armed analytics when the host is an embedding SDK. |
| `TELEMETRY_DISABLED`, `ENV` | Standard analytics opt-outs for `setupTelemetryAnalytics()`. |
| `RUST_LOG`, `LOG_LEVEL` | `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `setupLogging()`. |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_*` | Consumed by `setupTelemetry()`. |

---

## Appendix: low-level pipeline API

The original pipeline engine API is available under the `pipeline` namespace:

```ts
import { pipeline, init } from '@cognee/cognee-ts';

init();

const task = pipeline.createTask((input: pipeline.CogneeValue, ctx: pipeline.TaskContext) => {
  // process input …
  return input;
});

const p = new pipeline.Pipeline("my pipeline");
p.addTask(new pipeline.TaskInfo(task));

const [result] = await p.execute([pipeline.CogneeValue.fromString("hello")], ctx);
```

All symbols previously exported from `@cognee/pipeline` are available at the top
level of `@cognee/cognee-ts` for backward compatibility, and also under `pipeline.*`:

```ts
import {
  Pipeline,
  TaskInfo,
  createTask,
  CogneeValue,
  TaskContext,
  RunHandle,
  CancellationHandle,
  CancellationToken,
  createCancellationPair,
  ProgressToken,
  Watcher,
  createWatcher,
  createNoopWatcher,
} from '@cognee/cognee-ts';
```

---

## Migration guide

Rename the package and update imports:

```diff
- import { Pipeline } from '@cognee/pipeline';
+ import { pipeline } from '@cognee/cognee-ts';
+ const { Pipeline } = pipeline;
```

Or use the flat re-exports (still supported):

```ts
import { Pipeline } from '@cognee/cognee-ts'; // flat legacy export — unchanged
```

---

## References

- Observability: [docs/observability/opentelemetry.md](../docs/observability/opentelemetry.md), [docs/observability/send_telemetry.md](../docs/observability/send_telemetry.md)
- Python bindings: [python/README.md](../python/README.md)
- C API bindings: [capi/README.md](../capi/README.md)
- cognee-rs workspace: [README.md](../README.md)
- Source: [cognee-rs](https://github.com/topoteretes/cognee-rs)
