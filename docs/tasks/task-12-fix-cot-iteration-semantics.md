# Task 12: Fix CoT Iteration Semantics

## Summary

The Rust `GraphCompletionCotRetriever` treats `max_iter` as the total number of answer-generation iterations, with the reasoning/follow-up steps squeezed between iterations. The Python implementation treats `max_iter` as the number of **reasoning rounds** that happen **after** an initial completion. This means Python always generates an initial completion before entering the reasoning loop, and the loop body is: validate -> follow-up question -> fetch new triplets -> regenerate completion. The Rust loop omits the initial completion and conflates iteration indices. This task restructures the Rust loop to match Python's "initial completion + N reasoning rounds" pattern.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

### Loop in `get_completion` (lines 405-488)

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
) -> Result<SearchOutput, SearchError> {
    let mut current_context = match context {
        Some(existing_context) => existing_context,
        None => self.get_context(query).await?,
    };

    let system_prompt = resolve_system_prompt(
        self.system_prompt.as_deref(),
        self.system_prompt_path.as_deref(),
    )?;

    let mut final_answer = String::new();

    for iter_index in 0..self.max_iter {
        // Step A: Generate answer
        let answer_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &render_edges_context(&current_context),
        );

        final_answer = self
            .llm
            .generate(
                build_messages_with_history(system_prompt.clone(), answer_prompt, session),
                self.generation_options.clone(),
            )
            .await?
            .content;

        // Step B: If last iteration, skip reasoning
        if iter_index + 1 >= self.max_iter {
            break;
        }

        // Step C: Validate answer
        let validation_prompt = DEFAULT_COT_VALIDATION_USER_PROMPT
            .replace("{question}", query)
            .replace("{answer}", &final_answer)
            .replace("{context}", &render_edges_context(&current_context));

        let validation = self.llm.generate(
            vec![
                Message::system(DEFAULT_COT_VALIDATION_SYSTEM_PROMPT),
                Message::user(validation_prompt),
            ],
            self.generation_options.clone(),
        ).await?.content;

        // Step D: Generate follow-up query
        let follow_up_prompt = DEFAULT_COT_FOLLOW_UP_USER_PROMPT
            .replace("{question}", query)
            .replace("{answer}", &final_answer)
            .replace("{validation}", &validation);

        let follow_up_query = self.llm.generate(
            vec![
                Message::system(DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT),
                Message::user(follow_up_prompt),
            ],
            self.generation_options.clone(),
        ).await?.content.trim().to_string();

        if follow_up_query.is_empty() {
            break;
        }

        // Step E: Fetch new context
        let additional_context = self.get_context(&follow_up_query).await?;
        current_context = merge_dedup_context(&current_context, &additional_context);
    }

    Ok(SearchOutput::Text(final_answer))
}
```

### Iteration semantics problem

With `max_iter = 4`:
- Rust loop: `iter_index` goes 0, 1, 2, 3. Each iteration generates an answer first, then does reasoning (except the last). This yields **4 answer generations** and **3 reasoning rounds**.
- Python loop (`_run_cot_completion`, lines 162-172): Generates an initial completion **before** the loop, then the loop runs `max_iter` times (4 rounds). Each round: validate -> follow-up -> fetch triplets -> regenerate completion. This yields **1 initial + 4 regenerations = 5 answer generations** and **4 reasoning rounds**.

The key difference: Python separates the initial completion from the reasoning loop. The loop runs for exactly `max_iter` reasoning rounds, and each round both validates AND regenerates a completion.

### LLM call count comparison (max_iter=4)

| Operation | Python | Rust |
|---|---|---|
| Initial completion | 1 | 0 |
| Reasoning rounds | 4 | 3 |
| LLM calls per round | 3 (validate + follow-up + completion) | 3 (answer + validate + follow-up) |
| Final answer | From last round's completion | From iter_index=3 answer |
| Total answer generations | 5 | 4 |
| Total validate calls | 4 | 3 |
| Total follow-up calls | 4 | 3 |

## Required Python Behavior

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_cot_retriever.py`

### `_run_cot_completion` orchestrator (lines 140-172)

```python
async def _run_cot_completion(
    self,
    query_batch: List[str],
    conversation_history: str = "",
    skip_final_completion: bool = False,
) -> tuple[List[Any], List[str], List[List[Edge]]]:
    states = {q: QueryState() for q in query_batch}

    # 1. Fetch initial triplets and resolve context
    await self._fetch_initial_triplets_and_context(states)

    # 2. Generate INITIAL completion (before any reasoning)
    await self._generate_completions(states, conversation_history)

    # 3. Run max_iter REASONING rounds
    for reasoning_iteration in range(self.max_iter):
        followup_queries = await self._run_cot_round(states)
        await self._merge_followup_triplets(states, followup_queries)
        if not (skip_final_completion and reasoning_iteration == self.max_iter - 1):
            await self._generate_completions(states, conversation_history)

    return self._collect_results(states, query_batch, skip_final_completion)
```

### Per-round logic `_run_cot_round` (lines 204-213)

```python
async def _run_cot_round(self, states: dict) -> List[str]:
    """Run one CoT round: validate answers, generate follow-up questions."""
    validation_prompts, validation_system = self._build_validation_prompts(states)
    reasoning_batch = await batch_llm_completion(validation_prompts, validation_system)

    followup_prompts, followup_system = self._build_followup_prompts(states, reasoning_batch)
    followup_questions = await batch_llm_completion(followup_prompts, followup_system)

    logger.info(f"Chain-of-thought: follow-up questions: {followup_questions}")
    return followup_questions
```

### Key structural observation

Python's per-round sequence:
1. **Validate** the current answer against the context (uses `states[q].completion`)
2. **Generate follow-up question** based on the validation reasoning
3. *(back in orchestrator)* **Fetch new triplets** using the follow-up question
4. *(back in orchestrator)* **Regenerate completion** with the enriched context

The completion (answer) from step 4 becomes the input to step 1 of the next round.

### `skip_final_completion` optimization (line 169)

```python
if not (skip_final_completion and reasoning_iteration == self.max_iter - 1):
    await self._generate_completions(states, conversation_history)
```

When called from `get_retrieved_objects` (the retrieval path), `skip_final_completion=True`. This skips the last regeneration because the final completion will be generated later in `get_completion_from_context`. In Rust's architecture, `get_completion` handles both retrieval and final completion, so this optimization is not needed -- always regenerate.

## Step-by-Step Changes

### Step 1: Restructure the loop to "initial completion + N reasoning rounds"

Replace the loop body in `get_completion` (lines 405-488) with:

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
) -> Result<SearchOutput, SearchError> {
    let mut current_context = match context {
        Some(existing_context) => existing_context,
        None => self.get_context(query).await?,
    };

    let system_prompt = resolve_system_prompt(
        self.system_prompt.as_deref(),
        self.system_prompt_path.as_deref(),
    )?;

    // Step 1: Generate INITIAL completion (before any reasoning rounds)
    let context_text = render_edges_context(&current_context);
    let answer_prompt = render_user_prompt(
        self.user_prompt_template.as_deref(),
        query,
        &context_text,
    );

    let mut current_answer = self
        .llm
        .generate(
            build_messages_with_history(system_prompt.clone(), answer_prompt, session),
            self.generation_options.clone(),
        )
        .await?
        .content;

    // Step 2: Run max_iter REASONING rounds
    for _ in 0..self.max_iter {
        // 2a. Validate the current answer against the context
        let validation_prompt = DEFAULT_COT_VALIDATION_USER_PROMPT
            .replace("{question}", query)
            .replace("{answer}", &current_answer)
            .replace("{context}", &render_edges_context(&current_context));

        let validation = self
            .llm
            .generate(
                vec![
                    Message::system(DEFAULT_COT_VALIDATION_SYSTEM_PROMPT),
                    Message::user(validation_prompt),
                ],
                self.generation_options.clone(),
            )
            .await?
            .content;

        // 2b. Generate follow-up question based on validation reasoning
        let follow_up_prompt = DEFAULT_COT_FOLLOW_UP_USER_PROMPT
            .replace("{question}", query)
            .replace("{answer}", &current_answer)
            .replace("{validation}", &validation);

        let follow_up_query = self
            .llm
            .generate(
                vec![
                    Message::system(DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT),
                    Message::user(follow_up_prompt),
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

        // 2c. Fetch new context using the follow-up question
        let additional_context = self.get_context(&follow_up_query).await?;
        current_context = merge_dedup_context(&current_context, &additional_context);

        // 2d. Regenerate completion with the enriched context
        let enriched_context_text = render_edges_context(&current_context);
        let regeneration_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &enriched_context_text,
        );

        current_answer = self
            .llm
            .generate(
                build_messages_with_history(
                    system_prompt.clone(),
                    regeneration_prompt,
                    session,
                ),
                self.generation_options.clone(),
            )
            .await?
            .content;
    }

    Ok(SearchOutput::Text(current_answer))
}
```

### Step 2: Update the default max_iter

Python defaults to `max_iter = 4` (line 68 of `graph_completion_cot_retriever.py`). The current Rust default is `DEFAULT_COT_MAX_ITER = 2` (line 23). Update to match Python:

```rust
const DEFAULT_COT_MAX_ITER: usize = 4;
```

### Step 3: Update existing test

The test `graph_cot_returns_answer_from_last_iteration` (line 804) uses `max_iter = 2` and queues 4 LLM responses:

```rust
let llm = Arc::new(TestLlm::new(vec![
    "first answer",       // old: iter 0 answer
    "needs more evidence", // old: iter 0 validation
    "find graph neighbors", // old: iter 0 follow-up
    "second answer",      // old: iter 1 answer (last iter, no validation)
]));
```

Under the new algorithm with `max_iter = 2`:
1. **Initial completion**: consumes "first answer"
2. **Round 1**: validate (consumes "needs more evidence"), follow-up (consumes "find graph neighbors"), fetch context, regenerate answer (consumes "second answer")
3. **Round 2**: validate (needs response 5), follow-up (needs response 6), ...

The test needs to be updated because the new algorithm makes **more LLM calls** with the same `max_iter`. With `max_iter = 2`, the new algorithm needs:
- 1 initial completion
- 2 rounds x 3 LLM calls each (validate + follow-up + regenerate) = 6
- Total: 7 LLM calls

Update the test to either:
- **Option A**: Reduce `max_iter` to 1 and queue 4 responses (initial answer + validate + follow-up + regenerated answer).
- **Option B**: Keep `max_iter = 2` and queue 7 responses.

**Recommended: Option A** -- simplest change, still tests the core flow:

```rust
let llm = Arc::new(TestLlm::new(vec![
    "first answer",          // initial completion
    "needs more evidence",   // round 1: validation
    "find graph neighbors",  // round 1: follow-up question
    "second answer",         // round 1: regenerated completion
]));

let retriever = GraphCompletionCotRetriever::new(
    // ...
    Some(1),  // max_iter = 1 (one reasoning round after initial completion)
    // ...
);
```

Expected final output: `"second answer"` (the regenerated completion from round 1).

### Step 4: Add a test for multi-round reasoning

Add a test with `max_iter = 2` to verify two full reasoning rounds:

```rust
#[tokio::test]
async fn cot_runs_two_reasoning_rounds_after_initial_completion() {
    let llm = Arc::new(TestLlm::new(vec![
        "initial answer",         // initial completion
        "validation round 1",     // round 1: validation
        "follow-up round 1",      // round 1: follow-up
        "answer after round 1",   // round 1: regenerated completion
        "validation round 2",     // round 2: validation
        "follow-up round 2",      // round 2: follow-up
        "final answer",           // round 2: regenerated completion
    ]));

    let retriever = GraphCompletionCotRetriever::new(
        build_vector_db(),
        Arc::new(TestEmbeddingEngine),
        build_graph_db().await,
        Arc::clone(&llm) as Arc<dyn Llm>,
        Some(5),
        Some(5),
        Some(0.0),
        Some(2),   // 2 reasoning rounds
        None, None, None, None,
    );

    let output = retriever
        .get_completion("Who knows Bob?", None, &SessionContext::default())
        .await
        .unwrap();

    match output {
        SearchOutput::Text(text) => assert_eq!(text, "final answer"),
        _ => panic!("expected text output"),
    }

    // Verify total LLM calls: 1 initial + 2 rounds * 3 calls each = 7
    assert_eq!(llm.captured_messages.lock().unwrap().len(), 7);
}
```

### Step 5: Add a test for early termination on empty follow-up

```rust
#[tokio::test]
async fn cot_stops_early_on_empty_follow_up() {
    let llm = Arc::new(TestLlm::new(vec![
        "initial answer",      // initial completion
        "all looks good",      // round 1: validation
        "",                    // round 1: follow-up (empty -> break)
    ]));

    let retriever = GraphCompletionCotRetriever::new(
        build_vector_db(),
        Arc::new(TestEmbeddingEngine),
        build_graph_db().await,
        Arc::clone(&llm) as Arc<dyn Llm>,
        Some(5), Some(5), Some(0.0),
        Some(3),  // max_iter = 3, but should stop after 1
        None, None, None, None,
    );

    let output = retriever
        .get_completion("Who knows Bob?", None, &SessionContext::default())
        .await
        .unwrap();

    match output {
        SearchOutput::Text(text) => assert_eq!(text, "initial answer"),
        _ => panic!("expected text output"),
    }

    // Only 3 LLM calls: initial completion, validation, follow-up
    assert_eq!(llm.captured_messages.lock().unwrap().len(), 3);
}
```

## LLM Call Comparison After Fix

With `max_iter = 4`:

| Operation | Python | Rust (after fix) |
|---|---|---|
| Initial completion | 1 | 1 |
| Reasoning rounds | 4 | 4 |
| Calls per round | 3 (validate + follow-up + completion) | 3 (validate + follow-up + completion) |
| Total LLM calls | 13 | 13 |

## Test Verification

1. **Updated test `graph_cot_returns_answer_from_last_iteration`**: Reduce `max_iter` to 1, keep 4 queued responses, verify "second answer" is the final output.

2. **New test `cot_runs_two_reasoning_rounds_after_initial_completion`**: 7 queued responses, `max_iter = 2`, verify "final answer" output and 7 captured LLM calls.

3. **New test `cot_stops_early_on_empty_follow_up`**: 3 queued responses, empty follow-up causes break, verify "initial answer" is returned.

4. Run `cargo check --all-targets` and `scripts/check_all.sh`.

## Dependencies

- No new crate dependencies.
- No changes to struct fields or constructor signatures (only `DEFAULT_COT_MAX_ITER` value change and loop restructuring).
- The `merge_dedup_context` function remains unchanged.
- The prompt constants (`DEFAULT_COT_VALIDATION_*`, `DEFAULT_COT_FOLLOW_UP_*`) remain unchanged -- only their placement in the loop changes.
