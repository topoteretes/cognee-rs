#![cfg(feature = "engine")]

use std::collections::HashMap;

use cognee_mcp::config::{AgentConfig, EnvSource};
use cognee_mcp::embedding_generation::EmbeddingGeneration;
use cognee_mcp::engine::{
    CogneeEngineFactory, EngineFactory, RecallRequest, normalize_recall_item,
};
use serde_json::json;
use tempfile::tempdir;

struct FakeEnv(HashMap<String, String>);

impl EnvSource for FakeEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

#[tokio::test]
async fn factory_constructs_and_closes_a_short_lived_handle_without_warming_storage() {
    let temporary = tempdir().expect("temporary root");
    let env = FakeEnv(HashMap::from([
        (
            "APEX_COGNEE_ROOT".to_owned(),
            temporary.path().join("cognee").display().to_string(),
        ),
        ("APEX_COGNEE_PROXY_KEY".to_owned(), "test-key".to_owned()),
        ("APEX_COGNEE_LLM_PROVIDER".to_owned(), "openai".to_owned()),
        (
            "APEX_COGNEE_LLM_ENDPOINT".to_owned(),
            "https://llm-proxy.example".to_owned(),
        ),
        (
            "APEX_COGNEE_LLM_MODEL".to_owned(),
            "gpt-5.4-nano".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_PROVIDER".to_owned(),
            "openai".to_owned(),
        ),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT".to_owned(),
            "https://llm-proxy.example".to_owned(),
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
    let config = AgentConfig::from_env(&env).expect("agent config");
    let embedding = config.embedding.as_ref().expect("embedding config");
    let generation =
        EmbeddingGeneration::new(&config.layout, "generation-1", embedding).expect("generation");
    let database = generation.data().join("cognee.db");
    let factory = CogneeEngineFactory::new(config, generation);

    let engine = factory.open().await.expect("open engine handle");
    assert!(!database.exists(), "opening the factory must stay lazy");
    engine.close().await;
    assert!(
        !database.exists(),
        "closing an unused handle must not warm storage"
    );
}

#[test]
fn graph_recall_normalization_retains_reference_provenance_without_changing_private_schema() {
    let request = RecallRequest {
        query: "restart".to_owned(),
        dataset: "agent_sessions".to_owned(),
        session_id: Some("session-1".to_owned()),
        top_k: 3,
        search_type: Some("CHUNKS".to_owned()),
        auto_route: false,
    };
    let item = normalize_recall_item(
        &json!({
            "source": "graph",
            "content": {
                "payload": {
                    "text": "rolling restart",
                    "external_metadata": "{\"cognee_external_event_id\":\"event-2\",\"reference_source_id\":\"source-1\",\"reference_revision\":2,\"reference_label\":\"restart.md\",\"content_type\":\"text/markdown\",\"content_sha256\":\"sha256:content\"}"
                }
            },
            "score": 0.75
        }),
        &request,
    );

    assert_eq!(item.event_id.as_deref(), Some("event-2"));
    assert_eq!(item.metadata["reference_source_id"], "source-1");
    assert_eq!(item.metadata["reference_revision"], 2);
    assert_eq!(item.metadata["reference_label"], "restart.md");
    assert_eq!(item.metadata["reference_content_type"], "text/markdown");
    assert_eq!(item.metadata["reference_content_sha256"], "sha256:content");
    assert_eq!(
        serde_json::to_value(&item)
            .expect("serialize private recall item")
            .as_object()
            .expect("private recall object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "content",
            "dataset",
            "event_id",
            "metadata",
            "score",
            "session_id",
            "source",
            "timestamp",
        ]
        .into_iter()
        .collect()
    );
}

#[cfg(all(feature = "fleet", target_os = "linux"))]
mod fleet_crash_convergence {
    use std::collections::{HashMap, VecDeque};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use cognee::cognee_vector::VectorPoint;
    use cognee_bindings_common::HandleState;
    use cognee_mcp::engine::{
        ApplyPlan, ApplyReceipt, EngineFactory, ForgetReceipt, ForgetTarget, ImproveReceipt,
        MemoryEngine, RecallRequest, RecallResponse, plan_event_application,
    };
    use cognee_mcp::error::AgentError;
    use cognee_mcp::event::{CaptureMetadata, EventEnvelope, EventKind};
    use cognee_mcp::layout::StateLayout;
    use cognee_mcp::lease::EngineLease;
    use cognee_mcp::ledger::{IngestionState, Ledger};
    use cognee_mcp::limits::ResourceLimits;
    use cognee_mcp::spool::{Priority, Spool};
    use cognee_mcp::worker::{DrainBudget, FaultPoint, Worker, WorkerRuntime};
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{AgentConfig, EmbeddingGeneration};

    const EVENT_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SESSION_ID: &str = "fleet-crash-session";
    const NODE_A: &str = "fleet-proof-node-a";
    const NODE_B: &str = "fleet-proof-node-b";
    const VECTOR_TYPE: &str = "FleetCrashProof";
    const VECTOR_FIELD: &str = "embedding";

    struct FaultSequence {
        pending: Mutex<VecDeque<FaultPoint>>,
    }

    impl FaultSequence {
        fn new(points: impl IntoIterator<Item = FaultPoint>) -> Self {
            Self {
                pending: Mutex::new(points.into_iter().collect()),
            }
        }
    }

    impl WorkerRuntime for FaultSequence {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::parse_from_rfc3339("2026-08-19T20:00:01Z")
                .expect("fixture time")
                .with_timezone(&chrono::Utc)
        }

        fn check_fault(&self, point: FaultPoint) -> Result<(), AgentError> {
            let mut pending = self.pending.lock().expect("fault sequence lock");
            if pending.front().copied() == Some(point) {
                pending.pop_front();
                return Err(AgentError::InjectedFault(point));
            }
            Ok(())
        }
    }

    struct FleetFactory {
        config: AgentConfig,
        generation: EmbeddingGeneration,
        runtime: Arc<FaultSequence>,
        closed_states: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait]
    impl EngineFactory for FleetFactory {
        async fn open(&self) -> Result<Box<dyn MemoryEngine>, AgentError> {
            let settings = self
                .config
                .cognee_settings(&self.generation)
                .map_err(|_| AgentError::Blocked("configuration_drift"))?;
            Ok(Box::new(FleetEngine {
                state: HandleState::from_settings(settings),
                runtime: self.runtime.clone(),
                closed_states: self.closed_states.clone(),
            }))
        }
    }

    struct FleetEngine {
        state: HandleState,
        runtime: Arc<FaultSequence>,
        closed_states: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait]
    impl MemoryEngine for FleetEngine {
        async fn contains_event(
            &mut self,
            dataset: &str,
            event_id: &str,
        ) -> Result<bool, AgentError> {
            cognee_bindings_common::ops::memory::run_contains_external_event(
                &self.state,
                dataset,
                None,
                event_id,
            )
            .await
            .map_err(|_| AgentError::Engine("contains_external_event"))
        }

        async fn contains_event_for(&mut self, event: &EventEnvelope) -> Result<bool, AgentError> {
            cognee_bindings_common::ops::memory::run_contains_external_event(
                &self.state,
                &event.dataset,
                Some(&event.session_id),
                &event.event_id,
            )
            .await
            .map_err(|_| AgentError::Engine("contains_session_event"))
        }

        async fn apply_event(&mut self, event: &EventEnvelope) -> Result<ApplyReceipt, AgentError> {
            let ApplyPlan::SessionEntry {
                dataset,
                session_id,
                entry,
                options,
            } = plan_event_application(event)?
            else {
                return Err(AgentError::Engine("unexpected_fleet_apply_plan"));
            };
            let result = cognee_bindings_common::ops::memory::run_remember_entry(
                &self.state,
                entry,
                &dataset,
                &session_id,
                &options,
            )
            .await
            .map_err(|_| AgentError::Engine("remember_entry"))?;
            Ok(ApplyReceipt::new(
                result
                    .get("entry_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ))
        }

        async fn improve(
            &mut self,
            _dataset: &str,
            session_ids: &[String],
        ) -> Result<ImproveReceipt, AgentError> {
            let services = self
                .state
                .services()
                .await
                .map_err(|_| AgentError::Engine("fleet_services"))?;
            let entities = [
                (
                    1,
                    NODE_A,
                    "Fleet proof alpha",
                    "00000000-0000-0000-0000-000000000001",
                    vec![1.0, 0.0, 0.0, 0.0],
                ),
                (
                    2,
                    NODE_B,
                    "Fleet proof bravo",
                    "00000000-0000-0000-0000-000000000002",
                    vec![0.0, 1.0, 0.0, 0.0],
                ),
            ];

            for (ordinal, id, name, vector_id, vector) in entities {
                services
                    .graph_db
                    .add_node_raw(json!({
                        "id": id,
                        "name": name,
                        "type": "FleetCrashProof",
                        "cognee_external_event_id": EVENT_ID
                    }))
                    .await
                    .map_err(|_| AgentError::Engine("fleet_graph_node"))?;
                services
                    .vector_db
                    .upsert_raw_vectors(
                        VECTOR_TYPE,
                        VECTOR_FIELD,
                        &[VectorPoint {
                            id: vector_id.parse().expect("fixture vector UUID"),
                            vector,
                            metadata: HashMap::from([(
                                "cognee_external_event_id".to_owned(),
                                json!(EVENT_ID),
                            )]),
                        }],
                    )
                    .await
                    .map_err(|_| AgentError::Engine("fleet_vector_entity"))?;
                self.runtime
                    .check_fault(FaultPoint::DuringImproveEntity(ordinal))?;
            }

            services
                .graph_db
                .add_edge(NODE_A, NODE_B, "supports", None)
                .await
                .map_err(|_| AgentError::Engine("fleet_graph_edge"))?;
            Ok(ImproveReceipt {
                sessions_persisted: session_ids.len(),
            })
        }

        async fn recall(&mut self, _request: RecallRequest) -> Result<RecallResponse, AgentError> {
            Err(AgentError::Engine("unexpected_fleet_recall"))
        }

        async fn forget(&mut self, _target: ForgetTarget) -> Result<ForgetReceipt, AgentError> {
            Err(AgentError::Engine("unexpected_fleet_forget"))
        }

        async fn close(self: Box<Self>) {
            self.state.close().await;
            self.closed_states
                .lock()
                .expect("closed states lock")
                .push(self.state.is_closed());
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn crash_windows_converge_real_session_graph_and_vector_state() {
        let temporary = tempdir().expect("temporary root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused endpoint");
        let unused_endpoint = listener.local_addr().expect("unused endpoint address");
        drop(listener);

        let env = super::FakeEnv(HashMap::from([
            (
                "APEX_COGNEE_ROOT".to_owned(),
                temporary.path().join("cognee").display().to_string(),
            ),
            ("APEX_COGNEE_PROXY_KEY".to_owned(), "test-key".to_owned()),
            ("APEX_COGNEE_LLM_PROVIDER".to_owned(), "openai".to_owned()),
            (
                "APEX_COGNEE_LLM_ENDPOINT".to_owned(),
                format!("http://{unused_endpoint}/v1"),
            ),
            (
                "APEX_COGNEE_LLM_MODEL".to_owned(),
                "gpt-5.4-nano".to_owned(),
            ),
            (
                "APEX_COGNEE_EMBEDDING_PROVIDER".to_owned(),
                "mock".to_owned(),
            ),
            (
                "APEX_COGNEE_EMBEDDING_ENDPOINT".to_owned(),
                format!("http://{unused_endpoint}/v1"),
            ),
            (
                "APEX_COGNEE_EMBEDDING_MODEL".to_owned(),
                "mock-deterministic".to_owned(),
            ),
            (
                "APEX_COGNEE_EMBEDDING_DIMENSIONS".to_owned(),
                "4".to_owned(),
            ),
        ]));
        let config = AgentConfig::from_env(&env).expect("fleet agent config");
        let embedding = config.embedding.as_ref().expect("fleet embedding config");
        let generation = EmbeddingGeneration::new(&config.layout, "fleet-generation", embedding)
            .expect("fleet generation");
        let layout = config.layout.clone();
        let mut limits = ResourceLimits::default();
        limits.improve_every = 1;
        let event = EventEnvelope {
            schema_version: 1,
            event_id: EVENT_ID.to_owned(),
            engineer: "alice".to_owned(),
            host: "fleet-host".to_owned(),
            session_id: SESSION_ID.to_owned(),
            event: EventKind::AfterAgent,
            timestamp: "2026-08-19T20:00:00Z".to_owned(),
            cwd: "/x/eng/project".to_owned(),
            dataset: "agent_sessions".to_owned(),
            dataset_generation: 0,
            payload_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            payload: json!({
                "prompt": "Which two proof nodes belong together?",
                "prompt_response": "Fleet proof alpha supports fleet proof bravo."
            }),
            capture: CaptureMetadata {
                original_bytes: 96,
                retained_bytes: 96,
                redaction_count: 0,
                truncation_count: 0,
                prompt_truncated: false,
                response_truncated: false,
                tool_input_truncated: false,
                tool_response_truncated: false,
                capture_degraded: false,
            },
        };
        Spool::new(layout.clone(), limits.clone())
            .enqueue(&event, Priority::Normal)
            .expect("enqueue fleet event");
        let runtime = Arc::new(FaultSequence::new([
            FaultPoint::AfterApplyBeforeLedgerCommit,
            FaultPoint::DuringImproveEntity(2),
        ]));
        let closed_states = Arc::new(Mutex::new(Vec::new()));
        let factory = Arc::new(FleetFactory {
            config: config.clone(),
            generation: generation.clone(),
            runtime: runtime.clone(),
            closed_states: closed_states.clone(),
        });

        let first = worker_for(&layout, &limits, factory.clone(), runtime.clone())
            .drain(DrainBudget::from_limits(&limits))
            .await;
        assert_eq!(first.committed, 0);
        assert_eq!(first.last_error_class.as_deref(), Some("injected_fault"));
        assert_eq!(spool_depths(&layout), (0, 1));
        assert_eq!(ledger_state(&layout, EVENT_ID), IngestionState::Applying);

        let second = worker_for(&layout, &limits, factory.clone(), runtime.clone())
            .drain(DrainBudget::from_limits(&limits))
            .await;
        assert_eq!(second.committed, 1);
        assert_eq!(second.improved, 0);
        assert_eq!(second.last_error_class.as_deref(), Some("injected_fault"));
        assert_eq!(spool_depths(&layout), (0, 0));
        assert_eq!(ledger_state(&layout, EVENT_ID), IngestionState::Committed);

        let third = worker_for(&layout, &limits, factory, runtime)
            .drain(DrainBudget::from_limits(&limits))
            .await;
        assert_eq!(third.committed, 0);
        assert_eq!(third.improved, 1);
        assert_eq!(third.failed, 0);
        assert_eq!(
            closed_states.lock().expect("closed states lock").as_slice(),
            [true, true, true]
        );
        assert!(!layout.locks.join("engine").exists());

        let verification_state = HandleState::from_settings(
            config
                .cognee_settings(&generation)
                .expect("verification settings"),
        );
        let services = verification_state
            .services()
            .await
            .expect("verification services");
        let owner_id = verification_state.owner_id().await.expect("owner id");
        let entries = services
            .session_store
            .get_all_qa_entries(SESSION_ID, Some(&owner_id.to_string()))
            .await
            .expect("session entries");
        assert_eq!(entries.len(), 1, "session retry must remain idempotent");
        assert_eq!(entries[0].external_event_id.as_deref(), Some(EVENT_ID));

        let (nodes, edges) = services
            .graph_db
            .get_graph_data()
            .await
            .expect("fleet graph data");
        let node_ids: std::collections::HashSet<&str> =
            nodes.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(node_ids, [NODE_A, NODE_B].into_iter().collect());
        assert_eq!(edges.len(), 1, "one converged edge");
        assert_eq!(edges[0].0, NODE_A);
        assert_eq!(edges[0].1, NODE_B);
        assert_eq!(edges[0].2, "supports");
        assert_eq!(
            services
                .vector_db
                .collection_size(VECTOR_TYPE, VECTOR_FIELD)
                .await
                .expect("fleet vector count"),
            2,
            "vector upserts must converge by id"
        );
        drop(services);
        verification_state.close().await;
        assert!(verification_state.is_closed());
        assert!(
            TcpStream::connect_timeout(&unused_endpoint, Duration::from_millis(100)).is_err(),
            "the transient engine must not leave a listener"
        );
    }

    fn worker_for(
        layout: &StateLayout,
        limits: &ResourceLimits,
        factory: Arc<dyn EngineFactory>,
        runtime: Arc<dyn WorkerRuntime>,
    ) -> Worker {
        Worker::new(
            layout.clone(),
            Spool::new(layout.clone(), limits.clone()),
            EngineLease::new(
                layout.clone(),
                Duration::from_secs(u64::from(limits.lease_stale_seconds)),
            ),
            Ledger::open(layout.clone()).expect("open fleet ledger"),
            factory,
            limits.clone(),
        )
        .with_runtime(runtime)
    }

    fn ledger_state(layout: &StateLayout, event_id: &str) -> IngestionState {
        Ledger::open(layout.clone())
            .expect("inspect fleet ledger")
            .state(event_id)
            .expect("read fleet ledger")
            .expect("fleet ledger event")
            .state
    }

    fn spool_depths(layout: &StateLayout) -> (usize, usize) {
        let depths = Spool::new(layout.clone(), ResourceLimits::default())
            .depths()
            .expect("fleet spool depths");
        (depths.pending, depths.processing)
    }
}
