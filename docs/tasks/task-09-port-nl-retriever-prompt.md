# Task 09: Port Natural Language Retriever Prompt from Python to Rust

## Summary

The Rust `NaturalLanguageRetriever` uses a minimal 1-line system prompt that lacks the full Cypher-generation instructions present in the Python reference. The Python prompt is a 67-line document containing hardcoded node schemas for all 5 core node types (EntityType, Entity, TextDocument, DocumentChunk, TextSummary), 17 numbered rules across three categories (Query Requirements, Performance Optimization, Error Prevention), a worked example, and two Jinja2 template placeholders (`{{edge_schemas}}` and `{{previous_attempts}}`). This task replaces the Rust constant with the full Python prompt, adapted for Rust `str::replace`-based templating.

## Current Rust Prompt

**File:** `crates/search/src/retrievers/cypher_nl_retrievers.rs`
**Constant name:** `NL_SYSTEM_PROMPT_TEMPLATE`
**Line:** 14

```rust
const NL_SYSTEM_PROMPT_TEMPLATE: &str = "You convert natural language requests into graph queries. Return ONLY a query string.\n\nGraph edge schema:\n{edge_schemas}\n\nPrevious attempts:\n{previous_attempts}";
```

This is a 3-sentence stub. It provides no node schema information, no Cypher-specific rules, no examples, and no error prevention guidance.

## Target Python Prompt

**File:** `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/llm/prompts/natural_language_retriever_system.txt`

Full text (67 lines):

```
You are an expert Neo4j Cypher query generator tasked with translating natural language questions into precise, optimized Cypher queries.

TASK:
Generate a valid, executable Cypher query that accurately answers the user's question based on the provided graph schema.

GRAPH SCHEMA INFORMATION:
- You will be given node labels and their properties in format: NodeLabels [list of properties]
- You will be given relationship types between nodes
- ONLY use node labels, properties, and relationship types that exist in the provided schema
- Respect relationship directions (source→target) exactly as specified in the schema
- Properties may have specific formats (e.g., dates, codes) - infer these from examples when possible

QUERY REQUIREMENTS:
1. Return ONLY the exact Cypher query with NO explanations, comments, or markdown
2. Generate syntactically correct Neo4j Cypher code (Neo4j 4.4+ compatible)
3. Be precise - match the exact property names and relationship types from the schema
4. Handle complex queries by breaking them into logical pattern matching parts
5. Use parameters (e.g., $name) for literal values when appropriate
6. Use appropriate data types for parameters (strings, numbers, booleans)

PERFORMANCE OPTIMIZATION:
1. Use indexes and constraints when available (assume they exist on ID properties)
2. Include LIMIT clauses for queries that could return large result sets
3. Use efficient patterns - avoid unnecessary pattern complexity
4. Consider using OPTIONAL MATCH for parts that might not exist
5. For aggregation, use efficient aggregation functions (count, sum, avg)
6. For pathfinding, consider using shortestPath() or apoc.algo.* procedures

ERROR PREVENTION:
1. Validate your query steps mentally before finalizing
2. Ensure relationship directions match schema
3. Check property names match exactly what's in the schema
4. Use pattern variables consistently throughout the query
5. If previous attempts failed, analyze the failures and adjust your approach

Node schemas:
- EntityType
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents the categories or classifications for entities in the database.

- Entity
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents individual entities that belong to a specific type or classification.

- TextDocument
Properties: raw_data_location, name, mime_type, external_metadata, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents documents containing text data, along with metadata about their storage and format.

- DocumentChunk
Properties: version, created_at, type, topological_rank, cut_type, text, metadata, chunk_index, chunk_size, updated_at, id
Purpose: Represents segmented portions of larger documents, useful for processing or analysis at a more granular level.

- TextSummary
Properties: topological_rank, metadata, id, type, updated_at, created_at, text, version
Purpose: Represents summarized content generated from larger text documents, retaining essential information and metadata.

Edge schema (relationship properties):
`{{edge_schemas}}`

This queries doesn't work. Do NOT use them:
`{{previous_attempts}}`

Example 1:
Get all nodes connected to John
MATCH (n:Entity {'name': 'John'})--(neighbor)
RETURN n, neighbor
```

## Step-by-Step Implementation

### Step 1: Replace the `NL_SYSTEM_PROMPT_TEMPLATE` constant

In `crates/search/src/retrievers/cypher_nl_retrievers.rs`, replace line 14 with the full Python prompt text.

**Template variable adaptation:** The Python prompt uses Jinja2 `{{ }}` double-brace syntax for its two placeholders. In Rust, we use `str::replace("{placeholder}", value)`, so these must become single-brace `{edge_schemas}` and `{previous_attempts}`. The existing Rust code at lines 143-145 already calls `.replace("{edge_schemas}", ...)` and `.replace("{previous_attempts}", ...)`, so the single-brace convention must be preserved.

Replace the current single-line constant:

```rust
const NL_SYSTEM_PROMPT_TEMPLATE: &str = "You convert natural language requests into graph queries. Return ONLY a query string.\n\nGraph edge schema:\n{edge_schemas}\n\nPrevious attempts:\n{previous_attempts}";
```

With a multi-line raw string containing the full Python prompt (adapted):

```rust
const NL_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are an expert Neo4j Cypher query generator tasked with translating natural language questions into precise, optimized Cypher queries.

TASK:
Generate a valid, executable Cypher query that accurately answers the user's question based on the provided graph schema.

GRAPH SCHEMA INFORMATION:
- You will be given node labels and their properties in format: NodeLabels [list of properties]
- You will be given relationship types between nodes
- ONLY use node labels, properties, and relationship types that exist in the provided schema
- Respect relationship directions (source→target) exactly as specified in the schema
- Properties may have specific formats (e.g., dates, codes) - infer these from examples when possible

QUERY REQUIREMENTS:
1. Return ONLY the exact Cypher query with NO explanations, comments, or markdown
2. Generate syntactically correct Neo4j Cypher code (Neo4j 4.4+ compatible)
3. Be precise - match the exact property names and relationship types from the schema
4. Handle complex queries by breaking them into logical pattern matching parts
5. Use parameters (e.g., $name) for literal values when appropriate
6. Use appropriate data types for parameters (strings, numbers, booleans)

PERFORMANCE OPTIMIZATION:
1. Use indexes and constraints when available (assume they exist on ID properties)
2. Include LIMIT clauses for queries that could return large result sets
3. Use efficient patterns - avoid unnecessary pattern complexity
4. Consider using OPTIONAL MATCH for parts that might not exist
5. For aggregation, use efficient aggregation functions (count, sum, avg)
6. For pathfinding, consider using shortestPath() or apoc.algo.* procedures

ERROR PREVENTION:
1. Validate your query steps mentally before finalizing
2. Ensure relationship directions match schema
3. Check property names match exactly what's in the schema
4. Use pattern variables consistently throughout the query
5. If previous attempts failed, analyze the failures and adjust your approach

Node schemas:
- EntityType
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents the categories or classifications for entities in the database.

- Entity
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents individual entities that belong to a specific type or classification.

- TextDocument
Properties: raw_data_location, name, mime_type, external_metadata, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents documents containing text data, along with metadata about their storage and format.

- DocumentChunk
Properties: version, created_at, type, topological_rank, cut_type, text, metadata, chunk_index, chunk_size, updated_at, id
Purpose: Represents segmented portions of larger documents, useful for processing or analysis at a more granular level.

- TextSummary
Properties: topological_rank, metadata, id, type, updated_at, created_at, text, version
Purpose: Represents summarized content generated from larger text documents, retaining essential information and metadata.

Edge schema (relationship properties):
{edge_schemas}

This queries doesn't work. Do NOT use them:
{previous_attempts}

Example 1:
Get all nodes connected to John
MATCH (n:Entity {{'name': 'John'}})--(neighbor)
RETURN n, neighbor";
```

**Key adaptation notes:**

1. **Jinja2 `{{edge_schemas}}` becomes `{edge_schemas}`** -- The Python file uses `{{` and `}}` because Jinja2 requires double braces. The Rust code uses `str::replace()` which matches literal single braces, so we use `{edge_schemas}` and `{previous_attempts}`.

2. **Cypher literal braces must be doubled** -- The example on the last two lines contains Cypher map syntax `{'name': 'John'}`. Since `str::replace` only replaces exact matches of `{edge_schemas}` and `{previous_attempts}`, simple braces like `{'name': 'John'}` would NOT be affected and could remain as-is. However, to be safe and consistent and to allow potential future migration to `format!()`, the Cypher map braces in the example should be doubled: `{{'name': 'John'}}`. This is a defensive measure -- with `str::replace()` it is technically unnecessary, but it ensures the prompt is forward-compatible.

3. **Unicode arrow `→` preserved** -- The Python prompt uses Unicode `→` (U+2192) at line 9, not ASCII `->`. The Rust replacement must use the same Unicode character `→` to match Python exactly.

4. **Backtick wrapping preserved** -- The Python prompt wraps placeholders in backticks (`` `{{edge_schemas}}` `` and `` `{{previous_attempts}}` ``). These backticks are **intentional literal characters** that appear in the rendered prompt (they survive Jinja2 rendering). The Rust replacement must also wrap the placeholder values in backticks: `` `{edge_schemas}` `` and `` `{previous_attempts}` `` to match the Python behavior exactly.

### Step 2: Update the default for `previous_attempts`

In the `execute_nl_query` method (line 163), the initial value of `previous_attempts` is an empty string. The Python template has a `{{previous_attempts}}` placeholder which renders to the default content when no attempts have been made. To match Python behavior and give the LLM useful context, update the initial value:

**Current (line 163):**
```rust
let mut previous_attempts = String::new();
```

**Change to:**
```rust
let mut previous_attempts = String::from("No attempts yet.");
```

This ensures the first LLM call sees `"No attempts yet."` in the prompt rather than a blank line, matching the Python behavior where the prompt section reads "This queries doesn't work. Do NOT use them: No attempts yet." on the first iteration.

### Step 3: Verify the `generate_cypher_query` replacement logic is unchanged

The existing code at lines 143-145 already performs the correct replacements:

```rust
let system_prompt = NL_SYSTEM_PROMPT_TEMPLATE
    .replace("{edge_schemas}", &edge_schema_text)
    .replace("{previous_attempts}", previous_attempts);
```

No changes needed here. The `str::replace` calls will substitute the single-brace placeholders in the new prompt exactly as before.

### Step 4: Update the existing test

The test `natural_language_retriever_retries_until_results` at line 457 does not inspect the system prompt content, so it should continue to pass without modification. However, verify this by running:

```bash
cargo test --package cognee-search -- cypher_nl_retrievers::tests
```

The test uses a `TestLlm` mock that ignores the message content and returns canned responses, so the prompt change will not affect test outcomes.

### Step 5: (Optional) Add a prompt content test

Consider adding a test that verifies the template contains the expected placeholders and key content:

```rust
#[test]
fn nl_system_prompt_contains_required_sections() {
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("{edge_schemas}"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("{previous_attempts}"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("QUERY REQUIREMENTS:"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("PERFORMANCE OPTIMIZATION:"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("ERROR PREVENTION:"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Node schemas:"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("EntityType"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Entity"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("TextDocument"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("DocumentChunk"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("TextSummary"));
    assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Example 1:"));
}
```

## Test Verification

Run the following commands to verify the change:

```bash
# 1. Check compilation
cargo check --all-targets

# 2. Run the search crate tests (includes cypher_nl tests)
cargo test --package cognee-search

# 3. Run the full check suite
scripts/check_all.sh
```

Expected outcomes:
- The existing `cypher_retriever_returns_query_rows` and `natural_language_retriever_retries_until_results` tests pass unchanged.
- No compilation errors from the multi-line string constant.
- Clippy and formatting checks pass.

## Dependencies

- **No new crate dependencies.** This is a prompt-text-only change.
- **No changes to public API.** The `NL_SYSTEM_PROMPT_TEMPLATE` constant is `const` (not `pub`), used only within the module.
- **No changes to the `Llm` trait or `GraphDBTrait`.** The `generate_cypher_query` method signature and replacement logic remain identical.
- **Blocks / blocked by:** None. This is a standalone prompt port with no cross-crate impact.
