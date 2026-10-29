# Configuration

Canonical reference for configuring cognee-rust. The complete, field-level source
of truth is the [`Settings`](../crates/lib/src/config.rs) struct and the
[`ConfigManager`](../crates/lib/src/config.rs) runtime API — build the rustdoc with
`cargo doc -p cognee --no-deps --open` to browse every field and setter with
its type. This page groups those fields by subsystem and gives the env-var name
and default for each.

## Configuration model

**The rule.** Every behaviour knob is a field on a typed config struct. The
environment is a source of *defaults* for those fields, never the only way to set
them: anything an env var can set must also be settable from code (the Rust API,
every language binding, and the CLI). Env vars are read only at a
**configuration boundary**, the one place that turns the environment into a
config struct. Everything below that boundary (adapters, retrievers, pipelines)
receives resolved values through constructors and context structs, and never
reads the process environment itself.

### Precedence

Lowest precedence first:

1. **Defaults:** `Settings::default()` in [`crates/lib/src/config.rs`](../crates/lib/src/config.rs).
2. **Persisted config file** *(CLI only)*: JSON at `~/.config/cognee-rust/config.json`
   (`$XDG_CONFIG_HOME/cognee-rust/config.json`), managed by `cognee-cli config`.
   See [`crates/cli/src/config_store.rs`](../crates/cli/src/config_store.rs).
3. **`.env` file:** loaded into the process environment by
   `Settings::overlay_from_env()` (via `cognee_config_core::load_dotenv`). The file
   is found in the working directory or any ancestor. Variables already set in the
   process environment win over `.env`.
4. **Process environment:** bound by `Settings::overlay_from_env()`.
5. **Explicit code:** `ConfigManager` setters (below), a binding's constructor
   settings object, the binding config APIs, and CLI `--flags`.

So: `defaults < config.json < .env < env < code`.

Parsing notes:
* Booleans accept `true|1|yes|on` and `false|0|no` (`cognee_utils::parse_env_bool`).
* Empty env values are treated as unset.
* Numeric vars that fail to parse are ignored, and the previous value (usually
  the default) stays in effect. A value outside the field's integer type counts
  as a parse failure, so a port of `70000`, `-1` or `5000000000` keeps the
  default. Any in-range value is accepted as-is, `0` included. Every port key
  (`VECTOR_DB_PORT`, `GRAPH_DATABASE_PORT`, `DB_PORT`, `CACHE_PORT`) behaves this
  way, as does every other integer key; the config goldens pin it.

### Two scopes

| Scope | Config struct | Built by |
|---|---|---|
| **Instance** | `Settings`, behind `ConfigManager` | each SDK handle / CLI run, from `Settings::load_from_env()` or `Settings::overlay_from(&dyn EnvSource)` |
| **Process** | `LoggingConfig`, the observability settings (`EnvSettingsView`), product analytics (`TelemetryConfig`) | once per process, because each configures a process-wide singleton (the `tracing` subscriber, the OTLP exporter, the analytics dispatcher). Env is the default; a programmatic call (`init_logging`, a binding's `setup_*` options, `cognee::telemetry::set_enabled` / `configure`) wins. See [Logging](#logging) and [Observability & telemetry](#observability--telemetry). |

### Component config sections

Settings that belong to one component are declared **by that component**, not in
`Settings`. A section is a typed struct declared with
[`cognee_config_core::config_section!`](../crates/config-core/src/section.rs). Each
field becomes a setting with:

* a canonical, namespaced key: `"<section>.<field>"`, e.g. `graph.ladybug.max_db_size`;
* its env vars, in priority order;
* optional plain aliases, typically the Python SDK's flat name, e.g. `graph_max_db_size`;
* a secret flag.

The component's factory returns the section from `config_sections()`. **Registering
the factory registers its settings.** `ComponentRegistry::config_schema()` collects
the sections of every registered factory, plus any added with
`register_config_section` (for settings that no factory owns, e.g. a server
module's).

So the settings a build accepts follow the components it contains:

* a factory compiled out by a disabled feature brings no keys: its env vars are not
  read, and setting its keys is an unknown-key condition;
* a downstream build that registers its own factory (`register_llm`,
  `register_vector`, `register_storage`, `register_ontology_resolver`,
  `register_graph_extractor`, `register_relational`, `register_session_store`, …) brings that factory's keys, settable through the same
  `ConfigManager::set`, binding `set` and config files as the built-in ones.

`ComponentManager::with_registry` adds the registry's sections to its
`ConfigManager`. `cognee::config::builtin_schema()` is the schema of a default
build. A process built on a custom registry must load the environment with the
**full** schema, so the env vars of the registry's own sections are read too:

* `ConfigManager::from_env_with_schema(&registry.config_schema()?)`;
* `HandleState::from_env_with_registry(registry)` in the bindings;
* `Settings::load_from_env_with_schema(&cognee::config::schema_with(…)?)`, or the CLI's
  `load_settings_with_schema`.

Adding the sections afterwards makes their keys settable, but cannot read their
env vars.

Section values travel inside `Settings`, in its flattened `sections` map, so
config files and binding settings objects carry them unchanged. A factory reads
its typed section from the build context with `ctx.section::<T>()`. A section key,
its canonical key and its aliases are all accepted by `ConfigManager::set` /
`get`; `set` and `unset` also clear any alias spelling of the setting. When a
loaded config carries one setting under both its canonical key and an alias, the
canonical value wins and the alias is dropped with a warning, so a stale flat key
(`llm_api_key` from an older config file) never overrides what an env var, a CLI
flag or `set` wrote under `llm.api_key`. `cognee-cli` rewrites those flat keys to
canonical ones when it loads a pre-v4 config file. The TS and Java constructors
also accept a section key with its dots flattened to underscores
(`llm_debug_requests`), so the camelCase keys a TS `config.get()` returns can be
passed back to `new Cognee(...)`: a section key with a Python alias reads back
under the alias's camelCase (`llm.auto_rate_limit` -> `autoRateLimit`), one
without an alias with its dots as word breaks (`llm.debug_requests` ->
`llmDebugRequests`). Two sections
claiming the same key, alias or primary env var is a registration error. So is a
section claiming a `Settings` key.

### Unknown keys and strict mode

A key can be unknown because it is misspelled, or because the component that owns
it is not compiled into, or registered with, this build. The two look the same, so
the policy is a setting:

| `strict_config` / `COGNEE_STRICT_CONFIG` | Unknown key in `set` / `cognee-cli config set` | Unknown key in a loaded config (config file, binding settings object) |
|---|---|---|
| `false` (default) | warning; nothing changes | warning; the entry is **kept**, so a config written by a build with more components survives |
| `true` | `UnknownKey` error | error at first use: the CLI command, or the handle's first operation |

`cognee-cli config unset <key>` removes a stale entry even in strict mode, so a
config can always be repaired. `cognee-cli config set` / `unset` see strict mode
the way every other command does: the saved `strict_config` with
`COGNEE_STRICT_CONFIG` overlaid.

A value of the wrong type for a known key is always an error. From `set` it fails
the call; in a loaded config it is dropped (the setting keeps its default) and the
error is raised at first use, as for an unknown key in strict mode, until the key
is set again. A loaded config may also carry `Settings` keys under a name serde
does not know (the in-memory `llm_args` / `summarization_schema`, or an alias such
as `embedding_model`); those are applied as the setting they name, not treated as
unknown.

A setting marked `secret` (a registry row or a section key) is masked as
`***REDACTED***` (`cognee_config_core::REDACTED`) in every binding's
`config.get()` output when it is set. An unset secret (`null`, or `""` for a
string-typed one) is shown as itself: its emptiness reveals nothing, and the
marker would claim it is set (`cognee_config_core::secret_is_set` /
`mask_secret`).

`Debug` output follows the same rule, so `{:?}` of a configuration does not
print credentials:

* a `config_section!` struct prints a set secret field as `***REDACTED***` and an
  unset one as itself (`None`, `""`). Every other field prints its own `Debug`
  with embedded credentials masked (`cognee_config_core::redact_credentials`):
  the password of a URL (`postgres://cognee:***REDACTED***@db/cognee`) and the
  value of a password-like `key=value` pair (a query parameter such as
  `sslpassword=`, or a libpq keyword DSN such as `host=db password=…`). This
  covers the connection URLs (`relational.url` / `DATABASE_URL`,
  `relational.migration_url`, `vector.url`, `graph.url`), which are not marked
  `secret` because they are mostly not credentials;
* `Settings` prints every setting as JSON with the same masking. A `sections`
  entry no built-in section owns (a custom registry's, or a stale one) is
  masked when it is a non-empty string, because only its owner knows whether it
  is a credential. Formatting has no side effects: it consults the built-in
  schema only once something else has built it;
* `LlmConfig` and `EmbeddingConfig` mask their API keys and their endpoint's
  credentials; `AwsInputs` / `AwsSettings` mask their credentials;
* `cognee_observability::EnvSettingsView` (the process-level OTEL settings every
  binding builds) masks its OTLP headers, like `otel_exporter_otlp_headers` in
  `Settings`, and its endpoint's credentials.

The masking is best effort for the shapes above; a credential in a free-form
value of some other shape is not recognised.

### How the rule is enforced

* The only sanctioned read of the process environment is
  [`cognee_config_core::ProcessEnv`](../crates/config-core/src/lib.rs). The
  boundary loaders read through an `EnvSource`, so tests can pass a `MapEnv`
  instead of mutating the shared process environment:
  * `Settings::overlay_from(&dyn EnvSource)`
  * `LoggingConfig::from_env_source(&dyn EnvSource)`
  * `EnvSettingsView::from_env_source(&dyn EnvSource)` (and `overlay`)
  * `TelemetryConfig::from_env_source(&dyn EnvSource)`

  The `from_env()` / `overlay_from_env()` entry points are thin boundaries over them.
* The root [`clippy.toml`](../clippy.toml) bans the following:
  * `std::env::{var, var_os, vars, vars_os, set_var, remove_var}`
  * `dotenv::*`
  * `cognee_config_core::ProcessEnv::new`

  A configuration boundary opens the process environment explicitly, under
  `#[allow(clippy::disallowed_methods, reason = "config boundary: …")]`. That keeps
  every boundary visible and easy to grep.
* Three `reason` prefixes are accepted on that `allow` in production code:
  * `config boundary: …` turns the environment into a config struct.
  * `env-only by design: …` covers a conventional variable whose whole point is to
    be ambient: proxy variables, `HOME`, the AWS default credential chain,
    `COGNEE_CONFIG_HOME` (it locates the config file itself),
    `COGNEE_BINDING_SUPPRESS_LOGS` (read at module load, before user code runs) and
    `LOG_FILE_NAME` (hands the log file to child processes).
  * `config-unification: legacy direct env read, …` marks code that predates the
    rule and is being migrated behind a config boundary. **New code must never add
    one.**
* An `allow` can hide more than one read, so the reads themselves are counted.
  [`scripts/check_env_boundary.sh`](../scripts/check_env_boundary.sh) counts every
  direct-access call site in production code, per file. Test modules, `tests/`,
  `examples/` and `crates/test-utils` are excluded. It compares the counts with
  [`scripts/env_boundary_baseline.txt`](../scripts/env_boundary_baseline.txt) and
  fails if any file's count goes **up**, with any `allow` reason. It also fails if
  a count goes **down**, so the lower number gets locked in. Run
  `scripts/check_env_boundary.sh --update` to rewrite the baseline. A genuinely new
  config boundary is therefore always an explicit, reviewed baseline change. The
  check runs in `scripts/check_all.sh` and CI, and covers the `capi/`, Python,
  TypeScript and Java binding crates too.
* Tests may drive configuration through the process environment, under a
  file-level or module-level
  `allow(clippy::disallowed_methods, reason = "tests drive configuration through the process env")`.
  Prefer a `MapEnv` where the code under test accepts an `EnvSource`. Example
  binaries carry their own `example binary: …` reason.

### Adding a setting

1. **Pick the owner.**
   * A setting **one component reads** (an adapter or backend option) goes in that
     component's **config section**:
     ```rust
     cognee_config_core::config_section! {
         /// What the section configures.
         pub struct MyAdapterConfig in "llm.my_adapter" {
             /// What the setting does.
             pub timeout_seconds: u32 = 30 => {
                 env: ["MY_ADAPTER_TIMEOUT_SECONDS"],
                 alias: ["my_adapter_timeout_seconds"],
                 secret: false,
             },
         }
     }
     ```
     Return `MyAdapterConfig::schema()` from the factory's `config_sections()`,
     and read it in `build` with `ctx.section::<MyAdapterConfig>()`. The setting
     then exists exactly when the factory is registered.
   * A setting **shared across components or cognee-wide** (paths, the LLM
     provider and model, cognify behaviour) is a field on `Settings` with its
     default, plus **one row in the key registry,
     [`crates/lib/src/config/keys.rs`](../crates/lib/src/config/keys.rs)**. For
     example: `row!(my_setting: u32, env ["COGNEE_MY_SETTING"])`. The row names
     the parse rules (`str`, `u16`, `u32`, `u64`, `bool`, `opt_u32`,
     `opt_f64`), the env vars in priority order, any key aliases, and `secret`
     for credentials.
2. Either way the setting is now:
   * bound from the environment;
   * settable from code by `ConfigManager::set`, and so by every binding's
     generic `set`;
   * listed, shown, set and unset by `cognee-cli config`.

   Keep the Python SDK's env-var name when the setting exists there, and give a
   namespaced section key the Python flat name as an alias. Note any deliberate
   divergence in this document. A process-scoped struct reads its env in its own
   `from_env_source(&dyn EnvSource)` instead.
3. Add a typed `ConfigManager` setter and binding sugar only if the setting is
   commonly used. The generic `set` already reaches it.
4. Pass the resolved value down to the component that needs it, through
   `Settings::backend_context()` (`BackendBuildContext` / `LlmInputs` /
   `EmbeddingInputs`) or the component's constructor or builder. Never read the
   environment inside the component.
5. Test it by setting the value **through `ConfigManager`** (not the environment)
   and asserting the effect. Add the env var to `ENV_VARS` in
   `crates/lib/tests/config_key_golden.rs` and regenerate the golden snapshots
   (`UPDATE_GOLDEN=1`); the env binding is then covered there.
6. Document it here: the env var, aliases, `Settings` key and default.

## LLM

The `llm` section, registered by the LLM kind and shared by every LLM adapter
(see [Component config sections](#component-config-sections)). Each key also
answers to its Python flat name, the alias shown in brackets. The deep reference is
[tools/cli.md](tools/cli.md#llm-retries) for retries and the `cognee-llm` rustdoc
for the adapter.

| Env var (aliases) | Key (alias) | Default |
|---|---|---|
| `LLM_PROVIDER` | `llm.provider` (`llm_provider`) | `openai` |
| `LLM_MODEL` / `OPENAI_MODEL` | `llm.model` (`llm_model`) | `openai/gpt-5-mini` |
| `LLM_API_KEY` / `OPENAI_TOKEN` | `llm.api_key` (`llm_api_key`) | _(empty)_ |
| `LLM_ENDPOINT` / `OPENAI_URL` | `llm.endpoint` (`llm_endpoint`) | _(empty)_ |
| `LLM_API_VERSION` | `llm.api_version` (`llm_api_version`) | _(empty)_ |
| `LLM_REASONING` | `llm.reasoning` (`llm_reasoning`) | `auto` |
| `LLM_STRUCTURED_OUTPUT_MODE` | `llm.structured_output_mode` (`llm_structured_output_mode`) | `auto` |
| `LLM_TEMPERATURE` | `llm.temperature` (`llm_temperature`) | *unset — no `temperature` is sent* |
| `LLM_STREAMING` | `llm.streaming` (`llm_streaming`) | `false` (accepted for Python parity; no effect, see below) |
| `LLM_MAX_COMPLETION_TOKENS` / `LLM_MAX_TOKENS` | `llm.max_completion_tokens` (`llm_max_completion_tokens`) | `16384` |
| `LLM_ARGS` | `llm.args` (`llm_args`) | _(empty)_ |
| `LLM_MAX_RETRIES` | `llm.max_retries` (`llm_max_retries`) | `3` |
| `LLM_NETWORK_RETRIES` | `llm.network_retries` (`llm_network_retries`) | `2` |
| `LLM_MIN_RETRY_SECONDS` | `llm.min_retry_seconds` (`llm_min_retry_seconds`) | `240` |
| `LLM_MAX_PARALLEL_REQUESTS` | `llm_max_parallel_requests` (a `Settings` field) | `1000` (`128` on Android/iOS) |
| `LLM_REQUEST_TIMEOUT_SECONDS` | `llm.request_timeout_seconds` (`llm_request_timeout_seconds`) | `600` |
| `LLM_CONNECT_TIMEOUT_SECONDS` | `llm.connect_timeout_seconds` (`llm_connect_timeout_seconds`) | `10` |
| `LLM_REQUEST_DEADLINE_SECONDS` | `llm.request_deadline_seconds` (`llm_request_deadline_seconds`) | `1200` (`0` disables) |
| `MOCK_LLM` | `llm.mock` (`llm_mock`) | `false` |
| `MOCK_LLM_CASSETTE` | `llm.cassette` (`llm_cassette`) | _(empty)_ |
| `COGNEE_RECORD_LLM` | `llm.record_path` (`llm_record_path`) | _(empty)_ |
| `LLM_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | `llm.rate_limit_enabled` / `_requests` / `_interval` (`llm_rate_limit_*`) | `false` / `60` / `60` |
| `AUTO_RATE_LIMIT` | `llm.auto_rate_limit` (`auto_rate_limit`) | `true` |
| `COGNEE_DEBUG_LLM_REQUEST` | `llm.debug_requests` | `false` (dump every request/response body to stderr) |

Adapter sections, registered by their factory and present only when it is:

| Env var (aliases) | Key (alias) | Default |
|---|---|---|
| `LLM_VISION_MODEL` | `llm.openai_compatible.vision_model` (`llm_vision_model`) | _(unset: uses `llm.model`)_ |
| `TRANSCRIPTION_MODEL` | `llm.openai_compatible.transcription_model` (`transcription_model`) | `whisper-1` |
| `ANTHROPIC_BASE_URL` / `ANTHROPIC_API_BASE` | `llm.anthropic.base_url` (`anthropic_base_url`) | _(unset: public API)_ |
| `AWS_*` (see [Bedrock](#bedrock-llm_providerbedrock)) | `aws.*` (`aws_*`), shared by the Bedrock LLM and embedding adapters (`bedrock` feature) | _(unset)_ |

`LLM_MAX_COMPLETION_TOKENS` is a *completion*-token budget, and it does double
duty: besides capping the option-less `generate` call, it is the LLM term in
cognify's chunk-size auto-calculation
(`min(embedding_max_sequence_length, llm_max_completion_tokens / 2)` — Python's
`get_max_chunk_tokens()`). Lowering it for a small-context endpoint therefore
shrinks the prompts cognify sends. On adapters that know their model's documented
output cap (Anthropic, Bedrock) the configured value is clamped to it first, as
Python does via litellm's `model_cost`.

> **Note:** structured-output calls do **not** apply this ceiling — option-less
> ones send the built-in `16384` default. Cognify's extraction calls send no cap
> at all (Python parity). See TOP-8 for the open policy question.

`LLM_ARGS` is the escape hatch for that last case. It takes a JSON **object**
whose keys are merged into each chat and structured-output request body — the
port of Python's `llm_config.llm_args` and its `{**self.llm_args, **kwargs}`
merge, so an explicitly-passed option always wins over an `LLM_ARGS` key of the
same name. Its practical use is giving the extraction path more output headroom:
because those calls send no `max_tokens`, the provider's own default applies, and
a low one (Baseten's is 4096) truncates the structured JSON mid-object. The
adapter detects that case (`finish_reason: "length"`) and re-asks at a raised
budget rather than cascading through its other modes, so the failure is reported
as a truncation — but starting from a sufficient budget avoids the wasted first
attempt altogether.

```bash
LLM_ARGS='{"max_tokens": 16384}'
```

Three details worth knowing. A bare `max_tokens` is folded into
`max_completion_tokens` for reasoning models, and sampling params those models
reject (`temperature`, `top_p`, `frequency_penalty`, `presence_penalty`) are
dropped rather than passed through. A malformed value, or valid JSON that is not
an object, is ignored with a `warn!` instead of aborting startup — matching the
lenient handling of the other optional LLM knobs. And two paths do not see it at
all: the **HTTP server has no `LLM_ARGS` equivalent** (it defaults `llm_args` to
an empty map, so a server deployment cannot raise a cap this way), and the
vision/transcription path bypasses the merge.

`LLM_TEMPERATURE` reaches every built-in provider (until SDK-1070 only Bedrock
applied it). The OpenAI-compatible, Azure and Anthropic factories fold it into
`LLM_ARGS` as `temperature`, as Python's `fold_sampling_params_into_llm_args`
does, so a `temperature` in `LLM_ARGS` itself wins. It is sent on every call
that does not pick a temperature itself: requests whose options carry none
(graph extraction) and option-less calls, plain or structured (search
completions, feedback detection), which otherwise send `0.0`. A temperature a
call picks (summarization's `0.3`) wins, and reasoning models, which reject it,
never receive one. Unset, these providers
send a `temperature` from `LLM_ARGS` if it has one: on option-less calls too,
where until SDK-1070 the built-in `0.0` shadowed it. Otherwise extraction sends
none and option-less calls send `0.0`, as before. Unlike Python, Rust does not
pin `0.0` for local inference servers when the variable is unset.

`LLM_STREAMING` has no effect. Python's `llm_streaming` is likewise unread by
its OpenAI and Azure adapters (its own `LLMConfig` says so, beside
`llm_answer_streaming`), and no Rust adapter streams: the `Llm` trait has no
token-streaming call for a stream to reach.

A fallback LLM (`llm.fallback_provider/_model/_endpoint/_api_key`, aliases
`llm_fallback_*`) is configurable programmatically (no env binding) but not used
yet (SDK-1070). `MOCK_LLM` + cassettes power the offline
benchmark — see [performance/mock-benchmark.md](performance/mock-benchmark.md).

### Supported `LLM_PROVIDER` values

Several providers are OpenAI-compatible HTTP endpoints, so they route through the
same adapter — differing only in base URL and litellm-style model-prefix stripping.
`LLM_API_KEY` is required for every provider **except `bedrock`** (matching the
Python SDK, whose `_API_KEY_REQUIRED_PROVIDERS` omits Bedrock and lists it under
`_NO_API_KEY_PROVIDERS` instead; for a local Ollama any non-empty value works).
On `bedrock` an unset or empty `LLM_API_KEY` is a supported configuration — it
falls through to SigV4 signing rather than erroring (see
[Bedrock](#bedrock-llm_providerbedrock) below). A missing required key does not
fail setup: the LLM is unavailable (`ComponentManager::llm_available()` returns
`false`), and each call to it fails with "LLM API key is not set.", as in Python.
An external provider that needs no key says so by overriding
`LlmFactory::requires_api_key`. The OpenAI-only request quirks
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

### Structured-output request shape (`LLM_STRUCTURED_OUTPUT_MODE`)

Nothing in the OpenAI-compatible protocol advertises which structured-output
shape a server understands, so by default the adapter cascades: native `tools`,
then legacy `functions`, then `response_format: {"type": "json_object"}`. The
first shape that yields a parseable payload wins.

That cascade costs nothing on an endpoint that answers the first shape, and a
great deal on one that answers none of them. A vLLM deployment started without
`--enable-auto-tool-choice --tool-call-parser` returns no tool call at all: every
call then pays all three modes. The adapter keeps a per-mode miss counter and
stops sending a mode that has produced nothing three calls running (re-probing
occasionally, so a redeploy that gains a parser recovers on its own) — but that
still spends the threshold in every fresh process.

`LLM_STRUCTURED_OUTPUT_MODE` skips the discovery when you already know the
answer:

- `auto` (default) — cascade as above, bounded by the miss counter.
- `json_schema` — try constrained decoding first, then cascade. See below.
- `tools` — only native tool-calling.
- `functions` — only the legacy `functions`/`function_call` pair.
- `json` — only JSON mode.

Pick `json` when the server **rejects** `tools`/`functions` outright. If it
merely *ignores* them — accepting the request and echoing JSON in `content` —
leave it on `auto`: tool-calling mode already succeeds there in one request, and
it is the only mode that puts the real JSON schema on the wire. JSON mode sends a
prose example derived from the schema instead, so pinning it trades one wasted
request for permanently weaker schema pressure, which shows up as more missing
fields and more corrective retries.

Pinning a mode means the other two are **never sent**, and exhausting the pinned
mode fails the call rather than falling through — the error names the pin, so a
mode the endpoint cannot actually speak is diagnosable rather than silent. An
unrecognised value falls back to `auto`.

**Python's `LLM_INSTRUCTOR_MODE` is not an alias for this.** It covers similar
ground but does not mean the same thing, so Rust does not read it. If a shared
`.env` sets `LLM_INSTRUCTOR_MODE`, translate it to `LLM_STRUCTURED_OUTPUT_MODE`
yourself. The two differ in four ways:

- **When it applies.** Python reads `llm_instructor_mode` only on its
  `instructor` structured-output framework (`STRUCTURED_OUTPUT_FRAMEWORK=instructor`).
  The default framework, `litellm_native`, ignores it. Aliasing it would make a
  value that does nothing in Python change behaviour in Rust.
- **Values.** Python takes an `instructor.Mode` value: `json_schema_mode`,
  `tool_call`, `tools_strict`, `markdown_json_mode`, `anthropic_tools`,
  `mistral_tools` and so on, many of them specific to one provider. Only
  `json_mode` and `function_call` are also spelled the same in Rust; anything
  else falls back to `auto` here.
- **Empty value.** Empty means the provider's entry in Python's static table
  (`instructor_modes.py`, for example `json_schema_mode` for OpenAI). In Rust,
  `auto` means the cascade.
- **Scope and pinning.** Python's mode applies to every adapter, and
  `json_schema_mode` is a pin. Rust's knob applies only to the OpenAI-compatible
  adapter, and `json_schema` is not a pin (see below).

Rough translation: `json_schema_mode` → `json_schema`; `tool_call` /
`tools_strict` / `parallel_tool_call` → `tools`; `function_call` → `functions`;
`json_mode` / `markdown_json_mode` → `json`.

#### `json_schema` — constrained decoding

`json_schema` is the odd one out: it is **not a pin**. It sends

```json
"response_format": {"type": "json_schema",
                    "json_schema": {"name": "extract_structured_data",
                                    "strict": true, "schema": {…}}}
```

ahead of the cascade, and falls back to it when the endpoint says no. On a
backend that supports it this is *constrained* decoding — the sampler cannot
emit a token that would break the schema — rather than the schema pressure the
other three modes apply through the prompt and a tool definition. Measured on
one runaway-prone chunk over an OpenAI chat-completions endpoint, the
unconstrained `tools` arms dropped a required `target_node_id` in 8 of 11
payloads while the strict arms were 5/5 clean.

Why it is opt-in rather than part of `auto`: Python requests the same shape on
its default path, but only for a model litellm's table advertises as
`supports_response_schema`. There is no such table for an arbitrary
OpenAI-compatible base URL — anything can sit behind `LLM_ENDPOINT` — and one of
the backends cognee is deployed against (Baseten's `gpt-oss-120b`) answers
**HTTP 501** to a constrained request. So you supply the knowledge the table
would have.

Setting it is safe on a mixed fleet even so, because the mode demotes rather than
failing:

1. `strict: true` with the all-required / `additionalProperties: false` schema;
2. on rejection, the same envelope without `strict`, carrying the ordinary
   schema — for gateways that reject the keyword but handle the response format
   (older Azure api-versions, some vLLM builds);
3. on rejection again, out of the mode entirely: the `tools` → `functions` →
   `json` cascade runs exactly as under `auto`.

Only an outright refusal of the request *shape* moves that ladder — HTTP 501, or
an HTTP 400 whose body names `response_format` / `json_schema` / `schema` /
`strict`. A rate limit, any **other** 5xx, a network error, a model refusal, a
bad answer, and a 400 about something else (an oversized prompt, say) all leave
the memo untouched. That last exclusion matters more than it looks: the memo is
monotonic and lives for the whole process, so demoting on an unrelated 400 would
strip constrained decoding from every *later* extraction in a long-lived server,
including the ones that would have succeeded. The trade is that a server which
refuses the shape without naming it is re-probed on each call rather than once —
the narrower risk, since the refusing backend this exists for answers 501, which
needs no prose matching.

Both statuses are terminal in the transport layer, so the probe costs one request
rather than a retry ladder, and discovery is single-flighted per schema: a wave
of concurrent extractions sharing one schema costs one rejection, not one per
task. Calls arriving while a probe is in flight use the cascade for that call.
Single-flighting covers the cold start and every rung after a demotion; what it
cannot cover is an endpoint that *stops* accepting a shape it had already
accepted (an Azure api-version rollover, a gateway upgrade), because there is no
way to learn that except by sending. Every call in flight at that moment pays one
rejection; from the demotion onwards the ladder is coordinated again.

A schema whose root is not an object never enters the mode at all — a
`response_format.json_schema` requires an object root, so there is nothing to
discover — and a schema using a JSON Schema feature outside OpenAI's strict
subset (a map field, `patternProperties`, `if`/`then`/`else`) is refused and
demoted rather than being silently rewritten into something the provider would
accept but that means something else.

The Bedrock adapter does not read this knob. It has the equivalent of litellm's
table (`crates/llm/src/adapters/bedrock/caps.rs`) and already picks Converse's
native `outputConfig.textFormat.jsonSchema` for the models that advertise
support, falling back to a synthetic `json_tool_call` tool for the rest.

> **Ollama embeddings:** set `EMBEDDING_ENDPOINT` explicitly when using
> `EMBEDDING_PROVIDER=ollama`. The Ollama embedder needs the `/api/embed` route, and
> the embedding endpoint does not inherit `LLM_ENDPOINT` (which points at the `/v1`
> chat base), so leaving it unset would target the wrong path.

## Embedding

The `embedding` section is registered by the embedding kind and shared by every
provider. Each provider's factory registers the settings only it reads (see
[Component config sections](#component-config-sections)). The Python flat name of
each key is accepted as an alias, shown in brackets.

| Env var (aliases) | Key (alias) | Default |
|---|---|---|
| `EMBEDDING_PROVIDER` | `embedding.provider` (`embedding_provider`) | `openai` (`onnx` on Android) |
| `EMBEDDING_MODEL` | `embedding.model` (`embedding_model_name`, `embedding_model`) | `text-embedding-3-small` (`BGE-Small-v1.5` on Android) |
| `EMBEDDING_DIMENSIONS` | `embedding.dimensions` (`embedding_dimensions`) | `1536` (`384` on Android) |
| `EMBEDDING_ENDPOINT` | `embedding.endpoint` (`embedding_endpoint`) | _(empty)_ |
| `EMBEDDING_API_KEY` (falls back to `LLM_API_KEY`) | `embedding.api_key` (`embedding_api_key`) | _(empty)_ |
| `EMBEDDING_MAX_SEQUENCE_LENGTH` | `embedding.max_sequence_length` (`embedding_max_sequence_length`) | `512` (local ONNX/fastembed only; remote providers use `EMBEDDING_MAX_COMPLETION_TOKENS`) |
| `EMBEDDING_MAX_COMPLETION_TOKENS` | `embedding.max_completion_tokens` (`embedding_max_completion_tokens`) | `8191` (input-token limit of a remote embedding API; the embedding term of the chunk-size auto-calculation for remote providers) |
| `EMBEDDING_BATCH_SIZE` | `embedding.batch_size` (`embedding_batch_size`) | `36` (texts per embedding request; raise for providers that allow larger batches. The OpenAI-compatible engine also dispatches up to 8 sub-batches concurrently) |
| `EMBEDDING_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | `embedding.rate_limit_*` (`embedding_rate_limit_*`) | `false` / `60` / `60` |
| `MOCK_EMBEDDING` | `embedding.mock` (`mock_embedding`) | _(empty)_. `true`/`1`/`yes`/`deterministic` select the mock engine with deterministic SHA-256-derived vectors; `zero` selects all-zero vectors (cosine KNN then returns nothing) |

Provider sections, registered by the provider's factory and present only when it
is compiled in:

| Env var (aliases) | Key (alias) | Provider | Default |
|---|---|---|---|
| `EMBEDDING_MODEL_PATH` / `COGNEE_E2E_EMBED_MODEL_PATH` | `embedding.onnx.model_path` (`embedding_model_path`) | `onnx` / `fastembed` (`onnx` feature) | _(empty: `<cache_root_directory>/models/BGE-Small-v1.5-model_quantized.onnx`)_ |
| `EMBEDDING_TOKENIZER_PATH` / `COGNEE_E2E_TOKENIZER_PATH` | `embedding.onnx.tokenizer_path` (`embedding_tokenizer_path`) | `onnx` / `fastembed` | _(empty: `<cache_root_directory>/models/bge-small-tokenizer.json`)_ |
| `EMBEDDING_ONNX_BATCH_SIZE` | `embedding.onnx.batch_size` (`embedding_onnx_batch_size`) | `onnx` / `fastembed` | `32` (inference batch size; independent of `embedding.batch_size`. Lower it under memory pressure on edge devices) |
| `EMBEDDING_API_VERSION` | `embedding.openai_compatible.api_version` (`embedding_api_version`) | `openai` / `openai_compatible` (Azure `api-version`) | _(unset)_ |
| `HUGGINGFACE_TOKENIZER` | `embedding.ollama.huggingface_tokenizer` (`huggingface_tokenizer`) | `ollama` | _(unset)_ |
| `AWS_*` | `aws.*` | `bedrock` (`bedrock` feature) | see [Bedrock](#bedrock-llm_providerbedrock) |

Provider values: `onnx`, `fastembed`, `openai`, `openai_compatible`, `ollama`,
`bedrock`, `mock`, plus any provider a custom registry adds with
`ComponentRegistry::register_embedding`. A provider whose factory is not
registered (a misspelling, or a backend compiled out) is an error, never a silent
ONNX fallback.

**`MOCK_EMBEDDING` diverges from the Python SDK, deliberately.** Python cognee's
embedding engines all return `[0.0] * dimensions` under a truthy `MOCK_EMBEDDING`
and have no deterministic mode. Rust defaults to SHA-256-derived vectors instead,
because zero vectors are silently discarded by cosine-distance KNN — the distance
is `NaN`, and LanceDB filters `NaN` rows out — so retrieval under zero vectors is
vacuously empty *and* unobservable, which is how a test or smoke check can search
under the mock engine and assert nothing at all. The mock engine is test-only and
no workflow enables it, so this does not affect production or cross-SDK parity.
Set `MOCK_EMBEDDING=zero` to reproduce Python's behaviour where a test wants it.

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

The `vector` section is registered by the vector kind. Backend-specific settings
are registered by the backend's factory, for example
[`vector.pgvector`](#pgvector-index-tuning). Python flat names are accepted as
aliases, shown in brackets.

| Env var | Key (alias) | Default |
|---|---|---|
| `VECTOR_DB_PROVIDER` | `vector.provider` (`vector_db_provider`) | `lancedb` (embedded, persistent) on non-Android; falls back to `brute-force` (in-memory) on Android |
| `VECTOR_DB_URL` | `vector.url` (`vector_db_url`) | _(empty — defaults to `{system_root_directory}/databases/cognee.lancedb`; set to `:memory:` to force the in-memory brute-force store)_ |
| `VECTOR_DB_HOST` / `VECTOR_DB_PORT` | `vector.host` / `vector.port` (`vector_db_host` / `vector_db_port`) | _(empty)_ / `1234` |
| `VECTOR_DB_NAME` / `VECTOR_DB_KEY` | `vector.name` / `vector.key` (`vector_db_name` / `vector_db_key`) | _(empty)_ |
| `VECTOR_DB_USERNAME` / `VECTOR_DB_PASSWORD` | `vector.username` / `vector.password` (`vector_db_*`) | _(empty)_ |
| `VECTOR_DATASET_DATABASE_HANDLER` | `vector.dataset_database_handler` (`vector_dataset_database_handler`) | _(empty — derived from the provider: `lancedb`, `pgvector`, `memory` for `brute-force`/`mock`)_; also `pgvector_shared` (a schema per dataset in the relational database). `pgvector` needs `CREATEDB` on the vector server, `pgvector_shared` `CREATE` on the relational database. Read only with `ENABLE_BACKEND_ACCESS_CONTROL` ([per-dataset storage](#per-dataset-storage)) |

Supported providers:

> **This list is the built-in set, not the set of valid values.** Any crate can
> register an additional provider under any id via `ComponentRegistry`, and
> `VECTOR_DB_PROVIDER` / `GRAPH_DATABASE_PROVIDER` accept it — including
> Postgres-backed providers, which receive the assembled
> `vector_postgres_url` / `graph_postgres_url` from the `*_HOST` / `*_PORT` /
> `*_NAME` / `*_USERNAME` / `*_PASSWORD` settings exactly as the built-ins do.
> Registering over a built-in id replaces it. See
> [guides/external-providers.md](guides/external-providers.md).
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

Qdrant is not part of OSS. See [tools/backends.md](tools/backends.md).
Setting `vector_db_provider` to `qdrant` is rejected at component
initialization in OSS (it returns a config error rather than falling back).

### pgvector index tuning

The `vector.pgvector` section, registered by the pgvector factory (`pgvector`
builds only): four optional overrides for the adapter's HNSW build, applied per
adapter and clamped to the range shown. Keys are
`vector.pgvector.hnsw_m`, `.hnsw_ef_construction`, `.maintenance_workers` and
`.maintenance_work_mem_mb`, with aliases `pgvector_hnsw_m` and so on. **Leave them unset** unless a
measurement says otherwise: the defaults are calibrated against a 100k-point
corpus of 384-d embeddings, and the two that cost server resources exist so
they can be turned *down* on a small or shared Postgres without patching the
crate.

| Env var | Default | Range | Effect |
|---|---|---|---|
| `COGNEE_PGVECTOR_HNSW_M` | `24` | 2–100 | `m` of every HNSW index built from now on |
| `COGNEE_PGVECTOR_HNSW_EF_CONSTRUCTION` | `128` | `2 × m` – 1000 | `ef_construction` of those builds |
| `COGNEE_PGVECTOR_MAINTENANCE_WORKERS` | `4` mid-load rebuild / `7` end-of-bulk-load build | 0–64 | `max_parallel_maintenance_workers` for a build; when set, overrides both defaults |
| `COGNEE_PGVECTOR_MAINTENANCE_WORK_MEM_MB` | `2048` | 64–65536 | ceiling of the per-build `maintenance_work_mem` (the adapter sizes each build from the row count and dimension and clamps to this) |

Notes:

- `m` / `ef_construction` apply to indexes **built after** the change. One
  already on disk keeps the parameters it was built with until it is rebuilt —
  `create_missing_vector_indexes()` (`cognee-cli vector-reindex`) only builds a
  *missing* or invalid index, never a valid one — so raising `m` on an existing
  store is a `DROP INDEX` + reindex, done deliberately.
- `ef_construction`'s floor follows `m` because pgvector requires
  `ef_construction >= 2 × m`; no combination of these two can produce a
  `CREATE INDEX` that fails.
- Asking for more parallel maintenance workers than the server can start is
  **not** an error. `max_parallel_maintenance_workers` is a plain per-session
  setting; Postgres launches as many as the server's `max_parallel_workers` /
  `max_worker_processes` still allow, down to zero (a serial build). The only
  cost of over-asking is a slower build, so no server-side configuration is
  required to adopt the defaults.
- Surrounding whitespace in an env value is ignored. A blank, negative or
  non-numeric env value is ignored and the default applies; an out-of-range
  value (however large) clamps. A typo in a deployment environment should not
  take the store down.

## Graph database

The `graph` section is registered by the graph kind; backend sections such as
`graph.ladybug` are registered by their factory. Python flat names are accepted as
aliases.

| Env var | Key (alias) | Default |
|---|---|---|
| `GRAPH_DATABASE_PROVIDER` | `graph.provider` (`graph_database_provider`) | `ladybug` (derived — see below) |
| `GRAPH_FILE_PATH` | `graph.file_path` (`graph_file_path`) | _(empty; defaults under the system root)_ |
| `GRAPH_DATABASE_URL` | `graph.url` (`graph_database_url`) | _(empty)_ |
| `GRAPH_DATABASE_HOST` / `GRAPH_DATABASE_PORT` | `graph.host` / `graph.port` (`graph_database_host` / `graph_database_port`) | _(empty)_ / `123` |
| `GRAPH_DATABASE_NAME` / `GRAPH_DATABASE_KEY` | `graph.name` / `graph.key` (`graph_database_*`) | _(empty)_ |
| `GRAPH_DATABASE_USERNAME` / `GRAPH_DATABASE_PASSWORD` | `graph.username` / `graph.password` (`graph_database_*`) | _(empty)_ |
| `GRAPH_DATASET_DATABASE_HANDLER` | `graph.dataset_database_handler` (`graph_dataset_database_handler`) | _(empty — derived from the provider: `ladybug`, `kuzu`, `postgres_graph`, `memory` for `mock`)_; also `postgres_graph_shared` (a schema per dataset in the relational database). `postgres_graph` needs `CREATEDB` on the graph server, `postgres_graph_shared` `CREATE` on the relational database. Read only with `ENABLE_BACKEND_ACCESS_CONTROL` ([per-dataset storage](#per-dataset-storage)) |
| `GRAPH_MAX_DB_SIZE` | section key `graph.ladybug.max_db_size` (alias `graph_max_db_size`; `ladybug` builds only) | `1073741824` (1 GiB) |

Supported providers: `ladybug`/`kuzu` (embedded), `postgres` (feature `pggraph`).

The default is **derived from the graph backends the binary was compiled with**,
not hardcoded. In order:

1. `ladybug`, when that feature is on — every default build, so the documented
   `ladybug` above is what you get;
2. otherwise `postgres`, when `pggraph` is on — a `--no-default-features … pggraph`
   image. This holds however many *other* ids are registered, so a `pggraph` build
   that also carries `testing` (registering `postgres`, `postgresql` and `mock`)
   still resolves to `postgres` on its own;
3. otherwise the single registered backend, when the build has exactly one.

Only a build registering two or more backends of which neither is `ladybug` nor
`postgres` has to set `GRAPH_DATABASE_PROVIDER` explicitly. Before this was
derived, a `pggraph`-only image defaulted to `ladybug` and aborted at start-up
with `Unsupported graph_database_provider 'ladybug'`.

When Postgres graph credentials are unset they fall back to the relational `DB_*`
config (see [roadmap/cognify-compatibility-plan.md](roadmap/cognify-compatibility-plan.md)).

> **The standalone HTTP server reads only `GRAPH_DATABASE_URL`.** The table
> above describes the SDK `Settings` surface, where the component-form
> `GRAPH_DATABASE_HOST`/`PORT`/`NAME`/`USERNAME`/`PASSWORD` variables and the
> `DB_*` fallback are assembled into a connection string. The server does not
> assemble one — with `GRAPH_DATABASE_PROVIDER=postgres` it requires
> `GRAPH_DATABASE_URL` (a `postgres://…` or `postgresql://…` string) and fails
> at startup otherwise. The server is not part of this repository; see [tools/http-server.md](tools/http-server.md).

## Relational database

The `relational` section is registered by the relational kind. Backends are
`RelationalDbFactory`s on the `ComponentRegistry`, selected by `relational.provider`
(when that is empty, by the scheme of the resolved URL). An external crate adds a
backend with `register_relational`, together with its own config sections; it
implements `cognee_database::RelationalDb`. The built-in SeaORM factory is
registered as `sqlite` (feature `sqlite`) and `postgres` / `postgresql` (feature
`postgres`). An unregistered provider is an error.

| Env var | Key (alias) | Default |
|---|---|---|
| `DB_PROVIDER` | `relational.provider` (`db_provider`) | `sqlite` |
| `DATABASE_URL` | `relational.url` (`relational_db_url`) | `sqlite:./cognee.db?mode=rwc` |
| — | `relational.migration_url` (`migration_db_url`) | _(empty)_ |
| `DB_HOST` / `DB_PORT` | `relational.host` / `relational.port` (`db_host` / `db_port`) | `localhost` / `5432` |
| `DB_NAME` | `relational.name` (`db_name`) | `cognee_db` |
| `DB_USERNAME` / `DB_PASSWORD` | `relational.username` / `relational.password` (`db_username` / `db_password`) | _(empty)_ |
| `DB_SSLMODE` (else derived from `DATABASE_CONNECT_ARGS`) | `db_sslmode` | _(empty)_ |
| `COGNEE_SINGLE_PROCESS` | `single_process` | _(unset — see below; effectively off)_ |

For `postgres` the URL is built from the connection parts; otherwise `relational.url`
is used verbatim. `db_sslmode` and `single_process` stay cognee-wide `Settings` keys:
the TLS mode is appended to every Postgres URL cognee builds (relational, pgvector
and the Postgres graph), and `single_process` gates startup recovery.

### `single_process` / `COGNEE_SINGLE_PROCESS` — does one process own this database?

**In practice this is an opt-in: startup recovery does nothing until you turn
it on.** The setting is tri-state, and the derived answer is only ever useful
in the negative:

| value | meaning |
|---|---|
| unset / `null` | derive it — see below |
| `true` (`1`, `yes`, `on`) | one process owns this database; enable startup recovery |
| any other non-blank value | explicitly not; leave everything alone |

`null` restores the derived answer the way it does for `chunk_size`. The env
var is parsed by the same code as the config key, so the two cannot disagree —
a blank or whitespace-only value means "derive", not `false`, and `1` / `0`
mean what `true` / `false` mean whether they arrive as a string, a JSON number
or a shell value.

#### Which surfaces read it

Not every entry point runs startup recovery, so not every entry point consults
this setting. Assert it the way your deployment is actually started:

| how you run cognee | how to assert it | what happens |
|---|---|---|
| a binding — Python, JS, C API, Java/Android | `set_config("single_process", true)`, `ConfigManager::set_single_process(true)`, or `COGNEE_SINGLE_PROCESS=1` | recovery runs on the first operation |
| `cognee-http-server` | `COGNEE_SINGLE_PROCESS=1` **only** | recovery runs at server startup |
| `cognee-cli cognify` / `memify` | — | **no startup recovery at all**; use `cognee-cli pipeline-unblock --clear` |

The two gaps are deliberate, not oversights:

- The HTTP server never loads the `cognee config` file (that file is CLI-only —
  see [Precedence](#precedence)) and builds no
  `Settings`, so `cognee config set single_process true` cannot reach it. Its
  environment is the only channel there is.
- The CLI runs no sweep on any command. Every invocation is its own process,
  and two concurrent `cognee-cli cognify` runs against one SQLite file are
  exactly the case the claim exists to catch — so sweeping on each invocation
  would be the wrong default even where this setting says it is safe.
  `cognee-cli pipeline-unblock --clear` is the supported way to clear a wedged
  dataset from the CLI, and it clears both gates.

#### Set it before the first operation

The setting is read **once per relational database per process**, on the first
operation that opens that database. Flipping it on afterwards is ignored for
the rest of that process's life — deliberately: by then the process may have
runs of its own in flight, whose rows and claims a sweep cannot tell apart
from a dead predecessor's, and rolling those back would delete a live run's
graph artifacts. A binding that notices the flip logs a `WARN` saying so;
restart to recover a dataset that is already wedged.

The derivation resolves `true` for **only an in-memory SQLite URL**
(`sqlite::memory:`, `?mode=memory`) and `false` for everything else — Postgres,
and **file-backed SQLite including the shipped default
`sqlite:./cognee.db?mode=rwc`**, which every cognee process started in that
directory opens. Since an in-memory database dies with its process, it can
never hold a leftover from a dead run, so the derived `true` gives the sweep
nothing to do. The derivation earns its place through the `false` half, which
is what stops a SQLite *file* from being mistaken for a private one.

#### What asserting it turns on

Startup recovery for a run killed mid-flight — SIGKILL, an OOM kill, Android
killing the app process. A run that fails while its process is alive cleans up
after itself; a killed one leaves three things behind, in the order recovery
deals with them:

1. **What it had already written** into the graph and vector stores, recorded
   in the ownership ledger against its `pipeline_run_id`. A live failure rolls
   this back through the same `RunSweeper` the `forget` path uses; a killed
   process never reaches that code, so the artifacts survive attributed to a
   run that will never finish, and no later sweep selects them again.
2. **The `pipeline_runs` row left at `Started`** — read first by the run gate
   and **never** expiring.
3. **The exclusive-run claim** on `(dataset, pipeline)` — released only by its
   holder, otherwise expiring 24 h later.

The order matters: retiring the status row is what makes the dataset runnable
again, so it happens *after* the rollback, never before. (Python does the same
— `cognify_rollback_handler` runs ahead of the status reset in
`modules/cognify/recovery.py`.) The rollback is scoped strictly to the dead
run in its own dataset; it is never a blanket delete, and artifacts another
run also claims are kept.

With one process per database, all three leftovers present at startup belong
to a dead predecessor, so all three are cleared and the dataset is runnable
again. With more than one, any of them may belong to a live peer, so nothing
is touched and the claim keeps excluding concurrent runs across processes
exactly as before.

A rollback that fails is logged and the status row is retired anyway. The
ownership rows survive as the record of what still needs deleting, whereas
leaving the status row would wedge the dataset permanently — that gate never
expires, and for an embedded consumer this is the only thing that clears it.

##### A divergence from Python worth knowing about

Retiring an orphan writes a **new `Errored` row** in Rust, where Python's
recovery resets the run to `INITIATED`. Both resolve to "you may run" at the
gate, and both are recorded rather than deleted, so nothing downstream
behaves differently — but a status poller does see different things after the
same crash: on Rust the run reads as a *failed build*, on Python as one that
*never started*. Rust's answer is the truthful one (the run did start, and it
did not finish) and it keeps the new-row-per-transition audit trail intact, so
it is deliberate. Note that Rust's own `reset_pipeline_run_status` API writes
`INITIATED`, matching Python; only the orphan sweep differs.

#### Who needs to set it

Any single-process deployment on a **file-backed** database: an Android app, a
desktop app, a one-process service, a CLI-only setup. The URL cannot show this
— the very same URL is what two `cognee-cli` invocations, or an HTTP server
restarting beside a running CLI, would use, and sweeping there would delete a
live sibling's claim and admit the concurrent run the claim exists to prevent.

Without the opt-in nothing regresses; you simply keep today's behaviour. The
claim still ages out after 24 h, and the orphaned row is still cleared by
`cognee-cli pipeline-unblock --clear` or by an HTTP-server restart. What you
do not get either way is the artifact rollback: nothing but this recovery ever
removes what a killed run wrote, so those nodes stay in the graph.

Set it to `false` to refuse the recovery outright — useful to pin the safe
behaviour in a config file so a later environment change cannot switch it on.

## Chunking & tokenizer

Read by [`crates/chunking/src/config.rs`](../crates/chunking/src/config.rs).
`chunk_strategy` (default `PARAGRAPH`) and `chunk_engine` are `Settings` /
`CognifyConfig` fields without env bindings. Chunk size and overlap are
env-bindable:

| Env var | `Settings` field | Default |
|---|---|---|
| `COGNEE_CHUNK_SIZE` / `CHUNK_SIZE` | `chunk_size` | _(unset — auto)_ |
| `COGNEE_CHUNK_OVERLAP` / `CHUNK_OVERLAP` | `chunk_overlap` | `10` |

`chunk_size` unset means "auto-calculate at pipeline time", mirroring Python's
`chunk_size=None`: `min(embedding_max_sequence_length,
llm_max_completion_tokens / 2)`. With OpenAI-compatible embeddings at their
defaults that is `min(8191, 8192) = 8191` tokens; with the local ONNX/BGE engine
it is its 512-token sequence limit. Set it explicitly (env, `cognee config set
chunk_size`, `--chunk-size`, or the HTTP `chunkSize` field) to override — an
explicit value is always honoured, including `1500`. `cognee config set
chunk_size null` restores the auto-calculation.

Precedence on the HTTP server: per-request `chunkSize` on the cognify/remember
payload > `COGNEE_CHUNK_SIZE` > auto.

The token counter is chosen by the `cognify` engine section (registered with the
kind sections), resolved when cognee lowers `Settings` into a `CognifyConfig`
(`Settings::cognify_config`; `CognifyConfig::default()` itself reads nothing):

| Env var | Key | Purpose | Default |
|---|---|---|---|
| `COGNEE_TOKEN_COUNTER` | `cognify.token_counter` | `tiktoken` / `word` / `huggingface`(`hf`) | auto from embedding provider |
| `HUGGINGFACE_TOKENIZER` | `embedding.ollama.huggingface_tokenizer` | model id when counter = `huggingface` | _(empty)_ |
| `EMBEDDING_TOKENIZER_PATH` | `embedding.onnx.tokenizer_path` | tokenizer file for the `onnx` provider's counter, used when it exists | `<cache_root_directory>/models/bge-small-tokenizer.json` |
| `COGNEE_LEGACY_PER_WORD_TOKEN_COUNT` | `cognify.legacy_per_word_token_count` | restore the legacy per-word token count (`1`/`true`/`yes`/`on`) | `false` |

`Settings::cognify_config` also applies the batch sizes, the graph-extraction
prompt and the summary schema:

| Env var | Key (alias) | Purpose | Default |
|---|---|---|---|
| `CHUNKS_PER_BATCH` | `cognify.chunks_per_batch` (`chunks_per_batch`) | chunks per graph-extraction batch (Python's `CognifyConfig.chunks_per_batch`); `0` keeps the default | `2000` |
| `COGNEE_DATA_PER_BATCH` | `cognify.data_per_batch` (`data_per_batch`) | data items per temporal-cognify batch (Python's `data_per_batch` parameter, which has no env var); `0` keeps the default | `20` |
| `COGNEE_COGNIFY_EXTRACTION_STRATEGY` | `cognify.extraction_strategy` | graph extraction: `single_pass` (one LLM call per chunk) or `cascade` (Python's multi-round `extract_graph_from_data_v2`: nodes → relationship names → edge triplets, `3 × rounds` calls per chunk, no date hints; ignores a custom prompt). Rust-only: Python reaches the cascade only from its eval framework. An unrecognised value keeps the default, with a warning | `single_pass` |
| `COGNEE_COGNIFY_CASCADE_ROUNDS` | `cognify.cascade_rounds` | rounds per cascade stage (Python's `n_rounds`); `0` keeps the default | `2` |
| `GRAPH_PROMPT_PATH` | `graph_prompt_path` | graph-extraction system prompt. The default and an empty value use the compiled-in prompt. The names of the other prompts Python ships in its prompts directory — `generate_graph_prompt_strict.txt`, `generate_graph_prompt_simple.txt`, `generate_graph_prompt_guided.txt`, `generate_graph_prompt_oneshot.txt` — select those prompts, compiled in, as Python resolves a relative name against that directory. Any other value is a file read as the prompt text, a relative path against the working directory. An unreadable file keeps the built-in prompt, with a warning. Unlike Python, a file is sent verbatim, not rendered as a template (the built-in prompts contain no template syntax; the default prompt's date-hint block is appended in code, and only to the default prompt). | `generate_graph_prompt.txt` |
| — | `summarization_schema` (code only: `ConfigManager::set_summarization_schema`, `set`) | JSON schema of the summary output (Python's `summarization_model`); must have a string `summary` | _(unset — the built-in `SummarizedContent` shape)_ |

### `COGNEE_LEGACY_PER_WORD_TOKEN_COUNT` — a deliberate divergence from Python

By default a chunk's size is the tokenizer's count of the chunk itself. Until
SDK-632 it was the sum of the counts of its *words, each tokenized in
isolation*, and sub-word tokenizers are not additive over a partition: cl100k
encodes `" the"` as one token but `"the"` and `" "` as two. MEASURED, the sum
over-states the span by **1.78x** on Alice in Wonderland, so a chunk configured
at 8191 tokens held **4,669** and the run made ~1.8x the LLM calls it was
configured for. The default now fills that budget to **8,188 of 8,191**.

**Python 1.5.x still counts per word** — VERIFIED at
`cognee/tasks/chunks/chunk_by_sentence.py`, whose `get_word_size(word)` calls
`embedding_engine.tokenizer.count_tokens(word)` once per word. So the Rust
default is knowingly *not* byte-for-byte with Python's chunk boundaries. Setting
`COGNEE_LEGACY_PER_WORD_TOKEN_COUNT=1` restores that parity at the cost of the
over-count; it exists for that comparison and for a deployment that has tuned
around the old effective chunk size, and is expected to go once Python ships the
same fix. Only `1`/`true`/`yes`/`on` enable it — an unrecognised value leaves
the default in place rather than silently halving the chunk size.

Full measurement record, methodology, the two further consequences this was
checked against, and how to reproduce it:
[docs/performance/chunk-token-overcount.md](performance/chunk-token-overcount.md).

## Cognify failure handling

Read by [`crates/cognify/src/failure.rs`](../crates/cognify/src/failure.rs).
When a stage cannot process one chunk or one file it now *records* the failure
— which stage, which file, which chunk, and why — instead of aborting the batch
by propagating. The report rides every stage output to `CognifyResult.failures`
(on success) or to `CognifyError::RunFailed` (when the run failed). Whether the
run fails is decided once, at the end, from two independent axes.

Each is a `CognifyConfig` builder, and each is also a key of the `cognify` engine
section, which `Settings::cognify_config` applies. They live in the section, not as
`Settings` fields, so `GET /api/v1/settings` keeps the shape the parity suite
compares against Python's.

| Builder | Field | Env var | Key | Default |
|---|---|---|---|---|
| `with_failure_stop` | `failure_stop` | `RAISE_INCREMENTAL_LOADING_ERRORS` / `COGNEE_RAISE_INCREMENTAL_LOADING_ERRORS` | `cognify.raise_incremental_loading_errors` (`raise_incremental_loading_errors`) | `FailFast` (unset) |
| `with_rollback_scope` | `rollback_scope` | `COGNEE_COGNIFY_ROLLBACK_SCOPE` (`whole_run` / `failed_items` only) | `cognify.rollback_scope` | `WholeRun` |
| `with_summarization_failure_tolerance` | `tolerate_summarization_failures` | `COGNEE_COGNIFY_TOLERATE_SUMMARIZATION_FAILURES` | `cognify.tolerate_summarization_failures` | `false` |
| `with_chunk_failure_ratio_threshold` | `chunk_failure_ratio_threshold` | `COGNEE_COGNIFY_MAX_CHUNK_FAILURE_RATIO` | `cognify.max_chunk_failure_ratio` | `0.05` |
| `with_failure_report_cap` | `failure_report_cap` | `COGNEE_COGNIFY_FAILURE_REPORT_CAP` | `cognify.failure_report_cap` | `100` |

**Axis 1 shares Python's variable name.** `RAISE_INCREMENTAL_LOADING_ERRORS` is
Python's own flag (`run_tasks_data_item.py:200`, default `true`), so one `.env`
configures both SDKs the same way. Its polarity is Python's and is inverted
relative to the variant names: *raising* on the first error is `FailFast`,
and `false` is `RunToEnd`. In Python that flag decides whether the first failed
item aborts the run or every item is attempted first — it never changes what
survives, because Python sweeps the whole run either way. That is precisely
what this axis means here. The `COGNEE_`-prefixed alias follows the same
dual-name convention as `CHUNK_SIZE` / `COGNEE_CHUNK_SIZE`.

**The other four are Rust-only** and say so with a `COGNEE_COGNIFY_` prefix:
Python has no counterpart for any of them. An unset value leaves the default in
place, and out-of-range values (a ratio outside `0..=1`, a zero cap, an unknown
rollback scope) are ignored with a warning rather than failing the process.
`raise_incremental_loading_errors` is a strict boolean: `true`/`1`/`yes`/`on`
is `FailFast`, `false`/`0`/`no`/`off` is `RunToEnd`, and any other value is
ignored with a warning, so a typo such as `ture` keeps the `FailFast` default
rather than silently switching to the costlier run-to-end mode.

**Axis 1 — when to stop.** `FailFast` stops the failing stage from scheduling
further work once a failure has been recorded; the files that already completed
still travel down the rest of the pipeline. `RunToEnd` continues past failures,
collecting them, and decides at the end — a full run's worth of LLM cost for the
complete failure list.

*"Once" is not "immediately", and it is not per file.* Each stage stops at the
boundary its own loop offers, and only one of them is the first failed file:

| Stage | Stops after | Set by | Default |
|---|---|---|---|
| Chunking | the file that failed | — | one file |
| Graph extraction (standard and custom-graph) | the batch that contained the failure | `chunks_per_batch` | **2000 chunks** |
| Temporal extraction | the batch that contained the failure | `data_per_batch` | **20 files** |
| Summarization | the calls already in flight when the failure came back | `max_parallel_extractions` | **1000 calls** (128 on Android/iOS) |

The extraction row is the one to read twice. 2000 is Python's own default
(`cognify.py:367-370`) and is deliberately large — the stage awaits each batch
before starting the next, so small batches serialise a run into waves. The
consequence is that any dataset under 2000 chunks *is* one batch: `FailFast`
dispatches every extraction call, learns about the failure when the batch is
collected, and only then stops. On such a dataset it saves nothing; what it
saves is every batch after the one that failed. Lowering `chunks_per_batch`
tightens the boundary at the cost of that serialisation.

Summarization has no batch loop at all — one bounded stream covers the whole
run — so it stops at the granularity of the concurrency window instead. The
first chunk to fail closes the gate; every chunk not yet dispatched is skipped
and its file is recorded as unreached, so it is neither marked complete nor
left half-summarized. Calls already in flight are never cancelled and their
summaries are never discarded, so a run with fewer chunks than
`max_parallel_extractions` again saves nothing. A summarization failure that
`COGNEE_COGNIFY_TOLERATE_SUMMARIZATION_FAILURES` makes non-fatal does not close
the gate at all: it fails no item, so there is nothing to stop for.

**Axis 2 — what to sweep.** `WholeRun` removes everything the run created;
`FailedItems` removes only the failed files' contributions and keeps the rest.
The default pair `FailFast` + `WholeRun` is Python's default behaviour in both
execution and end state.

`FailedItems` keeps the good files **only while the run as a whole is
survivable** — that is, only below the failure ratio, and only if at least one
item survived. Once a `FailedItems` run is judged failed it escalates to a
whole-run sweep, and the escalation deletes the files that *completed* too,
along with their completion markers, so the next run redoes the entire dataset
rather than just the failures. That is the intended reading of the threshold:
above this proportion, do not trust any of this run — and the pre-run state is
both the honest end state and the only one a retry converges from. A caller who
would rather keep the partial result raises
`COGNEE_COGNIFY_MAX_CHUNK_FAILURE_RATIO`; there is no setting that keeps
completed files once the run is fatal.

A third variant, `Nothing`, removes nothing — and it is **not selectable from
configuration**. `COGNEE_COGNIFY_ROLLBACK_SCOPE=nothing` (or `none`) is
rejected like any other unknown value and leaves the default in place; the
variant is reachable only in code, via
`CognifyConfig::with_rollback_scope(RollbackScope::Nothing)`. It is off the
configuration surface because what it leaves behind is permanent. A run that
wrote graph edges and then died before the vector stage leaves those edges in
the graph with no `Triplet_text` and no `EdgeType_relationship_name` point. The
retry does not repair them: the completion marker is unset, so the item is
processed again, but `retrieve_existing_edges` finds the orphaned edges and
expansion skips every edge already in the graph — so no row and no vector is
ever written for them again. Setting `incremental_loading = false` does not
help; that dedup filter is not the incremental one. `WholeRun` and
`FailedItems` both converge on a retry, because both remove the half-written
artifacts first; `Nothing` is the only scope that cannot.

**What each combination now does at the end of a run.**

| Combination | End state |
|---|---|
| `FailFast` + `WholeRun` *(default)* | Stops at the first failed batch, removes everything the run created, marks the run `ERRORED`, returns `Err`. Everything earlier runs completed is untouched. Python's default |
| `FailFast` + `FailedItems` | *Below the ratio:* stops at the first failed batch; the files that fully completed are kept, indexed and marked; the failed and never-reached ones are removed and left unmarked. *Above it, or with no survivor:* the run is fatal and escalates — everything it created goes, completed files included |
| `RunToEnd` + `WholeRun` | Pays for the whole run to produce the complete failure list, then removes everything the run created |
| `RunToEnd` + `FailedItems` | *Below the ratio:* every good file is cognified and marked; only the failed files are removed, and they come back to the caller in the report. *Above it:* the same escalation — the good files are removed too and the call returns `Err` |
| _either_ + `Nothing` *(code only)* | Nothing is removed. Knowingly leaves partially-cognified files in place, permanently — see above |

**What the SDK call returns.** Any combination that sweeps and ends with the
run `ERRORED` returns `Err`, matching Python, which re-raises after rolling
back. A `FailedItems` combination that completes below the ratio returns `Ok`
carrying the failed-file list in `CognifyResult.failures` — the run genuinely
completed. `Nothing` propagates whatever the pipeline produced.

**What a sweep keeps.** An artifact is removed only when nothing outside the
swept scope still claims it — another run, another dataset, a row predating
ownership tracking, or, for an item-scoped sweep, a *surviving* file in the same
run. So an entity a failed file shared with a good one stays.

**Cancellation is swept**, a deliberate divergence: Python's rollback handler
catches `Exception`, which excludes `CancelledError`, so a cancelled Python run
keeps its partial graph. There is currently no caller-facing cancel handle on
`cognify()`, so this is reachable only from inside the library; dropping the
`cognify()` future runs no sweep at all (crash recovery is out of scope).

**Per-stage policy.** Failure policy is a property of the stage, not a user
choice — with one exception.

| Stage | Failure unit | Behaviour |
|---|---|---|
| Chunking | data item | Collected; the item fails and contributes neither chunks nor a `Document` |
| Graph extraction | chunk | Collected; the item fails. Ratio-checked under `FailedItems` |
| Summarization | chunk | **Configurable.** Default: the item fails (Python parity). With tolerance on: recorded separately, never fails the item or the run, never counts toward the ratio |
| Temporal event extraction | chunk | Collected; the item fails. Ratio-checked under `FailedItems` |
| Temporal entity enrichment | chunk | Collected. One LLM call covers a whole batch, so the failure is recorded against **every chunk that fed the batch** and the batch's events are dropped — an event with no attributes carries none of the entity graph the pass exists to produce |
| Persistence — graph, vector or ledger writes, and embeddings | run | Always fatal, under every combination |

`tolerate_summarization_failures` governs **fatality only**. Either way the
summarization stage collects rather than propagates: the whole stream is drained
so already-completed summaries are kept and the reported list is complete.

**The failure ratio** is counted in chunks, evaluated per run, excludes
summarization failures, and applies only under `FailedItems`. It is backstopped
by an explicit rule: if no item survived, the run failed regardless of the
ratio — a file that failed at the chunk stage produced no chunks, so it
contributes to neither side of the ratio. Crossing the ratio (or tripping the
backstop) is what turns a `FailedItems` run fatal, and therefore what triggers
the escalation to a whole-run sweep described above: the completed files and
their markers go too.

**The report cap** bounds the individual entries the report lists (and therefore
the error message); the total count, the failed-file set and every counter are
never capped.

Note that a run that dies in a persistence stage loses the report of the
failures collected earlier: persistence errors keep their own `CognifyError`
variants, because they are already run-fatal under every configuration.

## Completion markers and incremental loading

`incremental_loading` (default **on**) is now live. After a successful run,
every data item the run finished is marked complete on its `Data` row, under
`pipeline_status["cognify_pipeline"][<dataset id>] =
"DATA_ITEM_PROCESSING_COMPLETED"` — Python's format byte for byte, dashed
dataset id and all, so the two SDKs can share a database. The next run reads
those markers before it builds a pipeline and skips the items already carrying
one: they are not classified, not chunked, and never sent to an LLM.

Three consequences worth stating plainly:

- **Re-cognifying an already-complete dataset is a no-op.** The call returns
  `Ok` with `already_completed = true` and empty payloads. This is a behaviour
  change: before this, every cognify re-processed everything.
- **A failed run marks nothing**, and a sweep clears the markers of every item
  it rolled back, so the next run redoes exactly the work that was lost.
- **A run that completed with failures outstanding is not a pipeline-cache
  hit**, even with `use_pipeline_cache` on — the next run has the failed files
  to redo.

**A divergence from Python worth knowing on a shared database.** A fully
skipped Rust run writes no `pipeline_runs` rows at all: the marker check runs
before the pipeline is built, so there is no `INITIATED` / `STARTED` /
`COMPLETED` trail for that call, and nothing appears under
`GET /api/v1/activity/pipeline-runs`. Python reaches the same end state by a
different route — `run_tasks` logs the run start before it looks at any item
(`operations/run_tasks.py`), and each item then yields
`PipelineRunAlreadyCompleted` from inside the open run
(`operations/run_tasks_data_item.py`) — so a repeat cognify leaves a complete
run trail there. The two SDKs agree on what a repeat run *did* (nothing) and
disagree on whether it is visible as a run.

**Temporal runs share the markers.** Both branches write and read the one
`cognify_pipeline` key, which is what Python does — its temporal cognify runs
under the same `pipeline_name`. The consequence is user-visible: a `cognify()`
followed by a temporal `cognify()` over the same dataset is a **no-op**, and the
other way round. Callers who want both graphs over one dataset must set
`with_incremental_loading(false)`.

Set `with_incremental_loading(false)` to restore the previous behaviour and
reprocess everything on every run.

## Search

The `search` and `search.hybrid` engine sections tune the standard retrievers;
cognee hands them to `SearchBuilder::with_tuning` (`Settings::search_tuning`).

| Env var | Key (alias) | Default | Effect |
|---|---|---|---|
| `ALLOW_CYPHER_QUERY` | `search.allow_cypher_query` (`allow_cypher_query`) | `true` | `false` (or `0`/`no`/`off`) refuses the CYPHER and NATURAL_LANGUAGE search types (Python's flag). Any other value is ignored with a warning and keeps them allowed, as Python disables only on `false`. |
| `COGNEE_HYBRID_MAX_CONTEXT_TOKENS` | `search.hybrid.max_context_tokens` | _(unset — the LLM's context length)_ | HYBRID_COMPLETION context budget in tokens; `0` turns budgeting off. |
| `COGNEE_HYBRID_SUMMARIZE_OVERFLOW` | `search.hybrid.summarize_overflow` | `false` | Summarize passages that overflow the budget instead of dropping them. |
| `COGNEE_HYBRID_SUMMARIZE_BATCH` | `search.hybrid.summarize_batch` | `1` | Overflow passages per summarization call. |
| `COGNEE_HYBRID_OVERFLOW_SUMMARY_FILE` | `search.hybrid.overflow_summary_file` | _(empty)_ | JSON object of precomputed overflow summaries (chunk id → summary). |

Three `Settings` fields tune the search orchestrator itself, lowered by the same
`Settings::search_tuning`:

| Env var | `Settings` field | Default | Effect |
|---|---|---|---|
| `ENABLE_LAST_ACCESSED` | `enable_last_accessed` | `false` | Update `last_accessed` on the source data records of every search's retrieved context, as Python does on every retrieval (Python's flag, also default off). On a plain completion search the context is then fetched by the search itself and handed to the retriever, so it is still fetched once. `FEEDBACK` searches retrieve no data and are not tracked. |
| `AUTO_FEEDBACK` | `auto_feedback` | `false` | Detect feedback in a session search's message (one extra LLM call per session turn) when the request leaves `auto_feedback_detection` unset, and only while `session.caching` is on (Python's `caching and auto_feedback`). The same switch gates improve's `extract_agent_context` stage (`auto_feedback_disabled` when off). **Python defaults it to `true`**; Rust keeps it off so a session search makes no extra LLM call unless asked. |
| — | `default_system_prompt_path` | `answer_simple_question.txt` | System prompt file for a search that names neither a system prompt nor a prompt path. The default (Python's file name for the built-in prompt) and an empty value leave each retriever's own compiled-in prompt; any other value is read as a file. Rust-only (Python has no such setting). |

### Triplet retriever hub cap

| Env var | Key | Default | Effect |
|---|---|---|---|
| `COGNEE_TRIPLET_NEIGHBOR_CAP` | `search.triplet_neighbor_cap` | _(unset — uncapped)_ | Per vector-search seed, keep every neighbour that is itself a seed plus at most `n` other neighbours (the smallest ids in byte order), then score the subgraph induced on what is kept. |

**Opt-in, and it changes results.** Unset — the default, and what every
deployment runs unless it says otherwise — the triplet retriever loads the full
depth-1 neighbourhood of its seeds, which is result-identical to loading the
whole graph. A cap truncates hub fan-out: edges from a hub seed to its dropped
neighbours never reach the ranking. They carry the non-seed distance penalty
and so rank below seed-to-seed edges, but they can still make the top *k*.

Measured on the synthetic 100k corpus, a cap of 100 keeps a Jaccard similarity
of 0.93–0.96 against the uncapped result set, with a worst query at 0.67; a cap
of 200 keeps 0.92–0.93 but has a worst query at 0.18. At 10k a cap of 100
measures 0.985 / worst 0.82. The latency it buys is small on a uniform corpus
(≈ no change at 100k) and large on real hub-heavy text: in the growth corpus a
`graph_completion` search went from 332 ms to 76 ms by document 11. Treat it as
a knob for a corpus with known hubs, measured on that corpus, not as a default.

The cap bounds single-query graph search only. Batch graph search
(`brute_force_triplet_search_batch`, several queries at once, as the
decomposition retriever issues them) loads the whole graph, as Python's batch
mode does, and the cap does not apply to it.

`PgGraphAdapter` applies the cap in SQL; every other backend gets the same
semantics from `GraphDBTrait::get_neighborhood_scoped`'s client-side default,
and `test_get_neighborhood_scoped_cap_is_deterministic` pins the two to the
same neighbour set.

## Search — hybrid retriever knobs

The `HYBRID_COMPLETION` search type (`SearchType::HybridCompletion`) accepts a
set of per-request tuning knobs. Unlike the env-var knobs elsewhere on this
page, these are **library / orchestrator-only** — they are typed
`Option<...>` fields on
[`SearchParams`](../crates/search/src/types/search_params.rs), populated in
`impl From<&SearchRequest>` from the **snake_case JSON keys** inside
`SearchRequest.retriever_specific_config`. They have no env binding.

**Reachability.** These knobs are reachable from a direct Rust library caller
that constructs a `SearchRequest` with a populated `retriever_specific_config`,
and from the **language bindings**, which forward a `retrieverSpecificConfig`
search option verbatim
([`bindings-common/src/ops/retrieval.rs`](../crates/bindings-common/src/ops/retrieval.rs)).
The **CLI** still hardcodes `retriever_specific_config: None`
([`cli/src/commands/search.rs`](../crates/cli/src/commands/search.rs)), and so
does the HTTP server, which is not part of this repository (its search,
recall and responses handlers): the knobs are **not** wire fields on
`SearchPayloadDTO`.

When a knob is unset the retriever falls back through a three-layer resolution:
`retriever_specific_config.<knob>` → `SearchRequest.top_k` capped at 10 → the
`HybridRetriever::new` constructor default (Python's `_hybrid_lane_top_k`; an
explicit per-lane knob is never capped). The effective defaults are:

| `SearchParams` field | `retriever_specific_config` JSON key | Effective Rust default | Python parity |
|---|---|---|---|
| `chunks_top_k` | `chunks_top_k` | `min(top_k, 10)`, else `5` | `chunks_top_k` (derives from `top_k`) |
| `entities_top_k` | `entities_top_k` | `min(top_k, 10)`, else `5` | `entities_top_k` |
| `facts_top_k` | `facts_top_k` | `min(top_k, 10)`, else `5` | `facts_top_k` |
| `max_edges_per_entity` | `max_edges_per_entity` | `10` | `max_edges_per_entity=10` |
| `text_summaries_top_k` | `text_summaries_top_k` | `None` (no fallback) | `text_summaries_top_k=None` |
| `chunk_lane_fusion` | `chunk_lane_fusion` | `"relative_score"` | **diverges** — Python is rank-only RRF (`"reciprocal_rank"`) |
| `summary_lane_weight` | `summary_lane_weight` | `0.75` | no Python equivalent |
| `use_importance_weight` | `use_importance_weight` | `true` | `use_importance_weight=True` |
| `node_name` | `node_name` | `None` | reused query node filter |
| `node_name_filter_operator` | `node_name_filter_operator` | `"OR"` | `"OR"` / `"AND"` |
| `candidate_top_k` | `candidate_top_k` | `top_k * 4` (TEMPORAL only) | `candidate_top_k` (defaults to `top_k * 4`) |

**TEMPORAL.** `TEMPORAL` search (`SearchType::Temporal`) runs the same hybrid
fetch, so it reads the same keys, with two differences. The three-layer
resolution above applies to the `HYBRID_COMPLETION` lanes only: TEMPORAL's
`top_k` is `SearchRequest.top_k` (default `5`) and is **not** capped at 10. And
its chunk and entity lanes are sized by `candidate_top_k` and its fact lane by
`top_k`, overriding `chunks_top_k` / `entities_top_k` / `facts_top_k`; the
time-window rerank then cuts the candidates back to `top_k`. A `top_k` of `0`
or a `candidate_top_k` below `top_k` is rejected with `InvalidInput`, as in
Python.

**Chunk-lane fusion.** `chunk_lane_fusion` chooses how the `DocumentChunk_text`
lane and the `TextSummary_text` lane are combined:

- `"relative_score"` (the default) normalises each lane's own similarities to
  `[0, 1]` against that lane's own candidates and adds them, the summary lane
  weighted by `summary_lane_weight`.
- `"reciprocal_rank"` is Python's `rank_chunk_summary_pairs` verbatim — RRF over
  ranks only, with the similarities discarded.

The default diverges from Python deliberately. RRF's constant `k` is calibrated
for candidate lists of thousands; these lists are `2 * chunks_top_k` and
`text_summaries_top_k` long, so the spread across a whole lane is smaller than
the bonus for appearing in the other one, and "present in both lanes" outranks
"similar to the query". Measured over 15 probe questions on *Alice in
Wonderland* across two chunkings and two summary-lane widths, relative-score
fusion at the default weight raised MRR in all four configurations (0.35–0.54 →
0.47–0.56) and lowered gold-passage-in-context in none. Set
`chunk_lane_fusion = "reciprocal_rank"` to get the Python ranking back.

`summary_lane_weight` applies only to `"relative_score"`; the chunk lane always
weighs `1.0`. The default of `0.75` is the centre of the measured plateau — the
summary lane searches a *derivative* of the same documents the chunk lane
searches, so it is a second opinion rather than an independent channel.

**Truth weighting.** `use_truth_weight` (default `false`) re-ranks the chunk
lane by truth-subspace alignment with the query. It needs exactly one dataset
in scope and chunk coordinates at the live centroid epoch, which
`improve(build_truth_subspace=true)` builds from the session's distilled
learnings; with neither, ranking stays at baseline. A failed build keeps the
previous epoch live and reports the `build_truth_subspace` stage as `errored`;
a graph backend without truth-state support (`GraphDBTrait::supports_truth_state`)
reports it as `skipped` / `backend_unsupported`.

**Reserved (inert).** These keys are parsed and stored but have no effect yet —
passing `true` must behave identically to the default:

- `include_global_context_index` (default `false`) and
  `global_context_index_top_k` (default `3`) — inert for `HYBRID_COMPLETION`
  until SDK-1045. `GRAPH_COMPLETION` reads them: it prepends the global
  context index `improve(build_global_context_index=true)` built.

The HTTP-facing degradation notes live with the server's search-router docs,
which are not part of this repository.

## Search — history

| Env var | `Settings` field | Default |
|---|---|---|
| `COGNEE_LOG_SEARCH_HISTORY` | `log_search_history` | `true` |

When on, each search records one query row and one result row. They carry the
searched dataset's `dataset_id` when exactly one dataset was searched.

## Improve (self-improvement loop)

The `improve` engine section is Python's `ImproveConfig` (`IMPROVE_*`).
`Settings::improve_config` resolves and checks it: a negative count, a
`feedback_alpha` outside (0, 1], an unknown stage name or the fatal
`persist_session_qa` in `stages_disabled` is an error. An explicit `improve()`
(the CLI verb, the bindings, `POST /api/v1/improve`) fails on it; the automatic
improve after a session `remember()` is skipped with a warning instead, so the
entry is still stored. A removed stage name (`triplet_enrichment`) is ignored
with a warning.

| Env var | Key | Default | Effect |
|---|---|---|---|
| `IMPROVE_AUTO_ENABLED` | `improve.auto_enabled` | `true` | Run `improve()` after a session `remember()` (detached), and the self-improvement pass of a permanent `remember()`. `false` is the kill switch on both paths; an explicit `self_improvement` does not override it. |
| `IMPROVE_DEBOUNCE_ENTRIES` | `improve.debounce_entries` | `1` | Auto-improve fires only once this many new session entries accumulated since the last one; `0` turns the entry trigger off. With `debounce_seconds` set, the default `1` steps aside (set `>= 2` to combine both). |
| `IMPROVE_DEBOUNCE_SECONDS` | `improve.debounce_seconds` | `0` | ... or once this many seconds passed; `0` turns the time trigger off. Checked only on the next `remember()`: there is no timer. |
| `IMPROVE_STAGES_DISABLED` | `improve.stages_disabled` | _(empty)_ | Comma-separated stage names skipped with `disabled_by_config`. |
| `IMPROVE_FEEDBACK_ALPHA` | `improve.feedback_alpha` | `0.1` | `feedback_weights` learning rate, in (0, 1], when a run passes none. |
| `IMPROVE_REVIEW_CONFLICTS` | `improve.review_conflicts` | `false` | Opt every run into `review_conflicts` (not ported yet: SDK-1038). |
| `IMPROVE_EFFECTIVE_DATE_KEY` | `improve.effective_date_key` | `effective_date` | Document metadata key of a source's date (read by `review_conflicts`; not ported yet). |
| `IMPROVE_TRACE_WINDOW_STEPS` | `improve.trace_window_steps` | `0` | Agent trace steps per persisted document; `0` = all pending. Not read yet (SDK-1047). |
| `IMPROVE_TRACE_WINDOWS_PER_RUN` | `improve.trace_windows_per_run` | `0` | Trace documents one run may cognify; `0` = drain. Not read yet (SDK-1047). |
| `IMPROVE_DISTILL_MAX_BATCHES_PER_RUN` | `improve.distill_max_batches_per_run` | `0` | Curator batches per distillation run; `0` = uncapped. Not read yet (SDK-1049). |

`IMPROVE_AUTO_ENABLED` and `IMPROVE_REVIEW_CONFLICTS` accept `true|1|yes|on`
and `false|0|no|off`; any other value is ignored with a warning.

### Feedback & personalization

Python's `BaseConfig` knobs, cognee-wide `Settings` fields. Each is
range-checked: out of range is an error from `ConfigManager::set` (and every
binding / `cognee-cli config set`), and is ignored with a warning from env.

| Env var | `Settings` field | Default | Effect |
|---|---|---|---|
| `DEFAULT_FEEDBACK_INFLUENCE` | `default_feedback_influence` | `0.0` | Blend weight of the learned feedback signal in graph search, in [0, 1]: the `feedback_influence` of a search request that sets none. |
| `PREFERENCE_ALPHA` | `preference_alpha` | `0.3` | How far one rating pulls a personal `prefers` weight, in (0, 1]. Read by improve's `update_user_preferences`. |
| `PREFERENCE_BETA` | `preference_beta` | `0.02` | Per-turn decay of an untouched `prefers` weight, in [0, 1); applied on read, by the preference update and by the retrievers. |
| `PERSONALIZATION_ENABLED` | `personalization_enabled` | `false` | Master switch of preference personalization: gates improve's `update_user_preferences` stage and the retrievers' preference read (graph and RAG completion). Off, nothing is written or read. |
| `PERSONALIZATION_INFLUENCE` | `personalization_influence` | `0.3` | The most a personal `prefers` weight may move a ranking score, in [0, 1] (graph triplet ranking, RAG chunk ranking). |

## Ontology

The `ontology` section is registered by the ontology-resolver kind. Resolvers are
factories (`OntologyResolverFactory`) on the `ComponentRegistry`, selected by
`ontology.resolver`; an external crate adds one with `register_ontology_resolver`.
With no file path the no-op resolver is used, whatever the resolver id.

| Env var | Key (alias) | Default |
|---|---|---|
| `ONTOLOGY_FILE_PATH` | `ontology.file_path` (`ontology_file_path`) | _(empty)_ |
| `ONTOLOGY_RESOLVER` | `ontology.resolver` (`ontology_resolver`) | `rdflib` |
| `ONTOLOGY_MATCHING_STRATEGY` | `ontology.matching_strategy` (`ontology_matching_strategy`) | `fuzzy` |

Only `rdflib` is built in; an unregistered resolver id with a file path set is an
error. `rdflib` implements only `fuzzy` matching and warns on any other strategy.
The CLI's `--ontology-file` overrides `ontology.file_path` for one run.

## File storage

The `storage` section is registered by the storage kind. Backends are factories
(`StorageFactory`) selected by `storage.backend`; an external crate adds one (an
object store, say) with `register_storage`.

| Env var | Key (alias) | Default |
|---|---|---|
| `STORAGE_BACKEND` | `storage.backend` (`storage_backend`) | `local` |
| `STORAGE_BUCKET_NAME` | `storage.bucket_name` (`storage_bucket_name`) | _(empty)_ |

Only `local` (files under `data_root_directory`) is built in. An unregistered
backend id is an error. No built-in backend reads `storage.bucket_name`.

## Graph extraction

The `graph_extraction` section is registered by the graph-extraction kind. It
picks the extractor cognify uses to turn chunks into graph facts.

| Env var | Key (alias) | Default |
|---|---|---|
| `GRAPH_EXTRACTOR` | `graph_extraction.backend` (`graph_extractor`) | `auto` |

`llm` is cognify's built-in LLM extractor. `auto`, the default as in Python,
picks `llm` when an LLM is usable and `gliner` otherwise (see
[Keyless cognify](#keyless-cognify)). `gliner_demo`, Python's name, means
`gliner`. Other kinds (GLiNER, …) are `GraphExtractorFactory`s that an external
crate registers with `register_graph_extractor`, together with their own config
sections. The setting is overridden per call by Python's `extractor` argument:
the bindings' `extractor` option and the CLI's `--extractor` flag on `cognify`,
`add-and-cognify` and `remember`. A binding's `graphBackend` cognify option (a
kind string, or `{"kind": …, …tuning}`) overrides both. A kind nothing is
registered under is an error; cognify never falls back to the LLM extractor
silently, and neither does `auto`.

Every operation that cognifies honours the setting: `cognify` and
`add-and-cognify` in the CLI and the bindings, and the operations that cognify
as a sub-step (`remember`, `improve`, and the bindings' `update`). Each one
resolves the extractor before any side effect, so an unregistered kind fails
before data is ingested or deleted. Library callers that build a `CognifyConfig`
themselves get the configured backend from
`ComponentManager::configured_graph_extractor` (or `graph_extractor` with a
per-call selection) and attach it with `with_graph_backend`;
`ComponentManager::resolve_graph_extractor` returns the resolved kind.

### Keyless cognify

With no usable LLM, cognify runs on local models instead of failing, as in
Python (`keyless_local_defaults_apply`):

* the `auto` graph extractor resolves to `gliner`. Nothing in this repository
  registers a `gliner` extractor, so a build without one fails cognify with an
  error naming the remedies: set `LLM_API_KEY`, register a `gliner` extractor,
  or set `GRAPH_EXTRACTOR` to a registered kind;
* an embedder with no embedding setting configured (provider and model at the
  stock default, no `EMBEDDING_API_KEY`, no `EMBEDDING_ENDPOINT`) runs on the
  local ONNX model (`onnx`, `BGE-Small-v1.5`, 384 dimensions; Python uses
  fastembed's `BAAI/bge-small-en-v1.5`). The model is downloaded on first use.

"Usable" is `ComponentManager::llm_available()` (see [LLM](#llm)): the
provider needs no key (`LlmFactory::requires_api_key` is `false`, as for
Bedrock and the mock LLM) or `LLM_API_KEY` is set. Without one, setup still
succeeds and only a call that needs the LLM (a completion search, the `llm`
extractor chosen explicitly) fails. Python also counts `llama_cpp`, Azure
managed identity and an MCP sampling session; this port has none of those
providers.

These settings keep the configuration as given (the LLM extractor and the
configured embedder), for runs with deliberately partial or fake provider
config:

| Env var | Key | Default |
|---|---|---|
| `COGNEE_SKIP_PREFLIGHT` | `skip_preflight` | `false` |
| `COGNEE_SKIP_CONNECTION_TEST` | `skip_connection_test` | `false` |
| `MOCK_EMBEDDING` | `embedding.mock` (`mock_embedding`) | _(empty)_ |

Python reads the same three env vars for this; it counts `MOCK_EMBEDDING` only
as `true`/`1`/`yes`, while here any mock mode (`zero`, `deterministic`, …)
counts. Python's settings report the effective embedder; here the configured
`embedding.*` values are what `config get` and the telemetry snapshot show,
while `ComponentManager::effective_embedding` (used by `cognee-cli export` to
label the vectors) reports the one in use.

### Custom graph schema (per call)

A custom graph model is a per-call option, not a setting: there is no env var
for it. Python exposes it the same way — `graph_model` is an argument of
`cognify()` (and a field of the HTTP cognify request), never an environment
variable — and so is `chunk_attachment`.

**The `graph_model` setting does not select it.** The key registry does carry a
`graph_model` row (`Settings::set_graph_model`, `ConfigManager::set("graph_model",
…)`, `cognee-cli config set graph_model …`, the bindings' config setters), but
nothing reads it: setting it succeeds and has no effect on cognify. That mirrors
Python, whose `config.set_graph_model` stores a value `cognify()` does not read.
Pass the schema per call instead, as below.

| Entry point | Graph schema | Chunk attachment |
|---|---|---|
| Rust (`cognee` / `cognee-cognify`) | `CognifyConfig::with_graph_schema(value)` (`parse_graph_schema` reads an object or a JSON string; `graph_spec_to_json_schema` compiles a spec) | `with_chunk_attachment(ChunkAttachment::…)` (`ChunkAttachment::parse`) |
| CLI `cognify`, `add-and-cognify` | `--graph-schema <FILE>` or `--graph-spec <FILE>` | `--chunk-attachment direct\|all` |
| Bindings (Python handle, TS, C, Java) | `graphSchema` opt (alias `graphModel`), or `graphSpec`: an object or a JSON string | `chunkAttachment` opt: `"direct"` or `"all"` |
| Python compat `cognify()` / `add_and_cognify()` | `graph_model=`: a Pydantic model class, a dict or a JSON string | `chunk_attachment=` |

`null` and `{}` mean the default `KnowledgeGraph`, as Python's HTTP route
treats an omitted or empty `graph_model`. Every entry point runs
`CognifyConfig::validate` before any side effect, so an invalid value (an
unknown attachment, an attachment without a schema, a schema that does not
compile, a schema with `temporal_cognify`, a `graph_backend` or the `cascade`
strategy) fails with a validation error. The binding opts are type-checked as
a whole: a wrongly-typed value for any known cognify option is an error, and an
unknown key is logged as a warning — Python forwards unknown `cognify()`
keywords into the LLM call instead of raising. See
[the custom graph schema guide](guides/custom-graph-schema.md).

## System paths, users & datasets

| Env var | `Settings` field | Default |
|---|---|---|
| `COGNEE_SYSTEM_ROOT_DIRECTORY` | `system_root_directory` | `./.cognee_system` |
| `COGNEE_DATA_ROOT_DIRECTORY` | `data_root_directory` | `./.data_storage` |
| `CACHE_ROOT_DIRECTORY` | `cache_root_directory` | `./.cognee_cache` (the default ONNX model and tokenizer live, and are downloaded, under `<cache_root_directory>/models`; until SDK-1070 they defaulted to `./target/models`) |
| `COGNEE_DEFAULT_USER_ID` | `default_user_id` | nil UUID |
| `COGNEE_DEFAULT_DATASET_NAME` | `default_dataset_name` | `main_dataset` (the dataset `cognee-cli add`/`add-and-cognify`/`remember`/`improve` and the Python compat `add`/`cognify`/`add_and_cognify` use without an explicit one; a blank value falls back to `main_dataset`) |
| `DEFAULT_USER_EMAIL` / `DEFAULT_USER_PASSWORD` | `default_user_email` / `default_user_password` | `default_user@example.com` / _(empty)_ (the password has no effect in this repository: the OSS build has no user accounts or login to give it to; Python uses it to make the default user loginable on its API server) |
| `ENABLE_BACKEND_ACCESS_CONTROL` | `enable_access_control` | `false` ([per-dataset storage](#per-dataset-storage); Python defaults to `true`) |

Setting `system_root_directory` cascades to the default `graph_file_path` and
`vector_db_url` unless those are set explicitly.

### Per-dataset storage

**`ENABLE_BACKEND_ACCESS_CONTROL` / `enable_access_control` gives every dataset
its own graph and vector database**, as Python does
([design](design/per-dataset-storage.md)). Off (the default), every dataset
shares one graph store and one vector store. On:

- A dataset's databases are provisioned on its first use by the selected
  dataset database handlers (`GRAPH_DATASET_DATABASE_HANDLER`,
  `VECTOR_DATASET_DATABASE_HANDLER`) and recorded in the `dataset_database`
  table — Python's table, so the relational database stays shared between the
  SDKs. Storage is namespaced by the dataset's **owner**: the embedded handlers
  keep `{system_root_directory}/databases/{owner_id}/{dataset_id}.lbug` and
  `…/{dataset_id}.lance.db`.
- The graph and vector engines the components hand out route every call to
  the dataset of the current `cognee_models::dataset_scope`. A call outside
  every dataset scope fails rather than reaching another dataset's data.
- Deleting a dataset drops its databases; prune drops every dataset's.
- The recorded embedding width is checked on every bind: switching to a model
  of another width fails with an error naming the dataset, the stored and the
  configured model.
- It is not authorization. Permission checks (an attached `AclDb`, or the
  ownership fallback) run in both modes.
- **Nothing is migrated.** Turning it on starts every dataset empty; re-add and
  re-cognify. Turning it off again goes back to the shared stores.

A configured provider without a handler is an error at the first component
build naming the handler, the provider and the supported handlers (Python's
`multi_user_support_possible`). An adapter crate registers its own with
`ComponentRegistry::register_dataset_database_handler`. The built-in handlers:

| handler | kind | providers | storage per dataset | Postgres privilege |
|---|---|---|---|---|
| `ladybug` / `kuzu` | graph | `ladybug`, `kuzu` | `{system_root}/databases/{owner_id}/{dataset_id}.lbug` (`.pkl` for `kuzu`) | — |
| `lancedb` | vector | `lancedb` | `{system_root}/databases/{owner_id}/{dataset_id}.lance.db` | — |
| `memory` | both | `mock`, `brute-force` | a fresh in-memory engine | — |
| `pgvector` (default for `pgvector`) | vector | `pgvector` | database `"{dataset_id}"` on the vector server, with the `vector` extension | `CREATEDB`, and the right to `CREATE EXTENSION vector` in a new database; Postgres 13+ |
| `pgvector_shared` | vector | `pgvector` | schema `ds_{dataset_id hex}` in the **relational** database, `search_path = ds_…, public` | `CREATE` on the relational database (and `vector` installed there, or the right to install it) |
| `postgres_graph` (default for `postgres`) | graph | `postgres`, `postgresql` | database `"{dataset_id}"` on the graph server | `CREATEDB`; Postgres 13+ |
| `postgres_graph_shared` | graph | `postgres`, `postgresql` | schema `ds_{dataset_id hex}` in the **relational** database, `search_path = ds_…` | `CREATE` on the relational database |

The Postgres handlers are compiled with the `pgvector` / `pggraph` features.
They store host, port, database and schema in `dataset_database`, never a
credential: every bind takes the username and password from the live
configuration — the resolved vector or graph URL for `pgvector` /
`postgres_graph`, the relational URL for the `_shared` handlers, which
therefore need a Postgres relational database. `CREATE DATABASE` runs on the
server's `postgres` maintenance database. Creation is idempotent and
serialized on the advisory locks Python takes, so concurrent first uses (in
any process) converge on one database. Deleting a dataset drops its database
(`DROP DATABASE … WITH (FORCE)`, which terminates sessions other processes
still hold on it, as Python does) or its schema (`DROP SCHEMA … CASCADE`).
Drops take the same advisory locks as creation, so a delete never interleaves
with another process's first bind of the same dataset. **The dedicated
handlers (`pgvector`, `postgres_graph`) need Postgres 13 or later** for
`WITH (FORCE)`; there is no fallback for older servers.

`ConfigManager::set_application_access_control` and
`HandleState::with_application_access_control()` are no longer needed for the
flag to be accepted (it used to be refused without them); they are kept so
existing callers compile. Every truthy spelling (`true`, `1`, `yes`, `on`, any
case) counts as `true`.

What an attached `AclDb` changes for search and recall:

- Explicit `dataset_ids` and dataset names are authorized against the caller's
  `read` grants (direct, tenant and role), counting only datasets in the
  caller's tenant (`SearchRequest::tenant_id`, strict equality: no tenant
  reads only tenant-less datasets), as Python's
  `get_all_user_permission_datasets` does.
- Under per-dataset storage, a request that names no dataset searches every
  dataset the caller may read, and returns an empty result when there is none
  (Python's ACL mode). Every such search needs the caller's user id; recall
  with an unknown caller gets no graph results.

How a search is scoped depends on storage, not on the ACL:

- **Per-dataset storage** (`ENABLE_BACKEND_ACCESS_CONTROL`): the retriever
  runs once per dataset, in that dataset's own databases —
  `SearchOrchestrator::search_datasets` returns one result per dataset
  (Python's `SearchResultPayload` list), `search` merges them (completions
  become one `Texts` answer per dataset; context, graphs and prompts are keyed
  by dataset id). Every search type works, top-k is per dataset, and a dataset
  with nothing to search fails the search only when every dataset does. Without
  an ACL, "every dataset the caller may read" is the caller's own datasets in
  its tenant.
- **Shared store** (the default): one unscoped search, whatever datasets the
  request names, after the permission check — Python's non-ACL mode. Nothing
  is filtered by dataset membership any more; the former post-filter could not
  be made tight, and the six types it refused (`TEMPORAL`, `CYPHER`,
  `NATURAL_LANGUAGE`, `CHUNKS_LEXICAL`, `FEELING_LUCKY`, `CODING_RULES`) run
  again. `SearchOrchestrator::refuses_dataset_scope` is always `false`.
  `CYPHER` and `NATURAL_LANGUAGE` stay gated by `ALLOW_CYPHER_QUERY`.

The per-dataset checks outside search — `DatasetManager` (`list_data`,
`has_data`, `empty_dataset`, `delete_data`), `AuthorizedDeleteService` and
`cognee::api::update` — use `AclDb::has_permission_in_tenant` with the caller's
(authenticated) tenant, strict equality. A grant on another tenant's dataset is
denied with the same error as no grant at all; under an ACL an unknown dataset
id is denied too, never "not found". Python answers these denials with
`PermissionDeniedError` (HTTP 403) on `list_data`, `has_data`, `empty_dataset`
and `update`, but `delete_data` re-raises it as `UnauthorizedDataAccessError`
(HTTP 401); the Rust layers return one permission-denied variant and leave the
status to the HTTP layer. `DatasetManager::list_datasets` and `delete_all`
count only grants on datasets in the caller's tenant (`delete_all` uses the
`delete` grants and deletes each listed dataset by id), and
`AuthorizedDeleteService`'s `DeleteScope::All` deletes the caller's
`delete`-authorized datasets in its tenant, like Python's `datasets.delete_all`.

Without an ACL, `DatasetManager::empty_dataset` / `delete_data` require the
dataset to be the caller's and in its tenant, then delete by id; `list_data` /
`has_data` require the same ownership and answer `NotFound` otherwise (unknown,
foreign and other-tenant ids alike — Python always authorizes these reads,
`datasets.py:155, 164`). Every by-name dataset lookup (`update`,
`AuthorizedDeleteService`, the plain `DeleteService`, search names, `forget`,
`add`, the CLI) resolves among the owner's datasets in the caller's tenant,
strictly — there is no tenant-loose name lookup — and the delete then runs by
id.

Deletes in more detail (SDK-936, Python @37f788bd2):

- The plain `DeleteService`'s owner-scoped scopes carry the tenant they
  resolve in: `DeleteScope::Dataset { tenant_id }` and
  `Data { dataset_name, tenant_id }` look the name up in that tenant only;
  `Data { dataset_name: None }` detaches the item only from the owner's
  datasets in that tenant; `User { owner_id, tenant_id }` covers the owner's
  datasets in that tenant only (`get_dataset_ids.py:29-34`).
- `DeleteScope::User` and `forget(ForgetTarget::All)` delete those datasets one
  after another **by id** and stop at the first failure: the datasets before it
  stay deleted and are reported in `DeleteError::Partial`. Like Python they do
  not delete search history (`datasets.py:316-323`, `forget.py:191-221`); the
  history table has no tenant, so a per-tenant delete could not scope it. Unlike Python, `forget(All)` does not prune the whole
  session cache. `DeleteService::execute_datasets_by_id` is the shared
  implementation.
- Under an ACL (`AuthorizedDeleteService`), an owner-scoped scope whose
  `tenant_id` is not the caller's tenant is denied, and a data item without its
  dataset (`Data { dataset_name: None }`) is refused with a validation error —
  Python has no dataset-less item delete (`datasets.py:218-224`,
  `forget.py:161-162`). A grant reaches another user's dataset only by id: a
  by-name scope whose owner is not the caller is denied before any lookup.
- A data item with no `Data` row inside an empty dataset deletes that dataset
  when `delete_dataset_if_empty` is set (`datasets.py:259-273`).
- `cognee-cli delete --all` without an ACL deletes the caller's datasets in its
  tenant (`--tenant-id`) — Python's `datasets.delete_all(user)` — not the whole
  store. The library's plain `DeleteScope::All` is still the store-wide wipe.

The ownership fallback that replaces the ACL — explicit
`dataset_ids`, dataset names and `DatasetManager::list_datasets` — also keeps
only datasets whose tenant equals the caller's: a tenant-less caller (`None`)
sees only tenant-less datasets (Python `get_dataset_ids.py:34`,
`get_all_user_permission_datasets.py:49-53`).

> **Migrating from Python:** Python's `.env.template` ships an uncommented
> `ENABLE_BACKEND_ACCESS_CONTROL=True`, and Python turns it on by default. A
> `.env` copied from it (cognee-rust loads `.env` automatically) turns on
> per-dataset storage here too.

## Session / cache & rate limiting

The session (Q&A history) store is chosen by the `session` section, registered by the
session-store kind. Stores are `SessionStoreFactory`s on the `ComponentRegistry`,
selected by `session.store`; an external crate adds one with `register_session_store`.
A factory is handed the shared relational database. Built in:

* `relational`, also registered as `seaorm`: the session tables in the relational
  database (any `RelationalDb`). This is the default.
* `fs` (crate feature `session-fs`): JSON files under `session.fs.directory`.
* `redis` (crate feature `session-redis`): Python's Redis layout, at `session.redis.*`.

A store compiled out by its feature brings no keys: in a default build the `CACHE_HOST`
… variables are not read, and setting `cache_host` is an unknown key. An unregistered
store id is an error.

> **Behaviour change for `cognee-cli` and the bindings.** `cognee-cli`
> (`search`/`recall`/`remember`/`improve`/`bench`) and the Python/TS/Java/C bindings
> used to build the relational store unconditionally and ignore
> `COGNEE_SESSION_STORE`. They now honour `session.store` like every other entry
> point — but they ship only the relational store (neither `session-fs` nor
> `session-redis` is enabled in their builds). So `fs` or `redis`, values the HTTP
> server accepts, are now an error there ("unsupported session_store …"). If such a
> process shares the server's environment, unset `COGNEE_SESSION_STORE` or set it to
> `relational` for it.

| Env var | Key (alias) | Default |
|---|---|---|
| `COGNEE_SESSION_STORE` | `session.store` (`session_store`) | _(empty: `CACHE_BACKEND`, else `relational`)_ |
| `COGNEE_SESSION_DIR` | `session.fs.directory` (`session_directory`; `session-fs` builds only) | `<system_root>/sessions` |
| `CACHE_HOST` / `CACHE_PORT` | `session.redis.host` / `.port` (`cache_host` / `cache_port`; `session-redis` builds only) | `localhost` / `6379` |
| `CACHE_USERNAME` / `CACHE_PASSWORD` | `session.redis.username` / `.password` (`cache_*`; `session-redis` builds only) | _(empty)_ |
| `COGNEE_FEEDBACK_LLM_TIMEOUT_MS` | `session.feedback_llm_timeout_ms` | `8000` (trace-step feedback LLM call) |
| `SESSION_ABANDON_AFTER_SECONDS` | `session.abandon_after_seconds` (`session_abandon_after_seconds`) | `1800` (idle running session reads as `abandoned`) |
| `CACHE_BACKEND` | `session.cache_backend` (`cache_backend`): used only while `session.store` is empty; an id this build lacks falls back to `relational` with a warning | _(empty)_ |
| `SESSION_TTL_SECONDS` | `session.ttl_seconds` (`session_ttl_seconds`) | `604800` (7d); `0` or a negative value never expires |
| `CACHING` | `session.caching` (`enable_caching`, `caching`) | `true` |
| `LLM_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | `llm.rate_limit_*` (see [LLM](#llm)) | `false` / `60` / `60` |
| `EMBEDDING_RATE_LIMIT_ENABLED` / `_REQUESTS` / `_INTERVAL` | … | `false` / `60` / `60` |
| `AUTO_RATE_LIMIT` | `llm.auto_rate_limit` (`auto_rate_limit`) | `true` |

**Python's session-cache settings.** Python selects its session cache with
`CACHE_BACKEND` (`redis`, `fs`, `sqlite`, `postgres`, `turso`, `tapes`; default
`sqlite`). Here it binds `session.cache_backend`, a fallback for
`session.store` (`COGNEE_SESSION_STORE`): it is used only while `session.store` is
empty, and an explicit `session.store` always wins. `sqlite` and `postgres` select
the `relational` store (Rust keeps sessions in the relational database, not a
separate `cache.db`); `fs` and `redis` need their build features, and `turso` /
`tapes` are not built in. Unlike `session.store`, a `CACHE_BACKEND` value naming a
store this build does not provide is **not** an error: it is logged as a warning
and the `relational` store is used, so a `.env` shared with Python (which documents
`CACHE_BACKEND=redis` next to `CACHE_HOST`) cannot take down a stock `cognee-cli`
or binding, none of which ship the `fs` or `redis` store. To insist on a store and
fail when it is missing, set `session.store` (`COGNEE_SESSION_STORE`) instead.
Both unset, the store is `relational`.

`session.ttl_seconds` expires a session's entries, trace steps and session-context
rows that long after its last write, as Python does. The `redis` store sets a
key expiry; the `fs` store deletes an expired file when it is next read. The
`relational` store does not expire sessions: Python's SQL cache keeps an expiry
per row, which the Rust session tables have no column for.

`session.caching = false` (`CACHING=false`) switches session memory off, as in
Python: whatever the store, session reads are empty, a search answers without
session history and stores nothing, and `auto_feedback` has no effect.
`remember(data, session_id=…)` still succeeds and only logs "session cache not
available", as Python's `_add_to_session` does; a typed entry (`remember_entry`:
Q&A, trace or feedback) fails with "session memory is disabled" before doing any
work (no `session_records` upsert, no feedback LLM call), as Python's
`_dispatch_session_entry` raises "Session cache unavailable".

A negative `SESSION_TTL_SECONDS` is accepted and, like `0`, never expires a
session, as in Python, whose cache adapters set an expiry only for a value `> 0`.

### How retries and pacing interact

These knobs form one resilience stack, matching Python cognee's:

- **Two retry counts, one per loop.** `LLM_MAX_RETRIES` (Python's
  `_MAX_VALIDATION_RETRIES`) is the number of *attempts* at one structured-output
  call — the first ask plus its corrective re-asks, so the default `3` is three
  attempts and at most two re-asks. A reply that fails validation, arrives
  truncated, or is rejected by the caller's validator is asked again with the
  reason in context. Every adapter floors it at `1`, so `0` means one attempt and
  no re-ask. `LLM_NETWORK_RETRIES` (Python's `stop_after_attempt(2)`) is the
  transport ladder underneath it, for 429s, 5xx and connection failures. They are
  separate knobs because they *nest*: one value feeding both multiplied the
  ladders into each other — `attempts x (network retries + 1) x
  LLM_REQUEST_TIMEOUT_SECONDS`, which is an hour of wall clock for a single chunk
  at defaults that look modest.
- **Retrying stops only once BOTH floors are met** — at least
  `LLM_NETWORK_RETRIES`
  attempts *and* at least `LLM_MIN_RETRY_SECONDS` elapsed. The time floor is the
  one that matters: an attempt count alone gives up in seconds, long before a
  provider's rate-limit window resets. Backoff is exponential with jitter,
  8s doubling to a 128s ceiling, and a `Retry-After` header is honoured (capped
  at 60s) when the provider sends one. Set `LLM_MIN_RETRY_SECONDS=0` to fail
  fast on attempts alone. The elapsed time measured against this floor excludes
  time spent queued for an in-flight slot (the
  `LLM_MAX_PARALLEL_REQUESTS` ceiling): the floor is a "keep
  retrying for at least this long" guarantee, so counting queue time against it
  could only ever end the ladder earlier, and a call that waited minutes for a
  slot would give up having barely retried at all.

  ⚠️ **Bedrock makes one attempt more.** Its attempt floor is
  `LLM_NETWORK_RETRIES + 1` — so `2` buys three attempts there against two
  elsewhere — preserving the `0..=LLM_NETWORK_RETRIES` ladder that predates this
  knob. The time floor now applies there like everywhere else: Bedrock used to
  hold none at all, so `LLM_MIN_RETRY_SECONDS` did not reach it and a throttle
  window the other providers ride out for 240s failed the item after ~30-60s,
  which under the default whole-run rollback discards the run. That was left
  standing deliberately until the aggregate deadline below reached this adapter,
  since the floor is a "keep retrying" guarantee and needs something to bound it
  from above; the deadline outranks it, as on every other provider. Bedrock does
  still ignore `Retry-After`, so its gaps are always the 8s-to-128s ladder.
- **Three timeouts, three scopes.** `LLM_CONNECT_TIMEOUT_SECONDS` bounds the TCP
  handshake (`reqwest` sets none by default, so a black-holed connect used to
  burn the whole request timeout without sending a byte).
  `LLM_REQUEST_TIMEOUT_SECONDS` bounds one HTTP request. `0` means "no limit"
  for both of those, matching curl — note that this is *not* the same as passing
  a zero duration to the HTTP client, which would time every request out
  instantly, so the zero case is handled explicitly. Both apply to the
  OpenAI-compatible (including Azure), Anthropic and Bedrock adapters; the
  Responses client still uses a fixed 600s request / 10s connect pair.
  `LLM_REQUEST_DEADLINE_SECONDS` bounds one *logical* structured-extraction
  call — and that last one is the only bound on total time. Structured
  extraction makes `LLM_MAX_RETRIES` attempts, each running a transport ladder
  that honours the `LLM_MIN_RETRY_SECONDS` floor above; multiplied out, the
  designed worst case runs past an hour while every individual request finishes
  well inside its timeout. Keep the deadline above
  `LLM_MAX_RETRIES x LLM_MIN_RETRY_SECONDS` — 720s at the defaults — or it will
  cut the rate-limit-window waits the floors exist to protect, and raise it
  alongside either factor if you change them.
  **Also keep it above 900s**, and here is the reason the ladder alone does not
  tell you: this clock starts at the head of the logical call and *includes*
  time spent paced and queued for an `LLM_MAX_PARALLEL_REQUESTS` slot. (The
  retry floor above deliberately excludes that time; the deadline deliberately
  counts it, because it is time the caller is blocked.) A provider 429/503 opens
  a **900-second** pacing episode, so a deadline at or below that is spent before
  the episode closes and every call queued behind one fails after a single
  dispatch — converting the paced recovery into hard failures. The `1200`
  default clears the cooldown with the 720s ladder still inside it. A warning naming the computed ladder is logged at
  startup if the deadline does not fit inside it. The three-mode cascade below
  is deliberately not a factor in that ladder: it is endpoint-capability
  discovery, memoised per endpoint, so a healthy endpoint pays for one mode per
  call. The deadline gates *starting* new work rather
  than cancelling in flight, so the true ceiling is
  `LLM_REQUEST_DEADLINE_SECONDS + LLM_REQUEST_TIMEOUT_SECONDS`. "Starting work"
  includes the wait in front of a send: on every retry, both the pacing wait and
  the wait for an `LLM_MAX_PARALLEL_REQUESTS` slot are bounded by whatever is
  left of the budget, so a retry cannot park in a 900-second cooldown on top of
  a ceiling it has already reached. A retry whose backoff was clamped to the
  last of the budget keeps its right to *dispatch* — it sends if nothing makes
  it wait — but not to wait. The **first** attempt of a call is exempt and may
  wait out a whole episode, because a call that never dispatches cannot report
  what the provider said. Set it to `0` to restore the previous unbounded
  behaviour.
- **The cascade stops sending an endpoint a mode that never produces anything.**
  The three request shapes above exist because OpenAI-compatible servers vary in
  what they accept, but tool calling and legacy `functions` both need a
  server-side parser. An endpoint without one — vLLM started without
  `--enable-auto-tool-choice --tool-call-parser` is the usual case — answers 200
  with the text in `message.content` and populates neither field, so those two
  shapes can never produce anything and a stateless cascade re-paid both retry
  ladders on every call. Measured on one such deployment: 4.14 API calls per
  extraction against 1.09 on an endpoint with a parser, with roughly 71% of
  output tokens and cost producing nothing.

  The OpenAI-compatible adapter now remembers, per endpoint and **per mode**,
  whether a shape is worth sending. A mode is skipped after three consecutive
  structured calls in which it ran out of attempts having produced nothing
  usable *and* every response that arrived carried no native payload — no
  `tool_calls` / `function_call` at all, or one whose `arguments` was blank. One
  non-blank native payload clears the count even if it then fails to parse: that
  is a deliberate false-positive guard, since a model returning occasional
  malformed JSON would otherwise get its mode disabled for 64 calls, and JSON
  mode sends only a schema *template* rather than the real schema, so a wrong
  skip costs extraction quality. Malformed output is the corrective-retry
  ladder's job. The corollary is a known gap: an endpoint whose native payload
  arrives non-blank but *never* parses is not caught, and re-pays the cascade
  every call. Tool calling and legacy `functions` are tracked separately, because
  the cascade exists precisely because a server may accept one and not the other;
  JSON mode is never skipped, being the terminal fallback and the only shape that
  needs no server-side parsing.

  Two things deliberately do **not** count as evidence against a mode. A request
  that *errored* — HTTP 4xx/5xx, a connection reset, a deadline abort — says
  nothing about the endpoint's parser, so a brief gateway hiccup cannot downgrade
  a healthy endpoint. And a truncated response says only that the output budget
  was too small, which `LLM_ARGS` and the truncation retry already handle. Any
  call a mode *answers* clears its count, including one whose payload arrived in
  `content` rather than as a native tool call: on a parser-less endpoint that
  echoes usable JSON, tool-calling mode succeeds on its first attempt and is the
  cheapest path available, so it is kept.

  A skipped mode is re-probed once every 64 calls, so a server that gains a
  parser (a redeploy behind a stable URL) recovers without a restart; the
  residual waste is ~1.6% of calls. There is no setting to configure — this is a
  property of the endpoint, observed rather than declared — and the transition is
  logged (`warn` for tool calling, naming the vLLM flags) so a mode downgrade is
  visible rather than silent. Python needs no equivalent because it has no
  cascade: it pins one instructor mode per provider from a static table and stays
  there.
- **Concurrency and rate are separate ceilings.** `LLM_MAX_PARALLEL_REQUESTS`
  bounds how many LLM requests are in flight at once; the `LLM_RATE_LIMIT_*`
  bucket below bounds how often they *start*. Neither substitutes for the other —
  a rate limit cannot stop a thousand simultaneous sockets, and a semaphore
  cannot stop a steady stream that exceeds a quota. The concurrency ceiling is
  enforced inside the LLM transport, so it applies on every surface including
  the HTTP server, whose cognify routes have no per-request knob of their own.
  A slot is held for the HTTP exchange itself and released while a request waits
  — for its pacing turn or for a retry backoff — because a waiting request holds
  no socket; counting waiters would let a 15-minute overload episode stall every
  other LLM caller in the process behind requests that are merely sleeping.
  It defaults to 1000, matching the connection-pool limit `openai-python` and
  `litellm` set, and to 128 on Android/iOS where a 1024-descriptor budget is
  shared with the graph, vector and relational stores.
- **Pacing is off until it is needed.** With `LLM_RATE_LIMIT_ENABLED=false`
  (the default) requests dispatch unbounded up to that concurrency ceiling. When the provider reports overload
  — HTTP 429, 503, 529, or a timeout — `AUTO_RATE_LIMIT` (default on) opens an
  *episode*: for the next 15 minutes dispatch is paced at
  `LLM_RATE_LIMIT_REQUESTS` per `LLM_RATE_LIMIT_INTERVAL` seconds. Fresh
  overload extends the episode; it lapses on quiet. Set `LLM_RATE_LIMIT_ENABLED=true`
  to pace from the start, or `AUTO_RATE_LIMIT=false` to never pace automatically.
- **Some failures are never retried,** because no wait can fix them: bad request,
  authentication, unknown model, payment required, and a 429 whose body reports
  exhausted quota or billing rather than a per-minute limit.

Embedding requests have their own `EMBEDDING_RATE_LIMIT_*` bucket, which is
flag-gated only — provider overload does not switch it on automatically.

## Logging

> **Canonical table.** Binding READMEs and `.env.example` link here. cognee writes
> structured logs to **stdout** and (when writable) to a rotating file. File logging
> is owned by [`cognee-logging`](../crates/logging/): the CLI installs it at startup,
> the language bindings when you call their `setup_logging` entry point.
>
> **Exception — machine-readable CLI output.** `cognee-cli` sends console logs to
> **stderr** for any invocation whose stdout is a contract with a program:
> `search`/`recall` with `-f json` or `-f simple`, and `export`, `visualize` and
> `bench`, which print a path or a JSON document. Logs on stdout make those
> unparseable (a leading timestamp is read as a JSON number). Human-facing
> `pretty` output, and every other subcommand, still log to stdout.

Logging configures a process-wide singleton (the `tracing` subscriber), so it is
**process config**: one [`LoggingConfig`](../crates/logging/src/config.rs), resolved
once, at startup. Its only window onto the environment is
`LoggingConfig::from_env_source` (the `from_env()` boundary). Every field is also
settable from code: assign it, or use `LoggingConfig::set(key, value)` with the
option keys below.

| Env var | Option key | Default | Purpose |
|---|---|---|---|
| `COGNEE_LOG_FILE` | `log_file` | `true` | Master file-logging toggle (`false`/`0`/`no` disables). |
| `COGNEE_LOGS_DIR` | `logs_root_directory` | `~/.cognee/logs` | Log directory (falls back to `/tmp/cognee_logs` if unwritable). |
| `COGNEE_LOG_FORMAT` | `log_format` | `plain` | `plain` (Python-compatible) or `json`. Applies to stdout + file. |
| `COGNEE_LOG_ROTATION` | `log_rotation` | `daily` | `daily` / `hourly` / `minutely` / `never`. |
| `COGNEE_LOG_BACKUP_COUNT` | `log_backup_count` | `5` | Rotated files retained by age. |
| `COGNEE_LOG_MAX_FILES` | `log_max_files` | `10` | Hard cap on retained log files. |
| `COGNEE_LOG_MAX_BYTES` | `log_max_bytes` | _(unset)_ | Accepted for Python parity, ignored (size-based rotation is not supported); a warning is logged. |
| `LOG_FILE_NAME` | `log_file_name` | _(timestamped)_ | Override the log file name. |
| `RUST_LOG` / `LOG_LEVEL` | `log_level` | cognee's default filter | Level filter (`RUST_LOG` preferred). The default is `info` with noisy libraries (`ort`, `reqwest`, `sqlx`, …) at `warn`. |
| — | `console_stream` | `stdout` | `stdout` or `stderr`. |

**Precedence:** defaults < environment < options from code.

* **CLI.** The CLI loads `Settings` before it installs logging, so the two
  `Settings` keys `log_level` and `logs_root_directory` (default empty: use the
  logging default) are lowered into the logging config. Each fills only what the
  logging environment left unset: `RUST_LOG` > `LOG_LEVEL` > the config file's
  `log_level` > the default filter, and `COGNEE_LOGS_DIR` > the config file's
  `logs_root_directory` > `~/.cognee/logs`. So `cognee-cli config set log_level debug`
  persists a level.
* **Bindings.** A binding installs logging from a module-level function that runs
  before any handle exists, so a handle's `Settings` (and `config.set("log_level", …)`)
  do **not** affect logging there. Pass the options to the entry point instead; they
  win over the environment. The first call wins, later calls are no-ops.

| Binding | Entry point |
|---|---|
| Rust | `cognee_logging::init_logging(LoggingConfig::from_env()?, layers)`; set fields or call `cfg.set(key, value)` first |
| Python | `cognee_py.setup_logging({"log_level": "debug", "logs_root_directory": "/var/log/cognee"})` |
| TypeScript | `setupLogging({ logLevel: "debug", logsRootDirectory: "/var/log/cognee" })` |
| Java | `Cognee.setupLogging(Map.of("log_level", "debug"))` |
| C | `cognee_setup_logging_with_options("{\"log_level\":\"debug\"}")` |

Option keys are snake_case; the bindings also accept camelCase (`logLevel`). An
unknown key or an invalid value is an error (Python `ValueError`, a JS exception,
`CogneeException` with `VALIDATION_ERROR`, C return code `2` with
`cg_last_error_message()`). The default bridge subscriber a binding installs on
import (into Python `logging`, or stderr) reads its level filter through the same
`LoggingConfig` boundary (`RUST_LOG` > `LOG_LEVEL` > default).

> **Multi-process warning** — when several cognee processes share one log file via
> `LOG_FILE_NAME`, rotation is not coordinated; concurrent rotation can corrupt the
> log. For sharded workers, give each shard its own `COGNEE_LOGS_DIR` (or unset
> `LOG_FILE_NAME` per shard).

## Observability & telemetry

cognee emits OpenTelemetry traces (behind the `telemetry` feature) and opt-out
product analytics. The **deep references** are
[observability/opentelemetry.md](observability/opentelemetry.md) and
[observability/send_telemetry.md](observability/send_telemetry.md).

### OpenTelemetry traces

| Env var | `Settings` / option key | Default | Purpose |
|---|---|---|---|
| `COGNEE_TRACING_ENABLED` | `cognee_tracing_enabled` | `false` | Activate OTLP trace export. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `otel_exporter_otlp_endpoint` | _(empty)_ | OTLP collector endpoint (non-empty also activates tracing). |
| `OTEL_SERVICE_NAME` | `otel_service_name` | `cognee` (CLI); `cognee.<binding>-binding` in a binding | Service name attribute. |
| `OTEL_EXPORTER_OTLP_HEADERS` / `_PROTOCOL` | `otel_exporter_otlp_headers` / `otel_exporter_otlp_protocol` | _(empty)_ / `grpc` | Exporter headers / protocol. |
| `OTEL_SPAN_PROCESSOR` | `otel_span_processor` | `batch` | `batch` or `simple`. |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `otel_traces_sampler` / `otel_traces_sampler_arg` | _(empty)_ | Sampler selection. |
| `ENV` | `otel_deployment_environment` | `development` | The `deployment.environment.name` resource attribute. |
| `MONITORING_TOOL` | `monitoring_tool` | `none` | Python's observer selection. Only `none` is supported, as in Python, whose `get_observe` rejects every other tool: setting another is an error, and from env it is ignored with a warning. Tracing goes through the OpenTelemetry settings above. |

**Precedence:** defaults < environment < code. The exporter is process-wide too, but
where it reads its settings depends on who initialises it:

* **CLI** — from `Settings` (defaults < config file < env < `--flags`), so
  `cognee-cli config set otel_exporter_otlp_endpoint …` works.
* **Rust embedders** — `cognee::telemetry::init_telemetry(&settings)` takes any
  `SettingsView`: a `Settings`, or an `EnvSettingsView` (env only, plus
  `EnvSettingsView::set(key, value)` from code).
* **Bindings** — from a module-level function that runs before any handle exists,
  so a handle's `Settings` do not apply. The binding's default service name
  (`cognee.python-binding`, `cognee.node-binding`, `cognee.java-binding`,
  `cognee.capi-binding`) is a *default*: `OTEL_SERVICE_NAME` and an explicit
  `otel_service_name` option both win over it, and the process environment is never
  modified. Options use the `Settings` key names above (camelCase also accepted):

| Binding | Entry point |
|---|---|
| Python | `cognee_py.setup_telemetry({"otel_exporter_otlp_endpoint": "http://collector:4317"})` |
| TypeScript | `setupTelemetry({ otelExporterOtlpEndpoint: "http://collector:4317" })` |
| Java | `Cognee.initOtlp(Map.of("otel_exporter_otlp_endpoint", "http://collector:4317"))` |
| C | `cognee_init_otlp_with_options("{\"otel_exporter_otlp_endpoint\":\"http://collector:4317\"}")` |

### Product analytics

Product analytics are **on by default** (opt-out, matching the Python SDK). The
switch is **process-wide**: one
[`TelemetryConfig`](../crates/telemetry/src/config.rs) governs every event the
process sends, whichever handle emits it. It is not a `Settings` key and not a
`cognee-cli config` key.

| Env var | `TelemetryConfig` field | Default | Purpose |
|---|---|---|---|
| `TELEMETRY_DISABLED` | `enabled` = `false` when set to any non-empty value | _(unset)_ | Opt out. |
| `ENV` | `enabled` = `false` when `test` or `dev` | _(unset)_ | Opt out in test/dev environments. |
| `COGNEE_HOST_SDK` | `host_sdk` | _(unset)_ | Set by a host SDK that embeds a binding and sends its own analytics; suppresses that binding's events (not the CLI's) to avoid double counting. |
| `TELEMETRY_REQUEST_TIMEOUT` | `request_timeout_secs` | `5` | HTTP timeout, clamped to `[1, 60]`. |
| `TRACKING_ID` | `tracking_id` | _(unset)_ | Fixed `anonymous_id`. |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `api_key_tracking_salt` | well-known default | PBKDF2 salt for `api_key_tracking_id`. |
| `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` | `endpoint_override` | _(unset)_ | Collector override for test harnesses; honoured only while `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty. |

**Precedence:** defaults (on) < environment < code. The environment is read when an
event is sent, so a `.env` loaded when the first handle is built, or a variable set
later, is honoured. An explicit call wins over the environment in both directions
and stays in force until the next one: `set_enabled` pins only the switch,
`configure` pins every field, `reset_to_env` returns to the environment. Disabling
also drops events that are queued but not yet sent.

| Surface | Turn analytics off | Query |
|---|---|---|
| Rust | `cognee::telemetry::set_enabled(false)`, or `configure(TelemetryConfig { .. })` | `cognee::telemetry::is_enabled()` |
| CLI | `cognee-cli --no-telemetry <command>` (or `TELEMETRY_DISABLED=1`) | — |
| Python | `cognee_py.set_telemetry_enabled(False)` | `cognee_py.is_telemetry_enabled()` |
| TypeScript | `setTelemetryEnabled(false)` | `isTelemetryEnabled()` |
| Java | `Cognee.setTelemetryEnabled(false)` | `Cognee.isTelemetryEnabled()` |
| C | `cognee_set_telemetry_enabled(0)` | `cognee_is_telemetry_enabled()` |

The existing `setup_telemetry_analytics()` / `setupTelemetryAnalytics()` /
`Cognee.initTelemetry()` / `cognee_init_telemetry()` arm the binding (which
importing it already does) and report the same state as the query functions.
Building without the `telemetry` cargo feature compiles analytics out entirely;
the switch still exists and `is_enabled()` is always `false`.

The `api_key_tracking_id` in each event is a salted hash of the **resolved** LLM key
(`llm.api_key`, from env, a config file or code). Every `ComponentManager` holds a
`cognee::telemetry::LlmKeyRegistration` for its key, updated when its configuration
changes and dropped with the manager; embedders without a manager call
`cognee::telemetry::set_llm_api_key`. The `LLM_API_KEY` variable is not read by the
analytics client itself. When every live registration uses the same key, its hash is
reported; when live handles use *different* keys, process-wide events cannot be
attributed to one of them and the id is reported empty rather than mislabelled.

*Divergence from Python:* Python hashes `os.getenv("LLM_API_KEY")` only. Rust hashes
the key it actually uses, which can also come from its `OPENAI_TOKEN` alias, a config
file or code. The derivation is identical (PBKDF2-HMAC-SHA256, same salt), so the same
key gives the same id in both SDKs, which is what cross-SDK grouping needs; a Rust
process configured only through `OPENAI_TOKEN` reports an id where a Python process
without `LLM_API_KEY` (and so without a working LLM) reports none.

## HTTP server

The server binary is not part of this repository. It reads its own env surface
(host/port, auth, body limits, pipeline registry, notebooks, health probes),
which is documented with the server rather than here; see
[tools/http-server.md](tools/http-server.md). Note that the `HYBRID_COMPLETION`
tuning knobs are **not** part of the HTTP wire surface — see
[Search — hybrid retriever knobs](#search--hybrid-retriever-knobs).

## Cloud

Cloud/Auth0 configuration (`COGNEE_CLOUD_URL`, `COGNEE_AUTH0_*`) and the
`serve()`/`disconnect()` flow are not part of OSS.

## Runtime configuration API

`ConfigManager` (`Arc<RwLock<Settings>>`) is the runtime API that the bindings
and the CLI use.

* **Generic setter:** `set(key, value)` accepts **every key in the
  [key registry](../crates/lib/src/config/keys.rs)** (each `Settings` field name,
  plus aliases such as `embedding_model`) and **every key of a registered component
  section**, canonical or alias (see [Component config sections](#component-config-sections)).
  An unknown key follows [strict mode](#unknown-keys-and-strict-mode). `get(key)`
  reads any of them. A JSON `null` clears the optional
  settings (`chunk_size`, `llm_temperature`, `single_process`,
  `summarization_schema`). Setting `system_root_directory` also moves the graph
  and vector paths derived from the old root.
* **Typed setters** for the common keys, in the families `set_llm_*`,
  `set_embedding_*`, `set_vector_db_*`, `set_graph_*`, `set_chunk_*`,
  `set_relational_db_*`, `set_*_root_directory`, `set_ontology_*`,
  `set_classification_model` / `set_summarization_model` /
  `set_summarization_schema`.
* **Bulk setters:** `set_llm_config`, `set_embedding_config`,
  `set_vector_db_config` and `set_graph_db_config`.
* **Introspection:** `read()`, `version()`, `get_settings()` (secrets masked).
  `Settings::get_key` / `set_key` / `reset_key` work on a plain `Settings`,
  `cognee::config::keys()` lists the registry, and `ConfigManager::schema()` the
  component sections it knows. `ConfigManager::apply_patch` applies a settings
  object in bulk, which is how the C and Python constructors take theirs.

Full signatures are in the [`ConfigManager` rustdoc](../crates/lib/src/config.rs).
The binding ergonomics (granular JS setters vs generic `set` in Python/C) are
documented in [tools/bindings.md §config](tools/bindings.md#configuration).

## CLI `config` subcommand

`cognee-cli config get|set|unset <key>` reads/writes the persisted JSON file.
The settable keys are the registry keys whose value is saved in that file
(every `Settings` field except `llm_args` and `summarization_schema`), plus the
keys of the component sections compiled into this build. `cognee-cli config list`
prints them. Example:

```bash
cognee-cli config set llm_max_retries 4
cognee-cli config get llm_model
cognee-cli config unset embedding_endpoint
```
