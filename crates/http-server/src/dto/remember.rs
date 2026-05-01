//! DTOs for `POST /api/v1/remember`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Form fields ─────────────────────────────────────────────────────────────

/// Parsed multipart form for `POST /api/v1/remember`.
///
/// Populated by the handler iterating over multipart parts; not derived via
/// serde (multipart extraction is manual).
#[derive(Debug, Default)]
pub struct RememberFormDTO {
    /// camelCase wire name: `datasetName`.
    pub dataset_name: Option<String>,
    /// camelCase wire name: `datasetId`. Empty string → `None`.
    pub dataset_id: super::util::DatasetIdRef,
    /// Repeated form field.  `[""]` is translated to `None` after extraction.
    pub node_set: Option<Vec<String>>,
    /// `"true"` / `"1"` → `true`.
    pub run_in_background: Option<bool>,
    pub custom_prompt: Option<String>,
    pub chunks_per_batch: Option<u32>,
    /// Optional session id forwarded to `cognee.remember(session_id=...)` per
    /// Python (`get_remember_router.py:34` / `:84`). Empty string is treated
    /// as `None` (Python's `examples=[""]` is illustrative — empty is the
    /// "absent" sentinel).
    pub session_id: Option<String>,
}

// ─── Uploaded file part ───────────────────────────────────────────────────────

/// One spooled file part from the multipart body.
pub struct UploadedFilePart {
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub temp_path: std::path::PathBuf,
    pub byte_count: u64,
}

// ─── Wire status enum ─────────────────────────────────────────────────────────

/// Wire-format status for the `/remember` and `/remember/entry` HTTP responses.
///
/// Python's `RememberResult.to_dict()` emits these exact lowercase strings —
/// see `cognee/api/v1/remember/remember.py:323-324, 480, 521, 720, 751`.
///
/// **Decision 15** (two-layer status convention): the library
/// `cognee_lib::api::remember::RememberStatus` enum (LIB-06, commit b39cd05)
/// emits CamelCase for internal Rust consistency with
/// `cognee_core::PipelineRunStatus`. The HTTP layer translates back to
/// Python's lowercase here for strict wire parity. **No wire divergence.**
///
/// The cross-crate `From<cognee_lib::api::remember::RememberStatus>`
/// translation is **deferred to the P5 wiring task** because
/// `cognee-http-server` cannot depend on `cognee-lib` (cycle constraint —
/// `cognee-lib`'s `server` feature pulls in `cognee-http-server`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum WireRememberStatus {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "errored")]
    Errored,
    #[serde(rename = "session_stored")]
    SessionStored,
}

// ─── Per-item DTO ────────────────────────────────────────────────────────────

/// Per-item result info attached to `RememberResultDTO.items`.
///
/// Mirrors the fields of `cognee_lib::api::remember::RememberItemInfo`
/// (`crates/lib/src/api/remember.rs:72-82`) but is defined locally because
/// `cognee-http-server` cannot depend on `cognee-lib` (cycle constraint).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RememberItemDTO {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
}

// ─── Response ─────────────────────────────────────────────────────────────────

/// Response body for `POST /api/v1/remember`.
///
/// Wire shape mirrors Python's `RememberResult.to_dict()`
/// (`cognee/api/v1/remember/remember.py:415-437`).
///
/// **CLEAN-01 carve-out**: `#[serde(rename_all = "snake_case")]` is preserved
/// because Python's `RememberResult` is a plain class (not pydantic
/// `BaseModel`), so its `to_dict()` produces snake_case keys directly and
/// `jsonable_encoder()` does not apply alias conversion. See
/// `docs/http-api-v2/tasks/clean-01-v1-dto-camelcase.md` §3.1 row for
/// `dto/remember.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RememberResultDTO {
    pub status: WireRememberStatus,
    /// Python emits the key always (may be `null` on the session-stored path);
    /// no `skip_serializing_if`.
    pub pipeline_run_id: Option<uuid::Uuid>,
    /// Python emits the key always (may be `null`); no `skip_serializing_if`.
    pub dataset_id: Option<uuid::Uuid>,
    pub dataset_name: String,
    /// Always emitted (default 0). Mirrors Python's
    /// `RememberResult.items_processed` (`remember.py:418`).
    pub items_processed: u32,
    /// Always emitted (`null` when absent). Mirrors Python's
    /// `RememberResult.elapsed_seconds` (`remember.py:422`).
    pub elapsed_seconds: Option<f64>,
    /// Conditional — only emitted when set
    /// (Python `if self.session_ids:` `remember.py:425-426`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ids: Option<Vec<String>>,
    /// Conditional (Python `if self.content_hash:` `remember.py:427-428`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Conditional (Python `if self.items:` `remember.py:429-430`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<RememberItemDTO>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Discriminator string for the typed-entry path
    /// (`"qa"` / `"trace"` / `"feedback"`).
    ///
    /// Reserved for `POST /api/v1/remember/entry` (E-02, Decision 5). Skipped
    /// when `None` so the existing file-payload responses (E-01) stay
    /// byte-identical (Python omits both keys on the file path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<String>,
    /// Cache-returned entry id (`qa_id` / `trace_id`). For feedback entries
    /// this is the input `qa_id` even when the QA was not found in the
    /// session (Python parity at `remember.py:307`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `WireRememberStatus` variant must serialize to Python's exact
    /// lowercase wire string (Decision 15).
    #[test]
    fn wire_remember_status_serde_roundtrip() {
        let cases = [
            (WireRememberStatus::Running, "\"running\""),
            (WireRememberStatus::Completed, "\"completed\""),
            (WireRememberStatus::Errored, "\"errored\""),
            (WireRememberStatus::SessionStored, "\"session_stored\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(json, expected, "variant {variant:?} → {expected}");
            let parsed: WireRememberStatus = serde_json::from_str(expected).expect("deserialize");
            assert_eq!(parsed, variant, "round-trip {expected}");
        }
    }

    /// `RememberResultDTO` must match Python's `RememberResult.to_dict()` wire
    /// shape — required keys always present, conditional keys absent when
    /// `None`. `dataset_id` / `pipeline_run_id` / `elapsed_seconds` are
    /// always-emit (may be `null`); `session_ids` / `content_hash` / `items`
    /// / `error` are skip-on-`None`.
    #[test]
    fn remember_result_dto_minimal_wire_shape() {
        let dto = RememberResultDTO {
            status: WireRememberStatus::Completed,
            pipeline_run_id: None,
            dataset_id: None,
            dataset_name: "ds".into(),
            items_processed: 0,
            elapsed_seconds: None,
            session_ids: None,
            content_hash: None,
            items: None,
            error: None,
            entry_type: None,
            entry_id: None,
        };
        let v = serde_json::to_value(&dto).expect("to_value");
        let obj = v.as_object().expect("object");

        // Always-emitted keys.
        assert_eq!(obj["status"], "completed");
        assert!(obj.contains_key("pipeline_run_id"));
        assert!(obj["pipeline_run_id"].is_null());
        assert!(obj.contains_key("dataset_id"));
        assert!(obj["dataset_id"].is_null());
        assert_eq!(obj["dataset_name"], "ds");
        assert_eq!(obj["items_processed"], 0);
        assert!(obj.contains_key("elapsed_seconds"));
        assert!(obj["elapsed_seconds"].is_null());

        // Conditional keys must be absent when `None`.
        assert!(!obj.contains_key("session_ids"));
        assert!(!obj.contains_key("content_hash"));
        assert!(!obj.contains_key("items"));
        assert!(!obj.contains_key("error"));

        // E-02 reserved keys must NOT appear here (Decision 5).
        assert!(!obj.contains_key("entry_type"));
        assert!(!obj.contains_key("entry_id"));
    }

    #[test]
    fn remember_result_dto_populated_wire_shape() {
        let dto = RememberResultDTO {
            status: WireRememberStatus::SessionStored,
            pipeline_run_id: None,
            dataset_id: None,
            dataset_name: "ds".into(),
            items_processed: 3,
            elapsed_seconds: Some(1.25),
            session_ids: Some(vec!["sess-1".into()]),
            content_hash: Some("abc123".into()),
            items: Some(vec![RememberItemDTO {
                name: Some("doc.txt".into()),
                content_hash: Some("hash".into()),
                token_count: Some(42),
            }]),
            error: None,
            entry_type: None,
            entry_id: None,
        };
        let v = serde_json::to_value(&dto).expect("to_value");
        let obj = v.as_object().expect("object");

        assert_eq!(obj["status"], "session_stored");
        assert_eq!(obj["items_processed"], 3);
        assert_eq!(obj["elapsed_seconds"], 1.25);
        assert_eq!(obj["session_ids"][0], "sess-1");
        assert_eq!(obj["content_hash"], "abc123");
        let items = obj["items"].as_array().expect("items array");
        assert_eq!(items[0]["name"], "doc.txt");
        assert_eq!(items[0]["content_hash"], "hash");
        assert_eq!(items[0]["token_count"], 42);

        // Without `entry_type` / `entry_id` set, both keys must be absent
        // — the file/text path of `RememberResultDTO` does not carry them
        // (Python parity, Decision 5).
        assert!(!obj.contains_key("entry_type"));
        assert!(!obj.contains_key("entry_id"));
    }

    /// E-02, Decision 5: when the typed-entry handler populates the new
    /// `entry_type` / `entry_id` fields, they must serialize to the wire
    /// under their snake_case names alongside the rest of the DTO.
    #[test]
    fn remember_result_dto_serializes_entry_fields_when_set() {
        let dto = RememberResultDTO {
            status: WireRememberStatus::SessionStored,
            pipeline_run_id: None,
            dataset_id: None,
            dataset_name: "main_dataset".into(),
            items_processed: 0,
            elapsed_seconds: Some(0.01),
            session_ids: Some(vec!["sess-1".into()]),
            content_hash: None,
            items: None,
            error: None,
            entry_type: Some("qa".into()),
            entry_id: Some("qa-abc-123".into()),
        };
        let v = serde_json::to_value(&dto).expect("to_value");
        let obj = v.as_object().expect("object");

        assert_eq!(obj["status"], "session_stored");
        assert_eq!(obj["entry_type"], "qa");
        assert_eq!(obj["entry_id"], "qa-abc-123");
    }
}
