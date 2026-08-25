//! [`BackendBuildContext`] — the resolved, env-free input to every factory.
//!
//! The construction contract: **all config-specific resolution (assembling
//! Postgres URLs from parts) and all environment-variable reads happen when a
//! caller lowers its config into a `BackendBuildContext`** (see
//! `Settings::backend_context` / `HttpServerConfig::backend_context`). The
//! registry and its factories are pure — given the same context they build the
//! same components, regardless of the process environment. This keeps
//! provider-specific URL assembly where the field differences live and lets
//! each caller opt into mock / recording behavior explicitly.

use std::fmt;
use std::path::PathBuf;

/// Resolved inputs consumed by [`crate::ComponentRegistry`] and the free
/// `build_storage` / `build_database` constructors.
#[derive(Clone)]
pub struct BackendBuildContext {
    // ── storage / relational database ─────────────────────────────────────
    /// Root directory for ingested data files (LocalStorage).
    pub data_root_directory: PathBuf,
    /// Root for system state; graph / vector backends derive default paths from
    /// it when their explicit path is unset.
    pub system_root_directory: PathBuf,
    /// Fully-resolved relational DB URL (sqlite:… or postgres://…).
    pub relational_db_url: String,

    // ── graph ─────────────────────────────────────────────────────────────
    /// Lowercase graph provider id (`ladybug` | `kuzu` | `postgres`).
    pub graph_provider: String,
    /// Explicit ladybug/kuzu graph file path. Empty → the factory derives
    /// `{system_root_directory}/graph`.
    pub graph_file_path: String,
    /// Resolution outcome for the Postgres graph backend: `None` when the
    /// provider is not Postgres, `Some(Ok(url))` on success, `Some(Err(msg))`
    /// when the provider *is* Postgres but URL resolution failed — carrying the
    /// specific cause (e.g. missing credentials) so the factory can restate it
    /// in the returned error rather than only logging it.
    pub graph_postgres_url: Option<Result<String, String>>,

    // ── vector ────────────────────────────────────────────────────────────
    /// Lowercase vector provider id (`pgvector` | `lancedb` | `brute-force` |
    /// `mock` | …).
    pub vector_provider: String,
    /// Raw vector DB URL/path as configured. Consulted for the `:memory:`
    /// escape hatch and for the LanceDB on-disk path.
    pub vector_db_url: String,
    /// Resolution outcome for the pgvector backend: `None` when the provider is
    /// not pgvector, `Some(Ok(url))` on success, `Some(Err(msg))` when the
    /// provider *is* pgvector but URL resolution failed (carries the cause).
    pub vector_postgres_url: Option<Result<String, String>>,
    /// Embedding vector dimensionality (needed by pgvector table creation).
    pub embedding_dimensions: usize,

    // ── embedding / llm ───────────────────────────────────────────────────
    /// Resolved embedding-engine inputs.
    pub embedding: EmbeddingInputs,
    /// Resolved LLM / transcriber inputs.
    pub llm: LlmInputs,
}

/// Resolved embedding-engine inputs. Mapped to a `cognee_embedding::EmbeddingConfig`
/// by [`crate::build_embedding_config`].
#[derive(Clone, Default)]
pub struct EmbeddingInputs {
    /// Lowercase provider string. An empty value defaults to `onnx`; a
    /// non-empty *unrecognized* value is rejected as a misconfiguration by the
    /// default embedding factory (rather than silently falling back to `onnx`).
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    /// Resolved endpoint (embedding-specific, falling back to the LLM endpoint).
    pub endpoint: Option<String>,
    /// Resolved API key (embedding-specific, falling back to the LLM key).
    pub api_key: Option<String>,
    pub batch_size: usize,
    /// Pace embedding dispatch (`EMBEDDING_RATE_LIMIT_ENABLED`). Flag-gated
    /// only: unlike the LLM path, provider overload never switches this on by
    /// itself, matching Python's embedding limiter.
    pub rate_limit_enabled: bool,
    /// Requests admitted per `rate_limit_interval` once pacing is active.
    pub rate_limit_requests: u32,
    /// Pacing window in seconds.
    pub rate_limit_interval: u32,
    /// `MOCK_EMBEDDING` opt-in — overrides `provider` to the mock engine.
    pub mock: bool,
    /// When `mock` is set, selects SHA-256-derived vectors instead of zeros.
    pub mock_deterministic: bool,
    /// Forward-compat fields historically read from the environment.
    pub api_version: Option<String>,
    pub huggingface_tokenizer: Option<String>,
    pub max_completion_tokens: usize,

    // ONNX asset paths — carried unconditionally; only consumed under the
    // `onnx` feature.
    pub onnx_model_path: PathBuf,
    pub onnx_tokenizer_path: PathBuf,
    pub onnx_model_name: String,
    pub onnx_dimensions: usize,
    pub onnx_max_sequence_length: usize,
    pub onnx_batch_size: usize,

    /// Resolved AWS inputs for the Bedrock embedding engine. Env-only, filled
    /// by [`aws_inputs_from_env`] at both lowering sites; ignored by every
    /// other provider.
    pub aws: AwsInputs,
}

/// Resolved LLM / transcriber inputs.
#[derive(Clone, Default)]
pub struct LlmInputs {
    /// Lowercase provider string (`openai` | `ollama` | `mistral` | `gemini` |
    /// `custom` | `openai_compatible` | `mock` | closed providers).
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub endpoint: String,
    /// Optional Anthropic Messages API base URL override (env `ANTHROPIC_BASE_URL`,
    /// alias `ANTHROPIC_API_BASE`). `None` uses the public Anthropic API. Kept
    /// separate from `endpoint` on purpose: `endpoint` aliases `OPENAI_URL`, so
    /// inheriting it would point Anthropic traffic at the OpenAI host. Only the
    /// Anthropic factory consumes it; other providers ignore it.
    pub anthropic_base_url: Option<String>,
    pub max_retries: u32,
    /// Minimum seconds a transient failure is retried for. Together with
    /// `max_retries` this is Python's dual-floor stop condition; `0` reduces it
    /// to a plain attempt cap. See `Settings::llm_min_retry_seconds`.
    pub min_retry_seconds: u32,
    /// Pace dispatch unconditionally (`LLM_RATE_LIMIT_ENABLED`).
    pub rate_limit_enabled: bool,
    /// Requests admitted per `rate_limit_interval` once pacing is active.
    pub rate_limit_requests: u32,
    /// Pacing window in seconds.
    pub rate_limit_interval: u32,
    /// Let provider overload switch pacing on by itself (`AUTO_RATE_LIMIT`).
    pub auto_rate_limit: bool,
    /// Output-token ceiling (Python's `llm_max_completion_tokens`), lowered from
    /// `Settings.llm_max_completion_tokens` (`setLlmMaxCompletionTokens`). Applied
    /// to LLM calls that pass no per-call generation options (the search/completion
    /// retrievers) so the setter actually caps `recall`/`search`. Wired into the
    /// OpenAI-compatible adapter via `OpenAIAdapter::with_default_max_tokens`;
    /// adapters that must send an explicit `max_tokens` (Anthropic) clamp it
    /// against the model's documented output limit.
    pub max_completion_tokens: u32,
    /// Extra request parameters merged into every chat-completion request body,
    /// lowered from `LLM_ARGS` (Python `llm_config.llm_args`). Empty = no-op.
    /// Applied by the OpenAI-compatible factory via
    /// `OpenAIAdapter::with_extra_args`. See that field's docs for semantics.
    pub llm_args: serde_json::Map<String, serde_json::Value>,
    /// Azure `api-version` (empty = unset). Only consumed by the azure provider,
    /// which requires it alongside a deployment `endpoint`.
    pub api_version: String,
    /// Explicit override for OpenAI reasoning-model detection, from `LLM_REASONING`
    /// (`auto` | `always` | `never`). `None` keeps the adapter's name/host
    /// auto-detection; `Some(true)`/`Some(false)` force the reasoning / legacy
    /// parameter shape. Lets an operator correct an endpoint whose reasoning
    /// nature the model name mis-signals. Applied via
    /// `OpenAIAdapter::with_reasoning_override`.
    pub reasoning_override: Option<bool>,
    /// Replaces the provider adapter with a cassette replay mock.
    pub mock: bool,
    /// Cassette path for the replay mock (consumed only under `mock-llm`).
    pub cassette: String,
    /// When non-empty, wraps the real adapter in a recorder (`mock-llm`).
    pub record_path: String,

    /// Resolved AWS inputs for the Bedrock adapter. Env-only, filled by
    /// [`aws_inputs_from_env`] at both lowering sites; ignored by every other
    /// provider.
    pub aws: AwsInputs,
}

/// Caller-supplied AWS parameters for the Bedrock LLM adapter and embedding
/// engine, resolved from the environment when a config is lowered into a
/// [`BackendBuildContext`].
///
/// Deliberately mirrors `cognee_llm::adapters::bedrock::aws::env::AwsInputs`
/// field-for-field: that struct lives behind the `bedrock` feature (see
/// `crates/llm/src/adapters/mod.rs`) while this module stays feature-free, so
/// the two are a mirrored pair rather than a re-export, and the field-for-field
/// conversion between them lives on the consumer side. **The two declarations
/// must move together** — adding, renaming or reordering a field here without
/// doing the same there breaks that conversion.
///
/// Follows the [`anthropic_base_url_from_env`] precedent: env-only, carried on
/// [`LlmInputs`] / [`EmbeddingInputs`], and **not** a `Settings` field — Python
/// resolves these from the environment and never threads them through its LLM
/// config.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AwsInputs {
    /// An explicitly caller-supplied region **only** — deliberately never
    /// backfilled from `AWS_REGION_NAME` / `AWS_REGION` by
    /// [`aws_inputs_from_env`].
    ///
    /// The plan's §1.3 chain is: this parameter → the region embedded in a
    /// model ARN → `AWS_REGION_NAME` → `AWS_REGION` → the ambient profile chain
    /// → the hard default `us-west-2`, and it is implemented by
    /// `cognee_llm::adapters::bedrock::aws::region::resolve_region_without_ambient`.
    /// Filling this field from the environment here would make the first rung
    /// always populated and silently push the model-ARN region below the env
    /// vars, inverting Python's precedence for every ARN model id. Nothing sets
    /// it today; it is kept for shape parity with the adapter-side struct and
    /// for a future explicit override.
    pub region: Option<String>,
    /// `aws_access_key_id` ← `AWS_ACCESS_KEY_ID`.
    pub access_key_id: Option<String>,
    /// `aws_secret_access_key` ← `AWS_SECRET_ACCESS_KEY`.
    pub secret_access_key: Option<String>,
    /// `aws_session_token` ← `AWS_SESSION_TOKEN`.
    pub session_token: Option<String>,
    /// `aws_profile_name` ← `AWS_PROFILE_NAME` (**not** `AWS_PROFILE`).
    pub profile_name: Option<String>,
    /// `aws_role_name` ← `AWS_ROLE_NAME` — an IAM role ARN to assume.
    pub role_name: Option<String>,
    /// `aws_session_name` ← `AWS_SESSION_NAME` — STS session name.
    pub session_name: Option<String>,
    /// `aws_web_identity_token` ← `AWS_WEB_IDENTITY_TOKEN`.
    pub web_identity_token: Option<String>,
    /// `aws_sts_endpoint` ← `AWS_STS_ENDPOINT`.
    pub sts_endpoint: Option<String>,
    /// `aws_external_id` ← `AWS_EXTERNAL_ID`.
    pub external_id: Option<String>,
    /// `aws_bedrock_runtime_endpoint` ← `AWS_BEDROCK_RUNTIME_ENDPOINT`.
    pub bedrock_runtime_endpoint: Option<String>,
    /// Bedrock API-key bearer token ← `AWS_BEARER_TOKEN_BEDROCK`.
    pub bearer_token: Option<String>,
}

/// Hand-written (never derived) so credentials cannot reach a log line: the
/// sibling input structs on [`BackendBuildContext`] are routinely rendered with
/// `{:?}` in build-failure diagnostics. Mirrors the redaction of
/// `cognee_llm::adapters::bedrock::aws::env::AwsInputs`.
impl fmt::Debug for AwsInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsInputs")
            .field("region", &self.region)
            .field("access_key_id", &shown(&self.access_key_id))
            .field("secret_access_key", &shown(&self.secret_access_key))
            .field("session_token", &shown(&self.session_token))
            .field("profile_name", &self.profile_name)
            .field("role_name", &self.role_name)
            .field("session_name", &self.session_name)
            .field("web_identity_token", &shown(&self.web_identity_token))
            .field("sts_endpoint", &self.sts_endpoint)
            .field("external_id", &self.external_id)
            .field("bedrock_runtime_endpoint", &self.bedrock_runtime_endpoint)
            .field("bearer_token", &shown(&self.bearer_token))
            .finish()
    }
}

/// The field-for-field conversion to the adapter-side struct.
///
/// The two declarations are a deliberately mirrored pair (see [`AwsInputs`]):
/// this module stays feature-free so every consumer can carry the inputs, while
/// `cognee_llm::adapters::bedrock::aws::env::AwsInputs` — the one the
/// credential/region/endpoint chains actually resolve — lives behind the
/// `bedrock` feature. This impl is the single crossing point between them, and
/// it is exhaustive on purpose: adding a field on either side without the other
/// breaks the build here rather than silently dropping a credential.
#[cfg(feature = "bedrock")]
impl From<&AwsInputs> for cognee_llm::adapters::bedrock::aws::env::AwsInputs {
    fn from(inputs: &AwsInputs) -> Self {
        let AwsInputs {
            region,
            access_key_id,
            secret_access_key,
            session_token,
            profile_name,
            role_name,
            session_name,
            web_identity_token,
            sts_endpoint,
            external_id,
            bedrock_runtime_endpoint,
            bearer_token,
        } = inputs;
        Self {
            region: region.clone(),
            access_key_id: access_key_id.clone(),
            secret_access_key: secret_access_key.clone(),
            session_token: session_token.clone(),
            profile_name: profile_name.clone(),
            role_name: role_name.clone(),
            session_name: session_name.clone(),
            web_identity_token: web_identity_token.clone(),
            sts_endpoint: sts_endpoint.clone(),
            external_id: external_id.clone(),
            bedrock_runtime_endpoint: bedrock_runtime_endpoint.clone(),
            bearer_token: bearer_token.clone(),
        }
    }
}

/// `"set"` / `"unset"`, so a secret's presence can be debugged without the
/// secret itself reaching a log line.
fn shown(value: &Option<String>) -> &'static str {
    if value.is_some() { "set" } else { "unset" }
}

/// Read the Bedrock/AWS knobs from the environment.
///
/// The names are litellm's `params_to_check` uppercase set
/// (`base_aws_llm.py::get_credentials`) plus the endpoint and bearer-token
/// variables — several are not the ones a reader guesses, notably
/// `AWS_PROFILE_NAME` (**not** the boto3-standard `AWS_PROFILE`). Every value
/// is trimmed and an empty (or all-whitespace) value is treated as unset, so an
/// exported-but-empty variable cannot select a code path with an unusable
/// value.
///
/// [`AwsInputs::region`] is deliberately left `None` — see that field's docs.
///
/// Lives here — the env-read boundary for [`LlmInputs`] / [`EmbeddingInputs`] —
/// so both the SDK `Settings` and the standalone HTTP server resolve these
/// knobs identically when lowering their config into a
/// [`BackendBuildContext`].
pub fn aws_inputs_from_env() -> AwsInputs {
    aws_inputs_from(|key| std::env::var(key).ok())
}

/// [`aws_inputs_from_env`] over an injectable lookup, so the name mapping is
/// testable without mutating shared process state (the workspace test runner is
/// parallel, and `std::env::set_var` is `unsafe` in edition 2024).
fn aws_inputs_from(lookup: impl Fn(&str) -> Option<String>) -> AwsInputs {
    let read = |key: &str| {
        lookup(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    AwsInputs {
        // No env leg on purpose — see the field docs (plan §1.3 / `region.rs`).
        region: None,
        access_key_id: read("AWS_ACCESS_KEY_ID"),
        secret_access_key: read("AWS_SECRET_ACCESS_KEY"),
        session_token: read("AWS_SESSION_TOKEN"),
        profile_name: read("AWS_PROFILE_NAME"),
        role_name: read("AWS_ROLE_NAME"),
        session_name: read("AWS_SESSION_NAME"),
        web_identity_token: read("AWS_WEB_IDENTITY_TOKEN"),
        sts_endpoint: read("AWS_STS_ENDPOINT"),
        external_id: read("AWS_EXTERNAL_ID"),
        bedrock_runtime_endpoint: read("AWS_BEDROCK_RUNTIME_ENDPOINT"),
        bearer_token: read("AWS_BEARER_TOKEN_BEDROCK"),
    }
}

/// Read the optional Anthropic base-URL override from the environment
/// (`ANTHROPIC_BASE_URL`, alias `ANTHROPIC_API_BASE`). Trims surrounding
/// whitespace and treats an empty value as unset (`None`).
///
/// Lives here — the env-read boundary for [`LlmInputs`] — so both the SDK
/// `Settings` and the standalone HTTP server resolve the same knob identically
/// when lowering their config into a [`BackendBuildContext`].
pub fn anthropic_base_url_from_env() -> Option<String> {
    std::env::var("ANTHROPIC_BASE_URL")
        .or_else(|_| std::env::var("ANTHROPIC_API_BASE"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse the `LLM_REASONING` knob into a reasoning-detection override for
/// [`LlmInputs::reasoning_override`]. `always`/`true`/`on`/`1` force reasoning
/// mode on, `never`/`false`/`off`/`0` force it off, and everything else —
/// including the default `auto`, an empty value, or an unrecognised token —
/// leaves auto-detection in place (`None`). Case- and whitespace-insensitive.
///
/// Lives here — the shared config→[`LlmInputs`] boundary — so the SDK `Settings`
/// and the standalone HTTP server resolve the knob identically.
pub fn parse_reasoning_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "always" | "true" | "on" | "1" => Some(true),
        "never" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{AwsInputs, aws_inputs_from, parse_reasoning_override};

    #[test]
    fn parse_reasoning_override_maps_known_tokens() {
        for on in ["always", "true", "on", "1", "  ALWAYS  ", "True"] {
            assert_eq!(parse_reasoning_override(on), Some(true), "{on:?}");
        }
        for off in ["never", "false", "off", "0", "Never"] {
            assert_eq!(parse_reasoning_override(off), Some(false), "{off:?}");
        }
        // Default, empty, and unrecognised tokens fall through to auto (None).
        for auto in ["auto", "", "   ", "maybe", "yes"] {
            assert_eq!(parse_reasoning_override(auto), None, "{auto:?}");
        }
    }

    /// A fake environment: every key not listed is unset.
    fn env_of(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    /// One row of [`FIELDS`]: an env-var name and the accessor for the field it
    /// must land in.
    type FieldRow = (&'static str, fn(&AwsInputs) -> Option<&str>);

    /// Every field paired with the **exact** variable it must be read from.
    /// `AWS_PROFILE_NAME` (not `AWS_PROFILE`) and the other non-obvious litellm
    /// names are the whole point of this table.
    const FIELDS: &[FieldRow] = &[
        ("AWS_ACCESS_KEY_ID", |i| i.access_key_id.as_deref()),
        ("AWS_SECRET_ACCESS_KEY", |i| i.secret_access_key.as_deref()),
        ("AWS_SESSION_TOKEN", |i| i.session_token.as_deref()),
        ("AWS_PROFILE_NAME", |i| i.profile_name.as_deref()),
        ("AWS_ROLE_NAME", |i| i.role_name.as_deref()),
        ("AWS_SESSION_NAME", |i| i.session_name.as_deref()),
        ("AWS_WEB_IDENTITY_TOKEN", |i| {
            i.web_identity_token.as_deref()
        }),
        ("AWS_STS_ENDPOINT", |i| i.sts_endpoint.as_deref()),
        ("AWS_EXTERNAL_ID", |i| i.external_id.as_deref()),
        ("AWS_BEDROCK_RUNTIME_ENDPOINT", |i| {
            i.bedrock_runtime_endpoint.as_deref()
        }),
        ("AWS_BEARER_TOKEN_BEDROCK", |i| i.bearer_token.as_deref()),
    ];

    #[test]
    fn each_field_reads_exactly_its_own_env_var() {
        for (var, get) in FIELDS {
            // Only this one variable is set, so a field reading the wrong name
            // (e.g. `AWS_PROFILE` for `profile_name`) stays `None` and fails.
            let inputs = aws_inputs_from(env_of(&[(var, "value-for-this-var")]));

            assert_eq!(
                get(&inputs),
                Some("value-for-this-var"),
                "{var} must populate its field"
            );
            for (other, other_get) in FIELDS {
                if other != var {
                    assert_eq!(
                        other_get(&inputs),
                        None,
                        "{var} must not also populate the {other} field"
                    );
                }
            }
        }
    }

    /// Guards [`FIELDS`] against drifting behind the struct: the exhaustive
    /// destructuring is `E0027` the moment a field is added to [`AwsInputs`],
    /// and a field with no row in the table stays `None` and fails here — so a
    /// newly mirrored field cannot slip in untested.
    #[test]
    fn every_field_except_region_has_a_row_in_the_table() {
        let pairs: Vec<(&'static str, &'static str)> =
            FIELDS.iter().map(|(var, _)| (*var, "covered")).collect();
        let AwsInputs {
            region,
            access_key_id,
            secret_access_key,
            session_token,
            profile_name,
            role_name,
            session_name,
            web_identity_token,
            sts_endpoint,
            external_id,
            bedrock_runtime_endpoint,
            bearer_token,
        } = aws_inputs_from(env_of(&pairs));

        assert_eq!(
            region, None,
            "region has no env leg — see `region_is_never_read_from_the_environment`"
        );
        for (name, value) in [
            ("access_key_id", access_key_id),
            ("secret_access_key", secret_access_key),
            ("session_token", session_token),
            ("profile_name", profile_name),
            ("role_name", role_name),
            ("session_name", session_name),
            ("web_identity_token", web_identity_token),
            ("sts_endpoint", sts_endpoint),
            ("external_id", external_id),
            ("bedrock_runtime_endpoint", bedrock_runtime_endpoint),
            ("bearer_token", bearer_token),
        ] {
            assert_eq!(
                value.as_deref(),
                Some("covered"),
                "{name} is not covered by FIELDS"
            );
        }
    }

    #[test]
    fn values_are_trimmed() {
        let pairs: Vec<(&'static str, &'static str)> = FIELDS
            .iter()
            .map(|(var, _)| (*var, "  us-east-1  "))
            .collect();
        let inputs = aws_inputs_from(env_of(&pairs));

        for (var, get) in FIELDS {
            assert_eq!(get(&inputs), Some("us-east-1"), "{var} must be trimmed");
        }
    }

    #[test]
    fn exported_but_empty_values_are_unset() {
        for blank in ["", "   ", "\t\n"] {
            let pairs: Vec<(&'static str, &'static str)> =
                FIELDS.iter().map(|(var, _)| (*var, blank)).collect();
            let inputs = aws_inputs_from(env_of(&pairs));

            assert_eq!(
                inputs,
                AwsInputs::default(),
                "{blank:?} must read as unset, not Some(\"\")"
            );
        }
    }

    #[test]
    fn an_empty_environment_yields_the_default() {
        assert_eq!(aws_inputs_from(|_: &str| None), AwsInputs::default());
    }

    /// Plan §1.3: the region chain is caller parameter → model-ARN region →
    /// `AWS_REGION_NAME` → `AWS_REGION` → ambient profile → `us-west-2`, and it
    /// is owned by `cognee_llm::adapters::bedrock::aws::region`. Backfilling
    /// `region` here would occupy the first rung and silently push the model-ARN
    /// region below the env vars, so this must stay `None`.
    #[test]
    fn region_is_never_read_from_the_environment() {
        let inputs = aws_inputs_from(env_of(&[
            ("AWS_REGION_NAME", "eu-central-1"),
            ("AWS_REGION", "ap-south-1"),
            ("AWS_DEFAULT_REGION", "sa-east-1"),
        ]));

        assert_eq!(inputs.region, None);
        assert_eq!(inputs, AwsInputs::default());
    }

    #[test]
    fn debug_redacts_secret_material() {
        let inputs = AwsInputs {
            access_key_id: Some("AKIA_VISIBLE".to_string()),
            secret_access_key: Some("super-secret".to_string()),
            session_token: Some("session-secret".to_string()),
            web_identity_token: Some("jwt-secret".to_string()),
            bearer_token: Some("bearer-secret".to_string()),
            profile_name: Some("dev".to_string()),
            ..AwsInputs::default()
        };

        let rendered = format!("{inputs:?}");

        for secret in [
            "AKIA_VISIBLE",
            "super-secret",
            "session-secret",
            "jwt-secret",
            "bearer-secret",
        ] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        assert!(rendered.contains("secret_access_key: \"set\""));
        assert!(
            rendered.contains("profile_name: Some(\"dev\")"),
            "non-secret fields stay readable: {rendered}"
        );
    }
}
