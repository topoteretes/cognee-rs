#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `ComponentManager::close` — the "every slot, not just
//! the database" half of topoteretes/cognee-rs#132.
//!
//! `close()` used to release the relational pool only, on the documented (and
//! measurably wrong) premise that the other components "release everything on
//! drop, so they are left untouched". They do release on drop — but the version
//! keyed cache in `ComponentManager` **is the last strong reference**, so a slot
//! that is never emptied is a destructor that never runs. "Left untouched" meant
//! "never released".
//!
//! Three independent observables are asserted here, none of which counts file
//! descriptors or threads — every audit number for those was taken with macOS
//! `lsof`/`ps -M` and would not port to CI:
//!
//! 1. `Arc::strong_count` per slot, which covers the components with no visible
//!    OS footprint (storage, embedding, llm, transcriber).
//! 2. Files on disk for the embedded graph (the `.wal` sidecar).
//! 3. Live accepted peers counted **server-side** by a stub HTTP server, for the
//!    `reqwest` connection pools inside the embedding and LLM engines. Counted by
//!    the server rather than by fds, and asserted after an awaited `close()`
//!    rather than after a sleep, because hyper evicts its own idle connections
//!    after ~90 s on its own — a timeout-based assertion would eventually pass
//!    for the wrong reason.
//!
//! Every test here runs on a **multi-thread** runtime, deliberately.
//! `cognee_database::close` finishes sqlx's close by waiting for the pool to
//! empty, and the straggling connection it waits on is handed back by a task
//! sqlx spawned. On a current-thread runtime that task cannot be scheduled while
//! the drain is waiting, so the drain times out and the sidecars survive — a test
//! then fails (or passes) for reasons unrelated to what it asserts. Every real
//! teardown path builds a multi-thread runtime (see `cognee_cli::teardown`), so
//! this matches production rather than working around it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee::cognee_llm::{Message, MessageRole};
use cognee::{ComponentManager, ConfigManager, PipelineContext, Settings};
use serial_test::serial;
use tempfile::TempDir;

/// Settings with every on-disk artefact under `root` and every network-backed
/// engine pointed at `endpoint`.
///
/// `Settings::default()` uses *relative* paths (`./.cognee_system`,
/// `sqlite:./cognee.db?mode=rwc`), which would write into the crate directory,
/// so all of it is redirected. The embedding/LLM providers are the
/// OpenAI-compatible HTTP ones: they build a real `reqwest` client (and therefore
/// a real connection pool) without performing any I/O at construction time.
fn settings(root: &std::path::Path, endpoint: &str) -> Settings {
    Settings {
        system_root_directory: root.join("system").to_string_lossy().into_owned(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        relational_db_url: format!(
            "sqlite:{}?mode=rwc",
            root.join("cognee.db").to_string_lossy()
        ),
        graph_database_provider: "ladybug".to_string(),
        graph_file_path: root
            .join("system")
            .join("graph")
            .to_string_lossy()
            .into_owned(),
        // In-memory, so the vector slot contributes no OS resource of its own —
        // deliberate: this test is about slot eviction, and the embedded vector
        // store was measured to hold nothing closable.
        vector_db_provider: "brute-force".to_string(),
        embedding_provider: "openai".to_string(),
        embedding_endpoint: format!("{endpoint}/v1/embeddings"),
        embedding_api_key: "sk-test".to_string(),
        embedding_model_name: "text-embedding-3-small".to_string(),
        embedding_dimensions: 3,
        llm_provider: "openai".to_string(),
        llm_model: "gpt-4o-mini".to_string(),
        llm_api_key: "sk-test".to_string(),
        llm_endpoint: endpoint.to_string(),
        ..Settings::default()
    }
}

/// Every cached slot must be emptied by `close()`, which is the only thing that
/// lets the component's destructor run at all.
///
/// The measurement is `Arc::strong_count` on a clone the test keeps: `2` means
/// "the cache still holds it", `1` means "the cache let go and this test's clone
/// is the last owner". Before the fix, six of the seven slots stay at `2`
/// (measured: storage, graph, vector, embedding, llm, transcriber — only the
/// relational connection dropped to `1`).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn close_empties_every_component_slot() {
    let dir = TempDir::new().expect("tempdir");
    // No server needed: none of these engines performs I/O at construction.
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));

    let storage = cm.storage().await.expect("storage");
    let database = cm.database().await.expect("database");
    let graph = cm.graph_db().await.expect("graph");
    let vector = cm.vector_db().await.expect("vector");
    let embedding = cm.embedding_engine().await.expect("embedding");
    let llm = cm.llm().await.expect("llm");
    let transcriber = cm
        .transcriber()
        .await
        .expect("transcriber")
        .expect("openai provider yields a transcriber");

    // Precondition: the cache holds a second reference to each one.
    for (what, count) in [
        ("storage", Arc::strong_count(&storage)),
        ("database", Arc::strong_count(&database)),
        ("graph", Arc::strong_count(&graph)),
        ("vector", Arc::strong_count(&vector)),
        ("embedding", Arc::strong_count(&embedding)),
        ("llm", Arc::strong_count(&llm)),
        ("transcriber", Arc::strong_count(&transcriber)),
    ] {
        assert!(
            count >= 2,
            "precondition: the {what} slot must be warm (strong_count {count} < 2)"
        );
    }

    cm.close().await;

    for (what, count) in [
        ("storage", Arc::strong_count(&storage)),
        ("database", Arc::strong_count(&database)),
        ("graph", Arc::strong_count(&graph)),
        ("vector", Arc::strong_count(&vector)),
        ("embedding", Arc::strong_count(&embedding)),
        ("llm", Arc::strong_count(&llm)),
        ("transcriber", Arc::strong_count(&transcriber)),
    ] {
        assert_eq!(
            count, 1,
            "close() must release the {what} slot — this test's clone should be \
             the last owner, but strong_count is {count}"
        );
    }
}

/// `close()` on a manager that never warmed, and a second `close()`, are both
/// no-ops rather than panics — required because the two teardown tiers in
/// `HandleState` can each reach it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn close_is_idempotent_and_safe_on_a_cold_manager() {
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));
    cm.close().await;

    let _ = cm.graph_db().await.expect("graph");
    cm.close().await;
    cm.close().await;

    // Still reusable, just cold: the next access rebuilds.
    let _ = cm.graph_db().await.expect("re-warm after close");
    cm.close().await;
}

/// The on-disk measurement: after `close()` the embedded graph has no `.wal`
/// sidecar left under the system root.
///
/// This is the real-OS-resource half of the P1 fix, and it fails before it: the
/// manager's cache is the last owner of the `LadybugAdapter`, so nothing runs the
/// checkpoint and the `.wal` (measured at ~660 KB for this workload) survives.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn close_leaves_no_graph_wal_on_disk() {
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));

    {
        let graph = cm.graph_db().await.expect("graph");
        let nodes: Vec<_> = (0..500)
            .map(|i| {
                serde_json::json!({
                    "id": format!("n{i}"),
                    "name": format!("Node {i}"),
                    "type": "TestNode",
                    "properties": {"idx": i, "pad": "x".repeat(64)},
                })
            })
            .collect();
        graph.add_nodes_raw(nodes).await.expect("add_nodes_raw");

        let wal = wal_files(dir.path());
        assert!(
            !wal.is_empty(),
            "precondition: the writes must leave an un-checkpointed WAL under {}",
            dir.path().display()
        );
    }

    cm.close().await;

    let leftover = wal_files(dir.path());
    assert!(
        leftover.is_empty(),
        "close() must leave no graph WAL behind, found: {leftover:?}"
    );
}

/// Collect every `*.wal` under `root` (the embedded graph's sidecar). SQLite's
/// `-wal`/`-shm` use a different suffix and are covered by #135's tests.
fn wal_files(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("wal") {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found
}

/// Counts connections that a client has open to a stub HTTP/1.1 server, from the
/// server's side.
///
/// Server-side counting is the portable observable: it needs no `lsof`, behaves
/// identically on macOS and Linux, and — unlike an fd count — cannot be confused
/// by a descriptor the runtime is holding for another reason. `live` is
/// incremented on accept and decremented when that peer reaches EOF, so it is
/// exactly "how many keep-alive connections is the client still holding open".
struct PeerCountingServer {
    endpoint: String,
    live: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
}

impl PeerCountingServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let live = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));

        let live_srv = Arc::clone(&live);
        let accepted_srv = Arc::clone(&accepted);
        // A plain OS thread per connection, deliberately: this must not depend on
        // the tokio runtime under test, which is being torn down by `close()`.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                accepted_srv.fetch_add(1, Ordering::SeqCst);
                live_srv.fetch_add(1, Ordering::SeqCst);
                let live_conn = Arc::clone(&live_srv);
                std::thread::spawn(move || {
                    serve_keepalive(stream);
                    live_conn.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        Self {
            endpoint: format!("http://{addr}/v1"),
            live,
            accepted,
        }
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Serve keep-alive HTTP/1.1 requests on one connection until the peer closes
/// it, answering both the embeddings and the chat-completions shapes.
///
/// Returning from this function is the EOF signal the live-peer counter uses, so
/// it must loop until the client actually goes away rather than closing after one
/// response — a `Connection: close` server would make the assertion pass without
/// the fix.
fn serve_keepalive(mut stream: TcpStream) {
    let read_half = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(read_half);
    loop {
        // Request line + headers.
        let mut content_length = 0usize;
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break; // peer closed — the connection is really gone
        }
        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let trimmed = header.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }

        let body = if path.contains("embeddings") {
            r#"{"data":[{"embedding":[0.0,0.0,1.0],"index":0}],"model":"stub","usage":{"prompt_tokens":1,"total_tokens":1}}"#.to_string()
        } else {
            r#"{"id":"stub","object":"chat.completion","created":0,"model":"stub","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#.to_string()
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            body.len(),
            body
        );
        if stream.write_all(response.as_bytes()).is_err() {
            return;
        }
        let _ = stream.flush();
    }
}

/// `close()` must release the `reqwest` connection pools the HTTP-backed engines
/// hold, not just their Rust-side memory.
///
/// Before the fix the embedding and LLM engines stay in the cache forever, so
/// their idle keep-alive sockets stay open: measured 2 live peers before
/// `close()` and still 2 after. After the fix: 2 before, 0 after.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn close_releases_the_http_client_connection_pools() {
    let server = PeerCountingServer::start();
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(dir.path(), &server.endpoint)));

    // One real request per engine, so each one's pool actually has a socket.
    let embedding = cm.embedding_engine().await.expect("embedding");
    embedding.embed(&["hello"]).await.expect("embed");
    let llm = cm.llm().await.expect("llm");
    llm.generate(
        vec![Message {
            role: MessageRole::User,
            content: "hi".to_string(),
        }],
        None,
    )
    .await
    .expect("generate");

    assert_eq!(
        server.accepted(),
        2,
        "precondition: one connection per engine"
    );
    assert_eq!(
        server.live(),
        2,
        "precondition: both connections are held open by the clients' idle pools"
    );

    // Drop this test's clones first: `close()` releases the cache's reference,
    // and the pool goes away when the last one does.
    drop(embedding);
    drop(llm);
    cm.close().await;

    // Server-side EOF handling is a different thread; give it a bounded window.
    // The assertion is still on `close()`, not on a timeout: without the fix the
    // count never drops, so this loop always runs to exhaustion and fails.
    let mut live = server.live();
    for _ in 0..200 {
        if live == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        live = server.live();
    }
    assert_eq!(
        live, 0,
        "close() must release the HTTP connection pools; {live} peer(s) still open"
    );

    drop(server);
}

// ── Ordering under a bounded teardown, and the release tier ──────────────────

/// Builds a [`cognee_graph::MockGraphDB`] whose `close()` never returns, standing
/// in for a slot whose teardown outlives the caller's patience: a big embedded
/// checkpoint, or a pool waiting on a connection that will not come back.
struct StuckGraphFactory;

#[async_trait::async_trait]
impl cognee::GraphDbFactory for StuckGraphFactory {
    fn provider(&self) -> &str {
        "stuck"
    }
    async fn build(
        &self,
        _ctx: &cognee::BackendBuildContext,
    ) -> Result<Arc<dyn cognee::graph::GraphDBTrait>, cognee::ComponentError> {
        Ok(Arc::new(cognee_graph::MockGraphDB::hanging_on_close()))
    }
}

/// A manager whose graph provider hangs on close, warm on every slot the test needs.
async fn warm_manager_with_stuck_graph(root: &std::path::Path) -> ComponentManager {
    let mut registry = cognee::ComponentRegistry::with_builtins();
    registry.register_graph(Arc::new(StuckGraphFactory));
    let mut s = settings(root, "http://127.0.0.1:1/v1");
    s.graph_database_provider = "stuck".to_string();
    let cm = ComponentManager::with_registry(ConfigManager::new(s), registry);
    cm.database().await.expect("database");
    cm.graph_db().await.expect("graph");
    cm
}

/// The relational pool must be closed **first**, so a caller that bounds the
/// teardown does not lose it to a slot that hangs.
///
/// Callers do bound it: `cognee-cli` wraps `close()` in a `timeout` because a pool
/// close waits for connections to come back and a command's runtime may have been
/// dropped with one checked out. With the relational close queued behind the graph
/// checkpoint, an expired budget skipped exactly the close that #132 was about,
/// leaving the `-wal`/`-shm` sidecars on disk — green, and still leaking.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_timed_out_close_still_released_the_relational_pool() {
    let dir = TempDir::new().expect("tempdir");
    let cm = warm_manager_with_stuck_graph(dir.path()).await;

    let db = dir.path().join("cognee.db");
    let wal = db.with_file_name("cognee.db-wal");
    let shm = db.with_file_name("cognee.db-shm");
    assert!(
        wal.exists() && shm.exists(),
        "precondition: a warm SQLite pool has both WAL sidecars"
    );

    // The graph close never returns, so the timeout is guaranteed to fire: this
    // is not a race against a slow machine.
    let timed_out = tokio::time::timeout(std::time::Duration::from_millis(300), cm.close())
        .await
        .is_err();
    assert!(
        timed_out,
        "precondition: the stuck graph must exhaust the budget"
    );

    assert!(
        !wal.exists(),
        "the relational close must happen before the slot that hangs — -wal survived"
    );
    assert!(!shm.exists(), "-shm survived a timed-out close");
}

/// `release()` — the implicit tier — must not close a component another owner is
/// still holding.
///
/// A store's `close()` mutates state behind the shared `Arc`, so it is visible to
/// every clone: an embedded graph empties its inner handle, a pool flags itself
/// closed. A finalizer running `close()` therefore breaks an operation that is
/// still in flight, which is why the implicit tier only closes what it holds the
/// last reference to.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn release_leaves_a_component_a_survivor_still_holds() {
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));

    // Stand in for an operation in flight: it owns a clone of the graph and the
    // relational connection for its duration.
    let graph = cm.graph_db().await.expect("graph");
    let database = cm.database().await.expect("database");

    cm.release().await;

    // The survivor's handles still work.
    graph
        .is_empty()
        .await
        .expect("release() must not close a graph an in-flight op is holding");
    database
        .ping()
        .await
        .expect("release() must not close a pool an in-flight op is holding");

    // And the cache really was evicted — this is a release, not a no-op.
    assert_eq!(
        Arc::strong_count(&graph),
        1,
        "the cache must have given up its graph reference"
    );
    assert_eq!(Arc::strong_count(&database), 1);
}

/// The other half of the tier: with no survivor, `release()` releases everything
/// `close()` would — including the SQLite sidecars, which is the whole point of
/// the finalizer path (a Python handle collected without `close()`).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn release_closes_what_nobody_else_is_using() {
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));
    cm.database().await.expect("database");
    cm.graph_db().await.expect("graph");

    let wal = dir.path().join("cognee.db-wal");
    assert!(wal.exists(), "precondition: warm pool has a -wal");

    cm.release().await;

    assert!(
        !wal.exists(),
        "with no other owner, release() must close the pool — otherwise a \
         garbage-collected handle leaks its sidecars, which is #132 again"
    );
    assert!(
        wal_files(dir.path()).is_empty(),
        "the graph .wal must be checkpointed away too: {:?}",
        wal_files(dir.path())
    );
}

/// `has_cached_components()` must see a **partly** warmed manager.
///
/// Warming is slot-by-slot and can fail in the middle: a bad `llm_api_key` fails
/// after the SQLite pool and the graph are already cached. A probe that reported
/// "nothing cached" for that state let the finalizer skip the teardown entirely,
/// leaving an open database behind.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn has_cached_components_sees_a_partly_warmed_manager() {
    let dir = TempDir::new().expect("tempdir");
    let cm = ComponentManager::new(ConfigManager::new(settings(
        dir.path(),
        "http://127.0.0.1:1/v1",
    )));
    assert!(
        !cm.has_cached_components(),
        "a cold manager has nothing cached"
    );

    cm.database().await.expect("database");
    assert!(
        cm.has_cached_components(),
        "one warm slot is enough to have something to release"
    );

    cm.close().await;
    assert!(
        !cm.has_cached_components(),
        "close() empties every slot, so there is nothing left to release"
    );
}
