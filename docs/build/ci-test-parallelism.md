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

## Approach E — cassette replay for LLM-bound integration tests

A handful of integration tests hit the live OpenAI API on every CI run (20–60s
each). Beyond their own latency, they saturate the 4-core runner, which inflates
the wall time of hundreds of otherwise-millisecond tests via contention (a 12 ms
test can stretch to >15 s). Approach E moves the **deterministic** ones offline
using the cassette record/replay infra in `crates/llm/src/mock/`.

### How it works

- `crates/{cognify,search}/tests/test_utils.rs` (and an inlined copy in the
  delete test) expose `create_llm_from_env(cassette)`:
  - `COGNEE_TEST_REPLAY=1` → replay from `tests/fixtures/cassettes/<name>.json`
    with `MissPolicy::Error` (a stale/missing entry fails loudly instead of the
    `ReplayLlm` default of silently returning an empty graph). No credentials.
  - `COGNEE_RECORD_LLM=1` → wrap the real adapter and write/merge the cassette.
  - neither → the real adapter.
- Embeddings use `create_deterministic_embedding_engine()`
  (`MockEmbeddingEngine::deterministic(384)`, `sha256(text)`-derived) — chosen
  **per test in code**, never via a global `MOCK_EMBEDDING` env var, so the
  real-embedding tests below keep a real engine in the same CI run. Using
  deterministic embeddings in *both* record and replay keeps retrieval (and
  therefore retrieval-augmented LLM prompts) byte-reproducible, so cassette keys
  hit.
- CI sets `COGNEE_TEST_REPLAY=1` on the `test` job; the `mock` feature is enabled
  on each crate's `cognee-llm` dev-dep.

### Converted (offline, deterministic, zero API cost)

`integration_fact_extraction`, `integration_summarization`,
`e2e_triplet_vector_cleanup`, `e2e_delete_preview_accuracy`,
`e2e_shared_entity_graph_delete`, `e2e_lifecycle_loop` (cognify);
`last_accessed_update` (search); `hard_mode_orphan_sweep` (delete).

### The cassette boundary (kept on the real API)

Cassettes capture only the **LLM**, not embeddings. A test is *not* convertible
when its assertions depend on real embeddings:

- **Semantic-retrieval assertions** — e.g. `integration_search_matrix` asserts
  the answer contains "germany"/"netherlands"; `search_after_partial_delete`
  expects a query to match a specific doc. Deterministic vectors don't cluster
  semantically, so these fail (even at record time). Also `integration_embeddings`.
- **Non-deterministic query derivation** — `e2e_full_pipeline_memify` and
  `integration_default_backend` build their search query from `entities[0]`,
  whose order isn't stable; the query (and thus the retrieval-augmented prompt)
  varies between record and replay → cassette miss.
- **HTTP-server e2e** (`test_cognify_blocking`, `test_ontology_cognify_search_e2e`)
  — assert on semantic markers and wire the LLM deep into `ComponentHandles`.

These stay on the real API. A `heavy-real-api` nextest concurrency cap was
trialled to stop them co-saturating the runner but showed **no measurable
effect** (3 vs 4 concurrent on 4 cores is marginal; the tests are largely
network-bound) and was removed.

### Re-recording cassettes

When a prompt, schema, or model changes, replay fails with `MissPolicy::Error`.
Regenerate locally with real credentials:

```bash
COGNEE_RECORD_LLM=1 cargo test -p cognee-cognify --test <name> -- --test-threads=1
```

or trigger the **Record LLM cassettes** workflow
(`.github/workflows/record-cassettes.yml`, `workflow_dispatch`), which records
against the live API and commits the refreshed cassettes back to the branch.

## Actions cache budget (why Lint sometimes goes cold)

GitHub allows **10 GB of Actions cache per repo** and evicts least-recently-used
entries past that. The steady-state working set here is healthy — roughly
`capi-check` 3 GB + `workspace-test-v6` ~1 GB + `workspace-lint-v6` ~1 GB +
`workspace-msrv-v1` 0.5 GB + small (ort, ccache, node) ≈ 6–7 GB.

It blows past 10 GB during **bursts of rapid pushes**: each change to a
`Cargo.toml` (or the lockfile) shifts the dependency resolution, so
`Swatinem/rust-cache` writes a *new* cache version and keeps the old ones until
they age out (7 days) or are evicted. A dozen pushes in a day can leave 3–5
stale versions of each key (≈ 11 GB), and the LRU eviction then drops whichever
working cache ran longest ago — usually `workspace-lint-v6` — so the next Lint
job rebuilds cold (observed climbing 3 → 36 min over one afternoon). Caches from
other branches count toward the same 10 GB, so this is partly repo-wide.

**If Lint (or any job) is unexpectedly cold:** check usage and prune stale
duplicate versions (keep the newest per key — that one matches the current dep
state and will be restored):

```bash
gh api repos/<owner>/<repo>/actions/cache/usage
gh api "repos/<owner>/<repo>/actions/caches?per_page=100"   # find stale ids
gh api -X DELETE repos/<owner>/<repo>/actions/caches/<id>   # delete stale ones
```

Avoid "fixing" this by dropping a job's cache save — every cache here is
load-bearing (a cold `capi-check` is ~40 min, a cold `msrv`/`lint` ~20–36 min).
The accumulation is self-correcting once a churn of Cargo.toml edits settles.

### What E delivered

Removing the live-API dependency from 8 tests makes them deterministic, free,
and network-flake-free (CI flakiness dropped to zero). Its **wall-time** impact
is modest — the run phase moved 452s → ~420s — because the uncassettable
real-API tests still dominate it. The `test` job's larger costs are compilation
and `cargo doc`, addressed by separate workflow restructuring.
