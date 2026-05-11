# Task 06-11 — Docs + CI + gap closure

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: 06-01 through 06-10.
**Blocks**: —

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: all (this is the closeout).

---

## 1. Goal

Land the final closeout work:

1. Update [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) —
   flip the row corresponding to file-based logging from "Not
   implemented" to "Implemented (gap 06)" with a commit-list link.
2. Document the new env-var surface in three places:
   - `README.md` — short "Logging" section under the existing
     configuration table.
   - [`docs/cli/README.md`](../../cli/README.md) — env-var subsection.
   - [`docs/http-server/observability.md`](../../http-server/observability.md)
     — deployment note (with the multi-process rotation warning per
     decision 5).
3. Wire `test_logging_parity.py` (from 06-10) into the same
   `http-parity.yml` workflow that already runs
   `test_provenance_parity.py` — as a new pytest step under the
   same `if: ${{ env.HAS_OPENAI_KEY == 'true' }}` gate (the test
   itself does not need OpenAI, but the harness is currently only
   reachable through that workflow).
4. Write the "Closure summary" section at the bottom of
   `docs/telemetry/06-file-logging-rotation.md` listing every commit
   in landing order, mirroring the gap-05 closure summary's shape.
5. Add a "What the gap delivered" and "Known follow-ups" section
   under the closure summary, again mirroring gap 05.

## 2. Rationale

- Decision 5 produced a sharp edge (multi-process rotation can
  corrupt the shared log file). Documenting it in `README.md` and
  the http-server deployment doc is the only thing standing between
  a future operator and a bug report.
- The gap analysis is the single index telemetry parity is tracked
  against. Flipping the row is what tells the next gap-author that
  06 is closed.
- The closure summary preserves a chronological commit log so
  future readers can `git log <sha>..<sha>` the gap in one go.

## 3. Pre-conditions

- All implementation tasks 06-01 through 06-10 committed.
- The orchestrator can quote every per-task commit SHA from
  sub-agent D's report.
- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) still has
  the file-logging row in its "Not implemented" state.

## 4. Step-by-step

### 4.1 Flip the gap-analysis row

Open [`docs/telemetry/gap-analysis.md`](../gap-analysis.md), find
the row for file-based logging (search for "logging" or
"`PlainFileHandler`"; the exact wording lives in that doc), and
change the status cell from whatever it currently says (e.g.
"Not found" / "❌") to:

```
✅ Implemented in [gap 06](06-file-logging-rotation.md)
```

If the row doesn't exist, add one alphabetically into the relevant
"Logging / observability" sub-table.

### 4.2 README "Logging" section

Append to [`README.md`](../../../README.md), near the existing
"Configuration" / env-var section:

```markdown
### Logging

Cognee writes structured logs to **stdout** and (when a writable
directory is available) to a rotating file under
`~/.cognee/logs/<timestamp>.log`.

| Variable | Default | Purpose |
|---|---|---|
| `COGNEE_LOG_FILE` | `true` | Master toggle (`false`/`0`/`no` disables file logging). |
| `COGNEE_LOGS_DIR` | `~/.cognee/logs` | Log directory. Falls back to `/tmp/cognee_logs` if the primary is unwritable. |
| `COGNEE_LOG_FORMAT` | `plain` | `plain` (Python-compatible text) or `json` (JSON lines). Applies to both stdout and file. |
| `COGNEE_LOG_ROTATION` | `daily` | One of `daily` / `hourly` / `minutely` / `never`. Time-based only; size-based rotation is a future enhancement. |
| `COGNEE_LOG_BACKUP_COUNT` | `5` | Files kept by the active rotation policy. |
| `COGNEE_LOG_MAX_FILES` | `10` | Startup-time cap; older files past this count are removed. |
| `LOG_LEVEL` | `info` | Fallback level when `RUST_LOG` is unset. `RUST_LOG` wins when both are set. |
| `LOG_FILE_NAME` | _(generated)_ | Set automatically by the parent process and inherited by children, so all processes append to one file. |

> **Multi-process warning** — when several cognee processes share a
> log file via `LOG_FILE_NAME`, rotation is not coordinated.
> Concurrent rotation events from multiple processes can corrupt
> the log. If you run sharded workers, give each shard a different
> `COGNEE_LOGS_DIR` (or unset `LOG_FILE_NAME` per shard).
```

### 4.3 `docs/cli/README.md` reference

Append a short subsection that says "see the README's Logging
section for the canonical env-var table" with a relative link, and
add one CLI-specific example:

```bash
COGNEE_LOG_FORMAT=json COGNEE_LOGS_DIR=/var/log/cognee cognee cognify ./data
```

### 4.4 `docs/http-server/observability.md` deployment note

Append (or insert after the existing "Span buffer" section):

```markdown
## File logging

The HTTP server inherits the same file-logging behaviour as the CLI
(see the project README's "Logging" section). When deploying behind
a process supervisor (systemd, supervisord, Docker), prefer setting
`COGNEE_LOGS_DIR` to a host-mounted volume so logs persist across
restarts.

The in-memory `SpanBufferLayer` that powers `/spans` is **not**
mirrored to disk. To archive spans, scrape the `/spans` endpoint or
configure the OTEL exporter (see [`observability.md`](observability.md)).

Multi-process deployments: avoid running multiple HTTP-server
instances with the same `LOG_FILE_NAME` env var. The rotation is
not coordinated across processes and can corrupt the shared file.
Either set distinct `COGNEE_LOGS_DIR` per worker or `unset
LOG_FILE_NAME` in each worker's environment.
```

### 4.5 Wire the cross-SDK test into CI

Edit [`.github/workflows/http-parity.yml`](../../../.github/workflows/http-parity.yml).
Find the existing "Provenance parity (LLM-gated)" step (around
line 152). Add a sibling step immediately after it:

```yaml
- name: Logging parity (no LLM needed)
  if: ${{ env.HAS_OPENAI_KEY == 'true' }}
  env:
    HAS_OPENAI_KEY: ${{ secrets.OPENAI_KEY != '' }}
  run: >-
    docker compose
    -f cognee-rust/e2e-cross-sdk/docker-compose.yml
    run --rm e2e-tests
    pytest -vs /harness/test_logging_parity.py
    --tb=short
```

Note: the `HAS_OPENAI_KEY` gate is kept even though the test does
not need OpenAI, because the entire workflow today is `workflow_dispatch`-only
(see the file header about the alembic migration freeze). When the
workflow is restored to push/PR triggers, this step keeps running
under the same gate — no special-casing needed.

If `HAS_OPENAI_KEY` is the wrong gate for a no-LLM test, switch to
running the step unconditionally (drop the `if:` line). Sub-agent A
verifies the gate's intent against the workflow's current state.

### 4.6 Closure summary

Append a "Closure summary" section at the very bottom of
[`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md),
mirroring the format used in
[`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
"Closure summary":

```markdown
## Closure summary

Gap 06 closed in N commits. The table below lists every commit in
landing order — each sub-task lands as a pair (implementation
commit + sub-doc status flip).

| # | Commit | Subject |
|---|---|---|
| 06-01 | `<SHA>` | telemetry/logging-06-01: add tracing-appender workspace dep + json feature |
| 06-01 | `<SHA>` | telemetry/logging-06-01: mark action item 01 complete |
| 06-02 | `<SHA>` | telemetry/logging-06-02: create cognee-logging crate + LoggingConfig |
| 06-02 | `<SHA>` | telemetry/logging-06-02: mark action item 02 complete |
| ... | ... | ... |
| 06-11 | _(this commit)_ | telemetry/logging-06-11: mark action item 11 complete + close gap 06 |

### What the gap delivered

- New [`cognee-logging`](../../../crates/logging/) workspace crate
  exposing `LoggingConfig::from_env()`, `init_logging`, `LogGuards`,
  and the Python-byte-exact `PythonPlainFormatter`.
- File-based logging with daily time-based rotation, backed by
  `tracing-appender::RollingFileAppender` + non-blocking writer.
- Multi-process `LOG_FILE_NAME` inheritance matching Python's
  parent-writes / children-inherit semantics.
- Broad library-noise suppression as the default filter (`info,
  ort=warn, reqwest=warn, hyper=warn, h2=warn, rustls=warn,
  sqlx=warn, sea_orm=warn, sea_orm_migration=warn, tower_http=warn,
  qdrant_segment=warn, qdrant_shard=warn`) applied when neither
  `RUST_LOG` nor `LOG_LEVEL` is set.
- `LOG_LEVEL` env-var fallback for Python parity, with `RUST_LOG`
  retaining precedence.
- `setup_logging()` entrypoints in Python (PyO3), JS (Neon), and C
  (extern "C") bindings, each idempotent via singleton `LogGuards`.
- Android demo automatically wires
  `COGNEE_LOGS_DIR=/data/local/tmp/cognee/runtime/logs`.
- Cross-SDK parity test
  [`test_logging_parity.py`](../../../e2e-cross-sdk/harness/test_logging_parity.py)
  asserting loose file-presence + strict per-message byte equality.

### Known follow-ups

- **Size-based rotation.** Decision 1 deferred this; `COGNEE_LOG_MAX_BYTES`
  is currently accepted as a documented no-op. A follow-up should
  either implement size-based via `tracing-rolling-file` behind a
  feature flag, or remove the env var.
- **Per-process file isolation.** Decision 5 chose to replicate
  Python's `LOG_FILE_NAME` inheritance (multi-process rotation is
  racy). A future enhancement could opt in to per-PID files via a
  new `COGNEE_LOG_PER_PROCESS=true` toggle.
- **OTel log signals.** This gap added file logging; the OTEL
  bridge (gap 01) handles trace signals. Bridging tracing events
  into OTLP log signals is a separate concern.
- **JSON vs plain format coupling.** Decision 3 coupled stdout +
  file format. A future split (e.g. JSON to file, plain to stdout)
  would help log-shipper deployments.
```

Replace `N` with the actual commit count once the orchestrator has
finished. Replace `<SHA>` placeholders with real SHAs from each
sub-agent D report.

## 5. Verification

```bash
# 1. README + docs render correctly.
test -f README.md && grep -q "COGNEE_LOG_FILE" README.md
test -f docs/cli/README.md && grep -q "COGNEE_LOGS_DIR" docs/cli/README.md
test -f docs/http-server/observability.md && grep -q "File logging" docs/http-server/observability.md

# 2. gap-analysis flipped.
grep -q "gap 06" docs/telemetry/gap-analysis.md

# 3. CI workflow syntax is valid.
# Use the same yamllint command the repo runs (check CI config).
python -c "import yaml; yaml.safe_load(open('.github/workflows/http-parity.yml'))"

# 4. Parent doc has a Closure summary.
grep -q "^## Closure summary" docs/telemetry/06-file-logging-rotation.md

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) — row flip.
- [`README.md`](../../../README.md) — Logging section.
- [`docs/cli/README.md`](../../cli/README.md) — short subsection.
- [`docs/http-server/observability.md`](../../http-server/observability.md)
  — deployment note.
- [`.github/workflows/http-parity.yml`](../../../.github/workflows/http-parity.yml)
  — new pytest step.
- [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md)
  — Closure summary appended.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| README env-var table drifts from `LoggingConfig::from_env()` in future changes | Medium | Add a `// NOTE: keep README "Logging" section in sync with this struct's fields` doc comment on `LoggingConfig` (task 06-02). Future PRs touching the struct will see the reminder. |
| http-parity workflow is dispatch-only today; the new step won't run on PRs | By design — see the workflow header about the alembic migration freeze | When the upstream Python fix lands and push/PR triggers are restored, the step starts running automatically. |
| Multi-process warning is missed by users who don't read deployment docs | Medium | Mirror the warning into a one-shot `tracing::warn!` emitted from `init_logging` *only when* `LOG_FILE_NAME` is read from env (i.e., this process is a child sharing a file). Implementor judges whether to add this; if so, document the deferred decision. |
| Closure summary SHA placeholders are forgotten | Medium | Sub-agent E reads its own report log to populate them; sub-agent A verifies before marking 06-11 complete. |

## 8. Out of scope

- A separate `docs/logging/` directory. The single
  parent-doc + README pair is sufficient for parity with gap 05's
  documentation depth.
- Migration guide for users currently relying on stdout-only logs.
  The new behaviour is additive (file logging is opt-in via
  `COGNEE_LOG_FILE=true` which is the default; users who don't want
  it set `false`). No breaking change to document.
- Restoring the http-parity workflow's push/PR triggers. That's an
  upstream-Python-blocking task tracked in the workflow header.
