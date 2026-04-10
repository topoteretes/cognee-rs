# Task 13: Fix prompt resolution priority -- Check inline `system_prompt` before `system_prompt_path`

## Summary

The Rust `resolve_system_prompt()` function checks `system_prompt_path` (file-based prompt) **before** `system_prompt` (inline string). The Python `generate_completion()` does the opposite: it checks the inline `system_prompt` parameter first and only falls back to `read_query_prompt(system_prompt_path)` when the inline value is `None`/falsy. This means that in Rust, passing both an inline prompt and a file path will silently ignore the inline prompt, whereas Python would use the inline prompt.

## Current Rust Behavior

**File:** `crates/search/src/utils/completion.rs`, lines 8-24

```rust
pub fn resolve_system_prompt(
    system_prompt: Option<&str>,
    system_prompt_path: Option<&str>,
) -> Result<String, SearchError> {
    if let Some(path) = system_prompt_path {
        let prompt = fs::read_to_string(path).map_err(|error| {
            SearchError::InvalidInput(format!("failed to read system prompt path: {error}"))
        })?;
        return Ok(prompt);
    }

    if let Some(inline_prompt) = system_prompt {
        return Ok(inline_prompt.to_string());
    }

    Ok(DEFAULT_RAG_SYSTEM_PROMPT.to_string())
}
```

**Priority order (wrong):**
1. `system_prompt_path` (file) -- checked first
2. `system_prompt` (inline) -- checked second
3. `DEFAULT_RAG_SYSTEM_PROMPT` -- fallback

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/completion.py`, line 21

```python
system_prompt = system_prompt if system_prompt else read_query_prompt(system_prompt_path)
```

**Priority order (correct):**
1. `system_prompt` (inline) -- checked first; if truthy, used as-is
2. `system_prompt_path` (file) -- only read from disk if inline is `None`/falsy
3. (implicit) -- if both are `None`, `read_query_prompt` would receive `None` and handle its own fallback

The same pattern is used in `summarize_text()` (line 168):
```python
system_prompt = system_prompt if system_prompt else read_query_prompt(system_prompt_path)
```

## Callers Affected

Every retriever that calls `resolve_system_prompt` passes both `system_prompt` and `system_prompt_path`. All are affected:

| File | Line |
|------|------|
| `crates/search/src/retrievers/completion_retriever.rs` | 108 |
| `crates/search/src/retrievers/triplet_retriever.rs` | 131 |
| `crates/search/src/retrievers/graph_completion_retriever.rs` | 125 |
| `crates/search/src/retrievers/temporal_retriever.rs` | 393 |
| `crates/search/src/retrievers/advanced_graph_retrievers.rs` | 204, 328, 416 |

No caller changes are needed -- only the internal priority of `resolve_system_prompt` must be swapped.

## Step-by-Step Code Changes

### Change 1: Swap the priority in `resolve_system_prompt`

**File:** `crates/search/src/utils/completion.rs`

Replace lines 8-24:

```rust
pub fn resolve_system_prompt(
    system_prompt: Option<&str>,
    system_prompt_path: Option<&str>,
) -> Result<String, SearchError> {
    if let Some(path) = system_prompt_path {
        let prompt = fs::read_to_string(path).map_err(|error| {
            SearchError::InvalidInput(format!("failed to read system prompt path: {error}"))
        })?;
        return Ok(prompt);
    }

    if let Some(inline_prompt) = system_prompt {
        return Ok(inline_prompt.to_string());
    }

    Ok(DEFAULT_RAG_SYSTEM_PROMPT.to_string())
}
```

With:

```rust
pub fn resolve_system_prompt(
    system_prompt: Option<&str>,
    system_prompt_path: Option<&str>,
) -> Result<String, SearchError> {
    if let Some(inline_prompt) = system_prompt {
        return Ok(inline_prompt.to_string());
    }

    if let Some(path) = system_prompt_path {
        let prompt = fs::read_to_string(path).map_err(|error| {
            SearchError::InvalidInput(format!("failed to read system prompt path: {error}"))
        })?;
        return Ok(prompt);
    }

    Ok(DEFAULT_RAG_SYSTEM_PROMPT.to_string())
}
```

The only change is swapping the two `if let` blocks so that `system_prompt` (inline) is checked first.

## Test Verification

### Existing tests

There are no dedicated unit tests for `resolve_system_prompt` in `completion.rs`. The function is exercised indirectly by retriever integration tests, but those don't cover the priority conflict case (they typically pass only one of the two parameters).

### New tests to add

Add a `#[cfg(test)] mod tests` block at the bottom of `crates/search/src/utils/completion.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn inline_prompt_takes_priority_over_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "prompt from file").unwrap();
        let path = tmp.path().to_str().unwrap();

        let result =
            resolve_system_prompt(Some("inline prompt"), Some(path)).unwrap();
        assert_eq!(result, "inline prompt");
    }

    #[test]
    fn falls_back_to_file_when_inline_is_none() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "prompt from file").unwrap();
        let path = tmp.path().to_str().unwrap();

        let result = resolve_system_prompt(None, Some(path)).unwrap();
        assert_eq!(result, "prompt from file");
    }

    #[test]
    fn falls_back_to_default_when_both_are_none() {
        let result = resolve_system_prompt(None, None).unwrap();
        assert_eq!(result, DEFAULT_RAG_SYSTEM_PROMPT);
    }

    #[test]
    fn file_not_found_returns_error() {
        let result = resolve_system_prompt(None, Some("/nonexistent/path.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn render_user_prompt_uses_default_template() {
        let rendered = render_user_prompt(None, "what is X?", "X is Y");
        assert!(rendered.contains("what is X?"));
        assert!(rendered.contains("X is Y"));
    }

    #[test]
    fn render_user_prompt_uses_custom_template() {
        let rendered =
            render_user_prompt(Some("Q: {question} | C: {context}"), "q", "c");
        assert_eq!(rendered, "Q: q | C: c");
    }
}
```

Note: `tempfile` is already a dev-dependency of the search crate. If not, add `tempfile = "3"` under `[dev-dependencies]` in `crates/search/Cargo.toml`.

### How to verify

```bash
cargo test -p cognee-search -- completion::tests
```

All six tests should pass, and `inline_prompt_takes_priority_over_file` specifically validates the fix.
