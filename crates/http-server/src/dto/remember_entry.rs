//! Request DTO for `POST /api/v1/remember/entry` (E-02).
//!
//! Wire shape mirrors Python's `RememberEntryRequest` model at
//! `cognee/api/v1/remember/routers/get_remember_router.py:101-113`.
//!
//! The `entry` field re-uses [`cognee_models::memory::MemoryEntry`] directly
//! (the LIB-01 type) — no separate wrapper DTO is needed because that type
//! already carries:
//! - the discriminated-union `serde(tag = "type")` shape (`"qa"` /
//!   `"trace"` / `"feedback"`);
//! - `serde(rename_all = "camelCase")` on every inner struct;
//! - per-field `serde(alias = "<snake_form>")` for Python's
//!   `populate_by_name=True` parity.
//!
//! See the round-trip tests at `crates/models/src/memory.rs:142-365`.

use cognee_models::memory::MemoryEntry;
use serde::Deserialize;
use utoipa::ToSchema;

/// JSON body for `POST /api/v1/remember/entry`.
///
/// Wire is camelCase per Decision 10. snake_case input forms are also
/// accepted via per-field aliases for compatibility with Python's
/// `populate_by_name=True`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RememberEntryRequestDTO {
    /// Discriminated union: `{"type": "qa"|"trace"|"feedback", ...}`.
    /// Type defined in `cognee_models::memory::MemoryEntry`.
    ///
    /// The OpenAPI schema is documented as `serde_json::Value` because
    /// `MemoryEntry` lives in `cognee-models` (not annotated with
    /// `ToSchema`). Full discriminated-union schema documentation is
    /// deferred to a follow-up doc-only task.
    #[schema(value_type = serde_json::Value)]
    pub entry: MemoryEntry,

    /// Target dataset name. Defaults to `"main_dataset"` to match Python.
    #[serde(default = "default_dataset_name", alias = "dataset_name")]
    pub dataset_name: String,

    /// Required session id. Empty strings are rejected by the handler
    /// with the Python validation envelope (`{"detail":[{"loc":["body",
    /// "session_id"], "msg":"...", "type":"value_error"}]}`).
    #[serde(alias = "session_id")]
    pub session_id: String,
}

fn default_dataset_name() -> String {
    "main_dataset".to_string()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_models::memory::QAEntry;

    #[test]
    fn parses_camel_case_payload() {
        let raw = r#"{
            "entry": {"type": "qa", "question": "Q?", "answer": "A."},
            "datasetName": "ds",
            "sessionId": "s1"
        }"#;
        let dto: RememberEntryRequestDTO = serde_json::from_str(raw).expect("parse");
        assert_eq!(dto.dataset_name, "ds");
        assert_eq!(dto.session_id, "s1");
        match dto.entry {
            MemoryEntry::Qa(QAEntry {
                question, answer, ..
            }) => {
                assert_eq!(question, "Q?");
                assert_eq!(answer, "A.");
            }
            other => panic!("expected qa, got {other:?}"),
        }
    }

    #[test]
    fn parses_snake_case_aliases() {
        let raw = r#"{
            "entry": {"type": "feedback", "qa_id": "qa-1"},
            "dataset_name": "ds2",
            "session_id": "s2"
        }"#;
        let dto: RememberEntryRequestDTO = serde_json::from_str(raw).expect("parse");
        assert_eq!(dto.dataset_name, "ds2");
        assert_eq!(dto.session_id, "s2");
        assert!(matches!(dto.entry, MemoryEntry::Feedback(_)));
    }

    #[test]
    fn dataset_name_defaults_to_main_dataset() {
        let raw = r#"{
            "entry": {"type": "qa", "question": "q", "answer": "a"},
            "sessionId": "s"
        }"#;
        let dto: RememberEntryRequestDTO = serde_json::from_str(raw).expect("parse");
        assert_eq!(dto.dataset_name, "main_dataset");
    }

    #[test]
    fn unknown_entry_type_fails_to_parse() {
        let raw = r#"{
            "entry": {"type": "bogus"},
            "sessionId": "s"
        }"#;
        let result: Result<RememberEntryRequestDTO, _> = serde_json::from_str(raw);
        assert!(result.is_err());
    }
}
