//! Activity & telemetry endpoints.
//!
//! Five handlers:
//! - `GET /pipeline-runs` — durable observability tier (joins `pipeline_runs ⨝
//!   datasets ⨝ users`).
//! - `GET /spans` — live observability tier (in-memory ring buffer).
//! - `GET /users` — list of users in the *default user's* tenant (Python
//!   parity: not the authenticated user's tenant).
//! - `GET /agents` — active users with agent metadata (OSS returns an empty
//!   list; the closed `cognee-http-cloud` crate provides the real handler).
//! - `GET /export/{dataset_id}` — Markdown report for one dataset.
//!
//! See [`docs/http-server/routers/activity.md`](../../../../docs/http-server/routers/activity.md).

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, SecondsFormat, Utc};
use cognee_database::DeleteDb;
use cognee_database::IngestDb;
use cognee_database::PipelineRunRepository;
use cognee_database::SeaOrmPipelineRunRepository;
use cognee_models::Data;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::activity::{
    AgentDTO, PipelineRunListItemDTO, RecordedSpanDTO, SpansErrorEnvelopeDTO, TenantUserDTO,
    TraceSummaryDTO,
};
use crate::error::ApiError;
use crate::observability::SpanStatus;
use crate::state::AppState;

// ─── Mount ───────────────────────────────────────────────────────────────────

/// Build the activity router. Mounted by `build_router` at `/api/v1/activity`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pipeline-runs", get(get_pipeline_runs))
        .route("/spans", get(get_spans))
        .route("/users", get(get_users))
        .route("/agents", get(get_agents))
        .route("/export/{dataset_id}", get(get_export))
}

// ─── 2.1  GET /pipeline-runs ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PipelineRunsQuery {
    pub dataset_id: Option<Uuid>,
}

/// `GET /api/v1/activity/pipeline-runs` — list recent pipeline runs.
///
/// Reads `pipeline_runs ⨝ datasets ⨝ users` so the response carries
/// "who/what/which dataset" attribution. No tenant filter (Python parity).
pub async fn get_pipeline_runs(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Query(filter): Query<PipelineRunsQuery>,
) -> Result<Json<Vec<PipelineRunListItemDTO>>, ApiError> {
    let handles = state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))?;
    let repo = SeaOrmPipelineRunRepository::new(handles.database.clone());
    let rows = repo
        .list_recent_with_attribution(filter.dataset_id, 50)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let dtos = rows
        .into_iter()
        .map(|r| PipelineRunListItemDTO {
            id: r.id,
            pipeline_name: r.pipeline_name,
            status: Some(status_to_str(&r.status)),
            dataset_id: r.dataset_id,
            dataset_name: r.dataset_name,
            owner_id: r.owner_id,
            owner_email: r.owner_email,
            created_at: Some(format_iso8601(r.created_at)),
            pipeline_run_id: Some(r.pipeline_run_id),
        })
        .collect();
    Ok(Json(dtos))
}

fn status_to_str(s: &cognee_database::PipelineRunStatus) -> String {
    match s {
        cognee_database::PipelineRunStatus::Initiated => "DATASET_PROCESSING_INITIATED".into(),
        cognee_database::PipelineRunStatus::Started => "DATASET_PROCESSING_STARTED".into(),
        cognee_database::PipelineRunStatus::Completed => "DATASET_PROCESSING_COMPLETED".into(),
        cognee_database::PipelineRunStatus::Errored => "DATASET_PROCESSING_ERRORED".into(),
    }
}

/// `chrono::DateTime<Utc>::to_rfc3339_opts(SecondsFormat::AutoSi, false)`
/// produces `"2026-04-24T18:30:00+00:00"` — matches Python's
/// `datetime.isoformat()`. Passing `true` would emit `"...Z"` instead.
fn format_iso8601(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, false)
}

// ─── 2.2  GET /spans ─────────────────────────────────────────────────────────

/// `GET /api/v1/activity/spans` — read the in-memory span buffer.
///
/// **SELF-REFERENTIAL**: this handler emits a `cognee.api.activity.spans` span
/// that lands in the buffer and shows up in the *next* call's response.
/// Documented in [`docs/http-server/routers/activity.md §6.6`](../../../../docs/http-server/routers/activity.md#6-open-questions).
#[tracing::instrument(name = "cognee.api.activity.spans", skip_all)]
pub async fn get_spans(State(state): State<AppState>, _user: AuthenticatedUser) -> Response {
    // Python wraps the entire body in a `try/except` and returns 200 with
    // `{"error": "..."}` on failure. Our buffer read can only panic on a
    // poisoned mutex; we catch via `catch_unwind` to mirror the wire shape.
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.spans.all_traces()));
    match result {
        Ok(traces) => {
            let dtos: Vec<TraceSummaryDTO> = traces
                .into_iter()
                .map(|t| TraceSummaryDTO {
                    trace_id: t.trace_id,
                    root_name: t.root_name,
                    duration_ms: t.duration_ms,
                    span_count: t.span_count,
                    status: t.status.map(span_status_to_string),
                    spans: t
                        .spans
                        .into_iter()
                        .map(|s| RecordedSpanDTO {
                            name: s.name,
                            trace_id: s.trace_id,
                            span_id: s.span_id,
                            parent_span_id: s.parent_span_id,
                            start_time_ns: s.start_time_ns,
                            end_time_ns: s.end_time_ns,
                            duration_ms: s.duration_ms,
                            status: span_status_to_string(s.status),
                            attributes: s.attributes,
                        })
                        .collect(),
                })
                .collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(_) => {
            tracing::error!("spans buffer read failed (panic in catch_unwind)");
            (
                StatusCode::OK,
                Json(SpansErrorEnvelopeDTO {
                    error: "spans buffer read failed".into(),
                }),
            )
                .into_response()
        }
    }
}

fn span_status_to_string(status: SpanStatus) -> String {
    status.as_str().to_string()
}

// ─── 2.3  GET /users ─────────────────────────────────────────────────────────

/// `GET /api/v1/activity/users` — list users in the *default user's* tenant.
///
/// OSS stub: the auth tables (`users`, `tenants`, `user_tenants`) moved
/// closed alongside `PermissionsRepository`, so the OSS surface returns
/// an empty list. The closed `cognee-http-cloud` crate re-introduces the
/// real handler via its own router.
pub async fn get_users(
    State(_state): State<AppState>,
    _user: AuthenticatedUser,
) -> Json<Vec<TenantUserDTO>> {
    Json(Vec::new())
}

// ─── 2.4  GET /agents ────────────────────────────────────────────────────────

/// `GET /api/v1/activity/agents` — list every active user with agent metadata.
///
/// OSS stub: the `users` / `user_api_key` tables moved closed
/// alongside `SeaOrmUserAuthRepository`. OSS returns an empty list; the
/// closed `cognee-http-cloud` crate provides the real handler.
pub async fn get_agents(
    State(_state): State<AppState>,
    _user: AuthenticatedUser,
) -> Result<Json<Vec<AgentDTO>>, ApiError> {
    Ok(Json(Vec::new()))
}

// ─── 2.5  GET /export/{dataset_id} ───────────────────────────────────────────

/// Sanitize a dataset name for use in `Content-Disposition: filename=`.
///
/// RFC 6266 minimal: strip CR/LF, replace `"` with `'`. Python doesn't
/// URL-encode, neither do we.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '\r' && *c != '\n')
        .map(|c| if c == '"' { '\'' } else { c })
        .collect()
}

/// `GET /api/v1/activity/export/{dataset_id}` — Markdown report.
pub async fn get_export(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(dataset_id): Path<Uuid>,
) -> Response {
    let Some(handles) = state.components() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "components not initialized",
        )
            .into_response();
    };

    // 1. Dataset lookup. 404 body is plain text per Python parity.
    let dataset = match handles.database.get_dataset(dataset_id).await {
        Ok(Some(ds)) => ds,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "Dataset not found",
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("export error: {e}"),
            )
                .into_response();
        }
    };

    // 2. Documents in the dataset.
    let docs = match handles.database.get_dataset_data(dataset_id).await {
        Ok(rows) => rows,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("export error: {e}"),
            )
                .into_response();
        }
    };

    // 3. Graph data — errors silently swallow to empty (Python parity).
    let graph_data = handles
        .formatted_graph_data(Some(dataset_id), user.id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "graph fetch failed during export");
            serde_json::json!({"nodes": [], "edges": []})
        });

    let nodes = graph_data
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let edges = graph_data
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let body = render_markdown(&dataset.name, &docs, &nodes, &edges, Utc::now());
    let filename = format!("{}-memory-export.md", sanitize_filename(&dataset.name));
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

// ─── Markdown rendering ──────────────────────────────────────────────────────

/// Render the Markdown body for `/export/{dataset_id}`.
///
/// Mirrors Python's L248–L319 verbatim:
/// - header, summaries, entities, relationships, documents, other-nodes
/// - section gating on emptiness
/// - `|` → `\|` in table cells; `\n` → ` ` in entity descriptions
/// - `"related_to"` edge fallback; first-12-chars source/target fallback
fn render_markdown(
    dataset_name: &str,
    docs: &[Data],
    nodes: &[serde_json::Value],
    edges: &[serde_json::Value],
    now: DateTime<Utc>,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Categorize nodes.
    let mut entities: Vec<&serde_json::Value> = Vec::new();
    let mut summaries: Vec<&serde_json::Value> = Vec::new();
    let mut others: Vec<&serde_json::Value> = Vec::new();
    let mut node_label_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for n in nodes {
        let ty = n.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let id = n
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let label = n
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        if !id.is_empty() {
            node_label_by_id.insert(id, label.clone());
        }
        match ty {
            "Entity" => entities.push(n),
            "TextSummary" => summaries.push(n),
            "DocumentChunk" | "TextDocument" => {} // silently dropped
            _ => others.push(n),
        }
    }

    // Header
    lines.push(format!("# Dataset: {dataset_name}"));
    lines.push(String::new());
    lines.push(format!(
        "Exported: {} | {} documents | {} entities | {} relationships",
        now.format("%b %d, %Y %H:%M UTC"),
        docs.len(),
        entities.len(),
        edges.len(),
    ));
    lines.push(String::new());

    // Summaries
    if !summaries.is_empty() {
        lines.push("## Summaries".into());
        lines.push(String::new());
        for s in &summaries {
            let text = s
                .get("properties")
                .and_then(|p| p.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            lines.push(format!("> {text}"));
        }
        lines.push(String::new());
    }

    // Entities
    if !entities.is_empty() {
        lines.push("## Entities".into());
        lines.push(String::new());
        lines.push("| Entity | Description |".into());
        lines.push("|--------|-------------|".into());
        for e in &entities {
            let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let description = e
                .get("properties")
                .and_then(|p| p.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            lines.push(format!(
                "| {} | {} |",
                escape_pipes(label),
                escape_pipes(&description.replace('\n', " ")),
            ));
        }
        lines.push(String::new());
    }

    // Relationships
    if !edges.is_empty() {
        lines.push("## Relationships".into());
        lines.push(String::new());
        lines.push("| Source | Relationship | Target |".into());
        lines.push("|--------|-------------|--------|".into());
        for edge in edges {
            let source_id = edge.get("source").and_then(|v| v.as_str()).unwrap_or("?");
            let target_id = edge.get("target").and_then(|v| v.as_str()).unwrap_or("?");
            let label = edge
                .get("label")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("related_to");
            let source_label = node_label_by_id
                .get(source_id)
                .cloned()
                .unwrap_or_else(|| source_id.chars().take(12).collect());
            let target_label = node_label_by_id
                .get(target_id)
                .cloned()
                .unwrap_or_else(|| target_id.chars().take(12).collect());
            lines.push(format!(
                "| {} | {} | {} |",
                escape_pipes(&source_label),
                escape_pipes(label),
                escape_pipes(&target_label),
            ));
        }
        lines.push(String::new());
    }

    // Documents
    if !docs.is_empty() {
        lines.push("## Documents".into());
        lines.push(String::new());
        for d in docs {
            let name = if d.name.is_empty() {
                "unnamed".to_string()
            } else {
                d.name.clone()
            };
            let extension = d.extension.to_uppercase();
            let created = d.created_at.format("%b %d, %Y").to_string();
            lines.push(format!("- **{name}** ({extension}, {created})"));
        }
        lines.push(String::new());
    }

    // Other nodes
    if !others.is_empty() {
        lines.push("## Other Nodes".into());
        lines.push(String::new());
        for n in &others {
            let ty = n.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let label = n.get("label").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(format!("- [{ty}] {label}"));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

fn escape_pipes(s: &str) -> String {
    s.replace('|', r"\|")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_pipe_escape() {
        // One entity whose label contains a pipe.
        let nodes = vec![serde_json::json!({
            "id": "n1",
            "type": "Entity",
            "label": "a|b",
            "properties": {"description": "fine"},
        })];
        let body = render_markdown("ds", &[], &nodes, &[], Utc::now());
        assert!(body.contains(r"a\|b"));
    }

    #[test]
    fn render_markdown_section_gating_no_entities() {
        let body = render_markdown("ds", &[], &[], &[], Utc::now());
        assert!(!body.contains("## Entities"));
        assert!(!body.contains("## Summaries"));
        assert!(!body.contains("## Relationships"));
        assert!(!body.contains("## Documents"));
        assert!(body.contains("# Dataset: ds"));
    }

    #[test]
    fn render_markdown_uses_related_to_fallback() {
        let edges = vec![serde_json::json!({
            "source": "A",
            "target": "B",
        })];
        let body = render_markdown("ds", &[], &[], &edges, Utc::now());
        assert!(body.contains("related_to"));
    }

    #[test]
    fn iso_format_produces_trailing_zero_offset() {
        let t = DateTime::parse_from_rfc3339("2026-04-24T18:30:00Z")
            .expect("parse")
            .with_timezone(&Utc);
        let s = format_iso8601(t);
        // SecondsFormat::AutoSi suppresses fractional seconds when zero.
        assert!(s.starts_with("2026-04-24T18:30:00"), "got {s}");
        assert!(s.ends_with("+00:00"), "got {s}");
    }

    #[test]
    fn sanitize_filename_strips_crlf_and_quotes() {
        assert_eq!(sanitize_filename("ok\nname"), "okname");
        assert_eq!(sanitize_filename("a\"b"), "a'b");
    }

    #[test]
    fn span_status_to_string_matches_wire() {
        assert_eq!(span_status_to_string(SpanStatus::Ok), "OK");
        assert_eq!(span_status_to_string(SpanStatus::Error), "ERROR");
        assert_eq!(span_status_to_string(SpanStatus::Unset), "UNSET");
    }
}
