#![cfg(feature = "runtime")]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cognee_mcp::atomic_fs::{SyncOps, SystemSyncOps};
use cognee_mcp::reference::{
    CurrentPointer, DeltaStore, GenerationManifest, PreparedDocument, PublishFaultPoint,
    PublishHooks, PublisherLock, ReferenceConfig, ReferenceEngineFactory, ReferenceEngineIdentity,
    ReferenceEngineInput, ReferenceEngineOpen, ReferenceError, ReferenceLimits,
    ReferenceProviderFingerprint, ReferenceReadEngine, ReferenceRecallProbe, ReferenceRecord,
    ReferenceWriteEngine, Source, recover_publish_lock, run_reference_doctor,
    validate_published_generation,
};
use sha2::{Digest, Sha256};

fn config(root: PathBuf) -> ReferenceConfig {
    ReferenceConfig {
        layout: cognee_mcp::reference::ReferenceLayout::under(root),
        dataset: "fleet_reference",
        limits: ReferenceLimits::default(),
    }
}

fn document(
    content: &str,
    logical_source_id: &str,
    label: &str,
    limits: &ReferenceLimits,
) -> PreparedDocument {
    PreparedDocument::from_bytes(
        Source::Stdin,
        content.as_bytes(),
        Some(logical_source_id),
        Some(label),
        limits,
    )
    .expect("reference document")
}

fn seed(config: &ReferenceConfig, content: &str, source_id: &str, label: &str) -> ReferenceRecord {
    let store = DeltaStore::new(config.layout.clone(), config.limits);
    store
        .commit_batch(&[document(content, source_id, label, &config.limits)])
        .expect("commit reference")
        .records
        .into_iter()
        .find(|record| record.content == content)
        .expect("committed record")
}

fn identity() -> ReferenceEngineIdentity {
    ReferenceEngineIdentity {
        cognee_rs_commit: "0123456789abcdef".to_owned(),
        adapter_version: "1.4.4".to_owned(),
        user_agent: format!(
            "Apex/{} ({}; {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        llm: ReferenceProviderFingerprint {
            provider: "openai".to_owned(),
            endpoint_class: "https://proxy.example".to_owned(),
            model: "gpt-5.4-mini".to_owned(),
            dimensions: None,
        },
        embedding: ReferenceProviderFingerprint {
            provider: "openai".to_owned(),
            endpoint_class: "https://proxy.example".to_owned(),
            model: "text-embedding-3-large".to_owned(),
            dimensions: Some(3072),
        },
    }
}

#[derive(Debug, Clone, Copy)]
enum UnsafeArtifact {
    Symlink,
    HardLink,
    Socket,
}

#[derive(Default)]
struct FakeState {
    opens: Mutex<Vec<ReferenceEngineOpen>>,
    batches: Mutex<Vec<Vec<ReferenceEngineInput>>>,
    probes: Mutex<Vec<ReferenceRecallProbe>>,
    on_ingest: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    mutate_during_probe: Mutex<bool>,
    mutate_access_time_during_probe: Mutex<bool>,
    unsafe_artifact: Mutex<Option<UnsafeArtifact>>,
}

#[derive(Clone)]
struct FakeFactory {
    state: Arc<FakeState>,
    identity: ReferenceEngineIdentity,
}

impl FakeFactory {
    fn new() -> Self {
        Self::with_identity(identity())
    }

    fn with_identity(identity: ReferenceEngineIdentity) -> Self {
        Self {
            state: Arc::new(FakeState::default()),
            identity,
        }
    }

    fn batches(&self) -> Vec<Vec<ReferenceEngineInput>> {
        self.state.batches.lock().expect("batches lock").clone()
    }

    fn opens(&self) -> Vec<ReferenceEngineOpen> {
        self.state.opens.lock().expect("opens lock").clone()
    }

    fn set_on_ingest(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self.state.on_ingest.lock().expect("ingest callback lock") = Some(Arc::new(callback));
    }

    fn mutate_during_probe(&self) {
        *self
            .state
            .mutate_during_probe
            .lock()
            .expect("probe mutation lock") = true;
    }

    fn mutate_access_time_during_probe(&self) {
        *self
            .state
            .mutate_access_time_during_probe
            .lock()
            .expect("probe access-time mutation lock") = true;
    }

    fn create_unsafe_artifact(&self, artifact: UnsafeArtifact) {
        *self
            .state
            .unsafe_artifact
            .lock()
            .expect("unsafe artifact lock") = Some(artifact);
    }
}

struct FakeWriter {
    root: PathBuf,
    state: Arc<FakeState>,
}

#[async_trait]
impl ReferenceWriteEngine for FakeWriter {
    async fn add_and_cognify(
        &mut self,
        _dataset: &str,
        inputs: Vec<ReferenceEngineInput>,
    ) -> Result<(), ReferenceError> {
        let data = self.root.join("data");
        let vector = self.root.join("vector");
        let graph = self.root.join("graph");
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&vector)?;
        std::fs::create_dir_all(&graph)?;
        std::fs::write(data.join("cognee.db"), b"writer-open")?;
        std::fs::write(vector.join("vectors.lance"), b"vector-bytes")?;
        std::fs::write(graph.join("graph.db"), b"graph-bytes")?;

        if let Some(artifact) = *self
            .state
            .unsafe_artifact
            .lock()
            .expect("unsafe artifact lock")
        {
            create_unsafe_artifact(&data, artifact)?;
        }

        self.state
            .batches
            .lock()
            .expect("batches lock")
            .push(inputs);
        if let Some(callback) = self
            .state
            .on_ingest
            .lock()
            .expect("ingest callback lock")
            .take()
        {
            callback();
        }
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        std::fs::write(self.root.join("data/cognee.db"), b"writer-closed")?;
        Ok(())
    }
}

struct FakeReader {
    root: PathBuf,
    state: Arc<FakeState>,
}

#[async_trait]
impl ReferenceReadEngine for FakeReader {
    async fn recall_contains(
        &mut self,
        _dataset: &str,
        probe: &ReferenceRecallProbe,
    ) -> Result<bool, ReferenceError> {
        self.state
            .probes
            .lock()
            .expect("probes lock")
            .push(probe.clone());
        if *self
            .state
            .mutate_during_probe
            .lock()
            .expect("probe mutation lock")
        {
            std::fs::write(self.root.join("data/probe-write"), b"unexpected")?;
        }
        if *self
            .state
            .mutate_access_time_during_probe
            .lock()
            .expect("probe access-time mutation lock")
        {
            let file = std::fs::File::open(self.root.join("data/cognee.db"))?;
            file.set_times(
                std::fs::FileTimes::new()
                    .set_accessed(std::time::SystemTime::now() + Duration::from_secs(86_400)),
            )?;
        }
        Ok(true)
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        Ok(())
    }
}

#[async_trait]
impl ReferenceEngineFactory for FakeFactory {
    fn identity(&self) -> ReferenceEngineIdentity {
        self.identity.clone()
    }

    async fn open_writer(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceWriteEngine>, ReferenceError> {
        self.state
            .opens
            .lock()
            .expect("opens lock")
            .push(request.clone());
        Ok(Box::new(FakeWriter {
            root: request.root.clone(),
            state: Arc::clone(&self.state),
        }))
    }

    async fn open_reader(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceReadEngine>, ReferenceError> {
        self.state
            .opens
            .lock()
            .expect("opens lock")
            .push(request.clone());
        Ok(Box::new(FakeReader {
            root: request.root.clone(),
            state: Arc::clone(&self.state),
        }))
    }
}

fn publisher(
    config: ReferenceConfig,
    factory: FakeFactory,
) -> cognee_mcp::reference::ReferencePublisher {
    cognee_mcp::reference::ReferencePublisher::with_dependencies(
        config,
        Arc::new(factory),
        Arc::new(SystemSyncOps),
        Arc::new(NoopHooks),
        "builder-host".to_owned(),
    )
}

#[derive(Debug)]
struct NoopHooks;

impl PublishHooks for NoopHooks {
    fn checkpoint(&self, _point: PublishFaultPoint) -> Result<(), ReferenceError> {
        Ok(())
    }
}

#[tokio::test]
async fn publisher_uses_one_head_snapshot_then_incrementally_applies_only_new_sources() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let first = seed(&config, "alpha fleet standard", "alpha", "alpha.md");
    let store = DeltaStore::new(config.layout.clone(), config.limits);
    let factory = FakeFactory::new();
    let callback_config = config.clone();
    factory.set_on_ingest(move || {
        seed(
            &callback_config,
            "bravo fleet standard",
            "bravo",
            "bravo.md",
        );
    });
    let publisher = publisher(config.clone(), factory.clone());

    let first_publish = publisher.publish_once().await.expect("first publication");

    assert_eq!(first_publish.included_through, first.sequence);
    assert_eq!(
        store
            .snapshot_after(0)
            .expect("new head")
            .head
            .highest_committed_sequence,
        2
    );
    assert_eq!(read_current(&config).included_through, first.sequence);
    assert_eq!(factory.batches().len(), 1);
    assert_eq!(factory.batches()[0].len(), 1);
    assert_eq!(
        factory.batches()[0][0].external_metadata,
        BTreeMap::from([
            (
                "externalEventId".to_owned(),
                serde_json::Value::String(first.event_id.clone()),
            ),
            (
                "cognee_external_event_id".to_owned(),
                serde_json::Value::String(first.event_id.clone()),
            ),
            (
                "reference_source_id".to_owned(),
                serde_json::Value::String(first.source_id.clone()),
            ),
            (
                "reference_revision".to_owned(),
                serde_json::Value::from(first.revision),
            ),
            (
                "reference_label".to_owned(),
                serde_json::Value::String(first.source_label.clone()),
            ),
            (
                "content_type".to_owned(),
                serde_json::Value::String(first.content_type.clone()),
            ),
            (
                "content_sha256".to_owned(),
                serde_json::Value::String(first.content_sha256.clone()),
            ),
        ])
    );

    let second_publish = publisher.publish_once().await.expect("second publication");

    assert!(!second_publish.rebuilt);
    assert_eq!(second_publish.included_through, 2);
    assert_eq!(factory.batches().len(), 2);
    assert_eq!(factory.batches()[1].len(), 1);
    assert_eq!(factory.batches()[1][0].label, "bravo.md");
}

#[tokio::test]
async fn bounded_worker_republishes_when_delta_advances_during_a_build() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "alpha fleet standard", "alpha", "alpha.md");
    let factory = FakeFactory::new();
    let callback_config = config.clone();
    factory.set_on_ingest(move || {
        seed(
            &callback_config,
            "bravo fleet standard",
            "bravo",
            "bravo.md",
        );
    });

    let report = publisher(config.clone(), factory.clone())
        .publish_until_caught_up(Duration::from_secs(5))
        .await
        .expect("bounded publisher run");

    assert!(report.caught_up);
    assert_eq!(report.publications, 2);
    assert_eq!(report.committed_head, 2);
    assert_eq!(report.included_through, 2);
    assert!(!report.delegated);
    assert_eq!(factory.batches().len(), 2);
}

#[tokio::test]
async fn bounded_worker_treats_a_publish_lock_race_as_delegated_success() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "pending standard", "pending", "pending.md");
    let sync: Arc<dyn SyncOps> = Arc::new(SystemSyncOps);
    let _owner = PublisherLock::acquire(&config.layout, 1, "other-worker", Arc::clone(&sync))
        .expect("existing publisher owner");

    let report = publisher(config, FakeFactory::new())
        .publish_until_caught_up(Duration::from_secs(1))
        .await
        .expect("lock race is delegated");

    assert!(report.delegated);
    assert!(!report.caught_up);
    assert_eq!(report.publications, 0);
    assert_eq!(report.committed_head, 1);
    assert_eq!(report.included_through, 0);
}

#[tokio::test]
async fn superseding_a_source_rebuilds_from_only_the_latest_source_catalog() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let first = seed(&config, "old standard", "standard", "standard.md");
    let factory = FakeFactory::new();
    let publisher = publisher(config.clone(), factory.clone());
    publisher.publish_once().await.expect("initial publication");
    let replacement = seed(&config, "new standard", "standard", "standard.md");

    let result = publisher
        .publish_once()
        .await
        .expect("replacement publication");

    assert!(result.rebuilt);
    let batches = factory.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[1].len(), 1);
    assert_eq!(batches[1][0].content, "new standard");
    assert_eq!(
        batches[1][0].external_metadata["reference_revision"],
        replacement.revision
    );
    assert_eq!(
        batches[1][0].external_metadata["cognee_external_event_id"],
        replacement.event_id
    );
    assert_ne!(first.event_id, replacement.event_id);
}

#[tokio::test]
async fn builder_manifest_build_fingerprints_force_full_rebuilds_when_they_change() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "alpha standard", "alpha", "alpha.md");
    publisher(config.clone(), FakeFactory::new())
        .publish_once()
        .await
        .expect("initial publication");
    let initial_manifest: serde_json::Value =
        read_json(&config.layout.builder.join("manifest.json"));
    assert_eq!(initial_manifest["cognee_rs_commit"], "0123456789abcdef");
    assert_eq!(initial_manifest["adapter_version"], "1.4.4");

    seed(&config, "bravo standard", "bravo", "bravo.md");
    let mut changed_commit = identity();
    changed_commit.cognee_rs_commit = "fedcba9876543210".to_owned();
    let commit_factory = FakeFactory::with_identity(changed_commit.clone());
    let commit_receipt = publisher(config.clone(), commit_factory.clone())
        .publish_once()
        .await
        .expect("commit-change publication");
    assert!(commit_receipt.rebuilt);
    assert_eq!(commit_factory.batches()[0].len(), 2);

    seed(&config, "charlie standard", "charlie", "charlie.md");
    changed_commit.adapter_version = "1.4.5".to_owned();
    let adapter_factory = FakeFactory::with_identity(changed_commit);
    let adapter_receipt = publisher(config, adapter_factory.clone())
        .publish_once()
        .await
        .expect("adapter-change publication");
    assert!(adapter_receipt.rebuilt);
    assert_eq!(adapter_factory.batches()[0].len(), 3);
}

#[tokio::test]
async fn publisher_rejects_missing_or_unknown_engine_commit_before_ingesting_nonempty_content() {
    for commit in ["", "unknown"] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = config(temporary.path().join("reference"));
        seed(&config, "identity required", "identity", "identity.md");
        let mut invalid = identity();
        invalid.cognee_rs_commit = commit.to_owned();
        let factory = FakeFactory::with_identity(invalid);

        let error = publisher(config.clone(), factory.clone())
            .publish_once()
            .await
            .expect_err("missing or unknown build identity must block publication");

        assert!(matches!(error, ReferenceError::Unavailable));
        assert!(factory.batches().is_empty());
        assert!(!config.layout.current.exists());
    }
}

#[tokio::test]
async fn interrupted_incremental_cognify_is_quarantined_and_rebuilt_on_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "alpha standard", "alpha", "alpha.md");
    let factory = FakeFactory::new();
    publisher(config.clone(), factory.clone())
        .publish_once()
        .await
        .expect("initial publication");
    seed(&config, "bravo standard", "bravo", "bravo.md");
    let faulted = cognee_mcp::reference::ReferencePublisher::with_dependencies(
        config.clone(),
        Arc::new(factory.clone()),
        Arc::new(SystemSyncOps),
        Arc::new(FailOnceAt {
            point: PublishFaultPoint::AfterIngest,
            fired: AtomicBool::new(false),
        }),
        "builder-host".to_owned(),
    );

    let error = faulted
        .publish_once()
        .await
        .expect_err("injected incremental failure");
    assert!(matches!(error, ReferenceError::Unavailable));
    let dirty_manifest: serde_json::Value = read_json(&config.layout.builder.join("manifest.json"));
    assert_eq!(dirty_manifest["dirty"], true);

    let retry = faulted.publish_once().await.expect("retry publication");

    assert!(retry.rebuilt);
    let batches = factory.batches();
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[1].len(), 1);
    assert_eq!(batches[2].len(), 2);
    let mut rebuilt_contents = batches[2]
        .iter()
        .map(|input| input.content.as_str())
        .collect::<Vec<_>>();
    rebuilt_contents.sort_unstable();
    assert_eq!(rebuilt_contents, ["alpha standard", "bravo standard"]);
    assert!(
        std::fs::read_dir(&config.layout.staging)
            .expect("staging directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("invalid-builder-"))
    );
}

#[tokio::test]
async fn published_generation_is_closed_complete_hashed_probed_and_immutable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let record = seed(
        &config,
        "known verification sentinel",
        "sentinel",
        "sentinel.md",
    );
    let factory = FakeFactory::new();
    let result = publisher(config.clone(), factory.clone())
        .publish_once()
        .await
        .expect("publication");
    let generation = config.layout.generations.join(&result.generation_id);

    assert_eq!(
        std::fs::read(generation.join("data/cognee.db")).expect("published database"),
        b"writer-closed"
    );
    let manifest: GenerationManifest = read_json(&generation.join("manifest.json"));
    assert_eq!(manifest.dataset, "fleet_reference");
    assert_eq!(manifest.included_through, record.sequence);
    assert_eq!(manifest.sources.len(), 1);
    assert_eq!(manifest.sources[0].event_id, record.event_id);
    assert_eq!(manifest.cognee_rs_commit, "0123456789abcdef");
    assert_eq!(manifest.adapter_version, "1.4.4");
    assert_eq!(manifest.llm.model, "gpt-5.4-mini");
    assert_eq!(manifest.embedding.dimensions, Some(3072));

    let inventory = inventory(&generation, &["manifest.json"]);
    let declared = manifest
        .files
        .iter()
        .map(|entry| (entry.path.clone(), entry.bytes, entry.sha256.clone()))
        .collect::<Vec<_>>();
    assert_eq!(declared, inventory);
    assert!(declared.iter().any(|entry| entry.0 == "sources.jsonl"));
    assert_eq!(
        std::fs::read_to_string(generation.join("sources.jsonl"))
            .expect("source catalog")
            .lines()
            .count(),
        1
    );

    let opens = factory.opens();
    assert_eq!(opens.len(), 2);
    assert!(!opens[0].read_only);
    assert!(opens[1].read_only);
    assert!(opens.iter().all(|open| open.dataset == "fleet_reference"));
    assert!(
        opens
            .iter()
            .all(|open| open.user_agent.starts_with("Apex/"))
    );
    assert_eq!(factory.state.probes.lock().expect("probes lock").len(), 1);
    assert_tree_modes(&generation, 0o555, 0o444);
    assert_eq!(mode(&config.layout.current), 0o444);

    let current = read_current(&config);
    assert_eq!(current.generation_id, result.generation_id);
    assert_eq!(current.included_through, record.sequence);
    assert_eq!(
        current.manifest_sha256,
        hash_file(&generation.join("manifest.json"))
    );
}

#[tokio::test]
async fn generation_validation_checks_inventory_catalog_and_model_fingerprint() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "validated reference", "validated", "validated.md");
    let factory = FakeFactory::new();
    publisher(config.clone(), factory.clone())
        .publish_once()
        .await
        .expect("publication");

    let status = validate_published_generation(&config, Some(&identity()))
        .expect("valid generation diagnostics");

    assert_eq!(status.included_through, 1);
    assert_eq!(status.source_count, 1);
    assert_eq!(status.file_count, 4);
    let doctor = run_reference_doctor(&config).expect("full reference diagnostics");
    assert_eq!(
        doctor.generation_id.as_deref(),
        Some(status.generation_id.as_str())
    );
    assert_eq!(doctor.included_through, 1);
    assert_eq!(doctor.source_count, 1);
    assert_eq!(doctor.generation_files, 4);
    assert!(!doctor.publisher_locked);

    let mut mismatch = identity();
    mismatch.cognee_rs_commit = "fedcba9876543210".to_owned();
    assert!(matches!(
        validate_published_generation(&config, Some(&mismatch)),
        Err(ReferenceError::ModelMismatch)
    ));

    let mut mismatch = identity();
    mismatch.adapter_version = "1.4.5".to_owned();
    assert!(matches!(
        validate_published_generation(&config, Some(&mismatch)),
        Err(ReferenceError::ModelMismatch)
    ));

    let mut mismatch = identity();
    mismatch.embedding.model = "different-embedding".to_owned();
    assert!(matches!(
        validate_published_generation(&config, Some(&mismatch)),
        Err(ReferenceError::ModelMismatch)
    ));

    let current = read_current(&config);
    let artifact = config
        .layout
        .generations
        .join(current.generation_id)
        .join("vector/vectors.lance");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o644))
            .expect("make artifact writable for corruption fixture");
    }
    std::fs::write(&artifact, b"tampered-vector").expect("tamper artifact");
    #[cfg(unix)]
    std::fs::set_permissions(
        &artifact,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o444),
    )
    .expect("restore artifact mode");

    assert!(matches!(
        validate_published_generation(&config, Some(&identity())),
        Err(ReferenceError::CorruptRecord)
    ));
}

#[tokio::test]
async fn caught_up_fast_path_rebuilds_and_republishes_a_corrupt_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "validated reference", "validated", "validated.md");
    let factory = FakeFactory::new();
    let initial = publisher(config.clone(), factory.clone())
        .publish_once()
        .await
        .expect("initial publication");
    let artifact = config
        .layout
        .generations
        .join(&initial.generation_id)
        .join("vector/vectors.lance");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o644))
            .expect("make artifact writable for corruption fixture");
    }
    std::fs::write(&artifact, b"corrupt-vector").expect("corrupt current generation");

    let replacement = publisher(config.clone(), factory)
        .publish_once()
        .await
        .expect("replacement publication");

    assert!(replacement.published);
    assert!(replacement.rebuilt);
    assert_ne!(replacement.generation_id, initial.generation_id);
    validate_published_generation(&config, Some(&identity()))
        .expect("replacement generation validates");
}

#[tokio::test]
async fn caught_up_fast_path_rebuilds_for_a_changed_model_fingerprint() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "model-bound reference", "model", "model.md");
    let initial_factory = FakeFactory::new();
    publisher(config.clone(), initial_factory)
        .publish_once()
        .await
        .expect("initial publication");
    let mut changed_identity = identity();
    changed_identity.llm.model = "gpt-5.4".to_owned();
    let changed_factory = FakeFactory::with_identity(changed_identity.clone());

    let replacement = publisher(config.clone(), changed_factory.clone())
        .publish_once()
        .await
        .expect("model-change publication");

    assert!(replacement.published);
    assert!(replacement.rebuilt);
    assert_eq!(changed_factory.batches()[0].len(), 1);
    validate_published_generation(&config, Some(&changed_identity))
        .expect("replacement model fingerprint validates");
}

#[tokio::test]
async fn caught_up_fast_path_preserves_generation_io_failures() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "missing generation", "missing", "missing.md");
    let factory = FakeFactory::new();
    publisher(config.clone(), factory.clone())
        .publish_once()
        .await
        .expect("initial publication");
    let mut current = read_current(&config);
    current.generation_id = "missing-generation".to_owned();
    replace_current(&config, &current);

    let error = publisher(config, factory)
        .publish_once()
        .await
        .expect_err("missing generation is an I/O failure");

    assert!(matches!(error, ReferenceError::Io(_)));
}

#[tokio::test]
async fn staged_read_only_probe_must_not_change_the_generation_tree() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "immutable sentinel", "sentinel", "sentinel.md");
    let factory = FakeFactory::new();
    factory.mutate_during_probe();

    let error = publisher(config.clone(), factory)
        .publish_once()
        .await
        .expect_err("mutating reader must block publication");

    assert!(matches!(error, ReferenceError::ReadOnly));
    assert!(!config.layout.current.exists());
    assert!(
        std::fs::read_dir(&config.layout.generations)
            .expect("generation directory")
            .next()
            .is_none()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn staged_read_only_probe_must_not_change_file_access_times() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(&config, "access-time sentinel", "sentinel", "sentinel.md");
    let factory = FakeFactory::new();
    factory.mutate_access_time_during_probe();

    let error = publisher(config.clone(), factory)
        .publish_once()
        .await
        .expect_err("access-time mutation must block publication");

    assert!(matches!(error, ReferenceError::ReadOnly));
    assert!(!config.layout.current.exists());
}

#[tokio::test]
#[cfg(unix)]
async fn publisher_rejects_symlinks_hard_links_and_sockets_from_the_builder() {
    for artifact in [
        UnsafeArtifact::Symlink,
        UnsafeArtifact::HardLink,
        UnsafeArtifact::Socket,
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = config(temporary.path().join("reference"));
        seed(&config, "unsafe fixture", "unsafe", "unsafe.md");
        let factory = FakeFactory::new();
        factory.create_unsafe_artifact(artifact);

        let error = publisher(config.clone(), factory)
            .publish_once()
            .await
            .expect_err("unsafe entry must block publication");

        assert!(matches!(error, ReferenceError::CorruptRecord));
        assert!(!config.layout.current.exists());
    }
}

#[derive(Debug)]
struct FailAt {
    point: PublishFaultPoint,
}

#[derive(Debug)]
struct FailOnceAt {
    point: PublishFaultPoint,
    fired: AtomicBool,
}

impl PublishHooks for FailOnceAt {
    fn checkpoint(&self, point: PublishFaultPoint) -> Result<(), ReferenceError> {
        if point == self.point && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(ReferenceError::Unavailable);
        }
        Ok(())
    }
}

impl PublishHooks for FailAt {
    fn checkpoint(&self, point: PublishFaultPoint) -> Result<(), ReferenceError> {
        if point == self.point {
            return Err(ReferenceError::Unavailable);
        }
        Ok(())
    }
}

#[tokio::test]
async fn every_publish_failure_before_pointer_replacement_preserves_the_old_pointer() {
    for point in PublishFaultPoint::BEFORE_POINTER_REPLACEMENT {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = config(temporary.path().join("reference"));
        seed(&config, "first generation", "first", "first.md");
        let factory = FakeFactory::new();
        publisher(config.clone(), factory.clone())
            .publish_once()
            .await
            .expect("initial publication");
        seed(&config, "second generation", "second", "second.md");
        let old_pointer = std::fs::read(&config.layout.current).expect("old pointer");
        let faulted = cognee_mcp::reference::ReferencePublisher::with_dependencies(
            config.clone(),
            Arc::new(factory),
            Arc::new(SystemSyncOps),
            Arc::new(FailAt { point }),
            "builder-host".to_owned(),
        );

        let error = faulted
            .publish_once()
            .await
            .expect_err("injected publication failure");

        assert!(matches!(error, ReferenceError::Unavailable), "{point:?}");
        assert_eq!(
            std::fs::read(&config.layout.current).expect("unchanged pointer"),
            old_pointer,
            "{point:?} replaced current.json before completing"
        );
    }
}

#[tokio::test]
async fn publisher_failure_writes_only_a_secret_free_status_class() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    seed(
        &config,
        "operator-secret-content-that-must-not-leak",
        "secret-source",
        "secret.md",
    );
    let faulted = cognee_mcp::reference::ReferencePublisher::with_dependencies(
        config.clone(),
        Arc::new(FakeFactory::new()),
        Arc::new(SystemSyncOps),
        Arc::new(FailAt {
            point: PublishFaultPoint::AfterCopy,
        }),
        "builder-host".to_owned(),
    );

    faulted
        .publish_once()
        .await
        .expect_err("injected publication failure");

    let status = std::fs::read_to_string(config.layout.status.join("publisher.json"))
        .expect("publisher status");
    let parsed: serde_json::Value = serde_json::from_str(&status).expect("publisher status JSON");
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error_class"], "REFERENCE_UNAVAILABLE");
    assert_eq!(parsed["target_watermark"], 1);
    assert!(!status.contains("operator-secret-content"));
}

#[test]
fn publisher_lock_is_exclusive_and_release_is_nonce_fenced() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    config
        .layout
        .ensure_admin_tree()
        .expect("administrator tree");
    let sync: Arc<dyn SyncOps> = Arc::new(SystemSyncOps);
    let lock = PublisherLock::acquire(&config.layout, 7, "builder-host", Arc::clone(&sync))
        .expect("publisher lock");

    assert!(matches!(
        PublisherLock::acquire(&config.layout, 7, "builder-host", Arc::clone(&sync)),
        Err(ReferenceError::WriterBusy)
    ));

    let owner_path = config.layout.publish_lock.join("owner.json");
    let mut owner: serde_json::Value = read_json(&owner_path);
    owner["nonce"] = serde_json::Value::String("replacement-owner".to_owned());
    std::fs::write(
        &owner_path,
        serde_json::to_vec(&owner).expect("replacement owner JSON"),
    )
    .expect("replace owner nonce");
    drop(lock);

    assert!(config.layout.publish_lock.exists());
}

#[test]
fn stale_publish_lock_requires_explicit_same_host_recovery() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    config
        .layout
        .ensure_admin_tree()
        .expect("administrator tree");
    let sync: Arc<dyn SyncOps> = Arc::new(SystemSyncOps);
    let lock = PublisherLock::acquire(&config.layout, 9, "builder-host", Arc::clone(&sync))
        .expect("publisher lock");
    std::mem::forget(lock);

    assert!(matches!(
        PublisherLock::acquire(&config.layout, 9, "builder-host", Arc::clone(&sync)),
        Err(ReferenceError::WriterBusy)
    ));
    assert!(
        recover_publish_lock(&config.layout, "builder-host", |_| false, Arc::clone(&sync),)
            .expect("same-host recovery")
    );
    assert!(!config.layout.publish_lock.exists());
}

#[test]
fn recovery_never_reclaims_a_live_or_cross_host_publish_lock() {
    for (owner_host, owner_alive) in [("remote-host", false), ("builder-host", true)] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = config(temporary.path().join("reference"));
        config
            .layout
            .ensure_admin_tree()
            .expect("administrator tree");
        let sync: Arc<dyn SyncOps> = Arc::new(SystemSyncOps);
        let lock = PublisherLock::acquire(&config.layout, 11, owner_host, Arc::clone(&sync))
            .expect("publisher lock");
        std::mem::forget(lock);

        assert!(matches!(
            recover_publish_lock(
                &config.layout,
                "builder-host",
                |_| owner_alive,
                Arc::clone(&sync),
            ),
            Err(ReferenceError::WriterBusy)
        ));
        assert!(config.layout.publish_lock.exists());
    }
}

fn read_current(config: &ReferenceConfig) -> CurrentPointer {
    read_json(&config.layout.current)
}

fn replace_current(config: &ReferenceConfig, current: &CurrentPointer) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &config.layout.current,
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("make current pointer writable");
    }
    std::fs::write(
        &config.layout.current,
        serde_json::to_vec(current).expect("serialize current pointer"),
    )
    .expect("replace current pointer fixture");
    #[cfg(unix)]
    std::fs::set_permissions(
        &config.layout.current,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o444),
    )
    .expect("restore current pointer mode");
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    serde_json::from_slice(&std::fs::read(path).expect("read JSON file")).expect("parse JSON file")
}

fn inventory(root: &Path, excluded: &[&str]) -> Vec<(String, u64, String)> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(&directory)
            .expect("inventory directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("inventory entries");
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).expect("inventory metadata");
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("relative inventory path")
                .to_string_lossy()
                .replace('\\', "/");
            if excluded.contains(&relative.as_str()) {
                continue;
            }
            result.push((relative, metadata.len(), hash_file(&path)));
        }
    }
    result.sort();
    result
}

fn hash_file(path: &Path) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(std::fs::read(path).expect("hash file"))
    )
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .expect("mode metadata")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(not(unix))]
fn mode(_path: &Path) -> u32 {
    0
}

#[cfg(unix)]
fn assert_tree_modes(root: &Path, directory_mode: u32, file_mode: u32) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        assert_eq!(mode(&directory), directory_mode, "{}", directory.display());
        for entry in std::fs::read_dir(&directory).expect("mode directory") {
            let path = entry.expect("mode entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                assert_eq!(mode(&path), file_mode, "{}", path.display());
            }
        }
    }
}

#[cfg(not(unix))]
fn assert_tree_modes(_root: &Path, _directory_mode: u32, _file_mode: u32) {}

#[cfg(unix)]
fn create_unsafe_artifact(directory: &Path, artifact: UnsafeArtifact) -> io::Result<()> {
    match artifact {
        UnsafeArtifact::Symlink => {
            std::os::unix::fs::symlink("cognee.db", directory.join("unsafe-link"))?;
        }
        UnsafeArtifact::HardLink => {
            std::fs::hard_link(
                directory.join("cognee.db"),
                directory.join("unsafe-hard-link"),
            )?;
        }
        UnsafeArtifact::Socket => {
            let listener = std::os::unix::net::UnixListener::bind(directory.join("unsafe.sock"))?;
            drop(listener);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_unsafe_artifact(_directory: &Path, _artifact: UnsafeArtifact) -> io::Result<()> {
    Ok(())
}

#[test]
#[cfg(feature = "engine")]
fn concrete_reference_settings_include_publishable_build_and_provider_identity() {
    use std::collections::HashMap;

    use cognee_mcp::config::{AgentConfig, EnvSource};
    use cognee_mcp::reference::CogneeReferenceEngineFactory;

    struct FakeEnv(HashMap<String, String>);

    impl EnvSource for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    let temporary = tempfile::tempdir().expect("temporary directory");
    let env = FakeEnv(HashMap::from([
        ("HOME".to_owned(), temporary.path().display().to_string()),
        ("APEX_COGNEE_PROXY_KEY".to_owned(), "fixture-key".to_owned()),
        ("APEX_COGNEE_LLM_PROVIDER".to_owned(), "openai".to_owned()),
        (
            "APEX_COGNEE_LLM_ENDPOINT".to_owned(),
            "https://proxy.example/v1".to_owned(),
        ),
        (
            "APEX_COGNEE_LLM_MODEL".to_owned(),
            "gpt-5.4-mini".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_PROVIDER".to_owned(),
            "openai".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT".to_owned(),
            "https://proxy.example/v1".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_MODEL".to_owned(),
            "text-embedding-3-large".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_DIMENSIONS".to_owned(),
            "3072".to_owned(),
        ),
    ]));
    let agent = AgentConfig::from_env(&env).expect("agent config");
    let factory = CogneeReferenceEngineFactory::new(agent).expect("reference engine factory");
    let identity = factory.identity();
    assert!(
        matches!(identity.cognee_rs_commit.len(), 40 | 64)
            && identity
                .cognee_rs_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "repository builds must embed an exact Git commit, got {:?}",
        identity.cognee_rs_commit
    );
    let open = ReferenceEngineOpen {
        root: temporary.path().join("generation"),
        dataset: "fleet_reference".to_owned(),
        read_only: true,
        user_agent: identity.user_agent,
    };

    let settings = factory.settings_for(&open).expect("reference settings");
    let backend = settings.backend_context();

    assert!(settings.read_only);
    assert_eq!(settings.default_dataset_name, "fleet_reference");
    assert_eq!(
        backend.llm.user_agent.as_deref(),
        Some(open.user_agent.as_str())
    );
    assert_eq!(
        backend.embedding.user_agent.as_deref(),
        Some(open.user_agent.as_str())
    );
    assert!(
        settings
            .relational_db_url
            .contains("generation/data/cognee.db")
    );
    assert!(settings.vector_db_url.ends_with("generation/vector"));
    assert!(settings.graph_file_path.ends_with("generation/graph"));
}

#[test]
#[cfg(feature = "engine")]
fn reference_factory_rejects_any_noncanonical_embedding_contract() {
    use std::collections::HashMap;

    use cognee_mcp::config::{AgentConfig, EnvSource};
    use cognee_mcp::reference::CogneeReferenceEngineFactory;

    struct FakeEnv(HashMap<String, String>);

    impl EnvSource for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    for (model, dimensions) in [
        ("text-embedding-3-small", "3072"),
        ("text-embedding-3-large", "1536"),
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let env = FakeEnv(HashMap::from([
            ("HOME".to_owned(), temporary.path().display().to_string()),
            ("APEX_COGNEE_PROXY_KEY".to_owned(), "fixture-key".to_owned()),
            ("APEX_COGNEE_LLM_PROVIDER".to_owned(), "openai".to_owned()),
            (
                "APEX_COGNEE_LLM_ENDPOINT".to_owned(),
                "https://proxy.example/v1".to_owned(),
            ),
            (
                "APEX_COGNEE_LLM_MODEL".to_owned(),
                "gpt-5.4-mini".to_owned(),
            ),
            (
                "APEX_COGNEE_EMBEDDING_PROVIDER".to_owned(),
                "openai".to_owned(),
            ),
            (
                "APEX_COGNEE_EMBEDDING_ENDPOINT".to_owned(),
                "https://proxy.example/v1".to_owned(),
            ),
            ("APEX_COGNEE_EMBEDDING_MODEL".to_owned(), model.to_owned()),
            (
                "APEX_COGNEE_EMBEDDING_DIMENSIONS".to_owned(),
                dimensions.to_owned(),
            ),
        ]));
        let agent = AgentConfig::from_env(&env).expect("syntactically valid agent config");

        assert!(matches!(
            CogneeReferenceEngineFactory::new(agent),
            Err(ReferenceError::Unavailable)
        ));
    }
}
