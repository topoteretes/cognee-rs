//! Built-in session-store factories: the relational database, and (behind
//! features) the filesystem and Redis.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_database::RelationalDb;
use cognee_session::{RelationalSessionStore, SessionStore};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::SessionStoreFactory;

/// Sessions in the relational database the other components share
/// (`session_qa_entries`, …). Registered as `relational` and `seaorm`.
pub struct RelationalSessionFactory {
    provider: &'static str,
}

impl RelationalSessionFactory {
    /// The factory under one store id.
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl SessionStoreFactory for RelationalSessionFactory {
    fn provider(&self) -> &str {
        self.provider
    }

    async fn build(
        &self,
        _ctx: &BackendBuildContext,
        database: Arc<dyn RelationalDb>,
    ) -> Result<Arc<dyn SessionStore>, ComponentError> {
        let store = RelationalSessionStore::new(database)
            .await
            .map_err(|e| ComponentError::SessionStore(e.to_string()))?;
        Ok(Arc::new(store))
    }
}

/// Sessions as JSON files under `session.fs.directory`.
#[cfg(feature = "session-fs")]
pub struct FsSessionFactory;

#[cfg(feature = "session-fs")]
#[async_trait]
impl SessionStoreFactory for FsSessionFactory {
    fn provider(&self) -> &str {
        "fs"
    }

    fn config_sections(&self) -> Vec<&'static cognee_config_core::SectionSchema> {
        vec![<crate::sections::FsSessionConfig as cognee_config_core::ConfigSection>::schema()]
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
        _database: Arc<dyn RelationalDb>,
    ) -> Result<Arc<dyn SessionStore>, ComponentError> {
        let configured = ctx.section::<crate::sections::FsSessionConfig>().directory;
        let directory = if configured.trim().is_empty() {
            ctx.system_root_directory.join("sessions")
        } else {
            std::path::PathBuf::from(configured)
        };
        Ok(Arc::new(cognee_session::FsSessionStore::new(directory)))
    }
}

/// Sessions in Redis, in the Python SDK's key layout.
#[cfg(feature = "session-redis")]
pub struct RedisSessionFactory;

#[cfg(feature = "session-redis")]
#[async_trait]
impl SessionStoreFactory for RedisSessionFactory {
    fn provider(&self) -> &str {
        "redis"
    }

    fn config_sections(&self) -> Vec<&'static cognee_config_core::SectionSchema> {
        vec![<crate::sections::RedisSessionConfig as cognee_config_core::ConfigSection>::schema()]
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
        _database: Arc<dyn RelationalDb>,
    ) -> Result<Arc<dyn SessionStore>, ComponentError> {
        let cfg = ctx.section::<crate::sections::RedisSessionConfig>();
        // Host and credentials go in as separate fields, never spliced into a
        // `redis://user:pass@host` URL: a password with `@`/`/`/`#`/`%` or a
        // bare IPv6 host would otherwise mis-parse.
        let store = cognee_session::RedisSessionStore::connect(
            &cfg.host,
            cfg.port,
            &cfg.username,
            &cfg.password,
        )
        .await
        .map_err(|e| ComponentError::SessionStore(e.to_string()))?;
        Ok(Arc::new(store))
    }
}
