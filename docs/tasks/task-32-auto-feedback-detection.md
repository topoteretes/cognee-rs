# Task 32: Add auto-feedback detection in session management

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python has an automatic feedback detection system (`cognee/infrastructure/session/feedback_detection.py`) that analyzes each user message via LLM to determine whether it contains feedback about a previous response. If feedback is detected, it is stored before the main search proceeds. The Rust session management does not yet include this automatic feedback detection step. This task ports the feedback detection mechanism to Rust.

## Current Rust State

The Rust `FeedbackRetriever` in `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs` handles explicit feedback via the `SearchType::Feedback` search type. It uses LLM-based `FeedbackAnalysis` to extract sentiment and score, then stores a `FeedbackNode` in the graph DB.

The `SearchOrchestrator` in `crates/search/src/orchestration/search_orchestrator.rs` processes search requests but does not automatically detect feedback in user queries before routing them.

The session management crate (`crates/session/`) handles Q&A history persistence but has no feedback detection logic.

## Python Reference

In `/tmp/cognee-python/cognee/infrastructure/session/feedback_detection.py`:

```python
async def detect_feedback(user_message: str) -> FeedbackDetectionResult:
    system_prompt = read_query_prompt("feedback_detection_system.txt")
    result = await LLMGateway.acreate_structured_output(
        text_input=user_message.strip(),
        system_prompt=system_prompt,
        response_model=FeedbackDetectionResult,
    )
    return result
```

The `FeedbackDetectionResult` model (from `/tmp/cognee-python/cognee/infrastructure/session/feedback_models.py`):

```python
class FeedbackDetectionResult(BaseModel):
    feedback_detected: bool
    feedback_text: Optional[str]
    feedback_score: Optional[float]       # 1-5 scale
    response_to_user: Optional[str]
    contains_followup_question: bool
```

The system prompt at `cognee/infrastructure/llm/prompts/feedback_detection_system.txt` instructs the LLM to detect whether the user is evaluating correctness/quality of the previous answer.

## Step-by-Step Changes

### Step 1: Create `FeedbackDetectionResult` struct

Add to `crates/search/src/types/` (or a new file `crates/search/src/types/feedback.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackDetectionResult {
    pub feedback_detected: bool,
    pub feedback_text: Option<String>,
    pub feedback_score: Option<f32>,
    pub response_to_user: Option<String>,
    pub contains_followup_question: bool,
}
```

### Step 2: Create `detect_feedback` function

Add a new module `crates/search/src/utils/feedback_detection.rs`:

```rust
pub async fn detect_feedback(
    llm: &dyn Llm,
    user_message: &str,
) -> Result<FeedbackDetectionResult, SearchError> {
    if user_message.trim().is_empty() {
        return Ok(FeedbackDetectionResult::no_feedback());
    }
    // Use LLM structured output with the feedback detection system prompt
    // On failure, return no_feedback() so the main flow is never blocked
}
```

### Step 3: Add feedback detection system prompt

Create `crates/search/prompts/feedback_detection_system.txt` (or embed as a `const` string) matching the Python prompt content.

### Step 4: Integrate into `SearchOrchestrator::search`

In `crates/search/src/orchestration/search_orchestrator.rs`, before routing to the retriever:

1. If session is active, call `detect_feedback(llm, &request.query_text)`.
2. If `feedback_detected` is true:
   - Store the feedback via `FeedbackRetriever::store_feedback` or similar.
   - If `contains_followup_question` is false, return an acknowledgment response.
   - If `contains_followup_question` is true, proceed with normal search using the original query.

This requires the orchestrator to have access to an `Arc<dyn Llm>` -- add it as an optional field.

### Step 5: Add configuration flag

Add an `auto_feedback_detection: bool` field to `SearchRequest` (default `false`) so callers can opt in, similar to Python's environment variable approach.

**Files to modify:**
- `crates/search/src/types/mod.rs` (new type)
- `crates/search/src/utils/feedback_detection.rs` (new file)
- `crates/search/src/utils/mod.rs` (register module)
- `crates/search/src/orchestration/search_orchestrator.rs` (integration)
- `crates/search/src/types/search_request.rs` (config flag)

## Test Verification

1. **Unit test:** `detect_feedback` with empty message returns `feedback_detected: false`.
2. **Unit test:** Mock LLM returns structured feedback result -- verify deserialization.
3. **Unit test:** LLM failure returns `no_feedback()` (graceful degradation).
4. **Integration test:** Orchestrator with `auto_feedback_detection: true` detects feedback and stores it, then proceeds with follow-up if present.

## Dependencies

- Requires `Arc<dyn Llm>` in `SearchOrchestrator` (add as optional field with builder method).
- Requires `schemars::JsonSchema` derive for structured output (already a workspace dependency).
- No blocking dependencies from other tasks.
