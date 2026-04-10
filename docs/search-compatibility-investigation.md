# Search Operation: Python-Rust Compatibility Investigation

> **Date:** 2026-04-09
> **Goal:** Make the Rust `search` operation fully compatible with the Python SDK so both can be swapped interchangeably — same tasks, same DB structures, same data, same order.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Architecture Overview](#2-architecture-overview)
3. [Search API Signature Gaps](#3-search-api-signature-gaps)
4. [Retriever Pipeline Pattern](#4-retriever-pipeline-pattern)
5. [Brute Force Triplet Search](#5-brute-force-triplet-search)
6. [Context Rendering (resolve_edges_to_text)](#6-context-rendering-resolve_edges_to_text)
7. [LLM Prompts](#7-llm-prompts)
8. [Session Management](#8-session-management)
9. [Per-Retriever Gap Analysis](#9-per-retriever-gap-analysis)
10. [Default Value Mismatches](#10-default-value-mismatches)
11. [Vector DB Interface Differences](#11-vector-db-interface-differences)
12. [Graph DB Interface Differences](#12-graph-db-interface-differences)
13. [Dataset Authorization & Multi-Tenancy](#13-dataset-authorization--multi-tenancy)
14. [Query Logging & Observability](#14-query-logging--observability)
15. [Result Formatting & Return Types](#15-result-formatting--return-types)
16. [Prioritized Change List](#16-prioritized-change-list)

---

## 1. Executive Summary

The Rust search implementation covers all 15 `SearchType` variants and has comprehensive test coverage.

### Implementation Progress (updated 2026-04-10)

All **P0 (Critical)**, **P1 (High)**, **P2 (Medium)**, and **P3 (Low)** tasks have been completed (tasks 01–37, 27 commits). Overall compatibility improved from **~50-55%** to **~95%+**.

| Area | Original Severity | Status | Summary |
|------|----------|--------|---------|
| **Brute force triplet search** | **Critical** | **FIXED** (task-01) | 3-component distance scoring, penalty=3.5, `min` merge, ascending sort, edge distance via `EdgeType_relationship_name` |
| **Vector collections searched** | **Critical** | **FIXED** (task-02) | Added `EntityType_name`, `EdgeType_relationship_name`; removed `Entity_description`, `Triplet_embeddable_text`; added EntityType indexing in cognify |
| **Context rendering** | **High** | **FIXED** (task-04) | Two-section Nodes/Connections format with title generation, stop words, `__node_content_start/end__` markers, `--[REL]-->` syntax |
| **LLM prompts** | **High** | **FIXED** (tasks 05, 07-10) | All prompts match Python wording: system, RAG user, graph user, CoT validation/follow-up, summarization, FEELING_LUCKY (125 lines), NL retriever (67 lines), temporal (14 lines) |
| **Default values** | **High** | **FIXED** (task-03) | All `top_k`=10, `wide_search_top_k`=100, `triplet_distance_penalty`=3.5, `max_iter`=4, `context_extension_rounds`=4, separators=`\n` |
| **Iteration semantics** | **High** | **FIXED** (tasks 11, 12) | CoT: initial completion + N rounds; Context extension: answer-driven expansion |
| **Bugs fixed** | **High** | **FIXED** (tasks 13-15) | Prompt resolution priority inverted; lexical ranking broken with `with_scores=false`; NL retry loop broken by LLM errors |
| **Missing features** | **Medium** | **FIXED** (tasks 16-27) | Feedback influence, node filtering (AND/OR), verbose mode, structured LLM output, per-request SearchParams, session history alignment, user_id, retriever_specific_config, Unicode lowercasing, unconditional query logging |
| **Per-request parameter limitation** | **High** | **FIXED** (task-26) | `SearchParams` struct threaded through all `SearchRetriever` trait methods; per-request `top_k`, `system_prompt`, `wide_search_top_k`, etc. override constructor defaults |
| **Infrastructure** | **Low** | **FIXED** (tasks 28-37) | Batch query support, batch vector search, ID-filtered graph data, lexical chunk caching, auto-feedback detection, OpenTelemetry spans, dynamic collection discovery, access timestamps, community retriever plugins |

---

## 2. Architecture Overview

### Python Pipeline
```
search() API
  → resolve user + authorize datasets
  → for each dataset (parallel):
      → set_database_global_context_variables(dataset)
      → get_retriever_output():
          1. retriever.get_retrieved_objects(query)
          2. update_node_access_timestamps(objects)
          3. retriever.get_context_from_objects(query, objects)
          4. [skip if only_context] retriever.get_completion_from_context(query, objects, context)
          → SearchResultPayload(result_object, context, completion)
  → _backwards_compatible_search_results(verbose)
```

### Rust Pipeline
```
SearchOrchestrator.search()
  → registry.get(search_type) → retriever
  → [optional] database.log_query()
  → retriever.get_context(query)
  → scope_context_by_datasets(context, dataset_ids)  [post-hoc filtering]
  → [skip if only_context]
  → load session history
  → retriever.get_completion(query, context, session)
  → save session Q&A
  → prepare_search_result()  [build graphs from context]
```

### Key Structural Differences

| Aspect | Python | Rust |
|--------|--------|------|
| Retriever steps | 3 (`get_retrieved_objects` → `get_context_from_objects` → `get_completion_from_context`) | 2 (`get_context` → `get_completion`) |
| Dataset scoping | Pre-retrieval per-dataset DB context switching, parallel retriever calls | Post-retrieval payload filtering |
| Retriever lifecycle | New instance per call (factory pattern) | Shared `Arc<dyn SearchRetriever>` singleton; per-request `SearchParams` overrides (task-26) |
| Session injection | History string prepended to system prompt (`history + "\nTASK:" + prompt`) | **ALIGNED** (task-21): Prepends `{history}\nTASK:{prompt}` to system message |
| Authorization | Built-in with `get_authorized_existing_datasets()` | Delegated to caller (`user_id` field available via task-22) |
| Structured output | Pydantic `response_model` per retriever | **ALIGNED** (task-25): `SearchOutput::Structured(Value)` via `response_schema` on `SearchParams` |
| Batch queries | Supported in graph-based retrievers (`query_batch` on all abstract methods) | **ALIGNED** (task-28): `get_context_batch`, `get_completion_batch`, `search_batch` with default sequential impl |
| Per-request params | Baked into fresh retriever instance per call | **ALIGNED** (task-26): `SearchParams` passed to all retriever methods; `top_k`, `system_prompt`, etc. override per request |
| Prompt resolution priority | Inline `system_prompt` takes priority over `system_prompt_path` | **ALIGNED** (task-13): Inline takes priority over file path |
| Context conditional | `get_retrieved_objects` and `get_context_from_objects` always called | `get_context` only called when `include_context` flag is true |

---

## 3. Search API Signature Gaps

### Parameters in Python but missing in Rust

| Parameter | Python Type | Default | Used By | Priority |
|-----------|------------|---------|---------|----------|
| `user` | `Optional[User]` | resolved internally | Authorization | Medium |
| `feedback_influence` | `float` | `0.0` | Graph retrievers (scoring) | **High** |
| `verbose` | `bool` | `False` | Result formatting | Medium |
| `node_name_filter_operator` | `str` | `"OR"` | Graph retrieval filtering | Medium |
| `retriever_specific_config` | `Optional[dict]` | `None` | Per-retriever overrides (`response_model`, `max_iter`, etc.) | Medium |

### Parameters in Rust but not in Python

| Parameter | Rust Type | Notes |
|-----------|----------|-------|
| `use_combined_context` | `Option<bool>` | Rust-specific context merging strategy |
| `save_interaction` | `Option<bool>` | Opt-in query logging (Python always logs) |

### Type Differences

| Parameter | Python | Rust | Action Needed |
|-----------|--------|------|---------------|
| `node_type` | `Optional[Type]` (class ref, default `NodeSet`) | `Option<String>` | Align naming/semantics; Python passes a class, Rust passes a string |
| `node_name` | `Optional[List[str]]` (list of strings) | `Option<String>` (single string) | **Change to `Option<Vec<String>>`** for multi-name filtering |
| `top_k` | `int` (default `10` at API) | `Option<usize>` | OK, but retriever defaults differ |
| `datasets` | `Optional[Union[list[str], str]]` (accepts single string) | `Option<Vec<String>>` (always vec) | Minor shape difference |

### Architectural Limitation: Per-Request Parameters

**NEW FINDING:** Because Rust retrievers are `Arc` singletons (created once by `SearchBuilder`), per-request parameters like `top_k`, `system_prompt`, `node_name`, `wide_search_top_k`, and `triplet_distance_penalty` are set at **construction time** and cannot be overridden per search call. The `SearchRetriever` trait's methods (`get_context(&self, query)` and `get_completion(&self, query, context, session)`) do not carry a `SearchRequest` parameter.

In Python, retrievers are instantiated fresh per call with the current request's parameters baked into the constructor. This is a fundamental architectural gap that affects all per-request configurability.

---

## 4. Retriever Pipeline Pattern

### Change: Consider adding a three-step pipeline

Python's three-step pattern (`get_retrieved_objects` → `get_context_from_objects` → `get_completion_from_context`) provides:

1. **Access tracking** between steps 1 and 2 (Python calls `update_node_access_timestamps(retrieved_objects)`)
2. **Raw object access** in step 3 — `get_completion_from_context` receives both the raw objects AND the formatted context
3. **Session ID tracking** — `_extract_context_object_ids()` extracts graph element IDs from raw objects for session metadata

The Rust two-step pattern (`get_context` → `get_completion`) merges steps 1-2 and loses access to raw pre-context objects in the completion step.

**Recommendation:** This is a design trade-off. The two-step Rust pattern is cleaner. For compatibility, consider:
- Adding an `update_access_timestamps()` hook within `get_context`
- Storing intermediate data (used graph element IDs) in the `SearchItem` payload for session tracking
- No change needed to the trait — the current design is sufficient

---

## 5. Brute Force Triplet Search

**Files:**
- Python: `cognee/modules/retrieval/utils/brute_force_triplet_search.py`, `cognee/modules/graph/cognee_graph/CogneeGraph.py`
- Rust: `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, `crates/search/src/graph_retrieval/triplet_ranking.rs`

This is the **most critical area** for compatibility.

### 5.1 Vector Collections Searched

| Collection | Python | Rust | Action |
|------------|--------|------|--------|
| `Entity_name` | Yes | Yes | OK |
| `Entity_description` | No | Yes | **Remove from Rust** |
| `EntityType_name` | Yes | No | **Add to Rust** |
| `TextSummary_text` | Yes | Yes | OK |
| `DocumentChunk_text` | Yes | Yes | OK |
| `EdgeType_relationship_name` | Yes (always appended) | No | **Add to Rust** |
| `Triplet_embeddable_text` | No | Yes | **Remove from Rust** (or make configurable) |

Python also supports dynamic collection discovery from `DataPoint` subclass `metadata["index_fields"]`. Rust has hardcoded collections only.

**Required change in Rust:**
```rust
const SEARCH_COLLECTIONS: [(&str, &str); 4] = [
    ("Entity", "name"),
    ("TextSummary", "text"),
    ("EntityType", "name"),
    ("DocumentChunk", "text"),
];
// Always append EdgeType/relationship_name
const EDGE_COLLECTION: (&str, &str) = ("EdgeType", "relationship_name");
```

### 5.2 Scoring Algorithm

**This is the single most impactful difference.**

| Aspect | Python | Rust |
|--------|--------|------|
| Score components | **3**: node1_distance + node2_distance + **edge_distance** | **2**: source_score + target_score |
| Score semantics | **Distance** (lower = better) | **Similarity** (higher = better) |
| Selection | `heapq.nsmallest(k)` | `sort_by` descending |
| Unmatched penalty | **6.5** (configurable via `triplet_distance_penalty`) | **0.0** (via `unwrap_or(0.0)`) |

**Required changes:**

1. **Add edge scoring:** The Rust `rank_edge_score` must include an edge distance component. This requires:
   - Searching the `EdgeType_relationship_name` collection
   - Building an `edge_scores` map (keyed by edge type ID or relationship name hash)
   - Passing edge scores into the ranking function

2. **Switch to distance-based scoring:** Either:
   - Convert similarity scores to distances (`distance = 1.0 - similarity` for cosine) and use `nsmallest` selection
   - Or keep similarity scores but invert the ranking to match Python's behavior

3. **Fix the penalty semantics:** In Python, `triplet_distance_penalty` is the **default distance** assigned to ALL unmatched graph elements (nodes and edges not found in vector search). In Rust, it is a **score deduction** applied only to nodes found via the `Triplet` collection — a completely different meaning. Change the Rust semantics to match Python: use the penalty as a default distance for unmatched components, and change the default from `0.0` to `3.5` (verified actual Python default).

4. **Fix multi-collection score merging:** When a node appears in multiple vector collections, Python **overwrites** with the last-processed score (`CogneeGraphElements.py:75`), while Rust keeps the **maximum** score (`brute_force_triplet_search.rs:86`). Align to Python's behavior (last-write-wins) or document the difference.

5. **New ranking formula:**
   ```rust
   // Python-compatible scoring
   fn rank_edge_score(
       source_distance: f32,
       target_distance: f32,
       edge_distance: f32,
   ) -> f32 {
       source_distance + target_distance + edge_distance  // lower = better
   }
   ```

### 5.3 Graph Projection and Filtering

| Aspect | Python | Rust | Action |
|--------|--------|------|--------|
| Node property projection | `["id", "description", "name", "type", "text"]` | Only `name` extracted | **Expand to match Python** |
| Edge property projection | `["relationship_name", "edge_text", "edge_object_id"]` | Not extracted (only `relationship_name` from edge data) | **Add `edge_text`, `edge_object_id`** |
| Feedback weight projection | Added when `feedback_influence > 0` | Not supported | Add when feedback is implemented |
| ID-filtered graph load | `get_id_filtered_graph_data(target_ids=...)` when wide search active | Always full graph load, post-filter | **Optimize: add ID-filtered graph load** |
| NodeSet subgraph | `get_nodeset_subgraph(node_type, node_name)` | Not supported | **Add support** |
| Configurable collections | Yes (caller can override) | Hardcoded constant | **Add parameter** |

### 5.4 Feedback Influence

Python's `_effective_distance` blending formula:
```
normalized_distance = distance / 2.0
blended = (1 - feedback_influence) * normalized_distance + feedback_influence * (1 - feedback_weight)
effective = blended * 2.0
```

**Required change:** Add `feedback_influence: f32` to `GraphRetrievalConfig` and implement the blending formula in the ranking function.

### 5.5 Wide Search Filtering

| Aspect | Python | Rust |
|--------|--------|------|
| Default `wide_search_top_k` | **100** | **20** |
| Batch mode | Forces `None` (unlimited) | Not supported |
| `node_name` override | Forces `None` | Not supported |
| Graph filtering | Pre-filter via `get_id_filtered_graph_data` | Post-filter after full load |

**Required changes:**
- Change default `wide_search_top_k` from `20` to `100`
- Consider adding `get_id_filtered_graph_data` to `GraphDBTrait` for efficiency

---

## 6. Context Rendering (resolve_edges_to_text)

**Files:**
- Python: `cognee/modules/graph/utils/resolve_edges_to_text.py`
- Rust: `crates/search/src/utils/resolve_edges_to_text.rs`

### Current Rust Output
```
Alice -[KNOWS]-> Bob
Charlie -[WORKS_AT]-> Acme Corp
```

### Required Python-Compatible Output
```
Nodes:
Node: Alice went to the store and... [alice, store, went]
__node_content_start__
Alice went to the store and bought some groceries. She then went home.
__node_content_end__

Node: Bob
__node_content_start__
A software engineer working at Acme Corp
__node_content_end__

Connections:
Alice went to the store and... [alice, store, went] --[knows]--> Bob
```

### Required Changes

1. **Implement `_create_title_from_text`:** First 7 words + top 3 frequent non-stop words in brackets
2. **Implement node content extraction:** For nodes with `text` attribute, render full text. For others, use `name` or `description`.
3. **Two-section format:** Render "Nodes:" section with `__node_content_start__` / `__node_content_end__` markers, then "Connections:" section
4. **Arrow syntax:** Change from `-[REL]->` to `--[REL]-->`
5. **Stop words list:** Port or embed the Python stop words set
6. **Prerequisite:** The brute force triplet search must project node properties (`text`, `description`, `name`, `type`) so they are available for rendering

---

## 7. LLM Prompts

All Rust prompts need to be updated to match Python's wording. The prompts are a critical part of the search behavior contract.

### 7.1 System Prompt (answer_simple_question)

| | Python | Rust |
|-|--------|------|
| **Text** | `Answer the question using the provided context. Be as brief as possible.` | `You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.` |
| **Action** | **Change Rust to match Python exactly** | |

### 7.2 RAG User Prompt (context_for_question)

| | Python | Rust |
|-|--------|------|
| **Text** | `` The question is: `{{ question }}` And here is the context: `{{ context }}` `` | `Question:\n{question}\n\nContext:\n{context}` |
| **Action** | **Change Rust to match Python** (backtick wrapping, prose style) | |

### 7.3 Graph User Prompt (graph_context_for_question)

| | Python | Rust |
|-|--------|------|
| **Text** | `` The question is: `{{ question }}` and here is the context provided with a set of relationships from a knowledge graph separated by \n---\n each represented as node1 -- relation -- node2 triplet: `{{ context }}` `` | Uses generic RAG template (no graph-specific framing) |
| **Action** | **Add a separate `GRAPH_CONTEXT_USER_PROMPT_TEMPLATE` in Rust** | |

### 7.4 Summarize Search Results

| | Python | Rust |
|-|--------|------|
| **Text** | `You are a top-tier summarization engine that is meant to eliminate redundancies. The input contains relationships enclosed by "--". Summarize the input into natural sentences, listing all relationships.` | `You summarize graph evidence into concise factual context.` |
| **Action** | **Change Rust to match Python** | |

### 7.5 Search Type Selector (FEELING_LUCKY)

| | Python | Rust |
|-|--------|------|
| **Text** | 125-line detailed prompt with per-type descriptions and examples | `You are a search method selector. Return ONLY one valid search type name in SCREAMING_SNAKE_CASE.` |
| **Action** | **Port the full Python prompt to Rust** | |

### 7.6 CoT Validation System Prompt

| | Python | Rust |
|-|--------|------|
| **Text** | `You are a helpful agent who are allowed to use only the provided question answer and context. I want to you find reasoning what is missing from the context or why the answer is not answering the question or not correct strictly based on the context.` | `You validate whether an answer is sufficiently grounded in graph context.` |
| **Action** | **Change Rust to match Python** | |

### 7.7 CoT Validation User Prompt

| | Python | Rust |
|-|--------|------|
| **Format** | XML tags: `<QUESTION>`, `<ANSWER>`, `<CONTEXT>` | Labeled: `Question:\n...\nAnswer:\n...\nContext:\n...` |
| **Action** | **Change Rust to use XML tags matching Python** | |

### 7.8 CoT Follow-up System Prompt

| | Python | Rust |
|-|--------|------|
| **Text** | `You are a helpful assistant whose job is to ask exactly one clarifying follow-up question, to collect the missing piece of information needed to fully answer the user's original query. Respond with the question only (no extra text, no punctuation beyond what's needed).` | `Generate one concise follow-up graph query to improve the answer.` |
| **Action** | **Change Rust to match Python** | |

### 7.9 CoT Follow-up User Prompt

| | Python | Rust |
|-|--------|------|
| **Format** | Preamble ("Based on the following, ask exactly one question...") + `<QUERY>`, `<ANSWER>`, `<REASONING>` XML tags | `Question:\n...\nAnswer:\n...\nValidation:\n...\nProvide one follow-up graph query.` |
| **Action** | **Change Rust to match Python** (XML tags + preamble) | |

### 7.10 Natural Language Retriever System Prompt

| | Python | Rust |
|-|--------|------|
| **Text** | 66-line prompt with full node schema, 17 rules across 3 categories, examples | `You convert natural language requests into graph queries. Return ONLY a query string.` |
| **Action** | **Port the full Python prompt to Rust** | |

### 7.11 Temporal Interval Extraction Prompt

| | Python | Rust |
|-|--------|------|
| **Text** | 14 lines with current time injection (`{{ time_now }}`), 8 extraction rules | `Extract the temporal interval for a user question. Return JSON...` |
| **Action** | **Port the full Python prompt to Rust, inject current time** | |

### 7.12 Prompt Storage Strategy

Python uses Jinja2 template files on disk (`cognee/infrastructure/llm/prompts/*.txt`). Rust uses inline `const` strings.

**Recommendation:** Either:
- (A) Keep inline constants in Rust but match the wording exactly, OR
- (B) Port to file-based prompts with a simple `{variable}` template engine

Option (A) is simpler and sufficient for compatibility. Option (B) allows runtime customization.

### 7.13 Prompt Resolution Priority (NEW FINDING)

Python's `resolve_system_prompt` checks inline `system_prompt` **first**, then falls back to `system_prompt_path` file. Rust's `resolve_system_prompt` checks `system_prompt_path` **first**, then inline `system_prompt`, then the default constant. **The priority is inverted.** This means a caller providing both `system_prompt` and `system_prompt_path` gets different behavior in each implementation.

**Action:** Align Rust to check inline `system_prompt` first (matching Python).

---

## 8. Session Management

### Current State

| Aspect | Python | Rust |
|--------|--------|------|
| History format | Formatted string: `"Previous conversation:\n\n[time]\nQUESTION: ...\nANSWER: ...\n\n"` | `Vec<Message>` objects (User/Assistant) |
| Injection point | Prepended to system prompt: `history + "\nTASK:" + system_prompt` | Separate messages between system and user |
| History length | `session_history_last_n = 10` | Last 10 entries (same) |
| Include context | `include_context=False` for completion | Not included |
| Auto-feedback detection | Yes (parallel LLM call in `generate_completion_with_session`) | No |

### Required Changes

1. **Format history as a string prepended to the system prompt** (matching Python's `history + "\nTASK:" + system_prompt` pattern) instead of injecting separate messages.

   **OR** document this as an intentional improvement in Rust (multi-message history is arguably better for LLM understanding). This is a behavioral difference that affects LLM output quality but not data compatibility.

2. **Add auto-feedback detection** if enabled (lower priority — this is a Python feature that enhances session management but doesn't affect core search compatibility).

---

## 9. Per-Retriever Gap Analysis

### 9.1 Chunks Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Default `top_k` | Python: 5, Rust: 10 | High |

### 9.2 Summaries Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Default `top_k` | Python: 5, Rust: 10 | High |

### 9.3 Completion Retriever (RAG_COMPLETION)

| Gap | Details | Priority |
|-----|---------|----------|
| Default `top_k` | Python: 1 (!), Rust: 10 | **Critical** |
| User prompt template | Different wording (see Section 7.2) | High |
| System prompt | Different wording (see Section 7.1) | High |
| Structured output | Python supports `response_model` (Pydantic), Rust returns `String` only | Medium |
| Context join separator | Python: `"\n"`, Rust: `"\n\n"` | Medium |
| Prompt resolution priority | Python: inline first; Rust: file path first (**inverted**, see 7.13) | Medium |

### 9.4 Triplet Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Default `top_k` | Python: 5, Rust: 10 | High |
| Collection | Python: only `Triplet_text`, Rust: tries `text` then falls back to `embeddable_text` | Low (Rust is more resilient) |
| Context join separator | Python: `"\n"`, Rust: `"\n\n"` (same issue as CompletionRetriever) | Medium |
| Dual-level fallback | Rust has fallback at BOTH collection level AND per-item payload extraction (text→embeddable_text); Python has neither | Low |

### 9.5 Graph Completion Retriever (DEFAULT)

| Gap | Details | Priority |
|-----|---------|----------|
| Scoring algorithm | 2-component vs 3-component (see Section 5.2) | **Critical** |
| Vector collections | Different sets (see Section 5.1) | **Critical** |
| Context rendering | Flat vs two-section (see Section 6) | **Critical** |
| Default `top_k` | Python: 5, Rust: 10 | High |
| Default `wide_search_top_k` | Python: 100, Rust: 20 | High |
| Default `triplet_distance_penalty` | Python: 3.5, Rust: 0.0 — **different semantics too** (see Section 5.2) | **Critical** |
| Multi-collection score merging | Python overwrites (last-write-wins); Rust keeps max score | High |
| User prompt template | Generic RAG vs graph-specific (see Section 7.3) | High |
| Node property projection | Python projects 5 props (`id`, `description`, `name`, `type`, `text`); Rust only `name` | High |
| Edge property projection | Python projects `relationship_name`, `edge_text`, `edge_object_id`; Rust only `relationship_name` | High |
| Node type/name filtering | Python has `node_type`, `node_name`, `node_name_filter_operator`; Rust has none | Medium |
| Feedback influence | Python has full blending formula; Rust has nothing | Medium |
| Batch query support | Python supports `query_batch`; Rust single query only | Low |

### 9.6 Graph Summary Completion Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Summarization prompt | Terse vs detailed (see Section 7.4) | High |
| Inherits all Graph Completion gaps | Same scoring, collections, rendering issues | **Critical** |

### 9.7 Graph Completion Context Extension Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Default rounds | Python: 4, Rust: 2 | High |
| Extension strategy | Python: uses completion text as search query. Rust: asks LLM to generate a follow-up query | **High** |
| Deduplication granularity | Python: by Edge object identity. Rust: by UUID or JSON payload string | Medium |
| Architectural difference | Python modifies only retrieval phase (base class handles completion). Rust controls the entire `get_completion` flow inline | Low |
| Inherits all Graph Completion gaps | Same scoring, collections, rendering issues | **Critical** |

**Note:** The extension strategy is fundamentally different. Python generates a **full answer** using the standard answer prompts, then uses that answer text as an embedding search query. Rust generates a **purpose-built follow-up graph query** using dedicated extension prompts. This is answer-driven expansion (Python) vs query-driven expansion (Rust) — a fundamentally different retrieval philosophy.

### 9.8 Graph Completion CoT Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Default `max_iter` | Python: 4, Rust: 2 | High |
| Iteration semantics | Python: initial completion + N rounds of (validate, followup, merge, re-complete). Rust: N iterations with answer at top, validate/followup afterward. With defaults: Python does 5 answers + 4 validate/followup; Rust does 2 answers + 1 validate/followup | **High** |
| Early termination | Python: no convergence-based early termination (always runs all iterations). Rust: breaks on empty follow-up query | Medium |
| All CoT prompts | Different wording and format (see Sections 7.6-7.9). Python followup explicitly mentions "exploring a knowledge graph with entities, entity types and document chunks"; Rust has no such guidance | High |
| Inherits all Graph Completion gaps | Same scoring, collections, rendering issues | **Critical** |

### 9.9 Cypher Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Feature gate | **CORRECTED:** Both have `ALLOW_CYPHER_QUERY` check. Python checks it in the factory (`get_search_type_retriever_instance.py:230`), Rust checks inside the retriever itself. Same effect | — |
| Python retriever incomplete | Python's `get_context_from_objects` and `get_completion_from_context` return `None` (TODO). Rust is more complete | — |

### 9.10 Natural Language Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| System prompt | 1 line vs 66 lines (see Section 7.10) | **Critical** |
| Node schema info | Python **hardcodes** node schemas in prompt (5 types: EntityType, Entity, TextDocument, DocumentChunk, TextSummary); Rust has none. Note: Python also fetches schemas dynamically but does not pass them to the prompt | High |
| LLM error handling | Rust `?` on LLM call immediately aborts retry loop; Python catches all exceptions and continues retrying | Medium |
| Default previous_attempts | Python: `"No attempts yet"`, Rust: `""` (empty string) | Low |
| Both include edge schemas and retry loop | Same pattern (max_attempts=3 in both) | — |

### 9.11 Temporal Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Time extraction prompt | **3 sentences** (corrected from "2") vs 14 lines (see Section 7.11) | High |
| Current time injection | Python passes `datetime.now()` to prompt; Rust does not | High |
| Field names | Python: `starts_at`/`ends_at` (structured `Timestamp` model with year/month/day/hour/minute/second int fields), Rust: `start`/`end` (plain `Option<String>`) | Medium |
| Default `top_k` | Python: 5, Rust: 10 | High |
| Event graph model | Python: separate Timestamp nodes connected to Event nodes via graph edges (2-hop traversal). Rust: timestamp properties directly on event nodes (property-based filtering) | **High** |
| Scoring inversion | Python: ascending sort (distance, lower=better). Rust: descending sort (similarity, higher=better) with dual-source scoring (graph edges + vector) | High |
| Context format | Python: description-only joined with `\n#####################\n`. Rust: structured `"name (time): description"` lines | Medium |
| Event identification | Python: only checks `type='Event'`. Rust: checks 6 type keys + considers nodes with any temporal property as events (much more permissive) | Medium |
| Error resilience | Rust catches LLM errors gracefully (falls back); Python propagates exceptions | Low |
| Graph engine methods | Python uses `collect_time_ids()`/`collect_events()` (Cypher-based); Rust reimplements locally (full graph scan) | Low (functional parity) |

### 9.12 Coding Rules Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Rule loading | Python: targeted `get_nodeset_subgraph` query for `NodeSet` type. Rust: full graph scan + heuristic `is_rule_node` (matches any node with "rule" in type/kind/label — much looser) | Medium |
| Rule set input | Python: constructor parameter (list); Rust: parsed from query string (comma/semicolon/newline split) | Low |
| Rule text extraction | Python: only `"text"` field. Rust: `"text"` first, falls back to `"rule"` field | Low |
| Output format | Python: returns raw string list. Rust: returns `SearchOutput::Rules(Vec<Rule>)` with `node_set` association preserved | Low |

### 9.13 Lexical Retriever (Chunks Lexical)

| Gap | Details | Priority |
|-----|---------|----------|
| Default `top_k` | **CORRECTED:** Both default to 10 (document previously incorrectly claimed Python=5) | — |
| **Ranking bug in Rust** | When `with_scores=false`, all items get score=0.0 via `unwrap_or_default()`, making sort order arbitrary. Python always computes scores internally regardless of `with_scores` | **High** |
| Caching | Python caches tokenized chunks with `_initialized` flag + asyncio lock; Rust reloads every call | Medium (performance) |
| Unicode lowercasing | Python: `text.lower()` (full Unicode). Rust: `ch.to_ascii_lowercase()` (ASCII only — fails for accented chars) | Medium |
| Tokenization regex | Python: `re.findall(r"\w+", ...)`, Rust: manual char iteration with `is_alphanumeric() || ch == '_'` | Low (functionally similar for ASCII) |
| Pluggability | Python: generic `LexicalRetriever` accepts any tokenizer/scorer callables. Rust: hardcodes Jaccard | Low |

### 9.14 Feeling Lucky Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Selection prompt | Rust: 1-line constant + dynamic allowed-types list. Python: 125-line static prompt with per-type descriptions and examples (see Section 7.5) | **Critical** |
| Architecture | Python: NOT a retriever class — handled as a special case in the factory function. Rust: dedicated `FeelingLuckyRetriever` struct with `SearchRetriever` trait | Low |
| Self-referencing guard | Rust filters out `FeelingLucky` from selected type (prevents infinite recursion). Python has NO such guard — LLM returning "FEELING_LUCKY" would cause infinite recursion | Low (Rust is safer) |
| Dynamic vs static types | Rust dynamically builds allowed-types list from registered retrievers. Python hardcodes types in the prompt file (adding a type requires editing the prompt) | Low (Rust is better) |
| Dead `CODE` type in Python prompt | Python prompt describes a `CODE` search type that doesn't exist in the enum — selecting it always falls back to RAG_COMPLETION | — |
| Fallback | Both fall back to `RAG_COMPLETION` | OK |

### 9.15 Feedback Retriever

| Gap | Details | Priority |
|-----|---------|----------|
| Python equivalent | **CORRECTED:** Python handles feedback via a standalone `detect_feedback()` function in `feedback_detection.py` (not on `SessionManager`), called from `completion.py`. It is NOT a search type — Python's `SearchType` enum has no `FEEDBACK` variant | Medium |
| Rust-only search type | Rust has `SearchType::Feedback` and a full `FeedbackRetriever`. Python's `skill.md` documents a FEEDBACK type that doesn't actually exist in the code | Medium |
| `SearchOutput::Ack` | Rust-only output variant for feedback acknowledgments; no Python equivalent | Low |

---

## 10. Default Value Mismatches

All defaults that differ between Python and Rust:

| Parameter | Python Default | Rust Default | Location in Rust | Action |
|-----------|---------------|-------------|-----------------|--------|
| `top_k` (ChunksRetriever) | 5 | 10 | `chunks_retriever.rs:14` | Change to 5 |
| `top_k` (SummariesRetriever) | 5 | 10 | `summaries_retriever.rs:14` | Change to 5 |
| `top_k` (CompletionRetriever) | 1 | 10 | `completion_retriever.rs:18` | Change to 1 |
| `top_k` (TripletRetriever) | 5 | 10 | `triplet_retriever.rs:19` | Change to 5 |
| `top_k` (GraphCompletionRetriever) | 5 | 10 | `graph_completion_retriever.rs:20` | Change to 5 |
| `top_k` (TemporalRetriever) | 5 | 10 | `temporal_retriever.rs:21` | Change to 5 |
| `top_k` (brute_force_triplet_search) | 5 | 10 | `brute_force_triplet_search.rs:30` | Change to 5 |
| `top_k` (LexicalRetriever) | 10 | 10 | — | OK (both match) |
| `wide_search_top_k` | 100 | 20 | `brute_force_triplet_search.rs:11` | Change to 100 |
| `triplet_distance_penalty` | 3.5 | 0.0 | `brute_force_triplet_search.rs:32` | Change to 3.5 **and fix semantics** |
| `context_extension_rounds` | 4 | 2 | `advanced_graph_retrievers.rs:22` | Change to 4 |
| `max_iter` (CoT) | 4 | 2 | `advanced_graph_retrievers.rs:23` | Change to 4 |
| Context join separator (RAG+Triplet) | `"\n"` | `"\n\n"` | `completion_retriever.rs`, `triplet_retriever.rs` | Change to `"\n"` |
| Prompt resolution priority | inline first | file path first | `completion.rs` | **Invert to match Python** |
| NL previous_attempts default | `"No attempts yet"` | `""` | `cypher_nl_retrievers.rs` | Change to `"No attempts yet"` |

---

## 11. Vector DB Interface Differences

### Python `VectorDBInterface.search()` Parameters Not in Rust

| Parameter | Python Type | Purpose | Action |
|-----------|------------|---------|--------|
| `query_text` | `Optional[str]` | Text query (alternative to vector) | Consider adding |
| `with_vector` | `bool` | Return vectors with results | Low priority |
| `include_payload` | `bool` | Return metadata (default False) | Rust always returns metadata |
| `node_name` | `Optional[List[str]]` | Filter by `belongs_to_set` field | **Add for node filtering** |
| `node_name_filter_operator` | `str` | `"AND"` / `"OR"` | **Add for node filtering** |

### Python `batch_search` Method

Python has `batch_search(collection_name, query_texts, limit, ...)` which processes multiple queries at once. Rust has no batch search API.

**Action:** Add `batch_search` to `VectorDb` trait (needed for batch query support).

---

## 12. Graph DB Interface Differences

### Python Methods Not in Rust's `GraphDBTrait`

| Method | Python Signature | Used By | Priority |
|--------|-----------------|---------|----------|
| `get_nodeset_subgraph` | `(node_type, node_name, node_name_filter_operator)` | Graph retrieval with node filtering | Medium |
| `get_id_filtered_graph_data` | `(target_ids: List[str])` | Efficient wide-search graph load | Medium |
| `get_node_feedback_weights` | `(node_ids) -> Dict[str, float]` | Feedback influence | Low |
| `set_node_feedback_weights` | `(weights) -> Dict[str, bool]` | Feedback influence | Low |
| `get_edge_feedback_weights` | `(edge_ids) -> Dict[str, float]` | Feedback influence | Low |
| `set_edge_feedback_weights` | `(weights) -> Dict[str, bool]` | Feedback influence | Low |
| `get_connections` | `(node_id) -> List[(Node, Edge, Node)]` | Graph traversal | Low |
| `get_neighbors` | `(node_id) -> List[NodeData]` | Graph traversal | Low |

**Note:** `collect_time_ids` and `collect_events` are NOT in the Python interface either — they appear to be adapter-specific methods. Rust's local reimplementation is fine.

---

## 13. Dataset Authorization & Multi-Tenancy

### Current Differences

| Aspect | Python | Rust |
|--------|--------|------|
| Authorization | `get_authorized_existing_datasets(datasets, "read", user)` | No authorization check |
| DB context switching | `set_database_global_context_variables(dataset.id, dataset.owner_id)` per dataset | N/A |
| Parallel per-dataset retrieval | `asyncio.gather()` over authorized datasets | Single retrieval, post-hoc filtering |
| Dataset info in results | `dataset_name`, `dataset_id`, `dataset_tenant_id` from DB | `dataset_id` from vector payload |

### Required Changes for Compatibility

For true interoperability, the Rust search must:

1. **Accept dataset IDs and filter at the DB level** rather than post-hoc payload filtering — otherwise the LLM sees data from unauthorized datasets
2. **Pass dataset_id to vector search as metadata filter** — most vector DBs support this (Qdrant has payload filtering)
3. **Consider adding a pre-retrieval authorization hook** — even if actual auth logic is external

---

## 14. Query Logging & Observability

### Python
- Every search query is logged to a `queries` table (`id`, `text`, `query_type`, `user_id`, `created_at`, `updated_at`)
- Logged unconditionally via `log_query()` at the start of `search()`
- OpenTelemetry spans around each retriever step (`cognee.retrieval.get_objects`, `cognee.retrieval.get_context`, `cognee.retrieval.get_completion`)

### Rust
- Query logging is opt-in via `save_interaction` flag
- Logged via `SearchHistoryDb` trait (`log_query`, `log_result`)
- `tracing::debug!` logging only
- The `queries` table schema in Rust uses `query_log` and `result_log` (different table names)

### Required Changes

1. **Table naming:** Align Rust table names with Python (`queries` instead of `query_log`)
2. **Make logging unconditional** (or at least default to `true`) to match Python
3. **Consider adding OpenTelemetry spans** for observability parity (lower priority)

---

## 15. Result Formatting & Return Types

### Python `SearchResultPayload`
```python
class SearchResultPayload(BaseModel):
    result_object: Any          # raw retrieved objects
    context: Optional[...]      # formatted context string/list
    completion: Optional[...]   # LLM completion
    search_type: SearchType
    only_context: bool
    dataset_name: Optional[str]
    dataset_id: Optional[UUID]
    dataset_tenant_id: Optional[UUID]
```

### Rust `SearchResponse`
```rust
pub struct SearchResponse {
    pub search_type: SearchType,
    pub result: SearchOutput,         // enum: Items/Text/Texts/GraphQueryRows/Rules/Ack
    pub context: Option<HashMap<String, SearchContext>>,
    pub graphs: Option<HashMap<String, SearchGraph>>,
    pub diagnostics: Option<HashMap<String, Value>>,
    pub datasets: Option<Vec<Uuid>>,
    pub only_context: bool,
    pub use_combined_context: bool,
}
```

### Differences
1. Python returns a `List[SearchResult]` (one per dataset); Rust returns a single `SearchResponse` with per-dataset context maps
2. Python has `verbose` mode that expands results into `text_result`/`context_result`/`objects_result`
3. Rust has `graphs` (auto-derived graph visualization) and `diagnostics` that Python lacks
4. Rust has `SearchOutput` enum with strong typing; Python uses `Any`

### Required Changes
- Add `verbose` mode support to Rust (or document that Rust always returns full data)
- Ensure the serialized JSON shapes can be consumed by the same client code

---

## 16. Prioritized Change List

### P0 — Critical (Blocks interoperability)

| # | Change | Task Doc | Status | Effort |
|---|--------|----------|--------|--------|
| 1 | **Fix scoring algorithm:** 3-component distance scoring, penalty semantics, score merging | [task-01](tasks/task-01-fix-scoring-algorithm.md) | **Done** | Medium |
| 2 | **Fix vector collections:** Align with Python defaults, separate edge collection | [task-02](tasks/task-02-fix-vector-collections.md) | **Done** | Small |
| 3 | **Fix default values:** All `top_k`, `wide_search_top_k`, penalty, iterations, separators | [task-03](tasks/task-03-fix-default-values.md) | **Done** | Small |
| 4 | **Context rendering:** Two-section format with node content, title algorithm | [task-04](tasks/task-04-context-rendering.md) | **Done** | Medium |
| 5 | **Graph-specific user prompt:** `graph_context_for_question` template | [task-05](tasks/task-05-graph-user-prompt.md) | **Done** | Small |
| 6 | **Node property projection:** Extract `description`, `type`, `text`, `id` | [task-06](tasks/task-06-expand-node-property-projection.md) | **Done** (implemented as part of task-04) | Small |

### P1 — High (Affects search quality/behavior)

| # | Change | Task Doc | Status | Effort |
|---|--------|----------|--------|--------|
| 7 | **Port core LLM prompts:** System, RAG user, CoT, summarization | [task-07](tasks/task-07-port-core-llm-prompts.md) | **Done** | Medium |
| 8 | **Port FEELING_LUCKY prompt:** Full search type selector | [task-08](tasks/task-08-port-feeling-lucky-prompt.md) | **Done** | Small |
| 9 | **Port NL retriever prompt:** Full Cypher generator with schemas | [task-09](tasks/task-09-port-nl-retriever-prompt.md) | **Done** | Small |
| 10 | **Port Temporal prompt:** Extraction rules + current time injection | [task-10](tasks/task-10-port-temporal-prompt.md) | **Done** | Medium |
| 11 | **Fix context extension strategy:** Answer-driven expansion | [task-11](tasks/task-11-fix-context-extension-strategy.md) | **Done** | Medium |
| 12 | **Fix CoT iteration semantics:** Initial + N rounds pattern | [task-12](tasks/task-12-fix-cot-iteration-semantics.md) | **Done** | Medium |
| 13 | **Fix prompt resolution priority:** Inline before file path | [task-13](tasks/task-13-fix-prompt-resolution-priority.md) | **Done** | Tiny |
| 14 | **Fix Lexical ranking bug:** Always compute scores internally | [task-14](tasks/task-14-fix-lexical-ranking-bug.md) | **Done** | Small |
| 15 | **Fix NL retriever error handling:** Catch LLM errors in retry | [task-15](tasks/task-15-fix-nl-retriever-error-handling.md) | **Done** | Small |

### P2 — Medium (Feature parity)

| # | Change | Task Doc | Status | Effort |
|---|--------|----------|--------|--------|
| 16 | **Add `feedback_influence`:** To request, config, and scoring | [task-16](tasks/task-16-add-feedback-influence.md) | **Done** | Medium |
| 17 | **Add node filtering:** `node_type`/`node_name`/operator | [task-17](tasks/task-17-add-node-filtering.md) | **Done** | Medium |
| 18 | **Fix `node_name` type:** `Option<String>` → `Option<Vec<String>>` | [task-18](tasks/task-18-fix-node-name-type.md) | **Done** | Small |
| 19 | **Add `verbose` mode:** Result formatting | [task-19](tasks/task-19-add-verbose-mode.md) | **Done** | Small |
| 20 | **Add `retriever_specific_config`:** Per-retriever overrides | [task-20](tasks/task-20-add-retriever-specific-config.md) | **Done** | Medium |
| 21 | **Align session history:** Prepend to system prompt | [task-21](tasks/task-21-align-session-history.md) | **Done** | Small |
| 22 | **Add `user` parameter:** Authorization context | [task-22](tasks/task-22-add-user-parameter.md) | **Done** | Small |
| 23 | **Align query log table:** `queries` instead of `query_log` | [task-23](tasks/task-23-align-query-log-table.md) | **Done** | Small |
| 24 | **Unconditional query logging:** Default to enabled | [task-24](tasks/task-24-unconditional-query-logging.md) | **Done** | Tiny |
| 25 | **Add `response_model` support:** Structured LLM output | [task-25](tasks/task-25-add-response-model-support.md) | **Done** | Large |
| 26 | **Fix per-request params:** Pass through trait or SearchParams | [task-26](tasks/task-26-fix-per-request-params.md) | **Done** | Large |
| 27 | **Fix Unicode lowercasing:** Full Unicode `to_lowercase()` | [task-27](tasks/task-27-fix-unicode-lowercasing.md) | **Done** | Tiny |

### P3 — Low (Nice to have)

| # | Change | Task Doc | Status | Effort |
|---|--------|----------|--------|--------|
| 28 | Batch query support (`query_batch`) | [task-28](tasks/task-28-batch-query-support.md) | **Done** | Large |
| 29 | `batch_search` on `VectorDb` trait | [task-29](tasks/task-29-batch-search-vector-db.md) | **Done** | Medium |
| 30 | `get_id_filtered_graph_data` on `GraphDBTrait` | [task-30](tasks/task-30-id-filtered-graph-data.md) | **Done** | Medium |
| 31 | Lexical retriever chunk caching | [task-31](tasks/task-31-lexical-chunk-caching.md) | **Done** | Medium |
| 32 | Auto-feedback detection in session | [task-32](tasks/task-32-auto-feedback-detection.md) | **Done** | Medium |
| 33 | OpenTelemetry spans | [task-33](tasks/task-33-opentelemetry-spans.md) | **Done** | Medium |
| 34 | Dynamic vector collection discovery | [task-34](tasks/task-34-dynamic-collection-discovery.md) | **Done** | Medium |
| 35 | Access timestamp tracking | [task-35](tasks/task-35-access-timestamp-tracking.md) | **Done** | Small |
| 36 | Community retriever plugin mechanism | [task-36](tasks/task-36-community-retriever-plugin.md) | **Done** | Medium |
| 37 | `FeelingLucky` self-referencing guard | [task-37](tasks/task-37-feeling-lucky-guard.md) | **Done** (already in Rust) | — |

---

## Appendix A: File Mapping

| Python File | Rust File |
|-------------|-----------|
| `cognee/api/v1/search/search.py` | `crates/search/src/orchestration/search_orchestrator.rs` |
| `cognee/modules/search/methods/search.py` | `crates/search/src/orchestration/search_orchestrator.rs` |
| `cognee/modules/search/methods/get_retriever_output.py` | `crates/search/src/orchestration/search_orchestrator.rs` |
| `cognee/modules/search/methods/get_search_type_retriever_instance.py` | `crates/search/src/orchestration/search_execution_builder.rs` |
| `cognee/modules/search/types/SearchType.py` | `crates/search/src/types/search_type.rs` |
| `cognee/modules/search/types/SearchResult.py` | `crates/search/src/types/search_result.rs` |
| `cognee/modules/search/models/SearchResultPayload.py` | `crates/search/src/types/search_result.rs` |
| `cognee/modules/retrieval/base_retriever.py` | `crates/search/src/retrievers/base_retriever.rs` |
| `cognee/modules/retrieval/chunks_retriever.py` | `crates/search/src/retrievers/chunks_retriever.rs` |
| `cognee/modules/retrieval/summaries_retriever.py` | `crates/search/src/retrievers/summaries_retriever.rs` |
| `cognee/modules/retrieval/completion_retriever.py` | `crates/search/src/retrievers/completion_retriever.rs` |
| `cognee/modules/retrieval/triplet_retriever.py` | `crates/search/src/retrievers/triplet_retriever.rs` |
| `cognee/modules/retrieval/graph_completion_retriever.py` | `crates/search/src/retrievers/graph_completion_retriever.rs` |
| `cognee/modules/retrieval/graph_completion_cot_retriever.py` | `crates/search/src/retrievers/advanced_graph_retrievers.rs` |
| `cognee/modules/retrieval/graph_completion_context_extension_retriever.py` | `crates/search/src/retrievers/advanced_graph_retrievers.rs` |
| `cognee/modules/retrieval/graph_summary_completion_retriever.py` | `crates/search/src/retrievers/advanced_graph_retrievers.rs` |
| `cognee/modules/retrieval/cypher_search_retriever.py` | `crates/search/src/retrievers/cypher_nl_retrievers.rs` |
| `cognee/modules/retrieval/natural_language_retriever.py` | `crates/search/src/retrievers/cypher_nl_retrievers.rs` |
| `cognee/modules/retrieval/temporal_retriever.py` | `crates/search/src/retrievers/temporal_retriever.rs` |
| `cognee/modules/retrieval/coding_rules_retriever.py` | `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs` |
| `cognee/modules/retrieval/lexical_retriever.py` + `jaccard_retrival.py` | `crates/search/src/retrievers/lexical_retriever.rs` |
| `cognee/modules/retrieval/utils/brute_force_triplet_search.py` | `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` |
| `cognee/modules/retrieval/utils/completion.py` | `crates/search/src/utils/completion.rs` |
| `cognee/modules/graph/utils/resolve_edges_to_text.py` | `crates/search/src/utils/resolve_edges_to_text.rs` |
| `cognee/modules/graph/cognee_graph/CogneeGraph.py` | `crates/search/src/graph_retrieval/triplet_ranking.rs` |
| `cognee/infrastructure/session/session_manager.py` | `crates/session/src/session_manager.rs` |

## Appendix B: Search Type Compatibility Matrix

| SearchType | Python | Rust | Logic Parity | Prompt Parity | Default Parity | Key Issues | P0/P1 Status |
|------------|--------|------|:-------------|:-------------|:---------------|:-----------|:-------------|
| `SUMMARIES` | Yes | Yes | ~100% | N/A (no LLM) | **Yes** (top_k=5) | | Fixed (task-03) |
| `CHUNKS` | Yes | Yes | ~100% | N/A (no LLM) | **Yes** (top_k=5) | | Fixed (task-03) |
| `RAG_COMPLETION` | Yes | Yes | **~95%** | **Yes** | **Yes** (top_k=1, separator=\n, priority=inline-first) | | Fixed (tasks 03, 07, 13) |
| `TRIPLET_COMPLETION` | Yes | Yes | **~95%** | **Yes** | **Yes** (top_k=5, separator=\n) | | Fixed (tasks 03, 07) |
| `GRAPH_COMPLETION` | Yes | Yes | **~85%** | **Yes** | **Yes** | Remaining: feedback influence, node filtering, batch queries (P2/P3) | Fixed (tasks 01-06) |
| `GRAPH_SUMMARY_COMPLETION` | Yes | Yes | **~85%** | **Yes** | **Yes** | Inherits remaining graph gaps (P2/P3) | Fixed (tasks 01-07) |
| `GRAPH_COMPLETION_COT` | Yes | Yes | **~85%** | **Yes** | **Yes** (max_iter=4, initial+N pattern) | | Fixed (tasks 01-07, 12) |
| `GRAPH_COMPLETION_CONTEXT_EXTENSION` | Yes | Yes | **~85%** | **Yes** | **Yes** (rounds=4, answer-driven) | | Fixed (tasks 01-07, 11) |
| `CYPHER` | Yes | Yes | ~95% | N/A | OK | Both have ALLOW_CYPHER_QUERY | No changes needed |
| `NATURAL_LANGUAGE` | Yes | Yes | **~90%** | **Yes** | OK | Retry loop now catches LLM errors | Fixed (tasks 09, 15) |
| `FEELING_LUCKY` | Yes | Yes | **~90%** | **Yes** | OK | Full 125-line prompt + CODE mapping; Rust has better self-ref guard | Fixed (task-08) |
| `TEMPORAL` | Yes | Yes | **~75%** | **Yes** | **Yes** (top_k=5, penalty=3.5, time injection) | Graph model still differs (property-based vs node-based) | Fixed (tasks 03, 10) |
| `CODING_RULES` | Yes | Yes | ~75% | N/A | OK | Loose rule detection heuristic (P2) | No P0/P1 changes needed |
| `CHUNKS_LEXICAL` | Yes | Yes | **~95%** | N/A | OK | Ranking bug **fixed** | Fixed (task-14) |
| `FEEDBACK` | No | Yes | N/A | N/A | N/A | Rust-only search type | N/A |

**Overall compatibility after P0+P1 implementation: ~85-90%.** (Was ~50-55% before.) Remaining gaps are P2 (feedback influence, node filtering, per-request params, verbose mode, session alignment) and P3 (batch queries, caching, telemetry).

## Appendix C: Verification Corrections Log

The following claims from the initial investigation were corrected during the verification pass:

1. **Cypher `ALLOW_CYPHER_QUERY`:** Initially claimed Python does not have this check. **Corrected:** Python checks it in `get_search_type_retriever_instance.py:230` (at the factory layer, not inside the retriever).
2. **Lexical `top_k`:** Initially claimed Python defaults to 5. **Corrected:** Both Python and Rust default to 10.
3. **Temporal prompt:** Initially described as "2-sentence". **Corrected:** 3 sentences.
4. **Feedback location:** Initially said `SessionManager.detect_feedback()`. **Corrected:** It's a standalone function in `feedback_detection.py`, called from `completion.py`.
5. **`triplet_distance_penalty` semantics:** Initially described as just a different default value. **Corrected:** The semantics are entirely different — Python uses it as a default distance for ALL unmatched elements; Rust uses it as a score deduction only for Triplet-sourced nodes.
6. **Multi-collection score merging:** Not initially documented. **Added:** Python overwrites (last-write-wins), Rust keeps maximum score.
7. **Prompt resolution priority:** Not initially documented. **Added:** Python checks inline first, Rust checks file path first (inverted).
8. **Per-request parameter limitation:** Not initially documented. **Added:** Rust singleton retrievers cannot receive per-request parameter overrides through the trait interface.
9. **CoT iteration semantics:** Initially described as just different default counts. **Corrected:** The iteration flow structure itself differs — Python does initial + N rounds; Rust does N iterations with answer at top.
10. **Context extension strategy:** Initially described correctly but understated. **Expanded:** This is answer-driven expansion (Python) vs query-driven expansion (Rust) — a fundamental philosophy difference.
11. **Lexical retriever ranking bug:** Not initially documented. **Added:** Rust `with_scores=false` breaks top-k ranking because all items get score 0.0.
12. **NL retriever error handling:** Not initially documented. **Added:** Rust `?` on LLM call breaks retry loop; Python catches all exceptions.
13. **Temporal event graph model:** Not initially documented. **Added:** Python uses separate Timestamp graph nodes; Rust uses properties on event nodes.
14. **`triplet_distance_penalty` default value:** Investigation document originally stated Python default is `6.5`. **Corrected during implementation:** The actual Python source uses `3.5` (verified in `brute_force_triplet_search.py` line 143 and `CogneeGraphElements.py` line 27). Implemented as `3.5`.
15. **Edge type lookup key:** Task-01 originally proposed keying edge distances by `edge_type_id` from graph edge properties. **Corrected:** `edge_type_id` is NOT stored in graph edge properties by cognify. Implementation uses `relationship_name` string as the lookup key instead, matching the `EdgeType_relationship_name` vector point metadata.

## Appendix D: Implementation Log

All P0 and P1 tasks were implemented on 2026-04-09. Commits (in order):

| Commit | Task | Description |
|--------|------|-------------|
| `6814f83` | 01 | Fix scoring algorithm: 3-component distance scoring, penalty=3.5, `min` merge |
| `cd5bc86` | 02 | Fix vector collections: add EntityType_name indexing |
| `436c91a` | 03 | Fix default values: top_k, wide_search_top_k, iterations, separators |
| `ca50e05` | 04+06 | Implement Python-compatible context rendering (two-section format) |
| `fc54a00` | 05 | Add graph-specific user prompt template |
| `8e8596d` | 07 | Port core LLM prompts to match Python wording |
| `b8bde90` | 08 | Port full FEELING_LUCKY search type selector prompt |
| `0406037` | 09 | Port full natural language retriever system prompt |
| `e7b0ec3` | 10 | Port temporal interval prompt and fix field names |
| `221d835` | 11 | Clean up context extension loop (answer-driven) |
| `8ea38e2` | 12 | Fix CoT iteration semantics: initial completion before loop |
| `8038b35` | 13 | Fix prompt resolution priority: inline before file path |
| `6ff6b48` | 14 | Fix lexical retriever ranking bug when with_scores=false |
| `b23de0f` | 15 | Fix NL retriever error handling: catch LLM errors in retry loop |
