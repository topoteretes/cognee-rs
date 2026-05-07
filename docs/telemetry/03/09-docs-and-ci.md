# Task 03-09 — User docs + CI

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 03-04 — Pipeline lifecycle events](04-pipeline-lifecycle-events.md)
- [Task 03-05 — Task lifecycle events](05-task-lifecycle-events.md)
- [Task 03-06 — `cognee.search EXECUTION STARTED`](06-search-execution-events.md)
- [Task 03-07 — `cognee.api.improve` OTEL span](07-improve-otel-span.md)

**Blocks**: —

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)

---

## 1. Goal

Two deliverables, both small:

1. **Extend the user-facing telemetry doc**
   ([`docs/observability/send_telemetry.md`](../../observability/send_telemetry.md))
   with the new event catalog (pipeline + task + search). Per-event
   table: name, when fired, payload keys.
2. **Verify CI lanes still cover the new emission paths.** The
   gap-02-12 lanes already build & test `cognee-telemetry` in three
   modes (default, `--features telemetry`, `--no-default-features`).
   This task confirms `cognee-core` is similarly covered (since we
   just added a `cognee-telemetry` dep edge there) and adds a lane
   if missing.

No new code, only docs and (potentially) a CI yaml tweak.

## 2. Rationale

User docs are the entry point for anyone who:

- Sees an unfamiliar event name on their analytics dashboard and
  wants to understand it.
- Wants to opt out of telemetry — the existing doc covers
  `TELEMETRY_DISABLED` but does not list every event so users can
  audit.
- Is integrating with the Rust SDK from Python and needs to know
  which events the two emit in common.

CI coverage is structural insurance: gap 02-12 added the
`cognee-telemetry` lanes; the new emission sites in `cognee-core`
exercise those lanes transitively, but a quick audit here catches
any gap.

## 3. Pre-conditions

- Tasks 03-01 through 03-07 merged.
- The user-facing doc
  [`docs/observability/send_telemetry.md`](../../observability/send_telemetry.md)
  exists and follows the format established by gap 02-11.

## 4. Step-by-step

### 4.1 Extend `docs/observability/send_telemetry.md`

Sub-agent A should read the current state of
[`docs/observability/send_telemetry.md`](../../observability/send_telemetry.md)
first — gap 02-11 set the format. Add a new section after the
existing "Event catalog" / "Events emitted today" section (whatever
the existing heading is), titled:

```markdown
## Pipeline + task lifecycle events

Fired automatically by every pipeline run that goes through
`cognee_core::pipeline::execute()`. Mirrors Python's emission from
`run_tasks_with_telemetry.py` and `run_tasks_base.py`. Pipeline-run
events are emitted *next to* the `PipelineWatcher` callbacks, not
through them — the watcher is a structural extension point and is
not part of the analytics surface.

| Event | When fired | Identity | Properties |
|---|---|---|---|
| `Pipeline Run Started` | After `execute()` builds `run_info`, before tasks run. | `user_id` from `PipelineContext.user_id` (else `"sdk"`). | `pipeline_name`, `cognee_version`, `tenant_id` (`"Single User Tenant"` when unset), plus the curated config snapshot — see below. |
| `Pipeline Run Completed` | On the `Ok(...)` arm of `execute()`. | same | same |
| `Pipeline Run Errored` | On both `Err` arms (`Cancelled` and generic `Err`). No error string in the payload. | same | same |
| `${task_type} Task Started` | Once per task, before the first attempt of `call_with_retry`. | same as enclosing run | `task_name` (else `"unknown"`), `cognee_version`, `tenant_id` |
| `${task_type} Task Completed` | Once per task, on the first successful attempt. | same | same |
| `${task_type} Task Errored` | Once per task, after retries are exhausted. No error string. | same | same |

`${task_type}` is one of `Function`, `Coroutine`, `Generator`, or
`Async Generator` — see [`Task::python_task_type()`](../../crates/core/src/task.rs)
for the mapping.

### Curated `Pipeline Run *` config snapshot

The settings dump merged into `Pipeline Run Started/Completed/Errored`
events is a hand-curated allowlist — never the full `Config` struct.
Currently allowed:

- `sdk_runtime` (`"rust"` literal)
- `vector_db_provider`, `graph_db_provider`, `relational_db_provider`
- `llm_provider`, `llm_model`
- `embedding_provider`, `embedding_model`, `embedding_dimensions`
- `chunk_strategy`
- `token_counter` (when the pipeline is built from a `CognifyConfig`)

Adding a field to this allowlist requires a code change in
[`crates/lib/src/config.rs::Config::telemetry_snapshot()`](../../crates/lib/src/config.rs)
and an update to a snapshot test that locks the wire shape. URLs,
credentials, and file paths are intentionally omitted — see
[`docs/telemetry/03/03-settings-snapshot.md`](../telemetry/03/03-settings-snapshot.md).

## Search lifecycle events

| Event | When fired | Identity | Properties |
|---|---|---|---|
| `cognee.search EXECUTION STARTED` | First statement of `SearchOrchestrator::search`, before any work. | `request.user_id` | `cognee_version`, `tenant_id` |
| `cognee.search EXECUTION COMPLETED` | Each `Ok(...)` return path of `SearchOrchestrator::search`. Not fired on errors. | same | same |
```

The exact heading anchor can be tuned to match the doc's existing
structure. Sub-agent B should read the doc end-to-end before editing.

### 4.2 Update the cross-reference table

If the existing `send_telemetry.md` has a table mapping
event-name to source-of-truth (Python file path or Rust file path),
extend it to cover the seven new events. Otherwise, skip.

### 4.3 CI audit

Run `grep -rn "cognee-telemetry\|cognee-core" .github/workflows/`
and confirm:

- The default-features lane covers `cognee-core` builds (already
  does — `cargo check --workspace` is workspace-wide).
- A `--no-default-features` lane exists for at least one
  `cognee-core` test target. If the gap-02-12 lane only exercises
  `cognee-telemetry`, extend it to also build `cognee-core` with
  `--no-default-features`.

If a new lane is needed, add it to the existing
`.github/workflows/ci.yml` (where the gap-02-12 lanes live — the
`Compilation check (no default features)` step at line ~90 and the
`Test (no default features, telemetry crate noop fallback)` step at
line ~180). One extra `cargo check -p cognee-core --no-default-features`
step is sufficient.

> **Recommend skipping the CI tweak unless sub-agent A finds an
> actual gap.** The workspace-wide `cargo check` already covers
> `cognee-core`, and `cognee-core/Cargo.toml`'s `cognee-telemetry`
> dep is unconditional (always-on, with telemetry feature gating
> the body). So feature unification across the workspace will already
> exercise both states.

### 4.4 Closure summary on the parent doc

(Sub-agent E will do this in step 3 of its prompt template, but
documented here for clarity.)

When this task commits, sub-agent E adds a "Closure summary"
section to
[`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
listing every gap-03 commit in landing order — same format as the
gap-02 closure summary.

## 5. Verification

```bash
# 1. Doc renders.
mdbook serve docs/  # or just visually inspect — no mdbook in repo

# 2. Markdown lints clean.
# (No markdown linter is configured; eyeball it.)

# 3. CI yaml syntax valid.
yamllint .github/workflows/*.yml  # if yamllint installed

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/observability/send_telemetry.md`](../../observability/send_telemetry.md)
  — extend with the lifecycle + search events sections.
- (Optional) [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml)
  — add `cognee-core --no-default-features` step if missing.
- [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  — sub-agent E adds the "Closure summary" section.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Doc drifts from code | Medium — every code change should update the doc. | The doc and the integration test (task 03-08) are the two parallel sources of truth. Sub-agent A confirms they agree; if they diverge, treat the test as authoritative. |
| CI lane addition breaks existing pipelines | Low — `cargo check` is fast and additive. | Run the new lane locally before pushing. |
| Closure summary forgotten | Low — sub-agent E's template requires it for task 09. | Orchestrator prompt enforces. |

## 8. Out of scope

- Restructuring the existing `send_telemetry.md` doc.
- Adding tutorials for instrumenting *new* events. The doc is a
  reference; "how to add an event" lives in the per-task sub-docs.
- Moving the lifecycle catalog out of `send_telemetry.md` into a
  dedicated `pipeline_events.md` — track separately if the file
  grows unmanageable; not needed for one new section.

**Status**: implemented in commit bc8ed86
