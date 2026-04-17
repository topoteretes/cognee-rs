# Temporal Search — Implementation Plan

## Problem

`SearchType::Temporal` is listed in the Rust API but produces wrong results for every query. The cognify pipeline never creates `Event` or `Timestamp` graph nodes — the `temporal_cognify` flag in `CognifyConfig` is silently ignored. As a result the `TemporalRetriever` always falls through to the generic triplet-search fallback, returning results unrelated to time-based reasoning.

## Current State

| Component | Status |
|---|---|
| `SearchType::Temporal` variant | Defined |
| `CognifyConfig::temporal_cognify` flag | Defined — never read by the pipeline |
| `CognifyConfig::data_per_batch` field | Defined — never used |
| `Event` model type | Missing |
| `Timestamp` model type | Missing |
| `Interval` model type | Missing |
| LLM prompts for event/timestamp extraction | Missing |
| LLM prompts for entity enrichment | Missing |
| Temporal cognify pipeline | Missing — Python runs a separate pipeline branch |
| `Event_name` vector collection population | Missing |
| `TemporalRetriever` | Implemented — but searches for node types that the pipeline never creates; uses in-memory full-graph scan instead of typed node queries |

## Key Corrections from Python Reference Audit

Three details differ from what the first draft of this plan assumed:

1. **`time_at` is milliseconds, not seconds.** Python computes `int(datetime.timestamp() * 1000)`. The retriever Cypher queries compare millisecond values. Rust must match.

2. **`during` uses an intermediate `Interval` node.** For range events Python creates `Event -[during]-> Interval -[time_from]-> Timestamp` and `Interval -[time_to]-> Timestamp`. There are no direct `Event -[starts_at/ends_at]-> Timestamp` edges.

3. **Temporal pipeline is a full replacement, not an append.** When `temporal_cognify=True` Python runs a completely different five-stage pipeline (classify → chunk → extract-events → enrich-entities → add-data-points). The standard KG extraction and summarization stages do not run.

## Phases

| # | Title | Status | Detail |
|---|---|---|---|
| 1 | Data Models | Done | [phase-1-data-models.md](temporal/phase-1-data-models.md) |
| 2 | LLM Prompts | Done | [phase-2-llm-prompts.md](temporal/phase-2-llm-prompts.md) |
| 3 | Event Extractor | Done | [phase-3-event-extractor.md](temporal/phase-3-event-extractor.md) |
| 4 | Entity Enrichment | Done | [phase-4-entity-enrichment.md](temporal/phase-4-entity-enrichment.md) |
| 5 | Cognify Pipeline Stages | Done | [phase-5-pipeline-stages.md](temporal/phase-5-pipeline-stages.md) |
| 6 | Temporal Retriever Fixes | Done | [phase-6-retriever-fixes.md](temporal/phase-6-retriever-fixes.md) |
| 7 | Integration Tests | Done | [phase-7-integration-tests.md](temporal/phase-7-integration-tests.md) |
| 8 | E2E Cross-SDK Tests | Done | [phase-8-e2e-tests.md](temporal/phase-8-e2e-tests.md) |

Phases 1–4 are purely additive (new files, new types). Phase 5 adds a new pipeline builder behind the flag — existing behavior is unchanged. Phase 6 removes dead heuristic code from the retriever. Phases 7–8 are tests only.


Each phase ends with `cargo check --all-targets` passing before the next begins.

## Acceptance Criteria

| Criterion | Verified by |
|---|---|
| `temporal_cognify=true` creates ≥ 5 `Event` nodes from a biography text | Phase 7 |
| `temporal_cognify=true` creates ≥ 5 `Timestamp` nodes | Phase 7 |
| Every `Event` node has an `at` or `during` edge to a temporal node | Phase 7 |
| `Event_name` vector collection is non-empty after temporal cognify | Phase 7 |
| `SearchType::Temporal` returns event-anchored results when temporal nodes exist | Phase 7 |
| `SearchType::Temporal` falls back to triplet search when no temporal nodes exist | Phase 7 (existing test preserved)  |
| Rust and Python `Event` node counts within 50% on same input | Phase 8 |
| Rust and Python `Timestamp` node counts within 50% on same input | Phase 8 |
| `scripts/check_all.sh` passes | After Phase 6 |
