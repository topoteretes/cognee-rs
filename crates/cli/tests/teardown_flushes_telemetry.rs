#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test for the CLI's telemetry flush.
//!
//! `send_telemetry` dispatches a detached task onto the **current** runtime and
//! returns. Every command builds its own runtime and drops it, so a flush issued
//! after that — which is where `main` used to do it, on a freshly built runtime —
//! has nothing left to wait for: the POST was cancelled with the runtime that
//! owned it, and the in-flight counter went to zero with it. The flush then
//! returned "drained" immediately and the event never arrived. Measured against
//! the stub below: **0 of 1 delivered**, while looking exactly like a working
//! flush.
//!
//! The fix is that `teardown::run_command` flushes *inside* the command's runtime,
//! before it goes away. This test asserts the delivered count both ways round, so
//! it fails if the flush ever moves back out.
#![cfg(feature = "telemetry")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cognee::{ComponentManager, ConfigManager, Settings};
use serial_test::serial;

/// A stub collector that counts complete POSTs it has received.
///
/// Blocking, on its own OS thread, counting *arrived* requests server-side: an
/// in-runtime stub would be torn down along with the runtime under test, and
/// client-side state is exactly what cannot be trusted here.
struct Collector {
    url: String,
    arrived: Arc<AtomicUsize>,
}

impl Collector {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let arrived = Arc::new(AtomicUsize::new(0));
        let arrived_srv = Arc::clone(&arrived);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let counter = Arc::clone(&arrived_srv);
                std::thread::spawn(move || serve(stream, &counter));
            }
        });
        Self {
            url: format!("http://{addr}/"),
            arrived,
        }
    }

    /// The count, after giving the stub's threads a moment to finish reading.
    fn arrived_settled(&self) -> usize {
        for _ in 0..100 {
            if self.arrived.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.arrived.load(Ordering::SeqCst)
    }
}

/// Read one request (headers + body), answer 200, and count it only once the
/// **whole body** has arrived — a half-written POST is not a delivered event,
/// which is the failure mode under test.
fn serve(mut stream: TcpStream, arrived: &AtomicUsize) {
    let read_half = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(read_half);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut content_length = 0usize;
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
    arrived.fetch_add(1, Ordering::SeqCst);
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let _ = stream.flush();
}

/// Point telemetry at `url` and make sure it is enabled for this test.
fn configure(url: &str) {
    // SAFETY: single-threaded section of a `#[serial]` test.
    unsafe {
        std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
        std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", url);
        std::env::remove_var("TELEMETRY_DISABLED");
        std::env::remove_var("ENV");
    }
}

/// A cold manager: the teardown must not need a warm one, and this test is about
/// the flush, not about closing anything.
fn cold_manager(root: &std::path::Path) -> Arc<ComponentManager> {
    let settings = Settings {
        system_root_directory: root.join("system").to_string_lossy().into_owned(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        relational_db_url: format!(
            "sqlite:{}?mode=rwc",
            root.join("cognee.db").to_string_lossy()
        ),
        ..Settings::default()
    };
    Arc::new(ComponentManager::new(ConfigManager::new(settings)))
}

/// The event a command dispatched must have left the process by the time
/// `run_command` returns.
#[test]
#[serial]
fn the_command_runtime_flushes_before_it_goes_away() {
    let collector = Collector::start();
    configure(&collector.url);
    let dir = tempfile::tempdir().expect("tempdir");
    let cm = cold_manager(dir.path());

    // Warm the telemetry HTTP client before the measured run. The client is a
    // process-wide `Lazy<reqwest::Client>`, and its first request pays connector
    // and DNS-resolver start-up — hundreds of milliseconds on a loaded machine.
    // `TELEMETRY_FLUSH_TIMEOUT` is 500ms *by design* (an exit code must never
    // wait on analytics), so without this the test measures "can reqwest cold
    // start in 500ms" instead of "does the teardown flush what the command
    // dispatched", and fails on a busy host while passing on an idle one.
    {
        let warm = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("warm-up runtime");
        warm.block_on(async {
            cognee::cognee_telemetry::send_telemetry("cognee.test.warmup", "test-user", None);
            cognee::cognee_telemetry::flush(Duration::from_secs(5)).await;
        });
    }
    let baseline = collector.arrived_settled();

    let outcome: Result<(), _> = cognee_cli::teardown::run_command(Arc::clone(&cm), async {
        // Exactly what a command does mid-run: fire and forget.
        cognee::cognee_telemetry::send_telemetry("cognee.test.cli_teardown", "test-user", None);
        Ok(())
    });
    outcome.expect("the command itself succeeds");

    assert_eq!(
        collector.arrived_settled() - baseline,
        1,
        "the event dispatched inside the command must be delivered before the \
         runtime is dropped — a flush issued after that has nothing to wait for"
    );
}

/// The control: the same event, with the runtime dropped first and the flush
/// issued afterwards on a fresh one — which is what `main` used to do, and which
/// delivers nothing.
///
/// Asserted rather than merely described, so the two halves of the measurement
/// live in the same file: 0 of 1 the old way, 1 of 1 the new way.
#[test]
#[serial]
fn a_flush_after_the_runtime_is_gone_delivers_nothing() {
    let collector = Collector::start();
    configure(&collector.url);

    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            cognee::cognee_telemetry::send_telemetry("cognee.test.too_late", "test-user", None);
        });
        drop(rt); // the command runtime going away, as it did before the fix
    }

    // A fresh runtime, a flush on it: reports success, delivers nothing.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let drained = rt.block_on(cognee::cognee_telemetry::flush(Duration::from_millis(500)));
    assert!(
        drained,
        "the misleading part: with the queue counter already zeroed, the flush \
         reports a clean drain"
    );

    // Give it the same settling window the positive case gets before concluding.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        collector.arrived.load(Ordering::SeqCst),
        0,
        "nothing can be delivered once the runtime that owned the POST is gone"
    );
}

/// A deferred teardown must still flush.
///
/// `defer_teardown` exists so `run-sequence` can replay many commands against one
/// warm manager without paying a re-warm per step. It postpones the *close* — but
/// the flush is not postponable: each step's POSTs are detached on that step's
/// runtime, which `run_command` shuts down on the way out, so anything not waited
/// for there is cancelled and no later flush can recover it.
///
/// Conflating the two meant a whole sequence file delivered **zero** events while
/// this module's header claimed the defect was fixed. The bug was invisible to the
/// test above, which only covers the non-deferred path.
#[test]
#[serial]
fn a_deferred_teardown_still_flushes_the_step_it_dispatched() {
    let collector = Collector::start();
    configure(&collector.url);
    let dir = tempfile::tempdir().expect("tempdir");
    let cm = cold_manager(dir.path());

    // Warm the client outside the measured window — see the note in the first
    // test: the first POST in a process pays connector start-up, and
    // TELEMETRY_FLUSH_TIMEOUT is deliberately 500ms.
    {
        let warm = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("warm-up runtime");
        warm.block_on(async {
            cognee::cognee_telemetry::send_telemetry("cognee.test.warmup", "test-user", None);
            cognee::cognee_telemetry::flush(Duration::from_secs(5)).await;
        });
    }
    let baseline = collector.arrived_settled();

    // Exactly the `run-sequence` shape: a guard held across the step dispatch.
    let deferred = cognee_cli::teardown::defer_teardown();
    let outcome: Result<(), _> = cognee_cli::teardown::run_command(Arc::clone(&cm), async {
        cognee::cognee_telemetry::send_telemetry("cognee.test.deferred_step", "test-user", None);
        Ok(())
    });
    outcome.expect("the step itself succeeds");
    drop(deferred);

    assert_eq!(
        collector.arrived_settled() - baseline,
        1,
        "a deferred step must still flush its own telemetry: the close is \
         postponable, the POSTs are not — they die with the step's runtime"
    );
}
