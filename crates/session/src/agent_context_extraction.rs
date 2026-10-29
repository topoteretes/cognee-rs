//! Turn agent trace evidence into agent-profile session-context lessons.
//!
//! Port of Python's `cognee/infrastructure/session/agent_context_extraction.py`.
//! Two passes feed the shared applier ([`apply_candidate_updates`]), so
//! lessons are confidence-gated and de-duplicated like Q&A guidance:
//!
//! 1. LIVE (no LLM): an errored trace step becomes a `failure_lessons`
//!    candidate built from its (redacted) error text —
//!    [`build_live_agent_candidates`].
//! 2. BATCH (LLM): the pending trace steps are rendered into one prompt
//!    together with the lessons already stored, and the model proposes at
//!    most [`MAX_BATCH_LESSONS`] new ones — [`extract_pending_agent_context`],
//!    which `improve()`'s `extract_agent_context` stage runs per session.
//!
//! The batch pass advances the [`TRACE_EXTRACTION_WATERMARK`] only after it
//! succeeded, and it propagates its errors: an LLM outage must neither be
//! mistaken for "no lessons" nor skip the traces it never read.

use std::sync::LazyLock;

use cognee_llm::{Llm, LlmExt};
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context_builder::{CandidateContextUpdate, apply_candidate_updates};
use crate::context_entry::{
    CONTEXT_PROFILE_AGENT, MAX_CONTEXT_CONTENT_CHARS, SessionContextEntry,
    coerce_active_context_entries,
};
use crate::error::SessionError;
use crate::session_manager::SessionManager;
use crate::types::SessionTraceStep;
use crate::watermark::TRACE_EXTRACTION_WATERMARK;

/// Confidence of a live failure lesson: an errored step is a strong signal,
/// so it clears the 0.75 gate comfortably.
pub const LIVE_FAILURE_CONFIDENCE: f64 = 0.85;
/// Most trace steps one batch reads.
pub const BATCH_TRACE_LIMIT: usize = 40;
/// How much of a step's (serialized) output a batch line keeps.
pub const BATCH_RETURN_CHARS: usize = 600;
/// New lessons kept from one batch.
pub const MAX_BATCH_LESSONS: usize = 5;
/// Stored agent lessons shown to the model so it does not repeat them.
pub const EXISTING_LESSONS_SHOWN: usize = 40;
/// Longest error text kept on a batch line.
pub const MAX_ERROR_TEXT_CHARS: usize = 600;
/// New traces that trigger a mid-session extraction (the improve stage
/// flushes from 1).
pub const TRACE_EXTRACTION_INTERVAL: usize = 10;
/// Already-processed traces re-read before the pending ones, for context.
pub const TRACE_EXTRACTION_OVERLAP: usize = 3;

/// The batch pass's system prompt (Python's
/// `agent_context_extraction_system.txt`, verbatim).
pub const AGENT_CONTEXT_EXTRACTION_PROMPT: &str =
    include_str!("prompts/agent_context_extraction_system.txt");

/// Strings longer than this are cut in a sanitized trace output.
const MAX_SERIALIZED_VALUE_LENGTH: usize = 1000;
/// Items kept from a list or object in a sanitized trace output.
const MAX_TRACE_CONTAINER_ITEMS: usize = 20;

#[allow(clippy::expect_used, reason = "the patterns are fixed and unit-tested")]
fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("the redaction patterns are fixed and valid")
}

static JWT_RE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"));
static BEARER_RE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(Bearer)\s+[A-Za-z0-9._~+/=-]+"));
static SECRET_ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|password|secret)\s*([=:])\s*([^\s,;]+)",
    )
});
static LONG_HEX_RE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b[0-9a-fA-F]{32,}\b"));
static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
});
static LONG_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b\d{8,}\b"));

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Redact sensitive and volatile values before an error becomes reusable
/// context (Python's `_sanitize_error_text`): bearer tokens, secret
/// assignments, JWTs and long hex strings become `[redacted]`, UUIDs
/// `[uuid]`, numbers of 8+ digits `[number]`; whitespace is collapsed.
pub fn sanitize_error_text(error_message: &str, max_chars: usize) -> String {
    let error = error_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let error = BEARER_RE.replace_all(&error, "${1} [redacted]");
    let error = SECRET_ASSIGNMENT_RE.replace_all(&error, "${1}${2}[redacted]");
    let error = JWT_RE.replace_all(&error, "[redacted]");
    let error = LONG_HEX_RE.replace_all(&error, "[redacted]");
    let error = UUID_RE.replace_all(&error, "[uuid]");
    let error = LONG_NUMBER_RE.replace_all(&error, "[number]");
    truncate_chars(&error, max_chars)
}

/// Deterministic, no-LLM candidates from one trace step: an errored step
/// with an error message becomes a `failure_lessons` candidate.
pub fn build_live_agent_candidates(
    origin_function: &str,
    status: &str,
    error_message: &str,
) -> Vec<CandidateContextUpdate> {
    if status != "error" || error_message.trim().is_empty() {
        return Vec::new();
    }
    let origin = match origin_function.trim() {
        "" => "a tool",
        origin => origin,
    };
    let error = sanitize_error_text(error_message, MAX_CONTEXT_CONTENT_CHARS);
    let content = truncate_chars(
        &format!("{origin} failed: {error}"),
        MAX_CONTEXT_CONTENT_CHARS,
    );
    vec![CandidateContextUpdate::new(
        CONTEXT_PROFILE_AGENT,
        "failure_lessons",
        &content,
        LIVE_FAILURE_CONFIDENCE,
    )]
}

/// Store the lessons derivable from one just-saved trace step. Fail-open:
/// any error is logged and yields no lesson.
pub async fn extract_live_agent_context(
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    trace_id: &str,
    origin_function: &str,
    status: &str,
    error_message: &str,
) -> Vec<String> {
    let candidates = build_live_agent_candidates(origin_function, status, error_message);
    if candidates.is_empty() {
        return Vec::new();
    }
    apply_candidate_updates(manager, session_id, user_id, trace_id, &candidates).await
}

/// One agent-profile section the batch pass may propose a lesson under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentContextSection {
    /// A reusable constraint or safe/required way to invoke a tool.
    ToolRules,
    /// Where the task stands and what must survive context compaction.
    WorkflowState,
    /// An approach worth repeating.
    SuccessPatterns,
    /// An error, retry or rejected path and what to do differently.
    FailureLessons,
    /// A discovered fact about the repository, runtime or environment.
    EnvironmentFacts,
}

impl AgentContextSection {
    /// The section's stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolRules => "tool_rules",
            Self::WorkflowState => "workflow_state",
            Self::SuccessPatterns => "success_patterns",
            Self::FailureLessons => "failure_lessons",
            Self::EnvironmentFacts => "environment_facts",
        }
    }
}

impl std::str::FromStr for AgentContextSection {
    type Err = String;

    /// Python's `section_valid`: trimmed and lowercased before it is checked.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let normalized = raw.trim().to_lowercase();
        [
            Self::ToolRules,
            Self::WorkflowState,
            Self::SuccessPatterns,
            Self::FailureLessons,
            Self::EnvironmentFacts,
        ]
        .into_iter()
        .find(|section| section.as_str() == normalized)
        .ok_or_else(|| format!("section must be one of the agent sections, got {raw:?}"))
    }
}

/// A lesson's `section`, normalized as Python's validator does.
fn deserialize_section<'de, D>(deserializer: D) -> Result<AgentContextSection, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    raw.parse().map_err(serde::de::Error::custom)
}

/// A lesson's `confidence` as Python's `confidence_clamp` reads it: `null`
/// is 0, and a number is clamped to `[0, 1]`.
fn deserialize_confidence<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<f64>::deserialize(deserializer)?;
    Ok(value.unwrap_or(0.0).clamp(0.0, 1.0))
}

/// One lesson the batch pass proposes (Python's agent candidate variants).
///
/// Parsed as leniently as Python's validators: the section is trimmed and
/// lowercased first and a `null` confidence is 0 — a lesson Python accepts
/// must not fail the whole batch here, or the session's watermark never
/// advances.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentLesson {
    /// Which kind of tool/workflow lesson the content is.
    #[serde(deserialize_with = "deserialize_section")]
    pub section: AgentContextSection,
    /// One short, self-contained sentence: a takeaway, not a description of
    /// one specific call.
    pub content: String,
    /// Confidence from 0 to 1. Only lessons with confidence >= 0.75 are
    /// stored.
    #[serde(default, deserialize_with = "deserialize_confidence")]
    pub confidence: f64,
}

/// The batch pass's LLM output (Python's `AgentContextExtraction`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentContextExtraction {
    /// Reusable agent/tool lessons drawn from the traces (at most 5).
    #[serde(default)]
    pub lessons: Vec<AgentLesson>,
}

/// Python's `json.dumps` separators (`", "` and `": "`), so a trace line
/// reads as Python renders it.
struct PythonJsonFormatter;

impl serde_json::ser::Formatter for PythonJsonFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

fn python_json(value: &Value) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonJsonFormatter);
    if value.serialize(&mut serializer).is_err() {
        return value.to_string();
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

/// A trace output made safe to show (Python's `sanitize_value` over JSON):
/// strings are cut to 1000 characters (ending in `...`), lists and objects
/// keep their first 20 items.
fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_SERIALIZED_VALUE_LENGTH => {
            let mut cut = truncate_chars(text, MAX_SERIALIZED_VALUE_LENGTH - 3);
            cut.push_str("...");
            Value::String(cut)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(MAX_TRACE_CONTAINER_ITEMS)
                .map(sanitize_value)
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .take(MAX_TRACE_CONTAINER_ITEMS)
                .map(|(key, item)| (key.clone(), sanitize_value(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn is_empty_output(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// One trace step as a compact prompt line (Python's `_trace_line`).
fn trace_line(step: &SessionTraceStep) -> String {
    let mut parts = vec![format!("- {} [{}]", step.origin_function, step.status)];
    if !step.session_feedback.is_empty() {
        parts.push(format!("feedback: {}", step.session_feedback));
    }
    if !step.error_message.is_empty() {
        parts.push(format!(
            "error: {}",
            sanitize_error_text(&step.error_message, MAX_ERROR_TEXT_CHARS)
        ));
    }
    if let Some(output) = step.method_return_value.as_ref().map(sanitize_value)
        && !is_empty_output(&output)
    {
        parts.push(format!(
            "output: {}",
            truncate_chars(&python_json(&output), BATCH_RETURN_CHARS)
        ));
    }
    parts.join(" | ")
}

/// Trace steps joined into one prompt blob; empty when there is nothing to
/// summarize.
pub fn build_trace_batch(traces: &[SessionTraceStep]) -> String {
    traces
        .iter()
        .map(trace_line)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stored lessons and the new traces as one prompt input; showing the
/// existing lessons keeps the model from re-proposing them.
pub fn build_extraction_input(
    trace_batch: &str,
    existing_lessons: &[SessionContextEntry],
) -> String {
    let mut sections = Vec::new();
    if !existing_lessons.is_empty() {
        let rendered = existing_lessons
            .iter()
            .map(|entry| format!("- [{}] {}", entry.section, entry.content))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "EXISTING LESSONS (already saved — do not repeat or reword these):\n{rendered}"
        ));
    }
    sections.push(format!("TRACES:\n{trace_batch}"));
    sections.join("\n\n")
}

/// The session's agent-profile lessons, capped for the prompt.
async fn existing_agent_lessons(
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
) -> Result<Vec<SessionContextEntry>, SessionError> {
    let rows = manager
        .get_session_context_entries(Some(session_id), Some(user_id))
        .await?;
    Ok(coerce_active_context_entries(&rows)?
        .into_iter()
        .filter(|entry| entry.context_profile == CONTEXT_PROFILE_AGENT)
        .take(EXISTING_LESSONS_SHOWN)
        .collect())
}

/// The LLM batch pass over an already-selected trace window; returns the
/// ids of the lessons stored. Errors (the LLM, the store) propagate.
async fn extract_batch_from_traces(
    manager: &SessionManager,
    llm: &dyn Llm,
    session_id: &str,
    user_id: &str,
    traces: &[SessionTraceStep],
) -> Result<Vec<String>, SessionError> {
    let batch = build_trace_batch(traces);
    if batch.trim().is_empty() {
        return Ok(Vec::new());
    }
    let existing = existing_agent_lessons(manager, session_id, user_id).await?;
    let extraction: AgentContextExtraction = llm
        .create_structured_output(
            &build_extraction_input(&batch, &existing),
            AGENT_CONTEXT_EXTRACTION_PROMPT,
            None,
        )
        .await
        .map_err(SessionError::Llm)?;
    let candidates: Vec<CandidateContextUpdate> = extraction
        .lessons
        .iter()
        .take(MAX_BATCH_LESSONS)
        .map(|lesson| {
            CandidateContextUpdate::new(
                CONTEXT_PROFILE_AGENT,
                lesson.section.as_str(),
                &lesson.content,
                lesson.confidence,
            )
        })
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // A batch lesson has no single source trace: link none and rely on the
    // content dedup.
    Ok(apply_candidate_updates(manager, session_id, user_id, "", &candidates).await)
}

/// The batch pass over the session's last `last_n` trace steps.
pub async fn extract_batch_agent_context(
    manager: &SessionManager,
    llm: &dyn Llm,
    session_id: &str,
    user_id: &str,
    last_n: usize,
) -> Result<Vec<String>, SessionError> {
    let traces = manager
        .get_agent_trace_session(user_id, Some(session_id), Some(last_n))
        .await?;
    extract_batch_from_traces(manager, llm, session_id, user_id, &traces).await
}

/// The trace window for `pending` unprocessed steps:
/// `max(1, min(max_window, pending + overlap))`.
pub fn trace_window_size(pending: usize, overlap: usize, max_window: usize) -> usize {
    pending.saturating_add(overlap).min(max_window).max(1)
}

/// Extract agent lessons from the session's unprocessed trace steps; returns
/// the ids of the lessons stored.
///
/// Nothing runs without a configured LLM, or while fewer than
/// `min_new_traces` steps are newer than the watermark (`0` never runs). The
/// model sees the last `pending + overlap` steps, at most `max_window`. The
/// watermark advances to the trace count only after the extraction
/// succeeded; failures propagate, the caller owns the fail-open policy.
pub async fn extract_pending_agent_context(
    manager: &SessionManager,
    llm: &dyn Llm,
    session_id: &str,
    user_id: &str,
    min_new_traces: usize,
    overlap: usize,
    max_window: usize,
) -> Result<Vec<String>, SessionError> {
    if !llm.is_configured() || min_new_traces == 0 {
        return Ok(Vec::new());
    }
    let traces = manager
        .get_agent_trace_session(user_id, Some(session_id), None)
        .await?;
    let total = traces.len();
    let processed = TRACE_EXTRACTION_WATERMARK
        .read_count(manager, Some(session_id), Some(user_id))
        .await?;
    let pending = total.saturating_sub(usize::try_from(processed).unwrap_or(usize::MAX));
    if pending < min_new_traces {
        return Ok(Vec::new());
    }
    let window = trace_window_size(pending, overlap, max_window);
    let start = total.saturating_sub(window);
    let touched =
        extract_batch_from_traces(manager, llm, session_id, user_id, &traces[start..]).await?;
    TRACE_EXTRACTION_WATERMARK
        .write_count(
            manager,
            Some(session_id),
            Some(user_id),
            i64::try_from(total).unwrap_or(i64::MAX),
        )
        .await?;
    Ok(touched)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    fn step(origin: &str, status: &str, error: &str, feedback: &str) -> SessionTraceStep {
        SessionTraceStep {
            trace_id: "t".to_string(),
            origin_function: origin.to_string(),
            status: status.to_string(),
            memory_query: String::new(),
            memory_context: String::new(),
            method_params: json!({}),
            method_return_value: None,
            error_message: error.to_string(),
            session_feedback: feedback.to_string(),
        }
    }

    // Python `test_errored_step_builds_failure_candidate`.
    #[test]
    fn an_errored_step_builds_a_failure_candidate() {
        let candidates =
            build_live_agent_candidates("run_tests", "error", "ModuleNotFoundError: dotenv");
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.section, "failure_lessons");
        assert_eq!(candidate.context_profile, "agent");
        assert_eq!(candidate.confidence, LIVE_FAILURE_CONFIDENCE);
        assert!(candidate.content.contains("run_tests failed"));
        assert!(candidate.content.contains("dotenv"));
    }

    // Python `test_errored_step_redacts_sensitive_error_text`.
    #[test]
    fn an_errored_step_redacts_sensitive_error_text() {
        let candidates = build_live_agent_candidates(
            "call_api",
            "error",
            "Request failed: Bearer sk-live-secret token=abc123 request_id=123456789 \
             path /tmp/550e8400-e29b-41d4-a716-446655440000",
        );
        let content = &candidates[0].content;
        for secret in [
            "sk-live-secret",
            "abc123",
            "123456789",
            "550e8400-e29b-41d4-a716-446655440000",
        ] {
            assert!(!content.contains(secret), "{secret} leaked: {content}");
        }
        for kept in [
            "Bearer [redacted]",
            "token=[redacted]",
            "request_id=[number]",
            "[uuid]",
        ] {
            assert!(content.contains(kept), "{kept} missing: {content}");
        }
    }

    // Python `test_successful_step_builds_no_candidate` and
    // `test_errored_step_without_message_builds_no_candidate`.
    #[test]
    fn success_or_a_blank_error_builds_no_candidate() {
        assert!(build_live_agent_candidates("run_tests", "success", "").is_empty());
        assert!(build_live_agent_candidates("run_tests", "error", "   ").is_empty());
    }

    // Python `test_build_trace_batch_renders_compact_lines`.
    #[test]
    fn a_trace_batch_renders_compact_lines() {
        let mut read = step("read_file", "success", "", "read config");
        read.method_return_value = Some(json!({"k": "v"}));
        let batch = build_trace_batch(&[step("run_tests", "error", "exit 1", ""), read]);
        assert!(batch.contains("run_tests [error]"));
        assert!(batch.contains("error: exit 1"));
        assert!(batch.contains("read_file [success] | feedback: read config"));
        assert!(batch.contains(r#"output: {"k": "v"}"#), "{batch}");
    }

    // Python `test_build_trace_batch_redacts_sensitive_error_text`.
    #[test]
    fn a_trace_batch_redacts_sensitive_error_text() {
        let batch = build_trace_batch(&[step(
            "fetch_remote",
            "error",
            "HTTP 401 api_key=super-secret jwt=eyJabc.def.ghi request 987654321",
            "",
        )]);
        assert!(!batch.contains("super-secret"));
        assert!(!batch.contains("eyJabc.def.ghi"));
        assert!(!batch.contains("987654321"));
        assert!(batch.contains("api_key=[redacted]"));
        assert!(batch.contains("jwt=[redacted]"));
        assert!(batch.contains("request [number]"));
    }

    #[test]
    fn empty_outputs_are_not_rendered_and_long_ones_are_cut() {
        let mut empty = step("noop", "success", "", "");
        empty.method_return_value = Some(json!([]));
        assert_eq!(build_trace_batch(&[empty]), "- noop [success]");

        let mut long = step("dump", "success", "", "");
        long.method_return_value = Some(json!("x".repeat(5000)));
        let line = build_trace_batch(&[long]);
        let output = line.split("output: ").nth(1).unwrap();
        assert_eq!(output.chars().count(), BATCH_RETURN_CHARS);
    }

    #[test]
    fn the_window_is_pending_plus_overlap_capped_and_at_least_one() {
        assert_eq!(trace_window_size(3, 1, 40), 4);
        assert_eq!(trace_window_size(100, 3, 40), 40);
        assert_eq!(trace_window_size(0, 0, 40), 1);
    }

    #[test]
    fn existing_lessons_lead_the_extraction_input() {
        let entry = SessionContextEntry::from_row(
            json!({
                "id": "e1",
                "section": "failure_lessons",
                "context_profile": "agent",
                "content": "Old lesson about uv.",
                "created_at": "2026-06-11T10:00:00",
            })
            .as_object()
            .unwrap(),
        )
        .unwrap();
        let input = build_extraction_input("- step [ok]", &[entry]);
        assert_eq!(
            input,
            "EXISTING LESSONS (already saved — do not repeat or reword these):\n\
             - [failure_lessons] Old lesson about uv.\n\nTRACES:\n- step [ok]"
        );
        assert_eq!(build_extraction_input("x", &[]), "TRACES:\nx");
    }

    // Python's validators lowercase the section and read a `null` confidence
    // as 0: such a lesson parses (and is gated out by its confidence) instead
    // of failing the batch.
    #[test]
    fn lessons_parse_as_leniently_as_python() {
        let extraction: AgentContextExtraction = serde_json::from_value(serde_json::json!({
            "lessons": [
                {"section": " Tool_Rules ", "content": "Quote paths.", "confidence": 0.9},
                {"section": "failure_lessons", "content": "Retry once.", "confidence": null},
                {"section": "environment_facts", "content": "Rust 1.91.", "confidence": 3}
            ]
        }))
        .unwrap();
        let parsed: Vec<_> = extraction
            .lessons
            .iter()
            .map(|l| (l.section, l.confidence))
            .collect();
        assert_eq!(
            parsed,
            [
                (AgentContextSection::ToolRules, 0.9),
                (AgentContextSection::FailureLessons, 0.0),
                (AgentContextSection::EnvironmentFacts, 1.0),
            ]
        );
        assert!(
            serde_json::from_value::<AgentLesson>(serde_json::json!({
                "section": "goals", "content": "x", "confidence": 1
            }))
            .is_err(),
            "a section outside the agent profile is still rejected"
        );
    }
}
