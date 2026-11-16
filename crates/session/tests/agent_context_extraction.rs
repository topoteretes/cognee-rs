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
    TRACE_EXTRACTION_INTERVAL, TRACE_EXTRACTION_STATE_ID, TRACE_EXTRACTION_STATE_KIND,
    TRACE_EXTRACTION_WATERMARK, extract_batch_agent_context, extract_live_agent_context,
    extract_pending_agent_context,
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
    // Automatic extraction off: these tests drive the passes themselves (the
    // trace-write hook has its own tests below).
    let manager =
        SessionManager::new(Arc::new(FsSessionStore::new(dir.path()))).with_auto_feedback(false);
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

// ---------------------------------------------------------------------------
// The trace-write hook (SDK-1048): Python's
// `SessionManager._maybe_extract_agent_context`, run by every
// `add_agent_trace_step` while automatic feedback analysis is on.
// ---------------------------------------------------------------------------

/// A manager over a fresh store with `auto_feedback` as given and,
/// optionally, an LLM for the batch pass.
fn hooked_manager(auto_feedback: bool, llm: Option<Arc<ScriptedLlm>>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = SessionManager::new(Arc::new(FsSessionStore::new(dir.path())))
        .with_auto_feedback(auto_feedback);
    if let Some(llm) = llm {
        manager = manager.with_llm(llm);
    }
    Fixture { _dir: dir, manager }
}

/// Store one trace step without LLM feedback; returns its trace id.
async fn write_step(manager: &SessionManager, origin: &str, status: &str, error: &str) -> String {
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
        .unwrap()
}

// Python `test_add_agent_trace_step_runs_extraction_when_enabled`.
#[tokio::test]
async fn a_trace_write_stores_the_live_failure_lesson() {
    let f = hooked_manager(true, None);
    let trace_id = write_step(&f.manager, "run_tests", "error", "exit code 1").await;
    let stored = lessons(&f.manager).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["section"], "failure_lessons");
    assert_eq!(stored[0]["source_trace_ids"], json!([trace_id]));
}

/// A successful step yields no live lesson, and below the interval the
/// batch pass asks nothing of the LLM.
#[tokio::test]
async fn a_successful_trace_write_below_the_interval_extracts_nothing() {
    let llm = ScriptedLlm::new(json!([]));
    let f = hooked_manager(true, Some(Arc::clone(&llm)));
    write_step(&f.manager, "run_tests", "success", "").await;
    assert!(lessons(&f.manager).await.is_empty());
    assert!(llm.prompts().is_empty());
}

// Python `test_add_agent_trace_step_skips_extraction_when_disabled`.
#[tokio::test]
async fn a_trace_write_extracts_nothing_while_auto_feedback_is_off() {
    let llm = ScriptedLlm::new(json!([lesson("tool_rules", "Never runs.", 0.9)]));
    let f = hooked_manager(false, Some(Arc::clone(&llm)));
    write_step(&f.manager, "run_tests", "error", "exit code 1").await;
    for i in 0..TRACE_EXTRACTION_INTERVAL {
        write_step(&f.manager, &format!("step-{i}"), "success", "").await;
    }
    assert!(rows(&f.manager).await.is_empty());
    assert!(llm.prompts().is_empty());
}

/// The batch pass runs once `TRACE_EXTRACTION_INTERVAL` steps are newer than
/// the watermark (Python's trace-write default), then advances it.
#[tokio::test]
async fn the_trace_write_runs_the_batch_pass_at_the_interval() {
    let llm = ScriptedLlm::new(json!([lesson(
        "success_patterns",
        "Run the focused test before the full suite.",
        0.9
    )]));
    let f = hooked_manager(true, Some(Arc::clone(&llm)));
    for i in 0..TRACE_EXTRACTION_INTERVAL - 1 {
        write_step(&f.manager, &format!("step-{i}"), "success", "").await;
    }
    assert!(llm.prompts().is_empty());
    assert_eq!(processed(&f.manager).await, None);

    write_step(&f.manager, "step-last", "success", "").await;
    assert_eq!(llm.prompts().len(), 1);
    assert_eq!(
        processed(&f.manager).await,
        Some(TRACE_EXTRACTION_INTERVAL as u64)
    );
    assert_eq!(lessons(&f.manager).await.len(), 1);

    // improve's flush shares the watermark: nothing is pending, no LLM call.
    let touched = extract_pending_agent_context(&f.manager, llm.as_ref(), SESSION, USER, 1, 3, 40)
        .await
        .unwrap();
    assert!(touched.is_empty());
    assert_eq!(llm.prompts().len(), 1);
}

/// Fail-open: a failing batch pass never fails the trace write, and leaves
/// the watermark for the next attempt.
#[tokio::test]
async fn a_failing_batch_pass_never_fails_the_trace_write() {
    let llm = ScriptedLlm::failing("llm down");
    let f = hooked_manager(true, Some(Arc::clone(&llm)));
    for i in 0..TRACE_EXTRACTION_INTERVAL {
        write_step(&f.manager, &format!("step-{i}"), "success", "").await;
    }
    assert_eq!(llm.prompts().len(), 1);
    assert_eq!(processed(&f.manager).await, None);
    let steps = f
        .manager
        .get_agent_trace_session(USER, Some(SESSION), None)
        .await
        .unwrap();
    assert_eq!(steps.len(), TRACE_EXTRACTION_INTERVAL);
}
