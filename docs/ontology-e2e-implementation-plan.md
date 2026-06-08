# Ontology E2E Tests - Full Implementation Plan

Status: proposal / ready to implement  
Owner: TBD  
Last updated: 2026-06-08

## 1. Goal

Deliver end-to-end ontology coverage across:

1. Rust pipeline tests (`add -> cognify -> search`) with a real ontology resolver.
2. Rust HTTP integration tests (`POST /api/v1/ontologies -> POST /api/v1/cognify -> POST /api/v1/search`).
3. Persistence checks proving ontology-enriched graph artifacts survive storage.
4. Multi-ontology behavior checks.
5. Cross-SDK parity tests in `e2e-cross-sdk/` mirroring Python workflow.

This closes the current gap where ontology behavior is tested only at unit/component level, not as a full user-facing workflow.

## 2. Prerequisite Gate (Must Be Completed First)

## Blocker: COG-4687 (ontology support in HTTP wiring/handlers)

Gate status: completed on 2026-06-08.

Readiness evidence:

1. `POST /api/v1/cognify` now resolves payload ontology keys via user-scoped `OntologyManager` lookups and passes a request-scoped resolver into pipeline execution.
2. Unknown ontology keys now return a non-200 ontology error contract (`404` envelope) instead of silently falling back to no-op.
3. Resolver construction is key-scoped (only requested keys) and user-scoped (cross-user lookups fail).

Validation evidence:

1. `cargo test -p cognee-http-server resolve_request_ontology_resolver_ -- --nocapture` passes with the new resolver-scope tests.
2. `cargo check --all-targets` passes.

Before executing this plan, ensure the HTTP server actually constructs and uses per-request ontology resolvers from payload `ontology_key` values.

Required readiness checks:

1. `POST /api/v1/cognify` consumes payload ontology keys and does not silently fall back to no-op resolver.
2. Unknown ontology keys produce the expected HTTP error contract.
3. Resolver construction is user-scoped and key-scoped.

If this gate is not green, implement Step 1 and Step 2 from COG-4687 first.

## 3. Current Coverage (Baseline)

Implemented already:

1. Ontology manager CRUD and validation tests:
   - `crates/ontology/tests/manager_tests.rs`
2. Cognify ontology expansion stage-only roundtrip test:
   - `crates/cognify/tests/ontology_round_trip.rs`
3. No-op resolver behavior:
   - `crates/ontology/tests/noop_test.rs`
4. HTTP ontology route auth/validation checks:
   - `crates/http-server/tests/test_ontologies.rs`

Missing:

1. Full pipeline ontology E2E through ingestion and retrieval.
2. HTTP ontology-enabled cognify and retrieval E2E.
3. Persistence verification for ontology-created graph semantics.
4. Multi-ontology cognify behavior.
5. Cross-SDK parity tests for ontology flow.

## 4. Deliverables

## D1 - Pipeline E2E test (Rust crate level)

Status: completed on 2026-06-08.

Implementation landed:

1. Added `crates/cognify/tests/e2e_ontology_pipeline.rs`.
2. Added ontology fixture `crates/cognify/tests/test_data/ontology/tech_taxonomy.ttl`.
3. Assertions now cover:
   - at least one ontology-valid entity type,
   - ontology-derived `is_a` edges in result and persisted graph,
   - persisted ancestor nodes (`Technology` or `LegalEntity`),
   - ontology-enriched search discoverability (`GraphCompletion`).

Validation evidence:

1. `cargo test -p cognee-cognify --test e2e_ontology_pipeline -- --nocapture` passed.

Create:

- `crates/cognify/tests/e2e_ontology_pipeline.rs`

Purpose:

1. Ingest realistic domain text through `AddPipeline`.
2. Run `cognify` with `RdfLibOntologyResolver` loaded from real `.ttl` fixture(s).
3. Assert graph persistence and ontology-derived structures.
4. Execute search and assert discoverability of ontology-enriched entities.

Core assertions:

1. At least one ontology-matched entity has `ontology_valid=true`.
2. `is_a` edges exist for matched entities.
3. Ancestor nodes injected by ontology exist in persisted graph.
4. Search query returns results referencing ontology-enriched concepts.

Environment:

1. LLM-backed (same gating model as existing E2E tests).
2. Requires OpenAI-compatible env vars used by existing scripts.

## D2 - HTTP integration E2E test (Rust HTTP server level)

Status: completed on 2026-06-08.

Implementation landed:

1. Added `crates/http-server/tests/test_ontology_cognify_search_e2e.rs`.
2. Test flow now covers:
   - ontology upload via `POST /api/v1/ontologies`,
   - blocking cognify with payload `ontologyKey` via `POST /api/v1/cognify`,
   - retrieval check via `POST /api/v1/search`,
   - negative unknown-key contract (`404`) for cognify.

Validation evidence:

1. `cargo test -p cognee-http-server --test test_ontology_cognify_search_e2e -- --nocapture` passed.

Extend or add tests under:

- `crates/http-server/tests/`

Recommended new file:

- `crates/http-server/tests/test_ontology_cognify_search_e2e.rs`

Flow:

1. Authenticated user uploads ontology via `POST /api/v1/ontologies`.
2. Dataset is added/seeded.
3. `POST /api/v1/cognify` with `ontologyKey` (and alias form if needed for compatibility).
4. `POST /api/v1/search` query verifies ontology-enriched retrieval.

Core assertions:

1. Upload response is 200 with expected metadata shape.
2. Cognify run reaches success status.
3. Search results contain expected ontology-influenced entity/type evidence.
4. Negative: unknown ontology key returns expected non-200 contract.

## D3 - Persistence verification coverage

Status: completed on 2026-06-08.

Implementation landed:

1. Added explicit persistence assertions to `crates/http-server/tests/test_ontology_cognify_search_e2e.rs`:
   - persisted graph contains ontology-derived `is_a` edges after cognify,
   - persisted graph contains ontology-expanded ancestor nodes (`Technology` or `LegalEntity`).
2. Strengthened retrieval assertions to ensure search payload contains ontology concepts (`algorithm`, `technology`, or `is_a`).

Validation evidence:

1. `cargo test -p cognee-http-server --test test_ontology_cognify_search_e2e -- --nocapture` passed with persistence assertions enabled.

Add explicit assertions (in D1 and/or D2 tests) to validate persistence, not only transient payload:

1. Graph storage contains ontology-expanded nodes after run completion.
2. Graph storage contains ontology-derived `is_a` edges.
3. Retrieval APIs can discover these persisted artifacts.

Implementation note:

Use graph DB reads and/or formatted graph retrieval APIs used by existing test helpers.

## D4 - Multi-ontology test

Add at least one test that uses multiple ontology keys in one cognify call.

Candidate locations:

1. Rust pipeline test (`crates/cognify/tests/e2e_ontology_pipeline.rs`) as second test case.
2. HTTP integration test file in `crates/http-server/tests/`.

Core assertions:

1. Both ontologies are loaded/applied.
2. Expected combined enrichment is present.
3. No silent override/drop of one key.

## D5 - Cross-SDK parity tests

Add parity scenario in:

- `e2e-cross-sdk/harness/`

Recommended file:

- `e2e-cross-sdk/harness/test_http_ontology.py`

Flow (Python vs Rust):

1. Register/login both backends.
2. Upload ontology on both.
3. Seed dataset text on both.
4. Cognify with ontology key(s) on both.
5. Query search on both.
6. Compare status and semantic outcomes with tolerant matching.

Parity strategy for nondeterministic LLM output:

1. Compare structural invariants, not exact phrasing.
2. Compare presence of expected ontology concepts/labels/relations.
3. Exclude run IDs and timing fields from strict diff.

## 5. Detailed Work Breakdown

## Phase A - Fixtures and helpers

1. Reuse ontology fixtures from:
   - `crates/ontology/tests/fixtures/`
2. Add dedicated test text fixture(s) near new tests if needed.
3. Add helper utilities for:
   - resolver creation from fixture file(s)
   - robust graph assertions
   - tolerant search content checks

Exit criteria:

1. Tests can run without ad hoc inline ontology blobs.
2. Assertions are deterministic where possible.

## Phase B - Pipeline E2E implementation

1. Implement happy-path ontology pipeline test.
2. Implement multi-ontology variant.
3. Implement persistence-focused assertions.

Exit criteria:

1. `cargo test -p cognee-cognify --test e2e_ontology_pipeline` passes in configured env.

## Phase C - HTTP integration implementation

1. Implement upload + cognify + search end-to-end test.
2. Add unknown-key negative test.
3. Add multi-key test if not fully covered in Phase B.

Exit criteria:

1. HTTP ontology E2E tests pass locally.
2. Failures clearly indicate routing/wiring regressions.

## Phase D - Cross-SDK parity implementation

1. Add parity test module in harness.
2. Reuse existing parity helper conventions (`DEFAULT_IGNORE`, tolerant semantic checks).
3. Validate against both backends in docker harness.

Exit criteria:

1. New parity tests pass under e2e harness profile.
2. Differences are explainable and documented (if any intentional divergence remains).

## Phase E - Documentation and CI follow-up

1. Update relevant docs if endpoint behavior/contracts are clarified by tests.
2. Ensure new tests are included in existing scripts or documented execution paths.

Exit criteria:

1. Running instructions are clear.
2. CI impact is explicit (required/gated).

## 6. Test Matrix

## Pipeline layer

1. Single ontology key, happy path.
2. Multiple ontology keys combined.
3. Persistence assertions for nodes and edges.
4. Retrieval assertions for ontology-enriched concepts.

## HTTP layer

1. Upload ontology success.
2. Cognify with ontology key success.
3. Search returns ontology-enriched evidence.
4. Unknown ontology key failure contract.
5. Multiple ontology keys in payload.

## Cross-SDK parity layer

1. Upload/list behavior parity.
2. Cognify with ontology keys parity.
3. Search outcome parity via tolerant semantic checks.

## 7. Definition of Done

All items below must be true:

1. New pipeline E2E ontology test file exists and is passing in configured env.
2. New HTTP ontology-enabled E2E test(s) exist and pass.
3. At least one test proves persisted ontology-derived graph artifacts are discoverable.
4. At least one multi-ontology test passes.
5. Cross-SDK ontology parity test exists in `e2e-cross-sdk/harness/` and passes.
6. No regressions in existing ontology tests.
7. Full repo verification script passes after changes.

## 8. Verification Commands

Run in this order:

```bash
cargo fmt
cargo check --all-targets
cargo test -p cognee-cognify --test e2e_ontology_pipeline
cargo test -p cognee-http-server test_ontology
scripts/check_all.sh
```

Cross-SDK parity run:

```bash
cd e2e-cross-sdk
docker compose up --build
```

If harness runtime is too long for default CI lane, mark ontology parity tests as a dedicated gated job and document trigger conditions.

## 9. Risks and Mitigations

1. LLM nondeterminism causes flaky assertions.
   - Mitigation: assert structural invariants and tolerant semantic markers.
2. Hidden fallback to no-op resolver masks failures.
   - Mitigation: include negative tests and explicit resolver-effect assertions.
3. Multi-ontology merge order ambiguity.
   - Mitigation: assert set-level outcomes instead of strict ordering.
4. Cross-SDK behavior mismatch (Python vs Rust contracts).
   - Mitigation: normalize expected deltas with explicit ignore lists and document intentional differences.

## 10. Out of Scope

1. New ontology model features beyond existing resolver capability.
2. Broad search ranking parity tuning unrelated to ontology support.
3. Unrelated HTTP router refactors.

## 11. Suggested Implementation Sequence

1. Complete COG-4687 prerequisite wiring.
2. Implement D1 (pipeline E2E) first to secure core semantics.
3. Implement D2 and D3 (HTTP E2E + persistence checks).
4. Implement D4 (multi-ontology explicit case if not already complete).
5. Implement D5 (cross-SDK parity).
6. Run full verification and land docs updates.
