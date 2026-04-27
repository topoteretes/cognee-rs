//! Shared `PipelineRunInfoDTO` and wire-string helpers used by all four
//! pipeline routers (cognify, memify, remember, improve).
//!
//! # Wire strings
//!
//! Two distinct string namespaces coexist — see `docs/http-server/pipelines.md §3`:
//!
//! * **Durable status** written to `pipeline_runs.status` — `DATASET_PROCESSING_*`.
//! * **Live event status** emitted on the registry channel / WS frame — `PipelineRun*`.
//!
//! Both mappings live here so all routers share the same source of truth.

use cognee_core::pipeline_run_registry::RunEventKind;
use cognee_database::PipelineRunStatus;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ─── Shared response DTO ──────────────────────────────────────────────────────

/// Python's `PipelineRunInfo.model_dump()` shape — returned by every
/// pipeline router and embedded in WebSocket frames.
///
/// The `status` field carries one of the live-event strings
/// (`PipelineRunStarted`, `PipelineRunCompleted`, …).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunInfoDTO {
    /// `"PipelineRunStarted"` | `"PipelineRunYield"` | `"PipelineRunCompleted"`
    /// | `"PipelineRunAlreadyCompleted"` | `"PipelineRunErrored"`.
    pub status: String,
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    pub dataset_name: String,
    /// Free-form payload. Cognify yields a `GraphDTO`; add / memify yield
    /// `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Error message. Present only when `status == "PipelineRunErrored"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Per-data-item ingestion info rows. Set by the `add` router; absent
    /// for all other pipelines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_ingestion_info: Option<Vec<DataIngestionInfoDTO>>,
}

/// Per-data-item ingestion info row.  Set by the `add` router only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct DataIngestionInfoDTO {
    pub data_id: Uuid,
    pub content_hash: String,
    pub name: String,
    pub extension: String,
    pub mime_type: String,
    pub raw_data_location: String,
}

// ─── Live-event string mapping (registry channel / WS frame) ─────────────────

/// Map a `RunEventKind` to the Python wire string emitted in the WebSocket
/// frame `status` field and in the HTTP response `PipelineRunInfoDTO.status`.
///
/// Strings match Python's
/// [`PipelineRunInfo` subclasses](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
pub fn event_kind_to_python_string(kind: &RunEventKind) -> &'static str {
    match kind {
        RunEventKind::Started => "PipelineRunStarted",
        RunEventKind::Yield => "PipelineRunYield",
        RunEventKind::Completed => "PipelineRunCompleted",
        RunEventKind::Errored { .. } => "PipelineRunErrored",
        RunEventKind::AlreadyCompleted => "PipelineRunAlreadyCompleted",
    }
}

/// Inverse: map a Python live-event wire string back to a `RunEventKind`.
///
/// `Errored { message: "" }` is used when the string is recognised but no
/// error message is available.  Returns `None` for unrecognised strings.
pub fn python_string_to_event_kind(s: &str) -> Option<RunEventKind> {
    match s {
        "PipelineRunStarted" => Some(RunEventKind::Started),
        "PipelineRunYield" => Some(RunEventKind::Yield),
        "PipelineRunCompleted" => Some(RunEventKind::Completed),
        "PipelineRunErrored" => Some(RunEventKind::Errored {
            message: String::new(),
        }),
        "PipelineRunAlreadyCompleted" => Some(RunEventKind::AlreadyCompleted),
        _ => None,
    }
}

// ─── Durable-status string mapping (pipeline_runs.status column) ─────────────

/// Map a `PipelineRunStatus` (the in-memory enum) to the Python
/// `DATASET_PROCESSING_*` durable string written to the DB column.
///
/// Matches Python's
/// [`PipelineRunStatus`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py#L8-L12).
pub fn pipeline_status_to_db_string(status: &PipelineRunStatus) -> &'static str {
    match status {
        PipelineRunStatus::Initiated => "DATASET_PROCESSING_INITIATED",
        PipelineRunStatus::Started => "DATASET_PROCESSING_STARTED",
        PipelineRunStatus::Completed => "DATASET_PROCESSING_COMPLETED",
        PipelineRunStatus::Errored => "DATASET_PROCESSING_ERRORED",
    }
}

/// Inverse: parse a Python `DATASET_PROCESSING_*` string to the enum.
///
/// Returns `None` for unrecognised strings.
pub fn db_string_to_pipeline_status(s: &str) -> Option<PipelineRunStatus> {
    match s {
        "DATASET_PROCESSING_INITIATED" => Some(PipelineRunStatus::Initiated),
        "DATASET_PROCESSING_STARTED" => Some(PipelineRunStatus::Started),
        "DATASET_PROCESSING_COMPLETED" => Some(PipelineRunStatus::Completed),
        "DATASET_PROCESSING_ERRORED" => Some(PipelineRunStatus::Errored),
        _ => None,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Live-event round-trips ─────────────────────────────────────────────────

    #[test]
    fn event_kind_started_round_trip() {
        let s = event_kind_to_python_string(&RunEventKind::Started);
        assert_eq!(s, "PipelineRunStarted");
        assert!(matches!(
            python_string_to_event_kind(s).unwrap(),
            RunEventKind::Started
        ));
    }

    #[test]
    fn event_kind_yield_round_trip() {
        let s = event_kind_to_python_string(&RunEventKind::Yield);
        assert_eq!(s, "PipelineRunYield");
        assert!(matches!(
            python_string_to_event_kind(s).unwrap(),
            RunEventKind::Yield
        ));
    }

    #[test]
    fn event_kind_completed_round_trip() {
        let s = event_kind_to_python_string(&RunEventKind::Completed);
        assert_eq!(s, "PipelineRunCompleted");
        assert!(matches!(
            python_string_to_event_kind(s).unwrap(),
            RunEventKind::Completed
        ));
    }

    #[test]
    fn event_kind_errored_round_trip() {
        let kind = RunEventKind::Errored {
            message: "boom".into(),
        };
        let s = event_kind_to_python_string(&kind);
        assert_eq!(s, "PipelineRunErrored");
        assert!(matches!(
            python_string_to_event_kind(s).unwrap(),
            RunEventKind::Errored { .. }
        ));
    }

    #[test]
    fn event_kind_already_completed_round_trip() {
        let s = event_kind_to_python_string(&RunEventKind::AlreadyCompleted);
        assert_eq!(s, "PipelineRunAlreadyCompleted");
        assert!(matches!(
            python_string_to_event_kind(s).unwrap(),
            RunEventKind::AlreadyCompleted
        ));
    }

    #[test]
    fn unknown_live_event_string_returns_none() {
        assert!(python_string_to_event_kind("UnknownStatus").is_none());
        assert!(python_string_to_event_kind("").is_none());
    }

    // ── Durable-status round-trips ─────────────────────────────────────────────

    #[test]
    fn pipeline_status_initiated_round_trip() {
        let s = pipeline_status_to_db_string(&PipelineRunStatus::Initiated);
        assert_eq!(s, "DATASET_PROCESSING_INITIATED");
        assert!(matches!(
            db_string_to_pipeline_status(s).unwrap(),
            PipelineRunStatus::Initiated
        ));
    }

    #[test]
    fn pipeline_status_started_round_trip() {
        let s = pipeline_status_to_db_string(&PipelineRunStatus::Started);
        assert_eq!(s, "DATASET_PROCESSING_STARTED");
        assert!(matches!(
            db_string_to_pipeline_status(s).unwrap(),
            PipelineRunStatus::Started
        ));
    }

    #[test]
    fn pipeline_status_completed_round_trip() {
        let s = pipeline_status_to_db_string(&PipelineRunStatus::Completed);
        assert_eq!(s, "DATASET_PROCESSING_COMPLETED");
        assert!(matches!(
            db_string_to_pipeline_status(s).unwrap(),
            PipelineRunStatus::Completed
        ));
    }

    #[test]
    fn pipeline_status_errored_round_trip() {
        let s = pipeline_status_to_db_string(&PipelineRunStatus::Errored);
        assert_eq!(s, "DATASET_PROCESSING_ERRORED");
        assert!(matches!(
            db_string_to_pipeline_status(s).unwrap(),
            PipelineRunStatus::Errored
        ));
    }

    #[test]
    fn unknown_db_string_returns_none() {
        assert!(db_string_to_pipeline_status("UNKNOWN").is_none());
        assert!(db_string_to_pipeline_status("").is_none());
    }

    // ── All five live-event strings ────────────────────────────────────────────

    #[test]
    fn all_live_event_strings_are_python_literals() {
        let expected = [
            "PipelineRunStarted",
            "PipelineRunYield",
            "PipelineRunCompleted",
            "PipelineRunErrored",
            "PipelineRunAlreadyCompleted",
        ];
        let produced = [
            event_kind_to_python_string(&RunEventKind::Started),
            event_kind_to_python_string(&RunEventKind::Yield),
            event_kind_to_python_string(&RunEventKind::Completed),
            event_kind_to_python_string(&RunEventKind::Errored {
                message: "x".into(),
            }),
            event_kind_to_python_string(&RunEventKind::AlreadyCompleted),
        ];
        assert_eq!(expected, produced);
    }

    // ── All four durable strings ───────────────────────────────────────────────

    #[test]
    fn all_durable_strings_are_python_literals() {
        let expected = [
            "DATASET_PROCESSING_INITIATED",
            "DATASET_PROCESSING_STARTED",
            "DATASET_PROCESSING_COMPLETED",
            "DATASET_PROCESSING_ERRORED",
        ];
        let produced = [
            pipeline_status_to_db_string(&PipelineRunStatus::Initiated),
            pipeline_status_to_db_string(&PipelineRunStatus::Started),
            pipeline_status_to_db_string(&PipelineRunStatus::Completed),
            pipeline_status_to_db_string(&PipelineRunStatus::Errored),
        ];
        assert_eq!(expected, produced);
    }
}
