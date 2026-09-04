//! OpenAI API adapter with structured-output support.
//!
//! This adapter uses OpenAI's tool calling (`tools` + `tool_choice`) — the same
//! shape Python cognee sends via instructor/litellm — to generate structured
//! outputs based on JSON schemas derived from Rust types, falling back to legacy
//! function calling and JSON mode for older OpenAI-compatible servers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cognee_utils::pacing::{Pacer, llm_pacer};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, instrument, warn};

#[allow(unused_imports)]
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::{Llm, StructuredOutputValidator};
use crate::transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// OpenAI API adapter.
///
/// Supports structured output generation via (in fallback order):
/// - Tool calling (`tools` + forced `tool_choice`) — the primary path, matching
///   Python cognee's instructor/litellm `Mode.TOOLS`
/// - Legacy function calling (`functions` + `function_call`)
/// - JSON mode (response_format with type: "json_object")
///
/// # Example
/// ```ignore
/// use cognee_llm::adapters::OpenAIAdapter;
/// use cognee_llm::Llm;
///
/// let adapter = OpenAIAdapter::new(
///     "gpt-4-turbo-preview",
///     "sk-...",
///     None, // Use default base URL
/// )?;
///
/// let result: MyStruct = adapter.create_structured_output(
///     "Extract information from this text",
///     "You are a helpful assistant",
///     None,
/// ).await?;
/// ```
/// Per-mode memory of whether a structured-output cascade mode is worth sending
/// to one endpoint.
///
/// The cascade (tool calling -> legacy `functions`/`function_call` -> JSON mode)
/// exists because OpenAI-compatible servers differ in what they accept, but it
/// was stateless per call: nothing remembered that an endpoint had never once
/// answered a given mode. A server started without a tool-call parser -- vLLM
/// without `--enable-auto-tool-choice --tool-call-parser` -- answers 200 with
/// the text in `message.content` and populates neither `tool_calls` nor
/// `function_call`, so the first two modes can never produce anything and every
/// structured call re-paid their whole retry ladder. Measured on one such
/// deployment: zero tool calls in 11,890 responses, 4.14 API calls per
/// extraction against 1.09 on an endpoint with a parser, with ~71% of output
/// tokens and cost producing nothing.
///
/// The counter tracks "is this mode worth sending", not "does the server
/// implement it". The distinction matters: an endpoint with no parser that
/// echoes usable JSON in `content` is answered by mode 1 on its first attempt,
/// so mode 1 is the *cheapest* path there and must not be skipped.
///
/// A mode counts against itself only when it ran out of attempts **and** every
/// response that arrived carried no native payload — no `tool_calls` /
/// `function_call` at all, or one whose `arguments` was blank. Two things clear
/// the count:
///
/// - **The mode answered the call.** Whether the payload came from the native
///   field or from `content` is irrelevant — the mode did its job.
/// - **A native payload arrived**, even if it then failed to parse or validate.
///   This is a deliberate false-positive guard rather than a claim about the
///   server: a model that occasionally returns malformed JSON would otherwise
///   accumulate misses and get its mode disabled for
///   [`Self::RE_PROBE_INTERVAL`] calls, and JSON mode sends only a
///   `schema_to_example` template rather than the real schema, so a wrong skip
///   costs extraction quality. Malformed output is the corrective-retry
///   ladder's job, not the probe's.
///
/// **Known limitation.** That second rule means an endpoint whose native field
/// arrives non-blank but *never* parses is not caught: it clears the count on
/// every call, the mode never trips, and the full cascade is re-paid — the same
/// waste profile as a missing parser, from a different cause. Catching it would
/// mean counting parse failures as misses, which reintroduces the false-positive
/// above. Left uncaught deliberately; the corrective-retry ladder and the
/// `ValidationMiss` short-circuit are the mechanisms aimed at bad payloads.
///
/// Python needs no equivalent: it pins one instructor mode per provider from a
/// static table (`instructor_modes.py`) and stays there, overridable by
/// `llm_instructor_mode`. The cascade is Rust-only, so bounding it is bounding
/// our own mechanism rather than diverging from Python.
///
/// The memory is deliberately **not** permanent. Once tripped it still lets one
/// call in every [`Self::RE_PROBE_INTERVAL`] try the mode, so an endpoint that
/// gains a parser (a redeploy behind a stable URL) recovers on its own and a
/// run of bad responses cannot disable a mode for the life of the process.
#[derive(Debug)]
struct ModeProbe {
    /// Structured calls in a row in which this mode ran to exhaustion and every
    /// response that arrived carried no native payload. Reset by any call the
    /// mode answered, and by any non-blank native payload.
    consecutive_misses: AtomicU32,
    /// Calls that have skipped this mode since the last attempt at it. Drives
    /// the periodic re-probe.
    skipped_since_probe: AtomicU32,
}

impl ModeProbe {
    /// Consecutive misses before a mode is treated as not worth sending.
    ///
    /// Three rather than one: a single miss is far more likely to be a bad
    /// response than a missing parser, and being wrong in the cautious
    /// direction costs only two extra cascades.
    const MISS_THRESHOLD: u32 = 3;

    /// Calls skipped between re-probes once tripped. At 64 the residual waste
    /// is ~1.6% of calls, against the ~71% an unbounded cascade burns.
    const RE_PROBE_INTERVAL: u32 = 64;

    fn new() -> Self {
        Self {
            consecutive_misses: AtomicU32::new(0),
            skipped_since_probe: AtomicU32::new(0),
        }
    }

    /// Whether this call should attempt the mode.
    ///
    /// `Relaxed` throughout: the counters are a heuristic, and the worst a
    /// racing pair of calls can do is probe once more or once less than
    /// intended.
    fn should_try(&self) -> bool {
        if self.consecutive_misses.load(Ordering::Relaxed) < Self::MISS_THRESHOLD {
            return true;
        }
        if self.skipped_since_probe.fetch_add(1, Ordering::Relaxed) >= Self::RE_PROBE_INTERVAL {
            self.skipped_since_probe.store(0, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Record that the mode is worth sending: it either answered the call, or a
    /// non-blank native payload arrived even though that payload was unusable.
    /// Clears any accumulated suspicion.
    fn record_useful(&self) {
        self.consecutive_misses.store(0, Ordering::Relaxed);
        self.skipped_since_probe.store(0, Ordering::Relaxed);
    }

    /// Record that the mode ran out of attempts having produced nothing usable,
    /// on responses that actually arrived. Returns the new consecutive count so
    /// the caller can log what was observed rather than the constant.
    fn record_useless(&self) -> u32 {
        self.consecutive_misses.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Consecutive misses observed so far, for logging.
    fn misses(&self) -> u32 {
        self.consecutive_misses.load(Ordering::Relaxed)
    }
}

/// Cascade-mode memory for one endpoint.
///
/// Tool calling and legacy `functions` are tracked **separately**: both need a
/// server-side parser, but they are different parsers, and the cascade exists
/// precisely because a server may accept one and not the other. Collapsing them
/// into one counter would skip legacy mode on a server that supports only
/// legacy. JSON mode has no probe -- it is the terminal fallback and the only
/// mode that needs no server-side parsing, so there is nothing to fall back to
/// if it were skipped.
#[derive(Debug)]
struct CascadeProbe {
    tools: ModeProbe,
    legacy: ModeProbe,
}

impl CascadeProbe {
    fn new() -> Self {
        Self {
            tools: ModeProbe::new(),
            legacy: ModeProbe::new(),
        }
    }
}

#[derive(Clone)]
pub struct OpenAIAdapter {
    model: String,
    api_key: String,
    base_url: String,
    /// When `Some`, the adapter targets Azure OpenAI: requests authenticate with
    /// the `api-key` header (not `Authorization: Bearer`) and carry an
    /// `?api-version=<v>` query parameter. `None` is the standard OpenAI path.
    api_version: Option<String>,
    client: Client,
    structured_output_retries: usize,
    /// Minimum number of HTTP attempts before the request is allowed to fail.
    ///
    /// This is a *floor*, not a cap — see [`Self::retry_budget`].
    network_retries: usize,
    /// Minimum elapsed time before the request is allowed to fail. Paired with
    /// `network_retries` to form Python's dual-floor stop condition.
    retry_min_elapsed: Duration,
    /// Dispatch pacer. `None` leaves the adapter unpaced, which is what a plain
    /// library user without a component factory gets.
    pacer: Option<Arc<Pacer>>,
    /// Model name for audio transcription (e.g. `"whisper-1"`).
    transcription_model: String,
    /// Extra request parameters merged into every chat-completion request body,
    /// mirroring Python cognee's `LLM_ARGS` / `llm_config.llm_args`, which the
    /// litellm adapter merges into each call as `{**self.llm_args, **kwargs}`
    /// (see `openai/adapter.py`). Keys already present on the built request body
    /// (the explicit "kwargs", e.g. `model`, `messages`, an options-supplied
    /// `max_tokens`) win, so these only ever *fill gaps*. The canonical use is
    /// `{"max_tokens": 16384}` to lift a provider's small default output cap that
    /// would otherwise truncate a dense graph-extraction tool call mid-JSON.
    /// Empty by default (Python default `llm_args = {}`) — a no-op.
    extra_args: serde_json::Map<String, Value>,
    /// Default output-token ceiling applied to a plain-completion
    /// ([`generate`](Self::generate)) request when the caller passes **no**
    /// [`GenerationOptions`] at all. Lowered from
    /// `Settings.llm_max_completion_tokens` (`setLlmMaxCompletionTokens`) by the
    /// component factory so config actually governs the `max_tokens` the
    /// search/completion retrievers emit (issue #67).
    ///
    /// This is a *global* completion ceiling, not an answer-length knob: it
    /// applies to every option-less `generate` call — the user-facing
    /// `recall`/`search` answer **and** the internal machine generations that
    /// share that path (NL→Cypher query synthesis, the feeling-lucky retriever
    /// selector, graph-summary intermediates). That is deliberate: its primary
    /// purpose is to stay under a provider's hard `max_tokens` limit (e.g. Groq
    /// rejects `> 8192`), which requires *all* of those calls to respect it —
    /// leaving any uncapped would still emit the 16384 default and 400 on such a
    /// provider. Set it high enough to satisfy the largest internal generation.
    ///
    /// Scope and precedence:
    /// - Only the *fully option-less* `generate` path is filled from this
    ///   default. A caller that passes explicit `GenerationOptions` keeps full
    ///   control of `max_tokens` (including `None` = no cap), so the config
    ///   default never overrides an explicit choice.
    /// - Structured-output extraction (`structured_output_impl`) deliberately
    ///   ignores this *configured* default and keeps the historical
    ///   [`GenerationOptions::default`] cap (16384) for option-less calls: a
    ///   lower cap there would truncate tool-call JSON mid-object, so lowering
    ///   the completion ceiling never silently breaks internal structured calls
    ///   (e.g. feedback detection). Callers wanting NO cap pass explicit
    ///   `max_tokens: None` (as cognify's extraction paths do).
    ///
    /// `None` means "send no default cap". Defaults to
    /// [`Some(DEFAULT_MAX_COMPLETION_TOKENS)`](Self::DEFAULT_MAX_COMPLETION_TOKENS),
    /// matching the historical [`GenerationOptions::default`] cap so an adapter
    /// built without config behaves exactly as before.
    default_max_tokens: Option<u32>,
    /// Whether this adapter's `(model, base_url)` pair is an OpenAI reasoning
    /// model that needs the reasoning parameter shape. Computed once at
    /// construction (both inputs are fixed there) so the per-request build path
    /// does not re-parse `base_url` on every `is_reasoning_model()` call. See
    /// [`compute_reasoning_model`].
    reasoning_model: bool,
    /// Wall-clock ceiling on **one logical structured-output call** - spanning
    /// every cascade mode, every corrective re-ask, and every transport retry
    /// inside them. `None` leaves the call unbounded.
    ///
    /// Why a separate bound is needed: the reqwest client timeout is per-**HTTP
    /// request**, and nothing composed it into an aggregate. A structured
    /// extraction runs up to three modes (tools, legacy functions, JSON), each
    /// `structured_output_retries` deep, each attempt carrying its own
    /// [`RetryBudget`](crate::retry) whose *time floor* keeps retrying for
    /// `min_retry_elapsed` before it is allowed to give up. Multiplied out, the
    /// designed worst case runs over an hour, which is how a call can burn 45
    /// minutes against an operator's expectation of a 900s cap.
    ///
    /// Enforced in two places, which together cover the whole call: at the head
    /// of each cascade attempt, and inside the transport retry ladder, which
    /// refuses to start a further attempt once the budget is spent and clamps its
    /// backoff sleep to what remains.
    ///
    /// It bounds *starting* work rather than cancelling it, so a request already
    /// on the wire when the budget expires still runs to its own timeout: the
    /// effective ceiling is `request_deadline + request_timeout`, both
    /// configurable. Cancelling mid-flight would need the whole call wrapped in
    /// `tokio::time::timeout`, which would abandon a response the provider has
    /// already been paid for.
    request_deadline: Option<Duration>,
    /// Which cascade modes have proved worth sending to this endpoint.
    ///
    /// Shared across clones so the memory belongs to the endpoint, not to a
    /// particular handle on it. See [`CascadeProbe`] for why the cascade needs
    /// this at all.
    cascade_probe: Arc<CascadeProbe>,
}

/// Whether `model` is an OpenAI reasoning family (`gpt-5*`, `o1*`, `o3*`, `o4*`)
/// served from a host that requires the reasoning parameter shape.
///
/// Detection is name-based and host-agnostic for remote hosts, so it fires on
/// both official OpenAI and Azure deployments (`*.openai.azure.com`) of these
/// models, which both require the reasoning parameter shape. (A host gate on
/// `api.openai.com` used to leave Azure o-series/gpt-5 deployments sending
/// `max_tokens`+`temperature`, which Azure 400s on every call.) It is suppressed
/// only for a local host (see [`is_local_base_url`]) — loopback, or Ollama's
/// default port on a private-network address — which keeps accepting the legacy
/// parameters even when a served model name collides with a reasoning prefix.
///
/// A self-hosted gateway on a private network but a non-Ollama port (e.g. a LAN
/// vLLM at `http://192.168.1.5:8000`) is treated as remote and gets the
/// reasoning shape; such a host is often a proxy to real OpenAI, where that
/// shape is correct. A deployment that needs the opposite can point the model at
/// a loopback bind or use Ollama's port.
fn compute_reasoning_model(model: &str, base_url: &str) -> bool {
    if is_local_base_url(base_url) {
        return false;
    }
    // Name-based detection on the request-body model, plus a fallback on the
    // Azure deployment segment: Azure ignores the body model (the deployment in
    // the URL routes the request), and the config docs tell operators the model
    // is inert, so an o-series/gpt-5 deployment named after its model would
    // otherwise go undetected and 400 on `max_tokens`+`temperature`.
    is_reasoning_model_name(model)
        || azure_deployment_name(base_url).is_some_and(|d| is_reasoning_model_name(&d))
}

/// Whether a model name belongs to an OpenAI reasoning family (`gpt-5*`, `o1*`,
/// `o3*`, `o4*`), case-insensitively.
fn is_reasoning_model_name(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

/// Extract the Azure deployment name from a deployment-style base URL
/// (`https://<resource>.openai.azure.com/openai/deployments/<deployment>`) — the
/// path segment immediately following a `deployments` segment. Returns `None`
/// for any URL without that shape, so it is a no-op for standard OpenAI /
/// OpenAI-compatible endpoints.
///
/// Gated on the Azure host (`*.openai.azure.com`): only Azure encodes the model
/// in a `deployments/<name>` segment, so a non-Azure gateway whose path happens
/// to contain a `deployments` route segment (e.g.
/// `https://gw.example.com/deployments/o3-router/v1`) is NOT misclassified as a
/// reasoning model.
fn azure_deployment_name(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    if !host.to_ascii_lowercase().ends_with(".openai.azure.com") {
        return None;
    }
    let mut segments = url.path_segments()?;
    while let Some(seg) = segments.next() {
        if seg.eq_ignore_ascii_case("deployments") {
            return segments
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
    }
    None
}

/// Heuristic for a local / non-OpenAI-compatible host that does not accept the
/// reasoning-model parameter shape.
///
/// Matches on the parsed host authority, not a substring scan of the whole URL,
/// so a genuinely remote endpoint whose URL merely contains `localhost` as a
/// subdomain (`o3.localhost.example.com`) or `127.0.0.1` in a path/query is not
/// misclassified as local. Local means either:
/// - a loopback host (`localhost`, `127.0.0.0/8`, `::1`), any port; or
/// - Ollama's default port `11434` on a private-network host (loopback or an
///   RFC-1918 / link-local address). The port shortcut is gated on a private
///   host so a genuinely remote endpoint that merely listens on `11434`
///   (`https://gateway.example.com:11434`) is not classified local and still
///   gets the reasoning shape for a real o-series/gpt-5 model.
fn is_local_base_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        // An unparseable base_url can't be classified as local; treat it as
        // remote so a reasoning model still gets the correct parameter shape.
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // url keeps brackets on IPv6 literals; strip them so `[::1]` parses as `::1`.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        // A named remote host (not the `localhost` label) is not local.
        return false;
    };
    if ip.is_loopback() {
        return true;
    }
    let is_private = match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        // Unique-local (`fc00::/7`) detection is not stable on IpAddr; loopback
        // (handled above) covers the realistic local-IPv6 case.
        std::net::IpAddr::V6(_) => false,
    };
    is_private && url.port() == Some(11434)
}

impl OpenAIAdapter {
    /// Default OpenAI API base URL
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    /// Default retry attempts for structured output parsing paths.
    ///
    /// Python parity: instructor's `acreate_structured_output` retries up to
    /// `MAX_RETRIES = 5` times on a parse/validation failure. We match that
    /// count so transient malformed responses get the same number of repair
    /// chances before the cognify pipeline gives up.
    pub const DEFAULT_STRUCTURED_OUTPUT_RETRIES: usize = 5;
    /// Default retry attempts for transient network/server errors.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;
    /// Default minimum time a transient failure is retried for before the call
    /// is allowed to fail.
    ///
    /// Python parity: `LLM_MIN_RETRY_SECONDS = 240` in `retry_config.py`. The
    /// attempt count alone gives up in seconds, long before a provider's
    /// rate-limit window resets; the time floor is what actually carries a call
    /// through an overload episode.
    pub const DEFAULT_MIN_RETRY_ELAPSED: Duration = Duration::from_secs(240);
    /// Default output-token cap applied to option-less calls, mirroring the
    /// historical [`GenerationOptions::default`] cap and Python cognee's
    /// `llm_max_completion_tokens` default (`config.py`). Overridden per-adapter
    /// via [`with_default_max_tokens`](Self::with_default_max_tokens).
    pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = 16384;
    /// Default per-HTTP-request timeout. Unchanged from the value that used to be
    /// hardcoded in `new`, so an adapter built without config behaves as before.
    pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
    /// Default TCP connect timeout.
    ///
    /// `reqwest` applies none by default, so before this a black-holed connect -
    /// a stopped local Ollama, a wedged gateway - consumed the entire
    /// [request timeout](Self::DEFAULT_REQUEST_TIMEOUT) doing nothing. A connect
    /// either completes in well under ten seconds or is not going to.
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    /// Default aggregate ceiling for one logical structured-output call.
    ///
    /// Chosen to sit above the legitimate retry envelope and below the
    /// pathological one. That envelope is
    /// `CASCADE_MODES x max_retries x min_retry_seconds` — all three factors,
    /// since each of the three modes is retried `max_retries` times and every
    /// attempt honours the time floor before it may give up. At the defaults
    /// (3 x 2 x 240s) that is 24 minutes of *deliberate* waiting, which a call
    /// surviving a provider rate-limit window genuinely needs; the unbounded
    /// worst case ran past an hour. 30 minutes preserves the former and cuts the
    /// latter. Keep this figure in step with the ladder computed in
    /// `cognee_components::builtins::llm`, which warns when a configured
    /// deadline does not fit inside it.
    pub const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(1800);

    /// Create a new OpenAI adapter.
    ///
    /// # Arguments
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `api_key` - OpenAI API key
    /// * `base_url` - Optional custom base URL (defaults to OpenAI's API)
    ///
    /// # Returns
    /// A new OpenAI adapter instance
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
    ) -> LlmResult<Self> {
        let client =
            Self::build_http_client(Self::DEFAULT_REQUEST_TIMEOUT, Self::DEFAULT_CONNECT_TIMEOUT)?;

        let transcription_model =
            std::env::var("TRANSCRIPTION_MODEL").unwrap_or_else(|_| "whisper-1".to_string());

        // The model is used verbatim on the wire. litellm-style provider prefix
        // stripping (`openai/`, `baseten/`, …) is owned by
        // `build_openai_compatible_adapter`, which has the provider/endpoint
        // context needed to strip correctly (and to leave `custom` slugs
        // untouched). Stripping here as well would wrongly mangle real slugs
        // that legitimately contain a slash (e.g. Baseten's `openai/gpt-oss-120b`).
        let model: String = model.into();

        // Normalise a trailing slash so request URLs built as
        // `{base_url}/chat/completions` never produce a double slash. The
        // Gemini OpenAI-compat base ends in `/v1beta/openai/`, and a
        // user-supplied endpoint may too; both would otherwise 404.
        let base_url = base_url
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string());

        // Both `model` and `base_url` are fixed for the adapter's lifetime, so
        // classify once here instead of re-parsing `base_url` on every request.
        let reasoning_model = compute_reasoning_model(&model, &base_url);

        Ok(Self {
            model,
            api_key: api_key.into(),
            base_url,
            api_version: None,
            client,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            retry_min_elapsed: Self::DEFAULT_MIN_RETRY_ELAPSED,
            pacer: None,
            transcription_model,
            extra_args: serde_json::Map::new(),
            default_max_tokens: Some(Self::DEFAULT_MAX_COMPLETION_TOKENS),
            reasoning_model,
            // Off unless configured, so constructing an adapter directly (tests,
            // embedders, downstream users of the crate) keeps the historical
            // unbounded behaviour. The component factory opts in from settings.
            request_deadline: None,
            cascade_probe: Arc::new(CascadeProbe::new()),
        })
    }

    /// Set extra request parameters merged into every chat-completion request,
    /// mirroring Python cognee's `LLM_ARGS` / `llm_config.llm_args`.
    ///
    /// Merge semantics match Python's `{**self.llm_args, **kwargs}`: an entry is
    /// only applied when the request body does not already carry that key, so
    /// explicitly-set parameters (model, messages, an options-supplied
    /// `max_tokens`, …) always win. See the [`extra_args`](Self::extra_args)
    /// field docs for the primary use case (lifting a provider output cap).
    pub fn with_extra_args(mut self, args: serde_json::Map<String, Value>) -> Self {
        self.extra_args = args;
        self
    }

    /// Set the output-token cap applied to an option-less
    /// [`generate`](Self::generate) call, lowered from
    /// `Settings.llm_max_completion_tokens` (`setLlmMaxCompletionTokens`). See
    /// the [`default_max_tokens`](Self::default_max_tokens) field docs for scope
    /// and precedence.
    ///
    /// A `Some(0)` is treated as "no cap" (`None`): `0` is meaningless as an
    /// output cap and providers reject `max_tokens: 0` with HTTP 400, so a
    /// stray `setLlmMaxCompletionTokens(0)` must not break `recall`/`search`.
    pub fn with_default_max_tokens(mut self, value: Option<u32>) -> Self {
        self.default_max_tokens = value.filter(|&v| v > 0);
        self
    }

    /// Override reasoning-model auto-detection (see [`compute_reasoning_model`]).
    /// `Some(true)` forces the reasoning parameter shape (`max_completion_tokens`,
    /// suppressed `temperature`/`top_p`/penalties), `Some(false)` forces the
    /// legacy shape (`max_tokens` + sampling params), and `None` leaves the
    /// name/host auto-detection untouched.
    ///
    /// Wired from `LLM_REASONING` (`auto` | `always` | `never`) so an operator can
    /// correct a misclassified endpoint — e.g. a remote OpenAI-compatible gateway
    /// serving a reasoning-*named* model that nonetheless only accepts the legacy
    /// parameters (`LLM_REASONING=never`), or a proxy that hides the reasoning
    /// nature of its model behind an opaque alias (`LLM_REASONING=always`).
    pub fn with_reasoning_override(mut self, force: Option<bool>) -> Self {
        if let Some(force) = force {
            self.reasoning_model = force;
        }
        self
    }

    /// Resolve caller options for the plain-completion ([`generate`](Self::generate))
    /// path. When the caller passes no options at all, the config-derived
    /// [`default_max_tokens`](Self::default_max_tokens) becomes the output cap
    /// (every other field takes [`GenerationOptions::default`]); when the caller
    /// passes explicit options they are honoured verbatim, so an explicit
    /// `max_tokens` (including `None` = no cap) always wins over config. This
    /// applies the configured completion ceiling to every option-less
    /// `generate` call while leaving explicit callers (and, separately,
    /// structured extraction) alone.
    fn resolve_options(&self, options: Option<GenerationOptions>) -> GenerationOptions {
        match options {
            Some(opts) => opts,
            None => GenerationOptions {
                max_tokens: self.default_max_tokens,
                ..GenerationOptions::default()
            },
        }
    }

    /// Merge [`extra_args`](Self::extra_args) into a request body, filling only
    /// keys that are not already present (explicit params win — Python parity).
    fn apply_extra_args(&self, body: &mut Value) {
        if self.extra_args.is_empty() {
            return;
        }
        // Reasoning models (`gpt-5*`/`o1*`/`o3*`/`o4*`, detected by name/host —
        // NOT gated on api.openai.com, so this covers Azure o-series deployments
        // and remote gateways too) constrain the request body two ways:
        //   1. they require `max_completion_tokens` and reject `max_tokens`, and
        //   2. they reject `temperature`/`top_p`/`frequency_penalty`/
        //      `presence_penalty`.
        // The request builder already honours both (writes `max_completion_tokens`
        // via `write_max_tokens` and omits the sampling params for reasoning
        // models), so an `LLM_ARGS` value must not re-introduce a rejected key
        // here: fold a bare `max_tokens` into `max_completion_tokens`, drop the
        // suppressed sampling params, and only fill gaps for everything else
        // (matching Python's `{**self.llm_args, **kwargs}`).
        let reasoning = self.is_reasoning_model();
        if let Some(obj) = body.as_object_mut() {
            for (key, value) in &self.extra_args {
                if reasoning {
                    if key == "max_tokens" {
                        obj.entry("max_completion_tokens")
                            .or_insert_with(|| value.clone());
                        continue;
                    }
                    if matches!(
                        key.as_str(),
                        "temperature" | "top_p" | "frequency_penalty" | "presence_penalty"
                    ) {
                        // Suppressed for reasoning models; re-adding 400s the call.
                        continue;
                    }
                }
                obj.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    /// Configure retry attempts for structured output extraction.
    ///
    /// Values lower than 1 are coerced to 1.
    pub fn with_structured_output_retries(mut self, retries: u32) -> Self {
        let retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self.structured_output_retries = retries.max(1);
        self
    }

    /// Configure the minimum attempts for transient network and server errors
    /// (HTTP 429, 5xx).
    ///
    /// Each retry uses exponential backoff starting at 8 s, doubling up to
    /// 128 s. Note this is a floor: the call also keeps retrying until
    /// [`with_min_retry_elapsed`](Self::with_min_retry_elapsed) is satisfied.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    /// Configure the minimum time transient failures are retried for.
    ///
    /// [`Duration::ZERO`] reduces the stop condition to a plain attempt cap.
    /// Build the HTTP client used for every request.
    ///
    /// Kept as one place so `new` and
    /// [`with_http_timeouts`](Self::with_http_timeouts) cannot drift in which
    /// timeouts they set.
    fn build_http_client(
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> LlmResult<Client> {
        let mut builder = Client::builder();
        // `0` means "no limit" for both, matching curl and the `0` escape hatch
        // on the aggregate deadline. Handled explicitly because the alternative
        // is actively dangerous: `reqwest` given `Duration::ZERO` times every
        // request out instantly, so an operator generalising "0 disables it"
        // from `LLM_REQUEST_DEADLINE_SECONDS` to its two neighbours would stop
        // all LLM traffic rather than lift a bound. The in-flight semaphore
        // treats its own `0` explicitly for the same class of reason.
        if !request_timeout.is_zero() {
            builder = builder.timeout(request_timeout);
        }
        if !connect_timeout.is_zero() {
            builder = builder.connect_timeout(connect_timeout);
        }
        builder
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))
    }

    /// Override the per-request and TCP-connect timeouts.
    ///
    /// Rebuilds the client, which is why this is a builder rather than a setter:
    /// it is only sound before any request is in flight. On the (TLS-init-only)
    /// failure path the existing client is kept and a warning logged, so a
    /// misconfigured timeout degrades to the defaults rather than failing
    /// component construction.
    pub fn with_http_timeouts(
        mut self,
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> Self {
        match Self::build_http_client(request_timeout, connect_timeout) {
            Ok(client) => self.client = client,
            Err(e) => warn!(
                error = %e,
                "failed to rebuild the LLM HTTP client with configured timeouts; keeping defaults",
            ),
        }
        self
    }

    /// Set the aggregate ceiling for one logical structured-output call.
    ///
    /// `None` disables it. See the `request_deadline` field for what it does and
    /// does not bound.
    pub fn with_request_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.request_deadline = deadline;
        self
    }

    /// The deadline error for a call that started at `started`, if the budget is
    /// set and already spent.
    ///
    /// Returns the error rather than a bool so the message can name the budget,
    /// the elapsed time and the stage that was about to be entered - without
    /// that, an aggregate cut is indistinguishable from a provider timeout in a
    /// log.
    fn deadline_exceeded(&self, started: Instant, next_stage: &str) -> Option<LlmError> {
        let deadline = self.request_deadline?;
        let elapsed = started.elapsed();
        if elapsed < deadline {
            return None;
        }
        Some(LlmError::Timeout(format!(
            "structured output exceeded its {}s aggregate budget \
             (LLM_REQUEST_DEADLINE_SECONDS) after {:.0}s, before {next_stage}; raise the \
             budget, or lower LLM_MAX_RETRIES / LLM_MIN_RETRY_SECONDS so the retry \
             ladder fits inside it",
            deadline.as_secs(),
            elapsed.as_secs_f64(),
        )))
    }

    pub fn with_min_retry_elapsed(mut self, min_elapsed: Duration) -> Self {
        self.retry_min_elapsed = min_elapsed;
        self
    }

    /// Attach a dispatch pacer, overriding the process-wide one.
    pub fn with_pacer(mut self, pacer: Arc<Pacer>) -> Self {
        self.pacer = Some(pacer);
        self
    }

    /// The stop condition for this adapter's transient-failure retries.
    fn retry_budget(&self) -> crate::retry::RetryBudget {
        crate::retry::RetryBudget::new(
            u32::try_from(self.network_retries).unwrap_or(u32::MAX),
            self.retry_min_elapsed,
        )
    }

    /// The pacer governing this adapter: an explicitly attached one, else the
    /// process-wide one if a factory installed it, else unpaced.
    fn pacer(&self) -> Option<Arc<Pacer>> {
        self.pacer.clone().or_else(llm_pacer)
    }

    /// Configure the model used for audio transcription (default: `"whisper-1"`).
    pub fn with_transcription_model(mut self, model: impl Into<String>) -> Self {
        self.transcription_model = model.into();
        self
    }

    /// Build the authorization header value
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// Enable Azure OpenAI mode by setting the API version. An empty/whitespace
    /// value is treated as unset (stays on the standard OpenAI path). In Azure
    /// mode the `base_url` is expected to be the deployment endpoint
    /// (`https://<resource>.openai.azure.com/openai/deployments/<deployment>`),
    /// so `{base_url}/chat/completions?api-version=<v>` is the Azure request URL.
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        let v = api_version.into();
        let trimmed = v.trim();
        // Store trimmed: `endpoint_url` interpolates this raw, so trailing
        // whitespace would percent-encode to `?api-version=...%20` and Azure
        // 400s every request. (Matches the embedding path's api-version handling.)
        self.api_version = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self
    }

    /// Build a request URL for `path`, appending `api-version=<v>` in Azure mode.
    ///
    /// `path` is appended to the base URL's **path segments**, not by raw string
    /// concatenation. This matters when `base_url` already carries a query — e.g.
    /// an Azure portal endpoint copied with a trailing `?api-version=...`: a naive
    /// `{base_url}/{path}` would splice `chat/completions` into the *query* value
    /// (`?api-version=…/chat/completions`), leaving the request path without
    /// `/chat/completions` so every request 404s. Building on the parsed path
    /// keeps the existing query and the route segment separate.
    ///
    /// The api-version is then added through `Url::query_pairs_mut`, so the query
    /// separator is chosen correctly even if `base_url` already carries a query
    /// (no malformed double-`?`) and the value is percent-encoded rather than
    /// interpolated raw. Any `api-version` already present on `base_url` (e.g. a
    /// copied Azure portal "Target URI") is dropped first so the request never
    /// carries duplicate/conflicting `api-version` params — the configured
    /// `LLM_API_VERSION` wins, and other query params are preserved. If `base_url`
    /// does not parse as a URL we fall back to a manual append that still picks
    /// `?` vs `&` from the existing query, so the api-version is never silently
    /// dropped (Azure 400s without it).
    fn endpoint_url(&self, path: &str) -> String {
        if let Ok(mut url) = reqwest::Url::parse(&self.base_url) {
            if let Ok(mut segments) = url.path_segments_mut() {
                // Drop a trailing empty segment (a base_url ending in `/`) before
                // extending so we never emit `//`. The constructor trims one
                // trailing slash, but a bare-host base_url (`https://x.com`) still
                // parses with a single empty segment.
                segments
                    .pop_if_empty()
                    .extend(path.split('/').filter(|s| !s.is_empty()));
            }
            // A cannot-be-a-base URL (e.g. `mailto:`) has no path segments; that
            // never happens for the http(s) endpoints we accept, so leave it.
            if let Some(v) = &self.api_version {
                // Preserve every existing query pair EXCEPT a stray `api-version`,
                // then append the configured one, so a base_url that already
                // carries `api-version=...` yields exactly one (the configured
                // value wins) rather than a duplicate Azure may reject.
                let preserved: Vec<(String, String)> = url
                    .query_pairs()
                    .filter(|(k, _)| k != "api-version")
                    .map(|(k, val)| (k.into_owned(), val.into_owned()))
                    .collect();
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(preserved)
                    .append_pair("api-version", v);
            }
            return url.into();
        }
        // base_url did not parse as a URL: fall back to a raw append that still
        // chooses `?` vs `&` so the api-version is never silently dropped.
        let base = format!("{}/{path}", self.base_url);
        match &self.api_version {
            Some(v) => {
                let sep = if base.contains('?') { '&' } else { '?' };
                format!("{base}{sep}api-version={v}")
            }
            None => base,
        }
    }

    /// Apply the provider's auth header: `api-key` for Azure, `Authorization:
    /// Bearer` for standard OpenAI / OpenAI-compatible endpoints.
    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_version {
            Some(_) => req.header("api-key", &self.api_key),
            None => req.header("Authorization", self.auth_header()),
        }
    }

    /// Whether to request non-thinking mode for local Qwen OpenAI-compatible endpoints.
    fn should_disable_thinking(&self) -> bool {
        self.model.to_lowercase().starts_with("qwen") && !self.base_url.contains("api.openai.com")
    }

    /// True for OpenAI reasoning-model families (`gpt-5*`, `o1*`, `o3*`, `o4*`)
    /// that reject `temperature`/`top_p`/`frequency_penalty`/`presence_penalty`
    /// overrides and require `max_completion_tokens` in place of `max_tokens`.
    ///
    /// Returns the value classified once at construction (see
    /// [`compute_reasoning_model`]); no per-call URL parsing.
    fn is_reasoning_model(&self) -> bool {
        self.reasoning_model
    }

    /// Insert `max_tokens` (or `max_completion_tokens` on reasoning models) into a
    /// request body if `value` is `Some`.
    fn write_max_tokens(&self, body: &mut Value, value: Option<u32>) {
        if let Some(v) = value {
            let key = if self.is_reasoning_model() {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[key] = json!(v);
        }
    }

    /// `finish_reason` values meaning the answer was cut off at the output budget
    /// rather than finishing on its own.
    ///
    /// OpenAI and vLLM report `length`. Some OpenAI-compatible gateways front an
    /// Anthropic backend and echo its `max_tokens` spelling instead, so both are
    /// treated as truncation.
    fn is_length_truncated(choice: &OpenAIChoice) -> bool {
        matches!(
            choice.finish_reason.as_deref(),
            Some("length") | Some("max_tokens")
        )
    }

    /// The output budget that will actually reach the provider, or `0` when the
    /// request carries no cap and none can be inferred.
    ///
    /// Reading the body alone is not enough. [`apply_extra_args`](Self::apply_extra_args)
    /// merges `LLM_ARGS` inside `call_api`, *after* this request is built, and it
    /// only fills keys that are absent — so a body with no cap is not uncapped, it
    /// may still carry an `LLM_ARGS` budget on the wire. Writing a key here would
    /// suppress that value; a raise computed from the body alone could therefore
    /// *lower* the budget that just truncated, and re-truncate at a smaller one.
    ///
    /// `0` means genuinely unknown — the provider's own default applies. That is
    /// the common case on the structured-output path, because the cognify call
    /// sites deliberately pass `max_tokens: None` for Python parity (Baseten then
    /// defaults to 4096). Reporting it as `0` is what lets the first truncation
    /// replace that silent default with an explicit ceiling.
    fn effective_output_budget(&self, body: &Value) -> u32 {
        let key = if self.is_reasoning_model() {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        if let Some(explicit) = body[key].as_u64() {
            return explicit as u32;
        }
        // Absent from the body: `apply_extra_args` may still supply one. It folds
        // a bare `max_tokens` into `max_completion_tokens` for reasoning models,
        // so accept either spelling there.
        let from_extra_args = if self.is_reasoning_model() {
            self.extra_args
                .get("max_completion_tokens")
                .or_else(|| self.extra_args.get("max_tokens"))
        } else {
            self.extra_args.get(key)
        };
        from_extra_args.and_then(|v| v.as_u64()).unwrap_or(0) as u32
    }

    /// React to a length-truncated structured-output attempt by raising the
    /// request's output budget, or fail terminally when it cannot be raised.
    ///
    /// Re-asking at the budget that just truncated would truncate at the same
    /// point, so the next attempt is sent at the configured
    /// `llm_max_completion_tokens` ceiling. Returns the reason to carry into the
    /// corrective instruction, and the new budget so later cascade modes inherit
    /// it (they rebuild their bodies from `opts` and would otherwise drop back to
    /// the budget that already failed).
    ///
    /// Two cases are terminal rather than raised:
    ///
    /// - **The caller set `max_tokens` itself.** Overriding it would silently bill
    ///   a much larger completion than was asked for — the HTTP `custom-prompt`
    ///   route forwards client-supplied budgets straight into structured output,
    ///   so a client requesting 200 tokens could be re-issued at the ceiling.
    ///   A deliberate budget is treated as a constraint, not a suggestion.
    /// - **The budget is already at or above the ceiling.** Raising is impossible
    ///   and falling through to another request *mode* cannot help, because the
    ///   legacy and JSON-mode requests carry the same budget and truncate
    ///   identically.
    ///
    /// Mirrors the Anthropic adapter, which has rejected a `stop_reason ==
    /// "max_tokens"` response since it shipped, with one deliberate difference:
    /// Anthropic raises over a caller-supplied budget, this does not.
    fn raise_budget_after_truncation(
        &self,
        body: &mut Value,
        mode: &str,
        caller_requested: Option<u32>,
    ) -> LlmResult<(String, u32)> {
        let current = self.effective_output_budget(body);
        let ceiling = self.max_completion_tokens().max(1);

        if let Some(requested) = caller_requested {
            return Err(LlmError::InvalidResponse(format!(
                "{mode} structured output was truncated at the caller-requested \
                 {requested}-token output budget before the JSON object was complete. An \
                 explicit max_tokens is not raised automatically; request a larger one"
            )));
        }

        if current >= ceiling {
            // Name the budget that actually truncated, not the ceiling — they
            // differ whenever an option-less call sends the GenerationOptions
            // default while a lower ceiling is configured, and pointing at the
            // ceiling there sends the operator to raise a setting that changes
            // nothing.
            let what = if current == 0 {
                "its output budget".to_string()
            } else {
                format!("its {current}-token output budget")
            };
            return Err(LlmError::InvalidResponse(format!(
                "{mode} structured output was truncated at {what} before the JSON object was \
                 complete, and the configured ceiling (llm_max_completion_tokens = {ceiling}) \
                 is no higher, so the budget cannot be raised automatically. Raise \
                 LLM_MAX_COMPLETION_TOKENS above {current}, or set a larger cap with \
                 LLM_ARGS='{{\"max_tokens\": N}}' (CLI and SDK only — the HTTP server does not \
                 read LLM_ARGS)"
            )));
        }

        self.write_max_tokens(body, Some(ceiling));
        let previous = if current == 0 {
            "provider-default".to_string()
        } else {
            format!("{current}-token")
        };
        Ok((
            format!(
                "the previous answer was cut off at the {previous} output budget before the \
                 JSON object was complete; the budget has been raised to {ceiling}"
            ),
            ceiling,
        ))
    }

    /// Call the OpenAI chat completions API, retrying on transient network/server errors.
    ///
    /// Retries up to `self.network_retries` times with exponential backoff (1 s, 2 s, 4 s …
    /// capped at 30 s) on:
    /// - Network-level failures (connection refused, timeout, etc.)
    /// - HTTP 429 (rate limit exceeded)
    /// - HTTP 5xx (server errors)
    ///
    /// Errors on HTTP 400 and 401 are returned immediately without retrying.
    async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        self.call_api_before(request_body, None).await
    }

    /// Send a chat request with no aggregate deadline.
    ///
    /// Delegates to the instrumented
    /// [`send_chat_request_before`](Self::send_chat_request_before) so both entry
    /// points produce exactly one `llm.api_call` span. The attribute deliberately
    /// lives on the callee rather than here: the structured-output and chat paths
    /// reach the transport through `call_api_before`, never through this wrapper,
    /// so instrumenting the wrapper would drop the span on every call that
    /// matters.
    async fn send_chat_request(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        self.send_chat_request_before(request_body, None).await
    }

    /// [`call_api`](Self::call_api) with an absolute ceiling on when the
    /// transport retry ladder may still start another attempt.
    ///
    /// Structured output passes its aggregate budget down here so the ladder is
    /// covered too. Without it the cascade guards bound only the *gaps* between
    /// attempts, while the ladder inside one attempt kept retrying — with its own
    /// `min_retry_elapsed` floor and 8-128s backoff — past a spent budget.
    async fn call_api_before(
        &self,
        mut request_body: Value,
        deadline: Option<Instant>,
    ) -> LlmResult<OpenAIResponse> {
        // Merge configured `LLM_ARGS` (Python `llm_config.llm_args`) into every
        // chat-completion / structured-output request. Only fills keys the request
        // does not already set, so explicit parameters win — Python's
        // `{**self.llm_args, **kwargs}`. Scoped to the chat/structured paths: the
        // transcription (vision) path calls `send_chat_request` directly so a
        // graph-extraction `LLM_ARGS` (e.g. a large `max_tokens`) never leaks into
        // an image-description request.
        self.apply_extra_args(&mut request_body);
        self.send_chat_request_before(request_body, deadline).await
    }

    /// Perform the actual chat-completions HTTP POST, retrying on transient
    /// network/server errors. Does *not* merge [`extra_args`](Self::extra_args) —
    /// callers that want the `LLM_ARGS` merge go through [`call_api`](Self::call_api).
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body, deadline),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn send_chat_request_before(
        &self,
        request_body: Value,
        deadline: Option<Instant>,
    ) -> LlmResult<OpenAIResponse> {
        let url = self.endpoint_url("chat/completions");
        tracing::Span::current().record("url", url.as_str());
        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|v| cognee_utils::parse_env_bool(&v))
            .unwrap_or(false);

        if debug_enabled {
            let pretty_request = serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!("\n[COGNEE_DEBUG_LLM_REQUEST] POST {url}\n{pretty_request}\n");
        }

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());
        let budget = self.retry_budget();
        let pacer = self.pacer();
        // Wall clock for the whole call, started before the loop and so before
        // both the pacer's admission wait and the in-flight queue below. It is
        // what the logs and the deadline errors report, and time queued is time
        // the caller is blocked, so it must include the waits.
        //
        // The retry *floor* is measured off it minus `queued_for_permit` — see
        // there. `deadline` needs no clock of its own: it arrives as an absolute
        // `Instant` fixed by the caller, so it already counts every wait.
        let started = Instant::now();
        // Time this call has spent queued for an in-flight permit, subtracted
        // from the elapsed time handed to `RetryBudget::is_exhausted`.
        //
        // That predicate is `attempts >= min_attempts && elapsed >= min_elapsed`,
        // so a *larger* elapsed can only exhaust the budget earlier, and
        // `min_elapsed` (`LLM_MIN_RETRY_SECONDS`) is a "keep retrying for at
        // least this long" resilience guarantee rather than a deadline. Charging
        // queue time against it silently weakens it: with `LLM_MAX_RETRIES=2` and
        // a 240s floor, a call that spent 300s waiting for a permit would stop
        // after its second attempt with no actual retrying done at all. The
        // aggregate deadline above is the mechanism that bounds total time; this
        // floor is not.
        let mut queued_for_permit = Duration::ZERO;
        // `Retry-After` from the previous attempt. A usable hint replaces the
        // computed backoff outright — including when it asks for less, since the
        // provider knows when its own window resets. See `retry::retry_after_hint`
        // for which hints count as usable.
        let mut retry_after: Option<Duration> = None;
        let mut attempt: u32 = 0;

        loop {
            debug!(attempt, "LLM API attempt");
            if attempt > 0 {
                let backoff = crate::retry::retry_backoff(attempt);
                // A usable hint replaces the backoff outright, including when it
                // asks for less: the provider knows when its window resets.
                let mut delay = retry_after.take().unwrap_or(backoff);
                // The caller's aggregate budget outranks the retry ladder. Give
                // up rather than start an attempt that cannot finish inside it,
                // and never sleep past it — a 128s backoff against 5s of
                // remaining budget would otherwise blow the ceiling on its own.
                if let Some(deadline) = deadline {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(LlmError::Timeout(format!(
                            "LLM request abandoned after {:.0}s with {attempt} attempt(s): the \
                             call's aggregate budget (LLM_REQUEST_DEADLINE_SECONDS) was spent \
                             mid-retry; last error: {last_error}",
                            started.elapsed().as_secs_f64(),
                        )));
                    }
                    delay = delay.min(remaining);
                }
                warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    error = %last_error,
                    "LLM request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            let dispatch_wait_started = Instant::now();

            // Admission sits INSIDE the retry loop, so an overload episode
            // opened by any concurrent request throttles the remaining attempts
            // of calls already in flight. This mirrors Python entering the
            // limiter context manager inside tenacity.
            //
            // This first admission is the one that can pace a caller *without*
            // an in-flight permit in hand, which is why it comes before the
            // queue below: parking in the bucket while holding a permit idles
            // the pool during exactly the episode the pacer is draining. Its
            // return value records whether it actually cost a token — the second
            // admission after the queue reads it.
            let paced_before_queue = match pacer.as_deref() {
                Some(pacer) => pacer.admit().await,
                None => false,
            };

            // Transport-level concurrency ceiling, the analogue of the
            // connection-pool bound every Python HTTP client sets (see
            // `crate::in_flight`). Acquired *after* the pacer has admitted this
            // attempt and dropped at the end of the iteration, so a permit
            // covers only the window in which a socket actually exists. Taking
            // it before `admit()` would let requests parked in a 900s cooldown
            // hold permits they are not using, so the semaphore would count
            // sleepers as sockets and stall every other caller in the process.
            // A retry re-queues for a permit, which is correct: it opens a new
            // socket, so it is a new claim on the ceiling.
            let permit_queue_started = Instant::now();
            let _in_flight = crate::in_flight::acquire_in_flight().await;
            queued_for_permit += permit_queue_started.elapsed();

            // The pacer's contract is admission *immediately before the send*
            // (`cognee_utils::pacing` module docs) and the queue above breaks it.
            // Pacing is off by default, so N callers clear `admit()` on the fast
            // path in one go and then pile up on the semaphore; the first reply
            // is a 429, `record_overload` opens the 900s episode — and every
            // caller already past the pacer still fires one unpaced send at a
            // provider that has just said it is overloaded. That burst is what
            // opened the episode, and the queue would let it through again.
            //
            // Skipped when the admission above actually paced this attempt: that
            // caller has spent its token, and a second one would both halve the
            // configured rate during an episode and park it in the bucket with a
            // permit in hand — the failure the ordering above exists to avoid.
            // So an attempt costs exactly one token, and the only wait ever held
            // under a permit is the one no ordering can remove: an episode that
            // opened while this caller sat in the queue.
            if !paced_before_queue && let Some(pacer) = pacer.as_deref() {
                pacer.admit().await;
            }

            // Pacing and the in-flight queue can outlast the caller's aggregate
            // budget on their own — a 900s overload cooldown dwarfs a 240s
            // deadline — and the guard at the top of the loop cannot see that:
            // it runs before the wait, so an overshoot there was only noticed
            // one turn later, after an attempt the budget could never cover.
            //
            // Narrow on purpose. It fires only when budget *remained* when the
            // wait began and the wait is what spent it, so the guard above keeps
            // its existing behaviour: that one clamps its backoff to the
            // remaining budget and deliberately lets the attempt it sleeps for
            // start, and this must not retract it. Skipped on the first attempt
            // too — every call makes at least one, as it did before the deadline
            // existed.
            if attempt > 0
                && let Some(deadline) = deadline
                && dispatch_wait_started < deadline
                && Instant::now() >= deadline
            {
                return Err(LlmError::Timeout(format!(
                    "LLM request abandoned after {:.0}s with {attempt} attempt(s): the call's \
                     aggregate budget (LLM_REQUEST_DEADLINE_SECONDS) was spent waiting for \
                     dispatch (pacing or the in-flight queue); last error: {last_error}",
                    started.elapsed().as_secs_f64(),
                )));
            }

            attempt += 1;

            let response = match self
                .apply_auth(self.client.post(&url))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if e.is_timeout()
                        && let Some(pacer) = pacer.as_deref()
                    {
                        pacer.record_overload("timeout");
                    }
                    last_error = LlmError::NetworkError(e.to_string());
                    if budget
                        .is_exhausted(attempt, started.elapsed().saturating_sub(queued_for_permit))
                    {
                        break;
                    }
                    continue;
                }
            };

            let status = response.status();

            if !status.is_success() {
                let code = status.as_u16();
                // Read the hint before the body is consumed.
                let hint = crate::retry::retry_after_hint(response.headers());
                if let Some(reason) = crate::retry::overload_reason(code)
                    && let Some(pacer) = pacer.as_deref()
                {
                    pacer.record_overload(reason);
                }

                let error_body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());

                // A 429 carrying quota/billing wording is terminal, not a rate
                // limit: no wait makes an exhausted balance succeed. Python
                // classifies these the same way via `is_quota_or_billing_error`.
                let quota_exhausted =
                    code == 429 && crate::retry::is_quota_or_billing_error(&error_body);

                let err = match code {
                    401 => LlmError::AuthenticationError(error_body),
                    402 => LlmError::PaymentRequired(error_body),
                    404 => LlmError::ModelNotFound(error_body),
                    429 if quota_exhausted => LlmError::PaymentRequired(error_body),
                    429 => LlmError::RateLimitExceeded(error_body),
                    400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
                };

                // Non-retryable: bad request, auth, billing (402), unknown model
                // (404), and quota exhaustion. All mirror Python's terminal set
                // in `should_retry_llm_exception`; retrying any of them only
                // burns the budget a recoverable error will need.
                if matches!(code, 400..=402 | 404) || quota_exhausted {
                    return Err(err);
                }

                retry_after = hint;
                last_error = err;
                if budget.is_exhausted(attempt, started.elapsed().saturating_sub(queued_for_permit))
                {
                    break;
                }
                continue;
            }

            let response_body = response.text().await.map_err(|e| {
                LlmError::DeserializationError(format!("Failed to read response body: {e}"))
            })?;

            if debug_enabled {
                eprintln!("\n[COGNEE_DEBUG_LLM_RESPONSE] POST {url}\n{response_body}\n");
            }

            return serde_json::from_str::<OpenAIResponse>(&response_body).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse response: {e}. Raw body: {response_body}"
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "LLM request failed after {} attempt(s) over {:.1}s: {}",
            attempt,
            started.elapsed().as_secs_f64(),
            last_error
        )))
    }

    /// Convert our Message type to OpenAI's format
    fn convert_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                json!({
                    "role": match msg.role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    },
                    "content": msg.content
                })
            })
            .collect()
    }

    /// Convert JSON Schema to an example JSON with placeholder values
    /// This is clearer for LLMs than showing the full schema
    fn schema_to_example(schema: &Value) -> String {
        fn create_example(value: &Value, definitions: Option<&Value>) -> Value {
            match value {
                Value::Object(obj) => {
                    // Handle $ref references
                    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str())
                        && let Some(def_name) = ref_str.strip_prefix("#/definitions/")
                        && let Some(defs) = definitions
                        && let Some(def) = defs.get(def_name)
                    {
                        return create_example(def, definitions);
                    }

                    // Get the type of this field
                    let type_val = obj.get("type");

                    // Handle arrays
                    if let Some(Value::String(t)) = type_val
                        && t == "array"
                    {
                        if let Some(items) = obj.get("items") {
                            // Return array with one example item
                            return json!([create_example(items, definitions)]);
                        }
                        return json!([]);
                    }

                    // Handle objects with properties
                    if let Some(props) = obj.get("properties")
                        && let Value::Object(props_obj) = props
                    {
                        let mut result = serde_json::Map::new();
                        for (key, val) in props_obj {
                            result.insert(key.clone(), create_example(val, definitions));
                        }
                        return Value::Object(result);
                    }

                    // Handle primitive types
                    if let Some(Value::String(t)) = type_val {
                        return match t.as_str() {
                            "string" => json!("example"),
                            "number" | "integer" => json!(0),
                            "boolean" => json!(false),
                            _ => json!(null),
                        };
                    }

                    // Handle union types (e.g., ["string", "null"])
                    if let Some(Value::Array(types)) = type_val {
                        for t in types {
                            if let Value::String(type_str) = t
                                && type_str != "null"
                            {
                                return match type_str.as_str() {
                                    "string" => json!("example"),
                                    "number" | "integer" => json!(0),
                                    "boolean" => json!(false),
                                    _ => json!(null),
                                };
                            }
                        }
                    }

                    json!(null)
                }
                _ => value.clone(),
            }
        }

        let definitions = schema.get("definitions");
        let example = create_example(schema, definitions);

        serde_json::to_string_pretty(&example).unwrap_or_else(|_| "{}".to_string())
    }

    /// Recompute the schema's top-level `required` array to list every property
    /// whose subschema carries no literal `"default"` key — a port of
    /// instructor's `generate_openai_schema` (`instructor/processing/schema.py`),
    /// which cognee's Python side relies on for its default `Mode.TOOLS` path.
    ///
    /// Why this matters: Python's pydantic emits no schema `"default"` for
    /// `default_factory` fields, so instructor's rewrite re-adds them to
    /// `required`, which is what makes the model reliably populate fields like
    /// `KnowledgeGraph.edges` on the *non-strict* tool-calling path. Rust/schemars
    /// already derives an equivalent `required` from the absence of
    /// `#[serde(default)]`, so for a correctly-derived schema this is a no-op; it
    /// exists to guarantee that parity holds for every response model and to keep
    /// the request byte-aligned with what litellm/instructor sends.
    ///
    /// Deliberately shallow: only the top-level `required` is recomputed. It does
    /// NOT recurse into `$defs`, set `additionalProperties:false`, or add
    /// `strict:true` — those drive grammar-constrained decoding and make Baseten's
    /// gpt-oss-120b return HTTP 501 (see the note in `structured_output_impl`).
    /// The mild top-level rewrite is verified to be accepted by Baseten.
    fn recompute_top_level_required(schema: &Value) -> Value {
        let mut schema = schema.clone();
        let Some(props) = schema.get("properties").and_then(Value::as_object) else {
            return schema;
        };
        let mut required: Vec<String> = props
            .iter()
            .filter(|(_, subschema)| subschema.get("default").is_none())
            .map(|(name, _)| name.clone())
            .collect();
        required.sort();
        if let Some(obj) = schema.as_object_mut() {
            if required.is_empty() {
                obj.remove("required");
            } else {
                obj.insert("required".to_string(), json!(required));
            }
        }
        schema
    }

    /// Append a corrective instruction so the next attempt carries the failure
    /// reason. Thin wrapper over the shared
    /// [`crate::schema::append_corrective_instruction`] naming the forced tool as
    /// OpenAI addresses it (a `function` call).
    fn append_corrective_instruction(request: &mut Value, reason: Option<&str>) {
        crate::schema::append_corrective_instruction(
            request,
            reason,
            "extract_structured_data",
            "function",
        );
    }
}

#[async_trait]
impl Llm for OpenAIAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = self.resolve_options(options);

        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
        });

        // Add optional parameters. Reasoning models (gpt-5*/o1*/o3*/o4*)
        // reject sampling overrides and only accept `max_completion_tokens`.
        if !self.is_reasoning_model() {
            if let Some(temp) = opts.temperature {
                request_body["temperature"] = json!(temp);
            }
            if let Some(top_p) = opts.top_p {
                request_body["top_p"] = json!(top_p);
            }
            if let Some(freq_penalty) = opts.frequency_penalty {
                request_body["frequency_penalty"] = json!(freq_penalty);
            }
            if let Some(pres_penalty) = opts.presence_penalty {
                request_body["presence_penalty"] = json!(pres_penalty);
            }
        }
        self.write_max_tokens(&mut request_body, opts.max_tokens);
        if let Some(stop) = opts.stop
            && !stop.is_empty()
        {
            request_body["stop"] = json!(stop);
        }

        if self.should_disable_thinking() {
            request_body["think"] = json!(false);
            request_body["reasoning"] = json!({"effort": "none"});
        }

        let response = self.call_api(request_body).await?;

        // Extract the first choice
        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

        Ok(GenerationResponse {
            content: choice.message.content.clone().unwrap_or_default(),
            model: response.model,
            finish_reason: choice.finish_reason.clone(),
            usage: response.usage.map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        // The type-erased raw path has no Rust type to deserialize into, but it
        // must still enforce the schema's required fields (summarization's
        // custom-schema path and the HTTP structured endpoints rely on this — e.g.
        // `summarize_one` needs the `summary` field present). Synthesise a
        // schema-aware validator so an omitted required field drives the same
        // corrective retry a typed caller gets, matching instructor.
        let validator = crate::schema::schema_required_validator(json_schema);
        self.structured_output_impl(messages, json_schema, options, Some(&validator))
            .await
    }

    async fn create_structured_output_with_messages_raw_validated(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: StructuredOutputValidator<'_>,
    ) -> LlmResult<Value> {
        self.structured_output_impl(messages, json_schema, options, Some(validator))
            .await
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Context lengths for common OpenAI models
        match self.model.as_str() {
            m if m.starts_with("gpt-4-turbo") => 128_000,
            m if m.starts_with("gpt-4-32k") => 32_768,
            m if m.starts_with("gpt-4") => 8_192,
            m if m.starts_with("gpt-3.5-turbo-16k") => 16_384,
            m if m.starts_with("gpt-3.5-turbo") => 4_096,
            _ => 4_096, // Conservative default
        }
    }

    /// The configured completion ceiling
    /// ([`default_max_tokens`](Self::default_max_tokens), lowered from
    /// `Settings.llm_max_completion_tokens`). `None` there means "send no default
    /// cap", which carries no size information for chunk sizing, so it falls back
    /// to the shared default.
    fn max_completion_tokens(&self) -> u32 {
        self.default_max_tokens
            .unwrap_or(Self::DEFAULT_MAX_COMPLETION_TOKENS)
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        use base64::Engine as _;

        if !mime_type.starts_with("image/") {
            return Err(LlmError::InvalidResponse(format!(
                "Expected image/* MIME type, got: {mime_type}"
            )));
        }

        let b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let data_uri = format!("data:{mime_type};base64,{b64}");

        let vision_model = std::env::var("LLM_VISION_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.model.clone());

        let max_tokens = options.as_ref().and_then(|o| o.max_tokens).unwrap_or(300);

        let mut request_body = json!({
            "model": vision_model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "What's in this image?" },
                    { "type": "image_url", "image_url": { "url": data_uri } }
                ]
            }],
        });
        self.write_max_tokens(&mut request_body, Some(max_tokens));

        // Deliberately use `send_chat_request` (not `call_api`): `LLM_ARGS`
        // (`extra_args`) are scoped to chat/structured extraction and must not
        // bleed into the image-description request.
        let response = self.send_chat_request(request_body).await?;

        let choice = response.choices.first().ok_or_else(|| {
            LlmError::InvalidResponse("No choices in vision response".to_string())
        })?;

        choice.message.content.clone().ok_or_else(|| {
            LlmError::InvalidResponse("Vision response contained no content".to_string())
        })
    }

    fn supports_vision(&self) -> bool {
        let m = self.model.to_lowercase();
        m.contains("gpt-4")
            || m.contains("gpt-5")
            || m.contains("vision")
            || m.contains("o1")
            || m.contains("o3")
            || m.contains("o4")
            || m.contains("llava")
            || m.contains("moondream")
            || m.contains("llama-3.2-vision")
            || m.contains("gemma3")
    }
}

impl OpenAIAdapter {
    /// Shared implementation backing both the plain and the validated
    /// structured-output trait methods.
    ///
    /// When `validator` is `Some`, a response that parses as JSON but fails it
    /// (e.g. the model returned a well-formed object that omits a required
    /// field) is treated as a *retryable miss* — exactly like a malformed or
    /// empty payload — and re-asked with a corrective instruction carrying the
    /// validation error. This threads the caller's typed validation into the
    /// existing repair loop (the mechanism instructor uses for parity) without
    /// introducing a second, multiplying retry loop: total attempts stay bounded
    /// by `structured_output_retries` because validation reuses the same loop.
    async fn structured_output_impl(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: Option<StructuredOutputValidator<'_>>,
    ) -> LlmResult<Value> {
        // Start of the aggregate budget. Every mode, re-ask and transport retry
        // below is measured against this one instant, because the thing that
        // needs bounding is the *logical* call: no individual HTTP request in a
        // 45-minute extraction was itself slow.
        let call_started = Instant::now();
        // Absolute form of the budget, threaded into every transport call below
        // so the retry ladder inside an attempt is bounded by it too, not just
        // the gaps between attempts.
        let call_deadline = self.request_deadline.map(|d| call_started + d);

        // Blank = empty or whitespace-only. Kept separate from JSON *validity*
        // so a non-empty-but-invalid payload can surface a clear error instead
        // of being lumped together with "no output" (which should retry / fall
        // back to a different mode).
        let is_blank = |raw: &str| raw.trim().is_empty();

        let parse_json =
            |raw: &str| -> Result<Value, serde_json::Error> { serde_json::from_str(raw) };

        // Returns `Some(reason)` when a parsed value fails the caller's typed
        // validation (missing required field, wrong type, …), `None` otherwise
        // (including when no validator was supplied).
        let validation_error =
            |parsed: &Value| -> Option<String> { validator.and_then(|v| v(parsed).err()) };

        // Structured extraction intentionally does NOT inherit the config
        // `default_max_tokens` (the global completion ceiling applied to
        // `generate`): it keeps the historical `GenerationOptions::default()`
        // cap (16384) for option-less calls. Applying the *configured* ceiling
        // here risks truncating the tool-call JSON mid-object, so a user
        // lowering the answer ceiling never silently breaks internal structured
        // calls (e.g. feedback detection). Callers that want NO cap at all pass
        // explicit `max_tokens: None` (as cognify's extraction paths do).
        // A budget the caller *chose*, as distinct from the one
        // `GenerationOptions::default()` supplies when no options were passed at
        // all. Only the former is a deliberate constraint that must not be raised
        // on truncation; the default's 16384 is a library value and carries no
        // caller intent.
        let caller_max_tokens = options.as_ref().and_then(|o| o.max_tokens);
        let opts = options.unwrap_or_default();
        // Align the advertised `required` array with what instructor sends on its
        // default TOOLS path (every non-default property is required). See
        // `recompute_top_level_required`. This is the shallow, Baseten-safe
        // rewrite — NOT the all-required/strict transform warned about below.
        let schema = Self::recompute_top_level_required(json_schema);

        // Primary path: OpenAI tool calling (`tools` + forced `tool_choice`).
        //
        // This mirrors Python cognee's request: instructor's default `Mode.TOOLS`
        // (used by `LLMGateway.acreate_structured_output`) sends the response
        // model as a single function tool and forces the model to call it,
        // passing the schema *as-is*.
        //
        // We deliberately do NOT use `response_format: {type: json_schema,
        // strict: true}` here, and we do NOT do the *heavy* strict rewrite
        // (recursively forcing every nested field required +
        // `additionalProperties:false`). Both drive grammar-constrained decoding
        // on OpenAI-compatible backends; Baseten's gpt-oss-120b returns HTTP 501
        // "Error making prediction" on such requests (verified: even the
        // recursive all-required rewrite *without* `strict:true` reproduces the
        // 501). We DO apply the shallow top-level `required` recompute
        // (`recompute_top_level_required`) — the exact shape instructor's default
        // TOOLS mode sends, verified accepted by Baseten — so the model reliably
        // returns every non-default field (e.g. `KnowledgeGraph.edges`). The
        // required-field guarantee is further backed by retrying on a
        // malformed/incomplete/validation-failing response with a corrective
        // instruction below.
        let mut tools_request = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "tools": [{
                "type": "function",
                "function": {
                    "name": "extract_structured_data",
                    "description": "Extract structured data from the input",
                    "parameters": schema.clone()
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "extract_structured_data"}
            }
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            tools_request["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut tools_request, opts.max_tokens);
        if self.should_disable_thinking() {
            tools_request["think"] = json!(false);
            tools_request["reasoning"] = json!({"effort": "none"});
        }

        // Retry loop. A parseable object that also satisfies the validator
        // returns immediately. A non-empty but invalid *or* validation-failing
        // payload retries with a corrective instruction carrying the failure
        // reason (instructor parity) and, once retries are exhausted, surfaces a
        // `DeserializationError` carrying the raw payload. An empty / missing
        // tool call retries and, once exhausted, falls through to the legacy
        // function-calling / JSON-mode paths below (so servers that do not
        // support tool calling still work).
        // Last outcome of the tool-calling loop, used to decide how to proceed
        // once retries are exhausted. We distinguish a *validation miss* (the
        // server clearly speaks tool calling and returns JSON, it merely omits a
        // required field) from a *parse failure* / empty output / API error,
        // because the two want different post-loop handling (see below).
        enum ToolOutcome {
            /// No usable output yet, or the request itself errored — fall through.
            NoUsableOutput,
            /// Valid JSON that failed the caller's typed/schema validation.
            ValidationMiss { reason: String, raw: String },
            /// A non-empty payload that did not parse as JSON.
            ParseFailure,
        }
        let mut outcome = ToolOutcome::NoUsableOutput;
        // A budget raised after a truncation, carried into the legacy and
        // JSON-mode requests below. Without this the later modes rebuild their
        // bodies from `opts` and drop straight back to the budget that already
        // truncated — which, with `LLM_MAX_RETRIES=1`, turns this fix back into
        // the three-mode cascade it exists to prevent.
        let mut raised_budget: Option<u32> = None;
        // Set whenever a truncation was detected and the budget raised. If every
        // mode still runs out of attempts afterwards, this is what the caller
        // hears about — the generic "retries exhausted" message would otherwise
        // bury the one fact that explains the failure and says how to fix it.
        let mut truncation_seen: Option<String> = None;
        // Most recent failure reason, threaded into the next corrective retry.
        let mut last_reason: Option<String> = None;
        // Skip tool-calling mode entirely on an endpoint where it has
        // repeatedly produced nothing usable. Expressed as a zero-length attempt
        // range rather than a wrapping conditional so the loop body, the
        // `ValidationMiss` check after it and the budget variables above all
        // keep their existing shape: with no attempts the outcome stays
        // `NoUsableOutput`, the validation check is a no-op, and control falls
        // straight through to legacy mode. See [`CascadeProbe`].
        let try_tools = self.cascade_probe.tools.should_try();
        if !try_tools {
            debug!(
                consecutive_misses = self.cascade_probe.tools.misses(),
                "tool-calling mode has produced nothing usable on this endpoint; skipping it",
            );
        }
        // Whether the endpoint answered with a *native* tool call (`tool_calls`
        // or a legacy `function_call`), as opposed to JSON echoed in `content`.
        // Gates the `ValidationMiss` short-circuit below, which claims the
        // server "clearly speaks tool calling" — a claim only this can support.
        let mut native_tool_call = false;
        // Whether at least one response *arrived* carrying no native payload —
        // no `tool_calls`/`function_call`, or one whose `arguments` was blank.
        // This, not the absence of `native_tool_call`, is the evidence that the
        // mode is useless here: a transport error, a deadline abort or a
        // truncation says nothing about the endpoint's parser, and counting them
        // would let a brief gateway blip disable tool calling on a healthy
        // endpoint for the next `RE_PROBE_INTERVAL` calls.
        let mut tools_lacked_native_payload = false;
        let tool_attempts = if try_tools {
            self.structured_output_retries
        } else {
            0
        };
        for attempt in 0..tool_attempts {
            // Aggregate budget check. Placed at the head of the attempt rather
            // than only between modes so a long retry ladder inside one mode
            // cannot run past the budget either.
            if let Some(e) = self.deadline_exceeded(call_started, "another tool-call attempt") {
                return Err(e);
            }
            let mut request_for_attempt = tools_request.clone();
            if attempt > 0 {
                Self::append_corrective_instruction(
                    &mut request_for_attempt,
                    last_reason.as_deref(),
                );
                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            match self
                .call_api_before(request_for_attempt, call_deadline)
                .await
            {
                Ok(tools_response) => {
                    let choice = tools_response.choices.first().ok_or_else(|| {
                        LlmError::InvalidResponse("No choices in tool-call response".to_string())
                    })?;

                    // Truncation is checked before the payload is inspected,
                    // because a cut-off answer arrives in one of two disguises and
                    // neither is self-describing: usually non-blank but
                    // unparseable (which would be recorded as `ParseFailure` and
                    // fall through the *whole* cascade, re-truncating identically
                    // in legacy and JSON mode and surfacing as a bare "EOF while
                    // parsing a string"), or entirely blank when the budget was
                    // consumed by reasoning tokens.
                    if Self::is_length_truncated(choice) {
                        let (reason, budget) = self.raise_budget_after_truncation(
                            &mut tools_request,
                            "Tool-call",
                            caller_max_tokens,
                        )?;
                        raised_budget = Some(budget);
                        truncation_seen = Some(reason.clone());
                        debug!(
                            attempt,
                            %reason,
                            "tool-call response truncated at the output budget; re-asking with a raised budget",
                        );
                        last_reason = Some(reason);
                        outcome = ToolOutcome::NoUsableOutput;
                        continue;
                    }

                    // Prefer a modern `tool_calls[0]`, then a legacy
                    // `function_call`, then raw `content` (some servers echo the
                    // JSON directly).
                    // An empty/whitespace `arguments` string must be treated as
                    // *absent* so the `.or(content)` fallback engages — some
                    // servers emit a `tool_calls[0]` with empty arguments but put
                    // the JSON in `message.content`. Without the `filter`, the
                    // `Some("")` would shadow the real payload.
                    let non_blank = |s: &str| !s.trim().is_empty();
                    // Split out from the `content` fallback below so the two can
                    // be told apart: only these two fields prove the endpoint
                    // has a tool-call parser. The precedence and blank-filtering
                    // are unchanged.
                    let native_arguments = choice
                        .message
                        .tool_calls
                        .as_ref()
                        .and_then(|calls| calls.first())
                        .map(|c| c.function.arguments.as_str())
                        .filter(|s| non_blank(s))
                        .or_else(|| {
                            choice
                                .message
                                .function_call
                                .as_ref()
                                .map(|f| f.arguments.as_str())
                                .filter(|s| non_blank(s))
                        });
                    if native_arguments.is_some() {
                        // Recorded here rather than after the loop because the
                        // success path returns from inside it.
                        if !native_tool_call {
                            native_tool_call = true;
                            self.cascade_probe.tools.record_useful();
                        }
                    } else {
                        // A response that arrived with no usable native payload:
                        // either the field was absent, or its `arguments` was
                        // blank and the `non_blank` filter above dropped it.
                        // Only counts once the whole mode is exhausted (below).
                        tools_lacked_native_payload = true;
                    }
                    let arguments = native_arguments
                        .or(choice.message.content.as_deref())
                        .unwrap_or("");

                    if is_blank(arguments) {
                        // No usable output this attempt: retry until exhausted,
                        // then fall through to the legacy paths.
                        outcome = ToolOutcome::NoUsableOutput;
                        last_reason = None;
                        continue;
                    }

                    match parse_json(arguments) {
                        Ok(parsed) => {
                            // Valid JSON — but does it satisfy the caller's type?
                            // A missing required field is caught here and fed
                            // into the next corrective retry (instructor parity),
                            // rather than surfacing as an un-retried failure.
                            if let Some(reason) = validation_error(&parsed) {
                                debug!(
                                    attempt,
                                    structured_output_retries = self.structured_output_retries,
                                    %reason,
                                    "tool-call response parsed but failed typed validation; \
                                     retrying with corrective instruction",
                                );
                                last_reason = Some(reason.clone());
                                outcome = ToolOutcome::ValidationMiss {
                                    reason,
                                    raw: arguments.to_string(),
                                };
                                continue;
                            }
                            // Mode 1 answered the call. Worth sending again even
                            // if the payload came from `content` rather than a
                            // native tool call: on a parser-less endpoint that
                            // echoes JSON, this is the *cheapest* path — one
                            // request — and skipping it would push the call into
                            // the fallbacks, where JSON mode sends only a
                            // `schema_to_example` template rather than the real
                            // schema.
                            self.cascade_probe.tools.record_useful();
                            return Ok(parsed);
                        }
                        Err(e) => {
                            // Non-empty but invalid JSON: retry, and remember that
                            // the failure was a *parse* failure so we fall through
                            // to the legacy/JSON-mode fallbacks once exhausted.
                            last_reason = Some(e.to_string());
                            outcome = ToolOutcome::ParseFailure;
                            continue;
                        }
                    }
                }
                // Terminal: `send_chat_request` has already exhausted its own
                // transport budget when it returns MaxRetriesExceeded, and that
                // budget now has a *time* floor (LLM_MIN_RETRY_SECONDS, 240s by
                // default). Falling through would restart it from attempt 0 in
                // the legacy loop, doubling the wall-clock cost of a persistently
                // failing endpoint — 8 minutes per structured extraction, times
                // every concurrent chunk in a cognify. A server that cannot
                // answer the transport layer will not answer a different request
                // *mode* either. Mirrors the Anthropic adapter's guard.
                Err(e @ LlmError::MaxRetriesExceeded(_)) => return Err(e),
                Err(e) => {
                    // The tool-calling request itself errored (tool calling
                    // unsupported, schema rejected, transient API/network error).
                    // Fall through to the legacy/JSON-mode fallbacks — a server
                    // may reject tool calling yet answer one of those, and those
                    // loops re-issue the request and surface any real API error
                    // via `?`. Crucially we do NOT return a stale validation/parse
                    // error here [#5]; we discard the prior miss and fall through.
                    warn!(error = %e, "tool-call request failed; falling back to legacy function/JSON mode");
                    outcome = ToolOutcome::NoUsableOutput;
                    break;
                }
            }
        }

        // Tool-calling mode ran out of attempts having produced nothing usable,
        // on responses that actually arrived. Gated on
        // `tools_lacked_native_payload` so only that evidence counts:
        // transport errors (the `Err` arm below breaks without setting it),
        // deadline aborts and truncations (which `continue` before the check) are
        // all excluded, because none of them says anything about whether the
        // endpoint can emit a tool call.
        //
        // One such call still proves little on its own — the model may simply
        // have answered badly — so this only accumulates suspicion, and
        // `MISS_THRESHOLD` consecutive ones are needed before mode 1 is skipped.
        if try_tools && !native_tool_call && tools_lacked_native_payload {
            let misses = self.cascade_probe.tools.record_useless();
            if misses == ModeProbe::MISS_THRESHOLD {
                warn!(
                    consecutive_misses = misses,
                    "endpoint has answered {misses} consecutive structured calls without a tool \
                     call; skipping tool-calling mode from now on (re-probed every {} calls). If \
                     this is unexpected, the server may be missing a tool-call parser — for vLLM, \
                     start it with --enable-auto-tool-choice --tool-call-parser",
                    ModeProbe::RE_PROBE_INTERVAL,
                );
            }
        }

        // Every tool-calling attempt returned valid JSON that failed the caller's
        // typed/schema validation (e.g. persistently omits a required field). The
        // server clearly speaks tool calling and returns well-formed JSON, so the
        // legacy/JSON-mode fallbacks would only re-ask the same model; surface the
        // validation error instead (instructor parity), naming the field. This is
        // deliberately NOT done for a *parse* failure or empty output [#4], which
        // fall through below in case a different request mode succeeds.
        //
        // Gated on `native_tool_call`, which is the only thing that can support
        // the "clearly speaks tool calling" claim above. Without the gate, a
        // parser-less endpoint echoing an incomplete object in `content` takes
        // this branch and errors — until the probe trips, after which the
        // identical request is answered by JSON mode. Three failures then a
        // success, for byte-identical input, is worse than either outcome
        // consistently.
        if let ToolOutcome::ValidationMiss { reason, raw } = outcome
            && native_tool_call
        {
            return Err(LlmError::DeserializationError(format!(
                "Tool-call arguments failed schema validation after {} attempt(s): {reason}. Raw: {raw}",
                self.structured_output_retries
            )));
        }

        // Try legacy function calling next (older OpenAI-compatible servers)
        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "functions": [{
                "name": "extract_structured_data",
                "description": "Extract structured data from the input",
                "parameters": schema.clone()
            }],
            "function_call": {"name": "extract_structured_data"}
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            request_body["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut request_body, opts.max_tokens);
        // A truncation in tool-calling mode already established that `opts` is too
        // small; inherit the raised budget rather than repeating the failure.
        self.write_max_tokens(&mut request_body, raised_budget);
        if self.should_disable_thinking() {
            request_body["think"] = json!(false);
            request_body["reasoning"] = json!({"effort": "none"});
        }

        // Reason carried into the next attempt's corrective instruction so a
        // legacy retry is not a byte-identical re-send (which just reproduces the
        // same bad output) — it appends the failure detail and drops temperature
        // to 0, exactly like the tool-calling and JSON-mode loops.
        let mut legacy_last_reason: Option<String> = None;
        // Legacy `functions` needs a server-side parser just as tool calling
        // does, so a parser-less endpoint burns this ladder too. Probed
        // separately from tool calling rather than sharing its counter, because
        // the cascade exists precisely because a server may accept one shape and
        // not the other — a shared counter would skip legacy mode on a server
        // that supports only legacy.
        let try_legacy = self.cascade_probe.legacy.should_try();
        if !try_legacy {
            debug!(
                consecutive_misses = self.cascade_probe.legacy.misses(),
                "legacy function-call mode has produced nothing usable on this endpoint; \
                 skipping it",
            );
        }
        // Same distinction as tool calling above: only a response that actually
        // arrived carrying no usable `function_call` is evidence against the
        // mode.
        let mut legacy_lacked_native_payload = false;
        let legacy_attempts = if try_legacy {
            self.structured_output_retries
        } else {
            0
        };
        for attempt in 0..legacy_attempts {
            // Aggregate budget check. Placed at the head of the attempt rather
            // than only between modes so a long retry ladder inside one mode
            // cannot run past the budget either.
            if let Some(e) =
                self.deadline_exceeded(call_started, "another legacy function-call attempt")
            {
                return Err(e);
            }
            let mut request_for_attempt = request_body.clone();
            if attempt > 0 {
                Self::append_corrective_instruction(
                    &mut request_for_attempt,
                    legacy_last_reason.as_deref(),
                );
                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            let response = self
                .call_api_before(request_for_attempt, call_deadline)
                .await?;

            let choice = response
                .choices
                .first()
                .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

            if Self::is_length_truncated(choice) {
                let (reason, budget) = self.raise_budget_after_truncation(
                    &mut request_body,
                    "Function-call",
                    caller_max_tokens,
                )?;
                raised_budget = Some(budget);
                truncation_seen = Some(reason.clone());
                debug!(attempt, %reason, "function-call response truncated at the output budget");
                legacy_last_reason = Some(reason);
                continue;
            }

            if let Some(function_call) = &choice.message.function_call {
                // A native payload arrived — keep sending this shape regardless
                // of whether *this* one parses.
                //
                // Gated on non-blank `arguments` so the two modes agree. The
                // tools branch drops a blank-`arguments` native call before its
                // own check (the `non_blank` filter), so that shape counts
                // against tool-calling mode; without this gate an endpoint
                // emitting `function_call: {arguments: ""}` forever would clear
                // legacy's count on every call and never trip, while the
                // identical tools-shaped server would.
                if is_blank(&function_call.arguments) {
                    legacy_lacked_native_payload = true;
                } else {
                    self.cascade_probe.legacy.record_useful();
                }
                let last_attempt = attempt + 1 >= self.structured_output_retries;
                match parse_json(&function_call.arguments) {
                    Ok(parsed) => {
                        if let Some(reason) = validation_error(&parsed) {
                            // Valid JSON but fails the caller's type: retry, and
                            // surface the validation error once exhausted.
                            if last_attempt {
                                return Err(LlmError::DeserializationError(format!(
                                    "Function call arguments failed schema validation: {reason}. \
                                     Raw: {}",
                                    function_call.arguments
                                )));
                            }
                            legacy_last_reason = Some(reason);
                            continue;
                        }
                        return Ok(parsed);
                    }
                    Err(e) => {
                        if is_blank(&function_call.arguments) {
                            // Empty output: retry until exhausted, then fall
                            // through to JSON mode.
                            if last_attempt {
                                break;
                            }
                            legacy_last_reason = None;
                            continue;
                        }
                        // Non-empty but invalid: surface it on the last attempt,
                        // otherwise retry.
                        if last_attempt {
                            return Err(LlmError::DeserializationError(format!(
                                "Failed to deserialize function call arguments: {}. Raw: {}",
                                e, function_call.arguments
                            )));
                        }
                        legacy_last_reason = Some(e.to_string());
                        continue;
                    }
                }
            } else {
                // A response arrived with no `function_call` at all: evidence the
                // endpoint cannot parse this shape either.
                legacy_lacked_native_payload = true;
            }

            break;
        }

        // Legacy mode ran out of attempts having produced nothing usable, on
        // responses that arrived. Note the legacy loop propagates transport
        // errors with `?` rather than breaking, so an errored request cannot
        // reach here at all — but the flag is still what gates this, so the rule
        // reads the same way as tool calling above: a response arrived, and it
        // carried no usable native payload.
        if try_legacy && legacy_lacked_native_payload {
            let misses = self.cascade_probe.legacy.record_useless();
            if misses == ModeProbe::MISS_THRESHOLD {
                debug!(
                    consecutive_misses = misses,
                    "endpoint has answered {misses} consecutive structured calls without a \
                     function call; skipping legacy function-call mode from now on",
                );
            }
        }

        // Fallback to JSON mode (works with Ollama and other providers)
        let mut json_messages = Self::convert_messages(&messages);

        let example = Self::schema_to_example(&schema);

        if let Some(last_msg) = json_messages.last_mut()
            && last_msg["role"] == "user"
        {
            let original_content = last_msg["content"].as_str().unwrap_or("");
            last_msg["content"] = json!(format!(
                "{}\n\n\
                    Extract the information from the text above and return it as JSON.\n\
                    Use this structure as your template (but with actual data from the text):\n\
                    {}",
                original_content, example
            ));
        }

        let mut json_request = json!({
            "model": self.model,
            "messages": json_messages,
            "response_format": {"type": "json_object"}
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            json_request["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut json_request, opts.max_tokens);
        self.write_max_tokens(&mut json_request, raised_budget);
        if self.should_disable_thinking() {
            json_request["think"] = json!(false);
            json_request["reasoning"] = json!({"effort": "none"});
        }

        for attempt in 0..self.structured_output_retries {
            // Aggregate budget check. Placed at the head of the attempt rather
            // than only between modes so a long retry ladder inside one mode
            // cannot run past the budget either.
            if let Some(e) = self.deadline_exceeded(call_started, "another JSON-mode attempt") {
                return Err(e);
            }
            let mut request_for_attempt = json_request.clone();

            if attempt > 0 {
                if let Some(messages) = request_for_attempt["messages"].as_array_mut()
                    && let Some(last_msg) = messages.last_mut()
                    && last_msg["role"] == "user"
                {
                    let original_content = last_msg["content"].as_str().unwrap_or("");
                    last_msg["content"] = json!(format!(
                        "{}\n\n/no_think\nReturn ONLY one valid JSON object matching the required schema. No reasoning, no markdown, no extra text.",
                        original_content
                    ));
                }

                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            let json_response = self
                .call_api_before(request_for_attempt, call_deadline)
                .await?;

            let json_choice = json_response.choices.first().ok_or_else(|| {
                LlmError::InvalidResponse("No choices in JSON mode response".to_string())
            })?;

            // Checked before `content` is unwrapped: a truncation that spent the
            // whole budget on reasoning tokens leaves content absent, which would
            // otherwise surface as the misleading "No content in JSON mode
            // response".
            if Self::is_length_truncated(json_choice) {
                let (reason, budget) = self.raise_budget_after_truncation(
                    &mut json_request,
                    "JSON-mode",
                    caller_max_tokens,
                )?;
                let _ = budget; // JSON mode is last; nothing downstream inherits it.
                truncation_seen = Some(reason.clone());
                debug!(attempt, %reason, "JSON-mode response truncated at the output budget");
                continue;
            }

            let content = json_choice.message.content.as_ref().ok_or_else(|| {
                LlmError::InvalidResponse("No content in JSON mode response".to_string())
            })?;

            let last_attempt = attempt + 1 >= self.structured_output_retries;
            match parse_json(content) {
                Ok(parsed) => {
                    if let Some(reason) = validation_error(&parsed) {
                        // Valid JSON but fails the caller's type: retry, and
                        // surface the validation error once exhausted.
                        if last_attempt {
                            return Err(LlmError::DeserializationError(format!(
                                "JSON content failed schema validation: {reason}. Raw: {content}"
                            )));
                        }
                        continue;
                    }
                    return Ok(parsed);
                }
                Err(e) => {
                    // Retry on *any* parse failure while attempts remain — an
                    // empty response OR a non-empty-but-invalid one (e.g. JSON
                    // wrapped in prose/markdown). The retry above appends a
                    // "return ONLY one valid JSON object" instruction and drops
                    // temperature to 0, so a re-ask can recover; narrowing this to
                    // blank-only [#8] would give up on a malformed-but-present
                    // payload after a single attempt.
                    if !last_attempt {
                        continue;
                    }
                    return Err(LlmError::DeserializationError(format!(
                        "Failed to deserialize JSON content: {e}. Raw: {content}"
                    )));
                }
            }
        }

        if let Some(reason) = truncation_seen {
            return Err(LlmError::InvalidResponse(format!(
                "Structured output retries exhausted after a truncated response: {reason}. The \
                 answer does not fit the available output budget"
            )));
        }
        Err(LlmError::InvalidResponse(
            "Structured output retries exhausted without a parseable response".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Whisper transcription support
// ---------------------------------------------------------------------------

/// Response from the OpenAI Whisper `verbose_json` endpoint.
#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
    language: Option<String>,
    duration: Option<f32>,
}

/// Map a validated audio format extension to its MIME type.
fn audio_mime_type(format: &str) -> &'static str {
    match format {
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        // validate_audio_format ensures only the above values reach here
        _ => "application/octet-stream",
    }
}

impl OpenAIAdapter {
    /// Call the Whisper transcription API with the same retry logic as `call_api`.
    #[instrument(
        name = "llm.transcription_api_call",
        level = "info",
        skip(self, form),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.transcription_model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn call_transcription_api(
        &self,
        form: reqwest::multipart::Form,
    ) -> LlmResult<WhisperResponse> {
        let url = self.endpoint_url("audio/transcriptions");
        tracing::Span::current().record("url", url.as_str());

        // We cannot clone a multipart Form, so the first attempt uses the
        // original form and retries are not possible for the multipart body.
        // However, we keep the retry loop for network errors that occur
        // *before* the body is consumed (connection refused, DNS failure).
        // For simplicity and matching the guide's design, we rebuild the form
        // if needed by storing the bytes. But since `Form` doesn't support
        // Clone, we perform a single attempt with the form and rely on the
        // caller to retry externally if needed.
        //
        // Actually, the simplest approach is to send the form once and
        // handle retries at a higher level. But the guide says to mirror
        // call_api's retry. Since reqwest::multipart::Form is not Clone,
        // we accept `form` by value and do a single-shot request here,
        // while the `transcribe_audio` impl handles retry by rebuilding
        // the form on each attempt.

        let response = self
            .apply_auth(self.client.post(&url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(match status.as_u16() {
                401 => LlmError::AuthenticationError(error_body),
                402 => LlmError::PaymentRequired(error_body),
                429 => LlmError::RateLimitExceeded(error_body),
                400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
            });
        }

        let response_body = response.text().await.map_err(|e| {
            LlmError::DeserializationError(format!("Failed to read response body: {e}"))
        })?;

        serde_json::from_str::<WhisperResponse>(&response_body).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to parse Whisper response: {e}. Raw body: {response_body}"
            ))
        })
    }

    /// Build a `reqwest::multipart::Form` for a Whisper transcription request.
    fn build_transcription_form(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<reqwest::multipart::Form> {
        let mime = audio_mime_type(format);
        let filename = format!("audio.{format}");

        let file_part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| {
                LlmError::ConfigError(format!("Failed to set MIME type on multipart part: {e}"))
            })?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.transcription_model.clone())
            .text("response_format", "verbose_json");

        if let Some(lang) = language_hint {
            form = form.text("language", lang.to_string());
        }
        if let Some(prompt) = prompt_hint {
            form = form.text("prompt", prompt.to_string());
        }

        Ok(form)
    }
}

#[async_trait]
impl Transcriber for OpenAIAdapter {
    async fn transcribe_audio(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<TranscriptionOutput> {
        // Normalize and validate before any network I/O.
        let format_lower = format.to_ascii_lowercase();
        validate_audio_format(&format_lower)?;

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "Transcription API attempt");
            if attempt > 0 {
                let delay = crate::retry::retry_backoff(attempt as u32);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %last_error,
                    "Transcription request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            let form =
                self.build_transcription_form(audio, &format_lower, language_hint, prompt_hint)?;

            match self.call_transcription_api(form).await {
                Ok(resp) => {
                    return Ok(TranscriptionOutput {
                        text: resp.text,
                        language: resp.language,
                        duration: resp.duration,
                    });
                }
                Err(e) => {
                    // Non-retryable errors: bad request or authentication failure.
                    if matches!(
                        e,
                        LlmError::InvalidResponse(_) | LlmError::AuthenticationError(_)
                    ) {
                        return Err(e);
                    }
                    last_error = e;
                    continue;
                }
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Transcription request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    fn transcription_model(&self) -> &str {
        &self.transcription_model
    }
}

// OpenAI API response types
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: u32,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    /// Modern tool-calling response (`tool_choice`/`tools`); the structured
    /// output is the first call's `function.arguments` JSON string.
    tool_calls: Option<Vec<OpenAIToolCall>>,
    /// Legacy `function_call` response (older OpenAI-compatible servers).
    function_call: Option<OpenAIFunctionCall>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct OpenAIToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    call_type: Option<String>,
    /// Defaulted so a `tool_calls` entry missing its `function` object (seen on
    /// some OpenAI-compatible servers) does not fail deserialization of the whole
    /// response — the fallback chain then engages instead of erroring out.
    #[serde(default)]
    function: OpenAIFunctionCall,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct OpenAIFunctionCall {
    #[serde(default)]
    name: String,
    /// Defaulted to `""` so a `function` object without `arguments` deserializes
    /// (treated as empty → drives a retry / fallback) rather than erroring the
    /// entire `ApiResponse`.
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;

    #[test]
    fn mode_probe_tries_the_mode_until_the_miss_threshold() {
        let probe = super::ModeProbe::new();

        // Below the threshold every call still attempts the mode: a single bad
        // response must not disable it.
        for _ in 0..super::ModeProbe::MISS_THRESHOLD - 1 {
            probe.record_useless();
            assert!(
                probe.should_try(),
                "must keep probing below MISS_THRESHOLD consecutive misses"
            );
        }

        probe.record_useless();
        assert!(
            !probe.should_try(),
            "must stop sending the mode once MISS_THRESHOLD is reached"
        );
    }

    #[test]
    fn mode_probe_resets_when_the_mode_produces_something() {
        let probe = super::ModeProbe::new();
        for _ in 0..super::ModeProbe::MISS_THRESHOLD {
            probe.record_useless();
        }
        assert!(!probe.should_try(), "tripped");

        // One useful answer clears the suspicion outright.
        probe.record_useful();
        assert!(
            probe.should_try(),
            "a mode that produced output must be tried again"
        );
        assert_eq!(probe.misses(), 0, "the observed count is reset too");
    }

    #[test]
    fn mode_probe_re_probes_after_the_interval() {
        let probe = super::ModeProbe::new();
        for _ in 0..super::ModeProbe::MISS_THRESHOLD {
            probe.record_useless();
        }

        // Skips for the whole interval...
        for i in 0..super::ModeProbe::RE_PROBE_INTERVAL {
            assert!(!probe.should_try(), "call {i} should still skip");
        }
        // ...then lets exactly one call through, so an endpoint that gains a
        // parser recovers without restarting the process.
        assert!(
            probe.should_try(),
            "must re-probe once RE_PROBE_INTERVAL calls have been skipped"
        );
        assert!(
            !probe.should_try(),
            "the re-probe is one call, not a permanent reset"
        );
    }

    #[test]
    fn mode_probe_reports_the_observed_miss_count() {
        // The skip log names what was seen rather than the constant, so an
        // operator can tell three real misses from a mode that was skipped for
        // another reason.
        let probe = super::ModeProbe::new();
        assert_eq!(probe.record_useless(), 1);
        assert_eq!(probe.record_useless(), 2);
        assert_eq!(probe.misses(), 2);
    }

    #[test]
    fn cascade_probe_tracks_tools_and_legacy_independently() {
        // A server that accepts legacy `functions` but not modern `tools` is
        // exactly why the cascade exists; one shared counter would skip the mode
        // that works.
        let probe = super::CascadeProbe::new();
        for _ in 0..super::ModeProbe::MISS_THRESHOLD {
            probe.tools.record_useless();
        }
        assert!(!probe.tools.should_try(), "tool calling is tripped");
        assert!(
            probe.legacy.should_try(),
            "legacy mode must be unaffected by tool-calling misses"
        );
    }

    #[test]
    fn test_model_is_used_verbatim() {
        // The adapter no longer strips provider prefixes — that is owned by
        // `build_openai_compatible_adapter`. The model must reach the wire
        // exactly as constructed so real slugs containing a slash (e.g.
        // Baseten's `openai/gpt-oss-120b`) are preserved.
        let adapter = OpenAIAdapter::new("openai/gpt-oss-120b", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "openai/gpt-oss-120b");
        let adapter = OpenAIAdapter::new("gpt-5-mini", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "gpt-5-mini");
    }

    #[test]
    fn test_tool_call_missing_arguments_deserializes_to_empty() {
        // #7: a `tool_calls` entry whose `function` lacks `arguments` must not
        // fail deserialization of the whole response — it defaults to "" so the
        // fallback chain engages.
        let raw = r#"{
            "id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                {"id":"c1","type":"function","function":{"name":"extract_structured_data"}}
            ]},"finish_reason":"tool_calls"}]
        }"#;
        let resp: OpenAIResponse =
            serde_json::from_str(raw).expect("missing arguments should default, not error");
        let call = &resp.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.arguments, "");
    }

    #[test]
    fn test_tool_call_missing_function_deserializes() {
        // #7: a `tool_calls` entry with no `function` object at all must also
        // deserialize (defaulted) rather than erroring the whole `ApiResponse`.
        let raw = r#"{
            "id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                {"id":"c1","type":"function"}
            ]},"finish_reason":"tool_calls"}]
        }"#;
        let resp: OpenAIResponse =
            serde_json::from_str(raw).expect("missing function should default, not error");
        let call = &resp.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.name, "");
        assert_eq!(call.function.arguments, "");
    }

    #[test]
    fn test_openai_adapter_creation() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None);
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.model(), "gpt-4");
        assert_eq!(adapter.base_url, OpenAIAdapter::DEFAULT_BASE_URL);
        assert_eq!(
            adapter.structured_output_retries,
            OpenAIAdapter::DEFAULT_STRUCTURED_OUTPUT_RETRIES
        );
    }

    #[test]
    fn test_configurable_structured_output_retries() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(5);
        assert_eq!(adapter.structured_output_retries, 5);

        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(0);
        assert_eq!(adapter.structured_output_retries, 1);
    }

    #[test]
    fn test_openai_adapter_custom_base_url() {
        let adapter = OpenAIAdapter::new(
            "gpt-4",
            "test-key",
            Some("https://custom.api.com/v1".to_string()),
        );
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.base_url, "https://custom.api.com/v1");
    }

    #[test]
    fn test_base_url_trailing_slash_is_normalized() {
        // The Gemini OpenAI-compat base ends in `/`; without normalisation the
        // request URL would be `.../openai//chat/completions` and 404.
        let adapter = OpenAIAdapter::new(
            "gemini-2.0-flash",
            "test-key",
            Some("https://generativelanguage.googleapis.com/v1beta/openai/".to_string()),
        )
        .unwrap();
        assert_eq!(
            adapter.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }

    #[test]
    fn openai_mode_has_no_api_version_and_bearer_url() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "sk-test", None).unwrap();
        assert!(adapter.api_version.is_none());
        // No api-version query on the standard OpenAI path.
        assert_eq!(
            adapter.endpoint_url("chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn with_api_version_enables_azure_mode_and_query() {
        let adapter = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert_eq!(adapter.api_version.as_deref(), Some("2024-12-01-preview"));
        assert_eq!(
            adapter.endpoint_url("chat/completions"),
            "https://res.openai.azure.com/openai/deployments/gpt-4o-mini/chat/completions?api-version=2024-12-01-preview"
        );
    }

    #[test]
    fn endpoint_url_hardens_against_double_query_and_encodes_api_version() {
        // base_url already carrying a query must not yield a malformed double-`?`;
        // the api-version is appended with `&`.
        let with_query = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini?foo=bar".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        let url = with_query.endpoint_url("chat/completions");
        assert_eq!(url.matches('?').count(), 1, "exactly one '?': {url}");
        assert!(url.contains("foo=bar"), "existing query preserved: {url}");
        assert!(
            url.contains("api-version=2024-12-01-preview"),
            "api-version appended: {url}"
        );
        // The route must stay in the PATH, not slide into the query value: a
        // raw `{base}?foo=bar` + `/chat/completions` concat would leave the path
        // at `.../gpt-4o-mini` and 404 every request.
        let parsed = reqwest::Url::parse(&url).expect("endpoint_url produced a valid URL");
        assert!(
            parsed
                .path()
                .ends_with("/openai/deployments/gpt-4o-mini/chat/completions"),
            "route stays in the path when base_url carries a query: {}",
            parsed.path()
        );

        // A base_url that already carries `api-version=...` (e.g. a copied Azure
        // portal Target URI) must NOT yield a duplicate: the configured version
        // wins and appears exactly once.
        let dup = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some(
                "https://res.openai.azure.com/openai/deployments/gpt-4o-mini?api-version=2023-05-15"
                    .to_string(),
            ),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        let url = dup.endpoint_url("chat/completions");
        let parsed = reqwest::Url::parse(&url).expect("valid URL");
        let versions: Vec<_> = parsed
            .query_pairs()
            .filter(|(k, _)| k == "api-version")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(
            versions,
            vec!["2024-12-01-preview".to_string()],
            "exactly one api-version, configured value wins: {url}"
        );

        // A value with a reserved character is percent-encoded, not interpolated raw.
        let odd = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini".to_string()),
        )
        .unwrap()
        .with_api_version("2024 preview&x=1");
        let url = odd.endpoint_url("chat/completions");
        assert!(
            !url.contains("api-version=2024 preview&x=1"),
            "raw value must not appear unencoded: {url}"
        );
        assert!(url.contains("api-version="), "api-version present: {url}");
    }

    #[test]
    fn with_api_version_empty_stays_openai_mode() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "sk-test", None)
            .unwrap()
            .with_api_version("   ");
        assert!(adapter.api_version.is_none());
    }

    #[test]
    fn test_is_reasoning_model_matches_openai_families() {
        let cases = [
            ("gpt-5", true),
            ("gpt-5-mini", true),
            ("gpt-5-2025-06-01", true),
            ("o1", true),
            ("o1-mini", true),
            ("o3", true),
            ("o3-mini", true),
            ("o4-mini", true),
            ("GPT-5-Mini", true),
            ("gpt-4o-mini", false),
            ("gpt-4-turbo", false),
            ("gpt-3.5-turbo", false),
            ("o-foo", false),
        ];
        for (model, expected) in cases {
            let adapter = OpenAIAdapter::new(model, "test-key", None).unwrap();
            assert_eq!(
                adapter.is_reasoning_model(),
                expected,
                "is_reasoning_model({model})"
            );
        }
    }

    #[test]
    fn test_is_reasoning_model_skipped_for_custom_base_url() {
        // Custom OpenAI-compatible endpoints (Ollama, vLLM, …) may have
        // model names that look like reasoning families but still accept
        // legacy sampling parameters — the gate is conservative.
        let adapter = OpenAIAdapter::new(
            "gpt-5-mini",
            "test-key",
            Some("http://localhost:11434/v1".to_string()),
        )
        .unwrap();
        assert!(!adapter.is_reasoning_model());
    }

    #[test]
    fn is_reasoning_model_detected_on_azure_and_remote_gateways() {
        // The bug this fixes: a host gate on api.openai.com left Azure o-series /
        // gpt-5 deployments sending max_tokens + temperature, which Azure 400s on
        // every call. Detection is now name-based, so the Azure deployment matches.
        let azure = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("https://my-resource.openai.azure.com/openai/deployments/o3".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert!(azure.is_reasoning_model());

        // A remote (non-local) OpenAI-compatible gateway serving a reasoning
        // model is detected too, since detection is host-agnostic.
        let gateway = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/v1".to_string()),
        )
        .unwrap();
        assert!(gateway.is_reasoning_model());

        // Regression: the old substring scan misclassified a genuinely remote
        // host as local when the URL merely contained a loopback token. A
        // `localhost` subdomain label must NOT suppress detection.
        let subdomain = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("https://o3.localhost.example.com/v1".to_string()),
        )
        .unwrap();
        assert!(subdomain.is_reasoning_model());
        // `127.0.0.1` appearing only in a path must NOT suppress detection.
        let path_ip = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/proxy/127.0.0.1/v1".to_string()),
        )
        .unwrap();
        assert!(path_ip.is_reasoning_model());
        // An actual loopback host is still suppressed (local runtimes reject the shape).
        let loopback = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("http://127.0.0.1:8080/v1".to_string()),
        )
        .unwrap();
        assert!(!loopback.is_reasoning_model());

        // A genuinely remote endpoint that merely listens on Ollama's default
        // port 11434 is NOT local: a real reasoning model there still gets the
        // reasoning shape. (The port shortcut is gated on a private host.)
        let remote_11434 = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com:11434/v1".to_string()),
        )
        .unwrap();
        assert!(remote_11434.is_reasoning_model());

        // Ollama on a private-network host (RFC-1918 + port 11434) is local, so
        // a name-colliding model is suppressed.
        let lan_ollama = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("http://192.168.1.5:11434/v1".to_string()),
        )
        .unwrap();
        assert!(!lan_ollama.is_reasoning_model());

        // A private-network host on a non-Ollama port is treated as remote (often
        // a proxy to real OpenAI), so the reasoning shape applies.
        let lan_gateway = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("http://192.168.1.5:8000/v1".to_string()),
        )
        .unwrap();
        assert!(lan_gateway.is_reasoning_model());
    }

    #[test]
    fn is_reasoning_model_detected_from_azure_deployment_segment() {
        // Azure ignores the request-body model for routing (the deployment is in
        // the URL) and the docs tell operators the value is inert, so a reasoning
        // deployment is reachable with a non-reasoning LLM_MODEL. The deployment
        // segment must still trigger detection, else Azure 400s on max_tokens +
        // temperature for every call.
        let by_deployment = OpenAIAdapter::new(
            "gpt-4o-mini", // a non-reasoning placeholder, as .env.example ships
            "sk-test",
            Some("https://my-resource.openai.azure.com/openai/deployments/o3-prod".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert!(by_deployment.is_reasoning_model());

        // A non-reasoning deployment name with a non-reasoning model stays legacy.
        let non_reasoning = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://my-resource.openai.azure.com/openai/deployments/chat-prod".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert!(!non_reasoning.is_reasoning_model());

        // The deployment fallback is gated on the Azure host: a NON-Azure gateway
        // whose path merely contains a `deployments/<reasoning-name>` route
        // segment must NOT be misclassified (it may only accept legacy params).
        let non_azure_deployments_path = OpenAIAdapter::new(
            "my-alias", // non-reasoning model name
            "sk-test",
            Some("https://gw.example.com/deployments/o3-router/v1".to_string()),
        )
        .unwrap();
        assert!(!non_azure_deployments_path.is_reasoning_model());
    }

    #[test]
    fn with_reasoning_override_forces_detection_either_way() {
        // `never` un-fires a name/host match (remote gateway serving a
        // reasoning-named model that only accepts legacy parameters).
        let forced_off = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/v1".to_string()),
        )
        .unwrap()
        .with_reasoning_override(Some(false));
        assert!(!forced_off.is_reasoning_model());

        // `always` fires even for an opaque non-reasoning-looking alias.
        let forced_on = OpenAIAdapter::new(
            "my-alias",
            "sk-test",
            Some("https://gateway.example.com/v1".to_string()),
        )
        .unwrap()
        .with_reasoning_override(Some(true));
        assert!(forced_on.is_reasoning_model());

        // `None` (auto) leaves auto-detection untouched.
        let auto = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/v1".to_string()),
        )
        .unwrap()
        .with_reasoning_override(None);
        assert!(auto.is_reasoning_model());
    }

    #[test]
    fn test_write_max_tokens_renames_key_for_reasoning_models() {
        let mut body = json!({"model": "gpt-5-mini"});
        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None).unwrap();
        reasoning.write_max_tokens(&mut body, Some(2048));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 2048);

        let mut body = json!({"model": "gpt-4o-mini"});
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None).unwrap();
        classic.write_max_tokens(&mut body, Some(2048));
        assert_eq!(body["max_tokens"], 2048);
        assert!(body.get("max_completion_tokens").is_none());

        // None leaves body untouched.
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.write_max_tokens(&mut body, None);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }

    /// TOP-8: cognify's chunk-size auto-calculation reads the adapter's ceiling,
    /// so it must reflect the configured `LLM_MAX_COMPLETION_TOKENS`.
    #[test]
    fn max_completion_tokens_reports_the_configured_ceiling() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "k", None)
            .expect("adapter builds")
            .with_default_max_tokens(Some(4096));
        assert_eq!(adapter.max_completion_tokens(), 4096);
    }

    #[test]
    fn max_completion_tokens_falls_back_when_no_cap_is_configured() {
        // `None` ("send no default cap") carries no size information, so chunk
        // sizing uses the shared default rather than treating it as zero.
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "k", None)
            .expect("adapter builds")
            .with_default_max_tokens(None);
        assert_eq!(
            adapter.max_completion_tokens(),
            OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS
        );
    }

    #[test]
    fn default_max_tokens_governs_option_less_generate_in_azure_mode() {
        // The behaviour the Azure factory relies on (`.with_default_max_tokens`):
        // an option-less generate() must send the configured ceiling, not the
        // hardcoded 16384. Regression guard for the Azure factory previously
        // omitting the setter, which made Azure ignore LLM_MAX_COMPLETION_TOKENS.
        let azure = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview")
        .with_default_max_tokens(Some(4096));
        assert_eq!(azure.resolve_options(None).max_tokens, Some(4096));

        // Explicit caller options still win over the configured default.
        let explicit = GenerationOptions {
            max_tokens: Some(256),
            ..GenerationOptions::default()
        };
        assert_eq!(azure.resolve_options(Some(explicit)).max_tokens, Some(256));
    }

    #[test]
    fn test_apply_extra_args_fills_missing_keys_only() {
        // Mirrors Python's `{**self.llm_args, **kwargs}`: llm_args fill gaps,
        // explicitly-set request params win.
        let args = json!({"max_tokens": 16384, "top_p": 0.9})
            .as_object()
            .unwrap()
            .clone();
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args);

        // `max_tokens` absent → filled from extra_args; existing `temperature`
        // untouched (not in extra_args); `top_p` filled.
        let mut body = json!({"model": "gpt-4o-mini", "temperature": 0.0});
        adapter.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 16384);
        assert_eq!(body["top_p"], 0.9);
        assert_eq!(body["temperature"], 0.0);

        // An explicitly-set key is NOT overwritten by extra_args.
        let mut body = json!({"model": "gpt-4o-mini", "max_tokens": 512});
        adapter.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn test_apply_extra_args_translates_max_tokens_for_reasoning_models() {
        // #1: `write_max_tokens` emits `max_completion_tokens` for a reasoning
        // model; a bare `LLM_ARGS` `max_tokens` must be folded into
        // `max_completion_tokens` (never sent alongside it), or OpenAI 400s on
        // both keys.
        let args = json!({"max_tokens": 16384}).as_object().unwrap().clone();
        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args.clone());

        // Body already carries `max_completion_tokens` (from write_max_tokens):
        // the extra `max_tokens` must NOT be added, and no bare `max_tokens` key.
        let mut body = json!({"model": "gpt-5-mini", "max_completion_tokens": 2048});
        reasoning.apply_extra_args(&mut body);
        assert!(
            body.get("max_tokens").is_none(),
            "reasoning model must never carry a bare max_tokens"
        );
        assert_eq!(
            body["max_completion_tokens"], 2048,
            "explicit max_completion_tokens must win over LLM_ARGS"
        );

        // Body has no output cap yet: the LLM_ARGS max_tokens fills
        // max_completion_tokens (translated), still no bare max_tokens.
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.apply_extra_args(&mut body);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 16384);

        // A classic (non-reasoning) model keeps the bare max_tokens.
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args);
        let mut body = json!({"model": "gpt-4o-mini"});
        classic.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 16384);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_apply_extra_args_drops_suppressed_sampling_params_for_reasoning() {
        // Reasoning models reject temperature/top_p/frequency_penalty/
        // presence_penalty, and the request builder omits them. An LLM_ARGS value
        // for any of these must NOT be re-added by apply_extra_args, else the call
        // 400s ("Unsupported value: temperature").
        let args = json!({
            "temperature": 0.3,
            "top_p": 0.9,
            "frequency_penalty": 0.1,
            "presence_penalty": 0.2,
            "logit_bias": {"50256": -100}
        })
        .as_object()
        .unwrap()
        .clone();

        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args.clone());
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.apply_extra_args(&mut body);
        for suppressed in [
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
        ] {
            assert!(
                body.get(suppressed).is_none(),
                "reasoning model must not carry {suppressed}"
            );
        }
        // Non-sampling extra args (e.g. logit_bias) are still applied.
        assert_eq!(body["logit_bias"]["50256"], -100);

        // A classic model keeps all of them (they are valid there).
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args);
        let mut body = json!({"model": "gpt-4o-mini"});
        classic.apply_extra_args(&mut body);
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["top_p"], 0.9);
        assert_eq!(body["frequency_penalty"], 0.1);
        assert_eq!(body["presence_penalty"], 0.2);
    }

    #[test]
    fn test_apply_extra_args_empty_is_noop() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", None).unwrap();
        let mut body = json!({"model": "gpt-4o-mini"});
        let before = body.clone();
        adapter.apply_extra_args(&mut body);
        assert_eq!(body, before);
    }

    #[test]
    fn test_message_conversion() {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are helpful".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello".to_string(),
            },
        ];

        let converted = OpenAIAdapter::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "system");
        assert_eq!(converted[0]["content"], "You are helpful");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(converted[1]["content"], "Hello");
    }

    #[test]
    fn test_context_length() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo-preview", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 128_000);

        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 8_192);

        let adapter = OpenAIAdapter::new("gpt-3.5-turbo-16k", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 16_384);
    }

    #[test]
    fn test_supports_vision_gpt4o() {
        let adapter = OpenAIAdapter::new("gpt-4o", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4_turbo() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4o_mini() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt35_is_false() {
        let adapter = OpenAIAdapter::new("gpt-3.5-turbo", "key", None).unwrap();
        assert!(!adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_llava() {
        let adapter = OpenAIAdapter::new("llava:13b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_o1() {
        let adapter = OpenAIAdapter::new("o1-preview", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gemma3() {
        let adapter = OpenAIAdapter::new("gemma3:12b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[tokio::test]
    async fn transcribe_image_rejects_non_image_mime() {
        let adapter = OpenAIAdapter::new("gpt-4o", "fake-key", None).unwrap();
        let result = adapter
            .transcribe_image(b"not-an-image", "text/plain", None)
            .await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), LlmError::InvalidResponse(_)),
            "Expected InvalidResponse for non-image MIME type"
        );
    }

    #[test]
    fn test_transcription_model_default() {
        // Clear the env var to test the default value.
        // SAFETY: This test is single-threaded and no other thread reads
        // TRANSCRIPTION_MODEL concurrently.
        unsafe { std::env::remove_var("TRANSCRIPTION_MODEL") };
        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.transcription_model(), "whisper-1");
    }

    #[test]
    fn test_transcription_model_custom() {
        let adapter = OpenAIAdapter::new("gpt-4", "key", None)
            .unwrap()
            .with_transcription_model("whisper-large-v3");
        assert_eq!(adapter.transcription_model(), "whisper-large-v3");
    }

    #[test]
    fn test_audio_mime_type_mapping() {
        assert_eq!(audio_mime_type("mp3"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpeg"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpga"), "audio/mpeg");
        assert_eq!(audio_mime_type("mp4"), "audio/mp4");
        assert_eq!(audio_mime_type("m4a"), "audio/mp4");
        assert_eq!(audio_mime_type("wav"), "audio/wav");
        assert_eq!(audio_mime_type("webm"), "audio/webm");
    }
}
