//! `/api/v1/settings` — global LLM + vector-DB settings.
//!
//! Two endpoints per `routers/settings.md §2`:
//! - `GET /` — read current settings (with redacted API keys).
//! - `POST /` — save partial-update of either or both sub-configs.
//!
//! **Persistence**: in-process singleton. Python keeps these in process
//! memory (not a relational table) and resets on restart (`§3`). We replicate
//! exactly. The HTTP server cannot depend on `cognee` (would create a
//! dependency cycle), so the settings storage lives here rather than under
//! `cognee::settings`.
//!
//! The provider/model lists below are static constants copied verbatim from
//! Python's `cognee/modules/settings/get_settings.py L60-L179` for the
//! cross-SDK parity test.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::RwLock;

use axum::{
    Json, Router,
    routing::{get, post},
};

use crate::auth::AuthenticatedUser;
use crate::dto::settings::{
    ConfigChoice, LLMConfigInputDTO, LLMConfigOutputDTO, SettingsDTO, SettingsPayloadDTO,
    VectorDBConfigInputDTO, VectorDBConfigOutputDTO, redact_api_key, should_persist_api_key,
};
use crate::error::ApiError;
use crate::state::AppState;

// ── Static provider/model lists (Python parity) ────────────────────────────

fn llm_providers() -> Vec<ConfigChoice> {
    vec![
        ConfigChoice {
            value: "openai".into(),
            label: "OpenAI".into(),
        },
        ConfigChoice {
            value: "ollama".into(),
            label: "Ollama".into(),
        },
        ConfigChoice {
            value: "anthropic".into(),
            label: "Anthropic".into(),
        },
        ConfigChoice {
            value: "gemini".into(),
            label: "Gemini".into(),
        },
        ConfigChoice {
            value: "mistral".into(),
            label: "Mistral".into(),
        },
        ConfigChoice {
            value: "bedrock".into(),
            label: "AWS Bedrock".into(),
        },
    ]
}

fn vector_db_providers() -> Vec<ConfigChoice> {
    vec![
        ConfigChoice {
            value: "lancedb".into(),
            label: "LanceDB".into(),
        },
        ConfigChoice {
            value: "pgvector".into(),
            label: "pgvector".into(),
        },
        ConfigChoice {
            value: "brute-force".into(),
            label: "Brute-force (in-memory)".into(),
        },
    ]
}

fn llm_models() -> BTreeMap<String, Vec<ConfigChoice>> {
    let mut m = BTreeMap::new();
    m.insert(
        "openai".into(),
        vec![
            ConfigChoice {
                value: "gpt-4o".into(),
                label: "gpt-4o".into(),
            },
            ConfigChoice {
                value: "gpt-4o-mini".into(),
                label: "gpt-4o-mini".into(),
            },
            ConfigChoice {
                value: "gpt-4-turbo".into(),
                label: "gpt-4-turbo".into(),
            },
            ConfigChoice {
                value: "gpt-4".into(),
                label: "gpt-4".into(),
            },
            ConfigChoice {
                value: "gpt-3.5-turbo".into(),
                label: "gpt-3.5-turbo".into(),
            },
        ],
    );
    m.insert(
        "ollama".into(),
        vec![
            ConfigChoice {
                value: "llama3".into(),
                label: "llama3".into(),
            },
            ConfigChoice {
                value: "mistral".into(),
                label: "mistral".into(),
            },
        ],
    );
    m.insert(
        "anthropic".into(),
        vec![
            ConfigChoice {
                value: "claude-3-5-sonnet-latest".into(),
                label: "claude-3-5-sonnet-latest".into(),
            },
            ConfigChoice {
                value: "claude-3-opus-latest".into(),
                label: "claude-3-opus-latest".into(),
            },
        ],
    );
    m.insert(
        "gemini".into(),
        vec![
            ConfigChoice {
                value: "gemini-2.0-flash".into(),
                label: "gemini-2.0-flash".into(),
            },
            ConfigChoice {
                value: "gemini-1.5-pro".into(),
                label: "gemini-1.5-pro".into(),
            },
        ],
    );
    m.insert(
        "mistral".into(),
        vec![
            ConfigChoice {
                value: "mistral-large-latest".into(),
                label: "mistral-large-latest".into(),
            },
            ConfigChoice {
                value: "open-mistral-nemo".into(),
                label: "open-mistral-nemo".into(),
            },
        ],
    );
    m.insert(
        "bedrock".into(),
        vec![ConfigChoice {
            value: "anthropic.claude-3-5-sonnet-20240620-v1:0".into(),
            label: "anthropic.claude-3-5-sonnet-20240620-v1:0".into(),
        }],
    );
    m
}

// ── Process-singleton settings store ───────────────────────────────────────

#[derive(Debug, Clone)]
struct LlmSnapshot {
    provider: String,
    model: String,
    endpoint: Option<String>,
    api_version: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Clone)]
struct VectorSnapshot {
    provider: String,
    url: String,
    api_key: String,
}

struct SettingsStore {
    llm: RwLock<LlmSnapshot>,
    vector: RwLock<VectorSnapshot>,
}

impl SettingsStore {
    fn from_env() -> Self {
        Self {
            llm: RwLock::new(LlmSnapshot {
                provider: std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".into()),
                model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
                endpoint: std::env::var("LLM_ENDPOINT").ok(),
                api_version: std::env::var("LLM_API_VERSION").ok(),
                api_key: std::env::var("LLM_API_KEY")
                    .ok()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok()),
            }),
            vector: RwLock::new(VectorSnapshot {
                provider: std::env::var("VECTOR_DB_PROVIDER").unwrap_or_else(|_| "lancedb".into()),
                url: std::env::var("VECTOR_DB_URL").unwrap_or_default(),
                api_key: std::env::var("VECTOR_DB_KEY").unwrap_or_default(),
            }),
        }
    }
}

fn store() -> &'static SettingsStore {
    static S: OnceLock<SettingsStore> = OnceLock::new();
    S.get_or_init(SettingsStore::from_env)
}

// ── Handlers ───────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/settings",
    tag = "settings",
    responses(
        (status = 200, description = "current settings (api keys redacted)", body = SettingsDTO),
        (status = 401, description = "unauthorized"),
    )
)]
#[tracing::instrument(skip(_state), name = "cognee.api.settings.get")]
pub async fn get_settings(
    _user: AuthenticatedUser,
    axum::extract::State(_state): axum::extract::State<AppState>,
) -> Result<Json<SettingsDTO>, ApiError> {
    let s = store();
    // Read each snapshot under the lock; clone before responding.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    let llm = s.llm.read().unwrap().clone();
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    let vector = s.vector.read().unwrap().clone();

    let dto = SettingsDTO {
        llm: LLMConfigOutputDTO {
            provider: llm.provider,
            model: llm.model,
            endpoint: llm.endpoint,
            api_version: llm.api_version,
            api_key: redact_api_key(llm.api_key.as_deref()),
            providers: llm_providers(),
            models: llm_models(),
        },
        vector_db: VectorDBConfigOutputDTO {
            provider: vector.provider,
            url: vector.url,
            // Python returns the empty string when no key — see `§6.1`.
            api_key: redact_api_key(Some(&vector.api_key)).unwrap_or_default(),
            providers: vector_db_providers(),
        },
    };
    Ok(Json(dto))
}

#[utoipa::path(
    post,
    path = "/api/v1/settings",
    tag = "settings",
    request_body = SettingsPayloadDTO,
    responses(
        (status = 200, description = "settings saved; body is JSON null"),
        (status = 401, description = "unauthorized"),
        (status = 422, description = "invalid payload"),
    )
)]
#[tracing::instrument(skip(_state, payload), name = "cognee.api.settings.save")]
pub async fn save_settings(
    _user: AuthenticatedUser,
    axum::extract::State(_state): axum::extract::State<AppState>,
    Json(payload): Json<SettingsPayloadDTO>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let s = store();

    if let Some(LLMConfigInputDTO {
        provider,
        model,
        api_key,
    }) = payload.llm
    {
        #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
        let mut current = s.llm.write().unwrap();
        current.provider = match provider {
            crate::dto::settings::LlmProvider::Openai => "openai".into(),
            crate::dto::settings::LlmProvider::Ollama => "ollama".into(),
            crate::dto::settings::LlmProvider::Anthropic => "anthropic".into(),
            crate::dto::settings::LlmProvider::Gemini => "gemini".into(),
            crate::dto::settings::LlmProvider::Mistral => "mistral".into(),
        };
        current.model = model;
        if should_persist_api_key(&api_key) {
            current.api_key = Some(api_key);
        }
    }

    if let Some(VectorDBConfigInputDTO {
        provider,
        url,
        api_key,
    }) = payload.vector_db
    {
        #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
        let mut current = s.vector.write().unwrap();
        current.provider = match provider {
            crate::dto::settings::VectorDbProvider::Lancedb => "lancedb".into(),
            crate::dto::settings::VectorDbProvider::Chromadb => "chromadb".into(),
            crate::dto::settings::VectorDbProvider::Pgvector => "pgvector".into(),
            crate::dto::settings::VectorDbProvider::BruteForce => "brute-force".into(),
        };
        current.url = url;
        if should_persist_api_key(&api_key) {
            current.api_key = api_key;
        }
    }

    // Python parity: 200 with body `null`.
    Ok(Json(serde_json::Value::Null))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_settings))
        .route("/", post(save_settings))
}
