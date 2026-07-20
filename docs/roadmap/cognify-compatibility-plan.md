# Implementation Plan: Fully Compatible Cognify Operation (COG-4457)

Status: **mostly implemented** — Items 1, 2, 4, 5 are ✅ landed (see §3 work-items
table); only **Item 3** (full `PgHybridAdapter`, a large standalone milestone) remains
📋 Planned. All decisions are resolved (see Decision log).
Ticket: [COG-4457](https://linear.app/cognee/issue/COG-4457/fully-compatible-cognify-operation)
Branch: `feature/cog-4457-fully-compatible-cognify-operation`

---

## 0. Decision log

Resolved with the user on 2026-06-09. These are authoritative; the sub-documents
reflect them.

| # | Decision | Resolution |
|---|---|---|
| D1 | Where the Postgres graph host comes from (Settings has no `graph_database_host`) | **Add a `graph_database_host` field** to `Settings` + `GRAPH_DATABASE_HOST` env binding for full Python parity. Empty ⇒ fall back to `db_host`. |
| D2 | Scope of Item 4 "custom summarization model" | **Custom summary output *schema*, not a custom LLM.** Python's `summarization_model` is a Pydantic *schema* (default `SummarizedContent`), not a per-stage LLM — Python has no per-stage LLM. Add `summary_schema: Option<serde_json::Value>` + a `set_summarization_model` setter, mirroring Python. The ticket's literal "custom LLM for summarization" wording describes a feature Python does not have. |
| D3 | Scope of Item 3 `pghybrid` | **Full hybrid adapter.** Build a real `PgHybridAdapter` sharing one Postgres connection for graph + vector (true `PostgresHybridAdapter` parity), not just a config shim. Large effort — effectively its own milestone. |
| D4 | Item 5 E2E backend construction | **Through `ComponentManager`/`Settings`** — exercises the new wiring (items 1–3), not just the adapters. |
| D5 | Item 5 embedding source | **Real BGE-Small ONNX model** (`COGNEE_E2E_EMBED_MODEL_PATH`), to test real similarity/search, not mock zero-vectors. |

### Findings surfaced during planning (not blocking, noted for awareness)

- `CognifyConfig.graph_schema` ([config.rs:125](../../crates/cognify/src/config.rs#L125)) is **set but not consumed** by the core cognify graph-extraction task — its only live consumers are dataset-config persistence and the HTTP server. The ticket lists "custom graph schemas" as implemented; in the standalone pipeline path it is effectively a no-op. Item 4 must therefore *actually wire* `summary_schema` rather than copy the `graph_schema` pattern verbatim. A follow-up to wire `graph_schema` into the task may be worth a separate ticket.
- `PgGraphAdapter::new()` runs its own migrations internally (no separate `initialize()` call, unlike `LadybugAdapter`) and also exposes `from_connection(db)` to share an existing SeaORM connection — directly useful for the Item 3 hybrid adapter.
- The `Llm` trait already exposes a dynamic-schema path (`create_structured_output_raw(text, prompt, &Value, opts)` returning a raw `Value`), so Item 4 needs no new LLM API.

---

## 1. Background

The cognify pipeline is, for the most part, already at parity with the Python
`cognee` SDK. The full 5-stage pipeline (classify → chunk → extract graph →
summarize → add data points), summarization with custom prompts, custom graph
schemas, custom chunkers, temporal cognify, re-ingest/update, incremental
loading, the `PgVectorAdapter`, the SQLite + PostgreSQL relational backends,
ontology integration, and memify are **all implemented and tested**.

What remains are the gaps that block a **fully PostgreSQL-backed cognify**
deployment (relational + graph + vector all on Postgres) plus one summarization
ergonomics gap. The `PgGraphAdapter` itself is already fully written
([crates/graph/src/pg_graph_adapter.rs](../../crates/graph/src/pg_graph_adapter.rs),
1,265 lines, feature `postgres`) and `pggraph` is already in the default feature
set of both `cognee` and `cognee-cli` — it is simply **never reachable at
runtime** because `ComponentManager` rejects the `postgres` graph provider.

This document is the index. The one remaining work item (Item 3) has a dedicated
sub-document — [pghybrid-full-adapter.md](pghybrid-full-adapter.md) — with the
step-by-step plan, exact file/line anchors, and acceptance criteria.

---

## 2. Verified current state (2026-06-09)

| Area | State |
|---|---|
| `PgGraphAdapter` implementation | ✅ Exists, complete, feature `postgres`, `new(database_url: &str)` |
| `pggraph` in default features (`lib` + `cli`) | ✅ Already on by default |
| `ComponentManager::init_graph_db()` | ❌ Hard-rejects any provider except `ladybug`/`kuzu` ([component_manager.rs:107-112](../../crates/lib/src/component_manager.rs#L107-L112)) |
| Graph → relational credential fallback | ❌ No `resolved_graph_db_url()`; no fallback to `db_*` fields |
| Unified `pghybrid` mode | ❌ No equivalent of Python `USE_UNIFIED_PROVIDER=pghybrid` |
| Custom summarization **schema** (Python parity, per D2) | ❌ `SummaryExtractor` hardcodes the `SummarizedContent` output type ([extractor.rs:73-95](../../crates/cognify/src/summarization/extractor.rs#L73-L95)); no `summary_schema` / `set_summarization_model` |
| Full Postgres-stack E2E test | ❌ Only isolated `pg_graph_integration.rs` / `pgvector_integration.rs` exist; none combine all three |

---

## 3. Work items

Status legend: 📋 Planned · 🔨 In progress · ✅ Implemented.

Only **Item 3** remains; its plan is the lone surviving sub-document
([pghybrid-full-adapter.md](pghybrid-full-adapter.md)).
The per-item plans and implementation prompts for the landed items (1, 2, 4, 5)
have been removed now that the work is complete — see git history for them.

| # | Item | Effort | Blocking? | Status | Sub-document |
|---|---|---|---|---|---|
| 1 | Wire `PgGraphAdapter` into `ComponentManager` | Small | **Yes** — Postgres graph is unreachable at runtime without it | ✅ Implemented | _(landed; doc removed)_ |
| 2 | Graph → relational credential fallback (+ `graph_database_host`) | Small | No (quality-of-life parity) | ✅ Implemented | _(landed; doc removed)_ |
| 4 | Custom summarization **schema** (`summary_schema` + `set_summarization_model`) | Small–Medium | No | ✅ Implemented | _(landed; doc removed)_ |
| 3 | Full `PgHybridAdapter` + unified-engine wiring | **Large** | No | 📋 Planned | [pghybrid-full-adapter.md](pghybrid-full-adapter.md) |
| 5 | Full PostgreSQL-stack E2E test | Medium | No (validates 1–3) | ✅ Implemented | _(landed; doc removed)_ |

---

## 4. Recommended sequencing

1. **Item 1** first — it is the only blocker and unlocks everything else. Nothing
   downstream can be exercised until the `postgres` graph provider is reachable.
2. **Item 2** next — small, and item 5's test setup is much simpler when graph
   credentials fall back to the relational DB config.
3. **Item 4** is independent of the Postgres work and can land any time
   (small–medium). It does not depend on items 1–3.
4. **Item 5** can land right after items 1+2 — it validates the separate-provider
   Postgres stack (relational + `PgGraphAdapter` + `PgVectorAdapter`) and is the
   acceptance gate for the blocking work.
5. **Item 3** (full hybrid adapter) is the largest piece and effectively its own
   milestone; it builds on items 1+2 and should be tackled last. Item 5 should
   grow a second variant covering the hybrid path once item 3 lands.

Items 1, 2, and 4 are each a self-contained PR-sized change. Item 3 is a large,
multi-PR effort (new adapter implementing two traits + unified-engine concept +
hybrid write/search methods). Item 5 is the integration test that ties it
together.

---

## 5. Definition of done for the ticket

- `cognify` runs end-to-end with `GRAPH_DATABASE_PROVIDER=postgres`,
  `VECTOR_DB_PROVIDER=pgvector`, and `DB_PROVIDER=postgres` against a single
  PostgreSQL instance.
- Graph credentials fall back to the relational DB config when not set
  explicitly, matching Python `get_graph_engine.py:344-367`; a
  `graph_database_host` / `GRAPH_DATABASE_HOST` field exists.
- A custom summarization output schema can be configured via
  `set_summarization_model` / `CognifyConfig::summary_schema`, defaulting to
  `SummarizedContent` (Python parity).
- `USE_UNIFIED_PROVIDER=pghybrid` selects a real `PgHybridAdapter` that shares one
  Postgres connection across graph + vector, matching Python's
  `PostgresHybridAdapter` behavior.
- A gated E2E test exercises the full Postgres stack (separate-provider path now;
  hybrid path once item 3 lands) and asserts non-empty graph + vector output.
- `scripts/check_all.sh` passes.
