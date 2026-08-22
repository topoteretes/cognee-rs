//! Production assembly for the bounded, transient Cognee drain worker.

use std::sync::Arc;
use std::time::Duration;

use crate::config::{AgentConfig, EnvSource};
use crate::embedding_generation::{EmbeddingFingerprint, EmbeddingGeneration};
use crate::engine::CogneeEngineFactory;
use crate::error::AgentError;
use crate::lease::EngineLease;
use crate::ledger::Ledger;
use crate::spool::Spool;
use crate::worker::{DrainBudget, DrainReport, Worker};

pub fn run_drain_from_env(env: &impl EnvSource) -> Result<DrainReport, AgentError> {
    let config =
        AgentConfig::from_env(env).map_err(|_| AgentError::Blocked("configuration_drift"))?;
    run_drain(config)
}

pub fn run_drain(config: AgentConfig) -> Result<DrainReport, AgentError> {
    let embedding = config
        .embedding
        .as_ref()
        .ok_or(AgentError::Blocked("configuration_drift"))?;
    let generation_id = EmbeddingFingerprint::from_config(embedding).stable_id();
    let generation = EmbeddingGeneration::new(&config.layout, generation_id, embedding)
        .map_err(|_| AgentError::Blocked("configuration_drift"))?;
    let layout = config.layout.clone();
    let limits = config.limits.clone();
    let worker_budget = DrainBudget::from_limits(&limits);
    let spool = Spool::new(layout.clone(), limits.clone());
    let lease = EngineLease::new(
        layout.clone(),
        Duration::from_secs(u64::from(limits.lease_stale_seconds)),
    );
    let ledger = Ledger::open(layout.clone())?;
    let factory = Arc::new(CogneeEngineFactory::new(config, generation));
    let mut worker = Worker::new(layout, spool, lease, ledger, factory, limits);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| AgentError::Engine("runtime"))?;

    Ok(runtime.block_on(worker.drain(worker_budget)))
}
