#![cfg(feature = "runtime")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_mcp::atomic_fs::SystemSyncOps;
use cognee_mcp::engine::{RecallItem, RecallRequest, RecallResponse, RecallSource};
use cognee_mcp::reference::{
    CurrentPointer, DeltaStore, PreparedDocument, PublishFaultPoint, PublishHooks, ReferenceConfig,
    ReferenceEngineFactory, ReferenceEngineIdentity, ReferenceEngineInput, ReferenceEngineOpen,
    ReferenceError, ReferenceLimits, ReferenceProviderFingerprint, ReferenceReadEngine,
    ReferenceReadHooks, ReferenceReader, ReferenceRecallProbe, ReferenceRecallRequest,
    ReferenceWriteEngine, Source,
};
use serde_json::{Map, Value, json};

fn config(root: PathBuf) -> ReferenceConfig {
    ReferenceConfig {
        layout: cognee_mcp::reference::ReferenceLayout::under(root),
        dataset: "fleet_reference",
        limits: ReferenceLimits::default(),
    }
}

fn document(
    content: &str,
    source_id: &str,
    label: &str,
    limits: &ReferenceLimits,
) -> PreparedDocument {
    PreparedDocument::from_bytes(
        Source::Stdin,
        content.as_bytes(),
        Some(source_id),
        Some(label),
        limits,
    )
    .expect("reference document")
}

fn commit(config: &ReferenceConfig, content: &str, source_id: &str, label: &str) {
    DeltaStore::new(config.layout.clone(), config.limits)
        .commit_batch(&[document(content, source_id, label, &config.limits)])
        .expect("commit reference");
}

fn identity() -> ReferenceEngineIdentity {
    ReferenceEngineIdentity {
        cognee_rs_commit: "0123456789abcdef".to_owned(),
        adapter_version: "1.4.4".to_owned(),
        user_agent: "Apex/test (macos; arm64)".to_owned(),
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

#[derive(Default)]
struct FakeState {
    opens: Mutex<Vec<ReferenceEngineOpen>>,
    closes: AtomicUsize,
    recalls: Mutex<Vec<RecallRequest>>,
    graph_items: Mutex<Vec<RecallItem>>,
    fail_graph: AtomicBool,
}

#[derive(Clone)]
struct FakeFactory {
    state: Arc<FakeState>,
    identity: ReferenceEngineIdentity,
}

impl FakeFactory {
    fn new() -> Self {
        Self {
            state: Arc::new(FakeState::default()),
            identity: identity(),
        }
    }

    fn with_identity(identity: ReferenceEngineIdentity) -> Self {
        Self {
            state: Arc::new(FakeState::default()),
            identity,
        }
    }
}

struct FakeWriter {
    root: PathBuf,
}

#[async_trait]
impl ReferenceWriteEngine for FakeWriter {
    async fn add_and_cognify(
        &mut self,
        _dataset: &str,
        _inputs: Vec<ReferenceEngineInput>,
    ) -> Result<(), ReferenceError> {
        for directory in ["data", "vector", "graph"] {
            std::fs::create_dir_all(self.root.join(directory))?;
            std::fs::write(self.root.join(directory).join("store"), directory)?;
        }
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        Ok(())
    }
}

struct FakeReaderEngine {
    state: Arc<FakeState>,
}

#[async_trait]
impl ReferenceReadEngine for FakeReaderEngine {
    async fn recall_contains(
        &mut self,
        _dataset: &str,
        _probe: &ReferenceRecallProbe,
    ) -> Result<bool, ReferenceError> {
        Ok(true)
    }

    async fn recall(&mut self, request: RecallRequest) -> Result<RecallResponse, ReferenceError> {
        self.state
            .recalls
            .lock()
            .expect("recalls lock")
            .push(request.clone());
        if self.state.fail_graph.load(Ordering::SeqCst) {
            return Err(ReferenceError::Unavailable);
        }
        Ok(RecallResponse {
            items: self.state.graph_items.lock().expect("graph items").clone(),
            search_type_used: request.search_type,
            auto_routed: false,
        })
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        self.state.closes.fetch_add(1, Ordering::SeqCst);
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
        Ok(Box::new(FakeReaderEngine {
            state: Arc::clone(&self.state),
        }))
    }
}

struct NoopPublishHooks;

impl PublishHooks for NoopPublishHooks {
    fn checkpoint(&self, _point: PublishFaultPoint) -> Result<(), ReferenceError> {
        Ok(())
    }
}

async fn publish(config: &ReferenceConfig, factory: &FakeFactory) {
    cognee_mcp::reference::ReferencePublisher::with_dependencies(
        config.clone(),
        Arc::new(factory.clone()),
        Arc::new(SystemSyncOps),
        Arc::new(NoopPublishHooks),
        "reader-test-host".to_owned(),
    )
    .publish_once()
    .await
    .expect("publish generation");
}

fn request(query: &str) -> ReferenceRecallRequest {
    ReferenceRecallRequest {
        query: query.to_owned(),
        ..ReferenceRecallRequest::default()
    }
}

fn graph_item(
    content: &str,
    score: f64,
    event_id: Option<&str>,
    source_id: Option<&str>,
    revision: Option<u64>,
    label: Option<&str>,
    content_sha256: Option<&str>,
) -> RecallItem {
    let mut metadata = Map::new();
    for (key, value) in [
        ("cognee_external_event_id", event_id.map(Value::from)),
        ("reference_source_id", source_id.map(Value::from)),
        ("reference_revision", revision.map(Value::from)),
        ("reference_label", label.map(Value::from)),
        ("reference_content_type", Some(Value::from("text/plain"))),
        ("reference_content_sha256", content_sha256.map(Value::from)),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), value);
        }
    }
    RecallItem {
        source: RecallSource::Graph,
        content: content.to_owned(),
        score: Some(score),
        dataset: "fleet_reference".to_owned(),
        session_id: None,
        timestamp: None,
        event_id: event_id.map(str::to_owned),
        metadata,
    }
}

#[tokio::test]
async fn delta_only_root_recalls_committed_records_without_opening_a_graph() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    commit(
        &config,
        "The fleet restart standard is rolling restart.",
        "restart-standard",
        "restart.md",
    );
    let factory = FakeFactory::new();

    let response = ReferenceReader::new(config, Arc::new(factory.clone()))
        .recall(request("rolling restart"))
        .await
        .expect("delta recall");

    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].source, "reference_delta");
    assert_eq!(
        response.items[0].source_label.as_deref(),
        Some("restart.md")
    );
    assert!(!response.items[0].cognified);
    assert_eq!(response.reference.generation_id, None);
    assert_eq!(response.reference.included_through, 0);
    assert_eq!(response.reference.committed_head, 1);
    assert_eq!(response.reference.delta_examined, 1);
    assert!(factory.state.opens.lock().expect("opens lock").is_empty());
}

#[tokio::test]
async fn reader_rejects_wrong_schema_version_or_dataset_before_snapshotting() {
    for schema in [
        json!({"schema_version": 2, "dataset": "fleet_reference"}),
        json!({"schema_version": 1, "dataset": "agent_sessions"}),
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = config(temporary.path().join("reference"));
        commit(&config, "schema validation needle", "schema", "schema.md");
        replace_read_only_file(
            &config.layout.schema,
            &serde_json::to_vec(&schema).expect("serialize schema"),
        );

        let error = ReferenceReader::new(config, Arc::new(FakeFactory::new()))
            .recall(request("schema validation"))
            .await
            .expect_err("invalid schema identity");

        assert!(matches!(error, ReferenceError::CorruptRecord));
    }
}

struct AdvancingHooks {
    current_path: PathBuf,
    new_current: Vec<u8>,
    head_path: PathBuf,
    new_head: Vec<u8>,
}

impl ReferenceReadHooks for AdvancingHooks {
    fn after_current_snapshot(&self) {
        replace_read_only_file(&self.current_path, &self.new_current);
    }

    fn after_head_snapshot(&self) {
        replace_read_only_file(&self.head_path, &self.new_head);
    }
}

#[tokio::test]
async fn pointer_and_head_advances_leave_no_missing_interval_between_calls() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let factory = FakeFactory::new();
    commit(&config, "generation one anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    let current_one = std::fs::read(&config.layout.current).expect("current one");
    commit(&config, "second boundary fact", "second", "second.md");
    publish(&config, &factory).await;
    let current_two = std::fs::read(&config.layout.current).expect("current two");
    let head_two = std::fs::read(&config.layout.delta_head).expect("head two");
    commit(&config, "third boundary fact", "third", "third.md");
    let head_three = std::fs::read(&config.layout.delta_head).expect("head three");
    replace_read_only_file(&config.layout.current, &current_one);
    replace_read_only_file(&config.layout.delta_head, &head_two);
    let hooks = Arc::new(AdvancingHooks {
        current_path: config.layout.current.clone(),
        new_current: current_two,
        head_path: config.layout.delta_head.clone(),
        new_head: head_three,
    });
    let reader = ReferenceReader::with_hooks(config.clone(), Arc::new(factory.clone()), hooks);

    let first = reader
        .recall(request("boundary fact"))
        .await
        .expect("first coherent snapshot");
    assert_eq!(first.reference.included_through, 1);
    assert_eq!(first.reference.committed_head, 2);
    assert_eq!(first.items[0].source_label.as_deref(), Some("second.md"));

    let second = ReferenceReader::new(config, Arc::new(factory))
        .recall(request("boundary fact"))
        .await
        .expect("next coherent snapshot");
    assert_eq!(second.reference.included_through, 2);
    assert_eq!(second.reference.committed_head, 3);
    assert_eq!(second.items[0].source_label.as_deref(), Some("third.md"));
}

#[tokio::test]
async fn unsafe_generation_component_is_never_opened_and_delta_falls_back_safely() {
    let (config, factory) = published_then_pending_fixture().await;
    let mut pointer: CurrentPointer =
        serde_json::from_slice(&std::fs::read(&config.layout.current).expect("current pointer"))
            .expect("parse current pointer");
    pointer.generation_id = "../escape".to_owned();
    replace_read_only_file(
        &config.layout.current,
        &serde_json::to_vec(&pointer).expect("serialize pointer"),
    );
    let opens_before = factory.state.opens.lock().expect("opens lock").len();

    let response = ReferenceReader::new(config, Arc::new(factory.clone()))
        .recall(request("pending safety"))
        .await
        .expect("safe delta fallback");

    assert_eq!(response.reference.status, "degraded");
    assert_eq!(response.items[0].source, "reference_delta");
    assert_eq!(
        factory.state.opens.lock().expect("opens lock").len(),
        opens_before
    );
}

#[tokio::test]
async fn manifest_hash_mismatch_is_never_opened_and_delta_falls_back_safely() {
    let (config, factory) = published_then_pending_fixture().await;
    let mut pointer: CurrentPointer =
        serde_json::from_slice(&std::fs::read(&config.layout.current).expect("current pointer"))
            .expect("parse current pointer");
    pointer.manifest_sha256 = "sha256:forged".to_owned();
    replace_read_only_file(
        &config.layout.current,
        &serde_json::to_vec(&pointer).expect("serialize pointer"),
    );
    let opens_before = factory.state.opens.lock().expect("opens lock").len();

    let response = ReferenceReader::new(config, Arc::new(factory.clone()))
        .recall(request("pending safety"))
        .await
        .expect("safe delta fallback");

    assert_eq!(response.reference.status, "degraded");
    assert_eq!(response.items[0].source, "reference_delta");
    assert_eq!(
        factory.state.opens.lock().expect("opens lock").len(),
        opens_before
    );
}

#[tokio::test]
async fn invalid_generation_without_matching_delta_returns_a_typed_error() {
    let (config, factory) = published_then_pending_fixture().await;
    let mut pointer: CurrentPointer =
        serde_json::from_slice(&std::fs::read(&config.layout.current).expect("current pointer"))
            .expect("parse current pointer");
    pointer.manifest_sha256 = "sha256:forged".to_owned();
    replace_read_only_file(
        &config.layout.current,
        &serde_json::to_vec(&pointer).expect("serialize pointer"),
    );

    let error = ReferenceReader::new(config, Arc::new(factory))
        .recall(request("no matching committed content"))
        .await
        .expect_err("invalid generation without safe fallback");

    assert!(matches!(error, ReferenceError::CorruptRecord));
}

#[tokio::test]
async fn embedding_fingerprint_mismatch_is_a_typed_error_without_graph_open() {
    let (config, publisher_factory) = published_then_pending_fixture().await;
    let mut wrong_identity = identity();
    wrong_identity.embedding.dimensions = Some(1536);
    let reader_factory = FakeFactory::with_identity(wrong_identity);

    let error = ReferenceReader::new(config, Arc::new(reader_factory.clone()))
        .recall(request("pending safety"))
        .await
        .expect_err("model mismatch");

    assert!(matches!(error, ReferenceError::ModelMismatch));
    assert!(
        reader_factory
            .state
            .opens
            .lock()
            .expect("opens lock")
            .is_empty()
    );
    assert!(
        !publisher_factory
            .state
            .opens
            .lock()
            .expect("opens lock")
            .is_empty()
    );
}

#[tokio::test]
async fn merge_selects_latest_suppresses_old_graph_and_deduplicates_with_labels() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let factory = FakeFactory::new();
    commit(&config, "published anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    commit(&config, "old restart guidance", "restart", "old.md");
    commit(&config, "Canonical   Rolling Restart", "restart", "new.md");
    commit(
        &config,
        "canonical rolling restart",
        "alternate",
        "alternate.md",
    );
    let snapshot = DeltaStore::new(config.layout.clone(), config.limits)
        .snapshot_after(1)
        .expect("pending snapshot");
    let newest = snapshot
        .records
        .iter()
        .find(|record| record.source_label == "new.md")
        .expect("new revision");
    let oldest = snapshot
        .records
        .iter()
        .find(|record| record.source_label == "old.md")
        .expect("old revision");
    *factory.state.graph_items.lock().expect("graph items") = vec![
        graph_item(
            "old restart guidance",
            0.99,
            Some(&oldest.event_id),
            Some(&newest.source_id),
            Some(1),
            Some("old.md"),
            None,
        ),
        graph_item(
            "Canonical Rolling Restart",
            0.98,
            Some(&newest.event_id),
            Some(&newest.source_id),
            Some(2),
            Some("graph-copy.md"),
            Some(&newest.content_sha256),
        ),
        graph_item(
            "unprovenanced graph result",
            0.5,
            None,
            None,
            None,
            None,
            None,
        ),
    ];

    let closes_before = factory.state.closes.load(Ordering::SeqCst);
    let response = ReferenceReader::new(config, Arc::new(factory.clone()))
        .recall(ReferenceRecallRequest {
            query: "rolling restart".to_owned(),
            top_k: 10,
            ..ReferenceRecallRequest::default()
        })
        .await
        .expect("merged recall");

    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].source, "reference_delta");
    assert_eq!(response.items[0].revision, Some(2));
    assert_eq!(response.items[1].content, "unprovenanced graph result");
    assert_eq!(
        response.items[0].metadata["alternate_source_labels"],
        json!(["alternate.md", "graph-copy.md"])
    );
    assert_eq!(
        factory.state.closes.load(Ordering::SeqCst),
        closes_before + 1
    );
    let recalls = factory.state.recalls.lock().expect("recalls lock");
    assert_eq!(recalls.len(), 1);
    assert_eq!(recalls[0].dataset, "fleet_reference");
    assert!(!recalls[0].auto_route);
}

#[tokio::test]
async fn long_delta_excerpt_is_bounded_around_the_query_match() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    commit(
        &config,
        &format!("{} needle-at-the-end", "prefix ".repeat(600)),
        "long-delta",
        "long.md",
    );

    let response = ReferenceReader::new(config, Arc::new(FakeFactory::new()))
        .recall(request("needle at end"))
        .await
        .expect("bounded delta excerpt");

    assert_eq!(response.items.len(), 1);
    assert!(response.items[0].content.contains("needle-at-the-end"));
    assert!(response.items[0].content.len() <= 2 * 1024);
    assert!(response.truncated);
}

#[tokio::test]
async fn long_graph_excerpt_is_bounded_without_dropping_the_result() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let factory = FakeFactory::new();
    commit(&config, "published anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    *factory.state.graph_items.lock().expect("graph items") = vec![graph_item(
        &format!("graph needle {}", "x".repeat(4_000)),
        0.8,
        None,
        None,
        None,
        Some("graph.md"),
        None,
    )];

    let response = ReferenceReader::new(config, Arc::new(factory))
        .recall(request("graph needle"))
        .await
        .expect("bounded graph excerpt");

    assert_eq!(response.items.len(), 1);
    assert!(response.items[0].content.len() <= 2 * 1024);
    assert_eq!(
        response.items[0].content_type.as_deref(),
        Some("text/plain")
    );
    assert!(response.truncated);
}

#[tokio::test]
async fn corrupt_delta_is_counted_while_other_committed_records_remain_recallable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    commit(&config, "corrupt irrelevant fact", "corrupt", "corrupt.md");
    commit(&config, "healthy searchable fact", "healthy", "healthy.md");
    let store = DeltaStore::new(config.layout.clone(), config.limits);
    let corrupt = store.event_path(1).expect("first event path");
    make_writable(&corrupt);
    std::fs::write(&corrupt, b"{not-json").expect("corrupt event");

    let response = ReferenceReader::new(config, Arc::new(FakeFactory::new()))
        .recall(request("healthy searchable"))
        .await
        .expect("partial delta recall");

    assert_eq!(response.reference.delta_examined, 2);
    assert_eq!(response.reference.corrupt_delta_skipped, 1);
    assert_eq!(response.reference.status, "degraded");
    assert_eq!(
        response.items[0].source_label.as_deref(),
        Some("healthy.md")
    );
}

#[tokio::test]
async fn graph_failure_closes_the_engine_and_returns_safe_delta_as_degraded() {
    let (config, factory) = published_then_pending_fixture().await;
    factory.state.fail_graph.store(true, Ordering::SeqCst);
    let closes_before = factory.state.closes.load(Ordering::SeqCst);

    let response = ReferenceReader::new(config, Arc::new(factory.clone()))
        .recall(request("pending safety"))
        .await
        .expect("delta fallback");

    assert_eq!(response.reference.status, "degraded");
    assert_eq!(
        response.reference.graph_status.as_deref(),
        Some("unavailable")
    );
    assert_eq!(response.items[0].source, "reference_delta");
    assert_eq!(
        factory.state.closes.load(Ordering::SeqCst),
        closes_before + 1
    );
}

#[tokio::test]
async fn graph_failure_never_falls_back_to_a_match_older_than_corrupt_delta() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let factory = FakeFactory::new();
    commit(&config, "published anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    commit(
        &config,
        "stale candidate needle",
        "candidate",
        "candidate.md",
    );
    commit(&config, "later record", "later", "later.md");
    corrupt_event(&config, 3);
    factory.state.fail_graph.store(true, Ordering::SeqCst);

    let error = ReferenceReader::new(config, Arc::new(factory))
        .recall(request("stale candidate"))
        .await
        .expect_err("corruption after the match makes fallback unsafe");

    assert!(matches!(error, ReferenceError::CorruptRecord));
}

#[tokio::test]
async fn invalid_generation_never_falls_back_to_a_match_older_than_corrupt_delta() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = config(temporary.path().join("reference"));
    let factory = FakeFactory::new();
    commit(&config, "published anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    commit(
        &config,
        "stale candidate needle",
        "candidate",
        "candidate.md",
    );
    commit(&config, "later record", "later", "later.md");
    corrupt_event(&config, 3);
    let mut pointer: CurrentPointer =
        serde_json::from_slice(&std::fs::read(&config.layout.current).expect("current pointer"))
            .expect("parse current pointer");
    pointer.manifest_sha256 = "sha256:forged".to_owned();
    replace_read_only_file(
        &config.layout.current,
        &serde_json::to_vec(&pointer).expect("serialize pointer"),
    );

    let error = ReferenceReader::new(config, Arc::new(factory))
        .recall(request("stale candidate"))
        .await
        .expect_err("corruption after the match makes fallback unsafe");

    assert!(matches!(error, ReferenceError::CorruptRecord));
}

#[tokio::test]
async fn oversized_config_cannot_raise_response_hard_ceiling() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut config = config(temporary.path().join("reference"));
    config.limits.max_item_bytes = 64 * 1024;
    config.limits.max_payload_bytes = 64 * 1024;
    for index in 0..12 {
        commit(
            &config,
            &format!("budgetneedle {index} {}", "x".repeat(4_000)),
            &format!("budget-{index}"),
            &format!("budget-{index}.md"),
        );
    }
    let reader = ReferenceReader::new(config, Arc::new(FakeFactory::new()));

    let defaults = reader
        .recall(request("budgetneedle"))
        .await
        .expect("default budget recall");
    assert_eq!(defaults.items.len(), 3);

    let capped = reader
        .recall(ReferenceRecallRequest {
            query: "budgetneedle".to_owned(),
            top_k: 100,
            ..ReferenceRecallRequest::default()
        })
        .await
        .expect("capped budget recall");
    assert!(capped.items.len() <= 10);
    assert!(
        capped
            .items
            .iter()
            .all(|item| item.content.len() <= 2 * 1024)
    );
    assert!(capped.truncated);
    let serialized = serde_json::to_vec(&capped).expect("valid response JSON");
    assert!(serialized.len() <= 8 * 1024, "{} bytes", serialized.len());
    let reparsed: Value = serde_json::from_slice(&serialized).expect("parse capped response");
    assert_eq!(reparsed["truncated"], true);
}

async fn published_then_pending_fixture() -> (ReferenceConfig, FakeFactory) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.keep().join("reference");
    let config = config(root);
    let factory = FakeFactory::new();
    commit(&config, "published anchor", "anchor", "anchor.md");
    publish(&config, &factory).await;
    commit(&config, "pending safety guidance", "pending", "pending.md");
    (config, factory)
}

fn replace_read_only_file(path: &Path, bytes: &[u8]) {
    make_writable(path);
    std::fs::write(path, bytes).expect("replace fixture file");
    make_read_only(path);
}

fn corrupt_event(config: &ReferenceConfig, sequence: u64) {
    let path = DeltaStore::new(config.layout.clone(), config.limits)
        .event_path(sequence)
        .expect("event path");
    make_writable(&path);
    std::fs::write(path, b"{not-json").expect("corrupt event");
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
        .expect("make fixture writable");
}

#[cfg(not(unix))]
fn make_writable(_path: &Path) {}

#[cfg(unix)]
fn make_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))
        .expect("make fixture read-only");
}

#[cfg(not(unix))]
fn make_read_only(_path: &Path) {}
