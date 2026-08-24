#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test for `cognee_telemetry::flush`.
//!
//! `send_telemetry` spawns a detached task and returns, so a process that exits
//! (or a runtime that is dropped) straight afterwards discards whatever has not
//! yet been written to the socket. The lost event is usually the most interesting
//! one: the last thing a process reports is what it was doing when it stopped.
//!
//! The collector here is a blocking stub on its own OS thread, counting *arrived*
//! requests. That is deliberate on both counts: an in-runtime stub would be torn
//! down together with the runtime under test, and counting arrivals server-side
//! avoids any inference from client-side state.
#![cfg(feature = "telemetry")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serial_test::serial;

/// A stub collector that counts complete POSTs it has received.
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
                let arrived_conn = Arc::clone(&arrived_srv);
                std::thread::spawn(move || serve(stream, &arrived_conn));
            }
        });

        Self {
            url: format!("http://{addr}/"),
            arrived,
        }
    }

    fn arrived(&self) -> usize {
        self.arrived.load(Ordering::SeqCst)
    }
}

/// Read one request (headers + body) and answer 200. The counter is bumped only
/// after the **whole body** has been read, so a half-written POST does not count
/// as delivered — which is exactly the failure mode under test.
fn serve(mut stream: TcpStream, arrived: &AtomicUsize) {
    let read_half = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(read_half);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
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
}

/// Point telemetry at `url` and make sure it is enabled for this test.
///
/// The proxy override is only honoured when `COGNEE_TELEMETRY_INTEGRATION_TEST`
/// is set (see `env::proxy_url`), which is exactly what this is. `ENV=test`
/// disables telemetry outright and the harness may well be running with it set,
/// so it is cleared too — hence `#[serial]` on every test in this file.
fn configure(url: &str) {
    // SAFETY: single-threaded section of a `#[serial]` test; no other thread is
    // reading the environment at this point.
    unsafe {
        std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
        std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", url);
        std::env::remove_var("TELEMETRY_DISABLED");
        std::env::remove_var("ENV");
    }
}

/// Wait for any POST left over from another test in this process to finish.
///
/// The in-flight counter is process-global (the dispatcher is a free function
/// with no owner), so a POST still running from a previous case would make the
/// next `flush` wait for *it* — a real property of the design, but cross-test
/// noise here. Draining first keeps each case measuring only its own event.
fn settle() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let _ = cognee_telemetry::flush(Duration::from_secs(10)).await;
    });
}

/// A dispatched event followed by an **immediate** runtime shutdown is lost; the
/// same event followed by `flush` arrives.
///
/// Both halves run in one test so the delivery counts are directly comparable.
/// Measured: 0 of 1 without the flush, 1 of 1 with it.
#[test]
#[serial]
fn flush_is_what_makes_the_last_event_arrive() {
    settle();
    let collector = Collector::start();
    configure(&collector.url);

    // -- without flush: spawn, then drop the runtime immediately ------------
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            cognee_telemetry::send_telemetry("cognee.test.no_flush", "test-user", None);
        });
        // Dropping the runtime here is the process-exit analogue.
        drop(rt);
    }
    let without_flush = collector.arrived();

    // -- with flush --------------------------------------------------------
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let drained = rt.block_on(async {
            cognee_telemetry::send_telemetry("cognee.test.with_flush", "test-user", None);
            cognee_telemetry::flush(Duration::from_secs(5)).await
        });
        drop(rt);
        assert!(drained, "flush must report the queue drained within 5s");
    }

    // Give the stub's per-connection thread a moment to finish counting; the
    // assertion is on `flush`, so this window only covers the stub itself.
    for _ in 0..100 {
        if collector.arrived() > without_flush {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let with_flush = collector.arrived() - without_flush;

    assert_eq!(
        without_flush, 0,
        "precondition: an immediate runtime shutdown must lose the event — if this \
         ever becomes non-zero, the dispatcher stopped being fire-and-forget and \
         the flush's value needs re-deriving"
    );
    assert_eq!(
        with_flush, 1,
        "flush() must get the dispatched event delivered before teardown"
    );
}

/// `flush` on an idle process returns immediately and reports drained — it must
/// not add a fixed delay to every teardown.
#[tokio::test]
#[serial]
async fn flush_on_an_idle_queue_returns_at_once() {
    // Drain anything another case left behind; the point here is the *idle* path.
    let _ = cognee_telemetry::flush(Duration::from_secs(10)).await;
    let started = std::time::Instant::now();
    assert!(cognee_telemetry::flush(Duration::from_secs(5)).await);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "an idle flush must not wait, took {:?}",
        started.elapsed()
    );
}

/// A collector that never answers must not hang the exit: `flush` honours its
/// timeout and reports `false` rather than blocking forever.
#[test]
#[serial]
fn flush_is_bounded_when_the_collector_blackholes() {
    settle();
    // A listener that accepts and then answers nothing for `STALL`, which is an
    // order of magnitude longer than the flush window below — long enough that
    // the POST cannot possibly complete inside it, short enough that it does not
    // poison the other cases in this process (the counter is global, and the
    // reqwest client's timeout is fixed at its first use, so a genuinely
    // unbounded stall here would be inherited by whichever test ran later).
    const STALL: Duration = Duration::from_secs(3);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            std::thread::spawn(move || {
                std::thread::sleep(STALL);
                drop(stream);
            });
        }
    });
    configure(&format!("http://{addr}/"));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let (drained, elapsed) = rt.block_on(async {
        cognee_telemetry::send_telemetry("cognee.test.blackhole", "test-user", None);
        let started = std::time::Instant::now();
        let drained = cognee_telemetry::flush(Duration::from_millis(300)).await;
        (drained, started.elapsed())
    });

    assert!(
        elapsed < Duration::from_secs(3),
        "flush must honour its own timeout, not the HTTP timeout; took {elapsed:?}"
    );
    assert!(
        !drained,
        "a blackholed collector must be reported as not drained, not waited out"
    );

    // Leave the process clean for whichever case runs next.
    settle();
}
