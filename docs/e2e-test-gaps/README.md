# E2E Test Gap Analysis: add-cognify-memify-delete Pipeline

**Date:** 2026-04-21
**Scope:** Integration and E2E test coverage for the full `add → cognify → memify → search → delete` lifecycle

---

## Executive Summary

The current test suite has **28 Rust test files** and **6 Python cross-SDK test files** covering ~50 distinct test functions. Coverage is strong for individual pipeline stages, but several important multi-stage and lifecycle scenarios remain untested:

- No test exercises the full 5-stage pipeline (add → cognify → memify → search → delete)
- Delete-after-memify is untested (triplet vector cleanup never verified)
- Shared entity graph-level preservation tested at relational DB level only, not graph/vector
- No cross-SDK delete test exists
- Hard mode delete (orphan entity sweep) has zero test coverage
- Re-add/re-cognify after delete is untested

---

## Gap Register

### New Rust Integration Tests

| ID | Gap | Plan | Status |
|----|-----|------|--------|
| **A** | [Full pipeline with memify + delete](#a-full-pipeline-with-memify-stage) | [plan-A](plan-A-full-pipeline-memify.md) | Done |
| **B** | [Triplet vector cleanup after memify](#b-delete-cleans-up-triplet-vectors-after-memify) | [plan-B](plan-B-triplet-vector-cleanup.md) | Done |
| **C** | [Search after partial delete](#c-search-after-partial-delete) | [plan-C](plan-C-search-after-partial-delete.md) | Done |
| **D** | [Re-add and re-cognify after delete](#d-re-add-and-re-cognify-after-delete) | [plan-D](plan-D-readd-after-delete.md) | Done |
| **E** | [Hard mode delete (orphan sweep)](#e-hard-mode-delete-orphan-entity-sweep) | [plan-E](plan-E-hard-mode-delete.md) | Done |

### Python Test Ports

| ID | Gap | Plan | Status |
|----|-----|------|--------|
| **F/P2** | [`delete_dataset_if_empty` flag](#fp2-delete_dataset_if_empty-flag) | [plan-F/P2](plan-FP2-delete-dataset-if-empty.md) | Done |
| **G/P1** | [Shared entity graph-level delete verification](#gp1-multi-document-shared-entity-preservation-graph-level) | [plan-G/P1](plan-GP1-shared-entity-graph-delete.md) | Done |
| **H/P4** | [AuthorizedDeleteService + ACL integration](#hp4-authorizeddeleteservice-integration) | [plan-H/P4](plan-HP4-acl-delete.md) | Done |
| **I** | [Delete preview accuracy](#i-delete-preview-accuracy) | [plan-I](plan-I-preview-accuracy.md) | Done |
| **P3** | [Delete error paths](#p3-delete-error-paths) | [plan-P3](plan-P3-delete-error-paths.md) | Done |
| **P5** | [Re-cognify after content update](#p5-re-cognify-after-content-update) | [plan-P5](plan-P5-recognify-after-update.md) | Done |
| **P6** | [`last_accessed` update on search](#p6-last_accessed-update-on-search) | [plan-P6](plan-P6-last-accessed-update.md) | Done |

### Cross-SDK E2E Tests

| ID | Gap | Plan | Status |
|----|-----|------|--------|
| **E1** | [Delete parity (both SDKs)](#e1-delete-parity) | [plan-E1](plan-E1-cross-sdk-delete-parity.md) | Done |
| **E2** | [Cross-SDK delete interop](#e2-cross-sdk-delete-interop) | [plan-E2](plan-E2-cross-sdk-delete-interop.md) | Done |
| **E3** | [Shared entity delete parity](#e3-shared-entity-delete-parity) | [plan-E3](plan-E3-cross-sdk-shared-entity-delete.md) | Done |
| **E4** | [Memify + delete (extend memify_search)](#e4-full-pipeline-with-memify--delete) | [plan-E4](plan-E4-cross-sdk-memify-delete.md) | Done |
| **E5** | [Re-add after cross-SDK delete](#e5-re-add-after-cross-sdk-delete) | [plan-E5](plan-E5-cross-sdk-readd-after-delete.md) | Done |

---

## Gap Descriptions

### A. Full pipeline with memify stage

No existing test exercises the complete `add → cognify → memify → search(TripletCompletion) → delete → verify all cleanup` pipeline. The full-pipeline test in `integration_default_backend.rs` skips memify. The `e2e_memify.rs` test exercises memify → search but never deletes. This is the single most impactful missing test.

**Target file:** `crates/cognify/tests/e2e_full_pipeline_memify.rs`
**Backends:** Real (LLM, ONNX, Qdrant, Ladybug, SQLite) | **Env vars:** OpenAI + ONNX model

### B. Delete cleans up triplet vectors after memify

The delete service targets 7 vector collections including `Triplet/text`, but no test verifies that triplet entries created by memify are cleaned up during **data-scope** deletion. Distinct from A because it tests per-document vector point isolation.

**Target file:** `crates/cognify/tests/e2e_triplet_vector_cleanup.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### C. Search after partial delete

No test verifies search correctness after a partial delete. Existing delete tests check artifact counts but never re-invoke search. A stale vector index entry could return phantom results from deleted documents.

**Target file:** `crates/search/tests/search_after_partial_delete.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### D. Re-add and re-cognify after delete

Tests the full lifecycle loop. Pipeline status cleanup during delete must correctly reset state so re-cognify doesn't skip already-processed data. UUID5 determinism means re-added data should get the same `data_id`.

**Target file:** `crates/cognify/tests/e2e_lifecycle_loop.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### E. Hard mode delete (orphan entity sweep)

`DeleteMode::Hard` sweeps degree-1 Entity/EntityType nodes and orphan EdgeType nodes. Fully implemented but has **zero** integration test coverage. All existing delete tests use `Soft` mode only.

**Target file:** `crates/delete/tests/hard_mode_orphan_sweep.rs`
**Backends:** Real (LLM, Ladybug) | **Env vars:** OpenAI + ONNX model

### F/P2. `delete_dataset_if_empty` flag

Gap-10 implemented `DeleteScope::Data { delete_dataset_if_empty }` but there is no dedicated integration test. Port of Python's `test_delete_data_and_dataset_if_empty.py`. Three cases: flag=false (dataset survives), flag=true+empty (auto-delete), flag=true+non-empty (survives).

**Target file:** `crates/lib/tests/dataset_deletion.rs` (extend existing)
**Backends:** MockStorage, in-memory SQLite | **Env vars:** None

### G/P1. Multi-document shared entity preservation (graph-level)

Port of Python's `test_delete_default_graph.py` — the most thorough Python delete test. Rust's `dataset_deletion.rs` tests shared data at the relational DB level only. This test verifies at the **graph DB level**: shared entity node slugs survive while exclusive ones are removed.

**Target file:** `crates/cognify/tests/e2e_shared_entity_graph_delete.rs`
**Backends:** Real (LLM, Ladybug, Qdrant, ONNX) | **Env vars:** OpenAI + ONNX model

### H/P4. AuthorizedDeleteService integration

`AuthorizedDeleteService` wraps `DeleteService` with ACL enforcement. No integration test exercises this wrapper. Port of Python's `test_delete_by_id.py` and `test_delete_two_users_same_dataset.py` permission scenarios. Four tests: denied, granted, preview ACL, cross-user isolation.

**Target file:** `crates/delete/tests/authorized_delete_integration.rs`
**Backends:** in-memory SQLite, MockStorage | **Env vars:** None

### I. Delete preview accuracy

Preview (dry-run) is tested via CLI `--dry-run` but no test verifies that preview counts match actual execution results. Calls preview() then execute() with the same request and asserts all count fields match exactly.

**Target file:** `crates/cognify/tests/e2e_delete_preview_accuracy.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### P3. Delete error paths

No library-level error-path test exists for `DeleteService`. Port of Python's `test_delete_by_id.py` error scenarios: non-existent data_id, non-existent dataset_name, data not linked to specified dataset, user with no datasets.

**Target file:** `crates/delete/tests/delete_error_paths.rs`
**Backends:** in-memory SQLite, MockStorage | **Env vars:** None

### P5. Re-cognify after content update

Port of Python's `test_library.py` update portion. In Rust, "update" means adding new content (different hash → different `data_id`). Test verifies both old and new content are searchable, and that deleting old content removes it from search while preserving new.

**Target file:** `crates/cognify/tests/e2e_recognify_after_update.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### P6. `last_accessed` update on search

Port of Python's `test_delete_edge_cases.py`. `last_accessed` tracking IS implemented in Rust (model field, DB column, `update_node_access_timestamps()` in search orchestrator). Test verifies timestamp is updated within 30 seconds of search, and increases monotonically across searches.

**Target file:** `crates/search/tests/last_accessed_update.rs`
**Backends:** Real | **Env vars:** OpenAI + ONNX model

### E1. Delete parity

Both SDKs independently add+cognify the same text, then delete, then verify cleanup is equivalent. Checks that relational DB (data, datasets, provenance tables) is empty in both.

**Target file:** `e2e-cross-sdk/harness/test_delete_parity.py`
**Env vars:** OpenAI

### E2. Cross-SDK delete interop

One SDK creates data, the other deletes it. **Limited by different graph/vector backends** (Python: Kuzu/LanceDB, Rust: Ladybug/Qdrant). Tests relational DB cross-delete only. DB-copy approach between workspaces.

**Target file:** `e2e-cross-sdk/harness/test_cross_delete.py`
**Env vars:** OpenAI

### E3. Shared entity delete parity

Both SDKs add 2 docs with overlapping entities, delete one, compare remaining provenance tables. Tolerance-based comparison (Jaccard, count ratios) for LLM variability.

**Target file:** `e2e-cross-sdk/harness/test_delete_shared_entity_parity.py`
**Env vars:** OpenAI

### E4. Full pipeline with memify + delete

Extends existing `test_memify_search.py` pattern. Both SDKs: add → cognify → memify → search (non-empty) → delete → search (empty). Verifies triplet cleanup in both SDKs.

**Target file:** `e2e-cross-sdk/harness/test_memify_delete.py`
**Env vars:** OpenAI

### E5. Re-add after cross-SDK delete

One SDK adds, the other deletes, the first re-adds. Asserts UUID5 `data_id` matches original. Also tests pure Rust re-add-after-own-delete. No LLM needed for add-only tests.

**Target file:** `e2e-cross-sdk/harness/test_readd_after_delete.py`
**Env vars:** None (add-only) or OpenAI (with cognify)

---

## Implementation Order

The 17 gaps are ordered into 5 phases. Within each phase items can be implemented in parallel. Later phases depend on patterns and infrastructure established in earlier ones.

### Phase 1 — No-LLM quick wins (establish delete test patterns)

These three tests need only MockStorage + in-memory SQLite — no LLM, no embedding model, no external services. They run in milliseconds and can land immediately.

| # | ID | Gap | Effort | Why first |
|---|-----|-----|--------|-----------|
| 1 | **F/P2** | `delete_dataset_if_empty` flag | Low | Tests gap-10 feature with zero external deps; extends an existing test file so no boilerplate needed |
| 2 | **P3** | Delete error paths | Low | Exercises `DeleteError` variants that no library-level test covers today; four independent sub-tests |
| 3 | **H/P4** | ACL delete (`AuthorizedDeleteService`) | Low | The ACL wrapper is a critical auth boundary with zero test coverage; four independent sub-tests |

**Outcome after Phase 1:** Delete library surface fully tested at the unit/integration level for error handling, flags, and authorization — all without needing CI secrets.

---

### Phase 2 — Core pipeline gaps (highest impact)

These are the highest-value tests. Gap A establishes the full-pipeline test skeleton that B, C, D, E, and I all reuse. Gap G/P1 establishes the multi-document + graph-level verification pattern.

| # | ID | Gap | Effort | Why now |
|---|-----|-----|--------|---------|
| 4 | **A** | Full pipeline: add→cognify→memify→search→delete | Medium | Single most impactful missing test; proves all 5 stages work together; its setup code becomes the template for every later real-backend test |
| 5 | **G/P1** | Shared entity graph-level delete | Medium | Ports the most thorough Python delete test; establishes multi-doc + graph-node-identity assertions reused by E and C |
| 6 | **B** | Triplet vector cleanup (data-scope) | Low | Builds on A's pattern; adds per-document vector point isolation — the memify-specific delete path that no other test covers |

**Outcome after Phase 2:** The full 5-stage pipeline is tested end-to-end. Shared-entity preservation is verified at the graph DB level. Memify artifacts are confirmed to be cleaned up by delete.

---

### Phase 3 — Delete correctness properties

Each test here targets a specific correctness property of the delete system. They reuse the pipeline setup from Phase 2.

| # | ID | Gap | Effort | Why now |
|---|-----|-----|--------|---------|
| 7 | **C** | Search after partial delete | Medium | Verifies search returns no phantom results from deleted docs — uses multi-doc pattern from G/P1 |
| 8 | **E** | Hard mode orphan sweep | Medium | Tests `DeleteMode::Hard` (implemented, zero coverage); uses multi-doc shared-entity pattern from G/P1 |
| 9 | **D** | Re-add and re-cognify after delete | Medium | Lifecycle loop: proves pipeline_status reset and UUID5 ID determinism; builds on A's pipeline setup |
| 10 | **I** | Preview vs execute count accuracy | Low | Lightweight once A's setup exists; asserts preview counts match execute counts field-by-field |

**Outcome after Phase 3:** Every delete mode (soft, hard), every scope (data, dataset), and the re-add lifecycle are tested. Dry-run accuracy is verified. Search correctness post-delete is proven.

---

### Phase 4 — Remaining Rust scenarios

Lower-impact but still valuable coverage. Can be done in any order.

| # | ID | Gap | Effort | Why here |
|---|-----|-----|--------|----------|
| 11 | **P6** | `last_accessed` update on search | Low | Feature is fully implemented; just needs a test to prevent regressions |
| 12 | **P5** | Re-cognify after content update | Medium | Rust "update" = add new content; builds on D's re-add pattern |

**Outcome after Phase 4:** All Rust-side integration gaps are closed.

---

### Phase 5 — Cross-SDK E2E tests

All five require Docker harness changes (at minimum adding a `delete` handler to `helpers.py`). They should be implemented after the Rust-side tests prove the delete paths are solid.

| # | ID | Gap | Effort | Why here |
|---|-----|-----|--------|----------|
| 13 | **E1** | Delete parity (both SDKs) | Medium | Foundation: adds `delete` command to helpers.py — all other cross-SDK delete tests depend on this |
| 14 | **E4** | Memify + delete lifecycle | Medium | Extends existing `test_memify_search.py`; verifies triplet cleanup in both SDKs |
| 15 | **E5** | Re-add after cross-SDK delete | Medium | No LLM needed for add-only variant; validates UUID5 ID recovery across SDKs |
| 16 | **E3** | Shared entity delete parity | High | Tolerance-based comparison; most complex cross-SDK delete test |
| 17 | **E2** | Cross-SDK delete interop | High | Limited by different graph/vector backends; relational-only scope |

**Outcome after Phase 5:** The cross-SDK harness covers delete for the first time. Users migrating between SDKs can trust that delete behaves equivalently.

---

### Summary: effort vs dependencies

```
Phase 1 (no LLM)          Phase 2 (core)           Phase 3 (correctness)
  F/P2 ──┐                  A ──────────┬──────────── C
  P3  ───┤ (parallel)       G/P1 ──┬────┤             E
  H/P4 ──┘                  B ─────┘    ├──────────── D
                                        └──────────── I

Phase 4 (remaining)        Phase 5 (cross-SDK)
  P6  ──┐                   E1 ──┬── E4
  P5 ───┘                        ├── E5
                                  ├── E3
                                  └── E2
```

---

## Current Test Inventory

### Rust Integration Tests

#### Full Pipeline

| File | Stages | Backends | Notable |
|------|--------|----------|---------|
| `crates/cognify/tests/integration_default_backend.rs` | add→cognify→search→delete | Real (all) | Memify skipped |
| `crates/cli/tests/cli_e2e.rs` (30+ tests) | All CLI commands | Real | 4 delete scopes, preview |

#### Delete

| File | Stages | Backends | Notable |
|------|--------|----------|---------|
| `crates/lib/tests/dataset_deletion.rs` | add→delete | Mock+SQLite | Relational-level only |
| `crates/ingestion/tests/integration_deduplication.rs` | add→delete | SQLite/PG | Cascade logic at junction level |

#### Cognify / Memify

| File | Stages | Backends | Notable |
|------|--------|----------|---------|
| `crates/cognify/tests/e2e_memify.rs` | graph→memify→search | Real (ONNX, Qdrant) | No delete |
| `crates/cognify/tests/integration_memify.rs` (7 tests) | graph→memify | Mocks | Idempotency, filters, ID stability |
| `crates/cognify/tests/integration_fact_extraction.rs` | text→LLM→graph | Real LLM | Single/batch/custom prompt |
| `crates/cognify/tests/integration_summarization.rs` | chunks→LLM→summaries | Real LLM | Single/batch/custom prompt |
| `crates/cognify/tests/integration_embeddings.rs` | text→vectors | ONNX, Qdrant | Batching, caching |
| `crates/cognify/tests/temporal_cognify.rs` | text→temporal nodes | Real LLM | Event/Timestamp extraction |
| `crates/cli/tests/cli_memify.rs` (7 tests) | add→cognify→memify (CLI) | Real+mock embed | Filters, batch, output format |

#### Search

| File | Stages | Backends | Notable |
|------|--------|----------|---------|
| `crates/search/tests/integration_search_matrix.rs` | add→cognify→search(9 types) | Real (all) | Comprehensive search type coverage |
| `crates/search/tests/temporal_retriever_integration.rs` | temporal graph→search | Ladybug+LLM | Interval/date filter tests |
| `crates/search/tests/temporal_session.rs` | session→search | FS session | SessionContext propagation |

#### Ingestion

| File | Tests | Backends |
|------|-------|----------|
| `crates/ingestion/tests/integration_deduplication.rs` | 6 sub-tests (SQLite+PG each) | LocalStorage, SQLite/PG |
| `crates/ingestion/tests/tenant_isolation.rs` | Multi-tenant isolation | SQLite |
| `crates/ingestion/tests/dedup_cross_dataset.rs` | Cross-dataset dedup | SQLite |
| `crates/ingestion/tests/python_compat_ids.rs` | UUID5 determinism | — |
| `crates/lib/tests/ingest_pipeline_tests.rs` | Add, dedup, tenant | Mock+SQLite |

### Cross-SDK E2E Tests

| File | Tests | LLM Required | Notable |
|------|-------|-------------|---------|
| `test_add_parity.py` | 9 | No | Deterministic: hash, ID, filename, dedup |
| `test_cognify_structural.py` | 6 | Yes | Tolerance-based node/edge comparison |
| `test_cross_read.py` | — | Yes | One SDK reads other's DB |
| `test_memify_search.py` | 1 | Yes | Memify → TripletCompletion search |
| `test_temporal_search.py` | 8 | Yes | Event/Timestamp nodes, temporal search |
| `test_search_parity.py` | — | Yes | Search result parity |
| **Delete tests** | **0** | — | **Gap — none exist** |

### Coverage Matrix

| Combination | Tested? | Where |
|------------|---------|-------|
| add → cognify | Yes | integration_default_backend, search_matrix, cli_e2e |
| add → cognify → search | Yes | integration_default_backend, search_matrix, cli_e2e |
| add → cognify → search → delete | Yes | integration_default_backend, cli_e2e |
| add → delete | Yes | dataset_deletion, integration_deduplication, cli_e2e |
| cognify → memify | Yes | e2e_memify, integration_memify, cli_memify |
| cognify → memify → search | Yes | e2e_memify (TripletCompletion) |
| **add → cognify → memify → search → delete** | **NO** | Gap A |
| **add → cognify → memify → delete → verify** | **NO** | Gap B |
| **add → cognify → delete → re-add → cognify** | **NO** | Gap D |
| **add → cognify → delete(hard) → verify orphans** | **NO** | Gap E |
| **add → cognify → delete(data) → search** | **NO** | Gap C |
| **multi-doc → delete → verify shared entities (graph)** | **NO** | Gap G/P1 |
