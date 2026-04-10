# Task 5: Add Graph-Specific User Prompt Template

## Summary

Python uses two distinct user prompt templates for LLM completions:
1. **`context_for_question.txt`** -- generic RAG prompt for `CompletionRetriever`, `TripletRetriever`, `CypherSearchRetriever`
2. **`graph_context_for_question.txt`** -- graph-specific prompt for `GraphCompletionRetriever`, `GraphSummaryCompletionRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphCompletionCotRetriever`, `TemporalRetriever`

The Rust codebase currently uses a single `DEFAULT_RAG_USER_PROMPT_TEMPLATE` for all retrievers. This task adds a separate `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` and wires it into the graph-based retrievers as their default, while keeping the generic RAG template for non-graph retrievers.

## Current Rust Behavior

### File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`

```rust
pub const DEFAULT_RAG_SYSTEM_PROMPT: &str = "You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.";
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str = "Question:\n{question}\n\nContext:\n{context}";

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

pub fn render_user_prompt(template: Option<&str>, question: &str, context: &str) -> String {
    template
        .unwrap_or(DEFAULT_RAG_USER_PROMPT_TEMPLATE)
        .replace("{question}", question)
        .replace("{context}", context)
}
```

All retrievers call `render_user_prompt` with `self.user_prompt_template.as_deref()` which defaults to `None`, causing all of them to use `DEFAULT_RAG_USER_PROMPT_TEMPLATE`.

### File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/mod.rs`

```rust
pub use completion::{
    DEFAULT_RAG_SYSTEM_PROMPT, DEFAULT_RAG_USER_PROMPT_TEMPLATE, render_user_prompt,
    resolve_system_prompt,
};
```

### Retrievers that should use the graph prompt (currently using generic RAG prompt)

| Retriever | File | Line calling `render_user_prompt` |
|---|---|---|
| `GraphCompletionRetriever` | `graph_completion_retriever.rs` | 130-134 |
| `GraphSummaryCompletionRetriever` | `advanced_graph_retrievers.rs` | 209-213 |
| `GraphCompletionContextExtensionRetriever` | `advanced_graph_retrievers.rs` | 332-336 |
| `GraphCompletionCotRetriever` | `advanced_graph_retrievers.rs` | 424-428 |
| `TemporalRetriever` | `temporal_retriever.rs` | 398-402 |

### Retrievers that should keep the generic RAG prompt

| Retriever | File |
|---|---|
| `CompletionRetriever` | `completion_retriever.rs` |
| `TripletRetriever` | `triplet_retriever.rs` |

## Required Python Behavior

### Python prompt templates

#### `graph_context_for_question.txt` (graph-specific)
**File: `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/graph_context_for_question.txt`**

```
The question is: `{{ question }}`
and here is the context provided with a set of relationships from a knowledge graph separated by \n---\n each represented as node1 -- relation -- node2 triplet: `{{ context }}`
```

This is a Jinja2 template. Python renders it via `render_prompt(user_prompt_path, {"question": query, "context": context})` which uses `jinja2.Environment.get_template().render()`.

After Jinja2 rendering with `question="Who knows Bob?"` and `context="Alice --[KNOWS]--> Bob"`, the output is:
```
The question is: `Who knows Bob?`
and here is the context provided with a set of relationships from a knowledge graph separated by \n---\n each represented as node1 -- relation -- node2 triplet: `Alice --[KNOWS]--> Bob`
```

**Note:** The `\n---\n` in the template is a literal string (not actual newlines) because Jinja2 does not interpret `\n` as a newline inside template text -- it renders verbatim.

#### `context_for_question.txt` (generic RAG)
**File: `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/context_for_question.txt`**

```
The question is: `{{ question }}`
And here is the context: `{{ context }}`
```

### Python prompt mapping by retriever

| Python Retriever | `user_prompt_path` default |
|---|---|
| `GraphCompletionRetriever` | `graph_context_for_question.txt` |
| `GraphSummaryCompletionRetriever` | `graph_context_for_question.txt` |
| `GraphCompletionContextExtensionRetriever` | `graph_context_for_question.txt` |
| `GraphCompletionCotRetriever` | `graph_context_for_question.txt` |
| `TemporalRetriever` | `graph_context_for_question.txt` |
| `CompletionRetriever` | `context_for_question.txt` |
| `TripletRetriever` | `context_for_question.txt` |
| `CypherSearchRetriever` | `context_for_question.txt` |

### Python system prompt

**File: `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/answer_simple_question.txt`**

```
Answer the question using the provided context. Be as brief as possible.
```

This is used by all retrievers as the default system prompt (`system_prompt_path: str = "answer_simple_question.txt"`).

The current Rust default system prompt is:
```
You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.
```

This is close but not identical. Updating the system prompt text is out of scope for this task but should be tracked separately.

## Step-by-Step Changes

### Step 1: Add graph-specific user prompt constant

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`**

Add after the existing `DEFAULT_RAG_USER_PROMPT_TEMPLATE` constant (line 6):

```rust
pub const DEFAULT_GRAPH_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nand here is the context provided with a set of relationships from a knowledge graph separated by \\n---\\n each represented as node1 -- relation -- node2 triplet: `{context}`";
```

**Note on template syntax:** The Python template uses Jinja2 `{{ question }}` / `{{ context }}` syntax. The Rust code uses `{question}` / `{context}` with `str::replace()`. The literal `\n---\n` in the Python template renders as the verbatim string `\n---\n` (not actual newlines), so the Rust constant must use `\\n---\\n` to produce the same literal backslash-n characters in the output.

### Step 2: Add a `render_graph_user_prompt` function

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`**

Add after the existing `render_user_prompt` function:

```rust
/// Renders the user prompt for graph-based retrievers.
///
/// If a custom template is provided, it is used. Otherwise, the
/// `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` is used (matching Python's
/// `graph_context_for_question.txt`).
pub fn render_graph_user_prompt(template: Option<&str>, question: &str, context: &str) -> String {
    template
        .unwrap_or(DEFAULT_GRAPH_USER_PROMPT_TEMPLATE)
        .replace("{question}", question)
        .replace("{context}", context)
}
```

### Step 3: Export the new constant and function

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/mod.rs`**

Change:
```rust
pub use completion::{
    DEFAULT_RAG_SYSTEM_PROMPT, DEFAULT_RAG_USER_PROMPT_TEMPLATE, render_user_prompt,
    resolve_system_prompt,
};
```

To:
```rust
pub use completion::{
    DEFAULT_GRAPH_USER_PROMPT_TEMPLATE, DEFAULT_RAG_SYSTEM_PROMPT,
    DEFAULT_RAG_USER_PROMPT_TEMPLATE, render_graph_user_prompt, render_user_prompt,
    resolve_system_prompt,
};
```

### Step 4: Wire graph retrievers to use `render_graph_user_prompt`

#### 4a: `GraphCompletionRetriever`

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/graph_completion_retriever.rs`**

**Line 17 -- update import:**

Change:
```rust
use crate::utils::{
    build_messages_with_history, render_edges_context, render_user_prompt, resolve_system_prompt,
};
```

To:
```rust
use crate::utils::{
    build_messages_with_history, render_edges_context, render_graph_user_prompt,
    resolve_system_prompt,
};
```

**Lines 130-134 -- change `render_user_prompt` to `render_graph_user_prompt`:**

Change:
```rust
        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &graph_context_text,
        );
```

To:
```rust
        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &graph_context_text,
        );
```

#### 4b: `GraphSummaryCompletionRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphCompletionCotRetriever`

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`**

**Line 17 -- update import:**

Change:
```rust
use crate::utils::{
    build_messages_with_history, render_edges_context, render_user_prompt, resolve_system_prompt,
};
```

To:
```rust
use crate::utils::{
    build_messages_with_history, render_edges_context, render_graph_user_prompt,
    resolve_system_prompt,
};
```

**Line 209-213 (`GraphSummaryCompletionRetriever::get_completion`):**

Change:
```rust
        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &summarized_context,
        );
```

To:
```rust
        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &summarized_context,
        );
```

**Line 332-336 (`GraphCompletionContextExtensionRetriever::get_completion`):**

Change:
```rust
        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &render_edges_context(&extended_context),
        );
```

To:
```rust
        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &render_edges_context(&extended_context),
        );
```

**Line 424-428 (`GraphCompletionCotRetriever::get_completion`):**

Change:
```rust
            let answer_prompt = render_user_prompt(
                self.user_prompt_template.as_deref(),
                query,
                &render_edges_context(&current_context),
            );
```

To:
```rust
            let answer_prompt = render_graph_user_prompt(
                self.user_prompt_template.as_deref(),
                query,
                &render_edges_context(&current_context),
            );
```

#### 4c: `TemporalRetriever`

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/temporal_retriever.rs`**

**Line 19 -- update import:**

Change:
```rust
use crate::utils::{build_messages_with_history, render_user_prompt, resolve_system_prompt};
```

To:
```rust
use crate::utils::{build_messages_with_history, render_graph_user_prompt, resolve_system_prompt};
```

**Lines 398-402 (`TemporalRetriever::get_completion`):**

Change:
```rust
        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &Self::temporal_context_to_text(&completion_context),
        );
```

To:
```rust
        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &Self::temporal_context_to_text(&completion_context),
        );
```

### Step 5: Keep non-graph retrievers unchanged

The following retrievers already correctly use `render_user_prompt` (which defaults to `DEFAULT_RAG_USER_PROMPT_TEMPLATE`), matching Python's `context_for_question.txt`:

- `CompletionRetriever` (`completion_retriever.rs` line 114)
- `TripletRetriever` (`triplet_retriever.rs` line 137)

No changes needed for these files.

### Step 6: Update existing tests

#### 6a: `GraphCompletionRetriever` test

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/graph_completion_retriever.rs`**

The test `renders_graph_context_for_completion` (line 531) uses a custom `user_prompt_template`:

```rust
Some("Question={question}\nGraph={context}".to_string()),
```

This test overrides the default template, so it will continue to work as-is. However, a new test should be added to verify that the **default** graph prompt template is used when `user_prompt_template` is `None`.

#### 6b: `advanced_graph_retrievers.rs` tests

The existing tests in this file use `None` for `user_prompt_template` (line 751, 782, 825), meaning they will now use `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` instead of `DEFAULT_RAG_USER_PROMPT_TEMPLATE`. The tests don't currently assert on the prompt format, only on the final response text, so they should pass without modification.

## Test Verification

### New unit tests to add

**File: `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_graph_user_prompt_default_template() {
        let result = render_graph_user_prompt(
            None,
            "Who knows Bob?",
            "Alice --[KNOWS]--> Bob",
        );

        assert!(result.contains("The question is: `Who knows Bob?`"));
        assert!(result.contains("knowledge graph"));
        assert!(result.contains("Alice --[KNOWS]--> Bob"));
        // Verify literal \n---\n is present (not actual newlines)
        assert!(result.contains("\\n---\\n"));
    }

    #[test]
    fn render_graph_user_prompt_custom_template() {
        let result = render_graph_user_prompt(
            Some("Q={question} C={context}"),
            "test question",
            "test context",
        );

        assert_eq!(result, "Q=test question C=test context");
    }

    #[test]
    fn render_user_prompt_uses_rag_template_by_default() {
        let result = render_user_prompt(None, "question", "context");

        assert!(result.contains("Question:\nquestion"));
        assert!(result.contains("Context:\ncontext"));
        // Should NOT contain graph-specific text
        assert!(!result.contains("knowledge graph"));
    }

    #[test]
    fn graph_and_rag_templates_are_different() {
        let graph = render_graph_user_prompt(None, "q", "c");
        let rag = render_user_prompt(None, "q", "c");

        assert_ne!(graph, rag);
    }
}
```

### Integration test for GraphCompletionRetriever default prompt

Add a test in `graph_completion_retriever.rs` that verifies the default prompt format when `user_prompt_template` is `None`:

```rust
#[tokio::test]
async fn uses_graph_prompt_template_by_default() {
    let llm = Arc::new(TestLlm {
        response_text: "answer".to_string(),
        ..Default::default()
    });

    let retriever = GraphCompletionRetriever::new(
        /* vector_db, embedding_engine, graph_db as in existing tests */
        Arc::clone(&llm) as Arc<dyn Llm>,
        Some(2),
        Some(5),
        Some(0.0),
        None,  // system_prompt
        None,  // system_prompt_path
        None,  // user_prompt_template -- should use graph default
        None,  // generation_options
    );

    let context = vec![crate::types::SearchItem {
        id: None,
        score: Some(1.0),
        payload: json!({
            "source_name": "Alice",
            "target_name": "Bob",
            "relationship": "KNOWS"
        }),
    }];

    let _ = retriever
        .get_completion("Who knows Bob?", Some(context), &SessionContext::default())
        .await
        .unwrap();

    let messages = llm.last_messages.lock().unwrap().clone();
    // User message should use graph_context_for_question format
    assert!(messages[1].content.contains("The question is: `Who knows Bob?`"));
    assert!(messages[1].content.contains("knowledge graph"));
    // Should NOT use the generic RAG format
    assert!(!messages[1].content.starts_with("Question:\n"));
}
```

## Dependencies on Other Tasks

- **Task 4 (context rendering):** Task 4 changes the format of the `context` string that gets inserted into the `{context}` placeholder. Both tasks are independent (can be implemented in either order) but together they produce Python-compatible LLM inputs for graph-based search.
- **No upstream dependencies:** This task only modifies prompt constants and function calls within the search crate. No changes to data structures, graph retrieval, or other crates are needed.
