# Item 5 — Full PostgreSQL-stack E2E test

Parent: [../.internal/cognify-compatibility-implementation-plan.md](../.internal/cognify-compatibility-implementation-plan.md)
Effort: **medium** · Validates [Item 1](01-wire-pggraph-component-manager.md) + [Item 2](02-postgres-graph-credential-fallback.md); hybrid variant validates [Item 3](03-pghybrid-full-adapter.md)
Status: ✅ Implemented

> **Decisions D4 + D5 (resolved):** construct the backends **through
> `ComponentManager`/`Settings`** (exercise the real wiring, not just the
> adapters), and use the **real BGE-Small ONNX model** for embeddings (not
> `MOCK_EMBEDDING`), so similarity/search is exercised for real.

---

## Problem

Each Postgres backend is tested in isolation, but **nothing exercises all three
together**:

- [crates/graph/tests/pg_graph_integration.rs](../../crates/graph/tests/pg_graph_integration.rs) — `PgGraphAdapter` only, gated on `PGGRAPH_TEST_URL`, `#![cfg(feature = "postgres")]`
- [crates/vector/tests/pgvector_integration.rs](../../crates/vector/tests/pgvector_integration.rs) — `PgVectorAdapter` only
- relational Postgres — covered by `cognee-database` migration/schema tests

There is no test that runs `AddPipeline` + `CognifyPipeline` with relational
PostgreSQL **and** `PgGraphAdapter` **and** `PgVectorAdapter` simultaneously, which
is exactly the configuration items 1–3 enable.

---

## Steps

### Step 5.1 — Add a gated test file

Create `crates/cognify/tests/pg_full_stack_e2e.rs`. Gate it so it is skipped
unless the Postgres + LLM environment is present, following the existing skip
conventions (e.g. `integration_fact_extraction.rs` skips without OpenAI creds;
`pg_graph_integration.rs` skips without `PGGRAPH_TEST_URL`).

Gating:

```rust
#![cfg(all(feature = "postgres", /* pgvector + pggraph features */))]
```

Env vars required (skip with a logged message if any are missing):

| Var | Purpose |
|---|---|
| `TEST_POSTGRES_URL` | Single Postgres instance for all three stores |
| `OPENAI_URL` / `OPENAI_TOKEN` | LLM for graph extraction (per project test guide) |
| `COGNEE_E2E_EMBED_MODEL_PATH` / `COGNEE_E2E_TOKENIZER_PATH` | **Required** — real BGE-Small ONNX model + tokenizer (per D5) |

Per **D5**, use the **real BGE-Small-v1.5 ONNX model** (not `MOCK_EMBEDDING`) so
the vector write path *and* similarity search are exercised end-to-end. Reuse the
model-download/caching that `scripts/run_tests_with_openai.sh` already performs
(`COGNEE_TEST_MODEL_DIR`, auto-resolved `COGNEE_E2E_EMBED_MODEL_PATH` /
`COGNEE_E2E_TOKENIZER_PATH`). The graph-extraction step needs a real LLM — reuse
whatever the other cognify integration tests use. Skip cleanly if the model
artifacts or `TEST_POSTGRES_URL` are absent.

### Step 5.2 — Wire the three Postgres backends

Per **D4**, construct **through `ComponentManager`/`Settings`** (not direct
adapter construction): build `Settings` with `db_provider=postgres`,
`graph_database_provider=postgres`, `vector_db_provider=pgvector`, all pointing at
`TEST_POSTGRES_URL`, and let `ComponentManager` initialize all three. This
validates the Item 1 dispatch and Item 2 credential fallback, not just the
adapters in isolation.

Lean on [Item 2](02-postgres-graph-credential-fallback.md): set only the relational `db_*` creds and assert the
graph URL falls back correctly (i.e. do **not** set `graph_database_url`) — this
makes the test double as coverage for the fallback path.

**Hybrid variant (added once [Item 3](03-pghybrid-full-adapter.md) lands):** a second test case sets
`USE_UNIFIED_PROVIDER=pghybrid` and asserts the same end-state through the shared
`PgHybridAdapter` connection.

### Step 5.3 — Run the pipeline and assert

1. `AddPipeline` ingests a small text fixture (reuse a fixture from
   `crates/cognify/tests/test_data/`).
2. `CognifyPipeline` runs end-to-end.
3. Assertions:
   - Graph DB has > 0 nodes and > 0 edges (query via `GraphDBTrait`).
   - Vector DB has the expected collections (`DocumentChunk:text`, `Entity:name`,
     etc.) with > 0 points (query via `VectorDB`).
   - Relational DB has the `Data`/`Dataset` provenance rows.
4. Optionally run one `search` (e.g. `GraphCompletion` or `Chunks`) and assert a
   non-empty result, to cover the read path across all three stores.

### Step 5.4 — Isolation & cleanup

- Use a unique schema or table prefix per run, or `DROP`/truncate in a teardown,
  so repeated runs against a shared Postgres are idempotent. Check how
  `pg_graph_integration.rs` isolates itself and reuse that mechanism.
- Mark `#[serial_test::serial]` if multiple Postgres tests could contend (the
  project already uses `serial_test` for Postgres tests).

### Step 5.5 — CI / docs

- Document the new env vars (`TEST_POSTGRES_URL`) in the root README test section
  and/or the project guide's "Running Integration & E2E Tests" table.
- The test must **skip cleanly** (not fail) in the default CI where Postgres is
  absent. Confirm `scripts/run_tests_with_openai.sh` still passes without
  `TEST_POSTGRES_URL`.

---

## Files touched

- `crates/cognify/tests/pg_full_stack_e2e.rs` (new)
- README / project guide — env-var documentation
- Possibly `crates/cognify/Cargo.toml` — `dev-dependencies` / feature plumbing if
  the test needs `postgres`/`pgvector`/`pggraph` features enabled for the test build

## Acceptance criteria

- With `TEST_POSTGRES_URL` (+ LLM + embedding) set, the test runs the full
  add→cognify (→search) cycle on an all-Postgres stack and all assertions pass.
- Without those env vars the test skips with a clear message and the suite stays
  green.
- Re-running the test against the same Postgres instance is idempotent.

## Risks / notes

- This is the most involved item because it needs a live Postgres + LLM. Keep the
  fixture tiny to bound LLM cost/time.
- Depends on items 1–2 (and 3 if testing via the unified flag). Land it after
  those so it actually exercises the new wiring rather than only the adapters.
- The embedding dimension passed to `PgVectorAdapter::new` must match the
  embedding engine (or the mock's zero-vector dimension) — mismatches surface as
  insert failures.
