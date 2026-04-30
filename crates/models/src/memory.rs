//! Discriminated-union memory entries for `remember()` typed dispatch.
//!
//! Mirrors Python's `cognee/memory/entries.py`. Typed payloads let callers
//! pass rich structured data to `cognee.remember()` — Q&A turns, agent
//! trace steps, feedback attachments — in addition to the legacy
//! "blob of text/files" shape. Each entry carries a literal `type`
//! discriminator so the `remember_entry()` dispatch can route to the
//! right `SessionManager` method.
//!
//! Wire shape (Decision 10): the `type` discriminator stays snake_case
//! (`"qa"` / `"trace"` / `"feedback"`) per Python's
//! `Literal["qa"|"trace"|"feedback"]`, while every multi-word inner
//! field name is camelCase on the wire (`feedbackText`, `originFunction`,
//! `methodParams`, etc.). Snake-case `serde(alias)` attributes accept
//! the legacy snake_case form on input (Python `populate_by_name=True`
//! parity).

use serde::{Deserialize, Serialize};

/// Tagged union of typed memory payloads dispatched by `remember_entry()`.
///
/// Python parity: `cognee/memory/entries.py:67` (`Union[QAEntry,
/// TraceEntry, FeedbackEntry]`). The `type` discriminator on the wire
/// stays snake_case (`"qa"` / `"trace"` / `"feedback"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryEntry {
    /// A Q&A turn stored in the session cache. Dispatched to
    /// `SessionManager::save_qa` (+ optional `update_qa`).
    Qa(QAEntry),
    /// One step of an agent trace. Dispatched to
    /// `SessionManager::add_agent_trace_step`.
    Trace(TraceEntry),
    /// Feedback attached to an existing QA entry. Dispatched to
    /// `SessionManager::add_feedback`.
    Feedback(FeedbackEntry),
}

impl MemoryEntry {
    /// Python parity helper — the lowercase string discriminator
    /// (`"qa"` / `"trace"` / `"feedback"`) populated on
    /// `RememberResult.entry_type`.
    pub fn type_str(&self) -> &'static str {
        match self {
            MemoryEntry::Qa(_) => "qa",
            MemoryEntry::Trace(_) => "trace",
            MemoryEntry::Feedback(_) => "feedback",
        }
    }
}

/// A Q&A turn stored in the session cache.
///
/// Python parity: `cognee/memory/entries.py:18-31`. `context` defaults
/// to `""`; the three optional feedback fields default to `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QAEntry {
    /// The user question.
    pub question: String,
    /// The assistant answer.
    pub answer: String,
    /// Optional retrieval context. Defaults to `""` (Python parity).
    #[serde(default)]
    pub context: String,
    /// Optional free-form feedback string.
    #[serde(default, alias = "feedback_text")]
    pub feedback_text: Option<String>,
    /// Optional 1..=5 feedback score (validated downstream by
    /// `SessionManager::add_feedback`).
    #[serde(default, alias = "feedback_score")]
    pub feedback_score: Option<i32>,
    /// Optional graph element ids consulted to produce the answer.
    /// Wire shape mirrors Python's `dict` — unconstrained `serde_json::Value`.
    #[serde(default, alias = "used_graph_element_ids")]
    pub used_graph_element_ids: Option<serde_json::Value>,
}

/// One step of an agent trace.
///
/// Python parity: `cognee/memory/entries.py:34-50`. `status` defaults
/// to `"success"`; `generate_feedback_with_llm` defaults to `false`;
/// the three string fields (`memory_query`, `memory_context`,
/// `error_message`) default to `""`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    /// Name of the originating function/tool.
    #[serde(alias = "origin_function")]
    pub origin_function: String,
    /// Free-form per Python validator; typically `"success"` / `"error"`.
    #[serde(default = "default_trace_status")]
    pub status: String,
    /// Method parameters (wire: `methodParams`). Optional so callers
    /// may omit it; converted to `Value::Null` when dispatched.
    #[serde(default, alias = "method_params")]
    pub method_params: Option<serde_json::Value>,
    /// Optional method return value.
    #[serde(default, alias = "method_return_value")]
    pub method_return_value: Option<serde_json::Value>,
    /// Memory query string. Defaults to `""`.
    #[serde(default, alias = "memory_query")]
    pub memory_query: String,
    /// Memory context string. Defaults to `""`.
    #[serde(default, alias = "memory_context")]
    pub memory_context: String,
    /// Error message string. Defaults to `""`.
    #[serde(default, alias = "error_message")]
    pub error_message: String,
    /// If `true`, instructs the dispatcher to generate `session_feedback`
    /// via an LLM call. **TODO(LIB-01-followup)**: LLM plumbing not in
    /// scope for LIB-01; the dispatch passes `session_feedback = ""`.
    #[serde(default, alias = "generate_feedback_with_llm")]
    pub generate_feedback_with_llm: bool,
}

/// Feedback attached to an existing QA entry.
///
/// Python parity: `cognee/memory/entries.py:53-64`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackEntry {
    /// QA id this feedback is attached to (required).
    #[serde(alias = "qa_id")]
    pub qa_id: String,
    /// Optional free-form feedback string.
    #[serde(default, alias = "feedback_text")]
    pub feedback_text: Option<String>,
    /// Optional 1..=5 feedback score.
    #[serde(default, alias = "feedback_score")]
    pub feedback_score: Option<i32>,
}

fn default_trace_status() -> String {
    "success".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_memory_entry_qa_json() {
        // camelCase wire input
        let camel = r#"{
            "type": "qa",
            "question": "Q?",
            "answer": "A.",
            "feedbackText": "good",
            "feedbackScore": 5,
            "usedGraphElementIds": {"node_ids": ["n1"], "edge_ids": []}
        }"#;
        let entry: MemoryEntry = serde_json::from_str(camel).expect("camelCase parse");
        match entry {
            MemoryEntry::Qa(ref q) => {
                assert_eq!(q.question, "Q?");
                assert_eq!(q.answer, "A.");
                assert_eq!(q.context, "", "context defaults to empty string");
                assert_eq!(q.feedback_text.as_deref(), Some("good"));
                assert_eq!(q.feedback_score, Some(5));
                assert!(q.used_graph_element_ids.is_some());
            }
            other => panic!("expected MemoryEntry::Qa, got {other:?}"),
        }

        // snake_case alias parity
        let snake = r#"{
            "type": "qa",
            "question": "Q?",
            "answer": "A.",
            "feedback_text": "good",
            "feedback_score": 4
        }"#;
        let entry: MemoryEntry = serde_json::from_str(snake).expect("snake_case alias parse");
        match entry {
            MemoryEntry::Qa(q) => {
                assert_eq!(q.feedback_text.as_deref(), Some("good"));
                assert_eq!(q.feedback_score, Some(4));
                assert_eq!(q.context, "");
            }
            other => panic!("expected MemoryEntry::Qa, got {other:?}"),
        }

        // Minimal QAEntry — only required fields.
        let minimal = r#"{"type":"qa","question":"q","answer":"a"}"#;
        let entry: MemoryEntry = serde_json::from_str(minimal).expect("minimal parse");
        match entry {
            MemoryEntry::Qa(q) => {
                assert_eq!(q.context, "");
                assert!(q.feedback_text.is_none());
                assert!(q.feedback_score.is_none());
                assert!(q.used_graph_element_ids.is_none());
            }
            other => panic!("expected MemoryEntry::Qa, got {other:?}"),
        }

        // Round-trip emits camelCase + the snake_case `type` discriminator.
        let entry = MemoryEntry::Qa(QAEntry {
            question: "q".into(),
            answer: "a".into(),
            context: "".into(),
            feedback_text: Some("nice".into()),
            feedback_score: Some(3),
            used_graph_element_ids: None,
        });
        let s = serde_json::to_string(&entry).expect("serialize");
        assert!(
            s.contains("\"type\":\"qa\""),
            "discriminator stays snake_case: {s}"
        );
        assert!(
            s.contains("\"feedbackText\":\"nice\""),
            "camelCase wire: {s}"
        );
        assert!(s.contains("\"feedbackScore\":3"), "camelCase wire: {s}");
    }

    #[test]
    fn test_round_trip_memory_entry_trace_json() {
        // camelCase wire input with all fields.
        let camel = r#"{
            "type": "trace",
            "originFunction": "search",
            "status": "error",
            "methodParams": {"q": "hello"},
            "methodReturnValue": {"hits": 3},
            "memoryQuery": "what?",
            "memoryContext": "context",
            "errorMessage": "boom",
            "generateFeedbackWithLlm": true
        }"#;
        let entry: MemoryEntry = serde_json::from_str(camel).expect("camelCase trace parse");
        match entry {
            MemoryEntry::Trace(t) => {
                assert_eq!(t.origin_function, "search");
                assert_eq!(t.status, "error");
                assert_eq!(t.memory_query, "what?");
                assert_eq!(t.memory_context, "context");
                assert_eq!(t.error_message, "boom");
                assert!(t.generate_feedback_with_llm);
                assert!(t.method_params.is_some());
                assert!(t.method_return_value.is_some());
            }
            other => panic!("expected MemoryEntry::Trace, got {other:?}"),
        }

        // snake_case alias parity + defaults.
        let snake = r#"{
            "type": "trace",
            "origin_function": "fn",
            "method_params": null,
            "method_return_value": null
        }"#;
        let entry: MemoryEntry = serde_json::from_str(snake).expect("snake_case trace parse");
        match entry {
            MemoryEntry::Trace(t) => {
                assert_eq!(t.origin_function, "fn");
                assert_eq!(t.status, "success", "status defaults to success");
                assert_eq!(t.memory_query, "");
                assert_eq!(t.memory_context, "");
                assert_eq!(t.error_message, "");
                assert!(!t.generate_feedback_with_llm);
                // null inputs deserialize to Some(Value::Null) but Option deser of null is None.
                assert!(t.method_params.is_none());
                assert!(t.method_return_value.is_none());
            }
            other => panic!("expected MemoryEntry::Trace, got {other:?}"),
        }

        // Round-trip: serialization uses camelCase + snake-case discriminator.
        let entry = MemoryEntry::Trace(TraceEntry {
            origin_function: "f".into(),
            status: "success".into(),
            method_params: Some(serde_json::json!({"k": "v"})),
            method_return_value: None,
            memory_query: "".into(),
            memory_context: "".into(),
            error_message: "".into(),
            generate_feedback_with_llm: false,
        });
        let s = serde_json::to_string(&entry).expect("serialize trace");
        assert!(s.contains("\"type\":\"trace\""));
        assert!(s.contains("\"originFunction\":\"f\""));
        assert!(s.contains("\"methodParams\""));
        assert!(s.contains("\"generateFeedbackWithLlm\":false"));
    }

    #[test]
    fn test_round_trip_memory_entry_feedback_json() {
        // camelCase wire input.
        let camel = r#"{
            "type": "feedback",
            "qaId": "abc-123",
            "feedbackText": "great",
            "feedbackScore": 5
        }"#;
        let entry: MemoryEntry = serde_json::from_str(camel).expect("camelCase feedback parse");
        match entry {
            MemoryEntry::Feedback(ref f) => {
                assert_eq!(f.qa_id, "abc-123");
                assert_eq!(f.feedback_text.as_deref(), Some("great"));
                assert_eq!(f.feedback_score, Some(5));
            }
            other => panic!("expected MemoryEntry::Feedback, got {other:?}"),
        }

        // snake_case alias.
        let snake = r#"{
            "type": "feedback",
            "qa_id": "xyz",
            "feedback_text": "ok"
        }"#;
        let entry: MemoryEntry = serde_json::from_str(snake).expect("snake_case feedback parse");
        match entry {
            MemoryEntry::Feedback(f) => {
                assert_eq!(f.qa_id, "xyz");
                assert_eq!(f.feedback_text.as_deref(), Some("ok"));
                assert!(f.feedback_score.is_none());
            }
            other => panic!("expected MemoryEntry::Feedback, got {other:?}"),
        }

        // Round-trip emits camelCase wire fields + snake-case `type`.
        let entry = MemoryEntry::Feedback(FeedbackEntry {
            qa_id: "id".into(),
            feedback_text: Some("ok".into()),
            feedback_score: None,
        });
        let s = serde_json::to_string(&entry).expect("serialize feedback");
        assert!(s.contains("\"type\":\"feedback\""));
        assert!(s.contains("\"qaId\":\"id\""));
        assert!(s.contains("\"feedbackText\":\"ok\""));
    }

    #[test]
    fn test_type_str_helper() {
        let q = MemoryEntry::Qa(QAEntry {
            question: "".into(),
            answer: "".into(),
            context: "".into(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: None,
        });
        assert_eq!(q.type_str(), "qa");

        let t = MemoryEntry::Trace(TraceEntry {
            origin_function: "x".into(),
            status: "success".into(),
            method_params: None,
            method_return_value: None,
            memory_query: "".into(),
            memory_context: "".into(),
            error_message: "".into(),
            generate_feedback_with_llm: false,
        });
        assert_eq!(t.type_str(), "trace");

        let f = MemoryEntry::Feedback(FeedbackEntry {
            qa_id: "x".into(),
            feedback_text: None,
            feedback_score: None,
        });
        assert_eq!(f.type_str(), "feedback");
    }
}
