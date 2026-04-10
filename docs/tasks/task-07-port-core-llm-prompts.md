# Task 7: Port All Core LLM Prompts to Match Python Wording

## Summary

The Rust search crate uses short, generic placeholder prompts as inline `const` strings. The Python SDK loads its prompts from dedicated `.txt` files under `cognee/infrastructure/llm/prompts/` using Jinja2 templating (`{{ variable }}`). This task replaces every Rust prompt constant with the exact Python wording, adapting Jinja2 `{{ var }}` syntax to Rust `{var}` simple-replace syntax.

**Scope:** 8 prompt constants across 2 Rust files.

---

## 1. System Prompt (`answer_simple_question`)

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/answer_simple_question.txt`

```
Answer the question using the provided context. Be as brief as possible.
```

No template variables.

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`
**Line:** 5
**Constant:** `DEFAULT_RAG_SYSTEM_PROMPT`

```rust
pub const DEFAULT_RAG_SYSTEM_PROMPT: &str = "You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.";
```

### Required change

```rust
pub const DEFAULT_RAG_SYSTEM_PROMPT: &str = "Answer the question using the provided context. Be as brief as possible.";
```

### Template variable differences

None. Both versions are plain text with no template variables.

---

## 2. RAG User Prompt (`context_for_question`)

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/context_for_question.txt`

```
The question is: `{{ question }}`
And here is the context: `{{ context }}`
```

Template variables: `{{ question }}`, `{{ context }}` (Jinja2).

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`
**Line:** 6
**Constant:** `DEFAULT_RAG_USER_PROMPT_TEMPLATE`

```rust
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str = "Question:\n{question}\n\nContext:\n{context}";
```

### Required change

```rust
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nAnd here is the context: `{context}`";
```

### Template variable differences

| Python (Jinja2) | Rust (simple replace) |
|---|---|
| `{{ question }}` | `{question}` |
| `{{ context }}` | `{context}` |

The existing `render_user_prompt()` function already uses `.replace("{question}", ...)` and `.replace("{context}", ...)`, so only the template text needs to change. The variable names remain identical.

### Note on graph vs RAG user prompts

Python uses two distinct user prompt templates:
- `context_for_question.txt` -- used by `CompletionRetriever` (RAG), `TripletRetriever`, `CypherSearchRetriever`
- `graph_context_for_question.txt` -- used by `GraphCompletionRetriever`, `GraphCompletionCotRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphSummaryCompletionRetriever`, `TemporalRetriever`

The graph variant text is:

```
The question is: `{{ question }}`
and here is the context provided with a set of relationships from a knowledge graph separated by \n---\n each represented as node1 -- relation -- node2 triplet: `{{ context }}`
```

In the current Rust code, all retrievers share the same `DEFAULT_RAG_USER_PROMPT_TEMPLATE` via `render_user_prompt()`. To match Python, a second constant should be added for graph-based retrievers. This is tracked separately in the implementation steps below.

**Additional constant to add** in `completion.rs`:

```rust
pub const DEFAULT_GRAPH_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nand here is the context provided with a set of relationships from a knowledge graph separated by \\n---\\n each represented as node1 -- relation -- node2 triplet: `{context}`";
```

The graph-based retrievers (`GraphCompletionRetriever`, `GraphSummaryCompletionRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphCompletionCotRetriever`, `TemporalRetriever`) should default to `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` instead of `DEFAULT_RAG_USER_PROMPT_TEMPLATE` when no custom user prompt template is provided.

---

## 3. CoT Validation System Prompt

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/cot_validation_system_prompt.txt`

```
You are a helpful agent who are allowed to use only the provided question answer and context.
I want to you find reasoning what is missing from the context or why the answer is not answering the question or not correct strictly based on the context.
```

No template variables.

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 34-35
**Constant:** `DEFAULT_COT_VALIDATION_SYSTEM_PROMPT`

```rust
const DEFAULT_COT_VALIDATION_SYSTEM_PROMPT: &str =
    "You validate whether an answer is sufficiently grounded in graph context.";
```

### Required change

```rust
const DEFAULT_COT_VALIDATION_SYSTEM_PROMPT: &str = "You are a helpful agent who are allowed to use only the provided question answer and context.\nI want to you find reasoning what is missing from the context or why the answer is not answering the question or not correct strictly based on the context.";
```

### Template variable differences

None.

---

## 4. CoT Validation User Prompt

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/cot_validation_user_prompt.txt`

```
<QUESTION>
`{{ query}}`
</QUESTION>

<ANSWER>
`{{ answer }}`
</ANSWER>

<CONTEXT>
`{{ context }}`
</CONTEXT>
```

Template variables: `{{ query }}`, `{{ answer }}`, `{{ context }}` (Jinja2).

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 36
**Constant:** `DEFAULT_COT_VALIDATION_USER_PROMPT`

```rust
const DEFAULT_COT_VALIDATION_USER_PROMPT: &str = "Question:\n{question}\n\nAnswer:\n{answer}\n\nContext:\n{context}\n\nSay whether more context is needed and why.";
```

### Required change

```rust
const DEFAULT_COT_VALIDATION_USER_PROMPT: &str = "<QUESTION>\n`{question}`\n</QUESTION>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<CONTEXT>\n`{context}`\n</CONTEXT>";
```

### Template variable differences

| Python (Jinja2) | Current Rust | Required Rust |
|---|---|---|
| `{{ query }}` | `{question}` | `{question}` |
| `{{ answer }}` | `{answer}` | `{answer}` |
| `{{ context }}` | `{context}` | `{context}` |

**Important:** Python uses `{{ query }}` but the Rust code uses `{question}` as the variable name. The calling code in `GraphCompletionCotRetriever::get_completion()` (line 443) does `.replace("{question}", query)`, so the Rust variable name `{question}` is correct and should be kept (it is semantically the same as Python's `query`). No code change needed in the calling site.

---

## 5. CoT Follow-up System Prompt

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/cot_followup_system_prompt.txt`

```
You are a helpful assistant whose job is to ask exactly one clarifying follow-up question,
to collect the missing piece of information needed to fully answer the user's original query.
Respond with the question only (no extra text, no punctuation beyond what's needed).
```

No template variables.

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 38-39
**Constant:** `DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT`

```rust
const DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT: &str =
    "Generate one concise follow-up graph query to improve the answer.";
```

### Required change

```rust
const DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT: &str = "You are a helpful assistant whose job is to ask exactly one clarifying follow-up question,\nto collect the missing piece of information needed to fully answer the user's original query.\nRespond with the question only (no extra text, no punctuation beyond what's needed).";
```

### Template variable differences

None.

---

## 6. CoT Follow-up User Prompt

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/cot_followup_user_prompt.txt`

```
Based on the following, ask exactly one question that would directly resolve the gap identified in the validation reasoning and allow a valid answer.
Think in a way that with the followup question you are exploring a knowledge graph which contains entities, entity types and document chunks

<QUERY>
`{{ query}}`
</QUERY>

<ANSWER>
`{{ answer }}`
</ANSWER>

<REASONING>
`{{ reasoning }}`
</REASONING>
```

Template variables: `{{ query }}`, `{{ answer }}`, `{{ reasoning }}` (Jinja2).

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 40
**Constant:** `DEFAULT_COT_FOLLOW_UP_USER_PROMPT`

```rust
const DEFAULT_COT_FOLLOW_UP_USER_PROMPT: &str = "Question:\n{question}\n\nAnswer:\n{answer}\n\nValidation:\n{validation}\n\nProvide one follow-up graph query.";
```

### Required change

```rust
const DEFAULT_COT_FOLLOW_UP_USER_PROMPT: &str = "Based on the following, ask exactly one question that would directly resolve the gap identified in the validation reasoning and allow a valid answer.\nThink in a way that with the followup question you are exploring a knowledge graph which contains entities, entity types and document chunks\n\n<QUERY>\n`{question}`\n</QUERY>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<REASONING>\n`{validation}`\n</REASONING>";
```

### Template variable differences

| Python (Jinja2) | Current Rust | Required Rust |
|---|---|---|
| `{{ query }}` | `{question}` | `{question}` |
| `{{ answer }}` | `{answer}` | `{answer}` |
| `{{ reasoning }}` | `{validation}` | `{validation}` |

**Important:** Python uses `{{ reasoning }}` as the variable name but the Rust calling code at line 461 uses `.replace("{validation}", &validation)`. The Rust placeholder name `{validation}` should remain as-is in the template since the calling code already substitutes it. The template text around it must change to match Python's `<REASONING>` XML tags and preamble.

---

## 7. Summarization Prompt (`summarize_search_results`)

### Python source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/summarize_search_results.txt`

```
You are a top-tier summarization engine that is meant to eliminate redundancies.
The input contains relationships enclosed by "--" .
Summarize the input into natural sentences, listing all relationships.
```

No template variables.

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 25-26
**Constant:** `DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT`

```rust
const DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT: &str =
    "You summarize graph evidence into concise factual context.";
```

### Required change

```rust
const DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT: &str = "You are a top-tier summarization engine that is meant to eliminate redundancies.\nThe input contains relationships enclosed by \"--\" .\nSummarize the input into natural sentences, listing all relationships.";
```

### Template variable differences

None.

---

## 8. Graph Summary User Prompt

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Line:** 27-28
**Constant:** `DEFAULT_GRAPH_SUMMARY_USER_PROMPT`

```rust
const DEFAULT_GRAPH_SUMMARY_USER_PROMPT: &str =
    "Summarize the following graph context:\n\n{context}";
```

### Python equivalent

In Python, `summarize_text()` in `completion.py` (line 162-180) passes the text directly as `text_input` to the LLM, with `summarize_search_results.txt` used only as the system prompt. There is no separate user prompt template -- the raw context text is the user message.

### Required change

Remove the separate user prompt template. The calling code should pass the graph context directly as the user message instead of wrapping it in a template:

```rust
const DEFAULT_GRAPH_SUMMARY_USER_PROMPT: &str = "{context}";
```

Or, more cleanly, refactor the caller to pass the context directly without template substitution. The minimal change is to set the template to just `{context}` so the rendered output equals the raw context.

---

## 9. Context Extension Prompts

### Current Rust text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`
**Lines:** 30-32
**Constants:** `DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT`, `DEFAULT_CONTEXT_EXTENSION_USER_PROMPT`

```rust
const DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT: &str =
    "Generate a follow-up graph query that expands useful context for the question.";
const DEFAULT_CONTEXT_EXTENSION_USER_PROMPT: &str = "Original question:\n{question}\n\nCurrent graph context:\n{context}\n\nProvide one short follow-up graph query.";
```

### Python equivalent

In Python, `GraphCompletionContextExtensionRetriever` does NOT have separate context-extension-specific prompts. It re-uses the standard `answer_simple_question.txt` system prompt and `graph_context_for_question.txt` user prompt to generate intermediate completions, then uses those completions as new search queries. The extension rounds call `generate_completion_batch()` with the same `system_prompt_path` and `user_prompt_path` as the final answer.

### Required change

Remove the separate context extension prompt constants. Refactor `GraphCompletionContextExtensionRetriever::get_completion()` to use the standard system prompt (`resolve_system_prompt`) and graph user prompt template for the intermediate completion step, matching Python's behavior. Then use the generated completion text as the follow-up search query (as Python does).

As a minimal prompt-text-only change, replace:

```rust
const DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT: &str =
    "Generate a follow-up graph query that expands useful context for the question.";
const DEFAULT_CONTEXT_EXTENSION_USER_PROMPT: &str = "Original question:\n{question}\n\nCurrent graph context:\n{context}\n\nProvide one short follow-up graph query.";
```

with removal of these constants, and in the extension loop, re-use the standard system/user prompts (the same ones used for the final answer). This matches Python where intermediate completions use the same prompts as the final answer.

---

## Implementation Steps

### Step 1: Update `completion.rs` (2 constants + 1 new constant)

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/utils/completion.rs`

1. Change `DEFAULT_RAG_SYSTEM_PROMPT` (line 5) to Python's `answer_simple_question.txt` text
2. Change `DEFAULT_RAG_USER_PROMPT_TEMPLATE` (line 6) to Python's `context_for_question.txt` text
3. Add new `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` constant with Python's `graph_context_for_question.txt` text
4. Export the new constant in `utils/mod.rs`

### Step 2: Update `advanced_graph_retrievers.rs` (6 constants)

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/advanced_graph_retrievers.rs`

1. Change `DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT` (line 25-26) to Python's `summarize_search_results.txt` text
2. Change `DEFAULT_GRAPH_SUMMARY_USER_PROMPT` (line 27-28) to just `"{context}"`
3. Change `DEFAULT_COT_VALIDATION_SYSTEM_PROMPT` (line 34-35) to Python's `cot_validation_system_prompt.txt` text
4. Change `DEFAULT_COT_VALIDATION_USER_PROMPT` (line 36) to Python's `cot_validation_user_prompt.txt` text
5. Change `DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT` (line 38-39) to Python's `cot_followup_system_prompt.txt` text
6. Change `DEFAULT_COT_FOLLOW_UP_USER_PROMPT` (line 40) to Python's `cot_followup_user_prompt.txt` text
7. Remove `DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT` and `DEFAULT_CONTEXT_EXTENSION_USER_PROMPT`, refactor extension loop to use standard prompts

### Step 3: Update graph-based retrievers to use graph user prompt template

Update the following retrievers to default to `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` instead of `DEFAULT_RAG_USER_PROMPT_TEMPLATE`:
- `GraphCompletionRetriever` in `graph_completion_retriever.rs`
- `GraphSummaryCompletionRetriever` in `advanced_graph_retrievers.rs`
- `GraphCompletionContextExtensionRetriever` in `advanced_graph_retrievers.rs`
- `GraphCompletionCotRetriever` in `advanced_graph_retrievers.rs`
- `TemporalRetriever` in `temporal_retriever.rs`

---

## Test Verification

### Unit tests to update

1. **`completion_retriever.rs` test** (line 360): The test asserts `messages[0].content == DEFAULT_RAG_SYSTEM_PROMPT`. This will need to match the new text.

2. **`advanced_graph_retrievers.rs` tests**: The `TestLlm` in these tests captures messages. Existing tests verify behavior (number of LLM calls, output text) but do not assert exact prompt text, so they should pass without changes.

### New tests to add

1. **Prompt text snapshot tests** -- Add a test module in `completion.rs` or a dedicated `prompt_tests.rs` that asserts each constant matches the expected Python text verbatim:
   ```rust
   #[test]
   fn system_prompt_matches_python() {
       assert_eq!(
           DEFAULT_RAG_SYSTEM_PROMPT,
           "Answer the question using the provided context. Be as brief as possible."
       );
   }
   
   #[test]
   fn rag_user_prompt_matches_python() {
       let rendered = render_user_prompt(None, "What is X?", "X is Y.");
       assert_eq!(rendered, "The question is: `What is X?`\nAnd here is the context: `X is Y.`");
   }
   
   #[test]
   fn graph_user_prompt_matches_python() {
       let rendered = render_graph_user_prompt(None, "What is X?", "Alice -- knows -- Bob");
       assert!(rendered.contains("relationships from a knowledge graph"));
       assert!(rendered.contains("node1 -- relation -- node2 triplet"));
   }
   ```

2. **CoT prompt rendering tests** -- Verify that variable substitution in the XML-tagged CoT templates produces the expected format:
   ```rust
   #[test]
   fn cot_validation_user_prompt_renders_correctly() {
       let rendered = DEFAULT_COT_VALIDATION_USER_PROMPT
           .replace("{question}", "Who is Alice?")
           .replace("{answer}", "Alice is a person.")
           .replace("{context}", "Alice -- is_a -- Person");
       assert!(rendered.contains("<QUESTION>"));
       assert!(rendered.contains("`Who is Alice?`"));
       assert!(rendered.contains("<ANSWER>"));
       assert!(rendered.contains("<CONTEXT>"));
   }
   ```

3. **Integration test** -- Run the existing E2E search matrix test (`tests/search_matrix.rs` or equivalent) to verify that the full pipeline (add, cognify, search) still produces valid results with the updated prompts. The test does not check exact LLM output text but verifies that the pipeline completes without errors.

### Verification command

```bash
cargo test --all-targets
scripts/check_all.sh
```
