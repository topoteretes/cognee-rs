# Task 8: Port FEELING_LUCKY Prompt -- Full 125-Line Search Type Selector

## Summary

The Rust `FeelingLuckyRetriever` uses a single-sentence generic prompt to select a search type. The Python SDK uses a detailed 125-line prompt (`search_type_selector_prompt.txt`) that describes every available `SearchType` with best-for guidance and concrete examples. This task replaces the Rust prompt constant with the full Python prompt text.

---

## Python Source

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/search_type_selector_prompt.txt`

Full text (125 lines):

```
You are an expert query analyzer for a **GraphRAG system**. Your primary goal is to analyze a user's query and select the single most appropriate `SearchType` tool to answer it.

Here are the available `SearchType` tools and their specific functions:

- **`SUMMARIES`**: The `SUMMARIES` search type retrieves summarized information from the knowledge graph.

  **Best for:**

  - Getting concise overviews of topics
  - Summarizing large amounts of information
  - Quick understanding of complex subjects

  **Best for:**

  - Discovering how entities are connected
  - Understanding relationships between concepts
  - Exploring the structure of your knowledge graph

* **`CHUNKS`**: The `CHUNKS` search type retrieves specific facts and information chunks from the knowledge graph.

  **Best for:**

  - Finding specific facts
  - Getting direct answers to questions
  - Retrieving precise information

* **`RAG_COMPLETION`**: Use for direct factual questions that can likely be answered by retrieving a specific text passage from a document. It does not use the graph's relationship structure.

  **Best for:**

  - Getting detailed explanations or comprehensive answers
  - Combining multiple pieces of information
  - Getting a single, coherent answer that is generated from relevant text passages

* **`GRAPH_COMPLETION`**: The `GRAPH_COMPLETION` search type leverages the graph structure to provide more contextually aware completions.

  **Best for:**

  - Complex queries requiring graph traversal
  - Questions that benefit from understanding relationships
  - Queries where context from connected entities matters

* **`GRAPH_SUMMARY_COMPLETION`**: The `GRAPH_SUMMARY_COMPLETION` search type combines graph traversal with summarization to provide concise but comprehensive answers.

  **Best for:**

  - Getting summarized information that requires understanding relationships
  - Complex topics that need concise explanations
  - Queries that benefit from both graph structure and summarization

* **`GRAPH_COMPLETION_COT`**: The `GRAPH_COMPLETION_COT` search type combines graph traversal with chain of thought to provide answers to complex multi hop questions.

  **Best for:**

  - Multi-hop questions that require following several linked concepts or entities
  - Tracing relational paths in a knowledge graph while also getting clear step-by-step reasoning
  - Summarizing completx linkages into a concise, human-readable answer once all hops have been explored

* **`GRAPH_COMPLETION_CONTEXT_EXTENSION`**: The `GRAPH_COMPLETION_CONTEXT_EXTENSION` search type combines graph traversal with multi-round context extension.

  **Best for:**

  - Iterative, multi-hop queries where intermediate facts aren't all present upfront
  - Complex linkages that benefit from multi-round "search → extend context → reason" loops to uncover deep connections.
  - Sparse or evolving graphs that require on-the-fly expansion—issuing follow-up searches to discover missing nodes or properties

* **`CODE`**: The `CODE` search type is specialized for retrieving and understanding code-related information from the knowledge graph.

  **Best for:**

  - Code-related queries
  - Programming examples and patterns
  - Technical documentation searches

* **`CYPHER`**: The `CYPHER` search type allows user to execute raw Cypher queries directly against your graph database.

  **Best for:**

  - Executing precise graph queries with full control
  - Leveraging Cypher features and functions
  - Getting raw data directly from the graph database

* **`NATURAL_LANGUAGE`**: The `NATURAL_LANGUAGE` search type translates a natural language question into a precise Cypher query that is executed directly against the graph database.

  **Best for:**

  - Getting precise, structured answers from the graph using natural language.
  - Performing advanced graph operations like filtering and aggregating data using natural language.
  - Asking precise, database-style questions without needing to write Cypher.

**Examples:**

Query: "Summarize the key findings from these research papers"
Response: `SUMMARIES`

Query: "When was Einstein born?"
Response: `CHUNKS`

Query: "Explain Einstein's contributions to physics"
Response: `RAG_COMPLETION`

Query: "Provide a comprehensive analysis of how these papers contribute to the field"
Response: `GRAPH_COMPLETION`

Query: "Explain the overall architecture of this codebase"
Response: `GRAPH_SUMMARY_COMPLETION`

Query: "Who was the father of the person who invented the lightbulb"
Response: `GRAPH_COMPLETION_COT`

Query: "What county was XY born in"
Response: `GRAPH_COMPLETION_CONTEXT_EXTENSION`

Query: "How to implement authentication in this codebase"
Response: `CODE`

Query: "MATCH (n) RETURN labels(n) as types, n.name as name LIMIT 10"
Response: `CYPHER`

Query: "Get all nodes connected to John"
Response: `NATURAL_LANGUAGE`



Your response MUST be a single word, consisting of only the chosen `SearchType` name. Do not provide any explanation.
```

No template variables. The prompt is static text.

---

## Current Rust Text

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs`
**Line:** 18
**Constant:** `DEFAULT_FEELING_LUCKY_PROMPT`

```rust
const DEFAULT_FEELING_LUCKY_PROMPT: &str = "You are a search method selector. Return ONLY one valid search type name in SCREAMING_SNAKE_CASE.";
```

---

## Required Change

Replace the constant with the full Python prompt. Because the text is 125 lines, use a raw string literal or a regular string with `\n` escapes.

```rust
const DEFAULT_FEELING_LUCKY_PROMPT: &str = "\
You are an expert query analyzer for a **GraphRAG system**. Your primary goal is to analyze a user's query and select the single most appropriate `SearchType` tool to answer it.

Here are the available `SearchType` tools and their specific functions:

- **`SUMMARIES`**: The `SUMMARIES` search type retrieves summarized information from the knowledge graph.

  **Best for:**

  - Getting concise overviews of topics
  - Summarizing large amounts of information
  - Quick understanding of complex subjects

  **Best for:**

  - Discovering how entities are connected
  - Understanding relationships between concepts
  - Exploring the structure of your knowledge graph

* **`CHUNKS`**: The `CHUNKS` search type retrieves specific facts and information chunks from the knowledge graph.

  **Best for:**

  - Finding specific facts
  - Getting direct answers to questions
  - Retrieving precise information

* **`RAG_COMPLETION`**: Use for direct factual questions that can likely be answered by retrieving a specific text passage from a document. It does not use the graph's relationship structure.

  **Best for:**

  - Getting detailed explanations or comprehensive answers
  - Combining multiple pieces of information
  - Getting a single, coherent answer that is generated from relevant text passages

* **`GRAPH_COMPLETION`**: The `GRAPH_COMPLETION` search type leverages the graph structure to provide more contextually aware completions.

  **Best for:**

  - Complex queries requiring graph traversal
  - Questions that benefit from understanding relationships
  - Queries where context from connected entities matters

* **`GRAPH_SUMMARY_COMPLETION`**: The `GRAPH_SUMMARY_COMPLETION` search type combines graph traversal with summarization to provide concise but comprehensive answers.

  **Best for:**

  - Getting summarized information that requires understanding relationships
  - Complex topics that need concise explanations
  - Queries that benefit from both graph structure and summarization

* **`GRAPH_COMPLETION_COT`**: The `GRAPH_COMPLETION_COT` search type combines graph traversal with chain of thought to provide answers to complex multi hop questions.

  **Best for:**

  - Multi-hop questions that require following several linked concepts or entities
  - Tracing relational paths in a knowledge graph while also getting clear step-by-step reasoning
  - Summarizing completx linkages into a concise, human-readable answer once all hops have been explored

* **`GRAPH_COMPLETION_CONTEXT_EXTENSION`**: The `GRAPH_COMPLETION_CONTEXT_EXTENSION` search type combines graph traversal with multi-round context extension.

  **Best for:**

  - Iterative, multi-hop queries where intermediate facts aren't all present upfront
  - Complex linkages that benefit from multi-round \"search → extend context → reason\" loops to uncover deep connections.
  - Sparse or evolving graphs that require on-the-fly expansion—issuing follow-up searches to discover missing nodes or properties

* **`CODE`**: The `CODE` search type is specialized for retrieving and understanding code-related information from the knowledge graph.

  **Best for:**

  - Code-related queries
  - Programming examples and patterns
  - Technical documentation searches

* **`CYPHER`**: The `CYPHER` search type allows user to execute raw Cypher queries directly against your graph database.

  **Best for:**

  - Executing precise graph queries with full control
  - Leveraging Cypher features and functions
  - Getting raw data directly from the graph database

* **`NATURAL_LANGUAGE`**: The `NATURAL_LANGUAGE` search type translates a natural language question into a precise Cypher query that is executed directly against the graph database.

  **Best for:**

  - Getting precise, structured answers from the graph using natural language.
  - Performing advanced graph operations like filtering and aggregating data using natural language.
  - Asking precise, database-style questions without needing to write Cypher.

**Examples:**

Query: \"Summarize the key findings from these research papers\"
Response: `SUMMARIES`

Query: \"When was Einstein born?\"
Response: `CHUNKS`

Query: \"Explain Einstein's contributions to physics\"
Response: `RAG_COMPLETION`

Query: \"Provide a comprehensive analysis of how these papers contribute to the field\"
Response: `GRAPH_COMPLETION`

Query: \"Explain the overall architecture of this codebase\"
Response: `GRAPH_SUMMARY_COMPLETION`

Query: \"Who was the father of the person who invented the lightbulb\"
Response: `GRAPH_COMPLETION_COT`

Query: \"What county was XY born in\"
Response: `GRAPH_COMPLETION_CONTEXT_EXTENSION`

Query: \"How to implement authentication in this codebase\"
Response: `CODE`

Query: \"MATCH (n) RETURN labels(n) as types, n.name as name LIMIT 10\"
Response: `CYPHER`

Query: \"Get all nodes connected to John\"
Response: `NATURAL_LANGUAGE`



Your response MUST be a single word, consisting of only the chosen `SearchType` name. Do not provide any explanation.";
```

**Note on escaped characters:** Inside a regular Rust string literal, the double quotes in `"search → extend context → reason"` and in the example queries must be escaped as `\"`. The em-dash and arrow characters are UTF-8 and need no escaping.

---

## Calling Code Changes

**File:** `/home/dmytro/dev/cognee/cognee-rust/crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs`
**Method:** `FeelingLuckyRetriever::select_retriever()` (line 85-119)

### Current behavior

```rust
let allowed_types = self
    .retrievers
    .keys()
    .copied()
    .filter(|search_type| *search_type != SearchType::FeelingLucky)
    .map(|search_type| format!("{:?}", search_type).to_ascii_uppercase())
    .collect::<Vec<_>>()
    .join(", ");

let selector_prompt = format!(
    "{DEFAULT_FEELING_LUCKY_PROMPT}\nAllowed types: {allowed_types}\nReturn only one value."
);
```

The current code dynamically appends the list of allowed types. The Python version does NOT do this -- it uses the static prompt as-is and relies on the LLM to choose from the types described in the prompt.

### Required change

Remove the dynamic `allowed_types` suffix. Use the full prompt directly as the system prompt:

```rust
let selector_prompt = DEFAULT_FEELING_LUCKY_PROMPT.to_string();
```

This matches Python's `select_search_type()` in `/home/dmytro/dev/cognee/cognee/cognee/modules/search/operations/select_search_type.py` (lines 24-27) which loads the prompt from file and uses it directly without appending allowed types.

### Python behavior reference

From `select_search_type.py`:
```python
system_prompt = read_query_prompt(system_prompt_path)  # reads search_type_selector_prompt.txt as-is
response = await LLMGateway.acreate_structured_output(
    text_input=query,
    system_prompt=system_prompt,
    response_model=str,
)
```

The query text is passed as the user message, the full prompt is the system message. The Rust code already follows this pattern (lines 101-108).

---

## Template Variable Differences

None. The Python prompt has no template variables (`{{ }}`). It is a static system prompt.

---

## SearchType Mapping Note

The Python prompt includes `CODE` as a search type. The Rust `SearchType` enum uses `CodingRules` instead. If `CODE` is not a variant in the Rust enum, the `parse_search_type()` method will fail to match it, and the fallback will activate. Two options:

1. Replace `CODE` with `CODING_RULES` in the prompt text to match the Rust enum variant name
2. Add a mapping in `parse_search_type()` so that `"CODE"` maps to `SearchType::CodingRules`

Option 2 is recommended since it preserves the Python prompt exactly. Add to `parse_search_type()`:

```rust
fn parse_search_type(raw: &str) -> Option<SearchType> {
    let normalized = raw
        .trim()
        .trim_matches('"')
        .replace([' ', '-'], "_")
        .to_ascii_uppercase();

    // Handle Python prompt's "CODE" -> Rust's "CodingRules"
    let mapped = match normalized.as_str() {
        "CODE" => "CODING_RULES".to_string(),
        other => other.to_string(),
    };

    serde_json::from_value::<SearchType>(Value::String(mapped)).ok()
}
```

---

## Test Verification

### Existing tests to verify

1. **`feeling_lucky_falls_back_on_invalid_selection`** (line 553-587): This test sends `"NOT_A_REAL_TYPE"` as the LLM response and expects the fallback. It should pass unchanged since the prompt change does not affect fallback behavior.

### New tests to add

1. **Prompt text assertion** -- Verify the constant contains key Python prompt fragments:
   ```rust
   #[test]
   fn feeling_lucky_prompt_contains_python_search_types() {
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("expert query analyzer"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("GraphRAG system"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("SUMMARIES"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("CHUNKS"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("RAG_COMPLETION"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("GRAPH_COMPLETION"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("GRAPH_COMPLETION_COT"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("NATURAL_LANGUAGE"));
       assert!(DEFAULT_FEELING_LUCKY_PROMPT.contains("Your response MUST be a single word"));
   }
   ```

2. **CODE mapping test** -- Verify the `parse_search_type` handles `"CODE"`:
   ```rust
   #[test]
   fn parse_search_type_maps_code_to_coding_rules() {
       assert_eq!(
           FeelingLuckyRetriever::parse_search_type("CODE"),
           Some(SearchType::CodingRules)
       );
   }
   ```

3. **Valid type selection test** -- A test where the mock LLM returns `"GRAPH_COMPLETION"` and verifies the correct retriever is selected:
   ```rust
   #[tokio::test]
   async fn feeling_lucky_selects_graph_completion_when_llm_says_so() {
       let llm = Arc::new(TestLlm {
           plain_responses: Mutex::new(VecDeque::from(["GRAPH_COMPLETION".to_string()])),
           feedback_response: None,
       });
       // ... build retrievers map with GraphCompletion ...
       let retriever = FeelingLuckyRetriever::new(llm, retrievers, None, None);
       // verify it delegates to GraphCompletion retriever
   }
   ```

### Verification command

```bash
cargo test --all-targets
scripts/check_all.sh
```
