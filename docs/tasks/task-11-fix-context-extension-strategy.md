# Task 11: Fix Context Extension Strategy

## Summary

The Rust `GraphCompletionContextExtensionRetriever` asks the LLM to generate a dedicated follow-up graph query each round and uses that query to search for new triplets. The Python implementation instead generates an LLM **completion** (answer) from the current context each round and then uses the **completion text itself** as the next search query. This is Python's "answer-driven expansion" pattern: the answer naturally contains entity names and relationship terms that serve as effective search queries for the next round. This task restructures the Rust extension loop to match Python's algorithm.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

### Constants (lines 30-32)

```rust
const DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT: &str =
    "Generate a follow-up graph query that expands useful context for the question.";
const DEFAULT_CONTEXT_EXTENSION_USER_PROMPT: &str = "Original question:\n{question}\n\nCurrent graph context:\n{context}\n\nProvide one short follow-up graph query.";
```

These prompts instruct the LLM to generate a follow-up **query** -- a dedicated retrieval question, not an answer to the user's question.

### Extension loop in `get_completion` (lines 283-347)

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
) -> Result<SearchOutput, SearchError> {
    let mut extended_context = match context {
        Some(existing_context) => existing_context,
        None => self.get_context(query).await?,
    };

    for _ in 0..self.context_extension_rounds {
        let current_context_text = render_edges_context(&extended_context);
        let extension_prompt = DEFAULT_CONTEXT_EXTENSION_USER_PROMPT
            .replace("{question}", query)
            .replace("{context}", &current_context_text);

        // ASK LLM FOR A FOLLOW-UP QUERY (wrong pattern)
        let follow_up_query = self
            .llm
            .generate(
                vec![
                    Message::system(DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT),
                    Message::user(extension_prompt),
                ],
                self.generation_options.clone(),
            )
            .await?
            .content
            .trim()
            .to_string();

        if follow_up_query.is_empty() {
            break;
        }

        let new_context = self.get_context(&follow_up_query).await?;
        let merged_context = merge_dedup_context(&extended_context, &new_context);

        if merged_context.len() == extended_context.len() {
            break;
        }

        extended_context = merged_context;
    }

    // Final completion
    let system_prompt = resolve_system_prompt(...)?;
    let user_prompt = render_user_prompt(...);
    let completion = self.llm.generate(...).await?;
    Ok(SearchOutput::Text(completion.content))
}
```

**Problem:** The loop asks the LLM to generate a follow-up *query*, not an *answer*. This requires two dedicated prompts (`DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT`, `DEFAULT_CONTEXT_EXTENSION_USER_PROMPT`) that do not exist in Python. The Python pattern uses the answer itself as the search query for the next round.

## Required Python Behavior

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_context_extension_retriever.py`

### Extension round logic (lines 98-125)

```python
async def _run_extension_round(self, states: dict):
    """Run one extension round: generate completions, fetch new triplets, check convergence."""
    active_queries = [q for q, s in states.items() if not s.done]
    active_contexts = [states[q].context_text for q in active_queries]
    prev_sizes = [len(states[q].triplets) for q in active_queries]

    # GENERATE ANSWER (completion) FROM CURRENT CONTEXT
    completions = await generate_completion_batch(
        query_batch=active_queries,
        context=active_contexts,
        user_prompt_path=self.user_prompt_path,
        system_prompt_path=self.system_prompt_path,
        system_prompt=self.system_prompt,
    )

    # USE THE COMPLETION TEXT AS THE NEW SEARCH QUERY
    new_triplets_batch = await self.get_triplets(query_batch=list(completions))
    for q, new_triplets in zip(active_queries, new_triplets_batch):
        states[q].merge_triplets(new_triplets)

    # Resolve updated context
    context_batch = await asyncio.gather(
        *[self.resolve_edges_to_text(states[q].triplets) for q in active_queries]
    )
    for q, context, prev_size in zip(active_queries, context_batch, prev_sizes):
        states[q].context_text = context
        states[q].check_convergence(prev_size)
```

**Key insight:** Python calls `generate_completion_batch` with the original `active_queries` and current `active_contexts` to produce a real **answer**. Then it passes `list(completions)` (the answer texts) as query strings to `get_triplets`. The answers contain entity names and relationships that naturally expand the search space.

### Overall flow in `get_retrieved_objects` (lines 52-94)

```python
async def get_retrieved_objects(self, query=None, query_batch=None):
    effective_batch = [query] if query else query_batch

    # Initial triplet fetch
    triplets_batch = await self.get_triplets(query_batch=effective_batch)
    context_batch = [resolve_edges_to_text(t) for t in triplets_batch]
    states = {q: QueryState(t, c) for q, t, c in zip(effective_batch, triplets_batch, context_batch)}

    # Extension rounds
    for _ in range(self.context_extension_rounds):
        if all(s.done for s in states.values()):
            break
        await self._run_extension_round(states)

    return self._collect_triplets(states, query, effective_batch)
```

**Important:** In Python, context extension happens in `get_retrieved_objects` (the retrieval phase), and the final completion is generated later in `get_completion_from_context` (via the base class). In Rust, both retrieval and completion are in `get_completion`. The Rust architecture should be preserved (keeping both in `get_completion`) but the loop body must change to the answer-driven pattern.

## Step-by-Step Changes

### Step 1: Remove the dedicated extension prompts

Delete the following constants (lines 30-32) from `advanced_graph_retrievers.rs`:

```rust
// DELETE these two constants:
const DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT: &str = ...;
const DEFAULT_CONTEXT_EXTENSION_USER_PROMPT: &str = ...;
```

These prompts have no Python equivalent and encode the wrong algorithm.

### Step 2: Restructure the extension loop

Replace the loop body in `get_completion` (lines 294-326) with the answer-driven expansion pattern:

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
) -> Result<SearchOutput, SearchError> {
    let mut extended_context = match context {
        Some(existing_context) => existing_context,
        None => self.get_context(query).await?,
    };

    let system_prompt = resolve_system_prompt(
        self.system_prompt.as_deref(),
        self.system_prompt_path.as_deref(),
    )?;

    for _ in 0..self.context_extension_rounds {
        let current_context_text = render_edges_context(&extended_context);

        // Generate an ANSWER to the user's question using current context
        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &current_context_text,
        );

        let completion = self
            .llm
            .generate(
                build_messages_with_history(system_prompt.clone(), user_prompt, session),
                self.generation_options.clone(),
            )
            .await?
            .content;

        let completion_trimmed = completion.trim();
        if completion_trimmed.is_empty() {
            break;
        }

        // Use the ANSWER TEXT as the search query for new triplets
        let new_context = self.get_context(completion_trimmed).await?;
        let prev_len = extended_context.len();
        extended_context = merge_dedup_context(&extended_context, &new_context);

        // Convergence check: stop if no new context was added
        if extended_context.len() == prev_len {
            break;
        }
    }

    // Final completion with the fully extended context
    let final_context_text = render_edges_context(&extended_context);
    let final_user_prompt = render_user_prompt(
        self.user_prompt_template.as_deref(),
        query,
        &final_context_text,
    );

    let final_completion = self
        .llm
        .generate(
            build_messages_with_history(system_prompt, final_user_prompt, session),
            self.generation_options.clone(),
        )
        .await?;

    Ok(SearchOutput::Text(final_completion.content))
}
```

### Step 3: Update the default round count

Python defaults to `context_extension_rounds = 4` (line 32 of context extension retriever). The current Rust default is `DEFAULT_CONTEXT_EXTENSION_ROUNDS = 2` (line 23). Update to match Python:

```rust
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 4;
```

### Step 4: Update existing tests

The test `graph_context_extension_returns_final_answer` (line 770) sets up `TestLlm` with two queued responses: `["Find Bob relations", "extended answer"]`. Under the old algorithm, the first response is a follow-up query and the second is the final answer.

Under the new algorithm with 1 extension round:
- Round 1: Generate an answer using context -> consumes response 1 ("Find Bob relations"), then use it as search query -> retrieves new context.
- Final completion: Generate final answer with extended context -> consumes response 2 ("extended answer").

The test should still pass with the same queued responses since the consumption order is the same (round answer, then final answer). However, verify the captured messages -- the first LLM call should now use the answer system prompt (not the deleted extension system prompt).

Update the test to verify that no message contains the old `DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT` text.

### Step 5: Add a new test for convergence

Add a test that verifies early termination when no new triplets are discovered:

```rust
#[tokio::test]
async fn context_extension_converges_when_no_new_context() {
    // Set up graph with two nodes, one edge
    // LLM returns answers that don't lead to new triplets
    // Verify the loop terminates before max rounds
    // Verify final answer is generated with the unchanged context
}
```

## Test Verification

1. **Existing test `graph_context_extension_returns_final_answer`** (line 770): Should still pass -- update assertions if the captured message format changes.

2. **New test: answer text used as search query**: Set up a `TestLlm` that returns specific entity names in its completion. Verify those entity names are passed to `get_context` (observable via the vector DB / graph DB mock receiving them).

3. **New test: convergence on no new triplets**: Verify the loop breaks early when the answer-driven search finds no new context items.

4. Run `cargo check --all-targets` and `scripts/check_all.sh`.

## Dependencies

- No new crate dependencies.
- Uses existing `resolve_system_prompt`, `render_user_prompt`, `build_messages_with_history`, `render_edges_context` utilities from `crate::utils`.
- The `merge_dedup_context` function (line 107) remains unchanged.
