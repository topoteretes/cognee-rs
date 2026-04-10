# Task 21: Align session history injection -- prepend to system prompt as `history + "\nTASK:" + prompt`

## Summary

Python's completion utility prepends conversation history directly into the system prompt using the format `conversation_history + "\nTASK:" + system_prompt`. This means the LLM sees a single system message where the history comes first, followed by a `\nTASK:` separator, followed by the original system instructions.

Rust currently injects session history as separate `User`/`Assistant` messages between the system prompt and the current user prompt. This is a different multi-turn conversation approach that changes the LLM's behavior: the model sees history as prior conversation turns rather than as context prepended to its instructions.

## Current Rust Behavior

**File:** `crates/search/src/utils/session_messages.rs`

```rust
pub fn build_messages_with_history(
    system_prompt: String,
    user_prompt: String,
    session: &SessionContext,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(2 + session.history.len());
    messages.push(Message::system(system_prompt));
    messages.extend(session.history.iter().cloned());
    messages.push(Message::user(user_prompt));
    messages
}
```

This produces a message list like:

```
[System("You are a helpful assistant...")]
[User("What is Rust?")]                      <-- history Q1
[Assistant("A systems programming language")]  <-- history A1
[User("Tell me more")]                        <-- history Q2
[Assistant("It focuses on safety")]            <-- history A2
[User("How does ownership work?")]             <-- current user prompt
```

**File:** `crates/session/src/session_manager.rs`

The `SessionManager::load_history_messages` method converts Q&A entries to alternating `User`/`Assistant` messages:

```rust
fn entries_to_messages(entries: &[SessionQAEntry]) -> Vec<Message> {
    let mut messages = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        messages.push(Message::user(&entry.question));
        messages.push(Message::assistant(&entry.answer));
    }
    messages
}
```

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/completion.py`, lines 23-24

```python
if conversation_history:
    system_prompt = conversation_history + "\nTASK:" + system_prompt
```

This produces a single system message with history prepended:

```
[System("Previous conversation:\n\n[2025-01-15T10:00:00]\nQUESTION: What is Rust?\nANSWER: A systems programming language.\n\n\nTASK:You are a helpful assistant...")]
[User("Question:\nHow does ownership work?\n\nContext:\n<context text>")]
```

**File:** `/tmp/cognee-python/cognee/infrastructure/session/session_manager.py`, lines 120-129

The `_get_formatted_history` method produces a formatted string (not structured messages):

```python
async def _get_formatted_history(self, user_id: str, session_id: str) -> str:
    history = await self.get_session(
        user_id=user_id,
        session_id=session_id,
        formatted=True,
        last_n=self.session_history_last_n,
        include_context=False,
    )
    return history if isinstance(history, str) else ""
```

And `format_entries` (lines 260-278) formats entries as:

```python
def format_entries(entries, include_context=True):
    lines = ["Previous conversation:\n\n"]
    for entry in entries:
        lines.append(f"[{entry.get('time', 'Unknown time')}]\n")
        lines.append(f"QUESTION: {entry.get('question', '')}\n")
        if include_context:
            lines.append(f"CONTEXT: {entry.get('context', '')}\n")
        lines.append(f"ANSWER: {entry.get('answer', '')}\n\n")
    return "".join(lines)
```

Key difference: Python formats history as a **text block** and prepends it to the system prompt. Rust converts history into **structured User/Assistant messages** interleaved in the message list.

## Step-by-Step Code Changes

### Change 1: Add `format_entries_for_prompt` to `SessionManager`

**File:** `crates/session/src/session_manager.rs`

Add a new method that returns a formatted history string (matching Python's `format_entries` with `include_context=False`):

```rust
/// Format Q&A entries as a prompt-ready string for prepending to system prompt.
/// Matches Python's `SessionManager.format_entries(entries, include_context=False)`.
pub fn format_entries_for_prompt(entries: &[SessionQAEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Previous conversation:\n\n".to_string()];
    for entry in entries {
        lines.push(format!("[{}]\n", entry.created_at.to_rfc3339()));
        lines.push(format!("QUESTION: {}\n", entry.question));
        lines.push(format!("ANSWER: {}\n\n", entry.answer));
    }
    lines.concat()
}
```

Note: This is essentially the same as the existing `format_entries` method. The existing method already omits context (unlike Python where `include_context=True` is default). So the existing `format_entries` can be reused directly.

### Change 2: Add `load_history_formatted` to `SessionManager`

**File:** `crates/session/src/session_manager.rs`

Add a new method that loads history as a formatted string:

```rust
/// Load conversation history as a formatted text string for system prompt injection.
/// Returns empty string if no history is found.
pub async fn load_history_formatted(
    &self,
    session_id: Option<&str>,
    user_id: Option<&str>,
) -> Result<String, SessionError> {
    let resolved_id = self.resolve_session_id(session_id);
    let entries = self
        .store
        .get_latest_qa_entries(resolved_id, user_id, self.history_limit)
        .await?;

    debug!(
        session_id = resolved_id,
        entries = entries.len(),
        "Loaded session history (formatted)"
    );

    Ok(Self::format_entries(&entries))
}
```

### Change 3: Update `build_messages_with_history` to prepend history to system prompt

**File:** `crates/search/src/utils/session_messages.rs`

Replace the current implementation that interleaves history as separate messages with one that prepends history text to the system prompt:

```rust
use cognee_llm::Message;
use cognee_session::SessionContext;

/// Build the full LLM message list, injecting any session history by
/// prepending it to the system prompt (matching Python's
/// `conversation_history + "\nTASK:" + system_prompt` pattern).
pub fn build_messages_with_history(
    system_prompt: String,
    user_prompt: String,
    session: &SessionContext,
) -> Vec<Message> {
    let effective_system_prompt = if session.formatted_history.is_empty() {
        system_prompt
    } else {
        format!("{}\nTASK:{}", session.formatted_history, system_prompt)
    };

    vec![
        Message::system(effective_system_prompt),
        Message::user(user_prompt),
    ]
}
```

### Change 4: Add `formatted_history` field to `SessionContext`

**File:** `crates/session/src/types.rs`

Extend `SessionContext` to carry the formatted history string:

```rust
/// Session context passed to retrievers: the session ID, loaded
/// conversation history (as LLM messages for backward compat), and
/// formatted history string for system prompt injection.
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub session_id: Option<String>,
    pub history: Vec<Message>,
    pub formatted_history: String,
}
```

### Change 5: Populate `formatted_history` in `SearchOrchestrator::search`

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

Update the session context construction to also load the formatted history:

```rust
let session_context =
    if let (Some(session_id), Some(sm)) = (&request.session_id, &self.session_manager) {
        let history = sm
            .load_history_messages(Some(session_id), None)
            .await
            .unwrap_or_default();
        let formatted_history = sm
            .load_history_formatted(Some(session_id), None)
            .await
            .unwrap_or_default();
        SessionContext {
            session_id: Some(session_id.clone()),
            history,
            formatted_history,
        }
    } else {
        SessionContext {
            session_id: request.session_id.clone(),
            ..SessionContext::default()
        }
    };
```

**Optimization note:** This loads history from the store twice. To avoid that, add a single method that returns both the entries and the messages/formatted string in one call. Alternatively, load entries once and derive both representations:

```rust
let session_context =
    if let (Some(session_id), Some(sm)) = (&request.session_id, &self.session_manager) {
        let (history_messages, formatted_history) = sm
            .load_history_both(Some(session_id), None)
            .await
            .unwrap_or_default();
        SessionContext {
            session_id: Some(session_id.clone()),
            history: history_messages,
            formatted_history,
        }
    } else {
        SessionContext {
            session_id: request.session_id.clone(),
            ..SessionContext::default()
        }
    };
```

Where `load_history_both` is a new `SessionManager` method:

```rust
/// Load history as both structured messages and a formatted string, with a single
/// store round-trip.
pub async fn load_history_both(
    &self,
    session_id: Option<&str>,
    user_id: Option<&str>,
) -> Result<(Vec<Message>, String), SessionError> {
    let resolved_id = self.resolve_session_id(session_id);
    let entries = self
        .store
        .get_latest_qa_entries(resolved_id, user_id, self.history_limit)
        .await?;

    debug!(
        session_id = resolved_id,
        entries = entries.len(),
        "Loaded session history (both formats)"
    );

    let messages = entries_to_messages(&entries);
    let formatted = Self::format_entries(&entries);
    Ok((messages, formatted))
}
```

### Change 6: Update tests in `session_messages.rs`

If there are any existing tests for `build_messages_with_history`, they need to be updated. Currently the file has no tests, but add new ones:

**File:** `crates/search/src/utils/session_messages.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cognee_llm::MessageRole;
    use cognee_session::SessionContext;

    #[test]
    fn empty_history_passes_system_prompt_unchanged() {
        let messages = build_messages_with_history(
            "system instructions".to_string(),
            "user question".to_string(),
            &SessionContext::default(),
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[0].content, "system instructions");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "user question");
    }

    #[test]
    fn history_prepended_to_system_prompt_with_task_separator() {
        let session = SessionContext {
            session_id: Some("s1".to_string()),
            history: vec![],
            formatted_history: "Previous conversation:\n\nQUESTION: hi\nANSWER: hello\n\n"
                .to_string(),
        };
        let messages = build_messages_with_history(
            "You are a helpful assistant.".to_string(),
            "What is Rust?".to_string(),
            &session,
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(messages[0]
            .content
            .starts_with("Previous conversation:"));
        assert!(messages[0]
            .content
            .contains("\nTASK:You are a helpful assistant."));
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "What is Rust?");
    }
}
```

### Change 7: Update retriever tests that construct `SessionContext`

All tests that construct `SessionContext::default()` will continue to work because `formatted_history` defaults to `String::new()` (empty string). Tests that construct `SessionContext` with explicit fields need the new `formatted_history` field:

```rust
SessionContext {
    session_id: Some("test".to_string()),
    history: vec![...],
    formatted_history: String::new(),  // <-- NEW field
}
```

Search for all explicit `SessionContext` construction in tests across:
- `crates/search/src/orchestration/search_orchestrator.rs`
- Any retriever test files

## Test Verification

### Existing tests

The `SessionManager` already has tests for `format_entries` and `entries_to_messages`. These remain valid.

### New tests

1. **`build_messages_with_history` tests** (described in Change 6 above)
2. **`load_history_both` integration test** in `crates/session/src/session_manager.rs`:

```rust
// This would require a mock SessionStore, which already exists in the session crate tests
#[tokio::test]
async fn load_history_both_returns_messages_and_formatted() {
    // ... set up store with entries ...
    let (messages, formatted) = manager.load_history_both(None, None).await.unwrap();
    assert_eq!(messages.len(), 4); // 2 entries * 2 messages each
    assert!(formatted.contains("Previous conversation:"));
    assert!(formatted.contains("QUESTION:"));
    assert!(formatted.contains("ANSWER:"));
}
```

### How to verify

```bash
cargo test -p cognee-session
cargo test -p cognee-search
scripts/check_all.sh
```

## Dependencies

- No new crate dependencies required.
- This changes the `SessionContext` struct (adding a field), which is a minor breaking change for code that constructs it with explicit field syntax. All in-tree usages must be updated.
- The `history: Vec<Message>` field is kept for backward compatibility with any code that might use the structured message list directly. It can be deprecated in a follow-up task once all consumers use the formatted history approach.
