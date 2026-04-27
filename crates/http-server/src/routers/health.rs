//! Health router — `GET /health` (shallow) and `GET /health/detailed`.
//!
//! Both endpoints are public (no `AuthenticatedUser` extractor).  They bypass
//! `ApiError` and return bespoke JSON shapes per the Python parity spec in
//! [`routers/health.md`](../../../docs/http-server/routers/health.md).
//!
//! Python parity notes (do NOT change):
//! - Shallow returns 503 only for UNHEALTHY; DEGRADED stays 200.
//! - Detailed returns 503 for DEGRADED *or* UNHEALTHY.
//! - Shallow failure key: `reason`; detailed failure key: `error`.
//! - Shallow health field: `health`; detailed health field: `status`.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use crate::state::AppState;

// ─── HealthStatus enum ────────────────────────────────────────────────────────

/// Aggregate / per-component health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

// ─── HealthChecker trait ──────────────────────────────────────────────────────

/// Abstraction for the cognee health-check machinery.
///
/// P0 uses `MockHealthChecker`; later phases wire in
/// `cognee_lib::health::DefaultHealthChecker` which fans out to all backends.
#[async_trait]
pub trait HealthChecker: Send + Sync {
    /// Run the health checks and return a report.
    ///
    /// `detailed` controls whether non-critical components (LLM, embedding)
    /// are also checked.
    async fn get_health_status(
        &self,
        detailed: bool,
    ) -> Result<HealthCheckReport, HealthCheckError>;
}

/// Report returned by a successful `HealthChecker::get_health_status` call.
#[derive(Debug, Clone)]
pub struct HealthCheckReport {
    pub status: HealthStatus,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub version: String,
    pub uptime: Duration,
    pub components: HashMap<String, ComponentHealth>,
}

/// Per-component health data.
#[derive(Debug, Clone)]
pub struct ComponentHealth {
    pub status: HealthStatus,
    pub provider: String,
    pub response_time: Duration,
    pub details: String,
}

/// Error returned when the health-check machinery itself fails (not just
/// a component being unhealthy).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HealthCheckError(pub String);

// ─── Mock health checker (P0) ─────────────────────────────────────────────────

/// Simple in-process health checker used before the real `cognee_lib::health`
/// module is wired in.  Always returns HEALTHY with stub component entries.
pub struct MockHealthChecker {
    start_time: Instant,
    status: HealthStatus,
    /// If set, `get_health_status` returns `Err(HealthCheckError(msg))`.
    pub fail_with: Option<String>,
}

impl MockHealthChecker {
    /// Create a healthy mock checker.
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            status: HealthStatus::Healthy,
            fail_with: None,
        }
    }

    /// Create a mock that always reports the given status.
    pub fn with_status(status: HealthStatus) -> Self {
        Self {
            start_time: Instant::now(),
            status,
            fail_with: None,
        }
    }

    /// Create a mock that always returns an error (simulates a checker panic).
    pub fn failing(msg: impl Into<String>) -> Self {
        Self {
            start_time: Instant::now(),
            status: HealthStatus::Healthy,
            fail_with: Some(msg.into()),
        }
    }
}

impl Default for MockHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HealthChecker for MockHealthChecker {
    async fn get_health_status(
        &self,
        _detailed: bool,
    ) -> Result<HealthCheckReport, HealthCheckError> {
        if let Some(msg) = &self.fail_with {
            return Err(HealthCheckError(msg.clone()));
        }

        let component = ComponentHealth {
            status: self.status,
            provider: "mock".into(),
            response_time: Duration::from_millis(1),
            details: "mock health check".into(),
        };

        let mut components = HashMap::new();
        components.insert("relational_db".into(), component.clone());
        components.insert("vector_db".into(), component.clone());
        components.insert("graph_db".into(), component.clone());
        components.insert("file_storage".into(), component);

        Ok(HealthCheckReport {
            status: self.status,
            timestamp: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime: self.start_time.elapsed(),
            components,
        })
    }
}

// ─── DTOs ─────────────────────────────────────────────────────────────────────

/// Success/UNHEALTHY body for `GET /health` (shallow probe).
/// Key `health` (not `status`) for the enum — Python parity.
#[derive(Debug, Serialize)]
struct HealthShallowDTO {
    /// "ready" when 200, "not ready" when 503.
    status: &'static str,
    /// Aggregate health status enum value.
    health: HealthStatus,
    version: String,
}

/// Failure body for `GET /health` when the checker itself errored.
/// Key `reason` — Python parity.
#[derive(Debug, Serialize)]
struct HealthShallowFailureDTO {
    status: &'static str, // always "not ready"
    reason: String,       // "health check failed: <err>"
}

/// Success/DEGRADED/UNHEALTHY body for `GET /health/detailed`.
/// Key `status` (not `health`) for the enum — Python parity.
#[derive(Debug, Serialize)]
struct HealthDetailedDTO {
    /// Health status enum value — key is `status` here (differs from shallow).
    status: HealthStatus,
    timestamp: String,
    version: String,
    uptime: u64,
    components: HashMap<String, ComponentHealthDTO>,
}

/// Per-component entry in the detailed response.
#[derive(Debug, Serialize)]
struct ComponentHealthDTO {
    status: HealthStatus,
    provider: String,
    response_time_ms: u64,
    details: String,
}

/// Failure body for `GET /health/detailed` when the checker itself errored.
/// Key `error` (not `reason`) and value `"unhealthy"` (not `"not ready"`) —
/// Python parity.
#[derive(Debug, Serialize)]
struct HealthDetailedFailureDTO {
    status: &'static str, // always "unhealthy"
    error: String,        // "Health check system failure: <err>"
}

// ─── Helper: resolve checker ──────────────────────────────────────────────────

fn get_checker(state: &AppState) -> Arc<dyn HealthChecker> {
    state
        .health
        .clone()
        .unwrap_or_else(|| Arc::new(MockHealthChecker::new()))
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `GET /health` — shallow liveness probe.
///
/// No auth required (public endpoint).
/// Returns 200 for HEALTHY or DEGRADED, 503 for UNHEALTHY.
/// On checker error returns 503 with `{status, reason}` shape.
async fn get_shallow(State(state): State<AppState>) -> Response {
    let checker = get_checker(&state);
    match checker.get_health_status(false).await {
        Ok(report) => {
            let (http_status, ready_str) = if report.status == HealthStatus::Unhealthy {
                (StatusCode::SERVICE_UNAVAILABLE, "not ready")
            } else {
                (StatusCode::OK, "ready")
            };
            (
                http_status,
                axum::Json(HealthShallowDTO {
                    status: ready_str,
                    health: report.status,
                    version: report.version,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(HealthShallowFailureDTO {
                status: "not ready",
                reason: format!("health check failed: {e}"),
            }),
        )
            .into_response(),
    }
}

/// `GET /health/detailed` — comprehensive component report.
///
/// No auth required (public endpoint).
/// Returns 200 for HEALTHY, 503 for DEGRADED *or* UNHEALTHY (Python parity).
/// On checker error returns 503 with `{status, error}` shape.
async fn get_detailed(State(state): State<AppState>) -> Response {
    let checker = get_checker(&state);
    match checker.get_health_status(true).await {
        Ok(report) => {
            let is_unhealthy = report.status != HealthStatus::Healthy;
            let http_status = if is_unhealthy {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            };

            let components: HashMap<String, ComponentHealthDTO> = report
                .components
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        ComponentHealthDTO {
                            status: v.status,
                            provider: v.provider.clone(),
                            response_time_ms: v.response_time.as_millis() as u64,
                            details: v.details.clone(),
                        },
                    )
                })
                .collect();

            (
                http_status,
                axum::Json(HealthDetailedDTO {
                    status: report.status,
                    timestamp: report.timestamp.to_rfc3339(),
                    version: report.version,
                    uptime: report.uptime.as_secs(),
                    components,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(HealthDetailedFailureDTO {
                status: "unhealthy",
                error: format!("Health check system failure: {e}"),
            }),
        )
            .into_response(),
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Builds the health router.
///
/// Mount point `/` and `/detailed` — the `/health` prefix is supplied by
/// `.nest("/health", health::router())` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_shallow))
        .route("/detailed", get(get_detailed))
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::to_bytes,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    /// Build a minimal router backed by the given checker.
    fn make_router(checker: impl HealthChecker + 'static) -> Router {
        let mut state = AppState {
            config: Arc::new(crate::config::HttpServerConfig::default()),
            pipelines: AppState::noop_pipelines(),
            lib: None,
            auth: None,
            mailer: Arc::new(crate::auth::LoggingMailer),
            health: Some(Arc::new(checker)),
            spans: None,
            sync: None,
        };
        // Suppress unused assignment warning — state is consumed by with_state.
        let _ = &mut state;
        Router::new()
            .route("/health", get(get_shallow))
            .route("/health/detailed", get(get_detailed))
            .with_state(state)
    }

    async fn parse_body(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    // ── shallow probe ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_shallow_healthy_returns_200() {
        let app = make_router(MockHealthChecker::with_status(HealthStatus::Healthy));
        let req = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "ready");
        assert_eq!(body["health"], "healthy");
    }

    #[tokio::test]
    async fn test_shallow_unhealthy_returns_503() {
        let app = make_router(MockHealthChecker::with_status(HealthStatus::Unhealthy));
        let req = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "not ready");
        assert_eq!(body["health"], "unhealthy");
    }

    /// Python parity guard: DEGRADED on shallow stays 200 (not 503).
    #[tokio::test]
    async fn test_shallow_degraded_stays_200() {
        let app = make_router(MockHealthChecker::with_status(HealthStatus::Degraded));
        let req = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "DEGRADED must stay 200 on shallow probe"
        );
        let body = parse_body(resp).await;
        assert_eq!(body["health"], "degraded");
        assert_eq!(body["status"], "ready");
    }

    /// Checker panic/error on shallow must use key `reason` (not `error`).
    #[tokio::test]
    async fn test_shallow_checker_error_returns_503_with_reason_key() {
        let app = make_router(MockHealthChecker::failing("db exploded"));
        let req = Request::builder()
            .uri("/health")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "not ready");
        assert!(body["reason"].is_string(), "key must be `reason`: {body}");
        // The `error` key must NOT be present on the shallow path.
        assert!(
            body.get("error").is_none(),
            "`error` key must not appear on shallow: {body}"
        );
    }

    // ── detailed probe ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_detailed_healthy_returns_200() {
        let app = make_router(MockHealthChecker::with_status(HealthStatus::Healthy));
        let req = Request::builder()
            .uri("/health/detailed")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "healthy");
        assert!(body["components"].is_object());
        // Four critical components must be present.
        for key in &["relational_db", "vector_db", "graph_db", "file_storage"] {
            assert!(
                body["components"][key].is_object(),
                "missing component {key}"
            );
        }
    }

    /// Python parity guard: DEGRADED on detailed flips to 503.
    #[tokio::test]
    async fn test_detailed_degraded_returns_503() {
        let app = make_router(MockHealthChecker::with_status(HealthStatus::Degraded));
        let req = Request::builder()
            .uri("/health/detailed")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "DEGRADED must flip to 503 on detailed probe"
        );
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "degraded");
    }

    /// Checker panic/error on detailed must use key `error` (not `reason`).
    #[tokio::test]
    async fn test_detailed_checker_error_returns_503_with_error_key() {
        let app = make_router(MockHealthChecker::failing("qdrant offline"));
        let req = Request::builder()
            .uri("/health/detailed")
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = parse_body(resp).await;
        assert_eq!(body["status"], "unhealthy");
        assert!(body["error"].is_string(), "key must be `error`: {body}");
        // The `reason` key must NOT be present on the detailed path.
        assert!(
            body.get("reason").is_none(),
            "`reason` key must not appear on detailed: {body}"
        );
    }
}
