# T6 — `cognee-cli bench` subcommand

**Status:** Not implemented
**Crate:** `cognee-cli` (new `bench` feature)
**Depends on:** T4 (and T5 for a meaningful search phase)
**Unblocks:** T7, T8

---

## Rationale

The Python orchestrator needs **one process that runs every phase and writes the
result JSON** — this is `bench_cognee.py`. `cognee-cli` already bootstraps the
whole pipeline (`ComponentManager`, config, add/cognify/search), so the bench
driver should be a thin subcommand that reuses that wiring rather than a new
binary that re-implements it. Emitting the exact Python JSON schema is what lets
us reuse the Python reporter unchanged (T7).

## Expected output

- `Commands::Bench(BenchArgs)` in
  [`crates/cli/src/cli.rs:15`](../../../crates/cli/src/cli.rs#L15) and a new
  `crates/cli/src/commands/bench.rs`, gated behind a `bench` cargo feature.
- CLI surface mirroring `bench_cognee.py`'s flags:
  `--memories <path>`, `--mock-memories <cassette path>`, `--llm-model`,
  `--llm-provider`, `--embedding-model`, `--embedding-provider`,
  `--embedding-dims`, `--num-memories`, `--mock-llm`, `--dataset-name`,
  `--output/-o`.
- A `--output` JSON file with **exactly** these keys (Python parity):
  `memories_count, add_time_s, cognify_time_s, total_ingest_time_s,
  prune_time_s, db_setup_time_s, search_time, status, success, config`
  where `config = {llm_model, embedding_model, embedding_dimensions,
  dataset_name, mock_llm}` and `status` maps each phase →
  `"success"` | `"failed: <msg>"`.

## Step-by-step implementation

1. **Feature + args.** Add `bench = []` to `crates/cli/Cargo.toml` and include it
   in `default`. Define `BenchArgs` with the flags above (clap), `#[cfg(feature =
   "bench")]`. Add the `Bench` variant to `Commands` and a dispatch arm in
   `main.rs`/`commands/mod.rs`.

2. **Memories loader.** Read the corpus JSON (array of `{title, content,
   references}`). Reuse the Python `memory_to_text` shaping:
   `"Title: {title}\n\n{content}\n\nReferences: {refs}"`. Honor `--num-memories`
   (truncate). The corpus file is provided in T8.

3. **Mock plumbing.** When `--mock-llm` is set, configure `Settings` before
   building the `ComponentManager`:
   - `set_llm_mock(true)` + `set_llm_cassette(--mock-memories)`.
   - `set_embedding` to deterministic mock (`MOCK_EMBEDDING=deterministic`
     equivalent via the setter, or set the env before init) so search is
     meaningful (T5).
   - Use a dummy API key so config validation passes (Python uses `"mock-key"`).
   When not set, use the real provider/model from flags/env as today.

4. **Isolated state per run.** Point `DATA_ROOT_DIRECTORY` /
   `SYSTEM_ROOT_DIRECTORY` (and session dir) at a fresh `tempfile::tempdir()` for
   the invocation so repeated orchestrator runs don't share/clobber state. (The
   prune phase still runs and is timed, matching Python.)

5. **Phase timing.** Mirror `bench_cognee.py`'s structure — each phase wrapped so
   a failure is recorded in `status` but doesn't abort the rest:
   - `prune` → `prune_time_s`
   - DB setup / `ComponentManager` init → `db_setup_time_s`
   - `add(text_list, dataset)` → `add_time_s`
   - `cognify(dataset)` → `cognify_time_s`
   - `total_ingest_time_s = add + cognify`
   - one `search("What is in the document", only_context=true)` → `search_time`
   Use `std::time::Instant`. Round to 3 decimals to match Python output.

6. **Serialize & exit code.** Build a `#[derive(Serialize)] struct BenchResult`
   with the exact field names/order and write pretty JSON to `--output`.
   **Exit-code policy (decided — Python parity):** always exit `0` once the run
   completes and the result file is written, *even if individual phases failed*
   (failures are captured in `status` and `success: false`).
   [`bench_cognee.py`](../../../../cognee/cognee/tests/performance/statistics_percentile/bench_cognee.py)
   does exactly this — it catches per-phase exceptions and never `sys.exit`s on a
   phase failure, while the orchestrator treats a **nonzero** exit as a hard run
   failure that aborts before reading the file
   ([`statistics_percentile_report.py`](../../../../cognee/cognee/tests/performance/statistics_percentile_report.py)
   line ~72). Exit **nonzero only** for catastrophic errors: bad arguments, an
   unreadable corpus, or inability to write `--output`.

7. **stdout discipline.** The orchestrator captures the subprocess; keep
   human-readable progress on stderr (or behind a `--quiet`), and write only the
   machine result to `--output`.

8. **Smoke test (no API key, no pre-recorded cassette needed).** A `cli`
   integration test that runs `bench --mock-llm` with `MOCK_EMBEDDING=deterministic`
   against a **minimal cassette the test writes to a temp file** — even an empty
   one, `{"version":1,"model":"mock","entries":{}}`, works: `ReplayLlm`'s default
   `EmptyGraph` miss policy (T3) makes every extraction return an empty graph and
   every summary a stub, so the full `prune → setup → add → cognify → search`
   pipeline still completes offline. Assert: exit 0, JSON parses, all six metric
   keys present and ≥ 0, `success == true`. (A richer recorded cassette from T8
   exercises real graph content but is **not** required for this test — this is
   what lets T6 be validated before T8 exists.)

## Acceptance / verification

- `cargo run -p cognee-cli --features bench -- bench --mock-llm
  --mock-memories <cassette> --memories <corpus> --output /tmp/r.json` exits 0 and
  produces a schema-valid file with **no network and no API key**.
- `cargo check -p cognee-cli` (without `bench`) still compiles.
- `cargo clippy -p cognee-cli --features bench -- -D warnings` clean.
