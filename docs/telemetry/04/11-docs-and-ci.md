# Task 04-11 — Docs + CI updates + gap closure

**Status**: ✅ implemented in commit f78dd1f
**Owner**: _unassigned_
**Depends on**: every prior task in this gap (01–10).
**Blocks**: —

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)

---

## 1. Goal

Land the documentation, CI, and `gap-analysis.md` updates that
formally close gap 04. Specifically:

1. Update [`docs/telemetry/gap-analysis.md`](../gap-analysis.md):
   - Move the "LLM / DB Span Coverage" section from "open gap" to
     "Completed work" (mirroring how gap 01 was closed).
   - Update section 4 of the gap-analysis to reflect that
     `crates/utils/src/tracing_keys.rs` is now the single source of
     truth and `crates/search/src/observability.rs` is a re-export.
   - Update the "Detailed Inventory — Rust Side" table to mark
     `cognee.db.system`, `cognee.db.query`, `cognee.db.row_count`,
     `cognee.llm.provider` as **used** (no longer "defined and unused").
2. Update [`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md)
   to document the new `cognee.db.{vector,graph,relational}.*` and
   `cognee.llm.*` span names + attributes for OTLP consumers.
3. Update [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md):
   - Flip the Action items table rows to ✅ with commit SHAs.
   - Add a "Closure summary" section at the bottom listing every
     commit in landing order (mirroring gap 03's closure summary).
4. Update CI:
   - Confirm `.github/workflows/ci.yml` picks up the new test files
     automatically (cargo test --workspace already does).
   - Add a `cognee-database --features postgres` lane if it isn't
     already running, so the relational ops tests exercise both
     SQLite and Postgres.
   - Confirm the binding-check steps inside `.github/workflows/ci.yml`
     (capi/js/python) still pass — bindings are unaffected by adapter
     spans, but `cargo build` may pull in `regex` differently across
     the workspace.

## 2. Rationale

The gap analysis is the index that future maintainers consult to
understand what's still open. Leaving section 4 marked
"defined and unused" after this gap closes would mislead anyone
working on follow-on telemetry work. The closure summary at the
bottom of [`04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
is a stable record of which commits closed which sub-task — same
shape as gap 03's closure summary.

The user-facing docs in [`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md)
need the new attribute names so operators wiring up Tempo / Honeycomb
/ Dash0 can write span filters against the canonical attribute set.

CI is a smoke step — the new test files are picked up by
`cargo test --workspace` automatically, and the existing CI lane
should pass with no changes. The pg-feature lane is the only
genuinely new addition.

## 3. Pre-conditions

- All tasks 04-01 through 04-10 are complete and committed.
- `scripts/check_all.sh` is green on the current `main`.
- The implementor has the per-task SHA list from sub-agent D.

## 4. Step-by-step

### 4.1 Update `gap-analysis.md`

Edit [`docs/telemetry/gap-analysis.md`](../gap-analysis.md):

#### 4.1.1 Section 4 — "LLM / DB Span Coverage"

Change the section header to indicate completion. Replace the
"Rust status" sub-block with a short paragraph noting closure and
linking to the gap-04 doc:

```markdown
## 4. LLM / DB Span Coverage — closed by gap 04

Python instruments the active graph adapter (Neo4j or Ladybug) and
the LanceDB vector adapter via `new_span("cognee.db.{graph,vector}.*")`
with `cognee.db.system` / `cognee.db.query` / `cognee.db.row_count`
attributes. LLM adapters add `cognee.llm.{model,provider}` to their
surrounding span.

**Rust:** closed in [`04-db-adapter-instrumentation.md`](04-db-adapter-instrumentation.md)
— see commits `<list>`. Spans are emitted by `QdrantAdapter`,
`LadybugAdapter`, `OpenAIAdapter` (host), `LiteRtAdapter` (Android),
`PgVectorAdapter`, and every public function in
`crates/database/src/ops/*.rs`. Per-method `pg_graph_adapter` spans
are deferred (see 04-08 sub-doc note). The `redact_secrets` helper now
lives at [`cognee_utils::redact::redact`](../../crates/utils/src/redact.rs)
so adapter crates can call it without depending on
`cognee-http-server`. Constants are consolidated under
[`cognee_utils::tracing_keys`](../../crates/utils/src/tracing_keys.rs);
[`cognee_search::observability`](../../crates/search/src/observability.rs)
is a re-export for backwards compatibility.
```

#### 4.1.2 "Detailed Inventory — Rust Side" — semantic-attribute constants

Update the bullets:

```diff
- These mirror Python's namespaces but several (`cognee.db.system`, `cognee.db.query`, `cognee.db.row_count`, `cognee.llm.provider`) are defined and unused.
+ These mirror Python's namespaces. Every key is now consumed by at least one call site after gap 04 closure.
```

#### 4.1.3 "Prioritized Gap List"

Move gap 04 from the open list to "Completed work" (same format as
the gap 01 entry):

```markdown
- ✅ **Instrument DB / LLM adapters with spans + attributes** — Qdrant,
  Ladybug, pgvector, pg_graph, SeaORM ops, OpenAI, LiteRT now emit
  `cognee.db.{vector,graph,relational}.*` and `cognee.llm.*` spans.
  Redaction helper relocated to `cognee-utils`. Constants
  consolidated. → [04-db-adapter-instrumentation.md](04-db-adapter-instrumentation.md)
  (complete — see commits `<sha-01>..<sha-11>`).
```

### 4.2 Update `docs/observability/opentelemetry.md`

Append a section documenting the canonical span names and attributes
introduced by gap 04. Suggested addition (place after the existing
"What spans does cognee emit?" section if it exists, otherwise add
the section):

````markdown
## Adapter span reference

After gap 04, every database / vector / LLM call goes through a
named tracing span. Operators can filter on these in Tempo / Honeycomb
/ any OTLP consumer.

### Vector adapters

| Span name | System values | Attributes |
|---|---|---|
| `cognee.db.vector.search` | `qdrant`, `pgvector` | `cognee.vector.collection`, `cognee.vector.result_count` |
| `cognee.db.vector.upsert` | `qdrant`, `pgvector` | `cognee.vector.collection`, `cognee.db.row_count` |
| `cognee.db.vector.delete` | `qdrant`, `pgvector` | `cognee.vector.collection`, `cognee.db.row_count` |
| `cognee.db.vector.delete_collection` | `qdrant`, `pgvector` | `cognee.vector.collection` |

### Graph adapters

| Span name | System values | Attributes |
|---|---|---|
| `cognee.db.graph.query` | `ladybug`, `postgres` | `cognee.db.query` (truncated 500 chars + redacted), `cognee.db.row_count` |

### Relational ops

Span names: `cognee.db.relational.<file_stem>.<fn_name>` — one per
public function in `crates/database/src/ops/*.rs`. Attributes:
`cognee.db.system` (`sqlite` / `postgres` / `mysql`),
`cognee.db.row_count` (when the function returns a `Vec<_>`).

### LLM adapters

| Span name | Provider values | Attributes |
|---|---|---|
| `llm.api_call` | `openai` | `cognee.llm.model`, `cognee.llm.provider`, `url` |
| `llm.transcription_api_call` | `openai` | same as `llm.api_call` (model from `transcription_model`) |
| `llm.litert_call` | `litert` (Android only) | `cognee.llm.model`, `cognee.llm.provider` |
| `llm.litert_structured_call` | `litert` | same as `llm.litert_call` |

### Sample query — Tempo / Grafana

```logql
{cognee.db.system="ladybug"} | json | cognee.db.row_count > 0
```

### Redaction

`cognee.db.query` values are passed through `cognee_utils::redact::redact`
before recording. The four-pattern set masks OpenAI-style keys
(`sk-…`), generic `api_key=`, `Bearer …` tokens, and `password=`
assignments. The first 6 characters of each match survive (so
`sk-AAA***REDACTED***` is still distinguishable in traces).
````

If [`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md)
does not yet exist (gap-analysis.md notes it as the canonical
operator reference), create it now. Otherwise extend it.

### 4.3 Flip Action items + add Closure summary in parent doc

Edit [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md):

- Find the "Action items" table at the bottom of the doc.
- For each row, replace the placeholder `Status: ⬜` with `✅ <SHA>`
  (sub-agent E does this incrementally per task; this task only
  fills in the final 04-10 / 04-11 rows).
- Update row 09 of the parent doc's Action items table — the
  original estimate of "~80 functions across 14 files" is stale.
  Replace it with the actually-landed numbers: ~93 functions across
  13 ops files (the `mod.rs` and helper-only file in
  `crates/database/src/ops/` are not ops).
- Append a "Closure summary" section at the bottom, listing every
  commit in landing order. Format mirrors gap 03's closure summary.

```markdown
## Closure summary

Gap 04 closed in 11 commits between <date> and <date>:

| Commit | Task | Subject |
|---|---|---|
| `<sha-01>` | 04-01 | telemetry/db-spans-04-01: relocate redact() to cognee-utils |
| `<sha-02>` | 04-02 | telemetry/db-spans-04-02: deduplicate cognee.* tracing-key constants |
| `<sha-03>` | 04-03 | telemetry/db-spans-04-03: SpanCapture test helper in cognee-test-utils |
| `<sha-04>` | 04-04 | telemetry/db-spans-04-04: instrument QdrantAdapter |
| `<sha-05>` | 04-05 | telemetry/db-spans-04-05: instrument LadybugAdapter::execute_query |
| `<sha-06>` | 04-06 | telemetry/db-spans-04-06: cognee.llm.{model,provider} on OpenAIAdapter |
| `<sha-07>` | 04-07 | telemetry/db-spans-04-07: cognee.llm.{model,provider} on LiteRtAdapter |
| `<sha-08>` | 04-08 | telemetry/db-spans-04-08: instrument pgvector + pg_graph adapters |
| `<sha-09>` | 04-09 | telemetry/db-spans-04-09: ops-level instrumentation for relational layer |
| `<sha-10>` | 04-10 | telemetry/db-spans-04-10: adapter span integration tests |
| `<sha-11>` | 04-11 | telemetry/db-spans-04-11: docs + CI + gap closure |

### What the gap delivered

- 5 adapter crates instrumented (vector × 2, graph × 2, llm × 1
  with OpenAI host, LiteRT Android).
- ~93 ops-level spans across 13 ops files in `crates/database/src/ops/`.
- `cognee_utils::redact::redact` now reachable from any adapter
  crate.
- `cognee_utils::tracing_keys::*` is the single source of truth for
  semantic attribute keys.
- `cognee_test_utils::SpanCapture` lets every adapter integration
  test assert on structured field values.
- 6 new integration test files (~30 individual `#[tokio::test]`
  functions).
- Operator docs in `docs/observability/opentelemetry.md` document
  every span name + attribute.

### Known follow-ups

- **Cross-SDK OTEL parity test** — already listed under
  [`gap-analysis.md` "Future work"](gap-analysis.md#future-work--out-of-scope).
- **Per-method `pg_graph_adapter` spans** — locked decision 1 left
  these out (~22 methods); revisit if cloud users ask for higher
  granularity.
- **LiteRT on-device span tests** — not part of host CI; future
  Android-runner work.
```

### 4.4 Update CI

Edit [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml).

The new test files (`*_span_instrumentation.rs`) are picked up by
`cargo test --workspace` automatically. **Verify** that the
existing job already runs `cargo test --workspace` (it should — see
gap-03 closure for prior art).

If a separate Postgres lane does not already exist, add one:

```yaml
relational-postgres-spans:
  runs-on: ubuntu-latest
  services:
    postgres:
      image: postgres:16
      env:
        POSTGRES_USER: postgres
        POSTGRES_PASSWORD: postgres
        POSTGRES_DB: cognee_test
      ports: [5432:5432]
      options: >-
        --health-cmd "pg_isready -U postgres"
        --health-interval 10s
        --health-timeout 5s
        --health-retries 5
  env:
    DB_PROVIDER: postgres
    DB_HOST: localhost
    DB_PORT: "5432"
    DB_NAME: cognee_test
    DB_USERNAME: postgres
    DB_PASSWORD: postgres
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo test -p cognee-vector --test pgvector_span_instrumentation
    - run: cargo test -p cognee-graph --test pg_graph_span_instrumentation
```

If a Postgres lane already exists for other tests, just add the
two `cargo test` invocations there — don't duplicate the service
container.

(Note: gap 04 does not introduce a `pg_graph_span_instrumentation`
test file because per-method `PgGraphAdapter` spans were deferred in
04-08; drop that line if no such test exists yet.)

### 4.5 Smoke pass

```bash
scripts/check_all.sh
cargo test -p cognee-utils redact::
cargo test -p cognee-test-utils span_capture
cargo test -p cognee-vector --test qdrant_span_instrumentation
cargo test -p cognee-graph --test ladybug_span_instrumentation
cargo test -p cognee-llm --test openai_span_instrumentation
cargo test -p cognee-database --test relational_ops_span_instrumentation
```

All must pass on the host lane before this task commits.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Doc generation (catches rustdoc syntax errors).
cargo doc --workspace --no-deps

# 3. Markdown links — sanity-grep for broken doc references.
grep -rn '04-db-adapter-instrumentation.md' docs/ | grep -v ':#' | head

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) — section 4
  rewrite + completed-work entry.
- [`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md)
  — append the "Adapter span reference" section (or create the file
  if it does not exist yet).
- [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
  — Action items column flip + Closure summary.
- [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml)
  — Postgres span-instrumentation lane.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `docs/observability/opentelemetry.md` does not exist yet | Possible — gap-analysis.md mentions it as canonical but the file may be a stub. | If absent, create it with the bare scaffolding shown in 4.2. |
| Postgres CI lane is flaky on `ci.yml` | Real for any service-container job. | Use the same health-check pattern as the existing pgvector tests (see `ci.yml` history). |
| The closure summary lists wrong SHAs because tasks were rebased | Possible if any task was amended after committing. | Sub-agent E reads `git log` directly at task time rather than copying from the orchestrator's per-task report. |
| Markdown links use the wrong relative path after the parent doc moves | Low — the docs/ tree is stable. | The grep in 5 catches obvious breakage. |

## 8. Out of scope

- Cross-SDK OTEL parity tests (future work, see
  [`gap-analysis.md` "Future work"](../gap-analysis.md#future-work--out-of-scope)).
- Adding metrics export (separate gap, listed in
  [`gap-analysis.md` "Future work"](../gap-analysis.md#future-work--out-of-scope)).
- Auto-init telemetry in bindings — that is gap 07
  ([`07-bindings-auto-init.md`](../07-bindings-auto-init.md)).
- Removing `crates/search/src/observability.rs`. Locked decision 7 +
  04-02 keep it as a re-export shim; full removal would force a
  rename PR across every search call site.
