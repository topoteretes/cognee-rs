# Mock-LLM Percentile Benchmark — Task Index

This directory tracks porting the Python `cognee` mock-LLM percentile benchmark
to `cognee-rust`, plus the new **LLM response recorder** that auto-generates the
mock fixtures.

Start with the rationale doc — it explains the Python design, why mocking both
backends matters, and the overall porting strategy:

- **[python-approach.md](python-approach.md)** — Python approach, rationale, and
  the Rust porting strategy (read first).

Each task below has a dedicated subdocument with **rationale**, **expected
output**, and **step-by-step implementation instructions**. Update the **Status**
column here as work lands.

To **implement** these tasks with an automated, sub-agent-driven workflow (a
5-step check-description → implement → review → validate → commit scheme per
task), follow [EXECUTION-PROMPT.md](EXECUTION-PROMPT.md).

Status values: `Not implemented` · `In progress` · `Implemented`.

## Task list

The critical path is **T1 → T2 → T3 → T4** (recorder + replay mock + factory
wiring). T5 is independent. T6 → T7 → T8 depend on T4. T9 is last.

| ID | Task | Short description | Status | Plan |
|----|------|-------------------|--------|------|
| T1 | Cassette format & hashing | Serde types for the recorded-response cassette + a stable `sha256(input)` match-key, behind a `mock` feature in `cognee-llm`. | Implemented | [task-01-cassette-format.md](tasks/task-01-cassette-format.md) |
| T2 | `RecordingLlm` decorator | Wraps the real `Arc<dyn Llm>`, forwards every call, captures `(input → response)` into a cassette; flushes on demand and on drop. | Implemented | [task-02-recording-llm.md](tasks/task-02-recording-llm.md) |
| T3 | `ReplayLlm` content-aware mock | Loads a cassette and replays by input hash; configurable miss policy (empty-graph default, Python parity). | Implemented | [task-03-replay-llm.md](tasks/task-03-replay-llm.md) |
| T4 | Factory wiring (`MOCK_LLM` / recording) | `LlmProvider::Mock` + `MOCK_LLM`, `MOCK_LLM_CASSETTE`, `COGNEE_RECORD_LLM` env hooks wired through `Settings` and `init_llm`. | Implemented | [task-04-factory-wiring.md](tasks/task-04-factory-wiring.md) |
| T5 | Deterministic mock embedding | Port the Python SHA-256 vector scheme into `MockEmbeddingEngine`, selectable via `MOCK_EMBEDDING=deterministic`. | Implemented | [task-05-deterministic-embedding.md](tasks/task-05-deterministic-embedding.md) |
| T6 | `cognee-cli bench` subcommand | Phase-timed `prune → setup → add → cognify → search` driver emitting the exact JSON contract; gated behind a `bench` feature. | Implemented | [task-06-cli-bench-subcommand.md](tasks/task-06-cli-bench-subcommand.md) |
| T7 | Reuse the Python orchestrator | `BENCH_CMD` override on `../cognee/.../statistics_percentile_report.py` + a wrapper script that drives the Rust CLI through it. | Implemented | [task-07-python-orchestrator.md](tasks/task-07-python-orchestrator.md) |
| T8 | Cassette fixture & corpus | Record a cassette once against a real LLM and commit it (+ a `memories.json` corpus) so mock benches need no API key. | Implemented | [task-08-cassette-fixture.md](tasks/task-08-cassette-fixture.md) |
| T9 | Docs & verification | User-facing how-to doc + unit/smoke tests + `scripts/check_all.sh`. | Not implemented | [task-09-docs-verification.md](tasks/task-09-docs-verification.md) |

## Conventions

- **Feature gates.** Mock/recorder code lives in `cognee-llm` behind a `mock`
  feature; the bench driver behind a `bench` feature in `cognee-cli`. Both are
  enabled in the `default` feature sets of `cognee-lib` / `cognee-cli` (they pull
  in no heavy dependencies) but kept named so they can be turned off.
- **Existing test mock untouched.** The FIFO-queue
  [`MockLlm`](../../crates/test-utils/src/mock_llm.rs) in `test-utils` stays for
  unit tests; the new content-aware mock is a separate, production-reachable type.
- **Python parity.** Defaults mirror the Python tooling (empty-graph on cassette
  miss, skip inter-run cooldown in mock mode, identical result JSON keys).
