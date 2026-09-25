//! DTOs for the `GET /api/v1/activity/*` family.
//!
//! All ISO-8601 timestamp fields stay `Option<String>` to match Python's
//! `.isoformat()` shape (with the literal `+00:00` suffix). This intentionally
//! diverges from the OpenAPI `date-time` representation so the wire format is
//! byte-equivalent across SDKs.

use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

/// One row of `GET /api/v1/activity/pipeline-runs`.
///
/// Mirrors the Python dict at
/// `cognee/api/v1/activity/routers/get_activity_router.py` lines 51–62.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunListItemDTO {
    pub id: Uuid,
    pub pipeline_name: String,
    /// `DATASET_PROCESSING_*` enum string. `None` when the row's status is NULL.
    pub status: Option<String>,
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub owner_email: Option<String>,
    /// ISO-8601, e.g. `"2026-04-24T18:30:00+00:00"`.
    pub created_at: Option<String>,
    pub pipeline_run_id: Option<Uuid>,
}

/// One trace returned by `GET /api/v1/activity/spans`.
///
/// Wire shape matches Python's exporter dict at
/// `get_activity_router.py` lines 88–96.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TraceSummaryDTO {
    pub trace_id: String,
    pub root_name: Option<String>,
    pub duration_ms: f64,
    pub span_count: usize,
    /// `"OK" | "ERROR" | "UNSET"`. `None` only when the trace has no spans.
    pub status: Option<String>,
    pub spans: Vec<RecordedSpanDTO>,
}

/// One span inside a [`TraceSummaryDTO`].
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RecordedSpanDTO {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_time_ns: u64,
    pub end_time_ns: u64,
    pub duration_ms: f64,
    /// Already redacted by `SpanBufferLayer::on_close`. Stringified to match
    /// Python's already-stringified shape — `"OK" | "ERROR" | "UNSET"`.
    pub status: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// One row of `GET /api/v1/activity/users`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TenantUserDTO {
    pub id: Uuid,
    pub email: String,
    pub is_superuser: bool,
    pub created_at: Option<String>,
}

/// One row of `GET /api/v1/activity/agents`.
///
/// Mirrors Python's dict at L181–L194 of `get_activity_router.py`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AgentDTO {
    pub id: Uuid,
    pub email: String,
    pub agent_type: String,
    pub agent_short_id: String,
    pub is_agent: bool,
    pub is_default: bool,
    /// `"LIVE"` if the user has at least one API key, else `"INACTIVE"`.
    pub status: String,
    pub api_key_count: u64,
    pub created_at: Option<String>,
}

/// Body returned by `GET /api/v1/activity/spans` on the catch-all path.
///
/// Status stays 200 (Python parity). Body is the literal `{"error": "..."}`
/// object (not an array) so existing dashboards continue to render.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SpansErrorEnvelopeDTO {
    pub error: String,
}
