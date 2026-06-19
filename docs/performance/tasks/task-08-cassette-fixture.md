# T8 — Cassette fixture & corpus

**Status:** Implemented
**Location:** `scripts/perf/fixtures/`
**Depends on:** T2, T4, T6
**Unblocks:** zero-API CI benchmarking

---

## Rationale

The mock benchmark is only "zero-API" if the cassette already exists. This task
performs the **record-once** step: run the bench against a real LLM with the
recorder enabled, then commit the resulting cassette and the input corpus. After
this, anyone (and CI) can run the mock percentile report with no API key — the
Rust equivalent of Python's committed `mock_memories.json`, but generated from
real model output instead of hand-authored.

## Expected output

- `scripts/perf/fixtures/memories.json` — the input corpus (array of
  `{title, content, references}`), copied from the Python fixture.
- `scripts/perf/fixtures/cassette.json` — recorded LLM responses for that corpus,
  committed to the repo.
- A documented command to regenerate the cassette when the corpus or pipeline
  prompts change.

## Step-by-step implementation

1. **Corpus.** Copy
   [`../cognee/cognee/tests/performance/statistics_percentile/memories.json`](../../../../cognee/cognee/tests/performance/statistics_percentile/memories.json)
   to `scripts/perf/fixtures/memories.json`. The Python corpus is **50 short
   memories** (`{title, content, references}`); keep it at or below that so the
   cassette and run time stay small. If size is a concern, trim with
   `--num-memories` and record only that subset.

2. **Record.** With real LLM credentials configured (`LLM_API_KEY`, `OPENAI_URL`,
   model), run the bench once with recording enabled and **no** `--mock-llm`:
   ```sh
   COGNEE_RECORD_LLM=$(pwd)/scripts/perf/fixtures/cassette.json \
   MOCK_EMBEDDING=deterministic \
     cargo run --release -p cognee-cli --features bench -- bench \
       --memories scripts/perf/fixtures/memories.json \
       --output /tmp/record_run.json
   ```
   `RecordingLlm` (T2) wraps the real adapter (T4) and writes the cassette on
   completion/drop. `MOCK_EMBEDDING=deterministic` keeps embeddings free/offline
   even while recording the LLM (we only need to record the LLM).

3. **Determinism check.** Re-run in mock mode and confirm a full hit rate:
   ```sh
   MOCK_LLM=true MOCK_EMBEDDING=deterministic \
     cargo run --release -p cognee-cli --features bench -- bench \
       --mock-llm --mock-memories scripts/perf/fixtures/cassette.json \
       --memories scripts/perf/fixtures/memories.json --output /tmp/mock_run.json
   ```
   If the mock run logs cassette **misses** (empty-graph fallbacks), the chunking
   wasn't reproduced identically — investigate before committing (common causes:
   nondeterministic chunk text, prompt changes, batching differences). The corpus
   must contain **no random content** (unlike the Criterion bench, which appends a
   random UUID paragraph) so inputs — and therefore hashes — are stable.

4. **Commit.** Add `memories.json` and `cassette.json` under
   `scripts/perf/fixtures/`. Note the cassette size in the PR; if large, consider
   trimming the corpus rather than using Git LFS.

5. **Regeneration doc.** Record the exact regeneration command in
   [task-09](task-09-docs-verification.md)'s how-to doc, and reference it from a
   comment at the top of `cassette.json` is not possible (JSON) — instead add a
   `scripts/perf/README.md` snippet or a Make/justfile target `perf-record`.

## Acceptance / verification

- The mock run in step 3 reports **0 cassette misses** and `success: true`.
- A fresh checkout with no API credentials can run
  `RUNS=3 scripts/perf/run_mock_bench.sh` (T7) to completion.
- The committed cassette parses as a valid `LlmCassette` (a tiny test can
  `LlmCassette::load` it under the `mock` feature).
