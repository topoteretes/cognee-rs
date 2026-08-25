# AWS Bedrock provider — implementation plan

Status: **landed in this repository; one upstream item outstanding** · Scope:
**OSS `cognee-rs`**, plus a small set of Python-side edits for two-way parity ·
Tracking: issue **#17** (the remaining tier; Tier 2 = Anthropic, Tier 3 = Azure,
both shipped earlier).

| Item | State |
|---|---|
| R1 — `aws/` module (env, region, endpoint, credentials, signer, transport) + the `bedrock` feature and pinned AWS deps | ✅ `b8755114` |
| R2 — `AwsInputs` on `BackendBuildContext`, both lowering sites | ✅ `c81a049f` |
| R3 — `BedrockAdapter` over Converse | ✅ `950d9827` |
| R4 — `BedrockEmbeddingEngine` + provider plumbing | ✅ `ea4d47f0` |
| R5 — `BedrockLlmFactory` + registry registration and drift guard | ✅ `287208d0` |
| R6 — feature-wiring verification | ✅ folded into R1/R5 (§6.8) |
| R7 / P2 — `/settings` `Bedrock` DTO variant, router arm, refreshed model list | ✅ `b76a216d` |
| R8 — test suites (`crates/llm/tests/bedrock_*.rs`, embedding + `caps`/`route` unit tests) | ✅ landed inside `b8755114` / `950d9827` |
| P3 — `docs/http-server/routers/settings.md` asymmetry retired | ✅ `8a89ed43` |
| P4 — `e2e-cross-sdk/harness/test_http_settings.py` | ✅ `28afb533` |
| §8 — docs landing (`configuration.md`, `not-implemented.md`, crate READMEs, `architecture.md`) | ✅ this pass |
| **P1 — Python `Literal["bedrock"]` on `LLMConfigInputDTO.provider`** | ⬜ **open — upstream `topoteretes/cognee`, cannot land from this repo** |
| P6 — raise Python's `transcribe_image` to match Rust (§6.5) | ⬜ optional, not required for parity |

**Why this doc stays in the roadmap folder.** P1 is still outstanding upstream,
and it is the tracker that `docs/http-server/routers/settings.md` §6.4 and the
`xfail(strict=True)` case `test_settings_post_bedrock_accepted_by_python` in
`e2e-cross-sdk/harness/test_http_settings.py` both point at. Several source files
also cite this path — including a user-visible `LlmError` message in
`crates/llm/src/adapters/bedrock/mod.rs` (§6.7) — so §1 (the wire spec) and §6
(decisions and caveats) are load-bearing references, not historical notes. Delete
the doc only once P1 has landed upstream **and** those references have been
re-pointed.

Parity baseline: Python `cognee` and the `litellm` it depends on, read directly
rather than assumed. §1 is the wire spec; everything after implements it.

> **Review pass (2026-08-24).** §1 was audited line-by-line against
> `topoteretes/cognee` and `BerriAI/litellm` at HEAD. The spec held, with three
> corrections now folded in: the default cognee path is `litellm_native`, not
> the instructor adapter (§1.0); routing strips cross-region prefixes before the
> converse-table lookup (§1.4.1); and structured output is
> **capability-gated** — the forced `json_tool_call` tool is the *fallback*
> branch, not the primary one (§1.4.3). Upstream line numbers below are as of
> that pass. Findings that changed the work breakdown are marked
> **[R]** where they land.

---

## 1. What Python actually does

### 1.0 Which adapter actually runs

`cognee/infrastructure/llm/config.py:90` — `structured_output_framework:
str = "litellm_native"`. **The instructor `BedrockAdapter` of §1.1 is not the
default path.** `LLMGateway.py:120` dispatches on that setting:

| Framework | Module | Default? |
|---|---|---|
| `litellm_native` | `litellm_native/get_native_client.py` → `NativeLiteLLMAdapter` | **yes** |
| `litellm_instructor` | `litellm_instructor/llm/bedrock/adapter.py` → `BedrockAdapter` | no |

`NativeLiteLLMAdapter` (`get_native_client.py:33-62`, `native_adapter.py:80-96`,
`231-240`) differs from §1.1 in ways that matter:

* it passes **no** `aws_*` parameters, no `custom_llm_provider`, and no
  `drop_params` — AWS auth is resolved entirely by litellm's uppercase-env
  fallbacks (§1.2), never by cognee;
* it qualifies the model as `bedrock/{model}`;
* it sends litellm's own `response_format` (a strict Pydantic class, or
  `_nonstrict_response_format`) rather than an instructor-shaped
  `json_schema` — litellm translates either through the same §1.4.3 branch;
* it clamps `max_completion_tokens = min(litellm model cap, user cap)`
  (`get_native_client.py:88-92`) — the origin of R3's cap requirement.

**Consequence for this plan:** the `AwsInputs`-from-env design (§2.1) is
*more* correct than §1.1 implied — the default Python path has no cognee-side
credential threading at all, so env-only resolution is exact parity rather than
an approximation. But P5's acceptance criteria must be read against §1.2/§1.4,
not against `get_s3_config()`.

### 1.1 The cognee-side adapter (`litellm_instructor`, non-default)

`cognee/infrastructure/llm/structured_output_framework/litellm_instructor/llm/bedrock/adapter.py`

* `BedrockAdapter` wraps `instructor.from_litellm(litellm.acompletion)` with
  `custom_llm_provider="bedrock"` and `drop_params=True`.
* Instructor mode **`json_schema_mode`** (`instructor_modes.py` →
  `INSTRUCTOR_MODE_TABLE["bedrock"]`): instructor emits an OpenAI-shaped
  `response_format={"type":"json_schema","json_schema":{...}}`, which litellm
  then translates (§1.4).
* Messages are always exactly `[{system}, {user}]`.
* `max_retries = 2` inside instructor, wrapped by tenacity
  (`wait_exponential_jitter(8, 128)`).
* Auth params come from `get_s3_config()` — a `pydantic-settings` object read
  **inside the adapter**, never threaded through `llm_config`. Precedence:
  1. `self.api_key` (`LLM_API_KEY`) → litellm `api_key`
  2. else `aws_access_key_id` + `aws_secret_access_key` (+ optional
     `aws_session_token`)
  3. else `aws_profile_name`
  * plus, whenever set: `aws_region_name`, `aws_bedrock_runtime_endpoint`.
* **`create_transcript` and `transcribe_image` both `raise NotImplementedError`**
  (see §6.5).
* Bedrock is exempt from the API-key requirement: absent from
  `_API_KEY_REQUIRED_PROVIDERS` (`get_llm_client.py:98`) and listed in
  `_NO_API_KEY_PROVIDERS` (`get_native_client.py:20`). **This exemption holds on
  both framework paths** — it is the one §1.1 fact that is load-bearing for the
  default flow too (R5).

Env vars cognee itself reads (`S3Config`): `AWS_REGION`, `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_PROFILE_NAME`,
`AWS_BEDROCK_RUNTIME_ENDPOINT`, `AWS_ENDPOINT_URL`. Per §1.0 these are read
**only** on the instructor path; on the default path the same variables reach
Bedrock through litellm's own env fallbacks (§1.2), which is why the two paths
converge on the same wire behaviour despite the different plumbing.

### 1.2 Auth resolution inside litellm

`litellm/llms/bedrock/base_aws_llm.py::_sign_request` + `get_credentials`:

```
api_key given?                          → Authorization: Bearer <api_key>   (early return, NO SigV4)
else AWS_BEARER_TOKEN_BEDROCK set?      → Authorization: Bearer <env>       (early return, NO SigV4)
else SigV4, credentials resolved in order:
  web_identity_token + role + session   → STS AssumeRoleWithWebIdentity
  role_name                             → STS AssumeRole (skipped when already running as that role)
  profile_name                          → boto3 profile
  access_key + secret + session_token   → static session credentials
  access_key + secret + region          → static credentials
  otherwise                             → boto3.Session() default chain
                                          (env / shared config / SSO / ECS / IMDS)
```

Every `aws_*` parameter left `None` falls back to its **UPPERCASE** env var of
the same name (the `params_to_check` loop). Note the non-obvious names:
`AWS_PROFILE_NAME` (not `AWS_PROFILE`), `AWS_REGION_NAME`, `AWS_ROLE_NAME`,
`AWS_SESSION_NAME`, `AWS_WEB_IDENTITY_TOKEN`, `AWS_STS_ENDPOINT`,
`AWS_EXTERNAL_ID`.

Signing service name is `bedrock`. SigV4 signs a filtered header subset
(`_filter_headers_for_aws_signature`); unsigned headers are re-applied
afterwards, and an explicit `Authorization` is never overwritten.

### 1.3 Region and endpoint chains

Region (`_get_aws_region_name`): `aws_region_name` param → region embedded in a
model ARN → `AWS_REGION_NAME` → `AWS_REGION` → `boto3.Session().region_name` →
hard default **`us-west-2`**.

Endpoint (`get_runtime_endpoint`): `api_base` → `aws_bedrock_runtime_endpoint`
→ `AWS_BEDROCK_RUNTIME_ENDPOINT` →
`https://bedrock-runtime.{region}.amazonaws.com`.

### 1.4 Chat: route selection and wire shape

#### 1.4.1 Model-id normalisation happens *before* routing — **[R3]**

This step was missing from the first draft of this plan and is the single
highest-consequence correction in it. `get_bedrock_route`
(`common_utils.py:926`) does not look the raw model id up in the converse
table. It first calls `get_bedrock_base_model` (`common_utils.py:685`), which
strips, in order:

1. an ARN wrapper (foundation-model / inference-profile ARNs → the bare id);
2. a **cross-region inference prefix** —
   `global|us|eu|apac|jp|au|us-gov` (`get_bedrock_cross_region_inference_regions`);
3. a provisioned-**throughput** suffix such as `:0:51k`
   (`strip_bedrock_throughput_suffix`);
4. a **context-window** suffix such as `[1m]`.

`BEDROCK_CONVERSE_MODELS` (`constants.py:1192`) stores only the *bare* ids —
`anthropic.claude-sonnet-4-5-20250929-v1:0`, never
`eu.anthropic.claude-sonnet-4-5-20250929-v1:0`.

> **Every model in P2's refreshed dropdown is `eu.`-prefixed.** The claim "all
> three models cognee ships route to converse" is true, but *only* via this
> normalisation. A port that looks the raw id up in the table routes 3 of 3
> shipped models to `invoke` — i.e. the adapter fails on its own defaults.
> `route.rs` implements normalisation **and** lookup, and R8 tests the prefixed
> ids specifically.

#### 1.4.2 Route selection

* explicit prefixes win: `converse/`, `invoke/`, `converse_like/`, `agent/`,
  `agentcore/`, `async_invoke/`, `openai/`, plus `claude_platform/` and
  `mantle/` (`common_utils.py:630-636`) — the last two were missing from the
  first draft;
* an **application-inference-profile ARN** → converse
  (`common_utils.py:981-982`) — a common enterprise deployment shape, also
  missing from the first draft;
* `nova/` and `nova-2/` → converse;
* else normalised base model ∈ `litellm.bedrock_converse_models` → **converse**,
  otherwise → **invoke**.

`BEDROCK_CONVERSE_MODELS` covers all Anthropic Claude (v1 → 4.6), Nova, Llama
3.x, Mistral large/small, DeepSeek, Qwen3, `openai.gpt-oss-*`, AI21 Jamba.

URL (`converse_handler.py:353-363`):
`POST {endpoint}/model/{modelId}/converse` (`/converse-stream` when streaming).
Note the URL carries the **original** id, prefix included — normalisation feeds
the routing decision only, never the request path.

Body (`converse_transformation.py:1616-1679`):

```json
{ "messages": [...], "system": [...], "inferenceConfig": {...},
  "additionalModelRequestFields": {...}, "toolConfig": {...} }
```

#### 1.4.3 Structured output is capability-gated — **[R3]**

`_translate_response_format_param` (`converse_transformation.py:1007-1075`)
picks one of two shapes per model, reading capability flags out of
`model_prices_and_context_window.json`:

* `supports_native_structured_output` → **`outputConfig.textFormat`** carrying
  the JSON schema, with `additionalProperties: false` forced onto every object
  node;
* **otherwise** → inject a synthetic tool named **`json_tool_call`**
  (`RESPONSE_FORMAT_TOOL_NAME`, `constants.py:1342`) whose input schema *is* the
  response schema. The forcing `toolChoice: {tool: {name}}` is applied **only
  when** `supports_tool_choice(model)` **and** thinking is not enabled
  (`converse_transformation.py:1057-1063`).

Either way `json_mode = True`, and the result is unwrapped from the tool input /
text format.

Against cognee's own three shipped models this resolves as:

| Model | Branch | `toolChoice` forced? |
|---|---|---|
| `eu.anthropic.claude-sonnet-4-5-20250929-v1:0` | native `outputConfig` | n/a |
| `eu.anthropic.claude-haiku-4-5-20251001-v1:0` | native `outputConfig` | n/a |
| `eu.amazon.nova-lite-v1:0` | `json_tool_call` | **no** — no `supports_tool_choice` |

> So the forced-tool path is the **fallback**, and it is not exercised by either
> Anthropic model cognee ships. An R3 that implements only the forced tool
> diverges from Python on all three defaults — and, since Nova is documented to
> reject a specific `toolChoice`, may 400 outright on nova-lite. R3 carries a
> small capability table and both branches.

The fallback form is structurally what `AnthropicAdapter` already does with its
forced `extract_structured_data` tool. Its repair loop is the right *pattern* to
follow, but see §6.7 — it is not reusable code.

### 1.5 Embeddings

`EMBEDDING_PROVIDER=bedrock` falls through `get_embedding_engine.py` to
`LiteLLMEmbeddingEngine` → `litellm.aembedding`, which routes Bedrock embeddings
to **InvokeModel**, not Converse: `POST {endpoint}/model/{modelId}/invoke`, one
request per input text for the Titan families (`_single_func_embeddings` loops),
batched for Cohere.

| Family | Request | Response |
|---|---|---|
| `amazon.titan-embed-text-v1` (g1) | `{"inputText": str}` | `{"embedding": [...], "inputTextTokenCount": n}` |
| `amazon.titan-embed-text-v2:0` | `{"inputText": str, "dimensions"?, "normalize"?}` | same |
| `amazon.titan-embed-image-v1` | `{"inputText"?, "inputImage"?, "embeddingConfig"}` | same |
| `cohere.embed-*` | `{"texts": [...], "input_type": "search_document", "embedding_types"?, "output_dimension"?}` | `{"embeddings": [[...]]}` |
| `async_invoke/*` (incl. some nova / twelvelabs ids) | async-invoke variants | — |

(The async-invoke branch keys on an explicit `async_invoke/` model prefix,
`embed/embedding.py:392-394` — it is not an inherent property of the nova or
twelvelabs embedding families, as the first draft implied. cognee ships
neither, so this row is informational.)

cognee passes no AWS credentials on the embedding path — only `api_key`
(bearer) and `endpoint`; everything else comes from the ambient env / boto3
chain.

### 1.6 Streaming

`converse-stream` uses the binary `vnd.amazon.eventstream` framing. No OSS Rust
adapter actually streams today (`OpenAIAdapter::supports_streaming()` returns
`true`, but nothing parses a stream). **Out of scope** —
`supports_streaming()` returns `false`.

---

## 2. Where it lands in the tree

The provider seam already exists and needs no new abstraction:

* `cognee_components::LlmFactory` — `provider() -> &str` + `build(ctx)`; the
  Anthropic (Tier 2) and Azure (Tier 3) factories in
  `crates/components/src/builtins/llm.rs` are the template.
* `ComponentRegistry::with_builtins()` registers built-ins per enabled feature;
  `register_llm` is a plain `HashMap<String, Arc<dyn LlmFactory>>` with no enum
  validation, so `LLM_PROVIDER=bedrock` resolves as soon as the factory is
  registered.
* Embedding provider selection is an exhaustive match on `EmbeddingProvider`
  inside `EmbeddingConfig::create_engine`
  (`crates/embedding/src/config.rs:468`) — a new variant is a compile-checked
  edit.
* `LlmProvider::Bedrock` already exists in `cognee_llm::config::LlmProvider`
  (unused) — that enum needs no change.

### 2.1 AWS config plumbing — the `anthropic_base_url` precedent

`BackendBuildContext` deliberately keeps factories env-free: "all
environment-variable reads happen when a caller lowers its config into a
`BackendBuildContext`" (`crates/components/src/context.rs`). The existing
precedent for provider-specific, env-only config is
`anthropic_base_url_from_env()` — a helper in that same module, carried as a
field on `LlmInputs`, called from both lowering sites, and **not** a `Settings`
field.

Bedrock follows it exactly:

```rust
// crates/components/src/context.rs
#[derive(Clone, Default)]
pub struct AwsInputs {
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
    pub profile_name: Option<String>,
    pub role_name: Option<String>,
    pub session_name: Option<String>,
    pub web_identity_token: Option<String>,
    pub sts_endpoint: Option<String>,
    pub external_id: Option<String>,
    pub bedrock_runtime_endpoint: Option<String>,
    pub bearer_token: Option<String>,   // AWS_BEARER_TOKEN_BEDROCK
}

pub fn aws_inputs_from_env() -> AwsInputs { /* §1.2 / §1.3 names, trimmed, empty ⇒ None */ }
```

`LlmInputs` and `EmbeddingInputs` each gain `pub aws: AwsInputs`, populated by
`aws_inputs_from_env()` at both lowering sites
(`crates/lib/src/config.rs:850` and `crates/http-server/src/config.rs:700`).

This matters for scope: because Python resolves these from the environment and
never threads them through its LLM config (§1.0 — the default path does not even
read `S3Config`), **no `Settings` fields are added — and therefore no binding
setters, no `to_dict`/`from_map` entries, and no wrapper-gate regeneration.**
That is both correct parity and the cheapest surface.

Audited and confirmed: `Settings.llm_provider` / `embedding_provider` are plain
`String`s (`crates/lib/src/config.rs:232`), the bindings pass provider ids as
strings (e.g. `ts/src/cognee.ts:128` — `setEmbeddingProvider(v: string)`), and
`to_dict`/`from_map` (config.rs:1035, 2058) key on existing fields only. R7's
DTO enum is server-internal, and `test_http_openapi.py:135-147` compares
`components.schemas` **key sets** only — so the enum-value change trips no
gate. **The bet holds; nothing crosses a binding boundary.**

Two internal costs the first draft priced at zero, though:

* `LlmInputs` and `EmbeddingInputs` are `#[derive(Clone)]` with **no `Default`**
  (`crates/components/src/context.rs:96`), and are built as exhaustive struct
  literals in eight places — `registry.rs:408,427`,
  `builtins/database.rs:114,133`, `builtins/embedding.rs:195`,
  `lib/src/config.rs:830,849`, `http-server/src/config.rs:681,700`. Adding a
  field is `E0063` at every one of them. Mechanical, but not free (R2).
* Default-on Bedrock (§6.2) means every default build compiles the
  aws-config/smithy tree — including the binding prebuild legs.

### 2.2 Crate layout

Adapters live inside `cognee-llm`, so Bedrock does too — feature-gated because
the AWS deps are not free:

```
crates/llm/src/adapters/
  bedrock/
    mod.rs          BedrockAdapter (impl Llm) — re-exported as adapters::BedrockAdapter
    aws/
      env.rs        AwsInputs → resolved settings; the S3Config analogue
      region.rs     §1.3 region chain incl. the us-west-2 hard default
      endpoint.rs   §1.3 runtime-endpoint chain
      credentials.rs the §1.2 precedence ladder over aws-config providers
      signer.rs     SigV4 over a reqwest::Request via aws-sigv4 (service "bedrock")
      transport.rs  trait BedrockTransport — the seam isolating the §3 decision
    model_id.rs     §1.4.1 normalisation: ARN unwrap, cross-region prefix,
                    throughput + context suffixes
    route.rs        §1.4.2 routing over the normalised id + BEDROCK_CONVERSE_MODELS
    caps.rs         per-model capability + limit table: supports_native_structured_output,
                    supports_tool_choice, max output tokens (§1.4.3, R3)
    converse.rs     request/response transform for /converse (both structured-output branches)

crates/embedding/src/
  provider.rs       + EmbeddingProvider::Bedrock
  config.rs         + create_engine arm, + aws: AwsInputs on EmbeddingConfig
  bedrock/
    mod.rs          BedrockEmbeddingEngine (impl EmbeddingEngine)
    titan.rs        g1 / v2 / multimodal
    cohere.rs

crates/components/src/builtins/llm.rs
                    + BEDROCK_PROVIDER, BedrockLlmFactory
crates/components/src/builtins/embedding.rs
                    + parse_embedding_provider arm, + aws on build_embedding_config
crates/components/src/registry.rs
                    + #[cfg(feature = "bedrock")] registration in with_builtins()
                    + bedrock assertion in builtins_register_documented_providers
```

Three touch points the first draft omitted, all inside `cognee-components`:

* **`parse_embedding_provider`** (`builtins/embedding.rs:62-72`) is a hard
  allowlist match — the declared single source of truth for provider ids.
  Without a `"bedrock"` arm, `EMBEDDING_PROVIDER=bedrock` is rejected as a
  misconfiguration no matter what `EmbeddingProvider` gains (R4).
* **`EmbeddingConfig` has no `aws` field today** — `build_embedding_config` must
  carry `AwsInputs` across (R4).
* **`builtins_register_documented_providers`** (`registry.rs:318-374`) is a
  drift guard over the registered provider set; it wants a bedrock assertion
  (R5).

The chat-side `/invoke` transforms are deliberately **not** in this tree — see
§6.7.

The `aws/` module is shared by the LLM adapter and the embedding engine — one
credential/region/endpoint resolver, one signer. It lives under `cognee-llm`
and is re-exported for `cognee-embedding` to depend on (or, if that edge is
unwanted, factored into `cognee-utils` — decide at R1).

### 2.3 Feature wiring

Leaf crates expose `bedrock` as a non-default feature; the shipped surfaces turn
it on by default, exactly as `lancedb` / `onnx` / `ladybug` are handled today:

```toml
# crates/llm/Cargo.toml
bedrock = ["dep:aws-config", "dep:aws-credential-types", "dep:aws-sigv4",
           "dep:aws-smithy-runtime-api"]

# crates/embedding/Cargo.toml
bedrock = ["cognee-llm/bedrock"]

# crates/components/Cargo.toml
default = [..., "bedrock"]
bedrock = ["cognee-llm/bedrock", "cognee-embedding/bedrock"]

# crates/lib/Cargo.toml and crates/http-server/Cargo.toml
default = [..., "bedrock"]
bedrock = ["cognee-components/bedrock", "cognee-llm/bedrock", "cognee-embedding/bedrock"]
```

Consumers that do not want the AWS stack (wasm, Android, slim binding builds)
drop it with `--no-default-features` plus explicit forwarding, the same way they
already drop `lancedb`.

These entries do **not** all land in one late step — the `cognee-llm` half is a
prerequisite of R1 and the `cognee-components` half of R5. See §6.8.

---

## 3. Transport decision

**Recommendation: `aws-config` (credential + region resolution) + `aws-sigv4`
(signing) + the existing `reqwest` client**, hand-writing the Converse /
InvokeModel JSON — rather than `aws-sdk-bedrockruntime`.

* The Bearer path must be a plain header with **no** SigV4 and no credential
  lookup at all (litellm early-returns, §1.2). Hand-rolled that is a two-line
  branch; through the SDK it is a fight with the auth-scheme resolver.
* The SigV4 fallback must reach the full boto3-equivalent chain — env, shared
  config/credentials, SSO, ECS, IMDS, web identity, STS assume-role.
  `aws-config`'s `DefaultCredentialsChain` is the only Rust equivalent;
  hand-rolling would silently cover static keys only.
* Bodies stay hand-written, so the adapter is the same *shape* as
  `AnthropicAdapter` and reuses its structured-output repair loop, corrective
  re-ask, truncation retry, tracing spans, and `LlmError` taxonomy verbatim. An
  SDK-based adapter would need all of that re-expressed against smithy types.
* Streaming — the one place the raw SDK genuinely earns its keep (eventstream
  framing) — is out of scope (§1.6).

Dep weight is roughly a wash; `aws-config` pulls the smithy runtime either way.
Escape hatch is cheap: transport sits behind one internal trait (§2.2), so
swapping in `aws-sdk-bedrockruntime` later touches one file.

Workspace note: `hyper 1.10.1` is already in the graph via `reqwest 0.12`
(`Cargo.lock:4281`), so adding a hyper-1 consumer is not new; no `aws-*` crate is
in the lock today. (The closed `cognee-cloud-rust` workspace also carries a
`hyper 0.14` qdrant fork under `[patch.crates-io]`; the two already coexist
there, so this does not disturb the closed build either.)

**MSRV ceiling — [R1].** The workspace pins rustc **1.91**
(`rust-toolchain.toml:2`, `Cargo.toml:55` `rust-version = "1.91"`), but
`aws-config` ≥ 1.9.0 and `aws-sigv4` 1.5.x require **1.94.1**. The newest
compatible line is `aws-config 1.8.18` / `aws-sigv4 1.4.5` /
`aws-credential-types 1.2.14` (MSRV 1.91.1). `resolver = "3"` selects these
automatically, so the build works — but the adapter is **frozen on a mid-2026
AWS release line until the toolchain moves**. Pin those versions explicitly with
a comment pointing here, so the next `cargo update` doesn't produce a confusing
MSRV failure.

Also: the SigV4 fallback's SSO leg needs `aws-config`'s `sso` feature turned on
explicitly — verify it is not in the default set before relying on §1.2's
"full boto3-equivalent chain".

---

## 4. Work breakdown — Rust

### R1 — ✅ landed (`b8755114`) — `aws/` module: env, region, endpoint, credentials, signer, transport
Testable standalone against `httpmock` plus SigV4 golden vectors. Decide the
`cognee-llm` vs `cognee-utils` home for the shared module here. **The `bedrock`
cargo feature and its optional deps must land with this step**, not at R6 — the
module cannot compile without them (§2.3, §6.8). Pin the AWS crate versions per
§3's MSRV note.

Two credential-ladder subtleties that are easy to get subtly wrong, and are the
reason R8 has a dedicated `credentials.rs`: the uppercase-env expansion loop
(`base_aws_llm.py:222-247`) and "skip `AssumeRole` when already running as that
role" (`base_aws_llm.py:1076+`).

### R2 — ✅ landed (`c81a049f`) — `AwsInputs` on the context, populated at both lowering sites
`crates/components/src/context.rs` (struct + `aws_inputs_from_env()`),
`crates/lib/src/config.rs:850`, `crates/http-server/src/config.rs:700`.
Additive in behaviour but **not** free to compile: `LlmInputs` /
`EmbeddingInputs` have no `Default` and are exhaustive struct literals in eight
places (§2.1) — expect `E0063` at each and fix them in this step. Adding
`#[derive(Default)]` to both input structs is the cheaper alternative and worth
taking.

### R3 — ✅ landed (`950d9827`) — `BedrockAdapter` (`impl Llm`)

**This is the riskiest step in the plan** — it is where silent non-parity is
easiest to ship, and the first draft of this section was wrong on two counts
(§1.4.1, §1.4.3). Implement in this order:

1. **Model-id normalisation and routing** (§1.4.1 / §1.4.2), `model_id.rs` +
   `route.rs`. Strip a leading `bedrock/`; unwrap ARNs; strip the cross-region
   prefix, throughput suffix and context suffix; honour the explicit route
   prefixes (`converse/`, `invoke/`, `converse_like/`, `agent/`, `agentcore/`,
   `async_invoke/`, `openai/`, `claude_platform/`, `mantle/`) and the
   application-inference-profile-ARN → converse branch. **The normalised id
   feeds routing only — the request URL keeps the original id.** Get this wrong
   and all three shipped models break.
2. **Capability table** (`caps.rs`) — `supports_native_structured_output`,
   `supports_tool_choice`, and max output tokens, keyed on the *normalised* id.
   This is the Rust stand-in for litellm's
   `model_prices_and_context_window.json`; hand-maintain the entries cognee
   ships plus a documented conservative default for unknown ids (no native
   structured output, no forced tool choice). Note the existing Anthropic
   `model_max_output_tokens` (`crates/llm/src/adapters/anthropic.rs:169`) keys
   on `claude-*` names and will **not** match Bedrock ids — this table is the
   cap source R3 needs, not that one.
3. **`generate()`** → Converse; map `output.message.content[].text` →
   `GenerationResponse`, `usage` → `TokenUsage`. System messages hoisted to the
   top-level `system` block array (Converse has no system role).
4. **`create_structured_output_with_messages_raw_validated()`** — **both**
   §1.4.3 branches, selected from the capability table:
   * native → `outputConfig.textFormat` with the schema,
     `additionalProperties: false` forced onto every object node; unwrap the
     text format;
   * fallback → synthetic `json_tool_call` tool whose input schema is the
     response schema, with `toolChoice` forced **only when**
     `supports_tool_choice`; unwrap `toolUse.input`.

   Re-implement the Anthropic repair loop's *behaviour* over Converse's JSON
   (`stopReason`, `toolUse.input`): invalid / empty / validator-rejected output
   triggers a corrective re-ask inside the same retry budget. See §6.7 — this is
   a re-implementation, not a call into shared code.
5. **Inference config** — `inferenceConfig.maxTokens` =
   `min(ctx.llm.max_completion_tokens, model cap)` per §1.0, plus `temperature`
   / `topP` / `stopSequences`.
6. **`LLM_ARGS`** (`ctx.llm.llm_args`) merged into
   `additionalModelRequestFields` — litellm's `{**self.llm_args, **kwargs}`,
   explicit keys winning.
7. **Error mapping** — Bedrock signals throttling as HTTP **400
   `ThrottlingException`** as well as 429, unlike the 429-only mapping the
   existing adapters use. Map both to `LlmError::RateLimitExceeded`
   (`crates/llm/src/error.rs`) so the retry layer engages; also map
   `ValidationException`, `AccessDeniedException`,
   `ModelNotReadyException`/`ServiceUnavailableException`.
8. `supports_streaming() = false`, `supports_function_calling() = true`.

### R4 — ✅ landed (`ea4d47f0`) — `BedrockEmbeddingEngine` (`impl EmbeddingEngine`)
InvokeModel per §1.5. Titan loops one text per request under bounded
concurrency; Cohere batches. `dimension()` from `EmbeddingConfig.dimensions`.
The OSS trait contract says embeddings are L2-normalised: Titan v2 normalises
server-side with `normalize: true`; normalise client-side for the families that
do not.

Plumbing, per §2.2: `EmbeddingProvider::Bedrock`, the `create_engine` arm, a
`"bedrock"` arm in **`parse_embedding_provider`** (without it the provider id is
rejected before `create_engine` is ever reached), and an `aws` field on
`EmbeddingConfig` carried across by `build_embedding_config`.

### R5 — ✅ landed (`287208d0`) — `BedrockLlmFactory` + registration
`crates/components/src/builtins/llm.rs` alongside `AnthropicLlmFactory` /
`AzureLlmFactory`; registered in `with_builtins()` under
`#[cfg(feature = "bedrock")]`. `build_transcriber()` returns `Ok(None)` (§6.5),
matching anthropic / ollama / gemini / mistral. **No API-key requirement** —
Bedrock is absent from Python's `_API_KEY_REQUIRED_PROVIDERS`, so an empty
`LLM_API_KEY` must fall through to SigV4 rather than erroring the way the
Anthropic factory does.

Also add the bedrock assertion to `builtins_register_documented_providers`
(`registry.rs:318-374`), the drift guard over the registered provider set.

### R6 — ✅ ~~Feature wiring~~ — folded into R1 and R5 (§6.8)
Retained as a checklist item only: confirm §2.3's table matches what R1 and R5
actually added, and that `default = [..., "bedrock"]` is present on `cognee`,
`cognee-http-server` and `cognee-cli`.

### R7 — ✅ landed (`b76a216d`) — HTTP `/settings` (see §5 for the parity ordering)
`dto/settings.rs:62` gains `Bedrock`; `routers/settings.rs:~294` gains the
match arm (the match is exhaustive, so this is a compile error until done);
`routers/settings.rs:162` model list refreshed to Python's current three.

### R8 — ✅ landed (inside `b8755114` / `950d9827` / `ea4d47f0`) — Tests
* `model_id.rs` — §1.4.1 normalisation: ARN unwrap, each cross-region prefix,
  throughput and `[1m]` suffixes. **Must include the three `eu.`-prefixed ids
  cognee actually ships**, asserting they route to converse — that is the
  regression this suite exists for.
* `route_table.rs` — converse vs invoke selection, every explicit route prefix,
  the application-inference-profile-ARN branch, and that the request URL keeps
  the un-normalised id.
* `structured_output.rs` — branch selection off the capability table: native
  `outputConfig.textFormat` for the two Anthropic ids, `json_tool_call`
  **without** forced `toolChoice` for nova-lite, forced `toolChoice` for a model
  that advertises `supports_tool_choice`, and the conservative default for an
  unknown id.
* `converse_transform.rs` — system hoisting, `toolConfig`, `inferenceConfig`
  clamping, `additionalModelRequestFields` merge precedence.
* Error mapping — 400 `ThrottlingException` → `RateLimitExceeded`, not a
  generic request error.
* `sigv4_golden.rs` — signature vectors; bearer path asserts **no**
  `Authorization: AWS4-HMAC-SHA256` and no credential lookup.
* `credentials.rs` — the §1.2 ladder, including the uppercase-env fallbacks and
  the `AWS_PROFILE_NAME` (not `AWS_PROFILE`) spelling.
* `integration_bedrock.rs` — `httpmock`-backed, plus `#[ignore]`d live tests,
  one per auth mode (bearer, static keys, profile).
* Embedding: per-family request/response transforms, normalisation.

---

## 5. Work breakdown — Python parity

Two-way parity is part of this plan, not a follow-up. Three of these are
divergences that exist **today**, independent of the adapter work.

### P1 — ⬜ **open, upstream** — Python: accept `bedrock` on settings save
`cognee/api/v1/settings/routers/get_settings_router.py:25-29` — add
`Literal["bedrock"]` to `LLMConfigInputDTO.provider`. `save_llm_config.py`
already takes `provider: str`, so this is the only gate. **Land this before
R7**, or Rust starts accepting a payload Python rejects and the documented
replication inverts.

### P2 — ✅ landed (`b76a216d`) — Rust: refresh the bedrock model list (existing divergence)
`routers/settings.rs:162` offers a single stale
`anthropic.claude-3-5-sonnet-20240620-v1:0`. Python's `get_settings.py:165-178`
now lists three: `eu.anthropic.claude-sonnet-4-5-20250929-v1:0`,
`eu.anthropic.claude-haiku-4-5-20251001-v1:0`, `eu.amazon.nova-lite-v1:0`. The
two GET responses have already drifted apart.

### P3 — ✅ landed (`8a89ed43`) — Rust: retire the replicated asymmetry from the spec
`docs/http-server/routers/settings.md` — validation rule §2 ("we replicate this
asymmetry; the frontend treats `bedrock` as read-only in v1"), the DTO doc
comment at :163, open question §6.4, and the planned
`POST provider: "bedrock" → 400` case in test plan §5.7.

### P4 — ✅ landed (`28afb533`) — Write the settings parity test the spec promised
`e2e-cross-sdk/harness/test_http_settings.py` is referenced by
`settings.md` §5.8 but **was never written**. Add it: GET byte-equality modulo
the API-key mask, and `POST provider: "bedrock"` accepted on both SDKs. This is
what keeps P1/P2/P3 from re-diverging.

*(No CI gate blocks P1–P3 today: `test_http_openapi.py` compares path, method,
security-scheme, and `components.schemas` **key sets** only — per-schema field
diff is explicitly deferred there — so an enum-value change does not trip it.)*

### P5 — ✅ satisfied by R3/R4 — Behavioural parity checklist for the adapter
The acceptance criteria for R3/R4, each traceable to §1:

| Behaviour | Source of truth |
|---|---|
| Bearer short-circuits before any credential resolution | §1.2 |
| Credential ladder order, incl. STS and profile | §1.2 |
| Uppercase env fallbacks, `AWS_PROFILE_NAME` spelling | §1.2 |
| Region chain ending in `us-west-2` | §1.3 |
| Endpoint chain incl. `AWS_BEDROCK_RUNTIME_ENDPOINT` | §1.3 |
| Model-id normalisation before the route lookup (cross-region prefix, ARN, suffixes) | §1.4.1 |
| converse/invoke routing + all explicit prefixes | §1.4.2 |
| Structured output branch chosen by capability, not hard-coded | §1.4.3 |
| `toolChoice` forced only when `supports_tool_choice` | §1.4.3 |
| `max_completion_tokens` clamped to the model cap | §1.0 |
| No API key required for bedrock | §1.1 |
| Embeddings via InvokeModel, per-family bodies | §1.5 |

Read these against §1.2/§1.4 rather than `get_s3_config()` — per §1.0 the
default Python path never touches the latter.

### P6 — ⬜ optional, open — raise Python to meet Rust on vision
See §6.5. If strict two-way parity is wanted, implement `transcribe_image` in
Python's `BedrockAdapter` using Converse image blocks rather than dropping it
from the Rust side.

---

## 6. Decisions and caveats

**6.1 Env-only AWS config is deliberate.** §2.1 — matches Python's `S3Config`
and avoids the entire bindings/settings surface. If a future need arises to set
region from the settings API, add the `Settings` fields then; the
`BackendBuildContext` shape already accommodates it.

**6.2 Default-on.** §2.3 — the shipped surfaces (`cognee`, `cognee-http-server`,
`cognee-cli`) build with Bedrock available; slim targets drop it the same way
they drop `lancedb`.

**6.3 No streaming.** §1.6.

**6.4 Audio transcription stays unsupported.** Bedrock has no Whisper
equivalent; `build_transcriber()` returns `Ok(None)`, matching Python (which
raises `NotImplementedError`) and the anthropic/ollama/gemini factories.

**6.5 Vision exceeds Python parity — on purpose.** Python's
`BedrockAdapter.transcribe_image` raises `NotImplementedError`. Converse *does*
accept image content blocks, so implementing it in R3 costs ~40 lines (a base64
`image` block plus the existing prompt). It ships, flagged in the crate docs as
*exceeding* parity, with P6 as the optional path to closing the gap from the
Python side instead.

**6.6 `not-implemented.md` needed updating on landing — ✅ done.** Its
provider-breadth entry listed Bedrock as missing for both LLM and embeddings;
both mentions were removed in the §8 docs pass, per this folder's conventions.

**6.7 The Anthropic repair loop is a pattern, not shared code — and `/invoke`
chat is out of scope.** `structured_output_impl` is a private method on
`AnthropicAdapter`, coupled to `AnthropicResponse`, Anthropic `stop_reason`,
`tool_use` blocks and `call_api` (`crates/llm/src/adapters/anthropic.rs:396-460`).
Only `append_corrective_instruction` and `schema_required_validator`
(`crates/llm/src/schema.rs:195,233`) are genuinely reusable. Converse's response
JSON differs (`stopReason`, `toolUse.input`), so R3 re-implements the loop
against Converse types. §3's "reuses its repair loop verbatim" overstated this;
the *shape* argument for hand-written bodies still stands, the free-lunch
framing does not — budget R3 accordingly.

Relatedly, the first draft's `invoke.rs` carried legacy chat transforms for five
model families. **No model cognee ships routes to invoke** (§1.4), and those
transforms were the largest single lump of code in the plan. Scope is cut to:
the InvokeModel *embedding* bodies (§1.5, genuinely required), and a clear
`LlmError` for an invoke-route chat model. Reinstate if a user actually needs a
pre-Converse chat model.

**6.8 Feature wiring cannot be a late step.** `bedrock = ["dep:aws-config", …]`
on `crates/llm` must exist at R1 or the `aws/` module has no deps to compile
against; the `cognee-components` feature must exist at R5 for the `#[cfg]`'d
registration. R6 as an independent late step was a paper step — intermediate
commits would not build. Folded into R1/R5, kept as a verification checklist.

**6.10 Credentials are refreshed, not snapshotted.** §1.2 specifies how a
credential set is *resolved* and says nothing about how long it lives — an
omission that shipped as a real defect: the ladder ran once at construction and
every later request was signed with that snapshot. Every non-static rung yields
temporary credentials (`AssumeRole` ~1h, the default chain over IMDS/ECS/SSO
comparable), and because the built adapter is cached for the process lifetime, a
long-running host started returning terminal 403 `ExpiredTokenException` and
never recovered. Python does not have this problem: boto3 hands litellm
`RefreshableCredentials`. `BedrockAuthProvider` closes it — it keeps the ladder
inputs, re-runs the resolution when the cached credentials are within 60s of
expiry, and is a no-op clone for bearer tokens and static keys. Re-running
reaches the same rung, because the rung is chosen from `AwsSettings` and the
ambient identity, neither of which changes over the process lifetime.

**6.9 The AWS crate line is pinned by the toolchain.** §3 — 1.8.x/1.4.x until
rustc moves past 1.91. Revisit on the next toolchain bump.

---

## 7. Ordering

```
R1 aws/ module (env, region, endpoint, credentials, signer, transport)
     └─ carries the cognee-llm `bedrock` feature + pinned AWS deps  (§6.8, §3)
R2 AwsInputs on BackendBuildContext + both lowering sites (+ 8 E0063 fixes)
     ├─ R3 BedrockAdapter
     │    model_id → route → caps → converse → structured output (both
     │    branches) → error mapping → vision
     └─ R4 BedrockEmbeddingEngine (+ parse_embedding_provider, EmbeddingConfig.aws)
R5 factory + registry registration      ← first point LLM_PROVIDER=bedrock works
     └─ carries the components/lib/http-server features            (§6.8)
R6 feature-wiring verification checklist (no longer a standalone step)
P1 Python Literal["bedrock"]            ← must precede R7
R7 /settings DTO + match arm + P2 model list
P3 spec doc cleanup · P4 parity test
R8 tests · §8 docs
```

R3 and R4 are independent once R1+R2 land. P1 is independent of everything and
can go first. R7's exhaustive match does work as a compile-time forcing function
— the DTO variant will not build without the router arm — so that pairing is
correctly sequenced.

> **As executed**, P1 could not go first: it lives in `topoteretes/cognee` and is
> not landable from this repository. R7 shipped ahead of it, which inverts the
> documented replication in exactly the direction §5 P1 warned about — Rust now
> accepts a payload Python rejects. That inversion is documented in
> `docs/http-server/routers/settings.md` §6.4 and guarded by the
> `xfail(strict=True)` case in P4's test, which turns red the moment upstream
> accepts the value.

**Riskiest item: R3**, specifically steps 1 and 2 (normalisation + capability
table). Both were wrong in the first draft, both fail *silently* into
plausible-looking wrong behaviour, and both are exercised by the models cognee
ships by default. Second riskiest: R1's credential ladder. Effort is materially
heavier than the first draft's tone implied — comparable to the Anthropic Tier-2
landing, **plus** an auth stack this workspace has never carried.

---

## 8. Docs to update — ✅ landed

* ✅ `docs/configuration.md` — the "Native Bedrock adapters are tracked
  separately in issue #17" line is gone, replaced by a
  **Bedrock (`LLM_PROVIDER=bedrock`)** section carrying the env-var table, the
  §1.2 auth ladder, the §1.3 region/endpoint chains, the converse/invoke note and
  the capability/feature-gating notes. Three neighbouring claims were corrected
  at the same time: the "`LLM_API_KEY` is required for every provider" sentence
  now carries the Bedrock carve-out (§1.1), the audio-transcription note lists
  `bedrock` under graceful no-audio (§6.4), and the Embedding section documents
  `EMBEDDING_PROVIDER=bedrock` (§1.5).
* ✅ `docs/roadmap/not-implemented.md` — Bedrock dropped from the LLM and
  embedding provider-breadth entries (§6.6).
* ✅ `docs/http-server/routers/settings.md` — P3.
* ✅ `crates/llm/README.md` / `crates/embedding/README.md` — `BedrockAdapter` and
  `BedrockEmbeddingEngine` bullets (and the stale "Planned: Anthropic adapter"
  line retired).
* ✅ `docs/architecture.md` — crate-tree comments, the `cognee-embedding` /
  `cognee-llm` impl lists, the rustdoc entry-point table, and a key-dependency
  row for the pinned AWS crate line.
* **This doc — kept, not deleted.** The `docs/roadmap/README.md` convention says
  a completed plan leaves the folder; this one has not completed (P1 is open
  upstream), and a dozen in-repo references cite its path — source comments, two
  Cargo manifests, `settings.md`, the cross-SDK parity test, and one runtime
  `LlmError` message. See the note under the status table at the top.

---

## 9. Verification

* `cargo check --all-targets`, then `scripts/check_all.sh` (fmt, clippy
  `-D warnings`, wrapper-binding gates).
* `cargo check --no-default-features` on `cognee`, `cognee-http-server`, and
  `cognee-llm` — confirms the feature gating is clean and the AWS stack is
  genuinely droppable.
* ~~`ci/assert-bindings-layout.sh` as the env-only tripwire~~ — **this does not
  work.** That script guards the path contract between the `bindings/`
  workspace and the prebuild workflows (`ci/assert-bindings-layout.sh:1-25`); it
  would be a no-op even if `Settings` fields *were* added, so it cannot detect a
  §2.1 violation. The real tripwires are the wrapper-gate scripts run by
  `scripts/check_all.sh` (`python/scripts/check.sh`, `ts/scripts/check.sh`,
  `java/scripts/check.sh`) — if any of them regenerates output, the env-only
  decision has been violated somewhere.
* The R8 test set, plus P4's cross-SDK settings test.
* One `#[ignore]`d live test per auth mode, runnable by hand against a real
  account.
