# Task 02-12 — CI updates for `send_telemetry`

**Status**: implemented in commit d66f490 (note: ci.yml extends the gap-01 telemetry feature step rather than adding a new lane; cross-SDK Docker harness wires into the existing http-parity.yml workflow rather than a new file).
**Owner**: _unassigned_
**Depends on**:
- [Task 02-07 — Callsite migration](07-callsite-migration.md)
- [Task 02-08 — Unit tests](08-unit-tests.md)
- [Task 02-09 — Integration tests](09-integration-tests.md)
- [Task 02-10 — Cross-SDK parity](10-cross-sdk-parity.md)

**Blocks**: nothing — this is the final task in the gap.

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Make sure CI exercises the new code in three feature states:

1. **Default** (`telemetry` ON per decision 1) — already the case
   for the existing `cargo check`/`cargo test` lanes once the
   feature is added to `cognee-lib/default`. Verify the existing
   lanes catch regressions in `cognee-telemetry`.
2. **`--features telemetry` only** — the explicit-feature lane added
   by gap 01 already runs `cargo test -p cognee-observability`. Add
   `cargo test -p cognee-telemetry --features telemetry`.
3. **`--no-default-features`** — the noop fallback path. Gap 01
   already added a no-default-features check; ensure
   `cognee-telemetry`'s noop lane is exercised by it.
4. **Network isolation** — assert no outbound HTTP fires when
   `TELEMETRY_DISABLED=1` is set globally for the lane.
5. **(Optional)** Cross-SDK parity — gate the docker-compose lane
   from [task 02-10](10-cross-sdk-parity.md) on a separate workflow
   so the main `ci.yml` stays fast.

   The repo's existing cross-SDK Docker workflow is
   [`.github/workflows/http-parity.yml`](../../.github/workflows/http-parity.yml).
   It currently runs only on `workflow_dispatch` (the upstream
   Python migrations are blocking re-enablement on PR/push). The
   telemetry parity service is already wired into
   `e2e-cross-sdk/docker-compose.yml` (the
   `e2e-telemetry` service runs
   `pytest -vs /harness/test_telemetry_parity.py`, see
   `docker-compose.yml:72`); the only outstanding work is wiring a
   step into a workflow.

Plus a small extension to `scripts/check_all.sh` so the local
pre-commit suite also covers the noop path.

## 2. Rationale

### Why mirror gap 01's lane structure

Gap 01 added a `--features telemetry` lane (`ci.yml:70-73`,
`ci.yml:80-83` for clippy, and `ci.yml:163-166` for tests) and a
`--no-default-features` canary lane (`ci.yml:90-93`). Reusing the
same shape:

- Keeps the workflow file readable.
- Avoids duplicating the cargo cache setup.
- Matches reviewer expectations.

The change in this task is **scope expansion** — the existing lanes
must compile and test the new `cognee-telemetry` crate, not the
introduction of new top-level lanes.

### Why a network-isolation assertion

`TELEMETRY_DISABLED` is the most-used opt-out. A regression where
the env check moved after identity derivation (a Python bug in 2024,
fwiw) would silently leak. CI asserts the contract by setting the
env globally for the lane and verifying no `test.prometh.ai` lookup.

### Why decouple the cross-SDK lane

The Docker-compose harness adds ~2 minutes to CI even with cached
layers. Most PRs don't touch telemetry. Gate behind a path filter
(e.g. `crates/telemetry/**`, `e2e-cross-sdk/**`,
`docs/telemetry/02/**`) so unrelated PRs don't pay the cost.

## 3. Pre-conditions

- Tasks 02-01 through 02-11 merged.
- `scripts/check_all.sh` passes locally on `main`.
- The CI runner has network egress restricted to the docker
  registry (no `test.prometh.ai`).

## 4. Step-by-step

### 4.1 Extend `.github/workflows/ci.yml`

The repo has a single primary workflow file: `.github/workflows/ci.yml`
(there is no `lib-tests.yml`, `lint.yml`, `capi-check.yml`,
`js-check.yml`, or `python-check.yml` — those are jobs *within* `ci.yml`,
not separate files). The other workflow file in the repo is
`http-parity.yml`, which is the cross-SDK Docker harness lane.

#### 4.1.1 Existing default lane

The default `cargo check --all-targets` and `cargo test` lanes will
now include `cognee-telemetry` (because `cognee-lib/default` now
pulls `telemetry` per decision 1). No edits needed; verify by
inspecting the workflow logs after the gap lands.

#### 4.1.2 Existing `--features telemetry` lane

The lane currently runs (`ci.yml:163-166`):

```yaml
- name: Test (telemetry feature)
  env:
    CARGO_TARGET_DIR: target/telemetry
  run: cargo test -p cognee-observability --features telemetry -- --nocapture
```

Replace the explicit `-p cognee-observability` with two invocations
that cover both crates:

```yaml
- name: Test (telemetry feature, observability + telemetry crates)
  env:
    CARGO_TARGET_DIR: target/telemetry
    TELEMETRY_DISABLED: "1"            # belt-and-braces; tests
                                       # explicitly opt in via env
                                       # override.
  run: |
    cargo test -p cognee-observability --features telemetry -- --nocapture
    cargo test -p cognee-telemetry --features telemetry -- --nocapture
```

The `TELEMETRY_DISABLED=1` global env is intentional: the only
tests that want to fire HTTP are the mockito-driven ones in
[task 02-09](09-integration-tests.md), which set
`COGNEE_TELEMETRY_INTEGRATION_TEST=1` to override and inject the
mockito URL. Outside of those tests, telemetry stays disabled —
preventing accidental egress to the live proxy.

#### 4.1.3 Existing `--no-default-features` lane

The lane currently only runs `cargo check -p cognee-lib
--no-default-features` (`ci.yml:90-93`). Add a sibling step in the
test job that *runs* the noop tests:

```yaml
- name: Test (no default features, telemetry crate noop fallback)
  env:
    CARGO_TARGET_DIR: target/no-default
  run: cargo test -p cognee-telemetry --no-default-features -- --nocapture
```

This exercises `crates/telemetry/tests/noop_fallback.rs` from
[task 02-08](08-unit-tests.md). Reusing
`CARGO_TARGET_DIR=target/no-default` matches the existing canary
step's target dir for cache reuse.

#### 4.1.4 New network-isolation lane (optional)

```yaml
- name: Network isolation (telemetry must not egress when disabled)
  env:
    TELEMETRY_DISABLED: "1"
  run: |
    # Run the full unit + integration suite with telemetry off.
    # mockito tests skip themselves under TELEMETRY_DISABLED — they
    # set COGNEE_TELEMETRY_INTEGRATION_TEST when they need to fire.
    cargo test -p cognee-telemetry --features telemetry -- --nocapture
    # Sanity: confirm the dispatcher logs the "disabled" debug line
    # at least once.
    RUST_LOG=cognee.telemetry=debug \
      cargo test -p cognee-lib --features telemetry test_forget -- --nocapture 2>&1 \
      | grep -q 'telemetry feature disabled\|TELEMETRY_DISABLED' \
      || (echo 'expected disabled-log line; saw none' && exit 1)
```

If the runner cannot capture per-process logs reliably, drop the
grep step — the in-test mockito assertions in
[task 02-09](09-integration-tests.md) (`mock.expect(0)`) already
cover the no-egress invariant.

### 4.2 Cross-SDK workflow

Two viable options — pick one:

**Option A (preferred): extend `.github/workflows/http-parity.yml`.**
The harness already builds the `cognee-telemetry-emit` binary
(`Dockerfile:35-61, 124`) and ships an `e2e-telemetry` service
in `docker-compose.yml:52-72` that runs
`pytest -vs /harness/test_telemetry_parity.py`. Add a phase step to
`http-parity.yml` after Phase-1 that invokes that service:

```yaml
- name: Telemetry parity (cross-SDK identity layers)
  run: >-
    docker compose
    -f cognee-rust/e2e-cross-sdk/docker-compose.yml
    run --rm e2e-telemetry
```

`http-parity.yml` runs only on `workflow_dispatch` today (see the
TODO at the top of the file — blocked on Python migrations). The
new step rides along when that's lifted.

**Option B: a dedicated `.github/workflows/telemetry-parity.yml`.**
Use this only if you need a faster cadence than `http-parity.yml`'s
eventual PR/push triggers. Skeleton:

```yaml
name: Cross-SDK telemetry parity

on:
  pull_request:
    paths:
      - 'crates/telemetry/**'
      - 'e2e-cross-sdk/**'
      - 'docs/telemetry/02/**'
      - '.github/workflows/telemetry-parity.yml'
  push:
    branches: [main]
    paths:
      - 'crates/telemetry/**'
      - 'e2e-cross-sdk/**'
  schedule:
    # Daily run on main so we catch upstream Python drift even when
    # nobody touches the Rust side.
    - cron: '0 5 * * *'

jobs:
  cross-sdk:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@v3
      - name: Cache buildx layers
        uses: actions/cache@v4
        with:
          path: /tmp/.buildx-cache
          key: telemetry-parity-${{ runner.os }}-${{ hashFiles('e2e-cross-sdk/**') }}
          restore-keys: |
            telemetry-parity-${{ runner.os }}-
      - name: Run cross-SDK parity
        run: >-
          docker compose
          -f e2e-cross-sdk/docker-compose.yml
          run --rm e2e-telemetry
```

Note: telemetry parity does not need OPENAI secrets — the parity
test mocks the proxy with the local `telemetry-proxy` service and
asserts identity-layer parity only.

The workflow runs:

- **On PRs touching telemetry, the harness, or these docs** — fast
  feedback for the engineer.
- **On push to `main` touching the same paths** — protects the
  default branch.
- **Daily at 05:00 UTC** — catches upstream Python drift (e.g. a
  cognee Python release that changes the wire schema).

Path filtering keeps the harness off the critical path of
unrelated PRs.

### 4.3 Extend `scripts/check_all.sh`

The script already runs (in order): `cargo fmt --all -- --check`,
`cargo check --all-targets`, `cargo clippy --all-targets -- -D
warnings`, `cargo check --all-targets --features telemetry`,
`cargo check -p cognee-lib --no-default-features`, and the binding
checks (capi/python/js). The two telemetry-related checks
(`--features telemetry` and `-p cognee-lib --no-default-features`)
were added by gap 01.

To upgrade the noop coverage from a *check* to a *test* run, add
one step after the existing `cargo check -p cognee-lib
--no-default-features` block:

```bash
echo "================================================================"
echo "=== Rust: Test (telemetry crate noop fallback) ==="
echo "================================================================"
CARGO_TARGET_DIR=target/check-noop \
  cargo test -p cognee-telemetry --no-default-features --tests
```

This is the local equivalent of the CI lane added in §4.1.3.

### 4.4 Verify

Local:

```bash
scripts/check_all.sh
cargo test -p cognee-telemetry --features telemetry
cargo test -p cognee-telemetry --no-default-features
```

Remote (PR):

- Open a small PR that touches `crates/telemetry/src/lib.rs` and
  observe `ci.yml` fire. (The cross-SDK harness in
  `http-parity.yml` only runs on `workflow_dispatch` today —
  trigger it manually from the Actions tab to verify the
  telemetry-parity step.)

## 5. Verification

```bash
# 1. Local check suite passes.
scripts/check_all.sh

# 2. Default lane covers cognee-telemetry transitively.
cargo test -p cognee-lib  # default features include telemetry per decision 1
# Inspect output for any cognee-telemetry test names.

# 3. Explicit-feature lane covers the new crate.
cargo test -p cognee-telemetry --features telemetry

# 4. No-default-features lane covers the noop.
cargo test -p cognee-telemetry --no-default-features

# 5. Cross-SDK lane (manual smoke). The compose file declares
#    `e2e-telemetry` as the test runner (with `telemetry-proxy` as
#    its dependency); other services like `e2e-tests` and
#    `e2e-http-tests` belong to the HTTP-parity harness and are
#    irrelevant here.
cd e2e-cross-sdk
docker compose run --rm --build e2e-telemetry

# 6. The new workflow file lints cleanly.
yamllint .github/workflows/telemetry-parity.yml
# (yamllint may not be in CI; visual review is the gate.)
```

## 6. Files modified

- `.github/workflows/ci.yml` — extend the existing telemetry test
  step to cover `cognee-telemetry`; add a no-default-features test
  step; optionally add a network-isolation env wrapper.
- `.github/workflows/http-parity.yml` — add a step that runs
  `docker compose run --rm e2e-telemetry` (Option A in §4.2).
  Alternatively, create
  `.github/workflows/telemetry-parity.yml` as a separate workflow
  (Option B).
- `scripts/check_all.sh` — add a noop-fallback test step after the
  existing `cargo check -p cognee-lib --no-default-features`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `TELEMETRY_DISABLED=1` global env masks a bug where another env-var path bypasses the check | Low | The mockito tests explicitly remove `TELEMETRY_DISABLED` in `IsolatedEnv::install`, so the dispatcher path is exercised. |
| New lane adds noticeable CI time | The cargo cache is shared with the existing telemetry lane via `CARGO_TARGET_DIR=target/telemetry`. Incremental cost ≈ 10 s for the new crate. | Acceptable. |
| Cross-SDK workflow uses too much CI minutes via daily cron | One run/day, ~5 min/run, ≈ 150 min/month | Within free tier limits; if billing matters, drop to weekly. |
| Path filter misses a relevant change (e.g. a `cognee-lib` change that breaks parity) | Real risk | Add `crates/lib/**` to the path filter if it surfaces. Keep the filter scoped enough that unrelated changes don't trigger a 5-minute build. |
| Daily cron drift if `cognee-python` upstream changes the wire schema | The whole point of the daily run | Surface failure to the team via the standard GH Actions alert; the cross-SDK test failure message is detailed enough to guide the fix. |
| `scripts/check_all.sh` step on noop becomes the slowest step | Each `cargo test` rebuild can take a minute | Reuse the workspace target dir (`CARGO_TARGET_DIR=target/check-noop`) so the second invocation in the same shell is incremental. |
| GitHub-hosted runner can't reach Docker Hub due to a transient outage | Occasional flake | Pin base images to specific digests; cache buildx layers as shown above. |

## 8. Out of scope

- Replacing `lib-tests.yml` with a matrix-based workflow — out of
  scope; the existing lane structure is good enough.
- Adding telemetry to nightly performance benchmarks — gap 02 is
  about correctness, not perf.
- Coverage reporting — workspace-wide coverage is a separate
  initiative.
