# Temporal Tests Implementation Plan

This plan enumerates every temporal test present in the Python cognee SDK that is missing (or only weakly covered) in cognee-rust. Each phase is a self-contained work unit with detailed test scenarios in a linked sub-document.

Python repo assumed at `/tmp/cognee-python` (clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`).

---

## Summary

| Phase | Tests | Type | LLM? | Priority | Status |
|-------|------:|------|------|----------|--------|
| [1. `parse_bound` / `to_cognify_timestamp`](#phase-1) | 20 | Unit (pure functions) | No | High | Not Started |
| [2. `extract_interval`](#phase-2) | 5 | Unit (mocked LLM) | No | High | Not Started |
| [3. `get_context` edge cases](#phase-3) | 6 | Unit (mocked backends) | No | High | Not Started |
| [4. `get_completion`](#phase-4) | 5 | Unit (mocked backends) | No | High | Not Started |
| [5. `rank_temporal_events`](#phase-5) | 5 | Unit (mocked backends) | No | Medium | Not Started |
| [6. `temporal_context_to_text`](#phase-6) | 5 | Unit (pure function) | No | Medium | Not Started |
| [7. Retriever integration](#phase-7) | 7 | Integration (real backends) | Yes | Medium | Not Started |
| [8. `TemporalEventExtractor`](#phase-8) | 6 | Unit (mocked LLM) | No | Medium | Not Started |
| [9. `TemporalEntityEnricher`](#phase-9) | 4 | Unit (mocked LLM) | No | Medium | Not Started |
| [10. Session history](#phase-10) | 2 | Integration (real backends) | Yes | Low | Not Started |
| [11. Cross-SDK E2E](#phase-11) | 4 | Cross-SDK E2E | Yes | Low | Not Started |
| **Total** | **69** | | | | |

**Phases 1-4** (36 tests) are the highest priority -- they test core logic with mocks, run fast, and close the largest gap vs Python (which has ~32 unit tests vs Rust's 2).

**Phases 5-9** (20 tests) cover ranking, formatting, and the cognify extraction modules.

**Phases 10-11** (6 tests) strengthen end-to-end coverage and session integration.

---

<a id="phase-1"></a>
## Phase 1 — `parse_bound` / `to_cognify_timestamp` unit tests

**[Detailed test scenarios](temporal-tests/phase01-parse-bound-timestamp-units.md)** | 20 tests | No LLM required | **Status: Not Started**

Unit tests for pure date-parsing functions that have zero coverage today. Covers `parse_bound()` (13 format variants including RFC-3339, ISO-8601, year-month with leap years, year-only, empty/garbage input), `is_within_interval_ms()` (basic + open-ended bounds), `to_cognify_timestamp()` (happy path, time components, invalid dates, serde defaults), and `QueryInterval::parse()` integration.

**Target files:**
- `crates/search/src/retrievers/temporal_retriever.rs` (inside existing `#[cfg(test)] mod tests`)
- `crates/models/src/temporal_event.rs` (new `#[cfg(test)] mod tests`)

---

<a id="phase-2"></a>
## Phase 2 — `TemporalRetriever` unit tests: `extract_interval`

**[Detailed test scenarios](temporal-tests/phase02-extract-interval-units.md)** | 5 tests | No LLM required | **Status: Not Started**

Tests `extract_interval()` in isolation using the existing `TestLlm` mock. Covers: full interval from LLM, None/None response, LLM failure gracefully returning None, starts_at only, ends_at only.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

**Python reference:** `temporal_retriever_test.py:603-701` (3 tests for `extract_time_from_query`).

---

<a id="phase-3"></a>
## Phase 3 — `TemporalRetriever` unit tests: `get_context` edge cases

**[Detailed test scenarios](temporal-tests/phase03-get-context-edge-cases.md)** | 6 tests | No LLM required | **Status: Not Started**

Rust has only 2 `get_context` tests (full interval match + extraction failure). This phase adds: partial time bounds (time_from only, time_to only), fallback when timestamps exist but no events match the range, empty graph, top_k enforcement, and the critical **2-hop Interval traversal path** (`Event -[during]-> Interval -[from/to]-> Timestamp`).

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

**Python reference:** `temporal_retriever_test.py:178-347` (5 `get_context` tests).

---

<a id="phase-4"></a>
## Phase 4 — `TemporalRetriever` unit tests: `get_completion`

**[Detailed test scenarios](temporal-tests/phase04-get-completion-units.md)** | 5 tests | No LLM required | **Status: Not Started**

Rust has zero `get_completion` tests. This phase covers: text generation from provided context, verifying LLM receives context text, internal `get_context` call when context is None, structured output via `response_schema`, and session history inclusion in messages.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

**Python reference:** `temporal_retriever_test.py:350-600` (5 `get_completion` tests).

---

<a id="phase-5"></a>
## Phase 5 — `TemporalRetriever` unit tests: `rank_temporal_events`

**[Detailed test scenarios](temporal-tests/phase05-rank-temporal-events.md)** | 5 tests | No LLM required | **Status: Not Started**

Tests the score-combining logic that merges graph edge scores and vector similarity scores. Covers: sorting by combined score, events missing from vector results getting high default scores, empty vector results, empty event_ids, and graceful handling of mismatched IDs.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

**Python reference:** `temporal_retriever_test.py:54-148` (5 `filter_top_k_events` tests).

---

<a id="phase-6"></a>
## Phase 6 — `TemporalRetriever` unit tests: `temporal_context_to_text`

**[Detailed test scenarios](temporal-tests/phase06-context-to-text.md)** | 5 tests | No LLM required | **Status: Not Started**

Tests the static formatting function that converts `SearchContext` items into human-readable text for the LLM prompt. Covers: event formatting (`name (time): description`), triplet formatting (`source -[rel]-> target`), empty context, missing fields with defaults, and mixed event + triplet items.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

**Python reference:** `temporal_retriever_test.py:36-116` (2 `descriptions_to_string` tests).

---

<a id="phase-7"></a>
## Phase 7 — `TemporalRetriever` integration tests with pre-populated events

**[Detailed test scenarios](temporal-tests/phase07-retriever-integration.md)** | 7 tests | Requires LLM | **Status: Not Started**

Creates a test fixture with 4 known events pre-populated into real Ladybug + Qdrant backends (ported from Python's `test_temporal_retriever.py` fixture). Tests: time range query, single month query, non-temporal fallback, full completion pipeline, completion fallback, top_k limiting, and multi-event retrieval.

**Target file:** New file `crates/search/tests/temporal_retriever_integration.rs`

**Python reference:** `test_temporal_retriever.py:196-336` (10 integration tests with 4 pre-loaded events).

---

<a id="phase-8"></a>
## Phase 8 — `TemporalEventExtractor` unit tests

**[Detailed test scenarios](temporal-tests/phase08-event-extractor-units.md)** | 6 tests | No LLM required | **Status: Not Started**

The `TemporalEventExtractor` has zero unit tests. Covers: happy path extraction with mock LLM, empty Vec on LLM error, filtering of empty-name events, `convert_raw_event` with point-in-time (`at`), interval (`during`), and invalid timestamps.

**Target file:** `crates/cognify/src/temporal_extraction/event_extractor.rs` (new `#[cfg(test)] mod tests`)

**Python reference:** `test_temporal_graph.py:76-122` (indirect integration coverage).

---

<a id="phase-9"></a>
## Phase 9 — `TemporalEntityEnricher` unit tests

**[Detailed test scenarios](temporal-tests/phase09-entity-enricher-units.md)** | 4 tests | No LLM required | **Status: Not Started**

The `TemporalEntityEnricher` has zero unit tests. Covers: attribute population from LLM response, original events unchanged on LLM error, name-based matching (only matching events enriched), and empty event list.

**Target file:** `crates/cognify/src/temporal_extraction/entity_enricher.rs` (new `#[cfg(test)] mod tests`)

---

<a id="phase-10"></a>
## Phase 10 — Session/conversation history with temporal search

**[Detailed test scenarios](temporal-tests/phase10-session-history.md)** | 2 tests | Requires LLM | **Status: Not Started**

Validates that temporal search results are stored in session history with correct `used_graph_element_ids` shape. Covers: single QA entry storage and multiple queries creating separate history entries.

**Target file:** New file `crates/search/tests/temporal_session.rs`

**Python reference:** `test_conversation_history.py:278-297` (temporal session tracking with shape validation).

---

<a id="phase-11"></a>
## Phase 11 — Strengthen cross-SDK E2E assertions

**[Detailed test scenarios](temporal-tests/phase11-cross-sdk-e2e.md)** | 4 tests | Requires LLM | **Status: Not Started**

Current cross-SDK tests assert `>=1` for node counts (Python asserts `>=10`). This phase raises thresholds to `>=5`, adds edge type validation (`at`/`during` edges), and adds a Python-Rust search parity test.

**Target file:** `e2e-cross-sdk/harness/test_temporal_search.py`

**Python reference:** `test_temporal_graph.py:109-122` (node/edge count assertions).
