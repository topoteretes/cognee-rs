//! End-to-end integration test: spans emitted through cognee's OTEL bring-up
//! must reach an OTLP/gRPC collector. We stand up an in-process tonic server
//! implementing `opentelemetry_proto::collector::trace::v1::TraceService`,
//! point `OTEL_EXPORTER_OTLP_ENDPOINT` at it, run a small instrumented
//! function, drop the guard, and assert the collector received the span we
//! expected.
//!
//! Gated on the `telemetry` feature — without it `init_telemetry` is a
//! noop and there is nothing to test.
//!
//! Run:
//! ```bash
//! cargo test -p cognee-observability --features telemetry \
//!     --test otel_export -- --nocapture --test-threads=1
//! ```

#![cfg(feature = "telemetry")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cognee_observability::{SettingsView, init_telemetry};

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify, oneshot};
use tonic::{Request, Response, Status, transport::Server};
use tracing_subscriber::{Registry, layer::SubscriberExt};

#[derive(Default, Clone)]
struct CapturedExports {
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    arrived: Arc<Notify>,
}

struct MockTraceService {
    captured: CapturedExports,
}

#[tonic::async_trait]
impl TraceService for MockTraceService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let req = request.into_inner();
        self.captured.requests.lock().await.push(req);
        self.captured.arrived.notify_waiters();
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

async fn spawn_mock_collector() -> (
    CapturedExports,
    SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0 — port 0 must always be available on loopback");
    let addr = listener
        .local_addr()
        .expect("the listener was just bound, so local_addr must exist");

    let captured = CapturedExports::default();
    let svc = MockTraceService {
        captured: captured.clone(),
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let handle = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(TraceServiceServer::new(svc))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    (captured, addr, shutdown_tx, handle)
}

#[tracing::instrument(name = "test.span", fields(foo = "bar"))]
fn emit_span() {}

struct StaticSettings {
    endpoint: String,
}

impl SettingsView for StaticSettings {
    fn tracing_enabled(&self) -> bool {
        true
    }
    fn service_name(&self) -> &str {
        "cognee"
    }
    fn otlp_endpoint(&self) -> &str {
        &self.endpoint
    }
    fn otlp_headers(&self) -> &str {
        ""
    }
    fn otlp_protocol(&self) -> &str {
        "grpc"
    }
    fn span_processor(&self) -> &str {
        "batch"
    }
    fn traces_sampler(&self) -> &str {
        ""
    }
    fn traces_sampler_arg(&self) -> &str {
        ""
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn spans_flow_to_otlp_collector() {
    let (captured, addr, shutdown_tx, server_task) = spawn_mock_collector().await;

    let endpoint = format!("http://{addr}");
    // SAFETY: tests in this file are serialised by `#[serial_test::serial]`
    // and cargo test runs each integration file in its own process, so this
    // env-var write does not race with other telemetry tests.
    unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint) };

    let settings = StaticSettings {
        endpoint: endpoint.clone(),
    };

    let (layer, guard) = init_telemetry::<Registry>(&settings).expect(
        "init_telemetry must succeed when telemetry feature is on and endpoint is reachable",
    );

    let subscriber = Registry::default().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_span();
    });

    let arrived = captured.arrived.clone();
    let notified = arrived.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();

    let flush_handle = tokio::task::spawn_blocking(move || {
        drop(guard);
    });

    tokio::time::timeout(Duration::from_secs(10), notified)
        .await
        .expect("collector did not receive any spans within 10s — flush/shutdown likely failed");
    let _ = flush_handle.await;

    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;

    let exports = captured.requests.lock().await;
    assert!(
        !exports.is_empty(),
        "collector received zero ExportTraceServiceRequests"
    );

    let mut found_span = false;
    let mut found_service_name = false;

    for export in exports.iter() {
        for resource_spans in &export.resource_spans {
            if let Some(resource) = &resource_spans.resource {
                for kv in &resource.attributes {
                    if kv.key == "service.name"
                        && let Some(any_value) = &kv.value
                        && let Some(
                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                                s,
                            ),
                        ) = &any_value.value
                    {
                        assert_eq!(
                            s, "cognee",
                            "service.name resource attribute must equal 'cognee', got '{s}'"
                        );
                        found_service_name = true;
                    }
                }
            }

            for scope_spans in &resource_spans.scope_spans {
                for span in &scope_spans.spans {
                    if span.name == "test.span" {
                        found_span = true;
                        let foo = span.attributes.iter().find(|kv| kv.key == "foo");
                        let foo_kv = foo.unwrap_or_else(|| {
                            panic!(
                                "span 'test.span' has no 'foo' attribute; attributes were: {:?}",
                                span.attributes
                            )
                        });
                        let foo_value = foo_kv
                            .value
                            .as_ref()
                            .and_then(|v| v.value.as_ref())
                            .expect("foo attribute has no value");
                        match foo_value {
                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => {
                                assert_eq!(s, "bar", "span attribute 'foo' must equal 'bar'");
                            }
                            other => {
                                panic!("span attribute 'foo' must be a string, got {other:?}")
                            }
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_span,
        "no span named 'test.span' found in captured exports: {exports:#?}"
    );
    assert!(
        found_service_name,
        "no resource attribute 'service.name' found in captured exports: {exports:#?}"
    );
}
