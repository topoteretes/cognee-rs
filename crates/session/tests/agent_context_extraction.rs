#![cfg(feature = "fs")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The agent-context extraction passes over a real (filesystem) session
//! store and a scripted LLM — port of Python's
//! `tests/unit/infrastructure/session/test_agent_context_extraction.py`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message};
use cognee_session::{
    FsSessionStore, MAX_BATCH_LESSONS, SessionContextRow, SessionManager,
    TRACE_EXTRACTION_STATE_ID, TRACE_EXTRACTION_STATE_KIND, TRACE_EXTRACTION_WATERMARK,
    extract_batch_agent_context, extract_live_agent_context, extract_pending_agent_context,
};
use serde_json::{Value, json};

const USER: &str = "u";
const SESSION: &str = "s";

/// Answers every structured call with `lessons` (or `error`), recording the
/// user prompt of each call.
struct ScriptedLlm {
    lessons: Value,
    error: Option<String>,
    configured: bool,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedLlm {
    fn new(lessons: Value) -> Arc<Self> {
        Arc::new(Self {
            lessons,
            error: None,
            configured: true,
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            lessons: json!([]),
            error: Some(message.to_string()),
            configured: true,
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn unconfigured() -> Arc<Self> {
        Arc::new(Self {
            lessons: json!([]),
            error: None,
            configured: false,
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::ApiError("generate is not scripted".to_string()))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let prompt = messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        self.prompts.lock().unwrap().push(prompt);
        match &self.error {
            Some(message) => Err(LlmError::ApiError(message.clone())),
            None => Ok(json!({ "lessons": self.lessons })),
        }
    }

    fn model(&self) -> &str {
        "scripted"
    }

    fn is_configured(&self) -> bool {
        self.configured
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    manager: SessionManager,
}

async fn fixture(traces: &[(&str, &str, &str)]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(Arc::new(FsSessionStore::new(dir.path())));
    for (origin, status, error) in traces {
        manager
            .add_agent_trace_step(
                USER,
                Some(SESSION),
                origin,
                status,
                "",
                "",
                json!({}),
                None,
                error,
                false,
            )
            .await
            .unwrap();
    }
    Fixture { _dir: dir, manager }
}

async fn steps(n: usize) -> Fixture {
    let names: Vec<String> = (0..n).map(|i| format!("step-{i}")).collect();
    let traces: Vec<(&str, &str, &str)> = names
        .iter()
        .map(|name| (name.as_str(), "success", ""))
        .collect();
    fixture(&traces).await
}

async fn rows(manager: &SessionManager) -> Vec<SessionContextRow> {
    manager
        .get_session_context_entries(Some(SESSION), Some(USER))
        .await
        .unwrap()
}

async fn lessons(manager: &SessionManager) -> Vec<SessionContextRow> {
    rows(manager)
        .await
        .into_iter()
        .filter(|row| row.get("kind").and_then(Value::as_str) == Some("context"))
        .collect()
}

async fn processed(manager: &SessionManager) -> Option<u64> {
    let rows = rows(manager).await;
    TRACE_EXTRACTION_WATERMARK
        .find_row(&rows)
        .map(|_| TRACE_EXTRACTION_WATERMARK.count_from_rows(&rows))
}

fn lesson(section: &str, content: &str, confidence: f64) -> Value {
    json!({"section": section, "content": content, "confidence": confidence})
}

// Python `test_extract_live_stores_agent_entry_linked_to_trace`.
#[tokio::test]
async fn a_live_failure_lesson_is_linked_to_its_trace() {
    let f = fixture(&[]).await;
    let touched = extract_live_agent_context(
        &f.manager,
        SESSION,
        USER,
        "trace-1",
        "run_tests",
        "error",
        "exit code 1",
    )
    .await;
    assert_eq!(touched.len(), 1);
    let stored = lessons(&f.manager).await;
    assert_eq!(stored[0]["context_profile"], "agent");
    assert_eq!(stored[0]["section"], "failure_lessons");
    assert_eq!(stored[0]["source_trace_ids"], json!(["trace-1"]));
    assert_eq!(stored[0]["source_feedback_ids"], json!([]));
}

// Python `test_extract_live_noop_on_success`.
#[tokio::test]
async fn a_successful_step_stores_no_live_lesson() {
    let f = fixture(&[]).await;
    let touched =
        extract_live_agent_context(&f.manager, SESSION, USER, "t", "run_tests", "success", "")
            .await;
    assert!(touched.is_empty());
    assert!(rows(&f.manager).await.is_empty());
}

// Python `test_batch_stores_llm_lessons`.
#[tokio::test]
async fn the_batch_pass_stores_the_proposed_lessons() {
    let f = fixture(&[("run_tests", "error", "exit 1")]).await;
    let llm = ScriptedLlm::new(json!([
        lesson("environment_facts", "Tests need uv sync first", 0.9),
        lesson("tool_rules", "Run tests with uv run pytest", 0.8),
    ]));
    let touched = extract_batch_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 40)
        .await
        .unwrap();
    assert_eq!(touched.len(), 2);
    let stored = lessons(&f.manager).await;
    let mut sections: Vec<&str> = stored
        .iter()
        .map(|row| row["section"].as_str().unwrap())
        .collect();
    sections.sort_unstable();
    assert_eq!(sections, ["environment_facts", "tool_rules"]);
    // A batch lesson has no single source trace.
    assert!(
        stored
            .iter()
            .all(|row| row["source_trace_ids"] == json!([]))
    );
    assert!(stored.iter().all(|row| row["context_profile"] == "agent"));
}

// Python `test_batch_noop_without_traces`.
#[tokio::test]
async fn the_batch_pass_without_traces_calls_no_llm() {
    let f = fixture(&[]).await;
    let llm = ScriptedLlm::new(json!([lesson("tool_rules", "x", 0.9)]));
    let touched = extract_batch_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
    assert!(llm.prompts().is_empty());
}

// Python `test_batch_llm_error_propagates`.
#[tokio::test]
async fn an_llm_error_propagates_from_the_batch_pass() {
    let f = fixture(&[("run_tests", "error", "exit 1")]).await;
    let error = extract_batch_agent_context(
        &f.manager,
        ScriptedLlm::failing("llm down").as_ref(),
        SESSION,
        USER,
        40,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("llm down"), "{error}");
    assert!(rows(&f.manager).await.is_empty());
}

// Python `test_batch_shows_existing_lessons_to_the_model`.
#[tokio::test]
async fn the_model_is_shown_the_stored_lessons() {
    let f = fixture(&[("run_tests", "error", "exit 1")]).await;
    extract_live_agent_context(
        &f.manager,
        SESSION,
        USER,
        "t0",
        "uv",
        "error",
        "old failure",
    )
    .await;
    let llm = ScriptedLlm::new(json!([]));
    extract_batch_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 40)
        .await
        .unwrap();
    let prompt = &llm.prompts()[0];
    assert!(prompt.contains("EXISTING LESSONS"), "{prompt}");
    assert!(prompt.contains("[failure_lessons] uv failed: old failure"));
    assert!(prompt.contains("TRACES:"));
}

// Python `test_batch_caps_new_lessons_per_run`.
#[tokio::test]
async fn a_batch_stores_at_most_five_lessons() {
    let f = fixture(&[("run_tests", "error", "exit 1")]).await;
    let proposed: Vec<Value> = (0..7)
        .map(|i| lesson("tool_rules", &format!("Lesson number {i}."), 0.9))
        .collect();
    let touched = extract_batch_agent_context(
        &f.manager,
        ScriptedLlm::new(Value::Array(proposed)).as_ref(),
        SESSION,
        USER,
        40,
    )
    .await
    .unwrap();
    assert_eq!(touched.len(), MAX_BATCH_LESSONS);
    assert_eq!(lessons(&f.manager).await.len(), MAX_BATCH_LESSONS);
}

#[tokio::test]
async fn low_confidence_lessons_are_not_stored() {
    let f = fixture(&[("run_tests", "error", "exit 1")]).await;
    let llm = ScriptedLlm::new(json!([lesson("tool_rules", "Maybe retry", 0.5)]));
    let touched = extract_batch_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
}

// Python `test_pending_extraction_noop_below_interval`.
#[tokio::test]
async fn nothing_runs_below_the_interval() {
    let f = steps(2).await;
    let llm = ScriptedLlm::new(json!([]));
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 3, 1, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
    assert!(llm.prompts().is_empty());
    assert_eq!(processed(&f.manager).await, None);
}

// Python `test_pending_extraction_noop_without_a_usable_llm`.
#[tokio::test]
async fn nothing_runs_without_a_configured_llm() {
    let f = steps(3).await;
    let llm = ScriptedLlm::unconfigured();
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 3, 1, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
    assert!(llm.prompts().is_empty());
    assert!(rows(&f.manager).await.is_empty());
}

// Python `test_pending_extraction_runs_at_interval_and_sets_watermark`.
#[tokio::test]
async fn the_pending_pass_runs_at_the_interval_and_sets_the_watermark() {
    let f = steps(3).await;
    let llm = ScriptedLlm::new(json!([lesson(
        "success_patterns",
        "Batch trace windows can produce reusable lessons.",
        0.9
    )]));
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 3, 1, 40)
        .await
        .unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(processed(&f.manager).await, Some(3));
}

// Python `test_pending_extraction_can_flush_below_interval`.
#[tokio::test]
async fn the_pending_pass_can_flush_below_the_interval() {
    let f = steps(2).await;
    let llm = ScriptedLlm::new(json!([lesson(
        "tool_rules",
        "Flush pending traces before distillation.",
        0.9
    )]));
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 1, 1, 40)
        .await
        .unwrap();
    assert_eq!(touched.len(), 1);
    assert_eq!(processed(&f.manager).await, Some(2));
}

// Python `test_pending_extraction_uses_overlap_after_watermark`.
#[tokio::test]
async fn the_pending_pass_reads_an_overlap_past_the_watermark() {
    let f = steps(13).await;
    TRACE_EXTRACTION_WATERMARK
        .write_count(&f.manager, Some(SESSION), Some(USER), 10)
        .await
        .unwrap();
    let llm = ScriptedLlm::new(json!([]));
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 3, 2, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
    // 3 pending + 2 overlap: steps 8..=12.
    let prompt = &llm.prompts()[0];
    assert!(prompt.contains("step-8 "), "{prompt}");
    assert!(prompt.contains("step-12 "));
    assert!(!prompt.contains("step-7 "));
    assert_eq!(processed(&f.manager).await, Some(13));
}

// Python `test_pending_extraction_does_not_advance_watermark_on_llm_error`.
#[tokio::test]
async fn an_llm_error_leaves_the_watermark_alone() {
    let f = steps(3).await;
    let error = extract_pending_agent_context(
        &f.manager,
        ScriptedLlm::failing("llm down").as_ref(),
        SESSION,
        USER,
        3,
        1,
        40,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("llm down"));
    assert_eq!(processed(&f.manager).await, None);
}

// Python `test_stage4_watermark_is_the_shared_state_row_helper` and
// `test_pending_extraction_reads_and_writes_through_the_shared_helper`.
#[tokio::test]
async fn the_pending_pass_shares_the_state_row_watermark() {
    assert_eq!(
        TRACE_EXTRACTION_WATERMARK.state_id,
        TRACE_EXTRACTION_STATE_ID
    );
    assert_eq!(
        TRACE_EXTRACTION_WATERMARK.state_kind,
        TRACE_EXTRACTION_STATE_KIND
    );
    assert_eq!(TRACE_EXTRACTION_WATERMARK.field, "processed_trace_count");

    let f = steps(5).await;
    TRACE_EXTRACTION_WATERMARK
        .write_count(&f.manager, Some(SESSION), Some(USER), 3)
        .await
        .unwrap();
    let llm = ScriptedLlm::new(json!([]));
    extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 1, 0, 40)
        .await
        .unwrap();
    // 5 total - 3 processed = 2 pending, read with no overlap.
    let prompt = &llm.prompts()[0];
    assert!(prompt.contains("step-3 ") && prompt.contains("step-4 "));
    assert!(!prompt.contains("step-2 "));
    assert_eq!(processed(&f.manager).await, Some(5));
    let state_rows = rows(&f.manager)
        .await
        .into_iter()
        .filter(|row| row.get("kind").and_then(Value::as_str) == Some(TRACE_EXTRACTION_STATE_KIND))
        .count();
    assert_eq!(state_rows, 1);
}
