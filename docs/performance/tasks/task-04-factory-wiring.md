# T4 — Factory wiring (`MOCK_LLM` / recording)

**Status:** Implemented
**Crates:** `cognee-llm` (config), `cognee-lib` (`Settings` + `ComponentManager`)
**Depends on:** T2, T3
**Unblocks:** T6 (and makes the existing Criterion bench mock-capable)

---

## Rationale

Rust can't monkey-patch like Python, so the mock and recorder must be selectable
through config/env at the place the LLM is constructed. This task makes
`MOCK_LLM` a first-class switch, exactly paralleling the existing `MOCK_EMBEDDING`
precedent ([`crates/embedding/src/config.rs:194`](../../../crates/embedding/src/config.rs#L194)),
and adds an opt-in `COGNEE_RECORD_LLM` wrap around the real adapter. After this
task, *any* entry point (CLI, HTTP server, examples) can run the pipeline with a
mock or recording LLM — not just the bench.

## Expected output

- `LlmProvider::Mock` variant in
  [`crates/llm/src/config.rs:8`](../../../crates/llm/src/config.rs#L8).
- New `Settings` fields + env bindings:
  - `MOCK_LLM` (bool-ish: `true`/`1`/`yes`) → selects the mock provider.
  - `MOCK_LLM_CASSETTE` (path) → cassette for `ReplayLlm`.
  - `COGNEE_RECORD_LLM` (path) → wrap the real adapter in `RecordingLlm`.
- `init_llm` ([`crates/lib/src/component_manager.rs:488`](../../../crates/lib/src/component_manager.rs#L488))
  honors all three.
- Config setters (`set_llm_mock`, `set_llm_cassette`, `set_llm_record_path`) in
  the `Settings` style ([`crates/lib/src/config.rs:806`](../../../crates/lib/src/config.rs#L806) onward).

## Step-by-step implementation

1. **Feature propagation.** Enable `cognee-llm/mock` from `cognee-lib` and
   `cognee-cli`. Add a `mock-llm` feature to each that turns on
   `cognee-llm/mock`, and include it in their `default` feature sets (per the
   repo's feature strategy — see project `CLAUDE.md`). Mock code pulls in no heavy
   deps, so default-on is fine.

2. **Provider enum.** Add `Mock` to `LlmProvider` (serde `lowercase` → `"mock"`).

3. **Settings fields.** In [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs):
   - Add `llm_mock: bool`, `llm_cassette: String`, `llm_record_path: String`
     (empty = unset), with defaults `false`/`""`/`""` in the `Default` impl
     (around line 613).
   - Add env parsing where the other LLM env vars are read: `MOCK_LLM`,
     `MOCK_LLM_CASSETTE`, `COGNEE_RECORD_LLM`. Mirror the `MOCK_EMBEDDING` parsing
     (accept `true`/`1`/`yes`, case-insensitive).
   - Add to the serialized form (around line 573) and `set_llm_config` dispatch
     (around line 1232/1328) for parity with existing keys.
   - Add setters `set_llm_mock(bool)`, `set_llm_cassette(&str)`,
     `set_llm_record_path(&str)`.

4. **`init_llm` branching.** Restructure the match in
   [`component_manager.rs:488`](../../../crates/lib/src/component_manager.rs#L488):
   - Read the new fields alongside the existing ones.
   - **Mock first** (overrides provider, like `MOCK_EMBEDDING`): if `llm_mock` is
     set *or* `provider == "mock"`, require a non-empty `llm_cassette`, build
     `ReplayLlm::from_path(cassette)?` (feature-gated import), and return it. If
     the cassette path is empty, return a `ComponentError::Config` with a clear
     message.
   - **Otherwise** build the real adapter exactly as today (`openai`/`litert`).
   - **Recording wrap:** if `llm_record_path` is non-empty, wrap the constructed
     real adapter: `Arc::new(RecordingLlm::new(adapter, record_path))`. (Recording
     a mock is pointless, so only wrap on the real path.)
   - Behind `#[cfg(feature = "mock")]`; if the feature is off and `MOCK_LLM` is
     requested, error with a message telling the user to build with `mock-llm`.

5. **Embedding pairing.** No change here, but note for T6: a mock LLM run almost
   always wants `MOCK_EMBEDDING` too. Document the pairing; the bench sets both.

6. **Criterion bench bonus.** Add `MOCK_LLM`, `MOCK_LLM_CASSETTE` to the
   `LLM_ENV_VARS` forwarded list in
   [`crates/bench/benches/batch_add_cognify.rs:133`](../../../crates/bench/benches/batch_add_cognify.rs#L133)
   so the existing HTTP-server bench can also run in mock mode. (Optional but
   cheap.)

## Acceptance / verification

- `MOCK_LLM=true MOCK_LLM_CASSETTE=<file> MOCK_EMBEDDING=deterministic` lets a
  cognify run complete with **no `LLM_API_KEY`** set.
- `cargo check -p cognee-lib` and `-p cognee-cli` compile with and without the
  `mock-llm` feature.
- An integration test in `cognee-lib` (or `cognee-cli`) drives add→cognify→search
  end to end against a small committed cassette + deterministic embeddings,
  asserting it succeeds offline.
- `scripts/check_all.sh` is run after this lands (binding checks included).
