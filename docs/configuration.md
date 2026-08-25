# Configuration

Canonical reference for configuring cognee-rust. The complete, field-level source
of truth is the [`Settings`](../crates/lib/src/config.rs) struct and the
[`ConfigManager`](../crates/lib/src/config.rs) runtime API — build the rustdoc with
`cargo doc -p cognee --no-deps --open` to browse every field and setter with
its type. This page groups those fields by subsystem and gives the env-var name
and default for each.

## How configuration resolves

Three layers, lowest precedence first:

1. **Defaults** — `Settings::default()` in [`crates/lib/src/config.rs`](../crates/lib/src/config.rs).
2. **Persisted config file** *(CLI only)* — JSON at `~/.config/cognee-rust/config.json`
   (`$XDG_CONFIG_HOME/cognee-rust/config.json`), managed by `cognee-cli config`.
   See [`crates/cli/src/config_store.rs`](../crates/cli/src/config_store.rs).
3. **Environment variables** — bound by `Settings::overlay_from_env()`. A `.env`
   file in the working directory (or any ancestor) is loaded automatically via `dotenv`.

So: `defaults < config.json < env`. At runtime, code can also mutate settings
through `ConfigManager`'s `set_*` methods (below) or the binding config APIs.

Parsing notes: booleans accept `true|1|yes` / `false|0|no` (`cognee_utils::parse_env_bool`);
empty env values are treated as unset; numeric vars that fail to parse are ignored.

## LLM

Read by the LLM adapter. The deep reference is [tools/cli.md](tools/cli.md#llm-retries)
for retries and the `cognee-llm` rustdoc for the adapter.

| Env var (aliases) | `Settings` field | Default |
|---|---|---|
| `LLM_PROVIDER` | `llm_provider` | `openai` |
| `LLM_MODEL` / `OPENAI_MODEL` | `llm_model` | `openai/gpt-5-mini` |
| `LLM_API_KEY` / `OPENAI_TOKEN` | `llm_api_key` | _(empty)_ |
| `LLM_ENDPOINT` / `OPENAI_URL` | `llm_endpoint` | _(empty)_ |
| `LLM_API_VERSION` | `llm_api_version` | _(empty)_ |
| `LLM_REASONING` | `llm_reasoning` | `auto` |
| `LLM_TEMPERATURE` | `llm_temperature` | `0.0` |
| `LLM_STREAMING` | `llm_streaming` | `false` |
| `LLM_MAX_COMPLETION_TOKENS` / `LLM_MAX_TOKENS` | `llm_max_completion_tokens` | `16384` |
| `LLM_MAX_RETRIES` | `llm_max_retries` | `2` |
| `LLM_MAX_PARALLEL_REQUESTS` | `llm_max_parallel_requests` | `20` |
| `MOCK_LLM` | `llm_mock` | `false` |
| `MOCK_LLM_CASSETTE` | `llm_cassette` | _(empty)_ |
| `COGNEE_RECORD_LLM` | `llm_record_path` | _(empty)_ |

A fallback LLM (`llm_fallback_provider/_model/_endpoint/_api_key`) is configurable
programmatically (no env binding). `MOCK_LLM` + cassettes power the offline
benchmark — see [performance/mock-benchmark.md](performance/mock-benchmark.md).

### Supported `LLM_PROVIDER` values

Several providers are OpenAI-compatible HTTP endpoints, so they route through the
same adapter — differing only in base URL and litellm-style model-prefix stripping.
`LLM_API_KEY` is required for every provider **except `bedrock`** (matching the
Python SDK, whose `_API_KEY_REQUIRED_PROVIDERS` omits Bedrock and lists it under
`_NO_API_KEY_PROVIDERS` instead; for a local Ollama any non-empty value works).
On `bedrock` an unset or empty `LLM_API_KEY` is a supported configuration — it
falls through to SigV4 signing rather than erroring (see
[Bedrock](#bedrock-llm_providerbedrock) below). The OpenAI-only request quirks
are gated on the `api.openai.com` host, so they never fire against another
endpoint.

| `LLM_PROVIDER` | `LLM_API_KEY` | `LLM_ENDPOINT` default | Model prefix stripped |
|---|---|---|---|
| `openai` | required | `https://api.openai.com/v1` | `openai/` |
| `ollama` | required (any value for local) | `http://localhost:11434/v1` | `ollama/` |
| `mistral` | required | `https://api.mistral.ai/v1` | `mistral/` |
| `gemini` | required | `https://generativelanguage.googleapis.com/v1beta/openai/` | `gemini/` |
| `custom` / `openai_compatible` | required | **required** (no default) | _(none)_ |
| `mock` | — | — | — (see `MOCK_LLM`) |

`LLM_ENDPOINT` always overrides the default when set. Audio transcription
(Whisper) is wired only for `openai` and `custom`/`openai_compatible` (which may
expose `/audio/transcriptions`); `ollama`/`mistral`/`gemini`/`bedrock` get
graceful no-audio.

`anthropic` uses a **native Messages-API adapter** (not the OpenAI-compatible
factory): it authenticates with `x-api-key`, hoists the system prompt into the
top-level `system` field, and produces structured output via a forced `tool_use`.
It requires `LLM_API_KEY`, strips an `anthropic/` model prefix, honors `LLM_ARGS`
(merged into each request), and supports image description (vision) but not audio
transcription. The endpoint defaults to `https://api.anthropic.com/v1`: unlike the
OpenAI-compatible path it deliberately ignores `LLM_ENDPOINT`/`OPENAI_URL` (which
alias each other) so a stray `OPENAI_URL` cannot misroute Anthropic traffic —
matching the Python SDK, which passes no base URL. To route Anthropic traffic
through a proxy / gateway / Bedrock-compatible endpoint, set the dedicated
`ANTHROPIC_BASE_URL` (alias `ANTHROPIC_API_BASE`); it overrides the default only
on the Anthropic path and other providers ignore it.

`azure` reuses the OpenAI request path with Azure's auth and URL conventions: it
authenticates with the `api-key` header and appends an `?api-version=<v>` query.
Set `LLM_PROVIDER=azure`, `LLM_API_KEY`, `LLM_API_VERSION` (e.g.
`2024-12-01-preview`), and `LLM_ENDPOINT` to the **deployment** URL
(`https://<resource>.openai.azure.com/openai/deployments/<deployment>`); the model
in the request body is ignored by Azure for **routing** since the deployment is in
the URL. It is *not* inert for reasoning-model detection (below): `LLM_MODEL` still
selects the parameter shape, so for an o-series/`gpt-5` deployment set `LLM_MODEL`
to the underlying model name (the adapter also treats a `.../deployments/o3` style
deployment segment as reasoning), or pin it explicitly with `LLM_REASONING`.

### Bedrock (`LLM_PROVIDER=bedrock`)

`bedrock` is a **native adapter over Amazon Bedrock's Converse API**
(`POST {endpoint}/model/{modelId}/converse`), not an OpenAI-compatible endpoint.
Everything it needs beyond `LLM_MODEL` comes from the ambient `AWS_*`
environment — deliberately, so AWS configuration never has to travel through
`Settings` or the language bindings. The names mirror the ones litellm reads, and
several of them are *not* the boto3-standard spellings:

| Env var | Purpose |
|---|---|
| `AWS_ACCESS_KEY_ID` | Static access key |
| `AWS_SECRET_ACCESS_KEY` | Static secret key |
| `AWS_SESSION_TOKEN` | Session token accompanying temporary static credentials |
| `AWS_REGION_NAME` | Region — the first env rung of the region chain below |
| `AWS_REGION` | Region — one rung below `AWS_REGION_NAME` |
| `AWS_DEFAULT_REGION` | Not part of the region chain; read only when resolving the STS signing region |
| `AWS_PROFILE_NAME` | Shared-config profile. **Not** the boto3-standard `AWS_PROFILE`, which this path ignores |
| `AWS_ROLE_NAME` | Role ARN to assume via STS |
| `AWS_ROLE_ARN` | Ambient role ARN published by an IRSA / EKS pod identity |
| `AWS_SESSION_NAME` | STS session name |
| `AWS_WEB_IDENTITY_TOKEN` | **Path to** a readable OIDC web-identity token file. A literal token value is rejected with a config error — a documented divergence from litellm, which resolves this through `get_secret()` and passes the token *value* to `sts:AssumeRoleWithWebIdentity`. See `aws/credentials.rs` for why |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | Ambient web-identity token file published by an IRSA / EKS pod identity |
| `AWS_STS_ENDPOINT` | Override the STS endpoint |
| `AWS_EXTERNAL_ID` | `ExternalId` passed to `AssumeRole` |
| `AWS_BEDROCK_RUNTIME_ENDPOINT` | Override the Bedrock runtime host |
| `AWS_BEARER_TOKEN_BEDROCK` | Bedrock API key, used as a bearer token |

**Auth ladder.** `LLM_API_KEY`, when set, is sent verbatim as
`Authorization: Bearer <key>` and the request is **not** signed — no credential
lookup runs at all, so a bearer-only deployment needs no AWS credentials
present. Otherwise `AWS_BEARER_TOKEN_BEDROCK` takes the same short-circuit. Only
when both are absent does the request fall through to SigV4 (signing service
`bedrock`), whose credentials resolve in this order:

1. web-identity token + role + session name → STS `AssumeRoleWithWebIdentity`;
2. role ARN → STS `AssumeRole` — **skipped when the process is already running
   as that role**, so an IRSA pod that already holds it does not assume itself;
3. profile name → the shared-config profile;
4. static access key + secret (plus session token when present);
5. otherwise the default chain (env / shared config / SSO / ECS / IMDS).

**Region and endpoint.** The region resolves as: an explicitly configured
region → a region embedded in a model ARN → `AWS_REGION_NAME` → `AWS_REGION` →
the profile / default chain → the hard default **`us-west-2`**. The ARN rung
outranks the env vars, matching litellm. The endpoint resolves as: an explicit
API base → `AWS_BEDROCK_RUNTIME_ENDPOINT` →
`https://bedrock-runtime.{region}.amazonaws.com`.

`LLM_ENDPOINT` is **not** that API base: like the Anthropic path above, the
Bedrock factory deliberately passes no base URL, so a stray `LLM_ENDPOINT` (which
aliases `OPENAI_URL`) cannot misroute Bedrock traffic to an OpenAI host. Point a
Bedrock deployment at a custom runtime host with `AWS_BEDROCK_RUNTIME_ENDPOINT`
instead.

**Converse only.** Every model cognee ships routes to Converse — but only after
the model id is normalised (an ARN wrapper is unwrapped, a cross-region
inference prefix such as `eu.`/`us.`/`apac.` is stripped, as are provisioned-
throughput and context-window suffixes). Normalisation feeds the routing and
capability decisions only; the request URL always carries the model id exactly as
configured. A model that routes to Bedrock's legacy `invoke` API for **chat** is
unsupported by design: construction fails with a clear
`LlmError::FeatureNotSupported` rather than posting a Converse body to an
endpoint that cannot serve it. (Embeddings are a separate path and *do* go over
InvokeModel — see [Embedding](#embedding).)

**Capabilities.** No streaming. No audio transcription (Bedrock has no Whisper
equivalent, so the transcriber is simply absent). Image description (vision)
**is** implemented, over Converse image blocks — this deliberately *exceeds*
Python parity, where `BedrockAdapter.transcribe_image` raises
`NotImplementedError`. It is gated on a per-model capability table, though: a
model the table does not list is treated conservatively as text-only, and asking
it for image description returns `LlmError::FeatureNotSupported` rather than a
request the model would reject. Structured output is capability-gated per model
from the same table: models
that advertise native structured output get Converse's `outputConfig.textFormat`
carrying the JSON schema, and the rest get a synthetic `json_tool_call` tool
whose input schema is the response schema (with `toolChoice` forced only for
models that support tool choice).

**Feature gating.** The adapter and the embedding engine live behind the
`bedrock` Cargo feature. It is **non-default** on the two leaf crates that carry
the code, `cognee-llm` and `cognee-embedding` — the AWS crate stack is not free —
but it **is** in the `default` feature list of `cognee-components`, `cognee`,
`cognee-http-server` and `cognee-cli`, so every shipped binary has Bedrock
available. Building any of those with `--no-default-features` (or without
`bedrock` in the feature list) drops the whole AWS stack, and
`LLM_PROVIDER=bedrock` then reports an unsupported provider.

### Reasoning-model parameters (`LLM_REASONING`)

OpenAI reasoning families (`o1*`, `o3*`, `o4*`, `gpt-5*`) require a different
request shape — `max_completion_tokens` instead of `max_tokens`, and no
`temperature`/`top_p`/penalties. The adapter auto-detects this from the model
name for any non-local endpoint (loopback and Ollama's port keep the legacy
shape). `LLM_REASONING` overrides that detection:

- `auto` (default) — name/host auto-detection as above.
- `always` — force the reasoning shape (e.g. a proxy that hides a reasoning model
  behind an opaque alias).
- `never` — force the legacy `max_tokens`+`temperature` shape (e.g. a remote
  OpenAI-compatible gateway serving a reasoning-*named* model that only accepts
  the legacy parameters).

> **Ollama embeddings:** set `EMBEDDING_ENDPOINT` explicitly when using
> `EMBEDDING_PROVIDER=ollama`. The Ollama embedder needs the `/api/embed` route, and
> the embedding endpoint does not inherit `LLM_ENDPOINT` (which points at the `/v1`
> chat base), so leaving it unset would target the wrong path.

## Embedding

Read by `EmbeddingConfig::from_env()` ([`crates/embedding/src/config.rs`](../crates/embedding/src/config.rs)).

| Env var (aliases) | `Settings` field | Default |
|---|---|---|
| `EMBEDDING_PROVIDER` | `embedding_provider` | `openai` (`onnx` on Android) |
| `EMBEDDING_MODEL` | `embedding_model_name` | `text-embedding-3-small` (`BGE-Small-v1.5` on Android) |
| `EMBEDDING_DIMENSIONS` | `embedding_dimensions` | `1536` (`384` on Android) |
| `EMBEDDING_ENDPOINT` | `embedding_endpoint` | _(empty)_ |
| `EMBEDDING_API_KEY` (falls back to `LLM_API_KEY`) | `embedding_api_key` | _(empty)_ |
| `EMBEDDING_API_VERSION` | `embedding_api_version` | _(empty)_ |
| `EMBEDDING_MODEL_PATH` / `COGNEE_E2E_EMBED_MODEL_PATH` | `embedding_model_path` | `./target/models/BGE-Small-v1.5-model_quantized.onnx` |
| `EMBEDDING_TOKENIZER_PATH` / `COGNEE_E2E_TOKENIZER_PATH` | `embedding_tokenizer_path` | `./target/models/bge-small-tokenizer.json` |
| `EMBEDDING_MAX_SEQUENCE_LENGTH` | `embedding_max_sequence_length` | `512` |
| `EMBEDDING_BATCH_SIZE` | `embedding_batch_size` | `36` (texts per embedding request; raise for providers that allow larger batches. The OpenAI-compatible engine also dispatches up to 8 sub-batches concurrently) |
| `EMBEDDING_ONNX_BATCH_SIZE` | `embedding_onnx_batch_size` | `32` (ONNX inference batch size; independent of `EMBEDDING_BATCH_SIZE`. Lower it under memory pressure on edge devices) |
| `MOCK_EMBEDDING` | _(provider override)_ | `false` (also accepts `deterministic`) |

Provider values: `onnx`, `fastembed`, `openai`, `openai_compatible`, `ollama`,
`bedrock`, `mock`.

`EMBEDDING_PROVIDER=bedrock` uses Amazon Bedrock's **InvokeModel** API
(`POST {endpoint}/model/{modelId}/invoke`) — Converse is the chat API and is
never used here. Two model families are supported, selected from the normalised
model id: **Titan** (`amazon.titan-embed-text-v1`,
`amazon.titan-embed-text-v2:0`, `amazon.titan-embed-image-v1`), which takes one
text per request, and **Cohere** (`cohere.embed-*`), which batches. Set
`EMBEDDING_MODEL` to the Bedrock model id; `EMBEDDING_DIMENSIONS` drives the
engine's reported `dimension()` and is sent as `dimensions` on Titan v2, so it
must match what the model actually returns. AWS configuration comes from the
same `AWS_*` environment variables as the LLM path
(see [Bedrock](#bedrock-llm_providerbedrock)) — the engine and the chat adapter
share one AWS implementation — and `EMBEDDING_API_KEY` (falling back to
`LLM_API_KEY`) is the Bedrock API key, which short-circuits to a bearer header
with no SigV4; leaving it unset is supported and runs the ambient credential
ladder. Returned vectors are always L2-normalised: Titan v2 normalises
server-side (the engine sends `normalize: true`), and the families that do not
are normalised client-side.

> **Bedrock embeddings and `EMBEDDING_ENDPOINT`:** `EMBEDDING_ENDPOINT` defaults
> to the OpenAI base URL and nothing clears it when the provider changes, so the
> Bedrock engine ignores an endpoint that still points at OpenAI rather than
> POSTing Bedrock bodies there. Set it only to a real Bedrock runtime host (or
> use `AWS_BEDROCK_RUNTIME_ENDPOINT`).

## Vector database

| Env var | `Settings` field | Default |
|---|---|---|
| `VECTOR_DB_PROVIDER` | `vector_db_provider` | `lancedb` (embedded, persistent) on non-Android; falls back to `brute-force` (in-memory) on Android |
| `VECTOR_DB_URL` | `vector_db_url` | _(empty — defaults to `{system_root_directory}/databases/cognee.lancedb`; set to `:memory:` to force the in-memory brute-force store)_ |
| `VECTOR_DB_HOST` / `VECTOR_DB_PORT` | `vector_db_host` / `vector_db_port` | _(empty)_ / `1234` |
| `VECTOR_DB_NAME` / `VECTOR_DB_KEY` | `vector_db_name` / `vector_db_key` | _(empty)_ |
| `VECTOR_DB_USERNAME` / `VECTOR_DB_PASSWORD` | … | _(empty)_ |

Supported providers:
- `lancedb` — embedded Apache-Arrow / Lance vector store, on disk. The default
  provider, and default-on in `cognee`, `cognee-http-server` and every binding;
  requires the `lancedb` Cargo feature (the Arrow/Lance native stack is a large
  build, so a consumer on another vector store can drop it) and is unavailable on
  Android, where its native stack does not cross-compile. The on-disk layout
  matches the Python SDK's default LanceDB store, so a Rust deployment can be
  opened from Python and vice versa.
- `brute-force` — pure-Rust in-memory linear scan. Default on Android (where
  LanceDB's native stack does not cross-compile). Selected on any target by
  setting `vector_db_url = ":memory:"`.
- `pgvector` — Postgres + the `pgvector` extension; requires the `pgvector`
  Cargo feature on the binary build.

Qdrant lives in closed `cognee-cloud-rs` as the `cognee-vector-qdrant` crate
and is not part of OSS. See [tools/backends.md](tools/backends.md).
Setting `vector_db_provider` to `qdrant` is rejected at component
initialization in OSS (it returns a config error rather than falling back).

## Graph database

| Env var | `Settings` field | Default |
|---|---|---|
| `GRAPH_DATABASE_PROVIDER` | `graph_database_provider` | `ladybug` |
| `GRAPH_FILE_PATH` | `graph_file_path` | _(empty; defaults under the system root)_ |
| `GRAPH_DATABASE_URL` | `graph_database_url` | _(empty)_ |
| `GRAPH_DATABASE_HOST` / `GRAPH_DATABASE_PORT` | `graph_database_host` / `graph_database_port` | _(empty)_ / `123` |
| `GRAPH_DATABASE_NAME` / `GRAPH_DATABASE_KEY` | … | _(empty)_ |
| `GRAPH_DATABASE_USERNAME` / `GRAPH_DATABASE_PASSWORD` | … | _(empty)_ |

Supported providers: `ladybug`/`kuzu` (embedded), `postgres` (feature `pggraph`).
When Postgres graph credentials are unset they fall back to the relational `DB_*`
config (see [roadmap/cognify-compatibility-plan.md](roadmap/cognify-compatibility-plan.md)).

## Relational database

| Env var | `Settings` field | Default |
|---|---|---|
| `DATABASE_URL` | `relational_db_url` | `sqlite:./cognee.db?mode=rwc` |
| `DB_PROVIDER` | `db_provider` | `sqlite` |
| `DB_HOST` / `DB_PORT` | `db_host` / `db_port` | `localhost` / `5432` |
| `DB_NAME` | `db_name` | `cognee_db` |
| `DB_USERNAME` / `DB_PASSWORD` | … | _(empty)_ |

## Chunking & tokenizer

Read by [`crates/chunking/src/config.rs`](../crates/chunking/src/config.rs). Most
chunking knobs (`chunk_strategy` default `PARAGRAPH`, `chunk_size` `1500`,
`chunk_overlap` `10`, `chunk_engine`) are `Settings`/`CognifyConfig` fields without
env bindings. The token counter is env-selected:

| Env var | Purpose | Default |
|---|---|---|
| `COGNEE_TOKEN_COUNTER` | `tiktoken` / `word` / `huggingface`(`hf`) | auto from embedding provider |
| `HUGGINGFACE_TOKENIZER` | model id when counter = `huggingface` | _(empty)_ |

## Search — hybrid retriever knobs

The `HYBRID_COMPLETION` search type (`SearchType::HybridCompletion`) accepts a
set of per-request tuning knobs. Unlike the env-var knobs elsewhere on this
page, these are **library / orchestrator-only** — they are typed
`Option<...>` fields on
[`SearchParams`](../crates/search/src/types/search_params.rs), populated in
`impl From<&SearchRequest>` from the **snake_case JSON keys** inside
`SearchRequest.retriever_specific_config`. They have no env binding.

**Phase-1 reachability gap.** Every external adapter currently hardcodes
`retriever_specific_config: None` (`http-server/src/routers/search.rs`,
`http-server/src/responses_dispatch.rs`, `cli/src/commands/search.rs`,
`bindings-common/src/ops/retrieval.rs`), so in Phase 1 these knobs are reachable
**only by a direct Rust library caller** that constructs a `SearchRequest` with
a populated `retriever_specific_config`. They are **not** wire fields on
`SearchPayloadDTO` and are not exposed over HTTP, CLI, or the language bindings
— the same class as `neighborhood_depth` and the other orchestrator-only
`SearchParams` knobs (see the wire-passthrough open question in
[`http-server/routers/search.md`](http-server/routers/search.md)).

When a knob is unset the retriever falls back through a three-layer resolution:
`retriever_specific_config.<knob>` → `SearchRequest.top_k` → the
`HybridRetriever::new` constructor default. The effective defaults are:

| `SearchParams` field | `retriever_specific_config` JSON key | Effective Rust default | Python parity |
|---|---|---|---|
| `chunks_top_k` | `chunks_top_k` | `top_k`, else `15` | `chunks_top_k` (derives from `top_k`) |
| `entities_top_k` | `entities_top_k` | `top_k`, else `15` | `entities_top_k` |
| `facts_top_k` | `facts_top_k` | `top_k`, else `15` | `facts_top_k` |
| `max_edges_per_entity` | `max_edges_per_entity` | `10` | `max_edges_per_entity=10` |
| `text_summaries_top_k` | `text_summaries_top_k` | `None` (no fallback) | `text_summaries_top_k=None` |
| `use_importance_weight` | `use_importance_weight` | `true` | `use_importance_weight=True` |
| `node_name` | `node_name` | `None` | reused query node filter |
| `node_name_filter_operator` | `node_name_filter_operator` | `"OR"` | `"OR"` / `"AND"` |

(NB: the `chunks_top_k` / `entities_top_k` / `facts_top_k` code comments in
`search_params.rs` still say `unwrap_or(10)` — that is a stale comment; the
constructor uses `DEFAULT_TOP_K = 15`.)

**Reserved (Phase 2, inert).** These keys are parsed and stored but have no
effect in Phase 1 — passing `true` must behave identically to the default:

- `use_truth_weight` (default `false`) — truth-subspace weighting is Phase 2
  only; the Python `build_truth_subspace` path runs on `improve()`, not
  implemented on the Rust side yet.
- `include_global_context_index` (default `false`) and
  `global_context_index_top_k` (default `3`) — the global-context lane depends
  on a `GlobalContextSummary` node type and global-context utilities not yet
  ported to Rust.

See [`http-server/routers/search.md` §7 Known limitations](http-server/routers/search.md)
for the full degradation notes.

## Ontology

| Env var | `Settings` field | Default |
|---|---|---|
| `ONTOLOGY_FILE_PATH` | `ontology_file_path` | _(empty)_ |
| `ONTOLOGY_RESOLVER` | `ontology_resolver` | `rdflib` |
| `ONTOLOGY_MATCHING_STRATEGY` | `ontology_matching_strategy` | `fuzzy` |

## System paths, users & datasets

| Env var | `Settings` field | Default |
|---|---|---|
| `COGNEE_SYSTEM_ROOT_DIRECTORY` | `system_root_directory` | `./.cognee_system` |
| `COGNEE_DATA_ROOT_DIRECTORY` | `data_root_directory` | `./.data_storage` |
| `CACHE_ROOT_DIRECTORY` | `cache_root_directory` | `./.cognee_cache` |
| `COGNEE_DEFAULT_USER_ID` | `default_user_id` | nil UUID |
| `COGNEE_DEFAULT_DATASET_NAME` | `default_dataset_name` | `main_dataset` |
| `DEFAULT_USER_EMAIL` / `DEFAULT_USER_PASSWORD` | … | `default_user@example.com` / _(empty)_ |
| `ENABLE_BACKEND_ACCESS_CONTROL` | `enable_access_control` | `false` |

Setting `system_root_directory` cascades to the default `graph_file_path` and
`vector_db_url` unless those are set explicitly.

## Session / cache & rate limiting

| Env var | `Settings` field | Default |
|---|---|---|
| `CACHE_BACKEND` | `cache_backend` | `fs` |
| `CACHE_HOST` / `CACHE_PORT` | … | `localhost` / `6379` |
| `SESSION_TTL_SECONDS` | `session_ttl_seconds` | `604800` (7d) |
| `CACHING` | `enable_caching` | `true` |
| `LLM_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | … | `false` / `60` / `60` |
| `EMBEDDING_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | … | `false` / `60` / `60` |

## Logging

> **Canonical table.** Binding READMEs and `.env.example` link here. cognee writes
> structured logs to **stdout** and (when writable) to a rotating file. File logging
> is owned by [`cognee-logging`](../crates/logging/), initialised by the CLI and HTTP
> server via `cognee_logging::init_logging`.

| Env var | Default | Purpose |
|---|---|---|
| `COGNEE_LOG_FILE` | `true` | Master file-logging toggle (`false`/`0`/`no` disables). |
| `COGNEE_LOGS_DIR` | `~/.cognee/logs` | Log directory (falls back to `/tmp/cognee_logs` if unwritable). |
| `COGNEE_LOG_FORMAT` | `plain` | `plain` (Python-compatible) or `json`. Applies to stdout + file. |
| `COGNEE_LOG_ROTATION` | `daily` | `daily` / `hourly` / `minutely` / `never`. |
| `COGNEE_LOG_BACKUP_COUNT` | `5` | Rotated files retained by age. |
| `COGNEE_LOG_MAX_FILES` | `10` | Hard cap on retained log files. |
| `LOG_FILE_NAME` | _(timestamped)_ | Override the log file name. |
| `RUST_LOG` / `LOG_LEVEL` | `info` | Level filter (`RUST_LOG` preferred). |

> **Multi-process warning** — when several cognee processes share one log file via
> `LOG_FILE_NAME`, rotation is not coordinated; concurrent rotation can corrupt the
> log. For sharded workers, give each shard its own `COGNEE_LOGS_DIR` (or unset
> `LOG_FILE_NAME` per shard).

## Observability & telemetry

cognee emits OpenTelemetry traces (behind the `telemetry` feature) and opt-out
product analytics. The **deep references** are
[observability/opentelemetry.md](observability/opentelemetry.md) and
[observability/send_telemetry.md](observability/send_telemetry.md); the env surface:

| Env var | Default | Purpose |
|---|---|---|
| `COGNEE_TRACING_ENABLED` | `false` | Activate OTLP trace export. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | _(empty)_ | OTLP collector endpoint (non-empty also activates tracing). |
| `OTEL_SERVICE_NAME` | `cognee` | Service name attribute. |
| `OTEL_EXPORTER_OTLP_HEADERS` / `_PROTOCOL` | _(empty)_ / `grpc` | Exporter headers / protocol. |
| `OTEL_SPAN_PROCESSOR` | `batch` | `batch` or `simple`. |
| `OTEL_TRACES_SAMPLER` / `_ARG` | _(empty)_ | Sampler selection. |
| `TELEMETRY_DISABLED`, `ENV=test\|dev` | _(unset)_ | Opt out of product analytics. |

## HTTP server

The server binary reads its own env surface ([`crates/http-server/src/config.rs`](../crates/http-server/src/config.rs)) —
host/port, auth, body limits, pipeline registry, notebooks, health probes. See
[tools/http-server.md](tools/http-server.md) and
[http-server/architecture.md §config](http-server/architecture.md). Note that
the `HYBRID_COMPLETION` tuning knobs are **not** part of the HTTP wire surface
in Phase 1 — see [Search — hybrid retriever knobs](#search--hybrid-retriever-knobs)
and [`http-server/routers/search.md`](http-server/routers/search.md).

## Cloud

Cloud/Auth0 configuration (`COGNEE_CLOUD_URL`, `COGNEE_AUTH0_*`) and the
`serve()`/`disconnect()` flow live in the closed `cognee-cloud-rs` product
(the `cognee-cloud` crate) and are not part of OSS.

## Runtime configuration API

`ConfigManager` (`Arc<RwLock<Settings>>`) exposes typed setters used by the
bindings and CLI. Families: `set_llm_*`, `set_embedding_*`, `set_vector_db_*`,
`set_graph_*`, `set_chunk_*`, `set_relational_db_*`, `set_*_root_directory`,
`set_ontology_*`, `set_classification_model` / `set_summarization_model` /
`set_summarization_schema`, plus four bulk setters (`set_llm_config`,
`set_embedding_config`, `set_vector_db_config`, `set_graph_db_config`) and a
generic `set(key, value)`. Introspection: `read()`, `version()`, `get_settings()`
(secrets masked). Full signatures are in the
[`ConfigManager` rustdoc](../crates/lib/src/config.rs). The binding ergonomics
(granular JS setters vs generic `set` in Python/C) are documented in
[tools/bindings.md §config](tools/bindings.md#configuration).

## CLI `config` subcommand

`cognee-cli config get|set|unset <key>` reads/writes the persisted JSON file.
The settable keys are the snake_case `Settings` field names — see
[`known_keys()`](../crates/cli/src/config_store.rs). Example:

```bash
cognee-cli config set llm_max_retries 4
cognee-cli config get llm_model
cognee-cli config unset embedding_endpoint
```
