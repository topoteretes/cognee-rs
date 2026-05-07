//! Integration tests for the pipeline + task lifecycle analytics
//! events introduced by gap 03 (tasks 03-04 and 03-05).
//!
//! These tests assert *emission semantics* — the wire format and
//! ordering of events fired by [`cognee_core::pipeline::execute`] —
//! not production-path coverage. Each test drives a hand-built
//! `Pipeline` through `execute()` against a `mockito` HTTP server
//! bound to 127.0.0.1; the live proxy `https://test.prometh.ai` is
//! NEVER contacted.
//!
//! The test override hook is the gap-02-09 contract:
//! `COGNEE_TELEMETRY_INTEGRATION_TEST=1` plus
//! `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS=<server.url()>`. Production
//! builds without both env vars ignore the override.
//!
//! Soundness note: every test is `#[serial_test::serial]` and uses an
//! `IsolatedEnv` guard to snapshot/restore process-wide env vars.
//! The Rust 2024 edition makes `std::env::set_var` / `remove_var`
//! `unsafe`; `#[serial]` is the soundness argument for the `unsafe`
//! blocks below.

#![cfg(feature = "telemetry")]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cognee_core::cancellation::cancellation_pair;
use cognee_core::error::CoreError;
use cognee_core::exec_status::NoopExecStatusManager;
use cognee_core::pipeline::{
    ExecutionError, NoopWatcher, Pipeline, RetryDelay, RetryPolicy, execute,
};
use cognee_core::progress::ProgressToken;
use cognee_core::task::{Task, TaskError, Value};
use cognee_core::task_context::{PipelineContext, TaskContext};
use cognee_core::thread_pool::CpuPool;
use mockito::Server;
use serde_json::Value as JsonValue;
use serial_test::serial;
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct StubPool;
impl CpuPool for StubPool {
    fn spawn_raw(
        &self,
        _task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'static>> {
        Box::pin(async { Ok(()) })
    }
}

/// Set of env vars touched by each test. Centralised so `IsolatedEnv`
/// is the single place that knows what to reset on Drop.
const ENV_VARS: &[&str] = &[
    "HOME",
    "TRACKING_ID",
    "LLM_API_KEY",
    "TELEMETRY_API_KEY_TRACKING_SALT",
    "TELEMETRY_DISABLED",
    "ENV",
    "TELEMETRY_REQUEST_TIMEOUT",
    "COGNEE_TELEMETRY_INTEGRATION_TEST",
    "COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS",
];

/// Install a fresh, isolated env: a temp HOME, a fixed TRACKING_ID,
/// no LLM_API_KEY (so PBKDF2 is skipped — this saves ~50ms/event),
/// and the mockito URL injected via the gap-02-09 test override.
struct IsolatedEnv {
    _home: TempDir,
}

impl IsolatedEnv {
    fn install(server_url: &str) -> Self {
        let home = TempDir::new().expect("tempdir");
        // SAFETY: `#[serial]` orders this against every other env-mutating
        //   test in the binary; no concurrent reader/writer of these vars
        //   exists while this body runs.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("TRACKING_ID", "fixed-anon-pipeline-tests");
            std::env::remove_var("LLM_API_KEY");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
            std::env::remove_var("TELEMETRY_DISABLED");
            std::env::remove_var("ENV");
            std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
            std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
            std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", server_url);
        }
        // Wipe identity caches so the new HOME / TRACKING_ID take effect.
        cognee_telemetry::ids::__test_only_reset_caches();
        Self { _home: home }
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for k in ENV_VARS {
            // SAFETY: Drop runs inside the same `#[serial]` section as
            //   `install`, so no concurrent access exists.
            unsafe {
                std::env::remove_var(k);
            }
        }
    }
}

/// Build a TaskContext with an attached `pipeline_ctx` carrying the
/// supplied `tenant_id`. Other fields use stub/mocked dependencies.
async fn build_test_task_context_with_tenant(tenant_id: Option<Uuid>) -> Arc<TaskContext> {
    let db = cognee_database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    cognee_database::initialize(&db)
        .await
        .expect("initialize sqlite");
    let (_handle, token) = cancellation_pair();
    Arc::new(TaskContext {
        thread_pool: Arc::new(StubPool),
        database: Arc::new(db),
        graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
        vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
        cancellation: token,
        progress: ProgressToken::new(),
        pipeline_ctx: Some(PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test_pipeline".to_string(),
            user_id: None,
            tenant_id,
            dataset_id: None,
            current_data: None,
            run_id: None,
        }),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: None,
    })
}

/// Recording mockito mock that captures every POST body. Returns
/// `(mock, captured)` so the caller can install the mock then read
/// the body list at assertion time.
fn recording_mock(server: &mut mockito::ServerGuard) -> (mockito::Mock, Arc<Mutex<Vec<Vec<u8>>>>) {
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_cb = Arc::clone(&captured);
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_body_from_request(move |req| {
            if let Ok(body) = req.body() {
                // lock poison is unrecoverable
                captured_for_cb.lock().unwrap().push(body.clone());
            }
            Vec::new()
        })
        .expect_at_least(1)
        .create();
    (mock, captured)
}

/// Wait up to `timeout` for the captured request list to reach the
/// expected count. Polls at 25ms.
async fn wait_for_count(
    captured: &Arc<Mutex<Vec<Vec<u8>>>>,
    expected: usize,
    timeout: Duration,
) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        let n = {
            // lock poison is unrecoverable
            let g = captured.lock().unwrap();
            g.len()
        };
        if n >= expected {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Decode the captured POST bodies into JSON and pull `event_name`
/// out of each, in order. Returns the list as `String`s.
fn event_names(captured: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<String> {
    // lock poison is unrecoverable
    let bodies = captured.lock().unwrap().clone();
    bodies
        .into_iter()
        .map(|bytes| {
            let v: JsonValue = serde_json::from_slice(&bytes).expect("body is JSON");
            v["event_name"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_default()
        })
        .collect()
}

/// Decode all captured bodies into JSON.
fn captured_bodies(captured: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<JsonValue> {
    // lock poison is unrecoverable
    let bodies = captured.lock().unwrap().clone();
    bodies
        .into_iter()
        .map(|bytes| serde_json::from_slice(&bytes).expect("body is JSON"))
        .collect()
}

// ---------------------------------------------------------------------------
// 4.3.1 — Happy-path 4-event sequence
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn pipeline_lifecycle_emits_4_events_in_order() {
    let mut server = Server::new_async().await;
    let (mock, captured) = recording_mock(&mut server);
    let _env = IsolatedEnv::install(&server.url());

    let task = Task::async_fn_typed(|input: &i32, _ctx| {
        let v = *input;
        Box::pin(async move { Ok(Box::new(v + 1)) })
    });

    let pipeline = Pipeline::new("happy-path test")
        .with_name("test_pipeline_happy")
        .with_task(task);

    // None tenant → expect literal "Single User Tenant".
    let ctx = build_test_task_context_with_tenant(None).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(1_i32)];
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("execute should succeed");

    assert!(
        wait_for_count(&captured, 4, Duration::from_secs(5)).await,
        "expected 4 telemetry POSTs, got {:?}",
        event_names(&captured)
    );
    mock.assert_async().await;

    let names = event_names(&captured);
    assert_eq!(
        names,
        vec![
            "Pipeline Run Started".to_string(),
            "Coroutine Task Started".to_string(),
            "Coroutine Task Completed".to_string(),
            "Pipeline Run Completed".to_string(),
        ],
        "event ordering mismatch"
    );

    // Spot-check the payload shape on the first body.
    let bodies = captured_bodies(&captured);
    let started = &bodies[0];
    let p = &started["properties"];
    assert_eq!(p["pipeline_name"], "test_pipeline_happy");
    assert_eq!(p["tenant_id"], "Single User Tenant");
    assert_eq!(p["sdk_runtime"], "rust");
    assert!(
        p["cognee_version"].as_str().is_some(),
        "cognee_version must be a string"
    );
    // Locked decision 6: dataset_id and pipeline_run_id MUST NOT be in
    // the payload.
    assert!(
        p.get("dataset_id").is_none(),
        "dataset_id must not be on payload (locked decision 6)"
    );
    assert!(
        p.get("pipeline_run_id").is_none(),
        "pipeline_run_id must not be on payload (locked decision 6)"
    );
}

#[tokio::test]
#[serial]
async fn pipeline_started_emits_real_tenant_uuid_when_some() {
    let mut server = Server::new_async().await;
    let (mock, captured) = recording_mock(&mut server);
    let _env = IsolatedEnv::install(&server.url());

    let tenant = Uuid::new_v4();

    let task =
        Task::async_fn_typed(|_input: &i32, _ctx| Box::pin(async move { Ok(Box::new(0_i32)) }));
    let pipeline = Pipeline::new("tenant test")
        .with_name("tenant_pipeline")
        .with_task(task);

    let ctx = build_test_task_context_with_tenant(Some(tenant)).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("execute should succeed");

    assert!(wait_for_count(&captured, 4, Duration::from_secs(5)).await);
    mock.assert_async().await;

    let bodies = captured_bodies(&captured);
    for body in &bodies {
        let p = &body["properties"];
        assert_eq!(
            p["tenant_id"],
            tenant.to_string(),
            "tenant_id should be the formatted UUID, got {:?}",
            p["tenant_id"]
        );
    }
}

// ---------------------------------------------------------------------------
// 4.3.2 — Variant matrix: each Task family maps to the right event-name prefix
// ---------------------------------------------------------------------------

async fn drive_pipeline_with_task(
    server_url: &str,
    pipeline: Pipeline,
    inputs: Vec<Arc<dyn Value>>,
) -> Vec<String> {
    // Caller is responsible for `IsolatedEnv` setup outside this fn so
    // the env stays installed across the whole helper.
    let _ = server_url;
    let ctx = build_test_task_context_with_tenant(None).await;
    let _ = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("execute should succeed");
    Vec::new() // placeholder — caller reads the captured list
}

#[tokio::test]
#[serial]
async fn task_type_strings_match_python_for_each_variant() {
    // Async (Coroutine).
    {
        let mut server = Server::new_async().await;
        let (mock, captured) = recording_mock(&mut server);
        let _env = IsolatedEnv::install(&server.url());

        let task = Task::async_fn_typed(|_: &i32, _| Box::pin(async move { Ok(Box::new(0_i32)) }));
        let pipeline = Pipeline::new("v")
            .with_name("variant_pipeline")
            .with_task(task);
        let _ = drive_pipeline_with_task(&server.url(), pipeline, vec![Arc::new(0_i32)]).await;

        assert!(wait_for_count(&captured, 4, Duration::from_secs(5)).await);
        mock.assert_async().await;
        let names = event_names(&captured);
        assert_eq!(
            names,
            vec![
                "Pipeline Run Started",
                "Coroutine Task Started",
                "Coroutine Task Completed",
                "Pipeline Run Completed",
            ]
        );
    }
    // Sync (Function).
    {
        let mut server = Server::new_async().await;
        let (mock, captured) = recording_mock(&mut server);
        let _env = IsolatedEnv::install(&server.url());

        let task = Task::sync_typed(|x: &i32, _| Ok(Box::new(*x)));
        let pipeline = Pipeline::new("v")
            .with_name("variant_pipeline")
            .with_task(task);
        let _ = drive_pipeline_with_task(&server.url(), pipeline, vec![Arc::new(0_i32)]).await;

        assert!(wait_for_count(&captured, 4, Duration::from_secs(5)).await);
        mock.assert_async().await;
        let names = event_names(&captured);
        assert_eq!(
            names,
            vec![
                "Pipeline Run Started",
                "Function Task Started",
                "Function Task Completed",
                "Pipeline Run Completed",
            ]
        );
    }
    // SyncIter (Generator).
    {
        let mut server = Server::new_async().await;
        let (mock, captured) = recording_mock(&mut server);
        let _env = IsolatedEnv::install(&server.url());

        let task = Task::sync_iter_typed(|_: &i32, _| Ok(std::iter::empty::<Box<i32>>()));
        let pipeline = Pipeline::new("v")
            .with_name("variant_pipeline")
            .with_task(task);
        let _ = drive_pipeline_with_task(&server.url(), pipeline, vec![Arc::new(0_i32)]).await;

        assert!(wait_for_count(&captured, 4, Duration::from_secs(5)).await);
        mock.assert_async().await;
        let names = event_names(&captured);
        assert_eq!(
            names,
            vec![
                "Pipeline Run Started",
                "Generator Task Started",
                "Generator Task Completed",
                "Pipeline Run Completed",
            ]
        );
    }
    // AsyncStream (Async Generator).
    {
        let mut server = Server::new_async().await;
        let (mock, captured) = recording_mock(&mut server);
        let _env = IsolatedEnv::install(&server.url());

        use futures::stream;
        let task = Task::async_stream_typed(|_: &i32, _| Ok(stream::empty::<Box<i32>>()));
        let pipeline = Pipeline::new("v")
            .with_name("variant_pipeline")
            .with_task(task);
        let _ = drive_pipeline_with_task(&server.url(), pipeline, vec![Arc::new(0_i32)]).await;

        assert!(wait_for_count(&captured, 4, Duration::from_secs(5)).await);
        mock.assert_async().await;
        let names = event_names(&captured);
        assert_eq!(
            names,
            vec![
                "Pipeline Run Started",
                "Async Generator Task Started",
                "Async Generator Task Completed",
                "Pipeline Run Completed",
            ]
        );
    }
}

// ---------------------------------------------------------------------------
// 4.3.3 — Error path: exactly one Task Started despite retries (decision 7)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn pipeline_run_errored_fires_after_retries_exhausted() {
    let mut server = Server::new_async().await;
    let (mock, captured) = recording_mock(&mut server);
    let _env = IsolatedEnv::install(&server.url());

    let task = Task::async_fn_typed(|_input: &i32, _ctx| {
        Box::pin(async move { Err::<Box<i32>, TaskError>("always fails".into()) })
    });
    let policy = RetryPolicy::Limited {
        max_attempts: std::num::NonZeroU32::new(2).expect("2 is non-zero"),
        delay: RetryDelay::Constant(Duration::from_millis(1)),
    };
    let pipeline = Pipeline::new("error path")
        .with_name("err_pipeline")
        .with_retry(policy)
        .with_task(task);

    let ctx = build_test_task_context_with_tenant(None).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let res = execute(&pipeline, inputs, ctx, &NoopWatcher).await;
    assert!(matches!(res, Err(ExecutionError::TaskFailed { .. })));

    assert!(
        wait_for_count(&captured, 4, Duration::from_secs(5)).await,
        "expected 4 telemetry POSTs, got {:?}",
        event_names(&captured)
    );
    mock.assert_async().await;

    let names = event_names(&captured);
    // Locked decision 7: exactly ONE Task Started, even though we
    // retried twice. The Errored event fires once after retries
    // exhausted.
    assert_eq!(
        names,
        vec![
            "Pipeline Run Started",
            "Coroutine Task Started",
            "Coroutine Task Errored",
            "Pipeline Run Errored",
        ],
        "expected once-per-task semantics with no error string in payload"
    );

    // Sub-doc 03/04 §2.2 / 03/05 §2.3 — neither Errored event carries
    // an `error` string property (Python parity).
    let bodies = captured_bodies(&captured);
    let task_errored = &bodies[2];
    assert!(
        task_errored["properties"].get("error").is_none(),
        "Coroutine Task Errored payload must not carry `error` (Python parity)"
    );
    let pipeline_errored = &bodies[3];
    assert!(
        pipeline_errored["properties"].get("error").is_none(),
        "Pipeline Run Errored payload must not carry `error` (Python parity)"
    );
}

// ---------------------------------------------------------------------------
// 4.3.4 — Opt-out via TELEMETRY_DISABLED produces zero POSTs
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn telemetry_disabled_emits_zero_events() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .expect(0)
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());
    // SAFETY: still inside the same `#[serial]` section as `_env`'s
    //   `install`. `_env`'s Drop will remove TELEMETRY_DISABLED on
    //   exit.
    unsafe {
        std::env::set_var("TELEMETRY_DISABLED", "1");
    }

    let task = Task::async_fn_typed(|_: &i32, _| Box::pin(async move { Ok(Box::new(0_i32)) }));
    let pipeline = Pipeline::new("opt-out")
        .with_name("optout_pipeline")
        .with_task(task);

    let ctx = build_test_task_context_with_tenant(None).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("execute should succeed");

    // Wait a generous window to confirm no late dispatch sneaks in.
    tokio::time::sleep(Duration::from_millis(500)).await;
    mock.assert_async().await;
}

// ---------------------------------------------------------------------------
// 4.3.5 — Fire-and-forget timing: a stalled proxy must not block execute()
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn stalled_proxy_does_not_block_pipeline() {
    let mut server = Server::new_async().await;
    // Stall every response by sleeping inside the chunked-body writer.
    // Each `send_telemetry` call is fire-and-forget and dispatches on a
    // detached task — execute()'s wall-clock time must NOT scale with
    // proxy latency.
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_chunked_body(|w| {
            std::thread::sleep(Duration::from_millis(2_000));
            w.write_all(b"{}")
        })
        .expect_at_least(1)
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());

    let task = Task::async_fn_typed(|_: &i32, _| Box::pin(async move { Ok(Box::new(0_i32)) }));
    let pipeline = Pipeline::new("stalled")
        .with_name("stalled_pipeline")
        .with_task(task);

    let ctx = build_test_task_context_with_tenant(None).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];

    let start = tokio::time::Instant::now();
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("execute should succeed");
    let elapsed = start.elapsed();

    // Generous bound — the no-op pipeline + 4 fire-and-forget dispatches
    // should clock well under 100ms on any reasonable machine. The
    // proxy stalls 2s per response.
    assert!(
        elapsed < Duration::from_millis(500),
        "execute() blocked on stalled proxy: {elapsed:?} > 500ms (proxy stalls 2s)"
    );

    // We don't care whether requests eventually complete — the contract
    // is fire-and-forget.
    let _ = mock;
}
