# LIB-06-05 — Cleanup `TODO(LIB-06 follow-up)` markers

**Status**: implemented in commit d1b7f75
**Owner**: _unassigned_
**Depends on**: LIB-06-01, LIB-06-02, LIB-06-03, LIB-06-04 (all four convenience functions must route through `execute()` before the TODOs can be honestly removed).
**Blocks**:
- [LIB-06-06 — Tests + closure summary](06-tests-and-closure-summary.md) — closure summary references the final cleanup.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 13 (TODOs survive until this cleanup pass).

---

## 1. Problem statement

Three `TODO(LIB-06 follow-up)` comments survived sub-tasks 01-04 by
design (Decision 13). They mark the historical bypass behaviour the
convenience functions had pre-refactor:

- [`crates/cognify/src/tasks.rs:1762`](../../crates/cognify/src/tasks.rs#L1762)
  — cognify.
- [`crates/cognify/src/memify/pipeline.rs:48`](../../crates/cognify/src/memify/pipeline.rs#L48)
  — memify.
- [`crates/ingestion/src/pipeline.rs:771`](../../crates/ingestion/src/pipeline.rs#L771)
  + [`crates/ingestion/src/pipeline.rs:804`](../../crates/ingestion/src/pipeline.rs#L804)
  — `AddPipeline::add` + `add_with_params`.

This sub-task removes them. **Audit before removing** — the comments
explicitly reference
`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md §3 finding 1`
which is the consumer of the executor-route fix. Confirm the consumer
doc no longer needs the TODOs as breadcrumbs.

## 2. Locked decisions consulted

- **Decision 13** — TODOs are removed only by this cleanup pass.

## 3. Pre-conditions

- LIB-06-01, -02, -03, -04 committed.
- `rg "LIB-06 follow-up" crates/` returns the three known sites
  (cognify, memify, ingestion). If it returns *more* than three, the
  preceding tasks either (a) added new bypass paths (regression — fix
  before this task), or (b) other unrelated LIB-06 work landed (sub-agent
  A surfaces and decides whether to clean those up too).
- `rg "LIB-06" docs/` shows the historical references in
  `docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md` and the
  current LIB-06 docs. Those are documentation, not code, and stay.
- Tests / unrelated comments referencing LIB-06 (e.g.
  `crates/core/tests/pipeline_payload_events.rs:1` and
  `scoped_watcher_payload_persistence.rs:1`) are *not* what this task
  removes — they reference the *consumer* of the payload mechanism, not
  the bypass behaviour. Leave them alone.

## 4. Step-by-step

### 4.1 Audit

```bash
rg "LIB-06 follow-up\|TODO\(LIB-06" crates/
```

Expected output: the three known sites. If anything else surfaces,
investigate and document before removing.

### 4.2 Remove the cognify TODO

Edit [`crates/cognify/src/tasks.rs:1762-1771`](../../crates/cognify/src/tasks.rs#L1762-L1771)
— delete the entire `// TODO(LIB-06 follow-up): ...` comment block
preceding `pub async fn cognify(`. The function still gets a doc-comment
above (the existing `/// Run the complete cognify pipeline ...`); leave
that intact.

### 4.3 Remove the memify TODO

Edit [`crates/cognify/src/memify/pipeline.rs:48-56`](../../crates/cognify/src/memify/pipeline.rs#L48-L56)
— delete the `// TODO(LIB-06 follow-up): ...` block preceding
`pub async fn memify(`.

### 4.4 Remove the ingestion TODOs

Edit [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs):

- Delete the block at lines 771-780 preceding `#[instrument]` on
  `pub async fn add(...)`.
- Delete the block at lines 804-808 preceding `#[instrument]` on
  `pub async fn add_with_params(...)`.

### 4.5 Update the pipeline-payload-mechanism doc

Edit [`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md)
§3 finding 1 (if it exists with that anchor):

- If the finding describes the bypass as a *current* problem, update it
  to "resolved by `docs/telemetry/lib-06-executor-routed-convenience.md`"
  and reference the closure commit (filled in by LIB-06-06).
- If the doc is structured as a historical record, leave it but add a
  closure note.

If the doc anchor has shifted, sub-agent A reports `STATUS:
needs-update` and updates references in the LIB-06 sub-docs accordingly.

### 4.6 Re-audit

```bash
rg "LIB-06 follow-up\|TODO\(LIB-06" crates/
```

Expected: zero matches.

```bash
rg "LIB-06" docs/
```

Expected: only the LIB-06 docs themselves and the http-api-v2 sidecar
doc (with its updated closure-aware text).

## 5. Files modified

- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) —
  remove TODO block.
- [`crates/cognify/src/memify/pipeline.rs`](../../crates/cognify/src/memify/pipeline.rs)
  — remove TODO block.
- [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs)
  — remove two TODO blocks.
- [`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md)
  — update the §3 finding 1 reference (or wherever the bypass is called
  out as a current problem).

## 6. Verification

```bash
# 1. No TODOs remain.
rg "LIB-06 follow-up\|TODO\(LIB-06" crates/

# 2. Workspace still compiles (no comment-load-bearing weirdness).
cargo check --all-targets

# 3. Cargo doc still renders cleanly.
cargo doc --no-deps -p cognee-cognify -p cognee-ingestion

# 4. Full check.
scripts/check_all.sh
```

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Removing the TODO orphans the reference in `docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md` (the doc explicitly links back to the TODO sites) | Medium | Update the doc in the same commit. Sub-agent A's audit catches this. |
| A late-discovered fourth bypass path exists | Low | Sub-agent A's `rg` audit before removal catches it; that fourth site becomes a new sub-task or is added to LIB-06-01/02/03/04 retroactively. |
| The doc-comment above each function relied on the TODO block for context (unlikely — TODOs are *under* the doc-comment, not woven into it) | Low | Visually inspect each removal site; preserve the function-level `///` doc-comment. |

## 8. Out of scope

- Removing the LIB-06 mentions from test file comments
  (`pipeline_payload_events.rs`, `scoped_watcher_payload_persistence.rs`).
  Those reference the *consumer* of the executor route, not the bypass.
- Removing references to LIB-06 from the gap-analysis doc — that's a
  future-work bookkeeping change handled in LIB-06-06's closure summary.
- Removing the `lib-06-pipeline-payload-mechanism.md` doc itself.
