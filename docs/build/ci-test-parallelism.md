# CI test parallelism

## TL;DR

The workspace test suite runs **in parallel** under `cargo nextest`. Earlier it
ran fully serially (`cargo nextest run --no-capture`, which forces
`--test-threads=1`), taking ~20 min for the run phase alone. Parallel execution
brings that down to a few minutes with no loss of correctness.

## Why serial was unnecessary

The serial requirement was a holdover from the `cargo test` era. Under
`cargo test`, every test in a binary shares **one process and runs on threads**,
so tests that mutate process-global state race with each other:

- `std::env::set_var` / `remove_var` (125+ call sites across the workspace)
- `once_cell`/`OnceLock` singletons (e.g. the telemetry client)

`cargo nextest` runs **each test in its own process**. That process isolation
makes the above state per-test by construction — the global serialization is
redundant. Empirically, the full suite (2312 tests) passes under aggressive
parallelism (validated at 14-way locally) with zero races, leaks, port
conflicts, or timeouts.

The only shared resource the LLM integration tests touch is the OpenAI API
endpoint. That is safe to hit concurrently: `gpt-4o-mini` tolerates the handful
of concurrent calls a CI runner produces, and the OpenAI adapter retries
HTTP 429/5xx with exponential backoff (`crates/llm/src/adapters/openai.rs`).

## Configuration

- **`.config/nextest.toml`** — `default` and `ci` profiles. Both run in
  parallel with `fail-fast = false` (surface every failure in one run). The
  `ci` profile adds `retries = 1` to absorb genuinely transient external
  flakiness without masking deterministic failures (a real bug fails the retry
  too). Concurrency is left at the nextest default (= CPU cores).
- **`scripts/lib/common.sh`** — `run_cargo_tests` invokes
  `cargo nextest run --workspace` (no `--no-capture`). To debug a single test
  serially with live output, pass `--no-capture` manually or set
  `NEXTEST_NO_CAPTURE=1`.
- **`.github/workflows/ci.yml`** — the `test` job exports `NEXTEST_PROFILE=ci`,
  which nextest reads natively to select the `ci` profile.

## Cross-platform test isolation gotcha (found while enabling parallelism)

The CLI integration tests set `XDG_CONFIG_HOME` to a per-test temp dir so each
test gets an isolated config file. `crates/cli/src/config_store.rs`'s
`config_file_path()` originally resolved the config dir via
`dirs::config_dir()`, which **only honors `XDG_CONFIG_HOME` on Linux**. On macOS
`dirs` returns `~/Library/Application Support` unconditionally, so every test
subprocess collided on one shared `config.json` and raced on the atomic
`config.json[.tmp]` replace (`fs::rename` → `ENOENT`). It also polluted the
developer's real config dir.

CI runs on Linux, so this never affected CI — but it broke local parallel runs
on macOS and is a latent correctness gap. The fix makes `config_file_path()`
honor `XDG_CONFIG_HOME` explicitly on **all** platforms (a no-op on Linux,
where `dirs` already resolves it), restoring identical isolation everywhere.

**Lesson for new tests:** to isolate per-test on-disk state across platforms,
set the relevant env override yourself (`XDG_CONFIG_HOME`, and `HOME` for the
`~/.cognee/logs` fallback) — do not rely on `dirs::*` honoring XDG on macOS.
