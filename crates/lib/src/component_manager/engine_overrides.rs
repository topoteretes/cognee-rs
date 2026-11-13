//! Per-call LLM and embedding engines built from configs: the engines behind
//! recall's and search's `llm_config` / `embedding_config` (Python
//! `recall.py:467-468`, `search.py:71-72`, set as context variables at
//! `context_global_variables.py:198-201`).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use cognee_components::{EmbeddingConfig, EmbeddingInputs, LlmConfig};
use cognee_config_core::{CallSection, replace_section};
use cognee_search::EngineOverride;
use sha2::{Digest, Sha256};

use super::ComponentManager;
use crate::api::ApiError;
use crate::context::PipelineContext;

/// How many per-call engine sets are kept. Python caches its engine
/// constructors with `lru_cache` (`get_embedding_engine.py`,
/// `get_llm_client.py`); a bound keeps a stream of distinct configs from
/// holding every engine (an ONNX model, an HTTP client) it ever built.
const CACHE_CAPACITY: usize = 16;

/// A cache key: the SHA-256 of the configuration version and the per-call
/// configs, so no API key a config carries is kept in plain text.
type CacheKey = [u8; 32];

/// The engines built for recent per-call configs, most recently used first,
/// keyed by [`cache_key`].
#[derive(Default)]
pub(super) struct OverrideCache {
    entries: Mutex<VecDeque<(CacheKey, EngineOverride)>>,
}

impl OverrideCache {
    /// The engines cached under `key`, moved to the front.
    fn get(&self, key: &CacheKey) -> Option<EngineOverride> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let index = entries.iter().position(|(k, _)| k == key)?;
        let entry = entries.remove(index)?;
        let engines = entry.1.clone();
        entries.push_front(entry);
        Some(engines)
    }

    /// Cache `engines` under `key` at the front, dropping the least recently
    /// used entry beyond the capacity.
    fn insert(&self, key: CacheKey, engines: EngineOverride) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(k, _)| *k != key);
        entries.push_front((key, engines));
        entries.truncate(CACHE_CAPACITY);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// The cache key of one call: a digest of the configuration version and the
/// settings each config gave (canonical keys, sorted, so equal configs hash
/// equally).
fn cache_key(
    version: u64,
    llm: Option<&CallSection<LlmConfig>>,
    embedding: Option<&CallSection<EmbeddingConfig>>,
) -> CacheKey {
    let canonical = serde_json::json!([
        version,
        llm.map(CallSection::values),
        embedding.map(CallSection::values)
    ])
    .to_string();
    Sha256::digest(canonical.as_bytes()).into()
}

impl ComponentManager {
    /// The engines a call with these per-call configs runs with: pass them as
    /// [`RecallOptions::engine_override`](crate::api::RecallOptions::engine_override),
    /// or run a search under
    /// [`with_engine_override`](cognee_search::with_engine_override).
    ///
    /// As in Python, a per-call config **replaces the whole section** for the
    /// call; it is not overlaid onto the configured one. A setting the config
    /// leaves out takes the value the **environment** gave it, as Python's
    /// per-call `LLMConfig` / `EmbeddingConfig` (pydantic `BaseSettings`) read
    /// it from the environment: the configuration's env layer
    /// ([`Settings::env_layer`](crate::config::Settings::env_layer)), captured
    /// when the configuration was read from the environment and `.env` file,
    /// with no new environment read here. A value set in code or loaded from
    /// a file (`ConfigManager::set`, a binding's settings object) is not
    /// inherited, as Python's runtime-modified global config is not seen by a
    /// fresh `LLMConfig()`; where the environment sets nothing either, the
    /// section's default applies. So an `llm_config` that names only a model
    /// keeps an `LLM_API_KEY` from the environment, and one on a deployment
    /// whose key was set in code has no key, and its calls fail as an
    /// unconfigured LLM's do (recall then answers completion types with
    /// context only).
    ///
    /// The configs are lowered the way the configuration is: an embedder with
    /// no key or endpoint of its own inherits the call's LLM's, and with no
    /// usable LLM the keyless local defaults apply. An `llm_config` alone
    /// therefore also rebuilds the embedding engine when the embedder it
    /// lowers to differs from the configured one (Python's
    /// `get_embedding_engine` reads the call's LLM config for that fallback).
    /// On a keyless deployment embedding with the local model, a keyed
    /// `llm_config` thus moves the embedder off it, which the width check
    /// below refuses unless an `embedding_config` names a model of the stored
    /// width. Every other input (paths, stores, provider sections) is the
    /// configuration's.
    ///
    /// Engines are built through this manager's registry, so a per-call config
    /// can select any registered provider, and cached: a later call with the
    /// same configs on the same configuration version reuses them. The cache
    /// is keyed by a SHA-256 digest of the configs, never their plain text.
    ///
    /// A typed config with every setting chosen converts with
    /// `CallSection::from_values(config.to_values())`.
    ///
    /// # Errors
    /// [`ApiError::EmbeddingDimensionMismatch`] when the call's embedding
    /// engine is not as wide as the configured one, which built the vector
    /// store (Python's `EmbeddingDimensionMismatchError`), and
    /// [`ApiError::Component`] when the configuration is unsupported or an
    /// engine cannot be built (an unknown provider, say).
    pub async fn build_engines_with_overrides(
        &self,
        llm: Option<&CallSection<LlmConfig>>,
        embedding: Option<&CallSection<EmbeddingConfig>>,
    ) -> Result<EngineOverride, ApiError> {
        if llm.is_none() && embedding.is_none() {
            return Ok(EngineOverride::default());
        }
        let configured = self.build_context().await?;
        let key = cache_key(self.config.version(), llm, embedding);

        let engines = match self.engine_overrides.get(&key) {
            Some(engines) => engines,
            None => {
                let ctx = {
                    let mut settings = self.config.read().clone();
                    let from_env = settings.env_layer.values();
                    let llm_values = llm.map(|call| call.over(from_env));
                    let embedding_values = embedding.map(|call| call.over(from_env));
                    if let Some(values) = llm_values {
                        replace_section::<LlmConfig>(&mut settings.sections, values);
                    }
                    if let Some(values) = embedding_values {
                        replace_section::<EmbeddingConfig>(&mut settings.sections, values);
                    }
                    self.lower_settings(&settings)
                };
                let llm_engine = match llm {
                    Some(_) => Some(self.registry.build_llm(&ctx).await?),
                    None => None,
                };
                let embedding_engine = if embedding.is_some()
                    || !same_embedder(&configured.embedding, &ctx.embedding)
                {
                    Some(self.registry.build_embedding(&ctx).await?)
                } else {
                    None
                };
                let engines = EngineOverride {
                    llm: llm_engine,
                    embedding: embedding_engine,
                };
                self.engine_overrides.insert(key, engines.clone());
                engines
            }
        };
        if engines.embedding.is_some() {
            let configured_width = self.embedding_engine().await?.dimension();
            engines.check_embedding_dimension(configured_width)?;
        }
        Ok(engines)
    }
}

/// Whether two lowered embedders are the same engine: the inputs an
/// `llm_config` can change through the key and endpoint fallback or the
/// keyless switch.
fn same_embedder(a: &EmbeddingInputs, b: &EmbeddingInputs) -> bool {
    a.provider == b.provider
        && a.model == b.model
        && a.dimensions == b.dimensions
        && a.endpoint == b.endpoint
        && a.api_key == b.api_key
        && a.mock == b.mock
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_config_core::MapEnv;
    use serde_json::json;

    use super::*;
    use crate::config::{ConfigManager, Settings};

    /// A deployment with a keyed OpenAI LLM and a 4-dimensional mock
    /// embedder, everything set in code (no environment layer).
    fn manager() -> ComponentManager {
        let settings = Settings {
            sections: [
                ("llm.provider".to_string(), json!("openai")),
                ("llm.api_key".to_string(), json!("sk-deployment")),
                ("llm.model".to_string(), json!("gpt-4o-mini")),
                ("embedding.mock".to_string(), json!("true")),
                ("embedding.dimensions".to_string(), json!(4)),
            ]
            .into(),
            ..Settings::default()
        };
        ComponentManager::new(ConfigManager::new(settings))
    }

    /// The same deployment read from the environment: its LLM key and
    /// embedder come from env vars, as a server's do.
    fn env_manager() -> ComponentManager {
        let mut settings = Settings::default();
        settings.overlay_from(
            &MapEnv::new()
                .with("LLM_PROVIDER", "openai")
                .with("LLM_API_KEY", "sk-from-env")
                .with("LLM_MODEL", "gpt-4o-mini")
                .with("MOCK_EMBEDDING", "true")
                .with("EMBEDDING_DIMENSIONS", "4"),
        );
        ComponentManager::new(ConfigManager::new(settings))
    }

    fn llm(object: serde_json::Value) -> CallSection<LlmConfig> {
        CallSection::from_object(object.as_object().unwrap()).unwrap()
    }

    fn mock_embedding(dimensions: u32) -> CallSection<EmbeddingConfig> {
        CallSection::from_object(
            json!({ "mock_embedding": "true", "embedding_dimensions": dimensions })
                .as_object()
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn no_configs_override_nothing() {
        let engines = manager()
            .build_engines_with_overrides(None, None)
            .await
            .unwrap();
        assert!(engines.is_empty());
    }

    /// The call's `llm_config` replaces the whole section: a key the
    /// deployment set in code is not inherited (Python's fresh `LLMConfig()`
    /// does not see runtime changes), so a config without one is an
    /// unconfigured LLM, and one with a key is usable.
    #[tokio::test]
    async fn an_llm_config_does_not_inherit_a_key_set_in_code() {
        let cm = manager();
        assert!(cm.llm().await.unwrap().is_configured());

        let engines = cm
            .build_engines_with_overrides(Some(&llm(json!({ "llm_model": "gpt-4o" }))), None)
            .await
            .unwrap();
        assert!(!engines.llm.as_ref().unwrap().is_configured());

        let keyed = llm(json!({ "llm_model": "gpt-4o", "llm_api_key": "sk-call" }));
        let engines = cm
            .build_engines_with_overrides(Some(&keyed), None)
            .await
            .unwrap();
        assert!(engines.llm.as_ref().unwrap().is_configured());
        assert_eq!(engines.llm.as_ref().unwrap().model(), "gpt-4o");
    }

    /// Python's per-call `LLMConfig(llm_model=...)` is a `BaseSettings`: the
    /// key it leaves out comes from `LLM_API_KEY`. A model-only `llm_config`
    /// on a deployment read from the environment keeps that key, and the
    /// embedder, which inherits the LLM key, is the configured one.
    #[tokio::test]
    async fn a_model_only_llm_config_keeps_the_env_sourced_key() {
        let cm = env_manager();
        let engines = cm
            .build_engines_with_overrides(Some(&llm(json!({ "llm_model": "gpt-4o" }))), None)
            .await
            .unwrap();
        let call_llm = engines.llm.as_ref().unwrap();
        assert!(call_llm.is_configured(), "the env key is inherited");
        assert_eq!(call_llm.model(), "gpt-4o");
        assert!(
            engines.embedding.is_none(),
            "the embedder inherits the same env key and is kept"
        );
    }

    /// A value set in code after the environment was read is not inherited:
    /// the env layer still has the environment's value.
    #[tokio::test]
    async fn a_value_set_in_code_over_the_env_is_not_inherited() {
        let cm = env_manager();
        cm.config().set_llm_model("gpt-set-in-code");
        let engines = cm
            .build_engines_with_overrides(Some(&llm(json!({ "llm_api_key": "sk-call" }))), None)
            .await
            .unwrap();
        assert_eq!(engines.llm.as_ref().unwrap().model(), "gpt-4o-mini");
    }

    /// On a deployment read from the environment the embedder's key is
    /// env-sourced too: `embedding.api_key` binds `EMBEDDING_API_KEY`, then
    /// `LLM_API_KEY`. A call that changes only the LLM key therefore keeps the
    /// configured embedder, which still embeds. (Python's `EmbeddingConfig`
    /// reads only `EMBEDDING_API_KEY` and falls back to the call's LLM key in
    /// `get_embedding_engine`; the Rust env binding differs, see
    /// `docs/operations.md`.)
    #[tokio::test]
    async fn an_env_deployments_embedder_keeps_working_under_an_llm_config() {
        let cm = env_manager();
        let engines = cm
            .build_engines_with_overrides(Some(&llm(json!({ "llm_api_key": "sk-call" }))), None)
            .await
            .unwrap();
        assert!(
            engines.embedding.is_none(),
            "the configured embedder is kept"
        );
        let embedder = cm.embedding_engine().await.unwrap();
        assert_eq!(embedder.embed(&["hello"]).await.unwrap()[0].len(), 4);
    }

    #[tokio::test]
    async fn the_same_configs_reuse_the_cached_engines() {
        let cm = manager();
        let call = llm(json!({ "llm_api_key": "sk-call" }));
        let embedding = mock_embedding(4);
        let first = cm
            .build_engines_with_overrides(Some(&call), Some(&embedding))
            .await
            .unwrap();
        let second = cm
            .build_engines_with_overrides(Some(&call), Some(&embedding))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(
            first.llm.as_ref().unwrap(),
            second.llm.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            first.embedding.as_ref().unwrap(),
            second.embedding.as_ref().unwrap()
        ));
        assert_eq!(cm.engine_overrides.len(), 1);

        // Configs differing only in their API key are separate entries.
        let other = llm(json!({ "llm_api_key": "sk-other" }));
        let third = cm
            .build_engines_with_overrides(Some(&other), Some(&embedding))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(
            first.llm.as_ref().unwrap(),
            third.llm.as_ref().unwrap()
        ));
        assert_eq!(cm.engine_overrides.len(), 2);
    }

    /// The cache key is a digest: no API key is kept in plain text, and keys
    /// differing only in the API key differ.
    #[test]
    fn the_cache_key_is_a_digest_of_the_configs() {
        let a = llm(json!({ "llm_api_key": "sk-secret-a" }));
        let b = llm(json!({ "llm_api_key": "sk-secret-b" }));
        let key_a = cache_key(3, Some(&a), None);
        assert_eq!(key_a, cache_key(3, Some(&a), None));
        assert_ne!(key_a, cache_key(3, Some(&b), None));
        assert_ne!(key_a, cache_key(4, Some(&a), None));
        assert!(
            !key_a
                .windows(b"sk-secret-a".len())
                .any(|window| window == b"sk-secret-a")
        );
    }

    /// An embedding model of another width than the configured one is
    /// refused before anything runs, cached or not.
    #[tokio::test]
    async fn an_embedding_config_of_the_wrong_width_is_refused() {
        let cm = manager();
        let engines = cm
            .build_engines_with_overrides(None, Some(&mock_embedding(4)))
            .await
            .unwrap();
        assert_eq!(engines.embedding.as_ref().unwrap().dimension(), 4);
        assert!(engines.llm.is_none());

        for _ in 0..2 {
            let err = cm
                .build_engines_with_overrides(None, Some(&mock_embedding(8)))
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    ApiError::EmbeddingDimensionMismatch(
                        cognee_search::EmbeddingDimensionMismatch {
                            configured_dimensions: 4,
                            override_dimensions: 8,
                        }
                    )
                ),
                "{err:?}"
            );
        }
    }

    /// An `llm_config` alone rebuilds the embedder only when the embedder
    /// inherits from it: here the deployment's mock embedder inherits the LLM
    /// key, which the call changes.
    #[tokio::test]
    async fn an_llm_config_rebuilds_an_embedder_that_inherits_its_key() {
        let cm = manager();
        let same_key = llm(json!({
            "llm_provider": "openai",
            "llm_api_key": "sk-deployment",
            "llm_model": "gpt-4o-mini",
        }));
        let engines = cm
            .build_engines_with_overrides(Some(&same_key), None)
            .await
            .unwrap();
        assert!(engines.embedding.is_none(), "the same embedder is kept");

        let other_key = llm(json!({ "llm_api_key": "sk-call" }));
        let engines = cm
            .build_engines_with_overrides(Some(&other_key), None)
            .await
            .unwrap();
        assert_eq!(engines.embedding.as_ref().unwrap().dimension(), 4);
    }

    #[tokio::test]
    async fn an_unknown_provider_is_a_component_error() {
        let cm = manager();
        let call = llm(json!({ "llm_provider": "no-such-provider", "llm_api_key": "k" }));
        let err = cm
            .build_engines_with_overrides(Some(&call), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Component(_)), "{err:?}");
    }

    #[test]
    fn the_cache_keeps_the_most_recently_used_entries() {
        let cache = OverrideCache::default();
        let key = |i: u8| [i; 32];
        for i in 0..=CACHE_CAPACITY as u8 {
            cache.insert(key(i), EngineOverride::default());
        }
        assert!(cache.get(&key(0)).is_none(), "the oldest entry is dropped");
        assert!(cache.get(&key(1)).is_some());
        // 1 is now the most recent: one more insert drops 2, not 1.
        cache.insert(key(200), EngineOverride::default());
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(2)).is_none());
    }
}
