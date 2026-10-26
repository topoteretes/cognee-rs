//! Built-in dataset database handlers for the embedded and in-memory
//! backends: `ladybug` / `kuzu` (graph), `lancedb` (vector) and `memory`.
//!
//! Python's `LadybugDatasetDatabaseHandler` and `LanceDBDatasetDatabaseHandler`
//! keep every dataset's store under `{system_root}/databases/{owner_id}/`, named
//! after the dataset: `{dataset_id}.lbug` (`.pkl` for `kuzu`) and
//! `{dataset_id}.lance.db`. These use the same layout, so a system root is
//! interchangeable between the SDKs.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cognee_database::RelationalDb;
use uuid::Uuid;

use crate::context::BackendBuildContext;
use crate::dataset_storage::{DatasetDatabaseHandler, HandlerKind, HandlerRecord, MEMORY_HANDLER};
use crate::error::ComponentError;

/// `{system_root}/databases/{owner_id}`: where an owner's embedded dataset
/// stores live.
fn owner_databases_dir(base: &BackendBuildContext, owner_id: Uuid) -> PathBuf {
    base.system_root_directory
        .join("databases")
        .join(owner_id.to_string())
}

/// Probe of the embedded handlers: `{system_root}/databases` exists (created
/// if missing, as the handlers create it on first use) and is a writable
/// directory. Writes no file: the check is the directory's metadata.
async fn probe_databases_dir(base: &BackendBuildContext) -> Result<String, ComponentError> {
    let dir = base.system_root_directory.join("databases");
    let shown = dir.display();
    let unusable = |why: String| {
        ComponentError::Storage(format!(
            "the embedded databases directory {shown} is unusable: {why}"
        ))
    };
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| unusable(e.to_string()))?;
    let meta = tokio::fs::metadata(&dir)
        .await
        .map_err(|e| unusable(e.to_string()))?;
    if !meta.is_dir() {
        return Err(unusable("not a directory".to_string()));
    }
    if meta.permissions().readonly() {
        return Err(unusable("read-only".to_string()));
    }
    Ok(format!("directory {shown}"))
}

async fn remove_path(path: &Path) -> Result<(), ComponentError> {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await?,
        Ok(_) => tokio::fs::remove_file(path).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// The embedded graph per dataset: one ladybug/kuzu database file per dataset
/// under its owner's databases directory.
pub struct LadybugDatasetHandler {
    name: &'static str,
}

impl LadybugDatasetHandler {
    /// Registered as `ladybug` and as `kuzu` (Python registers both names for
    /// the one handler).
    pub fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[async_trait]
impl DatasetDatabaseHandler for LadybugDatasetHandler {
    fn name(&self) -> &str {
        self.name
    }

    fn kind(&self) -> HandlerKind {
        HandlerKind::Graph
    }

    fn providers(&self) -> &[&'static str] {
        &["ladybug", "kuzu"]
    }

    async fn create_dataset(
        &self,
        base: &BackendBuildContext,
        dataset_id: Uuid,
        _owner_id: Uuid,
    ) -> Result<HandlerRecord, ComponentError> {
        let extension = if base.graph_provider == "kuzu" {
            "pkl"
        } else {
            "lbug"
        };
        Ok(HandlerRecord {
            provider: base.graph_provider.clone(),
            database_name: format!("{dataset_id}.{extension}"),
            ..HandlerRecord::default()
        })
    }

    fn dataset_context(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<BackendBuildContext, ComponentError> {
        let mut ctx = base.clone();
        ctx.graph_provider = record.provider.clone();
        ctx.graph_file_path = owner_databases_dir(base, owner_id)
            .join(&record.database_name)
            .to_string_lossy()
            .into_owned();
        Ok(ctx)
    }

    /// Removes the database file and the companions a crashed engine can
    /// leave behind, so a same-name recreate cannot replay stale data (Python
    /// removes `.lock` and `.wal` too).
    async fn delete_dataset(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<(), ComponentError> {
        let path = owner_databases_dir(base, owner_id).join(&record.database_name);
        remove_path(&path).await?;
        for suffix in ["wal", "lock"] {
            let mut companion = path.clone().into_os_string();
            companion.push(format!(".{suffix}"));
            remove_path(Path::new(&companion)).await?;
        }
        Ok(())
    }

    async fn probe(
        &self,
        base: &BackendBuildContext,
        _relational: &dyn RelationalDb,
    ) -> Result<String, ComponentError> {
        probe_databases_dir(base).await
    }
}

/// The embedded vector store per dataset: one LanceDB directory per dataset
/// under its owner's databases directory.
pub struct LanceDbDatasetHandler;

#[async_trait]
impl DatasetDatabaseHandler for LanceDbDatasetHandler {
    fn name(&self) -> &str {
        "lancedb"
    }

    fn kind(&self) -> HandlerKind {
        HandlerKind::Vector
    }

    fn providers(&self) -> &[&'static str] {
        &["lancedb"]
    }

    async fn create_dataset(
        &self,
        base: &BackendBuildContext,
        dataset_id: Uuid,
        owner_id: Uuid,
    ) -> Result<HandlerRecord, ComponentError> {
        let dir = owner_databases_dir(base, owner_id);
        tokio::fs::create_dir_all(&dir).await?;
        let name = format!("{dataset_id}.lance.db");
        Ok(HandlerRecord {
            provider: base.vector_provider.clone(),
            database_url: Some(dir.join(&name).to_string_lossy().into_owned()),
            database_name: name,
            ..HandlerRecord::default()
        })
    }

    fn dataset_context(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<BackendBuildContext, ComponentError> {
        let mut ctx = base.clone();
        ctx.vector_provider = record.provider.clone();
        ctx.vector_db_url = match record.database_url.as_deref() {
            Some(url) if !url.is_empty() => url.to_string(),
            _ => owner_databases_dir(base, owner_id)
                .join(&record.database_name)
                .to_string_lossy()
                .into_owned(),
        };
        Ok(ctx)
    }

    async fn delete_dataset(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<(), ComponentError> {
        let ctx = self.dataset_context(base, record, owner_id)?;
        remove_path(Path::new(&ctx.vector_db_url)).await
    }

    async fn probe(
        &self,
        base: &BackendBuildContext,
        _relational: &dyn RelationalDb,
    ) -> Result<String, ComponentError> {
        probe_databases_dir(base).await
    }
}

/// In-memory stores: each dataset's engine is a fresh one from the configured
/// in-memory factory, and nothing outlives the process. For tests and the
/// `mock` / `brute-force` providers.
pub struct MemoryDatasetHandler {
    kind: HandlerKind,
}

impl MemoryDatasetHandler {
    pub fn new(kind: HandlerKind) -> Self {
        Self { kind }
    }
}

#[async_trait]
impl DatasetDatabaseHandler for MemoryDatasetHandler {
    fn name(&self) -> &str {
        MEMORY_HANDLER
    }

    fn kind(&self) -> HandlerKind {
        self.kind
    }

    fn providers(&self) -> &[&'static str] {
        match self.kind {
            HandlerKind::Graph => &["mock"],
            HandlerKind::Vector => &["mock", "brute-force"],
        }
    }

    async fn create_dataset(
        &self,
        base: &BackendBuildContext,
        dataset_id: Uuid,
        _owner_id: Uuid,
    ) -> Result<HandlerRecord, ComponentError> {
        let provider = match self.kind {
            HandlerKind::Graph => base.graph_provider.clone(),
            HandlerKind::Vector => base.vector_provider.clone(),
        };
        Ok(HandlerRecord {
            provider,
            database_name: dataset_id.to_string(),
            ..HandlerRecord::default()
        })
    }

    fn dataset_context(
        &self,
        base: &BackendBuildContext,
        _record: &HandlerRecord,
        _owner_id: Uuid,
    ) -> Result<BackendBuildContext, ComponentError> {
        Ok(base.clone())
    }

    async fn delete_dataset(
        &self,
        _base: &BackendBuildContext,
        _record: &HandlerRecord,
        _owner_id: Uuid,
    ) -> Result<(), ComponentError> {
        Ok(())
    }

    async fn probe(
        &self,
        _base: &BackendBuildContext,
        _relational: &dyn RelationalDb,
    ) -> Result<String, ComponentError> {
        Ok("in-memory".to_string())
    }
}
