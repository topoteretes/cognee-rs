# T2 — `RecordingLlm` decorator

**Status:** Implemented
**Crate:** `cognee-llm` (`mock` feature)
**Depends on:** T1
**Unblocks:** T4, T8

---

## Rationale

The Python mock fixtures (`mock_memories.json`) are hand-authored — tedious and
prone to drifting from real model output. Instead of hand-writing the Rust
fixtures, we generate them: a decorator wraps the *real* LLM, passes every call
through unchanged, and records the parsed response. Run cognify once with
recording on and you have a complete, faithful cassette to replay forever.

Recording at the trait's single chokepoint
([`create_structured_output_with_messages_raw`](../../../crates/llm/src/llm_trait.rs#L54))
captures exactly the `Value` the pipeline consumes — so replay (T3) is bit-for-bit
what the real run produced.

## Expected output

- `crates/llm/src/mock/recording.rs` with:
  ```rust
  pub struct RecordingLlm { inner: Arc<dyn Llm>, entries: Mutex<BTreeMap<String, CassetteEntry>>, path: PathBuf }
  impl RecordingLlm { pub fn new(inner: Arc<dyn Llm>, path: impl Into<PathBuf>) -> Self }
  impl RecordingLlm { pub fn flush(&self) -> LlmResult<()> }
  impl Llm for RecordingLlm { /* delegate + record */ }
  impl Drop for RecordingLlm { /* best-effort flush */ }
  ```
- Round-trip guarantee: record against any `Llm`, then `ReplayLlm` (T3) over the
  written cassette returns identical values.

## Step-by-step implementation

1. **Struct + constructor.** Hold `inner: Arc<dyn Llm>`, an in-memory
   `Mutex<BTreeMap<String, CassetteEntry>>`, and the output `path`. If `path`
   already exists, load it first and seed the map (so re-recording merges instead
   of clobbering).

2. **Implement `Llm`.** For each method: call `inner`, and on success insert a
   `CassetteEntry` keyed by the T1 hash before returning the value:
   - `create_structured_output_with_messages_raw(messages, schema, opts)`:
     hash = `input_hash(&messages, Some(schema))`; record
     `method = StructuredOutput`, `user_input_preview` = first ~120 chars of the
     last `User` message, `schema_name` = best-effort from the schema's `title`
     field if present; `response` = the returned `Value`.
   - `generate(messages, opts)`: hash = `input_hash(&messages, None)`; store the
     `GenerationResponse.content` as `Value::String`, `method = Generate`.
   - `transcribe_image(bytes, mime, opts)`: hash = `vision_hash(bytes, mime)`;
     store the returned `String`, `method = TranscribeImage`.
   - Pass-through trait introspection (`model`, `supports_*`, `max_context_length`)
     straight to `inner`.
   - **Do not** override the default `create_structured_output_raw` — it funnels
     into `*_with_messages_raw`, which we already intercept (avoids double
     recording).

3. **Flush.** `flush()` snapshots the map under the lock into an `LlmCassette`
   (`model = inner.model()`, `version = 1`) and calls `LlmCassette::save(path)`.
   Lock poisoning is unrecoverable — `// lock poison is unrecoverable`.

4. **Drop.** Implement `Drop` to call `flush()` and log (not panic) on error, so a
   crash mid-run still persists what was recorded. Because writes accumulate in
   memory and flush at the end, a long cognify run isn't slowed by per-call IO.

5. **Concurrency.** cognify extracts graphs concurrently, so multiple async tasks
   may record at once — the `Mutex<BTreeMap>` makes inserts safe; identical inputs
   collapse to one entry (idempotent by hash).

6. **Tests:**
   - Record against an in-memory `MockLlm` (from `test-utils`, a dev-dependency)
     returning a known graph; assert the cassette contains one entry with that
     response after `flush()`.
   - Record two calls with identical messages → exactly one entry (dedup).
   - Drop-flushes: drop a `RecordingLlm` without explicit `flush()` and assert the
     file exists and parses.

## Acceptance / verification

- `cargo test -p cognee-llm --features mock` passes (the round-trip test will be
  completed in T3 once `ReplayLlm` exists; here assert cassette contents directly).
- No `unwrap()` in non-test code (lock `.unwrap()` allowed with the poison
  comment).
- `cargo clippy -p cognee-llm --features mock -- -D warnings` clean.
