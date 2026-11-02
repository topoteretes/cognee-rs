//! Graph-readiness probe for recall's warm-up short-circuit.
//!
//! The probe reads only `pipeline_runs`, never a graph or vector engine, and
//! classifies the target datasets:
//!
//! * `warm`: a run that writes the graph completed, or only staging runs
//!   exist (data was added but not cognified, or a custom pipeline built a
//!   graph without logging runs). A false warm only means a normal search.
//! * `build_failed`: nothing writing the graph completed, and the latest run
//!   that writes it errored.
//! * `never_built`: no run has been recorded.
//!
//! Python also has `partial` (chunks searchable before extraction ends),
//! which needs its indexing pipeline; Rust has none yet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cognee_database::pipelines::{
    ADD_PIPELINE_NAME, CHANGE_RECORD_PIPELINE_NAMES, GraphBuildEvidence,
};
use cognee_database::{DatabaseError, PipelineRunRepository};
use uuid::Uuid;

/// Recall's warm-up settings.
#[derive(Debug, Clone, PartialEq)]
pub struct WarmupConfig {
    /// Short-circuit a recall whose graph is not ready.
    pub shortcircuit: bool,
    /// The datapoint count a graph needs to be warm; clamped to
    /// [`WARM_COUNT`], so it can never read a populated graph as cold.
    pub threshold: u64,
    /// How long a warm verdict is cached. Cold verdicts never are, so the
    /// first recall after a build sees it.
    pub cache_ttl: Duration,
}

impl Default for WarmupConfig {
    fn default() -> Self {
        Self {
            shortcircuit: true,
            threshold: 1,
            cache_ttl: Duration::from_secs(60),
        }
    }
}

/// How ready the target datasets' knowledge graph is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessState {
    Warm,
    NeverBuilt,
    BuildFailed,
}

impl ReadinessState {
    /// The state's wire name, which recall's marker also uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::NeverBuilt => "never_built",
            Self::BuildFailed => "build_failed",
        }
    }
}

/// The datapoint count a warm probe reports. The probe is binary, not a real
/// count, so a warm one satisfies any threshold up to this value.
pub const WARM_COUNT: u64 = 1 << 31;

/// The most verdicts cached at once.
const CACHE_MAX_ENTRIES: usize = 1024;

/// The probe's verdict on the target datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupProbe {
    pub state: ReadinessState,
    /// [`WARM_COUNT`] when warm, else 0.
    pub datapoint_count: u64,
    /// For `build_failed`: the error the latest build recorded.
    pub error_message: Option<String>,
}

impl WarmupProbe {
    pub fn warm() -> Self {
        Self {
            state: ReadinessState::Warm,
            datapoint_count: WARM_COUNT,
            error_message: None,
        }
    }

    pub fn never_built() -> Self {
        Self {
            state: ReadinessState::NeverBuilt,
            datapoint_count: 0,
            error_message: None,
        }
    }

    pub fn is_warm(&self) -> bool {
        self.state == ReadinessState::Warm
    }
}

/// Whose recall over which datasets a verdict answers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    requester: Uuid,
    tenant: Option<Uuid>,
    /// Sorted; `None` for every dataset the caller can read.
    dataset_ids: Option<Vec<Uuid>>,
}

impl CacheKey {
    pub fn new(requester: Uuid, tenant: Option<Uuid>, dataset_ids: Option<&[Uuid]>) -> Self {
        let dataset_ids = dataset_ids.map(|ids| {
            let mut ids = ids.to_vec();
            ids.sort_unstable();
            ids
        });
        Self {
            requester,
            tenant,
            dataset_ids,
        }
    }
}

/// The probe: reads `pipeline_runs` and caches warm verdicts.
pub struct GraphWarmup {
    pipeline_runs: Arc<dyn PipelineRunRepository>,
    config: WarmupConfig,
    cache: Mutex<HashMap<CacheKey, (WarmupProbe, Instant)>>,
}

impl GraphWarmup {
    pub fn new(pipeline_runs: Arc<dyn PipelineRunRepository>, config: WarmupConfig) -> Self {
        Self {
            pipeline_runs,
            config,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &WarmupConfig {
        &self.config
    }

    /// The cached verdict for `key`, while it has not expired.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn cached(&self, key: &CacheKey) -> Option<WarmupProbe> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(key)
            .filter(|(_, expires_at)| *expires_at > Instant::now())
            .map(|(probe, _)| probe.clone())
    }

    /// Cache `probe` for `key` when it is warm.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn remember(&self, key: CacheKey, probe: &WarmupProbe) {
        if !probe.is_warm() {
            return;
        }
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= CACHE_MAX_ENTRIES {
            cache.retain(|_, (_, expires_at)| *expires_at > now);
            if cache.len() >= CACHE_MAX_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(key, (probe.clone(), now + self.config.cache_ttl));
    }

    /// Probe the graph of `dataset_ids`, which the caller may read.
    pub async fn probe(&self, dataset_ids: &[Uuid]) -> Result<WarmupProbe, DatabaseError> {
        if dataset_ids.is_empty() {
            return Ok(WarmupProbe::never_built());
        }
        let evidence = self
            .pipeline_runs
            .graph_build_evidence(
                dataset_ids,
                &[ADD_PIPELINE_NAME],
                &CHANGE_RECORD_PIPELINE_NAMES,
            )
            .await?;
        let probe = classify(&evidence);
        if probe.is_warm() && probe.datapoint_count < self.config.threshold.min(WARM_COUNT) {
            return Ok(WarmupProbe::never_built());
        }
        Ok(probe)
    }
}

/// The verdict `pipeline_runs` gives: a built graph wins over a failed
/// build, and a failed build over the staging-only fail-safe.
pub fn classify(evidence: &GraphBuildEvidence) -> WarmupProbe {
    if evidence.graph_built {
        return WarmupProbe::warm();
    }
    if let Some(error_message) = &evidence.latest_build_error {
        return WarmupProbe {
            state: ReadinessState::BuildFailed,
            datapoint_count: 0,
            error_message: error_message.clone(),
        };
    }
    if evidence.any_run {
        return WarmupProbe::warm();
    }
    WarmupProbe::never_built()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_graph_wins_over_a_failed_build_over_the_fail_safe() {
        let built = GraphBuildEvidence {
            graph_built: true,
            latest_build_error: Some(Some("an older failure".to_string())),
            any_run: true,
        };
        assert_eq!(classify(&built), WarmupProbe::warm());

        let failed = GraphBuildEvidence {
            graph_built: false,
            latest_build_error: Some(Some("LLM unreachable".to_string())),
            any_run: true,
        };
        let probe = classify(&failed);
        assert_eq!(probe.state, ReadinessState::BuildFailed);
        assert_eq!(probe.error_message.as_deref(), Some("LLM unreachable"));

        let staged = GraphBuildEvidence {
            any_run: true,
            ..GraphBuildEvidence::default()
        };
        assert_eq!(classify(&staged), WarmupProbe::warm());

        assert_eq!(
            classify(&GraphBuildEvidence::default()),
            WarmupProbe::never_built()
        );
    }

    fn warmup(cache_ttl: Duration) -> GraphWarmup {
        let config = WarmupConfig {
            cache_ttl,
            ..WarmupConfig::default()
        };
        let pipeline_runs = Arc::new(cognee_database::pipelines::NoopPipelineRunRepository::new());
        GraphWarmup::new(pipeline_runs, config)
    }

    #[test]
    fn only_a_warm_verdict_is_cached_and_only_until_it_expires() {
        let key = CacheKey::new(Uuid::new_v4(), None, None);
        let cache = warmup(Duration::from_secs(60));
        cache.remember(key.clone(), &WarmupProbe::never_built());
        assert_eq!(cache.cached(&key), None);
        cache.remember(key.clone(), &WarmupProbe::warm());
        assert_eq!(cache.cached(&key), Some(WarmupProbe::warm()));

        let expired = warmup(Duration::ZERO);
        expired.remember(key.clone(), &WarmupProbe::warm());
        assert_eq!(expired.cached(&key), None);
    }

    #[test]
    fn a_cache_key_ignores_dataset_order_but_not_the_tenant() {
        let (requester, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let forward = CacheKey::new(requester, None, Some(&[a, b]));
        let backward = CacheKey::new(requester, None, Some(&[b, a]));
        assert_eq!(forward, backward);
        let other_tenant = CacheKey::new(requester, Some(Uuid::new_v4()), Some(&[a, b]));
        assert_ne!(forward, other_tenant);
    }
}
