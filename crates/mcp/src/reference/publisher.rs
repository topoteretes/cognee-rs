use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::engine::{
    ReferenceEngineFactory, ReferenceEngineIdentity, ReferenceEngineInput, ReferenceEngineOpen,
    ReferenceProviderFingerprint, ReferenceRecallProbe,
};
use super::record::{REFERENCE_SCHEMA_VERSION, sha256_bytes};
use super::{DeltaStore, ReferenceConfig, ReferenceError, ReferenceLayout, ReferenceRecord};
#[cfg(feature = "runtime")]
use crate::atomic_fs::SystemSyncOps;
use crate::atomic_fs::{ReplaceMode, SyncOps, write_atomic, write_atomic_with_permissions};

const JSON_FILE_LIMIT: u64 = 16 * 1024 * 1024;
static PUBLISH_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifestEntry {
    pub source_id: String,
    pub source_label: String,
    pub revision: u64,
    pub event_id: String,
    pub content_type: String,
    pub content_sha256: String,
}

impl From<&ReferenceRecord> for SourceManifestEntry {
    fn from(record: &ReferenceRecord) -> Self {
        Self {
            source_id: record.source_id.clone(),
            source_label: record.source_label.clone(),
            revision: record.revision,
            event_id: record.event_id.clone(),
            content_type: record.content_type.clone(),
            content_sha256: record.content_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifestEntry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeManifest {
    pub query_sha256: String,
    pub expected_event_id: String,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub schema_version: u32,
    pub generation_id: String,
    pub dataset: String,
    pub included_through: u64,
    pub sources: Vec<SourceManifestEntry>,
    pub cognee_rs_commit: String,
    pub adapter_version: String,
    pub llm: ReferenceProviderFingerprint,
    pub embedding: ReferenceProviderFingerprint,
    pub files: Vec<FileManifestEntry>,
    pub created_at: String,
    pub probe: ProbeManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentPointer {
    pub schema_version: u32,
    pub generation_id: String,
    pub included_through: u64,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BuilderManifest {
    schema_version: u32,
    dataset: String,
    included_through: u64,
    sources: Vec<SourceManifestEntry>,
    #[serde(default = "dirty_builder_manifest")]
    dirty: bool,
    #[serde(default)]
    cognee_rs_commit: String,
    #[serde(default)]
    adapter_version: String,
    llm: ReferenceProviderFingerprint,
    embedding: ReferenceProviderFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishReceipt {
    pub generation_id: String,
    pub included_through: u64,
    pub source_count: usize,
    pub rebuilt: bool,
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishRunReport {
    pub publications: usize,
    pub included_through: u64,
    pub committed_head: u64,
    pub caught_up: bool,
    pub delegated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedGenerationStatus {
    pub generation_id: String,
    pub included_through: u64,
    pub source_count: usize,
    pub file_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFaultPoint {
    AfterSnapshot,
    AfterWriterOpen,
    AfterIngest,
    AfterWriterClose,
    AfterBuilderManifest,
    AfterCopy,
    AfterManifest,
    AfterProbe,
    AfterSeal,
    AfterGenerationInstall,
    BeforePointerReplace,
}

impl PublishFaultPoint {
    pub const BEFORE_POINTER_REPLACEMENT: [Self; 11] = [
        Self::AfterSnapshot,
        Self::AfterWriterOpen,
        Self::AfterIngest,
        Self::AfterWriterClose,
        Self::AfterBuilderManifest,
        Self::AfterCopy,
        Self::AfterManifest,
        Self::AfterProbe,
        Self::AfterSeal,
        Self::AfterGenerationInstall,
        Self::BeforePointerReplace,
    ];
}

pub trait PublishHooks: Send + Sync {
    fn checkpoint(&self, point: PublishFaultPoint) -> Result<(), ReferenceError>;
}

#[derive(Debug, Clone, Copy, Default)]
#[cfg(feature = "runtime")]
struct SystemPublishHooks;

#[cfg(feature = "runtime")]
impl PublishHooks for SystemPublishHooks {
    fn checkpoint(&self, _point: PublishFaultPoint) -> Result<(), ReferenceError> {
        Ok(())
    }
}

pub struct ReferencePublisher {
    config: ReferenceConfig,
    factory: Arc<dyn ReferenceEngineFactory>,
    sync: Arc<dyn SyncOps>,
    hooks: Arc<dyn PublishHooks>,
    host: String,
}

impl ReferencePublisher {
    #[cfg(feature = "runtime")]
    pub fn new(
        config: ReferenceConfig,
        factory: Arc<dyn ReferenceEngineFactory>,
    ) -> Result<Self, ReferenceError> {
        Ok(Self::with_dependencies(
            config,
            factory,
            Arc::new(SystemSyncOps),
            Arc::new(SystemPublishHooks),
            local_hostname()?,
        ))
    }

    pub fn with_dependencies(
        config: ReferenceConfig,
        factory: Arc<dyn ReferenceEngineFactory>,
        sync: Arc<dyn SyncOps>,
        hooks: Arc<dyn PublishHooks>,
        host: String,
    ) -> Self {
        Self {
            config,
            factory,
            sync,
            hooks,
            host,
        }
    }

    pub async fn publish_once(&self) -> Result<PublishReceipt, ReferenceError> {
        let result = self.publish_once_inner().await;
        if !matches!(result, Err(ReferenceError::WriterBusy)) {
            self.write_status(&result);
        }
        result
    }

    async fn publish_once_inner(&self) -> Result<PublishReceipt, ReferenceError> {
        let store = DeltaStore::new(self.config.layout.clone(), self.config.limits);
        store.initialize()?;
        let snapshot = store.snapshot_after(0)?;
        let _lock = PublisherLock::acquire(
            &self.config.layout,
            snapshot.head.highest_committed_sequence,
            &self.host,
            Arc::clone(&self.sync),
        )?;
        self.hooks.checkpoint(PublishFaultPoint::AfterSnapshot)?;

        let latest = latest_sources(&snapshot.records);
        if snapshot.head.highest_committed_sequence == 0 {
            return Ok(PublishReceipt {
                generation_id: String::new(),
                included_through: 0,
                source_count: 0,
                rebuilt: false,
                published: false,
            });
        }
        let identity = self.factory.identity();
        if !has_known_build_fingerprint(&identity.cognee_rs_commit)
            || !has_known_build_fingerprint(&identity.adapter_version)
        {
            return Err(ReferenceError::Unavailable);
        }
        let mut rebuild_for_invalid_current = false;
        if let Some(current) = read_optional_json::<CurrentPointer>(&self.config.layout.current)?
            && current.included_through == snapshot.head.highest_committed_sequence
        {
            match validate_published_generation(&self.config, Some(&identity)) {
                Ok(validated) => {
                    return Ok(PublishReceipt {
                        generation_id: validated.generation_id,
                        included_through: validated.included_through,
                        source_count: validated.source_count,
                        rebuilt: false,
                        published: false,
                    });
                }
                Err(ReferenceError::CorruptRecord | ReferenceError::ModelMismatch) => {
                    rebuild_for_invalid_current = true;
                }
                Err(error) => return Err(error),
            }
        }

        let existing_builder = read_optional_json::<BuilderManifest>(
            &self.config.layout.builder.join("manifest.json"),
        )?;
        let rebuild = rebuild_for_invalid_current
            || builder_requires_rebuild(
                existing_builder.as_ref(),
                &snapshot.records,
                &identity,
                &self.config,
            );
        let (inputs, previous_sources) = if rebuild {
            reset_builder(&self.config.layout, self.sync.as_ref())?;
            (latest.values().cloned().collect::<Vec<_>>(), Vec::new())
        } else {
            let manifest = existing_builder
                .as_ref()
                .ok_or(ReferenceError::CorruptRecord)?;
            (
                snapshot
                    .records
                    .iter()
                    .filter(|record| record.sequence > manifest.included_through)
                    .cloned()
                    .collect::<Vec<_>>(),
                manifest.sources.clone(),
            )
        };

        let sources = merge_source_manifest(previous_sources, &inputs, rebuild, &latest);
        let mut builder_manifest = BuilderManifest {
            schema_version: REFERENCE_SCHEMA_VERSION,
            dataset: self.config.dataset.to_owned(),
            included_through: snapshot.head.highest_committed_sequence,
            sources: sources.clone(),
            dirty: !inputs.is_empty(),
            cognee_rs_commit: identity.cognee_rs_commit.clone(),
            adapter_version: identity.adapter_version.clone(),
            llm: identity.llm.clone(),
            embedding: identity.embedding.clone(),
        };
        if !inputs.is_empty() {
            write_private_json(
                &self.config.layout.builder.join("manifest.json"),
                &builder_manifest,
                ReplaceMode::Replace,
                self.sync.as_ref(),
            )?;
            self.ingest_builder(&identity, &inputs).await?;
            builder_manifest.dirty = false;
        }
        write_private_json(
            &self.config.layout.builder.join("manifest.json"),
            &builder_manifest,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        )?;
        self.hooks
            .checkpoint(PublishFaultPoint::AfterBuilderManifest)?;

        let generation_id = next_generation_id(snapshot.head.highest_committed_sequence);
        let stage = self.config.layout.staging.join(&generation_id);
        create_private_directory(&stage)?;
        copy_builder_tree(&self.config.layout.builder, &stage, self.sync.as_ref())?;
        self.hooks.checkpoint(PublishFaultPoint::AfterCopy)?;

        write_sources_catalog(&stage.join("sources.jsonl"), &sources, self.sync.as_ref())?;
        let files = generation_inventory(&stage, &["manifest.json"])?;
        let probe = make_probe(
            latest
                .values()
                .next()
                .ok_or(ReferenceError::CorruptRecord)?,
        );
        let manifest = GenerationManifest {
            schema_version: REFERENCE_SCHEMA_VERSION,
            generation_id: generation_id.clone(),
            dataset: self.config.dataset.to_owned(),
            included_through: snapshot.head.highest_committed_sequence,
            sources,
            cognee_rs_commit: identity.cognee_rs_commit.clone(),
            adapter_version: identity.adapter_version.clone(),
            llm: identity.llm.clone(),
            embedding: identity.embedding.clone(),
            files,
            created_at: Utc::now().to_rfc3339(),
            probe: ProbeManifest {
                query_sha256: sha256_bytes(probe.query.as_bytes()),
                expected_event_id: probe.expected_event_id.clone(),
                verified: true,
            },
        };
        write_private_json(
            &stage.join("manifest.json"),
            &manifest,
            ReplaceMode::NoReplace,
            self.sync.as_ref(),
        )?;
        self.hooks.checkpoint(PublishFaultPoint::AfterManifest)?;

        let before_probe = tree_fingerprint(&stage)?;
        self.verify_stage(&stage, &identity, &probe).await?;
        let after_probe = tree_fingerprint(&stage)?;
        if before_probe != after_probe {
            return Err(ReferenceError::ReadOnly);
        }
        self.hooks.checkpoint(PublishFaultPoint::AfterProbe)?;

        seal_tree(&stage)?;
        self.hooks.checkpoint(PublishFaultPoint::AfterSeal)?;
        let generation = self.config.layout.generations.join(&generation_id);
        durable_rename_no_replace(&stage, &generation, self.sync.as_ref())?;
        set_mode(&generation, 0o555)?;
        self.sync.sync_directory(&generation)?;
        self.sync.sync_directory(&self.config.layout.generations)?;
        self.hooks
            .checkpoint(PublishFaultPoint::AfterGenerationInstall)?;

        let manifest_sha256 = sha256_bytes(&fs::read(generation.join("manifest.json"))?);
        let current = CurrentPointer {
            schema_version: REFERENCE_SCHEMA_VERSION,
            generation_id: generation_id.clone(),
            included_through: snapshot.head.highest_committed_sequence,
            manifest_sha256,
        };
        self.hooks
            .checkpoint(PublishFaultPoint::BeforePointerReplace)?;
        write_public_json(
            &self.config.layout.current,
            &current,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        )?;

        Ok(PublishReceipt {
            generation_id,
            included_through: snapshot.head.highest_committed_sequence,
            source_count: latest.len(),
            rebuilt: rebuild,
            published: true,
        })
    }

    pub async fn publish_until_caught_up(
        &self,
        budget: Duration,
    ) -> Result<PublishRunReport, ReferenceError> {
        let started = Instant::now();
        let mut publications = 0_usize;
        loop {
            let receipt = match self.publish_once().await {
                Ok(receipt) => receipt,
                Err(ReferenceError::WriterBusy) => {
                    let (included_through, committed_head) = self.watermarks()?;
                    return Ok(PublishRunReport {
                        publications,
                        included_through,
                        committed_head,
                        caught_up: included_through >= committed_head,
                        delegated: true,
                    });
                }
                Err(error) => return Err(error),
            };
            if receipt.published {
                publications = publications.saturating_add(1);
            }
            let (included_through, committed_head) = self.watermarks()?;
            if included_through >= committed_head {
                return Ok(PublishRunReport {
                    publications,
                    included_through,
                    committed_head,
                    caught_up: true,
                    delegated: false,
                });
            }
            if started.elapsed() >= budget {
                return Ok(PublishRunReport {
                    publications,
                    included_through,
                    committed_head,
                    caught_up: false,
                    delegated: false,
                });
            }
        }
    }

    fn watermarks(&self) -> Result<(u64, u64), ReferenceError> {
        let snapshot =
            DeltaStore::new(self.config.layout.clone(), self.config.limits).snapshot_after(0)?;
        let included_through = read_optional_json::<CurrentPointer>(&self.config.layout.current)?
            .map_or(0, |current| current.included_through);
        Ok((included_through, snapshot.head.highest_committed_sequence))
    }

    fn write_status(&self, result: &Result<PublishReceipt, ReferenceError>) {
        let target_watermark =
            read_optional_json::<super::DeltaHead>(&self.config.layout.delta_head)
                .ok()
                .flatten()
                .filter(super::DeltaHead::verify_hash)
                .map_or(0, |head| head.highest_committed_sequence);
        let status = match result {
            Ok(receipt) => PublisherStatus {
                status: "ok",
                target_watermark,
                included_through: Some(receipt.included_through),
                generation_id: (!receipt.generation_id.is_empty())
                    .then(|| receipt.generation_id.clone()),
                error_class: None,
                updated_at: Utc::now().to_rfc3339(),
            },
            Err(error) => PublisherStatus {
                status: "error",
                target_watermark,
                included_through: None,
                generation_id: None,
                error_class: Some(error.class()),
                updated_at: Utc::now().to_rfc3339(),
            },
        };
        let _ = write_private_json(
            &self.config.layout.status.join("publisher.json"),
            &status,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        );
    }

    async fn ingest_builder(
        &self,
        identity: &ReferenceEngineIdentity,
        records: &[ReferenceRecord],
    ) -> Result<(), ReferenceError> {
        let request = ReferenceEngineOpen {
            root: self.config.layout.builder.clone(),
            dataset: self.config.dataset.to_owned(),
            read_only: false,
            user_agent: identity.user_agent.clone(),
        };
        let mut engine = self.factory.open_writer(&request).await?;
        if let Err(error) = self.hooks.checkpoint(PublishFaultPoint::AfterWriterOpen) {
            let _ = engine.close().await;
            return Err(error);
        }
        let result = engine
            .add_and_cognify(
                self.config.dataset,
                records.iter().map(engine_input).collect(),
            )
            .await;
        if let Err(error) = result {
            let _ = engine.close().await;
            return Err(error);
        }
        if let Err(error) = self.hooks.checkpoint(PublishFaultPoint::AfterIngest) {
            let _ = engine.close().await;
            return Err(error);
        }
        engine.close().await?;
        self.hooks.checkpoint(PublishFaultPoint::AfterWriterClose)?;
        Ok(())
    }

    async fn verify_stage(
        &self,
        stage: &Path,
        identity: &ReferenceEngineIdentity,
        probe: &ReferenceRecallProbe,
    ) -> Result<(), ReferenceError> {
        let request = ReferenceEngineOpen {
            root: stage.to_path_buf(),
            dataset: self.config.dataset.to_owned(),
            read_only: true,
            user_agent: identity.user_agent.clone(),
        };
        let mut engine = self.factory.open_reader(&request).await?;
        let result = engine.recall_contains(self.config.dataset, probe).await;
        let close = engine.close().await;
        let found = result?;
        close?;
        if !found {
            return Err(ReferenceError::CorruptRecord);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PublisherStatus {
    status: &'static str,
    target_watermark: u64,
    included_through: Option<u64>,
    generation_id: Option<String>,
    error_class: Option<&'static str>,
    updated_at: String,
}

pub fn validate_published_generation(
    config: &ReferenceConfig,
    expected_identity: Option<&ReferenceEngineIdentity>,
) -> Result<PublishedGenerationStatus, ReferenceError> {
    let current = read_optional_json::<CurrentPointer>(&config.layout.current)?
        .ok_or(ReferenceError::Unavailable)?;
    if current.schema_version != REFERENCE_SCHEMA_VERSION
        || !is_safe_component(&current.generation_id)
    {
        return Err(ReferenceError::CorruptRecord);
    }
    let generation = config.layout.generations.join(&current.generation_id);
    let metadata = fs::symlink_metadata(&generation)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReferenceError::CorruptRecord);
    }
    let manifest_path = generation.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    if sha256_bytes(&manifest_bytes) != current.manifest_sha256 {
        return Err(ReferenceError::CorruptRecord);
    }
    let manifest: GenerationManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| ReferenceError::CorruptRecord)?;
    if manifest.schema_version != REFERENCE_SCHEMA_VERSION
        || manifest.generation_id != current.generation_id
        || manifest.dataset != config.dataset
        || manifest.included_through != current.included_through
        || !has_known_build_fingerprint(&manifest.cognee_rs_commit)
        || !has_known_build_fingerprint(&manifest.adapter_version)
        || !manifest.probe.verified
    {
        return Err(ReferenceError::CorruptRecord);
    }
    if let Some(expected) = expected_identity
        && (manifest.cognee_rs_commit != expected.cognee_rs_commit
            || manifest.adapter_version != expected.adapter_version
            || manifest.llm != expected.llm
            || manifest.embedding != expected.embedding)
    {
        return Err(ReferenceError::ModelMismatch);
    }
    let inventory = generation_inventory(&generation, &["manifest.json"])?;
    if inventory != manifest.files {
        return Err(ReferenceError::CorruptRecord);
    }
    let source_catalog = read_source_catalog(&generation.join("sources.jsonl"))?;
    if source_catalog != manifest.sources {
        return Err(ReferenceError::CorruptRecord);
    }
    let unique_sources = manifest
        .sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>();
    if unique_sources.len() != manifest.sources.len() {
        return Err(ReferenceError::CorruptRecord);
    }
    validate_immutable_modes(&generation)?;
    Ok(PublishedGenerationStatus {
        generation_id: current.generation_id,
        included_through: current.included_through,
        source_count: manifest.sources.len(),
        file_count: manifest.files.len(),
    })
}

fn engine_input(record: &ReferenceRecord) -> ReferenceEngineInput {
    ReferenceEngineInput {
        content: record.content.clone(),
        label: record.source_label.clone(),
        external_metadata: BTreeMap::from([
            (
                "externalEventId".to_owned(),
                Value::String(record.event_id.clone()),
            ),
            (
                "cognee_external_event_id".to_owned(),
                Value::String(record.event_id.clone()),
            ),
            (
                "reference_source_id".to_owned(),
                Value::String(record.source_id.clone()),
            ),
            (
                "reference_revision".to_owned(),
                Value::from(record.revision),
            ),
            (
                "reference_label".to_owned(),
                Value::String(record.source_label.clone()),
            ),
            (
                "content_type".to_owned(),
                Value::String(record.content_type.clone()),
            ),
            (
                "content_sha256".to_owned(),
                Value::String(record.content_sha256.clone()),
            ),
        ]),
    }
}

fn latest_sources(records: &[ReferenceRecord]) -> BTreeMap<String, ReferenceRecord> {
    let mut latest = BTreeMap::new();
    for record in records {
        latest.insert(record.source_id.clone(), record.clone());
    }
    latest
}

fn builder_requires_rebuild(
    manifest: Option<&BuilderManifest>,
    records: &[ReferenceRecord],
    identity: &ReferenceEngineIdentity,
    config: &ReferenceConfig,
) -> bool {
    let Some(manifest) = manifest else {
        return true;
    };
    if manifest.schema_version != REFERENCE_SCHEMA_VERSION
        || manifest.dataset != config.dataset
        || manifest.dirty
        || manifest.cognee_rs_commit != identity.cognee_rs_commit
        || manifest.adapter_version != identity.adapter_version
        || manifest.llm != identity.llm
        || manifest.embedding != identity.embedding
        || !config.layout.builder.join("data").is_dir()
        || !config.layout.builder.join("vector").is_dir()
        || !config.layout.builder.join("graph").is_dir()
    {
        return true;
    }
    let latest_at_builder = latest_sources(
        &records
            .iter()
            .filter(|record| record.sequence <= manifest.included_through)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let expected = latest_at_builder
        .values()
        .map(SourceManifestEntry::from)
        .collect::<Vec<_>>();
    if expected != manifest.sources {
        return true;
    }
    let known_sources = manifest
        .sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>();
    records
        .iter()
        .filter(|record| record.sequence > manifest.included_through)
        .any(|record| {
            record.supersedes_event_id.is_some()
                || known_sources.contains(record.source_id.as_str())
        })
}

fn has_known_build_fingerprint(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.eq_ignore_ascii_case("unknown")
}

const fn dirty_builder_manifest() -> bool {
    true
}

fn merge_source_manifest(
    previous: Vec<SourceManifestEntry>,
    inputs: &[ReferenceRecord],
    rebuilt: bool,
    latest: &BTreeMap<String, ReferenceRecord>,
) -> Vec<SourceManifestEntry> {
    if rebuilt {
        return latest.values().map(SourceManifestEntry::from).collect();
    }
    let mut sources = previous
        .into_iter()
        .map(|source| (source.source_id.clone(), source))
        .collect::<BTreeMap<_, _>>();
    for record in inputs {
        sources.insert(record.source_id.clone(), SourceManifestEntry::from(record));
    }
    sources.into_values().collect()
}

fn reset_builder(layout: &ReferenceLayout, sync: &dyn SyncOps) -> Result<(), ReferenceError> {
    if layout.builder.exists() {
        let invalid = layout
            .staging
            .join(format!("invalid-builder-{}", unique_nonce()));
        sync.before_rename(&layout.builder, &invalid)?;
        fs::rename(&layout.builder, &invalid)?;
        sync.sync_directory(&layout.admin)?;
    }
    create_private_directory(&layout.builder)
}

fn copy_builder_tree(
    builder: &Path,
    stage: &Path,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let metadata = fs::symlink_metadata(builder)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReferenceError::CorruptRecord);
    }
    copy_directory_contents(builder, stage, true, sync)
}

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    builder_root: bool,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        if builder_root && entry.file_name() == "manifest.json" {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(ReferenceError::CorruptRecord);
        }
        if metadata.is_dir() {
            create_private_directory(&destination_path)?;
            copy_directory_contents(&source_path, &destination_path, false, sync)?;
            sync.sync_directory(&destination_path)?;
        } else if metadata.is_file() {
            reject_surprising_hard_link(&metadata)?;
            copy_regular_file(&source_path, &destination_path, sync)?;
        } else {
            return Err(ReferenceError::CorruptRecord);
        }
    }
    sync.sync_directory(destination)?;
    Ok(())
}

fn copy_regular_file(
    source: &Path,
    destination: &Path,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let mut source_file = File::open(source)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut destination_file = options.open(destination)?;
    io::copy(&mut source_file, &mut destination_file)?;
    destination_file.flush()?;
    sync.sync_file(&destination_file)?;
    Ok(())
}

fn reject_surprising_hard_link(metadata: &fs::Metadata) -> Result<(), ReferenceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(ReferenceError::CorruptRecord);
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

fn write_sources_catalog(
    path: &Path,
    sources: &[SourceManifestEntry],
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let mut bytes = Vec::new();
    for source in sources {
        serde_json::to_writer(&mut bytes, source).map_err(|_| ReferenceError::CorruptRecord)?;
        bytes.push(b'\n');
    }
    write_atomic(path, &bytes, ReplaceMode::NoReplace, sync)?;
    Ok(())
}

fn generation_inventory(
    root: &Path,
    excluded: &[&str],
) -> Result<Vec<FileManifestEntry>, ReferenceError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(ReferenceError::CorruptRecord);
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(ReferenceError::CorruptRecord);
            }
            reject_surprising_hard_link(&metadata)?;
            let relative = safe_relative_string(root, &path)?;
            if excluded.contains(&relative.as_str()) {
                continue;
            }
            files.push(FileManifestEntry {
                path: relative,
                bytes: metadata.len(),
                sha256: sha256_bytes(&fs::read(path)?),
            });
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    path: String,
    directory: bool,
    bytes: u64,
    modified_nanos: u128,
    accessed_nanos: u128,
    sha256: Option<String>,
}

fn tree_fingerprint(root: &Path) -> Result<Vec<TreeEntry>, ReferenceError> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(ReferenceError::CorruptRecord);
            }
            let directory = metadata.is_dir();
            if !directory && !metadata.is_file() {
                return Err(ReferenceError::CorruptRecord);
            }
            result.push(TreeEntry {
                path: safe_relative_string(root, &path)?,
                directory,
                bytes: metadata.len(),
                modified_nanos: 0,
                accessed_nanos: 0,
                sha256: if directory {
                    None
                } else {
                    Some(sha256_bytes(&fs::read(&path)?))
                },
            });
            if directory {
                pending.push(path);
            }
        }
    }
    // Reading directories and hashing files can update their access times.
    // Capture all metadata only after those fingerprint-internal reads finish,
    // so the before/after comparison measures the probe rather than itself.
    for entry in &mut result {
        let metadata = fs::symlink_metadata(root.join(&entry.path))?;
        entry.bytes = metadata.len();
        entry.modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        entry.accessed_nanos = metadata
            .accessed()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn safe_relative_string(root: &Path, path: &Path) -> Result<String, ReferenceError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ReferenceError::CorruptRecord)?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ReferenceError::CorruptRecord);
    }
    relative
        .to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or(ReferenceError::CorruptRecord)
}

fn read_source_catalog(path: &Path) -> Result<Vec<SourceManifestEntry>, ReferenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > JSON_FILE_LIMIT
    {
        return Err(ReferenceError::CorruptRecord);
    }
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ReferenceError::CorruptRecord)?;
    text.lines()
        .map(|line| {
            if line.is_empty() {
                return Err(ReferenceError::CorruptRecord);
            }
            serde_json::from_str(line).map_err(|_| ReferenceError::CorruptRecord)
        })
        .collect()
}

fn validate_immutable_modes(root: &Path) -> Result<(), ReferenceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let metadata = fs::symlink_metadata(&directory)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.permissions().mode() & 0o777 != 0o555
            {
                return Err(ReferenceError::CorruptRecord);
            }
            for entry in fs::read_dir(&directory)? {
                let path = entry?.path();
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() {
                    return Err(ReferenceError::CorruptRecord);
                }
                if metadata.is_dir() {
                    pending.push(path);
                } else if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o444 {
                    return Err(ReferenceError::CorruptRecord);
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn make_probe(record: &ReferenceRecord) -> ReferenceRecallProbe {
    let query = record.content.chars().take(256).collect::<String>();
    ReferenceRecallProbe {
        query,
        expected_event_id: record.event_id.clone(),
    }
}

fn seal_tree(root: &Path) -> Result<(), ReferenceError> {
    // macOS requires owner-write permission on the directory inode being
    // renamed. Seal every descendant here, retain the private 0700 staging
    // root through the rename, and seal that root at its generation path
    // before the public pointer is advanced.
    let mut directories = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(ReferenceError::CorruptRecord);
            }
            if metadata.is_dir() {
                directories.push(path.clone());
                pending.push(path);
            } else if metadata.is_file() {
                set_mode(&path, 0o444)?;
            } else {
                return Err(ReferenceError::CorruptRecord);
            }
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        set_mode(&directory, 0o555)?;
    }
    Ok(())
}

fn durable_rename_no_replace(
    source: &Path,
    destination: &Path,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    if destination.exists() {
        return Err(ReferenceError::CorruptRecord);
    }
    sync.before_rename(source, destination)?;
    fs::rename(source, destination)?;
    let parent = destination.parent().ok_or(ReferenceError::InvalidRoot)?;
    sync.sync_directory(parent)?;
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), ReferenceError> {
    fs::create_dir(path)?;
    set_mode(path, 0o700)
}

fn write_private_json<T: Serialize>(
    path: &Path,
    value: &T,
    replace: ReplaceMode,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ReferenceError::CorruptRecord)?;
    write_atomic(path, &bytes, replace, sync)?;
    Ok(())
}

fn write_public_json<T: Serialize>(
    path: &Path,
    value: &T,
    replace: ReplaceMode,
    sync: &dyn SyncOps,
) -> Result<(), ReferenceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ReferenceError::CorruptRecord)?;
    write_atomic_with_permissions(path, &bytes, replace, 0o755, 0o444, sync)?;
    Ok(())
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, ReferenceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ReferenceError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > JSON_FILE_LIMIT
    {
        return Err(ReferenceError::CorruptRecord);
    }
    serde_json::from_slice(&fs::read(path)?)
        .map(Some)
        .map_err(|_| ReferenceError::CorruptRecord)
}

fn next_generation_id(watermark: u64) -> String {
    format!("generation-{watermark:020}-{}", unique_nonce())
}

fn unique_nonce() -> String {
    let sequence = PUBLISH_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let digest = sha256_bytes(
        json!({"pid": std::process::id(), "nanos": nanos, "sequence": sequence})
            .to_string()
            .as_bytes(),
    );
    digest
        .strip_prefix("sha256:")
        .unwrap_or(&digest)
        .chars()
        .take(20)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishLockOwner {
    pub host: String,
    pub pid: u32,
    pub nonce: String,
    pub started_at: String,
    pub target_watermark: u64,
}

pub struct PublisherLock {
    path: PathBuf,
    nonce: String,
    sync: Arc<dyn SyncOps>,
}

impl PublisherLock {
    pub fn acquire(
        layout: &ReferenceLayout,
        target_watermark: u64,
        host: &str,
        sync: Arc<dyn SyncOps>,
    ) -> Result<Self, ReferenceError> {
        match fs::create_dir(&layout.publish_lock) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(ReferenceError::WriterBusy);
            }
            Err(error) => return Err(ReferenceError::Io(error)),
        }
        if let Err(error) = set_mode(&layout.publish_lock, 0o700) {
            cleanup_failed_lock(&layout.publish_lock, sync.as_ref());
            return Err(error);
        }
        if let Some(parent) = layout.publish_lock.parent()
            && let Err(error) = sync.sync_directory(parent)
        {
            cleanup_failed_lock(&layout.publish_lock, sync.as_ref());
            return Err(ReferenceError::Io(error));
        }
        let nonce = unique_nonce();
        let owner = PublishLockOwner {
            host: host.to_owned(),
            pid: std::process::id(),
            nonce: nonce.clone(),
            started_at: Utc::now().to_rfc3339(),
            target_watermark,
        };
        if let Err(error) = write_private_json(
            &layout.publish_lock.join("owner.json"),
            &owner,
            ReplaceMode::NoReplace,
            sync.as_ref(),
        ) {
            cleanup_failed_lock(&layout.publish_lock, sync.as_ref());
            return Err(error);
        }
        Ok(Self {
            path: layout.publish_lock.clone(),
            nonce,
            sync,
        })
    }
}

impl Drop for PublisherLock {
    fn drop(&mut self) {
        let owner = read_optional_json::<PublishLockOwner>(&self.path.join("owner.json"));
        if !matches!(owner, Ok(Some(owner)) if owner.nonce == self.nonce) {
            return;
        }
        let _ = fs::remove_file(self.path.join("owner.json"));
        let _ = fs::remove_dir(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = self.sync.sync_directory(parent);
        }
    }
}

pub fn recover_publish_lock(
    layout: &ReferenceLayout,
    local_host: &str,
    process_is_alive: impl Fn(u32) -> bool,
    sync: Arc<dyn SyncOps>,
) -> Result<bool, ReferenceError> {
    let Some(owner) =
        read_optional_json::<PublishLockOwner>(&layout.publish_lock.join("owner.json"))?
    else {
        if layout.publish_lock.exists() {
            return Err(ReferenceError::CorruptRecord);
        }
        return Ok(false);
    };
    if owner.host != local_host || process_is_alive(owner.pid) {
        return Err(ReferenceError::WriterBusy);
    }
    let current = read_optional_json::<PublishLockOwner>(&layout.publish_lock.join("owner.json"))?
        .ok_or(ReferenceError::CorruptRecord)?;
    if current.nonce != owner.nonce {
        return Err(ReferenceError::WriterBusy);
    }
    fs::remove_file(layout.publish_lock.join("owner.json"))?;
    fs::remove_dir(&layout.publish_lock)?;
    if let Some(parent) = layout.publish_lock.parent() {
        sync.sync_directory(parent)?;
    }
    Ok(true)
}

#[cfg(feature = "runtime")]
pub(crate) fn publish_lock_present(layout: &ReferenceLayout) -> Result<bool, ReferenceError> {
    let metadata = match fs::symlink_metadata(&layout.publish_lock) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ReferenceError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReferenceError::CorruptRecord);
    }
    read_optional_json::<PublishLockOwner>(&layout.publish_lock.join("owner.json"))?
        .ok_or(ReferenceError::CorruptRecord)?;
    Ok(true)
}

#[cfg(feature = "runtime")]
pub(crate) fn local_hostname() -> Result<String, ReferenceError> {
    #[cfg(unix)]
    {
        let mut bytes = [0_u8; 256];
        // SAFETY: `bytes` is writable for the advertised length and remains
        // alive for the complete libc call.
        if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
            return Err(ReferenceError::Io(io::Error::last_os_error()));
        }
        let length = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..length])
            .ok()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .ok_or(ReferenceError::Unavailable)
    }
    #[cfg(not(unix))]
    {
        std::env::var("HOSTNAME")
            .ok()
            .map(|host| host.trim().to_owned())
            .filter(|host| !host.is_empty())
            .ok_or(ReferenceError::Unavailable)
    }
}

#[cfg(feature = "runtime")]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: kill with signal 0 performs only existence/permission checking.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn cleanup_failed_lock(path: &Path, sync: &dyn SyncOps) {
    let _ = fs::remove_dir_all(path);
    if let Some(parent) = path.parent() {
        let _ = sync.sync_directory(parent);
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), ReferenceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}
