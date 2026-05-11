# Task 07-08 — Docs and CI for gap 07 closure

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: tasks 07-01 through 07-07.
**Blocks**: —

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: all (this task documents the surface the previous tasks shipped and writes the closure summary).

---

## 1. Goal

Close gap 07 by:

1. Updating
   [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) §6 to point
   at gap 07 closure (status: ✅ Implemented).
2. Adding a "Binding init matrix" section to each binding's README:
   - [`python/README.md`](../../../python/README.md) (or `pyproject.toml`
     long_description block — verify existing layout).
   - [`js/README.md`](../../../js/README.md).
   - [`capi/README.md`](../../../capi/README.md).
3. Adding a CI lane (or extending an existing one) that runs the
   new Python and Neon smoke tests on push/PR.
4. Writing the "Closure summary" section at the bottom of
   [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md).

## 2. Rationale

- Gap-analysis update keeps the top-level status table accurate so
  future contributors don't relitigate the gap.
- README per-binding documents the four-step init matrix
  (`setup_logging` / `setup_telemetry` / `setup_telemetry_analytics`
  / `COGNEE_BINDING_SUPPRESS_LOGS`) for hosts.
- CI catches regressions in the bridge / panic hook / policy logic
  on every push without needing to run the full
  `scripts/check_all.sh` suite manually.
- Closure summary is the contract that gap-06 followed; replicating
  it here preserves the audit trail.

## 3. Pre-conditions

- All preceding tasks committed.
- Existing CI workflows live under
  [`.github/workflows/`](../../../.github/workflows/).
- Each binding has at least one README that ships with the package.

## 4. Step-by-step

### 4.1 Update `docs/telemetry/gap-analysis.md`

Locate §6 (Bindings) — line ~100 in the file (verify). Replace the
"do not initialize tracing at all" sentence with:

```markdown
## 6. Bindings (capi/python/js/android)

Python SDK auto-initializes telemetry on import. ✅ **Implemented
in [gap 07](07-bindings-auto-init.md)** — Rust bindings now ship
auto-init for the default tracing bridge plus explicit
`setup_logging()` (gap 06), `setup_telemetry()` (gap 07),
`setup_telemetry_analytics()` (gap 07) entrypoints. PyO3 bridges
into Python's `logging` via `pyo3-log`; Neon writes a stderr fmt
subscriber by default; C API stays fully explicit (with a panic
hook installed by `cg_init` for FFI debuggability). Auto-init can
be suppressed via `COGNEE_BINDING_SUPPRESS_LOGS=1`.
```

### 4.2 Binding READMEs

For each binding, add a "Initialisation" section with the matrix
below. The text is the same across all three with binding-specific
function names; the differences are flagged inline.

**Template** (adjust function names per binding):

````markdown
## Initialisation

cognee's Rust core uses `tracing` for structured diagnostics and
optionally exports spans via OpenTelemetry (OTLP). When the binding
is loaded, it installs a minimal default subscriber so events are
never silently dropped:

| Binding | Default subscriber on import |
|---|---|
| Python (`cognee_pipeline`) | `pyo3-log` bridge — events route into Python's `logging` module |
| Node.js (`cognee-neon`) | `tracing-subscriber::fmt` writing to stderr |
| C (`cognee-capi`) | None — install via `cognee_setup_logging()` |

### Opt-out

Set `COGNEE_BINDING_SUPPRESS_LOGS=1` before importing/require'ing
the binding to skip the default subscriber. The host then owns
subscriber setup.

### Optional upgrades

| Call | Effect | Idempotent |
|---|---|---|
| `setup_logging()` (gap 06) | Adds the rotating file appender (default `~/.cognee/logs/<ts>.log`, daily rotation, configurable via `COGNEE_LOG_*`). | Yes |
| `setup_telemetry()` (gap 07) | Composes an OTLP exporter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set; reads all standard `OTEL_*` env vars; defaults `service.name` to `cognee.<binding>-binding`. | Yes |
| `setup_telemetry_analytics()` (gap 07) | Arms product-analytics emission (`https://test.prometh.ai`) per the binding's default policy (see table below). Returns `True`/`true` if armed. | Yes |

### Analytics defaults

| Binding | Default | Opt-in / out |
|---|---|---|
| Python | OFF | Set `COGNEE_RUST_TELEMETRY=1` to opt in. Suppressed by `COGNEE_HOST_SDK=<any non-empty>`. |
| Node.js | ON | Set `TELEMETRY_DISABLED=1`, `ENV=test`, `ENV=dev`, or `COGNEE_HOST_SDK=<any non-empty>` to opt out. |
| C | Explicit | `cognee_init_telemetry()` must be called; returns 1 if `TELEMETRY_DISABLED` / `ENV=test`/`dev` / `COGNEE_HOST_SDK` suppresses. |

**Important — Python users embedding via upstream `cognee` SDK:**
do not set `COGNEE_RUST_TELEMETRY=1`. The upstream Python SDK is
the canonical sender of `send_telemetry` events; the Rust binding
defers to it via the `COGNEE_HOST_SDK=python` sentinel.

### Panic visibility (C only)

`cg_init()` installs a one-shot `std::panic::set_hook` that writes
`[cognee-capi panic] <message> at <file:line:col>` to stderr.
Replace it via `std::panic::set_hook` from your own Rust glue if
you need chained or routed handling.
````

### 4.3 CI lane

Extend `.github/workflows/lib-tests.yml` (or add a new
`bindings-tests.yml`) with two jobs:

```yaml
python-bindings:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-python@v5
      with: { python-version: '3.12' }
    - uses: dtolnay/rust-toolchain@stable
    - name: Build wheel
      run: bash python/scripts/check.sh
    - name: Install wheel + run gap-07 tests
      run: |
        pip install maturin
        cd python && maturin develop --release
        pytest tests/test_pyo3_log_bridge.py \
               tests/test_setup_telemetry_idempotent.py \
               tests/test_setup_telemetry_analytics.py -v

js-bindings:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-node@v4
      with: { node-version: '20' }
    - uses: dtolnay/rust-toolchain@stable
    - name: Build addon
      run: bash js/scripts/check.sh
    - name: Run gap-07 tests
      run: |
        cd js && npm install && npm test -- default_subscriber setup_telemetry setup_telemetry_analytics
```

If `python/scripts/check.sh` and `js/scripts/check.sh` already exist
(they do, see [`python/scripts/check.sh`](../../../python/scripts/check.sh)
and [`js/scripts/check.sh`](../../../js/scripts/check.sh)) and run
the right test commands, prefer extending those scripts over
adding jobs — keeps CI definition compact.

### 4.4 Closure summary

After the orchestrator commits all eight tasks, append to
[`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md):

```markdown
---

## Closure summary

Gap 07 closed in N commits. The table below lists every commit in
landing order — each sub-task lands as a pair (implementation
commit + sub-doc status flip), following the gap-06 convention.

| # | Commit | Subject |
|---|---|---|
| 07-00 | `<sha>` | telemetry/bindings-07-00: add gap-07 design decisions and implementation runbook |
| 07-01 | `<sha>` | telemetry/bindings-07-01: add pyo3-log/tracing-log + observability/telemetry deps to bindings |
| 07-01 | `<sha>` | telemetry/bindings-07-01: mark action item 01 complete |
| ... | ... | ... |

### What the gap delivered

- Default `tracing` subscriber per binding (PyO3 → `pyo3-log`,
  Neon → stderr fmt) installed automatically on module load,
  suppressed by `COGNEE_BINDING_SUPPRESS_LOGS=1`.
- New `setup_telemetry()` (Python/JS) + `cognee_init_otlp()` (C)
  entrypoints composing `cognee_observability::init_telemetry`
  with binding-specific `OTEL_SERVICE_NAME` defaults.
- New `setup_telemetry_analytics()` (Python/JS) +
  `cognee_init_telemetry()` (C) entrypoints implementing the
  per-binding default policy from decision 11.
- `COGNEE_HOST_SDK` sentinel honoured by `cognee_telemetry::env::is_disabled`
  only when a binding has explicitly armed emission (decision 10).
- `cg_init` panic hook for FFI debuggability.
- Cross-SDK no-double-emit harness wired (skipped pending a future
  gap that surfaces `cognee_lib::api::*` through bindings).

### Known follow-ups

- **C-side reload-capable subscriber.** Task 07-05 documented the
  v1 limitation: the C binding's OTLP layer is built but not
  composed into a `tracing::Subscriber`. The OpenTelemetry SDK's
  `TracerProvider` still works, but a follow-up should add a
  reload-capable C subscriber for parity with PyO3/Neon.
- **JS callback bridge (parent-doc Option B).** Decision 7
  deferred this.
- **Binding emission of `send_telemetry`.** Decision 4 landed the
  policy and plumbing; surfacing `cognee_lib::api::*` through
  bindings is a separate gap.
- **`BINDING_ARMED` reset for tests.** Sub-doc 07-06 §4.1 added a
  `#[cfg(test)] reset_binding_armed()` helper; non-test code has
  no way to disarm. No host has requested it.
```

(The exact SHAs and the `N` count are filled in by sub-agent E when
the loop completes.)

## 5. Verification

```bash
# 1. Gap-analysis edit doesn't break markdown.
markdownlint docs/telemetry/gap-analysis.md

# 2. CI workflow YAML parses.
yamllint .github/workflows/

# 3. Each binding README renders.
markdownlint python/README.md js/README.md capi/README.md

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) — §6 status flip.
- [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md) —
  "Closure summary" section.
- [`python/README.md`](../../../python/README.md) — Initialisation section.
- [`js/README.md`](../../../js/README.md) — Initialisation section.
- [`capi/README.md`](../../../capi/README.md) — Initialisation section.
- `.github/workflows/lib-tests.yml` (or new `bindings-tests.yml`) — jobs.
- Optionally [`python/scripts/check.sh`](../../../python/scripts/check.sh),
  [`js/scripts/check.sh`](../../../js/scripts/check.sh) — extend to
  run gap-07 tests.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `markdownlint` not installed in CI image | Low — already used by gap 05/06 lanes. | Reuse the same step from those gaps. |
| CI lane runtime balloons with the new Python/Node test commands | Medium — Python wheel build is ~1m. | Cache `~/.cargo` and `~/.cache/pip`; reuse the lane already configured for `lib-tests.yml`. |
| READMEs duplicate content that already lives in `docs/telemetry/07-bindings-auto-init.md` | Acknowledged — bindings need self-contained docs because npm/PyPI consumers don't browse the repo docs. | Keep the README sections short (matrix tables, no narrative). Link to the gap doc for rationale. |
| `gap-analysis.md` §6 line shifts since the doc was written | Medium | Sub-agent A's update step uses `grep`+`sed` rather than line-number reference. |

## 8. Out of scope

- Renaming `setup_telemetry` to `setup_observability` for clarity.
  The name was locked in decision 2; renaming requires user
  approval.
- Adding a `cognee_pipeline.disable_logging()` Python helper. Not
  needed for v1 — host calls `logging.disable()` from stdlib.
- Per-binding crate-level `telemetry = []` feature flag for
  compile-time opt-out. Decision 3 forced the feature on.
- Migrating Android demo scripts. Decision 9 excluded Android.
